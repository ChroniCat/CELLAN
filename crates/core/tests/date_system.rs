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

#[test]
fn chart_category_axis_uses_the_workbook_date_system() {
    use sheetcore::charts::{Chart, ChartKind, Series};

    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.date_system = DateSystem::D1904;
    let id = wb.styles.intern(sheetcore::style::CellStyle { format: "yyyy-mm-dd".into(), ..Default::default() });
    // 1904 体系：1 = 1904-01-02，2 = 1904-01-03
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(2.0), formula: None });
    wb.sheets[0].set_style(0, 0, id);
    wb.sheets[0].set_style(1, 0, id);

    let mut ch = Chart {
        kind: ChartKind::Column,
        series: vec![Series {
            cat_ref: "S!$A$1:$A$2".into(),
            ..Default::default()
        }],
        legend: false,
        anchor: (0, 0, 10, 10),
        legend_pos: String::new(),
        ..Default::default()
    };
    sheetcore::charts::resolve_series_in(
        &mut ch,
        &wb.sheets[0],
        Some(&wb.styles),
        wb.date_system,
    );
    assert_eq!(ch.series[0].cats, vec!["1904-01-02", "1904-01-03"], "图表分类轴要按日期格式+体系渲染");

    // 同一份数据换成 1900 体系（序列 1 = 1900-01-01）
    let mut wb2 = wb.clone();
    wb2.date_system = DateSystem::D1900;
    let mut ch2 = ch.clone();
    ch2.series[0].cats.clear();
    sheetcore::charts::resolve_series_in(&mut ch2, &wb2.sheets[0], Some(&wb2.styles), wb2.date_system);
    assert_eq!(ch2.series[0].cats, vec!["1900-01-01", "1900-01-02"]);
}

/// 生成给真实 Excel/WPS 对拍用的夹具（不进常规回归）。路径固定，方便 COM 脚本引用：
///   %TEMP%\sc_c04_excel\a1904.xlsx / a1900.xlsx
#[test]
#[ignore = "生成 Excel/WPS 对拍夹具（人工驱动的实机验收用）"]
fn make_excel_fixtures() {
    let dir = std::env::temp_dir().join("sc_c04_excel");
    let _ = std::fs::create_dir_all(&dir);
    for (name, sys) in [("a1904.xlsx", DateSystem::D1904), ("a1900.xlsx", DateSystem::D1900)] {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "S".into();
        wb.date_system = sys;
        let id = wb
            .styles
            .intern(sheetcore::style::CellStyle { format: "yyyy-mm-dd".into(), ..Default::default() });
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        wb.sheets[0].set_style(0, 0, id);
        wb.sheets[0].cols.set(0, 96.0); // 否则 Excel/WPS 里 A 列太窄，日期显示成 #######
        wb.sheets[0].cols.set(1, 64.0);
        wb.sheets[0].layout_dirty = true;
        wb.sheets[0].set(0, 1, Cell { value: Value::Text("保留".into()), formula: None });
        let p = dir.join(name);
        sheetcore::xlsx::save(&p, &wb).expect("写夹具");
        println!("FIXTURE {} {}", p.display(), sys.name());
    }
}

/// Excel 另存出来的文件用我们的读端打开：日期体系与序列都不能漂。
/// 由 COM 脚本先跑，再 `SC_C04_EXCEL_OUT=... cargo test --test date_system excel_saved -- --ignored`
#[test]
#[ignore = "需要先由 Excel/WPS 另存出文件（COM 驱动）"]
fn excel_saved_file_keeps_the_date_system() {
    let path = std::env::var("SC_C04_EXCEL_OUT").expect("需要 SC_C04_EXCEL_OUT=<Excel 另存的文件>");
    let wb = sheetcore::xlsx::load(std::path::Path::new(&path)).expect("读 Excel 另存的文件");
    assert_eq!(wb.date_system, DateSystem::D1904, "Excel 另存后 date1904 丢了");
    assert_eq!(wb.sheets[0].get(0, 0).unwrap().value.clone(), Value::Number(1.0), "A1 序列漂了");
    assert_eq!(
        format_value_in(wb.date_system, &Value::Number(1.0), "yyyy-mm-dd"),
        "1904-01-02"
    );
    // Excel 改了非日期格 B2=7 再另存：证明读的确实是 Excel 写出来的文件
    assert_eq!(wb.sheets[0].get(1, 1).unwrap().value.clone(), Value::Number(7.0));
}

/// 新簿 + 自定义数字格式：styles.xml 不能写出非法字体属性。
/// 实测 `sz val="0"` / `u val="none"` / `strike val="0"` 会让 Excel 16.0 直接拒绝打开整个文件
/// （COM 报 0x800A03EC），我们自己的读端却读得回来——所以必须断言包内容。
#[test]
fn saved_styles_never_emit_zero_font_size_or_val_none() {
    let dir = std::env::temp_dir().join(format!("sc_c04_font_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let id = wb
        .styles
        .intern(sheetcore::style::CellStyle { format: "yyyy-mm-dd".into(), ..Default::default() });
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set_style(0, 0, id);
    let p = dir.join("font.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let xml = String::from_utf8(sheetcore::fixups::read_part(&p, "xl/styles.xml").expect("styles.xml"))
        .expect("utf8");
    for bad in ["sz val=\"0\"", "u val=\"none\"", "strike val=\"0\""] {
        assert!(!xml.contains(bad), "styles.xml 里不该出现 {bad}：\n{xml}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一套检查用在 Excel 另存出来的 **1900** 文件上：不许被顺手写成 1904。
#[test]
#[ignore = "需要先由 Excel/WPS 另存出文件（COM 驱动）"]
fn excel_saved_1900_file_stays_1900() {
    let path = std::env::var("SC_C04_EXCEL_OUT_1900").expect("需要 SC_C04_EXCEL_OUT_1900");
    let wb = sheetcore::xlsx::load(std::path::Path::new(&path)).expect("读 Excel 另存的文件");
    assert_eq!(wb.date_system, DateSystem::D1900);
    assert_eq!(wb.sheets[0].get(0, 0).unwrap().value.clone(), Value::Number(1.0));
    assert_eq!(wb.sheets[0].get(1, 1).unwrap().value.clone(), Value::Number(7.0));
    assert_eq!(
        format_value_in(wb.date_system, &Value::Number(1.0), "yyyy-mm-dd"),
        "1900-01-01"
    );
}

/// 原要求路径：本项目导入 1904 夹具 → 本项目改非日期格 → 本项目另存**新路径** →
/// Excel 与 WPS 各自打开这个新文件。
/// 生成两对文件（1904/1900）供 `scratch/c04_export_probe.ps1` 实机打开。
#[test]
#[ignore = "生成 C04 另存新路径的夹具（Excel/WPS 实机打开用）"]
fn make_c04_export_fixture() {
    let dir = std::env::temp_dir().join("sc_c04_export");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, sys) in [("1904", DateSystem::D1904), ("1900", DateSystem::D1900)] {
        // 1) 先造夹具（本项目写出）
        let mut seed_wb = Workbook::new_blank();
        seed_wb.sheets[0].name = "S".into();
        seed_wb.date_system = sys;
        let id = seed_wb
            .styles
            .intern(sheetcore::style::CellStyle { format: "yyyy-mm-dd".into(), ..Default::default() });
        seed_wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        seed_wb.sheets[0].set_style(0, 0, id);
        seed_wb.sheets[0].cols.set(0, 96.0);
        seed_wb.sheets[0].cols.set(1, 64.0);
        seed_wb.sheets[0].layout_dirty = true;
        let seed = dir.join(format!("seed_{name}.xlsx"));
        sheetcore::xlsx::save(&seed, &seed_wb).expect("写夹具");

        // 2) 本项目导入（读端）：体系必须是夹具声明的那个
        let mut wb = sheetcore::xlsx::load(&seed).expect("导入夹具");
        assert_eq!(wb.date_system, sys, "{name} 夹具导入后体系漂了");
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(1.0));

        // 3) 改一个**非日期**格
        wb.sheets[0].set(1, 1, Cell { value: Value::Number(7.0), formula: None });

        // 4) 另存到新路径（不是原地）
        let out = dir.join(format!("export_{name}.xlsx"));
        sheetcore::xlsx::save(&out, &wb).expect("另存新路径");

        // 5) 自检：新文件读回来体系、序列、非日期格都对
        let back = sheetcore::xlsx::load(&out).expect("读另存文件");
        assert_eq!(back.date_system, sys);
        assert_eq!(back.sheets[0].value(0, 0), Value::Number(1.0));
        assert_eq!(back.sheets[0].value(1, 1), Value::Number(7.0));
        let fmt = back.styles.get(back.sheet(0).style_of(0, 0)).format.clone();
        let disp = format_value_in(sys, &Value::Number(1.0), &fmt);
        println!("EXPORT system={} file={} A1=1 display={} B2=7 fmt={}",
                 sys.name(), out.display(), disp, fmt);
    }
}
