use crate::model::{Cell, Sheet, Value, Workbook};
use anyhow::{Context, Result};
use calamine::{open_workbook_auto, Data, Reader};
use std::path::Path;

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
pub fn load(path: &Path) -> Result<Workbook> {
    let mut reader =
        open_workbook_auto(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let names = reader.sheet_names().to_owned();

    let mut wb = Workbook {
        sheets: Vec::new(),
        path: Some(path.to_path_buf()),
        dirty: false,
        styles: crate::style::StyleTable::new(),
    };

    for name in names {
        let mut sheet = Sheet::new(name.clone());

        if let Ok(range) = reader.worksheet_range(&name) {
            // calamine 的 cells() 给的是相对 Range 左上角的坐标，必须加上 start 偏移
            let (r0, c0) = range.start().unwrap_or((0, 0));
            for (row, col, data) in range.cells() {
                let v = data_to_value(data);
                if !v.is_empty() {
                    sheet.set(r0 + row as u32, c0 + col as u32, Cell { value: v, formula: None });
                }
            }
        }

        // 样式：calamine 已解析好样式表，折算成我们的紧凑结构
        if let Ok(st) = reader.worksheet_style(&name) {
            for (row, col, style) in st.cells() {
                let cs = convert_style(style);
                let id = wb.styles.intern(cs);
                if id != 0 {
                    sheet.set_style_loaded(row, col, id);
                }
            }
        }

        // 公式与值分开返回，补上公式原文
        if let Ok(forms) = reader.worksheet_formula(&name) {
            let (fr0, fc0) = forms.start().unwrap_or((0, 0));
            for (row, col, f) in forms.cells() {
                if f.is_empty() {
                    continue;
                }
                let text = if f.starts_with('=') { f.to_string() } else { format!("={}", f) };
                let rr = fr0 + row as u32;
                let cc = fc0 + col as u32;
                let mut cell = sheet.get(rr, cc).cloned().unwrap_or_default();
                cell.formula = Some(text.into());
                sheet.set(rr, cc, cell);
            }
        }

        wb.sheets.push(sheet);
    }

    if wb.sheets.is_empty() {
        wb.sheets.push(Sheet::new("Sheet1"));
    }
    // 合并单元格（calamine 不提供，单独解析一次）
    if let Ok(layouts) = crate::merges::load_layout(path) {
        for sheet in wb.sheets.iter_mut() {
            if let Some(lay) = layouts.get(&sheet.name) {
                sheet.merges = lay.merges.clone();
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

    Ok(wb)
}

/// 写回 xlsx。文件已存在时先读入，以保留样式、列宽等未改动的部分。
pub fn save(path: &Path, wb: &Workbook) -> Result<()> {
    // 先把被清空的单元格从文件里删掉（见 Sheet::cleared）
    use umya_spreadsheet::{new_file, reader::xlsx::read, writer::xlsx::write};

    let prof = std::env::var("SC_SAVE_PROF").is_ok();
    let t_read = std::time::Instant::now();
    // 基准文件必须用「打开时的原文件」，不能用另存的目标路径：
    // 否则另存出来的文件会丢掉合并单元格、样式、数据验证、图片等
    // 我们不解析也不重建的结构（umya 从零建表只会写单元格）。
    let base = match wb.path.as_ref().filter(|p| p.exists()) {
        Some(p) => p.clone(),
        None => path.to_path_buf(),
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
                continue;
            }
            let coord = (cref.col + 1, cref.row + 1);
            if let Some(f) = &cell.formula {
                ws.get_cell_mut(coord).set_formula(f.clone());
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

    // umya 不管条件格式的 dxf 细节，保存完直接补 XML
    let any_cf = wb.sheets.iter().any(|s| s.cf_dirty);
    let any_dv = wb.sheets.iter().any(|s| s.dv_dirty);
    let any_freeze = wb.sheets.iter().any(|s| s.freeze_dirty);
    if any_cf || any_dv || any_freeze {
        if let Err(e) = apply_cf_patch(path, wb, any_cf, any_dv, any_freeze) {
            eprintln!("[save] 条件格式/数据验证补丁失败: {e}");
        }
    }
    Ok(())
}

/// 把条件格式写进 xlsx（重写 sheet XML 与 styles.xml 的 dxfs）
fn apply_cf_patch(path: &Path, wb: &Workbook, do_cf: bool, do_dv: bool, do_freeze: bool) -> Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    // 先把每张表要写的块算出来
    let mut per_sheet: std::collections::HashMap<String, (String, String, Option<(u32, u32)>, bool)> =
        std::collections::HashMap::new();
    for sheet in wb.sheets.iter() {
        if let Some(t) = targets.get(&sheet.name) {
            let cf = if do_cf { crate::patch::cf_blocks_xml(&sheet.cf.blocks) } else { String::new() };
            let dv = if do_dv { crate::patch::dv_xml(&sheet.dv) } else { String::new() };
            if !cf.is_empty() || !dv.is_empty() || sheet.cf_dirty || sheet.dv_dirty || sheet.freeze_dirty {
                per_sheet.insert(t.clone(), (cf, dv, sheet.freeze, sheet.freeze_dirty));
            }
        }
    }
    let dxfs: Vec<crate::cf::Dxf> = wb
        .sheets
        .iter()
        .find(|s| s.cf_dirty)
        .map(|s| s.cf.dxfs.clone())
        .unwrap_or_default();
    crate::patch::rewrite_zip(path, |name, buf| {
        if let Some((blocks, dv, freeze, freeze_dirty)) = per_sheet.get(name) {
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
                return x.into_bytes();
            }
        }
        if name == "xl/styles.xml" {
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
