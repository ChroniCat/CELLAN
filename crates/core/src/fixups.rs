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

fn read_entry(path: &Path, name: &str) -> Option<Vec<u8>> {
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
        let Some(old_xml) = read_entry(base, &name).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        let Some(new_xml) = read_entry(path, &name).and_then(|b| String::from_utf8(b).ok()) else {
            // 新文件里整个 rels 都没了：直接把基准的搬过来
            if let Some(raw) = read_entry(base, &name) {
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

/// 删掉没有任何关系指向的部件（umya 每次保存都会留下旧的表格/批注部件）
pub fn prune_orphans(path: &Path) -> Result<Vec<String>> {
    let all = entries(path);
    // 收集所有 rels 里出现过的 Target（相对路径统一成 xl/ 开头）
    let mut referenced: HashSet<String> = HashSet::new();
    for n in all.iter() {
        if !n.ends_with(".rels") {
            continue;
        }
        let Some(xml) = read_entry(path, n).and_then(|b| String::from_utf8(b).ok()) else {
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
        if !prunable(n) {
            continue;
        }
        if referenced.contains(n) {
            continue;
        }
        if crate::patch::remove_entry(path, n).is_ok() {
            removed.push(n.clone());
        }
    }
    // Content_Types 里的 Override 也要清掉，否则校验会报“声明了不存在的部件”
    if !removed.is_empty() {
        if let Some(xml) = read_entry(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()) {
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
