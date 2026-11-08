//! 保存后的收尾修补：umya 会重写 drawing rels（丢掉图表关系）、
//! 把批注写到新路径（旧部件留成孤儿）、重复挂表格部件。
//!
//! 这里做两件事：
//! 1. 按基准文件把 drawing rels 里被改坏的关系改回去（图表 / 图片）；
//! 2. 删掉没有任何关系指向的表格、批注、图表部件（孤儿）。

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

pub fn read_part(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn entries(path: &Path) -> Vec<String> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .collect()
}

/// rId → (Type, Target)，从 rels XML 里抠出来
fn parse_rels(xml: &str) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<Relationship") {
        let end = rest[pos..].find("/>").map(|e| pos + e).unwrap_or(rest.len());
        let tag = &rest[pos..end];
        let attr = |key: &str| -> Option<String> {
            let pat = format!("{key}=\"");
            let i = tag.find(&pat)? + pat.len();
            let r = &tag[i..];
            let q = r.find('"')?;
            Some(r[..q].to_string())
        };
        if let (Some(id), Some(t), Some(target)) = (attr("Id"), attr("Type"), attr("Target")) {
            out.insert(id, (t, target));
        }
        rest = &rest[end.min(rest.len())..];
    }
    out
}

/// 把 drawing rels 里被 umya 改坏的关系按基准文件改回来。
///
/// 只在「rId 相同但 Type/Target 不同」时覆盖：umya 会把 chart 关系删掉、
/// 把 image 关系的 Target 换成相对路径并把 id 挪位，绘图 XML 里的 rId 却不动。
pub fn fix_drawing_rels(path: &Path, base: &Path) -> Result<Vec<String>> {
    let mut fixed = Vec::new();
    if !base.exists() {
        return Ok(fixed);
    }
    for name in entries(path) {
        if !name.starts_with("xl/drawings/_rels/") || !name.ends_with(".rels") {
            continue;
        }
        let Some(old_xml) = read_part(base, &name).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let Some(new_xml) = read_part(path, &name).and_then(|b| String::from_utf8(b).ok()) else {
            // 新文件里整个 rels 都没了：直接把基准的搬过来
            if let Some(raw) = read_part(base, &name) {
                crate::patch::append_entry(path, &name, &raw)?;
                fixed.push(format!("{name}（整体恢复）"));
            }
            continue;
        };
        let old = parse_rels(&old_xml);
        let new = parse_rels(&new_xml);

        let mut add = String::new();
        let mut drop_ids: Vec<String> = Vec::new();
        for (id, (t, target)) in old.iter() {
            match new.get(id) {
                Some((nt, ntarget)) if nt == t && ntarget == target => {}
                _ => {
                    // 这个 id 在新文件里要么没了，要么被改成了别的东西
                    drop_ids.push(id.clone());
                    add.push_str(&format!(
                        "<Relationship Id=\"{id}\" Type=\"{t}\" Target=\"{target}\"/>"
                    ));
                }
            }
        }
        if add.is_empty() {
            continue;
        }
        // 先把冲突的 id 从新 rels 里删掉，再追加正确的关系
        let mut rebuilt = String::new();
        let mut rest = new_xml.as_str();
        while let Some(pos) = rest.find("<Relationship") {
            let end = rest[pos..].find("/>").map(|e| pos + e + 2).unwrap_or(rest.len());
            let tag = &rest[pos..end];
            let keep = !drop_ids.iter().any(|id| tag.contains(&format!("Id=\"{id}\"")));
            if keep {
                rebuilt.push_str(tag);
            }
            rest = &rest[end.min(rest.len())..];
        }
        let merged = if rebuilt.trim().is_empty() {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{add}</Relationships>"
            )
        } else {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rebuilt}{add}</Relationships>"
            )
        };
        crate::patch::append_entry(path, &name, merged.as_bytes())?;
        fixed.push(format!("{name}（补回 {} 条关系）", drop_ids.len()));
    }
    Ok(fixed)
}







/// 没被编辑过的表：整段沿用原文件的 XML 和它引用的部件。
///
/// umya 重写工作表时会丢三落四（表对象、批注、绘图、图形属性…），
/// 凡是我们没动过的表，直接按基准文件覆盖回去最稳，也最保真。
pub fn restore_unedited_sheets(
    path: &Path,
    base: &Path,
    wb: &crate::model::Workbook,
) -> Result<Vec<String>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let base_targets = crate::patch::sheet_targets(base)?;
    let new_targets = crate::patch::sheet_targets(path)?;
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    let mut log = Vec::new();

    for sheet in wb.sheets.iter() {
        let dirty = sheet.edited
            || sheet.merges_dirty
            || sheet.cf_dirty
            || sheet.dv_dirty
            || sheet.notes_dirty
            || sheet.freeze_dirty
            || sheet.filter_dirty
            || sheet.page_dirty
            || sheet.charts_dirty;
        if dirty {
            continue;
        }
        let Some(base_part) = base_targets.get(&sheet.name) else { continue };
        let Some(new_part) = new_targets.get(&sheet.name) else { continue };

        let Some(xml) = read_part(base, base_part) else { continue };
        pending.push((new_part.clone(), xml));

        // 关系表：路径可能变了，Target 要按新目录重算
        let base_rels = base_part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        let new_rels = new_part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        if base_part == new_part {
            // 路径没变：rels 原样照抄，一个字节都不改
            if let Some(raw) = read_part(base, &base_rels) {
                if let Ok(text) = String::from_utf8(raw.clone()) {
                    for (_, (_, target)) in parse_rels(&text) {
                        let abs = resolve_target(&base_rels, &target);
                        if let Some(body) = read_part(base, &abs) {
                            pending.push((abs.clone(), body));
                        }
                    }
                }
                pending.push((base_rels.clone(), raw));
                log.push(sheet.name.clone());
                continue;
            }
        }
        if let Some(rels) = read_part(base, &base_rels).and_then(|b| String::from_utf8(b).ok()) {
            let mut out = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
            );
            for (id, (ty, target)) in parse_rels(&rels) {
                let abs = resolve_target(&base_rels, &target);
                // 被引用的部件也按基准恢复（表格定义、绘图、VML、批注…）
                if let Some(body) = read_part(base, &abs) {
                    pending.push((abs.clone(), body));
                }
                let rel = relative_to(&new_rels, &abs);
                out.push_str(&format!(
                    "<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{rel}\"/>"
                ));
            }
            out.push_str("</Relationships>");
            pending.push((new_rels, out.into_bytes()));
        }
        log.push(sheet.name.clone());
    }

    if !pending.is_empty() {
        crate::patch::append_entries(path, &pending)?;
    }
    Ok(log)
}


/// rels 路径：xl/worksheets/sheet1.xml -> xl/worksheets/_rels/sheet1.xml.rels
fn rels_of(part: &str) -> String {
    match part.rfind('/') {
        Some(i) => format!("{}/_rels/{}.rels", &part[..i], &part[i + 1..]),
        None => format!("_rels/{part}.rels"),
    }
}

/// 把 umya 搬家后的工作表放回原位。
///
/// umya 一律把工作表写到 xl/worksheets/，而真实工作簿可能把它们放在
/// xl/任意子目录/worksheets/ 下。搬家会让所有相对引用（绘图、图表、透视表）
/// 全部对不上，Excel 直接拒收。这里把部件挪回基准路径，并把
/// workbook.xml.rels 与各表 rels 的目标改回去。
pub fn rehome_sheets(path: &Path, base: &Path) -> Result<Vec<String>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let base_targets = crate::patch::sheet_targets(base)?;
    let new_targets = crate::patch::sheet_targets(path)?;

    let mut moves: Vec<(String, String)> = Vec::new(); // (新路径, 基准路径)
    for (name, bpart) in base_targets.iter() {
        if let Some(npart) = new_targets.get(name) {
            if npart != bpart {
                moves.push((npart.clone(), bpart.clone()));
            }
        }
    }
    if moves.is_empty() {
        return Ok(Vec::new());
    }

    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    let mut removes: Vec<String> = Vec::new();

    // 先把内容全部读出来再写：两个基准目录下的同名文件会撞车，
    // 边读边写会让后一个把前一个覆盖掉。
    let snapshot: Vec<(String, String, Option<Vec<u8>>, Option<Vec<u8>>)> = moves
        .iter()
        .map(|(npart, bpart)| {
            let body = read_part(path, npart);
            let rels = read_part(path, &rels_of(npart));
            (npart.clone(), bpart.clone(), body, rels)
        })
        .collect();
    for (npart, bpart, body, rels_raw) in snapshot.iter() {
        if let Some(body) = body {
            pending.push((bpart.clone(), body.clone()));
        }
        removes.push(npart.clone());

        let nrels = rels_of(npart);
        let brels = rels_of(bpart);
        if let Some(raw) = rels_raw.clone().and_then(|b| String::from_utf8(b).ok()) {
            let mut out = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
            );
            for (id, (ty, target)) in parse_rels(&raw) {
                let abs = resolve_target(&nrels, &target);
                let rel = relative_to(&brels, &abs);
                out.push_str(&format!(
                    "<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{rel}\"/>"
                ));
            }
            out.push_str("</Relationships>");
            pending.push((brels, out.into_bytes()));
            removes.push(nrels);
        }
    }

    // workbook.xml.rels 指向新的工作表路径
    let wrels = "xl/_rels/workbook.xml.rels";
    if let Some(raw) = read_part(path, wrels).and_then(|b| String::from_utf8(b).ok()) {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        );
        for (id, (ty, target)) in parse_rels(&raw) {
            let abs = resolve_target(wrels, &target);
            let fixed = moves
                .iter()
                .find(|(n, _)| *n == abs)
                .map(|(_, b)| b.clone())
                .unwrap_or(abs);
            let rel = relative_to(wrels, &fixed);
            out.push_str(&format!(
                "<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{rel}\"/>"
            ));
        }
        out.push_str("</Relationships>");
        pending.push((wrels.to_string(), out.into_bytes()));
    }

    crate::patch::append_entries(path, &pending)?;
    // 别删掉刚好是别人目的地的路径
    let dests: std::collections::HashSet<String> = moves.iter().map(|(_, b)| b.clone()).collect();
    for r in removes {
        if dests.contains(&r) {
            continue;
        }
        let _ = crate::patch::remove_entry(path, &r);
    }
    Ok(moves.into_iter().map(|(_, b)| b).collect())
}


/// 工作表名字与顺序没变时，workbook.xml / workbook.xml.rels 整段用基准的。
///
/// umya 重写工作簿骨架时会丢掉 definedNames、pivotCaches、外部连接这些东西，
/// 而透视表、打印区域都靠它们；丢一个 Excel 就拒收整个文件。
pub fn restore_workbook_skeleton(path: &Path, base: &Path, wb: &crate::model::Workbook) -> Result<bool> {
    if !base.exists() || wb.names_dirty {
        return Ok(false);
    }
    // 有任何一处改动就不动骨架：打印区域、名称、透视缓存都挂在 workbook.xml 上
    let touched = wb.sheets.iter().any(|s| {
        s.edited
            || s.merges_dirty
            || s.cf_dirty
            || s.dv_dirty
            || s.notes_dirty
            || s.freeze_dirty
            || s.filter_dirty
            || s.page_dirty
            || s.charts_dirty
    });
    if touched {
        return Ok(false);
    }
    let base_xml = match read_part(base, "xl/workbook.xml").and_then(|b| String::from_utf8(b).ok()) {
        Some(v) => v,
        None => return Ok(false),
    };
    let mut base_names: Vec<String> = Vec::new();
    let mut rest = base_xml.as_str();
    while let Some(i) = rest.find("<sheet ") {
        rest = &rest[i + 7..];
        let end = match rest.find('>') {
            Some(e) => e,
            None => break,
        };
        let tag = &rest[..end];
        if let Some(p) = tag.find("name=\"") {
            let after = &tag[p + 6..];
            if let Some(q) = after.find('"') {
                base_names.push(after[..q].to_string());
            }
        }
    }
    let here: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    if here != base_names {
        return Ok(false);
    }
    let mut pending: Vec<(String, Vec<u8>)> = vec![("xl/workbook.xml".to_string(), base_xml.into_bytes())];
    if let Some(rels) = read_part(base, "xl/_rels/workbook.xml.rels") {
        pending.push(("xl/_rels/workbook.xml.rels".to_string(), rels));
    }
    crate::patch::append_entries(path, &pending)?;
    Ok(true)
}


/// 去掉 [Content_Types].xml 里指向已不存在部件的 Override。
///
/// 部件被搬走/删除后 Override 会悬空，Excel 见到就判文件损坏。
pub fn prune_content_types(path: &Path) -> Result<usize> {
    let names: std::collections::HashSet<String> = entries(path)
        .into_iter()
        .map(|n| n.trim_start_matches('/').to_string())
        .collect();
    let Some(ct) = read_part(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok())
    else {
        return Ok(0);
    };
    let mut out = String::with_capacity(ct.len());
    let mut rest = ct.as_str();
    let mut removed = 0usize;
    while let Some(i) = rest.find("<Override ") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find("/>").map(|e| e + 2).or_else(|| tail.find('>').map(|e| e + 1));
        let Some(end) = end else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let tag = &tail[..end];
        let gone = tag
            .find("PartName=\"")
            .map(|p| {
                let after = &tag[p + 10..];
                let name = after.split('"').next().unwrap_or("");
                !names.contains(name.trim_start_matches('/'))
            })
            .unwrap_or(false);
        if gone {
            removed += 1;
        } else {
            out.push_str(tag);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    if removed > 0 {
        crate::patch::append_entry(path, "[Content_Types].xml", out.as_bytes())?;
    }
    Ok(removed)
}


/// 重算 styles.xml 里各容器的 count 属性。
///
/// 我们补 dxf 时是往后追加的，count 没跟着变；Excel 对不上就判文件损坏。
pub fn normalize_styles_counts(path: &Path) -> Result<bool> {
    let Some(xml) = read_part(path, "xl/styles.xml").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(false);
    };
    let groups: [(&str, &[&str]); 9] = [
        ("numFmts", &["numFmt"]),
        ("fonts", &["font"]),
        ("fills", &["fill"]),
        ("borders", &["border"]),
        ("cellStyleXfs", &["xf"]),
        ("cellXfs", &["xf"]),
        ("cellStyles", &["cellStyle"]),
        ("dxfs", &["dxf"]),
        ("tableStyles", &["tableStyle"]),
    ];
    let mut out = xml;
    let mut changed = false;
    for (group, kids) in groups {
        let open = format!("<{group}");
        let Some(start) = out.find(&open) else { continue };
        let Some(gt) = out[start..].find('>').map(|i| start + i) else { continue };
        let open_tag = out[start..gt + 1].to_string();
        if open_tag.ends_with("/>") {
            continue;
        }
        let close = format!("</{group}>");
        let Some(cend) = out[start..].find(&close).map(|i| start + i) else { continue };
        let body = out[gt + 1..cend].to_string();
        let mut n = 0usize;
        for k in kids {
            let pat = format!("<{k}");
            let mut from = 0usize;
            while let Some(i) = body[from..].find(&pat) {
                let idx = from + i;
                let next = body[idx + pat.len()..].chars().next().unwrap_or(' ');
                if matches!(next, '>' | ' ' | '/' | '\r' | '\n') {
                    n += 1;
                }
                from = idx + pat.len();
            }
        }
        let new_tag = match open_tag.find("count=\"") {
            Some(p) => {
                let after = &open_tag[p + 7..];
                let q = after.find('"').unwrap_or(after.len());
                format!("{}count=\"{}\"{}", &open_tag[..p], n, &after[(q + 1).min(after.len())..])
            }
            None => {
                let mut t = open_tag.clone();
                let at = t.len() - 1;
                t.insert_str(at, &format!(" count=\"{n}\""));
                t
            }
        };
        if new_tag != open_tag {
            out = format!("{}{}{}", &out[..start], new_tag, &out[gt + 1..]);
            changed = true;
        }
    }
    if changed {
        crate::patch::append_entry(path, "xl/styles.xml", out.as_bytes())?;
    }
    Ok(changed)
}

/// 绘图里的非法属性值清理：umya 会写出 prst=""（空预设形状），Excel 判非法。
pub fn sanitize_drawings(path: &Path) -> Result<Vec<String>> {
    let mut fixed = Vec::new();
    for name in entries(path) {
        if !name.starts_with("xl/") || !name.contains("/drawings/") || !name.ends_with(".xml") {
            continue;
        }
        let Some(xml) = read_part(path, &name).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        if !xml.contains("prst=\"\"") && !xml.contains("prstGeom prst=\"\"") {
            continue;
        }
        let fixed_xml = xml.replace("prst=\"\"", "prst=\"rect\"");
        crate::patch::append_entry(path, &name, fixed_xml.as_bytes())?;
        fixed.push(name);
    }
    Ok(fixed)
}

/// 取出某个标签对应的完整片段（含自闭合）
fn take_element(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}"))?;
    let rest = &xml[start..];
    let head_end = rest.find('>')?;
    if rest[..=head_end].ends_with("/>") {
        return Some(rest[..=head_end].to_string());
    }
    let close = format!("</{tag}>");
    let end = rest.find(&close)?;
    Some(rest[..end + close.len()].to_string())
}

/// umya 重写工作表时会丢掉 <drawing>/<tableParts>/<pivotTable> 这些「引用元素」，
/// 部件和关系都在、表里却没指过去 —— Excel 直接判包损坏。这里按基准补回来。
pub fn restore_sheet_refs(path: &Path, base: &Path) -> Result<Vec<String>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let base_targets = crate::patch::sheet_targets(base)?;
    let new_targets = crate::patch::sheet_targets(path)?;
    let order = [
        "drawing",
        "legacyDrawing",
        "legacyDrawingHF",
        "picture",
        "oleObjects",
        "controls",
        "tableParts",
        "extLst",
    ];
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    let mut log = Vec::new();

    for (sheet, base_part) in base_targets.iter() {
        let Some(new_part) = new_targets.get(sheet) else { continue };
        let Some(base_xml) = read_part(base, base_part).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let Some(mut new_xml) = read_part(path, new_part).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        // pivotTable 元素不是尾部元素，单独处理
        let mut add = String::new();
        let mut added = 0usize;
        for tag in order {
            if new_xml.contains(&format!("<{tag}")) {
                continue;
            }
            if let Some(frag) = take_element(&base_xml, tag) {
                add.push_str(&frag);
                added += 1;
            }
        }
        if let Some(close) = new_xml.rfind("</worksheet>") {
            if added > 0 {
                new_xml.insert_str(close, &add);
            }
        }
        // pivotTable：基准里每个 <pivotTable r:id> 都补回去（放在 tableParts 之前）
        let mut piv = String::new();
        let mut rest = base_xml.as_str();
        while let Some(pos) = rest.find("<pivotTable ") {
            let end = rest[pos..].find("/>").map(|e| pos + e + 2).unwrap_or(rest.len());
            let frag = &rest[pos..end];
            let id = frag
                .find("r:id=\"")
                .and_then(|i| frag[i + 6..].find('"').map(|q| frag[i + 6..i + 6 + q].to_string()));
            if let Some(id) = id {
                if !new_xml.contains(&format!("r:id=\"{id}\"")) {
                    piv.push_str(frag);
                    added += 1;
                }
            }
            rest = &rest[end.min(rest.len())..];
        }
        if !piv.is_empty() {
            if let Some(pos) = new_xml.find("<tableParts")
                .or_else(|| new_xml.rfind("</worksheet>"))
            {
                new_xml.insert_str(pos, &piv);
            }
        }
        if added > 0 {
            pending.push((new_part.clone(), new_xml.into_bytes()));
            log.push(format!("{sheet}: 补回 {added} 个引用元素"));
        }
    }
    crate::patch::append_entries(path, &pending)?;
    Ok(log)
}

/// workbook 级关系与 <pivotCaches>：umya 会把透视缓存的登记整段删掉，
/// 但工作表里还指着 pivotTable 部件 —— Excel 认为包损坏（WPS 不校验）。
pub fn migrate_workbook_rels(path: &Path, base: &Path) -> Result<Vec<String>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let Some(base_rels) = read_part(base, "xl/_rels/workbook.xml.rels").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let Some(new_rels) = read_part(path, "xl/_rels/workbook.xml.rels").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let Some(base_wb) = read_part(base, "xl/workbook.xml").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let Some(new_wb) = read_part(path, "xl/workbook.xml").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let parts = entries(path);
    let have = parse_rels(&new_rels);
    let mut add = String::new();
    let mut added = 0usize;

    for (id, (ty, target)) in parse_rels(&base_rels) {
        if have.contains_key(&id) {
            continue;
        }
        // 工作表/样式/共享字符串由 umya 管，别的（透视缓存、切片器…）缺了就补
        if ty.ends_with("/worksheet") || ty.ends_with("/styles") || ty.ends_with("/sharedStrings")
            || ty.ends_with("/theme") || ty.ends_with("/calcChain")
        {
            continue;
        }
        let abs = resolve_target("xl/_rels/workbook.xml.rels", &target);
        if !parts.iter().any(|p| p == &abs) {
            continue;
        }
        let rel = relative_to("xl/_rels/workbook.xml.rels", &abs);
        add.push_str(&format!("<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{rel}\"/>"));
        added += 1;
    }

    let mut log = Vec::new();
    if added > 0 {
        let merged = new_rels.replacen("</Relationships>", &format!("{add}</Relationships>"), 1);
        crate::patch::append_entry(path, "xl/_rels/workbook.xml.rels", merged.as_bytes())?;
        log.push(format!("workbook rels 补 {added} 条"));
    }

    // <pivotCaches> 整段照抄（r:id 与上面补的关系一致）
    if !new_wb.contains("<pivotCaches") {
        if let Some(start) = base_wb.find("<pivotCaches") {
            if let Some(endrel) = base_wb[start..].find("</pivotCaches>") {
                let block = &base_wb[start..start + endrel + "</pivotCaches>".len()];
                let mut wb2 = new_wb.clone();
                let pos = wb2
                    .find("<calcPr")
                    .or_else(|| wb2.find("</workbook>"))
                    .unwrap_or(wb2.len());
                wb2.insert_str(pos, block);
                crate::patch::append_entry(path, "xl/workbook.xml", wb2.as_bytes())?;
                log.push("workbook.xml 补回 pivotCaches".to_string());
            }
        }
    }
    Ok(log)
}

/// 补齐内容类型：从基准文件把缺的 Default（按扩展名）和 Override（按部件）抄过来。
/// 少一条 Excel 就判整个包损坏（WPS 不校验，所以能开）。
pub fn ensure_content_types(path: &Path, base: &Path) -> Result<Vec<String>> {
    let Some(base_ct) = read_part(base, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let Some(new_ct) = read_part(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let parts = entries(path);
    let mut add = String::new();
    let mut added = 0usize;

    // 1) Default：按扩展名
    let mut rest = base_ct.as_str();
    while let Some(pos) = rest.find("<Default") {
        let end = rest[pos..].find("/>").map(|e| pos + e + 2).unwrap_or(rest.len());
        let tag = &rest[pos..end];
        if let Some(ext) = tag
            .find("Extension=\"")
            .and_then(|i| tag[i + 11..].find('"').map(|q| tag[i + 11..i + 11 + q].to_string()))
        {
            let pat = format!("Extension=\"{ext}\"");
            if !new_ct.contains(&pat) && !add.contains(&pat) {
                add.push_str(tag);
                added += 1;
            }
        }
        rest = &rest[end.min(rest.len())..];
    }

    // 2) Override：只补「新包里存在但没声明」的部件
    let mut rest = base_ct.as_str();
    while let Some(pos) = rest.find("<Override") {
        let end = rest[pos..].find("/>").map(|e| pos + e + 2).unwrap_or(rest.len());
        let tag = &rest[pos..end];
        if let Some(part) = tag
            .find("PartName=\"")
            .and_then(|i| tag[i + 10..].find('"').map(|q| tag[i + 10..i + 10 + q].to_string()))
        {
            let name = part.trim_start_matches('/').to_string();
            let exists = parts.iter().any(|p| p == &name);
            let declared = new_ct.contains(&format!("PartName=\"{part}\""));
            let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
            let has_default = new_ct.contains(&format!("Extension=\"{ext}\""));
            if exists && !declared && !has_default {
                add.push_str(tag);
                added += 1;
            }
        }
        rest = &rest[end.min(rest.len())..];
    }

    if added == 0 {
        return Ok(Vec::new());
    }
    let merged = new_ct.replacen("</Types>", &format!("{add}</Types>"), 1);
    crate::patch::append_entry(path, "[Content_Types].xml", merged.as_bytes())?;
    Ok(vec![format!("[Content_Types] 补 {added} 条声明")])
}

/// 一张表只能有一条 comments / vmlDrawing 关系：迁移时可能和 umya 写的重复，
/// 这里保留第一条（umya 写的），把多余的删掉，随后 prune 会清掉孤儿部件。
pub fn dedupe_singleton_rels(path: &Path) -> Result<Vec<String>> {
    let mut log = Vec::new();
    for name in entries(path) {
        if !name.ends_with(".rels") {
            continue;
        }
        let Some(xml) = read_part(path, &name).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = String::new();
        let mut dropped = 0usize;
        let mut rest = xml.as_str();
        let head_end = xml.find("<Relationship").unwrap_or(xml.len());
        out.push_str(&xml[..head_end]);
        while let Some(pos) = rest.find("<Relationship") {
            let end = rest[pos..].find("/>").map(|e| pos + e + 2).unwrap_or(rest.len());
            let tag = &rest[pos..end];
            let ty = tag
                .find("Type=\"")
                .and_then(|i| tag[i + 6..].find('"').map(|q| &tag[i + 6..i + 6 + q]))
                .unwrap_or("");
            let singleton = ty.contains("/comments") || ty.contains("/vmlDrawing");
            if singleton && !seen.insert(ty.to_string()) {
                dropped += 1;
            } else {
                out.push_str(tag);
            }
            rest = &rest[end.min(rest.len())..];
        }
        if dropped == 0 {
            continue;
        }
        if let Some(close) = xml.rfind("</Relationships>") {
            out.push_str(&xml[close..]);
        }
        crate::patch::append_entry(path, &name, out.as_bytes())?;
        log.push(format!("{name}: 去掉 {dropped} 条重复关系"));
    }
    Ok(log)
}

/// 删掉没有任何关系指向的部件（umya 每次保存都会留下旧的表格/批注部件）
pub fn prune_orphans(path: &Path) -> Result<Vec<String>> {
    let all = entries(path);
    // 收集所有 rels 里出现过的 Target（相对路径统一成 xl/ 开头）
    let mut referenced: HashSet<String> = HashSet::new();
    for n in all.iter() {
        if !n.ends_with(".rels") {
            continue;
        }
        let Some(xml) = read_part(path, n).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let dir = n.rsplit_once("_rels/").map(|(d, _)| d).unwrap_or("xl/");
        for (_id, (_t, target)) in parse_rels(&xml) {
            let t = target.trim_start_matches('/').to_string();
            if target.starts_with('/') {
                referenced.insert(t);
            } else {
                // 相对路径：按 rels 所在目录解析
                let mut parts: Vec<&str> = dir.trim_end_matches('/').split('/').collect();
                for seg in t.split('/') {
                    match seg {
                        ".." => {
                            parts.pop();
                        }
                        "." => {}
                        other => parts.push(other),
                    }
                }
                referenced.insert(parts.join("/"));
            }
        }
    }

    // 只清理这几类：表格、批注、图表、media
    let prunable = |n: &str| -> bool {
        (n.starts_with("xl/tables/") || n.starts_with("xl/comments") || n.starts_with("xl/charts/"))
            && n.ends_with(".xml")
    };
    let mut removed = Vec::new();
    for n in all.iter() {
        if !prunable(n) || referenced.contains(n) {
            continue;
        }
        removed.push(n.clone());
    }
    crate::patch::remove_entries(path, &removed)?;
    // Content_Types 里的 Override 也要清掉，否则校验会报“声明了不存在的部件”
    if !removed.is_empty() {
        if let Some(xml) = read_part(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()) {
            let mut out = String::new();
            let mut rest = xml.as_str();
            while let Some(pos) = rest.find("<Override") {
                let end = rest[pos..].find("/>").map(|e| pos + e + 2).unwrap_or(rest.len());
                let tag = &rest[pos..end];
                let drop = removed.iter().any(|r| tag.contains(&format!("PartName=\"/{r}\"")));
                if !drop {
                    out.push_str(tag);
                }
                rest = &rest[end.min(rest.len())..];
            }
            let head_end = xml.find("<Override").unwrap_or(xml.len());
            let mut merged = String::from(&xml[..head_end]);
            merged.push_str(&out);
            if let Some(close) = xml.rfind("</Types>") {
                merged.push_str(&xml[close..]);
            }
            crate::patch::append_entry(path, "[Content_Types].xml", merged.as_bytes())?;
        }
    }
    Ok(removed)
}

// ---------------- 工作表关系迁移 ----------------

fn split_path(p: &str) -> Vec<String> {
    p.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
}

/// 把 rels 里的相对 Target 解析成包内绝对路径
pub fn resolve_target(rels_name: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let dir = rels_name.rsplit_once("_rels/").map(|(d, _)| d).unwrap_or("");
    let mut parts = split_path(dir);
    for seg in split_path(target) {
        match seg.as_str() {
            ".." => {
                parts.pop();
            }
            "." => {}
            other => parts.push(other.to_string()),
        }
    }
    parts.join("/")
}

/// 从某个目录出发，算出到目标部件的相对路径
fn relative_to(from_rels: &str, abs: &str) -> String {
    let dir = from_rels.rsplit_once("_rels/").map(|(d, _)| d).unwrap_or("");
    let from: Vec<String> = split_path(dir);
    let to: Vec<String> = split_path(abs);
    let mut common = 0;
    while common < from.len() && common < to.len() && from[common] == to[common] {
        common += 1;
    }
    let mut out: Vec<String> = vec!["..".to_string(); from.len() - common];
    out.extend(to[common..].iter().cloned());
    if out.is_empty() {
        ".".to_string()
    } else {
        out.join("/")
    }
}

/// umya 把工作表从原目录挪到 xl/worksheets/ 时不会搬 rels，
/// 结果图表/图片/表格的关系全丢。这里按基准文件把关系补到新位置，
/// 相对路径按新目录重算，并顺带把 Content_Types 里缺的 Override 补上。
pub fn migrate_sheet_rels(path: &Path, base: &Path) -> Result<Vec<String>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let base_targets = crate::patch::sheet_targets(base)?;
    let new_targets = crate::patch::sheet_targets(path)?;
    let mut log = Vec::new();
    let mut need_override: Vec<String> = Vec::new();
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();

    for (sheet, base_part) in base_targets.iter() {
        let Some(new_part) = new_targets.get(sheet) else { continue };
        // 路径没变也要检查：umya 偶尔会把表的 rels 整个漏掉（S4 的 30 张图就这样丢的）
        let base_rels = base_part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        let Some(old_xml) = read_part(base, &base_rels).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let new_rels = new_part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        let existing = read_part(path, &new_rels)
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        // 已经存在的按「类型 + 目标（解析成绝对路径）」判重，避免换了个 rId 就重复加
        let have: HashSet<String> = parse_rels(&existing)
            .values()
            .map(|(t, target)| format!("{t}|{}", resolve_target(&new_rels, target)))
            .collect();

        let mut add = String::new();
        let mut added = 0usize;
        // 这些类型一张表只能有一条：umya 已经写了自己的版本，就别再塞基准的旧版本
        let singleton = |ty: &str| ty.contains("/comments\"") || ty.contains("/vmlDrawing\"");
        let have_singleton: HashSet<String> = parse_rels(&existing)
            .values()
            .filter(|(t, _)| singleton(t))
            .map(|(t, _)| t.clone())
            .collect();

        for (id, (ty, target)) in parse_rels(&old_xml) {
            let abs = resolve_target(&base_rels, &target);
            if have.contains(&format!("{ty}|{abs}")) {
                continue;
            }
            if singleton(&ty) && have_singleton.contains(&ty) {
                continue;
            }
            let rel = relative_to(&new_rels, &abs);
            add.push_str(&format!(
                "<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{rel}\"/>"
            ));
            need_override.push(abs);
            added += 1;
        }
        if add.is_empty() {
            continue;
        }
        let merged = if existing.trim().is_empty() {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{add}</Relationships>"
            )
        } else {
            existing.replacen(
                "</Relationships>",
                &format!("{add}</Relationships>"),
                1,
            )
        };
        pending.push((new_rels.clone(), merged.into_bytes()));
        log.push(format!("{sheet}: {base_part} → {new_part}，补 {added} 条关系"));
    }

    // Content_Types：引用了原来目录下的部件，就要有对应的 Override
    if !need_override.is_empty() {
        let base_ct = read_part(base, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok());
        let new_ct = read_part(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok());
        if let (Some(base_ct), Some(new_ct)) = (base_ct, new_ct) {
            let mut add = String::new();
            for abs in need_override.iter() {
                let pat = format!("PartName=\"/{abs}\"");
                if new_ct.contains(&pat) || add.contains(&pat) {
                    continue;
                }
                // 从基准里把这条 Override 原样捞出来
                if let Some(pos) = base_ct.find(&pat) {
                    let start = base_ct[..pos].rfind("<Override").unwrap_or(pos);
                    if let Some(endrel) = base_ct[start..].find("/>") {
                        add.push_str(&base_ct[start..start + endrel + 2]);
                    }
                }
            }
            if !add.is_empty() {
                let merged = new_ct.replacen("</Types>", &format!("{add}</Types>"), 1);
                pending.push(("[Content_Types].xml".to_string(), merged.into_bytes()));
                log.push(format!("[Content_Types] 补 {} 条", add.matches("<Override").count()));
            }
        }
    }
    // 一次重写把所有关系写进去（逐条写会把 100MB 的包重写十几遍）
    crate::patch::append_entries(path, &pending)?;
    Ok(log)
}
