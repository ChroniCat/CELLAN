//! 单元格批注：同时支持 `.xlsx`（传统 `comments*.xml` + 新版 Excel `threadedComments`）与 `.xls`（BIFF8 `OBJ`+`TXO`+`NOTE`）。

use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub r: u32,
    pub c: u32,
    pub text: String,
    pub author: String,
}

/// 表名 → 批注列表（自动识别 .xlsx 与 .xls）
pub fn load_notes(path: &Path) -> Result<HashMap<String, Vec<Note>>> {
    let head = {
        let mut f = std::fs::File::open(path)?;
        let mut b = [0u8; 4];
        let n = f.read(&mut b).unwrap_or(0);
        (n, b)
    };
    if head.0 >= 4 && head.1 == [0xD0, 0xCF, 0x11, 0xE0] {
        return Ok(load_xls_notes(path));
    }

    let targets = crate::patch::sheet_targets(path)?;
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;

    // 读取新版 Excel 线程批注作者表（xl/persons/person.xml，若存在）
    let mut persons: HashMap<String, String> = HashMap::new();
    for i in 0..zip.len() {
        let pname = match zip.by_index(i) {
            Ok(zf) => zf.name().to_string(),
            Err(_) => continue,
        };
        if pname.starts_with("xl/persons/") && pname.ends_with(".xml") {
            if let Ok(mut f) = zip.by_name(&pname) {
                let mut px = String::new();
                if f.read_to_string(&mut px).is_ok() {
                    for (id, disp) in parse_persons(&px) {
                        persons.insert(id, disp);
                    }
                }
            }
        }
    }

    let mut out = HashMap::new();
    for (name, sheet_path) in targets.iter() {
        let (dir, base) = match sheet_path.rsplit_once('/') {
            Some(v) => v,
            None => ("xl/worksheets", sheet_path.as_str()),
        };
        let rels = format!("{dir}/_rels/{base}.rels");
        let mut xml = String::new();
        {
            let Ok(mut f) = zip.by_name(&rels) else { continue };
            let _ = f.read_to_string(&mut xml);
        }
        let (legacy_targets, threaded_targets) = find_comment_targets(&xml);
        let mut by_cell: HashMap<(u32, u32), Note> = HashMap::new();
        let mut order: Vec<(u32, u32)> = Vec::new();

        for target in legacy_targets {
            let entry = resolve_comment_part(dir, &target);
            let mut cx = String::new();
            if let Ok(mut f) = zip.by_name(&entry) {
                if f.read_to_string(&mut cx).is_ok() {
                    for n in parse_comments(&cx) {
                        let key = (n.r, n.c);
                        if !by_cell.contains_key(&key) {
                            order.push(key);
                        }
                        by_cell.insert(key, n);
                    }
                }
            }
        }

        // 新版 Excel 2019/2021/365「新建批注」默认写入 threadedComment，且常在旧 comments 里留兼容提示占位文；
        // 用真实线程批注覆盖同坐标的旧占位提示。
        for target in threaded_targets {
            let entry = resolve_comment_part(dir, &target);
            let mut tx = String::new();
            if let Ok(mut f) = zip.by_name(&entry) {
                if f.read_to_string(&mut tx).is_ok() {
                    for n in parse_threaded_comments(&tx, &persons) {
                        let key = (n.r, n.c);
                        if !by_cell.contains_key(&key) {
                            order.push(key);
                        }
                        by_cell.insert(key, n);
                    }
                }
            }
        }

        if !by_cell.is_empty() {
            let notes: Vec<Note> = order
                .into_iter()
                .filter_map(|k| by_cell.remove(&k))
                .collect();
            if !notes.is_empty() {
                out.insert(name.clone(), notes);
            }
        }
    }
    Ok(out)
}

fn resolve_comment_part(sheet_dir: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
    }
    if target.starts_with("xl/") {
        return target.to_string();
    }
    resolve(sheet_dir, target)
}

fn find_comments_target(rels_xml: &str) -> Option<String> {
    let (legacy, _) = find_comment_targets(rels_xml);
    legacy
        .into_iter()
        .next()
        .map(|t| t.trim_start_matches("../").to_string())
}

fn find_comment_targets(rels_xml: &str) -> (Vec<String>, Vec<String>) {
    let mut legacy = Vec::new();
    let mut threaded = Vec::new();
    let mut r = Reader::from_str(rels_xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if local(e.name().as_ref()) == "Relationship" =>
            {
                let mut ty = String::new();
                let mut target = String::new();
                let mut external = false;
                for a in e.attributes().flatten() {
                    let k = local(a.key.as_ref());
                    let v = crate::model::xml_unescape(&a.value.to_string());
                    if k == "Type" {
                        ty = v;
                    } else if k == "Target" {
                        target = v;
                    } else if k == "TargetMode" && v.eq_ignore_ascii_case("External") {
                        external = true;
                    }
                }
                if external || target.is_empty() {
                    continue;
                }
                let ty_trimmed = ty.trim_end_matches('/');
                if ty_trimmed.ends_with("/comments") {
                    legacy.push(target);
                } else if ty_trimmed.ends_with("/threadedComment") {
                    threaded.push(target);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    (legacy, threaded)
}

/// 取本地名（去掉前缀）
fn local(name: &str) -> &str {
    match name.rsplit_once(':') {
        Some((_, l)) => l,
        None => name,
    }
}

fn decode_general_ref(raw: &str) -> Option<char> {
    match raw {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            if let Some(hex) = raw.strip_prefix("#x").or_else(|| raw.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = raw.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}

fn normalize_note_text(s: &str) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end_matches('\0')
        .to_string()
}

pub fn parse_comments(xml: &str) -> Vec<Note> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut authors: Vec<String> = Vec::new();
    let mut notes: Vec<Note> = Vec::new();
    let mut in_authors = false;
    let mut in_author = false;
    let mut author_buf = String::new();
    let mut cur: Option<Note> = None;
    let mut author_id = 0usize;
    let mut in_text = false;
    let mut in_t = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref());
                if name == "authors" {
                    in_authors = true;
                } else if in_authors && name == "author" {
                    in_author = true;
                    author_buf.clear();
                } else if name == "comment" {
                    let mut coord = String::new();
                    let mut aid = 0usize;
                    for a in e.attributes().flatten() {
                        let k = local(a.key.as_ref());
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        if k == "ref" {
                            coord = v;
                        } else if k == "authorId" {
                            aid = v.parse().unwrap_or(0);
                        }
                    }
                    if let Some((row, col)) = parse_a1(&coord) {
                        author_id = aid;
                        cur = Some(Note {
                            r: row,
                            c: col,
                            text: String::new(),
                            author: String::new(),
                        });
                    }
                } else if cur.is_some() && name == "text" {
                    in_text = true;
                } else if (in_text || cur.is_some()) && name == "t" {
                    in_t = true;
                }
            }
            Ok(Event::Text(t)) => {
                let raw: &str = &t;
                let txt = crate::model::xml_unescape(raw);
                if in_author {
                    author_buf.push_str(&txt);
                } else if in_t {
                    if let Some(n) = cur.as_mut() {
                        n.text.push_str(&txt);
                    }
                }
            }
            Ok(Event::CData(c)) => {
                let txt: &str = c.as_ref();
                if in_author {
                    author_buf.push_str(txt);
                } else if in_t || in_text {
                    if let Some(n) = cur.as_mut() {
                        n.text.push_str(txt);
                    }
                }
            }
            Ok(Event::GeneralRef(g)) => {
                let gname = String::from_utf8_lossy(g.as_ref().as_ref()).to_string();
                if let Some(ch) = decode_general_ref(&gname) {
                    if in_author {
                        author_buf.push(ch);
                    } else if in_t {
                        if let Some(n) = cur.as_mut() {
                            n.text.push(ch);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref());
                if name == "author" && in_author {
                    authors.push(author_buf.trim().to_string());
                    in_author = false;
                } else if name == "authors" {
                    in_authors = false;
                } else if name == "t" {
                    in_t = false;
                } else if name == "text" {
                    in_text = false;
                } else if name == "comment" {
                    if let Some(mut n) = cur.take() {
                        n.author = authors.get(author_id).cloned().unwrap_or_default();
                        n.text = normalize_note_text(&n.text);
                        if !n.text.is_empty() {
                            notes.push(n);
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    notes
}

fn parse_persons(xml: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if local(e.name().as_ref()) == "person" => {
                let mut id = String::new();
                let mut disp = String::new();
                for a in e.attributes().flatten() {
                    let k = local(a.key.as_ref());
                    let v = crate::model::xml_unescape(&a.value.to_string());
                    if k == "id" {
                        id = v;
                    } else if k == "displayName" {
                        disp = v;
                    }
                }
                if !id.is_empty() {
                    out.insert(id, disp);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

pub fn parse_threaded_comments(xml: &str, persons: &HashMap<String, String>) -> Vec<Note> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut grouped: HashMap<(u32, u32), Note> = HashMap::new();
    let mut order: Vec<(u32, u32)> = Vec::new();
    let mut cur: Option<(u32, u32, String, String)> = None; // (r, c, author, text)
    let mut in_text = false;

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let name = local(e.name().as_ref()).to_string();
                if name == "threadedComment" {
                    let mut coord = String::new();
                    let mut pid = String::new();
                    for a in e.attributes().flatten() {
                        let k = local(a.key.as_ref());
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        if k == "ref" {
                            coord = v;
                        } else if k == "personId" {
                            pid = v;
                        }
                    }
                    if let Some((row, col)) = parse_a1(&coord) {
                        let author = persons.get(&pid).cloned().unwrap_or_default();
                        cur = Some((row, col, author, String::new()));
                    }
                } else if cur.is_some() && (name == "text" || name == "t") {
                    in_text = true;
                }
            }
            Ok(Event::Text(t)) if in_text => {
                if let Some((_, _, _, buf)) = cur.as_mut() {
                    buf.push_str(&crate::model::xml_unescape(&t));
                }
            }
            Ok(Event::CData(c)) if in_text => {
                if let Some((_, _, _, buf)) = cur.as_mut() {
                    buf.push_str(c.as_ref());
                }
            }
            Ok(Event::GeneralRef(g)) if in_text => {
                let gname = String::from_utf8_lossy(g.as_ref().as_ref()).to_string();
                if let Some(ch) = decode_general_ref(&gname) {
                    if let Some((_, _, _, buf)) = cur.as_mut() {
                        buf.push(ch);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = local(e.name().as_ref()).to_string();
                if name == "text" || name == "t" {
                    in_text = false;
                } else if name == "threadedComment" {
                    if let Some((row, col, author, raw_txt)) = cur.take() {
                        let txt = normalize_note_text(&raw_txt);
                        if !txt.is_empty() {
                            let key = (row, col);
                            if let Some(existing) = grouped.get_mut(&key) {
                                let reply_line = if author.is_empty() {
                                    txt
                                } else {
                                    format!("{author}：{txt}")
                                };
                                existing.text.push('\n');
                                existing.text.push_str(&reply_line);
                            } else {
                                order.push(key);
                                grouped.insert(
                                    key,
                                    Note {
                                        r: row,
                                        c: col,
                                        text: txt,
                                        author,
                                    },
                                );
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    order
        .into_iter()
        .filter_map(|k| grouped.remove(&k))
        .collect()
}

// ---------------- BIFF8 (.xls) 批注解析 ----------------

fn u16le(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn u32le(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(i)?,
        *b.get(i + 1)?,
        *b.get(i + 2)?,
        *b.get(i + 3)?,
    ]))
}

fn read_biff_unicode(b: &[u8], at: usize) -> Option<String> {
    let cch = u16le(b, at)? as usize;
    let flags = *b.get(at + 2)?;
    let wide = flags & 1 != 0;
    let byte_len = if wide { cch * 2 } else { cch };
    let end = (at + 3 + byte_len).min(b.len());
    if at + 3 > end {
        return None;
    }
    let raw = &b[at + 3..end];
    let s = if wide {
        let units: Vec<u16> = raw
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        raw.iter().map(|&ch| ch as char).collect()
    };
    Some(s.trim_matches('\0').trim().to_string())
}

pub fn parse_xls_notes_stream(stream: &[u8]) -> HashMap<String, Vec<Note>> {
    const BOUNDSHEET: u16 = 0x0085;
    const OBJ: u16 = 0x005D;
    const TXO: u16 = 0x01B6;
    const CONTINUE_REC: u16 = 0x003C;
    const NOTE_REC: u16 = 0x001C;
    const EOF_REC: u16 = 0x000A;

    let mut bounds: Vec<(u32, String)> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        let typ = u16le(stream, i).unwrap_or(0);
        let len = u16le(stream, i + 2).unwrap_or(0) as usize;
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        if typ == BOUNDSHEET && body.len() >= 8 {
            let off = u32le(body, 0).unwrap_or(0);
            let cch = body[6] as usize;
            let flags = body[7];
            let wide = flags & 1 != 0;
            let raw = &body[8..(8 + if wide { cch * 2 } else { cch }).min(body.len())];
            let name = if wide {
                let u: Vec<u16> = raw
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                    .collect();
                String::from_utf16_lossy(&u)
            } else {
                String::from_utf8_lossy(raw).to_string()
            };
            bounds.push((off, name));
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }
    bounds.sort_by_key(|(off, _)| *off);

    let mut out: HashMap<String, Vec<Note>> = HashMap::new();
    if bounds.is_empty() {
        return out;
    }

    let mut cur_sheet: Option<usize> = None;
    let mut next_bound = 0usize;
    let mut obj_texts: HashMap<u16, String> = HashMap::new();
    let mut raw_notes: Vec<(u32, u32, u16, String, String)> = Vec::new(); // (r, c, id_obj, author, inline_text)
    let mut last_comment_obj: Option<u16> = None;
    let mut pending_txo: Option<(u16, usize, String)> = None;

    let flush_sheet = |si: usize,
                       bounds: &[(u32, String)],
                       obj_texts: &mut HashMap<u16, String>,
                       raw_notes: &mut Vec<(u32, u32, u16, String, String)>,
                       out: &mut HashMap<String, Vec<Note>>| {
        if raw_notes.is_empty() {
            obj_texts.clear();
            return;
        }
        let mut sheet_notes = Vec::new();
        for (r, c, id_obj, author, inline_txt) in raw_notes.drain(..) {
            let txt = obj_texts
                .get(&id_obj)
                .cloned()
                .filter(|s| !s.is_empty())
                .unwrap_or(inline_txt);
            let txt = normalize_note_text(&txt);
            if !txt.is_empty() {
                sheet_notes.push(Note {
                    r,
                    c,
                    text: txt,
                    author,
                });
            }
        }
        obj_texts.clear();
        if !sheet_notes.is_empty() {
            if let Some((_, name)) = bounds.get(si) {
                out.insert(name.clone(), sheet_notes);
            }
        }
    };

    let mut i = 0usize;
    while i + 4 <= stream.len() {
        while next_bound < bounds.len() && (i as u32) >= bounds[next_bound].0 {
            if let Some((oid, _, acc)) = pending_txo.take() {
                obj_texts.insert(oid, acc);
            }
            if let Some(prev_si) = cur_sheet.take() {
                flush_sheet(prev_si, &bounds, &mut obj_texts, &mut raw_notes, &mut out);
            }
            cur_sheet = Some(next_bound);
            last_comment_obj = None;
            next_bound += 1;
        }
        let typ = u16le(stream, i).unwrap_or(0);
        let len = u16le(stream, i + 2).unwrap_or(0) as usize;
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];

        if cur_sheet.is_some() {
            if typ != CONTINUE_REC {
                if let Some((oid, _, acc)) = pending_txo.take() {
                    obj_texts.insert(oid, acc);
                }
            }
            match typ {
                OBJ if body.len() >= 8 => {
                    let ft = u16le(body, 0).unwrap_or(0);
                    let ot = u16le(body, 4).unwrap_or(0);
                    let id = u16le(body, 6).unwrap_or(0);
                    // ftCmo(0x0015) 且 ot == 0x0019 (Comment)
                    if ft == 0x0015 && ot == 0x0019 {
                        last_comment_obj = Some(id);
                    } else {
                        last_comment_obj = None;
                    }
                }
                TXO if body.len() >= 14 => {
                    if let Some(oid) = last_comment_obj.take() {
                        let cch = u16le(body, 10).unwrap_or(0) as usize;
                        if cch > 0 {
                            pending_txo = Some((oid, cch, String::new()));
                        } else {
                            obj_texts.insert(oid, String::new());
                        }
                    }
                }
                CONTINUE_REC => {
                    if let Some((oid, rem, mut acc)) = pending_txo.take() {
                        if !body.is_empty() && rem > 0 {
                            let wide = body[0] & 1 != 0;
                            if wide {
                                let take_chars = ((body.len() - 1) / 2).min(rem);
                                let raw = &body[1..1 + take_chars * 2];
                                let units: Vec<u16> = raw
                                    .chunks(2)
                                    .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                                    .collect();
                                acc.push_str(&String::from_utf16_lossy(&units));
                                if take_chars < rem {
                                    pending_txo = Some((oid, rem - take_chars, acc));
                                } else {
                                    obj_texts.insert(oid, acc);
                                }
                            } else {
                                let take_chars = (body.len() - 1).min(rem);
                                for &ch in &body[1..1 + take_chars] {
                                    acc.push(ch as char);
                                }
                                if take_chars < rem {
                                    pending_txo = Some((oid, rem - take_chars, acc));
                                } else {
                                    obj_texts.insert(oid, acc);
                                }
                            }
                        } else {
                            obj_texts.insert(oid, acc);
                        }
                    }
                }
                NOTE_REC if body.len() >= 6 => {
                    let rw = u16le(body, 0).unwrap_or(0) as u32;
                    let col = u16le(body, 2).unwrap_or(0) as u32;
                    if body.len() >= 8 {
                        let id_obj = u16le(body, 6).unwrap_or(0);
                        let author = read_biff_unicode(body, 8).unwrap_or_default();
                        // 兼容早期 BIFF5 内联 NOTE：rw(2)+col(2)+cch(2)+text
                        let inline = if id_obj == 0 {
                            let cch = u16le(body, 4).unwrap_or(0) as usize;
                            if cch > 0 && 6 + cch <= body.len() {
                                String::from_utf8_lossy(&body[6..6 + cch]).to_string()
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };
                        raw_notes.push((rw, col, id_obj, author, inline));
                    }
                }
                EOF_REC => {
                    if let Some(si) = cur_sheet.take() {
                        flush_sheet(si, &bounds, &mut obj_texts, &mut raw_notes, &mut out);
                    }
                }
                _ => {}
            }
        }

        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }

    if let Some((oid, _, acc)) = pending_txo.take() {
        obj_texts.insert(oid, acc);
    }
    if let Some(si) = cur_sheet.take() {
        flush_sheet(si, &bounds, &mut obj_texts, &mut raw_notes, &mut out);
    }
    out
}

fn load_xls_notes(path: &Path) -> HashMap<String, Vec<Note>> {
    let Ok(bytes) = std::fs::read(path) else { return HashMap::new() };
    let Ok(cf) = crate::cfb::Compound::open(bytes) else { return HashMap::new() };
    let Some(stream) = cf
        .stream_by_path("Workbook")
        .or_else(|| cf.stream_by_path("Book"))
    else {
        return HashMap::new();
    };
    parse_xls_notes_stream(&stream)
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut row = 0u32;
    let mut digits = false;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() && !digits {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if ch.is_ascii_digit() {
            digits = true;
            row = row * 10 + (ch as u32 - '0' as u32);
        } else if ch == '$' {
            continue;
        } else {
            return None;
        }
    }
    if col == 0 || row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_xml_and_cdata() {
        let xml = r#"<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
          <authors><author>张三</author><author>李四</author></authors>
          <commentList>
            <comment ref="B2" authorId="0"><text><r><t>这行要复核</t></r></text></comment>
            <comment ref="A3" authorId="1"><text><r><t>第一段</t></r><r><t>第二段</t></r></text></comment>
            <comment ref="C4" authorId="0"><text><t><![CDATA[WPS CDATA 批注]]></t></text></comment>
          </commentList>
        </comments>"#;
        let notes = parse_comments(xml);
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0].r, 1);
        assert_eq!(notes[0].c, 1);
        assert_eq!(notes[0].author, "张三");
        assert_eq!(notes[0].text, "这行要复核");
        assert_eq!(notes[1].text, "第一段第二段", "多段富文本要拼起来");
        assert_eq!(notes[1].author, "李四");
        assert_eq!(notes[2].text, "WPS CDATA 批注");
    }

    #[test]
    fn parses_threaded_comments_with_persons() {
        let persons_xml = r#"<personList><person id="{P-1}" displayName="王五"/><person id="{P-2}" displayName="赵六"/></personList>"#;
        let tc_xml = r#"<ThreadedComments>
            <threadedComment ref="D5" personId="{P-1}" id="{C-1}"><text>请确认含税单价</text></threadedComment>
            <threadedComment ref="D5" personId="{P-2}" parentId="{C-1}" id="{C-2}"><text>已与供应商核实无误</text></threadedComment>
        </ThreadedComments>"#;
        let persons = parse_persons(persons_xml);
        let notes = parse_threaded_comments(tc_xml, &persons);
        assert_eq!(notes.len(), 1);
        assert_eq!((notes[0].r, notes[0].c), (4, 3));
        assert_eq!(notes[0].author, "王五");
        assert_eq!(notes[0].text, "请确认含税单价\n赵六：已与供应商核实无误");
    }

    #[test]
    fn parses_biff8_xls_notes_stream() {
        fn rec(typ: u16, body: &[u8]) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&typ.to_le_bytes());
            v.extend_from_slice(&(body.len() as u16).to_le_bytes());
            v.extend_from_slice(body);
            v
        }
        let mut s = Vec::new();
        s.extend(rec(0x0809, &[0u8; 4]));
        let mut bs = Vec::new();
        bs.extend_from_slice(&0u32.to_le_bytes());
        bs.extend_from_slice(&0u16.to_le_bytes());
        bs.push(6);
        bs.push(0);
        bs.extend_from_slice(b"Sheet1");
        let bs_rec = rec(0x0085, &bs);
        let off = (s.len() + bs_rec.len() + 4) as u32;
        let mut bs_fixed = bs_rec;
        bs_fixed[4..8].copy_from_slice(&off.to_le_bytes());
        s.extend(bs_fixed);
        s.extend(rec(0x000A, &[]));

        // Worksheet substream
        s.extend(rec(0x0809, &[0u8; 4]));
        // OBJ: ftCmo(0x0015), cb=18, ot=0x0019(Comment), id=7
        let mut obj = vec![0u8; 22];
        obj[0..2].copy_from_slice(&0x0015u16.to_le_bytes());
        obj[2..4].copy_from_slice(&18u16.to_le_bytes());
        obj[4..6].copy_from_slice(&0x0019u16.to_le_bytes());
        obj[6..8].copy_from_slice(&7u16.to_le_bytes());
        s.extend(rec(0x005D, &obj));

        // TXO + CONTINUE (UTF-16LE "核对发票")
        let text_utf16: Vec<u16> = "核对发票".encode_utf16().collect();
        let mut txo = vec![0u8; 18];
        txo[10..12].copy_from_slice(&(text_utf16.len() as u16).to_le_bytes());
        s.extend(rec(0x01B6, &txo));
        let mut cont = vec![1u8]; // UTF-16LE flag
        for u in &text_utf16 {
            cont.extend_from_slice(&u.to_le_bytes());
        }
        s.extend(rec(0x003C, &cont));

        // NOTE: row=1, col=2, grbit=0, idObj=7, author="财务部"
        let author_utf16: Vec<u16> = "财务部".encode_utf16().collect();
        let mut note = Vec::new();
        note.extend_from_slice(&1u16.to_le_bytes());
        note.extend_from_slice(&2u16.to_le_bytes());
        note.extend_from_slice(&0u16.to_le_bytes());
        note.extend_from_slice(&7u16.to_le_bytes());
        note.extend_from_slice(&(author_utf16.len() as u16).to_le_bytes());
        note.push(1);
        for u in &author_utf16 {
            note.extend_from_slice(&u.to_le_bytes());
        }
        s.extend(rec(0x001C, &note));
        s.extend(rec(0x000A, &[]));

        let parsed = parse_xls_notes_stream(&s);
        let list = parsed.get("Sheet1").expect("Sheet1 notes");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].r, 1);
        assert_eq!(list[0].c, 2);
        assert_eq!(list[0].author, "财务部");
        assert_eq!(list[0].text, "核对发票");
    }

    #[test]
    fn rela_paths() {
        let rels = r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments/comment1.xml"/></Relationships>"#;
        assert_eq!(find_comments_target(rels), Some("comments/comment1.xml".into()));
        assert_eq!(
            resolve_comment_part("xl/worksheets", "../comments1.xml"),
            "xl/comments1.xml"
        );
        assert_eq!(
            resolve_comment_part("xl/worksheets", "xl/comments1.xml"),
            "xl/comments1.xml"
        );
        assert_eq!(
            resolve_comment_part("xl/worksheets", "/xl/comments1.xml"),
            "xl/comments1.xml"
        );
    }
}

/// 删掉某张表的批注部件（comments + VML）及其工作表关系。
pub fn clear_sheet_notes(path: &std::path::Path, sheet_name: &str) -> anyhow::Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(part) = targets.get(sheet_name) else { return Ok(()) };
    let (dir, file) = match part.rsplit_once('/') {
        Some(v) => v,
        None => return Ok(()),
    };
    let rels_part = format!("{dir}/_rels/{file}.rels");
    let Some(rels) = crate::fixups::read_part(path, &rels_part).and_then(|b| String::from_utf8(b).ok())
    else {
        return Ok(());
    };
    let doc = match roxmltree::Document::parse(&rels) {
        Ok(d) => d,
        Err(e) => anyhow::bail!("关系部件 {rels_part} 不是合法 XML：{e}"),
    };
    let mut removed: Vec<String> = Vec::new();
    let mut keep_ids: Vec<String> = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("Relationship")) {
        let ty = node.attribute("Type").unwrap_or("");
        if ty.ends_with("/comments") || ty.ends_with("/vmlDrawing") {
            if node.attribute("TargetMode") != Some("External") {
                if let Some(t) = node.attribute("Target") {
                    removed.push(resolve(dir, t));
                }
            }
        } else if let Some(id) = node.attribute("Id") {
            keep_ids.push(id.to_string());
        }
    }
    if removed.is_empty() {
        return Ok(());
    }
    for p in &removed {
        let _ = crate::patch::remove_entry(path, p);
    }
    let root = doc.root_element();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
    out.push_str("<Relationships");
    for ns in root.namespaces() {
        match ns.name() {
            Some(prefix) => out.push_str(&format!(" xmlns:{}=\"{}\"", prefix, xml_escape(ns.uri()))),
            None => out.push_str(&format!(" xmlns=\"{}\"", xml_escape(ns.uri()))),
        }
    }
    for a in root.attributes() {
        out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
    }
    out.push('>');
    for node in doc.descendants().filter(|n| n.has_tag_name("Relationship")) {
        let Some(id) = node.attribute("Id") else { continue };
        if !keep_ids.iter().any(|k| k == id) {
            continue;
        }
        out.push_str("<Relationship");
        for a in node.attributes() {
            out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
        }
        out.push_str("/>");
    }
    out.push_str("</Relationships>");
    roxmltree::Document::parse(&out).map_err(|e| anyhow::anyhow!("重建的关系 XML 非法：{e}"))?;
    crate::fixups::write_parts(path, &[(rels_part, out.into_bytes())])?;
    let _ = crate::fixups::prune_content_types(path);
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn resolve(base: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
    }
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

pub fn capture_external_rels(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for name in crate::fixups::entries_of(path) {
        if !name.ends_with(".rels") {
            continue;
        }
        let Some(raw) = crate::fixups::read_part_file(path, &name).and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        let mut kept = Vec::new();
        let mut rest = raw.as_str();
        while let Some(i) = rest.find("<Relationship ") {
            let Some(gt) = rest[i..].find('>').map(|e| i + e + 1) else { break };
            let tag = &rest[i..gt];
            if tag.contains("TargetMode=\"External\"") || tag.contains("TargetMode='External'") {
                kept.push(tag.to_string());
            }
            rest = &rest[gt..];
        }
        if !kept.is_empty() {
            out.push((name, kept));
        }
    }
    out
}

pub fn restore_external_rels(path: &std::path::Path, captured: &[(String, Vec<String>)]) -> anyhow::Result<()> {
    if captured.is_empty() {
        return Ok(());
    }
    for (rels_part, tags) in captured {
        let Some(raw) = crate::fixups::read_part(path, rels_part).and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        let doc = match roxmltree::Document::parse(&raw) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let root = doc.root_element();
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
        out.push_str("<Relationships");
        for ns in root.namespaces() {
            match ns.name() {
                Some(p) => out.push_str(&format!(" xmlns:{}=\"{}\"", p, xml_escape(ns.uri()))),
                None => out.push_str(&format!(" xmlns=\"{}\"", xml_escape(ns.uri()))),
            }
        }
        for a in root.attributes() {
            out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
        }
        out.push('>');
        for node in doc.descendants().filter(|n| n.has_tag_name("Relationship")) {
            let id = node.attribute("Id").unwrap_or("");
            let cur_target = node.attribute("Target").unwrap_or("");
            let cur_external = node.attribute("TargetMode") == Some("External");
            let cap = tags.iter().find(|t| id_of(t) == id);
            let rebuilt = match cap {
                Some(tag) if !cur_external && cur_target.is_empty() => Some(tag.to_string()),
                Some(tag) => {
                    let cap_target = attr_of(tag, "Target").unwrap_or_default();
                    let cap_mode = attr_of(tag, "TargetMode");
                    if cur_target == cap_target {
                        let mut s = String::from("<Relationship");
                        for a in node.attributes() {
                            s.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
                        }
                        if cap_mode.is_some() && !cur_external {
                            s.push_str(" TargetMode=\"External\"");
                        }
                        s.push_str("/>");
                        Some(s)
                    } else if engine_mangled(&cap_target).trim_end_matches('/')
                        == cur_target.trim_end_matches('/')
                    {
                        Some(tag.clone())
                    } else {
                        let mut s = String::from("<Relationship");
                        for a in node.attributes() {
                            s.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
                        }
                        if cap_mode.is_some() && !cur_external {
                            s.push_str(" TargetMode=\"External\"");
                        }
                        s.push_str("/>");
                        Some(s)
                    }
                }
                None => None,
            };
            match rebuilt {
                Some(tag) => out.push_str(&tag),
                None => {
                    out.push_str("<Relationship");
                    for a in node.attributes() {
                        out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
                    }
                    out.push_str("/>");
                }
            }
        }
        out.push_str("</Relationships>");
        if roxmltree::Document::parse(&out).is_ok() {
            crate::fixups::write_parts(path, &[(rels_part.clone(), out.into_bytes())])?;
        }
    }
    Ok(())
}

fn engine_mangled(target: &str) -> String {
    let mut out = String::new();
    let mut prev_slash = false;
    for ch in target.chars() {
        if ch == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        out.push(ch);
    }
    out
}

fn attr_of(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=");
    let i = tag.find(&pat)?;
    let rest = &tag[i + pat.len()..];
    let quote = rest.chars().next()?;
    let body = rest.trim_start_matches(quote);
    Some(body.split(quote).next()?.to_string())
}

fn id_of(tag: &str) -> String {
    let Some(i) = tag.find("Id=") else { return String::new() };
    let rest = &tag[i + 3..];
    let quote = rest.chars().next().unwrap_or('"');
    let body = rest.trim_start_matches(quote);
    body.split(quote).next().unwrap_or("").to_string()
}
