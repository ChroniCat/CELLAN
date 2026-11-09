//! 跨工作簿引用：读 externalLinks、用缓存求值、刷新缓存、写回。
//!
//! 样本是手工拼的包（表格由我们自己写，externalLink 部件按 Excel 的实际格式补），
//! 这样测试不依赖 Excel，也不依赖外部文件。

use sheetcore::model::{Cell, Value, Workbook};
use std::path::{Path, PathBuf};

fn tmpdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("sheet_xlinks").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cell(v: Value, f: Option<&str>) -> Cell {
    Cell { value: v, formula: f.map(|s| s.into()) }
}

/// 被引用的工作簿：预算!A1=10 A2=20 A3=30 B1="预算" B2=A1+A2
fn make_source(dir: &Path) -> PathBuf {
    let p = dir.join("预算.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "预算".into();
    wb.sheets[0].set(0, 0, cell(Value::Number(10.0), None));
    wb.sheets[0].set(1, 0, cell(Value::Number(20.0), None));
    wb.sheets[0].set(2, 0, cell(Value::Number(30.0), None));
    wb.sheets[0].set(0, 1, cell(Value::Text("预算".into()), None));
    wb.sheets[0].set(1, 1, cell(Value::Number(30.0), Some("=A1+A2")));
    sheetcore::xlsx::save(&p, &wb).expect("写源文件");
    p
}

/// 引用方：汇总!A1..A5 全是 [1]预算!… 形式
fn make_main(dir: &Path, with_cache: bool) -> PathBuf {
    let p = dir.join("汇总.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "汇总".into();
    wb.sheets[0].set(0, 0, cell(Value::Number(10.0), Some("=[1]预算!A1")));
    wb.sheets[0].set(1, 0, cell(Value::Number(60.0), Some("=SUM([1]预算!A1:A3)")));
    wb.sheets[0].set(2, 0, cell(Value::Number(21.0), Some("=[1]预算!A1*2+1")));
    wb.sheets[0].set(3, 0, cell(Value::Text("预算元".into()), Some("=[1]预算!B1&\"元\"")));
    wb.sheets[0].set(4, 0, cell(Value::Number(30.0), Some("=[1]预算!B2")));
    sheetcore::xlsx::save(&p, &wb).expect("写引用方");

    // 补外部链接部件
    let cache = if with_cache {
        "<sheetDataSet><sheetData sheetId=\"0\">\
         <row r=\"1\"><cell r=\"A1\"><v>10</v></cell><cell r=\"B1\" t=\"str\"><v>预算</v></cell></row>\
         <row r=\"2\"><cell r=\"A2\"><v>20</v></cell><cell r=\"B2\"><v>30</v></cell></row>\
         <row r=\"3\"><cell r=\"A3\"><v>30</v></cell></row>\
         </sheetData></sheetDataSet>"
    } else {
        "<sheetDataSet/>"
    };
    let link = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <externalLink xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"\
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
         <externalBook r:id=\"rId1\"><sheetNames><sheetName val=\"预算\"/></sheetNames>{cache}\
         </externalBook></externalLink>"
    );
    let link_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" \
        Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath\" \
        Target=\"预算.xlsx\" TargetMode=\"External\"/></Relationships>";

    sheetcore::patch::append_entry(&p, "xl/externalLinks/externalLink1.xml", link.as_bytes()).unwrap();
    sheetcore::patch::append_entry(
        &p,
        "xl/externalLinks/_rels/externalLink1.xml.rels",
        link_rels.as_bytes(),
    )
    .unwrap();

    // workbook.xml：加 externalReferences（schema 里在 sheets 之后）
    let wbxml = part(&p, "xl/workbook.xml");
    let rid = format!("rId{}", next_rid(&part(&p, "xl/_rels/workbook.xml.rels")));
    let insert = format!(
        "<externalReferences><externalReference r:id=\"{rid}\"/></externalReferences>"
    );
    let new_wb = if let Some(i) = wbxml.find("<calcPr") {
        format!("{}{}{}", &wbxml[..i], insert, &wbxml[i..])
    } else if let Some(i) = wbxml.find("</workbook>") {
        format!("{}{}{}", &wbxml[..i], insert, &wbxml[i..])
    } else {
        panic!("workbook.xml 结构不对");
    };
    assert!(new_wb.contains("<externalReferences>"));
    sheetcore::patch::append_entry(&p, "xl/workbook.xml", new_wb.as_bytes()).unwrap();

    // workbook.xml.rels：加一条 externalLink 关系
    let rels = part(&p, "xl/_rels/workbook.xml.rels");
    let add = format!(
        "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink\" Target=\"externalLinks/externalLink1.xml\"/>"
    );
    let i = rels.find("</Relationships>").expect("rels 结尾");
    let new_rels = format!("{}{}{}", &rels[..i], add, &rels[i..]);
    sheetcore::patch::append_entry(&p, "xl/_rels/workbook.xml.rels", new_rels.as_bytes()).unwrap();

    // [Content_Types].xml：Override
    let ct = part(&p, "[Content_Types].xml");
    let ov = "<Override PartName=\"/xl/externalLinks/externalLink1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml\"/>";
    let i = ct.find("</Types>").expect("Types 结尾");
    let new_ct = format!("{}{}{}", &ct[..i], ov, &ct[i..]);
    sheetcore::patch::append_entry(&p, "[Content_Types].xml", new_ct.as_bytes()).unwrap();
    p
}

fn part(path: &Path, name: &str) -> String {
    let b = sheetcore::fixups::read_part_file(path, name)
        .unwrap_or_else(|| panic!("包里没有 {name}"));
    String::from_utf8(b).unwrap()
}

fn next_rid(rels: &str) -> u32 {
    let mut max = 0u32;
    let mut rest = rels;
    while let Some(i) = rest.find("Id=\"rId") {
        let after = &rest[i + 7..];
        let end = after.find('"').unwrap_or(0);
        if let Ok(n) = after[..end].parse::<u32>() {
            max = max.max(n);
        }
        rest = &after[end..];
    }
    max + 1
}

fn value(wb: &Workbook, r: u32) -> String {
    wb.sheets[0].get(r, 0).map(|c| c.value.display()).unwrap_or_default()
}

#[test]
fn reads_links_and_evaluates_from_cache() {
    let dir = tmpdir("cache");
    let src = make_source(&dir);
    let main = make_main(&dir, true);
    // 缓存路径不依赖源文件：删掉也能算
    let _ = std::fs::remove_file(&src);

    let mut wb = sheetcore::xlsx::load(&main).expect("读入");
    assert_eq!(wb.links.len(), 1, "应读到一个外部链接");
    assert_eq!(wb.links[0].index, 1);
    assert_eq!(wb.links[0].sheets, vec!["预算".to_string()]);
    assert!(wb.links[0].target.ends_with("预算.xlsx"), "target={}", wb.links[0].target);
    assert!(!wb.links[0].cache.is_empty(), "缓存应读进来");

    // 单格 + 范围 + 运算 + 文本拼接
    assert_eq!(
        sheetcore::formula::eval_formula(&wb, 0, "=[1]预算!A1").display(),
        "10"
    );
    assert_eq!(
        sheetcore::formula::eval_formula(&wb, 0, "=[预算.xlsx]预算!A1").display(),
        "10",
        "按文件名写也应该认"
    );
    sheetcore::formula::recalc_all(&mut wb);
    assert_eq!(value(&wb, 0), "10");
    assert_eq!(value(&wb, 1), "60");
    assert_eq!(value(&wb, 2), "21");
    assert_eq!(value(&wb, 3), "预算元");
    assert_eq!(value(&wb, 4), "30");
}

#[test]
fn refresh_reads_source_and_writes_cache_back() {
    let dir = tmpdir("refresh");
    let _src = make_source(&dir);
    let main = make_main(&dir, false);
    let out = dir.join("汇总_刷新.xlsx");

    let mut wb = sheetcore::xlsx::load(&main).expect("读入");
    // 没有缓存时算不了
    assert_eq!(
        sheetcore::formula::eval_formula(&wb, 0, "=[1]预算!A1").display(),
        "#REF!"
    );

    let rep = sheetcore::xlinks::refresh(&mut wb);
    assert_eq!(rep.ok, 1, "刷新失败: {:?}", rep.failed);
    assert!(wb.links[0].refreshed);

    sheetcore::formula::recalc_all(&mut wb);
    assert_eq!(value(&wb, 0), "10");
    assert_eq!(value(&wb, 1), "60");
    assert_eq!(value(&wb, 3), "预算元");

    // 存盘：刷新过的链接要重写 sheetDataSet
    wb.path = Some(main.clone());
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let xml = part(&out, "xl/externalLinks/externalLink1.xml");
    assert!(xml.contains("<v>10</v>"), "缓存没写回: {xml}");
    assert!(xml.contains("<v>60</v>") || xml.contains("<v>20</v>"), "{xml}");
    assert!(xml.contains("<sheetName val=\"预算\"/>"), "表名不能丢");
    assert_eq!(xml.matches("<sheetDataSet").count(), 1, "不该重复: {xml}");

    // 重开：缓存已经在文件里，直接可算
    let back = sheetcore::xlsx::load(&out).expect("重开");
    assert_eq!(back.links.len(), 1);
    assert_eq!(
        sheetcore::formula::eval_formula(&back, 0, "=[1]预算!A1").display(),
        "10"
    );
}

#[test]
fn missing_source_keeps_file_cache() {
    let dir = tmpdir("missing");
    let _ = make_source(&dir);
    let main = make_main(&dir, true);
    let _ = std::fs::remove_file(dir.join("预算.xlsx"));

    let mut wb = sheetcore::xlsx::load(&main).expect("读入");
    let rep = sheetcore::xlinks::refresh(&mut wb);
    assert_eq!(rep.ok, 0);
    assert_eq!(rep.failed.len(), 1, "应报告文件缺失");
    // 刷新失败不影响缓存求值
    sheetcore::formula::recalc_all(&mut wb);
    assert_eq!(value(&wb, 0), "10");
    assert_eq!(value(&wb, 1), "60");
}

#[test]
fn untouched_file_keeps_link_parts() {
    // 不刷新就存盘：externalLink 部件字节不变
    let dir = tmpdir("untouched");
    let _ = make_source(&dir);
    let main = make_main(&dir, true);
    let out = dir.join("汇总_原样.xlsx");
    let before = part(&main, "xl/externalLinks/externalLink1.xml");

    let mut wb = sheetcore::xlsx::load(&main).expect("读入");
    wb.path = Some(main.clone());
    wb.sheets[0].set(9, 9, cell(Value::Text("改一格".into()), None));
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    let after = part(&out, "xl/externalLinks/externalLink1.xml");
    assert_eq!(before, after, "没刷新就不该动 externalLink 部件");
    let back = sheetcore::xlsx::load(&out).expect("重开");
    assert_eq!(back.links.len(), 1);
}
#[test]
fn typed_formula_creates_link_and_parts() {
    let dir = tmpdir("typed");
    let _ = make_source(&dir);
    let path = dir.join("手输.xlsx");

    let mut wb = Workbook::new_blank();
    wb.path = Some(path.clone());
    let f = "=[预算.xlsx]预算!A1";
    wb.sheets[0].set(0, 0, cell(Value::Number(10.0), Some(f)));

    // 手输公式 → 自动建链接并读到缓存（和 Excel 的行为一致）
    assert_eq!(sheetcore::xlinks::absorb_formula(&mut wb, f), 1);
    assert_eq!(wb.links.len(), 1);
    assert!(!wb.links[0].cache.is_empty());
    assert_eq!(
        sheetcore::formula::eval_formula(&wb, 0, "=[预算.xlsx]预算!A1").display(),
        "10"
    );

    // 存盘前把文件名折回 [N]（界面提交时就是这么做的）
    let raw = sheetcore::xlinks::raw_formula(&wb, f);
    assert_eq!(raw, "=[1]预算!A1");
    wb.sheets[0].set(0, 0, cell(Value::Number(10.0), Some(&raw)));
    sheetcore::xlsx::save(&path, &wb).expect("保存");

    // 包里：新部件 + workbook 引用 + CT + [1] 公式
    let link = part(&path, "xl/externalLinks/externalLink1.xml");
    assert!(link.contains("<sheetName val=\"预算\"/>"), "{link}");
    assert!(link.contains("<v>10</v>"), "缓存应写进新部件: {link}");
    let wbxml = part(&path, "xl/workbook.xml");
    assert!(wbxml.contains("<externalReferences>"), "{wbxml}");
    assert!(part(&path, "[Content_Types].xml").contains("externalLink+xml"));
    let sheet = part(&path, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains("<f>[1]预算!A1</f>"), "公式要写成 [N] 形式: {sheet}");

    // 重开：链接、表名、缓存、求值全对
    let back = sheetcore::xlsx::load(&path).expect("重开");
    assert_eq!(back.links.len(), 1);
    assert_eq!(back.links[0].sheets, vec!["预算".to_string()]);
    assert_eq!(
        sheetcore::formula::eval_formula(&back, 0, "=[1]预算!A1").display(),
        "10"
    );

    // 再存一次不能重复加引用
    let out2 = dir.join("手输2.xlsx");
    let mut wb2 = back;
    wb2.path = Some(path.clone());
    sheetcore::xlsx::save(&out2, &wb2).expect("二次保存");
    let wbxml2 = part(&out2, "xl/workbook.xml");
    assert_eq!(
        wbxml2.matches("<externalReference ").count(),
        1,
        "externalReference 重复了: {wbxml2}"
    );
}

