use crate::model::{Cell, Sheet, Value, Workbook};
use anyhow::{Context, Result};
use calamine::{open_workbook_auto, Data, Reader};
use std::borrow::Cow;
use std::path::Path;

/// .xls 的样式/列宽/行高/合并区（calamine 只给值）
fn apply_xls_meta(path: &Path, wb: &mut Workbook) {
    let is_xls = std::fs::File::open(path).is_ok() && !crate::crypto::has_magic(path, b"PK");
    if !is_xls {
        return;
    }
    if let Some(sizes) = crate::xls_layout::read(path) {
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
    }
    if let Some(sty) = crate::xls_style::read(path) {
        let mut cell_ids: Vec<(usize, u32, u32, u16)> = Vec::new();
        let mut row_ids: Vec<(usize, u32, u16)> = Vec::new();
        let mut col_ids: Vec<(usize, u32, u16)> = Vec::new();
        for (i, sh) in sty.sheets.iter().enumerate() {
            for (r, st) in sh.rows.iter() {
                row_ids.push((i, *r, wb.styles.intern(st.clone())));
            }
            for (c, st) in sh.cols.iter() {
                col_ids.push((i, *c, wb.styles.intern(st.clone())));
            }
            for (r, c, st) in sh.cells.iter() {
                cell_ids.push((i, *r, *c, wb.styles.intern(st.clone())));
            }
        }
        for (i, c, id) in col_ids {
            if let Some(sh) = wb.sheets.get_mut(i) {
                if id != 0 {
                    sh.set_col_style_loaded(c, id);
                }
            }
        }
        for (i, r, id) in row_ids {
            if let Some(sh) = wb.sheets.get_mut(i) {
                if id != 0 {
                    sh.set_row_style_loaded(r, id);
                }
            }
        }
        for (i, r, c, id) in cell_ids {
            if let Some(sh) = wb.sheets.get_mut(i) {
                sh.set_style_loaded(r, c, id);
            }
        }
    }
    if let Ok(all) = crate::notes::load_notes(path) {
        for sheet in wb.sheets.iter_mut() {
            if let Some(list) = all.get(&sheet.name) {
                for n in list.iter() {
                    sheet.notes.insert(crate::model::CellRef::new(n.r, n.c), n.clone());
                }
            }
        }
    }
    if let Ok(calamine::Sheets::Xls(x)) = open_workbook_auto(path) {
        let names: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
        for (i, name) in names.iter().enumerate() {
            if i >= wb.sheets.len() {
                break;
            }
            if let Some(regions) = x.worksheet_merge_cells(name) {
                wb.sheets[i].merges = regions
                    .iter()
                    .map(|d| (d.start.0, d.start.1, d.end.0, d.end.1))
                    .collect();
            }
        }
    }
}
#[path = "xlsx_load.rs"]
mod sheets;
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

/// ErrKind → umya 的错误类型。umya 没有 #SPILL!，用 #VALUE! 代；循环引用在 Excel 里也报 #REF!。
fn to_umya_error(e: crate::formula::ErrKind) -> umya_spreadsheet::CellErrorType {
    use crate::formula::ErrKind as K;
    use umya_spreadsheet::CellErrorType as U;
    match e {
        K::Null => U::Null,
        K::Div0 => U::Div0,
        K::Value => U::Value,
        K::Ref | K::Circular => U::Ref,
        K::Name => U::Name,
        K::Num => U::Num,
        K::Na => U::NA,
        K::Spill => U::Value,
    }
}

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
    match reader.worksheet_style(&name) {
        Ok(range) => {
            for (row, col, style) in range.cells() {
                let id = styles.intern(convert_style(style));
                if id != 0 { sheet.set_style_loaded(row, col, id); }
            }
        }
        Err(e) => diag.push(Some(&name), &part, "单元格样式", format!("{e:#}"), true),
    }
    match reader.worksheet_formula(&name) {
        Ok(range) => {
            let (r0, c0) = range.start().unwrap_or((0, 0));
            for (row, col, f) in range.cells().filter(|(_, _, f)| !f.is_empty()) {
                let r = r0 + row as u32;
                let c = c0 + col as u32;
                let mut cell = sheet.get(r, c).cloned().unwrap_or_default();
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
        let mut cell = sheet.get(row, col).cloned().unwrap_or_default();
        cell.formula = Some(crate::normalize_formula(&format!("={translated}")).into());
        sheet.set(row, col, cell);
    }
    Ok(())
}

/// Office 加密文件是 CFB 容器，里面有一个叫 EncryptedPackage 的流。
/// 这里只做识别：按 1MB 块扫 UTF-16LE 流名，命中就认为需要密码。
fn is_password_encrypted(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 8];
    if f.read_exact(&mut head).is_err()
        || head != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
    {
        return false;
    }
    let needle: Vec<u8> = "EncryptedPackage"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let mut buf = vec![0u8; 1 << 20];
    let mut prev: Vec<u8> = Vec::new();
    loop {
        let Ok(n) = f.read(&mut buf) else { return false };
        if n == 0 {
            return false;
        }
        let mut window = Vec::with_capacity(prev.len() + n);
        window.extend_from_slice(&prev);
        window.extend_from_slice(&buf[..n]);
        if window.windows(needle.len()).any(|w| w == needle.as_slice()) {
            return true;
        }
        let keep = needle.len().saturating_sub(1).min(window.len());
        prev = window[window.len() - keep..].to_vec();
    }
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

pub fn load_diag(path: &Path) -> Result<(Workbook, LoadDiag)> {
    set_load_kind(LOAD_KIND_UNKNOWN);
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
    let (mut shared_zip, package_stamp) = is_ooxml.then(|| sheets::open_stamped(path)).transpose()?.unzip();
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
                let mut wb = crate::xls_read::load(path)?;
                apply_xls_meta(path, &mut wb);
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

    let is_cfb = crate::crypto::has_magic(path, &[0xD0, 0xCF, 0x11, 0xE0]);
    let mut defined_names = crate::patch::defined_names_xml(&workbook_xml);
    let manual_hidden = crate::patch::manual_hidden_from_names(&defined_names);
    defined_names.retain(|(name, _, _)| name != crate::patch::MANUAL_HIDDEN_NAME);
    let (calc_iterate, calc_max_iterations, calc_max_change) =
        crate::patch::calc_iteration_xml(&workbook_xml).unwrap_or((false, 100, 0.001));
    let mut wb = Workbook {
        load_incomplete: false,
        names: defined_names,
        // 数据源存在 `_sc_src_*` 名称里，读出来重建
        sources: crate::external::read_sources(path),
        links: crate::xlinks::read(path),
        names_dirty: false,
        pivot_shifts: Vec::new(),
        pivot_cache_base: Default::default(),
        pivot_session: Default::default(),
        has_vba: crate::vba::has_macros(path),
        date_system: if crate::patch::date1904_xml(&workbook_xml) {
            crate::model::DateSystem::D1904
        } else {
            crate::model::DateSystem::D1900
        },
        pivot_calc_fields: Vec::new(),
        pivots_pending: Vec::new(),
        pivots_pending2: Vec::new(),
        calc_manual: crate::patch::calc_manual_xml(&workbook_xml),
        // calcPr/@iterate / maxIterations / maxChange（缺省 = 关闭 / 100 / 0.001）
        calc_iterate,
        calc_max_iterations,
        calc_max_change,
        calc_pending: false,
        calc_pending_sheets: std::collections::BTreeSet::new(),
        sheets: Vec::new(),
        path: Some(path.to_path_buf()),
        decrypted: None,
        encryption: None,
        source_encryption: None,
        dirty: false,
        styles: crate::style::StyleTable::new(),
        chart_edits: Vec::new(),
    };

    set_load_kind(if is_ooxml { LOAD_KIND_OOXML } else { LOAD_KIND_BIFF_CALAMINE });

    let prof_load = std::env::var("SC_LOAD_PROF").is_ok();
    let t_all_load = std::time::Instant::now();
    // 图片和表格对象从独立部件读取，与工作表 XML 的流式解析并行。
    let (side_tx, side_rx) = std::sync::mpsc::channel();
    if is_ooxml {
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            if injected(&INJECT_SIDE_PANIC, &p) {
                panic!("注入：旁路扫描线程 panic");
            }
            let out = (
                crate::pic::load(&p),
                crate::table::load(&p),
            );
            let _ = side_tx.send(out);
        });
    }
    let t_all = std::time::Instant::now();
    // 主题色：Excel 的 <color theme tint> 不能只当 RGB，读盘时记下索引与 tint
    let theme_book = if is_ooxml {
        crate::style_theme::load(path, &[])
    } else {
        Default::default()
    };
    // 图案填充 / 对角线：不进 CellStyle，按 fillId/borderId 挂到样式表
    let ext_book = if is_ooxml {
        crate::style_ext::load(path)
    } else {
        Default::default()
    };
    let ooxml_styles: Vec<calamine::Style> = if let Some(h) = &header {
        h.styles.clone()
    } else if is_ooxml {
        if let Some(calamine::Sheets::Xlsx(xr)) = &mut reader {
            names
                .iter()
                .find_map(|n| xr.worksheet_cells_reader(n).ok().map(|cr| cr.styles().to_vec()))
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let mut style_ids = std::collections::HashMap::<u16, u16>::new();
    let mut loaded_style_id = |idx: u16, styles: &mut crate::style::StyleTable| {
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
    let mut page_names = std::collections::HashMap::new();
    if workbook_xml.contains("definedName") {
        for i in 0..names.len() as u32 {
            let settings = crate::page::parse_defined_names(&workbook_xml, i);
            if settings != (None, None, None) {
                page_names.insert(i, settings);
            }
        }
    }
    let states = crate::patch::sheet_states_xml(&workbook_xml);
    drop(workbook_xml);
    let mut layouts = std::collections::HashMap::new();
    let styles_xml = shared_zip.as_mut().and_then(|zip| read_zip_text(zip, "xl/styles.xml").ok()).unwrap_or_default();
    let dxfs = crate::cf::parse_dxfs(&styles_xml);
    let dxf_ext = crate::cf::dxfs_unsupported(&styles_xml);
    if let Some(header) = &header {
        let loaded = sheets::worksheets(
            path, shared_zip.as_mut().unwrap(), header, &targets, &dxfs, dxf_ext,
        )?;
        for mut part in loaded {
            for (r, c, xf) in part.xfs {
                let id = loaded_style_id(xf, &mut wb.styles);
                part.sheet.styles.insert(crate::CellRef::new(r, c), id);
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
                            let id = loaded_style_id(xf, &mut wb.styles);
                            sheet.styles.insert(crate::CellRef::new(r, c), id);
                        });
                        match shell {
                            Ok((shell, implicit_coordinates)) => {
                                sheet.source_needs_dom_repair |= implicit_coordinates;
                                layouts.insert(name.clone(), crate::merges::sheet_layout(&shell, &dxfs, dxf_ext));
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
    let (pics, tables) = if is_ooxml {
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
    apply_xls_meta(path, &mut wb);
    if is_cfb {
        wb.calc_manual = crate::xls_read::read_calc_manual(path);
    }

    // 还原工作表布局和合并区域
    {
        let wb_sheets = &mut wb.sheets;
        let wb_styles = &mut wb.styles;
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
                for c in lay.hidden_cols.iter() {
                    sheet.hidden_cols.insert(*c);
                }
                for r in lay.hidden_rows.iter() {
                    sheet.hidden_rows_user.insert(*r);
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
                    let fh = f.hidden_rows(sheet);
                    // 文件里 row hidden 既可能来自手动隐藏，也可能来自筛选。
                    // 先把筛选能解释的行从手动集合移出，否则关闭筛选时
                    // union 会把它们永久当成手动隐藏。同时存在手动和筛选重叠的行
                    // 无法从 OOXML 区分，这里按筛选隐藏处理；清除/关闭筛选时该行恢复。
                    for r in &fh {
                        sheet.hidden_rows_user.remove(r);
                    }
                    sheet.hidden_rows = fh.into();
                }
                if let Some(map) = &manual_hidden {
                    // 本应用保存的手动隐藏行来源：它明确区分了
                    // 筛选隐藏与手动隐藏，包括两者重叠的行，不再靠猜 hidden 。
                    sheet.hidden_rows_user = map.get(&sheet.name).cloned().unwrap_or_default().into();
                }
            }
        }
    }

    if prof_load {
        eprintln!("[load] 总计 {:.2}s", t_all_load.elapsed().as_secs_f64());
    }
    // 图表：drawing 是 OOXML 结构，BIFF(.xls) 没有，别拿 zip 扫描器去读
    let charts_res = if is_ooxml {
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

    // 批注
    if let Ok(all) = crate::notes::load_notes(path) {
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
            if let Some(&hide) = states.get(&sheet.name) {
                sheet.hidden = hide;
            }
            sheet.saved_cells = Some(sheet.cell_version().clone());
            if let (Some(zip), Some(part)) = (shared_zip.as_mut(), sheet.source_part.as_deref()) {
                sheet.source_cells = SourceCells::capture(zip, part, sheet);
                if let Some(source) = &mut sheet.source_cells { source.set_style_map(delta_styles.clone()); }
            }
        }
        wb.pivot_cache_base = std::sync::Arc::new(crate::model::PivotCacheBase {
            definitions: pivot_definitions(path),
            sheets: wb.sheets.iter().map(|s| (s.identity, s.name.clone())).collect(),
        });
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
    if let Some(stamp) = package_stamp {
        anyhow::ensure!(sheets::package_stamp(path)? == stamp, "读取期间工作簿已被替换，请重新打开");
    }
    Ok((wb, diag))
}

fn pivot_definitions(path: &Path) -> Vec<(String, Vec<u8>)> {
    crate::fixups::parts(path).into_iter()
        .filter(|name| name.contains("pivotCacheDefinition") && name.ends_with(".xml"))
        .filter_map(|name| crate::fixups::read_part_file(path, &name).map(|body| (name, body)))
        .collect()
}

/// Metadata produced by one successfully replaced OOXML package. It can be
/// applied to a newer live document without resetting edits made during save.
#[derive(Debug, Default)]
pub struct SaveReceipt {
    /// True when sheet cell changes were applied by the verified source-package delta writer.
    pub used_cell_delta: bool,
    sheets: std::collections::HashMap<u64, SavedSheet>,
    source_encryption: Option<crate::crypto::EncryptionSettings>,
    committed_charts: std::collections::HashMap<u64, Vec<SavedChart>>,
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
    part: String,
    drawing: String,
    anchor: (u32, u32, u32, u32),
    ref_fixes: Vec<(String, String)>,
    created: Option<(crate::charts::Chart, String, String)>,
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
        for sheet in &mut wb.sheets {
            if let Some(saved) = self.sheets.get(&sheet.identity) {
                sheet.source_part = Some(saved.part.clone());
            }
            if let Some(saved) = self.committed_charts.get(&sheet.identity) {
                let mut dirty = saved.len() != sheet.charts.len();
                for (index, chart) in sheet.charts.iter_mut().enumerate() {
                    let Some((saved_index, baseline)) = saved.iter().enumerate().find(|(i, old)| {
                        old.part == chart.part && (!old.part.is_empty() || *i == index)
                    }) else {
                        dirty = true;
                        continue;
                    };
                    // Edits made while the worker was saving append to this journal.
                    if let Some((original, part, drawing)) = &baseline.created {
                        let anchor = std::mem::replace(&mut chart.anchor, original.anchor);
                        let same = chart == original;
                        chart.anchor = anchor;
                        if !same {
                            dirty = true;
                            continue;
                        }
                        chart.part.clone_from(part);
                        chart.drawing.clone_from(drawing);
                    }
                    if chart.ref_fixes.starts_with(&baseline.ref_fixes) {
                        chart.ref_fixes.drain(..baseline.ref_fixes.len());
                    }
                    dirty |= saved_index != index || baseline.anchor != chart.anchor
                        || (baseline.created.is_none() && baseline.drawing != chart.drawing)
                        || !chart.ref_fixes.is_empty();
                }
                sheet.charts_dirty = dirty;
            }
        }
    }

    /// Adopt a completed save as the document's new source file. Exports and
    /// stale in-flight snapshots must not retire the live document's journals.
    pub fn adopt_document(&self, wb: &mut Workbook, target: &Path, calculation_matches: bool) {
        self.apply_to(wb);
        wb.path = Some(target.to_owned());
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
}

// Reconstruct only unambiguous raw XF numbers from the committed style part.
// The header reader supplies a synthetic empty worksheet, so collecting a save
// receipt never inflates sheetData or the shared-string table.
fn committed_style_map(
    path: &Path,
    zip: &mut zip::ZipArchive<std::fs::File>,
    wb: &Workbook,
    targets: &std::collections::HashMap<String, String>,
    book: &str,
    rels: &[u8],
) -> Option<std::sync::Arc<delta::StyleMap>> {
    let header = sheets::header(zip, targets, book, Some(rels)).ok()??;
    let theme = crate::style_theme::load(path, &[]);
    let ext = crate::style_ext::load(path);
    let mut ids = std::collections::HashMap::new();
    let mut defaults = std::collections::HashMap::new();
    for (index, style) in header.styles.iter().enumerate() {
        let raw = u16::try_from(index).ok()?;
        let mut cell = convert_style(style);
        let xf = theme.xfs.get(index).copied().unwrap_or_default();
        crate::style_theme::apply_resolved(
            &mut cell,
            Some(&xf),
            theme.resolved.get(index),
            &theme.palette,
        );
        let pattern = ext
            .xf_fill
            .get(index)
            .and_then(|&n| ext.patterns.get(n as usize))
            .and_then(Option::as_ref);
        let diagonal = ext
            .xf_border
            .get(index)
            .and_then(|&n| ext.diags.get(n as usize))
            .and_then(Option::as_ref);
        cell.diagonal = diagonal.cloned();
        let quoted = ext.xf_quote.get(index).copied().unwrap_or(false);
        let matching_id = |candidate: &crate::style::CellStyle| {
            let id = wb.styles.find_style(candidate)?;
            (wb.styles.theme_of(id) == xf
                && wb.styles.pattern_of(id) == pattern
                && wb.styles.diag_of(id) == diagonal
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
                    committed_style_map(path,&mut zip,wb,&targets,&book,rels.as_bytes())) {
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
        committed_charts: wb.sheets.iter().filter(|s| s.charts_dirty || write.created_charts.contains_key(&(s.identity, 0))).map(|sheet| {
            let charts = sheet.charts.iter().enumerate().map(|(i, chart)| SavedChart {
                part: chart.part.clone(), drawing: chart.drawing.clone(),
                anchor: chart.anchor, ref_fixes: chart.ref_fixes.clone(),
                created: write.created_charts.get(&(sheet.identity, i)).map(|(part, drawing)| (chart.clone(), part.clone(), drawing.clone())),
            }).collect();
            (sheet.identity, charts)
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
        // 两种合法情况：老的 .xls（BIFF），或加了打开密码的 OOXML（CFB 里装 EncryptedPackage）。
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
        let mut e = zip.by_index(i).with_context(|| format!("读取保存产物第 {i} 个条目失败"))?;
        let name = e.name().to_owned();
        if critical.contains_key(name.as_str()) {
            anyhow::ensure!(seen_critical.insert(name.clone()), "保存产物关键部件重复：{name}");
        } else if worksheets.contains(&name) {
            delta::stream::xml_input(&mut e, &name, |source| {
                check_xml_reader(&name, std::io::BufReader::new(source), Some("worksheet"))?;
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

pub(crate) fn read_zip_text(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String> {
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

    // 先在本机私有临时目录内完成全部序列化、XML 补丁与完整性校验，不在用户原文件目录产生中间碎片文件
    let build_scratch = private_workbook_dir()?;
    let staged = build_scratch.path().join(&name);
    let _logical_guard = LogicalSaveTargetGuard::set(path);
    let is_xls = path
        .extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("xls"));
    let receipt = if is_xls {
        crate::xls_write::write(&staged, wb).map(|_| SaveReceipt::default())?
    } else {
        write_ooxml(&staged, wb)?
    };
    // 写成功 ≠ 产物完整：临时文件先过完整性门禁，不通过绝不触碰原文件目录
    corrupt_tmp_for_test(&staged);
    if let Err(e) = verify_saved_package(&staged) {
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
                let rollback = build_scratch.path().join(format!("rollback-{name}"));
                let have_rollback = std::fs::copy(path, &rollback).is_ok();
                match std::fs::OpenOptions::new().write(true).open(path) {
                    Ok(mut dst_f) if have_rollback => {
                        original_state = OriginalState::Overwritten;
                        let copy_res = (|| -> std::io::Result<()> {
                            let src_path = if tmp.exists() { tmp.as_path() } else { staged.as_path() };
                            let mut src_f = std::fs::File::open(src_path)?;
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
        Ok(()) => Ok(receipt),
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

fn write_ooxml(path: &Path, wb: &mut Cow<'_, Workbook>) -> Result<SaveReceipt> {
    if wb.encryption.is_none() && wb.source_encryption.is_none() {
        let write = save_plain_base(path, wb, None)?;
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
        let bytes = crate::crypto::decrypt(src, &credentials.password)?;
        std::fs::write(&base, bytes).context("写临时明文基准失败")?;
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
        return package_receipt(output, wb, write);
    };
    corrupt_tmp_for_test(output);
    verify_saved_package(output).context("加密前的临时明文未通过完整性校验")?;
    let receipt = package_receipt(output, wb, write)?;
    let bytes = std::fs::read(output).context("读取临时明文失败")?;
    crate::crypto::encrypt(path, &bytes, enc)?;
    Ok(receipt)
}

/// 带打开密码的文件：解密到临时文件后按普通 xlsx 加载。
/// 工作簿记住原路径与密码，另存时继续加密。
pub fn load_with_password(path: &Path, password: &str) -> Result<Workbook> {
    load_with_password_diag(path, password).map(|(wb, _)| wb)
}

/// 带密码打开并返回诊断：加密文件里的工作表坏了同样要报"不完整"，
/// 不能因为是加密包就把诊断丢掉。
pub fn load_with_password_diag(path: &Path, password: &str) -> Result<(Workbook, LoadDiag)> {
    let plain = crate::crypto::decrypt(path, password)?;
    let scratch = private_workbook_dir()?;
    let tmp = scratch.path().join("source.xlsx");
    std::fs::write(&tmp, &plain).with_context(|| "写入解密临时文件失败")?;
    let (mut wb, diag) = load_diag(&tmp)?;
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

/// Sparse umya input and the data ranges that must be restored after its metadata patches.
struct SlimBase {
    path: tempfile::TempPath,
    omitted: Vec<String>,
    grids: std::collections::HashMap<String, PreservedGrid>,
}
struct PreservedGrid {
    source_part: String,
    edits: Option<tempfile::NamedTempFile>,
    dimension: Option<String>,
}

fn slim_umya_base(base: &Path, wb: &Workbook) -> Result<Option<SlimBase>> {
    use std::io::{BufWriter, Write};
    let targets = crate::patch::sheet_targets(base)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(base)?)?;
    let mut candidates = std::collections::HashMap::new();
    let styles_stable = wb.sheets.iter().all(|s| {
        !s.styles_dirty && !s.row_styles_dirty && !s.col_styles_dirty && s.style_edits.is_empty()
    });
    let mut omitted = Vec::new();
    let mut grids = std::collections::HashMap::new();
    for (idx, sheet) in wb.sheets.iter().enumerate() {
        let Some(part) = targets.get(&sheet.name) else {
            continue;
        };
        let large = *part == format!("xl/worksheets/sheet{}.xml", idx + 1)
            && zip.by_name(part)?.size() > crate::fixups::RAW_SHEET_MIN_BYTES;
        let omit = large && crate::fixups::can_restore_sheet_xml(sheet);
        let keep_grid = large
            && !omit
            && styles_stable
            && (sheet.notes_dirty || sheet.cf_dirty || sheet.dv_dirty || sheet.charts_dirty)
            && !sheet.layout_dirty
            && !sheet.filter_dirty
            && !sheet.rich_dirty
            && !sheet.merges_dirty
            && sheet
                .source_cells
                .as_ref()
                .is_some_and(|s| s.reusable_grid(&mut zip, part, sheet));
        let check_links =
            sheet.hyperlinks_dirty || sheet.hyperlinks.values().any(|h| h.span != (0, 0));
        if omit || keep_grid || check_links || sheet.source_needs_dom_repair {
            candidates.insert(
                part.clone(),
                (sheet.name.clone(), omit, keep_grid, check_links, sheet.source_needs_dom_repair),
            );
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let (file, path) = tempfile::Builder::new()
        .prefix("cellan-umya-")
        .suffix(".xlsx")
        .tempfile()?
        .into_parts();
    let mut out = zip::ZipWriter::new(BufWriter::new(file));
    for i in 0..zip.len() {
        let part = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        let Some((name, omit, mut keep_grid, check_links, repair_input)) = candidates.remove(&part) else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
            continue;
        };
        let mut shell = None;
        let mut edited = None;
        if keep_grid {
            let sheet = wb
                .sheets
                .iter()
                .find(|s| s.name == name)
                .context("找不到原工作表")?;
            let source = sheet.source_cells.as_ref().context("缺少单元格基准")?;
            if !source.unchanged_values(sheet) {
                edited = source.stage_grid_edits(&mut zip, &part, sheet, &wb.styles)?;
                keep_grid = edited.is_some();
            }
        }
        if omit || keep_grid {
            let mut bytes = Vec::new();
            let (portable, dimension) = if let Some(edited) = &edited {
                delta::split_grid(edited.reopen()?, &part, &mut bytes, None)?
            } else {
                delta::split_grid(zip.by_index(i)?, &part, &mut bytes, None)?
            };
            if omit {
                omitted.push(name.clone());
                shell = Some(String::from_utf8(bytes)?);
            } else if portable {
                grids.insert(
                    name,
                    PreservedGrid {
                        source_part: part.clone(),
                        edits: edited,
                        dimension,
                    },
                );
                shell = Some(String::from_utf8(bytes)?);
            }
        }
        if check_links {
            if shell.is_none() {
                let mut xml = read_zip_text(&mut zip, &part)?;
                if crate::hyperlinks::remove_range_links(&mut xml) {
                    shell = Some(xml);
                }
            } else if let Some(xml) = &mut shell {
                crate::hyperlinks::remove_range_links(xml);
            }
        }
        if repair_input {
            out.start_file(&part, zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated))?;
            if let Some(xml) = shell {
                delta::stream::repair_dom_input(xml.as_bytes(), &mut out)?;
            } else {
                delta::stream::xml_input(zip.by_index(i)?, &part, |input|
                    delta::stream::repair_dom_input(input, &mut out))?;
            }
        } else if let Some(xml) = shell {
            out.start_file(
                part,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )?;
            out.write_all(xml.as_bytes())?;
        } else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    out.finish()?.flush()?;
    Ok(Some(SlimBase {
        path,
        omitted,
        grids,
    }))
}

/// The shell is the fully patched worksheet. Transfer the source data directly
/// from the original ZIP (or its staged value edits), retaining new metadata.
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
    let result = save_plain_at(path, input, base_override, true, &mut flags);
    if let Some(book) = borrowed {
        *input = Cow::Borrowed(book);
    } else if let Cow::Owned(book) = input {
        restore_sheet_flags(book, &mut flags);
    }
    result
}

fn save_plain_at(
    path: &Path, input: &mut Cow<'_, Workbook>, base_override: Option<&Path>,
    allow_stream: bool, flags: &mut Vec<OriginalSheetFlags>,
) -> Result<CellWrite> {
    let wb: &Workbook = input;
    note_save_target(path);
    // 先把被清空的单元格从文件里删掉（见 Sheet::cleared）
    use umya_spreadsheet::{new_file, reader::xlsx::read, writer::xlsx::write};

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
    let t_read = std::time::Instant::now();
    // 基准文件必须用「打开时的原文件」，不能用另存的目标路径：
    // 否则另存出来的文件会丢掉合并单元格、样式、数据验证、图片等
    // 我们不解析也不重建的结构（umya 从零建表只会写单元格）。
    let original_base = match base_override
        .filter(|p| p.exists() && usable_base)
        .map(|p| p.to_path_buf())
        .or_else(|| wb.decrypted.as_ref().filter(|p| p.exists() && usable_base).cloned())
        // 打开不完整的文件不能当保存基准：里面的坏 XML 会让解析器直接崩，
        // 而且我们没读到的东西写回去就是丢数据。
        .or_else(|| wb.path.as_ref().filter(|p| p.exists() && !wb.load_incomplete).cloned())
    {
        Some(p) => p,
        None => path.to_path_buf(),
    };
    delta::verify_note_sources(&original_base, wb)?;
    // .xls / .xlsb 这类二进制基准不是 zip 包：不能当包结构模板，
    // 否则 prepare_base 读 xl/workbook.xml 直接失败（“读取基准工作簿目录失败”）。
    // 这种情况按全新文件从模型重建。
    let original_base = if crate::fixups::read_part(&original_base, "xl/workbook.xml").is_some() {
        original_base
    } else {
        path.to_path_buf()
    };
    // Reconcile the package manifest before umya reads it. This retains the
    // original worksheet parts even when their visible names/order have changed.
    let prepared_base = structure::prepare_base(&original_base, path, wb)?;
    let base = prepared_base.path.clone();
    // 宏部件：umya 写不出来，先留一份，写完塞回去
    let vba = crate::vba::read_vba(&base);
    // 原地保存时，写盘会把 base 覆盖掉——先把「要保住的那部分」复制一份出来
    let restore_src = if base == path && base.exists() {
        let tmp = path.with_extension("preserve.tmp");
        std::fs::copy(&base, &tmp).context("无法保留原地保存基准")?;
        Some(tmp)
    } else {
        None
    };
    let restore_base = restore_src.clone().unwrap_or_else(|| base.clone());
    let _restore_cleanup = RestoreFile(restore_src);
    // 必须在写入目标 path 之前锁定基准是否存在：新建文件时 restore_base 等于 path，
    // 一旦 umya::write 落盘后 restore_base.exists() 会由 false 变 true，误把新写文件当旧基准覆盖。
    let has_restore_base = restore_base.exists();
    let run_restore_steps = has_restore_base || std::env::var("SC_FAIL_SAVE_STEP").is_ok();
    // 没有可用的基准（抢救副本 / 全新文件）：整份内容只能从模型重建，
    // 做过载入的表也要把样式、合并、批注等写出来，不能等 dirty 标记。
    let fresh_base = !base.exists();
    let fresh_streaming = allow_stream && fresh_base && !wb.load_incomplete && !wb.styles.quote_of(0)
        && wb.links.is_empty() && wb.pivots_pending.is_empty() && wb.pivots_pending2.is_empty()
        && wb.pivot_calc_fields.is_empty();
    let streamed: Vec<_> = wb.sheets.iter()
        .map(|s| fresh_streaming && s.source_part.is_none() && fresh::supported(s)
            && !s.row_styles.values().chain(s.col_styles.values()).any(|&id| wb.styles.quote_of(id)))
        .collect();
    // A fresh umya sheet has no previous user metadata to remove. Rebuild all
    // populated features (including unedited rescue data) and keep our page defaults.
    if fresh_base {
        flags.extend(input.to_mut().sheets.iter_mut().zip(&streamed)
            .map(|(sheet, &streamed)| OriginalSheetFlags::prepare(sheet, streamed)));
    }
    let wb: &Workbook = input;
    // 外链关系的原文（URL/TargetMode）：有的引擎会把 URL 当包内路径改写
    let external_rels = crate::notes::capture_external_rels(if base.exists() { &base } else { path });
    let sheet_needs_umya = |s: &crate::model::Sheet| -> bool {
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
        && std::env::var("SC_FAIL_SAVE_STEP").is_err()
        && crate::patch::read_date1904(&base) == wb.date_system.is_1904()
        && {
            let bt = crate::patch::sheet_targets(&base).unwrap_or_default();
            bt.len() == wb.sheets.len() && wb.sheets.iter().all(|s| bt.contains_key(&s.name))
        };

    let t_write = std::time::Instant::now();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let unchanged_cells = !wb.sheets.iter().any(&sheet_needs_umya);
    let mut saved_styles = None;
    let patched = reusable_package && !unchanged_cells
        && delta::write_with_style_receipt(&restore_base, path, wb, &mut saved_styles)?;
    let can_bypass_umya = reusable_package && (unchanged_cells || patched);
    let untouched = wb.sheets.iter().all(|s| !s.styles_dirty && !s.col_styles_dirty && !s.row_styles_dirty);
    let base_styles = if has_restore_base && !can_bypass_umya && untouched {
        crate::fixups::read_part_file(&restore_base, "xl/styles.xml")
    } else {
        None
    };
    let mut restored_sheets = Vec::new();
    let mut omitted_sheets = Vec::new();
    let mut preserved_grids = std::collections::HashMap::new();
    if can_bypass_umya {
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
    } else {
        let mut book = if base.exists() {
            let mut slim = slim_umya_base(&base, wb)?;
            let source = if let Some(slim) = &mut slim {
                omitted_sheets = std::mem::take(&mut slim.omitted);
                preserved_grids = std::mem::take(&mut slim.grids);
                slim.path.as_ref()
            } else {
                base.as_path()
            };
            read(source).with_context(|| format!("读取现有文件失败 {}", base.display()))?
        } else {
            new_file()
        };

        // 日期体系：base 是原文件（多带同一个 flag），新建文件要显式写
        if book.date1904() != wb.date_system.is_1904() {
            book.set_date1904(wb.date_system.is_1904());
        }

        if prof {
            eprintln!(
                "  [save] 读入原文件 {:.2}s rss {:.0}MB",
                t_read.elapsed().as_secs_f64(),
                crate::mem::rss_mb()
            );
        }
        let t_build = std::time::Instant::now();
        for (idx, sheet) in wb.sheets.iter().enumerate() {
            // 新建文件默认带一个 Sheet1：名字不同时直接改名，避免多出一张空表
            let needs_rename = !base.exists()
                && idx == 0
                && book.get_sheet(&0).ok().map(|s| s.get_name().to_string()) != Some(sheet.name.clone());

            if needs_rename {
                if let Ok(ws) = book.get_sheet_mut(&0) {
                    ws.set_name(sheet.name.clone());
                }
            }

            let exists = book.get_sheet_by_name(&sheet.name).is_ok();
            if !exists {
                book.new_sheet(sheet.name.clone())
                    .with_context(|| format!("创建表失败 {}", sheet.name))?;
            } else if !fresh_base && has_restore_base && !sheet_needs_umya(sheet) {
                // 未编辑过的工作表在后续 restore_unedited_sheets 会整张按基准恢复，
                // 这里直接跳过几十万格 ws.get_cell_mut 遍历改写，同时完整保留 book 内的共享字符串引用。
                continue;
            }

            let ws = book
                .get_sheet_by_name_mut(&sheet.name)
                .with_context(|| format!("找不到表 {}", sheet.name))?;

            for cref in sheet.cleared.iter() {
                let coord = (cref.col + 1, cref.row + 1);
                if sheet.explicit_style_of(cref.row, cref.col).is_some() {
                    // Keep the original XF (including unmodeled fields) on a formatted blank.
                    *ws.get_cell_mut(coord).cell_value_mut() = Default::default();
                } else {
                    ws.remove_cell(coord);
                }
            }

            // 样式：正常保存只回写用户改过的格子（整表重写样式在 10 万格级别会拖慢保存）；
            // 抢救副本没有基准，整表有样式的格都要写，否则完好表的格式会丢。
            let style_targets: Vec<(u32, u32)> = if fresh_base {
                // 从样式表枚举：空白但有填充/边框的格不在 sheet.iter() 里，漏了就丢格式；
                // 同时保留对整行/整列样式的显式默认(0)覆盖
                let mut examples = std::collections::HashSet::new();
                sheet
                    .styles
                    .iter()
                    .filter(|(c, id)| {
                        **id != 0
                            || sheet.row_styles.contains_key(&c.row)
                            || sheet.col_styles.contains_key(&c.col)
                    })
                    // The streamed values need only one raw XF example per model style.
                    // Style-only blanks must still appear at their original coordinates.
                    .filter(|(c, id)| !streamed[idx] || !sheet.contains(c.row, c.col) || examples.insert(**id))
                    .map(|(c, _)| (c.row, c.col))
                    .collect()
            } else {
                sheet.style_edits.iter().map(|c| (c.row, c.col)).collect()
            };
            for (row, col) in style_targets {
                let sid = sheet.style_of(row, col);
                let cell = ws.get_cell_mut((col + 1, row + 1));
                write_umya_style(wb, sid, cell.get_style_mut());
                if streamed[idx] && sheet.contains(row, col) { cell.set_value_number(0.0); }
            }
            let style_source = sheet.source_cells.as_ref().filter(|_| !fresh_base);
            if fresh_base || sheet.row_styles_dirty {
                if let Some(src) = style_source {
                    for &r in src.row_styles.keys().filter(|r| !sheet.row_styles.contains_key(r)) {
                        ws.row_dimension_mut(r + 1).set_style(Default::default());
                    }
                }
                for (&r, &sid) in &sheet.row_styles {
                    if style_source.and_then(|s| s.row_styles.get(&r)) != Some(&sid) {
                        write_umya_style(wb, sid, ws.get_row_dimension_mut(&(r + 1)).get_style_mut());
                    }
                }
            }
            if fresh_base || sheet.col_styles_dirty {
                if let Some(src) = style_source {
                    for &c in src.col_styles.keys().filter(|c| !sheet.col_styles.contains_key(c)) {
                        ws.column_dimension_by_number_mut(c + 1).set_style(Default::default());
                    }
                }
                for (&c, &sid) in &sheet.col_styles {
                    if style_source.and_then(|s| s.col_styles.get(&c)) != Some(&sid) {
                        let cd = ws.get_column_dimension_by_number_mut(&(c + 1));
                        let w_px = sheet.cols.size(c);
                        if (w_px - crate::layout::DEFAULT_COL_W).abs() > 0.5 && w_px > 0.0 {
                            cd.set_width(((w_px - 5.0) / 8.0).max(0.5) as f64);
                        }
                        write_umya_style(wb, sid, cd.get_style_mut());
                    }
                }
            }

            // 合并区域：没有基准时整份重建；有基准时只在改过才同步
            if fresh_base || sheet.merges_dirty {
                let refs: Vec<umya_spreadsheet::Range> = sheet
                    .merges
                    .iter()
                    .map(|(r1, c1, r2, c2)| {
                        let mut rg = umya_spreadsheet::Range::default();
                        rg.set_range(format!(
                            "{}{}:{}{}",
                            col_name(*c1),
                            r1 + 1,
                            col_name(*c2),
                            r2 + 1
                        ));
                        rg
                    })
                    .collect();
                *ws.get_merge_cells_mut() = refs;
            }

            if streamed[idx] || preserved_grids.contains_key(&sheet.name) { continue; }
            for (cref, cell) in sheet.iter() {
                // 动态数组的溢出值不落盘：文件里只保存锚点公式，打开时重新计算
                if sheet.spilled.contains(&cref) {
                    // A moved spill may cover a previously persisted ordinary cell.
                    // Its old value must not become a blocker when reopening.
                    ws.remove_cell((cref.col + 1, cref.row + 1));
                    continue;
                }
                let coord = (cref.col + 1, cref.row + 1);
                if sheet.explicit_style_of(cref.row, cref.col).is_none() {
                    // Reuse the dimension's actual package style so a value-only save
                    // can restore the original styles.xml without dangling XF indices.
                    let inherited = if sheet.row_styles.contains_key(&cref.row) {
                        ws.row_dimension(coord.1).map(|r| r.style().clone())
                    } else if sheet.col_styles.contains_key(&cref.col) {
                        ws.column_dimension_by_number(coord.0).map(|c| c.style().clone())
                    } else {
                        None
                    };
                    if let Some(style) = inherited {
                        ws.cell_mut(coord).set_style(style);
                    }
                }

                if let Some(f) = &cell.formula {
                    // OOXML 里 <f> 不带等号；内部统一带，写盘时去掉。
                    // 加载时剥掉的 _xlfn./_xlws./_xlpm. 前缀在这里加回：umya 不会加，
                    // 少了前缀的 XLOOKUP/FILTER/LET 在 Excel 里打开就是 #NAME?
                    let text = crate::add_fn_prefixes(f.strip_prefix('=').unwrap_or(f));
                    let target = ws.get_cell_mut(coord);
                    target.set_formula(text);
                    // 缓存值一律按模型里的当前值写。只给外部引用写的话，改了输入格、
                    // 公式格重算过之后，文件里留的还是旧的 <v>（Excel 要重算才看得对）。
                    match &cell.value {
                        Value::Number(n) => {
                            target.set_formula_result_number(*n);
                        }
                        Value::Error(e) => {
                            // 真正的错误值写成 t="e"，Excel 打开就是 #DIV/0! 这类错误，
                            // 而不是一段看起来像错误的文本
                            target.set_formula_result_error(to_umya_error(*e));
                        }
                        Value::Text(t) => {
                            target.set_formula_result_string(t.to_string());
                        }
                        Value::Bool(v) => {
                            target.set_formula_result_bool(*v);
                        }
                        _ => {}
                    }
                    continue;
                }
                match &cell.value {
                    Value::Empty => {}
                    Value::Number(n) => {
                        ws.get_cell_mut(coord).set_value_number(*n);
                    }
                    Value::Text(t) => {
                        ws.get_cell_mut(coord).set_value_string(t.as_ref());
                    }
                    Value::Bool(b) => {
                        ws.get_cell_mut(coord).set_value_bool(*b);
                    }
                    Value::Error(e) => {
                        ws.get_cell_mut(coord).set_formula_result_error(to_umya_error(*e));
                    }
                }
            }
        }

        // New sheets may have been inserted between retained sheets. The serializer
        // must follow the model's order, not the order in the base package.
        let order: std::collections::HashMap<&str, usize> = wb.sheets.iter()
            .enumerate().map(|(i, sheet)| (sheet.name.as_str(), i)).collect();
        book.sheet_collection_mut().sort_by_key(|sheet| {
            order.get(sheet.get_name()).copied().unwrap_or(usize::MAX)
        });

        // 批注：直接走 umya（它会生成 comments 部件和 vml 绘图）
        for (idx, sheet) in wb.sheets.iter().enumerate() {
            // 删掉最后一条批注也要写：不能因为 notes 为空就跳过，
            // 否则旧批注还留在盘上的 comments 部件里。
            if !(fresh_base || sheet.notes_dirty) {
                continue;
            }
            if let Ok(ws) = book.get_sheet_by_name_mut(&sheet.name) {
                // 整表重写：先清空再从模型里灌（批注数量通常很小）；以 map key 为准避免移动后坐标漂移
                ws.get_comments_mut().clear();
                for (k, n) in sheet.notes.iter() {
                    let mut nc = umya_spreadsheet::Comment::default();
                    let mut coord = umya_spreadsheet::Coordinate::default();
                    coord.set_coordinate(format!("{}{}", crate::model::col_label(k.col), k.row + 1));
                    *nc.coordinate_mut() = coord;
                    nc.set_author(if n.author.is_empty() { "Hermes" } else { &n.author });
                    nc.set_text_string(n.text.clone());
                    ws.add_comments(nc);
                }
                let _ = idx;
            }
        }

        if prof {
            eprintln!(
                "  [save] 组装 DOM {:.2}s rss {:.0}MB",
                t_build.elapsed().as_secs_f64(),
                crate::mem::rss_mb()
            );
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        let out = write(&book, path).with_context(|| format!("写入失败 {}", path.display()));
        if prof {
            eprintln!(
                "  [save] 序列化+压缩 {:.2}s rss {:.0}MB",
                t_write.elapsed().as_secs_f64(),
                crate::mem::rss_mb()
            );
        }
        out?;
    }

    // 第一批内存暂存合并（merge-sheet-data / styles / custom-xml / vba / pivot-definitions / sources）
    crate::patch::begin_stage_for(path);

    // 只把 sheetData 换进去，工作表骨架沿用基准
    if run_restore_steps && restore_base.exists() && !can_bypass_umya {
        let merged = must_save!("merge-sheet-data", crate::fixups::merge_sheet_data(
            path,
            &restore_base,
            wb
        ));
        if !merged.is_empty() && prof {
            eprintln!("  [save] 合并单元格数据到基准骨架: {:?}", merged);
        }
    }

    // 样式表原样沿用基准：umya 重建的样式表和原文件对不上（字体/填充被裁、dxfs 被压）
    if !can_bypass_umya {
        if let Some(xml) = &base_styles {
            must_save!("styles", crate::patch::append_entry(path, "xl/styles.xml", xml));
            if prof {
                eprintln!("  [save] 样式表沿用基准");
            }
        }
    }

    // 自定义文档属性：umya 解析时会把不认识的 vt:r8 之类的值写成空 <property/>，
    // Excel 会判「内容有问题」并要求修复。没有数据源改动时原样搬回基准的 custom.xml。
    if wb.sources.is_empty() && has_restore_base && !can_bypass_umya {
        if let Some(xml) = crate::fixups::read_part_file(&restore_base, "docProps/custom.xml") {
            must_save!("custom-xml", {
                crate::patch::append_entry(path, "docProps/custom.xml", &xml)
            });
        }
    }

    // 宏部件：umya 写不出来，塞回去
    if let Some(v) = vba {
        must_save!("vba", crate::vba::restore_vba(path, &v));
    }

    // umya may emit an empty cacheSource/cacheFields for an imported pivot.
    // Restore its definition even when the part still exists, then apply our
    // explicit source changes. Missing-part recovery alone cannot detect this.
    if has_restore_base {
        let mut definitions = pivot_definitions(&restore_base);
        let changes: Vec<_> = wb.pivot_cache_base.sheets.iter().filter_map(|(id, old)| {
            let new = wb.sheets.iter().find(|s| s.identity == *id).map(|s| s.name.clone());
            (new.as_ref() != Some(old)).then(|| (old.clone(), new))
        }).collect();
        for (name, body) in &wb.pivot_cache_base.definitions {
            let body = structure::rewrite_pivot_source(body, &changes)?;
            if let Some((_, old)) = definitions.iter_mut().find(|(part, _)| part == name) {
                *old = body;
            } else {
                definitions.push((name.clone(), body));
            }
        }
        if !definitions.is_empty() && (!can_bypass_umya || !changes.is_empty() || !wb.pivot_cache_base.definitions.is_empty()) {
            crate::patch::append_entries(path, &definitions).context("保留透视表缓存定义失败")?;
        }
    }

    // 数据源存成"自定义文档属性"：这是 Excel 自己的规范位置，
    // 塞进 definedNames 会和查询表打架，单独加未知部件又会被拒收
    // 即使 sources 为空也要走一次：删掉最后一个数据源时，必须把本应用的属性清掉，
    // 第三方属性保持原样。基准文件用于读回 umya 重写包时已经丢掉的自定义属性。
    if std::env::var("SC_NO_SRC_PROP").is_err() {
        must_save!(
            "sources",
            crate::external::write_sources_from(path, &wb.sources, has_restore_base.then_some(restore_base.as_path()))
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

    // umya 丢掉的部件（图表、drawing 关系、透视缓存…）从基准文件搬回来
    if has_restore_base && !can_bypass_umya {
        match crate::patch::restore_missing_parts(path, &restore_base) {
            Ok(v) if !v.is_empty() => {
                if std::env::var("SC_SAVE_PROF").is_ok() {
                    eprintln!("[save] 找回丢失部件 {} 个: {:?}", v.len(), v);
                }
            }
            _ => {}
        }
    }

    // 图表引用改写（插删行列后）
    for sheet in wb.sheets.iter() {
        if !sheet.charts_dirty {
            continue;
        }
        for (idx, ch) in sheet.charts.iter().enumerate() {
            // 拖动/缩放后的锚点写回 drawing
            if !ch.drawing.is_empty() {
                let ord = sheet.charts[..idx]
                    .iter()
                    .filter(|c| c.drawing == ch.drawing)
                    .count();
                must_save!(
                    "chart-anchor",
                    crate::chart_write::write_anchor(path, &ch.drawing, ord, ch.anchor)
                );
            }
            if ch.part.is_empty() || ch.ref_fixes.is_empty() {
                continue;
            }
            must_save!(
                "chart-refs",
                crate::chart_write::replace_refs(path, &ch.part, &ch.ref_fixes)
            );
        }
    }

    // 表格对象：umya 会丢掉 tableParts，保存后自己挂回去
    if !can_bypass_umya {
        for sheet in wb.sheets.iter() {
            if !sheet.tables.is_empty() && std::env::var("SC_NO_TABLE_ATTACH").is_err() {
                must_save!("table-attach", crate::table::attach(path, &sheet.name, &sheet.tables));
            }
        }
    }

    // 第二批内存暂存合并：将条件格式/数据验证、全套 fixups、quotePrefix、未改表恢复及外链缓存在内存一次合成落盘
    crate::patch::begin_stage_for(path);

    // umya 不管条件格式的 dxf 细节，保存完直接补 XML
    // 只要表里还有规则就重写：umya 自己写条件格式会把图标集写成数据条、
    // 数据验证也会丢输入提示，所以不能只在 dirty 时接管；
    // 若走直通复制基准（can_bypass_umya），则仅在真正有 dirty 改动时改写。
    let any_cf = wb.sheets.iter().any(|s| s.cf_dirty || (!can_bypass_umya && !s.cf.blocks.is_empty()));
    let any_dv = wb.sheets.iter().any(|s| s.dv_dirty || (!can_bypass_umya && !s.dv.is_empty()));
    let any_freeze = wb.sheets.iter().any(|s| s.freeze_dirty);
    let any_filter = wb.sheets.iter().any(|s| s.filter_dirty);
    let any_page = wb.sheets.iter().any(|s| s.page_dirty);
    let any_arr = !can_bypass_umya && wb.sheets.iter().any(|s| !s.arrays.is_empty());
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
    // 收尾：umya 会改坏 drawing rels 的图表关系，并留下旧的表格/批注部件
    if run_restore_steps && restore_base.exists() && !can_bypass_umya && std::env::var("SC_NO_FIXUPS").is_err() {
        // 工作表被挪目录时，关系要跟着搬过去
        must_save!("fixups-migrate-sheet-rels", crate::fixups::migrate_sheet_rels(
            path,
            &restore_base
        ));
        must_save!("fixups-drawing-rels", crate::fixups::fix_drawing_rels(path, &restore_base));
        must_save!("fixups-dedupe-rels", crate::fixups::dedupe_singleton_rels(path));
        must_save!("fixups-prune-orphans", crate::fixups::prune_orphans(path));
        must_save!("fixups-sheet-refs", crate::fixups::restore_sheet_refs(path, &restore_base));
        must_save!("fixups-workbook-rels", crate::fixups::migrate_workbook_rels(path, &restore_base));
        must_save!("fixups-drawing-props", crate::fixups::sanitize_drawings(path));
        must_save!("fixups-content-types", crate::fixups::ensure_content_types(path, &restore_base));
        must_save!("fixups-threaded", crate::fixups::restore_threaded_parts(path, &restore_base));
        let chart_keep: Vec<String> = wb
            .chart_edits
            .iter()
            .cloned()
            .chain(
                wb.sheets
                    .iter()
                    .flat_map(|s| s.charts.iter())
                    .filter(|c| !c.ref_fixes.is_empty())
                    .map(|c| c.part.clone()),
            )
            .collect();
        must_save!(
            "fixups-chart-parts",
            crate::fixups::restore_chart_parts(path, &restore_base, &chart_keep)
        );
    }
    // quotePrefix 是 xf 上的未建模属性，umya 重建样式表会丢；保存后按格补回
    if !can_bypass_umya && (0..wb.styles.len()).any(|i| wb.styles.quote_of(i as u16)) {
        let live_targets = crate::patch::sheet_targets(path).unwrap_or_default();
        let quote_parts: Vec<(String, Vec<(u32, u32)>)> = wb
            .sheets
            .iter()
            .filter(|sh| {
                // 未编辑且样式表原样沿用基准的表，基准 XML 里原本就有完整的 quotePrefix，无需重复扫描
                !(has_restore_base
                    && !fresh_base
                    && untouched
                    && std::env::var("SC_FAIL_SAVE_STEP").is_err()
                    && !sheet_needs_umya(sh))
            })
            .filter_map(|sh| {
                let part = live_targets
                    .get(&sh.name)
                    .cloned()
                    .or_else(|| sh.source_part.clone())?;
                let mut cells: Vec<(u32, u32)> = sh
                    .iter()
                    .filter(|(r, _)| wb.styles.quote_of(sh.style_of(r.row, r.col)))
                    .map(|(r, _)| (r.row, r.col))
                    .collect();
                cells.extend(sh.styles.iter().filter_map(|(pos, &id)| {
                    (wb.styles.quote_of(id) && !sh.contains(pos.row, pos.col)).then_some((pos.row, pos.col))
                }));
                if cells.is_empty() {
                    None
                } else {
                    Some((part, cells))
                }
            })
            .collect();
        if !quote_parts.is_empty() {
            must_save!("fixups-quote-prefix", crate::fixups::ensure_quote_prefix(path, &quote_parts));
        }
    }

    // 最后一步：没编辑过的表整段按基准恢复（放在所有补丁之后，覆盖 umya 的改写）
    if run_restore_steps && restore_base.exists() && !can_bypass_umya {
        let kept = must_save!("restore-unedited-sheets", crate::fixups::restore_unedited_sheets(
            path,
            &restore_base,
            wb
        ));
        anyhow::ensure!(omitted_sheets.iter().all(|name| kept.contains(name)),
            "无法从基准完整恢复工作表，已停止保存");
        if !kept.is_empty() && prof {
            eprintln!("  [save] 原样沿用 {} 张表: {:?}", kept.len(), kept);
        }
        restored_sheets = kept;
        // umya 会把工作表搬到 xl/worksheets/，搬回去（绘图/图表/透视表的相对路径才成立）
        let rehomed = must_save!("rehome-sheets", crate::fixups::rehome_sheets(path, &restore_base));
        if !rehomed.is_empty() && prof {
            eprintln!("  [save] 工作表放回原位 {} 张", rehomed.len());
        }
        let skeleton = must_save!("workbook-skeleton", crate::fixups::restore_workbook_skeleton(
            path,
            &restore_base,
            wb
        ));
        if skeleton && prof {
            eprintln!("  [save] 工作簿骨架沿用基准（definedNames/pivotCaches 保住）");
        }
        let rels = must_save!("base-sheet-rels", crate::fixups::restore_base_sheet_rels(
            path,
            &restore_base,
            wb
        ));
        if rels > 0 && prof {
            eprintln!("  [save] 工作表 rels 沿用基准 {rels} 份");
        }
        must_save!("fixups-content-types-2", crate::fixups::ensure_content_types(path, &restore_base));
        // 恢复后清一遍孤儿部件（基准部件另有一份同类型副本时会重复）
        must_save!("fixups-prune-orphans-2", crate::fixups::prune_orphans(path));
        if std::env::var("SC_NO_NORM_STYLES").is_err() {
        let restyled = must_save!("styles-count", crate::fixups::normalize_styles_counts(path));
        if restyled && prof {
            eprintln!("  [save] 重算 styles.xml 的 count");
        }
        }
        let added_ss = must_save!("shared-strings-rel", crate::fixups::ensure_shared_strings_rel(path));
        if added_ss && prof {
            eprintln!("  [save] 补上 sharedStrings 关系");
        }
        must_save!("content-types", crate::fixups::rebuild_content_types(path, &restore_base));
        let pruned = must_save!("prune-content-types", crate::fixups::prune_content_types(path));
        if pruned > 0 && prof {
            eprintln!("  [save] 清掉 {pruned} 条悬空内容类型");
        }
    }
    // External-link repairs can read the pending raw parts. Publish those first
    // for linked books; all other books commit both kinds of part in one pass.
    if !wb.links.is_empty() && crate::patch::raw_pending() {
        crate::patch::apply_raw_copies(path, &restore_base).context("保存子步骤 raw-copies 失败")?;
    }
    // 跨工作簿引用：先把缺的部件补齐（用户新加的工作簿），再按本簿实际引用的范围写回缓存（不把整本外部表塞进包内）
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
    // from the complete current model after all skeleton/relationship fixups,
    // which must not overwrite the new registrations. No pending work is
    // consumed: structural undo can still restore its specifications.
    if has_restore_base {
        pivots::restore_imported_parts(path, &restore_base)?;
    }
    for (si, spec, res, rect, label) in &wb.pivots_pending {
        let sheet = wb.sheets.get(*si).context("透视表所属工作表不存在")?;
        crate::pivot_xml::attach_for_session_wb(path, wb, &sheet.name, spec, res, *rect, label, wb.pivot_session.token())
            .context("透视表部件写入失败")?;
    }
    for (si, spec, grid, rect) in &wb.pivots_pending2 {
        let sheet = wb.sheets.get(*si).context("多字段透视所属工作表不存在")?;
        crate::pivot_xml::attach2_for_session_wb(path, wb, &sheet.name, spec, grid, *rect, wb.pivot_session.token())
            .context("多字段透视部件写入失败")?;
    }
    // C12.4 计算字段：紧跟在透视表部件之后写缓存定义
    if !wb.pivot_calc_fields.is_empty() {
        // 按工作表分组：多透视表共存时只写各自缓存，不再污染别人的 pivotCacheDefinition
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
    // 第三批内存暂存合并：将布局、隐藏行、分级、标签色、保护、迷你图、页面设置、shrinkToFit 与收尾校验合并一次落盘
    crate::patch::begin_stage_for(path);

    // 列宽/行高/手动隐藏的行列：只改 <cols> 和 row 属性，最后单独落一遍
    must_save!("layout", crate::patch::apply_layout(path, wb));
    // 手动隐藏行来源写成隐藏 definedName，同时被筛选
    // 遮蔽的 overlap 行也能在重开后继续保留手动身份。
    let manual_hidden: std::collections::BTreeMap<String, Vec<u32>> = wb
        .sheets
        .iter()
        .filter(|s| !s.hidden_rows_user.is_empty())
        .map(|s| {
            let mut rows: Vec<u32> = s.hidden_rows_user.iter().copied().collect();
            rows.sort_unstable();
            (s.name.clone(), rows)
        })
        .collect();
    must_save!(
        "manual-hidden",
        crate::patch::write_manual_hidden(path, &manual_hidden)
    );
    // 关闭筛选后清掉 workbook.xml 的 _FilterDatabase：这一步必须在
    // restore_workbook_skeleton 之后，否则会被基准工作簿骨架再度带回。
    for sheet in &wb.sheets {
        if sheet.filter_dirty && sheet.filter.is_none() && sheet.tables.is_empty() {
            must_save!("filter-database", crate::patch::remove_filter_database(path, &sheet.name));
        }
    }
    must_save!("outline", crate::patch::apply_outline_pr(path, wb));
    must_save!("scenarios", crate::patch::restore_scenarios(path, wb, !can_bypass_umya));
    must_save!("ignored-errors", crate::patch::apply_ignored_errors(path, wb));
    must_save!("tab-colors", crate::patch::apply_tab_colors(path, wb));
    must_save!("protection", crate::patch::apply_protection(path, wb));
    must_save!("sparkline", crate::sparkline::apply(path, wb));

    must_save!("hyperlinks", crate::hyperlinks::apply_after_save(path, wb, !can_bypass_umya));
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
    // 确保 calcMode / 迭代计算设置在 restore_workbook_skeleton 或直通复制之后始终与内存模型一致
    if let Some(wb_bytes) = crate::fixups::read_part(path, "xl/workbook.xml") {
        if let Ok(wb_xml) = String::from_utf8(wb_bytes) {
            // Preserve autoNoTable and any fullCalcOnLoad flag from the source or
            // from invalidating a calculation chain during this save.
            let mode_xml = if crate::patch::calc_manual_xml(&wb_xml) == wb.calc_manual {
                std::borrow::Cow::Borrowed(wb_xml.as_str())
            } else {
                std::borrow::Cow::Owned(crate::patch::set_calc_mode(&wb_xml, wb.calc_manual))
            };
            let next_xml = crate::patch::set_calc_iteration(
                &mode_xml,
                wb.calc_iterate,
                wb.calc_max_iterations,
                wb.calc_max_change,
            );
            let updated = if !wb.calc_manual && wb.sheets.iter().any(|s| s.has_formulas())
                && (fresh_base || wb.names_dirty || wb.sheets.iter().any(|s| s.cell_data_needs_save())) {
                delta::recalc_on_open(&next_xml,false)?.context("workbook.xml: invalid calculation properties")?
            } else { next_xml.into_bytes() };
            if updated != wb_xml.as_bytes() {
                crate::patch::append_entry(path, "xl/workbook.xml", &updated)?;
            }
        }
    }
    if !can_bypass_umya {
        must_save!("rich", crate::patch::apply_rich(path, wb));
    }
    must_save!("page-setup", crate::patch::apply_page_setup(path, wb));
    if !can_bypass_umya {
        let shrink_parts: Vec<(String, Vec<(u32, u32)>, Vec<u32>, Vec<u32>)> = wb
            .sheets
            .iter()
            .filter(|sh| {
                !(has_restore_base
                    && !fresh_base
                    && untouched
                    && std::env::var("SC_FAIL_SAVE_STEP").is_err()
                    && !sheet_needs_umya(sh))
            })
            .filter_map(|sh| {
                let cells: Vec<(u32, u32)> = sh
                    .styles
                    .iter()
                    .filter(|(_, sid)| wb.styles.get(**sid).shrink_to_fit)
                    .map(|(c, _)| (c.row, c.col))
                    .collect();
                let rows: Vec<u32> = sh
                    .row_styles
                    .iter()
                    .filter(|(_, sid)| wb.styles.get(**sid).shrink_to_fit)
                    .map(|(r, _)| *r)
                    .collect();
                let cols: Vec<u32> = sh
                    .col_styles
                    .iter()
                    .filter(|(_, sid)| wb.styles.get(**sid).shrink_to_fit)
                    .map(|(c, _)| *c)
                    .collect();
                if cells.is_empty() && rows.is_empty() && cols.is_empty() {
                    None
                } else {
                    Some((sh.name.clone(), cells, rows, cols))
                }
            })
            .collect();
        if !shrink_parts.is_empty() {
            must_save!("fixups-shrink-to-fit", crate::fixups::ensure_shrink_to_fit(path, &shrink_parts));
        }
    }
    if wb
        .sheets
        .iter()
        .any(|s| (s.notes_dirty && s.notes.is_empty()) || s.tables_dirty)
    {
        crate::patch::commit_stage(path).context("保存表/批注清理前落盘失败")?;
    }
    // 批注被删空时必须把旧部件也删掉，否则重开会从 comments 部件里复活
    for sheet in &wb.sheets {
        if sheet.notes_dirty && sheet.notes.is_empty() {
            must_save!("notes-clear", crate::notes::clear_sheet_notes(path, &sheet.name));
        }
    }
    // 表格对象：删除/转换/新建后同步 table 部件
    for sheet in &wb.sheets {
        if sheet.tables_dirty {
            must_save!("table-sync", crate::table::sync(path, &sheet.name, &sheet.tables));
        }
    }
    crate::patch::begin_stage_for(path);
    // 所有部件落盘之后再清一次悬空的 [Content_Types] Override：前面的 prune 跑在
    // staged 补丁之前，加表/改名时会出现「Content-Types 声明 /xl/worksheets/sheet2.xml，
    // 包里只有 native-sheet-…」——verify_saved_package 抓到的正是这个（Excel 会判包损坏）。
    must_save!("prune-content-types-final", crate::fixups::prune_content_types(path));
    // 输出扩展名必须和包的主类型一致：.xlsm 的基准另存成 .xlsx 时，
    // [Content_Types].xml 里仍是 macroEnabled，Excel 直接报
    // 「文件格式或文件扩展名无效」。带宏的另存 .xlsx 属于必然丢宏，拒绝。
    // save_atomic 先写 ".~name.xlsx.saving-1234" 这样的临时文件，
    // 扩展名得从临时名里还原，否则会把 .xlsm 误判成未知格式。
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
    must_save!("restore-external-rels", crate::notes::restore_external_rels(path, &external_rels));
    crate::patch::commit_stage(path).context("保存收尾补丁落盘失败")?;
    let mut created_charts = std::collections::HashMap::new();
    if fresh_base {
        for sheet in wb.sheets.iter().filter(|s| s.source_part.is_none()) {
            let new_charts: Vec<_> = sheet.charts.iter().enumerate().filter(|(_,ch)| ch.part.is_empty()).collect();
            for (_, chart) in &new_charts {
                let mut committed = (*chart).clone();
                if committed.series.iter().any(|series| !series.val_ref.is_empty()) {
                    crate::chart_write::refresh_series_from_refs(wb, &sheet.name, &mut committed)?;
                }
                crate::chart_write::insert_chart(path, &sheet.name, &committed)?;
            }
            if !new_charts.is_empty() {
                let generated = crate::charts::load_charts(path, &sheet.name)?;
                anyhow::ensure!(generated.len() == new_charts.len(), "图表写入后的关系数量不一致");
                for ((i,_), chart) in new_charts.into_iter().zip(generated) {
                    created_charts.insert((sheet.identity, i), (chart.part, chart.drawing));
                }
            }
        }
    }
    if streamed.iter().any(|&s| s) {
        if let Err(error) = fresh::write(path, wb, &streamed) {
            if error.chain().any(|e| e.is::<std::io::Error>())
                || wb.sheets.iter().zip(&streamed).any(|(s,&selected)| selected && fresh::has_dynamic(s)) {
                // The compatibility writer has no dynamic cell metadata. An
                // unsuccessful attempt must not downgrade a valid dynamic array.
                return Err(error);
            }
            // Rebuild from the original model. The incomplete shell at path must
            // never be taken as the source of a second save.
            let retry = private_workbook_dir()?;
            let output = retry.path().join(path.file_name().context("缺少保存文件名")?);
            if let Cow::Owned(book) = input {
                restore_sheet_flags(book, flags);
            }
            let write = save_plain_at(&output, input, base_override, false, flags)
                .with_context(|| format!("streaming save failed ({error:#}); rebuilding workbook"))?;
            std::fs::copy(&output,path)?;
            return Ok(write);
        }
    }
    if !preserved_grids.is_empty() || !omitted_sheets.is_empty() {
        restore_grids(path,&restore_base,&restored_sheets,base == original_base,preserved_grids)?;
    }
    Ok(CellWrite { styles: saved_styles, used_delta: patched, created_charts })
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
    for sheet in wb.sheets.iter() {
        if let Some(t) = targets.get(&sheet.name) {
            let cf = if do_cf { crate::patch::cf_blocks_xml(&sheet.cf.blocks) } else { String::new() };
            let dv = if do_dv { crate::patch::dv_xml(&sheet.dv) } else { String::new() };
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
                        crate::patch::auto_filter_xml(f)
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
                        sheet.filter_dirty,
                        sheet.page.clone(),
                        sheet.page_dirty,
                        arrays_by_sheet.get(&sheet.name).cloned().unwrap_or_default(),
                    ),
                );
            }
        }
    }
    // dxfs 必须和实际写出的 cfRule 对得上：以前只在 dirty 时取，
    // 现在只要重写条件格式就用「有 dxfs 的那张表」，否则 dxfId 会越界
    let dxfs: Vec<crate::cf::Dxf> = wb
        .sheets
        .iter()
        .find(|s| s.cf_dirty || !s.cf.dxfs.is_empty())
        .map(|s| s.cf.dxfs.clone())
        .unwrap_or_default();
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
        let slim = slim_umya_base(&base, &loaded).unwrap().unwrap();
        assert_eq!(slim.omitted, ["Large"]);
        assert!(read_part(&slim.path, &part).unwrap().len() < 4096);
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
        let slim = slim_umya_base(&path,&loaded).unwrap().unwrap();
        assert!(slim.grids["Sheet1"].edits.is_none());
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
        let slim = slim_umya_base(&path, &model).unwrap().unwrap();
        assert!(slim.grids.contains_key("Sheet1"));
        assert!(read_part(&slim.path, &part).unwrap().len() < 4096);
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

/// 列号 → 字母（0 → A）
fn col_name(mut c: u32) -> String {
    let mut out = String::new();
    loop {
        out.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    out
}

fn write_umya_style(wb: &Workbook, sid: u16, s: &mut umya_spreadsheet::Style) {
    let st = wb.styles.get(sid);
    let theme = wb.styles.theme_of(sid);
    {
        let f = s.get_font_mut();
        f.set_bold(st.bold);
        f.set_italic(st.italic);
        // 只写真正设置过的字体属性：umya 对 set_underline("none") / set_strikethrough(false)
        // 会写成 <u val="none"/> / <strike val="0"/>，set_size(0) 写 <sz val="0"/>，
        // 这三样都不合 OOXML 约束，Excel 直接拒绝打开整个文件（0x800A03EC）。
        if st.underline {
            f.set_underline(if st.double_underline { "double" } else { "single" });
        }
        if st.strikethrough {
            f.set_strikethrough(true);
        }
        if st.size >= 1.0 {
            f.set_size(st.size as f64);
        }
        // 字体名：空串表示默认字体，不碰原文件的字体（避免把 Calibri 写成别的）
        if !st.font.is_empty() {
            f.set_name(st.font.clone());
        }
        let mut col = umya_spreadsheet::Color::default();
        if let Some(t) = theme.font {
            // 主题色：保留 theme 索引与 tint，别降级成固定 RGB
            col.set_theme_index(t.idx as u32);
            col.set_tint(t.tint as f64);
        } else {
            col.set_argb_str(format!(
                "FF{:02X}{:02X}{:02X}",
                st.color[0], st.color[1], st.color[2]
            ));
        }
        f.set_color(col);
    }
    if let Some(rgb) = st.fill {
        if let Some(t) = theme.fill {
            let pf = s.get_fill_mut().get_pattern_fill_mut();
            pf.set_pattern_type(umya_spreadsheet::PatternValues::Solid);
            let c = pf.get_foreground_color_mut();
            c.set_theme_index(t.idx as u32);
            c.set_tint(t.tint as f64);
        } else {
            s.set_background_color_solid(format!(
                "FF{:02X}{:02X}{:02X}",
                rgb[0], rgb[1], rgb[2]
            ));
        }
    } else {
        // 用户明确取消填充时，基准文件里的旧 fill 必须一起清掉；
        // 只跳过设置会让 save→reload 继续显示旧颜色。
        s.remove_fill();
    }
    // 图案填充：patternType + 前景/背景各自独立，不能退化成纯色
    if let Some(p) = wb.styles.pattern_of(sid).cloned() {
        let pf = s.get_fill_mut().get_pattern_fill_mut();
        if let Ok(kind) = p.kind.parse::<umya_spreadsheet::PatternValues>() {
            pf.set_pattern_type(kind);
        }
        apply_color(pf.get_foreground_color_mut(), p.fg, p.fg_theme);
        apply_color(pf.get_background_color_mut(), p.bg, p.bg_theme);
    }
    // 对角线：style + 颜色 + 方向都要留住
    if let Some(d) = wb.styles.diag_of(sid).cloned() {
        let b = s.get_borders_mut();
        b.set_diagonal_up(d.up);
        b.set_diagonal_down(d.down);
        let bd = b.diagonal_mut();
        if let Ok(v) = d.style.parse::<umya_spreadsheet::BorderStyleValues>() {
            bd.set_style(v);
        }
        let mut dc = umya_spreadsheet::Color::default();
        apply_color(&mut dc, Some(d.color), d.color_theme);
        bd.set_color(dc);
    }
    let a = s.get_alignment_mut();
    a.set_horizontal(match st.halign {
        1 => umya_spreadsheet::HorizontalAlignmentValues::Left,
        2 => umya_spreadsheet::HorizontalAlignmentValues::Center,
        3 => umya_spreadsheet::HorizontalAlignmentValues::Right,
        4 => umya_spreadsheet::HorizontalAlignmentValues::CenterContinuous,
        5 => umya_spreadsheet::HorizontalAlignmentValues::Distributed,
        _ => umya_spreadsheet::HorizontalAlignmentValues::General,
    });
    a.set_vertical(match st.valign {
        1 => umya_spreadsheet::VerticalAlignmentValues::Top,
        2 => umya_spreadsheet::VerticalAlignmentValues::Center,
        3 => umya_spreadsheet::VerticalAlignmentValues::Bottom,
        _ => umya_spreadsheet::VerticalAlignmentValues::Bottom,
    });
    a.set_wrap_text(st.wrap);
    if st.indent > 0 {
        a.set_indent(st.indent as u32);
    }
    if st.vertical_text {
        // OOXML：textRotation=255 表示竖排（字符正立，从上到下）
        a.set_text_rotation(255);
    } else if st.rotation != 0 {
        // OOXML：0..90 逆时针，91..180 表示 -1..-90
        let v = if st.rotation >= 0 {
            st.rotation as u32
        } else {
            (90 - st.rotation) as u32
        };
        a.set_text_rotation(v);
    }
    if !st.format.is_empty() {
        let mut nf = umya_spreadsheet::NumberingFormat::default();
        nf.set_format_code(st.format.clone());
        s.set_number_format(nf);
    }
    // 边框：四个边逐条写
    let bs = s.get_borders_mut();
    let make = |b: &crate::style::Border| {
        let mut ub = umya_spreadsheet::Border::default();
        if b.style == 0 {
            ub.set_border_style("none");
        } else {
            ub.set_border_style(crate::style::Border::ooxml_name(b.style));
            let mut col = umya_spreadsheet::Color::default();
            col.set_argb_str(format!("FF{:02X}{:02X}{:02X}", b.color[0], b.color[1], b.color[2]));
            ub.set_color(col);
        }
        ub
    };
    bs.set_left(make(&st.border[0]));
    bs.set_right(make(&st.border[1]));
    bs.set_top(make(&st.border[2]));
    bs.set_bottom(make(&st.border[3]));
    // 默认锁定=true、隐藏=false；只有非默认才写 <protection>，
    // 避免给所有普通格生成无意义的 protection 节点。用户把这两个字段
    // 改回默认时，必须删掉基准文件里原有的 <protection>，否则重开仍是旧值。
    if !st.locked || st.hidden {
        let p = s.protection_mut();
        p.set_locked(st.locked);
        p.set_hidden(st.hidden);
    } else {
        s.remove_protection();
    }
}

fn apply_color_ex(
    c: &mut umya_spreadsheet::Color,
    rgb: Option<[u8; 3]>,
    theme: Option<(u8, f32)>,
    indexed: Option<u8>,
) {
    if let Some(i) = indexed {
        c.set_indexed(i as u32);
        return;
    }
    apply_color(c, rgb, theme);
}

fn apply_color(c: &mut umya_spreadsheet::Color, rgb: Option<[u8; 3]>, theme: Option<(u8, f32)>) {
    if let Some((i, t)) = theme {
        c.set_theme_index(i as u32);
        c.set_tint(t as f64);
    } else if let Some(rgb) = rgb {
        c.set_argb_str(format!("FF{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]));
    }
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
