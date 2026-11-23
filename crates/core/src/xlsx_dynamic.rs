//! SpreadsheetML cell metadata distinguishes modern spills from fixed CSE arrays.
use anyhow::{Context, Result, ensure};
use roxmltree::Document;
use std::{collections::HashSet, fs::File};

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const XDA: &str = "http://schemas.microsoft.com/office/spreadsheetml/2017/dynamicarray";
const REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata";
const TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheetMetadata+xml";
pub(super) const PART: &str = "xl/metadata.xml";
pub(super) const RELS: &str = "xl/_rels/workbook.xml.rels";
pub(super) const TYPES: &str = "[Content_Types].xml";
pub(super) const METADATA: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xda="http://schemas.microsoft.com/office/spreadsheetml/2017/dynamicarray"><metadataTypes count="1"><metadataType name="XLDAPR" minSupportedVersion="120000" copy="1" pasteAll="1" pasteValues="1" merge="1" splitFirst="1" rowColShift="1" clearFormats="1" clearComments="1" assign="1" coerce="1" cellMeta="1"/></metadataTypes><futureMetadata name="XLDAPR" count="1"><bk><extLst><ext uri="{bdbb8cdc-fa1e-496e-a857-3c3f30c029c3}"><xda:dynamicArrayProperties fDynamic="1" fCollapsed="0"/></ext></extLst></bk></futureMetadata><cellMetadata count="1"><bk><rc t="1" v="0"/></bk></cellMetadata></metadata>"#;

pub(super) fn editable(zip: &mut zip::ZipArchive<File>) -> bool {
    // A cell metadata index may reference several records (e.g. rich data).
    // Replacing the entire index is safe only for this single-purpose package.
    if !zip
        .by_name(PART)
        .is_ok_and(|f| f.size() == METADATA.len() as u64)
    {
        return false;
    }
    if !super::read_zip_text(zip, PART).is_ok_and(|xml| xml == METADATA) {
        return false;
    }
    super::read_zip_text(zip, RELS).ok().is_some_and(|xml| {
        Document::parse(&xml).is_ok_and(|doc| {
            doc.root_element().children().any(|e| {
                e.attribute("Type") == Some(REL)
                    && e.attribute("TargetMode") != Some("External")
                    && e.attribute("Target")
                        .is_some_and(|t| crate::fixups::resolve_target(RELS, t) == PART)
            })
        })
    })
}

pub(super) fn area(key: crate::CellRef, (h, w): (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let last_row = key.row.checked_add(h.saturating_sub(1))?;
    let last_col = key.col.checked_add(w.saturating_sub(1))?;
    (last_row < crate::sync::MAX_ROWS && last_col < crate::sync::MAX_COLS)
        .then_some((key.row, key.col, last_row, last_col))
}

pub(super) fn reference(rect: (u32, u32, u32, u32)) -> String {
    if (rect.0, rect.1) == (rect.2, rect.3) {
        crate::model::cell_name(rect.0, rect.1)
    } else {
        crate::arr::a1_range(rect)
    }
}

pub(super) fn declare(xml: &mut String, part: &str) -> Result<()> {
    let doc = Document::parse(xml)?;
    let root = doc.root_element();
    let (closing, record) = if part == RELS {
        ensure!(
            !root.children().any(|r| r.attribute("Type") == Some(REL)),
            "工作簿已引用元数据"
        );
        let mut n = 1;
        let id = loop {
            let candidate = format!("rId{n}");
            if !root
                .children()
                .any(|r| r.attribute("Id") == Some(&candidate))
            {
                break candidate;
            }
            n += 1;
        };
        (
            "</Relationships>",
            format!("<Relationship Id=\"{id}\" Type=\"{REL}\" Target=\"metadata.xml\"/>"),
        )
    } else {
        ensure!(
            !root
                .children()
                .any(|r| r.attribute("PartName") == Some("/xl/metadata.xml")),
            "元数据类型已占用"
        );
        (
            "</Types>",
            format!("<Override PartName=\"/xl/metadata.xml\" ContentType=\"{TYPE}\"/>"),
        )
    };
    let index = xml[..root.range().end]
        .rfind(closing)
        .context("元数据清单无效")?;
    xml.insert_str(index, &record);
    Ok(())
}

pub(super) fn indices(zip: &mut zip::ZipArchive<File>, rels: &[u8]) -> Result<HashSet<u32>> {
    let mut result = HashSet::new();
    let relationships = Document::parse(std::str::from_utf8(rels)?)?;
    let Some(target) = relationships
        .root_element()
        .children()
        .find(|r| r.attribute("Type") == Some(REL) && r.attribute("TargetMode") != Some("External"))
        .and_then(|r| r.attribute("Target"))
    else {
        return Ok(result);
    };
    let body = super::read_zip_text(zip, &crate::fixups::resolve_target(RELS, target))?;
    let doc = Document::parse(&body)?;
    let root = doc.root_element();
    let child = |n| root.children().find(|e| e.has_tag_name((NS, n)));
    let (Some(types), Some(cells)) = (child("metadataTypes"), child("cellMetadata")) else {
        return Ok(result);
    };
    let Some(t) = types
        .children()
        .filter(|n| n.has_tag_name((NS, "metadataType")))
        .position(|n| n.attribute("name") == Some("XLDAPR"))
    else {
        return Ok(result);
    };
    let dynamic: HashSet<_> = root
        .children()
        .filter(|n| n.has_tag_name((NS, "futureMetadata")) && n.attribute("name") == Some("XLDAPR"))
        .flat_map(|n| n.children().filter(|n| n.has_tag_name((NS, "bk"))))
        .enumerate()
        .filter(|(_, n)| {
            n.descendants().any(|p| {
                p.has_tag_name((XDA, "dynamicArrayProperties"))
                    && matches!(p.attribute("fDynamic"), Some("1" | "true"))
            })
        })
        .map(|(i, _)| i)
        .collect();
    for (i, block) in cells
        .children()
        .filter(|n| n.has_tag_name((NS, "bk")))
        .enumerate()
    {
        if block.children().any(|r| {
            r.has_tag_name((NS, "rc"))
                && r.attribute("t").and_then(|a| a.parse().ok()) == Some(t + 1)
                && r.attribute("v")
                    .and_then(|a| a.parse::<usize>().ok())
                    .is_some_and(|v| dynamic.contains(&v))
        }) {
            result.insert(u32::try_from(i + 1)?);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn mixed_metadata_resolves_the_dynamic_type_and_future_block_at_their_actual_indexes() {
        let metadata = format!(
            r#"<metadata xmlns="{NS}" xmlns:xda="{XDA}"><metadataTypes count="2"><metadataType name="XLRICHVALUE"/><metadataType name="XLDAPR"/></metadataTypes><futureMetadata name="XLDAPR" count="2"><bk><extLst><ext><xda:dynamicArrayProperties fDynamic="0"/></ext></extLst></bk><bk><extLst><ext><xda:dynamicArrayProperties fDynamic="1"/></ext></extLst></bk></futureMetadata><cellMetadata count="3"><bk><rc t="1" v="0"/></bk><bk><rc t="2" v="1"/></bk><bk><rc t="2" v="0"/></bk></cellMetadata></metadata>"#
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(tmp.reopen().unwrap());
        zip.start_file(
            "xl/custom/meta1.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(metadata.as_bytes()).unwrap();
        zip.finish().unwrap();
        let rels = format!(
            r#"<Relationships><Relationship Id="rId9" Type="{REL}" Target="custom/meta1.xml"/></Relationships>"#
        );
        let mut zip = zip::ZipArchive::new(tmp.reopen().unwrap()).unwrap();
        assert_eq!(
            indices(&mut zip, rels.as_bytes()).unwrap(),
            HashSet::from([2])
        );
        assert!(!editable(&mut zip));
    }
}
