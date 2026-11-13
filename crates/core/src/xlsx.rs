use crate::model::{Cell, Sheet, Value, Workbook};
use anyhow::{Context, Result};
use calamine::{open_workbook_auto, Data, Reader};
use std::path::Path;

/// .xls 的样式/列宽/行高/合并区（calamine 只给值）
fn apply_xls_meta(path: &Path, wb: &mut Workbook) {
    let is_xls = std::fs::read(path)
        .map(|b| !b.starts_with(b"PK"))
        .unwrap_or(false);
    if !is_xls {
        return;
    }
    if let Some(sizes) = crate::xls_layout::read(path) {
        for (i, sheet) in wb.sheets.iter_mut().enumerate() {
            let Some(sz) = sizes.sheets.get(i) else { continue };
            for (a, b, w) in sz.cols.iter() {
                if b.saturating_sub(*a) < 1024 {
                    for c in *a..=*b {
                        sheet.cols.set(c, *w);
                    }
                } else {
                    sheet.cols.set_default(*w);
                }
            }
            for (r, h) in sz.rows.iter() {
                sheet.rows.set(*r, *h);
            }
        }
    }
    if let Some(sty) = crate::xls_style::read(path) {
        let mut ids: Vec<(usize, u32, u32, u16)> = Vec::new();
        for (i, sh) in sty.sheets.iter().enumerate() {
            for (r, c, st) in sh.cells.iter() {
                ids.push((i, *r, *c, wb.styles.intern(st.clone())));
            }
        }
        for (i, r, c, id) in ids {
            if let Some(sh) = wb.sheets.get_mut(i) {
                sh.set_style(r, c, id);
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

/// 并行解析的中间结果：只存原始数据，样式留给主线程 intern
struct RawSheet {
    name: String,
    cells: Vec<((u32, u32), Cell)>,
    styles: Vec<((u32, u32), crate::style::CellStyle)>,
    formulas: Vec<((u32, u32), String)>,
    sparse: bool,
}

/// 一个线程负责一组工作表：自己开 reader，避免共享状态
fn load_chunk(
    path: &Path,
    names: &[String],
    targets: &std::collections::HashMap<String, String>,
    shared: &[String],
    limit: u64,
) -> (Vec<RawSheet>, Vec<LoadProblem>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let mut reader = match crate::xlsx::open_workbook_auto(path) {
        Ok(r) => r,
        Err(e) => {
            for n in names {
                problems.push(LoadProblem {
                    sheet: Some(n.clone()),
                    part: "workbook".into(),
                    what: "整张工作表".into(),
                    detail: format!("无法打开工作簿：{e:#}"),
                    fatal: true,
                });
            }
            return (out, problems);
        }
    };
    for name in names {
        let mut raw = RawSheet {
            name: name.clone(),
            cells: Vec::new(),
            styles: Vec::new(),
            formulas: Vec::new(),
            sparse: false,
        };
        let sparse_part = targets
            .get(name)
            .filter(|p| crate::sparse::declared_area_of(path, p) > limit);
        if let Some(part) = sparse_part {
            match crate::sparse::read_sheet(path, part, shared) {
                Ok(cells) => {
                    raw.cells = cells.into_iter().collect();
                    raw.sparse = true;
                    out.push(raw);
                    continue;
                }
                Err(e) => {
                    // 稀疏解析失败说明这份表 XML 读不完整：退回 calamine 只补数据，
                    // 不能当成"完整打开"，必须标致命（覆盖保存会丢没读到的格）。
                    eprintln!("[load] {name} 稀疏读取失败，退回 calamine: {e:#}");
                    problems.push(LoadProblem {
                        sheet: Some(name.clone()),
                        part: part.clone(),
                        what: "大表稀疏读取".into(),
                        detail: format!("{e:#}"),
                        fatal: true,
                    });
                }
            }
        }
        match reader.worksheet_range(name) {
            Err(e) => problems.push(LoadProblem {
                sheet: Some(name.clone()),
                part: targets.get(name).cloned().unwrap_or_else(|| format!("{name} 的工作表部件")),
                what: "全部单元格数据".into(),
                detail: format!("{e:#}"),
                fatal: true,
            }),
            Ok(range) => {
            let (r0, c0) = range.start().unwrap_or((0, 0));
            for (row, col, data) in range.cells() {
                let v = data_to_value(data);
                if !v.is_empty() {
                    raw.cells.push((
                        (r0 + row as u32, c0 + col as u32),
                        Cell { value: v, formula: None },
                    ));
                }
            }
            }
        }
        match reader.worksheet_style(name) {
            Err(e) => problems.push(LoadProblem {
                sheet: Some(name.clone()),
                part: targets.get(name).cloned().unwrap_or_default(),
                what: "单元格样式".into(),
                detail: format!("{e:#}"),
                fatal: true,
            }),
            Ok(st) => {
                for (row, col, style) in st.cells() {
                    raw.styles.push(((row, col), convert_style(style)));
                }
            }
        }
        match reader.worksheet_formula(name) {
            Err(e) => problems.push(LoadProblem {
                sheet: Some(name.clone()),
                part: targets.get(name).cloned().unwrap_or_default(),
                what: "全部公式".into(),
                detail: format!("{e:#}"),
                fatal: true,
            }),
            Ok(forms) => {
                let (fr0, fc0) = forms.start().unwrap_or((0, 0));
                for (row, col, f) in forms.cells() {
                    if f.is_empty() {
                        continue;
                    }
                    let text = {
                        let raw = if f.starts_with('=') { f.to_string() } else { format!("={}", f) };
                        crate::normalize_formula(&raw)
                    };
                    raw.formulas.push(((fr0 + row as u32, fc0 + col as u32), text));
                }
            }
        }
        out.push(raw);
    }
    (out, problems)
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
    let mut reader = match open_workbook_auto(path) {
        Ok(r) => r,
        Err(e) => {
            let is_cfb = std::fs::read(path)
                .map(|b| b.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]))
                .unwrap_or(false);
            if !is_cfb {
                return Err(e).with_context(|| format!("无法打开 {}", path.display()));
            }
            eprintln!("[load] calamine 打不开，改用内置 BIFF 读取器：{e}");
            set_load_kind(LOAD_KIND_BIFF_BUILTIN);
            let mut wb = crate::xls_read::load(path)?;
            apply_xls_meta(path, &mut wb);
            return Ok((wb, diag));
        }
    };
    let names = reader.sheet_names().to_owned();

    let is_cfb = std::fs::read(path)
        .map(|b| b.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]))
        .unwrap_or(false);
    let manual_hidden = crate::patch::read_manual_hidden(path);
    let mut wb = Workbook {
        load_incomplete: false,
        names: {
            let mut names = crate::patch::read_defined_names(path);
            names.retain(|(name, _, _)| name != crate::patch::MANUAL_HIDDEN_NAME);
            names
        },
        // 数据源存在 `_sc_src_*` 名称里，读出来重建
        sources: crate::external::read_sources(path),
        links: crate::xlinks::read(path),
        names_dirty: false,
        pivot_shifts: Vec::new(),
        pivot_cache_base: Default::default(),
        pivot_session: Default::default(),
        has_vba: crate::vba::has_macros(path),
        date_system: if crate::patch::read_date1904(path) {
            crate::model::DateSystem::D1904
        } else {
            crate::model::DateSystem::D1900
        },
        pivots_pending: Vec::new(),
        calc_manual: crate::patch::read_calc_manual(path),
        calc_pending: false,
        calc_pending_sheets: std::collections::BTreeSet::new(),
        sheets: Vec::new(),
        path: Some(path.to_path_buf()),
        decrypted: None,
        encryption: None,
        dirty: false,
        styles: crate::style::StyleTable::new(),
    };

    // 稀疏表旁路：声明区域大到离谱的表（有人在 A1048576/XFD1048576 留了格子），
    // calamine 会按稠密网格分配内存，直接 OOM
    let targets = crate::patch::sheet_targets(path).unwrap_or_default();
    let t_shared = std::time::Instant::now();
    // 先判断容器格式：BIFF(.xls) 不是 zip，下面所有 OOXML 扫描器都不能跑
    let is_ooxml = std::fs::read(path)
        .map(|b| b.starts_with(b"PK"))
        .unwrap_or(false);
    set_load_kind(if is_ooxml { LOAD_KIND_OOXML } else { LOAD_KIND_BIFF_CALAMINE });
    let shared = match if is_ooxml {
        crate::sparse::shared_strings_checked(path)
    } else {
        Ok(Vec::new())
    } {
        Ok(v) => v,
        Err(e) => {
            // 缺 SST 只在真的有人引用 `t="s"` 时才算问题；没人引用时是正常的。
            if crate::sparse::any_sheet_uses_shared_strings(path) {
                diag.push(
                    None,
                    "xl/sharedStrings.xml",
                    "共享字符串表",
                    format!("{e:#}（有单元格按共享字符串引用）"),
                    true,
                );
            }
            Vec::new()
        }
    };
    if std::env::var("SC_LOAD_PROF").is_ok() {
        eprintln!("[load] 共享字符串 {} 条 {:.2}s", shared.len(), t_shared.elapsed().as_secs_f64());
    }

    let prof_load = std::env::var("SC_LOAD_PROF").is_ok();
    let t_all_load = std::time::Instant::now();
    // 版面/合并/数组公式/图片/表格这几趟扫描跟单元格数据无关，
    // 和下面的逐表解析并行跑，省掉串行等待
    let (side_tx, side_rx) = std::sync::mpsc::channel();
    if is_ooxml {
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            if injected(&INJECT_SIDE_PANIC, &p) {
                panic!("注入：旁路扫描线程 panic");
            }
            let out = (
                crate::arr::scan(&p),
                crate::merges::load_layout(&p),
                crate::pic::load(&p),
                crate::table::load(&p),
            );
            let _ = side_tx.send(out);
        });
    }
    let t_all = std::time::Instant::now();
    // 逐表解析并行：每线程自己开一个 reader，样式回主线程再 intern
    let limit = std::env::var("SC_DENSE_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(crate::sparse::DENSE_LIMIT);
    let nthreads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 6)
        .min(names.len().max(1));
    let chunk = names.len().div_ceil(nthreads).max(1);
    let (raws, chunked): (Vec<RawSheet>, Vec<LoadProblem>) = std::thread::scope(|sc| {
        let mut handles = Vec::new();
        for t in 0..nthreads {
            let lo = t * chunk;
            let hi = ((t + 1) * chunk).min(names.len());
            if lo >= hi {
                continue;
            }
            let slice = &names[lo..hi];
            let p = path.to_path_buf();
            let targets_ref = &targets;
            let shared_ref = &shared;
            handles.push(sc.spawn(move || load_chunk(&p, slice, targets_ref, shared_ref, limit)));
        }
        let mut all = Vec::new();
        let mut probs = Vec::new();
        for h in handles {
            match h.join() {
                Ok((v, p)) => {
                    all.extend(v);
                    probs.extend(p);
                }
                Err(_) => {
                    eprintln!("[load] 并行解析线程异常退出");
                    probs.push(LoadProblem {
                        sheet: None,
                        part: "xl/worksheets".into(),
                        what: "一组工作表的全部内容".into(),
                        detail: "解析线程异常退出（panic）".into(),
                        fatal: true,
                    });
                }
            }
        }
        (all, probs)
    });
    diag.problems.extend(chunked);
    // 声明了但没解析出来的表：宁可报错，也不能当空表打开
    for name in &names {
        if !raws.iter().any(|r| &r.name == name) {
            diag.problems.push(LoadProblem {
                sheet: Some(name.clone()),
                part: targets.get(name).cloned().unwrap_or_default(),
                what: "整张工作表".into(),
                detail: "workbook.xml 里声明了，但没有解析结果".into(),
                fatal: true,
            });
        }
    }

    // .xls 不是 zip：这条只给 ooXML 用
    let is_ooxml = std::fs::read(path)
        .map(|b| b.starts_with(b"PK"))
        .unwrap_or(false);
    let mut shared_zip = if is_ooxml {
        Some(zip::ZipArchive::new(std::fs::File::open(path)?)?)
    } else {
        None
    };
    // 主题色：Excel 的 <color theme tint> 不能只当 RGB，读盘时记下索引与 tint
    let theme_parts: Vec<String> = targets.values().cloned().collect();
    let theme_book = if is_ooxml {
        crate::style_theme::load(path, &theme_parts)
    } else {
        Default::default()
    };
    for raw in raws {
        let t_sheet = std::time::Instant::now();
        let mut sheet = Sheet::new(raw.name.clone());
        sheet.source_part = targets.get(&raw.name).cloned();
        sheet.loading = true;
        for ((r, c), cell) in raw.cells {
            sheet.set(r, c, cell);
        }
        let xf_map = targets
            .get(&raw.name)
            .and_then(|p| theme_book.cells.get(p));
        for ((r, c), mut cs) in raw.styles {
            let xf = xf_map
                .and_then(|m| m.get(&(r, c)))
                .and_then(|idx| theme_book.xfs.get(*idx as usize))
                .copied();
            crate::style_theme::apply(&mut cs, xf.as_ref(), &theme_book.palette);
            let id = wb.styles.intern(cs);
            if let Some(x) = xf {
                wb.styles.set_theme(id, x);
            }
            if id != 0 {
                sheet.set_style_loaded(r, c, id);
            }
        }
        for ((r, c), text) in raw.formulas {
            let mut cell = sheet.get(r, c).cloned().unwrap_or_default();
            cell.formula = Some(text.into());
            sheet.set(r, c, cell);
        }
        // 共享公式：解析库给子格的是主格原文，没按位移改，这里按 si 组重新推
        if !raw.sparse {
            if let Some(part) = targets.get(&raw.name) {
                let opened = shared_zip
                    .as_mut()
                    .map(|z| z.by_name(part))
                    .unwrap_or_else(|| Err(zip::result::ZipError::FileNotFound));
                if let Ok(mut f) = opened {
                    let mut xml = String::new();
                    if std::io::Read::read_to_string(&mut f, &mut xml).is_ok() {
                        for (r, c, text) in crate::sparse::fix_shared_children(&xml) {
                            let mut cell = sheet.get(r, c).cloned().unwrap_or_default();
                            cell.formula = Some(crate::normalize_formula(&text).into());
                            sheet.set(r, c, cell);
                        }
                    }
                }
            }
        }
        sheet.loading = false;
        sheet.edited = false;
        if prof_load {
            eprintln!(
                "[load] {:<16} {:>7} 格  {}{}",
                sheet.name,
                sheet.iter().count(),
                "",
                if raw.sparse { "（稀疏）" } else { "" }
            );
        }
        if prof_load {
            eprintln!("[load]   {} 物化 {:.2}s", sheet.name, t_sheet.elapsed().as_secs_f64());
        }
        wb.sheets.push(sheet);
    }
    if prof_load {
        eprintln!("[load] 物化合计 {:.2}s（并行解析已完成）", t_all.elapsed().as_secs_f64());
    }

    let t_side = std::time::Instant::now();
    let (found, layouts_raw, pics, tables) = if !is_ooxml {
        (Default::default(), Ok(Default::default()), Default::default(), Default::default())
    } else { match side_rx.recv() {
        Ok(v) => v,
        Err(_) => {
            diag.push(
                None,
                "xl/worksheets（版面扫描）",
                "合并单元格/数组公式/图片/表格",
                "旁路扫描线程异常退出，这些结构没有加载".to_string(),
                true,
            );
            (Default::default(), Ok(Default::default()), Default::default(), Default::default())
        }
    } };
    let layouts = match layouts_raw {
        Ok(v) => v,
        Err(e) => {
            diag.push(
                None,
                "xl/worksheets（版面扫描）",
                "合并单元格/版面/数组公式",
                format!("{e:#}"),
                true,
            );
            Default::default()
        }
    };
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
    // 数组公式：calamine 只给公式文本，t="array"/ref 得从原始 XML 拿（已并行取到）
    {
        {
            for sheet in wb.sheets.iter_mut() {
                if let Some(list) = found.get(&sheet.name) {
                    for a in list {
                        sheet.arrays.push((
                            crate::model::CellRef::new(a.anchor.0, a.anchor.1),
                            a.range,
                        ));
                    }
                }
            }
        }
    }
    apply_xls_meta(path, &mut wb);
    if is_cfb {
        wb.calc_manual = crate::xls_read::read_calc_manual(path);
    }

    // 合并单元格（已并行取到）
    {
        for sheet in wb.sheets.iter_mut() {
            if let Some(lay) = layouts.get(&sheet.name) {
                // .xls 没有 XML 版面，合并区来自 apply_xls_meta，别被空值覆盖
                if !lay.merges.is_empty() || !is_cfb {
                    sheet.merges = lay.merges.clone();
                }
                if let Some(w) = lay.col_default {
                    sheet.cols.set_default(w);
                }
                for (c, w) in lay.col_widths.iter() {
                    sheet.cols.set(*c, *w);
                }
                for (r, h) in lay.row_heights.iter() {
                    sheet.rows.set(*r, *h);
                }
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
                if !lay.scenarios.is_empty() {
                    sheet.scenarios = lay.scenarios.clone();
                }
                if let Some((below, right, symbols)) = lay.outline_pr {
                    sheet.outline_summary_below = below;
                    sheet.outline_summary_right = right;
                    sheet.outline_symbols = symbols;
                }
                for (c, lv) in lay.col_levels.iter() {
                    sheet.col_levels.insert(*c, *lv);
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
                    sheet.hidden_rows = fh;
                }
                if let Some(map) = &manual_hidden {
                    // 本应用保存的手动隐藏行来源：它明确区分了
                    // 筛选隐藏与手动隐藏，包括两者重叠的行，不再靠猜 hidden 。
                    sheet.hidden_rows_user = map.get(&sheet.name).cloned().unwrap_or_default();
                }
            }
        }
    }

    if prof_load {
        eprintln!("[load] 总计 {:.2}s", t_all_load.elapsed().as_secs_f64());
    }
    // 页面设置（表级 + 定义名里的打印区域/标题）
    {
        let names = crate::page::load_defined_names(path);
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut zip) = zip::ZipArchive::new(file) {
                let targets = crate::patch::sheet_targets(path).unwrap_or_default();
                for (idx, sheet) in wb.sheets.iter_mut().enumerate() {
                    let mut ps = targets
                        .get(&sheet.name)
                        .and_then(|t| {
                            use std::io::Read;
                            let mut f = zip.by_name(t).ok()?;
                            let mut xml = String::new();
                            f.read_to_string(&mut xml).ok()?;
                            Some(crate::page::parse_page_setup(&xml))
                        })
                        .unwrap_or_default();
                    if let Some((a, r, c)) = names.get(&(idx as u32)) {
                        ps.print_area = *a;
                        ps.title_rows = *r;
                        ps.title_cols = *c;
                    }
                    sheet.page = ps;
                }
            }
        }
    }

    // 图表：drawing 是 OOXML 结构，BIFF(.xls) 没有，别拿 zip 扫描器去读
    let charts_res = if is_ooxml {
        crate::charts::load_charts_all(path, &wb.sheets)
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

    // 超链接：工作表 <hyperlinks> + rels
    if is_ooxml {
        if let Ok(targets) = crate::patch::sheet_targets(path) {
            for sheet in wb.sheets.iter_mut() {
                if let Some(part) = targets.get(&sheet.name) {
                    if let Some(bytes) = crate::fixups::read_part_file(path, part) {
                        if let Ok(xml) = String::from_utf8(bytes) {
                            let rels = crate::hyperlinks::read_rels(path, part);
                            for raw in crate::hyperlinks::parse_sheet(&xml) {
                                let (target, location, external) = match &raw.rid {
                                    Some(id) => match rels.get(id) {
                                        Some((t, ext)) => (t.clone(), String::new(), *ext),
                                        None => (String::new(), String::new(), false),
                                    },
                                    None => (
                                        String::new(),
                                        raw.location.clone().unwrap_or_default(),
                                        false,
                                    ),
                                };
                                if target.is_empty() && location.is_empty() {
                                    continue;
                                }
                                sheet.hyperlinks.insert(
                                    (raw.r, raw.c),
                                    crate::hyperlinks::Hyperlink {
                                        target,
                                        location,
                                        tooltip: raw.tooltip.unwrap_or_default(),
                                        external,
                                    },
                                );
                            }
                        }
                    }
                    // 保存时 umya/修补链不保证原样保留 hyperlinks，载入即标记重写
                    if !sheet.hyperlinks.is_empty() {
                        sheet.hyperlinks_dirty = true;
                    }
                    // 文本框：drawing 部件里的 xdr:sp
                    let tbs = crate::textbox::load(path, part);
                    if !tbs.is_empty() {
                        sheet.textboxes = tbs;
                        sheet.textboxes_dirty = true;
                    }
                }
            }
        }
    }
    // inlineStr 富文本（我们写出的文件；Excel 的 sharedStrings 富文本仍是纯文本）
    if is_ooxml {
        // 迷你图：工作表 extLst 里的 x14:sparklineGroups
        if let Ok(targets) = crate::patch::sheet_targets(path) {
            for sheet in wb.sheets.iter_mut() {
                if let Some(part) = targets.get(&sheet.name) {
                    if let Some(bytes) = crate::fixups::read_part_file(path, part) {
                        if let Ok(xml) = String::from_utf8(bytes) {
                            let mut list = crate::sparkline::parse_sheet(&xml);
                            if !list.is_empty() {
                                // 渲染层按 (列, 行) 二分查找，这里保证有序
                                list.sort_by_key(|s| (s.c, s.r));
                                sheet.sparklines = list;
                                // 模型外的属性不保证原样保留，载入即标记重写
                                sheet.sparklines_dirty = true;
                            }
                        }
                    }
                }
            }
        }
        if let Ok(targets) = crate::patch::sheet_targets(path) {
            for sheet in wb.sheets.iter_mut() {
                if let Some(part) = targets.get(&sheet.name) {
                    if let Some(bytes) = crate::fixups::read_part_file(path, part) {
                        if let Ok(xml) = String::from_utf8(bytes) {
                            let rich = crate::richtext::inline_cells(&xml);
                            if !rich.is_empty() {
                                sheet.rich = rich;
                            }
                        }
                    }
                }
            }
        }
        // Excel 写在 sharedStrings.xml 里的富文本：按整段文字回填
        if let Some(bytes) = crate::fixups::read_part_file(path, "xl/sharedStrings.xml") {
            if let Ok(xml) = String::from_utf8(bytes) {
                let table = crate::richtext::sst_runs(&xml);
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
        let states = crate::patch::read_sheet_states(path);
        for sheet in wb.sheets.iter_mut() {
            if let Some(&hide) = states.get(&sheet.name) {
                sheet.hidden = hide;
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
        for (cref, cell) in sheet.iter() {
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
    sheets: Vec<(u64, String)>,
    /// 保存时已经把图表的引用改写落盘，回执应用后要清空写回日志，
    /// 否则下一次保存会拿同一批 (旧→新) 再改一遍（撤销后再保存就会来回跳）。
    committed_charts: bool,
}

struct RestoreFile(Option<std::path::PathBuf>);
impl Drop for RestoreFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 { let _ = std::fs::remove_file(path); }
    }
}

impl SaveReceipt {
    pub fn apply_to(&self, wb: &mut Workbook) {
        for sheet in &mut wb.sheets {
            if let Some((_, part)) = self.sheets.iter().find(|(id, _)| *id == sheet.identity) {
                sheet.source_part = Some(part.clone());
            }
            if self.committed_charts && sheet.charts_dirty {
                for ch in sheet.charts.iter_mut() {
                    ch.ref_fixes.clear();
                }
                sheet.charts_dirty = false;
            }
        }
    }
}

/// Save a snapshot, then acknowledge only its sheet identities in the live
/// workbook. Callers retaining the workbook should apply the returned receipt.
pub fn save_atomic_with_receipt(path: &Path, wb: &Workbook) -> Result<SaveReceipt> {
    refuse_overwrite_of_source(wb, path)?;
    save_atomic(path, wb)?;
    if path.extension().is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("xls")) {
        return Ok(SaveReceipt::default());
    }
    let targets = crate::patch::sheet_targets(path).context("读取已保存的工作表身份失败")?;
    let sheets = wb.sheets.iter().map(|sheet| {
        let part = targets.get(&sheet.name)
            .with_context(|| format!("已保存文件缺少工作表: {}", sheet.name))?;
        Ok((sheet.identity, part.clone()))
    }).collect::<Result<Vec<_>>>()?;
    Ok(SaveReceipt {
        sheets,
        committed_charts: wb.sheets.iter().any(|s| s.charts_dirty),
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
    // 不能靠扩展名判断类型：调用方传进来的常常是临时文件（.~a.xls.saving-1234），
    // 它的扩展名是 saving-1234。按文件头判断才可靠。
    let mut head = [0u8; 8];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(path).with_context(|| format!("打开保存产物失败：{}", path.display()))?;
        let n = f.read(&mut head).with_context(|| format!("读取保存产物失败：{}", path.display()))?;
        if n < 8 {
            anyhow::bail!("保存产物只有 {n} 字节，明显被截断");
        }
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
    let file = std::fs::File::open(path).with_context(|| format!("打开保存产物失败：{}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("保存产物不是有效的 ZIP（中央目录缺失或文件被截断）")?;
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.name_for_index(i).map(|s| s.to_string()).unwrap_or_default())
        .collect();
    for must in ["[Content_Types].xml", "_rels/.rels", "xl/workbook.xml", "xl/_rels/workbook.xml.rels"] {
        if !names.iter().any(|n| n == must) {
            anyhow::bail!("保存产物缺少必需部件 {must}");
        }
    }
    for i in 0..zip.len() {
        let mut e = zip.by_index(i).with_context(|| format!("读取保存产物第 {i} 个条目失败"))?;
        let name = e.name().to_string();
        let mut sink = std::io::sink();
        std::io::copy(&mut e, &mut sink).with_context(|| format!("保存产物条目损坏：{name}"))?;
    }
    for part in ["[Content_Types].xml", "_rels/.rels", "xl/workbook.xml", "xl/_rels/workbook.xml.rels"] {
        let body = read_zip_text(&mut zip, part)?;
        check_single_root(part, &body)?;
    }
    // 必需结构：Content-Types 至少要有 workbook 与 worksheet 的类型；
    // workbook.xml 至少要有一个 <sheet> 且带 name/r:id —— 空 XML、多根、只有壳子都要拦。
    let types = read_zip_text(&mut zip, "[Content_Types].xml")?;
    // 工作簿主部件的类型随格式不同：xlsx 是 sheet.main+xml，xlsm 是 macroEnabled.main+xml，
    // xltx/xltm 又是 template.main+xml。统一按「/xl/workbook.xml 的 Override 以 main+xml 结尾」判断。
    if !override_is_workbook_main(&types) {
        anyhow::bail!("[Content_Types].xml 里没有工作簿主部件（/xl/workbook.xml）的类型声明");
    }
    let wb_xml = read_zip_text(&mut zip, "xl/workbook.xml")?;
    let sheets = count_elements(&wb_xml, "sheet");
    if sheets == 0 {
        anyhow::bail!("workbook.xml 里没有任何 <sheet>");
    }
    for part in override_part_names(&types) {
        let want = part.trim_start_matches('/').to_string();
        if !names.iter().any(|n| *n == want) {
            anyhow::bail!("[Content_Types].xml 声明了不存在的部件 {part}");
        }
    }
    let rels = read_zip_text(&mut zip, "xl/_rels/workbook.xml.rels")?;
    for target in relationship_targets(&rels) {
        if target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:") {
            continue;
        }
        // 关系目标相对 xl/workbook.xml 解析：'/xl/...' 是包内绝对路径，'../' 要真的向上走，
        // 不能像以前那样遇到 '../' 直接跳过（等于漏检）。
        let Some(resolved) = resolve_part("xl", &target) else {
            anyhow::bail!("workbook.xml.rels 的目标越出包根：{target}");
        };
        if !names.iter().any(|n| *n == resolved) {
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
    let mut reader = quick_xml::Reader::from_str(body);
    reader.config_mut().check_end_names = true;
    let mut depth: i32 = 0;
    let mut roots = 0;
    let mut closed = false;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(_)) => {
                if closed {
                    anyhow::bail!("{part} 在根元素之后又出现了元素（多个根）");
                }
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
            }
            Ok(quick_xml::events::Event::Empty(_)) => {
                if closed {
                    anyhow::bail!("{part} 在根元素之后又出现了元素（多个根）");
                }
                // 自闭合元素既不进也不出栈；在顶层时它自己就是一个根。
                if depth == 0 {
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

fn read_zip_text(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String> {
    let mut body = String::new();
    let mut e = zip.by_name(name).with_context(|| format!("缺少部件 {name}"))?;
    std::io::Read::read_to_string(&mut e, &mut body).with_context(|| format!("读取 {name} 失败"))?;
    Ok(body)
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
    refuse_overwrite_of_source(wb, path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook.xlsx".into());
    let tmp = dir.join(format!(".~{name}.saving-{}", std::process::id()));
    // 覆盖前把旧版本留一份（第一次保存时什么都不做）
    crate::backup::keep_previous(path)
        .with_context(|| format!("无法备份旧版本，未覆盖 {}", path.display()))?;
    let is_xls = path.extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("xls"));
    let written = if is_xls {
        crate::xls_write::write(&tmp, wb)
    } else {
        save(&tmp, wb)
    };
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // 验收专用：SC_SAVE_PAUSE_MS 让「临时文件已写好、还没替换」的窗口可控制，
    // 用于模拟保存过程中进程被强杀（不是物理断电）。未设置时零影响。
    if let Ok(ms) = std::env::var("SC_SAVE_PAUSE_MS") {
        if let Ok(ms) = ms.trim().parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
    // 写成功 ≠ 产物完整：临时文件先过完整性门禁，不通过不许覆盖原文件
    corrupt_tmp_for_test(&tmp);
    if let Err(e) = verify_saved_package(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context(format!("保存产物完整性校验失败，未覆盖 {}", path.display())));
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // 目标被 Office 打开时 rename 会失败：留下临时文件并如实报错
            let keep = tmp.display().to_string();
            Err(anyhow::anyhow!(
                "写入临时文件成功，但替换 {} 失败（{}）；新内容留在 {}",
                path.display(),
                e,
                keep
            ))
        }
    }
}

/// 保存：原文件带打开密码时，先写明文到临时文件，再加密回目标路径。
pub fn save(path: &Path, wb: &Workbook) -> Result<()> {
    refuse_overwrite_of_source(wb, path)?;
    let Some(enc) = wb.encryption.as_ref() else {
        return save_plain(path, wb);
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".~plain-{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    // 基准必须是明文：加密文件不能直接给 umya 读。用完立即删。
    let base = if let Some(src) = wb.path.as_ref().filter(|p| p.exists()) {
        let bytes = crate::crypto::decrypt(src, &enc.password)?;
        let b = dir.join(format!(".~base-{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&b);
        std::fs::write(&b, &bytes).with_context(|| "写临时明文基准失败")?;
        Some(b)
    } else {
        None
    };
    let res = save_plain_base(&tmp, wb, base.as_deref());
    if let Some(b) = base.as_ref() {
        let _ = std::fs::remove_file(b);
    }
    let out = res.and_then(|()| {
        corrupt_tmp_for_test(&tmp);
        verify_saved_package(&tmp).context("加密前的临时明文未通过完整性校验")?;
        let bytes = std::fs::read(&tmp).with_context(|| "读取临时明文失败")?;
        crate::crypto::encrypt(path, &bytes, enc)
    });
    let _ = std::fs::remove_file(&tmp);
    out
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
    let tmp = std::env::temp_dir().join(format!(
        "sheets-native-dec-{}-{}.xlsx",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, &plain).with_context(|| "写入解密临时文件失败")?;
    let (mut wb, diag) = match load_diag(&tmp) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let _ = std::fs::remove_file(&tmp);
    wb.path = Some(path.to_path_buf());
    wb.decrypted = None;
    wb.encryption = Some(crate::crypto::EncryptionSettings::new(password));
    wb.load_incomplete = diag.has_fatal();
    Ok((wb, diag))
}

fn save_plain(path: &Path, wb: &Workbook) -> Result<()> {
    save_plain_base(path, wb, None)
}

thread_local! {
    /// 当前线程正在写的目标文件：让故障注入能只作用于某一次保存。
    /// 不记录的话，并行跑的其它保存会跟着一起失败（测试互相污染）。
    static SAVE_TARGET: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn note_save_target(path: &Path) {
    let p = path.display().to_string();
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

/// 打开不完整的簿子不许把结果写回它的来源文件。
/// 放在每个公开保存入口的最前面：另存/原子保存都会先写临时文件，
/// 只在底层比较临时路径会被整条绕过。
fn refuse_overwrite_of_source(wb: &Workbook, target: &Path) -> Result<()> {
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

fn save_plain_base(path: &Path, wb: &Workbook, base_override: Option<&Path>) -> Result<()> {
    note_save_target(path);
    // 先把被清空的单元格从文件里删掉（见 Sheet::cleared）
    use umya_spreadsheet::{new_file, reader::xlsx::read, writer::xlsx::write};

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
    // 样式表在写之前先留一份：base 可能就是目标文件，写完再读就变味了
    let base_styles: Option<Vec<u8>> = if restore_base.exists() {
        crate::fixups::read_part(&restore_base, "xl/styles.xml")
    } else {
        None
    };
    // 没有可用的基准（抢救副本 / 全新文件）：整份内容只能从模型重建，
    // 做过载入的表也要把样式、合并、批注等写出来，不能等 dirty 标记。
    let fresh_base = !base.exists();
    // 抢救副本从零建包：以后所有补丁都要按"全部脏"跑一遍，
    // 否则已加载但没编辑过的列宽/行高/保护/页面设置/批注会静默丢掉。
    let forced_snapshot;
    let wb: &Workbook = if fresh_base {
        let mut copy = wb.clone();
        for sh in copy.sheets.iter_mut() {
            sh.mark_all_dirty();
        }
        forced_snapshot = copy;
        &forced_snapshot
    } else {
        wb
    };
    // 外链关系的原文（URL/TargetMode）：有的引擎会把 URL 当包内路径改写
    let external_rels = crate::notes::capture_external_rels(if base.exists() { &base } else { path });
    let mut book = if base.exists() {
        read(&base).with_context(|| format!("读取现有文件失败 {}", base.display()))?
    } else {
        new_file()
    };

    // 日期体系：base 是原文件（多带同一个 flag），新建文件要显式写
    if book.date1904() != wb.date_system.is_1904() {
        book.set_date1904(wb.date_system.is_1904());
    }

    if prof {
        eprintln!("  [save] 读入原文件 {:.2}s", t_read.elapsed().as_secs_f64());
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
        }

        let ws = book
            .get_sheet_by_name_mut(&sheet.name)
            .with_context(|| format!("找不到表 {}", sheet.name))?;

        for cref in sheet.cleared.iter() {
            ws.remove_cell((cref.col + 1, cref.row + 1));
        }

        // 样式：正常保存只回写用户改过的格子（整表重写样式在 10 万格级别会拖慢保存）；
        // 抢救副本没有基准，整表有样式的格都要写，否则完好表的格式会丢。
        let style_targets: Vec<(u32, u32)> = if fresh_base {
            // 从样式表枚举：空白但有填充/边框的格不在 sheet.iter() 里，漏了就丢格式
            sheet
                .styles
                .iter()
                .filter(|(_, id)| **id != 0)
                .map(|(c, _)| (c.row, c.col))
                .collect()
        } else {
            sheet.style_edits.iter().map(|c| (c.row, c.col)).collect()
        };
        for (row, col) in style_targets {
            let sid = sheet.style_of(row, col);
            let st = wb.styles.get(sid);
            let theme = wb.styles.theme_of(sid);
            let cell = ws.get_cell_mut((col + 1, row + 1));
            let s = cell.get_style_mut();
            {
                let f = s.get_font_mut();
                f.set_bold(st.bold);
                f.set_italic(st.italic);
                // 只写真正设置过的字体属性：umya 对 set_underline("none") / set_strikethrough(false)
                // 会写成 <u val="none"/> / <strike val="0"/>，set_size(0) 写 <sz val="0"/>，
                // 这三样都不合 OOXML 约束，Excel 直接拒绝打开整个文件（0x800A03EC）。
                if st.underline {
                    f.set_underline("single");
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
            let a = s.get_alignment_mut();
            a.set_horizontal(match st.halign {
                1 => umya_spreadsheet::HorizontalAlignmentValues::Left,
                2 => umya_spreadsheet::HorizontalAlignmentValues::Center,
                3 => umya_spreadsheet::HorizontalAlignmentValues::Right,
                _ => umya_spreadsheet::HorizontalAlignmentValues::General,
            });
            a.set_vertical(match st.valign {
                1 => umya_spreadsheet::VerticalAlignmentValues::Top,
                2 => umya_spreadsheet::VerticalAlignmentValues::Center,
                3 => umya_spreadsheet::VerticalAlignmentValues::Bottom,
                _ => umya_spreadsheet::VerticalAlignmentValues::Bottom,
            });
            if st.indent > 0 {
                a.set_indent(st.indent as u32);
            }
            if st.rotation != 0 {
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
            let sides = ["thin", "medium", "thick", "double", "dashed", "dotted"];
            let bs = s.get_borders_mut();
            let mut make = |b: &crate::style::Border| {
                let mut ub = umya_spreadsheet::Border::default();
                if b.style == 0 {
                    ub.set_border_style("none");
                } else {
                    ub.set_border_style(*sides.get(b.style as usize - 1).unwrap_or(&"thin"));
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

        for (cref, cell) in sheet.iter() {
            // 动态数组的溢出值不落盘：文件里只保存锚点公式，打开时重新计算
            if sheet.spilled.contains(&cref) {
                // A moved spill may cover a previously persisted ordinary cell.
                // Its old value must not become a blocker when reopening.
                ws.remove_cell((cref.col + 1, cref.row + 1));
                continue;
            }
            let coord = (cref.col + 1, cref.row + 1);
            if let Some(f) = &cell.formula {
                // OOXML 里 <f> 不带等号；内部统一带，写盘时去掉
                let text = f.strip_prefix('=').unwrap_or(f);
                let target = ws.get_cell_mut(coord);
                target.set_formula(text.to_string());
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
                    ws.get_cell_mut(coord).set_value_string(t.clone());
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
            // 整表重写：先清空再从模型里灌（批注数量通常很小）
            ws.get_comments_mut().clear();
            for n in sheet.notes.values() {
                let mut nc = umya_spreadsheet::Comment::default();
                let mut coord = umya_spreadsheet::Coordinate::default();
                coord.set_coordinate(format!("{}{}", crate::model::col_label(n.c), n.r + 1));
                *nc.coordinate_mut() = coord;
                nc.set_author(if n.author.is_empty() { "Hermes" } else { &n.author });
                nc.set_text_string(n.text.clone());
                ws.add_comments(nc);
            }
            let _ = idx;
        }
    }

    if prof {
        eprintln!("  [save] 组装 DOM {:.2}s", t_build.elapsed().as_secs_f64());
    }
    let t_write = std::time::Instant::now();
    let out = write(&book, path).with_context(|| format!("写入失败 {}", path.display()));
    if prof {
        eprintln!("  [save] 序列化+压缩 {:.2}s", t_write.elapsed().as_secs_f64());
    }
    out?;

    // 只把 sheetData 换进去，工作表骨架沿用基准
    if restore_base.exists() {
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
    let untouched = wb.sheets.iter().all(|s| !s.styles_dirty);
    if let Some(xml) = base_styles.as_ref().filter(|_| untouched) {
        must_save!("styles", crate::patch::append_entry(path, "xl/styles.xml", xml));
        if prof {
            eprintln!("  [save] 样式表沿用基准");
        }
    }

    // 自定义文档属性：umya 解析时会把不认识的 vt:r8 之类的值写成空 <property/>，
    // Excel 会判「内容有问题」并要求修复。没有数据源改动时原样搬回基准的 custom.xml。
    if wb.sources.is_empty() {
        if let Some(xml) = crate::fixups::read_part(&restore_base, "docProps/custom.xml") {
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
    if restore_base.exists() {
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
        if !definitions.is_empty() {
            crate::patch::append_entries(path, &definitions).context("保留透视表缓存定义失败")?;
        }
    }

    // 透视表缓存里的源区域
    if !wb.pivot_shifts.is_empty() {
        must_save!("pivot-shifts", crate::patch::shift_pivot_caches(path, &wb.pivot_shifts));
    }

    // 自定义名称
    if wb.names_dirty {
        crate::patch::write_user_names(path, &wb.names).context("名称回写失败")?;
    }
    // 数据源存成"自定义文档属性"：这是 Excel 自己的规范位置，
    // 塞进 definedNames 会和查询表打架，单独加未知部件又会被拒收
    // 即使 sources 为空也要走一次：删掉最后一个数据源时，必须把本应用的属性清掉，
    // 第三方属性保持原样。基准文件用于读回 umya 重写包时已经丢掉的自定义属性。
    if std::env::var("SC_NO_SRC_PROP").is_err() {
        must_save!(
            "sources",
            crate::external::write_sources_from(path, &wb.sources, Some(&restore_base))
        );
    }

    // umya 丢掉的部件（图表、drawing 关系、透视缓存…）从基准文件搬回来
    if restore_base.exists() {
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
    for sheet in wb.sheets.iter() {
        if !sheet.tables.is_empty() && std::env::var("SC_NO_TABLE_ATTACH").is_err() {
            must_save!("table-attach", crate::table::attach(path, &sheet.name, &sheet.tables));
        }
    }
    // umya 不管条件格式的 dxf 细节，保存完直接补 XML
    // 只要表里还有规则就重写：umya 自己写条件格式会把图标集写成数据条、
    // 数据验证也会丢输入提示，所以不能只在 dirty 时接管
    let any_cf = wb.sheets.iter().any(|s| s.cf_dirty || !s.cf.blocks.is_empty());
    let any_dv = wb.sheets.iter().any(|s| s.dv_dirty || !s.dv.is_empty());
    let any_freeze = wb.sheets.iter().any(|s| s.freeze_dirty);
    let any_filter = wb.sheets.iter().any(|s| s.filter_dirty);
    let any_page = wb.sheets.iter().any(|s| s.page_dirty);
    let any_arr = wb.sheets.iter().any(|s| !s.arrays.is_empty());
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
    if restore_base.exists() && std::env::var("SC_NO_FIXUPS").is_err() {
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
    }

    // 最后一步：没编辑过的表整段按基准恢复（放在所有补丁之后，覆盖 umya 的改写）
    if restore_base.exists() {
        // 这一步要复制二十几张表的 XML（几百 MB），攒成一次落盘
        crate::patch::begin_stage();
        let kept = must_save!("restore-unedited-sheets", crate::fixups::restore_unedited_sheets(
            path,
            &restore_base,
            wb
        ));
        if !kept.is_empty() && prof {
            eprintln!("  [save] 原样沿用 {} 张表: {:?}", kept.len(), kept);
        }
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
    // 原样搬过来的部件（从基准包里 direct copy，不解压）
    if crate::patch::raw_pending() {
        match crate::patch::apply_raw_copies(path, &restore_base) {
            Ok(n) => {
                if prof {
                    eprintln!("  [save] 原样搬运 {n} 个部件");
                }
            }
            Err(e) => return Err(e.context("保存子步骤 raw-copies 失败")),
        }
    }
    // 跨工作簿引用：先把缺的部件补齐（用户新加的工作簿），再写缓存
    if !wb.links.is_empty() && std::env::var("SC_NO_XLINKS").is_err() {
        must_save!("xlink-parts", crate::xlinks::ensure_parts(path, &wb.links));
    }
    if !wb.links.is_empty() && std::env::var("SC_NO_XLINKS").is_err() {
        let refreshed = must_save!("xlink-cache", crate::xlinks::write_cache(path, &wb.links));
        if refreshed > 0 && prof {
            eprintln!("  [save] 刷新 {refreshed} 个外部链接缓存");
        }
    }
    match crate::patch::commit_stage(path) {
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
    pivots::restore_imported_parts(path, &restore_base)?;
    for (si, spec, res, rect, label) in &wb.pivots_pending {
        let sheet = wb.sheets.get(*si).context("透视表所属工作表不存在")?;
        crate::pivot_xml::attach_for_session(path, &sheet.name, spec, res, *rect, label, wb.pivot_session.token())
            .context("透视表部件写入失败")?;
    }
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
    must_save!("scenarios", crate::patch::apply_scenarios(path, wb));
    must_save!("ignored-errors", crate::patch::apply_ignored_errors(path, wb));
    must_save!("tab-colors", crate::patch::apply_tab_colors(path, wb));
    must_save!("protection", crate::patch::apply_protection(path, wb));
    must_save!("hyperlinks", crate::hyperlinks::apply(path, wb));
    must_save!("sparkline", crate::sparkline::apply(path, wb));
    must_save!("textbox", crate::textbox::apply(path, wb));
    must_save!("picture", crate::pic_write::apply(path, wb));
    must_save!("sheet-states", crate::patch::apply_sheet_states(path, wb));
    must_save!("rich", crate::patch::apply_rich(path, wb));
    must_save!("page-setup", crate::patch::apply_page_setup(path, wb));
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
    // 所有部件落盘之后再清一次悬空的 [Content_Types] Override：前面的 prune 跑在
    // staged 补丁之前，加表/改名时会出现「Content-Types 声明 /xl/worksheets/sheet2.xml，
    // 包里只有 native-sheet-…」——verify_saved_package 抓到的正是这个（Excel 会判包损坏）。
    must_save!("prune-content-types-final", crate::fixups::prune_content_types(path));
    // 最后一步：外链关系按原文恢复，避免 URL 被归一、TargetMode 被丢
    must_save!("restore-external-rels", crate::notes::restore_external_rels(path, &external_rels));
    Ok(())
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
                            let mut hidden = sheet.hidden_rows.clone();
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
    crate::patch::rewrite_zip(path, |name, buf| {
        if let Some((blocks, dv, freeze, freeze_dirty, af, hidden, f_dirty, page, page_dirty, arrs)) =
            per_sheet.get(name)
        {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
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
                return x.into_bytes();
            }
        }
        if name == "xl/workbook.xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = crate::patch::set_calc_mode(&xml, wb.calc_manual);
                if do_page {
                    for (idx, sheet) in wb.sheets.iter().enumerate() {
                        if sheet.page_dirty {
                            x = crate::patch::set_print_names(&x, idx as u32, &sheet.name, &sheet.page);
                        }
                    }
                }
                return x.into_bytes();
            }
        }
        if name == "xl/styles.xml" {
            // 没有 dxfs 就别碰 styles.xml：改写反而会写坏（Excel 直接判损坏）
            if dxfs.is_empty() {
                return buf;
            }
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return crate::patch::replace_dxfs(&xml, &dxfs).into_bytes();
            }
        }
        buf
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, CellRef, Sheet, Value, Workbook};

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
            sh.hidden_rows = f.hidden_rows(sh);
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
            sh.hidden_rows = f.hidden_rows(sh);
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
            sh.hidden_rows = f.hidden_rows(sh);
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
}

/// 把 calamine 的 Style 折成我们的 CellStyle（只保留看得见的部分）
fn convert_style(st: &calamine::Style) -> crate::style::CellStyle {
    let mut out = crate::style::CellStyle { size: 11.0, color: [0, 0, 0], ..Default::default() };
    if let Some(f) = &st.font {
        out.bold = matches!(f.weight, calamine::FontWeight::Bold);
        out.italic = matches!(f.style, calamine::FontStyle::Italic);
        out.underline = !matches!(f.underline, calamine::UnderlineStyle::None);
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
            _ => 0,
        };
        out.valign = match a.vertical {
            calamine::VerticalAlignment::Top => 1,
            calamine::VerticalAlignment::Center => 2,
            calamine::VerticalAlignment::Bottom => 3,
            _ => 0,
        };
        out.wrap = a.wrap_text;
        out.indent = a.indent.unwrap_or(0).min(15);
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
                calamine::BorderStyle::Thin | calamine::BorderStyle::Hair => 1,
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
