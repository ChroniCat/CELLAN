//! 真正的 Excel 查询表（QueryTable）。
//!
//! 让 Excel 自己认得"这是一张外部数据表"，在「数据 → 查询和连接」里能看到，
//! 也能点刷新。部件形状是照着 Excel 2016 实际写出来的文件对齐的：
//!
//! - `xl/connections.xml`：连接定义（type=6 表示文本文件，sourceFile 指向 CSV）
//! - `xl/queryTables/queryTableN.xml`：查询表本体，靠 connectionId 挂到连接上
//! - 工作表 rels 里加一条 queryTable 关系
//! - `[Content_Types].xml` 两条 Override
//!
//! 注意：工作表 XML 里**没有** queryTable 元素，Excel 就是靠 rels 认的。

use anyhow::{Context, Result};
use std::path::Path;

use crate::fixups;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn next_index(existing: &[String], prefix: &str, suffix: &str) -> u32 {
    let mut max = 0;
    for n in existing {
        if let Some(rest) = n.strip_prefix(prefix) {
            if let Some(num) = rest.strip_suffix(suffix) {
                if let Ok(v) = num.parse::<u32>() {
                    max = max.max(v);
                }
            }
        }
    }
    max + 1
}

/// 给某个工作表挂一个指向 CSV 的查询表。
///
/// `at` 是落点左上角（0 基），`header` 表示首行是字段名。
pub fn attach_csv(
    path: &Path,
    sheet_name: &str,
    at: (u32, u32),
    rows: u32,
    cols: u32,
    csv: &Path,
    header: bool,
) -> Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(sheet_part) = targets.get(sheet_name).cloned() else {
        anyhow::bail!("找不到工作表 {sheet_name}");
    };

    // 已经挂过同一条连接就不再重复挂（每次保存都会走这里）
    if let Some(old) = fixups::read_part(path, "xl/connections.xml").and_then(|b| String::from_utf8(b).ok()) {
        let needle = format!("sourceFile=\"{}\"", esc(&csv.to_string_lossy().replace('/', "\\")));
        if old.contains(&needle) {
            return Ok(());
        }
    }

    let names = fixups::parts(path);
    // ---- 连接：接在已有 connections.xml 后面
    let conn_id = {
        let existing = fixups::read_part(path, "xl/connections.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        let ids: Vec<u32> = existing
            .match_indices("<connection ")
            .filter_map(|(i, _)| {
                let tail = &existing[i..];
                tail.find("id=\"").and_then(|p| {
                    let after = &tail[p + 4..];
                    after.split('"').next().and_then(|v| v.parse().ok())
                })
            })
            .collect();
        ids.into_iter().max().unwrap_or(0) + 1
    };

    // 连接里的路径要写反斜杠，Excel 的 textPr/@sourceFile 只认这种
    let sources = csv.to_string_lossy().replace('/', "\\");
    // Excel 无论有没有表头都会写这个空字段表；省掉它文件就打不开
    let _ = header;
    let text_fields = "<textFields><textField/></textFields>";
    let conn_xml = format!(
        "<connection id=\"{conn_id}\" name=\"Query{conn_id}\" type=\"6\" refreshedVersion=\"8\" background=\"1\" saveData=\"1\">\
<textPr prompt=\"0\" codePage=\"65001\" sourceFile=\"{file}\" comma=\"1\">{tf}</textPr></connection>",
        file = esc(&sources),
        tf = text_fields
    );

    let merged = match fixups::read_part(path, "xl/connections.xml").and_then(|b| String::from_utf8(b).ok()) {
        Some(old) if old.contains("</connections>") => old.replacen("</connections>", &format!("{conn_xml}</connections>"), 1),
        _ => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<connections xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">{conn_xml}</connections>"
        ),
    };

    // ---- 查询表本体
    let qt_no = next_index(&names, "xl/queryTables/queryTable", ".xml");
    let qt_part = format!("xl/queryTables/queryTable{qt_no}.xml");
    let qt_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
<queryTable xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" name=\"ExternalData_{qt_no}\" connectionId=\"{conn_id}\" \
autoFormatId=\"0\" applyNumberFormats=\"0\" applyBorderFormats=\"0\" applyFontFormats=\"1\" applyPatternFormats=\"1\" \
applyAlignmentFormats=\"0\" applyWidthHeightFormats=\"0\"/>"
    );

    // ---- 工作表 rels：加一条 queryTable 关系（工作表 XML 里不用写东西）
    let rels_part = format!(
        "{}/_rels/{}.rels",
        sheet_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("xl"),
        sheet_part.rsplit_once('/').map(|(_, f)| f).unwrap_or("sheet1.xml")
    );
    let (rels_xml, rel_id) = match fixups::read_part(path, &rels_part).and_then(|b| String::from_utf8(b).ok()) {
        Some(old) => {
            let max_id = old
                .match_indices("Id=\"rId")
                .filter_map(|(i, _)| {
                    let after = &old[i + 7..];
                    after.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok()
                })
                .max()
                .unwrap_or(0);
            let id = format!("rId{}", max_id + 1);
            let rel = format!(
                "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/queryTable\" Target=\"../queryTables/queryTable{qt_no}.xml\"/>"
            );
            let out = match old.rfind("</Relationships>") {
                Some(i) => format!("{}{}{}", &old[..i], rel, &old[i..]),
                None => old,
            };
            (out, id)
        }
        None => {
            let id = "rId1".to_string();
            (
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/queryTable\" Target=\"../queryTables/queryTable{qt_no}.xml\"/></Relationships>"
                ),
                id,
            )
        }
    };
    let _ = (at, rel_id);

    // ---- 工作簿 rels：必须有一条 connections 关系，否则 Excel 打不开
    let wrels_part = "xl/_rels/workbook.xml.rels";
    let wrels = fixups::read_part(path, wrels_part)
        .and_then(|b| String::from_utf8(b).ok())
        .context("没有 workbook.xml.rels")?;
    let wrels_out = if wrels.contains("/relationships/connections") {
        wrels
    } else {
        let max_id = wrels
            .match_indices("Id=\"rId")
            .filter_map(|(i, _)| {
                let after = &wrels[i + 7..];
                after.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0);
        let rel = format!(
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/connections\" Target=\"connections.xml\"/>",
            max_id + 1
        );
        match wrels.rfind("</Relationships>") {
            Some(i) => format!("{}{}{}", &wrels[..i], rel, &wrels[i..]),
            None => wrels.clone(),
        }
    };

    // ---- 目标区域要有一条定义名称，Excel 靠它知道刷新写哪儿
    let wb_xml = fixups::read_part(path, "xl/workbook.xml")
        .and_then(|b| String::from_utf8(b).ok())
        .context("没有 workbook.xml")?;
    let local_idx = wb_xml
        .match_indices("<sheet ")
        .enumerate()
        .find_map(|(i, (pos, _))| {
            let tail = &wb_xml[pos..];
            let end = tail.find('>')?;
            let tag = &tail[..end];
            let name_at = tag.find("name=\"")?;
            let name = &tag[name_at + 6..];
            let name = &name[..name.find('"')?];
            if name == sheet_name { Some(i as u32) } else { None }
        })
        .unwrap_or(0);
    let col_name = |mut c: u32| -> String {
        let mut out = String::new();
        loop {
            out.insert(0, (b'A' + (c % 26) as u8) as char);
            if c < 26 {
                break;
            }
            c = c / 26 - 1;
        }
        out
    };
    let r2 = at.0 + rows.max(1) - 1;
    let c2 = at.1 + cols.max(1) - 1;
    let area = format!(
        "{}!${}${}:${}${}",
        sheet_name.replace(' ', "_"),
        col_name(at.1),
        at.0 + 1,
        col_name(c2),
        r2 + 1
    );
    let dn = format!(
        "<definedName name=\"{}\" localSheetId=\"{local_idx}\">{}</definedName>",
        esc(&format!("ExternalData_{qt_no}")),
        area
    );
    let wb_out = if wb_xml.contains("<definedNames />") {
        wb_xml.replacen("<definedNames />", &format!("<definedNames>{dn}</definedNames>"), 1)
    } else if wb_xml.contains("<definedNames/>") {
        wb_xml.replacen("<definedNames/>", &format!("<definedNames>{dn}</definedNames>"), 1)
    } else if wb_xml.contains("</definedNames>") {
        wb_xml.replacen("</definedNames>", &format!("{dn}</definedNames>"), 1)
    } else {
        match wb_xml.find("<calcPr") {
            Some(i) => format!("{}<definedNames>{dn}</definedNames>{}", &wb_xml[..i], &wb_xml[i..]),
            None => wb_xml.replacen("</workbook>", &format!("<definedNames>{dn}</definedNames></workbook>"), 1),
        }
    };
    fixups::write_parts(path, &[("xl/workbook.xml".to_string(), wb_out.into_bytes())])?;

    // ---- 一次写完
    fixups::write_parts(
        path,
        &[
            ("xl/connections.xml".to_string(), merged.into_bytes()),
            (qt_part.clone(), qt_xml.into_bytes()),
            (rels_part, rels_xml.into_bytes()),
            (wrels_part.to_string(), wrels_out.into_bytes()),
        ],
    )?;

    // ---- 内容类型
    let ct = fixups::read_part(path, "[Content_Types].xml")
        .and_then(|b| String::from_utf8(b).ok())
        .context("没有 [Content_Types].xml")?;
    let mut out = ct.clone();
    let mut add = |part: &str, ty: &str| {
        if !out.contains(part) {
            if let Some(i) = out.rfind("</Types>") {
                out.insert_str(
                    i,
                    &format!("<Override PartName=\"{part}\" ContentType=\"{ty}\"/>"),
                );
            }
        }
    };
    add(
        "/xl/connections.xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.connections+xml",
    );
    add(
        &format!("/{qt_part}"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.queryTable+xml",
    );
    if out != ct {
        fixups::write_parts(path, &[("[Content_Types].xml".to_string(), out.into_bytes())])?;
    }
    Ok(())
}
