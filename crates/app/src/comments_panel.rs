//! 批注管理侧边栏、财务常用语模板、批量操作与「批注汇总」工作表导出（第二期 P2/P3）。

use crate::{SheetApp, cell_name, ui_icons::Icon, ui_kit};
use eframe::egui::{self, Color32, Rect, Sense, Stroke, Vec2};
use sheetcore::model::{Cell, CellRef, Sheet, Value};
use sheetcore::notes::{Note, ThreadEntry, ThreadedComment, new_ooxml_guid};
use std::path::PathBuf;

/// 内置高频财务审阅常用语（一键插入批注，支持用户扩展自定义常用语）
pub(crate) const BUILTIN_COMMENT_PHRASES: &[&str] = &[
    "已核对无误",
    "待补充发票/凭证",
    "金额差异待核查",
    "跨期调整待确认",
    "请业务确认口径",
    "已回款核销",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum CommentStatusFilter {
    #[default]
    All,
    Open,
    Resolved,
    Threaded,
    Legacy,
}

impl CommentStatusFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Open => "未解决",
            Self::Resolved => "已解决",
            Self::Threaded => "仅线程",
            Self::Legacy => "仅传统",
        }
    }
}

/// 批注侧边栏与常用语模板状态
pub(crate) struct CommentsPanelState {
    pub(crate) open: bool,
    pub(crate) scope_all_sheets: bool,
    pub(crate) query: String,
    pub(crate) status_filter: CommentStatusFilter,
    pub(crate) author_filter: String,
    pub(crate) reply_target: Option<(usize, u32, u32)>,
    pub(crate) reply_draft: String,
    pub(crate) confirm_batch_delete: bool,
    pub(crate) custom_phrases: Vec<String>,
    pub(crate) phrases_loaded: bool,
    pub(crate) phrases_manage_open: bool,
    pub(crate) new_phrase_buf: String,
}

impl Default for CommentsPanelState {
    fn default() -> Self {
        Self {
            open: false,
            scope_all_sheets: false,
            query: String::new(),
            status_filter: CommentStatusFilter::All,
            author_filter: String::new(),
            reply_target: None,
            reply_draft: String::new(),
            confirm_batch_delete: false,
            custom_phrases: Vec::new(),
            phrases_loaded: false,
            phrases_manage_open: false,
            new_phrase_buf: String::new(),
        }
    }
}

/// 批注列表中的单条结构化视图（统一传统批注与 Excel 365 线程批注）
#[derive(Clone, Debug)]
pub(crate) struct CommentListItem {
    pub(crate) sheet_idx: usize,
    pub(crate) sheet_name: String,
    pub(crate) row: u32,
    pub(crate) col: u32,
    pub(crate) cell_addr: String,
    pub(crate) cell_value: String,
    pub(crate) is_thread: bool,
    pub(crate) resolved: bool,
    pub(crate) author: String,
    pub(crate) text: String,
    pub(crate) reply_count: usize,
    pub(crate) entries: Vec<ThreadEntry>,
}

/// ISO-8601 UTC 时间戳（用于线程回复与批注元数据）
pub(crate) fn iso_utc_now() -> String {
    let ts_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_secs_utc(ts_secs, true)
}

/// 面向普通用户的可读批注时间戳前缀（使用东八区 UTC+8 显示，格式 `[YYYY-MM-DD HH:MM 作者] `）
pub(crate) fn format_comment_timestamp(author: &str) -> String {
    let ts_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_add(8 * 3600);
    let stamp = format_unix_secs_utc(ts_secs, false);
    let who = author.trim();
    if who.is_empty() {
        format!("[{stamp}] ")
    } else {
        format!("[{stamp} {who}] ")
    }
}

fn format_unix_secs_utc(ts_secs: u64, iso_with_seconds: bool) -> String {
    let days = (ts_secs / 86_400) as i64;
    let rem = ts_secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    if iso_with_seconds {
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    } else {
        format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem % 3600) / 60)
    }
}

fn custom_phrases_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)?;
    Some(base.join("CELLAN").join("comment_phrases.json"))
}

impl CommentsPanelState {
    pub(crate) fn ensure_phrases_loaded(&mut self) {
        if self.phrases_loaded {
            return;
        }
        self.phrases_loaded = true;
        let Some(path) = custom_phrases_path() else {
            return;
        };
        if let Ok(raw) = std::fs::read_to_string(&path)
            && let Ok(list) = serde_json::from_str::<Vec<String>>(&raw)
        {
            self.custom_phrases = list
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .take(40)
                .collect();
        }
    }

    pub(crate) fn save_custom_phrases(&self) {
        let Some(path) = custom_phrases_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(data) = serde_json::to_string_pretty(&self.custom_phrases) {
            let _ = std::fs::write(&path, data);
        }
    }

    pub(crate) fn add_custom_phrase(&mut self, phrase: &str) -> bool {
        self.ensure_phrases_loaded();
        let clean = phrase.trim();
        if clean.is_empty() {
            return false;
        }
        if BUILTIN_COMMENT_PHRASES.contains(&clean)
            || self.custom_phrases.iter().any(|p| p == clean)
        {
            return false;
        }
        self.custom_phrases.push(clean.to_string());
        if self.custom_phrases.len() > 40 {
            self.custom_phrases.truncate(40);
        }
        self.save_custom_phrases();
        true
    }

    pub(crate) fn remove_custom_phrase(&mut self, idx: usize) -> bool {
        self.ensure_phrases_loaded();
        if idx >= self.custom_phrases.len() {
            return false;
        }
        self.custom_phrases.remove(idx);
        self.save_custom_phrases();
        true
    }
}

/// 在批注文本末尾智能拼接短语/时间戳（避免黏连，保留已有换行）
pub(crate) fn append_comment_snippet(buf: &mut String, snippet: &str) {
    if snippet.is_empty() {
        return;
    }
    if !buf.is_empty() && !buf.ends_with('\n') && !buf.ends_with(' ') {
        if snippet.starts_with('[') {
            buf.push('\n');
        } else {
            buf.push(' ');
        }
    }
    buf.push_str(snippet);
}

fn quote_sheet_ref(name: &str) -> String {
    let needs_quote = name.is_empty()
        || name.chars().any(|ch| {
            !ch.is_alphanumeric() && ch != '_' && !('\u{4E00}'..='\u{9FFF}').contains(&ch)
        });
    if needs_quote {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        name.to_string()
    }
}

impl SheetApp {
    pub(crate) fn comment_author_default(&self) -> String {
        if self.my_name.trim().is_empty() {
            "CELLAN".to_string()
        } else {
            self.my_name.trim().to_string()
        }
    }

    pub(crate) fn toggle_comments_panel(&mut self) {
        self.comments_panel.open = !self.comments_panel.open;
        self.comments_panel.confirm_batch_delete = false;
        if self.comments_panel.open {
            self.comments_panel.ensure_phrases_loaded();
            self.status = "已打开批注管理面板".into();
        } else {
            self.status = "已关闭批注管理面板".into();
        }
    }

    /// 汇总当前工作表或整个工作簿的全部批注与线程会话（按 sheet, row, col 有序去重）
    pub(crate) fn collect_comment_items(&self, scope_all_sheets: bool) -> Vec<CommentListItem> {
        let mut items = Vec::new();
        let sheet_range: Vec<usize> = if scope_all_sheets {
            (0..self.wb.sheets.len()).collect()
        } else {
            vec![self.sheet_idx]
        };
        for si in sheet_range {
            let Some(sh) = self.wb.sheets.get(si) else {
                continue;
            };
            let keys = self.sheet_comment_keys(si);
            for (r, c) in keys {
                let key = CellRef::new(r, c);
                let cell_value = sh
                    .get(r, c)
                    .filter(|cell| !cell.is_blank())
                    .map(|cell| {
                        let st = self.wb.styles.get(sh.style_of(r, c));
                        sheetcore::style::format_value_in(
                            self.wb.date_system,
                            &cell.value,
                            &st.format,
                        )
                    })
                    .unwrap_or_default();
                let cell_addr = cell_name(r, c);
                if let Some(tc) = sh.threads.get(&key) {
                    items.push(CommentListItem {
                        sheet_idx: si,
                        sheet_name: sh.name.clone(),
                        row: r,
                        col: c,
                        cell_addr,
                        cell_value,
                        is_thread: true,
                        resolved: tc.resolved,
                        author: tc.root_author().to_string(),
                        text: tc.summary_text(),
                        reply_count: tc.entries.len(),
                        entries: tc.entries.clone(),
                    });
                } else if let Some(n) = sh.notes.get(&key) {
                    items.push(CommentListItem {
                        sheet_idx: si,
                        sheet_name: sh.name.clone(),
                        row: r,
                        col: c,
                        cell_addr,
                        cell_value,
                        is_thread: false,
                        resolved: false,
                        author: n.author.clone(),
                        text: n.text.clone(),
                        reply_count: 0,
                        entries: Vec::new(),
                    });
                }
            }
        }
        items
    }

    pub(crate) fn validate_thread_limits(
        total_entries: usize,
        texts: &[&str],
        authors: &[&str],
    ) -> Result<(), String> {
        if total_entries > sheetcore::sync::MAX_THREAD_ENTRIES {
            return Err(format!(
                "单格批注线程最多支持 {} 条发言，无法继续添加",
                sheetcore::sync::MAX_THREAD_ENTRIES
            ));
        }
        for &t in texts {
            if t.len() > sheetcore::sync::MAX_THREAD_ENTRY_TEXT_BYTES {
                return Err("单条批注内容超过 64 KB 上限，无法保存".into());
            }
        }
        for &a in authors {
            if a.len() > sheetcore::sync::MAX_SHORT_TEXT {
                return Err("批注作者名称过长，无法保存".into());
            }
        }
        Ok(())
    }

    pub(super) fn thread_change_allowed(&mut self, needed: bool) -> bool {
        if needed && self.net.as_ref().is_some_and(|net| !net.supports_thread_changes()) {
            self.status = "此操作会更改批注会话，请先升级协作宿主再重试".into();
            return false;
        }
        true
    }

    fn thread_edit_fits(&mut self, before: Option<&ThreadedComment>, after: Option<&ThreadedComment>) -> bool {
        if self.net.is_some() && !sheetcore::sync::thread_snapshots_fit(before.into_iter().chain(after)) {
            self.status = "批注会话过长，本次更改超出协作同步上限".into();
            return false;
        }
        true
    }

    pub(super) fn thread_rects_allowed(&mut self, sheet: usize, rects: &[(u32, u32, u32, u32)]) -> bool {
        if self.net.as_ref().is_none_or(|net| net.supports_thread_changes()) { return true; }
        let sh = self.wb.sheet(sheet);
        let touched = sh.threads.keys().any(|key| {
            let (top, left, bottom, right) = sh.merge_at(key.row, key.col)
                .unwrap_or((key.row, key.col, key.row, key.col));
            rects.iter().any(|&(r1, c1, r2, c2)| top <= r2 && bottom >= r1 && left <= c2 && right >= c1)
        });
        self.thread_change_allowed(touched)
    }

    pub(super) fn broadcast_thread_change(
        &self, sheet: usize, row: u32, col: u32, before: Option<&ThreadedComment>,
        after: Option<&ThreadedComment>, before_note: Option<&Note>, legacy_note: Option<&Note>,
    ) -> bool {
        self.net.as_ref().is_none_or(|net| net.broadcast_sheet_meta(sheet, sheetcore::sync::SheetMetaKind::ThreadChange {
            row, col, before: before.cloned(), after: after.cloned(), before_note: before.is_none().then(|| before_note.cloned()).flatten(), legacy_note: legacy_note.cloned(),
        }))
    }

    pub(super) fn broadcast_comment_diffs(
        &self, sheet: usize, notes: &[((u32, u32), Option<Note>, Option<Note>)],
        threads: &[((u32, u32), Option<ThreadedComment>, Option<ThreadedComment>)],
    ) {
        if self.net.is_none() { return; }
        let note_targets: std::collections::HashMap<_, _> = notes.iter().map(|(pos, b, a)| (*pos, (b.as_ref(), a.as_ref()))).collect();
        let thread_cells: std::collections::HashSet<_> = threads.iter().map(|(pos, _, _)| *pos).collect();
        for &(pos, _, ref note) in notes {
            if !thread_cells.contains(&pos) { self.broadcast_note_meta(sheet, pos.0, pos.1, note.as_ref()); }
        }
        for &(pos, ref before, ref after) in threads {
            let (old, new) = note_targets.get(&pos).copied().unwrap_or_default();
            let legacy = if after.is_none() { new } else { None };
            self.broadcast_thread_change(sheet, pos.0, pos.1, before.as_ref(), after.as_ref(), old, legacy);
        }
    }

    /// 写入/更新单元格批注（统一撤销 + 协作广播；供侧边栏、查找替换与 AI 工具复用）
    pub(crate) fn set_comment_cell(
        &mut self,
        sheet: usize,
        r: u32,
        c: u32,
        text: &str,
        author_override: Option<&str>,
        as_thread: bool,
    ) -> Result<bool, String> {
        if sheet >= self.wb.sheets.len() {
            return Err("目标工作表不存在".into());
        }
        let (r, c) = self
            .wb
            .sheet(sheet)
            .merge_at(r, c)
            .map(|(mr1, mc1, _, _)| (mr1, mc1))
            .unwrap_or((r, c));
        if !self.write_preflight_on(sheet, &[(r, c)], "设置批注") {
            return Err(self.status.clone());
        }
        let cleaned = text.trim_end_matches(&['\r', '\n'][..]).to_string();
        if cleaned.trim().is_empty() {
            return Ok(self.batch_delete_comments(&[(sheet, r, c)], "删除批注") > 0);
        }
        let key = CellRef::new(r, c);
        let old_note = self.wb.sheet(sheet).notes.get(&key).cloned();
        let old_thread = self.wb.sheet(sheet).threads.get(&key).cloned();
        let author = author_override
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .or_else(|| {
                old_note
                    .as_ref()
                    .map(|n| n.author.trim().to_string())
                    .filter(|a| !a.is_empty())
            })
            .or_else(|| {
                old_thread
                    .as_ref()
                    .map(|tc| tc.root_author().trim().to_string())
                    .filter(|a| !a.is_empty())
            })
            .unwrap_or_else(|| self.comment_author_default());

        let new_thread = if let Some(mut tc) = old_thread.clone() {
            if let Some(first) = tc.entries.first_mut() {
                first.text = cleaned.clone();
            } else {
                tc.entries.push(ThreadEntry {
                    id: new_ooxml_guid(),
                    author: author.clone(),
                    timestamp: iso_utc_now(),
                    text: cleaned.clone(),
                });
            }
            Some(tc)
        } else if as_thread {
            Some(ThreadedComment {
                r,
                c,
                resolved: false,
                entries: vec![ThreadEntry {
                    id: new_ooxml_guid(),
                    author: author.clone(),
                    timestamp: iso_utc_now(),
                    text: cleaned.clone(),
                }],
            })
        } else {
            None
        };

        let projected_entries = new_thread.as_ref().map_or(0, |tc| tc.entries.len());
        if let Err(err) =
            Self::validate_thread_limits(projected_entries, &[cleaned.as_str()], &[author.as_str()])
        {
            self.status = err.clone();
            return Err(err);
        }

        let effective_text = new_thread
            .as_ref()
            .map(|tc| tc.summary_text())
            .unwrap_or_else(|| cleaned.clone());
        let new_note = Note {
            r,
            c,
            text: effective_text,
            author,
        };
        let changed_note = old_note.as_ref() != Some(&new_note);
        let changed_thread = old_thread != new_thread;
        if changed_thread && (!self.thread_change_allowed(true) || !self.thread_edit_fits(old_thread.as_ref(), new_thread.as_ref())) { return Err(self.status.clone()); }
        if !changed_note && !changed_thread {
            return Ok(false);
        }
        {
            let sh = self.wb.sheet_mut(sheet);
            sh.notes.insert(key, new_note.clone());
            sh.notes_dirty = true;
            if let Some(ref tc) = new_thread {
                sh.threads.insert(key, tc.clone());
                sh.threads_dirty = true;
            }
            sh.bump_display_style_revision();
        }
        let synced = if changed_thread {
            self.broadcast_thread_change(sheet, r, c, old_thread.as_ref(), new_thread.as_ref(), old_note.as_ref(), None)
        } else {
            self.broadcast_note_meta(sheet, r, c, Some(&new_note))
        };
        if !synced {
            let sh = self.wb.sheet_mut(sheet);
            match old_note {
                Some(n) => {
                    sh.notes.insert(key, n);
                }
                None => {
                    sh.notes.remove(&key);
                }
            }
            match old_thread {
                Some(tc) => {
                    sh.threads.insert(key, tc);
                }
                None => {
                    sh.threads.remove(&key);
                }
            }
            sh.bump_display_style_revision();
            let err = format!("{} 的批注未能同步到协作成员，已撤回本地修改", cell_name(r, c));
            self.status = err.clone();
            return Err(err);
        }
        self.commit_meta_tx(
            if old_note.is_some() || old_thread.is_some() {
                "编辑批注"
            } else {
                "新建批注"
            },
            (sheet, r, c),
            sheetcore::undo::MetaEdit {
                sheet,
                notes: vec![((r, c), old_note, Some(new_note))],
                threads: if changed_thread {
                    vec![((r, c), old_thread, new_thread)]
                } else {
                    Vec::new()
                },
                ..Default::default()
            },
            sheetcore::tx::RecalcScope::None,
            false,
            Vec::new(),
        );
        self.status = format!("已保存 {} 的批注", cell_name(r, c));
        Ok(true)
    }

    /// 向指定单元格追加线程回复（若原先为传统批注则平滑升级为线程会话）
    pub(crate) fn reply_comment_cell(
        &mut self,
        sheet: usize,
        r: u32,
        c: u32,
        reply_text: &str,
        author_override: Option<&str>,
    ) -> Result<usize, String> {
        if sheet >= self.wb.sheets.len() {
            return Err("目标工作表不存在".into());
        }
        let reply_clean = reply_text.trim();
        if reply_clean.is_empty() {
            return Err("回复内容不能为空".into());
        }
        let (r, c) = self
            .wb
            .sheet(sheet)
            .merge_at(r, c)
            .map(|(mr1, mc1, _, _)| (mr1, mc1))
            .unwrap_or((r, c));
        if !self.write_preflight_on(sheet, &[(r, c)], "回复批注") {
            return Err(self.status.clone());
        }
        let key = CellRef::new(r, c);
        let old_note = self.wb.sheet(sheet).notes.get(&key).cloned();
        let old_thread = self.wb.sheet(sheet).threads.get(&key).cloned();
        let author = author_override
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.comment_author_default());
        let ts = iso_utc_now();

        let seed_root = if old_thread.is_none() {
            old_note.as_ref().and_then(|on| {
                (!on.text.trim().is_empty()).then(|| ThreadEntry {
                    id: new_ooxml_guid(),
                    author: if on.author.trim().is_empty() {
                        author.clone()
                    } else {
                        on.author.trim().to_string()
                    },
                    timestamp: ts.clone(),
                    text: on.text.clone(),
                })
            })
        } else {
            None
        };
        let projected_len = old_thread
            .as_ref()
            .map(|tc| tc.entries.len())
            .unwrap_or_else(|| usize::from(seed_root.is_some()))
            + 1;
        let mut check_texts: Vec<&str> = vec![reply_clean];
        let mut check_authors: Vec<&str> = vec![author.as_str()];
        if let Some(ref root) = seed_root {
            check_texts.push(root.text.as_str());
            check_authors.push(root.author.as_str());
        }
        if let Err(err) =
            Self::validate_thread_limits(projected_len, &check_texts, &check_authors)
        {
            self.status = err.clone();
            return Err(err);
        }

        let new_entry = ThreadEntry {
            id: new_ooxml_guid(),
            author,
            timestamp: ts,
            text: reply_clean.to_string(),
        };

        let (new_note, new_tc) = {
            let sh = self.wb.sheet_mut(sheet);
            let tc = sh.threads.entry(key).or_insert_with(|| {
                let mut entries = Vec::new();
                if let Some(ref root) = seed_root {
                    entries.push(root.clone());
                }
                ThreadedComment {
                    r,
                    c,
                    resolved: false,
                    entries,
                }
            });
            tc.entries.push(new_entry.clone());
            let new_tc = tc.clone();
            sh.threads_dirty = true;
            let n = Note {
                r,
                c,
                text: new_tc.summary_text(),
                author: new_tc.root_author().to_string(),
            };
            sh.notes.insert(key, n.clone());
            sh.notes_dirty = true;
            sh.bump_display_style_revision();
            (n, new_tc)
        };
        if !self.broadcast_thread_reply_meta(sheet, r, c, seed_root, new_entry) {
            let sh = self.wb.sheet_mut(sheet);
            match old_note {
                Some(n) => {
                    sh.notes.insert(key, n);
                }
                None => {
                    sh.notes.remove(&key);
                }
            }
            match old_thread {
                Some(tc) => {
                    sh.threads.insert(key, tc);
                }
                None => {
                    sh.threads.remove(&key);
                }
            }
            sh.bump_display_style_revision();
            let err = format!(
                "{} 的线程回复未能同步（可能已达 {} 条上限或连接已断开），已撤回本地修改",
                cell_name(r, c),
                sheetcore::sync::MAX_THREAD_ENTRIES
            );
            self.status = err.clone();
            return Err(err);
        }
        let count = new_tc.entries.len();
        self.commit_meta_tx(
            "回复批注线程",
            (sheet, r, c),
            sheetcore::undo::MetaEdit {
                sheet,
                notes: vec![((r, c), old_note, Some(new_note))],
                threads: vec![((r, c), old_thread, Some(new_tc))],
                ..Default::default()
            },
            sheetcore::tx::RecalcScope::None,
            false,
            Vec::new(),
        );
        self.status = format!("已在 {} 添加线程回复", cell_name(r, c));
        Ok(count)
    }

    /// 设置单个批注线程的已解决/未解决状态（若为传统批注则先升级为单条线程后标记状态）
    pub(crate) fn set_comment_resolved(
        &mut self,
        sheet: usize,
        r: u32,
        c: u32,
        resolved: bool,
    ) -> Result<bool, String> {
        if sheet >= self.wb.sheets.len() {
            return Err("目标工作表不存在".into());
        }
        let (r, c) = self
            .wb
            .sheet(sheet)
            .merge_at(r, c)
            .map(|(mr1, mc1, _, _)| (mr1, mc1))
            .unwrap_or((r, c));
        if !self.write_preflight_on(sheet, &[(r, c)], "更新批注状态") {
            return Err(self.status.clone());
        }
        let key = CellRef::new(r, c);
        let old_note = self.wb.sheet(sheet).notes.get(&key).cloned();
        let old_thread = self.wb.sheet(sheet).threads.get(&key).cloned();
        if old_thread.is_none() && old_note.is_none() {
            return Ok(false);
        }
        if let Some(ref tc) = old_thread
            && tc.resolved == resolved
        {
            return Ok(false);
        }
        let default_author = self.comment_author_default();
        let seed_root = if old_thread.is_none() {
            old_note.as_ref().map(|n| ThreadEntry {
                id: new_ooxml_guid(),
                author: if n.author.trim().is_empty() {
                    default_author.clone()
                } else {
                    n.author.trim().to_string()
                },
                timestamp: iso_utc_now(),
                text: n.text.clone(),
            })
        } else {
            None
        };
        if let Some(ref root) = seed_root {
            if let Err(err) =
                Self::validate_thread_limits(1, &[root.text.as_str()], &[root.author.as_str()])
            {
                self.status = err.clone();
                return Err(err);
            }
        }
        let (new_note, new_tc) = {
            let sh = self.wb.sheet_mut(sheet);
            let tc = sh.threads.entry(key).or_insert_with(|| ThreadedComment {
                r,
                c,
                resolved,
                entries: seed_root.clone().into_iter().collect(),
            });
            tc.resolved = resolved;
            let new_tc = tc.clone();
            sh.threads_dirty = true;
            let n = Note {
                r,
                c,
                text: new_tc.summary_text(),
                author: new_tc.root_author().to_string(),
            };
            sh.notes.insert(key, n.clone());
            sh.notes_dirty = true;
            sh.bump_display_style_revision();
            (n, new_tc)
        };
        if !self.broadcast_thread_resolve_meta(sheet, r, c, seed_root, resolved) {
            let sh = self.wb.sheet_mut(sheet);
            match old_note {
                Some(n) => {
                    sh.notes.insert(key, n);
                }
                None => {
                    sh.notes.remove(&key);
                }
            }
            match old_thread {
                Some(tc) => {
                    sh.threads.insert(key, tc);
                }
                None => {
                    sh.threads.remove(&key);
                }
            }
            sh.bump_display_style_revision();
            let err = format!("{} 的批注状态未能同步到协作成员，已撤回本地修改", cell_name(r, c));
            self.status = err.clone();
            return Err(err);
        }
        self.commit_meta_tx(
            if resolved {
                "标记批注已解决"
            } else {
                "重新打开批注"
            },
            (sheet, r, c),
            sheetcore::undo::MetaEdit {
                sheet,
                notes: vec![((r, c), old_note, Some(new_note))],
                threads: vec![((r, c), old_thread, Some(new_tc))],
                ..Default::default()
            },
            sheetcore::tx::RecalcScope::None,
            false,
            Vec::new(),
        );
        self.status = format!(
            "已将 {} 批注标记为{}",
            cell_name(r, c),
            if resolved { "已解决" } else { "未解决" }
        );
        Ok(true)
    }

    /// 批量标记一批批注为已解决/未解决（按工作表合并撤销步，同步广播）
    pub(crate) fn batch_set_comments_resolved(
        &mut self,
        targets: &[(usize, u32, u32)],
        resolved: bool,
    ) -> usize {
        let mut by_sheet: std::collections::BTreeMap<usize, Vec<(u32, u32)>> =
            std::collections::BTreeMap::new();
        for &(si, r, c) in targets {
            if si < self.wb.sheets.len() {
                let (nr, nc) = self
                    .wb
                    .sheet(si)
                    .merge_at(r, c)
                    .map(|(mr1, mc1, _, _)| (mr1, mc1))
                    .unwrap_or((r, c));
                by_sheet.entry(si).or_default().push((nr, nc));
            }
        }
        let default_author = self.comment_author_default();
        let mut total_changed = 0usize;
        for (si, mut coords) in by_sheet {
            coords.sort_unstable();
            coords.dedup();
            if !self.write_preflight_on(si, &coords, "批量更新批注状态") {
                continue;
            }
            let mut note_diffs = Vec::new();
            let mut thread_diffs = Vec::new();
            let mut bcast_resolves: Vec<(u32, u32, Option<ThreadEntry>)> = Vec::new();
            let ts = iso_utc_now();
            {
                let sh = self.wb.sheet_mut(si);
                for &(r, c) in &coords {
                    let key = CellRef::new(r, c);
                    let old_note = sh.notes.get(&key).cloned();
                    let old_thread = sh.threads.get(&key).cloned();
                    if old_note.is_none() && old_thread.is_none() {
                        continue;
                    }
                    if let Some(ref tc) = old_thread
                        && tc.resolved == resolved
                    {
                        continue;
                    }
                    let seed_root = if old_thread.is_none() {
                        let n = old_note.as_ref().unwrap();
                        Some(ThreadEntry {
                            id: new_ooxml_guid(),
                            author: if n.author.trim().is_empty() {
                                default_author.clone()
                            } else {
                                n.author.trim().to_string()
                            },
                            timestamp: ts.clone(),
                            text: n.text.clone(),
                        })
                    } else {
                        None
                    };
                    let tc = sh.threads.entry(key).or_insert_with(|| ThreadedComment {
                        r,
                        c,
                        resolved,
                        entries: seed_root.clone().into_iter().collect(),
                    });
                    tc.resolved = resolved;
                    let new_tc = tc.clone();
                    let new_note = Note {
                        r,
                        c,
                        text: new_tc.summary_text(),
                        author: new_tc.root_author().to_string(),
                    };
                    sh.notes.insert(key, new_note.clone());
                    note_diffs.push(((r, c), old_note, Some(new_note)));
                    thread_diffs.push(((r, c), old_thread, Some(new_tc)));
                    bcast_resolves.push((r, c, seed_root));
                }
                if !thread_diffs.is_empty() {
                    sh.notes_dirty = true;
                    sh.threads_dirty = true;
                    sh.bump_display_style_revision();
                }
            }
            if !thread_diffs.is_empty() {
                total_changed += thread_diffs.len();
                let focus = (si, coords[0].0, coords[0].1);
                for (r, c, seed_root) in bcast_resolves {
                    let _ = self.broadcast_thread_resolve_meta(si, r, c, seed_root, resolved);
                }
                self.commit_meta_tx(
                    if resolved {
                        "批量标记批注已解决"
                    } else {
                        "批量重新打开批注"
                    },
                    focus,
                    sheetcore::undo::MetaEdit {
                        sheet: si,
                        notes: note_diffs,
                        threads: thread_diffs,
                        ..Default::default()
                    },
                    sheetcore::tx::RecalcScope::None,
                    false,
                    Vec::new(),
                );
            }
        }
        if total_changed > 0 {
            self.status = format!(
                "已批量将 {total_changed} 条批注标记为{}",
                if resolved { "已解决" } else { "未解决" }
            );
        }
        total_changed
    }

    /// 批量删除多格批注与线程（按工作表合并为原子撤销步并同步广播）
    pub(crate) fn batch_delete_comments(
        &mut self,
        targets: &[(usize, u32, u32)],
        label: &str,
    ) -> usize {
        let mut by_sheet: std::collections::BTreeMap<usize, Vec<(u32, u32)>> =
            std::collections::BTreeMap::new();
        for &(si, r, c) in targets {
            if si < self.wb.sheets.len() {
                let (nr, nc) = self
                    .wb
                    .sheet(si)
                    .merge_at(r, c)
                    .map(|(mr1, mc1, _, _)| (mr1, mc1))
                    .unwrap_or((r, c));
                by_sheet.entry(si).or_default().push((nr, nc));
            }
        }
        let changes_thread = by_sheet.iter().any(|(&si, coords)| coords.iter().any(|&(r, c)|
            self.wb.sheet(si).threads.contains_key(&CellRef::new(r, c))));
        if !self.thread_change_allowed(changes_thread) { return 0; }
        let mut total_deleted = 0usize;
        for (si, mut coords) in by_sheet {
            coords.sort_unstable();
            coords.dedup();
            if !self.write_preflight_on(si, &coords, label) {
                continue;
            }
            let sid = self.wb.sheet(si).identity;
            let mut note_diffs = Vec::new();
            let mut thread_diffs = Vec::new();
            {
                let sh = self.wb.sheet_mut(si);
                for &(r, c) in &coords {
                    let key = CellRef::new(r, c);
                    let rm_n = sh.notes.remove(&key);
                    let rm_t = sh.threads.remove(&key);
                    if rm_n.is_some() || rm_t.is_some() {
                        total_deleted += 1;
                        self.pinned_comments.remove(&(sid, key));
                        self.comment_card_offsets.remove(&(sid, key));
                        if let Some(old_n) = rm_n {
                            note_diffs.push(((r, c), Some(old_n), None));
                        }
                        if let Some(old_t) = rm_t {
                            thread_diffs.push(((r, c), Some(old_t), None));
                        }
                    }
                }
                if !note_diffs.is_empty() {
                    sh.notes_dirty = true;
                }
                if !thread_diffs.is_empty() {
                    sh.threads_dirty = true;
                }
                if !note_diffs.is_empty() || !thread_diffs.is_empty() {
                    sh.bump_display_style_revision();
                }
            }
            if !note_diffs.is_empty() || !thread_diffs.is_empty() {
                self.broadcast_comment_diffs(si, &note_diffs, &thread_diffs);
                let focus = (si, coords[0].0, coords[0].1);
                self.commit_meta_tx(
                    label,
                    focus,
                    sheetcore::undo::MetaEdit {
                        sheet: si,
                        notes: note_diffs,
                        threads: thread_diffs,
                        ..Default::default()
                    },
                    sheetcore::tx::RecalcScope::None,
                    false,
                    Vec::new(),
                );
            }
        }
        if total_deleted > 0 {
            self.status = format!("已删除 {total_deleted} 条批注");
        }
        total_deleted
    }

    /// 查找/替换中的「批注替换」：更新单格传统批注或线程条目正文（完整撤销 + 协作广播）
    pub(crate) fn replace_comment_text_at(
        &mut self,
        sheet: usize,
        r: u32,
        c: u32,
        needle: &str,
        replace: &str,
        whole_cell: bool,
        match_case: bool,
    ) -> Result<bool, String> {
        if sheet >= self.wb.sheets.len() || needle.is_empty() {
            return Ok(false);
        }
        let key = CellRef::new(r, c);
        let old_note = self.wb.sheet(sheet).notes.get(&key).cloned();
        let old_thread = self.wb.sheet(sheet).threads.get(&key).cloned();
        if old_note.is_none() && old_thread.is_none() {
            return Ok(false);
        }
        let replace_one = |src: &str| -> String {
            if whole_cell {
                let eq = if match_case {
                    src == needle
                } else {
                    src.to_lowercase() == needle.to_lowercase()
                };
                if eq {
                    replace.to_string()
                } else {
                    src.to_string()
                }
            } else {
                sheetcore::search::replace_in_text(src, needle, replace, match_case)
            }
        };

        let mut new_thread = old_thread.clone();
        let mut thread_changed = false;
        if let Some(ref mut tc) = new_thread {
            for e in &mut tc.entries {
                let next = replace_one(&e.text);
                if next != e.text {
                    e.text = next;
                    thread_changed = true;
                }
            }
        }
        let mut new_note = old_note.clone();
        let mut note_changed = false;
        if thread_changed {
            if let Some(ref tc) = new_thread {
                let updated = Note {
                    r,
                    c,
                    text: tc.summary_text(),
                    author: tc.root_author().to_string(),
                };
                if new_note.as_ref() != Some(&updated) {
                    new_note = Some(updated);
                    note_changed = true;
                }
            }
        } else if let Some(ref mut n) = new_note {
            let next = replace_one(&n.text);
            if next != n.text {
                n.text = next;
                note_changed = true;
            }
        }

        if thread_changed && (!self.thread_change_allowed(true) || !self.thread_edit_fits(old_thread.as_ref(), new_thread.as_ref())) { return Err(self.status.clone()); }
        if !note_changed && !thread_changed {
            return Ok(false);
        }
        if let Some(ref tc) = new_thread {
            let texts: Vec<&str> = tc.entries.iter().map(|e| e.text.as_str()).collect();
            let authors: Vec<&str> = tc.entries.iter().map(|e| e.author.as_str()).collect();
            if let Err(err) = Self::validate_thread_limits(tc.entries.len(), &texts, &authors) {
                self.status = err.clone();
                return Err(err);
            }
        } else if let Some(ref n) = new_note {
            if let Err(err) = Self::validate_thread_limits(0, &[n.text.as_str()], &[n.author.as_str()]) {
                self.status = err.clone();
                return Err(err);
            }
        }
        if !self.write_preflight_on(sheet, &[(r, c)], "替换批注") {
            return Err(self.status.clone());
        }
        {
            let sh = self.wb.sheet_mut(sheet);
            if let Some(ref n) = new_note
                && note_changed
            {
                sh.notes.insert(key, n.clone());
                sh.notes_dirty = true;
            }
            if let Some(ref tc) = new_thread
                && thread_changed
            {
                sh.threads.insert(key, tc.clone());
                sh.threads_dirty = true;
            }
            sh.bump_display_style_revision();
        }
        let synced = if thread_changed {
            self.broadcast_thread_change(sheet, r, c, old_thread.as_ref(), new_thread.as_ref(), old_note.as_ref(), None)
        } else if note_changed {
            self.broadcast_note_meta(sheet, r, c, new_note.as_ref())
        } else {
            true
        };
        if !synced {
            let sh = self.wb.sheet_mut(sheet);
            match old_note {
                Some(n) => {
                    sh.notes.insert(key, n);
                }
                None => {
                    sh.notes.remove(&key);
                }
            }
            match old_thread {
                Some(tc) => {
                    sh.threads.insert(key, tc);
                }
                None => {
                    sh.threads.remove(&key);
                }
            }
            sh.bump_display_style_revision();
            let err = format!("{} 的批注替换未能同步到协作成员，已撤回本地修改", cell_name(r, c));
            self.status = err.clone();
            return Err(err);
        }
        self.commit_meta_tx(
            "替换批注",
            (sheet, r, c),
            sheetcore::undo::MetaEdit {
                sheet,
                notes: if note_changed {
                    vec![((r, c), old_note, new_note)]
                } else {
                    Vec::new()
                },
                threads: if thread_changed {
                    vec![((r, c), old_thread, new_thread)]
                } else {
                    Vec::new()
                },
                ..Default::default()
            },
            sheetcore::tx::RecalcScope::None,
            false,
            Vec::new(),
        );
        Ok(true)
    }

    /// 批量替换范围内全部命中的批注正文（支持传统批注与线程回复，单步可撤销 + 协作广播）
    pub(crate) fn replace_all_comments(&mut self) -> usize {
        let needle = self.find.find.clone();
        if needle.is_empty() {
            return 0;
        }
        let replace = self.find.replace.clone();
        let whole_cell = self.find.whole_cell;
        let match_case = self.find.match_case;
        let hits = self.find_hits();
        if hits.is_empty() {
            self.status = format!("未找到包含「{needle}」的批注");
            return 0;
        }
        let mut by_sheet: std::collections::BTreeMap<usize, Vec<(u32, u32)>> =
            std::collections::BTreeMap::new();
        for h in hits {
            by_sheet.entry(h.sheet).or_default().push((h.row, h.col));
        }
        let replace_one = |src: &str| -> String {
            if whole_cell {
                let eq = if match_case {
                    src == needle
                } else {
                    src.to_lowercase() == needle.to_lowercase()
                };
                if eq {
                    replace.clone()
                } else {
                    src.to_string()
                }
            } else {
                sheetcore::search::replace_in_text(src, &needle, &replace, match_case)
            }
        };

        let changes_thread = by_sheet.iter().any(|(&si, coords)| coords.iter().any(|&(r, c)|
            self.wb.sheet(si).threads.get(&CellRef::new(r, c)).is_some_and(|tc|
                tc.entries.iter().any(|entry| replace_one(&entry.text) != entry.text))));
        if !self.thread_change_allowed(changes_thread) { return 0; }
        let mut total = 0usize;
        for (si, mut coords) in by_sheet {
            coords.sort_unstable();
            coords.dedup();
            if !self.write_preflight_on(si, &coords, "全部替换批注") {
                continue;
            }
            let mut note_diffs = Vec::new();
            let mut thread_diffs = Vec::new();

            {
                let sh = self.wb.sheet_mut(si);
                for &(r, c) in &coords {
                    let key = CellRef::new(r, c);
                    let old_note = sh.notes.get(&key).cloned();
                    let old_thread = sh.threads.get(&key).cloned();
                    let mut new_thread = old_thread.clone();
                    let mut tc_changed = false;
                    if let Some(ref mut tc) = new_thread {
                        for e in &mut tc.entries {
                            let next = replace_one(&e.text);
                            if next != e.text {
                                e.text = next;
                                tc_changed = true;
                            }
                        }
                    }
                    let mut new_note = old_note.clone();
                    let mut n_changed = false;
                    if tc_changed {
                        if let Some(ref tc) = new_thread {
                            let updated = Note {
                                r,
                                c,
                                text: tc.summary_text(),
                                author: tc.root_author().to_string(),
                            };
                            if new_note.as_ref() != Some(&updated) {
                                new_note = Some(updated);
                                n_changed = true;
                            }
                        }
                    } else if let Some(ref mut n) = new_note {
                        let next = replace_one(&n.text);
                        if next != n.text {
                            n.text = next;
                            n_changed = true;
                        }
                    }
                    if n_changed || tc_changed {
                        total += 1;
                        if n_changed && let Some(ref n) = new_note {
                            sh.notes.insert(key, n.clone());
                            note_diffs.push(((r, c), old_note, Some(n.clone())));
                        }
                        if tc_changed && let Some(ref tc) = new_thread {
                            sh.threads.insert(key, tc.clone());
                            thread_diffs.push(((r, c), old_thread, Some(tc.clone())));
                        }
                    }
                }
                if !note_diffs.is_empty() {
                    sh.notes_dirty = true;
                }
                if !thread_diffs.is_empty() {
                    sh.threads_dirty = true;
                }
                if !note_diffs.is_empty() || !thread_diffs.is_empty() {
                    sh.bump_display_style_revision();
                }
            }
            if !note_diffs.is_empty() || !thread_diffs.is_empty() {
                self.broadcast_comment_diffs(si, &note_diffs, &thread_diffs);
                let focus = (si, coords[0].0, coords[0].1);
                self.commit_meta_tx(
                    "全部替换批注",
                    focus,
                    sheetcore::undo::MetaEdit {
                        sheet: si,
                        notes: note_diffs,
                        threads: thread_diffs,
                        ..Default::default()
                    },
                    sheetcore::tx::RecalcScope::None,
                    false,
                    Vec::new(),
                );
            }
        }
        if total > 0 {
            self.status = format!("已在 {total} 条批注中完成替换");
        } else {
            self.status = format!("未在批注中找到可替换的「{needle}」");
        }
        total
    }

    /// 一键生成「批注汇总」工作表（带超链接回跳、表头样式、自动筛选与首行冻结，可撤销）
    pub(crate) fn export_comments_summary_sheet(&mut self, scope_all_sheets: bool) -> Option<usize> {
        let items = self.collect_comment_items(scope_all_sheets);
        if items.is_empty() {
            self.status = if scope_all_sheets {
                "当前工作簿没有任何批注可导出".into()
            } else {
                "当前工作表没有任何批注可导出".into()
            };
            return None;
        }
        if !self.can_change_sheets() {
            return None;
        }
        let prev_focus = (self.sheet_idx, self.sel.0, self.sel.1);
        let base_name = "批注汇总";
        let mut sheet_name = base_name.to_string();
        let mut suffix = 2usize;
        while self
            .wb
            .sheets
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(&sheet_name))
        {
            sheet_name = format!("{base_name} ({suffix})");
            suffix += 1;
        }

        let border_thin = sheetcore::style::Border {
            style: 1,
            color: [203, 213, 225],
        };
        let all_borders = [border_thin; 4];
        let mut hdr_st = sheetcore::style::CellStyle::default();
        hdr_st.bold = true;
        hdr_st.fill = Some([30, 58, 138]);
        hdr_st.color = [255, 255, 255];
        hdr_st.halign = 2;
        hdr_st.valign = 2;
        hdr_st.border = all_borders;
        let hdr_sid = self.wb.styles.intern(hdr_st);

        let make_row_style = |zebra: bool, link: bool, wrap: bool, center: bool| {
            let mut st = sheetcore::style::CellStyle::default();
            if zebra {
                st.fill = Some([248, 250, 252]);
            }
            if link {
                st.color = [29, 78, 216];
                st.underline = true;
            }
            st.wrap = wrap;
            st.halign = if center { 2 } else { 1 };
            st.valign = 1;
            st.border = all_borders;
            st
        };
        let body_l = [
            self.wb.styles.intern(make_row_style(false, false, false, false)),
            self.wb.styles.intern(make_row_style(true, false, false, false)),
        ];
        let body_c = [
            self.wb.styles.intern(make_row_style(false, false, false, true)),
            self.wb.styles.intern(make_row_style(true, false, false, true)),
        ];
        let body_link = [
            self.wb.styles.intern(make_row_style(false, true, false, true)),
            self.wb.styles.intern(make_row_style(true, true, false, true)),
        ];
        let body_wrap = [
            self.wb.styles.intern(make_row_style(false, false, true, false)),
            self.wb.styles.intern(make_row_style(true, false, true, false)),
        ];

        let mut sh = Sheet::new(&sheet_name);
        let headers = [
            ("序号", 56.0f32),
            ("工作表", 116.0),
            ("单元格", 76.0),
            ("单元格当前值", 142.0),
            ("类型", 84.0),
            ("状态", 78.0),
            ("作者", 98.0),
            ("回复数", 68.0),
            ("批注 / 讨论内容", 360.0),
        ];
        sh.rows.set(0, 26.0);
        for (ci, (title, width)) in headers.iter().enumerate() {
            let c = ci as u32;
            sh.cols.set(c, *width);
            sh.set(
                0,
                c,
                Cell {
                    value: Value::Text((*title).into()),
                    formula: None,
                },
            );
            sh.set_style(0, c, hdr_sid);
        }

        for (i, item) in items.iter().enumerate() {
            let r = (i + 1) as u32;
            let z = i % 2;
            let kind_str = if item.is_thread { "线程批注" } else { "传统批注" };
            let status_str = if !item.is_thread {
                "-"
            } else if item.resolved {
                "已解决"
            } else {
                "未解决"
            };
            let reply_num = if item.is_thread {
                item.reply_count as f64
            } else {
                1.0
            };
            let cells: [(Value, u16); 9] = [
                (Value::Number((i + 1) as f64), body_c[z]),
                (Value::Text(item.sheet_name.clone().into()), body_l[z]),
                (Value::Text(item.cell_addr.clone().into()), body_link[z]),
                (Value::Text(item.cell_value.clone().into()), body_l[z]),
                (Value::Text(kind_str.into()), body_c[z]),
                (Value::Text(status_str.into()), body_c[z]),
                (Value::Text(item.author.clone().into()), body_l[z]),
                (Value::Number(reply_num), body_c[z]),
                (Value::Text(item.text.clone().into()), body_wrap[z]),
            ];
            for (ci, (val, sid)) in cells.into_iter().enumerate() {
                sh.set(r, ci as u32, Cell { value: val, formula: None });
                sh.set_style(r, ci as u32, sid);
            }
            // 点击「单元格」列可直接跳回原批注格
            let loc = format!("{}!{}", quote_sheet_ref(&item.sheet_name), item.cell_addr);
            sh.hyperlinks.insert(
                (r, 2),
                sheetcore::hyperlinks::Hyperlink {
                    target: String::new(),
                    location: loc,
                    tooltip: format!("跳转到 {}!{}", item.sheet_name, item.cell_addr),
                    external: false,
                    span: (0, 0),
                    attributes: Vec::new(),
                },
            );
            let lines = item.text.lines().count().max(1) as f32;
            let wrap_extra = (item.text.chars().count() as f32 / 26.0).ceil();
            let row_h = (lines.max(wrap_extra) * 18.0 + 8.0).clamp(22.0, 140.0);
            sh.rows.set(r, row_h);
        }
        sh.hyperlinks_dirty = true;
        sh.styles_dirty = true;
        sh.layout_dirty = true;
        sh.freeze = Some((1, 0));
        sh.filter = Some(sheetcore::sort_filter::Filter::new(
            0,
            0,
            items.len() as u32,
            8,
        ));
        sh.filter_dirty = true;

        let index = self.wb.sheets.len();
        if !sheetcore::sheet_ops::insert_sheet(&mut self.wb, index, sh) {
            self.status = "无法创建批注汇总工作表".into();
            return None;
        }
        self.undo.record_appended_sheet(&self.wb, prev_focus);
        self.switch_sheet_view(index);
        self.selected_sheets.clear();
        self.sel = (0, 0);
        self.sel_end = (0, 0);
        self.refresh_after_sheet_change();
        self.status = format!("已生成「{sheet_name}」（共 {} 条批注，点击单元格列可回跳）", items.len());
        Some(index)
    }

    /// 右侧「批注管理」悬浮卡片侧边栏（搜索、按状态/作者筛选、点击跳转、行内快速回复、批量解决/删除、导出汇总表）
    pub(crate) fn draw_comments_panel(&mut self, ctx: &egui::Context) {
        if !self.comments_panel.open {
            return;
        }
        self.comments_panel.ensure_phrases_loaded();
        let all_items = self.collect_comment_items(self.comments_panel.scope_all_sheets);
        let total_count = all_items.len();
        let open_threads = all_items.iter().filter(|i| i.is_thread && !i.resolved).count();
        let resolved_threads = all_items.iter().filter(|i| i.is_thread && i.resolved).count();

        let mut authors: Vec<String> = all_items
            .iter()
            .map(|i| i.author.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();
        authors.sort();
        authors.dedup();
        if !self.comments_panel.author_filter.is_empty()
            && !authors.iter().any(|a| a == &self.comments_panel.author_filter)
        {
            self.comments_panel.author_filter.clear();
        }

        let q_low = self.comments_panel.query.trim().to_lowercase();
        let filtered: Vec<CommentListItem> = all_items
            .into_iter()
            .filter(|item| {
                match self.comments_panel.status_filter {
                    CommentStatusFilter::All => {}
                    CommentStatusFilter::Open => {
                        if item.is_thread && item.resolved {
                            return false;
                        }
                    }
                    CommentStatusFilter::Resolved => {
                        if !item.is_thread || !item.resolved {
                            return false;
                        }
                    }
                    CommentStatusFilter::Threaded => {
                        if !item.is_thread {
                            return false;
                        }
                    }
                    CommentStatusFilter::Legacy => {
                        if item.is_thread {
                            return false;
                        }
                    }
                }
                if !self.comments_panel.author_filter.is_empty()
                    && item.author.trim() != self.comments_panel.author_filter
                {
                    return false;
                }
                if !q_low.is_empty() {
                    let hit = item.cell_addr.to_lowercase().contains(&q_low)
                        || item.sheet_name.to_lowercase().contains(&q_low)
                        || item.author.to_lowercase().contains(&q_low)
                        || item.text.to_lowercase().contains(&q_low)
                        || item.entries.iter().any(|e| {
                            e.text.to_lowercase().contains(&q_low)
                                || e.author.to_lowercase().contains(&q_low)
                        });
                    if !hit {
                        return false;
                    }
                }
                true
            })
            .collect();

        enum PanelAction {
            Close,
            Jump(usize, u32, u32),
            Edit(usize, u32, u32),
            TogglePin(usize, u32, u32),
            SetResolved(usize, u32, u32, bool),
            DeleteOne(usize, u32, u32),
            CopyText(String, String),
            SendReply(usize, u32, u32, String),
            NewAtSelection,
            ExportSummary(bool),
            BatchResolve(Vec<(usize, u32, u32)>, bool),
            BatchDelete(Vec<(usize, u32, u32)>),
        }

        let mut actions: Vec<PanelAction> = Vec::new();
        let edit_blocked = self.is_edit_blocked();
        let cur_sheet = self.sheet_idx;
        let cur_sel = self.sel;
        let pal = crate::theme::palette();

        egui::SidePanel::right("comments_sidebar_panel")
            .resizable(true)
            .show_separator_line(false)
            .default_width(344.0)
            .width_range(288.0..=480.0)
            .frame(
                egui::Frame::NONE
                    .fill(pal.page_bg)
                    .inner_margin(egui::Margin {
                        left: 0,
                        right: 8,
                        top: 4,
                        bottom: 4,
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(pal.card_bg)
                    .stroke(Stroke::new(1.0_f32, pal.card_border))
                    .corner_radius(ui_kit::CARD_R)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 2],
                        blur: 8,
                        spread: 0,
                        color: pal.shadow_color(),
                    })
                    .inner_margin(egui::Margin::same(0))
                    .show(ui, |ui| {
                        let inner_rect = ui.max_rect();
                        ui.set_min_size(inner_rect.size());
                        ui.set_max_size(inner_rect.size());

                        // 顶部标题条
                        egui::Frame::NONE
                            .fill(pal.surface_subtle())
                            .corner_radius(egui::CornerRadius {
                                nw: 8,
                                ne: 8,
                                sw: 0,
                                se: 0,
                            })
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("批注管理 ({total_count})"))
                                            .strong()
                                            .size(14.0)
                                            .color(pal.text),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui_kit::close_icon_button(ui, "关闭批注面板").clicked() {
                                                actions.push(PanelAction::Close);
                                            }
                                            if !edit_blocked
                                                && ui_kit::pill(ui, "+ 新建", "在当前选中格新建/编辑批注 (Shift+F2)").clicked()
                                            {
                                                actions.push(PanelAction::NewAtSelection);
                                            }
                                            if total_count > 0
                                                && ui_kit::pill(ui, "导出汇总", "一键生成带超链接的「批注汇总」工作表").clicked()
                                            {
                                                actions.push(PanelAction::ExportSummary(
                                                    self.comments_panel.scope_all_sheets,
                                                ));
                                            }
                                        },
                                    );
                                });
                            });
                        ui.separator();

                        // 筛选与搜索区
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                // 范围切换 + 作者下拉
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(!self.comments_panel.scope_all_sheets, "当前表")
                                        .clicked()
                                    {
                                        self.comments_panel.scope_all_sheets = false;
                                        self.comments_panel.confirm_batch_delete = false;
                                    }
                                    if ui
                                        .selectable_label(self.comments_panel.scope_all_sheets, "全工作簿")
                                        .clicked()
                                    {
                                        self.comments_panel.scope_all_sheets = true;
                                        self.comments_panel.confirm_batch_delete = false;
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let author_lbl = if self.comments_panel.author_filter.is_empty() {
                                                "全部作者".to_string()
                                            } else {
                                                self.comments_panel.author_filter.clone()
                                            };
                                            egui::ComboBox::from_id_salt("comments_panel_author")
                                                .width(108.0)
                                                .selected_text(author_lbl)
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut self.comments_panel.author_filter,
                                                        String::new(),
                                                        "全部作者",
                                                    );
                                                    for a in &authors {
                                                        ui.selectable_value(
                                                            &mut self.comments_panel.author_filter,
                                                            a.clone(),
                                                            a,
                                                        );
                                                    }
                                                });
                                        },
                                    );
                                });

                                // 搜索框
                                ui.horizontal(|ui| {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if !self.comments_panel.query.is_empty()
                                                && ui
                                                    .small_button("清")
                                                    .on_hover_text("清空搜索")
                                                    .clicked()
                                            {
                                                self.comments_panel.query.clear();
                                            }
                                            ui.add(
                                                egui::TextEdit::singleline(
                                                    &mut self.comments_panel.query,
                                                )
                                                .desired_width(f32::INFINITY)
                                                .hint_text("搜索批注内容、作者或单元格…"),
                                            );
                                        },
                                    );
                                });

                                // 状态过滤芯片
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    for st in [
                                        CommentStatusFilter::All,
                                        CommentStatusFilter::Open,
                                        CommentStatusFilter::Resolved,
                                        CommentStatusFilter::Threaded,
                                        CommentStatusFilter::Legacy,
                                    ] {
                                        let active = self.comments_panel.status_filter == st;
                                        if ui.selectable_label(active, st.label()).clicked() {
                                            self.comments_panel.status_filter = st;
                                            self.comments_panel.confirm_batch_delete = false;
                                        }
                                    }
                                });

                                // 统计与批量操作条
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "显示 {}/{} · 未解决 {} · 已解决 {}",
                                            filtered.len(),
                                            total_count,
                                            open_threads,
                                            resolved_threads
                                        ))
                                        .size(11.0)
                                        .color(pal.text_weak),
                                    );
                                });

                                if !edit_blocked && !filtered.is_empty() {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        let all_coords: Vec<(usize, u32, u32)> = filtered
                                            .iter()
                                            .map(|i| (i.sheet_idx, i.row, i.col))
                                            .collect();
                                        let has_unresolved =
                                            filtered.iter().any(|i| !i.is_thread || !i.resolved);
                                        if has_unresolved
                                            && ui
                                                .small_button("批量标已解决")
                                                .on_hover_text("将当前筛选结果全部标记为已解决（可撤销）")
                                                .clicked()
                                        {
                                            actions.push(PanelAction::BatchResolve(
                                                all_coords.clone(),
                                                true,
                                            ));
                                        }
                                        if self.comments_panel.confirm_batch_delete {
                                            if ui
                                                .add(egui::Button::new(
                                                    egui::RichText::new(format!(
                                                        "确认删除 {} 条",
                                                        filtered.len()
                                                    ))
                                                    .size(11.0)
                                                    .color(pal.on_err()),
                                                )
                                                .fill(pal.err_fg))
                                                .clicked()
                                            {
                                                self.comments_panel.confirm_batch_delete = false;
                                                actions.push(PanelAction::BatchDelete(all_coords));
                                            }
                                            if ui.small_button("取消").clicked() {
                                                self.comments_panel.confirm_batch_delete = false;
                                            }
                                        } else if ui
                                            .small_button(
                                                egui::RichText::new("批量删除")
                                                    .color(pal.err_fg),
                                            )
                                            .on_hover_text("删除当前筛选出的全部批注（单步可撤销）")
                                            .clicked()
                                        {
                                            self.comments_panel.confirm_batch_delete = true;
                                        }
                                    });
                                }
                            });

                        ui.separator();

                        // 批注卡片滚动列表
                        egui::ScrollArea::vertical()
                            .id_salt("comments_sidebar_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Frame::NONE
                                    .inner_margin(egui::Margin::symmetric(10, 8))
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.y = 8.0;
                                        if filtered.is_empty() {
                                            ui_kit::state_view(
                                                ui,
                                                ui_kit::StateKind::Empty,
                                                Icon::Comment,
                                                if total_count == 0 {
                                                    "暂无批注"
                                                } else {
                                                    "没有匹配的批注"
                                                },
                                                if total_count == 0 {
                                                    "按 Shift+F2 或点击上方「+ 新建」为当前单元格添加批注"
                                                } else {
                                                    "尝试清空搜索关键字或切换状态/作者筛选"
                                                },
                                            );
                                            return;
                                        }

                                        for item in &filtered {
                                            let is_selected = item.sheet_idx == cur_sheet
                                                && (item.row, item.col) == cur_sel;
                                            let sheet_id = self
                                                .wb
                                                .sheets
                                                .get(item.sheet_idx)
                                                .map(|s| s.identity)
                                                .unwrap_or(0);
                                            let pinned = self.pinned_comments.contains(&(
                                                sheet_id,
                                                CellRef::new(item.row, item.col),
                                            ));
                                            let (card_bg, card_border) = if item.is_thread && item.resolved {
                                                (
                                                    pal.accent_tint,
                                                    if is_selected {
                                                        pal.accent
                                                    } else {
                                                        pal.accent_border()
                                                    },
                                                )
                                            } else {
                                                let note_bg = if is_selected {
                                                    pal.warn_bg
                                                } else if pal.theme == crate::theme::AppTheme::HighContrast {
                                                    pal.card_bg
                                                } else {
                                                    pal.surface_subtle().lerp_to_gamma(pal.warn_bg, 0.55)
                                                };
                                                (
                                                    note_bg,
                                                    if is_selected {
                                                        pal.info_fg
                                                    } else {
                                                        pal.warn_border()
                                                    },
                                                )
                                            };

                                            egui::Frame::NONE
                                                .fill(card_bg)
                                                .stroke(Stroke::new(
                                                    if is_selected { 1.4 } else { 1.0 },
                                                    card_border,
                                                ))
                                                .corner_radius(6.0)
                                                .inner_margin(egui::Margin::symmetric(9, 7))
                                                .show(ui, |ui| {
                                                    ui.set_width(ui.available_width());
                                                    // 卡片头：坐标跳转 + 状态徽标 + 快捷操作
                                                    ui.horizontal(|ui| {
                                                        let loc_lbl = if self.comments_panel.scope_all_sheets {
                                                            format!("{}!{}", item.sheet_name, item.cell_addr)
                                                        } else {
                                                            item.cell_addr.clone()
                                                        };
                                                        if ui
                                                            .add(
                                                                egui::Button::new(
                                                                    egui::RichText::new(&loc_lbl)
                                                                        .strong()
                                                                        .size(12.0)
                                                                        .color(pal.info_fg),
                                                                )
                                                                .frame(false),
                                                            )
                                                            .on_hover_text("定位到该单元格")
                                                            .clicked()
                                                        {
                                                            actions.push(PanelAction::Jump(
                                                                item.sheet_idx,
                                                                item.row,
                                                                item.col,
                                                            ));
                                                        }
                                                        if !item.author.is_empty() {
                                                            ui.label(
                                                                egui::RichText::new(&item.author)
                                                                    .size(11.0)
                                                                    .color(pal.warn_fg),
                                                            );
                                                        }
                                                        if item.is_thread {
                                                            let (badge, bcol) = if item.resolved {
                                                                ("已解决", pal.accent)
                                                            } else {
                                                                ("未解决", pal.warn_fg)
                                                            };
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "[{badge}·{}]",
                                                                    item.reply_count
                                                                ))
                                                                .size(11.0)
                                                                .color(bcol),
                                                            );
                                                        }
                                                    });

                                                    if !item.cell_value.is_empty() {
                                                        let val_snip: String =
                                                            item.cell_value.chars().take(36).collect();
                                                        ui.label(
                                                            egui::RichText::new(format!("格值: {val_snip}"))
                                                                .size(11.0)
                                                                .color(pal.text_weak),
                                                        );
                                                    }

                                                    // 正文（支持选中复制，双击跳转定位）
                                                    let body_resp = ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(&item.text)
                                                                .size(12.0)
                                                                .color(pal.text),
                                                        )
                                                        .selectable(true)
                                                        .wrap(),
                                                    );
                                                    if body_resp.double_clicked() {
                                                        actions.push(PanelAction::Jump(
                                                            item.sheet_idx,
                                                            item.row,
                                                            item.col,
                                                        ));
                                                    }

                                                    // 底部快捷动作条
                                                    ui.add_space(3.0);
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.spacing_mut().item_spacing.x = 6.0;
                                                        let mini = |ui: &mut egui::Ui, lbl: &str, col: Color32| {
                                                            ui.add(
                                                                egui::Button::new(
                                                                    egui::RichText::new(lbl)
                                                                        .size(11.0)
                                                                        .color(col),
                                                                )
                                                                .frame(false),
                                                            )
                                                        };
                                                        if mini(ui, "复制", pal.text_weak).clicked() {
                                                            actions.push(PanelAction::CopyText(
                                                                item.cell_addr.clone(),
                                                                item.text.clone(),
                                                            ));
                                                        }
                                                        if mini(
                                                            ui,
                                                            if pinned { "取消固定" } else { "固定" },
                                                            if pinned {
                                                                pal.info_fg
                                                            } else {
                                                                pal.text_weak
                                                            },
                                                        )
                                                        .clicked()
                                                        {
                                                            actions.push(PanelAction::TogglePin(
                                                                item.sheet_idx,
                                                                item.row,
                                                                item.col,
                                                            ));
                                                        }
                                                        if !edit_blocked {
                                                            if mini(ui, "编辑", pal.text).clicked() {
                                                                actions.push(PanelAction::Edit(
                                                                    item.sheet_idx,
                                                                    item.row,
                                                                    item.col,
                                                                ));
                                                            }
                                                            let is_replying = self.comments_panel.reply_target
                                                                == Some((item.sheet_idx, item.row, item.col));
                                                            if mini(
                                                                ui,
                                                                if is_replying { "收起回复" } else { "回复" },
                                                                pal.info_fg,
                                                            )
                                                            .clicked()
                                                            {
                                                                if is_replying {
                                                                    self.comments_panel.reply_target = None;
                                                                    self.comments_panel.reply_draft.clear();
                                                                } else {
                                                                    self.comments_panel.reply_target = Some((
                                                                        item.sheet_idx,
                                                                        item.row,
                                                                        item.col,
                                                                    ));
                                                                    self.comments_panel.reply_draft.clear();
                                                                }
                                                            }
                                                            let next_res = !(item.is_thread && item.resolved);
                                                            if mini(
                                                                ui,
                                                                if next_res { "标记解决" } else { "重开" },
                                                                pal.accent,
                                                            )
                                                            .clicked()
                                                            {
                                                                actions.push(PanelAction::SetResolved(
                                                                    item.sheet_idx,
                                                                    item.row,
                                                                    item.col,
                                                                    next_res,
                                                                ));
                                                            }
                                                            if mini(ui, "删", pal.err_fg)
                                                                .clicked()
                                                            {
                                                                actions.push(PanelAction::DeleteOne(
                                                                    item.sheet_idx,
                                                                    item.row,
                                                                    item.col,
                                                                ));
                                                            }
                                                        }
                                                    });

                                                    // 行内快速回复框
                                                    if !edit_blocked
                                                        && self.comments_panel.reply_target
                                                            == Some((item.sheet_idx, item.row, item.col))
                                                    {
                                                        ui.add_space(4.0);
                                                        ui.horizontal(|ui| {
                                                            ui.with_layout(
                                                                egui::Layout::right_to_left(
                                                                    egui::Align::Center,
                                                                ),
                                                                |ui| {
                                                                    let send_clicked =
                                                                        ui.small_button("发送").clicked();
                                                                    let r_resp = ui.add(
                                                                        egui::TextEdit::singleline(
                                                                            &mut self.comments_panel.reply_draft,
                                                                        )
                                                                        .desired_width(f32::INFINITY)
                                                                        .hint_text("输入回复，Enter 发送…"),
                                                                    );
                                                                    let enter_send = r_resp.lost_focus()
                                                                        && ui.input(|i| {
                                                                            i.key_pressed(
                                                                                egui::Key::Enter,
                                                                            )
                                                                        });
                                                                    if (send_clicked || enter_send)
                                                                        && !self
                                                                            .comments_panel
                                                                            .reply_draft
                                                                            .trim()
                                                                            .is_empty()
                                                                    {
                                                                        let draft = std::mem::take(
                                                                            &mut self
                                                                                .comments_panel
                                                                                .reply_draft,
                                                                        );
                                                                        self.comments_panel
                                                                            .reply_target = None;
                                                                        actions.push(
                                                                            PanelAction::SendReply(
                                                                                item.sheet_idx,
                                                                                item.row,
                                                                                item.col,
                                                                                draft,
                                                                            ),
                                                                        );
                                                                    }
                                                                },
                                                            );
                                                        });
                                                    }
                                                });
                                        }
                                    });
                            });
                    });
            });

        for act in actions {
            match act {
                PanelAction::Close => {
                    self.comments_panel.open = false;
                    self.comments_panel.confirm_batch_delete = false;
                }
                PanelAction::Jump(si, r, c) => {
                    if si != self.sheet_idx {
                        let _ = self.switch_sheet_view(si);
                    }
                    self.sel = (r, c);
                    self.sel_end = (r, c);
                    self.extra_sel.clear();
                    self.ensure_visible();
                }
                PanelAction::Edit(si, r, c) => {
                    if si != self.sheet_idx {
                        let _ = self.switch_sheet_view(si);
                    }
                    self.sel = (r, c);
                    self.sel_end = (r, c);
                    self.extra_sel.clear();
                    self.ensure_visible();
                    self.open_note_editor(Some(ctx));
                }
                PanelAction::TogglePin(si, r, c) => {
                    if si != self.sheet_idx {
                        let _ = self.switch_sheet_view(si);
                    }
                    self.toggle_pin_comment(si, r, c);
                }
                PanelAction::SetResolved(si, r, c, res) => {
                    let _ = self.set_comment_resolved(si, r, c, res);
                }
                PanelAction::DeleteOne(si, r, c) => {
                    self.batch_delete_comments(&[(si, r, c)], "删除批注");
                }
                PanelAction::CopyText(addr, txt) => {
                    ctx.copy_text(txt);
                    self.status = format!("已复制 {addr} 的批注");
                }
                PanelAction::SendReply(si, r, c, draft) => {
                    let _ = self.reply_comment_cell(si, r, c, &draft, None);
                }
                PanelAction::NewAtSelection => {
                    self.open_note_editor(Some(ctx));
                }
                PanelAction::ExportSummary(all) => {
                    self.export_comments_summary_sheet(all);
                }
                PanelAction::BatchResolve(coords, res) => {
                    self.batch_set_comments_resolved(&coords, res);
                }
                PanelAction::BatchDelete(coords) => {
                    self.batch_delete_comments(&coords, "批量删除批注");
                }
            }
        }
    }
}
