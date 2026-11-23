//! 跨工作簿引用：读/写 `xl/externalLinks`，刷新缓存，公式里 `[N]` 与文件名的互转。
//!
//! 文件格式（Excel 实测）：
//!
//! ```text
//! sheet1.xml      <c r="A1"><f>[1]预算!A1</f><v>10</v></c>       ← 方括号里是链接序号
//! workbook.xml    <externalReferences><externalReference r:id="rId2"/></externalReferences>
//! wb rels         rId2 → externalLinks/externalLink1.xml
//! externalLink1   <sheetNames><sheetName val="预算"/></sheetNames>
//!                 <sheetDataSet><sheetData sheetId="0"><row r="1">…</row></sheetData></sheetDataSet>
//! extLink rels    rId1 → ext_plan_src.xlsx（相对）  rId2 → file:///…（绝对）
//! ```
//!
//! 我们内部一律保留 `[N]` 原样，只有给人看的时候才换成文件名；写回绝不改序号。

use crate::model::{Value, Workbook};
use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// 引用计数写时复制的外部工作簿单元格缓存，使 `Workbook::clone()`（后台重算、自动保存）为 O(1)。
#[derive(Clone, Debug, Default)]
pub struct ExternalCache(pub std::sync::Arc<BTreeMap<(usize, u32, u32), Value>>);

impl ExternalCache {
    pub fn new() -> Self {
        Self(std::sync::Arc::new(BTreeMap::new()))
    }

    pub fn from_map(map: BTreeMap<(usize, u32, u32), Value>) -> Self {
        Self(std::sync::Arc::new(map))
    }

    pub fn insert(&mut self, k: (usize, u32, u32), v: Value) -> Option<Value> {
        std::sync::Arc::make_mut(&mut self.0).insert(k, v)
    }

    pub fn clear(&mut self) {
        if !self.0.is_empty() {
            self.0 = std::sync::Arc::new(BTreeMap::new());
        }
    }
}

impl std::ops::Deref for ExternalCache {
    type Target = BTreeMap<(usize, u32, u32), Value>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<BTreeMap<(usize, u32, u32), Value>> for ExternalCache {
    fn from(map: BTreeMap<(usize, u32, u32), Value>) -> Self {
        Self::from_map(map)
    }
}

impl PartialEq for ExternalCache {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl PartialEq<BTreeMap<(usize, u32, u32), Value>> for ExternalCache {
    fn eq(&self, other: &BTreeMap<(usize, u32, u32), Value>) -> bool {
        &*self.0 == other
    }
}

/// 一个外部工作簿（对应 `xl/externalLinks/externalLinkN.xml`）
#[derive(Clone, Debug, Default)]
pub struct ExternalBook {
    /// 1 起，与公式里的 `[N]` 对应
    pub index: usize,
    /// 包内部件名，例如 `xl/externalLinks/externalLink1.xml`
    pub part: String,
    /// 相对路径（externalLink rels 里 rId1 的 Target）
    pub target: String,
    /// 绝对路径（xxl21:absoluteUrl 指向的那个 file:/// URL）
    pub abs: Option<String>,
    /// 被引用表的名字，按 sheetId 顺序
    pub sheets: Vec<String>,
    /// 外部工作簿定义名称：(name, sheetId_0based, refersTo)
    pub defined_names: Vec<(String, Option<usize>, String)>,
    /// (sheetId, row, col) → 缓存值（Arc CoW，O(1) 克隆）
    pub cache: ExternalCache,
    /// 本轮是否从磁盘重读过（决定存盘要不要重写 sheetDataSet）
    pub refreshed: bool,
}

impl ExternalBook {
    pub fn file_name(&self) -> String {
        let t = self.target.replace('\\', "/");
        t.rsplit('/').next().unwrap_or(&t).to_string()
    }
}

/// 链接的可读名字：优先文件名，其次绝对路径，最后原样
pub fn label(link: &ExternalBook) -> String {
    let f = link.file_name();
    if f.is_empty() {
        link.abs.clone().unwrap_or_else(|| link.target.clone())
    } else {
        f
    }
}

// ---------------------------------------------------------------- 读

pub fn read(path: &Path) -> Vec<ExternalBook> {
    let Some(bytes) = crate::fixups::read_part(path, "xl/workbook.xml") else {
        return Vec::new();
    };
    let xml = String::from_utf8_lossy(&bytes).to_string();
    let ids = parse_external_ids(&xml);
    if ids.is_empty() {
        return Vec::new();
    }
    let rels = crate::fixups::read_part(path, "xl/_rels/workbook.xml.rels")
        .map(|b| parse_rels(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();

    let mut out = Vec::new();
    for (i, rid) in ids.iter().enumerate() {
        let Some((target, _)) = rels.get(rid) else { continue };
        let part = join_part("xl", target);
        let Some(pb) = crate::fixups::read_part(path, &part) else { continue };
        let pxml = String::from_utf8_lossy(&pb).to_string();
        let parsed = parse_link_part(&pxml);
        let prels = crate::fixups::read_part(path, &part_rels(&part))
            .map(|b| parse_rels(&String::from_utf8_lossy(&b)))
            .unwrap_or_default();
        let target = parsed
            .book_rid
            .as_ref()
            .and_then(|r| prels.get(r))
            .map(|(t, _)| t.clone())
            .unwrap_or_default();
        let abs = parsed
            .abs_rid
            .as_ref()
            .and_then(|r| prels.get(r))
            .map(|(t, _)| t.clone());
        out.push(ExternalBook {
            index: i + 1,
            part,
            target,
            abs,
            sheets: parsed.sheets,
            defined_names: parsed.defined_names,
            cache: ExternalCache::from_map(parsed.cache),
            refreshed: false,
        });
    }
    out
}

struct ParsedPart {
    sheets: Vec<String>,
    defined_names: Vec<(String, Option<usize>, String)>,
    book_rid: Option<String>,
    abs_rid: Option<String>,
    cache: BTreeMap<(usize, u32, u32), Value>,
}

fn parse_link_part(xml: &str) -> ParsedPart {
    let mut out = ParsedPart {
        sheets: Vec::new(),
        defined_names: Vec::new(),
        book_rid: None,
        abs_rid: None,
        cache: BTreeMap::new(),
    };
    let mut r = Reader::from_str(xml);
    let mut sheet_id: usize = 0;
    let mut cur: Option<(u32, u32)> = None;
    let mut cur_t = String::from("n");
    let mut text = String::new();
    let mut in_v = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = e.name().as_ref().to_string();
                match n.as_str() {
                    "externalBook" => {
                        if let Some(v) = attr(&e, "id") {
                            out.book_rid = Some(v);
                        }
                    }
                    "absoluteUrl" => {
                        if let Some(v) = attr(&e, "id") {
                            out.abs_rid = Some(v);
                        }
                    }
                    "sheetName" => {
                        if let Some(v) = attr(&e, "val") {
                            out.sheets.push(v);
                        }
                    }
                    "definedName" => {
                        if let Some(nm) = attr(&e, "name").filter(|s| !s.is_empty()) {
                            let refers = attr(&e, "refersTo").unwrap_or_default();
                            let sid = attr(&e, "sheetId").and_then(|s| s.parse::<usize>().ok());
                            out.defined_names.push((nm, sid, refers));
                        }
                    }
                    "sheetData" => {
                        sheet_id = attr(&e, "sheetId")
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(0);
                    }
                    "cell" => {
                        cur = attr(&e, "r").and_then(|s| crate::table::parse_a1(&format!("{s}:{s}")).map(|a| (a.0, a.1)));
                        cur_t = attr(&e, "t").unwrap_or_else(|| "n".into());
                    }
                    "v" => {
                        in_v = true;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let n = e.name().as_ref().to_string();
                if n == "sheetName" {
                    if let Some(v) = attr(&e, "val") {
                        out.sheets.push(v);
                    }
                } else if n == "definedName" {
                    if let Some(nm) = attr(&e, "name").filter(|s| !s.is_empty()) {
                        let refers = attr(&e, "refersTo").unwrap_or_default();
                        let sid = attr(&e, "sheetId").and_then(|s| s.parse::<usize>().ok());
                        out.defined_names.push((nm, sid, refers));
                    }
                } else if n == "externalBook" {
                    if let Some(v) = attr(&e, "id") {
                        out.book_rid = Some(v);
                    }
                } else if n == "absoluteUrl" {
                    if let Some(v) = attr(&e, "id") {
                        out.abs_rid = Some(v);
                    }
                } else if n == "sheetData" {
                    sheet_id = attr(&e, "sheetId")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(0);
                }
            }
            Ok(Event::Text(t)) => {
                if in_v {
                    let raw = t.as_ref().to_string();
                    text.push_str(&crate::model::xml_unescape(&raw));
                }
            }
            Ok(Event::End(e)) => {
                let n = e.name().as_ref().to_string();
                if n == "v" {
                    if let Some((row, col)) = cur {
                        let v = match cur_t.as_str() {
                            "str" => Value::Text(text.as_str().into()),
                            "b" => Value::Bool(text.trim() == "1"),
                            "e" => match crate::formula::ErrKind::from_text(&text) {
                                Some(k) => Value::Error(k),
                                None => Value::Text(text.as_str().into()),
                            },
                            _ => text
                                .trim()
                                .parse::<f64>()
                                .map(Value::Number)
                                .unwrap_or(Value::Empty),
                        };
                        out.cache.insert((sheet_id, row, col), v);
                    }
                    in_v = false;
                } else if n == "cell" {
                    cur = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

/// workbook.xml 里 `<externalReferences>` 的 r:id 列表（按出现顺序）
fn parse_external_ids(xml: &str) -> Vec<String> {
    let mut r = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut inside = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = e.name().as_ref().to_string();
                if n == "externalReferences" {
                    inside = true;
                } else if inside && n == "externalReference" {
                    if let Some(v) = attr(&e, "id") {
                        out.push(v);
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let n = e.name().as_ref().to_string();
                if inside && n == "externalReference" {
                    if let Some(v) = attr(&e, "id") {
                        out.push(v);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == "externalReferences" {
                    inside = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// rels 的 id → (Target, Type)
fn parse_rels(xml: &str) -> HashMap<String, (String, String)> {
    let mut r = Reader::from_str(xml);
    let mut out = HashMap::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == "Relationship" {
                    let id = attr(&e, "Id").unwrap_or_default();
                    let target = attr(&e, "Target").unwrap_or_default();
                    let ty = attr(&e, "Type").unwrap_or_default();
                    if !id.is_empty() {
                        out.insert(id, (target, ty));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().ends_with(key))
        .map(|a| crate::model::xml_unescape(&a.value.to_string()))
}

fn join_part(base: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
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

fn part_rels(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/_rels/{name}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

// ---------------------------------------------------------------- 引用区域过滤（防止整本外部表落盘膨胀与内存暴涨）

/// 单个外部链接在当前工作簿中被公式/名称实际引用的区域集合。
#[derive(Clone, Debug, Default)]
pub struct BookFilter {
    /// 表名（小写；`""` 表示未显式指定表名，默认第 0 张表）→ 闭区间矩形列表 `(r1, c1, r2, c2)`
    pub by_sheet: HashMap<String, Vec<(u32, u32, u32, u32)>>,
    /// 三维引用 `(起始表小写, 结束表小写, (r1, c1, r2, c2))`
    pub three_d: Vec<(String, String, (u32, u32, u32, u32))>,
    /// 被引用的外部定义名称 `(限定表名小写, 名称小写)`
    pub names: Vec<(Option<String>, String)>,
    /// 当前工作簿是否有任何公式引用此链接
    pub any_ref: bool,
    /// 存在无法静态解析的引用形态时的兜底标记
    pub fallback_all: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ResolvedFilter {
    pub by_slot: HashMap<usize, Vec<(u32, u32, u32, u32)>>,
    pub fallback_all: bool,
}

fn clean_sheet_key(name: Option<&str>) -> String {
    match name {
        None => String::new(),
        Some(s) => s.trim().trim_matches('\'').to_lowercase(),
    }
}

fn push_dedup_rect(list: &mut Vec<(u32, u32, u32, u32)>, rect: (u32, u32, u32, u32)) {
    for cur in list.iter_mut() {
        if rect.0 >= cur.0 && rect.1 >= cur.1 && rect.2 <= cur.2 && rect.3 <= cur.3 {
            return;
        }
        if cur.0 >= rect.0 && cur.1 >= rect.1 && cur.2 <= rect.2 && cur.3 <= rect.3 {
            *cur = rect;
            return;
        }
        // 同列相邻或重叠区间直接合并，防止数千行逐行 VLOOKUP 产生数千个矩形
        if cur.1 == rect.1 && cur.3 == rect.3 && rect.0 <= cur.2.saturating_add(1) && cur.0 <= rect.2.saturating_add(1) {
            cur.0 = cur.0.min(rect.0);
            cur.2 = cur.2.max(rect.2);
            return;
        }
    }
    if list.len() >= 256 {
        // 散点过多时合并到最后一个包络矩形，保证包含判定 O(1)
        if let Some(last) = list.last_mut() {
            last.0 = last.0.min(rect.0);
            last.1 = last.1.min(rect.1);
            last.2 = last.2.max(rect.2);
            last.3 = last.3.max(rect.3);
            return;
        }
    }
    list.push(rect);
}

impl BookFilter {
    fn add_sheet_rect(&mut self, sheet: Option<&str>, rect: (u32, u32, u32, u32)) {
        self.any_ref = true;
        let key = clean_sheet_key(sheet);
        push_dedup_rect(self.by_sheet.entry(key).or_default(), rect);
    }

    fn add_three_d(&mut self, s1: &str, s2: &str, rect: (u32, u32, u32, u32)) {
        self.any_ref = true;
        let k1 = clean_sheet_key(Some(s1));
        let k2 = clean_sheet_key(Some(s2));
        if !self.three_d.iter().any(|(a, b, r)| a == &k1 && b == &k2 && *r == rect) {
            self.three_d.push((k1, k2, rect));
        }
    }

    fn add_name(&mut self, sheet: Option<&str>, name: &str) {
        self.any_ref = true;
        let sk = sheet.map(|s| clean_sheet_key(Some(s)));
        let nk = name.trim().to_lowercase();
        if !self.names.iter().any(|(s, n)| s == &sk && n == &nk) {
            self.names.push((sk, nk));
        }
    }

    pub fn resolve(
        &self,
        sheets: &[String],
        defined_names: &[(String, Option<usize>, String)],
    ) -> ResolvedFilter {
        if self.fallback_all {
            return ResolvedFilter {
                by_slot: HashMap::new(),
                fallback_all: true,
            };
        }
        let mut expanded = self.clone();
        // 展开被引用的外部定义名称（最多递归 6 层）
        let mut idx = 0usize;
        let mut guard = 0usize;
        while idx < expanded.names.len() && guard < 64 {
            let (scope_sh, nm_lower) = expanded.names[idx].clone();
            idx += 1;
            guard += 1;
            let want_slot = scope_sh.as_deref().and_then(|sh| {
                if sheets.is_empty() {
                    Some(0)
                } else {
                    sheets.iter().position(|s| s.eq_ignore_ascii_case(sh))
                }
            });
            let found = if let Some(slot) = want_slot {
                defined_names
                    .iter()
                    .find(|(nm, s, _)| nm.eq_ignore_ascii_case(&nm_lower) && *s == Some(slot))
                    .or_else(|| {
                        defined_names
                            .iter()
                            .find(|(nm, s, _)| nm.eq_ignore_ascii_case(&nm_lower) && s.is_none())
                    })
            } else {
                defined_names
                    .iter()
                    .find(|(nm, s, _)| nm.eq_ignore_ascii_case(&nm_lower) && s.is_none())
            };
            if let Some((_, def_slot, refers)) = found {
                let body = refers.trim().trim_start_matches('=');
                if !body.is_empty() {
                    let norm = crate::normalize_formula(body);
                    if let Ok(mut node) = crate::formula::parse(&norm) {
                        let default_sh = scope_sh
                            .as_deref()
                            .or_else(|| def_slot.and_then(|s| sheets.get(s).map(String::as_str)))
                            .or_else(|| sheets.first().map(String::as_str));
                        crate::formula::qualify_ext_node(&mut node, "1", default_sh);
                        collect_single_link_node(&node, &mut expanded);
                    }
                }
            }
        }

        let mut by_slot: HashMap<usize, Vec<(u32, u32, u32, u32)>> = HashMap::new();
        for (sh_lower, rects) in &expanded.by_sheet {
            let slot_opt = if sh_lower.is_empty() || sheets.is_empty() {
                Some(0)
            } else {
                sheets.iter().position(|s| s.eq_ignore_ascii_case(sh_lower))
            };
            if let Some(slot) = slot_opt {
                let dst = by_slot.entry(slot).or_default();
                for &r in rects {
                    push_dedup_rect(dst, r);
                }
            }
        }
        for (s1, s2, rect) in &expanded.three_d {
            let a = sheets.iter().position(|s| s.eq_ignore_ascii_case(s1));
            let b = sheets.iter().position(|s| s.eq_ignore_ascii_case(s2));
            if let (Some(ia), Some(ib)) = (a, b) {
                let (lo, hi) = if ia <= ib { (ia, ib) } else { (ib, ia) };
                for slot in lo..=hi {
                    push_dedup_rect(by_slot.entry(slot).or_default(), *rect);
                }
            }
        }
        ResolvedFilter {
            by_slot,
            fallback_all: false,
        }
    }
}

impl ResolvedFilter {
    #[inline]
    pub fn includes_sheet(&self, sid: usize) -> bool {
        self.fallback_all || self.by_slot.contains_key(&sid)
    }

    #[inline]
    pub fn includes_cell(&self, sid: usize, r: u32, c: u32) -> bool {
        if self.fallback_all {
            return true;
        }
        let Some(rects) = self.by_slot.get(&sid) else {
            return false;
        };
        rects
            .iter()
            .any(|&(r1, c1, r2, c2)| r >= r1 && r <= r2 && c >= c1 && c <= c2)
    }
}

fn collect_single_link_node(node: &crate::formula::Node, bf: &mut BookFilter) {
    use crate::formula::Node;
    match node {
        Node::Ref(re) => {
            if re.book.is_some() && re.row >= 0 && re.col >= 0 {
                let (r, c) = (re.row as u32, re.col as u32);
                if let (Some(s1), Some(s2)) = (&re.sheet, &re.sheet2) {
                    bf.add_three_d(s1, s2, (r, c, r, c));
                } else {
                    bf.add_sheet_rect(re.sheet.as_deref(), (r, c, r, c));
                }
            }
        }
        Node::Spill(re) => {
            if re.book.is_some() && re.row >= 0 && re.col >= 0 {
                bf.add_sheet_rect(re.sheet.as_deref(), (re.row as u32, re.col as u32, 1_048_575, 16_383));
            }
        }
        Node::Range(a, b) => {
            if a.book.is_some() || b.book.is_some() {
                let rect = (
                    a.row.min(b.row).max(0) as u32,
                    a.col.min(b.col).max(0) as u32,
                    a.row.max(b.row).max(0) as u32,
                    a.col.max(b.col).max(0) as u32,
                );
                let rep = if a.sheet2.is_some() { a } else { b };
                if let (Some(s1), Some(s2)) = (&rep.sheet, &rep.sheet2) {
                    bf.add_three_d(s1, s2, rect);
                } else {
                    let sh = a.sheet.as_deref().or(b.sheet.as_deref());
                    bf.add_sheet_rect(sh, rect);
                }
            }
        }
        Node::ScopedName { book: Some(_), sheet, name } => {
            bf.add_name(sheet.as_deref(), name);
        }
        Node::Func(name, args) => {
            let amounts = if name == "SUMIF" {
                crate::formula::sumif_amount_range(args)
            } else {
                None
            };
            if name == "OFFSET" {
                if let Some(Node::Ref(re) | Node::Range(re, _)) = args.first() {
                    if re.book.is_some() {
                        bf.add_sheet_rect(re.sheet.as_deref(), (0, 0, 1_048_575, 16_383));
                    }
                }
            }
            for (i, a) in args.iter().enumerate() {
                let r = if i == 2 { amounts.as_ref().unwrap_or(a) } else { a };
                collect_single_link_node(r, bf);
            }
        }
        Node::Implicit(x) | Node::Unary(_, x) | Node::Percent(x) => collect_single_link_node(x, bf),
        Node::Binary(_, l, r) => {
            collect_single_link_node(l, bf);
            collect_single_link_node(r, bf);
        }
        _ => {}
    }
}

fn walk_wb_ext_node(wb: &Workbook, node: &crate::formula::Node, filters: &mut [BookFilter]) {
    use crate::formula::Node;
    match node {
        Node::Ref(re) => {
            if let Some(bk) = re.book.as_deref() {
                if let Some(li) = resolve_book(wb, bk) {
                    if re.row >= 0 && re.col >= 0 {
                        let (r, c) = (re.row as u32, re.col as u32);
                        if let (Some(s1), Some(s2)) = (&re.sheet, &re.sheet2) {
                            filters[li].add_three_d(s1, s2, (r, c, r, c));
                        } else {
                            filters[li].add_sheet_rect(re.sheet.as_deref(), (r, c, r, c));
                        }
                    }
                }
            }
        }
        Node::Spill(re) => {
            if let Some(bk) = re.book.as_deref() {
                if let Some(li) = resolve_book(wb, bk) {
                    let r = re.row.max(0) as u32;
                    let c = re.col.max(0) as u32;
                    filters[li].add_sheet_rect(re.sheet.as_deref(), (r, c, 1_048_575, 16_383));
                }
            }
        }
        Node::Range(a, b) => {
            if let Some(bk) = a.book.as_deref().or(b.book.as_deref()) {
                if let Some(li) = resolve_book(wb, bk) {
                    let rect = (
                        a.row.min(b.row).max(0) as u32,
                        a.col.min(b.col).max(0) as u32,
                        a.row.max(b.row).max(0) as u32,
                        a.col.max(b.col).max(0) as u32,
                    );
                    let rep = if a.sheet2.is_some() { a } else { b };
                    if let (Some(s1), Some(s2)) = (&rep.sheet, &rep.sheet2) {
                        filters[li].add_three_d(s1, s2, rect);
                    } else {
                        let sh = a.sheet.as_deref().or(b.sheet.as_deref());
                        filters[li].add_sheet_rect(sh, rect);
                    }
                }
            }
        }
        Node::ScopedName { book: Some(bk), sheet, name } => {
            if let Some(li) = resolve_book(wb, bk) {
                filters[li].add_name(sheet.as_deref(), name);
            }
        }
        Node::Func(name, args) => {
            let amounts = if name == "SUMIF" {
                crate::formula::sumif_amount_range(args)
            } else {
                None
            };
            if name == "OFFSET" {
                if let Some(Node::Ref(re) | Node::Range(re, _)) = args.first() {
                    if let Some(bk) = re.book.as_deref() {
                        if let Some(li) = resolve_book(wb, bk) {
                            filters[li].add_sheet_rect(re.sheet.as_deref(), (0, 0, 1_048_575, 16_383));
                        }
                    }
                }
            }
            for (i, a) in args.iter().enumerate() {
                let r = if i == 2 { amounts.as_ref().unwrap_or(a) } else { a };
                walk_wb_ext_node(wb, r, filters);
            }
        }
        Node::Implicit(x) | Node::Unary(_, x) | Node::Percent(x) => walk_wb_ext_node(wb, x, filters),
        Node::Binary(_, l, r) => {
            walk_wb_ext_node(wb, l, filters);
            walk_wb_ext_node(wb, r, filters);
        }
        _ => {}
    }
}

fn scan_formula_for_links(
    wb: &Workbook,
    row: u32,
    col: u32,
    f: &str,
    link_names_lower: &[String],
    filters: &mut [BookFilter],
) {
    if !f.contains('[') && !f.contains('!') {
        return;
    }
    // 普通表内公式（如 `=Sheet2!A1`）不含 `[`，若链接文件名也未出现在公式中则快速跳过
    if !f.contains('[') {
        if link_names_lower.is_empty() {
            return;
        }
        let f_lower = f.to_lowercase();
        if !link_names_lower.iter().any(|fnm| f_lower.contains(fnm)) {
            return;
        }
    }
    let raw = f.strip_prefix('=').unwrap_or(f);
    if crate::formula::with_compiled_node(raw, row, col, |node| {
        walk_wb_ext_node(wb, node, filters);
    })
    .is_err()
    {
        for tok in books_in_formula(f) {
            if let Some(li) = resolve_book(wb, &tok) {
                filters[li].any_ref = true;
                filters[li].fallback_all = true;
            }
        }
    }
}

/// 扫描工作簿内全部公式与名称，提取每个外部链接实际被引用的工作表与单元格范围。
pub fn build_book_filters(wb: &Workbook) -> Vec<BookFilter> {
    let mut filters = vec![BookFilter::default(); wb.links.len()];
    if filters.is_empty() {
        return filters;
    }
    let link_names_lower: Vec<String> = wb
        .links
        .iter()
        .map(|l| l.file_name().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    for sheet in &wb.sheets {
        for (cref, cell) in sheet.formula_cells() {
            if let Some(f) = cell.formula.as_deref() {
                scan_formula_for_links(wb, cref.row, cref.col, f, &link_names_lower, &mut filters);
            }
        }
        for dv in &sheet.dv {
            if dv.f1.starts_with('=') {
                scan_formula_for_links(wb, 0, 0, &dv.f1, &link_names_lower, &mut filters);
            }
            if dv.f2.starts_with('=') {
                scan_formula_for_links(wb, 0, 0, &dv.f2, &link_names_lower, &mut filters);
            }
        }
        for block in &sheet.cf.blocks {
            for rule in &block.rules {
                match &rule.kind {
                    crate::cf::CfKind::CellIs { a, b, .. } => {
                        if a.starts_with('=') {
                            scan_formula_for_links(wb, 0, 0, a, &link_names_lower, &mut filters);
                        }
                        if b.starts_with('=') {
                            scan_formula_for_links(wb, 0, 0, b, &link_names_lower, &mut filters);
                        }
                    }
                    crate::cf::CfKind::Expr { formula, .. } => {
                        scan_formula_for_links(wb, 0, 0, formula, &link_names_lower, &mut filters);
                    }
                    crate::cf::CfKind::Scale { points } => {
                        for (c, _) in points {
                            if !c.formula.is_empty() {
                                scan_formula_for_links(wb, 0, 0, &c.formula, &link_names_lower, &mut filters);
                            }
                        }
                    }
                    crate::cf::CfKind::Bar { min, max, .. } => {
                        if !min.formula.is_empty() {
                            scan_formula_for_links(wb, 0, 0, &min.formula, &link_names_lower, &mut filters);
                        }
                        if !max.formula.is_empty() {
                            scan_formula_for_links(wb, 0, 0, &max.formula, &link_names_lower, &mut filters);
                        }
                    }
                    crate::cf::CfKind::IconSet { thresholds, .. } => {
                        for c in thresholds {
                            if !c.formula.is_empty() {
                                scan_formula_for_links(wb, 0, 0, &c.formula, &link_names_lower, &mut filters);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    for (_, _, refers) in &wb.names {
        scan_formula_for_links(wb, 0, 0, refers, &link_names_lower, &mut filters);
    }
    filters
}

/// 判断新读取的受限缓存与现有 `link` 在被引用区域、三维表区间及被引用名称上是否确实发生变化。
pub fn link_differs_on_filter(
    link: &ExternalBook,
    bf: &BookFilter,
    new_sheets: &[String],
    new_vals: &BTreeMap<(usize, u32, u32), Value>,
    new_dnames: &[(String, Option<usize>, String)],
) -> bool {
    if !bf.any_ref {
        return false;
    }
    let effective_dnames = if new_dnames.is_empty() {
        link.defined_names.as_slice()
    } else {
        new_dnames
    };
    // 检查被引用的外部名称定义是否变化
    if !bf.names.is_empty() && !new_dnames.is_empty() {
        for (_, nm_lower) in &bf.names {
            let old_def = link
                .defined_names
                .iter()
                .find(|(n, _, _)| n.eq_ignore_ascii_case(nm_lower));
            let new_def = new_dnames
                .iter()
                .find(|(n, _, _)| n.eq_ignore_ascii_case(nm_lower));
            if old_def != new_def {
                return true;
            }
        }
    }
    // 三维引用需要感知首尾之间工作表增删或重排（如 SHEETS([1]First:Third!A1)）
    for (s1, s2, _) in &bf.three_d {
        let span_of = |list: &[String]| -> Option<Vec<String>> {
            let a = list.iter().position(|s| s.eq_ignore_ascii_case(s1))?;
            let b = list.iter().position(|s| s.eq_ignore_ascii_case(s2))?;
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            Some(list[lo..=hi].to_vec())
        };
        if span_of(&link.sheets) != span_of(new_sheets) {
            return true;
        }
    }
    let old_resolved = bf.resolve(&link.sheets, &link.defined_names);
    let new_resolved = bf.resolve(new_sheets, effective_dnames);
    // 若被引用的表在外部工作簿中的槽位序号变了（如前面插入了新表），需要同步重写
    if old_resolved.by_slot != new_resolved.by_slot {
        return true;
    }
    for (&k, v) in new_vals.iter() {
        if new_resolved.includes_cell(k.0, k.1, k.2) && link.cache.get(&k) != Some(v) {
            return true;
        }
    }
    for (&k, _) in link.cache.iter() {
        if old_resolved.includes_cell(k.0, k.1, k.2) && !new_vals.contains_key(&k) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------- 写缓存

/// 把刷新过的链接缓存写回 externalLink 部件（仅写入本工作簿实际引用的单元格，对标 Excel/WPS 杜绝体积暴涨）
pub fn write_cache_for_wb(path: &Path, wb: &Workbook) -> Result<usize> {
    let filters = build_book_filters(wb);
    write_cache_inner(path, &wb.links, Some(&filters))
}

/// 把刷新过的链接缓存写回 externalLink 部件（没刷新的一个字节都不动）
pub fn write_cache(path: &Path, links: &[ExternalBook]) -> Result<usize> {
    write_cache_inner(path, links, None)
}

fn write_cache_inner(
    path: &Path,
    links: &[ExternalBook],
    filters: Option<&[BookFilter]>,
) -> Result<usize> {
    let mut n = 0;
    for (i, l) in links.iter().enumerate() {
        if !l.refreshed || l.part.is_empty() {
            continue;
        }
        let Some(bytes) = crate::fixups::read_part(path, &l.part) else {
            continue;
        };
        let xml = String::from_utf8_lossy(&bytes).to_string();
        let resolved = filters
            .and_then(|fs| fs.get(i))
            .map(|bf| bf.resolve(&l.sheets, &l.defined_names));
        let new = replace_sheet_data_filtered(&xml, l, resolved.as_ref());
        if new != xml {
            crate::patch::append_entry(path, &l.part, new.as_bytes())
                .with_context(|| format!("写回 {} 失败", l.part))?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn replace_sheet_data(xml: &str, link: &ExternalBook) -> String {
    replace_sheet_data_filtered(xml, link, None)
}

fn replace_sheet_data_filtered(
    xml: &str,
    link: &ExternalBook,
    filter: Option<&ResolvedFilter>,
) -> String {
    let mut out_xml = xml.to_string();
    if !link.sheets.is_empty() && out_xml.contains("<sheetNames") {
        let names: String = link
            .sheets
            .iter()
            .map(|s| format!("<sheetName val=\"{}\"/>", xml_escape(s)))
            .collect();
        out_xml = replace_or_insert_block(
            &out_xml,
            "sheetNames",
            &format!("<sheetNames>{names}</sheetNames>"),
            &["<definedNames", "<sheetDataSet", "</externalBook>"],
        );
    }
    let dnames = defined_names_xml(link);
    if !dnames.is_empty() || out_xml.contains("<definedNames") {
        out_xml = replace_or_insert_block(
            &out_xml,
            "definedNames",
            &dnames,
            &["<sheetDataSet", "</externalBook>"],
        );
    }
    let body = sheet_data_xml_filtered(link, filter);
    replace_or_insert_block(&out_xml, "sheetDataSet", &body, &["</externalBook>"])
}

fn replace_or_insert_block(xml: &str, tag: &str, replacement: &str, anchors: &[&str]) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    if let Some(start) = xml.find(&open) {
        let after = &xml[start..];
        let end = if let Some(i) = after.find(&close) {
            Some(start + i + close.len())
        } else {
            after.find("/>").map(|i| start + i + 2)
        };
        if let Some(end) = end {
            let mut out = String::with_capacity(xml.len() + replacement.len());
            out.push_str(&xml[..start]);
            out.push_str(replacement);
            out.push_str(&xml[end..]);
            return out;
        }
    }
    if replacement.is_empty() {
        return xml.to_string();
    }
    for anc in anchors {
        if let Some(i) = xml.find(anc) {
            let mut out = String::with_capacity(xml.len() + replacement.len());
            out.push_str(&xml[..i]);
            out.push_str(replacement);
            out.push_str(&xml[i..]);
            return out;
        }
    }
    xml.to_string()
}

fn defined_names_xml(link: &ExternalBook) -> String {
    if link.defined_names.is_empty() {
        return String::new();
    }
    let mut out = String::from("<definedNames>");
    for (nm, sid, refers) in &link.defined_names {
        out.push_str("<definedName name=\"");
        out.push_str(&xml_escape(nm));
        out.push('"');
        if !refers.is_empty() {
            out.push_str(" refersTo=\"");
            out.push_str(&xml_escape(refers));
            out.push('"');
        }
        if let Some(id) = sid {
            out.push_str(&format!(" sheetId=\"{id}\""));
        }
        out.push_str("/>");
    }
    out.push_str("</definedNames>");
    out
}

fn sheet_data_xml_filtered(link: &ExternalBook, filter: Option<&ResolvedFilter>) -> String {
    // link.cache 本身已是 BTreeMap<(usize, u32, u32), Value>，天然按 (sheetId, row, col) 严格升序排列：
    // 落盘时仅输出当前工作簿实际引用的单元格，对标 Excel/WPS 行为，彻底杜绝将数万行未引用外部数据内嵌进包内。
    let est = match filter {
        Some(f) if !f.fallback_all => link.cache.len().min(512),
        _ => link.cache.len().min(4096),
    };
    let mut out = String::with_capacity(32 + est * 36);
    out.push_str("<sheetDataSet>");
    let mut cur_sid: Option<usize> = None;
    let mut cur_row: Option<u32> = None;
    for (&(sid, r, c), v) in link.cache.iter() {
        if matches!(v, Value::Empty) {
            continue;
        }
        if let Some(rf) = filter {
            if !rf.includes_cell(sid, r, c) {
                continue;
            }
        }
        if cur_sid != Some(sid) {
            if cur_row.take().is_some() {
                out.push_str("</row>");
            }
            if cur_sid.is_some() {
                out.push_str("</sheetData>");
            }
            use std::fmt::Write as _;
            let _ = write!(out, "<sheetData sheetId=\"{sid}\">");
            cur_sid = Some(sid);
        }
        if cur_row != Some(r) {
            if cur_row.is_some() {
                out.push_str("</row>");
            }
            use std::fmt::Write as _;
            let _ = write!(out, "<row r=\"{}\">", r + 1);
            cur_row = Some(r);
        }
        push_cell_xml(&mut out, r, c, v);
    }
    if cur_row.is_some() {
        out.push_str("</row>");
    }
    if cur_sid.is_some() {
        out.push_str("</sheetData>");
    }
    out.push_str("</sheetDataSet>");
    out
}

fn push_col_letters(out: &mut String, c: u32) {
    let mut buf = [0u8; 8];
    let mut idx = 8usize;
    let mut n = c + 1;
    while n > 0 {
        idx -= 1;
        buf[idx] = b'A' + ((n - 1) % 26) as u8;
        n = (n - 1) / 26;
    }
    if let Ok(s) = std::str::from_utf8(&buf[idx..]) {
        out.push_str(s);
    }
}

fn push_xml_escaped(out: &mut String, s: &str) {
    if !s.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"')) {
        out.push_str(s);
        return;
    }
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

fn push_cell_xml(out: &mut String, r: u32, c: u32, v: &Value) {
    use std::fmt::Write as _;
    match v {
        Value::Number(n) => {
            out.push_str("<cell r=\"");
            push_col_letters(out, c);
            let _ = write!(out, "{}\"><v>{}</v></cell>", r + 1, crate::model::format_number(*n));
        }
        Value::Text(t) => {
            out.push_str("<cell r=\"");
            push_col_letters(out, c);
            let _ = write!(out, "{}\" t=\"str\"><v>", r + 1);
            push_xml_escaped(out, t);
            out.push_str("</v></cell>");
        }
        Value::Bool(b) => {
            out.push_str("<cell r=\"");
            push_col_letters(out, c);
            let _ = write!(out, "{}\" t=\"b\"><v>{}</v></cell>", r + 1, if *b { 1 } else { 0 });
        }
        Value::Error(e) => {
            out.push_str("<cell r=\"");
            push_col_letters(out, c);
            let _ = write!(out, "{}\" t=\"e\"><v>{}</v></cell>", r + 1, e.text());
        }
        Value::Empty => {}
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------- 补部件

/// 按工作簿实际引用的范围补齐新建的 externalLink 部件。
pub fn ensure_parts_for_wb(path: &Path, wb: &Workbook) -> Result<usize> {
    let filters = build_book_filters(wb);
    ensure_parts_inner(path, &wb.links, Some(&filters))
}

/// 让包里的 externalLink 部件与 `wb.links` 对齐（用户手输新工作簿、或界面上加了链接）。
///
/// 只补缺的：已有部件、已有关系、已有 Override 都不动；`<externalReferences>`
/// 只在条目数/顺序对不上时才整段重写。
pub fn ensure_parts(path: &Path, links: &[ExternalBook]) -> Result<usize> {
    ensure_parts_inner(path, links, None)
}

fn ensure_parts_inner(
    path: &Path,
    links: &[ExternalBook],
    filters: Option<&[BookFilter]>,
) -> Result<usize> {
    if links.is_empty() {
        return Ok(0);
    }
    let Some(wb_bytes) = crate::fixups::read_part(path, "xl/workbook.xml") else {
        return Ok(0);
    };
    let wbxml = String::from_utf8_lossy(&wb_bytes).to_string();
    let mut ids = parse_external_ids(&wbxml);
    let rels_bytes = crate::fixups::read_part(path, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let mut rels = String::from_utf8_lossy(&rels_bytes).to_string();
    let mut rel_map = parse_rels(&rels);
    let ct_bytes = crate::fixups::read_part(path, "[Content_Types].xml").unwrap_or_default();
    let mut ct = String::from_utf8_lossy(&ct_bytes).to_string();
    let mut changed_parts = 0usize;

    for (i, link) in links.iter().enumerate() {
        let part = if link.part.is_empty() {
            format!("xl/externalLinks/externalLink{}.xml", i + 1)
        } else {
            link.part.clone()
        };
        // 部件不存在就写一份（含当前被引用缓存）
        if crate::fixups::read_part(path, &part).is_none() {
            let resolved = filters
                .and_then(|fs| fs.get(i))
                .map(|bf| bf.resolve(&link.sheets, &link.defined_names));
            crate::patch::append_entry(path, &part, link_xml(link, resolved.as_ref()).as_bytes())
                .with_context(|| format!("写 {part} 失败"))?;
            let rels_name = part_rels(&part);
            crate::patch::append_entry(path, &rels_name, link_rels_xml(link).as_bytes())
                .with_context(|| format!("写 {rels_name} 失败"))?;
            changed_parts += 1;
        }
        // workbook 级关系
        let rel_target = part.strip_prefix("xl/").unwrap_or(&part).to_string();
        let existing = rel_map
            .iter()
            .find(|(_, (t, ty))| t.eq_ignore_ascii_case(&rel_target) && ty.ends_with("externalLink"))
            .map(|(id, _)| id.clone());
        let had_existing = existing.is_some();
        let rid = match existing {
            Some(id) => id,
            None => {
                let id = format!("rId{}", next_rid(&rels));
                let add = format!(
                    "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink\" Target=\"{rel_target}\"/>"
                );
                insert_before_close(&mut rels, "</Relationships>", &add);
                rel_map.insert(id.clone(), (rel_target.clone(), "externalLink".into()));
                id
            }
        };
        if i < ids.len() {
            if ids[i] != rid && had_existing {
                ids[i] = rid;
            }
        } else {
            ids.push(rid);
        }
        // Content_Types
        let pn = format!("/{part}");
        if !ct.contains(&format!("PartName=\"{pn}\"")) {
            let add = format!(
                "<Override PartName=\"{pn}\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml\"/>"
            );
            insert_before_close(&mut ct, "</Types>", &add);
        }
    }

    // workbook.xml 的 <externalReferences>：数量或顺序不对才重写
    let want: Vec<String> = ids.iter().take(links.len()).cloned().collect();
    let have = parse_external_ids(&wbxml);
    if have != want {
        let block = format!(
            "<externalReferences>{}</externalReferences>",
            want.iter()
                .map(|id| format!("<externalReference r:id=\"{id}\"/>"))
                .collect::<String>()
        );
        let new_wb = replace_or_insert_external_refs(&wbxml, &block);
        crate::patch::append_entry(path, "xl/workbook.xml", new_wb.as_bytes())?;
    }
    if !rels.is_empty() {
        let orig = String::from_utf8_lossy(&rels_bytes).to_string();
        if rels != orig {
            crate::patch::append_entry(path, "xl/_rels/workbook.xml.rels", rels.as_bytes())?;
        }
    }
    if !ct.is_empty() {
        let orig = String::from_utf8_lossy(&ct_bytes).to_string();
        if ct != orig {
            crate::patch::append_entry(path, "[Content_Types].xml", ct.as_bytes())?;
        }
    }
    Ok(changed_parts)
}

fn link_xml(link: &ExternalBook, filter: Option<&ResolvedFilter>) -> String {
    let names: String = link
        .sheets
        .iter()
        .map(|s| format!("<sheetName val=\"{}\"/>", xml_escape(s)))
        .collect();
    let dnames = defined_names_xml(link);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><externalLink xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><externalBook r:id=\"rId1\"><sheetNames>{names}</sheetNames>{dnames}{}</externalBook></externalLink>",
        sheet_data_xml_filtered(link, filter)
    )
}

fn link_rels_xml(link: &ExternalBook) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath\" Target=\"{}\" TargetMode=\"External\"/></Relationships>",
        xml_escape(&link.target)
    )
}

fn insert_before_close(xml: &mut String, close: &str, add: &str) {
    if let Some(i) = xml.find(close) {
        xml.insert_str(i, add);
    }
}

fn replace_or_insert_external_refs(xml: &str, block: &str) -> String {
    if let Some(start) = xml.find("<externalReferences") {
        let after = &xml[start..];
        if let Some(i) = after.find("</externalReferences>") {
            let end = start + i + "</externalReferences>".len();
            return format!("{}{}{}", &xml[..start], block, &xml[end..]);
        }
        if let Some(i) = after.find("/>") {
            let end = start + i + 2;
            return format!("{}{}{}", &xml[..start], block, &xml[end..]);
        }
    }
    if let Some(i) = xml.find("<calcPr") {
        return format!("{}{}{}", &xml[..i], block, &xml[i..]);
    }
    if let Some(i) = xml.find("</workbook>") {
        return format!("{}{}{}", &xml[..i], block, &xml[i..]);
    }
    format!("{xml}{block}")
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

// ---------------------------------------------------------------- 刷新

pub struct RefreshReport {
    pub ok: usize,
    pub failed: Vec<(String, String)>,
}

/// 按链接里的 target 去磁盘上找文件，重读被引用表的区域填进缓存
pub fn refresh(wb: &mut Workbook) -> RefreshReport {
    let base = wb
        .path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let filters = build_book_filters(wb);
    let mut rep = RefreshReport { ok: 0, failed: Vec::new() };
    for (i, link) in wb.links.iter_mut().enumerate() {
        let Some(path) = resolve_path(link, &base) else {
            rep.failed.push((label(link), "找不到文件".into()));
            continue;
        };
        let bf_opt = filters.get(i).filter(|bf| bf.any_ref);
        match load_sheet_values_filtered(&path, &mut link.sheets, bf_opt) {
            Ok((vals, dnames)) => {
                link.cache = ExternalCache::from_map(vals);
                if !dnames.is_empty() {
                    link.defined_names = dnames;
                }
                link.refreshed = true;
                rep.ok += 1;
            }
            Err(e) => rep.failed.push((label(link), format!("{e:#}"))),
        }
    }
    rep
}

/// 在磁盘上查找外部链接对应的真实文件路径（相对路径 → 绝对路径 → 同目录同名文件）。
pub fn resolve_link_path(wb: &Workbook, link: &ExternalBook) -> Option<PathBuf> {
    let base = wb
        .path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    resolve_path(link, &base)
}

/// 带候选目录/最近文件提示的路径解析（用于桌面端全自动发现与刷新）：
/// 优先级：原相对路径 → 原绝对路径 → 同目录同名 → 当前目录下一级子目录（上限 40 个） → 最近打开文件及所在目录。
pub fn resolve_link_path_with_hints(
    wb: &Workbook,
    link: &ExternalBook,
    hints: &[PathBuf],
) -> Option<PathBuf> {
    let base = wb
        .path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    resolve_path_with_hints(link, &base, hints)
}

pub fn resolve_path_with_hints(
    link: &ExternalBook,
    base: &Path,
    hints: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(p) = resolve_path(link, base) {
        return Some(p);
    }
    let fname = link.file_name();
    if fname.is_empty() {
        return None;
    }
    let fname_lower = fname.to_lowercase();

    // 1) 当前工作簿目录的一级子文件夹（如 `./分表/xxx.xlsx`），排除盘符根目录以防扫全盘
    if !base.as_os_str().is_empty() && base.parent().is_some() {
        if let Ok(rd) = std::fs::read_dir(base) {
            for entry in rd.flatten().take(40) {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let cand = entry.path().join(&fname);
                    if cand.exists() {
                        return Some(cand);
                    }
                }
            }
        }
    }

    // 2) 最近打开或同窗口已打开标签页中的精确同名文件
    for hp in hints {
        if hp
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase() == fname_lower)
            .unwrap_or(false)
            && hp.exists()
        {
            return Some(hp.clone());
        }
    }

    // 3) 最近打开文件所在的工作目录中存在同名源文件
    let mut seen_dirs: Vec<PathBuf> = Vec::new();
    for hp in hints {
        let Some(dir) = hp.parent() else { continue };
        if dir.parent().is_none() || seen_dirs.iter().any(|d| d == dir) {
            continue;
        }
        seen_dirs.push(dir.to_path_buf());
        let cand = dir.join(&fname);
        if cand.exists() {
            return Some(cand);
        }
    }

    None
}

/// 读取指定磁盘工作簿生成外部链接快照（供后台文件监控线程无锁调用）
pub fn load_link_snapshot(
    path: &Path,
    prev_sheets: &[String],
) -> Result<(
    Vec<String>,
    BTreeMap<(usize, u32, u32), Value>,
    Vec<(String, Option<usize>, String)>,
)> {
    load_link_snapshot_filtered(path, prev_sheets, None)
}

/// 按被引用范围读取外部链接快照：跳过未引用的工作表与单元格，大幅降低内存占用与 I/O。
pub fn load_link_snapshot_filtered(
    path: &Path,
    prev_sheets: &[String],
    filter: Option<&BookFilter>,
) -> Result<(
    Vec<String>,
    BTreeMap<(usize, u32, u32), Value>,
    Vec<(String, Option<usize>, String)>,
)> {
    let mut sheets = prev_sheets.to_vec();
    let (vals, dnames) = load_sheet_values_filtered(path, &mut sheets, filter)?;
    Ok((sheets, vals, dnames))
}

/// 带候选提示刷新外部链接，并返回 `(报告, 实际数据发生变化的链接名列表)`。
/// 仅对比当前工作簿实际引用的单元格/表名/名称：若被引用内容与现有内嵌缓存完全一致，
/// 则不置 `refreshed = true`，避免误标脏数据或将整本外部工作簿塞进包内。
pub fn refresh_with_hints(wb: &mut Workbook, hints: &[PathBuf]) -> (RefreshReport, Vec<String>) {
    let base = wb
        .path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let filters = build_book_filters(wb);
    let mut rep = RefreshReport {
        ok: 0,
        failed: Vec::new(),
    };
    let mut changed = Vec::new();
    for (i, link) in wb.links.iter_mut().enumerate() {
        let Some(bf) = filters.get(i) else { continue };
        // 当前工作簿没有任何公式引用此外部链接时，跳过扫描，绝不读取大文件占内存
        if !bf.any_ref {
            continue;
        }
        let Some(path) = resolve_path_with_hints(link, &base, hints) else {
            rep.failed.push((label(link), "找不到文件".into()));
            continue;
        };
        let mut new_sheets = link.sheets.clone();
        match load_sheet_values_filtered(&path, &mut new_sheets, Some(bf)) {
            Ok((vals, dnames)) => {
                let diff = link_differs_on_filter(link, bf, &new_sheets, &vals, &dnames);
                link.sheets = new_sheets;
                if diff {
                    let lbl = label(link);
                    link.cache = ExternalCache::from_map(vals);
                    if !dnames.is_empty() {
                        link.defined_names = dnames;
                    }
                    link.refreshed = true;
                    changed.push(lbl);
                } else {
                    // 被引用值完全一致：顺手收敛内存缓存到被引用子集，不标 refreshed
                    if link.cache.len() > vals.len() {
                        link.cache = ExternalCache::from_map(vals);
                    }
                    if link.defined_names.is_empty() && !dnames.is_empty() {
                        link.defined_names = dnames;
                    }
                }
                rep.ok += 1;
            }
            Err(e) => rep.failed.push((label(link), format!("{e:#}"))),
        }
    }
    (rep, changed)
}

/// 直接从同进程已打开的内存 `Workbook` 实时同步外部链接缓存（无需等源标签页落盘保存！）。
pub fn sync_link_from_workbook(link: &mut ExternalBook, src_wb: &Workbook) -> bool {
    sync_link_from_workbook_filtered(link, src_wb, None)
}

/// 按当前工作簿实际引用的范围，从同进程已打开的内存 `Workbook` 同步外部链接缓存。
pub fn sync_link_from_workbook_filtered(
    link: &mut ExternalBook,
    src_wb: &Workbook,
    filter: Option<&BookFilter>,
) -> bool {
    if src_wb.sheets.is_empty() {
        return false;
    }
    if let Some(bf) = filter {
        if !bf.any_ref {
            return false;
        }
    }
    let live_sheets: Vec<String> = src_wb.sheets.iter().map(|s| s.name.clone()).collect();
    let effective_dnames = if src_wb.names.is_empty() {
        link.defined_names.as_slice()
    } else {
        src_wb.names.as_slice()
    };
    let resolved = filter.map(|bf| bf.resolve(&live_sheets, effective_dnames));

    let sheets_same = link.sheets == live_sheets;
    let dnames_diff = !src_wb.names.is_empty() && link.defined_names != src_wb.names;
    if sheets_same && !dnames_diff {
        let mut non_empty = 0usize;
        let mut all_match = true;
        'scan: for (sid, sh) in src_wb.sheets.iter().enumerate() {
            if let Some(ref rf) = resolved {
                if !rf.includes_sheet(sid) {
                    continue;
                }
            }
            for (cref, cell) in sh.iter() {
                if !cell.value.is_empty() {
                    if let Some(ref rf) = resolved {
                        if !rf.includes_cell(sid, cref.row, cref.col) {
                            continue;
                        }
                    }
                    non_empty += 1;
                    if link.cache.get(&(sid, cref.row, cref.col)) != Some(&cell.value) {
                        all_match = false;
                        break 'scan;
                    }
                }
            }
        }
        let expected_existing = match &resolved {
            Some(rf) => link
                .cache
                .keys()
                .filter(|&&(sid, r, c)| rf.includes_cell(sid, r, c))
                .count(),
            None => link.cache.len(),
        };
        if all_match && non_empty == expected_existing {
            return false;
        }
    }
    let mut live_cache: BTreeMap<(usize, u32, u32), Value> = BTreeMap::new();
    for (sid, sh) in src_wb.sheets.iter().enumerate() {
        if let Some(ref rf) = resolved {
            if !rf.includes_sheet(sid) {
                continue;
            }
        }
        for (cref, cell) in sh.iter() {
            if !cell.value.is_empty() {
                if let Some(ref rf) = resolved {
                    if !rf.includes_cell(sid, cref.row, cref.col) {
                        continue;
                    }
                }
                live_cache.insert((sid, cref.row, cref.col), cell.value.clone());
            }
        }
    }
    link.sheets = live_sheets;
    link.cache = ExternalCache::from_map(live_cache);
    if !src_wb.names.is_empty() {
        link.defined_names = src_wb.names.clone();
    }
    link.refreshed = true;
    true
}

fn resolve_path(link: &ExternalBook, base: &Path) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if !link.target.is_empty() {
        cands.push(path_of(&link.target, base));
    }
    if let Some(abs) = &link.abs {
        cands.push(path_of(abs, base));
    }
    let fname = link.file_name();
    if !fname.is_empty() {
        cands.push(base.join(&fname));
    }
    cands.into_iter().find(|p| p.exists())
}

fn path_of(target: &str, base: &Path) -> PathBuf {
    let t = target.trim();
    // file:///F:\dir\a.xlsx 或 file:///F:/dir/a.xlsx
    let stripped = t
        .strip_prefix("file:///")
        .or_else(|| t.strip_prefix("file://"))
        .unwrap_or(t);
    let decoded = percent_decode(stripped);
    let p = PathBuf::from(decoded.replace('/', "\\"));
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn load_sheet_values_filtered(
    path: &Path,
    sheets: &mut Vec<String>,
    filter: Option<&BookFilter>,
) -> Result<(BTreeMap<(usize, u32, u32), Value>, Vec<(String, Option<usize>, String)>)> {
    use calamine::{open_workbook_auto, Reader};
    let mut book = open_workbook_auto(path)?;
    // 总是与源文件最新工作表顺序同步，保证外部三维区间 `First:Third` 能感知插入/重排的中间表。
    let live_names = book.sheet_names().to_owned();
    if !live_names.is_empty() {
        *sheets = live_names.clone();
    }
    let mut dnames = crate::patch::read_defined_names(path);
    if dnames.is_empty() {
        for (nm, formula) in book.defined_names() {
            if !nm.starts_with("_xlnm.") {
                dnames.push((nm.clone(), None, formula.clone()));
            }
        }
    }
    if let Some(bf) = filter {
        if !bf.any_ref {
            return Ok((BTreeMap::new(), dnames));
        }
    }
    let resolved = filter.map(|bf| bf.resolve(sheets, &dnames));
    let is_ooxml = crate::crypto::has_magic(path, b"PK");
    let targets = if is_ooxml {
        crate::patch::sheet_targets(path).unwrap_or_default()
    } else {
        HashMap::new()
    };
    let mut shared_ss: Option<Vec<String>> = None;
    let mut out = BTreeMap::new();
    for (sid, want) in sheets.iter().enumerate() {
        if let Some(ref rf) = resolved {
            if !rf.includes_sheet(sid) {
                continue;
            }
        }
        let Some(name) = live_names.iter().find(|n| n.eq_ignore_ascii_case(want)) else {
            continue;
        };
        // 防止外部工作簿因角落残留格式/单元格（如 XFD1048576）触发 calamine 稠密网格数 GB OOM
        if is_ooxml {
            if let Some(part) = targets.get(name) {
                if crate::sparse::declared_area_of(path, part) > crate::sparse::DENSE_LIMIT {
                    let ss = shared_ss.get_or_insert_with(|| crate::sparse::shared_strings(path));
                    if let Ok(cells) = crate::sparse::read_sheet(path, part, ss) {
                        for ((r, c), cell) in cells {
                            if !cell.value.is_empty()
                                && resolved
                                    .as_ref()
                                    .is_none_or(|rf| rf.includes_cell(sid, r, c))
                            {
                                out.insert((sid, r, c), cell.value);
                            }
                        }
                        continue;
                    }
                }
            }
        }
        let range = match book.worksheet_range(name) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let (r0, c0) = range.start().unwrap_or((0, 0));
        for (row, col, data) in range.cells() {
            let rr = r0 + row as u32;
            let cc = c0 + col as u32;
            if let Some(ref rf) = resolved {
                if !rf.includes_cell(sid, rr, cc) {
                    continue;
                }
            }
            let v = data_to_value(data);
            if !v.is_empty() {
                out.insert((sid, rr, cc), v);
            }
        }
    }
    Ok((out, dnames))
}

fn data_to_value(d: &calamine::Data) -> Value {
    use calamine::Data;
    match d {
        Data::Int(i) => Value::Number(*i as f64),
        Data::Float(f) => Value::Number(*f),
        Data::String(s) => Value::Text(s.as_str().into()),
        Data::Bool(b) => Value::Bool(*b),
        Data::DateTime(x) => Value::Number(x.as_f64()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Value::Text(s.as_str().into()),
        Data::Error(e) => match e {
            calamine::CellErrorType::Div0 => Value::Error(crate::formula::ErrKind::Div0),
            calamine::CellErrorType::Name => Value::Error(crate::formula::ErrKind::Name),
            calamine::CellErrorType::NA => Value::Error(crate::formula::ErrKind::Na),
            calamine::CellErrorType::Num => Value::Error(crate::formula::ErrKind::Num),
            calamine::CellErrorType::Value => Value::Error(crate::formula::ErrKind::Value),
            calamine::CellErrorType::Ref => Value::Error(crate::formula::ErrKind::Ref),
            calamine::CellErrorType::Null => Value::Error(crate::formula::ErrKind::Null),
            _ => Value::Error(crate::formula::ErrKind::Value),
        },
        Data::Empty => Value::Empty,
    }
}

// ---------------------------------------------------------------- 公式里的书名

/// `[N]` 或 `[文件名]` → links 下标（0 基）
pub fn resolve_book(wb: &Workbook, name: &str) -> Option<usize> {
    let n = name.trim();
    if let Ok(i) = n.parse::<usize>() {
        if i >= 1 && i <= wb.links.len() {
            return Some(i - 1);
        }
    }
    let want = file_part(n);
    wb.links
        .iter()
        .position(|l| {
            l.target.eq_ignore_ascii_case(n)
                || l.file_name().eq_ignore_ascii_case(&want)
                || l.file_name().eq_ignore_ascii_case(n)
                || l.abs.as_deref().map(|a| a.eq_ignore_ascii_case(n)).unwrap_or(false)
        })
        .or_else(|| {
            // 带路径的写法：只看最后一段
            wb.links.iter().position(|l| l.file_name().eq_ignore_ascii_case(&want))
        })
}

fn file_part(s: &str) -> String {
    let t = s.replace('\\', "/");
    t.rsplit('/').next().unwrap_or(&t).to_string()
}

/// 表名 → sheetId（0 基）。没给表名或链接未带表名列表时用第 0 张；
/// 若链接已声明表名清单且找不到该表，返回 None（公式求值阶段会给 #REF!）。
pub fn sheet_slot(link: &ExternalBook, name: Option<&str>) -> Option<usize> {
    match name {
        None => Some(0),
        Some(n) => {
            let want = n.trim().trim_matches('\'');
            if link.sheets.is_empty() {
                Some(0)
            } else {
                link.sheets.iter().position(|s| s.eq_ignore_ascii_case(want))
            }
        }
    }
}

/// 给人看的公式：`[1]` → `[预算.xlsx]`
pub fn display_formula(wb: &Workbook, f: &str) -> String {
    map_books(f, |tok| {
        if let Ok(i) = tok.parse::<usize>() {
            if let Some(l) = wb.links.get(i.saturating_sub(1)) {
                return label(l);
            }
        }
        tok.to_string()
    })
}

/// 存盘/求值用的公式：`[预算.xlsx]` → `[1]`
pub fn raw_formula(wb: &Workbook, f: &str) -> String {
    map_books(f, |tok| match resolve_book(wb, tok) {
        Some(i) => (i + 1).to_string(),
        None => tok.to_string(),
    })
}

/// 断开第 `link_idx`（0 基）个外部链接：
/// 把本簿内所有引用该外部工作簿的公式静态化为当前缓存值（保留 `cell.value`、清除 `formula`），
/// 同时把后续链接序号 `[k]`（k > link_idx + 1）重排为 `[k - 1]`，并移除链接条目。
/// 返回转换为静态数值的单元格数量。
pub fn break_link(wb: &mut Workbook, link_idx: usize) -> usize {
    if link_idx >= wb.links.len() {
        return 0;
    }
    let broken_num = link_idx + 1;
    let broken_target = wb.links[link_idx].target.clone();
    let broken_fname = wb.links[link_idx].file_name();
    let broken_abs = wb.links[link_idx].abs.clone();
    let matches_broken = |tok: &str| -> bool {
        let t = tok.trim();
        if let Ok(i) = t.parse::<usize>() {
            return i == broken_num;
        }
        let want = file_part(t);
        broken_target.eq_ignore_ascii_case(t)
            || (!broken_fname.is_empty()
                && (broken_fname.eq_ignore_ascii_case(&want)
                    || broken_fname.eq_ignore_ascii_case(t)))
            || broken_abs
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(t))
    };

    let mut converted = 0usize;
    for sheet in wb.sheets.iter_mut() {
        let coords: Vec<crate::model::CellRef> = sheet
            .formula_cells()
            .map(|(c, _)| c)
            .collect();
        for cref in coords {
            let Some(orig) = sheet.get(cref.row, cref.col).and_then(|c| c.formula.clone()) else {
                continue;
            };
            let b: Vec<char> = orig.chars().collect();
            let mut out = String::with_capacity(orig.len());
            let mut uses_broken = false;
            let mut changed = false;
            let mut i = 0usize;
            while i < b.len() {
                if b[i] == '"' {
                    out.push('"');
                    i += 1;
                    while i < b.len() {
                        out.push(b[i]);
                        if b[i] == '"' {
                            if i + 1 < b.len() && b[i + 1] == '"' {
                                out.push('"');
                                i += 2;
                                continue;
                            }
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                if b[i] == '[' {
                    if let Some(close) = b[i + 1..].iter().position(|c| *c == ']') {
                        let inner: String = b[i + 1..i + 1 + close].iter().collect();
                        let starts_in_sq = i > 0 && b[i - 1] == '\'';
                        let looks_like_book = inner.chars().all(|c| c.is_ascii_digit())
                            || has_bang_after(&b, i + close + 2, starts_in_sq);
                        if looks_like_book {
                            if matches_broken(&inner) {
                                uses_broken = true;
                                break;
                            }
                            if let Ok(n) = inner.trim().parse::<usize>() {
                                if n > broken_num {
                                    out.push('[');
                                    out.push_str(&(n - 1).to_string());
                                    out.push(']');
                                    changed = true;
                                    i += close + 2;
                                    continue;
                                }
                            }
                        }
                    }
                }
                if b[i] == '\'' {
                    let start_q = i;
                    let mut j = i + 1;
                    let mut qname = String::new();
                    while j < b.len() {
                        if b[j] == '\'' {
                            if j + 1 < b.len() && b[j + 1] == '\'' {
                                qname.push('\'');
                                j += 2;
                                continue;
                            }
                            j += 1;
                            break;
                        }
                        qname.push(b[j]);
                        j += 1;
                    }
                    if b.get(j) == Some(&'!') && !qname.contains('[') && matches_broken(&qname) {
                        uses_broken = true;
                        break;
                    }
                    if b.get(j) == Some(&'!') && !qname.contains('[') {
                        for k in start_q..=j {
                            out.push(b[k]);
                        }
                        i = j + 1;
                        continue;
                    }
                }
                out.push(b[i]);
                i += 1;
            }
            if uses_broken {
                if let Some(cell) = sheet.get_mut(cref.row, cref.col) {
                    cell.formula = None;
                    converted += 1;
                }
            } else if changed {
                if let Some(cell) = sheet.get_mut(cref.row, cref.col) {
                    cell.formula = Some(out.into());
                }
            }
        }
    }
    wb.links.remove(link_idx);
    for (k, l) in wb.links.iter_mut().enumerate() {
        l.index = k + 1;
    }
    converted
}

fn map_books<F: Fn(&str) -> String>(f: &str, map: F) -> String {
    let b: Vec<char> = f.chars().collect();
    let mut out = String::with_capacity(f.len());
    let mut i = 0;
    while i < b.len() {
        // 双引号字符串字面量里的 `[1]` 只是普通文本，不能被重写。
        if b[i] == '"' {
            out.push('"');
            i += 1;
            while i < b.len() {
                out.push(b[i]);
                if b[i] == '"' {
                    if i + 1 < b.len() && b[i + 1] == '"' {
                        out.push('"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b[i] == '[' {
            if let Some(close) = b[i + 1..].iter().position(|c| *c == ']') {
                let inner: String = b[i + 1..i + 1 + close].iter().collect();
                // 只换「书名」形态：数字序号、文件名、带路径的文件名；
                // 结构化引用（表[列]）里的列名不匹配任何链接，map 会原样返回
                out.push('[');
                out.push_str(&map(&inner));
                out.push(']');
                i += close + 2;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// 公式里出现没登记的工作簿时自动建链接，并尝试从磁盘读到表名和缓存。
///
/// 返回新增的链接数。Excel 里手输 `=[预算.xlsx]Sheet1!A1` 就是这个语义。
pub fn absorb_formula(wb: &mut Workbook, formula: &str) -> usize {
    let mut added = 0;
    for name in books_in_formula(formula) {
        if name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if resolve_book(wb, &name).is_some() {
            continue;
        }
        let fname = file_part(&name);
        if fname.is_empty() || fname.eq_ignore_ascii_case("") {
            continue;
        }
        wb.links.push(ExternalBook {
            index: wb.links.len() + 1,
            target: fname,
            ..Default::default()
        });
        added += 1;
    }
    if added > 0 {
        refresh(wb);
    }
    added
}

/// 这条公式是否引用了外部工作簿
pub fn is_external_formula(f: &str) -> bool {
    !books_in_formula(f).is_empty()
}

/// 公式里引用了哪些外部工作簿（用于"用户手输公式后自动建链接"）
pub fn books_in_formula(f: &str) -> Vec<String> {
    let b: Vec<char> = f.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '"' {
            i += 1;
            while i < b.len() {
                if b[i] == '"' {
                    if i + 1 < b.len() && b[i + 1] == '"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b[i] == '[' {
            if let Some(close) = b[i + 1..].iter().position(|c| *c == ']') {
                let inner: String = b[i + 1..i + 1 + close].iter().collect();
                let after = b.get(i + 1 + close + 1).copied();
                let starts_in_sq = i > 0 && b[i - 1] == '\'';
                // 书名的特征是 `]` 后面跟表名（可选三维 `First:Third` / 带引号）与 `!`，
                // 或直接跟 `!`（工作簿级外部名称 `[1]!Rate`），或纯数字序号；`表[列]` 后面不会有 `!`。
                if !inner.is_empty()
                    && (inner.chars().all(|c| c.is_ascii_digit())
                        || has_bang_after(&b, i + close + 2, starts_in_sq))
                    && after.is_some()
                {
                    if !out.iter().any(|x| x.eq_ignore_ascii_case(&inner)) {
                        out.push(inner);
                    }
                }
                i += close + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn has_bang_after(b: &[char], from: usize, starts_in_sq: bool) -> bool {
    let mut i = from;
    let mut in_sq = starts_in_sq;
    let limit = (from + 160).min(b.len());
    while i < limit {
        let c = b[i];
        if in_sq {
            if c == '\'' {
                if b.get(i + 1) == Some(&'\'') {
                    i += 2;
                    continue;
                }
                in_sq = false;
            }
            i += 1;
            continue;
        }
        match c {
            '!' => return true,
            '\'' => in_sq = true,
            '[' | ']' | ',' | '(' | ')' | '+' | '-' | '*' | '/' | '&' | '=' => return false,
            ch if ch.is_whitespace() => return false,
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ids_in_order() {
        let xml = r#"<workbook><sheets/><externalReferences><externalReference r:id="rId2"/><externalReference r:id="rId5"/></externalReferences></workbook>"#;
        assert_eq!(parse_external_ids(xml), vec!["rId2", "rId5"]);
    }

    #[test]
    fn parses_sheet_names_and_cache() {
        let xml = r#"<externalLink><externalBook r:id="rId1"><sheetNames><sheetName val="预算"/><sheetName val="明细"/></sheetNames><sheetDataSet><sheetData sheetId="0"><row r="1"><cell r="A1"><v>10</v></cell><cell r="B1" t="str"><v>预算</v></cell></row><row r="2"><cell r="A2"><v>20.5</v></cell></row></sheetData><sheetData sheetId="1"><row r="3"><cell r="C3" t="b"><v>1</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"#;
        let p = parse_link_part(xml);
        assert_eq!(p.sheets, vec!["预算", "明细"]);
        assert!(matches!(p.cache.get(&(0, 0, 0)), Some(Value::Number(n)) if *n == 10.0));
        assert!(matches!(p.cache.get(&(0, 0, 1)), Some(Value::Text(t)) if &**t == "预算"));
        assert!(matches!(p.cache.get(&(1, 2, 2)), Some(Value::Bool(true))));
    }

    #[test]
    fn rewrites_sheet_data_only() {
        let xml = r#"<externalLink><externalBook r:id="rId1"><sheetNames><sheetName val="预算"/></sheetNames><sheetDataSet><sheetData sheetId="0" refreshError="1"/></sheetDataSet></externalBook></externalLink>"#;
        let mut link = ExternalBook { sheets: vec!["预算".into()], ..Default::default() };
        link.cache.insert((0, 0, 0), Value::Number(42.0));
        link.cache.insert((0, 0, 1), Value::Text("好".into()));
        let out = replace_sheet_data(xml, &link);
        assert!(out.contains("<cell r=\"A1\"><v>42</v></cell>"));
        assert!(out.contains("<cell r=\"B1\" t=\"str\"><v>好</v></cell>"));
        assert!(!out.contains("refreshError"), "旧缓存应被整段替换：{out}");
        assert_eq!(out.matches("<sheetDataSet").count(), 1);
        assert!(out.contains("<sheetName val=\"预算\"/>"), "表名要留着");
    }

    #[test]
    fn book_name_mapping() {
        let mut wb = Workbook::new_blank();
        wb.links.push(ExternalBook {
            index: 1,
            target: "预算.xlsx".into(),
            ..Default::default()
        });
        assert_eq!(display_formula(&wb, "=[1]Sheet1!A1"), "=[预算.xlsx]Sheet1!A1");
        assert_eq!(raw_formula(&wb, "=[预算.xlsx]Sheet1!A1"), "=[1]Sheet1!A1");
        // 结构化引用不动
        assert_eq!(display_formula(&wb, "=SUM(表[列])"), "=SUM(表[列])");
        assert_eq!(resolve_book(&wb, "预算.xlsx"), Some(0));
        assert_eq!(resolve_book(&wb, "1"), Some(0));
        assert_eq!(resolve_book(&wb, "别的.xlsx"), None);
    }

    #[test]
    fn finds_books_in_typed_formula() {
        assert_eq!(
            books_in_formula("=SUM([预算.xlsx]Sheet1!A1:A3)+[2]X!B2"),
            vec!["预算.xlsx", "2"]
        );
        assert!(books_in_formula("=SUM(表[列])").is_empty());
        assert_eq!(books_in_formula("=[预算.xlsx]明细!A1"), vec!["预算.xlsx"]);
    }

    #[test]
    fn joins_package_paths() {
        assert_eq!(join_part("xl", "externalLinks/externalLink1.xml"), "xl/externalLinks/externalLink1.xml");
        assert_eq!(join_part("xl", "../xl/externalLinks/e.xml"), "xl/externalLinks/e.xml");
        assert_eq!(part_rels("xl/externalLinks/externalLink1.xml"), "xl/externalLinks/_rels/externalLink1.xml.rels");
    }
}
