//! C08：保存时不得删掉第三方的自定义文档属性。
//!
//! 原缺陷：`external::write_sources` 每次新造整份 `docProps/custom.xml`，只写本应用的数据源属性，
//! 用户原有的 `Customer=original-metadata` 之类全部消失。

use sheetcore::external::{Kind, Source, write_sources, write_sources_from};
use sheetcore::model::{Cell, Value, Workbook};

const CT: &str = "application/vnd.openxmlformats-officedocument.custom-properties+xml";

fn src(i: u32, path: &str) -> Source {
    Source {
        name: format!("_sc_src_{i}"),
        kind: Kind::Csv { path: path.into(), delim: ',', header: true },
        at: (0, 0),
        target_sheet: None,
        rows: 2,
        cols: 2,
        csv_opts: None,
    }
}

/// 造一个「Excel 存出来的文件」：四类第三方属性 + 正确的 rels/Content_Types
fn book_with_props(dir: &std::path::Path) -> std::path::PathBuf {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
    let p = dir.join("props.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let custom = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="Customer"><vt:lpwstr>original-metadata</vt:lpwstr></property><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3" name="Revision"><vt:i4>42</vt:i4></property><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="4" name="Reviewed"><vt:filetime>2024-03-01T08:30:00Z</vt:filetime></property><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="5" name="Approved"><vt:bool>true</vt:bool></property></Properties>"#;
    let rels = sheetcore::fixups::read_part(&p, "_rels/.rels")
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap();
    let rel = "<Relationship Id=\"rId900\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties\" Target=\"docProps/custom.xml\"/>";
    let i = rels.rfind("</Relationships>").unwrap();
    let rels = format!("{}{}{}", &rels[..i], rel, &rels[i..]);
    let ct = sheetcore::fixups::read_part(&p, "[Content_Types].xml")
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap();
    let i = ct.rfind("</Types>").unwrap();
    let ct = format!("{}<Override PartName=\"/docProps/custom.xml\" ContentType=\"{CT}\"/>{}</Types>", &ct[..i], "").replace("</Types></Types>", "</Types>");
    let ct = if ct.contains("</Types>") { ct } else { format!("{}</Types>", &ct) };
    sheetcore::fixups::write_parts(
        &p,
        &[
            ("docProps/custom.xml".into(), custom.as_bytes().to_vec()),
            ("_rels/.rels".into(), rels.into_bytes()),
            ("[Content_Types].xml".into(), ct.into_bytes()),
        ],
    )
    .unwrap();
    p
}

/// 合法但写法刁钻的第三方文件：`<cp:property>` 前缀、单引号 pid/name、
/// 类型元素用 `v:` 而不是 `vt:`。旧实现字符串找 `<property` 会漏掉它。
fn book_with_prefixed_props(dir: &std::path::Path) -> std::path::PathBuf {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
    let p = dir.join("prefixed.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let custom = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:Properties xmlns:cp="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:v="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><cp:property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid='2' name='客户'><v:lpwstr>甲公司</v:lpwstr></cp:property><cp:property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3" name="Revision"><v:i4>42</v:i4></cp:property></cp:Properties>"#;
    let rels = sheetcore::fixups::read_part(&p, "_rels/.rels").and_then(|b| String::from_utf8(b).ok()).unwrap();
    let rel = "<Relationship Id=\"rId900\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties\" Target=\"docProps/custom.xml\"/>";
    let i = rels.rfind("</Relationships>").unwrap();
    let rels = format!("{}{}{}", &rels[..i], rel, &rels[i..]);
    let ct = sheetcore::fixups::read_part(&p, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()).unwrap();
    let i = ct.rfind("</Types>").unwrap();
    let ct = format!("{}<Override PartName=\"/docProps/custom.xml\" ContentType=\"{CT}\"/>{}</Types>", &ct[..i], "");
    let ct = ct.replace("</Types></Types>", "</Types>");
    sheetcore::fixups::write_parts(&p, &[
        ("docProps/custom.xml".into(), custom.as_bytes().to_vec()),
        ("_rels/.rels".into(), rels.into_bytes()),
        ("[Content_Types].xml".into(), ct.into_bytes()),
    ]).unwrap();
    p
}

fn pids(xml: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let b = xml.as_bytes();
    let mut i = 0;
    while let Some(pos) = xml[i..].find("pid=") {
        let at = i + pos + 4;
        let (q, start) = match b.get(at) { Some(b'\'') => (b'\'', at + 1), Some(b'"') => (b'"', at + 1), _ => { i = at; continue; } };
        let mut end = start;
        while end < b.len() && b[end] != q && b[end].is_ascii_digit() { end += 1; }
        if let Ok(n) = xml[start..end].parse() { out.push(n); }
        i = end;
    }
    out
}

fn custom_xml(p: &std::path::Path) -> String {
    String::from_utf8(sheetcore::fixups::read_part(p, "docProps/custom.xml").expect("custom.xml 应存在"))
        .unwrap()
}

fn has(xml: &str, needle: &str) -> bool {
    xml.contains(needle)
}

#[test]
fn third_party_properties_survive_writing_sources() {
    let dir = std::env::temp_dir().join(format!("sc_c08_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_props(&dir);

    write_sources(&p, &[src(1, "C:/data/a.csv")]).unwrap();
    let xml = custom_xml(&p);

    // 四类第三方属性：类型、pid、值都在
    assert!(has(&xml, r#"pid="2" name="Customer"><vt:lpwstr>original-metadata</vt:lpwstr>"#), "{xml}");
    assert!(has(&xml, r#"pid="3" name="Revision"><vt:i4>42</vt:i4>"#), "{xml}");
    assert!(has(&xml, r#"pid="4" name="Reviewed"><vt:filetime>2024-03-01T08:30:00Z</vt:filetime>"#), "{xml}");
    assert!(has(&xml, r#"pid="5" name="Approved"><vt:bool>true</vt:bool>"#), "{xml}");
    // 应用属性：新 pid 不与第三方冲突
    assert!(has(&xml, r#"name="_sc_src_1""#), "{xml}");
    assert!(has(&xml, "pid=\"6\""), "应用属性应接着最大 pid 分配：{xml}");
}

#[test]
fn repeated_saves_do_not_duplicate_or_drift() {
    let dir = std::env::temp_dir().join(format!("sc_c08_rep_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_props(&dir);

    write_sources(&p, &[src(1, "C:/a.csv"), src(2, "C:/b.csv")]).unwrap();
    let first = custom_xml(&p);
    write_sources(&p, &[src(1, "C:/a.csv"), src(2, "C:/b.csv")]).unwrap();
    let second = custom_xml(&p);
    assert_eq!(first, second, "连续保存不应重复或改变 pid");

    // 改名/换源：第三方不变，应用属性跟着换
    write_sources(&p, &[src(1, "C:/renamed.csv")]).unwrap();
    let third = custom_xml(&p);
    assert!(has(&third, "C:/renamed.csv"));
    assert!(!has(&third, "C:/a.csv"), "旧的数据源属性必须被替换掉：{third}");
    assert!(has(&third, r#"name="Customer""#));
    assert_eq!(third.matches("name=\"_sc_src_1\"").count(), 1);
    assert_eq!(third.matches("name=\"_sc_src_2\"").count(), 0);
}

#[test]
fn deleting_the_last_source_removes_only_app_metadata() {
    let dir = std::env::temp_dir().join(format!("sc_c08_del_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_props(&dir);
    write_sources(&p, &[src(1, "C:/a.csv")]).unwrap();
    write_sources(&p, &[]).unwrap();

    let xml = custom_xml(&p);
    assert!(!has(&xml, "_sc_src_"), "最后一个数据源删掉后不该留下应用属性：{xml}");
    assert!(has(&xml, r#"name="Customer""#), "第三方属性不能被一起删掉：{xml}");

    // 没有第三方属性时，最后一个数据源删掉 → 部件本身也要清理
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let q = dir.join("plain.xlsx");
    sheetcore::xlsx::save(&q, &wb).unwrap();
    write_sources(&q, &[src(1, "C:/a.csv")]).unwrap();
    assert!(sheetcore::fixups::read_part(&q, "docProps/custom.xml").is_some());
    write_sources(&q, &[]).unwrap();
    assert!(
        sheetcore::fixups::read_part(&q, "docProps/custom.xml").is_none(),
        "没有第三方属性时应删除整个部件"
    );
    let rels = String::from_utf8(sheetcore::fixups::read_part(&q, "_rels/.rels").unwrap()).unwrap();
    assert!(!rels.contains("custom-properties"), "关系也要删：{rels}");
    let ct = String::from_utf8(sheetcore::fixups::read_part(&q, "[Content_Types].xml").unwrap()).unwrap();
    assert!(!ct.contains("/docProps/custom.xml"), "内容类型覆盖也要删：{ct}");
}

#[test]
fn names_and_values_with_markup_are_escaped() {
    let dir = std::env::temp_dir().join(format!("sc_c08_esc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let p = dir.join("esc.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let mut s = src(1, "C:/x&y/<z>\"q\".csv");
    // 名称属性也要转义（应用只生成 _sc_src_N，但归属识别只要求前缀，
    // 需求明确写了「XML 名称和值必须转义」）
    s.name = "_sc_src_1&\"q\"".into();
    write_sources(&p, &[s]).unwrap();
    let xml = custom_xml(&p);
    assert!(has(&xml, "&amp;"), "& 必须转义：{xml}");
    assert!(!has(&xml, "C:/x&y"), "裸 & 不是合法 XML：{xml}");
    assert!(has(&xml, "&lt;z&gt;"), "<> 必须转义：{xml}");
    assert!(
        has(&xml, "name=\"_sc_src_1&amp;&quot;q&quot;\""),
        "属性里的 & 和 \" 必须转义：{xml}"
    );

    // 能被 XML 解析器读回来（含属性名转义）
    let mut r = quick_xml::Reader::from_str(&xml);
    loop {
        match r.read_event() {
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => panic!("生成的 custom.xml 不是合法 XML: {e}\n{xml}"),
        }
    }
}

#[test]
fn save_path_keeps_third_party_props_end_to_end() {
    // 走真实保存入口：Workbook.sources + xlsx::save
    let dir = std::env::temp_dir().join(format!("sc_c08_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_props(&dir);

    let mut wb = sheetcore::xlsx::load(&p).unwrap();
    wb.sources = vec![src(1, "C:/data/ledger.csv")];
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(7.0), formula: None });
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let xml = custom_xml(&p);
    assert!(has(&xml, r#"name="Customer"><vt:lpwstr>original-metadata</vt:lpwstr>"#), "{xml}");
    assert!(has(&xml, r#"name="Reviewed"><vt:filetime>"#), "{xml}");
    assert!(has(&xml, "_sc_src_1"), "{xml}");

    // 再打开、删源、保存：第三方仍在
    let mut wb2 = sheetcore::xlsx::load(&p).unwrap();
    assert_eq!(wb2.sources.len(), 1, "数据源应能读回");
    wb2.sources.clear();
    sheetcore::xlsx::save(&p, &wb2).unwrap();
    let xml = custom_xml(&p);
    assert!(has(&xml, r#"name="Customer""#), "{xml}");
    assert!(!has(&xml, "_sc_src_"), "{xml}");

    // 另存到新路径：第三方属性跟着走
    let out = dir.join("copy.xlsx");
    sheetcore::xlsx::save(&out, &wb2).unwrap();
    let xml = custom_xml(&out);
    assert!(has(&xml, r#"name="Customer""#), "另存后第三方属性丢了：{xml}");
}

#[test]
fn base_argument_reads_props_from_the_original_file() {
    // umya 重写包之后新包里可能没有 custom.xml；这时必须从基准文件读第三方属性
    let dir = std::env::temp_dir().join(format!("sc_c08_base_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let base = book_with_props(&dir);

    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let target = dir.join("target.xlsx");
    sheetcore::xlsx::save(&target, &wb).unwrap();
    assert!(sheetcore::fixups::read_part(&target, "docProps/custom.xml").is_none());

    write_sources_from(&target, &[src(1, "C:/a.csv")], Some(&base)).unwrap();
    let xml = custom_xml(&target);
    assert!(has(&xml, r#"name="Customer""#), "没从基准文件读回来：{xml}");
    assert!(has(&xml, "_sc_src_1"));
}

/// 生成给 Excel/WPS 实机看属性的夹具：%TEMP%\sc_c08_excel\c08.xlsx
#[test]
#[ignore = "生成 Excel/WPS 自定义属性对拍夹具（人工驱动的实机验收用）"]
fn make_c08_fixture() {
    let dir = std::env::temp_dir().join("sc_c08_excel");
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_props(&dir);
    let mut wb = sheetcore::xlsx::load(&p).unwrap();
    wb.sources = vec![src(1, "C:/data/ledger.csv")];
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let out = dir.join("c08.xlsx");
    if out != p {
        let _ = std::fs::remove_file(&out);
        std::fs::copy(&p, &out).unwrap();
    }
    println!("FIXTURE {} (第三方 4 项 + _sc_src_1)", out.display());
}

/// Excel/WPS 打开并另存之后，第三方属性与应用属性都要还在（实机往返）。
/// 由 `scratch/c08_excel_roundtrip.ps1` 先跑，再 `SC_C08_RT_FILE=... cargo test -- --ignored excel_roundtrip`。
#[test]
#[ignore = "需要先由 Excel/WPS 另存出文件（COM 驱动）"]
fn excel_roundtrip_keeps_custom_props() {
    let path = std::env::var("SC_C08_RT_FILE").expect("需要 SC_C08_RT_FILE");
    let xml = String::from_utf8(sheetcore::fixups::read_part(std::path::Path::new(&path), "docProps/custom.xml")
        .expect("Excel 另存后 custom.xml 应还在")).unwrap();
    for needle in [
        r#"name="Customer"><vt:lpwstr>original-metadata</vt:lpwstr>"#,
        r#"name="Revision"><vt:i4>42</vt:i4>"#,
        r#"name="Reviewed"><vt:filetime>2024-03-01T08:30:00Z</vt:filetime>"#,
        r#"name="Approved"><vt:bool>true</vt:bool>"#,
    ] {
        assert!(xml.contains(needle), "Excel 另存后丢了第三方属性 {needle}：\n{xml}");
    }
    assert!(xml.contains("_sc_src_1"), "应用数据源属性没了：\n{xml}");
    // 应用自己的读端还能把数据源解析回来
    let srcs = sheetcore::external::read_sources(std::path::Path::new(&path));
    assert_eq!(srcs.len(), 1, "数据源读不回来：{srcs:?}");
}

#[test]
fn refreshing_a_csv_source_keeps_third_party_props() {
    // 刷新数据源 = import_csv 覆盖目标区 + 保存；属性区不得被动到
    let dir = std::env::temp_dir().join(format!("sc_c08_ref_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_props(&dir);
    let csv = dir.join("ledger.csv");
    std::fs::write(&csv, "a,b\n1,2\n").unwrap();

    let mut wb = sheetcore::xlsx::load(&p).unwrap();
    wb.sources = vec![src(1, csv.to_string_lossy().as_ref())];
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let csv_path = match &wb.sources[0].kind {
        Kind::Csv { path, .. } => path.clone(),
        _ => unreachable!(),
    };
    let (r, c) = sheetcore::external::import_csv(&mut wb.sheets[0], &csv_path, ',', true, (2, 0)).unwrap();
    assert_eq!((r, c), (2, 2));
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let xml = custom_xml(&p);
    assert!(xml.contains(r#"name="Customer"><vt:lpwstr>original-metadata</vt:lpwstr>"#), "{xml}");
    assert!(xml.contains(r#"name="Approved"><vt:bool>true</vt:bool>"#), "{xml}");
    assert_eq!(xml.matches("name=\"_sc_src_1\"").count(), 1, "刷新不应重复写属性：{xml}");
}

#[test]
fn sources_with_arbitrary_labels_land_in_the_app_namespace() {
    // 老代码允许 Source.name 是任意文字（例如「导入1」），写出去后读端只认 _sc_src_ 前缀，
    // 于是这个源保存后直接消失。写端必须把名字归一到应用命名空间。
    let dir = std::env::temp_dir().join(format!("sc_c08_label_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_props(&dir);

    let mut wb = sheetcore::xlsx::load(&p).unwrap();
    let mut s1 = src(1, "C:/a.csv");
    s1.name = "导入1".into();
    let mut s2 = src(2, "C:/b.csv");
    s2.name = "_sc_src_1".into(); // 与第一条归一后的名字撞车
    wb.sources = vec![s1, s2];
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let xml = custom_xml(&p);
    assert!(!xml.contains(r#"name="导入1""#), "任意名字不能写进属性表：{xml}");
    assert_eq!(xml.matches("name=\"_sc_src_1\"").count(), 1, "名字必须唯一：{xml}");
    assert!(xml.contains("C:/a.csv") && xml.contains("C:/b.csv"), "{xml}");

    let back = sheetcore::xlsx::load(&p).unwrap();
    assert_eq!(back.sources.len(), 2, "两个数据源都要读回来：{:?}", back.sources);
}

#[test]
fn prefixed_properties_single_quotes_and_alt_type_prefix_survive() {
    let dir = std::env::temp_dir().join(format!("sc_c08_pref_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // PID 会复用，残留 target.xlsx 会让断言假失败
    let _ = std::fs::create_dir_all(&dir);
    let p = book_with_prefixed_props(&dir);
    write_sources(&p, &[src(1, "C:/data/a.csv")]).unwrap();
    let xml = custom_xml(&p);

    // 第三方属性原样保留（含单引号原文、v: 前缀、类型值）
    assert!(xml.contains("pid='2' name='客户'"), "单引号 pid/name 必须原样保留：{xml}");
    assert!(xml.contains("<v:lpwstr>甲公司</v:lpwstr>"), "替代前缀类型元素必须保留：{xml}");
    assert!(xml.contains("<v:i4>42</v:i4>"));
    assert!(xml.contains("fmtid=\"{D5CDD505-2E9C-101B-9397-08002B2CF9AE}\""));
    // 前缀仍然绑定（没有 unbound prefix）
    assert!(xml.contains("xmlns:cp=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\""));
    assert!(xml.contains("xmlns:v=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\""));
    // 本应用属性沿用文件里的前缀，不混用默认命名空间
    assert!(xml.contains("<cp:property"), "新属性应沿用 cp: 前缀：{xml}");
    assert!(!xml.contains("<property "), "不应写出无前缀属性：{xml}");
    // pid 唯一
    let mut ids = pids(&xml);
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "pid 必须唯一：{xml}");
    // 读端能读回数据源
    let back = sheetcore::external::read_sources(&p);
    assert_eq!(back.len(), 1, "读完应看到 1 条源：{back:?}");
}

/// 根上的命名空间用单引号声明也是合法 XML：不能再补一条同名声明（重复属性 → 非法）。
#[test]
fn single_quoted_namespace_declarations_are_not_duplicated() {
    let d = std::env::temp_dir().join(format!("sc_c08_sq_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
    let p = d.join("sq.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let custom = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\
<Properties xmlns='http://schemas.openxmlformats.org/officeDocument/2006/custom-properties' xmlns:v='http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes'>\
<property fmtid='{D5CDD505-2E9C-101B-9397-08002B2CF9AE}' pid='2' name='Owner'><v:lpwstr>甲</v:lpwstr></property></Properties>";
    let rels = sheetcore::fixups::read_part(&p, "_rels/.rels").and_then(|b| String::from_utf8(b).ok()).unwrap();
    let rel = "<Relationship Id=\"rId901\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties\" Target=\"docProps/custom.xml\"/>";
    let i = rels.rfind("</Relationships>").unwrap();
    let rels = format!("{}{}{}", &rels[..i], rel, &rels[i..]);
    let ct = sheetcore::fixups::read_part(&p, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()).unwrap();
    let i = ct.rfind("</Types>").unwrap();
    let ct = format!("{}<Override PartName=\"/docProps/custom.xml\" ContentType=\"{CT}\"/>{}</Types>", &ct[..i], "").replace("</Types></Types>", "</Types>");
    sheetcore::fixups::write_parts(&p, &[
        ("docProps/custom.xml".into(), custom.as_bytes().to_vec()),
        ("_rels/.rels".into(), rels.into_bytes()),
        ("[Content_Types].xml".into(), ct.into_bytes()),
    ]).unwrap();

    write_sources(&p, &[src(1, "C:/data/a.csv")]).unwrap();
    let xml = custom_xml(&p);
    // VT 命名空间只能声明一次（单引号/双引号都算）
    let n = xml.matches("docPropsVTypes").count();
    assert_eq!(n, 1, "VT 命名空间不能重复声明：{xml}");
    assert!(xml.contains("xmlns:v='http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes'"), "原声明应原样保留：{xml}");
    assert!(xml.contains("<v:lpwstr>甲</v:lpwstr>"));
    assert!(!xml.contains("<vt:lpwstr>"), "新属性要沿用文件里的前缀");
    // 属性名/值仍能读回
    assert_eq!(sheetcore::external::read_sources(&p).len(), 1);
}
