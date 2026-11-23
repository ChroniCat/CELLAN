use crate::{egui, SheetApp};
use sheetcore::csv::{self, Encoding};
use sheetcore::{
    external::{self, CsvOpts, Kind, Source},
    model::{Sheet, Workbook},
    undo::{self, Action, PreparedAction},
};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct Worker<T> {
    rx: Receiver<Result<T, String>>,
    cancelled: Arc<AtomicBool>,
}

impl<T: Send + 'static> Worker<T> {
    fn start(run: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static) -> Self {
        // A cancelled ready result is freed by its sender, outside the UI thread.
        let (tx, rx) = mpsc::sync_channel(0);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        std::thread::spawn(move || {
            let result = run(&flag);
            if !flag.load(Ordering::Relaxed) {
                let _ = tx.send(result);
            }
        });
        Self { rx, cancelled }
    }

    fn poll(&self) -> Option<Result<T, String>> {
        match self.rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("数据读取线程已退出".into())),
        }
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("已取消".into())
    } else {
        Ok(())
    }
}

pub(super) struct ParsedCsv {
    pub preview_rows: Vec<Vec<String>>,
    pub row_count: usize,
    pub cols: usize,
    pub delim: char,
    pub encoding: Encoding,
    pub raw_bytes: Option<Arc<Vec<u8>>>,
}

fn read_csv_bytes(path: &Path, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    let read_err = |e| format!("读不了 {}：{e}", path.display());
    let mut file = std::fs::File::open(path).map_err(read_err)?;
    let cap = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let mut bytes = Vec::with_capacity(cap);
    let mut chunk = vec![0; 256 * 1024];
    loop {
        check_cancelled(cancel)?;
        let n = file.read(&mut chunk).map_err(read_err)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(bytes)
}

fn read_csv_summary(
    path: &Path,
    delim: Option<char>,
    encoding: Option<Encoding>,
    quote: char,
    sniff_comma: bool,
    cancel: &AtomicBool,
) -> Result<ParsedCsv, String> {
    let bytes = read_csv_bytes(path, cancel)?;
    let (text, encoding, bad) = csv::decode_cow(&bytes, encoding);
    check_cancelled(cancel)?;
    if bad {
        return Err("所选编码无法完整解码这个文件".into());
    }
    let delim = match delim {
        None => csv::sniff(&text),
        Some(',') if sniff_comma && !text.lines().next().unwrap_or("").contains(',') => {
            csv::sniff(&text)
        }
        Some(ch) => ch,
    };
    let summary = csv::scan_summary_while(&text, delim, quote, encoding, 8, || {
        !cancel.load(Ordering::Relaxed)
    })?;
    if !within_grid((0, 0), summary.row_count, summary.cols) {
        return Err("数据超出工作表边界".into());
    }
    drop(text);
    // Retain raw file bytes up to 64 MiB so confirming the wizard avoids re-reading disk,
    // without holding hundreds of megabytes of per-cell String objects in memory.
    let raw_bytes = (bytes.len() <= 64 * 1024 * 1024).then(|| Arc::new(bytes));
    Ok(ParsedCsv {
        preview_rows: summary.preview_rows,
        row_count: summary.row_count,
        cols: summary.cols,
        delim: summary.delim,
        encoding: summary.encoding,
        raw_bytes,
    })
}

pub(super) struct CsvDlg {
    pub path: PathBuf,
    pub delim: char,
    pub header: bool,
    pub enc: Option<Encoding>,
    pub used_encoding: Encoding,
    pub quote: char,
    pub text_cols: std::collections::BTreeSet<u32>,
    fast_preview: Vec<Vec<String>>,
    records: Option<ParsedCsv>,
    pub cols: usize,
    pub err: Option<String>,
    target: Target,
    before: Option<Sheet>,
    pending: Option<Worker<ParsedCsv>>,
}

impl CsvDlg {
    pub fn new(path: &Path, target: Target, before: Sheet) -> Self {
        let mut dlg = Self {
            path: path.to_owned(),
            delim: ',',
            header: false,
            enc: None,
            used_encoding: Encoding::Utf8,
            quote: '"',
            text_cols: Default::default(),
            fast_preview: Vec::new(),
            records: None,
            cols: 0,
            err: None,
            pending: None,
            before: Some(before),
            target,
        };
        dlg.reload();
        dlg
    }

    pub fn reload(&mut self) {
        self.discard_records();
        self.fast_preview.clear();
        self.cols = 0;
        self.err = None;
        // For files > 64 KiB, sample the first 64 KiB immediately (< 1ms) so the wizard
        // shows the 8-row preview right away while background tail validation completes.
        if self.path.metadata().is_ok_and(|m| m.len() > 64 * 1024) {
            if let Some((rows, cols, delim, enc)) =
                csv::read_preview_head(&self.path, self.enc, self.quote, 8)
            {
                self.fast_preview = rows;
                self.cols = cols;
                self.delim = delim;
                self.used_encoding = enc;
            }
        }
        let path = self.path.clone();
        let encoding = self.enc;
        let quote = self.quote;
        self.pending = Some(Worker::start(move |cancel| {
            read_csv_summary(&path, None, encoding, quote, true, cancel)
        }));
    }

    pub fn poll_loading(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        let Some(result) = rx.poll() else { return true };
        self.pending = None;
        self.apply(result);
        false
    }

    fn apply(&mut self, result: Result<ParsedCsv, String>) {
        self.fast_preview.clear();
        match result {
            Ok(data) => {
                self.cols = data.cols;
                self.delim = data.delim;
                self.used_encoding = data.encoding;
                self.records = Some(data);
            }
            Err(err) => {
                self.cols = 0;
                self.err = Some(err);
            }
        }
    }

    pub(super) fn preview(&self) -> &[Vec<String>] {
        if let Some(data) = &self.records {
            &data.preview_rows[..data.preview_rows.len().min(8)]
        } else {
            &self.fast_preview[..self.fast_preview.len().min(8)]
        }
    }

    fn discard_records(&mut self) {
        if let Some(data) = self.records.take() {
            if data.raw_bytes.as_ref().is_some_and(|b| b.len() > 1024 * 1024) {
                std::thread::spawn(move || drop(data));
            }
        }
    }
}

impl Drop for CsvDlg {
    fn drop(&mut self) {
        self.discard_records();
    }
}

use super::sheet_edit_target::SheetEditTarget as Target;

enum Input {
    Import {
        path: PathBuf,
        delim: char,
        header: bool,
        opts: CsvOpts,
        parsed: Option<ParsedCsv>,
    },
    Refresh,
}

pub(super) struct PendingImport {
    target: Target,
    path: Option<PathBuf>,
    refreshing: bool,
    sources: Vec<Source>,
    worker: Worker<PreparedImport>,
}

struct PreparedImport {
    before: Sheet,
    result: Sheet,
    keys: Vec<(u32, u32)>,
    broadcast: Vec<(usize, u32, u32)>,
    sources: Vec<Source>,
    registration: Option<Registration>,
    history: PreparedAction,
    revision_sample: Vec<undo::CellDelta>,
    changed: usize,
    label: &'static str,
    status: String,
    internal: Vec<Sheet>,
}

fn within_grid(at: (u32, u32), rows: usize, cols: usize) -> bool {
    at.0 < sheetcore::sync::MAX_ROWS
        && at.1 < sheetcore::sync::MAX_COLS
        && rows <= (sheetcore::sync::MAX_ROWS - at.0) as usize
        && cols <= (sheetcore::sync::MAX_COLS - at.1) as usize
}

struct Registration {
    file: PathBuf,
    at: (u32, u32),
    size: (u32, u32),
    delim: char,
    header: bool,
    opts: CsvOpts,
}

fn store_source(wb: &mut Workbook, registration: Registration) {
    let Registration {
        file,
        at,
        size,
        delim,
        header,
        opts,
    } = registration;
    let stored = external::store_path(wb, &file);
    let sources = &mut wb.sources;
    let existing = sources
        .iter()
        .position(|s| s.at == at && matches!(&s.kind, Kind::Csv { path, .. } if path == &stored));
    let name = existing
        .map(|i| sources[i].name.clone())
        .unwrap_or_else(|| {
            let mut n = sources.len() + 1;
            while sources.iter().any(|s| s.name == format!("_sc_src_{n}")) {
                n += 1;
            }
            format!("_sc_src_{n}")
        });
    let src = Source {
        name,
        kind: Kind::Csv {
            path: stored,
            delim,
            header,
        },
        at,
        rows: size.0,
        cols: size.1,
        csv_opts: Some(opts),
    };
    if let Some(i) = existing {
        sources[i] = src;
    } else {
        sources.push(src);
    }
}

fn prepare(
    input: Input,
    target: Target,
    path: Option<PathBuf>,
    mut sources: Vec<Source>,
    before: Sheet,
    internal: Vec<Sheet>,
    cancel: &AtomicBool,
) -> Result<PreparedImport, String> {
    let mut result = before.reorder_snapshot();
    let mut keys = Vec::new();
    let mut location = Workbook::new_blank();
    location.path = path;
    let sizes = |sources: &[Source]| {
        sources
            .iter()
            .map(|s| (s.name.clone(), s.rows, s.cols))
            .collect::<Vec<_>>()
    };
    let mut refresh_sizes = None;
    let mut registration = None;
    let (label, status) = match input {
        Input::Import {
            path,
            delim,
            header,
            mut opts,
            parsed,
        } => {
            let owned_bytes;
            let (raw_slice, actual_delim, actual_enc) = if let Some(ref data) = parsed {
                if data.row_count == 0 {
                    return Err("这个文件里没有可导入的内容".into());
                }
                if !within_grid(target.at, data.row_count, data.cols) {
                    return Err("数据超出工作表边界".into());
                }
                let slice: &[u8] = if let Some(ref arc) = data.raw_bytes {
                    arc.as_slice()
                } else {
                    owned_bytes = read_csv_bytes(&path, cancel)?;
                    owned_bytes.as_slice()
                };
                (slice, delim, data.encoding)
            } else {
                owned_bytes = read_csv_bytes(&path, cancel)?;
                let (probe, enc, bad) = csv::decode_cow(&owned_bytes, opts.encoding);
                if bad {
                    return Err("所选编码无法完整解码这个文件".into());
                }
                let d = if delim == ',' && !probe.lines().next().unwrap_or("").contains(',') {
                    csv::sniff(&probe)
                } else {
                    delim
                };
                (owned_bytes.as_slice(), d, enc)
            };
            let (text, used_enc, bad) = csv::decode_cow(raw_slice, Some(actual_enc));
            check_cancelled(cancel)?;
            if bad {
                return Err("所选编码无法完整解码这个文件".into());
            }
            opts.encoding = Some(used_enc);
            let (rows, cols, imported_keys) = external::import_csv_str_parallel(
                &mut result,
                &text,
                actual_delim,
                opts.quote,
                header,
                &opts.text_cols,
                (0, 0),
                target.at,
                &|| !cancel.load(Ordering::Relaxed),
            )?;
            if rows == 0 {
                return Err("这个文件里没有可导入的内容".into());
            }
            if !within_grid(target.at, rows as usize, cols as usize) {
                return Err("数据超出工作表边界".into());
            }
            keys = imported_keys;
            let size = (rows, cols);
            registration = Some(Registration {
                file: path,
                at: target.at,
                size,
                delim: actual_delim,
                header,
                opts,
            });
            ("导入 CSV", format!("已导入 {} 行 × {} 列", size.0, size.1))
        }
        Input::Refresh => {
            let old_sizes = sizes(&sources);
            let mut ok = 0;
            let mut failures = Vec::new();
            for src in &mut sources {
                check_cancelled(cancel)?;
                let size = (|| match &src.kind {
                    Kind::Csv {
                        path,
                        delim,
                        header,
                    } => {
                        let opts = src.csv_opts.clone().unwrap_or_default();
                        let real = external::resolve_path(&location, path);
                        let raw_bytes = read_csv_bytes(&real, cancel)?;
                        let (text, _enc, bad) = csv::decode_cow(&raw_bytes, opts.encoding);
                        check_cancelled(cancel)?;
                        if bad {
                            return Err("所选编码无法完整解码这个文件".into());
                        }
                        let mut scratch_sheet = result.clone();
                        let (rows, cols, mut src_keys) = external::import_csv_str_parallel(
                            &mut scratch_sheet,
                            &text,
                            *delim,
                            opts.quote,
                            *header,
                            &opts.text_cols,
                            (src.rows, src.cols),
                            src.at,
                            &|| !cancel.load(Ordering::Relaxed),
                        )?;
                        if !within_grid(
                            src.at,
                            (rows as usize).max(src.rows as usize),
                            (cols as usize).max(src.cols as usize),
                        ) {
                            return Err("数据超出工作表边界".into());
                        }
                        result = scratch_sheet;
                        keys.append(&mut src_keys);
                        Ok((rows, cols))
                    }
                    Kind::Internal {
                        sheet,
                        rect: (r, c, end_r, end_c),
                    } => {
                        let sh = internal
                            .iter()
                            .find(|s| &s.name == sheet)
                            .ok_or("找不到其他来源表")?;
                        if end_r < r
                            || end_c < c
                            || *end_r >= sheetcore::sync::MAX_ROWS
                            || *end_c >= sheetcore::sync::MAX_COLS
                            || !within_grid(
                                src.at,
                                (end_r - r + 1) as usize,
                                (end_c - c + 1) as usize,
                            )
                        {
                            return Err("来源区域超出工作表边界".into());
                        }
                        let mut scratch_sheet = result.clone();
                        let mut src_keys = Vec::new();
                        let res = external::visit_source_sheet(
                            sh,
                            (*r, *c, *end_r, *end_c),
                            src.at,
                            &mut |r, c, cell| {
                                if src_keys.len() % 1024 == 0 {
                                    check_cancelled(cancel)?;
                                }
                                src_keys.push((r, c));
                                scratch_sheet.set(r, c, cell);
                                Ok::<_, String>(())
                            },
                        )?;
                        result = scratch_sheet;
                        keys.append(&mut src_keys);
                        Ok(res)
                    }
                })();
                check_cancelled(cancel)?;
                match size {
                    Ok((rows, cols)) => {
                        src.rows = rows;
                        src.cols = cols;
                        ok += 1;
                    }
                    Err(e) => failures.push(e),
                }
            }
            if ok == 0 {
                return Err(format!(
                    "刷新失败 {} 个数据源：{}",
                    failures.len(),
                    failures.first().map_or("", String::as_str)
                ));
            }
            let (before_sizes, after_sizes): (Vec<_>, Vec<_>) = old_sizes
                .into_iter()
                .zip(sizes(&sources))
                .filter(|(before, after)| before != after)
                .unzip();
            if !before_sizes.is_empty() {
                refresh_sizes = Some((before_sizes, after_sizes));
            }
            // Multiple sources may have written one cell. Its undo baseline is the original workbook.
            (
                "刷新外部数据",
                if failures.is_empty() {
                    format!("已刷新 {ok} 个数据源")
                } else {
                    format!(
                        "刷新完成：成功 {ok}，失败 {}：{}",
                        failures.len(),
                        failures[0]
                    )
                },
            )
        }
    };
    if !keys.windows(2).all(|w| w[0] < w[1]) {
        keys.sort_unstable();
        keys.dedup();
    }
    let mut deltas = Vec::with_capacity(keys.len().min(65_536));
    if before.is_empty() {
        let sheet_idx = target.sheet;
        for (i, &(r, c)) in keys.iter().enumerate() {
            if (i & 4095) == 0 {
                check_cancelled(cancel)?;
            }
            let after_style = result.style_of(r, c);
            if let Some(after) = result.get(r, c) {
                if !after.is_blank() || after_style != 0 {
                    deltas.push(undo::CellDelta {
                        sheet: sheet_idx,
                        row: r,
                        col: c,
                        before: sheetcore::Cell::default(),
                        after: after.clone(),
                        before_style: 0,
                        after_style,
                    });
                }
            } else if after_style != 0 {
                deltas.push(undo::CellDelta {
                    sheet: sheet_idx,
                    row: r,
                    col: c,
                    before: sheetcore::Cell::default(),
                    after: sheetcore::Cell::default(),
                    before_style: 0,
                    after_style,
                });
            }
        }
    } else {
        for chunk in keys.chunks(4096) {
            check_cancelled(cancel)?;
            deltas.extend(undo::finish(
                undo::capture(&before, target.sheet, chunk),
                &result,
            ));
        }
    }
    let changed = deltas.len();
    let broadcast = if target.room.is_some() {
        deltas.iter().map(|d| (d.sheet, d.row, d.col)).collect()
    } else {
        Vec::new()
    };
    let revision_sample = deltas
        .iter()
        .take(sheetcore::revision::MAX_CELLS)
        .cloned()
        .collect();
    let history = Action {
        label: label.into(),
        deltas,
        focus: (target.sheet, target.at.0, target.at.1),
        refresh_sizes,
        by_site: 0,
        snap_before: None,
        snap_after: None,
        pictures: None,
        shapes: None,
        merges: None,
    }
    .prepare();
    Ok(PreparedImport {
        before,
        result,
        keys,
        broadcast,
        sources,
        history,
        revision_sample,
        changed,
        label,
        status,
        internal,
        registration,
    })
}

impl SheetApp {
    fn start_import(&mut self, input: Input, target: Target) {
        self.start_import_from(input, target, None);
    }

    fn start_import_from(&mut self, input: Input, target: Target, before: Option<Sheet>) {
        if !target.valid(self) {
            self.status = "目标工作表或协作状态已改变，请重新导入".into();
            return;
        }
        self.csv_dlg = None;
        let before = before.unwrap_or_else(|| self.wb.sheet(target.sheet).reorder_snapshot());
        let refreshing = matches!(&input, Input::Refresh);
        let internal = if refreshing {
            self.wb
                .sheets
                .iter()
                .filter(|s| {
                    s.identity != before.identity && self.wb.sources.iter().any(|src|
                matches!(&src.kind, Kind::Internal { sheet, .. } if sheet == &s.name))
                })
                .map(Sheet::search_snapshot)
                .collect()
        } else {
            Vec::new()
        };
        let path = self.wb.path.clone();
        let sources = if refreshing {
            self.wb.sources.clone()
        } else {
            Vec::new()
        };
        let worker_path = path.clone();
        let worker_sources = sources.clone();
        let worker_target = target.clone();
        self.status = "正在准备外部数据…".into();
        self.csv_job = Some(PendingImport {
            target,
            path,
            refreshing,
            sources,
            worker: Worker::start(move |cancel| {
                prepare(
                    input,
                    worker_target,
                    worker_path,
                    worker_sources,
                    before,
                    internal,
                    cancel,
                )
            }),
        });
    }

    pub(super) fn confirm_csv(&mut self, mut dlg: CsvDlg) {
        let opts = CsvOpts {
            encoding: Some(dlg.used_encoding),
            text_cols: dlg.text_cols.iter().copied().collect(),
            quote: dlg.quote,
        };
        let Some(parsed) = dlg.records.take() else {
            return;
        };
        self.start_import_from(
            Input::Import {
                path: dlg.path.clone(),
                delim: dlg.delim,
                header: dlg.header,
                opts,
                parsed: Some(parsed),
            },
            dlg.target.clone(),
            dlg.before.take(),
        );
    }

    pub(super) fn import_csv_choice(
        &mut self,
        path: &Path,
        delim: char,
        header: bool,
        enc: Option<Encoding>,
        quote: char,
        text_cols: &[u32],
    ) {
        let opts = CsvOpts {
            encoding: enc,
            text_cols: text_cols.to_vec(),
            quote,
        };
        self.start_import(
            Input::Import {
                path: path.to_owned(),
                delim,
                header,
                opts,
                parsed: None,
            },
            Target::current(self, self.sel),
        );
    }

    pub(super) fn refresh_import(&mut self) {
        if !self.wb.sources.is_empty() {
            self.start_import(Input::Refresh, Target::current(self, self.sel));
        } else if let Some((path, r, c)) = self.last_import.clone() {
            self.start_import(
                Input::Import {
                    path,
                    delim: ',',
                    header: false,
                    opts: CsvOpts::default(),
                    parsed: None,
                },
                Target::current(self, (r, c)),
            );
        } else {
            self.status = "还没有导入过外部数据".into();
        }
    }

    fn cancel_csv_job(&mut self) {
        self.csv_job = None;
        self.status = "已取消外部数据操作".into();
    }

    pub(super) fn poll_csv_job(&mut self) {
        let Some(job) = &self.csv_job else { return };
        let moved_source_base = job.refreshing
            && self.wb.path.as_deref().and_then(Path::parent)
                != job.path.as_deref().and_then(Path::parent)
            && job
                .sources
                .iter()
                .any(|s| matches!(s.kind, Kind::Csv { .. }));
        if !job.target.valid(self)
            || moved_source_base
            || (job.refreshing && self.wb.sources != job.sources)
        {
            self.csv_job = None;
            self.status = "工作簿、目标表或协作状态已改变，已取消外部数据操作".into();
            return;
        }
        let Some(outcome) = job.worker.poll() else {
            return;
        };
        let idx = job.target.sheet;
        self.csv_job = None;
        let mut update = match outcome {
            Ok(update) => update,
            Err(e) => {
                self.status = format!("外部数据操作失败（未写入）：{e}");
                return;
            }
        };
        let live = self.wb.sheet(idx);
        let whole_snapshot_matches = live.matches_reorder_source(&update.before);
        let source_changed = update.sources.iter().any(|source| {
            let Kind::Internal { sheet, rect } = &source.kind else { return false };
            let Some(before) = update.internal.iter().find(|s| &s.name == sheet) else { return false };
            !self.wb.sheets.iter().any(|s| s.identity == before.identity && s.name == before.name
                && s.coordinate_revision() == before.coordinate_revision()
                && s.same_cells_in(before, &[*rect], false))
        });
        if source_changed || (!whole_snapshot_matches && !live.csv_inputs_match(&update.before, &update.keys)) {
            self.status = "来源表或目标表已改变，请重新操作".into();
            std::thread::spawn(move || drop(update));
            return;
        }
        if !self.write_preflight_on(idx, &update.keys, update.label) {
            std::thread::spawn(move || drop(update));
            return;
        }
        let live = self.wb.sheet_mut(idx);
        if !whole_snapshot_matches {
            live.rebase_row_changes(&update.before, &mut update.result,
                |k| update.keys.binary_search(&(k.row, k.col)).is_ok());
        }
        live.take_row_changes(&mut update.result);
        if let Some(registration) = update.registration {
            store_source(&mut self.wb, registration);
        } else {
            self.wb.sources = update.sources;
        }
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        self.record_revision_sample(update.label, &update.revision_sample, update.changed);
        self.undo.push_prepared(update.history);
        self.broadcast_cells(&update.broadcast);
        if update.changed != 0 {
            self.wb.calc_manual |= self.manual_calc;
            self.recalc_all_and_deps_adaptive();
        }
        self.status = update.status;
        // The old COW pages may now own the last copy of millions of strings.
        std::thread::spawn(move || drop((update.result, update.before, update.keys)));
    }

    pub(super) fn csv_job_ui(&mut self, ctx: &egui::Context) {
        if let Some(hash) = self.tasks.poll_base_hash(&self.wb, &mut self.wb_base_hash) {
            if let Some(n) = &self.net {
                if n.hosting && !self.wb.dirty {
                    n.set_base_hash(hash);
                }
            }
        }
        self.tasks.poll_pdf_export(&mut self.status, ctx);
        if self.csv_job.is_none() {
            return;
        }
        let mut open = true;
        let mut cancel = false;
        crate::ui_kit::window(ctx, "导入或刷新外部数据", 320.0)
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label("正在准备外部数据…");
                cancel = crate::ui_kit::secondary_button(ui, "取消").clicked();
            });
        if !open || cancel {
            self.cancel_csv_job();
            return;
        }
        self.poll_csv_job();
        if self.csv_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(40));
        }
    }

    // Used by the explicit synchronous command-line probe and integration tests.
    pub(super) fn finish_csv_probe(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while self.csv_job.is_some() && std::time::Instant::now() < deadline {
            self.poll_csv_job();
            if self.csv_job.is_some() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        if self.csv_job.is_some() {
            self.cancel_csv_job();
            self.status = "外部数据自检超时".into();
        }
    }
}

impl CsvDlg {
    pub(super) fn valid(&self, app: &SheetApp) -> bool {
        self.target.valid(app)
    }

    #[cfg(test)]
    pub(super) fn wait_preview(&mut self) {
        let worker = self.pending.take().unwrap();
        self.apply(
            worker
                .rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("CSV preview timed out"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::{Cell, Value};

    fn number(n: f64) -> Cell {
        Cell {
            value: Value::Number(n),
            formula: None,
        }
    }

    #[test]
    fn cancelling_or_replacing_workers_stops_processing_and_discards_ready_writes() {
        let env = crate::test_env::TestEnv::new("csv_cancel_replace");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let path = env.path("replace.csv");
        std::fs::write(&path, "1,2\n3,4\n").unwrap();
        app.open_csv_dialog(&path);
        let flag = app
            .csv_dlg
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .cancelled
            .clone();
        app.csv_dlg.as_mut().unwrap().reload();
        assert!(flag.load(Ordering::Relaxed));
        let dlg = app.csv_dlg.as_mut().unwrap();
        dlg.wait_preview();
        let dlg = app.csv_dlg.take().unwrap();
        app.confirm_csv(dlg);
        let flag = app.csv_job.as_ref().unwrap().worker.cancelled.clone();
        app.cancel_csv_job();
        assert!(flag.load(Ordering::Relaxed));
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(app.wb.sources.is_empty() && !app.wb.dirty);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty);
        let mut checks = 0;
        let enormous = format!("\"{}\"", "x".repeat(100_000));
        assert!(csv::records_strict_while(&enormous, ',', '"', || {
            checks += 1;
            checks < 3
        })
        .is_err());
        assert_eq!(checks, 3);
    }

    #[test]
    fn import_keeps_its_original_sheet_but_rejects_document_changes_and_target_overwrites() {
        let env = crate::test_env::TestEnv::new("csv_target_guard");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let path = env.path("source.csv");
        std::fs::write(&path, "7").unwrap();
        app.sel = (2, 3);
        app.open_csv_dialog(&path);
        app.csv_dlg.as_mut().unwrap().wait_preview();
        app.sel = (8, 8);
        let dlg = app.csv_dlg.take().unwrap();
        app.confirm_csv(dlg);
        app.finish_csv_probe();
        assert_eq!(app.wb.sheet(0).value(2, 3), Value::Number(7.));
        assert_eq!(app.sel, (8, 8));
        assert_eq!(app.wb.sources[0].at, (2, 3));
        app.wb.sheets.push(Sheet::new("Other"));
        app.import_csv_from(&path);
        assert!(app.switch_sheet_view(1));
        app.wb.sheet_mut(0).set(8, 9, number(81.));
        app.finish_csv_probe();
        assert_eq!(app.sheet_idx, 1);
        assert_eq!(app.wb.sheet(0).value(8, 8), Value::Number(7.));
        assert_eq!(app.wb.sheet(0).value(8, 9), Value::Number(81.));
        assert_eq!(app.wb.sheet(1).value(8, 8), Value::Empty);
        app.sel = (8, 8);
        app.import_csv_from(&path);
        app.doc_uid = crate::next_doc_uid();
        app.finish_csv_probe();
        assert_eq!(app.wb.sheet(1).value(8, 8), Value::Empty);
        app.import_csv_from(&path);
        app.wb.sheet_mut(1).set(8, 8, number(90.));
        app.finish_csv_probe();
        assert_eq!(app.wb.sheet(1).value(8, 8), Value::Number(90.));
        assert_eq!(app.undo.depth(), (2, 0));
    }

    #[test]
    fn wizard_checks_only_the_cells_it_will_write_since_opening() {
        let env = crate::test_env::TestEnv::new("csv_preview_edit");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let path = env.path("ragged.csv");
        std::fs::write(&path, "1,2\n3").unwrap();
        app.wb.sheet_mut(0).set(7, 8, number(0.));
        app.open_csv_dialog(&path);
        app.wb.sheet_mut(0).set(7, 8, number(-0.));
        app.wb.sheet_mut(0).set(1, 1, number(70.)); // the short row leaves B2 alone
        let mut dlg = app.csv_dlg.take().unwrap();
        assert!(dlg.valid(&app));
        dlg.wait_preview();
        app.confirm_csv(dlg);
        app.finish_csv_probe();
        let Value::Number(zero) = app.wb.sheet(0).value(7, 8) else { panic!("number") };
        assert!(zero.is_sign_negative());
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(3.));
        assert_eq!(app.wb.sheet(0).value(1, 1), Value::Number(70.));
        app.open_csv_dialog(&path);
        app.wb.sheet_mut(0).set(1, 0, number(80.));
        let mut dlg = app.csv_dlg.take().unwrap();
        dlg.wait_preview();
        app.confirm_csv(dlg);
        app.finish_csv_probe();
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(80.));
        assert_eq!(app.undo.depth(), (1, 0));
    }

    #[test]
    fn unrelated_edits_and_first_save_receipt_preserve_import_but_target_structure_cancels_it() {
        let env = crate::test_env::TestEnv::new("csv_save_receipt");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.wb.sheets.push(Sheet::new("Other"));
        let csv = env.path("saved.csv");
        let xlsx = env.path("saved.xlsx");
        std::fs::write(&csv, "24").unwrap();
        app.import_csv_from(&csv);
        app.wb.sheet_mut(1).set(1, 1, number(31.));
        app.doc_gen = app.doc_gen.wrapping_add(1);
        let receipt = sheetcore::xlsx::save_atomic_with_receipt(&xlsx, &app.wb).unwrap();
        receipt.apply_to(&mut app.wb);
        app.wb.path = Some(xlsx.clone());
        app.finish_csv_probe();
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(24.));
        assert_eq!(app.wb.sheet(1).value(1, 1), Value::Number(31.));
        assert!(matches!(&app.wb.sources[0].kind, Kind::Csv {path,..} if path == "saved.csv"));
        sheetcore::xlsx::save_atomic(&xlsx, &app.wb).unwrap();
        assert_eq!(
            sheetcore::xlsx::load(&xlsx).unwrap().sheet(0).value(0, 0),
            Value::Number(24.)
        );
        let depth = app.undo.depth();
        app.sel = (50, 0);
        app.import_csv_from(&csv);
        app.wb
            .sheet_mut(0)
            .apply_structural(&sheetcore::structural::Structural::InsertRows { at: 20, n: 1 });
        app.finish_csv_probe();
        assert_eq!(app.wb.sheet(0).value(50, 0), Value::Empty);
        assert_eq!(app.undo.depth(), depth);
    }

    #[test]
    fn csv_room_and_late_locks_guard_commit_and_published_cells_can_be_undone() {
        let env = crate::test_env::TestEnv::new("csv_room_locks");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let path = env.path("room.csv");
        std::fs::write(&path, "7,8").unwrap();
        app.import_csv_from(&path);
        app.net = Some(crate::net::host_for_test(91, "test", "csv-room", "test").0);
        app.peer = Some(sheetcore::sync::Peer::new(91));
        app.undo.set_site(91);
        app.finish_csv_probe();
        assert!(app.wb.sources.is_empty());
        app.import_csv_from(&path);
        app.locks.request(0, 0, 1, 92, "other", 0);
        app.finish_csv_probe();
        assert!(app.status.contains("other"), "{}", app.status);
        assert!(app.wb.sources.is_empty() && !app.wb.dirty);
        app.locks.release(0, 0, 1, 92);
        app.import_csv_from(&path);
        app.finish_csv_probe();
        assert_eq!(
            app.peer.as_ref().unwrap().cells[&(0, 0, 1)].value,
            sheetcore::sync::ValueWire::Number(8.)
        );
        app.do_undo(false);
        assert_eq!(
            app.peer.as_ref().unwrap().cells[&(0, 0, 1)].value,
            sheetcore::sync::ValueWire::Empty
        );
    }

    #[test]
    fn refresh_commits_successful_sources_together_and_undo_preserves_unrelated_writes() {
        let env = crate::test_env::TestEnv::new("csv_refresh_overlap");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let path = env.path("main.csv");
        std::fs::write(&path, "1,2").unwrap();
        app.import_csv_from(&path);
        app.finish_csv_probe();
        let mut src = app.wb.sources[0].clone();
        src.name = "bad".into();
        src.kind = Kind::Csv {
            path: env.path("missing.csv").to_string_lossy().into(),
            delim: ',',
            header: false,
        };
        app.wb.sources.push(src);
        let mut other = Sheet::new("Other");
        other.set(0, 0, number(5.));
        app.wb.sheets.push(other);
        app.wb.sources.push(Source {
            name: "internal".into(),
            kind: Kind::Internal {
                sheet: "Other".into(),
                rect: (0, 0, 0, 0),
            },
            at: (1, 0),
            rows: 1,
            cols: 1,
            csv_opts: None,
        });
        std::fs::write(&path, "3\n4").unwrap();
        app.refresh_import();
        app.finish_csv_probe();
        assert!(app.status.contains("成功 2，失败 1"), "{}", app.status);
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(5.));
        app.wb.sheet_mut(0).set(5, 5, number(99.));
        app.do_undo(false);
        assert_eq!((app.wb.sources[0].rows, app.wb.sources[0].cols), (1, 2));
        assert_eq!(app.wb.sheet(0).value(5, 5), Value::Number(99.));
        app.do_undo(true);
        assert_eq!((app.wb.sources[0].rows, app.wb.sources[0].cols), (2, 1));
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(5.));
        app.wb.sources.truncate(1);
        std::fs::write(&path, "\"broken").unwrap();
        let depth = app.undo.depth();
        app.wb.dirty = false;
        app.refresh_import();
        app.finish_csv_probe();
        assert_eq!(app.undo.depth(), depth);
        assert!(!app.wb.dirty);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(3.));
    }

    #[test]
    fn refresh_checks_internal_source_versions_and_undoes_a_size_only_change() {
        let env = crate::test_env::TestEnv::new("csv_refresh_size_only");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let path = env.path("shape.csv");
        std::fs::write(&path, "x").unwrap();
        app.import_csv_from(&path);
        app.finish_csv_probe();
        std::fs::write(&path, "x,").unwrap();
        app.refresh_import();
        app.finish_csv_probe();
        assert_eq!(app.wb.sources[0].cols, 2);
        app.do_undo(false);
        assert_eq!(app.wb.sources[0].cols, 1);
        app.do_undo(true);
        assert_eq!(app.wb.sources[0].cols, 2);
        app.wb.sheets.push(Sheet::new("Other"));
        app.wb.sources.push(Source {
            name: "reference".into(),
            kind: Kind::Internal {
                sheet: "Other".into(),
                rect: (0, 0, 0, 0),
            },
            at: (0, 0),
            rows: 1,
            cols: 1,
            csv_opts: None,
        });
        app.refresh_import();
        app.wb.sheet_mut(1).set(0, 0, number(81.));
        app.finish_csv_probe();
        assert!(app.status.contains("已改变"), "{}", app.status);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("x".into()));
        app.wb.sheet_mut(0).protection = Some(Default::default());
        let depth = app.undo.depth();
        app.refresh_import();
        app.finish_csv_probe();
        assert!(app.status.contains("受保护"), "{}", app.status);
        assert_eq!(app.undo.depth(), depth);
    }

    #[test]
    fn large_csv_validates_tail_and_preserves_columns_outside_preview() {
        let env = crate::test_env::TestEnv::new("csv_large_preview");
        let path = env.path("large.csv");
        let mut csv = "1,2\n".repeat(34_000);
        csv.push_str("late,wide,row\n");
        std::fs::write(&path, &csv).unwrap();
        let ctx = egui::Context::default();
        let app = SheetApp::new_with_context(&ctx, None);
        let mut dialog = CsvDlg::new(&path, Target::current(&app, (0, 0)), app.wb.sheet(0).reorder_snapshot());
        assert!(dialog.pending.is_some());
        // Wait for this dialog's worker without mutating the UI or workbook.
        dialog.wait_preview();
        assert_eq!(dialog.cols, 3);
        assert_eq!(dialog.preview().len(), 8);
        assert_eq!(dialog.records.as_ref().unwrap().row_count, 34_001);
        csv.push_str("\"unterminated");
        std::fs::write(&path, csv).unwrap();
        dialog.reload();
        dialog.wait_preview();
        assert!(dialog.err.is_some());
        assert!(dialog.preview().is_empty());
    }
}
