//! C14 第二轮：保存门禁要覆盖所有入口、抢救副本要保住完好表的结构、
//! 只对适用格式跑扫描器、加密包同样保留诊断。
//!
//! 每个用例都核对原文件的 **SHA-256**，不是长度。

use sha2::{Digest, Sha256};
use sheetcore::formula::ErrKind;
use sheetcore::model::{Cell, CellRef, Value, Workbook};
use sheetcore::style::CellStyle;

fn sha256(p: &std::path::Path) -> String {
    let bytes = std::fs::read(p).unwrap();
    Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("sc_c14g_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn part_of(p: &std::path::Path, sheet: &str) -> String {
    sheetcore::patch::sheet_targets(p)
        .unwrap()
        .get(sheet)
        .cloned()
        .unwrap_or_else(|| panic!("找不到 {sheet} 的部件"))
}

/// 造一个「坏表 A + 完好表 B」的包，返回路径与损坏前的 SHA
fn two_sheet_package(d: &std::path::Path) -> (std::path::PathBuf, String) {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "坏表".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(2.0), formula: None });
    let mut money = CellStyle::default();
    money.format = "#,##0.00".into();
    money.bold = true;
    let id = wb.styles.intern(money);
    let mut datefmt = CellStyle::default();
    datefmt.format = "yyyy-mm-dd".into();
    let did = wb.styles.intern(datefmt);
    wb.sheets.push(sheetcore::model::Sheet::new("完好表"));
    let b = wb.sheets.last_mut().unwrap();
    b.set(0, 0, Cell { value: Value::Number(1234.5), formula: None });
    b.set_style(0, 0, id);
    b.set(1, 0, Cell { value: Value::Number(45000.0), formula: None });
    b.set_style(1, 0, did);
    b.merge_range(1, 0, 1, 2);
    // 空白但有填充的格（不在 sheet.iter 里，必须从样式表枚举才不丢）
    let mut fill = CellStyle::default();
    fill.fill = Some([255, 235, 156]);
    let fid = wb.styles.intern(fill);
    b.set_style(4, 1, fid);
    // 列宽 / 行高 / 隐藏 / 保护 / 页面设置：载入时都是 dirty=false
    b.cols.set(3, 30.0);
    b.rows.set(5, 40.0);
    b.hidden_cols.insert(4);
    b.hidden_rows_user.insert(6);
    b.protection = Some(sheetcore::protection::SheetProtection::default());
    b.page.landscape = true;
    b.page.paper = 9;
    b.notes.insert(
        CellRef::new(3, 4),
        sheetcore::notes::Note { r: 3, c: 4, text: "保留这条批注".into(), author: "tester".into() },
    );
    let p = d.join("mixed.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    // 打坏第一张表：截断 sheetData
    let part = part_of(&p, "坏表");
    sheetcore::fixups::write_parts(&p, &[(part, b"<worksheet><sheetData><row r=\"1\">".to_vec())]).unwrap();
    // 损坏之后才是"原文件"的基准哈希
    let before = sha256(&p);
    (p, before)
}

#[test]
fn core_save_refuses_to_overwrite_a_fatally_damaged_source() {
    let d = dir("core_guard");
    let (p, before) = two_sheet_package(&d);
    let (wb, diag) = sheetcore::xlsx::load_diag(&p).unwrap();
    assert!(diag.has_fatal(), "损坏的包必须报致命问题");
    assert!(wb.load_incomplete);

    // 直接走核心保存接口（界面门禁之外的所有入口都经过这里）
    let err = sheetcore::xlsx::save_atomic_with_receipt(&p, &wb).expect_err("核心保存必须拒绝覆盖");
    let msg = format!("{err:#}");
    assert!(msg.contains("拒绝覆盖"), "{msg}");
    assert_eq!(sha256(&p), before, "原文件一个字节都不能变");

    // 普通 save 同样拒绝
    let err2 = sheetcore::xlsx::save(&p, &wb).expect_err("普通保存也必须拒绝");
    assert!(format!("{err2:#}").contains("拒绝覆盖"));
    assert_eq!(sha256(&p), before);
}

#[test]
fn rescue_copy_keeps_formats_merges_and_notes_of_the_healthy_sheet() {
    let d = dir("rescue");
    let (p, before) = two_sheet_package(&d);
    let (wb, diag) = sheetcore::xlsx::load_diag(&p).unwrap();
    assert!(diag.has_fatal());

    let rescue = d.join("rescue.xlsx");
    sheetcore::xlsx::save(&rescue, &wb).expect("另存抢救副本应允许");
    assert_eq!(sha256(&p), before, "原文件保持不动");

    let back = sheetcore::xlsx::load(&rescue).expect("抢救副本必须能打开");
    let b = back.sheets.iter().find(|s| s.name == "完好表").expect("完好表要保留");
    // 金额：值 + 数字格式 + 加粗
    assert_eq!(b.get(0, 0).map(|c| c.value.display()), Some("1234.5".to_string()));
    let st = back.styles.get(b.style_of(0, 0));
    assert_eq!(st.format, "#,##0.00", "数字格式必须保住");
    assert!(st.bold, "加粗必须保住");
    // 日期格式
    assert_eq!(back.styles.get(b.style_of(1, 0)).format, "yyyy-mm-dd");
    // 合并
    assert!(b.merges.contains(&(1, 0, 1, 2)), "合并区域必须保住：{:?}", b.merges);
    // 批注
    assert_eq!(
        b.notes.get(&CellRef::new(3, 4)).map(|n| n.text.as_str()),
        Some("保留这条批注"),
        "批注必须保住：{:?}",
        b.notes
    );
    // 空白填充格
    assert_eq!(back.styles.get(b.style_of(4, 1)).fill, Some([255, 235, 156]), "空白填充格必须保住");
    // 列宽 / 行高 / 隐藏
    assert!((b.cols.size(3) - 30.0).abs() < 0.6, "D 列宽应保住：{}", b.cols.size(3));
    assert!((b.rows.size(5) - 40.0).abs() < 0.6, "第 6 行高应保住：{}", b.rows.size(5));
    assert!(b.hidden_cols.contains(&4), "隐藏列必须保住：{:?}", b.hidden_cols);
    assert!(b.hidden_rows_user.contains(&6), "隐藏行必须保住：{:?}", b.hidden_rows_user);
    // 保护与页面设置
    assert!(b.protection.is_some(), "工作表保护必须保住");
    assert!(b.page.landscape, "页面方向必须保住");
    assert_eq!(b.page.paper, 9, "纸张大小必须保住");
}

#[test]
fn rescue_copy_of_an_encrypted_damaged_source_does_not_reuse_the_bad_package() {
    let d = dir("enc");
    let (p, _) = two_sheet_package(&d);
    // 把坏包整体加密成带密码文件
    let plain = std::fs::read(&p).unwrap();
    let enc = d.join("locked.xlsx");
    sheetcore::crypto::encrypt(&enc, &plain, &sheetcore::crypto::EncryptionSettings::new("pw-123"))
        .unwrap();
    let before = sha256(&enc);

    let (wb, diag) = sheetcore::xlsx::load_with_password_diag(&enc, "pw-123").expect("应能解密打开");
    assert!(diag.has_fatal(), "加密包里坏掉的表也要报出来：{:?}", diag.problems);
    assert!(wb.load_incomplete);

    let err = sheetcore::xlsx::save(&enc, &wb).expect_err("加密原件也不许覆盖");
    assert!(format!("{err:#}").contains("拒绝覆盖"), "{err:#}");
    assert_eq!(sha256(&enc), before, "加密原件一字不动");

    let rescue = d.join("locked_rescue.xlsx");
    sheetcore::xlsx::save(&rescue, &wb).expect("另存抢救副本应允许");
    // 另存件沿用原簿的密码，读回也要带密码
    let (back, d2) = sheetcore::xlsx::load_with_password_diag(&rescue, "pw-123")
        .expect("抢救副本能打开");
    assert!(!d2.has_fatal(), "抢救副本不应继承诊断：{:?}", d2.problems);
    assert!(back.sheets.iter().any(|s| s.name == "完好表"));
}

#[test]
fn healthy_biff_xls_has_no_fatal_diag_and_can_be_saved() {
    let d = dir("xls");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(42.0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("文本".into()), formula: None });
    let p = d.join("plain.xls");
    sheetcore::xls_write::write(&p, &wb).expect("写 BIFF");

    let (back, diag) = sheetcore::xlsx::load_diag(&p).expect("正常 xls 应能打开");
    assert!(!diag.has_fatal(), "正常 xls 不能被 zip 扫描器误判：{:?}", diag.problems);
    assert_eq!(back.sheets[0].get(0, 0).map(|c| c.value.display()), Some("42".to_string()));

    let out = d.join("plain_out.xls");
    sheetcore::xlsx::save_atomic(&out, &back).expect("xls 另存应成功");
    let again = sheetcore::xlsx::load(&out).expect("另存件应能打开");
    assert_eq!(again.sheets[0].get(0, 0).map(|c| c.value.display()), Some("42".to_string()));
}

/// 稀疏大表：声明区域巨大 → 走 sparse::read_sheet；XML 坏了必须标致命
#[test]
fn truncated_sparse_sheet_is_fatal_not_partial() {
    let d = dir("sparse");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "大表".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(7.0), formula: None });
    let p = d.join("sparse.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    // 声明区域撑到整表（> DENSE_LIMIT），再截断
    let part = part_of(&p, "大表");
    let xml = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:XFD1048576"/><sheetData><row r="1"><c r="A1"><v>7</v></c></row><row r="2"><c r="A2">"#;
    sheetcore::fixups::write_parts(&p, &[(part, xml.as_bytes().to_vec())]).unwrap();

    let (_wb, diag) = sheetcore::xlsx::load_diag(&p).expect("仍应能打开并给出诊断");
    assert!(
        diag.problems.iter().any(|p| p.fatal && p.what.contains("稀疏")),
        "稀疏解析失败必须报致命：{:?}",
        diag.problems
    );
}

/// 稀疏表引用了不存在的共享字符串 → 不能静默变空值
#[test]
fn sparse_sheet_with_missing_shared_strings_is_fatal() {
    let d = dir("sst");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "大表".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("甲".into()), formula: None });
    let p = d.join("sst.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let part = part_of(&p, "大表");
    let xml = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:XFD1048576"/><sheetData><row r="1"><c r="A1" t="s"><v>5</v></c></row></sheetData></worksheet>"#;
    let mut parts = vec![(part, xml.as_bytes().to_vec())];
    if let Some(ct) = sheetcore::fixups::read_part(&p, "[Content_Types].xml") {
        let mut ct = String::from_utf8(ct).unwrap();
        // 删掉 sharedStrings 的内容类型声明，制造"部件缺失"
        if let Some(i) = ct.find("<Override PartName=\"/xl/sharedStrings.xml\"") {
            if let Some(end) = ct[i..].find("/>") {
                ct.replace_range(i..i + end + 2, "");
            }
        }
        parts.push(("[Content_Types].xml".into(), ct.into_bytes()));
    }
    sheetcore::fixups::write_parts(&p, &parts).unwrap();
    // 真正删掉部件
    sheetcore::patch::remove_entry(&p, "xl/sharedStrings.xml").ok();

    let (_wb, diag) = sheetcore::xlsx::load_diag(&p).expect("应能打开");
    assert!(
        diag.problems
            .iter()
            .any(|p| p.fatal && (p.what.contains("稀疏") || p.what.contains("共享字符串"))),
        "缺 SST 且被引用时必须致命：{:?}",
        diag.problems
    );
    let _ = ErrKind::Na; // 保持导入被使用，避免未使用告警
}

#[test]
fn deleting_the_last_note_removes_it_from_the_saved_file() {
    let d = dir("notes");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].notes.insert(
        CellRef::new(0, 0),
        sheetcore::notes::Note { r: 0, c: 0, text: "旧的".into(), author: "t".into() },
    );
    wb.sheets[0].notes_dirty = true;
    let p = d.join("notes.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();
    assert_eq!(sheetcore::xlsx::load(&p).unwrap().sheets[0].notes.len(), 1);

    // 删掉最后一条批注再保存（正常保存，有基准）
    let mut wb = sheetcore::xlsx::load(&p).unwrap();
    wb.sheets[0].notes.clear();
    wb.sheets[0].notes_dirty = true;
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let back = sheetcore::xlsx::load(&p).unwrap();
    assert!(back.sheets[0].notes.is_empty(), "删掉最后一条批注后不能从旧部件里复活：{:?}", back.sheets[0].notes);
}

/// drawing 关系指向一个不存在的 chart 部件：图表丢了必须报出来，不能安静地给空列表。
#[test]
fn drawing_pointing_at_a_missing_chart_is_reported() {
    let d = dir("chart_missing");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    let p = d.join("chart.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let part = part_of(&p, "S");

    // 工作表里挂一个 drawing
    let xml = String::from_utf8(sheetcore::fixups::read_part(&p, &part).unwrap()).unwrap();
    let xml = xml.replacen(
        "</worksheet>",
        "<drawing r:id=\"rIdDraw\"/></worksheet>",
        1,
    );
    let sheet_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdDraw\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/drawing1.xml\"/></Relationships>";
    let drawing = "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:to><xdr:col>4</xdr:col><xdr:row>8</xdr:row></xdr:to><xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id=\"rId1\"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>";
    let drawing_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" Target=\"../charts/chart1.xml\"/></Relationships>";
    let rels_part = {
        let (dirp, file) = part.rsplit_once('/').unwrap();
        format!("{dirp}/_rels/{file}.rels")
    };
    sheetcore::fixups::write_parts(
        &p,
        &[
            (part.clone(), xml.into_bytes()),
            (rels_part, sheet_rels.as_bytes().to_vec()),
            ("xl/drawings/drawing1.xml".into(), drawing.as_bytes().to_vec()),
            ("xl/drawings/_rels/drawing1.xml.rels".into(), drawing_rels.as_bytes().to_vec()),
        ],
    )
    .unwrap();

    let (_wb, diag) = sheetcore::xlsx::load_diag(&p).expect("应能打开");
    assert!(
        diag.problems.iter().any(|x| x.fatal && x.what.contains("图表")),
        "缺 chart1.xml 必须报致命图表问题：{:?}",
        diag.problems
    );
}

/// 删除批注部件时重建的关系 XML 必须良构：根 <Relationships> 不能被当关系删掉。
fn rels_after_clear(first_is_comments: bool) {
    let d = dir(if first_is_comments { "notes_first" } else { "notes_mixed" });
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].notes.insert(
        CellRef::new(0, 0),
        sheetcore::notes::Note { r: 0, c: 0, text: "批注".into(), author: "t".into() },
    );
    wb.sheets[0].notes_dirty = true;
    let p = d.join("n.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let part = part_of(&p, "S");
    let rels_part = {
        let (dirp, file) = part.rsplit_once('/').unwrap();
        format!("{dirp}/_rels/{file}.rels")
    };
    let orig = String::from_utf8(sheetcore::fixups::read_part(&p, &rels_part).unwrap()).unwrap();
    // 造出「首个关系就是批注」/「批注和其他关系共存」两种排布
    let comments_rel = orig
        .split("<Relationship ")
        .find(|x| x.contains("/comments"))
        .map(|x| format!("<Relationship {}/>", x.split("/>").next().unwrap()))
        .expect("应有 comments 关系");
    let other = "<Relationship Id=\"rIdKeep\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"https://example.com/\" TargetMode=\"External\"/>";
    let head = orig.split("<Relationship ").next().unwrap();
    let rebuilt = if first_is_comments {
        format!("{head}{comments_rel}{other}</Relationships>")
    } else {
        format!("{head}{other}{comments_rel}</Relationships>")
    };
    sheetcore::fixups::write_parts(&p, &[(rels_part.clone(), rebuilt.clone().into_bytes())]).unwrap();

    // 删空批注再保存
    let mut wb = sheetcore::xlsx::load(&p).unwrap();
    wb.sheets[0].notes.clear();
    wb.sheets[0].notes_dirty = true;
    sheetcore::xlsx::save(&p, &wb).unwrap();
    assert!(sheetcore::xlsx::load(&p).unwrap().sheets[0].notes.is_empty());

    let out = String::from_utf8(sheetcore::fixups::read_part(&p, &rels_part).unwrap()).unwrap();
    let doc = roxmltree::Document::parse(&out).expect("关系部件必须仍是合法 XML");
    assert!(doc.root_element().has_tag_name("Relationships"), "根必须是 Relationships");
    assert!(
        doc.root_element().tag_name().namespace() == Some("http://schemas.openxmlformats.org/package/2006/relationships"),
        "根必须保留关系命名空间：{out}"
    );
    let keep = doc
        .descendants()
        .find(|n| n.has_tag_name("Relationship") && n.attribute("Id") == Some("rIdKeep"))
        .expect("保留的关系要在：{out}");
    assert_eq!(
        keep.attribute("TargetMode"),
        Some("External"),
        "外链关系的 TargetMode 必须保留：{out}"
    );
    assert_eq!(
        keep.attribute("Target"),
        Some("https://example.com/"),
        "外链 URL 不能被当作包内路径归一：{out}"
    );
    let types: Vec<String> = doc
        .descendants()
        .filter(|n| n.has_tag_name("Relationship"))
        .filter_map(|n| n.attribute("Type").map(|t| t.to_string()))
        .collect();
    assert!(!types.iter().any(|t| t.ends_with("/comments") || t.ends_with("/vmlDrawing")), "批注关系应删掉：{types:?}");
    assert!(types.iter().any(|t| t.ends_with("/hyperlink")), "其他关系必须保留：{types:?}");
    let ids: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("Relationship"))
        .filter_map(|n| n.attribute("Id"))
        .collect();
    assert_eq!(ids, vec!["rIdKeep"], "删除后只剩保留的关系：{ids:?}");
}

#[test]
fn clearing_notes_keeps_rel_xml_well_formed_when_comments_rels_is_first() {
    rels_after_clear(true);
}

#[test]
fn clearing_notes_keeps_other_relationships_when_comments_rels_is_not_first() {
    rels_after_clear(false);
}

/// 三种窄夹具：drawing 缺关系部件 / 缺 rId / chart XML 截断。
fn chart_fixture(tag: &str, d_rels: Option<&str>, chart: Option<&str>) {
    let d = dir(tag);
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    let p = d.join("c.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let part = part_of(&p, "S");
    let xml = String::from_utf8(sheetcore::fixups::read_part(&p, &part).unwrap()).unwrap();
    let xml = xml.replacen("</worksheet>", "<drawing r:id=\"rIdDraw\"/></worksheet>", 1);
    let sheet_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdDraw\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/drawing1.xml\"/></Relationships>";
    let drawing = "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:to><xdr:col>4</xdr:col><xdr:row>8</xdr:row></xdr:to><xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id=\"rId1\"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>";
    let rels_part = {
        let (dirp, file) = part.rsplit_once('/').unwrap();
        format!("{dirp}/_rels/{file}.rels")
    };
    let mut parts: Vec<(String, Vec<u8>)> = vec![
        (part.clone(), xml.into_bytes()),
        (rels_part, sheet_rels.as_bytes().to_vec()),
        ("xl/drawings/drawing1.xml".into(), drawing.as_bytes().to_vec()),
    ];
    if let Some(r) = d_rels {
        parts.push(("xl/drawings/_rels/drawing1.xml.rels".into(), r.as_bytes().to_vec()));
    }
    if let Some(c) = chart {
        parts.push(("xl/charts/chart1.xml".into(), c.as_bytes().to_vec()));
    }
    sheetcore::fixups::write_parts(&p, &parts).unwrap();
    let (_wb, diag) = sheetcore::xlsx::load_diag(&p).expect("应能打开");
    assert!(
        diag.problems.iter().any(|x| x.fatal && x.what.contains("图表")),
        "{tag}: 必须报致命图表问题：{:?}",
        diag.problems
    );
}

#[test]
fn drawing_without_rels_is_reported() {
    chart_fixture("chart_no_rels", None, None);
}

#[test]
fn drawing_rid_missing_from_rels_is_reported() {
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdOther\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" Target=\"../charts/chart1.xml\"/></Relationships>";
    let chart = "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart><c:plotArea><c:barChart/></c:plotArea></c:chart></c:chartSpace>";
    chart_fixture("chart_missing_rid", Some(rels), Some(chart));
}

#[test]
fn truncated_chart_xml_after_recognized_type_is_reported() {
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" Target=\"../charts/chart1.xml\"/></Relationships>";
    // 已经出现 barChart（parse_chart 会认出来），但后面被截断
    let chart = "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart><c:plotArea><c:barChart>";
    chart_fixture("chart_truncated", Some(rels), Some(chart));
}

/// 真实链路：首次保存带外链 → 重新打开改目标 → 再保存，必须写新 URL。
/// 保存末尾的"恢复原文"只允许修引擎归一化，不能把用户的新链接换回旧的。
#[test]
fn editing_an_existing_external_link_persists_the_new_target() {
    let d = std::env::temp_dir().join(format!("sc_c14_link_edit_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    let p = d.join("link.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "链接".into();
    wb.sheets[0].set(0, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("old".into()), formula: None });
    wb.sheets[0].hyperlinks.insert(
        (0, 0),
        sheetcore::hyperlinks::Hyperlink {
            target: "https://old.example/a".into(),
            location: String::new(),
            tooltip: "旧".into(),
            external: true,
        },
    );
    wb.sheets[0].hyperlinks_dirty = true;
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let rels = {
        let part = sheetcore::patch::sheet_targets(&p).unwrap()["链接"].clone();
        let (dd, ff) = part.rsplit_once('/').unwrap();
        format!("{dd}/_rels/{ff}.rels")
    };
    let first = String::from_utf8(sheetcore::fixups::read_part(&p, &rels).unwrap()).unwrap();
    assert!(first.contains("https://old.example/a"), "首次保存应写旧链接：{first}");

    let mut wb2 = sheetcore::xlsx::load(&p).unwrap();
    let link = wb2.sheets[0].hyperlinks.get_mut(&(0, 0)).expect("链接应读回");
    assert_eq!(link.target, "https://old.example/a");
    link.target = "https://new.example/b".into();
    wb2.sheets[0].hyperlinks_dirty = true;
    sheetcore::xlsx::save(&p, &wb2).unwrap();

    let second = String::from_utf8(sheetcore::fixups::read_part(&p, &rels).unwrap()).unwrap();
    assert!(second.contains("https://new.example/b"), "用户改的新链接必须落盘：{second}");
    assert!(!second.contains("old.example"), "不能把新链接恢复成旧值：{second}");
    assert!(second.contains("TargetMode=\"External\""), "外链 TargetMode 要保留：{second}");
    let doc = roxmltree::Document::parse(&second).expect("rels 必须良构");
    assert_eq!(
        doc.root_element().tag_name().namespace(),
        Some("http://schemas.openxmlformats.org/package/2006/relationships")
    );
    // 再打开读回模型
    let wb3 = sheetcore::xlsx::load(&p).unwrap();
    assert_eq!(wb3.sheets[0].hyperlinks[&(0, 0)].target, "https://new.example/b");
}
