//! C09：首次导入与刷新必须共用同一套解析/编码/列类型模型。
//!
//! 失败条件（审计原文）：字段里带换行的 CSV 被 `lines()` 拆成多条记录，
//! 应 2 行的文件变 3 行；刷新还只读 UTF-8 并把 00123 自动转成数字。

use sheetcore::external::{import_csv_with, refresh, CsvOpts, Kind, Source};
use sheetcore::model::{Sheet, Value, Workbook};

fn write(d: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let _ = std::fs::create_dir_all(d);
    let p = d.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn sheet() -> Sheet {
    Sheet::new("数据")
}

fn text(s: &Sheet, r: u32, c: u32) -> String {
    match &s.get(r, c).map(|c| c.value.clone()).unwrap_or(Value::Empty) {
        Value::Text(t) => t.to_string(),
        Value::Number(n) => format!("{n}"),
        Value::Bool(b) => b.to_string(),
        Value::Empty => String::new(),
        other => format!("{other:?}"),
    }
}

/// 字段内的换行属于同一个字段：两条记录，不是三条。
#[test]
fn newline_inside_quotes_is_one_field_for_import_and_refresh() {
    let d = std::env::temp_dir().join(format!("sc_c09_nl_{}", std::process::id()));
    let csv = write(&d, "a.csv", "\"line1\nline2\",B\nC,D\n".as_bytes());
    let mut sh = sheet();
    let (rows, cols) = import_csv_with(
        &mut sh,
        csv.to_str().unwrap(),
        ',',
        false,
        None,
        &[],
        (0, 0),
        (0, 0),
    )
    .unwrap();
    assert_eq!((rows, cols), (2, 2), "引号内换行不能拆成新记录");
    assert_eq!(text(&sh, 0, 0), "line1\nline2");
    assert_eq!(text(&sh, 1, 0), "C");

    // 用同一份文件刷新到同一位置：结果必须逐格一致
    let mut wb = Workbook::new_blank();
    wb.sheets[0] = sh.clone();
    let src = Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv { path: csv.to_str().unwrap().into(), delim: ',', header: false },
        at: (0, 0),
        target_sheet: None,
        rows,
        cols,
        csv_opts: Some(CsvOpts::default()),
    };
    refresh(&mut wb, 0, &src).unwrap();
    assert_eq!(text(&wb.sheets[0], 0, 0), "line1\nline2");
    assert_eq!(text(&wb.sheets[0], 1, 1), "D");
}

/// UTF-8 BOM / UTF-16LE / GB18030 都能读，且中文不丢。
#[test]
fn decodes_bom_utf16_and_gb18030() {
    let d = std::env::temp_dir().join(format!("sc_c09_explicit_enc_{}", std::process::id()));

    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice("城市,数量\n北京,1\n".as_bytes());
    let p1 = write(&d, "bom.csv", &bom);
    let mut sh = sheet();
    import_csv_with(&mut sh, p1.to_str().unwrap(), ',', true, None, &[], (0, 0), (0, 0)).unwrap();
    assert_eq!(text(&sh, 0, 0), "城市");
    assert_eq!(text(&sh, 1, 0), "北京");

    let utf16: Vec<u8> = "名称,值\n甲,2\n"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let mut with_bom = vec![0xFF, 0xFE];
    with_bom.extend_from_slice(&utf16);
    let p2 = write(&d, "u16.csv", &with_bom);
    let mut sh2 = sheet();
    import_csv_with(&mut sh2, p2.to_str().unwrap(), ',', true, None, &[], (0, 0), (0, 0)).unwrap();
    assert_eq!(text(&sh2, 1, 0), "甲");

    let (gb, _, _) = encoding_rs::GB18030.encode("城市,数量\n上海,3\n");
    let p3 = write(&d, "gb.csv", &gb);
    let mut sh3 = sheet();
    import_csv_with(&mut sh3, p3.to_str().unwrap(), ',', true, None, &[], (0, 0), (0, 0)).unwrap();
    assert_eq!(text(&sh3, 1, 0), "上海");
    assert_eq!(text(&sh3, 1, 1), "3");
}

/// 标记为文本的列：00123 与 18 位账号逐字保留；没标记的按原规则转数字。
#[test]
fn text_columns_keep_leading_zeros_and_long_accounts() {
    let d = std::env::temp_dir().join(format!("sc_c09_txt_{}", std::process::id()));
    let csv = write(
        &d,
        "t.csv",
        "代码,账号,金额\n00123,622202123456789012,12.50\n".as_bytes(),
    );
    let mut sh = sheet();
    import_csv_with(
        &mut sh,
        csv.to_str().unwrap(),
        ',',
        true,
        None,
        &[0, 1],
        (0, 0),
        (0, 0),
    )
    .unwrap();
    assert_eq!(text(&sh, 1, 0), "00123", "文本列的前导零必须保留");
    assert_eq!(text(&sh, 1, 1), "622202123456789012", "18 位账号不能变成浮点");
    assert_eq!(text(&sh, 1, 2), "12.5", "未标记的列仍按数字解析");

    // 不标记时按原规则：00123 → 123（这是 v1 行为，保持兼容）
    let mut sh2 = sheet();
    import_csv_with(&mut sh2, csv.to_str().unwrap(), ',', true, None, &[], (0, 0), (0, 0)).unwrap();
    assert_eq!(text(&sh2, 1, 0), "123");
}

/// 编码失败不能先清旧值。
#[test]
fn decode_failure_keeps_previous_data() {
    let d = std::env::temp_dir().join(format!("sc_c09_bad_{}", std::process::id()));
    let mut sh = sheet();
    sh.set(0, 0, sheetcore::model::Cell { value: Value::Text("旧值".into()), formula: None });
    // UTF-16 里塞奇数字节：解码会报 bad
    let p = write(&d, "bad.csv", &[0xFF, 0xFE, 0x41]);
    let err = import_csv_with(
        &mut sh,
        p.to_str().unwrap(),
        ',',
        false,
        Some(sheetcore::csv::Encoding::Utf16Le),
        &[],
        (0, 0),
        (0, 0),
    );
    assert!(err.is_err(), "坏编码应报错");
    assert_eq!(text(&sh, 0, 0), "旧值", "失败时旧数据必须还在");
}

/// 刷新后数据变少：只清自己上次写的区域，旁边用户的格不动。
#[test]
fn shrinking_refresh_clears_only_its_own_old_region() {
    let d = std::env::temp_dir().join(format!("sc_c09_shrink_{}", std::process::id()));
    let big = write(&d, "big.csv", "A,B,C\nD,E,F\nG,H,I\n".as_bytes());
    let mut wb = Workbook::new_blank();
    wb.sheets[0] = sheet();
    let src = Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv { path: big.to_str().unwrap().into(), delim: ',', header: false },
        at: (0, 0),
        target_sheet: None,
        rows: 0,
        cols: 0,
        csv_opts: Some(CsvOpts::default()),
    };
    let (r, c) = refresh(&mut wb, 0, &src).unwrap();
    // 用户在自己区域旁边写入
    wb.sheets[0].set(3, 3, sheetcore::model::Cell { value: Value::Text("用户".into()), formula: None });

    let small = write(&d, "small.csv", "X\n".as_bytes());
    let src2 = Source {
        kind: Kind::Csv { path: small.to_str().unwrap().into(), delim: ',', header: false },
        rows: r,
        cols: c,
        ..src
    };
    refresh(&mut wb, 0, &src2).unwrap();
    assert_eq!(text(&wb.sheets[0], 0, 0), "X");
    assert_eq!(text(&wb.sheets[0], 2, 2), "", "旧数据超出的部分要清掉");
    assert_eq!(text(&wb.sheets[0], 3, 3), "用户", "旁边的用户数据不能被误清");
}

/// 配置要持久化：编码与文本列随工作簿保存/重开仍在，刷新才可能一致。
#[test]
fn csv_options_survive_save_and_reload() {
    let d = std::env::temp_dir().join(format!("sc_c09_persist_{}", std::process::id()));
    let csv = write(&d, "p.csv", "00123\n".as_bytes());
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let mut sh = wb.sheets[0].clone();
    import_csv_with(
        &mut sh,
        csv.to_str().unwrap(),
        ',',
        false,
        Some(sheetcore::csv::Encoding::Gb18030),
        &[0],
        (0, 0),
        (0, 0),
    )
    .unwrap();
    wb.sheets[0] = sh;
    wb.sources.push(Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv { path: csv.to_str().unwrap().into(), delim: ',', header: false },
        at: (0, 0),
        target_sheet: None,
        rows: 1,
        cols: 1,
        csv_opts: Some(CsvOpts { encoding: Some(sheetcore::csv::Encoding::Gb18030), text_cols: vec![0], quote: '\'' }),
    });
    let x = d.join("p.xlsx");
    sheetcore::xlsx::save(&x, &wb).unwrap();
    let back = sheetcore::xlsx::load(&x).unwrap();
    let src = back.sources.iter().find(|s| s.name == "_sc_src_1").expect("源应还在");
    let opts = src.csv_opts.as_ref().expect("CSV 配置应持久化");
    assert_eq!(opts.encoding, Some(sheetcore::csv::Encoding::Gb18030));
    assert_eq!(opts.text_cols, vec![0]);
    assert_eq!(opts.quote, '\'', "引号字符也要持久化");
    assert_eq!(
        match &back.sheets[0].get(0, 0).map(|c| c.value.clone()) {
            Some(Value::Text(t)) => t.to_string(),
            other => format!("{other:?}"),
        },
        "00123"
    );
}

#[test]
fn strict_parser_rejects_unclosed_and_trailing_junk_quotes() {
    assert!(sheetcore::csv::records_strict("\"abc,1\n", ',').is_err(), "未闭合引号必须报错");
    assert!(
        sheetcore::csv::records_strict("\"abc\"x,1\n", ',').is_err(),
        "闭引号后多余字符必须报错"
    );
    let ok = sheetcore::csv::records_strict("\"a\"\"b\",2\n", ',').unwrap();
    assert_eq!(ok, vec![vec!["a\"b".to_string(), "2".to_string()]]);
    // CRLF、尾空列、空行
    let rows = sheetcore::csv::records_strict("a,b,\r\n\r\nc,d,e\r\n", ',').unwrap();
    assert_eq!(rows, vec![
        vec![String::from("a"), String::from("b"), String::new()],
        vec![String::from("c"), String::from("d"), String::from("e")],
    ]);
}

#[test]
fn explicit_encoding_is_never_silently_replaced_by_gb18030() {
    let d = std::env::temp_dir().join(format!("sc_c09_enc_{}", std::process::id()));
    let gb = write(&d, "gb.csv", &[0xC4, 0xE3, 0xBA, 0xC3, b',', b'1', b'\n']); // 你好,1 的 GB18030
    let bytes = std::fs::read(&gb).unwrap();
    let (txt, enc, bad) = sheetcore::csv::decode(&bytes, Some(sheetcore::csv::Encoding::Utf8));
    assert!(bad, "指定 UTF-8 解码失败必须报告，而不是回退 GB18030");
    assert!(txt.is_empty());
    assert_eq!(enc, sheetcore::csv::Encoding::Utf8);
    // 自动探测才允许回退
    let (txt2, enc2, bad2) = sheetcore::csv::decode(&bytes, None);
    assert!(!bad2);
    assert_eq!(enc2, sheetcore::csv::Encoding::Gb18030);
    assert_eq!(txt2, "你好,1\n");
}

#[test]
fn bad_quote_on_refresh_keeps_previous_cells_untouched() {
    let d = std::env::temp_dir().join(format!("sc_c09_badq_{}", std::process::id()));
    let csv = write(&d, "q.csv", b"A,B\nC,D\n");
    let mut sh = sheet();
    let (r, c) = import_csv_with(&mut sh, csv.to_str().unwrap(), ',', false, None, &[], (0, 0), (0, 0)).unwrap();
    assert_eq!((r, c), (2, 2));
    // 破坏引号：未闭合
    std::fs::write(&csv, b"A,B\n\"C\n").unwrap();
    let src = Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv { path: csv.to_str().unwrap().into(), delim: ',', header: false },
        at: (0, 0),
        target_sheet: None,
        rows: r,
        cols: c,
        csv_opts: Some(CsvOpts::default()),
    };
    let mut wb = Workbook::new_blank();
    wb.sheets = vec![sh];
    let err = refresh(&mut wb, 0, &src).unwrap_err();
    assert!(format!("{err}").contains("引号"), "错误要指出引号问题：{err}");
    assert_eq!(text(&wb.sheets[0], 0, 0), "A");
    assert_eq!(text(&wb.sheets[0], 0, 1), "B");
    assert_eq!(text(&wb.sheets[0], 1, 0), "C");
    assert_eq!(text(&wb.sheets[0], 1, 1), "D");
}

#[test]
fn short_row_refresh_matches_first_import_for_the_same_rectangle() {
    let d = std::env::temp_dir().join(format!("sc_c09_short_{}", std::process::id()));
    let csv = write(&d, "s.csv", b"A,B\nC,D\n");
    let mut fresh = sheet();
    let mut old = sheet();
    let (r0, c0) = import_csv_with(&mut old, csv.to_str().unwrap(), ',', false, None, &[], (0, 0), (0, 0)).unwrap();
    assert_eq!((r0, c0), (2, 2));
    std::fs::write(&csv, b"A\nB,C\n").unwrap();
    let src = Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv { path: csv.to_str().unwrap().into(), delim: ',', header: false },
        at: (0, 0),
        target_sheet: None,
        rows: r0,
        cols: c0,
        csv_opts: Some(CsvOpts::default()),
    };
    let mut wb = Workbook::new_blank();
    wb.sheets = vec![old];
    refresh(&mut wb, 0, &src).unwrap();
    let (r1, c1) = import_csv_with(&mut fresh, csv.to_str().unwrap(), ',', false, None, &[], (0, 0), (0, 0)).unwrap();
    for r in 0..r1.max(2) {
        for c in 0..c1.max(2) {
            assert_eq!(
                text(&wb.sheets[0], r, c),
                text(&fresh, r, c),
                "短行刷新后 ({r},{c}) 应与首次导入一致"
            );
        }
    }
    assert_eq!(text(&wb.sheets[0], 0, 1), "", "旧 B1 必须被清空");
}
