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
    Ok(wb)
}

/// 写回 xlsx。文件已存在时先读入，以保留样式、列宽等未改动的部分。
pub fn save(path: &Path, wb: &Workbook) -> Result<()> {
    // 先把被清空的单元格从文件里删掉（见 Sheet::cleared）
    use umya_spreadsheet::{new_file, reader::xlsx::read, writer::xlsx::write};

    let prof = std::env::var("SC_SAVE_PROF").is_ok();
    let t_read = std::time::Instant::now();
    let mut book = if path.exists() {
        read(path).with_context(|| format!("读取现有文件失败 {}", path.display()))?
    } else {
        new_file()
    };

    if prof {
        eprintln!("  [save] 读入原文件 {:.2}s", t_read.elapsed().as_secs_f64());
    }
    let t_build = std::time::Instant::now();
    for (idx, sheet) in wb.sheets.iter().enumerate() {
        // 新建文件默认带一个 Sheet1：名字不同时直接改名，避免多出一张空表
        let needs_rename = !path.exists()
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

    if prof {
        eprintln!("  [save] 组装 DOM {:.2}s", t_build.elapsed().as_secs_f64());
    }
    let t_write = std::time::Instant::now();
    let out = write(&book, path).with_context(|| format!("写入失败 {}", path.display()));
    if prof {
        eprintln!("  [save] 序列化+压缩 {:.2}s", t_write.elapsed().as_secs_f64());
    }
    out?;
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
