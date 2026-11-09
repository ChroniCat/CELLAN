//! 插入图片：媒体部件 + drawing + rels + content types。
//! 包结构的增删改跟 chart_write 走同一套（rewrite_with_additions）。

use anyhow::Result;
use std::io::Read;
use std::path::Path;

/// 9525 EMU = 1 像素
const EMU_PER_PX: i64 = 9525;

fn next_index(zip: &mut zip::ZipArchive<std::fs::File>, prefix: &str) -> u32 {
    let mut max = 0u32;
    for i in 0..zip.len() {
        let Ok(f) = zip.by_index(i) else { continue };
        let name = f.name().to_string();
        if let Some(rest) = name.strip_prefix(prefix) {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<u32>() {
                max = max.max(v);
            }
        }
    }
    max + 1
}

/// 从文件头取像素尺寸（PNG / JPEG / GIF）
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() > 24 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((w, h));
    }
    if bytes.len() > 10 && &bytes[0..3] == b"GIF" {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some((w, h));
    }
    if bytes.len() > 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            // SOF0..SOF15（跳过 DHT/DAC/RST 等）
            if (0xC0..=0xCF).contains(&marker)
                && marker != 0xC4
                && marker != 0xC8
                && marker != 0xCC
            {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

fn drawing_xml(cx: i64, cy: i64, col: u32, row: u32) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<xdr:oneCellAnchor>\
<xdr:from><xdr:col>{col}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{row}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
<xdr:ext cx=\"{cx}\" cy=\"{cy}\"/>\
<xdr:pic>\
<xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"图片 1\"/><xdr:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></xdr:cNvPicPr></xdr:nvPicPr>\
<xdr:blipFill><a:blip r:embed=\"rId1\"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill>\
<xdr:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></xdr:spPr>\
</xdr:pic><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"
    )
}

pub fn insert_picture(
    path: &Path,
    sheet_name: &str,
    bytes: &[u8],
    ext: &str,
    col: u32,
    row: u32,
) -> Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(sheet_path) = targets.get(sheet_name).cloned() else {
        return Err(anyhow::anyhow!("找不到工作表 {sheet_name}"));
    };
    let (w, h) = image_size(bytes).unwrap_or((240, 160));
    let (cx, cy) = (w as i64 * EMU_PER_PX, h as i64 * EMU_PER_PX);

    let (media_no, drawing_no, next_rid, has_sheet_rels) = {
        let file = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let media_no = next_index(&mut zip, "xl/media/image");
        let drawing_no = next_index(&mut zip, "xl/drawings/drawing");
        let rels_path = match sheet_path.rsplit_once('/') {
            Some((d, b)) => format!("{d}/_rels/{b}.rels"),
            None => String::new(),
        };
        let mut max_rid = 0u32;
        let mut has_rels = false;
        if let Ok(mut f) = zip.by_name(&rels_path) {
            has_rels = true;
            let mut xml = String::new();
            let _ = f.read_to_string(&mut xml);
            for chunk in xml.split("Id=\"rId").skip(1) {
                if let Some(end) = chunk.find('"') {
                    if let Ok(v) = chunk[..end].parse::<u32>() {
                        max_rid = max_rid.max(v);
                    }
                }
            }
        }
        (media_no, drawing_no, max_rid + 1, has_rels)
    };

    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "image/png",
    };
    let media_part = format!("xl/media/image{media_no}.{ext}");
    let drawing_part = format!("xl/drawings/drawing{drawing_no}.xml");
    let drawing_rels_part = format!("xl/drawings/_rels/drawing{drawing_no}.xml.rels");
    let sheet_rels_part = match sheet_path.rsplit_once('/') {
        Some((d, b)) => format!("{d}/_rels/{b}.rels"),
        None => return Err(anyhow::anyhow!("工作表路径异常")),
    };

    let drawing_body = drawing_xml(cx, cy, col, row);
    let drawing_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"../media/image{media_no}.{ext}\"/>\
</Relationships>"
    );

    let mut additions = vec![
        (media_part.clone(), bytes.to_vec()),
        (drawing_part.clone(), drawing_body.into_bytes()),
        (drawing_rels_part, drawing_rels.into_bytes()),
    ];
    if !has_sheet_rels {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId{next_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/drawing{drawing_no}.xml\"/>\
</Relationships>"
        );
        additions.push((sheet_rels_part.clone(), body.into_bytes()));
    }

    let sheet_path2 = sheet_path.clone();
    let sheet_rels2 = sheet_rels_part.clone();
    let drawing_name = format!("drawing{drawing_no}.xml");
    crate::chart_write::rewrite_with_additions_pub(path, additions, move |name, buf| {
        if name == sheet_rels2 {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                if !xml.contains(&drawing_name) {
                    let add = format!(
                        "<Relationship Id=\"rId{next_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/{drawing_name}\"/>"
                    );
                    return xml
                        .replace("</Relationships>", &format!("{add}</Relationships>"))
                        .into_bytes();
                }
            }
            return buf;
        }
        if name == sheet_path2 {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                if !x.contains("<drawing ") {
                    // 有些工作表根节点没声明 xmlns:r，就地补上，否则 Excel 报 unbound prefix
                    let tag = format!(
                        "<drawing xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId{next_rid}\"/>"
                    );
                    // schema 顺序：drawing 必须在 legacyDrawing/tableParts/extLst 之前
                    let mut pos = x.find("</worksheet>").unwrap_or(x.len());
                    for t in ["<legacyDrawing", "<tableParts", "<extLst"] {
                        if let Some(i) = x.find(t) {
                            pos = pos.min(i);
                        }
                    }
                    x = format!("{}{}{}", &x[..pos], tag, &x[pos..]);
                }
                return x.into_bytes();
            }
            return buf;
        }
        if name == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                if !x.contains(&format!("Extension=\"{ext}\"")) {
                    x = x.replace(
                        "</Types>",
                        &format!("<Default Extension=\"{ext}\" ContentType=\"{mime}\"/></Types>"),
                    );
                }
                if !x.contains(&drawing_name) {
                    x = x.replace(
                        "</Types>",
                        &format!("<Override PartName=\"/xl/drawings/{drawing_name}\" ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/></Types>"),
                    );
                }
                return x.into_bytes();
            }
            return buf;
        }
        buf
    })?;
    Ok(())
}
