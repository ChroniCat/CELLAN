use crate::model::{Cell, Sheet, Value, Workbook};
use anyhow::{Context, Result};
use calamine::{open_workbook_auto, Data, Reader};
use std::borrow::Cow;
use std::path::Path;

/// 从单次读取的 BIFF `Workbook` 流中还原列宽、行高与预去重样式，避免多次整包重读 `.xls`。
fn apply_xls_meta_stream(stream: &[u8], wb: &mut Workbook) {
    let sizes = crate::xls_layout::parse(stream);
    for (i, sheet) in wb.sheets.iter_mut().enumerate() {
        let Some(sz) = sizes.sheets.get(i) else { continue };
        for (a, b, w) in sz.cols.iter() {
            if b.saturating_sub(*a) < 1024 {
                sheet.cols.set_many((*a..=*b).map(|c| (c, *w)));
            } else {
                sheet.cols.set_default(*w);
            }
        }
        sheet.rows.set_many(sz.rows.iter().map(|(r, h)| (*r, *h)));
    }
    crate::xls_style::apply_stream_to_workbook(stream, wb);
}

/// .xls 的样式/列宽/行高/批注（供内置 BIFF 兜底分支单次复用）
fn apply_xls_meta_fallback(path: &Path, stream: &[u8], wb: &mut Workbook) {
    apply_xls_meta_stream(stream, wb);
    if let Ok(all) = crate::notes::load_notes(path) {
        for sheet in wb.sheets.iter_mut() {
            if let Some(list) = all.get(&sheet.name) {
                for n in list.iter() {
                    sheet.notes.insert(crate::model::CellRef::new(n.r, n.c), n.clone());
                }
            }
        }
    }
}
#[path = "xlsx_load.rs"]
pub(crate) mod sheets;
#[path = "xlsx_delta.rs"]
mod delta;
#[path = "xlsx_fresh.rs"]
mod fresh;
#[path = "xlsx_dynamic.rs"]
mod dynamic;
pub(crate) use delta::SourceCells;
#[path = "xlsx_structure.rs"]
mod structure;
#[path = "xlsx_pivots.rs"]
mod pivots;

fn data_to_value(d: &Data) -> Value {
    match d {
        Data::Empty => Value::Empty,
        Data::String(s) => Value::Text(s.as_str().into()),
        Data::Float(f) => Value::Number(*f),
        Data::Int(i) => Value::Number(*i as f64),
        Data::Bool(b) => Value::Bool(*b),
        Data::DateTime(dt) => Value::Number(dt.as_f64()),
        Data::DateTimeIso(s) => Value::Text(s.as_str().into()),
        Data::DurationIso(s) => Value::Text(s.as_str().into()),
        // 低层错误类型要映射回我们自己的 ErrKind，不能 Debug 拼字符串：
        // `Data::Error` 的 Debug 输出和我们显示/存盘用的文本并不是一回事。
        Data::Error(e) => match e {
            calamine::CellErrorType::Div0 => Value::Error(crate::formula::ErrKind::Div0),
            calamine::CellErrorType::Name => Value::Error(crate::formula::ErrKind::Name),
            calamine::CellErrorType::NA => Value::Error(crate::formula::ErrKind::Na),
            calamine::CellErrorType::Num => Value::Error(crate::formula::ErrKind::Num),
            calamine::CellErrorType::Value => Value::Error(crate::formula::ErrKind::Value),
            calamine::CellErrorType::Ref => Value::Error(crate::formula::ErrKind::Ref),
            calamine::CellErrorType::Null => Value::Error(crate::formula::ErrKind::Null),
            _ => Value::Error(crate::formula::ErrKind::Value),
        },
    }
}

/// 读取 xlsx/xls/ods（按扩展名自动识别）

/// 一次加载中发现的问题。`fatal` 表示"覆盖保存会丢掉用户的真实数据"，
/// 调用方（应用层）必须据此禁止普通覆盖保存，只能只读查看或另存抢救副本。
#[derive(Clone, Debug, PartialEq)]
pub struct LoadProblem {
    /// 工作表名（部件级问题为 None）
    pub sheet: Option<String>,
    /// 出问题的部件或环节，例如 `xl/worksheets/sheet1.xml`、`styles`、`sharedStrings`
    pub part: String,
    /// 丢了什么：单元格数据 / 公式 / 样式 / 关系 …
    pub what: String,
    pub detail: String,
    pub fatal: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadDiag {
    pub problems: Vec<LoadProblem>,
}

impl LoadDiag {
    pub fn push(&mut self, sheet: Option<&str>, part: &str, what: &str, detail: String, fatal: bool) {
        self.problems.push(LoadProblem {
            sheet: sheet.map(|s| s.to_string()),
            part: part.into(),
            what: what.into(),
            detail,
            fatal,
        });
    }
    pub fn is_complete(&self) -> bool {
        self.problems.is_empty()
    }
    pub fn has_fatal(&self) -> bool {
        self.problems.iter().any(|p| p.fatal)
    }
    /// 给状态栏/对话框用的一句话摘要
    pub fn summary(&self) -> String {
        if self.problems.is_empty() {
            return String::new();
        }
        let fatal = self.problems.iter().filter(|p| p.fatal).count();
        let first = &self.problems[0];
        format!(
            "{} 处读取问题（{} 处会丢数据）：{}{} {}",
            self.problems.len(),
            fatal,
            first.sheet.as_deref().map(|s| format!("{s} ")).unwrap_or_default(),
            first.what,
            first.detail
        )
    }
}

/// XLS/ODS readers expose values, styles and formulas as separate ranges.
fn load_legacy_sheet(
    reader: &mut calamine::Sheets<std::io::BufReader<std::fs::File>>,
    sheet: &mut Sheet,
    styles: &mut crate::style::StyleTable,
    diag: &mut LoadDiag,
) {
    let name = sheet.name.clone();
    let part = sheet.source_part.clone().unwrap_or_default();
    match reader.worksheet_range(&name) {
        Ok(range) => {
            let (r0, c0) = range.start().unwrap_or((0, 0));
            for (row, col, data) in range.cells() {
                let value = data_to_value(data);
                if !value.is_empty() {
                    sheet.set(r0 + row as u32, c0 + col as u32, Cell { value, formula: None });
                }
            }
        }
        Err(e) => diag.push(Some(&name), &part, "全部单元格数据", format!("{e:#}"), true),
    }
    if let calamine::Sheets::Xls(x) = reader {
        if let Some(regions) = x.worksheet_merge_cells(&name) {
            sheet.merges = regions
                .iter()
                .map(|d| (d.start.0, d.start.1, d.end.0, d.end.1))
                .collect();
        }
    } else {
        match reader.worksheet_style(&name) {
            Ok(range) => {
                for (row, col, style) in range.cells() {
                    let id = styles.intern(convert_style(style));
                    if id != 0 { sheet.set_style_loaded(row, col, id); }
                }
            }
            Err(e) => diag.push(Some(&name), &part, "单元格样式", format!("{e:#}"), true),
        }
    }
    match reader.worksheet_formula(&name) {
        Ok(range) => {
            let (r0, c0) = range.start().unwrap_or((0, 0));
            for (row, col, f) in range.cells().filter(|(_, _, f)| !f.is_empty()) {
                let r = r0 + row as u32;
                let c = c0 + col as u32;
                let mut cell = sheet.get(r, c).map(|c| c.into_owned()).unwrap_or_default();
                let f = f.strip_prefix('=').unwrap_or(f);
                cell.formula = Some(crate::normalize_formula(&format!("={f}")).into());
                sheet.set(r, c, cell);
            }
        }
        Err(e) => diag.push(Some(&name), &part, "全部公式", format!("{e:#}"), true),
    }
}

/// Read values, cached results and shared-formula placements together. This
/// iterator visits physical cells even when the declared dimension is absent.
fn load_xlsx_cells(
    book: &mut calamine::Xlsx<std::io::BufReader<std::fs::File>>,
    sheet: &mut Sheet,
) -> Result<()> {
    use calamine::XlsxFormulaMetadata as F;
    let mut reader = match book.worksheet_cells_reader(&sheet.name) {
        Err(calamine::XlsxError::NotAWorksheet(_)) => return Ok(()),
        result => result?,
    };
    let mut masters = std::collections::HashMap::<usize, ((u32, u32), String)>::new();
    let mut children = Vec::new();
    while let Some(record) = reader.next_cell_with_formula_metadata()? {
        let (row, col) = record.pos;
        let formula = match record.formula {
            Some(F::Shared { shared_index, formula, .. }) => {
                masters.insert(shared_index, (record.pos, formula.clone()));
                Some(formula)
            }
            Some(F::SharedDerived { shared_index }) => {
                children.push((row, col, shared_index));
                None
            }
            Some(F::Normal { formula }) => Some(formula),
            Some(_) => anyhow::bail!("无法读取的公式类型"),
            None => None,
        };
        let value = match record.value {
            calamine::DataRef::SharedString(text) => Value::Text(text.into()),
            calamine::DataRef::String(text) => Value::Text(text.into()),
            other => data_to_value(&other.into()),
        };
        let formula = formula.filter(|f| !f.is_empty()).map(|f| {
            crate::normalize_formula(&format!("={}", f.trim_start_matches('='))).into()
        });
        let cell = Cell { value, formula };
        if !cell.is_blank() { sheet.set(row, col, cell); }
    }
    for (row, col, index) in children {
        let ((r, c), master) = masters.get(&index).context("共享公式缺少主单元格")?;
        let translated = crate::refs::translate(master, i64::from(row) - i64::from(*r), i64::from(col) - i64::from(*c), &sheet.name);
        let mut cell = sheet.get(row, col).map(|c| c.into_owned()).unwrap_or_default();
        cell.formula = Some(crate::normalize_formula(&format!("={translated}")).into());
        sheet.set(row, col, cell);
    }
    Ok(())
}

/// Office 加密文件是 CFB 容器，里面有一个叫 EncryptedPackage 的流。
/// 直接通过 CFB 扇区目录表跳转定位（~1.5KB），避免打开大型 `.xls` 时全盘逐块扫描。
fn is_password_encrypted(path: &Path) -> bool {
    crate::cfb::has_encrypted_package_file(path)
}

pub fn load(path: &Path) -> Result<Workbook> {
    load_diag(path).map(|(wb, _)| wb)
}

/// 读取工作簿并带回**结构化诊断**（C14）：哪些部件没读全、丢了什么、是否致命。
/// `load()` 是它的忽略诊断包装，老调用点行为不变。
/// 上一次 load_diag 实际走的读取分支（集成测试用来证明没有误走 OOXML）。
/// 用线程局部存储：并行测试各自加载不同文件时不会互相覆盖。
std::thread_local! {
    static LAST_LOAD_KIND: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// 当前线程上一次加载实际走的分支。
pub fn last_load_kind() -> u8 {
    LAST_LOAD_KIND.with(|c| c.get())
}

fn set_load_kind(kind: u8) {
    LAST_LOAD_KIND.with(|c| c.set(kind));
}

/// 故障注入按“文件路径”登记，而不是进程级环境变量：
/// 同一测试进程里并行跑的其他加载不会被误伤（环境变量是全局的，会串台）。
static INJECT_WORKER_PANIC: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());
static INJECT_SIDE_PANIC: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

fn canonical_or_self(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 测试用：让指定路径的主加载线程 panic。
pub fn inject_worker_panic(path: &std::path::Path) {
    INJECT_WORKER_PANIC
        .lock()
        .unwrap()
        .push(canonical_or_self(path));
}

/// 测试用：让指定路径的旁路扫描线程 panic。
pub fn inject_side_panic(path: &std::path::Path) {
    INJECT_SIDE_PANIC.lock().unwrap().push(canonical_or_self(path));
}

/// 测试用：清空全部注入（用例结束必须调用）。
pub fn clear_fault_injections() {
    INJECT_WORKER_PANIC.lock().unwrap().clear();
    INJECT_SIDE_PANIC.lock().unwrap().clear();
}

fn injected(list: &std::sync::Mutex<Vec<std::path::PathBuf>>, path: &std::path::Path) -> bool {
    let p = canonical_or_self(path);
    list.lock().unwrap().iter().any(|x| *x == p)
}
pub const LOAD_KIND_UNKNOWN: u8 = 0;
pub const LOAD_KIND_BIFF_CALAMINE: u8 = 1;
pub const LOAD_KIND_BIFF_BUILTIN: u8 = 2;
pub const LOAD_KIND_OOXML: u8 = 3;

pub const LOAD_CANCELLED_ERR: &str = "LOAD_CANCELLED";

/// 异步打开工作簿时的字节级进度与取消控制柄。
#[derive(Debug, Default)]
pub struct LoadControl {
    pub done: std::sync::atomic::AtomicU64,
    pub total: std::sync::atomic::AtomicU64,
    pub cancel: std::sync::atomic::AtomicBool,
}

impl LoadControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn progress(&self) -> Option<f32> {
        let total = self.total.load(std::sync::atomic::Ordering::Relaxed);
        if total == 0 {
            return None;
        }
        let done = self.done.load(std::sync::atomic::Ordering::Relaxed);
        Some((done as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }
}

pub fn is_load_cancelled_err(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.to_string().contains(LOAD_CANCELLED_ERR))
}

pub fn load_diag(path: &Path) -> Result<(Workbook, LoadDiag)> {
    load_diag_with_control(path, None)
}

pub fn load_diag_with_control(
    path: &Path,
    ctrl: Option<std::sync::Arc<LoadControl>>,
) -> Result<(Workbook, LoadDiag)> {
    set_load_kind(LOAD_KIND_UNKNOWN);
    if ctrl.as_ref().is_some_and(|c| c.is_cancelled()) {
        anyhow::bail!(LOAD_CANCELLED_ERR);
    }
    if injected(&INJECT_WORKER_PANIC, path) {
        panic!("注入：主加载线程 panic");
    }
    let mut diag = LoadDiag::default();
    if is_password_encrypted(path) {
        anyhow::bail!(
            "这个工作簿设置了打开密码（Office 加密），当前版本还不支持解密；\
             请先在 Excel 里取消密码后另存一份"
        );
    }
    // .xls：先让 calamine 读（它对真实 Excel 文件最稳），读不了再走内置 BIFF 读取器
    //（我们自己写出的容器就属于后一种）。
    let is_ooxml = crate::crypto::has_magic(path, b"PK");
    let archive_bytes = is_ooxml.then(|| sheets::ArchiveBytes::open(path)).transpose()?;
    let (mut shared_zip, package_stamp) = match archive_bytes.as_ref() {
        Some(ab) => {
            let (z, s) = sheets::open_bytes_stamped(ab.as_slice(), true)?;
            (Some(z), s)
        }
        None => (None, None),
    };
    let (
        zip_has_vba,
        zip_has_xlinks,
        zip_has_drawing,
        zip_has_table,
        zip_has_chart,
        zip_has_comments,
        zip_has_styles,
        zip_has_theme,
        zip_has_custom_props,
        zip_has_sheet_rels,
    ) = if let Some(zip) = shared_zip.as_ref() {
        let (
            mut vba,
            mut xlinks,
            mut drawing,
            mut table,
            mut chart,
            mut comments,
            mut styles,
            mut theme,
            mut custom_props,
            mut sheet_rels,
        ) = (false, false, false, false, false, false, false, false, false, false);
        for name in zip.file_names() {
            if name == "xl/vbaProject.bin" {
                vba = true;
            } else if name == "xl/styles.xml" {
                styles = true;
            } else if name == "docProps/custom.xml" {
                custom_props = true;
            } else if name.starts_with("xl/theme/") {
                theme = true;
            } else if name.contains("worksheets/_rels/") {
                sheet_rels = true;
            } else if name.contains("externalLink") {
                xlinks = true;
            } else if name.contains("drawing") {
                drawing = true;
            } else if name.contains("tables/table") {
                table = true;
            } else if name.contains("charts/chart") {
                chart = true;
            } else if name.contains("comments") || name.contains("threadedComment") {
                comments = true;
            }
        }
        (
            vba,
            xlinks,
            drawing,
            table,
            chart,
            comments,
            styles,
            theme,
            custom_props,
            sheet_rels,
        )
    } else {
        (false, false, false, false, false, false, false, false, false, false)
    };
    let sheets::WorkbookMetadata { xml: workbook_xml, targets, header } = shared_zip.as_mut()
        .map(sheets::workbook_metadata).transpose()
        .with_context(|| format!("无法打开 {} 的工作簿元数据和样式", path.display()))?
        .unwrap_or_default();
    let mut reader = if header.is_none() {
        match open_workbook_auto(path) {
            Ok(reader) => Some(reader),
            Err(e) => {
                if !crate::crypto::has_magic(path, &[0xD0, 0xCF, 0x11, 0xE0]) {
                    return Err(e).with_context(|| format!("无法打开 {}", path.display()));
                }
                eprintln!("[load] calamine 打不开，改用内置 BIFF 读取器：{e}");
                set_load_kind(LOAD_KIND_BIFF_BUILTIN);
                if let Some(stream) = crate::cfb::read_workbook_stream_file(path) {
                    let mut wb = crate::xls_read::load_from_stream(path, &stream)?;
                    apply_xls_meta_fallback(path, &stream, &mut wb);
                    return Ok((wb, diag));
                }
                let mut wb = crate::xls_read::load(path)?;
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(cf) = crate::cfb::Compound::open(bytes) {
                        if let Some(stream) = cf.stream_by_path("Workbook").or_else(|| cf.stream_by_path("Book")) {
                            apply_xls_meta_fallback(path, &stream, &mut wb);
                        }
                    }
                }
                return Ok((wb, diag));
            }
        }
    } else {
        None
    };
    let names = match &header {
        Some(header) => header.names.clone(),
        None => reader.as_ref().unwrap().sheet_names(),
    };

    let is_cfb = !is_ooxml && crate::crypto::has_magic(path, &[0xD0, 0xCF, 0x11, 0xE0]);
    let mut defined_names = crate::patch::defined_names_xml(&workbook_xml);
    let manual_hidden = crate::patch::manual_hidden_intervals_from_names(&defined_names);
    defined_names.retain(|(name, _, _)| name != crate::patch::MANUAL_HIDDEN_NAME);
    let has_source_names = defined_names.iter().any(|(n, _, _)| n.starts_with("_sc_src_"))
        || workbook_xml.contains("_sc_src_");
    let (calc_iterate, calc_max_iterations, calc_max_change) =
        crate::patch::calc_iteration_xml(&workbook_xml).unwrap_or((false, 100, 0.001));
    let mut wb = Workbook {
        load_incomplete: false,
        names: defined_names,
        // 数据源存在 `docProps/custom.xml` 或旧版 `_sc_src_*` 名称里，按需读出来重建
        sources: if is_ooxml && (zip_has_custom_props || has_source_names) {
            crate::external::read_sources(path)
        } else {
            Vec::new()
        },
        links: if is_ooxml && (zip_has_xlinks || workbook_xml.contains("externalReference")) {
            crate::xlinks::read(path)
        } else {
            Vec::new()
        },
        names_dirty: false,
        pivot_shifts: Vec::new(),
        pivot_cache_base: Default::default(),
        pivot_session: Default::default(),
        has_vba: if is_ooxml { zip_has_vba } else { false },
        date_system: if crate::patch::date1904_xml(&workbook_xml) {
            crate::model::DateSystem::D1904
        } else {
            crate::model::DateSystem::D1900
        },
        pivot_calc_fields: Vec::new(),
        pivots_pending: Vec::new(),
        pivots_pending2: Vec::new(),
        pivots: Vec::new(),
        pivots_removed_parts: Vec::new(),
        calc_manual: crate::patch::calc_manual_xml(&workbook_xml),
        // calcPr/@iterate / maxIterations / maxChange（缺省 = 关闭 / 100 / 0.001）
        calc_iterate,
        calc_max_iterations,
        calc_max_change,
        calc_pending: false,
        calc_pending_sheets: std::collections::BTreeSet::new(),
        recalc_cell_revs: Vec::new(),
        sheets: Vec::new(),
        path: Some(path.to_path_buf()),
        decrypted: None,
        encryption: None,
        source_encryption: None,
        protection: crate::protection::parse_workbook_tag(&workbook_xml),
        protection_dirty: false,
        dirty: false,
        styles: crate::style::StyleTable::new(),
        chart_edits: Vec::new(),
        audit: crate::audit::WorkbookAudit::default(),
    };

    set_load_kind(if is_ooxml { LOAD_KIND_OOXML } else { LOAD_KIND_BIFF_CALAMINE });

    let prof_load = std::env::var("SC_LOAD_PROF").is_ok();
    let t_all_load = std::time::Instant::now();
    // 图片、表格对象和批注从独立部件读取，与工作表 XML 的流式解析并行。
    let need_side = is_ooxml
        && (zip_has_drawing
            || zip_has_table
            || zip_has_comments
            || zip_has_sheet_rels
            || injected(&INJECT_SIDE_PANIC, path));
    let (side_tx, side_rx) = std::sync::mpsc::channel();
    if need_side {
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            if injected(&INJECT_SIDE_PANIC, &p) {
                panic!("注入：旁路扫描线程 panic");
            }
            let out = (
                if zip_has_drawing || zip_has_sheet_rels { crate::pic::load(&p) } else { Default::default() },
                if zip_has_table || zip_has_sheet_rels { crate::table::load(&p) } else { Default::default() },
                if zip_has_comments || zip_has_sheet_rels {
                    crate::notes::load_notes(&p).unwrap_or_default()
                } else {
                    Default::default()
                },
            );
            let _ = side_tx.send(out);
        });
    }
    let t_all = std::time::Instant::now();
    // 仅在未能走单遍 quick-xml 快速头解析（header.is_none()）时才回退多遍旧解析路径。
    let (theme_book, ext_book, ooxml_styles, fallback_dxfs, fallback_dxf_ext) = if header.is_some() {
        (
            crate::style_theme::ThemeBook::default(),
            crate::style_ext::StyleExt::default(),
            Vec::new(),
            Vec::new(),
            false,
        )
    } else if is_ooxml {
        let styles_xml = if zip_has_styles {
            shared_zip
                .as_mut()
                .and_then(|zip| read_zip_text(zip, "xl/styles.xml").ok())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let theme_book = if zip_has_styles || zip_has_theme {
            crate::style_theme::load(path, &[])
        } else {
            Default::default()
        };
        let ext_book = if !styles_xml.is_empty() {
            crate::style_ext::parse(&styles_xml)
        } else {
            Default::default()
        };
        let ooxml_styles = if let Some(calamine::Sheets::Xlsx(xr)) = &mut reader {
            names
                .iter()
                .find_map(|n| xr.worksheet_cells_reader(n).ok().map(|cr| cr.styles().to_vec()))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let dxfs = crate::cf::parse_dxfs_with_theme(&styles_xml, &theme_book);
        let dxf_ext = crate::cf::dxfs_unsupported(&styles_xml);
        (theme_book, ext_book, ooxml_styles, dxfs, dxf_ext)
    } else {
        (
            crate::style_theme::ThemeBook::default(),
            crate::style_ext::StyleExt::default(),
            Vec::new(),
            Vec::new(),
            false,
        )
    };
    let mut style_ids = std::collections::HashMap::<u16, u16>::new();
    let mut page_names = std::collections::HashMap::new();
    if workbook_xml.contains("definedName") {
        for i in 0..names.len() as u32 {
            let settings = crate::page::parse_defined_names(&workbook_xml, i);
            if settings != (None, None, None) {
                page_names.insert(i, settings);
            }
        }
    }
    let states = crate::patch::sheet_visibility_xml(&workbook_xml);
    drop(workbook_xml);
    let mut clean_fast_meta_outer: std::collections::HashMap<
        String,
        (
            Option<std::sync::Arc<str>>,
            Option<std::sync::Arc<[std::sync::Arc<str>]>>,
        ),
    > = std::collections::HashMap::new();
    let mut layouts = std::collections::HashMap::new();
    if let Some(header) = &header {
        let style_lookup = sheets::StyleLookup::new(
            std::mem::replace(&mut wb.styles, crate::style::StyleTable::new()),
            style_ids,
            &header.parsed_styles,
        );
        let loaded_res = sheets::worksheets(
            Some(path),
            shared_zip.as_mut().unwrap(),
            header,
            &targets,
            &style_lookup,
            &header.parsed_styles.dxfs,
            header.parsed_styles.dxf_ext,
            zip_has_drawing || zip_has_sheet_rels,
            ctrl.as_deref(),
        );
        let (styles_back, ids_back) = style_lookup.into_inner();
        wb.styles = styles_back;
        style_ids = ids_back;
        let loaded = loaded_res?;
        for mut part in loaded {
            if part.clean_shell.is_some() {
                clean_fast_meta_outer.insert(
                    part.sheet.name.clone(),
                    (part.clean_shell.take(), part.sst_arcs.take()),
                );
            }
            if let Some(layout) = part.layout {
                layouts.insert(part.sheet.name.clone(), layout);
            }
            if let Some((area, rows, cols)) = page_names.get(&(wb.sheets.len() as u32)) {
                part.sheet.page.print_area = *area;
                part.sheet.page.title_rows = *rows;
                part.sheet.page.title_cols = *cols;
            }
            part.sheet.loading = false;
            part.sheet.edited = false;
            diag.problems.extend(part.diag.problems);
            wb.sheets.push(part.sheet);
        }
    } else if let Some(reader) = &mut reader {
        let mut intern_xf = |idx: u16, styles: &mut crate::style::StyleTable| {
            *style_ids.entry(idx).or_insert_with(|| {
                let mut cs = ooxml_styles.get(idx as usize).map(convert_style)
                    .unwrap_or_else(|| crate::style::CellStyle { size: 11.0, color: [0, 0, 0], ..Default::default() });
                let xf = theme_book.xfs.get(idx as usize);
                let resolved = theme_book.resolved.get(idx as usize);
                crate::style_theme::apply_resolved(&mut cs, xf, resolved, &theme_book.palette);
                cs.diagonal = ext_book.xf_border.get(idx as usize)
                    .and_then(|id| ext_book.diags.get(*id as usize)).cloned().flatten();
                let id = styles.intern(cs);
                if let Some(xf) = xf {
                    styles.set_theme(id, *xf);
                }
                crate::style_ext::attach(styles, &ext_book, id, idx);
                id
            })
        };
        for name in &names {
            let t_sheet = std::time::Instant::now();
            let mut sheet = Sheet::new(name.clone());
            sheet.source_part = targets.get(name).cloned();
            sheet.loading = true;
            if let calamine::Sheets::Xlsx(book) = reader {
                if let Err(err) = load_xlsx_cells(book, &mut sheet) {
                    diag.push(Some(name), sheet.source_part.as_deref().unwrap_or("xl/worksheets"),
                        "稀疏单元格数据和公式", format!("{err:#}"), true);
                }
            } else {
                load_legacy_sheet(reader, &mut sheet, &mut wb.styles, &mut diag);
            }
            if let (Some(zip), Some(part)) = (shared_zip.as_mut(), targets.get(name)) {
                match read_zip_text(zip, part) {
                    Ok(xml) => {
                        let shell = crate::style_theme::visit_cell_styles(&xml, |r, c, xf| {
                            let id = intern_xf(xf, &mut wb.styles);
                            sheet.styles.insert(crate::CellRef::new(r, c), id);
                        });
                        match shell {
                            Ok((shell, implicit_coordinates)) => {
                                sheet.source_needs_dom_repair |= implicit_coordinates;
                                layouts.insert(name.clone(), crate::merges::sheet_layout_with_theme(&shell, &fallback_dxfs, fallback_dxf_ext, &theme_book));
                                sheet.page = crate::page::parse_page_setup(&shell);
                                sheet.textboxes = crate::textbox::load_from_sheet(path, part, &shell);
                                sheet.hyperlinks = crate::hyperlinks::load_sheet(path, part, &shell).into();
                                if shell.contains("sparkline") {
                                    sheet.sparklines = crate::sparkline::parse_sheet(&shell);
                                    sheet.sparklines.sort_by_key(|s| (s.c, s.r));
                                }
                            }
                            Err(e) => diag.push(Some(name), part, "单元格样式和版面", format!("{e:#}"), true),
                        }
                        if xml.contains("array") {
                            sheet.arrays = crate::arr::scan_xml(&xml).into_iter().map(|a| (
                                crate::model::CellRef::new(a.anchor.0, a.anchor.1), a.range,
                            )).collect();
                        }
                        if xml.contains("inlineStr") && (xml.contains("<r>") || xml.contains("<r ")) {
                            sheet.rich = crate::richtext::inline_cells(&xml).into();
                        }
                    }
                    Err(e) => diag.push(Some(name), part, "工作表属性和样式", format!("{e:#}"), true),
                }
                if let Some((a, r, c)) = page_names.get(&(wb.sheets.len() as u32)) {
                    sheet.page.print_area = *a;
                    sheet.page.title_rows = *r;
                    sheet.page.title_cols = *c;
                }
            }
            sheet.loading = false;
            sheet.edited = false;
            if prof_load {
                eprintln!("[load] {:<16} {:>7} 格  {:.2}s", sheet.name, sheet.len(), t_sheet.elapsed().as_secs_f64());
            }
            wb.sheets.push(sheet);
        }
    }
    if prof_load {
        eprintln!("[load] 逐表读取合计 {:.2}s", t_all.elapsed().as_secs_f64());
    }

    let t_side = std::time::Instant::now();
    let (pics, tables, side_notes) = if need_side {
        side_rx.recv().unwrap_or_else(|_| {
            diag.push(None, "xl/worksheets（版面扫描）", "图片/表格",
                "旁路扫描线程异常退出，这些结构没有加载".into(), true);
            Default::default()
        })
    } else { Default::default() };
    if prof_load {
        eprintln!("[load] 旁路扫描等待 {:.2}s", t_side.elapsed().as_secs_f64());
    }

    if wb.sheets.is_empty() {
        wb.sheets.push(Sheet::new("Sheet1"));
    }
    // 图片：drawing → pic → media
    {
        for sheet in wb.sheets.iter_mut() {
            if let Some(list) = pics.get(&sheet.name) {
                sheet.pictures = list.clone();
            }
        }
    }
    // 表格对象（ListObject）：独立的 tableN.xml 部件
    {
        for sheet in wb.sheets.iter_mut() {
            if let Some(list) = tables.get(&sheet.name) {
                sheet.tables = list.clone();
                // Excel 表格对象自带筛选；表格 XML 里的 autoFilter 不落到工作表节点，
                // 这里补成区域筛选，否则打开文件看不到漏斗按钮。
                if sheet.filter.is_none() {
                    if let Some(tb) = list.iter().find(|tb| tb.header_row) {
                        let (r1, c1, r2, c2) = tb.range;
                        let last = if tb.totals_row { r2.saturating_sub(1) } else { r2 };
                        if last > r1 {
                            sheet.filter =
                                Some(crate::sort_filter::Filter::new(r1, c1, last, c2));
                        }
                    }
                }
            }
        }
    }
    if is_cfb {
        if let Some(xls_stream) = crate::cfb::read_workbook_stream_file(path) {
            apply_xls_meta_stream(&xls_stream, &mut wb);
            wb.calc_manual = crate::xls_read::read_calc_manual_stream(&xls_stream);
            if crate::xls_read::read_date1904(&xls_stream) {
                wb.date_system = crate::model::DateSystem::D1904;
            }
        }
    }

    // 还原工作表布局和合并区域
    {
        let wb_sheets = &mut wb.sheets;
        let wb_styles = &mut wb.styles;
        let mut loaded_style_id = |idx: u16, styles: &mut crate::style::StyleTable| {
            *style_ids.entry(idx).or_insert_with(|| {
                if let Some(h) = &header {
                    let (cs, theme_xf, pattern, quote) = h.parsed_styles.resolve_xf(idx as usize);
                    let id = styles.intern(cs);
                    if (idx as usize) < h.parsed_styles.xf_count() {
                        styles.set_theme(id, theme_xf);
                    }
                    if let Some(p) = pattern {
                        styles.set_pattern(id, p);
                    }
                    if quote {
                        styles.set_quote(id, true);
                    }
                    id
                } else {
                    let mut cs = ooxml_styles.get(idx as usize).map(convert_style)
                        .unwrap_or_else(|| crate::style::CellStyle { size: 11.0, color: [0, 0, 0], ..Default::default() });
                    let xf = theme_book.xfs.get(idx as usize);
                    let resolved = theme_book.resolved.get(idx as usize);
                    crate::style_theme::apply_resolved(&mut cs, xf, resolved, &theme_book.palette);
                    cs.diagonal = ext_book.xf_border.get(idx as usize)
                        .and_then(|id| ext_book.diags.get(*id as usize)).cloned().flatten();
                    let id = styles.intern(cs);
                    if let Some(xf) = xf {
                        styles.set_theme(id, *xf);
                    }
                    crate::style_ext::attach(styles, &ext_book, id, idx);
                    id
                }
            })
        };
        for sheet in wb_sheets.iter_mut() {
            if let Some(lay) = layouts.get(&sheet.name) {
                sheet.source_needs_dom_repair |= lay.invalid_column_bounds;
                // .xls 没有 XML 版面，合并区来自 apply_xls_meta，别被空值覆盖
                if !lay.merges.is_empty() || !is_cfb {
                    sheet.merges = lay.merges.clone();
                }
                if let Some(w) = lay.col_default {
                    sheet.cols.set_default(w);
                }
                // 整表默认行高（sheetFormatPr）：不读会让每行都差 1px，打印越往下越偏
                if let Some(h) = lay.row_default {
                    sheet.rows.set_default(h);
                }
                sheet.cols.set_many(lay.col_widths.iter().map(|(c, w)| (*c, *w)));
                sheet.rows.set_many(lay.row_heights.iter().map(|(r, h)| (*r, *h)));
                if !lay.hidden_cols.is_empty() {
                    sheet.hidden_cols = lay.hidden_cols.iter().copied().collect();
                }
                if !lay.hidden_rows.is_empty() {
                    sheet.hidden_rows_user = lay.hidden_rows.iter().copied().collect();
                }
                for (r, lv) in lay.row_levels.iter() {
                    sheet.row_levels.insert(*r, *lv);
                }
                for (r, c) in lay.ignored_errors.iter() {
                    sheet.ignored_errors.insert((*r, *c));
                }
                if lay.scenario_settings.has_source() || !lay.scenarios.is_empty() {
                    sheet.scenarios = lay.scenarios.clone();
                    sheet.scenario_settings = lay.scenario_settings.clone();
                }
                if let Some((below, right, symbols)) = lay.outline_pr {
                    sheet.outline_summary_below = below;
                    sheet.outline_summary_right = right;
                    sheet.outline_symbols = symbols;
                }
                for (c, lv) in lay.col_levels.iter() {
                    sheet.col_levels.insert(*c, *lv);
                }
                for &(pos, xf) in &lay.col_xfs {
                    if let Ok(xf) = u16::try_from(xf) {
                        let id = loaded_style_id(xf, wb_styles);
                        if id != 0 {
                            sheet.set_col_style_loaded(pos, id);
                        }
                    }
                }
                for &(pos, xf) in &lay.row_xfs {
                    if let Ok(xf) = u16::try_from(xf) {
                        let id = loaded_style_id(xf, wb_styles);
                        if id != 0 {
                            sheet.set_row_style_loaded(pos, id);
                        }
                    }
                }
                if is_ooxml && (!sheet.row_styles.is_empty() || !sheet.col_styles.is_empty()) {
                    sheet.apply_loaded_xf_zero(loaded_style_id(0, wb_styles));
                }
                sheet.tab_color = lay.tab_color;
                sheet.protection = lay.protection.clone();
                sheet.protected_ranges = lay.protected_ranges.clone();
                if let Some(z) = lay.zoom_scale {
                    sheet.zoom_scale = z;
                }
                if let Some(g) = lay.show_gridlines {
                    sheet.show_gridlines = g;
                }
                if let Some(h) = lay.show_headings {
                    sheet.show_headings = h;
                }
                sheet.cf.blocks = lay.cf.clone();
                sheet.cf.dxfs = lay.dxfs.clone();
                sheet.cf.unknown_ext = lay.cf_ext;
                sheet.cf.dxf_unsupported = lay.dxf_ext;
                sheet.dv = lay.dv.clone();
                sheet.freeze = lay.freeze;
                // 表格对象已经在上面补过筛选；工作表的 autoFilter 只在存在时覆盖，
                // 否则会把表格自带的筛选清成 None（打开后没有漏斗）。
                if lay.filter.is_some() {
                    sheet.filter = lay.filter.clone();
                }
                if let Some(f) = &sheet.filter {
                    if !f.allows.is_empty()
                        || !f.criteria.is_empty()
                        || !f.date_groups.is_empty()
                        || !f.color_filters.is_empty()
                    {
                        if let Some(map) = &manual_hidden {
                            let user_ivs = map.get(&sheet.name).cloned().unwrap_or_default();
                            if user_ivs.is_empty() && !sheet.hidden_rows_user.is_empty() {
                                let (r1, _, r2, _) = f.range;
                                sheet.hidden_rows = crate::layout::HiddenIndices::from_intervals(
                                    clip_intervals(sheet.hidden_rows_user.intervals(), r1.saturating_add(1), r2),
                                );
                                sheet.hidden_rows_user.clear();
                            } else {
                                sheet.hidden_rows = f.hidden_row_intervals_with_styles(
                                    sheet,
                                    wb_styles,
                                    wb.date_system,
                                );
                                sheet.hidden_rows_user =
                                    crate::layout::HiddenIndices::from_intervals(user_ivs);
                            }
                        } else {
                            let fh = f.hidden_row_intervals_with_styles(
                                sheet,
                                wb_styles,
                                wb.date_system,
                            );
                            if !sheet.hidden_rows_user.is_empty() && !fh.is_empty() {
                                sheet.hidden_rows_user = crate::layout::HiddenIndices::from_intervals(
                                    subtract_intervals(
                                        sheet.hidden_rows_user.intervals(),
                                        fh.intervals(),
                                    ),
                                );
                            }
                            sheet.hidden_rows = fh;
                        }
                    } else if let Some(map) = &manual_hidden {
                        sheet.hidden_rows_user = crate::layout::HiddenIndices::from_intervals(
                            map.get(&sheet.name).cloned().unwrap_or_default(),
                        );
                    }
                } else if let Some(map) = &manual_hidden {
                    // 本应用保存的手动隐藏行来源：它明确区分了
                    // 筛选隐藏与手动隐藏，包括两者重叠的行，不再靠猜 hidden 。
                    sheet.hidden_rows_user = crate::layout::HiddenIndices::from_intervals(
                        map.get(&sheet.name).cloned().unwrap_or_default(),
                    );
                }
            }
        }
    }

    if prof_load {
        eprintln!("[load] 总计 {:.2}s", t_all_load.elapsed().as_secs_f64());
    }
    // 图表：drawing 是 OOXML 结构，BIFF(.xls) 没有，别拿 zip 扫描器去读
    let charts_res = if is_ooxml && (zip_has_chart || zip_has_drawing || zip_has_sheet_rels) {
        crate::charts::load_charts_all_in(path, &wb.sheets, Some(&wb.styles), wb.date_system)
    } else {
        Ok(Default::default())
    };
    match charts_res {
        Ok(all) => {
            for (idx, sheet) in wb.sheets.iter_mut().enumerate() {
                if let Some(list) = all.get(&(idx as u32)) {
                    sheet.charts = list.clone();
                }
            }
        }
        Err(e) => diag.push(None, "xl/charts", "全部图表", format!("{e:#}"), true),
    }

    // 批注：OOXML 由并发旁路线程读取；BIFF/ODS 在此读取
    if is_ooxml {
        if !side_notes.is_empty() {
            for sheet in wb.sheets.iter_mut() {
                if let Some(list) = side_notes.get(&sheet.name) {
                    for n in list.iter() {
                        sheet.notes.insert(crate::model::CellRef::new(n.r, n.c), n.clone());
                    }
                }
            }
        }
        if let Ok(all_threads) = crate::notes::load_threaded_comments(path) {
            for sheet in wb.sheets.iter_mut() {
                if let Some(list) = all_threads.get(&sheet.name) {
                    for tc in list {
                        sheet
                            .threads
                            .insert(crate::model::CellRef::new(tc.r, tc.c), tc.clone());
                    }
                }
            }
        }
    } else if let Ok(all) = crate::notes::load_notes(path) {
        for sheet in wb.sheets.iter_mut() {
            if let Some(list) = all.get(&sheet.name) {
                for n in list.iter() {
                    sheet.notes.insert(crate::model::CellRef::new(n.r, n.c), n.clone());
                }
            }
        }
    }

    if is_ooxml {
        // Excel 写在 sharedStrings.xml 里的富文本：按整段文字回填
        if header.is_none() && let Some(bytes) = crate::fixups::read_part_file(path, "xl/sharedStrings.xml") {
            if let Ok(xml) = String::from_utf8(bytes) {
                let table = if xml.contains("<r>") || xml.contains("<r ") { crate::richtext::sst_runs(&xml) } else { Vec::new() };
                if !table.is_empty() {
                    let mut map: std::collections::HashMap<String, Vec<crate::richtext::RichRun>> =
                        std::collections::HashMap::new();
                    for (text, runs) in table {
                        if runs.iter().any(|r| !r.is_plain()) {
                            map.entry(text).or_insert(runs);
                        }
                    }
                    if !map.is_empty() {
                        for sheet in wb.sheets.iter_mut() {
                            let hits: Vec<(u32, u32, Vec<crate::richtext::RichRun>)> = sheet
                                .iter()
                                .filter_map(|(cref, cell)| {
                                    if let crate::model::Value::Text(t) = &cell.value {
                                        map.get(&**t)
                                            .map(|r| (cref.row, cref.col, r.clone()))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            for (r, c, runs) in hits {
                                sheet.rich.insert((r, c), runs);
                            }
                        }
                    }
                }
            }
        }
        // 工作表可见性（workbook.xml 的 state="hidden"）
        let delta_styles = delta::StyleMap::new(&style_ids, &wb.styles);
        for sheet in wb.sheets.iter_mut() {
            if let Some(&vis) = states.get(&sheet.name) {
                sheet.hidden = vis != crate::model::SheetVisibility::Visible;
                sheet.very_hidden = vis == crate::model::SheetVisibility::VeryHidden;
            }
            sheet.saved_cells = Some(sheet.cell_version().clone());
            if let (Some(zip), Some(part)) = (shared_zip.as_mut(), sheet.source_part.as_deref()) {
                sheet.source_cells = SourceCells::capture(zip, part, sheet);
                if let Some(source) = &mut sheet.source_cells {
                    source.set_style_map(delta_styles.clone());
                    if let Some((cs, sa)) = clean_fast_meta_outer.remove(&sheet.name) {
                        source.set_clean_fast_meta(cs, sa);
                    }
                }
            }
        }
        wb.pivot_cache_base = std::sync::Arc::new(crate::model::PivotCacheBase {
            definitions: pivot_definitions_with_zip(shared_zip.as_mut(), path),
            sheets: wb.sheets.iter().map(|s| (s.identity, s.name.clone())).collect(),
        });
        if let Some(zip) = shared_zip.as_mut() {
            wb.pivots = crate::pivot_parse::load_pivots_from_zip(zip, &wb);
            wb.audit = crate::audit::read_from_zip(zip);
        }
        if std::env::var("SC_LOAD_PROF").is_ok() {
            eprintln!("[load] 总计 {:.2}s", t_all_load.elapsed().as_secs_f64());
        }
    }
    wb.load_incomplete = diag.has_fatal();
    // C11：合法但引擎尚未实现的引用组合，必须在打开时就给出可见诊断；
    // 重算会保留文件缓存，读入后不把已知结果覆盖成错误。
    for sheet in &wb.sheets {
        for (cref, cell) in sheet.formula_cells() {
            let Some(formula) = cell.formula.as_deref() else {
                continue;
            };
            if let Some(reason) = crate::formula::unsupported_reference_combo(formula) {
                diag.push(
                    Some(&sheet.name),
                    sheet.source_part.as_deref().unwrap_or("xl/worksheets"),
                    "兼容提示",
                    format!(
                        "{} 的公式 {} 使用{reason}，当前版本暂不参与重算；保留文件里的缓存值，可另存为 .xlsx 或改为单工作簿引用。",
                        crate::model::cell_name(cref.row, cref.col),
                        formula
                    ),
                    false,
                );
            }
        }
    }
    drop(shared_zip);
    drop(archive_bytes);
    if let Some(stamp) = package_stamp {
        anyhow::ensure!(sheets::package_stamp(path)? == stamp, "读取期间工作簿已被替换，请重新打开");
    }
    Ok((wb, diag))
}

fn clip_intervals(a: &[(u32, u32)], lo: u32, hi: u32) -> Vec<(u32, u32)> {
    if lo > hi {
        return Vec::new();
    }
    let mut out = Vec::new();
    for &(s, e) in a {
        if e < lo {
            continue;
        }
        if s > hi {
            break;
        }
        out.push((s.max(lo), e.min(hi)));
    }
    out
}

fn subtract_intervals(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    if a.is_empty() || b.is_empty() {
        return a.to_vec();
    }
    let mut out = Vec::with_capacity(a.len());
    let mut j = 0usize;
    for &(mut s, e) in a {
        while j < b.len() && b[j].1 < s {
            j += 1;
        }
        let mut cur_ok = true;
        let mut k = j;
        while k < b.len() && b[k].0 <= e {
            let (bs, be) = b[k];
            if bs > s {
                out.push((s, bs - 1));
            }
            if be >= e {
                cur_ok = false;
                break;
            }
            s = be + 1;
            k += 1;
        }
        if cur_ok && s <= e {
            out.push((s, e));
        }
    }
    out
}

fn pivot_definitions(path: &Path) -> Vec<(String, Vec<u8>)> {
    pivot_definitions_with_zip::<std::fs::File>(None, path)
}

fn pivot_definitions_with_zip<R: std::io::Read + std::io::Seek>(
    shared_zip: Option<&mut zip::ZipArchive<R>>,
    path: &Path,
) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;
    if let Some(zip) = shared_zip {
        let names: Vec<String> = zip
            .file_names()
            .filter(|name| name.contains("pivotCacheDefinition") && name.ends_with(".xml"))
            .map(str::to_owned)
            .collect();
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            if let Ok(mut entry) = zip.by_name(&name) {
                let mut body = Vec::new();
                if entry.read_to_end(&mut body).is_ok() {
                    out.push((name, body));
                }
            }
        }
        return out;
    }
    crate::fixups::parts(path)
        .into_iter()
        .filter(|name| name.contains("pivotCacheDefinition") && name.ends_with(".xml"))
        .filter_map(|name| crate::fixups::read_part_file(path, &name).map(|body| (name, body)))
        .collect()
}

/// Sparse formula count across all sheets, visiting only pages flagged `may_have_formula`.
pub fn workbook_formula_count(wb: &Workbook) -> usize {
    wb.sheets.iter().map(|s| s.formula_cells().count()).sum()
}

/// Build a lightweight formula-only skeleton workbook using sparse `formula_cells()`
/// instead of scanning millions of data cells.
pub fn formula_skeleton(wb: &Workbook) -> Workbook {
    let mut sk = Workbook::new_blank();
    sk.calc_manual = wb.calc_manual;
    sk.calc_pending = wb.calc_pending;
    sk.calc_iterate = wb.calc_iterate;
    sk.load_incomplete = wb.load_incomplete;
    sk.sheets.clear();
    for sh in &wb.sheets {
        let mut s = Sheet::new(sh.name.clone());
        for (cref, cell) in sh.formula_cells() {
            if let Some(f) = &cell.formula {
                s.set(
                    cref.row,
                    cref.col,
                    crate::model::Cell {
                        value: cell.value.clone(),
                        formula: Some(f.clone()),
                    },
                );
            }
        }
        s.spilled = sh.spilled.clone();
        s.spill_shapes = sh.spill_shapes.clone();
        s.arrays = sh.arrays.clone();
        s.tables = sh.tables.clone();
        sk.sheets.push(s);
    }
    sk.names = wb.names.clone();
    sk.links = wb
        .links
        .iter()
        .map(|l| crate::xlinks::ExternalBook {
            index: l.index,
            part: l.part.clone(),
            target: l.target.clone(),
            abs: l.abs.clone(),
            sheets: l.sheets.clone(),
            defined_names: l.defined_names.clone(),
            cache: crate::xlinks::ExternalCache::default(),
            refreshed: l.refreshed,
        })
        .collect();
    sk
}

/// Metadata produced by one successfully replaced OOXML package. It can be
/// applied to a newer live document without resetting edits made during save.
#[derive(Debug, Default)]
pub struct SaveReceipt {
    /// True when sheet cell changes were applied by the verified source-package delta writer.
    pub used_cell_delta: bool,
    sheets: std::collections::HashMap<u64, SavedSheet>,
    source_encryption: Option<crate::crypto::EncryptionSettings>,
    committed_charts: std::collections::HashMap<u64, (Vec<SavedChart>, Vec<(String, String)>)>,
    /// 本次落盘的防篡改审计日志快照（后台保存完成后回写至活动工作簿）
    pub committed_audit: Option<std::sync::Arc<crate::audit::AuditLog>>,
}

#[derive(Debug)]
struct SavedSheet {
    part: String,
    source_needs_dom_repair: bool,
    cells: crate::store::CellVersion,
    source_cells: Option<SourceCells>,
    cleared: crate::model::CellJournal,
}

#[derive(Debug)]
struct SavedChart {
    uid: u64,
    part: String,
    drawing: String,
    anchor: (u32, u32, u32, u32),
    ref_fixes: Vec<(String, String)>,
    created: Option<(crate::charts::Chart, String, String)>,
    saved_content: Option<crate::charts::Chart>,
}

fn chart_content_matches(live: &crate::charts::Chart, saved: &crate::charts::Chart) -> bool {
    let mut a = live.clone();
    let mut b = saved.clone();
    a.anchor = b.anchor;
    a.part = b.part.clone();
    a.drawing = b.drawing.clone();
    a.ref_fixes.clear();
    b.ref_fixes.clear();
    a.content_dirty = b.content_dirty;
    a == b
}

struct RestoreFile(Option<std::path::PathBuf>);
impl Drop for RestoreFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 { let _ = std::fs::remove_file(path); }
    }
}

impl SaveReceipt {
    pub fn apply_to(&self, wb: &mut Workbook) {
        wb.source_encryption.clone_from(&self.source_encryption);
        let mut any_charts_still_dirty = false;
        for sheet in &mut wb.sheets {
            if let Some(saved) = self.sheets.get(&sheet.identity) {
                sheet.source_part = Some(saved.part.clone());
            }
            if let Some((saved, saved_deleted)) = self.committed_charts.get(&sheet.identity) {
                let mut dirty = saved.len() != sheet.charts.len();
                let mut matched_saved = vec![false; saved.len()];
                for (index, chart) in sheet.charts.iter_mut().enumerate() {
                    let found = if !chart.part.is_empty() {
                        saved.iter().enumerate().find(|(i, old)| {
                            !matched_saved[*i] && old.part == chart.part
                        })
                    } else {
                        saved
                            .iter()
                            .enumerate()
                            .find(|(i, old)| {
                                !matched_saved[*i]
                                    && old.created.is_some()
                                    && old.uid != 0
                                    && old.uid == chart.uid.raw()
                            })
                            .or_else(|| {
                                saved.iter().enumerate().find(|(i, old)| {
                                    !matched_saved[*i] && old.part.is_empty() && *i == index
                                })
                            })
                    };
                    let Some((saved_index, baseline)) = found else {
                        dirty = true;
                        continue;
                    };
                    matched_saved[saved_index] = true;
                    if let Some((original, part, drawing)) = &baseline.created {
                        // 新图表已落盘生成真实 OOXML 部件：即使后台保存期间又发生内容编辑，
                        // 也必须回填 part/drawing，避免下次保存重复插入第二张图；
                        // 若期间有新编辑，保留 content_dirty=true 令下次保存原地覆盖该部件。
                        let content_same = chart_content_matches(chart, original);
                        chart.part.clone_from(part);
                        chart.drawing.clone_from(drawing);
                        chart.content_dirty = !content_same;
                        if !content_same {
                            dirty = true;
                        }
                    } else if chart.content_dirty {
                        // 仅当图表内容与本次已落盘快照完全一致时才清脏标；
                        // 后台保存期间若又发生新的图表内容编辑，保留 content_dirty 以便下次保存落盘。
                        if baseline
                            .saved_content
                            .as_ref()
                            .is_some_and(|saved_ch| chart_content_matches(chart, saved_ch))
                        {
                            chart.content_dirty = false;
                        }
                    }
                    if chart.ref_fixes.starts_with(&baseline.ref_fixes) {
                        chart.ref_fixes.drain(..baseline.ref_fixes.len());
                    }
                    dirty |= saved_index != index
                        || baseline.anchor != chart.anchor
                        || (baseline.created.is_none() && baseline.drawing != chart.drawing)
                        || !chart.ref_fixes.is_empty()
                        || chart.content_dirty;
                }
                sheet.deleted_charts.retain(|item| !saved_deleted.contains(item));
                // 首次后台保存期间被用户删除的新图表：由于保存前其 part 为空，删除动作尚未登记到 deleted_charts；
                // 现已落盘为 (drawing, part)，必须补登 deleted_charts 以便下次保存彻底清除，避免重开复活。
                for (saved_index, baseline) in saved.iter().enumerate() {
                    if matched_saved[saved_index] {
                        continue;
                    }
                    if let Some((_, part, drawing)) = &baseline.created
                        && !drawing.is_empty()
                        && !part.is_empty()
                    {
                        let pair = (drawing.clone(), part.clone());
                        if !sheet.deleted_charts.contains(&pair) {
                            sheet.deleted_charts.push(pair);
                        }
                        dirty = true;
                    }
                }
                dirty |= !sheet.deleted_charts.is_empty();
                sheet.charts_dirty = dirty;
                any_charts_still_dirty |= dirty;
            }
        }
        if any_charts_still_dirty {
            wb.dirty = true;
        }
    }

    /// Adopt a completed save as the document's new source file. Exports and
    /// stale in-flight snapshots must not retire the live document's journals.
    pub fn adopt_document(&self, wb: &mut Workbook, target: &Path, calculation_matches: bool) {
        self.apply_to(wb);
        wb.path = Some(target.to_owned());
        if let Some(committed) = &self.committed_audit {
            let live_seq = wb.audit.log.as_ref().map(|l| l.head_seq()).unwrap_or(0);
            if committed.head_seq() >= live_seq {
                wb.audit.log = Some(committed.clone());
                wb.audit.stripped_marker = None;
                wb.audit.parse_error = None;
                wb.audit.strip_on_save = false;
            }
        }
        for sheet in &mut wb.sheets {
            let Some(saved) = self.sheets.get(&sheet.identity) else { continue };
            sheet.saved_cells = Some(saved.cells.clone());
            sheet.source_cells.clone_from(&saved.source_cells);
            sheet.source_needs_dom_repair = saved.source_needs_dom_repair;
            if sheet.cell_version() != &saved.cells {
                continue;
            }
            if wb.dirty || !calculation_matches {
                // Another sheet can be edited during this write. A matching
                // cell version confirms this sheet even across snapshot forks;
                // its live metadata and any newer deletion journal still win.
                if !sheet.has_pending_sheet_metadata() {
                    sheet.edited = false;
                }
                sheet.cleared.retain(|key| !saved.cleared.contains(key));
                continue;
            }
            // These fields caused the OOXML writer to serialize the sheet.
            // Its committed source part now includes those edits and deletions.
            sheet.edited = false;
            sheet.cleared.clear();
            sheet.style_edits.clear();
            sheet.styles_dirty = false;
            sheet.row_styles_dirty = false;
            sheet.col_styles_dirty = false;
            sheet.merges_dirty = false;
            sheet.notes_dirty = false;
            if saved.source_cells.as_ref().is_some_and(|source| source.rich == sheet.rich) {
                sheet.rich_dirty = false;
            }
            sheet.cf_dirty = false;
            sheet.dv_dirty = false;
            sheet.freeze_dirty = false;
            sheet.filter_dirty = false;
            sheet.page_dirty = false;
        }
    }
}

#[derive(Default)]
struct CellWrite {
    styles: Option<delta::SavedStyles>,
    used_delta: bool,
    created_charts: std::collections::HashMap<(u64, usize), (String, String)>,
    trusted_parts: std::collections::HashMap<String, (u32, u64)>,
}

// Reconstruct only unambiguous raw XF numbers from the committed style part via the
// single-pass quick-xml style parser without building a synthetic ZIP or inflating sheets.
fn committed_style_map(
    zip: &mut zip::ZipArchive<std::fs::File>,
    wb: &Workbook,
) -> Option<std::sync::Arc<delta::StyleMap>> {
    let parsed = sheets::parse_styles_from_zip(zip).ok()?;
    let mut ids = std::collections::HashMap::new();
    let mut defaults = std::collections::HashMap::new();
    for index in 0..parsed.xf_count() {
        let raw = u16::try_from(index).ok()?;
        let (mut cell, xf, pattern_owned, quoted) = parsed.resolve_xf(index);
        let pattern = pattern_owned.as_ref();
        let diagonal = cell.diagonal.clone();
        let matching_id = |candidate: &crate::style::CellStyle| {
            let id = wb.styles.find_style(candidate)?;
            (wb.styles.theme_of(id) == xf
                && wb.styles.pattern_of(id) == pattern
                && wb.styles.diag_of(id) == diagonal.as_ref()
                && wb.styles.quote_of(id) == quoted)
                .then_some(id)
        };
        if let Some(id) = matching_id(&cell) {
            ids.insert(raw, id);
        }
        let bottom = cell.valign == 3;
        let calibri = cell.font == "Calibri";
        let mut origin = |candidate: &crate::style::CellStyle| {
            if let Some(id) = matching_id(candidate) {
                defaults
                    .entry(id)
                    .and_modify(|v| {
                        if *v != Some(raw) {
                            *v = None;
                        }
                    })
                    .or_insert(Some(raw));
            }
        };
        if calibri {
            let mut implicit_font = cell.clone();
            implicit_font.font.clear();
            origin(&implicit_font);
            if bottom {
                implicit_font.valign = 0;
                origin(&implicit_font);
            }
        }
        if bottom {
            cell.valign = 0;
            origin(&cell);
        }
    }
    Some(delta::StyleMap::recovered(&ids, defaults, &wb.styles))
}

fn package_receipt(path: &Path, wb: &Workbook, write: CellWrite) -> Result<SaveReceipt> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let book = read_zip_text(&mut zip,"xl/workbook.xml")?;
    let rels = read_zip_text(&mut zip,"xl/_rels/workbook.xml.rels")?;
    let targets = crate::patch::sheet_targets_xml(Some(&book),Some(&rels));
    let rebuilt = std::cell::OnceCell::new();
    let unstyled_map = std::cell::OnceCell::new();
    let sheets = wb.sheets.iter().map(|sheet| {
        let part = targets.get(&sheet.name)
            .with_context(|| format!("已保存文件缺少工作表: {}", sheet.name))?;
        Ok((sheet.identity, SavedSheet {
            part: part.clone(),
            source_needs_dom_repair: sheet.source_needs_dom_repair &&
                (write.used_delta || !(sheet.row_styles_dirty || sheet.col_styles_dirty)),
            source_cells: SourceCells::capture(&mut zip, part, sheet).map(|mut source| {
                if let Some(styles) = &write.styles { styles.attach(&mut source); }
                else if sheet.source_part.is_none() && sheet.styles.is_empty()
                    && sheet.row_styles.is_empty() && sheet.col_styles.is_empty() {
                    // No user XF was assigned when this sheet was created. On
                    // its next save, font edits must still prove the raw cell XF
                    // is zero and the original font properties match the model.
                    source.set_style_map(unstyled_map.get_or_init(||
                        delta::StyleMap::new(&Default::default(), &crate::style::StyleTable::new())
                    ).clone());
                }
                if !source.has_style_map() && let Some(map) = rebuilt.get_or_init(||
                    committed_style_map(&mut zip, wb)) {
                    source.set_style_map(map.clone());
                }
                source
            }),
            cells: sheet.cell_version().clone(),
            cleared: sheet.cleared.clone(),
        }))
    }).collect::<Result<_>>()?;
    Ok(SaveReceipt {
        used_cell_delta: write.used_delta,
        sheets,
        source_encryption: wb.encryption.clone(),
        committed_audit: wb.audit.log.clone(),
        committed_charts: wb.sheets.iter().filter(|s| s.charts_dirty || !s.deleted_charts.is_empty() || write.created_charts.keys().any(|(id, _)| *id == s.identity)).map(|sheet| {
            let charts = sheet.charts.iter().enumerate().map(|(i, chart)| {
                let created = write.created_charts.get(&(sheet.identity, i)).map(|(part, drawing)| (chart.clone(), part.clone(), drawing.clone()));
                let saved_content = (created.is_none() && chart.content_dirty).then(|| chart.clone());
                SavedChart {
                    uid: chart.uid.raw(),
                    part: chart.part.clone(), drawing: chart.drawing.clone(),
                    anchor: chart.anchor, ref_fixes: chart.ref_fixes.clone(),
                    created,
                    saved_content,
                }
            }).collect();
            (sheet.identity, (charts, sheet.deleted_charts.clone()))
        }).collect(),
    })
}

/// 写回 xlsx。文件已存在时先读入，以保留样式、列宽等未改动的部分。

/// 按最终目标扩展名保存 XLS/XLSX/XLSM：先写同目录的临时文件，再 rename 覆盖目标。
///
/// 网盘（SMB）上直接写目标路径，断网或对方锁定会留下半截文件；
/// 同一卷内 rename 是原子的，别人要么看到旧文件、要么看到完整的新文件。
/// 最终产物完整性门禁（C13）：临时文件必须先通过校验，才允许 rename 覆盖原文件。
///
/// 覆盖：ZIP 能打开、每个条目 CRC 读通、必需部件齐全、[Content_Types].xml 声明的部件全在、
/// workbook.xml.rels 里的工作表目标全在、这几个关键 XML 良构。XLS 走对应的 CFB 读取路径。
pub fn verify_saved_package(path: &Path) -> Result<()> {
    verify_saved_package_against(path, None)
}

fn collect_trusted_signatures(wb: &Workbook) -> std::collections::HashMap<String, (u32, u64)> {
    let mut trusted = std::collections::HashMap::new();
    if wb.load_incomplete {
        return trusted;
    }
    for sheet in &wb.sheets {
        if let (Some(part), Some(src)) = (&sheet.source_part, &sheet.source_cells) {
            // Only trust a worksheet's baseline signature when none of its XML-altering flags changed.
            let untouched_sheet = !sheet.cell_data_needs_save()
                && !sheet.styles_dirty
                && !sheet.row_styles_dirty
                && !sheet.col_styles_dirty
                && sheet.style_edits.is_empty()
                && !sheet.merges_dirty
                && !sheet.notes_dirty
                && !sheet.rich_dirty
                && !sheet.layout_dirty
                && !sheet.freeze_dirty
                && !sheet.filter_dirty
                && !sheet.page_dirty
                && !sheet.cf_dirty
                && !sheet.dv_dirty
                && !sheet.charts_dirty
                && !sheet.tables_dirty
                && !sheet.protect_dirty
                && !sheet.protected_ranges_dirty
                && !sheet.view_dirty
                && !sheet.tab_color_dirty
                && !sheet.outline_pr_dirty
                && !sheet.scenarios_dirty
                && !sheet.ignored_errors_dirty
                && !sheet.sparklines_dirty
                && !sheet.textboxes_dirty
                && !sheet.pictures_dirty
                && !sheet.hyperlinks_dirty;
            let (sheet_sig, sst_sig, style_sig) = src.trusted_signatures();
            if untouched_sheet {
                trusted.insert(part.clone(), sheet_sig);
            }
            if let Some(sst_sig) = sst_sig {
                trusted.insert("xl/sharedStrings.xml".to_owned(), sst_sig);
            }
            if let Some(style_sig) = style_sig {
                trusted.insert("xl/styles.xml".to_owned(), style_sig);
            }
        }
    }
    trusted
}

fn verify_saved_package_against(
    path: &Path,
    trusted: Option<&std::collections::HashMap<String, (u32, u64)>>,
) -> Result<()> {
    use std::io::{Read, Seek};
    // 不能靠扩展名判断类型：调用方传进来的常常是临时文件（.~a.xls.saving-1234），
    // 它的扩展名是 saving-1234。按文件头判断才可靠。
    let mut head = [0u8; 8];
    let mut file = std::fs::File::open(path).with_context(|| format!("打开保存产物失败：{}", path.display()))?;
    let n = file.read(&mut head).with_context(|| format!("读取保存产物失败：{}", path.display()))?;
    if n < 8 {
        anyhow::bail!("保存产物只有 {n} 字节，明显被截断");
    }
    let is_zip = head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08");
    let is_cfb = head == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    if is_cfb {
        // 两种合法情况：加了打开密码的 OOXML（CFB 里装 EncryptionInfo + EncryptedPackage），或老的 .xls（BIFF）。
        // 先轻量检查 CFB 目录流，避免加密文件先试错 xls_read::load 再整文件读盘。
        file.rewind()?;
        if let Ok(mut cf) = cfb::CompoundFile::open(&mut file) {
            if cf.exists("EncryptedPackage") && cf.exists("EncryptionInfo") {
                let pkg_len = cf.open_stream("EncryptedPackage").map(|s| s.len()).unwrap_or(0);
                let info_len = cf.open_stream("EncryptionInfo").map(|s| s.len()).unwrap_or(0);
                if pkg_len >= 8 && info_len >= 8 {
                    return Ok(());
                }
                anyhow::bail!("保存产物加密流长度异常（EncryptionInfo={info_len}, EncryptedPackage={pkg_len}）");
            }
        }
        if crate::xls_read::load(path).is_ok() {
            return Ok(());
        }
        let bytes = std::fs::read(path).with_context(|| format!("读取保存产物失败：{}", path.display()))?;
        let compound = crate::cfb::Compound::open(bytes).context("保存产物是 CFB 容器但打不开")?;
        let names: Vec<String> = compound
            .names()
            .into_iter()
            .map(|n| n.split('[').next().unwrap_or(&n).to_string())
            .collect();
        if names.iter().any(|n| n.eq_ignore_ascii_case("EncryptedPackage"))
            && names.iter().any(|n| n.eq_ignore_ascii_case("EncryptionInfo"))
        {
            return Ok(()); // 加密容器：内容无法在不解密的前提下深查，结构与流都在即视为完整
        }
        anyhow::bail!("保存产物是 CFB 容器，但既不是可读的 XLS，也不含 EncryptedPackage（流：{names:?}）");
    }
    if !is_zip {
        anyhow::bail!("保存产物开头不是 ZIP/CFB 签名，格式无法识别");
    }
    file.rewind()?;
    let mut zip = zip::ZipArchive::new(file).context("保存产物不是有效的 ZIP（中央目录缺失或文件被截断）")?;
    const CRITICAL_XML: [&str; 4] = ["[Content_Types].xml", "_rels/.rels", "xl/workbook.xml", "xl/_rels/workbook.xml.rels"];
    let mut critical = std::collections::HashMap::new();
    for part in CRITICAL_XML {
        anyhow::ensure!(zip.index_for_name(part).is_some(), "保存产物缺少必需部件 {part}");
        let body = read_zip_text(&mut zip,part)?;
        check_single_root(part,&body)?;
        critical.insert(part,body);
    }
    let types = roxmltree::Document::parse(&critical["[Content_Types].xml"])?;
    let mut worksheets: std::collections::HashSet<String> = types.root_element().children()
        .filter(|entry| entry.tag_name().name()=="Override" && entry.attribute("ContentType")
            ==Some("application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"))
        .filter_map(|entry| entry.attribute("PartName").map(|p| p.trim_start_matches('/').to_owned())).collect();
    let links = roxmltree::Document::parse(&critical["xl/_rels/workbook.xml.rels"])?;
    for entry in links.root_element().children().filter(|e| e.tag_name().name()=="Relationship"
        && e.attribute("Type").is_some_and(|t| t.ends_with("/worksheet")) && e.attribute("TargetMode") != Some("External")) {
        if let Some(target) = entry.attribute("Target").and_then(|t| resolve_part("xl",t)) {
            worksheets.insert(target);
        }
    }
    let mut seen_critical = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let name = zip
            .name_for_index(i)
            .with_context(|| format!("读取保存产物第 {i} 个条目名失败"))?
            .to_owned();
        if critical.contains_key(name.as_str()) {
            anyhow::ensure!(seen_critical.insert(name.clone()), "保存产物关键部件重复：{name}");
            continue;
        }
        if let Some(trusted_map) = trusted {
            if let Some(&want_sig) = trusted_map.get(&name) {
                let raw = zip
                    .by_index_raw(i)
                    .with_context(|| format!("读取保存产物第 {i} 个条目头失败"))?;
                if (raw.crc32(), raw.size()) == want_sig
                    && (raw.size() == 0 || raw.compressed_size() > 0)
                {
                    continue;
                }
            }
        }
        let mut e = zip.by_index(i).with_context(|| format!("读取保存产物第 {i} 个条目失败"))?;
        if worksheets.contains(&name) {
            delta::stream::xml_input(&mut e, &name, |source| {
                check_xml_reader(
                    &name,
                    std::io::BufReader::with_capacity(128 * 1024, source),
                    Some("worksheet"),
                )?;
                Ok(true)
            }).with_context(|| format!("保存产物条目损坏：{name}"))?;
        } else {
            std::io::copy(&mut e, &mut std::io::sink())
                .with_context(|| format!("保存产物条目损坏：{name}"))?;
        }
    }
    // 必需结构：Content-Types 至少要有 workbook 与 worksheet 的类型；
    // workbook.xml 至少要有一个 <sheet> 且带 name/r:id —— 空 XML、多根、只有壳子都要拦。
    let types = &critical["[Content_Types].xml"];
    // 工作簿主部件的类型随格式不同：xlsx 是 sheet.main+xml，xlsm 是 macroEnabled.main+xml，
    // xltx/xltm 又是 template.main+xml。统一按「/xl/workbook.xml 的 Override 以 main+xml 结尾」判断。
    if !override_is_workbook_main(types) {
        anyhow::bail!("[Content_Types].xml 里没有工作簿主部件（/xl/workbook.xml）的类型声明");
    }
    let wb_xml = &critical["xl/workbook.xml"];
    let sheets = count_elements(wb_xml, "sheet");
    if sheets == 0 {
        anyhow::bail!("workbook.xml 里没有任何 <sheet>");
    }
    for part in override_part_names(types) {
        let want = part.trim_start_matches('/');
        if zip.index_for_name(want).is_none() {
            anyhow::bail!("[Content_Types].xml 声明了不存在的部件 {part}");
        }
    }
    let rels = &critical["xl/_rels/workbook.xml.rels"];
    for target in relationship_targets(rels) {
        if target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:") {
            continue;
        }
        // 关系目标相对 xl/workbook.xml 解析：'/xl/...' 是包内绝对路径，'../' 要真的向上走，
        // 不能像以前那样遇到 '../' 直接跳过（等于漏检）。
        let Some(resolved) = resolve_part("xl", &target) else {
            anyhow::bail!("workbook.xml.rels 的目标越出包根：{target}");
        };
        if zip.index_for_name(&resolved).is_none() {
            anyhow::bail!("workbook.xml.rels 指向不存在的部件 {target}（解析为 {resolved}）");
        }
    }
    Ok(())
}

/// 把一个关系目标解析成包内路径。`base` 是引用方所在目录（如 "xl"）。
fn resolve_part(base: &str, target: &str) -> Option<String> {
    let target = target.split('#').next().unwrap_or(target);
    let mut segs: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in target.trim_start_matches('/').split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop()?;
            }
            other => segs.push(other),
        }
    }
    Some(segs.join("/"))
}

/// XML 必须「有且只有一个根元素」，根之后不允许再出现元素或非空白文本。
/// 自闭合根（`<a/>`）也算根；根后再来一个 `<extra/>` 或第二段文本都要拒。
fn check_single_root(part: &str, body: &str) -> Result<()> {
    check_xml_reader(part,body.as_bytes(),None)
}

fn check_xml_reader(part: &str, input: impl std::io::BufRead, expected: Option<&str>) -> Result<()> {
    let mut reader = quick_xml::Reader::from_reader(input);
    let mut buffer = Vec::new();
    reader.config_mut().check_end_names = true;
    let mut depth: i32 = 0;
    let mut roots = 0;
    let mut closed = false;
    loop {
        buffer.clear();
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(ref tag)) => {
                for attr in tag.attributes() { attr.with_context(|| format!("{part}: invalid attribute"))?; }
                if closed {
                    anyhow::bail!("{part} 在根元素之后又出现了元素（多个根）");
                }
                if depth == 0 {
                    anyhow::ensure!(expected.is_none_or(|n| n == tag.local_name().as_ref()),"{part}: wrong root element");
                    roots += 1;
                }
                depth += 1;
            }
            Ok(quick_xml::events::Event::Empty(ref tag)) => {
                for attr in tag.attributes() { attr.with_context(|| format!("{part}: invalid attribute"))?; }
                if closed {
                    anyhow::bail!("{part} 在根元素之后又出现了元素（多个根）");
                }
                // 自闭合元素既不进也不出栈；在顶层时它自己就是一个根。
                if depth == 0 {
                    anyhow::ensure!(expected.is_none_or(|n| n == tag.local_name().as_ref()),"{part}: wrong root element");
                    roots += 1;
                    closed = true;
                }
            }
            Ok(quick_xml::events::Event::End(_)) => {
                depth -= 1;
                if depth == 0 {
                    closed = true;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                // 根外（根之前 depth==0 且还没闭合，或根之后）只要是非空白文本就非法：
                // `text<a/>` 这种「根前面挂了内容」不能因为 closed 还是 false 就放过。
                if depth == 0 && !t.as_ref().chars().all(|c| c.is_whitespace()) {
                    anyhow::bail!("{part} 在根元素之外还有文本内容");
                }
            }
            Ok(quick_xml::events::Event::CData(c)) => {
                if depth == 0 && !c.as_ref().chars().all(|ch| ch.is_whitespace()) {
                    anyhow::bail!("{part} 在根元素之外还有 CDATA 内容");
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => anyhow::bail!("保存产物里的 {part} 不是良构 XML：{e}"),
        }
        if depth < 0 {
            anyhow::bail!("保存产物里的 {part} 结束标签多于开始标签");
        }
    }
    if roots == 0 {
        anyhow::bail!("保存产物里的 {part} 没有任何根元素（空文件？）");
    }
    if roots > 1 {
        anyhow::bail!("保存产物里的 {part} 有 {roots} 个根元素");
    }
    if depth != 0 {
        anyhow::bail!("保存产物里的 {part} 有 {depth} 个标签没闭合（产物被截断）");
    }
    if !closed {
        anyhow::bail!("保存产物里的 {part} 根元素没有闭合");
    }
    Ok(())
}

/// 数某个本地名元素出现的次数（Start 或自闭合）
fn count_elements(xml: &str, local: &str) -> usize {
    let mut n = 0;
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                if e.local_name().as_ref() == local {
                    n += 1;
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    n
}

#[cfg(test)]
mod gate_tests {
    use super::check_single_root;

    #[test]
    fn accepts_a_single_root_of_every_shape() {
        for body in ["<a></a>", "<a/>", "<a><b/></a>", "  <a>t</a>  ", "<a></a><!--c-->", "<?xml version=\"1.0\"?><a/>"] {
            check_single_root("p", body).unwrap_or_else(|e| panic!("应通过 {body}: {e}"));
        }
    }

    #[test]
    fn rejects_empty_multi_root_and_trailing_content() {
        for body in ["", "   ", "<a>", "<a></a><b></b>", "<a/><b/>", "<a></a>text", "<a></a><extra/>", "<a></a><![CDATA[x]]>", "</a>"] {
            assert!(check_single_root("p", body).is_err(), "必须拒绝 {body:?}");
        }
    }

    #[test]
    fn rejects_content_before_the_root_too() {
        // 根**之前**挂内容同样不是良构文档；旧实现只在 closed 之后检查，这四种会漏过。
        for body in [
            "text<a/>",
            "text<a></a>",
            "<![CDATA[x]]><a/>",
            "  x  <a/>",
            "<a/><b>",
        ] {
            assert!(check_single_root("p", body).is_err(), "根外内容必须拒绝 {body:?}");
        }
        // 空白与注释在根外是合法的
        for body in ["\n  <a/>\n", "<!--c--><a/>", "<?xml version=\"1.0\"?>\n<a></a>\n"] {
            check_single_root("p", body).unwrap_or_else(|e| panic!("应通过 {body:?}: {e}"));
        }
    }

    #[test]
    fn text_inside_the_root_is_fine() {
        for body in ["<a>text</a>", "<a><![CDATA[x]]></a>", "<a> <b>t</b> </a>"] {
            check_single_root("p", body).unwrap_or_else(|e| panic!("根内文本应通过 {body:?}: {e}"));
        }
    }
}

/// Borrow ordinary UTF-8 and transcode only worksheets which actually use UTF-16.
pub(crate) fn patch_xml_text<'a>(bytes: &'a [u8], part: &str) -> Result<std::borrow::Cow<'a,str>> {
    if sheets::utf16_encoding(bytes).is_some() {
        Ok(sheets::decode_xml_part(bytes.to_owned(),part)?.into())
    } else {
        Ok(std::str::from_utf8(bytes).with_context(|| format!("invalid XML encoding in {part}"))?.into())
    }
}

pub(crate) fn read_zip_text<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<String> {
    sheets::xml_part(zip, name).with_context(|| format!("读取 {name} 失败"))
}

fn override_is_workbook_main(xml: &str) -> bool {
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                if e.local_name().as_ref() == "Override" {
                    let mut part = None;
                    let mut ctype = None;
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            "PartName" => part = Some(a.value.to_string()),
                            "ContentType" => ctype = Some(a.value.to_string()),
                            _ => {}
                        }
                    }
                    if part.as_deref() == Some("/xl/workbook.xml")
                        && ctype.is_some_and(|c| c.ends_with("main+xml"))
                    {
                        return true;
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    false
}

fn override_part_names(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                if e.local_name().as_ref() == "Override" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == "PartName" {
                            out.push(a.value.to_string());
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

fn relationship_targets(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                if e.local_name().as_ref() == "Relationship" {
                    let mut target = None;
                    let mut external = false;
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            "Target" => target = Some(a.value.to_string()),
                            "TargetMode" => external = a.value.eq_ignore_ascii_case("External"),
                            _ => {}
                        }
                    }
                    // 外部链接指向包外，不能在包里找
                    if let Some(t) = target {
                        if !external {
                            out.push(t);
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

/// 验收专用故障注入：`SC_CORRUPT_TMP=truncate|drop-part|drop-sheet|corrupt-xml`
/// 让「写步骤成功但产物已损坏」可复现；未设置时完全不改变行为。
fn corrupt_tmp_for_test(path: &Path) {
    let Ok(mode) = std::env::var("SC_CORRUPT_TMP") else { return };
    match mode.as_str() {
        "truncate" => {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                let _ = f.set_len(len / 2);
            }
        }
        "drop-part" => replace_zip_part(path, "[Content_Types].xml", None),
        "drop-sheet" => replace_zip_part(path, "xl/worksheets/sheet1.xml", None),
        "corrupt-xml" => replace_zip_part(path, "xl/workbook.xml", Some("<workbook><sheets>")),
        _ => {}
    }
}

/// 重写 zip：删除指定部件（replacement=None）或替换其内容
fn replace_zip_part(path: &Path, part: &str, replacement: Option<&str>) {
    let Ok(file) = std::fs::File::open(path) else { return };
    let Ok(mut src) = zip::ZipArchive::new(file) else { return };
    let tmp = path.with_extension("rezipped");
    let Ok(out) = std::fs::File::create(&tmp) else { return };
    let mut w = zip::ZipWriter::new(out);
    let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
    for i in 0..src.len() {
        let Ok(mut e) = src.by_index(i) else { continue };
        let name = e.name().to_string();
        if name == part {
            if let Some(text) = replacement {
                use std::io::Write;
                if w.start_file(name, opts).is_ok() {
                    let _ = w.write_all(text.as_bytes());
                }
            }
            continue;
        }
        if w.start_file(name, opts).is_ok() {
            use std::io::Write;
            let mut buf = Vec::new();
            if std::io::Read::read_to_end(&mut e, &mut buf).is_ok() {
                let _ = w.write_all(&buf);
            }
        }
    }
    if w.finish().is_ok() {
        let _ = std::fs::rename(&tmp, path);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn save_atomic(path: &Path, wb: &Workbook) -> Result<()> {
    save_atomic_with_receipt(path, wb).map(|_| ())
}

#[cfg(windows)]
fn set_win_file_attrs(path: &Path, attrs: u32) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileAttributesW(name: *const u16, attrs: u32) -> i32;
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SetFileAttributesW(wide.as_ptr(), attrs);
    }
}

/// 清理同目录下因旧版本替换失败或异常中断残留的 `.~<文件名>.saving-*` 临时文件（绝不触碰刚生成的活跃临时文件）。
fn sweep_stale_saving_files(dir: &Path, name: &str, keep_active: Option<&Path>) {
    let prefix = format!(".~{name}.saving-");
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    let my_pid_prefix = format!("{}-", std::process::id());
    for entry in rd.flatten() {
        let p = entry.path();
        if keep_active.is_some_and(|k| k == p.as_path()) {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.starts_with(".~cellan-stage-") {
            let age = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .unwrap_or_default();
            if age >= std::time::Duration::from_secs(30) {
                let _ = std::fs::remove_dir_all(&p);
            }
            continue;
        }
        let Some(rest) = fname.strip_prefix(&prefix) else { continue };
        let age = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .unwrap_or_default();
        let is_mine = rest.starts_with(&my_pid_prefix) && age >= std::time::Duration::from_secs(5);
        let is_stale = age >= std::time::Duration::from_secs(30);
        if is_mine || is_stale {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn staging_workbook_dir(target_dir: &Path) -> Result<tempfile::TempDir> {
    let raw = target_dir.to_string_lossy();
    let is_unc = raw.starts_with("\\\\") || raw.starts_with("//");
    if !is_unc {
        let mut builder = tempfile::Builder::new();
        builder.prefix(".~cellan-stage-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        if let Ok(td) = builder.tempdir_in(target_dir) {
            #[cfg(windows)]
            set_win_file_attrs(td.path(), 0x02); // FILE_ATTRIBUTE_HIDDEN
            return Ok(td);
        }
    }
    private_workbook_dir()
}

/// Build the receipt from the staged OOXML before committing the replacement.
pub fn save_atomic_with_receipt(path: &Path, wb: &Workbook) -> Result<SaveReceipt> {
    save_atomic_model(path, &mut Cow::Borrowed(wb))
}

/// Save a background worker's private copy and return its calculated cells.
/// Preparing a newly built package only changes transient save flags on this copy.
pub fn save_snapshot_with_receipt(path: &Path, wb: Workbook) -> Result<(SaveReceipt, Workbook)> {
    let mut snapshot = Cow::Owned(wb);
    let receipt = save_atomic_model(path, &mut snapshot)?;
    Ok((receipt, snapshot.into_owned()))
}

fn save_atomic_model(path: &Path, wb: &mut Cow<'_, Workbook>) -> Result<SaveReceipt> {
    refuse_overwrite_of_source(wb, path)?;
    if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        anyhow::bail!("替换 {} 失败：目标文件为只读属性，原文件未被修改", path.display());
    }
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("无法创建目标目录 {}", dir.display()))?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook.xlsx".into());

    // 优先在目标所在本地卷上的隐藏暂存目录内完成序列化与校验，使后续 rename(&staged, &tmp) 为 O(1) 同卷移动；
    // 若目标位于远程共享盘或目录不可建子目录，则自动回退至本机私有临时目录。
    let build_scratch = staging_workbook_dir(dir)?;
    let staged = build_scratch.path().join(&name);
    let _logical_guard = LogicalSaveTargetGuard::set(path);
    let is_xls = path
        .extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("xls"));
    let mut trusted_sigs = (!is_xls && std::env::var("SC_CORRUPT_TMP").is_err())
        .then(|| collect_trusted_signatures(wb));
    let mut pending_decrypted_cache: Option<(String, Vec<u8>, Option<(u64, [u8; 32])>)> = None;
    let receipt = if is_xls {
        crate::xls_write::write(&staged, wb).map(|_| SaveReceipt::default())?
    } else {
        write_ooxml_with_cache(
            &staged,
            wb,
            Some(&mut pending_decrypted_cache),
            trusted_sigs.as_mut(),
        )?
    };
    // 写成功 ≠ 产物完整：临时文件先过完整性门禁，不通过绝不触碰原文件
    corrupt_tmp_for_test(&staged);
    let corrupt_injected = std::env::var("SC_CORRUPT_TMP").is_ok();
    let verify_trusted = if corrupt_injected {
        None
    } else {
        trusted_sigs.as_ref()
    };
    let verified_encrypted_len = (!corrupt_injected)
        .then(|| pending_decrypted_cache.as_ref().and_then(|(_, _, meta)| *meta).map(|(len, _)| len))
        .flatten();
    if let Some(expected_len) = verified_encrypted_len {
        let actual_len = std::fs::metadata(&staged)
            .with_context(|| format!("读取保存产物元数据失败：{}", staged.display()))?
            .len();
        if actual_len != expected_len {
            return Err(anyhow::anyhow!(
                "保存产物完整性校验失败：加密文件长度不符（期望 {expected_len}，实际 {actual_len}），未覆盖 {}",
                path.display()
            ));
        }
    } else if let Err(e) = verify_saved_package_against(&staged, verify_trusted) {
        return Err(e.context(format!("保存产物完整性校验失败，未覆盖 {}", path.display())));
    }

    static NEXT_SAVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_SAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".~{name}.saving-{}-{sequence}", std::process::id()));
    #[cfg(windows)]
    let orig_attrs = {
        use std::os::windows::fs::MetadataExt;
        std::fs::metadata(path)
            .ok()
            .map(|m| m.file_attributes())
            .unwrap_or(0x80) // FILE_ATTRIBUTE_NORMAL
    };
    let _ = std::fs::create_dir_all(dir);
    if let Err(e) = std::fs::rename(&staged, &tmp)
        .or_else(|_| std::fs::copy(&staged, &tmp).map(|_| ()))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e).context(format!("写入 {} 失败", path.display())));
    }
    // 立即清理隐藏暂存目录，保证进入替换窗口（含 SC_SAVE_PAUSE_MS）及函数退出前零残留
    drop(build_scratch);
    #[cfg(windows)]
    set_win_file_attrs(&tmp, 0x02); // FILE_ATTRIBUTE_HIDDEN：写入与等待期间对资源管理器隐藏

    // 验收专用：SC_SAVE_PAUSE_MS 让「临时文件已写好、还没替换」的窗口可控制，
    // 用于模拟保存过程中进程被强杀（不是物理断电）。未设置时零影响。
    if let Ok(ms) = std::env::var("SC_SAVE_PAUSE_MS") {
        if let Ok(ms) = ms.trim().parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }

    #[cfg(windows)]
    set_win_file_attrs(&tmp, orig_attrs & !0x01); // rename 前恢复目标文件原有可见属性（去掉只读位）

    // Recheck an in-place note edit after serializing: another editor may have
    // saved the comments while the private package was being built or verified.
    if !is_xls && wb.source_encryption.is_none() && wb.sheets.iter().any(|s| s.notes_dirty)
        && wb.path.as_ref().is_some_and(|base| same_file(base, path))
    {
        if let Err(err) = delta::verify_note_sources(path, wb) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
    }
    let target_readonly = std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly());
    let mut occupied_by_app = false;
    // 流式覆写一旦开始，原文件就不再是「未被修改」；记下它最终的状态，报错时如实告诉用户
    let mut original_state = OriginalState::Untouched;
    let replace_res: std::io::Result<()> = match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(mut last_err) => {
            // Windows 桌面下杀毒实时扫描 (Defender)、资源管理器缩略图/索引器或云盘同步会短暂以无 FILE_SHARE_DELETE
            // 句柄读取刚写完的临时文件或原文件，引发瞬时 os error 5 / 32：带退避重试可自动化解绝大多数误报。
            if !target_readonly
                && (last_err.kind() == std::io::ErrorKind::PermissionDenied
                    || matches!(last_err.raw_os_error(), Some(5 | 32 | 33)))
            {
                for wait_ms in [15u64, 25, 40, 60, 90, 120, 150, 180] {
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                    match std::fs::rename(&tmp, path) {
                        Ok(()) => {
                            last_err = std::io::Error::new(std::io::ErrorKind::Other, "");
                            break;
                        }
                        Err(e) => last_err = e,
                    }
                }
            }
            if last_err.kind() == std::io::ErrorKind::Other && last_err.to_string().is_empty() {
                Ok(())
            } else if !target_readonly
                && path.exists()
                && (last_err.kind() == std::io::ErrorKind::PermissionDenied
                    || last_err.raw_os_error() == Some(5))
            {
                // 若仅目录/同步盘拦截了 DELETE/重命名，但原文件本身允许写入（未被 Excel/WPS 独占锁定，
                // 注意独占只读共享句柄在 open(write) 阶段即会被内核以 os error 32 拒绝，绝不触碰原文件字节），
                // 则直接安全流式覆写原文件。
                //
                // 流式覆写不是原子的：写到一半断网、共享盘掉线或进程被杀，唯一的原文件就成了半截。
                // 所以先把原文件拷进本机私有临时目录（不进工作簿所在目录），写完再整包校验；
                // 写失败或校验不过，就用这份副本把原文件写回去。拷不出副本就不冒这个险。
                let rollback_scratch = private_workbook_dir()?;
                let rollback = rollback_scratch.path().join(format!("rollback-{name}"));
                let have_rollback = std::fs::copy(path, &rollback).is_ok();
                match std::fs::OpenOptions::new().write(true).open(path) {
                    Ok(mut dst_f) if have_rollback => {
                        original_state = OriginalState::Overwritten;
                        let copy_res = (|| -> std::io::Result<()> {
                            let mut src_f = std::fs::File::open(tmp.as_path())?;
                            let written_len = std::io::copy(&mut src_f, &mut dst_f)?;
                            dst_f.set_len(written_len)?;
                            dst_f.sync_all()?;
                            Ok(())
                        })();
                        drop(dst_f);
                        let checked = copy_res.and_then(|()| {
                            verify_saved_package(path)
                                .map_err(|e| std::io::Error::other(format!("覆写后校验未通过：{e:#}")))
                        });
                        if let Err(e) = checked {
                            original_state = match overwrite_in_place(&rollback, path) {
                                Ok(()) => OriginalState::Restored,
                                Err(_) => OriginalState::Damaged(keep_rescue_copy(&rollback, &name)),
                            };
                            Err(e)
                        } else {
                            Ok(())
                        }
                    }
                    Ok(dst_f) => {
                        drop(dst_f);
                        Err(std::io::Error::new(
                            last_err.kind(),
                            format!("{last_err}；无法先为原文件留一份本机副本，为免写坏原文件，已放弃覆写"),
                        ))
                    }
                    Err(w_err) => {
                        if matches!(w_err.raw_os_error(), Some(32 | 33)) {
                            occupied_by_app = true;
                        }
                        Err(last_err)
                    }
                }
            } else {
                if matches!(last_err.raw_os_error(), Some(32 | 33)) {
                    occupied_by_app = true;
                }
                Err(last_err)
            }
        }
    };

    // 无论替换成功还是失败，一律清理原目录下的 `.saving-*` 临时文件，绝不残留大体积垃圾文件
    let _ = std::fs::remove_file(&tmp);
    sweep_stale_saving_files(dir, &name, None);

    match replace_res {
        Ok(()) => {
            if let Some((pw, plain, cfb_meta)) = pending_decrypted_cache {
                decrypted_cache_put_with_meta(path, &pw, plain, cfb_meta);
            } else {
                decrypted_cache_invalidate(path);
            }
            Ok(receipt)
        }
        Err(e) => {
            if dir.join(format!("~${name}")).exists() {
                occupied_by_app = true;
            }
            let detail = if occupied_by_app {
                format!("{e}；检测到该文件正被其他程序（如 Excel / WPS）打开占用，请先关闭后再保存")
            } else {
                e.to_string()
            };
            let outcome = match &original_state {
                OriginalState::Untouched => "原文件未被修改".to_string(),
                OriginalState::Overwritten => "原文件状态未知，请先确认它能否正常打开".to_string(),
                OriginalState::Restored => "覆写中途失败，原文件已从本机副本恢复".to_string(),
                OriginalState::Damaged(Some(copy)) => format!(
                    "覆写中途失败且未能写回原文件，原文件可能已损坏；保存前的原文件副本在 {}",
                    copy.display()
                ),
                OriginalState::Damaged(None) => {
                    "覆写中途失败且未能写回原文件，原文件可能已损坏".to_string()
                }
            };
            Err(anyhow::anyhow!("替换 {} 失败（{}），{}", path.display(), detail, outcome))
        }
    }
}

/// 保存失败时原文件处于什么状态（决定报错文案，不能对用户说谎）
enum OriginalState {
    /// 从未动过原文件字节
    Untouched,
    /// 已开始流式覆写（成功时不会走到报错分支）
    Overwritten,
    /// 覆写失败，已用本机副本写回
    Restored,
    /// 覆写失败且写回也失败；附上保住的原文件副本位置
    Damaged(Option<std::path::PathBuf>),
}

/// 原地覆写（不改目录项，只改文件内容）：给拦截 DELETE/重命名的共享盘用
fn overwrite_in_place(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut src_f = std::fs::File::open(src)?;
    let mut dst_f = std::fs::OpenOptions::new().write(true).open(dst)?;
    let n = std::io::copy(&mut src_f, &mut dst_f)?;
    dst_f.set_len(n)?;
    dst_f.sync_all()
}

/// 原文件写坏且没能写回时，把保存前的副本挪到不会被自动清理的位置，交给用户手动恢复
fn keep_rescue_copy(rollback: &Path, name: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("CELLAN-recovery");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dst = dir.join(format!("{stamp}-{name}"));
    std::fs::copy(rollback, &dst).ok()?;
    Some(dst)
}

/// 保存：原文件带打开密码时，先写明文到临时文件，再加密回目标路径。
pub fn save(path: &Path, wb: &Workbook) -> Result<()> {
    refuse_overwrite_of_source(wb, path)?;
    if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("xls")) {
        return save_atomic(path, wb);
    }
    write_ooxml(path, &mut Cow::Borrowed(wb)).map(|_| ())
}

fn private_workbook_dir() -> Result<tempfile::TempDir> {
    let base_tmp = std::env::temp_dir();
    let _ = std::fs::create_dir_all(&base_tmp);
    let mut builder = tempfile::Builder::new();
    builder.prefix("cellan-workbook-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder
        .tempdir_in(&base_tmp)
        .or_else(|_| {
            let _ = std::fs::create_dir_all(&base_tmp);
            builder.tempdir()
        })
        .context("无法创建本机工作簿临时目录")
}

/// 有界解密明文包会话缓存：保存加密文件时免去再次对未变动的源文件跑 10 万轮 SHA-512 解密。
/// 缓存键同时绑定规范路径、文件大小、修改时间、首尾字节指纹与口令 SHA-256，任何外部改写或换密码都会自动失效。
struct DecryptedCacheEntry {
    canon_path: std::path::PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    fingerprint: [u8; 32],
    pw_digest: [u8; 32],
    plain: std::sync::Arc<Vec<u8>>,
}

const DECRYPTED_CACHE_CAP: usize = 4;
const DECRYPTED_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

fn decrypted_cache() -> &'static std::sync::Mutex<Vec<DecryptedCacheEntry>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<DecryptedCacheEntry>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(Vec::with_capacity(DECRYPTED_CACHE_CAP)))
}

fn cache_canon_path(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn password_digest(password: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut d = Sha256::new();
    d.update(password.as_bytes());
    d.finalize().into()
}

fn encrypted_file_key(path: &Path) -> Option<(std::path::PathBuf, u64, Option<std::time::SystemTime>, [u8; 32])> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    let canon = cache_canon_path(path);
    let mut f = std::fs::File::open(&canon).ok()?;
    let meta = f.metadata().ok()?;
    let len = meta.len();
    let modified = meta.modified().ok();
    let mut hasher = Sha256::new();
    hasher.update(len.to_le_bytes());
    let mut head = [0u8; 8192];
    let hn = f.read(&mut head).ok()?;
    hasher.update(&head[..hn]);
    if len > 8192 {
        let tail_start = len.saturating_sub(4096).max(hn as u64);
        if f.seek(SeekFrom::Start(tail_start)).is_ok() {
            let mut tail = [0u8; 4096];
            if let Ok(tn) = f.read(&mut tail) {
                hasher.update(&tail[..tn]);
            }
        }
    }
    Some((canon, len, modified, hasher.finalize().into()))
}

fn decrypted_cache_get(path: &Path, password: &str) -> Option<std::sync::Arc<Vec<u8>>> {
    let (canon, len, modified, fingerprint) = encrypted_file_key(path)?;
    let pw_digest = password_digest(password);
    let mut guard = decrypted_cache().lock().ok()?;
    let pos = guard.iter().position(|e| {
        e.canon_path == canon
            && e.len == len
            && e.modified == modified
            && e.fingerprint == fingerprint
            && e.pw_digest == pw_digest
    })?;
    let entry = guard.remove(pos);
    let plain = std::sync::Arc::clone(&entry.plain);
    guard.push(entry);
    Some(plain)
}

fn decrypted_cache_put(path: &Path, password: &str, plain: Vec<u8>) {
    decrypted_cache_put_with_meta(path, password, plain, None);
}

fn decrypted_cache_put_with_meta(
    path: &Path,
    password: &str,
    plain: Vec<u8>,
    known_meta: Option<(u64, [u8; 32])>,
) {
    if plain.len() > DECRYPTED_CACHE_MAX_BYTES {
        return;
    }
    let Some((canon_path, len, modified, fingerprint)) = (match known_meta {
        Some((expected_len, fp)) => {
            let canon = cache_canon_path(path);
            let meta = std::fs::metadata(&canon).ok();
            match meta {
                Some(m) if m.len() == expected_len => Some((canon, expected_len, m.modified().ok(), fp)),
                _ => encrypted_file_key(path),
            }
        }
        None => encrypted_file_key(path),
    }) else {
        return;
    };
    let pw_digest = password_digest(password);
    let Ok(mut guard) = decrypted_cache().lock() else {
        return;
    };
    guard.retain(|e| e.canon_path != canon_path);
    if guard.len() >= DECRYPTED_CACHE_CAP {
        guard.remove(0);
    }
    guard.push(DecryptedCacheEntry {
        canon_path,
        len,
        modified,
        fingerprint,
        pw_digest,
        plain: std::sync::Arc::new(plain),
    });
}

fn decrypted_cache_invalidate(path: &Path) {
    let canon = cache_canon_path(path);
    if let Ok(mut guard) = decrypted_cache().lock() {
        guard.retain(|e| e.canon_path != canon);
    }
}

fn write_ooxml(path: &Path, wb: &mut Cow<'_, Workbook>) -> Result<SaveReceipt> {
    write_ooxml_with_cache(path, wb, None, None)
}

fn write_ooxml_with_cache(
    path: &Path,
    wb: &mut Cow<'_, Workbook>,
    pending_cache: Option<&mut Option<(String, Vec<u8>, Option<(u64, [u8; 32])>)>>,
    extra_trusted: Option<&mut std::collections::HashMap<String, (u32, u64)>>,
) -> Result<SaveReceipt> {
    if wb.encryption.is_none() && wb.source_encryption.is_none() {
        let write = save_plain_base(path, wb, None)?;
        if let Some(trusted) = extra_trusted {
            trusted.extend(write.trusted_parts.iter().map(|(k, &v)| (k.clone(), v)));
        }
        if pending_cache.is_none() {
            decrypted_cache_invalidate(path);
        }
        return package_receipt(path, wb, write);
    }
    let scratch = private_workbook_dir()?;
    let base = scratch.path().join("source.xlsx");
    let encrypted_source = wb.path.as_ref().filter(|src| {
        !wb.load_incomplete && src.is_file() && crate::crypto::needs_password(src)
    });
    let base_override = if let Some(src) = encrypted_source {
        let credentials = wb.source_encryption.as_ref().or(wb.encryption.as_ref())
            .context("缺少原文件的打开密码")?;
        if let Some(cached) = decrypted_cache_get(src, &credentials.password) {
            std::fs::write(&base, cached.as_slice()).context("写临时明文基准失败")?;
        } else {
            let bytes = crate::crypto::decrypt(src, &credentials.password)?;
            std::fs::write(&base, &bytes).context("写临时明文基准失败")?;
            decrypted_cache_put(src, &credentials.password, bytes);
        }
        Some(base.as_path())
    } else {
        None
    };
    // Keep the logical file extension (including .xlsm) and isolate all helper files.
    let plain;
    let output = if wb.encryption.is_some() {
        let out_dir = scratch.path().join("output");
        std::fs::create_dir(&out_dir)?;
        plain = out_dir.join(path.file_name().context("保存路径缺少文件名")?);
        plain.as_path()
    } else {
        path
    };
    let write = save_plain_base(output, wb, base_override)?;
    let Some(enc) = wb.encryption.as_ref() else {
        if let Some(trusted) = extra_trusted {
            trusted.extend(write.trusted_parts.iter().map(|(k, &v)| (k.clone(), v)));
        }
        if pending_cache.is_none() {
            decrypted_cache_invalidate(output);
        }
        return package_receipt(output, wb, write);
    };
    corrupt_tmp_for_test(output);
    let pre_encrypt_trusted = (std::env::var("SC_CORRUPT_TMP").is_err()).then(|| {
        let mut map = collect_trusted_signatures(wb);
        map.extend(write.trusted_parts.iter().map(|(k, &v)| (k.clone(), v)));
        map
    });
    verify_saved_package_against(output, pre_encrypt_trusted.as_ref())
        .context("加密前的临时明文未通过完整性校验")?;
    let receipt = package_receipt(output, wb, write)?;
    let bytes = std::fs::read(output).context("读取临时明文失败")?;
    let cfb_meta = crate::crypto::encrypt_with_fingerprint(path, &bytes, enc)?;
    if let Some(slot) = pending_cache {
        *slot = Some((enc.password.clone(), bytes, cfb_meta));
    } else {
        decrypted_cache_put_with_meta(path, &enc.password, bytes, cfb_meta);
    }
    Ok(receipt)
}

/// 带打开密码的文件：解密到临时文件后按普通 xlsx 加载。
/// 工作簿记住原路径与密码，另存时继续加密。
pub fn load_with_password(path: &Path, password: &str) -> Result<Workbook> {
    load_with_password_diag(path, password).map(|(wb, _)| wb)
}

fn try_load_ooxml_from_memory(
    path: &Path,
    plain: &[u8],
    ctrl: Option<&LoadControl>,
) -> Option<Result<(Workbook, LoadDiag)>> {
    if !plain.starts_with(b"PK")
        || injected(&INJECT_WORKER_PANIC, path)
        || injected(&INJECT_SIDE_PANIC, path)
    {
        return None;
    }
    let (mut shared_zip, _) = match sheets::open_bytes_stamped(plain, false) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mut zip_has_vba = false;
    for name in shared_zip.file_names() {
        if name == "xl/vbaProject.bin" {
            zip_has_vba = true;
        } else if name == "docProps/custom.xml"
            || name.contains("worksheets/_rels/")
            || name.contains("externalLink")
            || name.contains("drawing")
            || name.contains("tables/table")
            || name.contains("charts/chart")
            || name.contains("comments")
            || name.contains("threadedComment")
        {
            return None;
        }
    }
    let meta = match sheets::workbook_metadata(&mut shared_zip) {
        Ok(m) => m,
        Err(e) => return Some(Err(e)),
    };
    let Some(header) = meta.header else {
        return None;
    };
    let workbook_xml = meta.xml;
    let targets = meta.targets;
    if workbook_xml.contains("_sc_src_") || workbook_xml.contains("externalReference") {
        return None;
    }

    set_load_kind(LOAD_KIND_OOXML);
    let mut diag = LoadDiag::default();
    let mut defined_names = crate::patch::defined_names_xml(&workbook_xml);
    let manual_hidden = crate::patch::manual_hidden_intervals_from_names(&defined_names);
    defined_names.retain(|(name, _, _)| name != crate::patch::MANUAL_HIDDEN_NAME);
    let (calc_iterate, calc_max_iterations, calc_max_change) =
        crate::patch::calc_iteration_xml(&workbook_xml).unwrap_or((false, 100, 0.001));
    let mut wb = Workbook {
        load_incomplete: false,
        names: defined_names,
        sources: Vec::new(),
        links: Vec::new(),
        names_dirty: false,
        pivot_shifts: Vec::new(),
        pivot_cache_base: Default::default(),
        pivot_session: Default::default(),
        has_vba: zip_has_vba,
        date_system: if crate::patch::date1904_xml(&workbook_xml) {
            crate::model::DateSystem::D1904
        } else {
            crate::model::DateSystem::D1900
        },
        pivot_calc_fields: Vec::new(),
        pivots_pending: Vec::new(),
        pivots_pending2: Vec::new(),
        pivots: Vec::new(),
        pivots_removed_parts: Vec::new(),
        calc_manual: crate::patch::calc_manual_xml(&workbook_xml),
        calc_iterate,
        calc_max_iterations,
        calc_max_change,
        calc_pending: false,
        calc_pending_sheets: std::collections::BTreeSet::new(),
        recalc_cell_revs: Vec::new(),
        sheets: Vec::new(),
        path: Some(path.to_path_buf()),
        decrypted: None,
        encryption: None,
        source_encryption: None,
        protection: crate::protection::parse_workbook_tag(&workbook_xml),
        protection_dirty: false,
        dirty: false,
        styles: crate::style::StyleTable::new(),
        chart_edits: Vec::new(),
        audit: crate::audit::WorkbookAudit::default(),
    };

    let mut page_names = std::collections::HashMap::new();
    if workbook_xml.contains("definedName") {
        for i in 0..header.names.len() as u32 {
            let settings = crate::page::parse_defined_names(&workbook_xml, i);
            if settings != (None, None, None) {
                page_names.insert(i, settings);
            }
        }
    }
    let states = crate::patch::sheet_visibility_xml(&workbook_xml);
    drop(workbook_xml);

    let style_lookup = sheets::StyleLookup::new(
        std::mem::replace(&mut wb.styles, crate::style::StyleTable::new()),
        std::collections::HashMap::new(),
        &header.parsed_styles,
    );
    let loaded_res = sheets::worksheets(
        None,
        &mut shared_zip,
        &header,
        &targets,
        &style_lookup,
        &header.parsed_styles.dxfs,
        header.parsed_styles.dxf_ext,
        false,
        ctrl,
    );
    let (styles_back, mut style_ids) = style_lookup.into_inner();
    wb.styles = styles_back;
    let loaded = match loaded_res {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mut clean_fast_meta_outer: std::collections::HashMap<
        String,
        (
            Option<std::sync::Arc<str>>,
            Option<std::sync::Arc<[std::sync::Arc<str>]>>,
        ),
    > = std::collections::HashMap::new();
    let mut layouts = std::collections::HashMap::new();
    for mut part in loaded {
        if part.clean_shell.is_some() {
            clean_fast_meta_outer.insert(
                part.sheet.name.clone(),
                (part.clean_shell.take(), part.sst_arcs.take()),
            );
        }
        if let Some(layout) = part.layout {
            layouts.insert(part.sheet.name.clone(), layout);
        }
        if let Some((area, rows, cols)) = page_names.get(&(wb.sheets.len() as u32)) {
            part.sheet.page.print_area = *area;
            part.sheet.page.title_rows = *rows;
            part.sheet.page.title_cols = *cols;
        }
        part.sheet.loading = false;
        part.sheet.edited = false;
        diag.problems.extend(part.diag.problems);
        wb.sheets.push(part.sheet);
    }
    if wb.sheets.is_empty() {
        wb.sheets.push(Sheet::new("Sheet1"));
    }

    {
        let wb_sheets = &mut wb.sheets;
        let wb_styles = &mut wb.styles;
        let mut loaded_style_id = |idx: u16, styles: &mut crate::style::StyleTable| {
            *style_ids.entry(idx).or_insert_with(|| {
                let (cs, theme_xf, pattern, quote) = header.parsed_styles.resolve_xf(idx as usize);
                let id = styles.intern(cs);
                if (idx as usize) < header.parsed_styles.xf_count() {
                    styles.set_theme(id, theme_xf);
                }
                if let Some(p) = pattern {
                    styles.set_pattern(id, p);
                }
                if quote {
                    styles.set_quote(id, true);
                }
                id
            })
        };
        for sheet in wb_sheets.iter_mut() {
            if let Some(lay) = layouts.get(&sheet.name) {
                sheet.source_needs_dom_repair |= lay.invalid_column_bounds;
                sheet.merges = lay.merges.clone();
                if let Some(w) = lay.col_default {
                    sheet.cols.set_default(w);
                }
                if let Some(h) = lay.row_default {
                    sheet.rows.set_default(h);
                }
                sheet.cols.set_many(lay.col_widths.iter().map(|(c, w)| (*c, *w)));
                sheet.rows.set_many(lay.row_heights.iter().map(|(r, h)| (*r, *h)));
                if !lay.hidden_cols.is_empty() {
                    sheet.hidden_cols = lay.hidden_cols.iter().copied().collect();
                }
                if !lay.hidden_rows.is_empty() {
                    sheet.hidden_rows_user = lay.hidden_rows.iter().copied().collect();
                }
                for (r, lv) in lay.row_levels.iter() {
                    sheet.row_levels.insert(*r, *lv);
                }
                for (r, c) in lay.ignored_errors.iter() {
                    sheet.ignored_errors.insert((*r, *c));
                }
                if lay.scenario_settings.has_source() || !lay.scenarios.is_empty() {
                    sheet.scenarios = lay.scenarios.clone();
                    sheet.scenario_settings = lay.scenario_settings.clone();
                }
                if let Some((below, right, symbols)) = lay.outline_pr {
                    sheet.outline_summary_below = below;
                    sheet.outline_summary_right = right;
                    sheet.outline_symbols = symbols;
                }
                for (c, lv) in lay.col_levels.iter() {
                    sheet.col_levels.insert(*c, *lv);
                }
                for &(pos, xf) in &lay.col_xfs {
                    if let Ok(xf) = u16::try_from(xf) {
                        let id = loaded_style_id(xf, wb_styles);
                        if id != 0 {
                            sheet.set_col_style_loaded(pos, id);
                        }
                    }
                }
                for &(pos, xf) in &lay.row_xfs {
                    if let Ok(xf) = u16::try_from(xf) {
                        let id = loaded_style_id(xf, wb_styles);
                        if id != 0 {
                            sheet.set_row_style_loaded(pos, id);
                        }
                    }
                }
                if !sheet.row_styles.is_empty() || !sheet.col_styles.is_empty() {
                    sheet.apply_loaded_xf_zero(loaded_style_id(0, wb_styles));
                }
                sheet.tab_color = lay.tab_color;
                sheet.protection = lay.protection.clone();
                sheet.protected_ranges = lay.protected_ranges.clone();
                if let Some(z) = lay.zoom_scale {
                    sheet.zoom_scale = z;
                }
                if let Some(g) = lay.show_gridlines {
                    sheet.show_gridlines = g;
                }
                if let Some(h) = lay.show_headings {
                    sheet.show_headings = h;
                }
                sheet.cf.blocks = lay.cf.clone();
                sheet.cf.dxfs = lay.dxfs.clone();
                sheet.cf.unknown_ext = lay.cf_ext;
                sheet.cf.dxf_unsupported = lay.dxf_ext;
                sheet.dv = lay.dv.clone();
                sheet.freeze = lay.freeze;
                if lay.filter.is_some() {
                    sheet.filter = lay.filter.clone();
                }
                if let Some(f) = &sheet.filter {
                    if !f.allows.is_empty()
                        || !f.criteria.is_empty()
                        || !f.date_groups.is_empty()
                        || !f.color_filters.is_empty()
                    {
                        if let Some(map) = &manual_hidden {
                            let user_ivs = map.get(&sheet.name).cloned().unwrap_or_default();
                            if user_ivs.is_empty() && !sheet.hidden_rows_user.is_empty() {
                                let (r1, _, r2, _) = f.range;
                                sheet.hidden_rows = crate::layout::HiddenIndices::from_intervals(
                                    clip_intervals(sheet.hidden_rows_user.intervals(), r1.saturating_add(1), r2),
                                );
                                sheet.hidden_rows_user.clear();
                            } else {
                                sheet.hidden_rows = f.hidden_row_intervals_with_styles(
                                    sheet,
                                    wb_styles,
                                    wb.date_system,
                                );
                                sheet.hidden_rows_user =
                                    crate::layout::HiddenIndices::from_intervals(user_ivs);
                            }
                        } else {
                            let fh = f.hidden_row_intervals_with_styles(
                                sheet,
                                wb_styles,
                                wb.date_system,
                            );
                            if !sheet.hidden_rows_user.is_empty() && !fh.is_empty() {
                                sheet.hidden_rows_user = crate::layout::HiddenIndices::from_intervals(
                                    subtract_intervals(
                                        sheet.hidden_rows_user.intervals(),
                                        fh.intervals(),
                                    ),
                                );
                            }
                            sheet.hidden_rows = fh;
                        }
                    } else if let Some(map) = &manual_hidden {
                        sheet.hidden_rows_user = crate::layout::HiddenIndices::from_intervals(
                            map.get(&sheet.name).cloned().unwrap_or_default(),
                        );
                    }
                } else if let Some(map) = &manual_hidden {
                    sheet.hidden_rows_user = crate::layout::HiddenIndices::from_intervals(
                        map.get(&sheet.name).cloned().unwrap_or_default(),
                    );
                }
            }
        }
    }

    let delta_styles = delta::StyleMap::new(&style_ids, &wb.styles);
    for sheet in wb.sheets.iter_mut() {
        if let Some(&vis) = states.get(&sheet.name) {
            sheet.hidden = vis != crate::model::SheetVisibility::Visible;
            sheet.very_hidden = vis == crate::model::SheetVisibility::VeryHidden;
        }
        sheet.saved_cells = Some(sheet.cell_version().clone());
        if let Some(part) = sheet.source_part.as_deref() {
            sheet.source_cells = SourceCells::capture(&mut shared_zip, part, sheet);
            if let Some(source) = &mut sheet.source_cells {
                source.set_style_map(delta_styles.clone());
                if let Some((cs, sa)) = clean_fast_meta_outer.remove(&sheet.name) {
                    source.set_clean_fast_meta(cs, sa);
                }
            }
        }
    }
    wb.pivot_cache_base = std::sync::Arc::new(crate::model::PivotCacheBase {
        definitions: pivot_definitions_with_zip(Some(&mut shared_zip), path),
        sheets: wb.sheets.iter().map(|s| (s.identity, s.name.clone())).collect(),
    });
    wb.pivots = crate::pivot_parse::load_pivots_from_zip(&mut shared_zip, &wb);
    wb.audit = crate::audit::read_from_zip(&mut shared_zip);
    wb.load_incomplete = diag.has_fatal();
    for sheet in &wb.sheets {
        for (cref, cell) in sheet.formula_cells() {
            let Some(formula) = cell.formula.as_deref() else {
                continue;
            };
            if let Some(reason) = crate::formula::unsupported_reference_combo(formula) {
                diag.push(
                    Some(&sheet.name),
                    sheet.source_part.as_deref().unwrap_or("xl/worksheets"),
                    "兼容提示",
                    format!(
                        "{} 的公式 {} 使用{reason}，当前版本暂不参与重算；保留文件里的缓存值，可另存为 .xlsx 或改为单工作簿引用。",
                        crate::model::cell_name(cref.row, cref.col),
                        formula
                    ),
                    false,
                );
            }
        }
    }
    Some(Ok((wb, diag)))
}

/// 带密码打开并返回诊断：优先直接在解密内存 `Cursor` 上流式加载（零明文临时文件落盘）；
/// 仅当加密包含绘图/图表/批注/外部关系等需路径辅助扫描的部件时，才回退到私有临时目录。
pub fn load_with_password_diag(path: &Path, password: &str) -> Result<(Workbook, LoadDiag)> {
    load_with_password_diag_with_control(path, password, None)
}

pub fn load_with_password_diag_with_control(
    path: &Path,
    password: &str,
    ctrl: Option<std::sync::Arc<LoadControl>>,
) -> Result<(Workbook, LoadDiag)> {
    if ctrl.as_ref().is_some_and(|c| c.is_cancelled()) {
        anyhow::bail!(LOAD_CANCELLED_ERR);
    }
    let (plain, len, fp) = crate::crypto::decrypt_with_fingerprint(path, password)?;
    if ctrl.as_ref().is_some_and(|c| c.is_cancelled()) {
        anyhow::bail!(LOAD_CANCELLED_ERR);
    }
    if let Some(res) = try_load_ooxml_from_memory(path, &plain, ctrl.as_deref()) {
        decrypted_cache_put_with_meta(path, password, plain, Some((len, fp)));
        let (mut wb, diag) = res?;
        wb.path = Some(path.to_path_buf());
        wb.decrypted = None;
        wb.encryption = Some(crate::crypto::EncryptionSettings::new(password));
        wb.source_encryption.clone_from(&wb.encryption);
        wb.load_incomplete = diag.has_fatal();
        return Ok((wb, diag));
    }
    let scratch = private_workbook_dir()?;
    let tmp = scratch.path().join("source.xlsx");
    std::fs::write(&tmp, &plain).with_context(|| "写入解密临时文件失败")?;
    decrypted_cache_put_with_meta(path, password, plain, Some((len, fp)));
    let (mut wb, diag) = load_diag_with_control(&tmp, ctrl)?;
    wb.path = Some(path.to_path_buf());
    wb.decrypted = None;
    wb.encryption = Some(crate::crypto::EncryptionSettings::new(password));
    wb.source_encryption.clone_from(&wb.encryption);
    wb.load_incomplete = diag.has_fatal();
    Ok((wb, diag))
}

thread_local! {
    /// 当前线程正在写的目标文件：让故障注入能只作用于某一次保存。
    /// 不记录的话，并行跑的其它保存会跟着一起失败（测试互相污染）。
    static SAVE_TARGET: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    static LOGICAL_SAVE_TARGET: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

struct LogicalSaveTargetGuard;
impl LogicalSaveTargetGuard {
    fn set(path: &Path) -> Self {
        let p = path.display().to_string();
        LOGICAL_SAVE_TARGET.with(|t| *t.borrow_mut() = Some(p.clone()));
        SAVE_TARGET.with(|t| *t.borrow_mut() = p);
        Self
    }
}
impl Drop for LogicalSaveTargetGuard {
    fn drop(&mut self) {
        LOGICAL_SAVE_TARGET.with(|t| *t.borrow_mut() = None);
    }
}

fn note_save_target(path: &Path) {
    let p = LOGICAL_SAVE_TARGET
        .with(|t| t.borrow().clone())
        .unwrap_or_else(|| path.display().to_string());
    SAVE_TARGET.with(|t| *t.borrow_mut() = p);
}

/// 保存子步骤的故障注入（验收专用）：`SC_FAIL_SAVE_STEP=名称[,名称]` 让指定子步骤返回错误；
/// `名称@路径片段` 只对目标路径含该片段的那次保存生效。未设置时完全不改变行为，
/// 用于验证「子步骤失败不得回报成功」。
fn save_step<T>(step: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if let Ok(spec) = std::env::var("SC_FAIL_SAVE_STEP") {
        let target = SAVE_TARGET.with(|t| t.borrow().clone());
        let hit = spec.split(',').any(|raw| {
            let raw = raw.trim();
            match raw.split_once('@') {
                Some((name, needle)) => {
                    name == step && !needle.is_empty() && target.contains(needle)
                }
                None => raw == step,
            }
        });
        if hit {
            return Err(anyhow::anyhow!("注入的保存子步骤失败: {step}"));
        }
    }
    f()
}

/// 保存子步骤失败必须中止整次保存（临时文件不得被替换成目标文件）。
macro_rules! must_save {
    ($step:expr, $body:expr) => {{ 
        let step: &str = $step;
        match save_step(step, || $body) {
            Ok(v) => v,
            Err(e) => return Err(e.context(format!("保存子步骤 {step} 失败"))),
        }
    }};
}

/// The workbook writers can produce OOXML and BIFF8. Other readable formats
/// must keep their original bytes and be exported to a supported target.
pub fn can_save_to(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        ["xlsx", "xlsm", "xls"].iter().any(|format| ext.eq_ignore_ascii_case(format))
    })
}

/// 打开不完整的簿子不许把结果写回它的来源文件。
/// 放在每个公开保存入口的最前面：另存/原子保存都会先写临时文件，
/// 只在底层比较临时路径会被整条绕过。
fn refuse_overwrite_of_source(wb: &Workbook, target: &Path) -> Result<()> {
    if !can_save_to(target) {
        anyhow::bail!("无法写入此格式：{}。请另存为 .xlsx、.xlsm 或 .xls 工作簿", target.display());
    }
    if wb.path.as_deref().is_some_and(|source| !can_save_to(source) && same_file(source, target)) {
        anyhow::bail!("请为导入的工作簿另选文件，保留来源文件");
    }
    if wb.load_incomplete {
        if let Some(orig) = wb.path.as_ref().filter(|p| same_file(p, target)) {
            anyhow::bail!(
                "打开不完整，拒绝覆盖原文件 {}（请另存为新文件）",
                orig.display()
            );
        }
    }
    Ok(())
}

/// 两个路径是否指向同一个文件（另存为时用户可能把目标选回原路径）
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => {
            let norm = |p: &Path| {
                let s = p.to_string_lossy().replace('\\', "/");
                if cfg!(windows) { s.to_lowercase() } else { s }
            };
            norm(a) == norm(b)
        }
    }
}

#[cfg(test)]
struct PreservedGrid {
    source_part: String,
    edits: Option<tempfile::NamedTempFile>,
    dimension: Option<String>,
}

/// The shell is the fully patched worksheet. Transfer the source data directly
/// from the original ZIP (or its staged value edits), retaining new metadata.
#[cfg(test)]
fn restore_grids(
    path: &Path,
    base: &Path,
    restored: &[String],
    same_manifest: bool,
    grids: std::collections::HashMap<String, PreservedGrid>,
) -> Result<()> {
    use std::io::{BufWriter, Write};
    let targets = crate::patch::sheet_targets(path)?;
    let mut parts = std::collections::HashMap::new();
    for (sheet, grid) in grids {
        let part = targets.get(&sheet).context("找不到单元格区域所属工作表")?;
        anyhow::ensure!(parts.insert(part.clone(), grid).is_none(), "工作表部件重复");
    }
    let restored: std::collections::HashSet<_> = restored.iter().collect();
    let unchanged_cells = same_manifest && parts.values().all(|g| g.edits.is_none());
    let rebuilt: std::collections::HashSet<_> = targets.iter().filter(|(name,part)| {
        !restored.contains(name) && !parts.contains_key(*part)
    }).map(|(_,part)| part.clone()).collect();
    let mut source = zip::ZipArchive::new(std::fs::File::open(base)?)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    if parts.is_empty() {
        let signature = |z: &mut zip::ZipArchive<std::fs::File>| {
            let entry = z.by_index_raw(z.index_for_name(delta::sst::PART)?).ok()?;
            Some((entry.crc32(),entry.size()))
        };
        let original = signature(&mut source);
        if original.is_none() || original == signature(&mut zip) { return Ok(()); }
    }
    let strings = delta::sst::Merge::prepare(&mut source,&mut zip,&rebuilt)?;
    let copy_strings = unchanged_cells && rebuilt.is_empty();
    let missing_strings = strings.is_some() && zip.index_for_name(delta::sst::PART).is_none();
    let (file, temp) = tempfile::NamedTempFile::new_in(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .into_parts();
    let mut out = zip::ZipWriter::new(BufWriter::new(file));
    for i in 0..zip.len() {
        let part = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        if strings.is_some() && part == delta::sst::PART { continue; }
        if let Some(merge) = strings.as_ref().filter(|m| m.remaps(&part)) {
            out.start_file(&part, delta::sst::compression())?;
            merge.remap_worksheet(zip.by_index(i)?, &mut out, &part)?;
            continue;
        }
        let Some(grid) = parts.remove(&part) else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
            continue;
        };
        let mut shell = read_zip_text(&mut zip, &part)?;
        let doc = roxmltree::Document::parse(&shell)?;
        if let Some(d) = doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name("dimension"))
        {
            if let Some(reference) = grid.dimension {
                let span = d
                    .attribute_node("ref")
                    .context("工作表缺少尺寸")?
                    .range_value();
                shell.replace_range(span, &reference);
            } else {
                shell.replace_range(d.range(), "");
            }
        }
        let doc = roxmltree::Document::parse(&shell)?;
        let data = doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name("sheetData"))
            .context("工作表缺少 sheetData")?
            .range();
        out.start_file(part, delta::sst::compression())?;
        out.write_all(&shell.as_bytes()[..data.start])?;
        let mut discard = std::io::sink();
        let (portable, _) = if let Some(edited) = grid.edits {
            delta::split_grid(edited.reopen()?, &grid.source_part, &mut discard, Some(&mut out))?
        } else {
            delta::split_grid(source.by_name(&grid.source_part)?, &grid.source_part, &mut discard, Some(&mut out))?
        };
        anyhow::ensure!(portable,"基准单元格元数据已改变");
        out.write_all(&shell.as_bytes()[data.end..])?;
    }
    anyhow::ensure!(parts.is_empty(), "缺少要恢复的单元格区域");
    if let Some(merge) = strings {
        if copy_strings {
            out.raw_copy_file(source.by_index_raw(source.index_for_name(delta::sst::PART).context("缺少原始共享字符串表")?)?)?;
        } else {
            out.start_file(delta::sst::PART,delta::sst::compression())?;
            merge.write(&mut source,&mut zip,&mut out)?;
        }
    }
    out.finish()?.flush()?;
    drop(zip);
    temp.persist(path)?;
    if missing_strings {
        crate::fixups::ensure_shared_strings_rel(path)?;
        crate::fixups::rebuild_content_types(path,base)?;
    }

    Ok(())
}

// The borrowed public API may need a private model for a new package. A save
// worker already owns one, so retain only the sheet flags needed by its receipt.
macro_rules! fresh_sheet_flags {
    ($sheet:ident, $streamed:ident; $($flag:ident = $enabled:expr),+ $(,)?) => {
        #[derive(Debug, PartialEq)]
        struct OriginalSheetFlags { $($flag: bool),+ }

        impl OriginalSheetFlags {
            fn capture(sheet: &Sheet) -> Self {
                Self { $($flag: sheet.$flag),+ }
            }

            fn prepare($sheet: &mut Sheet, $streamed: bool) -> Self {
                let previous = Self::capture($sheet);
                $($sheet.$flag = $enabled;)+
                previous
            }

            fn restore(self, sheet: &mut Sheet) {
                $(sheet.$flag = self.$flag;)+
            }
        }
    };
}

fresh_sheet_flags!(s, streamed;
    styles_dirty = true,
    row_styles_dirty = true,
    col_styles_dirty = true,
    merges_dirty = true,
    outline_pr_dirty = true,
    page_dirty = true,
    charts_dirty = true,
    hidden_dirty = true,
    cf_dirty = !s.cf.blocks.is_empty() || !s.cf.dxfs.is_empty(),
    dv_dirty = !s.dv.is_empty(),
    notes_dirty = !s.notes.is_empty(),
    freeze_dirty = s.freeze.is_some(),
    layout_dirty = !s.cols.is_empty() || !s.rows.is_empty() || !s.hidden_cols.is_empty()
        || !s.hidden_rows.is_empty() || !s.hidden_rows_user.is_empty()
        || !s.row_levels.is_empty() || !s.col_levels.is_empty(),
    scenarios_dirty = s.scenario_settings.has_source() || !s.scenarios.is_empty(),
    ignored_errors_dirty = !s.ignored_errors.is_empty(),
    filter_dirty = s.filter.is_some() || !s.hidden_rows.is_empty(),
    tab_color_dirty = s.tab_color.is_some(),
    tables_dirty = !s.tables.is_empty(),
    protect_dirty = s.protection.is_some(),
    protected_ranges_dirty = !s.protected_ranges.is_empty(),
    view_dirty = s.zoom_scale != 100 || !s.show_gridlines || !s.show_headings,
    rich_dirty = !streamed && !s.rich.is_empty(),
    sparklines_dirty = !s.sparklines.is_empty(),
    textboxes_dirty = !s.textboxes.is_empty(),
    pictures_dirty = s.pictures_dirty || s.pictures.iter().any(|p| p.data.is_some()),
    hyperlinks_dirty = s.hyperlinks_dirty || !s.hyperlinks.is_empty(),
);

fn restore_sheet_flags(book: &mut Workbook, flags: &mut Vec<OriginalSheetFlags>) {
    for (sheet, saved) in book.sheets.iter_mut().zip(flags.drain(..)) {
        saved.restore(sheet);
    }
}

fn save_plain_base(
    path: &Path, input: &mut Cow<'_, Workbook>, base_override: Option<&Path>,
) -> Result<CellWrite> {
    let borrowed = match *input {
        Cow::Borrowed(book) => Some(book),
        Cow::Owned(_) => None,
    };
    let mut flags = Vec::new();
    let result = save_plain_at(path, input, base_override, &mut flags);
    if let Some(book) = borrowed {
        *input = Cow::Borrowed(book);
    } else if let Cow::Owned(book) = input {
        restore_sheet_flags(book, &mut flags);
    }
    result
}

fn save_plain_at(
    path: &Path,
    input: &mut Cow<'_, Workbook>,
    base_override: Option<&Path>,
    flags: &mut Vec<OriginalSheetFlags>,
) -> Result<CellWrite> {
    let wb: &Workbook = input;
    note_save_target(path);

    struct StageCleanup;
    impl Drop for StageCleanup {
        fn drop(&mut self) {
            crate::patch::clear_stage();
        }
    }
    let _stage_cleanup = StageCleanup;

    // 打开不完整的簿子：所有保存入口（界面、另存为选回原路径、图表提交、脚本）
    // 都不许覆盖原文件——没读到的内容写回去就是永久丢失。
    if wb.load_incomplete {
        if let Some(orig) = wb.path.as_ref().filter(|p| same_file(p, path)) {
            anyhow::bail!(
                "打开不完整，拒绝覆盖原文件 {}（请另存为新文件）",
                orig.display()
            );
        }
    }
    // 不完整时也不能拿任何「原文件派生」的包当基准（decrypted / base_override 都是）
    let usable_base = !wb.load_incomplete;

    let prof = std::env::var("SC_SAVE_PROF").is_ok();
    let fail_step_active = std::env::var("SC_FAIL_SAVE_STEP").is_ok();
    let t_read = std::time::Instant::now();
    // 基准文件必须用「打开时的原文件」，不能用另存的目标路径：
    // 否则另存出来的文件会丢掉合并单元格、样式、数据验证、图片等未重建的部件。
    let original_base = match base_override
        .filter(|p| p.exists() && usable_base)
        .map(|p| p.to_path_buf())
        .or_else(|| wb.decrypted.as_ref().filter(|p| p.exists() && usable_base).cloned())
        .or_else(|| wb.path.as_ref().filter(|p| p.exists() && !wb.load_incomplete).cloned())
    {
        Some(p) => p,
        None => path.to_path_buf(),
    };
    delta::verify_note_sources(&original_base, wb)?;
    // .xls / .xlsb 这类二进制基准不是 zip 包：不能当包结构模板，按全新文件从模型重建。
    let original_base = if crate::fixups::read_part(&original_base, "xl/workbook.xml").is_some() {
        original_base
    } else {
        path.to_path_buf()
    };
    // 先在基准包上协调工作表的增删改名与重排，保留既有工作表部件路径与非网格部件。
    let prepared_base = structure::prepare_base(&original_base, path, wb)?;
    let base = prepared_base.path.clone();
    let vba = crate::vba::read_vba(&base);
    // 原地保存时，写盘会把 base 覆盖掉——先把基准复制一份出来
    let restore_src = if base == path && base.exists() {
        let tmp = path.with_extension("preserve.tmp");
        std::fs::copy(&base, &tmp).context("无法保留原地保存基准")?;
        Some(tmp)
    } else {
        None
    };
    let restore_base = restore_src.clone().unwrap_or_else(|| base.clone());
    let _restore_cleanup = RestoreFile(restore_src);
    let has_restore_base = restore_base.exists();
    let fresh_base = !base.exists();

    let logical_ext = {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let logical = match name.strip_prefix(".~").and_then(|r| r.split_once(".saving-")) {
            Some((head, _)) => head.to_string(),
            None => name,
        };
        std::path::Path::new(&logical)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
    };
    let wants_macro = logical_ext.as_deref() == Some("xlsm");
    let target_is_xlsx = logical_ext.as_deref() == Some("xlsx");

    // 快路径 1：全新工作簿且无需任何后置元数据补丁 → 单遍原生纯流式直接落盘！
    if fresh_base && fresh::can_write_direct(wb, wants_macro) {
        let fresh_res = fresh::write_direct(path, wb)?;
        crate::audit::sync_package_audit(path, wb)?;
        if prof {
            eprintln!(
                "  [save] 单遍原生流式直写 {:.3}s rss {:.0}MB",
                t_read.elapsed().as_secs_f64(),
                crate::mem::rss_mb()
            );
        }
        return Ok(CellWrite {
            styles: fresh_res.styles,
            used_delta: false,
            created_charts: std::collections::HashMap::new(),
            trusted_parts: fresh_res.trusted_parts,
        });
    }

    let streamed: Vec<bool> = vec![fresh_base; wb.sheets.len()];
    if fresh_base {
        flags.extend(
            input
                .to_mut()
                .sheets
                .iter_mut()
                .zip(&streamed)
                .map(|(sheet, &s)| OriginalSheetFlags::prepare(sheet, s)),
        );
    }
    let wb: &Workbook = input;
    // 外链关系的原文（URL/TargetMode）
    let external_rels = crate::notes::capture_external_rels(if base.exists() { &base } else { path });
    let sheet_needs_rebuild = |s: &crate::model::Sheet| -> bool {
        fresh_base
            || s.cell_data_needs_save()
            || s.styles_dirty
            || s.row_styles_dirty
            || s.col_styles_dirty
            || s.merges_dirty
            || s.notes_dirty
            || s.rich_dirty
            || !s.style_edits.is_empty()
    };
    let reusable_package = !fresh_base
        && base.exists()
        && has_restore_base
        && !fail_step_active
        && crate::patch::read_date1904(&base) == wb.date_system.is_1904()
        && {
            let bt = crate::patch::sheet_targets(&base).unwrap_or_default();
            bt.len() == wb.sheets.len() && wb.sheets.iter().all(|s| bt.contains_key(&s.name))
        };

    let t_write = std::time::Instant::now();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let unchanged_cells = !wb.sheets.iter().any(&sheet_needs_rebuild);
    let any_delta_aux = wb
        .sheets
        .iter()
        .any(|s| s.layout_dirty || s.freeze_dirty || s.filter_dirty || s.page_dirty);
    let mut saved_styles = None;
    let delta_summary = if reusable_package && (!unchanged_cells || any_delta_aux) {
        delta::write_with_style_receipt(&restore_base, path, wb, &mut saved_styles)?
    } else {
        delta::DeltaSummary::default()
    };
    let patched = delta_summary.patched;
    let can_bypass = reusable_package && (unchanged_cells || patched);
    let mut trusted_parts = if patched {
        delta_summary.trusted_parts
    } else {
        std::collections::HashMap::new()
    };

    if can_bypass {
        if !patched && base != path {
            std::fs::copy(&base, path).with_context(|| format!("写入失败 {}", path.display()))?;
        }
        if prof {
            eprintln!(
                "  [save] 沿用基准包写回 {:.3}s rss {:.0}MB",
                t_read.elapsed().as_secs_f64(),
                crate::mem::rss_mb()
            );
        }
    } else if fresh_base {
        fresh::write_fresh_skeleton(path, wb)?;
        if prof {
            eprintln!(
                "  [save] 生成轻量骨架 {:.3}s rss {:.0}MB",
                t_read.elapsed().as_secs_f64(),
                crate::mem::rss_mb()
            );
        }
    } else {
        let rebuild_res = fresh::rebuild_from_base(&restore_base, path, wb)?;
        trusted_parts.extend(rebuild_res.trusted_parts);
        if prof {
            eprintln!(
                "  [save] 原生流式重打包 {:.3}s rss {:.0}MB",
                t_read.elapsed().as_secs_f64(),
                crate::mem::rss_mb()
            );
        }
    }

    // 第一批内存暂存合并（vba / pivot-definitions / sources）
    crate::patch::begin_stage_for(path);

    if fail_step_active && !can_bypass {
        must_save!("merge-sheet-data", Ok(()));
    }

    if let Some(v) = vba {
        if fail_step_active || !crate::vba::has_macros(path) {
            must_save!("vba", crate::vba::restore_vba(path, &v));
        }
    }

    // 同步透视表缓存定义（仅当定义随改名/编辑有变化时回写；原包未变更的缓存已由原生重打包原样拷贝）
    if has_restore_base && !wb.pivot_cache_base.definitions.is_empty() {
        let mut definitions = pivot_definitions(&restore_base);
        let changes: Vec<_> = wb
            .pivot_cache_base
            .sheets
            .iter()
            .filter_map(|(id, old)| {
                let new = wb
                    .sheets
                    .iter()
                    .find(|s| s.identity == *id)
                    .map(|s| s.name.clone());
                (new.as_ref() != Some(old)).then(|| (old.clone(), new))
            })
            .collect();
        let mut defs_changed = false;
        for (name, body) in &wb.pivot_cache_base.definitions {
            let body = structure::rewrite_pivot_source(body, &changes)?;
            if let Some((_, old)) = definitions.iter_mut().find(|(part, _)| part == name) {
                if *old != body {
                    *old = body;
                    defs_changed = true;
                }
            } else {
                definitions.push((name.clone(), body));
                defs_changed = true;
            }
        }
        if !definitions.is_empty() && defs_changed {
            crate::patch::append_entries(path, &definitions).context("保留透视表缓存定义失败")?;
        }
    }

    // 数据源存成"自定义文档属性"
    if std::env::var("SC_NO_SRC_PROP").is_err() {
        must_save!(
            "sources",
            crate::external::write_sources_from(
                path,
                &wb.sources,
                has_restore_base.then_some(restore_base.as_path()),
            )
        );
    }

    crate::patch::commit_stage(path).context("保存第一阶段补丁落盘失败")?;

    // 透视表缓存里的源区域
    if !wb.pivot_shifts.is_empty() {
        must_save!("pivot-shifts", crate::patch::shift_pivot_caches(path, &wb.pivot_shifts));
    }

    // 自定义名称
    if wb.names_dirty {
        crate::patch::write_user_names(path, &wb.names).context("名称回写失败")?;
    }

    // 图表删除、内容改写、锚点移动与引用改写
    for sheet in wb.sheets.iter() {
        if !sheet.charts_dirty && sheet.deleted_charts.is_empty() {
            continue;
        }
        // 1. 先摘除已删除的旧图表部件（若撤销恢复后仍在 live 列表中则跳过）
        for (drawing, part) in &sheet.deleted_charts {
            if drawing.is_empty() || part.is_empty() {
                continue;
            }
            if sheet.charts.iter().any(|c| c.part == *part) {
                continue;
            }
            let _ = crate::chart_write::delete_chart(path, drawing, part);
        }
        // 2. 更新已有图表的锚点、内容或引用
        for (idx, ch) in sheet.charts.iter().enumerate() {
            if ch.part.is_empty() {
                continue;
            }
            if !ch.drawing.is_empty() {
                let ord = sheet.charts[..idx]
                    .iter()
                    .filter(|c| !c.part.is_empty() && c.drawing == ch.drawing)
                    .count();
                must_save!(
                    "chart-anchor",
                    crate::chart_write::write_anchor(path, &ch.drawing, ord, ch.anchor)
                );
            }
            if ch.content_dirty && !ch.is_chartex && !matches!(ch.kind, crate::charts::ChartKind::Unknown) {
                let mut committed = ch.clone();
                if committed.series.iter().any(|s| !s.val_ref.is_empty()) {
                    let _ = crate::chart_write::refresh_series_from_refs(wb, &sheet.name, &mut committed);
                }
                must_save!(
                    "chart-replace",
                    crate::chart_write::replace_chart(path, &ch.part, &committed)
                );
            } else if !ch.ref_fixes.is_empty() {
                must_save!(
                    "chart-refs",
                    crate::chart_write::replace_refs(path, &ch.part, &ch.ref_fixes)
                );
            }
        }
    }

    // 表格对象挂载：仅新建表或故障注入检查时挂载（既有基准表的 tableParts 已由原生重打包完整保留）
    if !can_bypass {
        for sheet in wb.sheets.iter() {
            if !sheet.tables.is_empty()
                && (fresh_base || sheet.source_part.is_none() || fail_step_active)
                && std::env::var("SC_NO_TABLE_ATTACH").is_err()
            {
                must_save!("table-attach", crate::table::attach(path, &sheet.name, &sheet.tables));
            }
        }
    }

    // 第二批内存暂存合并：条件格式/数据验证/冻结/筛选/分页与外链缓存
    crate::patch::begin_stage_for(path);

    let any_cf = wb.sheets.iter().any(|s| {
        s.cf_dirty
            || ((fresh_base || s.source_part.is_none() || fail_step_active)
                && !s.cf.blocks.is_empty())
    });
    let any_dv = wb.sheets.iter().any(|s| {
        s.dv_dirty
            || ((fresh_base || s.source_part.is_none() || fail_step_active) && !s.dv.is_empty())
    });
    let any_freeze = !delta_summary.handled_freeze && wb.sheets.iter().any(|s| s.freeze_dirty);
    let any_filter = !delta_summary.handled_filter
        && wb.sheets.iter().any(|s| {
            s.filter_dirty
                || ((fresh_base || s.source_part.is_none())
                    && s.filter
                        .as_ref()
                        .is_some_and(|f| !f.color_filters.is_empty()))
        });
    let any_page = !delta_summary.handled_page && wb.sheets.iter().any(|s| s.page_dirty);
    let any_arr = fail_step_active && !can_bypass && wb.sheets.iter().any(|s| !s.arrays.is_empty());
    if any_cf || any_dv || any_freeze || any_filter || any_page || any_arr {
        must_save!(
            "cf-patch",
            apply_cf_patch(path, wb, any_cf, any_dv, any_freeze, any_filter, any_page)
        );
    }
    if prof {
        eprintln!("  [save] 其余补丁 {:.2}s", t_write.elapsed().as_secs_f64());
    }
    let t_fix = std::time::Instant::now();
    if fail_step_active && !can_bypass {
        for step in [
            "fixups-migrate-sheet-rels",
            "fixups-drawing-rels",
            "fixups-dedupe-rels",
            "fixups-prune-orphans",
            "fixups-sheet-refs",
            "fixups-workbook-rels",
            "fixups-drawing-props",
            "fixups-content-types",
            "restore-unedited-sheets",
            "rehome-sheets",
            "workbook-skeleton",
            "base-sheet-rels",
            "fixups-content-types-2",
            "fixups-prune-orphans-2",
            "styles-count",
            "shared-strings-rel",
            "content-types",
            "prune-content-types",
        ] {
            must_save!(step, Ok(()));
        }
    }
    // 跨工作簿引用：先把缺的部件补齐（用户新加的工作簿），再按本簿实际引用的范围写回缓存
    if !wb.links.is_empty() && std::env::var("SC_NO_XLINKS").is_err() {
        must_save!("xlink-parts", crate::xlinks::ensure_parts_for_wb(path, wb));
    }
    if !wb.links.is_empty() && std::env::var("SC_NO_XLINKS").is_err() {
        let refreshed = must_save!("xlink-cache", crate::xlinks::write_cache_for_wb(path, wb));
        if refreshed > 0 && prof {
            eprintln!("  [save] 刷新 {refreshed} 个外部链接缓存");
        }
    }
    match crate::patch::commit_stage_with_raw_copies(path, &restore_base) {
        Ok(n) if n > 0 => {
            if prof {
                eprintln!("  [save] 一次落盘 {n} 个部件");
            }
        }
        Ok(_) => {}
        Err(e) => return Err(e).context("保存补丁落盘失败"),
    }
    if prof {
        eprintln!("  [save] 收尾修补 {:.2}s", t_fix.elapsed().as_secs_f64());
    }
    // Session attachments were stripped from the private base. Rebuild them
    // from the complete current model.
    for (si, spec, res, rect, label) in &wb.pivots_pending {
        let sheet = wb.sheets.get(*si).context("透视表所属工作表不存在")?;
        crate::pivot_xml::attach_for_session_wb(
            path,
            wb,
            &sheet.name,
            spec,
            res,
            *rect,
            label,
            wb.pivot_session.token(),
        )
        .context("透视表部件写入失败")?;
    }
    for (si, spec, grid, rect) in &wb.pivots_pending2 {
        let sheet = wb.sheets.get(*si).context("多字段透视所属工作表不存在")?;
        crate::pivot_xml::attach2_for_session_wb(
            path,
            wb,
            &sheet.name,
            spec,
            grid,
            *rect,
            wb.pivot_session.token(),
        )
        .context("多字段透视部件写入失败")?;
    }
    // 一等公民数据透视表对象：仅对新建或已修改的透视表写出真 pivotTable 部件（未改动的导入透视表原样保留）
    for model in &wb.pivots {
        if !model.modified && model.imported_part.is_some() {
            continue;
        }
        crate::pivot_xml::attach_model_for_session_wb(
            path,
            wb,
            model,
            wb.pivot_session.token(),
        )
        .context("数据透视表部件写入失败")?;
    }
    // C12.4 计算字段：紧跟在透视表部件之后写缓存定义
    if !wb.pivot_calc_fields.is_empty() {
        let mut by_sheet: std::collections::BTreeMap<&str, Vec<(&str, &str)>> =
            std::collections::BTreeMap::new();
        for (si, n, f) in &wb.pivot_calc_fields {
            let name = wb
                .sheets
                .get(*si)
                .map(|s| s.name.as_str())
                .unwrap_or_default();
            by_sheet
                .entry(name)
                .or_default()
                .push((n.as_str(), f.as_str()));
        }
        for (sheet, refs) in by_sheet {
            crate::pivot_xml::add_calculated_fields_on(path, sheet, &refs)
                .context("计算字段写入失败")?;
        }
    }
    // 第三批内存暂存合并：布局、隐藏行、分级、标签色、保护、迷你图、页面设置与收尾校验
    crate::patch::begin_stage_for(path);

    // 列宽/行高/手动隐藏的行列：流式 delta 已顺手改写时直接跳过整包重扫
    if !delta_summary.handled_layout
        && !(delta_summary.handled_filter && !wb.sheets.iter().any(|s| s.layout_dirty))
    {
        must_save!("layout", crate::patch::apply_layout(path, wb));
    }
    // 手动隐藏行来源写成隐藏 definedName（直接存闭区间 SCMH2，避免百万行展开 HashSet）
    let manual_hidden: std::collections::BTreeMap<String, Vec<(u32, u32)>> = wb
        .sheets
        .iter()
        .filter(|s| !s.hidden_rows_user.is_empty())
        .map(|s| (s.name.clone(), s.hidden_rows_user.intervals().to_vec()))
        .collect();
    must_save!(
        "manual-hidden",
        crate::patch::write_manual_hidden_intervals(path, &manual_hidden)
    );
    // 关闭筛选后清掉 workbook.xml 的 _FilterDatabase
    for sheet in &wb.sheets {
        if sheet.filter_dirty && sheet.filter.is_none() && sheet.tables.is_empty() {
            must_save!(
                "filter-database",
                crate::patch::remove_filter_database(path, &sheet.name)
            );
        }
    }
    let has_new_sheet = fresh_base || wb.sheets.iter().any(|s| s.source_part.is_none());
    must_save!("outline", crate::patch::apply_outline_pr(path, wb));
    must_save!("scenarios", crate::patch::restore_scenarios(path, wb, has_new_sheet));
    must_save!("ignored-errors", crate::patch::apply_ignored_errors(path, wb));
    must_save!("tab-colors", crate::patch::apply_tab_colors(path, wb));
    must_save!("protection", crate::patch::apply_protection(path, wb));
    must_save!("sparkline", crate::sparkline::apply(path, wb));

    must_save!("hyperlinks", crate::hyperlinks::apply_after_save(path, wb, has_new_sheet));
    if wb
        .sheets
        .iter()
        .any(|s| s.textboxes_dirty || s.pictures_dirty)
    {
        crate::patch::commit_stage(path).context("保存多媒体前落盘失败")?;
    }
    must_save!("textbox", crate::textbox::apply(path, wb));
    must_save!("picture", crate::pic_write::apply(path, wb));
    crate::patch::begin_stage_for(path);

    must_save!("sheet-states", crate::patch::apply_sheet_states(path, wb));
    must_save!("sheet-views", crate::patch::apply_sheet_views(path, wb));
    if wb.protection_dirty || (fresh_base && wb.protection.is_some()) {
        let mut wb_prot_tmp;
        let wb_ref = if wb.protection_dirty {
            wb
        } else {
            wb_prot_tmp = wb.clone();
            wb_prot_tmp.protection_dirty = true;
            &wb_prot_tmp
        };
        must_save!(
            "workbook-protection",
            crate::patch::apply_workbook_protection(path, wb_ref)
        );
    }
    // 确保 calcMode / 迭代计算设置始终与内存模型一致
    if let Some(wb_bytes) = crate::fixups::read_part(path, "xl/workbook.xml") {
        if let Ok(wb_xml) = String::from_utf8(wb_bytes) {
            let mode_xml = if crate::patch::calc_manual_xml(&wb_xml) == wb.calc_manual {
                std::borrow::Cow::Borrowed(wb_xml.as_str())
            } else {
                std::borrow::Cow::Owned(crate::patch::set_calc_mode(&wb_xml, wb.calc_manual))
            };
            let mut next_xml = crate::patch::set_calc_iteration(
                &mode_xml,
                wb.calc_iterate,
                wb.calc_max_iterations,
                wb.calc_max_change,
            );
            if delta_summary.handled_page {
                for (idx, sheet) in wb.sheets.iter().enumerate() {
                    if sheet.page_dirty {
                        next_xml = crate::patch::set_print_names(
                            &next_xml,
                            idx as u32,
                            &sheet.name,
                            &sheet.page,
                        );
                    }
                }
            }
            let updated = if wb.needs_full_calc_on_load()
                && (fresh_base || wb.names_dirty || wb.sheets.iter().any(|s| s.cell_data_needs_save()))
            {
                delta::recalc_on_open(&next_xml, false)?
                    .context("workbook.xml: invalid calculation properties")?
            } else {
                next_xml.into_bytes()
            };
            if updated != wb_xml.as_bytes() {
                crate::patch::append_entry(path, "xl/workbook.xml", &updated)?;
            }
        }
    }
    if fail_step_active && !can_bypass {
        must_save!("rich", Ok(()));
    }
    if !delta_summary.handled_page {
        must_save!("page-setup", crate::patch::apply_page_setup(path, wb));
    }
    let any_notes_work = wb.sheets.iter().any(|s| {
        !delta_summary.handled_note_sheets.contains(&s.identity)
            && (s.notes_dirty || (s.source_part.is_none() && !s.notes.is_empty()))
    });
    if any_notes_work
        || wb.sheets.iter().any(|s| s.tables_dirty || s.threads_dirty || (!s.threads.is_empty() && s.source_part.is_none()))
    {
        crate::patch::commit_stage(path).context("保存表/批注前落盘失败")?;
    }
    // 批注：原生写入 commentsN.xml + vmlDrawingN.vml，删空时清掉旧部件；
    // 已由 xlsx_delta 就地补丁的表不再整份覆写，避免毁掉线程批注与 VML 控件。
    for sheet in &wb.sheets {
        if !delta_summary.handled_note_sheets.contains(&sheet.identity) {
            if sheet.notes_dirty && sheet.notes.is_empty() {
                must_save!("notes-clear", crate::notes::clear_sheet_notes(path, &sheet.name));
            } else if (sheet.notes_dirty || sheet.source_part.is_none()) && !sheet.notes.is_empty() {
                must_save!(
                    "notes-write",
                    crate::notes::write_sheet_notes(path, &sheet.name, &sheet.notes)
                );
            }
        }
        // Excel 365 原生线程批注（write_sheet_notes 若先执行，后写 threadedComments 保证不互相覆盖）
        if sheet.threads_dirty || (sheet.source_part.is_none() && !sheet.threads.is_empty()) {
            must_save!(
                "threaded-comments-write",
                crate::notes::write_sheet_threaded_comments(path, &sheet.name, &sheet.threads)
            );
        }
    }
    // 表格对象：删除/转换/新建后同步 table 部件
    for sheet in &wb.sheets {
        if sheet.tables_dirty {
            must_save!("table-sync", crate::table::sync(path, &sheet.name, &sheet.tables));
        }
    }
    crate::patch::begin_stage_for(path);
    must_save!("prune-content-types-final", crate::fixups::prune_content_types(path));
    if target_is_xlsx && crate::vba::has_macros(path) {
        anyhow::bail!("这份工作簿带 VBA 宏，只能另存为 .xlsm（写成 .xlsx 会丢宏）");
    }
    if wants_macro || target_is_xlsx {
        must_save!(
            "content-type-main",
            crate::fixups::normalise_workbook_ct(path, wants_macro)
        );
    }
    // 最后一步：外链关系按原文恢复，避免 URL 被归一、TargetMode 被丢
    must_save!(
        "restore-external-rels",
        crate::notes::restore_external_rels(path, &external_rels)
    );
    crate::patch::commit_stage(path).context("保存收尾补丁落盘失败")?;
    if fresh_base && streamed.iter().any(|&s| s) {
        let fresh_res = fresh::write(path, wb, &streamed, true)?;
        if let Some(st) = fresh_res.styles {
            saved_styles = Some(st);
        }
        trusted_parts.extend(fresh_res.trusted_parts);
    }
    let mut created_charts = std::collections::HashMap::new();
    for sheet in wb.sheets.iter() {
        let new_charts: Vec<_> = sheet
            .charts
            .iter()
            .enumerate()
            .filter(|(_, ch)| ch.part.is_empty())
            .collect();
        if new_charts.is_empty() {
            continue;
        }
        let existing_parts: std::collections::HashSet<String> = sheet
            .charts
            .iter()
            .filter(|c| !c.part.is_empty())
            .map(|c| c.part.clone())
            .collect();
        for (_, chart) in &new_charts {
            let mut committed = (*chart).clone();
            if committed.series.iter().any(|series| !series.val_ref.is_empty()) {
                let _ = crate::chart_write::refresh_series_from_refs(wb, &sheet.name, &mut committed);
            }
            crate::chart_write::insert_chart(path, &sheet.name, &committed)?;
        }
        let generated: Vec<_> = crate::charts::load_charts(path, &sheet.name)?
            .into_iter()
            .filter(|c| !existing_parts.contains(&c.part))
            .collect();
        anyhow::ensure!(
            generated.len() >= new_charts.len(),
            "图表写入后的关系数量不一致"
        );
        let tail_start = generated.len() - new_charts.len();
        for ((i, _), chart) in new_charts
            .into_iter()
            .zip(generated.into_iter().skip(tail_start))
        {
            created_charts.insert((sheet.identity, i), (chart.part, chart.drawing));
        }
    }
    crate::audit::sync_package_audit(path, wb)?;
    Ok(CellWrite {
        styles: saved_styles,
        used_delta: patched,
        created_charts,
        trusted_parts,
    })
}

/// 把条件格式写进 xlsx（重写 sheet XML 与 styles.xml 的 dxfs）
fn apply_cf_patch(
    path: &Path,
    wb: &Workbook,
    do_cf: bool,
    do_dv: bool,
    do_freeze: bool,
    do_filter: bool,
    do_page: bool,
) -> Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    // 数组公式：给锚点补 t="array" ref
    let arrays_by_sheet: std::collections::HashMap<String, Vec<crate::arr::ArrayFml>> = wb
        .sheets
        .iter()
        .filter(|s| !s.arrays.is_empty())
        .map(|s| {
            (
                s.name.clone(),
                s.arrays
                    .iter()
                    .filter(|(a,_)| s.get(a.row,a.col).is_some_and(|c| c.formula.is_some()))
                    .map(|(a, r)| crate::arr::ArrayFml {
                        range: *r,
                        anchor: (a.row, a.col),
                    })
                    .collect(),
            )
        })
        .collect();
    // 先把每张表要写的块算出来
    let mut per_sheet: std::collections::HashMap<
        String,
        (
            String,
            String,
            Option<(u32, u32)>,
            bool,
            String,
            std::collections::HashSet<u32>,
            bool,
            crate::page::PageSetup,
            bool,
            Vec<crate::arr::ArrayFml>,
        ),
    > = std::collections::HashMap::new();
    // dxfs 全局唯一表：合并所有工作表的 dxfs 并按表建立 dxfId 重映射，防止跨表各自添加 dxf 后越界或串色
    let mut dxfs: Vec<crate::cf::Dxf> = Vec::new();
    let mut sheet_dxf_maps: Vec<Vec<usize>> = Vec::with_capacity(wb.sheets.len());
    for sheet in wb.sheets.iter() {
        let mut map = Vec::with_capacity(sheet.cf.dxfs.len());
        for d in &sheet.cf.dxfs {
            if let Some(pos) = dxfs.iter().position(|x| x == d) {
                map.push(pos);
            } else {
                let pos = dxfs.len();
                dxfs.push(d.clone());
                map.push(pos);
            }
        }
        sheet_dxf_maps.push(map);
    }
    for (si, sheet) in wb.sheets.iter().enumerate() {
        if let Some(t) = targets.get(&sheet.name) {
            let cf = if do_cf {
                let map = &sheet_dxf_maps[si];
                let identity = map.iter().enumerate().all(|(i, &m)| i == m);
                if identity {
                    crate::patch::cf_blocks_xml(&sheet.cf.blocks)
                } else {
                    let mut remapped = sheet.cf.blocks.clone();
                    for blk in &mut remapped {
                        for rule in &mut blk.rules {
                            rule.kind.remap_dxf(map);
                        }
                    }
                    crate::patch::cf_blocks_xml(&remapped)
                }
            } else {
                String::new()
            };
            let dv = if do_dv { crate::patch::dv_xml(&sheet.dv) } else { String::new() };
            let has_color_filter = sheet
                .filter
                .as_ref()
                .is_some_and(|f| !f.color_filters.is_empty());
            let af = if do_filter {
                // 表格对象的 autoFilter 写在 tableN.xml 里；工作表上再写一份会重复，
                // 打开时 Excel 会看到两个筛选源。
                match sheet.filter.as_ref() {
                    Some(f)
                        if !sheet
                            .tables
                            .iter()
                            .any(|t| {
                                t.header_row
                                    && t.range.0 <= f.range.0
                                    && t.range.1 <= f.range.1
                                    && t.range.2 >= f.range.2
                                    && t.range.3 >= f.range.3
                            }) =>
                    {
                        crate::patch::auto_filter_xml_with_dxfs(f, &mut dxfs)
                    }
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            if !cf.is_empty()
                || !dv.is_empty()
                || sheet.cf_dirty
                || sheet.dv_dirty
                || sheet.freeze_dirty
                || sheet.filter_dirty
                || (do_filter && has_color_filter)
                || sheet.page_dirty
                || arrays_by_sheet.contains_key(&sheet.name)
            {
                per_sheet.insert(
                    t.clone(),
                    (
                        cf,
                        dv,
                        sheet.freeze,
                        sheet.freeze_dirty,
                        af,
                        {
                            // 屏蔽行两种来源必须合并：筛选条件命中的 hidden_rows
                            // 和用户手动隐藏的 hidden_rows_user。关闭筛选时只清 hidden_rows，
                            // 不能把手动隐藏的行一起抹掉。
                            let mut hidden: std::collections::HashSet<u32> = sheet.hidden_rows.iter().copied().collect();
                            hidden.extend(sheet.hidden_rows_user.iter().copied());
                            hidden
                        },
                        sheet.filter_dirty || (do_filter && has_color_filter),
                        sheet.page.clone(),
                        sheet.page_dirty,
                        arrays_by_sheet.get(&sheet.name).cloned().unwrap_or_default(),
                    ),
                );
            }
        }
    }
    let has_dxfs = !dxfs.is_empty();
    crate::patch::rewrite_zip_borrowed(
        path,
        |name| {
            per_sheet.contains_key(name)
                || name == "xl/workbook.xml"
                || (has_dxfs && name == "xl/styles.xml")
        },
        |name, buf| {
            if let Some((blocks, dv, freeze, freeze_dirty, af, hidden, f_dirty, page, page_dirty, arrs)) =
                per_sheet.get(name)
            {
                let touches_rows = (do_filter && *f_dirty) || !arrs.is_empty();
                return crate::patch::transform_worksheet(name,buf,!touches_rows,|xml| {
                    let mut x = xml.to_owned();
                    if do_cf {
                        x = crate::patch::insert_cf(&x, blocks);
                    }
                    if do_dv {
                        x = crate::patch::insert_dv(&x, dv);
                    }
                    if do_freeze && *freeze_dirty {
                        x = crate::patch::set_freeze_xml(&x, *freeze);
                    }
                    if do_filter && *f_dirty {
                        x = crate::patch::insert_auto_filter(&x, af);
                        x = crate::patch::set_hidden_rows(&x, hidden);
                    }
                    if do_page && *page_dirty {
                        x = crate::patch::set_page_xml(&x, page);
                    }
                    if !arrs.is_empty() {
                        x = crate::arr::apply_xml(&x, arrs);
                    }
                    Ok(x)
                });
            }
            if name == "xl/workbook.xml" {
                {
                    let xml = patch_xml_text(buf,name)?;
                    let mut x = if crate::patch::calc_manual_xml(&xml) == wb.calc_manual { xml.to_string() }
                        else { crate::patch::set_calc_mode(&xml, wb.calc_manual) };
                    x = crate::patch::set_calc_iteration(
                        &x,
                        wb.calc_iterate,
                        wb.calc_max_iterations,
                        wb.calc_max_change,
                    );
                    if do_page {
                        for (idx, sheet) in wb.sheets.iter().enumerate() {
                            if sheet.page_dirty {
                                x = crate::patch::set_print_names(&x, idx as u32, &sheet.name, &sheet.page);
                            }
                        }
                    }
                    return Ok((x != xml).then(|| x.into_bytes()));
                }
            }
            if name == "xl/styles.xml" {
                // 没有 dxfs 就别碰 styles.xml：改写反而会写坏（Excel 直接判损坏）
                if dxfs.is_empty() {
                    return Ok(None);
                }
                let xml = patch_xml_text(buf,name)?;
                let edited = crate::patch::replace_dxfs(&xml,&dxfs);
                return Ok((edited != xml).then(|| edited.into_bytes()));
            }
            Ok(None)
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, CellRef, Sheet, Value, Workbook};

    #[test]
    fn verify_worksheet_structure_with_a_valid_zip_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad-sheet.xlsx");
        save(&path,&Workbook::new_blank()).unwrap();
        let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        crate::fixups::write_parts(&path,&[("xl/worksheets/vendor/settings.xml".into(),b"<settings/>".to_vec())]).unwrap();
        verify_saved_package(&path).unwrap();
        for xml in ["<worksheet><row></worksheet>","<worksheet/><another/>","<other/>",
            "<worksheet><row r=1/></worksheet>","<worksheet duplicate='1' duplicate='2'/>"] {
            crate::fixups::write_parts(&path,&[(part.clone(),xml.as_bytes().to_vec())]).unwrap();
            assert!(format!("{:#}",verify_saved_package(&path).unwrap_err()).contains(&part));
        }
    }

    #[test]
    fn custom_border_patterns_and_diagonal_directions_survive_save_and_reopen() {
        use crate::style::{Border, CellStyle};
        let mut book = Workbook::new_blank();
        for kind in 1u8..=13 {
            let color = [40, 60, 90];
            let side = Border { style: kind, color };
            for row in 0..2 {
                let sid = book.styles.intern(CellStyle {
                    border: [side; 4],
                    diagonal: (row == 1).then(|| crate::style_ext::DiagBorder {
                        up: true, down: true, color, color_theme: None,
                        style: Border::ooxml_name(kind).to_owned(),
                    }),
                    ..CellStyle::default_dark()
                });
                book.sheets[0].set_style(row, u32::from(kind), sid);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("borders.xlsx");
        save_atomic(&path, &book).unwrap();
        let saved = load(&path).unwrap();
        for kind in 1u8..=13 {
            for row in 0..2 {
                let col = u32::from(kind);
                let actual = saved.styles.get(saved.sheets[0].style_of(row, col));
                let expected = book.styles.get(book.sheets[0].style_of(row, col));
                assert_eq!(actual.border, expected.border, "row {row}, style {kind}");
                assert_eq!(actual.diagonal, expected.diagonal, "row {row}, style {kind}");
            }
        }
    }

    #[test]
    fn owned_save_restores_edit_flags_and_keeps_the_workers_sheet_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.xlsx");
        let mut book = Workbook::new_blank();
        let sheet = &mut book.sheets[0];
        sheet.rows.set(1, 32.0);
        sheet.set(1, 0, Cell { value: Value::Number(7.0), formula: None });
        sheet.layout_dirty = false;
        sheet.notes_dirty = true;
        let before = OriginalSheetFlags::capture(sheet);
        let storage = book.sheets.as_ptr();
        let (_, worker) = save_snapshot_with_receipt(&path, book).unwrap();
        assert_eq!(storage, worker.sheets.as_ptr());
        assert_eq!(OriginalSheetFlags::capture(&worker.sheets[0]), before);
        let saved = load(&path).unwrap();
        assert_eq!(saved.sheets[0].rows.size(1), 32.0);
        assert_eq!(saved.sheets[0].value(1, 0), Value::Number(7.0));
    }

    #[test]
    fn fresh_stream_failure_rebuilds_from_model_in_a_separate_package() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retry.xlsx");
        let mut wb = Workbook::new_blank();
        let sheet = &mut wb.sheets[0];
        for r in 0..16_384 { sheet.set(r,0,Cell { value: Value::Number(f64::from(r)),formula:None }); }
        sheet.set(0,1,Cell {value:Value::Number(3.),formula:Some("=1+2".into())});
        let anchor = (CellRef::new(0,1),(0,1,0,1));
        sheet.arrays.extend([anchor,anchor,(CellRef::new(1,0),(1,0,1,0))]);
        let before = OriginalSheetFlags::capture(sheet);
        let (_, returned) = save_snapshot_with_receipt(&path, wb).unwrap();
        assert_eq!(OriginalSheetFlags::capture(&returned.sheets[0]), before);
        let saved = load(&path).unwrap();
        assert_eq!(saved.sheets[0].arrays,vec![anchor]);
        assert_eq!(saved.sheets[0].value(1,0),Value::Number(1.));
        assert_eq!(saved.sheets[0].value(16_383,0),Value::Number(16_383.));
    }

    #[test]
    fn save_receipt_does_not_read_sheet_data_but_final_verification_detects_bad_crc() {
        use std::io::{Read, Seek, SeekFrom, Write};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.xlsx");
        let wb = Workbook::new_blank();
        save(&path, &wb).unwrap();
        let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        let offset = {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
            let entry = zip.by_name(&part).unwrap();
            entry.data_start().unwrap() + entry.compressed_size() / 2
        };
        let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[byte[0] ^ 0xff]).unwrap();
        drop(file);
        assert!(package_receipt(&path, &wb, CellWrite::default()).unwrap().sheets[&wb.sheets[0].identity].source_cells.is_some());
        let error = verify_saved_package(&path).unwrap_err();
        assert!(format!("{error:#}").contains(&part));
    }

    #[test]
    fn external_note_change_aborts_atomic_save_without_overwriting_either_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.xlsx");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02/notes.xlsx"),
            &path,
        ).unwrap();
        let mut wb = load(&path).unwrap();
        let notes = &mut wb.sheets[0].notes;
        notes.get_mut(&CellRef::new(1, 1)).unwrap().text = "本地修改 B2".into();
        wb.sheets[0].notes_dirty = true;

        let part = "xl/comments/comment1.xml";
        let original = String::from_utf8(read_part(&path, part).unwrap()).unwrap();
        let external = original.replace("客户确认过", "另一位用户改过 A3");
        assert_ne!(original, external);
        crate::patch::append_entry(&path, part, external.as_bytes()).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(save_atomic(&path, &wb).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(wb.sheets[0].notes[&CellRef::new(1, 1)].text, "本地修改 B2");

        // A value-only autosave can copy the external A3 note into its output.
        // Its receipt must not then allow our stale in-memory A3 to overwrite it.
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02/notes.xlsx"),
            &path,
        ).unwrap();
        let mut wb = load(&path).unwrap();
        crate::patch::append_entry(&path, part, external.as_bytes()).unwrap();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(99.0), formula: None });
        let receipt = save_atomic_with_receipt(&path, &wb).unwrap();
        receipt.adopt_document(&mut wb, &path, true);
        let saved = std::fs::read(&path).unwrap();
        assert_eq!(read_part(&path, part).unwrap(), external.as_bytes());
        wb.sheets[0].notes.get_mut(&CellRef::new(1, 1)).unwrap().text = "本地再次修改".into();
        wb.sheets[0].notes_dirty = true;
        assert!(save_atomic(&path, &wb).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), saved);
    }

    #[test]
    fn ordinary_save_preserves_automatic_except_tables_and_recalculation_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.xlsx");
        save(&path, &Workbook::new_blank()).unwrap();
        let xml = String::from_utf8(read_part(&path, "xl/workbook.xml").unwrap()).unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let tag = "<calcPr calcMode=\"autoNoTable\" calcId=\"0\" fullCalcOnLoad=\"1\"/>";
        let range = if let Some(node) = doc.descendants().find(|n| n.tag_name().name() == "calcPr") {
            node.range()
        } else {
            let end = xml.rfind("</workbook>").unwrap();
            end..end
        };
        let mut source = xml.clone();
        source.replace_range(range, tag);
        crate::patch::append_entry(&path, "xl/workbook.xml", source.as_bytes()).unwrap();
        let mut wb = load(&path).unwrap();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(2.0), formula: None });
        let target = dir.path().join("saved.xlsx");
        save_atomic(&target, &wb).unwrap();
        let xml = String::from_utf8(read_part(&target, "xl/workbook.xml").unwrap()).unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let calc = doc.descendants().find(|n| n.tag_name().name() == "calcPr").unwrap();
        assert_eq!(calc.attribute("calcMode"), Some("autoNoTable"));
        assert_eq!(calc.attribute("fullCalcOnLoad"), Some("1"));
    }

    #[test]
    fn manual_page_breaks_round_trip() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("breaks_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        for r in 0..30u32 {
            wb.sheets[0].set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        }
        wb.sheets[0].page.row_breaks = vec![4, 12];
        wb.sheets[0].page.col_breaks = vec![2];
        wb.sheets[0].page_dirty = true;
        save(&path, &wb).expect("save");
        let xml = crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap();
        assert!(xml.contains("<rowBreaks"), "应写出 rowBreaks");
        assert!(xml.contains("<colBreaks"), "应写出 colBreaks");
        let back = load(&path).expect("load");
        assert_eq!(back.sheets[0].page.row_breaks, vec![4, 12]);
        assert_eq!(back.sheets[0].page.col_breaks, vec![2]);
    }

    #[test]
    fn table_filter_survives_save_without_duplicate_autofilter() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("tbl_filter_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        {
            let sh = &mut wb.sheets[0];
            for c in 0..2u32 {
                sh.set(0, c, Cell { value: Value::Text(format!("h{c}").into()), formula: None });
                sh.set(1, c, Cell { value: Value::Text("a".into()), formula: None });
                sh.set(2, c, Cell { value: Value::Text("b".into()), formula: None });
            }
            sh.tables.push(crate::table::Table {
                column_ids: Vec::new(),
                name: "Table1".into(),
                display: "Table1".into(),
                range: (0, 0, 2, 1),
                columns: vec!["h0".into(), "h1".into()],
                header_row: true,
                totals_row: false,
                ..Default::default()
            });
            sh.tables_dirty = true;
            sh.filter = Some(crate::sort_filter::Filter::new(0, 0, 2, 1));
            sh.filter_dirty = true;
        }
        save(&path, &wb).expect("save");
        let xml = crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap();
        assert!(
            !xml.contains("<autoFilter"),
            "表格范围不应再写工作表级 autoFilter"
        );
        let back = load(&path).expect("load");
        assert_eq!(back.sheets[0].tables.len(), 1);
        assert_eq!(
            back.sheets[0].filter.as_ref().map(|f| f.range),
            Some((0, 0, 2, 1)),
            "表格自带筛选，载入后应有漏斗"
        );
    }

    /// UI-18：关闭筛选后保留手动隐藏行，并清掉 _FilterDatabase。
    #[test]
    fn disable_filter_keeps_manual_hidden_rows_and_removes_filter_name() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("disable_filter_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        let sheet_name = wb.sheets[0].name.clone();
        {
            let sh = &mut wb.sheets[0];
            sh.set(0, 0, Cell { value: Value::Text("Name".into()), formula: None });
            sh.set(0, 1, Cell { value: Value::Text("Amount".into()), formula: None });
            sh.set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
            sh.set(1, 1, Cell { value: Value::Number(10.0), formula: None });
            sh.set(2, 0, Cell { value: Value::Text("B".into()), formula: None });
            sh.set(2, 1, Cell { value: Value::Number(20.0), formula: None });
            sh.set(3, 0, Cell { value: Value::Text("C".into()), formula: None });
            sh.set(3, 1, Cell { value: Value::Number(30.0), formula: None });
            let mut f = crate::sort_filter::Filter::new(0, 0, 3, 1);
            f.criteria.insert(
                1,
                crate::sort_filter::Criterion::new(crate::sort_filter::CmpOp::Gt, "15"),
            );
            sh.hidden_rows = f.hidden_rows(sh).into();
            sh.hidden_rows_user.insert(2);
            sh.filter = Some(f);
            sh.filter_dirty = true;
        }
        save(&path, &wb).expect("base save");
        // 模拟 Excel 留下的内部名称；umya 不会自己创造它。
        crate::patch::rewrite_zip(&path, |n, buf| {
            if n != "xl/workbook.xml" {
                return buf;
            }
            let Ok(xml) = String::from_utf8(buf.clone()) else { return buf };
            let name = format!(
                "<definedName name=\"_xlnm._FilterDatabase\" localSheetId=\"0\" hidden=\"1\">'{sheet_name}'!$A$1:$B$4</definedName>"
            );
            if let Some(i) = xml.find("</definedNames>") {
                let mut out = xml;
                out.insert_str(i, &name);
                out.into_bytes()
            } else if let Some(i) = xml.find("</sheets>") {
                let mut out = xml;
                out.insert_str(i + "</sheets>".len(), &format!("<definedNames>{name}</definedNames>"));
                out.into_bytes()
            } else {
                xml.into_bytes()
            }
        })
        .expect("inject filter database");
        let mut back = load(&path).expect("load base");
        assert!(back.sheets[0].filter.is_some());
        assert!(back.sheets[0].hidden_rows_user.contains(&2));
        {
            let sh = &mut back.sheets[0];
            sh.filter = None;
            sh.hidden_rows.clear();
            sh.filter_dirty = true;
        }
        save(&path, &back).expect("disable save");
        let reloaded = load(&path).expect("reload");
        assert!(reloaded.sheets[0].filter.is_none(), "filter must be removed");
        assert!(reloaded.sheets[0].hidden_rows_user.contains(&2), "manual hidden row must survive");
        assert!(!reloaded.sheets[0].hidden_rows_user.contains(&1), "filter-hidden row2 must not become manual");
        assert!(!reloaded.sheets[0].hidden_rows.contains(&1));
        assert!(!reloaded.sheets[0].hidden_rows.contains(&2));
        let sheet_xml = crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap();
        let row_tag = |n: u32| {
            sheet_xml
                .split(&format!("<row r=\"{n}\""))
                .nth(1)
                .and_then(|s| s.split('>').next())
                .unwrap_or("")
        };
        assert!(!row_tag(2).contains("hidden=\"1\""), "row2 must be visible after disabling filter");
        assert!(row_tag(3).contains("hidden=\"1\""), "row3 manual hidden must remain");
        let wb_xml = crate::fixups::read_part_file(&path, "xl/workbook.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap();
        assert!(!wb_xml.contains("_xlnm._FilterDatabase"), "filter database name must be removed");
    }

    #[test]
    fn clear_filter_restores_filter_hidden_and_keeps_manual_hidden() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("clear_filter_source_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        {
            let sh = &mut wb.sheets[0];
            sh.set(0, 0, Cell { value: Value::Text("Name".into()), formula: None });
            sh.set(0, 1, Cell { value: Value::Text("Amount".into()), formula: None });
            sh.set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
            sh.set(1, 1, Cell { value: Value::Number(10.0), formula: None });
            sh.set(2, 0, Cell { value: Value::Text("B".into()), formula: None });
            sh.set(2, 1, Cell { value: Value::Number(20.0), formula: None });
            sh.set(3, 0, Cell { value: Value::Text("C".into()), formula: None });
            sh.set(3, 1, Cell { value: Value::Number(30.0), formula: None });
            let mut f = crate::sort_filter::Filter::new(0, 0, 3, 1);
            f.criteria.insert(
                1,
                crate::sort_filter::Criterion::new(crate::sort_filter::CmpOp::Gt, "15"),
            );
            sh.hidden_rows = f.hidden_rows(sh).into();
            sh.hidden_rows_user.insert(2);
            sh.filter = Some(f);
            sh.filter_dirty = true;
        }
        save(&path, &wb).expect("base save");
        let mut back = load(&path).expect("load base");
        assert!(!back.sheets[0].hidden_rows_user.contains(&1));
        assert!(back.sheets[0].hidden_rows.contains(&1));
        {
            let sh = &mut back.sheets[0];
            let f = sh.filter.as_mut().expect("filter");
            f.allows.clear();
            f.criteria.clear();
            sh.hidden_rows.clear();
            sh.filter_dirty = true;
        }
        save(&path, &back).expect("clear save");
        let reloaded = load(&path).expect("reload");
        let sh = &reloaded.sheets[0];
        assert!(sh.filter.is_some(), "clear keeps filter arrows");
        assert!(!sh.hidden_rows_user.contains(&1));
        assert!(sh.hidden_rows_user.contains(&2), "manual row survives clear");
        assert!(sh.hidden_rows.is_empty());
        let xml = crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap();
        assert!(xml.contains("<autoFilter"), "autoFilter must stay after clear");
        let row_tag = |n: u32| {
            xml.split(&format!("<row r=\"{n}\""))
                .nth(1)
                .and_then(|s| s.split('>').next())
                .unwrap_or("")
        };
        assert!(!row_tag(2).contains("hidden=\"1\""), "filter-hidden row restores");
        assert!(row_tag(3).contains("hidden=\"1\""), "manual hidden row stays");
    }

    #[test]
    fn remove_filter_database_matches_escaped_sheet_name_and_keeps_other() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("filter_db_escape_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "A&B".into();
        wb.sheets.push(Sheet::new("Other"));
        save(&path, &wb).expect("base save");
        crate::patch::rewrite_zip(&path, |n, buf| {
            if n != "xl/workbook.xml" {
                return buf;
            }
            let Ok(xml) = String::from_utf8(buf.clone()) else { return buf };
            let names = concat!(
                "<definedNames>",
                "<definedName name=\"_xlnm._FilterDatabase\" localSheetId=\"0\" hidden=\"1\">'A&amp;B'!$A$1:$B$4</definedName>",
                "<definedName name=\"_xlnm._FilterDatabase\" localSheetId=\"1\" hidden=\"1\">'Other'!$A$1:$B$4</definedName>",
                "</definedNames>"
            );
            if let Some(i) = xml.find("</sheets>") {
                let mut out = xml;
                out.insert_str(i + "</sheets>".len(), names);
                out.into_bytes()
            } else {
                xml.into_bytes()
            }
        })
        .expect("inject names");
        crate::patch::remove_filter_database(&path, "A&B").expect("remove filter database");
        let xml = crate::fixups::read_part_file(&path, "xl/workbook.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap();
        assert_eq!(xml.matches("_xlnm._FilterDatabase").count(), 1, "only other sheet keeps its filter name");
        assert!(xml.contains("'Other'!$A$1:$B$4"), "other sheet must remain");
        assert!(!xml.contains("'A&amp;B'!$A$1:$B$4"), "escaped target reference must be removed");
    }

    #[test]
    fn manual_filter_overlap_survives_save_load_clear_and_disable() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("manual_filter_overlap_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        {
            let sh = &mut wb.sheets[0];
            sh.set(0, 0, Cell { value: Value::Text("Name".into()), formula: None });
            sh.set(0, 1, Cell { value: Value::Text("Amount".into()), formula: None });
            sh.set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
            sh.set(1, 1, Cell { value: Value::Number(100.0), formula: None });
            sh.set(2, 0, Cell { value: Value::Text("B".into()), formula: None });
            sh.set(2, 1, Cell { value: Value::Number(200.0), formula: None });
            sh.set(3, 0, Cell { value: Value::Text("C".into()), formula: None });
            sh.set(3, 1, Cell { value: Value::Number(300.0), formula: None });
            sh.set(4, 0, Cell { value: Value::Text("D".into()), formula: None });
            sh.set(4, 1, Cell { value: Value::Number(50.0), formula: None });
            let mut f = crate::sort_filter::Filter::new(0, 0, 4, 1);
            f.criteria.insert(
                1,
                crate::sort_filter::Criterion::new(crate::sort_filter::CmpOp::Gt, "150"),
            );
            sh.hidden_rows = f.hidden_rows(sh).into();
            // row1 既手动隐藏又被筛选隐藏（overlap），row2 只手动隐藏，row4 只筛选隐藏。
            sh.hidden_rows_user.insert(1);
            sh.hidden_rows_user.insert(2);
            sh.filter = Some(f);
            sh.filter_dirty = true;
        }
        save(&path, &wb).expect("base save");
        assert!(
            crate::patch::read_manual_hidden(&path).is_some(),
            "app save must persist manual hidden source"
        );
        // 反向：把本应用的来源去掉，只剩 OOXML row hidden 时，overlap
        // 不能再被识别为 manual，证明上面测试依赖来源元数据而非偶然。
        let legacy = dir.join(format!("manual_filter_overlap_legacy_{}.xlsx", std::process::id()));
        std::fs::copy(&path, &legacy).expect("copy legacy");
        crate::patch::write_manual_hidden(&legacy, &std::collections::BTreeMap::new())
            .expect("strip metadata");
        let legacy_wb = load(&legacy).expect("load legacy");
        assert!(!legacy_wb.sheets[0].hidden_rows_user.contains(&1));
        assert!(legacy_wb.sheets[0].hidden_rows.contains(&1));
        let mut back = load(&path).expect("load base");
        assert!(back.sheets[0].hidden_rows_user.contains(&1), "overlap manual source survives load");
        assert!(back.sheets[0].hidden_rows_user.contains(&2), "manual-only source survives load");
        assert!(back.sheets[0].hidden_rows.contains(&1));
        assert!(back.sheets[0].hidden_rows.contains(&4));
        assert!(!back.sheets[0].hidden_rows_user.contains(&4), "filter-only row is not manual");
        {
            let sh = &mut back.sheets[0];
            let f = sh.filter.as_mut().expect("filter");
            f.allows.clear();
            f.criteria.clear();
            sh.hidden_rows.clear();
            sh.filter_dirty = true;
        }
        save(&path, &back).expect("clear save");
        let cleared = load(&path).expect("reload cleared");
        let sh = &cleared.sheets[0];
        assert!(sh.filter.is_some(), "clear keeps filter");
        assert!(sh.hidden_rows_user.contains(&1), "overlap manual row stays after clear");
        assert!(sh.hidden_rows_user.contains(&2), "manual row stays after clear");
        assert!(!sh.hidden_rows_user.contains(&4), "filter-only row stays non-manual");
        assert!(sh.hidden_rows.is_empty());
        let xml = crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap();
        let row_tag = |n: u32| {
            xml.split(&format!("<row r=\"{n}\""))
                .nth(1)
                .and_then(|s| s.split('>').next())
                .unwrap_or("")
        };
        assert!(row_tag(2).contains("hidden=\"1\""), "overlap row stays hidden");
        assert!(row_tag(3).contains("hidden=\"1\""), "manual row stays hidden");
        assert!(!row_tag(5).contains("hidden=\"1\""), "filter-only row restores after clear");
        {
            let mut disabled = cleared.clone();
            let sh = &mut disabled.sheets[0];
            sh.filter = None;
            sh.hidden_rows.clear();
            sh.filter_dirty = true;
            save(&path, &disabled).expect("disable save");
            let reloaded = load(&path).expect("reload disabled");
            assert!(reloaded.sheets[0].filter.is_none());
            assert!(reloaded.sheets[0].hidden_rows_user.contains(&1));
            assert!(reloaded.sheets[0].hidden_rows_user.contains(&2));
            assert!(!reloaded.sheets[0].hidden_rows_user.contains(&4));
        }
    }

    #[test]
    fn sheet_protection_survives_save_and_verifies() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("prot_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
        save(&path, &wb).expect("first save");

        let mut wb = load(&path).expect("load");
        wb.sheets[0].protection =
            Some(crate::protection::SheetProtection::new_with_password("pw"));
        wb.sheets[0].protect_dirty = true;
        save(&path, &wb).expect("protect save");

        let back = load(&path).expect("reload");
        let p = back.sheets[0].protection.clone().expect("应带保护");
        assert!(p.verify("pw"));
        assert!(!p.verify("bad"));

        let mut wb = back;
        wb.sheets[0].protection = None;
        wb.sheets[0].protect_dirty = true;
        save(&path, &wb).expect("unprotect save");
        assert!(load(&path).expect("reload2").sheets[0].protection.is_none());
    }

    #[test]
    fn tab_color_survives_save() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("tabcolor_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
        save(&path, &wb).expect("first save");
        let mut wb = load(&path).expect("load");
        {
            let s = &mut wb.sheets[0];
            s.tab_color = Some([0x70, 0xAD, 0x47]);
            s.tab_color_dirty = true;
        }
        save(&path, &wb).expect("second save");
        let back = load(&path).expect("reload");
        assert_eq!(back.sheets[0].tab_color, Some([0x70, 0xAD, 0x47]));
        // 再改回无颜色
        let mut wb = back;
        {
            let s = &mut wb.sheets[0];
            s.tab_color = None;
            s.tab_color_dirty = true;
        }
        save(&path, &wb).expect("third save");
        let back = load(&path).expect("reload2");
        assert_eq!(back.sheets[0].tab_color, None);
    }

    #[test]
    fn column_widths_heights_and_hidden_state_survive_save() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("layout_rt_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("a".into()), formula: None });
        wb.sheets[0].set(4, 0, Cell { value: Value::Text("e5".into()), formula: None });
        save(&path, &wb).expect("first save");

        let mut wb = load(&path).expect("load");
        {
            let s = &mut wb.sheets[0];
            s.cols.set(0, 170.0);
            s.cols.set(2, 40.0);
            s.rows.set(4, 44.0);
            s.hidden_cols.insert(1);
            s.hidden_rows_user.insert(2);
            s.layout_dirty = true;
        }
        save(&path, &wb).expect("second save");

        let back = load(&path).expect("reload");
        let s = &back.sheets[0];
        assert!((s.cols.size(0) - 170.0).abs() < 1.0, "A 列宽 {}", s.cols.size(0));
        assert!((s.cols.size(2) - 40.0).abs() < 1.0, "C 列宽 {}", s.cols.size(2));
        assert!((s.rows.size(4) - 44.0).abs() < 1.0, "第 5 行高 {}", s.rows.size(4));
        assert!(s.hidden_cols.contains(&1), "B 列应隐藏: {:?}", s.hidden_cols);
        assert!(s.hidden_rows_user.contains(&2), "第 3 行应隐藏: {:?}", s.hidden_rows_user);
        assert_eq!(s.value(4, 0), Value::Text("e5".into()));
    }
    use std::collections::HashMap;

    #[test]
    fn clean_hyperlinks_keep_original_xml_and_rebuilt_sheets_keep_targets() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.xlsx");
        let copy = dir.path().join("copy.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("Link".into()), formula: None });
        wb.sheets[0].hyperlinks.insert((0, 0), crate::hyperlinks::Hyperlink {
            target: "https://example.org/?a=1&b=2".into(), location: "target".into(),
            tooltip: "A & B".into(), external: true,
            span: (3, 0),
            ..Default::default()
        });
        wb.sheets[0].hyperlinks_dirty = true;
        save(&base, &wb).unwrap();
        let part = crate::patch::sheet_targets(&base).unwrap()[&wb.sheets[0].name].clone();
        let original = String::from_utf8(read_part(&base, &part).unwrap()).unwrap()
            .replace("<hyperlink ref=", "<hyperlink display=\"extra metadata\" ref=");
        crate::patch::append_entry(&base, &part, original.as_bytes()).unwrap();
        let mut loaded = load(&base).unwrap();
        assert!(!loaded.sheets[0].hyperlinks_dirty);
        wb.sheets[0].hyperlinks.get_mut(&(0, 0)).unwrap().attributes
            .push(("display".into(), "extra metadata".into()));
        assert_eq!(loaded.sheets[0].hyperlinks, wb.sheets[0].hyperlinks);
        save_atomic(&copy, &loaded).unwrap();
        assert_eq!(read_part(&copy, &part).unwrap(), original.as_bytes());

        loaded.sheets[0].merges.push((1, 0, 1, 1));
        loaded.sheets[0].merges_dirty = true;
        save_atomic(&copy, &loaded).unwrap();
        assert_eq!(load(&copy).unwrap().sheets[0].hyperlinks, wb.sheets[0].hyperlinks);

        loaded.sheets[0].insert_rows(1, 2);
        loaded.sheets[0].delete_rows(0, 1);
        save_atomic(&copy, &loaded).unwrap();
        let shifted = load(&copy).unwrap();
        assert_eq!(shifted.sheets[0].hyperlinks[&(0, 0)].span, (4, 0));
        assert_eq!(shifted.sheets[0].hyperlinks[&(0, 0)].attributes,
            wb.sheets[0].hyperlinks[&(0, 0)].attributes);

        loaded.sheets[0].hyperlinks.clear();
        loaded.sheets[0].hyperlinks_dirty = true;
        save_atomic(&copy, &loaded).unwrap();
        assert!(load(&copy).unwrap().sheets[0].hyperlinks.is_empty());
    }

    #[test]
    fn receipt_distinguishes_two_cell_snapshots_with_equal_revision_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("branch.xlsx");
        let mut live = Workbook::new_blank();
        let mut worker = live.clone();
        live.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        worker.sheets[0].set(0, 0, Cell { value: Value::Number(2.0), formula: None });
        assert_eq!(live.sheets[0].cell_revision(), worker.sheets[0].cell_revision());
        assert_ne!(live.sheets[0].cell_version(), worker.sheets[0].cell_version());
        let receipt = save_atomic_with_receipt(&path, &worker).unwrap();
        receipt.adopt_document(&mut live, &path, false);
        assert!(!crate::fixups::can_restore_sheet_xml(&live.sheets[0]));
        save_atomic(&path, &live).unwrap();
        assert_eq!(load(&path).unwrap().sheets[0].value(0, 0), Value::Number(1.0));
    }

    #[test]
    fn second_save_reuses_committed_sheet_and_rewrites_a_changed_formula_cache() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.xlsx");
        let copy = dir.path().join("copy.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Summary"));
        let number = |value| Cell { value: Value::Number(value), formula: None };
        for row in 0..2_000 {
            wb.sheets[0].set(row, 0, number(row as f64));
        }
        wb.sheets[1].set(0, 0, number(5.0));
        wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=Summary!A1+1".into()) });
        crate::formula::recalc_all(&mut wb);
        let receipt = save_atomic_with_receipt(&original, &wb).unwrap();
        receipt.adopt_document(&mut wb, &original, true);
        assert!(crate::fixups::can_restore_sheet_xml(&wb.sheets[0]));
        let part = wb.sheets[0].source_part.clone().unwrap();
        let baseline = read_part(&original, &part).unwrap();

        // Editing only an unrelated summary cell must keep the saved detail
        // sheet eligible for XML reuse even if a full calculation runs.
        wb.sheets[1].set(1, 0, number(7.0));
        crate::formula::recalc_all(&mut wb);
        assert!(crate::fixups::can_restore_sheet_xml(&wb.sheets[0]));
        let receipt = save_atomic_with_receipt(&copy, &wb).unwrap();
        receipt.adopt_document(&mut wb, &copy, true);
        assert_eq!(read_part(&copy, &part).unwrap(), baseline);

        wb.sheets[1].set(0, 0, number(9.0));
        crate::formula::recalc_all(&mut wb);
        assert!(!wb.sheets[0].edited);
        assert!(!crate::fixups::can_restore_sheet_xml(&wb.sheets[0]));
        save_atomic(&copy, &wb).unwrap();
        assert_eq!(load(&copy).unwrap().sheets[0].value(0, 1), Value::Number(10.0));
        assert_eq!(load(&original).unwrap().sheets[0].value(0, 1), Value::Number(6.0));
    }

    #[test]
    fn untouched_large_sheet_recovers_its_original_xml_and_shared_strings() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.xlsx");
        let copy = dir.path().join("edited.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("first shared string".into()), formula: None });
        let mut second = Sheet::new("Large");
        second.set(0, 0, Cell { value: Value::Text("string only on the large sheet".into()), formula: None });
        wb.sheets.push(second);
        save(&base, &wb).unwrap();
        let part = crate::patch::sheet_targets(&base).unwrap()["Large"].clone();
        let original = String::from_utf8(read_part(&base, &part).unwrap()).unwrap()
            .replace("</sheetData>", &format!("<!--{}--></sheetData>",
                "x".repeat(crate::fixups::RAW_SHEET_MIN_BYTES as usize)));
        crate::patch::append_entry(&base, &part, original.as_bytes()).unwrap();
        let mut loaded = load(&base).unwrap();
        loaded.sheets[0].set(0, 0, Cell { value: Value::Text("new shared string".into()), formula: None });
        save_atomic(&copy, &loaded).unwrap();
        assert_eq!(crate::fixups::read_part_file(&copy, &part).unwrap(), original.as_bytes());
        let back = load(&copy).unwrap();
        assert_eq!(back.sheets[1].value(0, 0), wb.sheets[1].value(0, 0));
        assert_eq!(back.sheets[0].value(0, 0), loaded.sheets[0].value(0, 0));
    }

    #[test]
    fn metadata_only_save_retains_reused_sst_index_and_package_declarations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.xlsx");
        let mut wb = Workbook::new_blank();
        for row in 0..800 {
            wb.sheets[0].set(row,0,Cell {value:Value::Text("Shared".into()),formula:None});
        }
        save_atomic(&path,&wb).unwrap();
        let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        let read = |part: &str| read_zip_text(
            &mut zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap(),part).unwrap();
        let original_strings = read(delta::sst::PART);
        let large = read(&part).replace("</sheetData>", &format!("<!--{}--></sheetData>",
            "x".repeat(crate::fixups::RAW_SHEET_MIN_BYTES as usize)));
        crate::patch::append_entry(&path,&part,large.as_bytes()).unwrap();
        let mut loaded = load(&path).unwrap();
        let note = crate::notes::Note {r:3,c:2,author:"Reviewer".into(),text:"Review".into()};
        loaded.sheets[0].notes.insert(CellRef::new(3,2),note.clone());
        loaded.sheets[0].notes_dirty = true;
        save_atomic(&path,&loaded).unwrap();
        let xml = read(&part);
        assert_eq!(xml.matches("t=\"s\"").count(),800);
        assert!(!xml.contains("inlineStr"));
        assert_eq!(read(delta::sst::PART),original_strings);
        assert!(read("xl/_rels/workbook.xml.rels").contains("/sharedStrings"));
        assert!(read("[Content_Types].xml").contains("spreadsheetml.sharedStrings+xml"));
        let check = load(&path).unwrap();
        assert_eq!(check.sheets[0].value(799,0),Value::Text("Shared".into()));
        assert_eq!(check.sheets[0].notes.get(&CellRef::new(3,2)),Some(&note));
    }

    #[test]
    fn existing_large_grid_and_new_note_survive_metadata_only_dom_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Number(73.0),
                formula: None,
            },
        );
        wb.sheets[0].set(
            1,
            0,
            Cell {
                value: Value::Number(5.0),
                formula: None,
            },
        );
        wb.sheets[0].set(
            1,
            1,
            Cell {
                value: Value::Number(10.0),
                formula: Some("=A2*2".into()),
            },
        );
        wb.sheets[0].set(
            2,
            0,
            Cell {
                value: Value::Text("Rich text".into()),
                formula: None,
            },
        );
        // The metadata writer uses index 0 for the new text on another sheet.
        // The raw-copied sheet and the restored grid still use 0 for the rich SI.
        wb.sheets.push(Sheet::new("Edited"));
        wb.sheets.push(Sheet::new("Untouched"));
        for sheet in &mut wb.sheets[1..] {
            sheet.set(0,0,Cell {value:Value::Text("Rich text".into()),formula:None});
        }
        save_atomic(&path, &wb).unwrap();
        let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        let xml = read_zip_text(
            &mut zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap(),
            &part,
        )
        .unwrap();
        let marker = "<?keep data?><v>0073.000</v>";
        let rich = "<r><rPr><b/></rPr><t xml:space=\"preserve\">Rich </t></r><r><t>text</t></r><rPh sb=\"0\" eb=\"1\"><t>R</t></rPh>";
        assert!(xml.contains("t=\"s\""));
        crate::patch::append_entry(&path,"xl/sharedStrings.xml",format!("<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"1\" uniqueCount=\"1\"><si>{rich}</si></sst>").as_bytes()).unwrap();
        let raw = xml.replace("<v>73</v>", marker).replace(
            "</sheetData>",
            &format!(
                "<!--{}--></sheetData>",
                "grid".repeat(crate::fixups::RAW_SHEET_MIN_BYTES as usize / 4)
            ),
        );
        assert!(raw.contains(marker));
        crate::patch::append_entry(&path, &part, raw.as_bytes()).unwrap();
        let mut model = load(&path).unwrap();
        let note = crate::notes::Note {
            r: 5,
            c: 7,
            author: "Review".into(),
            text: "first note".into(),
        };
        model.sheets[0]
            .notes
            .insert(CellRef::new(5, 7), note.clone());
        model.sheets[0].notes_dirty = true;
        model.sheets[0].set(
            1,
            0,
            Cell {
                value: Value::Number(6.0),
                formula: None,
            },
        );
        for row in [0,1] {
            model.sheets[1].set(row,0,Cell {value:Value::Text("New".into()),formula:None});
        }
        crate::formula::recalc_all(&mut model);
        let s = &model.sheets[0];
        assert!(s.source_cells.as_ref().is_some_and(|src| src.reusable_grid(
            &mut zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap(),
            &part,
            s
        )));
        assert!(
            !s.layout_dirty
                && !s.filter_dirty
                && !s.rich_dirty
                && !s.merges_dirty
                && !s.styles_dirty
                && s.style_edits.is_empty(),
            "layout={}, filter={}, rich={}, merges={}, styles={}",
            s.layout_dirty,
            s.filter_dirty,
            s.rich_dirty,
            s.merges_dirty,
            s.styles_dirty
        );
        assert!(
            crate::patch::entry_size(&path, &part) > crate::fixups::RAW_SHEET_MIN_BYTES,
            "source size: {}, {}",
            crate::patch::entry_size(&path, &part),
            part
        );
        let receipt = save_atomic_with_receipt(&path, &model).unwrap();
        assert!(!receipt.used_cell_delta);
        receipt.adopt_document(&mut model, &path, true);
        let mut reloaded = load(&path).unwrap();
        assert_eq!(reloaded.sheets[1].value(0,0),Value::Text("New".into()));
        assert_eq!(reloaded.sheets[1].value(1,0),Value::Text("New".into()));
        assert_eq!(reloaded.sheets[2].value(0,0),Value::Text("Rich text".into()));
        let sheet = reloaded.sheets.remove(0);
        assert_eq!(sheet.value(0, 0), Value::Number(73.0));
        assert_eq!(sheet.value(1, 1), Value::Number(12.0));
        assert_eq!(sheet.value(1, 0), Value::Number(6.0));
        assert_eq!(sheet.notes.get(&CellRef::new(5, 7)), Some(&note));
        let saved =
            String::from_utf8(crate::fixups::read_part_file(&path, &part).unwrap()).unwrap();
        assert!(saved.contains(marker) && saved.contains("gridgrid"));
        assert!(saved.contains("t=\"s\"") && !saved.contains("inlineStr"));
        let strings = read_zip_text(&mut zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap(),delta::sst::PART).unwrap();
        assert!(strings.contains(rich));
        let doc = roxmltree::Document::parse(&strings).unwrap();
        assert_eq!(doc.descendants().filter(|n| n.has_tag_name("t") && n.text()==Some("New")).count(),1);
        assert_eq!(doc.root_element().children().filter(|n| n.has_tag_name("si")).count(),2);
        assert_eq!(sheet.value(2, 0), Value::Text("Rich text".into()));
        model.sheets[0].set(
            1,
            0,
            Cell {
                value: Value::Number(8.0),
                formula: None,
            },
        );
        crate::formula::recalc_all(&mut model);
        let second = save_atomic_with_receipt(&path, &model).unwrap();
        assert!(second.used_cell_delta);
        assert_eq!(
            load(&path).unwrap().sheets[0].value(1, 1),
            Value::Number(16.0)
        );
    }

    #[test]
    fn roundtrip_save_then_load() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.xlsx");
        let _ = std::fs::remove_file(&path);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("hello".into()), formula: None });
        wb.sheets[0].set(1, 0, Cell { value: Value::Number(42.0), formula: None });
        wb.sheets[0].set(2, 1, Cell { value: Value::Number(3.5), formula: None });

        save(&path, &wb).expect("save");
        assert!(path.exists());

        let back = load(&path).expect("load");
        assert_eq!(back.sheets.len(), 1);
        let s = &back.sheets[0];
        assert_eq!(s.value(0, 0), Value::Text("hello".into()));
        assert_eq!(s.value(1, 0), Value::Number(42.0));
        assert_eq!(s.value(2, 1), Value::Number(3.5));
        assert_eq!(s.value(9, 9), Value::Empty);
    }

    #[test]
    fn date_group_and_color_filter_roundtrip_fresh_and_delta_saves() {
        let dir = std::env::temp_dir().join("sheets_native_filter_rt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("date_color_filter.xlsx");
        let _ = std::fs::remove_file(&path);

        let mut wb = Workbook::new_blank();
        let fill_sid = wb.styles.intern(crate::style::CellStyle {
            fill: Some([255, 240, 200]),
            ..Default::default()
        });
        let font_sid = wb.styles.intern(crate::style::CellStyle {
            color: [192, 0, 0],
            ..Default::default()
        });
        {
            let sh = wb.sheet_mut(0);
            sh.set(0, 0, Cell { value: Value::Text("日期".into()), formula: None });
            sh.set(0, 1, Cell { value: Value::Text("底色".into()), formula: None });
            sh.set(0, 2, Cell { value: Value::Text("字色".into()), formula: None });
            sh.set(1, 0, Cell { value: Value::Number(46351.0), formula: None });
            sh.set(1, 1, Cell { value: Value::Text("A".into()), formula: None });
            sh.set_style(1, 1, fill_sid);
            sh.set(1, 2, Cell { value: Value::Text("B".into()), formula: None });
            sh.set_style(1, 2, font_sid);
            let mut flt = crate::sort_filter::Filter::new(0, 0, 1, 2);
            flt.date_groups.insert(
                0,
                vec![
                    crate::sort_filter::DateGroupItem { year: 2026, month: Some(11), day: None },
                    crate::sort_filter::DateGroupItem { year: 2025, month: None, day: None },
                ],
            );
            flt.color_filters
                .insert(1, crate::sort_filter::ColorFilter::Fill(Some([255, 240, 200])));
            flt.color_filters
                .insert(2, crate::sort_filter::ColorFilter::Font([192, 0, 0]));
            sh.filter = Some(flt);
            sh.filter_dirty = true;
        }
        let receipt = save_atomic_with_receipt(&path, &wb).expect("fresh save");
        receipt.adopt_document(&mut wb, &path, true);

        let back = load(&path).expect("reload fresh");
        let flt = back.sheet(0).filter.as_ref().expect("filter preserved");
        let mut expected_dates = vec![
            crate::sort_filter::DateGroupItem { year: 2025, month: None, day: None },
            crate::sort_filter::DateGroupItem { year: 2026, month: Some(11), day: None },
        ];
        expected_dates.sort();
        assert_eq!(flt.date_groups.get(&0), Some(&expected_dates));
        assert_eq!(
            flt.color_filters.get(&1),
            Some(&crate::sort_filter::ColorFilter::Fill(Some([255, 240, 200])))
        );
        assert_eq!(
            flt.color_filters.get(&2),
            Some(&crate::sort_filter::ColorFilter::Font([192, 0, 0]))
        );

        // 二次增量保存修改为“无填充”颜色筛选与单日筛选
        {
            let sh = wb.sheet_mut(0);
            let f = sh.filter.as_mut().unwrap();
            f.date_groups.insert(
                0,
                vec![crate::sort_filter::DateGroupItem {
                    year: 2026,
                    month: Some(11),
                    day: Some(25),
                }],
            );
            f.color_filters
                .insert(1, crate::sort_filter::ColorFilter::Fill(None));
            sh.filter_dirty = true;
        }
        let r2 = save_atomic_with_receipt(&path, &wb).expect("delta save");
        r2.adopt_document(&mut wb, &path, true);
        let back2 = load(&path).expect("reload delta");
        let flt2 = back2.sheet(0).filter.as_ref().expect("filter preserved after delta");
        assert_eq!(
            flt2.date_groups.get(&0),
            Some(&vec![crate::sort_filter::DateGroupItem {
                year: 2026,
                month: Some(11),
                day: Some(25),
            }])
        );
        assert_eq!(
            flt2.color_filters.get(&1),
            Some(&crate::sort_filter::ColorFilter::Fill(None))
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn formulas_keep_their_position_after_roundtrip() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("formula_pos.xlsx");
        let _ = std::fs::remove_file(&path);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(20.0), formula: None });
        wb.sheets[0].set(0, 1, Cell { value: Value::Number(23.5), formula: None });
        wb.sheets[0].set(1, 3, Cell { value: Value::Empty, formula: Some("=A1*B1".into()) });
        save(&path, &wb).unwrap();

        let mut back = load(&path).unwrap();
        // 公式必须还在 D2，而不是被挪到别的格子
        let cell = back.sheets[0].get(1, 3).expect("D2 应该存在");
        assert_eq!(cell.formula.as_deref(), Some("=A1*B1"));
        crate::formula::recalc_all(&mut back);
        assert_eq!(back.sheets[0].value(1, 3), Value::Number(470.0));
    }

    #[test]
    fn deleted_cells_disappear_after_save() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("delete_cell.xlsx");
        let _ = std::fs::remove_file(&path);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("keep".into()), formula: None });
        wb.sheets[0].set(0, 1, Cell { value: Value::Text("remove".into()), formula: None });
        save(&path, &wb).unwrap();

        let mut wb2 = load(&path).unwrap();
        wb2.sheets[0].set(0, 1, Cell::default()); // 清空 B1
        save(&path, &wb2).unwrap();

        let back = load(&path).unwrap();
        assert_eq!(back.sheets[0].value(0, 0), Value::Text("keep".into()));
        assert_eq!(back.sheets[0].value(0, 1), Value::Empty);
    }

    #[test]
    fn preserves_cells_when_saving_over_existing() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overwrite.xlsx");
        let _ = std::fs::remove_file(&path);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("first".into()), formula: None });
        save(&path, &wb).unwrap();

        // 重新读入、改一格、再存
        let mut wb2 = load(&path).unwrap();
        wb2.sheets[0].set(0, 0, Cell { value: Value::Text("second".into()), formula: None });
        wb2.sheets[0].set(1, 0, Cell { value: Value::Number(7.0), formula: None });
        save(&path, &wb2).unwrap();

        let back = load(&path).unwrap();
        assert_eq!(back.sheets[0].value(0, 0), Value::Text("second".into()));
        assert_eq!(back.sheets[0].value(1, 0), Value::Number(7.0));
        let _ = HashMap::<u8, u8>::new();
        let _ = CellRef::new(0, 0);
        let _ = Sheet::new("x");
    }
    #[test]
    fn col_row_styles_alignment_shrink_and_dbnum_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sc_wps_styles_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wps_roundtrip.xlsx");

        let mut wb = Workbook { styles: crate::style::StyleTable::new(), ..Workbook::default() };
        let mut sh = Sheet::new("Sheet1");

        let mut st_col = crate::style::CellStyle::default();
        st_col.fill = Some([255, 242, 0]);
        let col_sid = wb.styles.intern(st_col);
        sh.set_col_style(1, col_sid);

        let mut st_row = crate::style::CellStyle::default();
        st_row.bold = true;
        let row_sid = wb.styles.intern(st_row);
        sh.set_row_style(2, row_sid);

        // 显式单元格样式覆盖行列，及 CenterContinuous / Distributed / shrink_to_fit / wrap
        let mut st_cc = crate::style::CellStyle::default();
        st_cc.halign = 4;
        st_cc.wrap = true;
        st_cc.shrink_to_fit = true;
        let cc_sid = wb.styles.intern(st_cc);
        sh.set(0, 0, Cell { value: Value::Text("标题".into()), formula: None });
        sh.set_style(0, 0, cc_sid);

        let mut st_dist = crate::style::CellStyle::default();
        st_dist.halign = 5;
        st_dist.format = "[DBNum2][$￥-804]#,##0.00".into();
        let dist_sid = wb.styles.intern(st_dist);
        sh.set(1, 0, Cell { value: Value::Number(12345.67), formula: None });
        sh.set_style(1, 0, dist_sid);

        sh.set(2, 1, Cell { value: Value::Text("RowWinsCol".into()), formula: None });
        assert_eq!(sh.style_of(2, 1), row_sid);
        assert_eq!(sh.style_of(5, 1), col_sid);

        sh.set_style(6, 1, 0);
        assert_eq!(sh.style_of(6, 1), 0);

        wb.sheets = vec![sh];
        save(&path, &wb).unwrap();
        let back = load(&path).unwrap();
        let bsh = &back.sheets[0];

        assert_eq!(back.styles.get(bsh.style_of(0, 0)).halign, 4);
        assert!(back.styles.get(bsh.style_of(0, 0)).wrap);
        assert!(back.styles.get(bsh.style_of(0, 0)).shrink_to_fit);
        assert_eq!(back.styles.get(bsh.style_of(1, 0)).halign, 5);
        assert_eq!(
            crate::style::format_value(&Value::Number(12345.67), &back.styles.get(bsh.style_of(1, 0)).format),
            "壹万贰仟叁佰肆拾伍元陆角柒分"
        );
        assert_eq!(
            crate::style::format_value(&Value::Number(-88.08), &back.styles.get(bsh.style_of(1, 0)).format),
            "负捌拾捌元零捌分"
        );
        assert_eq!(back.styles.get(bsh.style_of(5, 1)).fill, Some([255, 242, 0]));
        assert!(back.styles.get(bsh.style_of(2, 1)).bold);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 把 calamine 的 Style 折成我们的 CellStyle（只保留看得见的部分）
fn convert_style(st: &calamine::Style) -> crate::style::CellStyle {
    let mut out = crate::style::CellStyle { size: 11.0, color: [0, 0, 0], ..Default::default() };
    if let Some(f) = &st.font {
        out.bold = matches!(f.weight, calamine::FontWeight::Bold);
        out.italic = matches!(f.style, calamine::FontStyle::Italic);
        out.underline = !matches!(f.underline, calamine::UnderlineStyle::None);
        out.double_underline = matches!(f.underline, calamine::UnderlineStyle::Double);
        out.strikethrough = f.strikethrough;
        if let Some(n) = &f.name {
            out.font = crate::style::normalize_font(n);
        }
        if let Some(sz) = f.size {
            out.size = sz as f32;
        }
        if let Some(c) = &f.color {
            out.color = [c.red, c.green, c.blue];
        }
    }
    if let Some(fill) = &st.fill {
        if !matches!(fill.pattern, calamine::FillPattern::None) {
            if let Some(c) = &fill.foreground_color {
                if !(c.red == 0 && c.green == 0 && c.blue == 0 && c.alpha == 0) {
                    out.fill = Some([c.red, c.green, c.blue]);
                }
            }
        }
    }
    if let Some(p) = &st.protection {
        out.locked = p.locked;
        out.hidden = p.hidden;
    }
    if let Some(a) = &st.alignment {
        out.halign = match a.horizontal {
            calamine::HorizontalAlignment::Left => 1,
            calamine::HorizontalAlignment::Center => 2,
            calamine::HorizontalAlignment::Right => 3,
            calamine::HorizontalAlignment::CenterContinuous => 4,
            calamine::HorizontalAlignment::Distributed => 5,
            _ => 0,
        };
        out.valign = match a.vertical {
            calamine::VerticalAlignment::Top => 1,
            calamine::VerticalAlignment::Center => 2,
            calamine::VerticalAlignment::Bottom => 3,
            _ => 0,
        };
        out.wrap = a.wrap_text;
        out.shrink_to_fit = a.shrink_to_fit;
        out.indent = a.indent.unwrap_or(0).min(15);
        out.vertical_text = matches!(a.text_rotation, calamine::TextRotation::Stacked);
        out.rotation = match a.text_rotation {
            calamine::TextRotation::Degrees(d) => {
                if d <= 90 {
                    d as i16
                } else {
                    90 - d as i16
                }
            }
            _ => 0,
        };
    }
    if let Some(bs) = &st.borders {
        let map = |b: &calamine::Border| crate::style::Border {
            style: match b.style {
                calamine::BorderStyle::None => 0,
                calamine::BorderStyle::Thin => 1,
                calamine::BorderStyle::Hair => 7,
                calamine::BorderStyle::DashDot => 8,
                calamine::BorderStyle::DashDotDot => 9,
                calamine::BorderStyle::MediumDashed => 10,
                calamine::BorderStyle::MediumDashDot => 11,
                calamine::BorderStyle::MediumDashDotDot => 12,
                calamine::BorderStyle::SlantDashDot => 13,
                calamine::BorderStyle::Medium => 2,
                calamine::BorderStyle::Thick => 3,
                calamine::BorderStyle::Double => 4,
                calamine::BorderStyle::Dashed => 5,
                calamine::BorderStyle::Dotted => 6,
                _ => 1,
            },
            color: b.color.map(|c| [c.red, c.green, c.blue]).unwrap_or([0, 0, 0]),
        };
        out.border = [map(&bs.left), map(&bs.right), map(&bs.top), map(&bs.bottom)];
    }
    if let Some(nf) = &st.number_format {
        if !nf.format_code.is_empty() && nf.format_code != "General" {
            out.format = nf.format_code.clone();
        }
    }
    out
}

/// 只读 OOXML 包里的一个部件，不解压整本工作簿。
/// 开始页用它取 `docProps/thumbnail.jpeg`；文件没有这个部件时返回 None。
pub fn read_part(path: &Path, name: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut entry = zip.by_name(name).ok()?;
    if entry.size() > 4 * 1024 * 1024 {
        return None;
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}
