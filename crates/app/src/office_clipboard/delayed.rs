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

    pub fn dimensions(&self) -> (u32, u32) {
        let (r1, c1, r2, c2) = self.rect;
        let rows = match &self.visible_rows {
            Some(v) => v.len() as u32,
            None => r2 - r1 + 1,
        };
        (rows, c2 - c1 + 1)
    }

    pub fn xmlss(&self) -> Option<String> {
        if let Some(rows) = &self.visible_rows {
            Some(super::selection_xmlss_filtered(&self.book, 0, self.rect, rows))
        } else if is_full_columns(self.rect) {
            super::sparse_column_xmlss(&self.book, 0, self.rect)
        } else {
            Some(super::selection_xmlss(&self.book, 0, self.rect))
        }
    }

    pub fn html(&self) -> String {
        crate::SheetApp::clipboard_html_filtered(&self.book, 0, self.rect, self.visible_rows.as_deref())
    }

    pub fn matches_cut_source(&self, live: &sheetcore::model::Sheet) -> bool {
        use std::collections::HashSet;
        let source = self.book.sheet(0);
        if source.identity != live.identity {
            return false;
        }
        let (r1, c1, r2, c2) = self.rect;
        let overlaps =
            |&&(a, b, c, d): &&(u32, u32, u32, u32)| a <= r2 && b <= c2 && c >= r1 && d >= c1;
        if !self.is_filtered()
            && !source
                .merges
                .iter()
                .filter(overlaps)
                .eq(live.merges.iter().filter(overlaps))
        {
            return false;
        }
        for col in c1..=c2 {
            if source.col_styles.get(&col) != live.col_styles.get(&col) {
                return false;
            }
        }
        let vis_set: Option<HashSet<u32>> = self
            .visible_rows
            .as_ref()
            .map(|rows| rows.iter().copied().collect());
        let row_in_scope = |r: u32| -> bool {
            r >= r1 && r <= r2 && vis_set.as_ref().is_none_or(|s| s.contains(&r))
        };
        for &r in source.row_styles.keys().chain(live.row_styles.keys()) {
            if row_in_scope(r) && source.row_styles.get(&r) != live.row_styles.get(&r) {
                return false;
            }
        }
        let same_cell = |r: u32, c: u32| {
            source.get(r, c) == live.get(r, c)
                && source.style_of(r, c) == live.style_of(r, c)
                && source.explicit_style_of(r, c) == live.explicit_style_of(r, c)
        };
        for (k, _) in source.iter_rect(self.rect).chain(live.iter_rect(self.rect)) {
            if row_in_scope(k.row) && !same_cell(k.row, k.col) {
                return false;
            }
        }
        for (k, _) in source.styles.iter().chain(live.styles.iter()) {
            if k.col >= c1 && k.col <= c2 && row_in_scope(k.row) && !same_cell(k.row, k.col) {
                return false;
            }
        }
        true
    }

    /// 稀疏提取选区内的非空或带非零有效样式的单元格 `(rel_row, rel_col, src_row, Option<Cell>, style_id)`，
    /// 避免大选区内部粘贴时将百万空格稠密物化进内存与 `last_paste`。
    pub fn sparse_entries(
        &self,
    ) -> Result<Vec<(u32, u32, u32, Option<sheetcore::Cell>, u16)>, &'static str> {
        use std::collections::{BTreeSet, HashMap};
        let (r1, c1, r2, c2) = self.rect;
        let sh = self.book.sheet(0);
        let vis_map: Option<HashMap<u32, u32>> = self.visible_rows.as_ref().map(|rows| {
            rows.iter()
                .enumerate()
                .map(|(idx, &r)| (r, idx as u32))
                .collect()
        });
        let map_row = |r: u32| -> Option<u32> {
            if r < r1 || r > r2 {
                return None;
            }
            match &vis_map {
                Some(m) => m.get(&r).copied(),
                None => Some(r - r1),
            }
        };

        // Collect sparse coordinates `(rel_row, rel_col, src_row)`
        let mut coords: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
        let max_keys = sheetcore::undo::MAX_DENSE_RECT_KEYS;

        for (k, _) in sh.iter_rect(self.rect) {
            if let Some(rr) = map_row(k.row) {
                coords.insert((rr, k.col - c1, k.row));
                if coords.len() > max_keys {
                    return Err("粘贴内容过大（实际非空或带格式单元格超过 50 万个）");
                }
            }
        }
        for (k, _) in sh.styles.iter() {
            if k.col >= c1 && k.col <= c2 {
                if let Some(rr) = map_row(k.row) {
                    coords.insert((rr, k.col - c1, k.row));
                    if coords.len() > max_keys {
                        return Err("粘贴内容过大（实际非空或带格式单元格超过 50 万个）");
                    }
                }
            }
        }
        for (&r, &sid) in &sh.row_styles {
            if sid != 0 {
                if let Some(rr) = map_row(r) {
                    for col in c1..=c2 {
                        coords.insert((rr, col - c1, r));
                        if coords.len() > max_keys {
                            return Err("粘贴内容过大（实际非空或带格式单元格超过 50 万个）");
                        }
                    }
                }
            }
        }
        for col in c1..=c2 {
            if sh.col_styles.get(&col).copied().unwrap_or(0) != 0 {
                match &self.visible_rows {
                    Some(rows) => {
                        for (idx, &r) in rows.iter().enumerate() {
                            coords.insert((idx as u32, col - c1, r));
                            if coords.len() > max_keys {
                                return Err("粘贴内容过大（实际非空或带格式单元格超过 50 万个）");
                            }
                        }
                    }
                    None => {
                        for r in r1..=r2 {
                            coords.insert((r - r1, col - c1, r));
                            if coords.len() > max_keys {
                                return Err("粘贴内容过大（实际非空或带格式单元格超过 50 万个）");
                            }
                        }
                    }
                }
            }
        }

        let mut out = Vec::with_capacity(coords.len());
        for (rel_r, rel_c, src_r) in coords {
            let src_c = c1 + rel_c;
            let cell = sh.get(src_r, src_c).cloned();
            let style = sh.style_of(src_r, src_c);
            if cell.as_ref().is_some_and(|c| !c.is_blank()) || style != 0 {
                out.push((rel_r, rel_c, src_r, cell, style));
            }
        }
        Ok(out)
    }

    /// 稀疏提取剪切源需要清空的非空或显式样式坐标 `(src_row, src_col)`。
    pub fn sparse_cut_source_keys(&self) -> Result<Vec<(u32, u32)>, &'static str> {
        use std::collections::{BTreeSet, HashSet};
        let (r1, c1, r2, c2) = self.rect;
        let sh = self.book.sheet(0);
        let vis_set: Option<HashSet<u32>> = self
            .visible_rows
            .as_ref()
            .map(|rows| rows.iter().copied().collect());
        let row_ok = |r: u32| r >= r1 && r <= r2 && vis_set.as_ref().is_none_or(|s| s.contains(&r));

        let mut keys = BTreeSet::new();
        let max_keys = sheetcore::undo::MAX_DENSE_RECT_KEYS;
        for (k, _) in sh.iter_rect(self.rect) {
            if row_ok(k.row) {
                keys.insert((k.row, k.col));
                if keys.len() > max_keys {
                    return Err("剪切源非空单元格超过 50 万个，请分批操作");
                }
            }
        }
        for (k, _) in sh.styles.iter() {
            if k.col >= c1 && k.col <= c2 && row_ok(k.row) {
                keys.insert((k.row, k.col));
                if keys.len() > max_keys {
                    return Err("剪切源带格式单元格超过 50 万个，请分批操作");
                }
            }
        }
        Ok(keys.into_iter().collect())
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
            let Some(xml) = self.range.xmlss() else {
                return Vec::new();
            };
            let mut bytes = xml.into_bytes();
            bytes.push(0);
            bytes
        } else if format == self.html {
            cf_html(&self.range.html())
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
            // of full columns and formulas without losing structure in HTML/TSV.
            let has_formula = match &range.visible_rows {
                Some(rows) => {
                    let vis: std::collections::HashSet<u32> = rows.iter().copied().collect();
                    sh.iter_rect(range.rect)
                        .any(|(k, cell)| cell.formula.is_some() && vis.contains(&k.row))
                }
                None => sh
                    .iter_rect(range.rect)
                    .any(|(_, cell)| cell.formula.is_some()),
            };
            let xml_only = (!range.is_filtered() && is_full_columns(range.rect)) || has_formula;
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
            let pending = if xml_only {
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

        let filtered = std::sync::Arc::new(Range::new_filtered(&wb, 0, (0, 0, bottom, 2), vec![0, bottom]));
        let owner_filt = Owner::publish_native(filtered.clone()).expect("register filtered delayed rich formats");
        let filt_xml = read(xml_format);
        let filt_html = read(html_format);
        assert!(filt_xml.contains("ss:ExpandedRowCount=\"2\""));
        assert!(filt_xml.contains(">快照</Data>") && filt_xml.contains(">42</Data>"));
        assert!(filt_html.contains("快照") && filt_html.contains("42"));
        drop(owner_filt);
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

    #[test]
    fn filtered_delayed_selection_renders_compacted_rich_xmlss_and_html() {
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(sheetcore::style::CellStyle {
            bold: true,
            fill: Some([255, 235, 156]),
            ..Default::default()
        });
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, Cell { value: Value::Text("保留首行".into()), formula: None });
        sh.set_style(0, 0, bold);
        sh.set(1, 0, Cell { value: Value::Text("隐藏行不应出现".into()), formula: None });
        sh.set(5, 0, Cell { value: Value::Number(10.0), formula: None });
        sh.set(5, 1, Cell { value: Value::Number(20.0), formula: Some("=A6*2".into()) });
        let range = Arc::new(Range::new_filtered(&wb, 0, (0, 0, 5, 1), vec![0, 5]));
        let xml = range.xmlss().unwrap();
        assert!(xml.contains("ss:ExpandedColumnCount=\"2\" ss:ExpandedRowCount=\"2\""));
        assert!(xml.contains("保留首行"));
        assert!(!xml.contains("隐藏行不应出现"));
        assert!(xml.contains("ss:Formula=\"=RC[-1]*2\""));
        assert!(xml.contains("ss:Bold=\"1\""));
        let html = range.html();
        assert_eq!(html.matches("<tr>").count(), 2);
        assert!(html.contains("保留首行") && !html.contains("隐藏行不应出现"));
        assert!(html.contains("font-weight:bold;"));
    }

    #[test]
    fn external_full_columns_and_million_cell_sparse_xmlss_paste_and_undo() {
        let _env = crate::test_env::TestEnv::new("sparse-xmlss-paste");
        let ctx = eframe::egui::Context::default();
        let mut app = crate::SheetApp::new_with_context(&ctx, None);

        // 构造外部 Excel 整列稀疏 XMLSS（3 列 × 1048576 行，含样式、合并与相对/混合公式）
        let mut src_wb = Workbook::new_blank();
        let bold = src_wb.styles.intern(sheetcore::style::CellStyle {
            bold: true,
            fill: Some([200, 230, 255]),
            ..Default::default()
        });
        src_wb.sheets[0].set(0, 0, Cell { value: Value::Number(7.0), formula: None });
        src_wb.sheets[0].set_style(0, 0, bold);
        src_wb.sheets[0].set(
            900_000,
            1,
            Cell {
                value: Value::Number(14.0),
                formula: Some("=A$1*2".into()),
            },
        );
        src_wb.sheets[0].merge_range(10, 0, 11, 1);
        src_wb.sheets[0].set(10, 0, Cell { value: Value::Text("整列合并".into()), formula: None });
        let xml = super::super::sparse_column_xmlss(&src_wb, 0, (0, 0, 1_048_575, 2)).unwrap();
        let sparse = sheetcore::clipboard_xmlss::parse_sparse(&xml).expect("稀疏整列解析应成功");

        // 目标表 D:F 尾部预置旧数据，验证粘贴后会被稀疏清理且可完整 Undo
        app.wb.sheets[0].set(950_000, 4, Cell { value: Value::Number(999.0), formula: None });
        app.wb.sheets[0].set_style(950_000, 4, bold);
        app.sel = (0, 3); // D1
        app.sel_end = app.sel;
        assert!(app.paste_sparse_xmlss(&sparse), "status={}", app.status);
        assert!(app.last_paste.is_none(), "大范围稀疏粘贴不应驻留稠密 last_paste");
        assert_eq!(app.wb.sheets[0].value(0, 3), Value::Number(7.0));
        assert!(app.wb.styles.get(app.wb.sheets[0].style_of(0, 3)).bold);
        assert_eq!(
            app.wb.sheets[0].get(900_000, 4).and_then(|c| c.formula.as_deref()),
            Some("=D$1*2")
        );
        assert_eq!(app.wb.sheets[0].value(900_000, 4), Value::Number(14.0));
        assert!(app.wb.sheets[0].merges.contains(&(10, 3, 11, 4)));
        assert!(app.wb.sheets[0].get(950_000, 4).is_none(), "目标列旧尾部非空格应被清空");
        assert_eq!(app.wb.sheets[0].style_of(950_000, 4), 0);

        // Undo 必须完整还原尾部旧格与合并状态
        app.do_undo(false);
        assert_eq!(app.wb.sheets[0].value(950_000, 4), Value::Number(999.0));
        assert!(app.wb.styles.get(app.wb.sheets[0].style_of(950_000, 4)).bold);
        assert!(app.wb.sheets[0].get(0, 3).is_none());
        assert!(!app.wb.sheets[0].merges.contains(&(10, 3, 11, 4)));
    }

    #[test]
    fn internal_million_cell_sparse_deferred_copy_and_cut_paste_without_dense_matrix() {
        let _env = crate::test_env::TestEnv::new("internal-sparse-deferred-paste");
        let ctx = eframe::egui::Context::default();
        let mut app = crate::SheetApp::new_with_context(&ctx, None);
        let bold = app.wb.styles.intern(sheetcore::style::CellStyle {
            bold: true,
            ..Default::default()
        });
        // A1:J100000 共 100 万格（超过 50 万稠密阈值），仅含少量非空/样式格
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
        app.wb.sheets[0].set_style(0, 0, bold);
        app.wb.sheets[0].set(
            99_999,
            9,
            Cell {
                value: Value::Number(15.0),
                formula: Some("=A1*3".into()),
            },
        );
        app.wb.sheets.push(sheetcore::Sheet::new("Dest"));
        app.wb.sheets[1].set(80_000, 5, Cell { value: Value::Text("旧脏值".into()), formula: None });

        app.sheet_idx = 0;
        app.sel = (0, 0);
        app.sel_end = (99_999, 9);
        app.clip_copy(false, &ctx);
        let range = app
            .clip
            .as_ref()
            .and_then(|c| c.deferred.clone())
            .expect("100 万格选区应走延迟快照");

        app.sheet_idx = 1;
        app.sel = (2, 1); // B3
        app.sel_end = app.sel;
        app.clip_paste(false, None);

        assert!(range.cells.get().is_none(), "严禁物化 100 万格稠密 Matrix");
        assert!(app.last_paste.is_none(), "严禁把百万空格塞进 last_paste");
        assert_eq!(app.wb.sheets[1].value(2, 1), Value::Number(5.0), "status={}", app.status);
        assert!(app.wb.styles.get(app.wb.sheets[1].style_of(2, 1)).bold);
        assert_eq!(
            app.wb.sheets[1].get(100_001, 10).and_then(|c| c.formula.as_deref()),
            Some("=B3*3")
        );
        assert_eq!(app.wb.sheets[1].value(100_001, 10), Value::Number(15.0));
        assert!(app.wb.sheets[1].get(80_000, 5).is_none(), "目标矩形内旧值应被稀疏清理");

        app.do_undo(false);
        assert_eq!(app.wb.sheets[1].value(80_000, 5), Value::Text("旧脏值".into()));
        assert!(app.wb.sheets[1].get(2, 1).is_none());

        // 5 万格稀疏延迟剪切（> 4,096 且 <= 50 万）同样走稀疏路径、清空剪切源并支持 Undo
        app.sheet_idx = 0;
        app.sel = (0, 0);
        app.sel_end = (4_999, 9);
        app.clip_copy(true, &ctx);
        let cut_range = app.clip.as_ref().and_then(|c| c.deferred.clone()).unwrap();
        app.sheet_idx = 1;
        app.sel = (10, 0);
        app.sel_end = app.sel;
        app.clip_paste(false, None);
        assert!(cut_range.cells.get().is_none(), "稀疏剪切粘贴不得稠密展开");
        assert!(app.wb.sheets[0].get(0, 0).is_none(), "剪切源应被清空");
        assert_eq!(app.wb.sheets[1].value(10, 0), Value::Number(5.0));
        app.do_undo(false);
        assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(5.0));
    }
}
