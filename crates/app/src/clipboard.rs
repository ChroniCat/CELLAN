//! Internal clipboard snapshots, ownership and source selection.

mod copy;
mod notes;
mod office;
mod paste;
mod sparse;
mod special;

pub(super) use sparse::BackgroundPaste;

use crate::{SheetApp, office_clipboard};
use sheetcore::{Cell, Workbook};

/// 内部剪贴板：整块格子 + 样式号
#[derive(Clone)]
pub(super) struct Clip {
    pub(super) deferred: Option<std::sync::Arc<office_clipboard::delayed::Range>>,
    pub(super) sheet: usize,
    pub(super) rect: (u32, u32, u32, u32),
    pub(super) cells: Vec<Vec<(sheetcore::model::Cell, u16)>>,
    /// Absolute source rows for a filtered or multi-area selection, in compact clipboard order.
    pub(super) source_rows: Option<Vec<u32>>,
    /// Absolute source columns for a non-contiguous multi-area selection, in compact clipboard order.
    pub(super) source_cols: Option<Vec<u32>>,
    /// 选区内的合并区域，已裁到选区范围（0 基绝对坐标）
    pub(super) merges: Vec<(u32, u32, u32, u32)>,
    pub(super) cut: bool,
}

impl Clip {
    pub(super) fn cells(&self) -> &[Vec<(Cell, u16)>] {
        self.deferred.as_ref().map_or(self.cells.as_slice(), |s| s.cells())
    }

    pub(super) fn live_sheet_index(&self, wb: &Workbook) -> Option<usize> {
        if let Some(range) = &self.deferred {
            let id = range.book().sheet(0).identity;
            wb.sheets.iter().position(|sheet| sheet.identity == id)
        } else {
            wb.sheets.get(self.sheet).map(|_| self.sheet)
        }
    }

    pub(super) fn source_row(&self, index: usize) -> u32 {
        self.source_rows
            .as_ref()
            .and_then(|rows| rows.get(index).copied())
            .unwrap_or_else(|| self.rect.0 + index as u32)
    }

    fn source_col(&self, index: usize) -> u32 {
        self.source_cols
            .as_ref()
            .and_then(|cols| cols.get(index).copied())
            .unwrap_or_else(|| self.rect.1 + index as u32)
    }
}

#[derive(Clone)]
pub(crate) struct LastPasteState {
    pub(crate) sheet_idx: usize,
    pub(crate) rect: (u32, u32, u32, u32),
    pub(crate) cells: Vec<Vec<(sheetcore::model::Cell, u16)>>,
    pub(crate) mode: u8,
    pub(crate) before: Vec<(u32, u32, Option<sheetcore::model::Cell>, u16)>,
    pub(crate) popup_open: bool,
}

fn copied_text_matches(copied: &str, current: &str) -> bool {
    copied == current || copied.lines().eq(current.lines())
}

/// 系统剪贴板快照（富格式由 office_clipboard 单独读取同一份系统剪贴板）。
#[derive(Clone, Default)]
pub(super) struct SysClip {
    pub(super) text: String,
}

#[cfg(test)]
std::thread_local! {
    /// 单测专用剪贴板：按线程隔离，避免并行用例互相污染。
    pub(super) static TEST_CLIP_GEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEST_CLIP: std::cell::RefCell<Option<SysClip>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_clipboard(text: &str) {
    TEST_CLIP_GEN.with(|seq| seq.set(seq.get().wrapping_add(1)));
    TEST_CLIP.with(|c| *c.borrow_mut() = Some(SysClip { text: text.to_string() }));
}

#[cfg(test)]
pub(crate) fn clear_test_clipboard() {
    TEST_CLIP_GEN.with(|seq| seq.set(seq.get().wrapping_add(1)));
    TEST_CLIP.with(|c| *c.borrow_mut() = None);
}

fn clipboard_text() -> String {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut c| c.get_text().ok())
        .unwrap_or_default()
}

pub(super) fn read_system_clip() -> SysClip {
    #[cfg(test)]
    {
        // 单测绝不读真实系统剪贴板，避免机器上残留的文本造成不确定性。
        return TEST_CLIP.with(|c| c.borrow().clone()).unwrap_or_default();
    }
    #[cfg(not(test))]
    {
        SysClip { text: clipboard_text() }
    }
}

#[cfg(windows)]
unsafe extern "system" {
    pub(super) fn GetClipboardSequenceNumber() -> u32;
}

#[inline]
pub(crate) fn current_os_clipboard_seq() -> Option<u32> {
    #[cfg(all(windows, not(test)))]
    {
        let seq = unsafe { GetClipboardSequenceNumber() };
        (seq != 0).then_some(seq)
    }
    #[cfg(not(all(windows, not(test))))]
    {
        None
    }
}

impl SheetApp {
    pub(super) fn retire_previous_clipboard(&mut self) {
        if let Some(owner) = self.clipboard_owner.take() {
            owner.after_replacement();
        }
        self.clip_notes.clear();
        self.clip_threads.clear();
        self.clip_notes_captured = false;
    }

    #[inline]
    pub(crate) fn clipboard_seq_changed_now(&self, now_seq: Option<u32>) -> bool {
        match (self.clip_seq, now_seq) {
            (Some(saved), Some(now)) => saved != now,
            _ => false,
        }
    }

    // A native delayed owner identifies the source even if another program later
    // copies identical plain text or publishes an Office-only clipboard.
    pub(super) fn deferred_clip_current(&self) -> Option<bool> {
        self.clip.as_ref()?.deferred.as_ref()?;
        self.clipboard_owner.as_ref().map(|owner| owner.is_current())
    }

    pub(super) fn system_paste_text(&self) -> Option<String> {
        let seq_now = current_os_clipboard_seq();
        let unchanged = self.deferred_clip_current() == Some(true)
            || (self.clip.is_some()
                && self.clip_stamp.is_some()
                && self.clip_seq.is_some_and(|saved| seq_now == Some(saved)));
        (!unchanged).then(Self::read_clipboard_text).flatten()
    }

    pub(super) fn internal_clip_for(&self, text: Option<&str>, trust_unstamped: bool) -> Option<&Clip> {
        let clip = self.clip.as_ref()?;
        if self.clipboard_seq_changed_now(current_os_clipboard_seq()) {
            return None;
        }
        let current = self.deferred_clip_current().unwrap_or_else(|| {
            text.is_none_or(|text| match &self.clip_stamp {
                Some(stamp) => copied_text_matches(stamp, text),
                None => trust_unstamped || self.clip_matches(text, clip),
            })
        });
        current.then_some(clip)
    }

    fn clip_matches(&self, text: &str, clip: &Clip) -> bool {
        if let Some(range) = &clip.deferred { return copied_text_matches(&range.text(), text); }
        let mut expect = String::new();
        for row in clip.cells().iter() {
            let mut line = Vec::new();
            for (cell, st) in row.iter() {
                let style = self.wb.styles.get(*st);
                line.push(sheetcore::style::format_value_in(
                    self.wb.date_system,
                    &cell.value,
                    &style.format,
                ));
            }
            expect.push_str(&line.join("\t"));
            expect.push('\n');
        }
        expect == text
    }

    /// 系统剪贴板读取集中加锁：并行测试/多个 arboard 实例在 Windows 上会互相踩。
    fn read_clipboard_text() -> Option<String> {
        static IO: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = IO.lock().ok()?;
        let text = read_system_clip().text;
        (!text.is_empty()).then_some(text)
    }

    pub(super) fn clear_internal_clip(&mut self) {
        self.clip = None;
        self.clip_notes.clear();
        self.clip_threads.clear();
        self.clip_notes_captured = false;
    }

    pub(crate) fn has_pasteable_clip(&self) -> bool {
        self.clip.is_some()
            || self.clip_pic.is_some()
            || crate::office_clipboard::has_pasteable()
    }
}
