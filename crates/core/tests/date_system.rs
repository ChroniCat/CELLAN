//! C04 日期体系：1900 vs 1904。
//!
//! 验收口径（core-requirements.md C04）：
//! - 读盘认 `workbookPr/@date1904`，Excel 1904 夹具的序列按 1904 基准解释；
//! - 日期函数、显示格式、局部/全量重算都用同一个基准；
//! - 存盘把 flag 写回去，重开序列不变（不能一边加 1462 一边又加一遍）；
//! - 1900 体系的闰年 bug 保留在 1900 侧，1904 侧没有这个 bug。

use sheetcore::model::{Cell, DateSystem, Value, Workbook};
use sheetcore::style::format_value_in;
use std::io::{Read, Write};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("sc_c04_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 用我们自己的写端存一份 1900 的 xlsx，再把 workbook.xml 改成 date1904="1"。
/// 这样夹具的 XML 是外部手改的，不是读写两端互相对齐出来的。
fn make_1904_fixture(dir: &std::path::Path, serial: f64) -> std::path::PathBuf {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(serial), formula: None });
    let src = dir.join("src.xlsx");
    sheetcore::xlsx::save(&src, &wb).expect("save src");

    let dst = dir.join("1904.xlsx");
    let file = std::fs::File::open(&src).unwrap();
    let mut zin = zip::ZipArchive::new(file).unwrap();
    let out = std::fs::File::create(&dst).unwrap();
    let mut zout = zip::ZipWriter::new(out);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        if name == "xl/workbook.xml" {
            let mut xml = String::from_utf8(buf).unwrap();
            if xml.contains("date1904=") {
                // 我们自己写的是 false/0，改成 1
                let start = xml.find("date1904=").unwrap();
                let end = xml[start..].find(|c: char| c == '"' ).map(|p| start + p).unwrap();
                // 替换属性值
                let after = &xml[start..];
                let q = after.find('"').unwrap();
                let q2 = after[q + 1..].find('"').unwrap() + q + 1;
                let full = &after[..=q2].to_string();
                xml = xml.replacen(full, "date1904=\"1\"", 1);
                let _ = end;
            } else {
                // 没有属性：塞到 workbookPr 里
                if let Some(p) = xml.find("<workbookPr") {
                    let close = xml[p..].find('>').map(|o| p + o).unwrap();
                    if xml.as_bytes()[close - 1] == b'/' {
                        xml.insert_str(close - 1, " date1904=\"1\"");
                    } else {
                        xml.insert_str(close, " date1904=\"1\"");
                    }
                }
            }
            buf = xml.into_bytes();
        }
        zout.start_file(name, opts).unwrap();
        zout.write_all(&buf).unwrap();
    }
    zout.finish().unwrap();
    dst
}

fn read_workbook_xml(path: &std::path::Path) -> String {
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut s = String::new();
    z.by_name("xl/workbook.xml").unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn a_1904_file_is_read_on_its_own_basis() {
    let dir = temp_dir("read");
    // 1904 体系里序列 1 = 1904-01-02
    let p = make_1904_fixture(&dir, 1.0);
    let wb = sheetcore::xlsx::load(&p).unwrap();
    assert_eq!(wb.date_system, DateSystem::D1904, "workbook.xml 里写了 date1904=1，load 必须认");
    let v = wb.sheets[0].get(0, 0).unwrap().value.clone();
    assert_eq!(v, Value::Number(1.0));
    assert_eq!(
        format_value_in(wb.date_system, &v, "yyyy-mm-dd"),
        "1904-01-02",
        "同一个序列 1，1900 体系是 1900-01-01，1904 体系是 1904-01-02"
    );
    // 函数侧同基准
    let y = sheetcore::formula::eval_formula(&wb, 0, "=YEAR(A1)");
    assert_eq!(y, sheetcore::formula::FValue::Number(1904.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_same_calendar_date_gets_two_serials_that_differ_by_1462() {
    let mut a = Workbook::new_blank();
    a.sheets[0].name = "S".into();
    a.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=DATE(2024,3,1)".into()) });
    let mut b = a.clone();
    b.date_system = DateSystem::D1904;

    sheetcore::formula::recalc_all(&mut a);
    sheetcore::formula::recalc_all(&mut b);
    let sa = a.sheets[0].get(0, 0).unwrap().value.clone();
    let sb = b.sheets[0].get(0, 0).unwrap().value.clone();
    let (Value::Number(na), Value::Number(nb)) = (sa.clone(), sb.clone()) else {
        panic!("DATE 应返回数字：{sa:?} / {sb:?}");
    };
    assert_eq!(na, 45352.0, "1900 体系 2024-03-01");
    assert_eq!(nb, 43890.0, "1904 体系同一天 = 45352 - 1462");
    assert_eq!(na - nb, DateSystem::OFFSET);

    // 显示必须还原成同一天
    assert_eq!(format_value_in(a.date_system, &sa, "yyyy-mm-dd"), "2024-03-01");
    assert_eq!(format_value_in(b.date_system, &sb, "yyyy-mm-dd"), "2024-03-01");
}

#[test]
fn date_functions_follow_the_workbook_system() {
    let mut wb = Workbook::new_blank();
    wb.date_system = DateSystem::D1904;
    wb.sheets[0].name = "S".into();
    let cases = [
        ("=DATE(1904,1,1)", 0.0),
        ("=DATE(1904,3,1)", 60.0),
        ("=DATE(2024,2,29)", 43889.0), // 闰日
        ("=DATEVALUE(\"2024-03-01\")", 43890.0),
    ];
    for (f, want) in cases {
        let v = sheetcore::formula::eval_formula(&wb, 0, f);
        assert_eq!(v, sheetcore::formula::FValue::Number(want), "{f}");
    }
    // 解析回来
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(43890.0), formula: None });
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=YEAR(A1)"), sheetcore::formula::FValue::Number(2024.0));
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=MONTH(A1)"), sheetcore::formula::FValue::Number(3.0));
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=DAY(A1)"), sheetcore::formula::FValue::Number(1.0));
    assert_eq!(
        sheetcore::formula::eval_formula(&wb, 0, "=TEXT(A1,\"yyyy-mm-dd\")"),
        sheetcore::formula::FValue::Text("2024-03-01".into())
    );
    // 日期相减还是天数
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(43900.0), formula: None });
    assert_eq!(
        sheetcore::formula::eval_formula(&wb, 0, "=A2-A1"),
        sheetcore::formula::FValue::Number(10.0)
    );
}

#[test]
fn both_systems_keep_their_own_leap_year_quirks() {
    // 1900 体系：Excel 把 1900 当闰年，序列 60 = 1900-02-29（不存在的日子）
    let mut a = Workbook::new_blank();
    a.sheets[0].name = "S".into();
    assert_eq!(
        sheetcore::formula::eval_formula(&a, 0, "=DATE(1900,2,29)"),
        sheetcore::formula::FValue::Number(60.0)
    );
    assert_eq!(
        sheetcore::formula::eval_formula(&a, 0, "=DATE(1900,2,28)"),
        sheetcore::formula::FValue::Number(59.0)
    );
    // 1904 体系没有这个 bug：1904-02-29 存在（1904 是闰年），序列 59
    let mut b = Workbook::new_blank();
    b.sheets[0].name = "S".into();
    b.date_system = DateSystem::D1904;
    assert_eq!(
        sheetcore::formula::eval_formula(&b, 0, "=DATE(1904,2,29)"),
        sheetcore::formula::FValue::Number(59.0)
    );
    // 1904 侧 1900-02-29 这个「日子」不存在，但序列照算
    assert_eq!(
        sheetcore::formula::eval_formula(&b, 0, "=YEAR(DATE(1904,1,1))"),
        sheetcore::formula::FValue::Number(1904.0)
    );
}

#[test]
fn today_is_offset_by_1462_in_the_1904_system() {
    let a = Workbook::new_blank();
    let mut b = a.clone();
    b.date_system = DateSystem::D1904;
    for (wb, label) in [(&a, "1900"), (&b, "1904")] {
        let v = sheetcore::formula::eval_formula(wb, 0, "=TODAY()");
        let sheetcore::formula::FValue::Number(n) = v else { panic!("{label}: TODAY 应返回数字") };
        let today = format_value_in(wb.date_system, &Value::Number(n), "yyyy-mm-dd");
        // 本机今天（UTC+08）—— 与 Excel 的 TODAY 同一天
        let want = today_from_system_clock();
        assert_eq!(today, want, "{label} 体系的 TODAY 应显示同一日历日");
    }
}

fn today_from_system_clock() -> String {
    // 用本机时钟拼 yyyy-mm-dd（不引入 chrono）
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 8 * 3600; // 本地是 UTC+08
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant civil_from_days（与实现无关的独立算法，用来核对）
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[test]
fn saving_a_1904_workbook_writes_the_flag_and_reopens_without_drift() {
    let dir = temp_dir("roundtrip");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.date_system = DateSystem::D1904;
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=DATE(2024,3,1)".into()) });
    sheetcore::formula::recalc_all(&mut wb);
    let before = wb.sheets[0].get(0, 0).unwrap().value.clone();
    assert_eq!(before, Value::Number(43890.0));

    let p = dir.join("out.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let xml = read_workbook_xml(&p);
    assert!(
        xml.contains("date1904=\"1\"") || xml.contains("date1904=\"true\""),
        "存出来的 workbook.xml 必须带 date1904=1：{xml}"
    );

    let mut back = sheetcore::xlsx::load(&p).unwrap();
    assert_eq!(back.date_system, DateSystem::D1904);
    assert_eq!(back.sheets[0].get(0, 0).unwrap().value.clone(), before, "重开后序列不能漂");
    sheetcore::formula::recalc_all(&mut back);
    assert_eq!(back.sheets[0].get(0, 0).unwrap().value.clone(), Value::Number(43890.0));
    assert_eq!(
        format_value_in(back.date_system, &before, "yyyy-mm-dd"),
        "2024-03-01"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_1900_workbook_does_not_gain_the_flag() {
    let dir = temp_dir("plain");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(45352.0), formula: None });
    let p = dir.join("plain.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let xml = read_workbook_xml(&p);
    assert!(!xml.contains("date1904=\"1\""), "默认 1900 不该写 1904：{xml}");
    let back = sheetcore::xlsx::load(&p).unwrap();
    assert_eq!(back.date_system, DateSystem::D1900);
    assert_eq!(
        format_value_in(back.date_system, &Value::Number(45352.0), "yyyy-mm-dd"),
        "2024-03-01"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn xls_datetime_mode_survives_a_roundtrip() {
    let dir = temp_dir("xls");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.date_system = DateSystem::D1904;
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(43890.0), formula: None });
    let p = dir.join("d.xls");
    sheetcore::xls_write::write(&p, &wb).expect("xls save");
    let back = sheetcore::xls_read::load(&p).expect("xls load");
    assert_eq!(back.date_system, DateSystem::D1904, "DATEMODE 记录没读回来");
    assert_eq!(back.sheets[0].get(0, 0).unwrap().value.clone(), Value::Number(43890.0));

    // 1900 的文件不写 DATEMODE，读回来还是 1900
    let mut wb2 = Workbook::new_blank();
    wb2.sheets[0].name = "S".into();
    wb2.sheets[0].set(0, 0, Cell { value: Value::Number(45352.0), formula: None });
    let p2 = dir.join("d1900.xls");
    sheetcore::xls_write::write(&p2, &wb2).unwrap();
    let back2 = sheetcore::xls_read::load(&p2).unwrap();
    assert_eq!(back2.date_system, DateSystem::D1900);
    let _ = std::fs::remove_dir_all(&dir);
}
