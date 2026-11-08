//! 外部数据源：CSV 导入、跨表取数、刷新、以及"存进名称管理器再读回来"。

use sheetcore::external::{self, Kind, Source};

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("sheets_native_ext");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn csv_import_fills_cells_and_types() {
    let p = tmp("a.csv");
    std::fs::write(&p, "部门,金额,备注\n研发,1200,\n销售,980.5,含税\n\"行,政\",30,呵呵\n").unwrap();
    let mut wb = sheetcore::model::Workbook::new_blank();
    let (rows, cols) =
        external::import_csv(&mut wb.sheets[0], p.to_str().unwrap(), ',', true, (0, 0)).unwrap();
    assert_eq!((rows, cols), (4, 3), "4 行 3 列");
    let s = &wb.sheets[0];
    assert_eq!(s.get(0, 0).unwrap().value.display(), "部门", "表头原样");
    assert_eq!(s.get(1, 1).unwrap().value.display(), "1200", "数字要当数字");
    match &s.get(1, 1).unwrap().value {
        sheetcore::model::Value::Number(n) => assert!((*n - 1200.0).abs() < 1e-9),
        v => panic!("B2 应该是数字，实际 {v:?}"),
    }
    assert_eq!(s.get(3, 0).unwrap().value.display(), "行,政", "引号里的分隔符不切");
    assert!(s.get(1, 2).is_none(), "空字段不落格");
}

#[test]
fn split_line_handles_quotes() {
    assert_eq!(external::split_line("a,b,c", ','), vec!["a", "b", "c"]);
    assert_eq!(external::split_line("\"a,1\",b", ','), vec!["a,1", "b"]);
    assert_eq!(external::split_line("\"a\"\"b\",c", ','), vec!["a\"b", "c"]);
    assert_eq!(external::split_line("a\tb", '\t'), vec!["a", "b"]);
}

#[test]
fn refresh_picks_up_file_changes() {
    let p = tmp("r.csv");
    std::fs::write(&p, "x\n1\n").unwrap();
    let mut wb = sheetcore::model::Workbook::new_blank();
    let src = Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv { path: p.to_str().unwrap().into(), delim: ',', header: true },
        at: (0, 0),
        rows: 0,
        cols: 0,
    };
    external::refresh(&mut wb, 0, &src).unwrap();
    assert_eq!(wb.sheets[0].get(1, 0).unwrap().value.display(), "1");
    std::fs::write(&p, "y\n42\n").unwrap();
    external::refresh(&mut wb, 0, &src).unwrap();
    assert_eq!(wb.sheets[0].get(1, 0).unwrap().value.display(), "42", "刷新要读到新值");
}

#[test]
fn cross_sheet_copy() {
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].set(
        0,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Text("甲".into()),
            formula: None,
        },
    );
    wb.sheets.push(sheetcore::model::Sheet::new("第二"));
    let src = Source {
        name: "_sc_src_1".into(),
        kind: Kind::Internal { sheet: "Sheet1".into(), rect: (0, 0, 0, 0) },
        at: (2, 2),
        rows: 0,
        cols: 0,
    };
    external::refresh(&mut wb, 1, &src).unwrap();
    assert_eq!(wb.sheets[1].get(2, 2).unwrap().value.display(), "甲");
}

#[test]
fn source_survives_save_and_load() {
    let csv = tmp("s.csv");
    std::fs::write(&csv, "k,v\n甲,1\n").unwrap();
    let out = tmp("ext.xlsx");
    let _ = std::fs::remove_file(&out);

    let mut wb = sheetcore::model::Workbook::new_blank();
    external::import_csv(&mut wb.sheets[0], csv.to_str().unwrap(), ',', true, (0, 0)).unwrap();
    wb.sources.push(Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv { path: csv.to_str().unwrap().into(), delim: ',', header: true },
        at: (0, 0),
        rows: 2,
        cols: 2,
    });
    wb.dirty = true;
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    let back = sheetcore::xlsx::load(&out).expect("读回");
    assert_eq!(back.sources.len(), 1, "数据源要跟着文件走");
    match &back.sources[0].kind {
        Kind::Csv { path, header, .. } => {
            assert!(path.ends_with("s.csv"), "路径读回来: {path}");
            assert!(*header);
        }
        k => panic!("类型错了: {k:?}"),
    }
    assert_eq!(back.sources[0].at, (0, 0));
    assert_eq!(back.sheets[0].get(1, 0).unwrap().value.display(), "甲");
    let _ = std::fs::remove_file(&out);
}
