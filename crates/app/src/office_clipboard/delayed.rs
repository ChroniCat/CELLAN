//! A message-only window owns the delayed formats independently of the grid's event loop.
use super::{alloc_global, cf_html, wide};
use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};
use std::thread::JoinHandle;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{DataExchange::*, LibraryLoader::GetModuleHandleW},
    UI::WindowsAndMessaging::*,
};

type Matrix = Vec<Vec<(sheetcore::Cell, u16)>>;

pub fn is_full_columns((first_row, _, last_row, _): (u32, u32, u32, u32)) -> bool {
    first_row == 0 && last_row == sheetcore::sync::MAX_ROWS - 1
}

pub struct Range {
    book: sheetcore::Workbook,
    rect: (u32, u32, u32, u32),
    visible_rows: Option<Vec<u32>>,
    cells: std::sync::OnceLock<Matrix>,
}

impl Range {
    pub fn new(wb: &sheetcore::Workbook, si: usize, rect: (u32, u32, u32, u32)) -> Self {
        Self::new_with_rows(wb, si, rect, None)
    }

    pub fn new_filtered(
        wb: &sheetcore::Workbook,
        si: usize,
        rect: (u32, u32, u32, u32),
        visible_rows: Vec<u32>,
    ) -> Self {
        Self::new_with_rows(wb, si, rect, Some(visible_rows))
    }

    fn new_with_rows(
        wb: &sheetcore::Workbook,
        si: usize,
        rect: (u32, u32, u32, u32),
        visible_rows: Option<Vec<u32>>,
    ) -> Self {
        let mut book = sheetcore::Workbook::new_blank();
        let source = wb.sheet(si);
        book.sheets[0] = source.search_snapshot();
        book.sheets[0].identity = source.identity;
        book.sheets[0].merges = source.merges.clone();
        book.sheets[0].hidden_cols = source.hidden_cols.clone();
        book.sheets[0].hidden_rows = source.hidden_rows.clone();
        book.sheets[0].hidden_rows_user = source.hidden_rows_user.clone();
        book.sheets[0].cols = source.cols.clone();
        book.sheets[0].rows = source.rows.clone();
        book.styles = wb.styles.clone();
        book.date_system = wb.date_system;
        Self {
            book,
            rect,
            visible_rows,
            cells: Default::default(),
        }
    }

    pub fn is_filtered(&self) -> bool {
        self.visible_rows.is_some()
    }

    pub fn book(&self) -> &sheetcore::Workbook {
        &self.book
    }

    pub fn matches_cut_source(&self, live: &sheetcore::model::Sheet) -> bool {
        let source = self.book.sheet(0);
        let (r1, c1, r2, c2) = self.rect;
        let overlaps =
            |&&(a, b, c, d): &&(u32, u32, u32, u32)| a <= r2 && b <= c2 && c >= r1 && d >= c1;
        let same_cell = |r: u32, c: u32| {
            source.get(r, c) == live.get(r, c)
                && source.style_of(r, c) == live.style_of(r, c)
                && source.explicit_style_of(r, c) == live.explicit_style_of(r, c)
        };
        source.identity == live.identity
            && (self.is_filtered()
                || source
                    .merges
                    .iter()
                    .filter(overlaps)
                    .eq(live.merges.iter().filter(overlaps)))
            && match &self.visible_rows {
                Some(rows) => rows.iter().all(|&r| (c1..=c2).all(|c| same_cell(r, c))),
                None => (r1..=r2).all(|r| (c1..=c2).all(|c| same_cell(r, c))),
            }
    }

    pub fn cells(&self) -> &Matrix {
        self.cells.get_or_init(|| {
            let (r1, c1, r2, c2) = self.rect;
            let sh = self.book.sheet(0);
            let build_row = |r: u32| -> Vec<(sheetcore::Cell, u16)> {
                (c1..=c2)
                    .map(|c| (sh.get(r, c).cloned().unwrap_or_default(), sh.style_of(r, c)))
                    .collect()
            };
            match &self.visible_rows {
                Some(rows) => rows.iter().map(|&r| build_row(r)).collect(),
                None => (r1..=r2).map(build_row).collect(),
            }
        })
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        let (r1, c1, r2, c2) = self.rect;
        let sh = self.book.sheet(0);
        let mut append_row = |r: u32| {
            for c in c1..=c2 {
                if c != c1 {
                    text.push('\t');
                }
                if let Some(cell) = sh.get(r, c) {
                    let style = self.book.styles.get(sh.style_of(r, c));
                    text.push_str(&sheetcore::style::format_value_in(
                        self.book.date_system,
                        &cell.value,
                        &style.format,
                    ));
                }
            }
            text.push('\n');
        };
        match &self.visible_rows {
            Some(rows) => {
                for &r in rows {
                    append_row(r);
                }
            }
            None => {
                for r in r1..=r2 {
                    append_row(r);
                }
            }
        }
        text
    }
}

struct Selection {
    range: std::sync::Arc<Range>,
    xml: u32,
    html: u32,
    pending: Vec<u32>,
}

impl Selection {
    fn bytes(&self, format: u32) -> Vec<u8> {
        if format == self.xml {
            let xml = if is_full_columns(self.range.rect) {
                let Some(xml) = super::sparse_column_xmlss(&self.range.book, 0, self.range.rect)
                else {
                    return Vec::new();
                };
                xml
            } else {
                super::selection_xmlss(&self.range.book, 0, self.range.rect)
            };
            let mut bytes = xml.into_bytes();
            bytes.push(0);
            bytes
        } else if format == self.html {
            cf_html(&crate::SheetApp::clipboard_html(
                &self.range.book,
                0,
                self.range.rect,
            ))
        } else {
            self.range
                .text()
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect()
        }
    }

    fn render(&mut self, format: u32) {
        let Some(i) = self.pending.iter().position(|f| *f == format) else {
            return;
        };
        let bytes = self.bytes(format);
        if bytes.is_empty() {
            return;
        }
        let mem = alloc_global(&bytes);
        if !mem.is_null() {
            unsafe {
                if SetClipboardData(format, mem).is_null() {
                    super::GlobalFree(mem);
                } else {
                    self.pending.swap_remove(i);
                }
            }
        }
    }
}

thread_local! { static SELECTION: RefCell<Option<Selection>> = const { RefCell::new(None) }; }

// The owner thread can be asked to retire after egui publishes a replacement,
// or to persist pending formats after the application window has gone away.
const PUBLISHED: u8 = 0;
const AFTER_REPLACEMENT: u8 = 1;
const FINISH: u8 = 2;

unsafe fn persist_formats(hwnd: HWND) -> bool {
    unsafe {
        if GetClipboardOwner() != hwnd {
            return true;
        }
        if OpenClipboard(hwnd) == 0 {
            return false;
        }
        let done = GetClipboardOwner() != hwnd
            || SELECTION.with_borrow_mut(|state| {
                state.as_mut().is_none_or(|s| {
                    for format in s.pending.clone() {
                        s.render(format);
                    }
                    s.pending.is_empty()
                })
            });
        CloseClipboard();
        done
    }
}

unsafe extern "system" fn messages(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_RENDERFORMAT => SELECTION.with_borrow_mut(|data| {
                if let Some(s) = data {
                    s.render(wp as u32);
                }
            }),
            WM_RENDERALLFORMATS => {
                persist_formats(hwnd);
            }
            WM_DESTROYCLIPBOARD => {
                SELECTION.with_borrow_mut(|s| *s = None);
                PostMessageW(hwnd, WM_NULL, 0, 0);
            }
            WM_DESTROY => PostQuitMessage(0),
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        0
    }
}

struct Worker {
    window: usize,
    request: Arc<AtomicU8>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn request(&self, mode: u8) {
        self.request.store(mode, Ordering::Release);
        // WM_NULL is harmless even if a completed worker's HWND has been reused.
        unsafe {
            PostMessageW(self.window as HWND, WM_NULL, 0, 0);
        }
    }
}

static RETIRING: Mutex<Vec<Worker>> = Mutex::new(Vec::new());

/// Called after run_native returns and has destroyed the GUI and dropped its
/// document locks. Keep the owning process alive until Win32 owns the bytes.
pub fn finish_after_gui_exit() {
    let retiring = std::mem::take(&mut *RETIRING.lock().unwrap_or_else(|e| e.into_inner()));
    for worker in &retiring {
        worker.request(FINISH);
    }
    for worker in retiring {
        let _ = worker.thread.join();
    }
}

pub struct Owner {
    window: usize,
    worker: Option<Worker>,
}

impl Owner {
    pub fn is_current(&self) -> bool {
        #[cfg(test)]
        {
            crate::TEST_CLIP_GEN.with(|seq| seq.get() == self.window)
        }
        #[cfg(not(test))]
        unsafe {
            GetClipboardOwner() == self.window as HWND
        }
    }

    pub fn publish(range: std::sync::Arc<Range>) -> Option<Self> {
        #[cfg(test)]
        {
            let _ = range;
            crate::clear_test_clipboard();
            Some(Self {
                window: crate::TEST_CLIP_GEN.with(|seq| seq.get()),
                worker: None,
            })
        }
        #[cfg(not(test))]
        Self::publish_native(range)
    }

    fn publish_native(range: std::sync::Arc<Range>) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let request = Arc::new(AtomicU8::new(PUBLISHED));
        let mode = request.clone();
        let thread = std::thread::spawn(move || unsafe {
            let sh = range.book.sheet(0);
            // The XMLSS table preserves the complete dimensions and sparse row indices
            // of full columns without expanding every blank into HTML or plain text.
            let xml_only = is_full_columns(range.rect)
                || sh
                    .iter_rect(range.rect)
                    .any(|(_, cell)| cell.formula.is_some());
            let class = wide("CellanDelayedClipboard");
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(messages),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return;
            }
            let xml = RegisterClipboardFormatW(wide("XML Spreadsheet").as_ptr());
            let html = RegisterClipboardFormatW(wide("HTML Format").as_ptr());
            if xml == 0 || html == 0 || OpenClipboard(hwnd) == 0 {
                DestroyWindow(hwnd);
                return;
            }
            let pending = if range.is_filtered() {
                vec![13]
            } else if xml_only {
                vec![xml]
            } else {
                vec![xml, html, 13]
            };
            let ready = EmptyClipboard() != 0
                && pending.iter().all(|&f| {
                    SetClipboardData(f, null_mut());
                    IsClipboardFormatAvailable(f) != 0
                });
            if ready {
                SELECTION.with_borrow_mut(|s| {
                    *s = Some(Selection {
                        range,
                        xml,
                        html,
                        pending,
                    })
                });
            }
            CloseClipboard();
            if !ready || tx.send(hwnd as usize).is_err() {
                DestroyWindow(hwnd);
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
                match mode.load(Ordering::Acquire) {
                    AFTER_REPLACEMENT if GetClipboardOwner() != hwnd => {
                        DestroyWindow(hwnd);
                    }
                    FINISH => {
                        if persist_formats(hwnd) {
                            DestroyWindow(hwnd);
                        } else {
                            SetTimer(hwnd, 1, 100, None);
                        }
                    }
                    _ => {}
                }
            }
        });
        let window = rx.recv().ok()?;
        Some(Self {
            window,
            worker: Some(Worker {
                window,
                thread,
                request,
            }),
        })
    }

    fn retire(&mut self, mode: u8) {
        if let Some(worker) = self.worker.take() {
            worker.request(mode);
            let mut retiring = RETIRING.lock().unwrap_or_else(|e| e.into_inner());
            retiring.retain(|worker| !worker.thread.is_finished());
            retiring.push(worker);
        }
    }

    pub fn after_replacement(mut self) {
        self.retire(AFTER_REPLACEMENT);
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.retire(FINISH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::{Cell, Value, Workbook};

    /// A fresh process gets its own window station and therefore its own clipboard.
    /// Exercise the Win32 render messages without ever opening the desktop user's clipboard.
    #[test]
    fn native_clipboard_renders_and_keeps_formats_after_owner_exit() {
        const CHILD_FLAG: &str = "CELLAN_PRIVATE_CLIPBOARD_TEST";
        const TEST: &str = "office_clipboard::delayed::tests::native_clipboard_renders_and_keeps_formats_after_owner_exit";
        if std::env::var_os(CHILD_FLAG).is_none() || !std::env::args().any(|arg| arg == TEST) {
            let _env = crate::test_env::TestEnv::new("native-clipboard");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD_FLAG, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use std::ptr::null;
        use windows_sys::Win32::System::StationsAndDesktops::*;
        const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
        let name = wide(&format!("CellanClipTest{}", std::process::id()));
        // This child owns the station handles for its entire lifetime.
        unsafe {
            let station = CreateWindowStationW(name.as_ptr(), 0, MAXIMUM_ALLOWED, null());
            assert!(!station.is_null(), "{}", std::io::Error::last_os_error());
            assert_ne!(
                SetProcessWindowStation(station),
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            let desktop = CreateDesktopW(
                wide("Default").as_ptr(),
                null(),
                null(),
                0,
                MAXIMUM_ALLOWED,
                null(),
            );
            assert!(!desktop.is_null(), "{}", std::io::Error::last_os_error());
            assert_ne!(
                SetThreadDesktop(desktop),
                0,
                "{}",
                std::io::Error::last_os_error()
            );
        }
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Text("快照".into()),
                formula: None,
            },
        );
        let range = std::sync::Arc::new(Range::new(&wb, 0, (0, 0, 35, 30)));
        let owner = Owner::publish_native(range.clone())
            .expect("register delayed formats in private station");
        let xml_format = unsafe { RegisterClipboardFormatW(wide("XML Spreadsheet").as_ptr()) };
        let html_format = unsafe { RegisterClipboardFormatW(wide("HTML Format").as_ptr()) };
        let read = |format| {
            String::from_utf8(super::super::read_format(format).expect("clipboard data")).unwrap()
        };
        assert!(read(xml_format).contains(">快照</Data>"));
        // Hold the clipboard open as an external reader. Releasing the GUI's
        // owner must return before that reader releases its lock; process exit
        // still waits until every outstanding format has been persisted.
        assert_ne!(unsafe { OpenClipboard(null_mut()) }, 0);
        let (released, receipt) = std::sync::mpsc::channel();
        let exit = std::thread::spawn(move || {
            drop(owner);
            let _ = released.send(());
            finish_after_gui_exit();
        });
        let returned = receipt
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        unsafe {
            CloseClipboard();
        }
        assert!(returned, "dropping the GUI blocked on clipboard rendering");
        exit.join().unwrap();
        assert!(read(html_format).contains("快照"));
        let utf16 = super::super::read_format_raw(13).unwrap();
        let words: Vec<_> = utf16
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|w| *w != 0)
            .collect();
        assert!(String::from_utf16(&words).unwrap().starts_with("快照"));
        assert!(range.cells.get().is_none());

        let old_range = Arc::new(Range::new(&wb, 0, (0, 0, 36, 30)));
        let old = Owner::publish_native(old_range.clone()).unwrap();
        old.after_replacement();
        let html = "<table><tr><td>替换</td></tr></table>";
        assert!(super::super::write_office_clipboard_native(
            "<Workbook>替换</Workbook>",
            html,
            "替换"
        ));
        // Synchronize the worker before checking the OS-owned replacement.
        finish_after_gui_exit();
        assert!(read(xml_format).contains("替换") && read(html_format).contains("替换"));
        assert_eq!(Arc::strong_count(&old_range), 1);

        let bottom = sheetcore::sync::MAX_ROWS - 1;
        wb.sheets[0].set(
            bottom,
            2,
            Cell {
                value: Value::Number(42.0),
                formula: None,
            },
        );
        let columns = std::sync::Arc::new(Range::new(&wb, 0, (0, 0, bottom, 2)));
        let owner = Owner::publish_native(columns.clone()).expect("register full columns");
        let xml = read(xml_format);
        assert!(xml.contains("ss:ExpandedColumnCount=\"3\""));
        assert!(xml.contains("ss:ExpandedRowCount=\"1048576\""));
        assert!(xml.contains("ss:Index=\"1048576\""));
        assert!(xml.contains(">42</Data>"));
        assert!(xml.len() < 16_384 && columns.cells.get().is_none());
        drop(owner);
        finish_after_gui_exit();
    }

    #[test]
    fn deferred_clipboard_format_requests_render_only_the_snapshot() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Text("中文".into()),
                formula: None,
            },
        );
        wb.sheets[0].set(
            1,
            0,
            Cell {
                value: Value::Number(42.0),
                formula: Some("=B2+2".into()),
            },
        );
        wb.sheets[0].merges.push((0, 0, 0, 1));
        let range = std::sync::Arc::new(Range::new(&wb, 0, (0, 0, 1, 1)));
        let selection = Selection {
            range: range.clone(),
            xml: 100,
            html: 101,
            pending: vec![100],
        };
        wb.sheets[0].set(0, 0, Cell::default());
        let xml = String::from_utf8(selection.bytes(100)).unwrap();
        assert!(xml.contains("ss:MergeAcross=\"1\""));
        assert!(xml.contains("ss:Formula=\"=RC[1]+2\""));
        assert!(xml.contains(">中文</Data>"));
        assert!(xml.ends_with('\0'));
        let html = String::from_utf8(selection.bytes(101)).unwrap();
        assert!(html.contains("colspan=\"2\""));
        assert!(html.contains("中文"));
        let bytes = selection.bytes(13);
        let words: Vec<_> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(String::from_utf16(&words).unwrap(), "中文\t\n42\t\n\0");
        assert!(range.cells.get().is_none());
    }
}
