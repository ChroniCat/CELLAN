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
    crate::chart_write::rewrite_with_additions_checked(path, additions, &[&sheet_path, &sheet_rels_part, "[Content_Types].xml"], move |name, buf| {
        if name == sheet_rels2 {
            let xml = crate::xlsx::patch_xml_text(&buf,name)?;
            return Ok(crate::textbox::add_drawing_relationship(&xml, &format!("rId{next_rid}"),
                &format!("../drawings/{drawing_name}"))?.into_bytes());
        }
        if name == sheet_path2 {
            let xml = crate::xlsx::patch_xml_text(&buf,name)?;
            return Ok(crate::textbox::insert_drawing_tag(&xml,&format!("rId{next_rid}"))?.into_bytes());
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
                return Ok(x.into_bytes());
            }
            return Ok(buf);
        }
        Ok(buf)
    })?;
    Ok(())
}

// ---------------------------------------------------------------- 统一保存

/// 新插入图片的稳定部件名：内容哈希，重复保存不会增生媒体文件
pub fn memory_part(bytes: &[u8], ext: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    let ext = if ext == "jpg" { "jpeg".to_string() } else { ext };
    format!("xl/media/sc{h:016x}.{ext}")
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "jpeg" | "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
}

fn xml_esc_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn pic_anchor_xml(pic: &crate::pic::Picture, rid: &str, id: u32) -> String {
    let (r1, c1, r2, c2) = pic.rect;
    let [dx1, dy1, dx2, dy2] = pic.off;
    let name = if pic.name.is_empty() { "图片" } else { pic.name.as_str() };
    let name_esc = xml_esc_attr(name);
    format!(
        "<xdr:twoCellAnchor editAs=\"oneCell\">\
<xdr:from><xdr:col>{c1}</xdr:col><xdr:colOff>{dx1}</xdr:colOff><xdr:row>{r1}</xdr:row><xdr:rowOff>{dy1}</xdr:rowOff></xdr:from>\
<xdr:to><xdr:col>{c2}</xdr:col><xdr:colOff>{dx2}</xdr:colOff><xdr:row>{r2}</xdr:row><xdr:rowOff>{dy2}</xdr:rowOff></xdr:to>\
<xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"{id}\" name=\"{name_esc}\"/><xdr:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></xdr:cNvPicPr></xdr:nvPicPr>\
<xdr:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill>\
<xdr:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></xdr:spPr>\
</xdr:pic><xdr:clientData/></xdr:twoCellAnchor>"
    )
}

/// 扫描已有 drawing XML 中最大的 `cNvPr id`，用于为新图片分配不冲突的唯一 id。
fn max_cnvpr_id(xml: &str) -> u32 {
    let mut max_id = 1u32;
    let mut rest = xml;
    while let Some(i) = rest.find("cNvPr") {
        let seg = &rest[i..];
        let Some(gt) = seg.find('>') else { break };
        let tag = &seg[..=gt];
        if let Some(p) = tag.find("id=\"") {
            let v = &tag[p + 4..];
            if let Some(q) = v.find('"') {
                if let Ok(n) = v[..q].parse::<u32>() {
                    max_id = max_id.max(n);
                }
            }
        }
        rest = &seg[gt + 1..];
    }
    max_id
}

/// 提取既有 drawing 中的所有图片锚点块（供未改动图片原样保留，保住裁剪/旋转/oneCellAnchor 等细节）。
fn extract_existing_pic_anchors(
    xml: &str,
    old_rels: &str,
    draw_part: &str,
) -> Vec<(String, (u32, u32, u32, u32), [i64; 4], String)> {
    let mut rid_to_part: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let dir = draw_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("xl/drawings");
    if let Ok(doc) = roxmltree::Document::parse(old_rels) {
        for n in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "Relationship") {
            if let (Some(id), Some(target)) = (n.attribute("Id"), n.attribute("Target")) {
                if n.attribute("Type").is_some_and(|t| t.ends_with("/image")) {
                    let resolved = if let Some(rest) = target.strip_prefix('/') {
                        rest.to_string()
                    } else {
                        let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
                        for seg in target.split('/') {
                            match seg {
                                "" | "." => {}
                                ".." => { parts.pop(); }
                                s => parts.push(s),
                            }
                        }
                        parts.join("/")
                    };
                    rid_to_part.insert(id.to_string(), resolved);
                }
            }
        }
    }
    let mut out = Vec::new();
    for tag in ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"] {
        for prefix in ["xdr:", ""] {
            let open = format!("<{prefix}{tag}");
            let close = format!("</{prefix}{tag}>");
            let mut from = 0usize;
            while let Some(rel) = xml[from..].find(&open) {
                let start = from + rel;
                let Some(endrel) = xml[start..].find(&close) else { break };
                let end = start + endrel + close.len();
                let chunk = &xml[start..end];
                if chunk.contains("<xdr:pic") || chunk.contains("<pic") {
                    if let Some((r1, c1, r2, c2, off, rid)) = crate::pic::parse_drawing(chunk).into_iter().next() {
                        if let Some(part) = rid_to_part.get(&rid) {
                            out.push((part.clone(), (r1, c1, r2, c2), off, chunk.to_string()));
                        }
                    }
                }
                from = end;
            }
        }
    }
    out
}

/// 替换原图片锚点块里的 `r:embed="..."`（或 `embed="..."`）与可选 `cNvPr id`，其余属性/样式原样保留。
fn rebind_pic_anchor_rid(block: &str, new_rid: &str) -> String {
    for pat in ["r:embed=\"", "embed=\""] {
        if let Some(p) = block.find(pat) {
            let start = p + pat.len();
            if let Some(q) = block[start..].find('"') {
                let mut out = block.to_string();
                out.replace_range(start..start + q, new_rid);
                return out;
            }
        }
    }
    block.to_string()
}

/// 去掉所有含 `<xdr:pic>` 的锚点，保留图表/文本框等其他锚点
fn strip_pic_anchors(xml: &str) -> String {
    let mut x = xml.to_string();
    for tag in ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"] {
        for prefix in ["xdr:", ""] {
            let open = format!("<{prefix}{tag}");
            let close = format!("</{prefix}{tag}>");
            let mut from = 0usize;
            loop {
                let Some(rel) = x[from..].find(&open) else { break };
                let start = from + rel;
                let Some(endrel) = x[start..].find(&close) else { break };
                let end = start + endrel + close.len();
                let chunk = x[start..end].to_string();
                if chunk.contains("<xdr:pic") || chunk.contains("<pic") {
                    x.replace_range(start..end, "");
                    from = start;
                } else {
                    from = end;
                }
            }
        }
    }
    x
}

fn rewrite_drawing(xml: &str, anchors: &str) -> String {
    let mut kept = strip_pic_anchors(xml);
    if anchors.is_empty() {
        return kept;
    }
    if let Some(i) = kept.rfind("</xdr:wsDr>") {
        kept.insert_str(i, anchors);
    } else if let Some(i) = kept.rfind("</wsDr>") {
        kept.insert_str(i, anchors);
    } else {
        return format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">{anchors}</xdr:wsDr>"
        );
    }
    kept
}

/// 画布关系里保留非图片关系（图表等），图片关系重建
fn keep_other_rels(xml: &str) -> anyhow::Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    anyhow::ensure!(root.tag_name().name() == "Relationships", "无效的绘图关系清单");
    let mut kept = xml.to_owned();
    for rel in root.children().rev().filter(|n| n.is_element() && n.tag_name().name() == "Relationship"
        && n.attribute("Type").is_some_and(|t| t.ends_with("/image"))) {
        kept.replace_range(rel.range(), "");
    }
    if root.children().next().is_none() && xml[root.range()].trim_end().ends_with("/>") {
        let end = root.range().end;
        let prefix = root.tag_name().namespace().and_then(|uri| root.lookup_prefix(uri));
        let tag = prefix.map_or_else(|| "Relationships".to_owned(), |p| format!("{p}:Relationships"));
        kept.replace_range(end-2..end,&format!("></{tag}>"));
    }
    Ok(kept)
}

fn empty_rels() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>"
        .to_string()
}

/// 保存收尾：把内存里的图片写回各表 drawing（只处理 dirty 的表）
pub fn apply(path: &Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.pictures_dirty) {
        return Ok(());
    }
    let targets = crate::patch::sheet_targets(path)?;
    let mut additions: Vec<(String, Vec<u8>)> = Vec::new();
    let mut patch: Vec<(String, String)> = Vec::new();
    let mut exts: Vec<String> = Vec::new();
    let mut new_drawings: Vec<String> = Vec::new();
    let mut touched = false;
    let mut next = crate::textbox::next_drawing_no(path)?;
    for sheet in &wb.sheets {
        if !sheet.pictures_dirty {
            continue;
        }
        let Some(sheet_part) = targets.get(&sheet.name).cloned() else { continue };
        let sheet_bytes = crate::fixups::read_part_file(path,&sheet_part).ok_or_else(|| anyhow::anyhow!("缺少工作表 {sheet_part}"))?;
        let sheet_xml = crate::xlsx::patch_xml_text(&sheet_bytes,&sheet_part)?;
        let (draw_part, draw_rels, rid) =
            crate::textbox::ensure_drawing(path, &sheet_part, &sheet_xml, &mut next)?;
        let existing = if rid.is_none() {
            let bytes = crate::fixups::read_part_file(path, &draw_part)
                .ok_or_else(|| anyhow::anyhow!("无法读取原有绘图 {draw_part}"))?;
            crate::xlsx::patch_xml_text(&bytes, &draw_part)?.into_owned()
        } else {
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"></xdr:wsDr>"
                    .to_string()
        };
        let old_rels = if crate::textbox::has_part(path,&draw_rels) {
            crate::textbox::drawing_relationships(path,&draw_rels)?
        } else { empty_rels() };
        let mut rels = keep_other_rels(&old_rels)?;
        let mut existing_pics = extract_existing_pic_anchors(&existing, &old_rels, &draw_part);
        let mut next_id = max_cnvpr_id(&existing).saturating_add(1);
        let mut anchors = String::new();
        for (i, pic) in sheet.pictures.iter().enumerate() {
            let rid_i = format!("rIdPic{}", i + 1);
            let media = if let Some(data) = pic.data.as_ref() {
                let part = memory_part(data, &pic.ext);
                if !crate::textbox::has_part(path, &part) && !additions.iter().any(|(name, _)| name == &part) {
                    additions.push((part.clone(), data.as_ref().clone()));
                }
                part
            } else {
                pic.part.clone()
            };
            let fname = media.rsplit('/').next().unwrap_or("image1.png");
            let rel = format!(
                "<Relationship xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\" Id=\"{rid_i}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"../media/{fname}\"/>"
            );
            let end = rels.rfind("</").ok_or_else(|| anyhow::anyhow!("绘图关系缺少根结束标记"))?;
            rels.insert_str(end,&rel);
            let ext = if pic.ext.is_empty() {
                fname.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_else(|| "png".into())
            } else {
                pic.ext.to_ascii_lowercase()
            };
            if !ext.is_empty() && !exts.contains(&ext) {
                exts.push(ext);
            }
            if let Some(pos) = existing_pics
                .iter()
                .position(|(ep, er, eo, _)| ep == &media && *er == pic.rect && *eo == pic.off)
            {
                let (_, _, _, raw_block) = existing_pics.remove(pos);
                anchors.push_str(&rebind_pic_anchor_rid(&raw_block, &rid_i));
            } else {
                let id = next_id;
                next_id = next_id.saturating_add(1);
                anchors.push_str(&pic_anchor_xml(pic, &rid_i, id));
            }
        }
        let body = rewrite_drawing(&existing, &anchors);
        if crate::textbox::has_part(path, &draw_part) {
            crate::textbox::patch_sheet_drawing(&mut patch, &draw_part, body);
            if crate::textbox::has_part(path, &draw_rels) {
                patch.push((draw_rels.clone(), rels));
            } else {
                additions.push((draw_rels.clone(), rels.into_bytes()));
            }
        } else {
            additions.push((draw_part.clone(), body.into_bytes()));
            additions.push((draw_rels.clone(), rels.into_bytes()));
            new_drawings.push(draw_part.clone());
        }
        if let Some(rid) = rid {
            let srels_name = crate::textbox::rels_part(&sheet_part);
            let target = format!("../{}", draw_part.strip_prefix("xl/").unwrap_or(&draw_part));
            let srels = crate::textbox::add_drawing_relationship(
                &crate::textbox::drawing_relationships(path,&srels_name)?, &rid, &target)?;
            if crate::textbox::has_part(path, &srels_name) {
                patch.push((srels_name, srels));
            } else {
                additions.push((srels_name, srels.into_bytes()));
            }
            patch.push((
                sheet_part.clone(),
                crate::textbox::insert_drawing_tag(&sheet_xml, &rid)?,
            ));
        }
        touched = true;
    }
    if !touched {
        return Ok(());
    }
    let targets: Vec<_> = patch.iter().map(|(name, _)| name.as_str())
        .chain(std::iter::once("[Content_Types].xml")).collect();
    crate::chart_write::rewrite_with_additions_pub(path, additions, &targets, |part, buf| {
        if let Some((_, body)) = patch.iter().find(|(n, _)| n == part) {
            return body.clone().into_bytes();
        }
        if part == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                for d in &new_drawings {
                    if !x.contains(&format!("PartName=\"/{d}\"")) {
                        x = x.replace(
                            "</Types>",
                            &format!(
                                "<Override PartName=\"/{d}\" ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/></Types>"
                            ),
                        );
                    }
                }
                for e in &exts {
                    if !x.contains(&format!("Extension=\"{e}\"")) {
                        x = x.replace(
                            "</Types>",
                            &format!(
                                "<Default Extension=\"{e}\" ContentType=\"{}\"/></Types>",
                                mime_of(e)
                            ),
                        );
                    }
                }
                return x.into_bytes();
            }
        }
        buf
    })?;
    Ok(())
}
