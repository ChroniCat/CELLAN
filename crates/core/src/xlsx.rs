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
        Data::Error(e) => Value::Text(format!("#{:?}", e).into()),
    }
}

/// 读取 xlsx/xls/ods（按扩展名自动识别）

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
) -> Vec<RawSheet> {
    let mut out = Vec::new();
    let Ok(mut reader) = crate::xlsx::open_workbook_auto(path) else {
        return out;
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
                Err(e) => eprintln!("[load] {name} 稀疏读取失败，退回 calamine: {e:#}"),
            }
        }
        if let Ok(range) = reader.worksheet_range(name) {
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
        if let Ok(st) = reader.worksheet_style(name) {
            for (row, col, style) in st.cells() {
                raw.styles.push(((row, col), convert_style(style)));
            }
        }
        if let Ok(forms) = reader.worksheet_formula(name) {
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
        out.push(raw);
    }
    out
}

pub fn load(path: &Path) -> Result<Workbook> {
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
            let mut wb = crate::xls_read::load(path)?;
            apply_xls_meta(path, &mut wb);
            return Ok(wb);
        }
    };
    let names = reader.sheet_names().to_owned();

    let is_cfb = std::fs::read(path)
        .map(|b| b.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]))
        .unwrap_or(false);
    let mut wb = Workbook {
        names: crate::patch::read_defined_names(path),
        // 数据源存在 `_sc_src_*` 名称里，读出来重建
        sources: crate::external::read_sources(path),
        links: crate::xlinks::read(path),
        names_dirty: false,
        pivot_shifts: Vec::new(),
        pivot_cache_base: Default::default(),
        pivot_session: Default::default(),
        has_vba: crate::vba::has_macros(path),
        pivots_pending: Vec::new(),
        sheets: Vec::new(),
        path: Some(path.to_path_buf()),
        dirty: false,
        styles: crate::style::StyleTable::new(),
    };

    // 稀疏表旁路：声明区域大到离谱的表（有人在 A1048576/XFD1048576 留了格子），
    // calamine 会按稠密网格分配内存，直接 OOM
    let targets = crate::patch::sheet_targets(path).unwrap_or_default();
    let shared = crate::sparse::shared_strings(path);

    let prof_load = std::env::var("SC_LOAD_PROF").is_ok();
    let t_all_load = std::time::Instant::now();
    // 版面/合并/数组公式/图片/表格这几趟扫描跟单元格数据无关，
    // 和下面的逐表解析并行跑，省掉串行等待
    let (side_tx, side_rx) = std::sync::mpsc::channel();
    {
        let p = path.to_path_buf();
        std::thread::spawn(move || {
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
    let raws: Vec<RawSheet> = std::thread::scope(|sc| {
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
        for h in handles {
            match h.join() {
                Ok(v) => all.extend(v),
                Err(_) => eprintln!("[load] 并行解析线程异常退出"),
            }
        }
        all
    });

    // .xls 不是 zip：这条只给 ooXML 用
    let is_ooxml = std::fs::read(path)
        .map(|b| b.starts_with(b"PK"))
        .unwrap_or(false);
    let mut shared_zip = if is_ooxml {
        Some(zip::ZipArchive::new(std::fs::File::open(path)?)?)
    } else {
        None
    };
    for raw in raws {
        let t_sheet = std::time::Instant::now();
        let mut sheet = Sheet::new(raw.name.clone());
        sheet.source_part = targets.get(&raw.name).cloned();
        sheet.loading = true;
        for ((r, c), cell) in raw.cells {
            sheet.set(r, c, cell);
        }
        for ((r, c), cs) in raw.styles {
            let id = wb.styles.intern(cs);
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
        let _ = t_sheet;
        wb.sheets.push(sheet);
    }
    if prof_load {
        eprintln!("[load] 物化合计 {:.2}s（并行解析已完成）", t_all.elapsed().as_secs_f64());
    }

    let (found, layouts_raw, pics, tables) = side_rx.recv().unwrap_or_else(|_| {
        (
            Default::default(),
            Ok(Default::default()),
            Default::default(),
            Default::default(),
        )
    });
    let layouts = layouts_raw.unwrap_or_default();

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
                sheet.cf.blocks = lay.cf.clone();
                sheet.cf.dxfs = lay.dxfs.clone();
                sheet.dv = lay.dv.clone();
                sheet.freeze = lay.freeze;
                sheet.filter = lay.filter.clone();
                if let Some(f) = &sheet.filter {
                    sheet.hidden_rows = f.hidden_rows(sheet);
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

    // 图表
    if let Ok(all) = crate::charts::load_charts_all(path, &wb.sheets) {
        for (idx, sheet) in wb.sheets.iter_mut().enumerate() {
            if let Some(list) = all.get(&(idx as u32)) {
                sheet.charts = list.clone();
            }
        }
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
        wb.pivot_cache_base = std::sync::Arc::new(crate::model::PivotCacheBase {
            definitions: pivot_definitions(path),
            sheets: wb.sheets.iter().map(|s| (s.identity, s.name.clone())).collect(),
        });
    }
    Ok(wb)
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
        }
    }
}

/// Save a snapshot, then acknowledge only its sheet identities in the live
/// workbook. Callers retaining the workbook should apply the returned receipt.
pub fn save_atomic_with_receipt(path: &Path, wb: &Workbook) -> Result<SaveReceipt> {
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
    Ok(SaveReceipt { sheets })
}

/// 写回 xlsx。文件已存在时先读入，以保留样式、列宽等未改动的部分。

/// 按最终目标扩展名保存 XLS/XLSX/XLSM：先写同目录的临时文件，再 rename 覆盖目标。
///
/// 网盘（SMB）上直接写目标路径，断网或对方锁定会留下半截文件；
/// 同一卷内 rename 是原子的，别人要么看到旧文件、要么看到完整的新文件。
pub fn save_atomic(path: &Path, wb: &Workbook) -> Result<()> {
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

pub fn save(path: &Path, wb: &Workbook) -> Result<()> {
    // 先把被清空的单元格从文件里删掉（见 Sheet::cleared）
    use umya_spreadsheet::{new_file, reader::xlsx::read, writer::xlsx::write};

    let prof = std::env::var("SC_SAVE_PROF").is_ok();
    let t_read = std::time::Instant::now();
    // 基准文件必须用「打开时的原文件」，不能用另存的目标路径：
    // 否则另存出来的文件会丢掉合并单元格、样式、数据验证、图片等
    // 我们不解析也不重建的结构（umya 从零建表只会写单元格）。
    let original_base = match wb.path.as_ref().filter(|p| p.exists()) {
        Some(p) => p.clone(),
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
    let mut book = if base.exists() {
        read(&base).with_context(|| format!("读取现有文件失败 {}", base.display()))?
    } else {
        new_file()
    };

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

        // 样式：只回写用户改过的格子（整表重写样式在 10 万格级别会拖慢保存）
        for cref in sheet.style_edits.iter() {
            let sid = sheet.style_of(cref.row, cref.col);
            let st = wb.styles.get(sid);
            let cell = ws.get_cell_mut((cref.col + 1, cref.row + 1));
            let s = cell.get_style_mut();
            {
                let f = s.get_font_mut();
                f.set_bold(st.bold);
                f.set_italic(st.italic);
                f.set_size(st.size as f64);
                // 字体名：空串表示默认字体，不碰原文件的字体（避免把 Calibri 写成别的）
                if !st.font.is_empty() {
                    f.set_name(st.font.clone());
                }
                let mut col = umya_spreadsheet::Color::default();
                col.set_argb_str(format!("FF{:02X}{:02X}{:02X}", st.color[0], st.color[1], st.color[2]));
                f.set_color(col);
            }
            if let Some(rgb) = st.fill {
                s.set_background_color_solid(format!("FF{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]));
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
        }

        // 合并区域：只有被改过才同步，没动过就沿用文件里原有的
        if sheet.merges_dirty {
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
                // 外部引用刷新过之后，把新的缓存值一起写进去：否则别人打开看到的还是旧数字
                if crate::xlinks::is_external_formula(f) {
                    match &cell.value {
                        Value::Number(n) => {
                            target.set_formula_result_number(*n);
                        }
                        Value::Text(t) if !t.starts_with('#') => {
                            target.set_formula_result_string(t.to_string());
                        }
                        Value::Bool(v) => {
                            target.set_formula_result_bool(*v);
                        }
                        _ => {}
                    }
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
        if !sheet.notes_dirty {
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
        match crate::fixups::merge_sheet_data(path, &restore_base, wb) {
            Ok(v) if !v.is_empty() => {
                if prof {
                    eprintln!("  [save] 合并单元格数据到基准骨架: {:?}", v);
                }
            }
            Err(e) => eprintln!("[save] 合并工作表骨架失败: {e:#}"),
            _ => {}
        }
    }

    // 样式表原样沿用基准：umya 重建的样式表和原文件对不上（字体/填充被裁、dxfs 被压）
    let untouched = wb.sheets.iter().all(|s| !s.styles_dirty);
    if let Some(xml) = base_styles.as_ref().filter(|_| untouched) {
        match crate::patch::append_entry(path, "xl/styles.xml", xml) {
            Ok(()) => {
                if prof {
                    eprintln!("  [save] 样式表沿用基准");
                }
            }
            Err(e) => eprintln!("[save] 恢复样式表失败: {e:#}"),
        }
    }

    // 宏部件：umya 写不出来，塞回去
    if let Some(v) = vba {
        if let Err(e) = crate::vba::restore_vba(path, &v) {
            eprintln!("[save] 宏恢复失败: {e:#}");
        }
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
        if let Err(e) = crate::patch::shift_pivot_caches(path, &wb.pivot_shifts) {
            eprintln!("[save] 透视表源区域回写失败: {e:#}");
        }
    }

    // 自定义名称
    if wb.names_dirty {
        crate::patch::write_user_names(path, &wb.names).context("名称回写失败")?;
    }
    // 数据源存成"自定义文档属性"：这是 Excel 自己的规范位置，
    // 塞进 definedNames 会和查询表打架，单独加未知部件又会被拒收
    if !wb.sources.is_empty() && std::env::var("SC_NO_SRC_PROP").is_err() {
        if let Err(e) = crate::external::write_sources(path, &wb.sources) {
            eprintln!("[save] 数据源写入失败: {e:#}");
        }
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
        for ch in sheet.charts.iter() {
            if ch.part.is_empty() || ch.ref_fixes.is_empty() {
                continue;
            }
            if let Err(e) = crate::chart_write::replace_refs(path, &ch.part, &ch.ref_fixes) {
                eprintln!("[save] 图表引用回写失败: {e:#}");
            }
        }
    }

    // 表格对象：umya 会丢掉 tableParts，保存后自己挂回去
    for sheet in wb.sheets.iter() {
        if !sheet.tables.is_empty() && std::env::var("SC_NO_TABLE_ATTACH").is_err() {
            if let Err(e) = crate::table::attach(path, &sheet.name, &sheet.tables) {
                eprintln!("[save] 表格对象写入失败: {e:#}");
            }
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
        if let Err(e) = apply_cf_patch(path, wb, any_cf, any_dv, any_freeze, any_filter, any_page) {
            eprintln!("[save] 条件格式/数据验证补丁失败: {e}");
        }
    }
    if prof {
        eprintln!("  [save] 其余补丁 {:.2}s", t_write.elapsed().as_secs_f64());
    }
    let t_fix = std::time::Instant::now();
    // 收尾：umya 会改坏 drawing rels 的图表关系，并留下旧的表格/批注部件
    if restore_base.exists() && std::env::var("SC_NO_FIXUPS").is_err() {
        // 工作表被挪目录时，关系要跟着搬过去
        if let Err(e) = crate::fixups::migrate_sheet_rels(path, &restore_base) {
            eprintln!("[save] 迁移工作表关系失败: {e:#}");
        }
        if let Err(e) = crate::fixups::fix_drawing_rels(path, &restore_base) {
            eprintln!("[save] 修复 drawing 关系失败: {e:#}");
        }
        if let Err(e) = crate::fixups::dedupe_singleton_rels(path) {
            eprintln!("[save] 去重关系失败: {e:#}");
        }
        if let Err(e) = crate::fixups::prune_orphans(path) {
            eprintln!("[save] 清理孤儿部件失败: {e:#}");
        }
        if let Err(e) = crate::fixups::restore_sheet_refs(path, &restore_base) {
            eprintln!("[save] 补回工作表引用元素失败: {e:#}");
        }
        if let Err(e) = crate::fixups::migrate_workbook_rels(path, &restore_base) {
            eprintln!("[save] 迁移 workbook 关系失败: {e:#}");
        }
        if let Err(e) = crate::fixups::sanitize_drawings(path) {
            eprintln!("[save] 清理绘图属性失败: {e:#}");
        }
        if let Err(e) = crate::fixups::ensure_content_types(path, &restore_base) {
            eprintln!("[save] 补齐内容类型失败: {e:#}");
        }
    }

    // 最后一步：没编辑过的表整段按基准恢复（放在所有补丁之后，覆盖 umya 的改写）
    if restore_base.exists() {
        // 这一步要复制二十几张表的 XML（几百 MB），攒成一次落盘
        crate::patch::begin_stage();
        match crate::fixups::restore_unedited_sheets(path, &restore_base, wb) {
            Ok(v) if !v.is_empty() => {
                if prof {
                    eprintln!("  [save] 原样沿用 {} 张表: {:?}", v.len(), v);
                }
            }
            Err(e) => eprintln!("[save] 恢复未编辑工作表失败: {e:#}"),
            _ => {}
        }
        // umya 会把工作表搬到 xl/worksheets/，搬回去（绘图/图表/透视表的相对路径才成立）
        match crate::fixups::rehome_sheets(path, &restore_base) {
            Ok(v) if !v.is_empty() => {
                if prof {
                    eprintln!("  [save] 工作表放回原位 {} 张", v.len());
                }
            }
            Err(e) => eprintln!("[save] 工作表归位失败: {e:#}"),
            _ => {}
        }
        match crate::fixups::restore_workbook_skeleton(path, &restore_base, wb) {
            Ok(true) => {
                if prof {
                    eprintln!("  [save] 工作簿骨架沿用基准（definedNames/pivotCaches 保住）");
                }
            }
            Err(e) => eprintln!("[save] 恢复工作簿骨架失败: {e:#}"),
            _ => {}
        }
        match crate::fixups::restore_base_sheet_rels(path, &restore_base, wb) {
            Ok(n) if n > 0 => {
                if prof {
                    eprintln!("  [save] 工作表 rels 沿用基准 {n} 份");
                }
            }
            Err(e) => eprintln!("[save] 恢复工作表 rels 失败: {e:#}"),
            _ => {}
        }
        let _ = crate::fixups::ensure_content_types(path, &restore_base);
        // 恢复后清一遍孤儿部件（基准部件另有一份同类型副本时会重复）
        let _ = crate::fixups::prune_orphans(path);
        if std::env::var("SC_NO_NORM_STYLES").is_err() {
        match crate::fixups::normalize_styles_counts(path) {
            Ok(true) => {
                if prof {
                    eprintln!("  [save] 重算 styles.xml 的 count");
                }
            }
            Err(e) => eprintln!("[save] 重算 styles count 失败: {e:#}"),
            _ => {}
        }
        }
        match crate::fixups::ensure_shared_strings_rel(path) {
            Ok(true) => {
                if prof {
                    eprintln!("  [save] 补上 sharedStrings 关系");
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("[save] 补 sharedStrings 关系失败: {e:#}"),
        }
        if let Err(e) = crate::fixups::rebuild_content_types(path, &restore_base) {
            eprintln!("[save] 重建内容类型失败: {e:#}");
        }
        match crate::fixups::prune_content_types(path) {
            Ok(n) if n > 0 => {
                if prof {
                    eprintln!("  [save] 清掉 {n} 条悬空内容类型");
                }
            }
            _ => {}
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
            Err(e) => eprintln!("[save] 原样搬运失败: {e:#}"),
        }
    }
    // 跨工作簿引用：先把缺的部件补齐（用户新加的工作簿），再写缓存
    if !wb.links.is_empty() && std::env::var("SC_NO_XLINKS").is_err() {
        if let Err(e) = crate::xlinks::ensure_parts(path, &wb.links) {
            eprintln!("[save] 补外部链接部件失败: {e:#}");
        }
    }
    if !wb.links.is_empty() && std::env::var("SC_NO_XLINKS").is_err() {
        match crate::xlinks::write_cache(path, &wb.links) {
            Ok(n) if n > 0 => {
                if prof {
                    eprintln!("  [save] 刷新 {n} 个外部链接缓存");
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("[save] 外部链接缓存写回失败: {e:#}"),
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
                sheet.filter.as_ref().map(crate::patch::auto_filter_xml).unwrap_or_default()
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
                        sheet.hidden_rows.clone(),
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
        if do_page && name == "xl/workbook.xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                for (idx, sheet) in wb.sheets.iter().enumerate() {
                    if sheet.page_dirty {
                        x = crate::patch::set_print_names(&x, idx as u32, &sheet.name, &sheet.page);
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
