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


/// 包里有哪些部件（含暂存中的改动）
pub fn parts(path: &Path) -> Vec<String> {
    entries(path)
}

/// 一次写入多个部件
pub fn write_parts(path: &Path, items: &[(String, Vec<u8>)]) -> Result<()> {
    crate::patch::append_entries(path, items)
}

/// 只读磁盘上的包，不看暂存（用于读"基准文件"，那是另一个文件）
pub fn read_part_file(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

pub fn read_part(path: &Path, name: &str) -> Option<Vec<u8>> {
    if let Some(v) = crate::patch::staged_at(path, name) {
        return v;
    }
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// 包内全部条目名（含暂存），供需要按后缀扫描的调用方使用
pub fn entries_of(path: &Path) -> Vec<String> {
    entries(path)
}

fn entries(path: &Path) -> Vec<String> {
    let deleted = crate::patch::staged_deleted_at(path);
    let mut list: Vec<String> = match std::fs::File::open(path).ok().and_then(|f| zip::ZipArchive::new(f).ok()) {
        Some(mut zip) => (0..zip.len())
            .filter_map(|i| zip.by_index_raw(i).ok().map(|e| e.name().to_string()))
            .filter(|n| !deleted.contains(n))
            .collect(),
        None => Vec::new(),
    };
    // 暂存里的改动还没落盘，这里要一并算上
    for n in crate::patch::staged_names_at(path) {
        if !list.contains(&n) {
            list.push(n);
        }
    }
    list
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
        let Some(old_xml) = read_part_file(base, &name).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let Some(new_xml) = read_part(path, &name).and_then(|b| String::from_utf8(b).ok()) else {
            // 新文件里整个 rels 都没了：直接把基准的搬过来
            if let Some(raw) = read_part_file(base, &name) {
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
            || sheet.styles_dirty
            || sheet.row_styles_dirty
            || sheet.col_styles_dirty
            || !sheet.style_edits.is_empty()
            || !sheet.cleared.is_empty()
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

        if base_part == new_part && crate::patch::entry_size(base, base_part) > 4 * 1024 * 1024 {
            // 大表直接搬压缩后的字节，省掉解压 + 重压缩（215 MB 的表很值）
            crate::patch::stage_raw_copy(base_part, new_part);
            log.push(sheet.name.clone());
            continue;
        }
        let Some(xml) = read_part_file(base, base_part) else { continue };
        pending.push((new_part.clone(), xml));

        // 关系表：路径可能变了，Target 要按新目录重算
        let base_rels = base_part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        let new_rels = new_part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        if base_part == new_part {
            // 路径没变：rels 原样照抄，一个字节都不改
            if let Some(raw) = read_part_file(base, &base_rels) {
                if let Ok(text) = String::from_utf8(raw.clone()) {
                    for (_, (_, target)) in parse_rels(&text) {
                        let abs = resolve_target(&base_rels, &target);
                        if let Some(body) = read_part_file(base, &abs) {
                            pending.push((abs.clone(), body));
                        }
                    }
                }
                pending.push((base_rels.clone(), raw));
                log.push(sheet.name.clone());
                continue;
            }
        }
        if let Some(rels) = read_part_file(base, &base_rels).and_then(|b| String::from_utf8(b).ok()) {
            let mut out = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
            );
            for (id, (ty, target)) in parse_rels(&rels) {
                let abs = resolve_target(&base_rels, &target);
                // 被引用的部件也按基准恢复（表格定义、绘图、VML、批注…）
                if base_part != new_part {
                    if let Some(body) = read_part_file(base, &abs) {
                        pending.push((abs.clone(), body));
                    }
                } else if crate::patch::entry_size(base, &abs) > 4 * 1024 * 1024 {
                    crate::patch::stage_raw_copy(&abs, &abs);
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
    let mut reserved: HashSet<String> = base_targets.values().cloned().collect();
    reserved.extend(new_targets.values().cloned());
    for (name, npart) in &new_targets {
        let destination = if let Some(bpart) = base_targets.get(name) {
            bpart.clone()
        } else if base_targets.values().any(|part| part == npart) {
            // A newly added sheet may occupy a retained sheet's original path
            // after umya renumbers everything. Give it its own stable part.
            let mut n = 1;
            loop {
                let candidate = format!("xl/worksheets/native-sheet-{n}.xml");
                if reserved.insert(candidate.clone()) { break candidate; }
                n += 1;
            }
        } else {
            npart.clone()
        };
        if npart != &destination {
            moves.push((npart.clone(), destination));
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
        // 搬过来的部件本身没 rels，但基准有（打印机设置之类）：把基准的补回去
        if rels_raw.is_none() {
            if let Some(raw) = read_part_file(base, &brels) {
                pending.push((brels.clone(), raw));
            }
        }
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
    let dests: std::collections::HashSet<String> = moves.iter()
        .flat_map(|(_, b)| [b.clone(), rels_of(b)]).collect();
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
    if !base.exists() {
        return Ok(false);
    }
    let mut base_xml = match read_part_file(base, "xl/workbook.xml").and_then(|b| String::from_utf8(b).ok()) {
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
                base_names.push(crate::model::xml_unescape(&after[..q]));
            }
        }
    }
    let here: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    if here != base_names {
        return Ok(false);
    }
    // Name/page edits affect only definedNames. Overlay that section while
    // retaining unrelated workbook extensions, pivot caches and properties.
    if wb.names_dirty || wb.sheets.iter().any(|s| s.page_dirty) {
        let current = read_part(path, "xl/workbook.xml")
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(|| anyhow::anyhow!("无法读取已修改的工作簿名称"))?;
        let names = defined_names_span(&current).map(|(a, b)| &current[a..b]).unwrap_or("");
        if let Some((a, b)) = defined_names_span(&base_xml) {
            base_xml.replace_range(a..b, names);
        } else if !names.is_empty() {
            if let Some(i) = base_xml.find("</sheets>") {
                base_xml.insert_str(i + "</sheets>".len(), names);
            }
        }
    }
    let mut pending: Vec<(String, Vec<u8>)> = vec![("xl/workbook.xml".to_string(), base_xml.into_bytes())];
    if let Some(rels) = read_part_file(base, "xl/_rels/workbook.xml.rels") {
        pending.push(("xl/_rels/workbook.xml.rels".to_string(), rels));
    }
    crate::patch::append_entries(path, &pending)?;
    Ok(true)
}

fn defined_names_span(xml: &str) -> Option<(usize, usize)> {
    let start = xml.find("<definedNames")?;
    let head_end = start + xml[start..].find('>')? + 1;
    let end = if xml[start..head_end].ends_with("/>") {
        head_end
    } else {
        head_end + xml[head_end..].find("</definedNames>")? + "</definedNames>".len()
    };
    Some((start, end))
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


/// 所有工作表 rels 一律用基准的（部件已经归位，路径对得上）。
///
/// 归位过程中两个目录下的同名表会抢同一个 rels 路径，谁后写谁赢；
/// 统一以基准为准最稳，也顺带修好"表里有 r:id 但 rels 丢了"。
pub fn restore_base_sheet_rels(
    path: &Path,
    base: &Path,
    wb: &crate::model::Workbook,
) -> Result<usize> {
    if !base.exists() {
        return Ok(0);
    }
    // 动过批注/绘图/图表的表不能覆盖 rels，否则新加的关系会被抹掉
    let dirty: std::collections::HashSet<String> = wb
        .sheets
        .iter()
        .filter(|s| s.notes_dirty || s.charts_dirty || s.merges_dirty)
        .map(|s| s.name.clone())
        .collect();
    // 直接按基准里的 rels 清单恢复：这时候输出里的表路径可能还在暂存里，
    // 用 sheet_targets 反查会漏（同名不同目录的表尤其容易漏）
    let part_to_name: std::collections::HashMap<String, String> = crate::patch::sheet_targets(base)?
        .into_iter()
        .map(|(n, p)| (p, n))
        .collect();
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in entries(base) {
        if !entry.contains("/worksheets/_rels/") || !entry.ends_with(".rels") {
            continue;
        }
        let sheet_part = entry.replace("/_rels/", "/").trim_end_matches(".rels").to_string();
        if !entry.starts_with("xl/") || !sheet_part.ends_with(".xml") {
            continue;
        }
        if part_to_name.get(&sheet_part).is_some_and(|n| dirty.contains(n)) {
            continue;
        }
        if let Some(raw) = read_part_file(base, &entry) {
            pending.push((entry, raw));
        }
    }
    let n = pending.len();
    if n > 0 {
        crate::patch::append_entries(path, &pending)?;
    }
    Ok(n)
}


/// 只把单元格数据换进去，工作表其余骨架沿用基准。
///
/// umya 重写整张表时会丢 <sheetPr>、绘图引用、扩展区等；这里保留基准的
/// 头尾，只替换 <sheetData>，改动最小、保真度最高。
pub fn merge_sheet_data(path: &Path, base: &Path, wb: &crate::model::Workbook) -> Result<Vec<String>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let base_targets = crate::patch::sheet_targets(base)?;
    let new_targets = crate::patch::sheet_targets(path)?;
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    let mut done = Vec::new();

    for sheet in wb.sheets.iter() {
        // 只处理"单纯改了单元格"的表；批注/条件格式/合并等改动由各自的补丁负责，
        // 混在一起会被基准骨架覆盖掉
        let plain_edit = sheet.edited
            && !sheet.notes_dirty
            && !sheet.cf_dirty
            && !sheet.dv_dirty
            && !sheet.merges_dirty
            && !sheet.charts_dirty;
        if !plain_edit {
            continue;
        }
        let (Some(bpart), Some(npart)) = (base_targets.get(&sheet.name), new_targets.get(&sheet.name))
        else {
            continue;
        };
        let (Some(bxml), Some(nxml)) = (
            read_part_file(base, bpart).and_then(|b| String::from_utf8(b).ok()),
            read_part(path, npart).and_then(|b| String::from_utf8(b).ok()),
        ) else {
            continue;
        };
        let (Some(b_start), Some(b_end)) = (bxml.find("<sheetData"), bxml.find("</sheetData>")) else {
            continue;
        };
        let (Some(n_start), Some(n_end)) = (nxml.find("<sheetData"), nxml.find("</sheetData>")) else {
            continue;
        };
        let b_end = b_end + "</sheetData>".len();
        let n_end = n_end + "</sheetData>".len();
        let mut head = bxml[..b_start].to_string();
        if sheet.col_styles_dirty {
            if let (Some(ns_c), Some(ne_c)) = (nxml[..n_start].find("<cols"), nxml[..n_start].find("</cols>")) {
                let ncol = &nxml[ns_c..ne_c + "</cols>".len()];
                if let (Some(hs_c), Some(he_c)) = (head.find("<cols"), head.find("</cols>")) {
                    head.replace_range(hs_c..he_c + "</cols>".len(), ncol);
                } else {
                    head.push_str(ncol);
                }
            }
        }
        // dimension 用新算出来的（行列范围可能变了）
        if let (Some(ns), Some(ne)) = (nxml.find("<dimension"), nxml.find("/>").filter(|_| false)) {
            let _ = (ns, ne);
        }
        if let Some(nd) = nxml[n_start.saturating_sub(200)..n_start].rfind("<dimension") {
            let abs = n_start.saturating_sub(200) + nd;
            let tag_end = nxml[abs..].find("/>").map(|i| abs + i + 2);
            if let Some(te) = tag_end {
                if let Some(hs) = head.find("<dimension") {
                    let he = head[hs..].find("/>").map(|i| hs + i + 2);
                    if let Some(he) = he {
                        head.replace_range(hs..he, &nxml[abs..te]);
                    }
                }
            }
        }
        let merged = format!("{}{}{}", head, &nxml[n_start..n_end], &bxml[b_end..]);
        pending.push((npart.clone(), merged.into_bytes()));
        done.push(sheet.name.clone());
    }

    if !pending.is_empty() {
        crate::patch::append_entries(path, &pending)?;
    }
    Ok(done)
}


/// 内容类型以基准为准重建：只保留实际存在的部件。
///
/// 归位后部件路径和 umya 写的那套对不上，CT 里就会缺 Override，
/// 工作表退回 Default(xml) → Excel 认为类型非法、拒收。
pub fn rebuild_content_types(path: &Path, base: &Path) -> Result<bool> {
    if !base.exists() {
        return Ok(false);
    }
    let (Some(bct), Some(oct)) = (
        read_part_file(base, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()),
        read_part(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()),
    ) else {
        return Ok(false);
    };
    let names: std::collections::HashSet<String> = entries(path).into_iter().collect();
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
    );
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push_overrides = |src: &str, out: &mut String, seen: &mut std::collections::HashSet<String>| {
        let mut rest = src;
        while let Some(i) = rest.find("<Override ") {
            rest = &rest[i..];
            let end = match rest.find("/>") {
                Some(e) => e + 2,
                None => break,
            };
            let tag = &rest[..end];
            let name = tag
                .find("PartName=\"")
                .map(|p| {
                    let after = &tag[p + 10..];
                    after.split('"').next().unwrap_or("").trim_start_matches('/').to_string()
                })
                .unwrap_or_default();
            let ct = tag
                .find("ContentType=\"")
                .map(|p| {
                    let after = &tag[p + 13..];
                    after.split('"').next().unwrap_or("").to_string()
                })
                .unwrap_or_default();
            if !name.is_empty() && names.contains(&name) && seen.insert(name.clone()) {
                out.push_str(&format!(
                    "<Override PartName=\"/{name}\" ContentType=\"{ct}\"/>"
                ));
            }
            rest = &rest[end..];
        }
    };
    // 默认项：以基准为主，补上输出里独有扩展名
    for tag in ["Default"] {
        let mut rest = bct.as_str();
        while let Some(i) = rest.find(&format!("<{tag} ")) {
            rest = &rest[i..];
            let end = match rest.find("/>") {
                Some(e) => e + 2,
                None => break,
            };
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    push_overrides(&bct, &mut out, &mut seen);
    push_overrides(&oct, &mut out, &mut seen);
    out.push_str("</Types>");
    crate::patch::append_entry(path, "[Content_Types].xml", out.as_bytes())?;
    Ok(true)
}


/// 工作表用了共享字符串（t="s"）就必须有 sharedStrings 关系。
///
/// 骨架沿用基准时，基准用的是内联字符串、没有这条关系；
/// 而改写过的表按共享字符串写，缺关系 Excel 解析不出来，整包拒收。
pub fn ensure_shared_strings_rel(path: &Path) -> Result<bool> {
    let names: std::collections::HashSet<String> = entries(path).into_iter().collect();
    if !names.contains("xl/sharedStrings.xml") {
        return Ok(false);
    }
    let wrels = "xl/_rels/workbook.xml.rels";
    let Some(raw) = read_part(path, wrels).and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(false);
    };
    if raw.contains("/sharedStrings") {
        return Ok(false);
    }
    let mut max_id = 0u32;
    let mut rest = raw.as_str();
    while let Some(i) = rest.find("Id=\"rId") {
        rest = &rest[i + 7..];
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(v) = num.parse::<u32>() {
            max_id = max_id.max(v);
        }
    }
    let id = format!("rId{}", max_id + 1);
    let rel = format!(
        "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings\" Target=\"sharedStrings.xml\"/>"
    );
    let out = match raw.rfind("</Relationships>") {
        Some(i) => format!("{}{}{}", &raw[..i], rel, &raw[i..]),
        None => return Ok(false),
    };
    crate::patch::append_entry(path, wrels, out.as_bytes())?;
    Ok(true)
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
    // 按完整标签名找起点（避免 legacyDrawing 命中 legacyDrawingHF）
    let pat = format!("<{tag}");
    let mut start = None;
    let mut from = 0usize;
    while let Some(i) = xml[from..].find(&pat) {
        let at = from + i + pat.len();
        match xml[at..].chars().next() {
            Some(' ') | Some('/') | Some('>') | Some('\r') | Some('\n') => {
                start = Some(from + i);
                break;
            }
            _ => from = at,
        }
    }
    let start = start?;
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
/// 标签名必须完整匹配：`<legacyDrawing` 不能命中 `<legacyDrawingHF`
fn has_tag(xml: &str, tag: &str) -> bool {
    let pat = format!("<{tag}");
    let mut from = 0usize;
    while let Some(i) = xml[from..].find(&pat) {
        let at = from + i + pat.len();
        match xml[at..].chars().next() {
            Some(' ') | Some('/') | Some('>') | Some('\r') | Some('\n') => return true,
            _ => from = at,
        }
    }
    false
}

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
        let Some(base_xml) = read_part_file(base, base_part).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let Some(mut new_xml) = read_part(path, new_part).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        // pivotTable 元素不是尾部元素，单独处理
        let mut add = String::new();
        let mut added = 0usize;
        for tag in order {
            if has_tag(&new_xml, tag) {
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
    let Some(base_rels) = read_part_file(base, "xl/_rels/workbook.xml.rels").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let Some(new_rels) = read_part(path, "xl/_rels/workbook.xml.rels").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(Vec::new());
    };
    let Some(base_wb) = read_part_file(base, "xl/workbook.xml").and_then(|b| String::from_utf8(b).ok()) else {
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
    let Some(base_ct) = read_part_file(base, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()) else {
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
            // 不能因为存在同扩展名的 Default 就跳过 Override：Default 的
            // `xml -> application/xml` 不是 worksheet/chart 的具体类型，
            // 少这一条 Excel 会判整包损坏（WPS 不校验，照样能开）。
            if exists && !declared {
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
        let Some(old_xml) = read_part_file(base, &base_rels).and_then(|b| String::from_utf8(b).ok()) else {
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
        let base_ct = read_part_file(base, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok());
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

fn parse_a1_coord(s: &str) -> Option<(u32, u32)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut col = 0u32;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        col = col
            .checked_mul(26)?
            .checked_add((b[i].to_ascii_uppercase() - b'A' + 1) as u32)?;
        i += 1;
    }
    if i == 0 || i == b.len() {
        return None;
    }
    let mut row = 0u32;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            return None;
        }
        row = row.checked_mul(10)?.checked_add((b[i] - b'0') as u32)?;
        i += 1;
    }
    if row == 0 || col == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

/// quotePrefix 是 xf 上的未建模属性，umya 不认它：一旦保存时重建样式表就会丢。
/// 保存后按「哪些格需要它」补一条带 quotePrefix="1" 的 xf，并把格的 s= 指过去。
/// 采用单遍 O(XML_LEN) 线性扫描与一次性批量回写，彻底消除万格表的二次方查找与 memmove。
pub fn ensure_quote_prefix(path: &Path, sheets: &[(String, Vec<(u32, u32)>)]) -> Result<usize> {
    if sheets.iter().all(|(_, cells)| cells.is_empty()) {
        return Ok(0);
    }
    let Some(xml) = read_part(path, "xl/styles.xml").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(0);
    };
    let Some(start) = xml.find("<cellXfs") else { return Ok(0) };
    let Some(gt) = xml[start..].find('>').map(|i| start + i) else { return Ok(0) };
    let Some(end) = xml[start..].find("</cellXfs>").map(|i| start + i) else { return Ok(0) };
    let block = &xml[gt + 1..end];
    let orig_xf_count = split_xf(block).len();
    let mut xfs = split_xf(block);
    let mut added = HashMap::<usize, usize>::new();
    let mut total = 0usize;
    let mut modified_parts: Vec<(String, Vec<u8>)> = Vec::new();

    let mut get_quote_xf = |old: usize| -> usize {
        if let Some(&idx) = added.get(&old) {
            return idx;
        }
        if let Some(existing) = xfs.get(old) {
            if existing.contains("quotePrefix=\"1\"") || existing.contains("quotePrefix=\"true\"") {
                added.insert(old, old);
                return old;
            }
        }
        let mut base = xfs.get(old).cloned().unwrap_or_else(|| {
            "<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>".to_string()
        });
        if !base.contains("quotePrefix=") {
            if let Some(g) = base.find('>') {
                // 自闭合 <xf .../> 要把属性插在 / 前面
                let at = if base[..g].ends_with('/') { g - 1 } else { g };
                base.insert_str(at, " quotePrefix=\"1\"");
            }
        }
        xfs.push(base);
        let idx = xfs.len() - 1;
        added.insert(old, idx);
        idx
    };

    for (part, cells) in sheets {
        if cells.is_empty() {
            continue;
        }
        let Some(bytes) = read_part(path, part) else { continue };
        let Ok(sheet) = String::from_utf8(bytes) else { continue };
        let want: HashSet<(u32, u32)> = cells.iter().copied().collect();
        let mut out = String::new();
        let mut emit = 0usize;
        let mut cursor = 0usize;
        let mut touched = false;

        while let Some(rel) = sheet[cursor..].find("<c r=\"") {
            let pos = cursor + rel;
            let coord_start = pos + 6;
            let Some(q_rel) = sheet[coord_start..].find('"') else {
                break;
            };
            let coord_end = coord_start + q_rel;
            let Some(gt_rel) = sheet[coord_end..].find('>') else {
                break;
            };
            let tag_end = coord_end + gt_rel;
            let coord_str = &sheet[coord_start..coord_end];
            if let Some(rc) = parse_a1_coord(coord_str) {
                if want.contains(&rc) {
                    let tag = &sheet[pos..=tag_end];
                    let had_s = tag.contains(" s=\"");
                    let old = attr_num(tag, "s").unwrap_or(0);
                    let new_idx = get_quote_xf(old);
                    if new_idx != old || !had_s {
                        if !touched {
                            out.reserve(sheet.len() + want.len() * 10);
                            touched = true;
                        }
                        out.push_str(&sheet[emit..pos]);
                        if had_s {
                            out.push_str(&tag.replacen(
                                &format!(" s=\"{old}\""),
                                &format!(" s=\"{new_idx}\""),
                                1,
                            ));
                        } else {
                            out.push_str(&sheet[pos..=coord_end]);
                            out.push_str(&format!(" s=\"{new_idx}\""));
                            out.push_str(&sheet[coord_end + 1..=tag_end]);
                        }
                        emit = tag_end + 1;
                        total += 1;
                    }
                }
            }
            cursor = tag_end + 1;
        }
        if touched {
            out.push_str(&sheet[emit..]);
            modified_parts.push((part.clone(), out.into_bytes()));
        }
    }
    if modified_parts.is_empty() && xfs.len() == orig_xf_count {
        return Ok(0);
    }
    if xfs.len() != orig_xf_count {
        let mut joined = String::new();
        for xf in &xfs {
            joined.push_str(xf);
        }
        let mut new_open = xml[start..=gt].to_string();
        if let Some(cpos) = new_open.find("count=\"") {
            let rest = &new_open[cpos + 7..];
            if let Some(q) = rest.find('"') {
                new_open.replace_range(cpos..cpos + 7 + q + 1, &format!("count=\"{}\"", xfs.len()));
            }
        }
        let rebuilt = format!("{}{}{}{}", &xml[..start], new_open, joined, &xml[end..]);
        modified_parts.push(("xl/styles.xml".to_string(), rebuilt.into_bytes()));
    }
    if !modified_parts.is_empty() {
        write_parts(path, &modified_parts)?;
    }
    Ok(total)
}


/// 保存后按「哪些格/行/列需要缩小字体填充」补一条带 shrinkToFit="1" 的 xf，并更新 s= / style= 索引。
/// 单遍 O(XML_LEN) 线性扫描替换，避免大表逐格 sheet.find + replace_range 造成数十秒卡顿。
pub fn ensure_shrink_to_fit(
    path: &Path,
    targets: &[(String, Vec<(u32, u32)>, Vec<u32>, Vec<u32>)],
) -> Result<usize> {
    if targets
        .iter()
        .all(|(_, cells, rows, cols)| cells.is_empty() && rows.is_empty() && cols.is_empty())
    {
        return Ok(0);
    }
    let Some(xml) = read_part(path, "xl/styles.xml").and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(0);
    };
    let Some(start) = xml.find("<cellXfs") else { return Ok(0) };
    let Some(gt) = xml[start..].find('>').map(|i| start + i) else { return Ok(0) };
    let Some(end) = xml[start..].find("</cellXfs>").map(|i| start + i) else { return Ok(0) };
    let block = &xml[gt + 1..end];
    let orig_xf_count = split_xf(block).len();
    let mut xfs = split_xf(block);
    let mut added = HashMap::<usize, usize>::new();
    let mut total = 0usize;
    let sheet_map = crate::patch::sheet_targets(path).unwrap_or_default();

    let sp_attr_num = |tag: &str, key: &str| -> Option<usize> {
        let pat = format!(" {key}=\"");
        let i = tag.find(&pat)? + pat.len();
        let rest = &tag[i..];
        let e = rest.find('"')?;
        rest[..e].parse().ok()
    };

    let set_sp_attr = |tag: &mut String, key: &str, val: &str| {
        let pat = format!(" {key}=\"");
        if let Some(p) = tag.find(&pat) {
            let vstart = p + pat.len();
            if let Some(vend) = tag[vstart..].find('"') {
                tag.replace_range(vstart..vstart + vend, val);
                return;
            }
        }
        let g = tag.len() - 1;
        let at = if g > 0 && tag.as_bytes()[g - 1] == b'/' { g - 1 } else { g };
        tag.insert_str(at, &format!(" {key}=\"{val}\""));
    };

    let mut get_shrink_xf = |old: usize| -> usize {
        if let Some(&idx) = added.get(&old) {
            return idx;
        }
        if let Some(existing) = xfs.get(old) {
            if existing.contains("shrinkToFit=\"1\"") || existing.contains("shrinkToFit=\"true\"") {
                added.insert(old, old);
                return old;
            }
        }
        let mut base = xfs.get(old).cloned().unwrap_or_else(|| {
            "<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>".to_string()
        });
        if !base.contains(" applyAlignment=") {
            if let Some(g) = base.find('>') {
                let at = if g > 0 && base.as_bytes()[g - 1] == b'/' { g - 1 } else { g };
                base.insert_str(at, " applyAlignment=\"1\"");
            }
        }
        if base.ends_with("/>") {
            base.truncate(base.len() - 2);
            base.push_str("><alignment shrinkToFit=\"1\"/></xf>");
        } else if let Some(ap) = base.find("<alignment") {
            let ins = ap + "<alignment".len();
            base.insert_str(ins, " shrinkToFit=\"1\"");
        } else if let Some(ce) = base.rfind("</xf>") {
            base.insert_str(ce, "<alignment shrinkToFit=\"1\"/>");
        }
        xfs.push(base);
        let idx = xfs.len() - 1;
        added.insert(old, idx);
        idx
    };

    let mut modified_parts: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, cells, rows, cols) in targets {
        if cells.is_empty() && rows.is_empty() && cols.is_empty() {
            continue;
        }
        let Some(part) = sheet_map.get(name) else { continue };
        let Some(bytes) = read_part(path, part) else { continue };
        let Ok(mut sheet) = String::from_utf8(bytes) else { continue };
        let mut touched = false;

        if !cells.is_empty() {
            let want_cells: HashSet<(u32, u32)> = cells.iter().copied().collect();
            let mut out = String::new();
            let mut emit = 0usize;
            let mut cursor = 0usize;
            let mut cell_touched = false;

            while let Some(rel) = sheet[cursor..].find("<c r=\"") {
                let pos = cursor + rel;
                let coord_start = pos + 6;
                let Some(q_rel) = sheet[coord_start..].find('"') else {
                    break;
                };
                let coord_end = coord_start + q_rel;
                let Some(gt_rel) = sheet[coord_end..].find('>') else {
                    break;
                };
                let tag_end = coord_end + gt_rel;
                if let Some(rc) = parse_a1_coord(&sheet[coord_start..coord_end]) {
                    if want_cells.contains(&rc) {
                        let orig_tag = &sheet[pos..=tag_end];
                        let old_opt = sp_attr_num(orig_tag, "s");
                        let old = old_opt.unwrap_or(0);
                        let new_idx = get_shrink_xf(old);
                        if new_idx != old || old_opt.is_none() {
                            if !cell_touched {
                                out.reserve(sheet.len() + want_cells.len() * 10);
                                cell_touched = true;
                            }
                            out.push_str(&sheet[emit..pos]);
                            let mut tag = orig_tag.to_string();
                            set_sp_attr(&mut tag, "s", &new_idx.to_string());
                            out.push_str(&tag);
                            emit = tag_end + 1;
                            total += 1;
                        }
                    }
                }
                cursor = tag_end + 1;
            }
            if cell_touched {
                out.push_str(&sheet[emit..]);
                sheet = out;
                touched = true;
            }
        }

        if !rows.is_empty() {
            let want_rows1: HashSet<u32> = rows.iter().map(|r| *r + 1).collect();
            let mut out = String::new();
            let mut emit = 0usize;
            let mut cursor = 0usize;
            let mut row_touched = false;

            while let Some(rel) = sheet[cursor..].find("<row ") {
                let pos = cursor + rel;
                let Some(gt_rel) = sheet[pos..].find('>') else {
                    break;
                };
                let tag_end = pos + gt_rel;
                let orig_tag = &sheet[pos..=tag_end];
                let r1 = orig_tag.find(" r=\"").and_then(|p| {
                    let after = &orig_tag[p + 4..];
                    let q = after.find('"')?;
                    after[..q].parse::<u32>().ok()
                });
                if let Some(r1) = r1 {
                    if want_rows1.contains(&r1) {
                        let old_opt = sp_attr_num(orig_tag, "s");
                        let old = old_opt.unwrap_or(0);
                        let new_idx = get_shrink_xf(old);
                        if new_idx != old || old_opt.is_none() {
                            if !row_touched {
                                out.reserve(sheet.len() + want_rows1.len() * 24);
                                row_touched = true;
                            }
                            out.push_str(&sheet[emit..pos]);
                            let mut tag = orig_tag.to_string();
                            set_sp_attr(&mut tag, "s", &new_idx.to_string());
                            if !tag.contains(" customFormat=") {
                                set_sp_attr(&mut tag, "customFormat", "1");
                            }
                            out.push_str(&tag);
                            emit = tag_end + 1;
                            total += 1;
                        }
                    }
                }
                cursor = tag_end + 1;
            }
            if row_touched {
                out.push_str(&sheet[emit..]);
                sheet = out;
                touched = true;
            }
        }

        for &c in cols {
            let col1 = (c + 1) as usize;
            let Some(cs) = sheet.find("<cols") else { break };
            let Some(ce) = sheet[cs..].find("</cols>").map(|i| cs + i) else { break };
            let cols_inner = &sheet[cs..ce];
            let mut scan = 0;
            let mut rep: Option<(usize, usize, String)> = None;
            while let Some(rel) = cols_inner[scan..].find("<col ") {
                let abs = cs + scan + rel;
                let Some(end_rel) = sheet[abs..].find("/>").map(|i| abs + i + 2) else { break };
                let tag = &sheet[abs..end_rel];
                let min_v = sp_attr_num(tag, "min").unwrap_or(0);
                let max_v = sp_attr_num(tag, "max").unwrap_or(0);
                if min_v <= col1 && col1 <= max_v {
                    let old = sp_attr_num(tag, "style").unwrap_or(0);
                    let new_idx = get_shrink_xf(old);
                    if new_idx != old || sp_attr_num(tag, "style").is_none() {
                        let mut rebuilt = String::new();
                        if min_v < col1 {
                            let mut left = tag.to_string();
                            set_sp_attr(&mut left, "max", &(col1 - 1).to_string());
                            rebuilt.push_str(&left);
                        }
                        let mut mid = tag.to_string();
                        set_sp_attr(&mut mid, "min", &col1.to_string());
                        set_sp_attr(&mut mid, "max", &col1.to_string());
                        set_sp_attr(&mut mid, "style", &new_idx.to_string());
                        rebuilt.push_str(&mid);
                        if col1 < max_v {
                            let mut right = tag.to_string();
                            set_sp_attr(&mut right, "min", &(col1 + 1).to_string());
                            rebuilt.push_str(&right);
                        }
                        rep = Some((abs, end_rel, rebuilt));
                    }
                    break;
                }
                scan += rel + 5;
            }
            if let Some((a, b, rtxt)) = rep {
                sheet.replace_range(a..b, &rtxt);
                touched = true;
                total += 1;
            }
        }

        if touched {
            modified_parts.push((part.clone(), sheet.into_bytes()));
        }
    }

    if total == 0 && xfs.len() == orig_xf_count && modified_parts.is_empty() {
        return Ok(0);
    }
    if xfs.len() != orig_xf_count {
        let mut joined = String::new();
        for xf in &xfs {
            joined.push_str(xf);
        }
        let mut new_open = xml[start..=gt].to_string();
        if let Some(cpos) = new_open.find("count=\"") {
            let rest = &new_open[cpos + 7..];
            if let Some(q) = rest.find('"') {
                new_open.replace_range(cpos..cpos + 7 + q + 1, &format!("count=\"{}\"", xfs.len()));
            }
        }
        let rebuilt = format!("{}{}{}{}", &xml[..start], new_open, joined, &xml[end..]);
        modified_parts.push(("xl/styles.xml".to_string(), rebuilt.into_bytes()));
    }
    if !modified_parts.is_empty() {
        write_parts(path, &modified_parts)?;
    }
    Ok(total)
}

fn split_xf(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = block[i..].find("<xf") {
        let s = i + p;
        let after = &block[s..];
        let next = after.as_bytes().get(3).copied().unwrap_or(b'>');
        if !(next == b' ' || next == b'>' || next == b'/') {
            i = s + 3;
            continue;
        }
        if let Some(gt) = after.find('>') {
            if after[..gt].ends_with('/') {
                out.push(after[..=gt].to_string());
                i = s + gt + 1;
                continue;
            }
        }
        let Some(c) = after.find("</xf>") else { break };
        out.push(after[..c + 5].to_string());
        i = s + c + 5;
    }
    out
}

fn cell_name(r: u32, c: u32) -> String {
    let mut n = c + 1;
    let mut s = String::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        s.insert(0, (b'A' + rem) as char);
        n = (n - 1) / 26;
    }
    format!("{s}{}", r + 1)
}

fn attr_num(tag: &str, key: &str) -> Option<usize> {
    let pat = format!("{key}=\"");
    let i = tag.find(&pat)? + pat.len();
    let rest = &tag[i..];
    let e = rest.find('"')?;
    rest[..e].parse().ok()
}

/// 线程批注（xl/threadedComments/*、xl/persons/*）我们没建模，umya 会把
/// person 列表清空、属性重排。保存后用基准文件里的字节原样覆盖，保证线程批注
/// 与作者信息不退化；部件本身的存在/关系与 Content-Type 已由前面的补丁负责。
pub fn restore_threaded_parts(path: &Path, base: &Path) -> Result<usize> {
    if std::env::var("SC_NO_FIXUPS").is_ok() {
        return Ok(0);
    }
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    let names = parts(base);
    let has_threaded = names.iter().any(|n| n.starts_with("xl/threadedComments/"));
    for name in names {
        let mirror_comments = has_threaded
            && name.starts_with("xl/comments")
            && name.ends_with(".xml");
        if !(name.starts_with("xl/threadedComments/")
            || name.starts_with("xl/persons/")
            || mirror_comments)
        {
            continue;
        }
        let Some(body) = read_part_file(base, &name) else { continue };
        if read_part(path, &name).as_deref() == Some(body.as_slice()) {
            continue;
        }
        pending.push((name, body));
    }
    let restored = pending.len();
    if !pending.is_empty() {
        write_parts(path, &pending)?;
    }
    Ok(restored)
}

/// umya 重写 `xl/charts/chartN.xml` 会丢它没建模的属性（次轴 numFmt 等）。
/// 用户没改过、也没因插删行列改过引用的图表，保存后按基准逐字节还原。
pub fn restore_chart_parts(path: &Path, base: &Path, edited: &[String]) -> Result<usize> {
    if base == path || !base.exists() {
        return Ok(0);
    }
    let keep: Vec<String> = edited.to_vec();
    let base_names = parts(base);
    let saved_names = parts(path);
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    for name in base_names.iter().filter(|n| n.starts_with("xl/charts/") && n.ends_with(".xml")) {
        if keep.iter().any(|k| k == name) || !saved_names.contains(name) {
            continue;
        }
        let want = read_part_file(base, name)
            .ok_or_else(|| anyhow::anyhow!("缺部件 {name}"))?;
        let have = read_part(path, name).unwrap_or_default();
        if want != have {
            pending.push((name.to_string(), want));
        }
    }
    let restored = pending.len();
    if !pending.is_empty() {
        write_parts(path, &pending)?;
    }
    Ok(restored)
}

/// 让 [Content_Types].xml 里 workbook 的主类型和输出扩展名一致。
///
/// Excel 认扩展名：`.xlsx` 里声明 `macroEnabled.main+xml`（或反过来）会报
/// 「文件格式或文件扩展名无效」，哪怕包本身结构完整。
pub fn normalise_workbook_ct(path: &std::path::Path, macro_enabled: bool) -> anyhow::Result<()> {
    let Some(bytes) = read_part(path, "[Content_Types].xml") else {
        return Ok(());
    };
    let Ok(xml) = String::from_utf8(bytes) else {
        return Ok(());
    };
    let want = if macro_enabled {
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml"
    } else {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
    };
    let mut out = xml.clone();
    for old in [
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
    ] {
        out = out.replace(old, want);
    }
    if out == xml {
        return Ok(());
    }
    crate::patch::append_entry(path, "[Content_Types].xml", out.as_bytes())?;
    Ok(())
}
