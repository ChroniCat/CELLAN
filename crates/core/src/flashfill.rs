//! 快速填充（Flash Fill）：从目标列已有的示例反推一条变换规则，再套用到其余行。
//!
//! 支持：
//! - 常见分隔符前后 / 最后一段 / 第 N 段 / 成对括号之间提取；
//! - 固定位置截取（前 N / 后 N / 切片 `(start, len)`）、手机号/证件号脱敏掩码；
//! - 纯数字 / 非数字 / 大小写 / 词首大写 / 去首尾空白；
//! - 带固定前后缀的模板包装（如 `zhangsan` -> `zhangsan@company.com`、`1001` -> `ORD-1001`）；
//! - 多列自由拼接模板（自动发现相邻多列与字面量组合，如 `A="张", B="三", C="研发"` -> `张三(研发)`）；
//! - `auto_flash_fill` 供 `Ctrl+E` 一键自动检测连续数据区域与源列执行。

use crate::model::{Cell, Sheet, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplatePart {
    Literal(String),
    Col(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// 原样
    Whole,
    /// 去首尾空白
    Trim,
    /// 第一个分隔符之前 / 之后
    BeforeFirst(char),
    AfterFirst(char),
    /// 最后一个分隔符之后
    AfterLast(char),
    /// 按分隔符切分后的第 N 段（0 起）
    NthToken(char, usize),
    /// 成对定界符之间（如 `(...)`、`【...】`）
    Between(char, char),
    /// 前 N 个字符 / 后 N 个字符 / 任意定长切片
    Left(usize),
    Right(usize),
    Slice(usize, usize),
    /// 保留前 `prefix`、后 `suffix` 字符，中间用 `mask_char` 重复 `mask_len` 次脱敏
    Mask {
        prefix: usize,
        suffix: usize,
        mask_char: char,
        mask_len: usize,
    },
    /// 只留数字 / 只留非数字
    Digits,
    NonDigits,
    /// 大小写 / 词首大写
    Upper,
    Lower,
    TitleCase,
    /// 拼接：源 + 分隔符 + 另一列
    Concat(char, u32),
    /// 直接拼接：源 + 另一列（无分隔符）
    Join(u32),
    /// 前后缀模板包装：`prefix + inner(src) + suffix`
    Template {
        prefix: String,
        inner: Box<Rule>,
        suffix: String,
    },
    /// 多列模板拼接（字面量 + 任意列组合）
    MultiConcat(Vec<TemplatePart>),
}

impl Rule {
    pub fn label(&self) -> String {
        match self {
            Rule::Whole => "原样".into(),
            Rule::Trim => "去除首尾空格".into(),
            Rule::BeforeFirst(c) => format!("「{c}」之前"),
            Rule::AfterFirst(c) => format!("「{c}」之后"),
            Rule::AfterLast(c) => format!("最后一个「{c}」之后"),
            Rule::NthToken(c, n) => format!("按「{c}」拆分第 {} 段", n + 1),
            Rule::Between(l, r) => format!("「{l}…{r}」之间"),
            Rule::Left(n) => format!("前 {n} 个字符"),
            Rule::Right(n) => format!("后 {n} 个字符"),
            Rule::Slice(s, n) => format!("第 {} 位起 {n} 个字符", s + 1),
            Rule::Mask { prefix, suffix, mask_char, .. } => {
                format!("前 {prefix} 后 {suffix} 位保留，中间「{mask_char}」脱敏")
            }
            Rule::Digits => "只留数字".into(),
            Rule::NonDigits => "只留非数字".into(),
            Rule::Upper => "大写".into(),
            Rule::Lower => "小写".into(),
            Rule::TitleCase => "词首大写".into(),
            Rule::Concat(c, c2) => format!("与 {} 列用「{c}」拼接", crate::model::col_label(*c2)),
            Rule::Join(c2) => format!("与 {} 列直接拼接", crate::model::col_label(*c2)),
            Rule::Template { prefix, inner, suffix } => {
                format!("模板「{prefix}{{{}}}{suffix}」", inner.label())
            }
            Rule::MultiConcat(parts) => {
                let cols: Vec<String> = parts
                    .iter()
                    .filter_map(|p| match p {
                        TemplatePart::Col(c) => Some(crate::model::col_label(*c)),
                        TemplatePart::Literal(_) => None,
                    })
                    .collect();
                format!("多列模板拼接（{}）", cols.join("+"))
            }
        }
    }

    pub fn apply(&self, src: &str, sheet: &Sheet, row: u32) -> String {
        match self {
            Rule::Whole => src.to_string(),
            Rule::Trim => src.trim().to_string(),
            Rule::BeforeFirst(c) => src.split(*c).next().unwrap_or("").to_string(),
            Rule::AfterFirst(c) => src.splitn(2, *c).nth(1).unwrap_or("").to_string(),
            Rule::AfterLast(c) => src.rsplit(*c).next().unwrap_or("").to_string(),
            Rule::NthToken(c, n) => src.split(*c).nth(*n).unwrap_or("").to_string(),
            Rule::Between(l, r) => {
                if let Some(after) = src.splitn(2, *l).nth(1) {
                    after.split(*r).next().unwrap_or("").to_string()
                } else {
                    String::new()
                }
            }
            Rule::Left(n) => src.chars().take(*n).collect(),
            Rule::Right(n) => {
                let len = src.chars().count();
                src.chars().skip(len.saturating_sub(*n)).collect()
            }
            Rule::Slice(start, len) => src.chars().skip(*start).take(*len).collect(),
            Rule::Mask {
                prefix,
                suffix,
                mask_char,
                mask_len,
            } => {
                let chars: Vec<char> = src.chars().collect();
                if chars.len() < prefix + suffix {
                    return src.to_string();
                }
                let mut out = String::with_capacity(src.len());
                for &ch in &chars[..*prefix] {
                    out.push(ch);
                }
                for _ in 0..*mask_len {
                    out.push(*mask_char);
                }
                for &ch in &chars[chars.len() - *suffix..] {
                    out.push(ch);
                }
                out
            }
            Rule::Digits => src.chars().filter(|c| c.is_ascii_digit()).collect(),
            Rule::NonDigits => src
                .chars()
                .filter(|c| !c.is_ascii_digit() && !c.is_whitespace())
                .collect(),
            Rule::Upper => src.to_uppercase(),
            Rule::Lower => src.to_lowercase(),
            Rule::TitleCase => title_case(src),
            Rule::Concat(c, col) => {
                let other = sheet.value(row, *col).display();
                format!("{src}{c}{other}")
            }
            Rule::Join(col) => {
                let other = sheet.value(row, *col).display();
                format!("{src}{other}")
            }
            Rule::Template {
                prefix,
                inner,
                suffix,
            } => {
                let mid = inner.apply(src, sheet, row);
                format!("{prefix}{mid}{suffix}")
            }
            Rule::MultiConcat(parts) => {
                let mut out = String::new();
                for p in parts {
                    match p {
                        TemplatePart::Literal(s) => out.push_str(s),
                        TemplatePart::Col(col) => out.push_str(&sheet.value(row, *col).display()),
                    }
                }
                out
            }
        }
    }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start_word = true;
    for ch in s.chars() {
        if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | '.') {
            start_word = true;
            out.push(ch);
        } else if start_word {
            for uc in ch.to_uppercase() {
                out.push(uc);
            }
            start_word = false;
        } else {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        }
    }
    out
}

const DELIMS: [char; 16] = [
    ' ', ',', '，', '-', '_', '/', '|', ':', '：', '.', '@', ';', '；', '\\', '、', '—',
];

const BRACKET_PAIRS: [(char, char); 6] = [
    ('(', ')'),
    ('（', '）'),
    ('[', ']'),
    ('【', '】'),
    ('<', '>'),
    ('{', '}'),
];

#[derive(Debug)]
pub struct FlashFill {
    pub rule: Rule,
    pub filled: usize,
}

fn common_prefix(strs: &[&str]) -> String {
    let Some((first, rest)) = strs.split_first() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.chars().collect();
    for s in rest {
        let chars: Vec<char> = s.chars().collect();
        let mut n = 0;
        while n < prefix.len() && n < chars.len() && prefix[n] == chars[n] {
            n += 1;
        }
        prefix.truncate(n);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.into_iter().collect()
}

fn common_suffix(strs: &[&str]) -> String {
    let Some((first, rest)) = strs.split_first() else {
        return String::new();
    };
    let mut suffix: Vec<char> = first.chars().rev().collect();
    for s in rest {
        let chars: Vec<char> = s.chars().rev().collect();
        let mut n = 0;
        while n < suffix.len() && n < chars.len() && suffix[n] == chars[n] {
            n += 1;
        }
        suffix.truncate(n);
        if suffix.is_empty() {
            break;
        }
    }
    suffix.into_iter().rev().collect()
}

fn infer_multi_col_template(
    sheet: &Sheet,
    examples: &[(u32, String, String)],
    cand_cols: &[u32],
) -> Option<Rule> {
    if cand_cols.len() < 2 || examples.is_empty() {
        return None;
    }
    let (row0, _, dst0) = &examples[0];
    // 贪心把示例 0 拆解成若干列值和字面量片段，再用全部示例验证
    let col_vals: Vec<(u32, String)> = cand_cols
        .iter()
        .map(|&c| (c, sheet.value(*row0, c).display()))
        .filter(|(_, v)| !v.is_empty())
        .collect();
    if col_vals.len() < 2 {
        return None;
    }
    let mut parts: Vec<TemplatePart> = Vec::new();
    let mut pos = 0usize;
    let mut used_cols = 0usize;
    let mut used_mask = vec![false; col_vals.len()];
    while pos < dst0.len() {
        // 找从 pos 之后最早出现（长度优先）的未用列值
        let mut best: Option<(usize, usize, usize)> = None; // (match_byte_offset, col_val_idx, byte_len)
        for (idx, (_, val)) in col_vals.iter().enumerate() {
            if used_mask[idx] {
                continue;
            }
            if let Some(rel) = dst0[pos..].find(val.as_str()) {
                let off = pos + rel;
                let better = match best {
                    None => true,
                    Some((boff, _, blen)) => off < boff || (off == boff && val.len() > blen),
                };
                if better {
                    best = Some((off, idx, val.len()));
                }
            }
        }
        let Some((off, idx, vlen)) = best else {
            break;
        };
        if off > pos {
            parts.push(TemplatePart::Literal(dst0[pos..off].to_string()));
        }
        parts.push(TemplatePart::Col(col_vals[idx].0));
        used_mask[idx] = true;
        used_cols += 1;
        pos = off + vlen;
    }
    if used_cols < 2 {
        return None;
    }
    if pos < dst0.len() {
        parts.push(TemplatePart::Literal(dst0[pos..].to_string()));
    }
    let rule = Rule::MultiConcat(parts);
    if examples
        .iter()
        .all(|(row, src, want)| rule.apply(src, sheet, *row) == *want)
    {
        Some(rule)
    } else {
        None
    }
}

fn infer_mask_rule(examples: &[(u32, String, String)]) -> Option<Rule> {
    let (_, src0, dst0) = examples.first()?;
    let sc: Vec<char> = src0.chars().collect();
    let dc: Vec<char> = dst0.chars().collect();
    if sc.len() < 4 || dc.len() < 3 {
        return None;
    }
    let mut prefix = 0usize;
    while prefix < sc.len() && prefix < dc.len() && sc[prefix] == dc[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < sc.len() - prefix
        && suffix < dc.len() - prefix
        && sc[sc.len() - 1 - suffix] == dc[dc.len() - 1 - suffix]
    {
        suffix += 1;
    }
    if prefix == 0 && suffix == 0 {
        return None;
    }
    let mid = &dc[prefix..dc.len() - suffix];
    if mid.is_empty() {
        return None;
    }
    let mask_char = mid[0];
    if !matches!(mask_char, '*' | '#' | 'X' | 'x' | '•' | '·') || !mid.iter().all(|&c| c == mask_char) {
        return None;
    }
    Some(Rule::Mask {
        prefix,
        suffix,
        mask_char,
        mask_len: mid.len(),
    })
}

fn build_base_candidates(
    examples: &[(u32, String, String)],
    aux_col: Option<u32>,
) -> Vec<Rule> {
    let mut cands: Vec<Rule> = vec![
        Rule::Whole,
        Rule::Trim,
        Rule::Digits,
        Rule::NonDigits,
        Rule::Upper,
        Rule::Lower,
        Rule::TitleCase,
    ];
    for d in DELIMS {
        cands.push(Rule::BeforeFirst(d));
        cands.push(Rule::AfterFirst(d));
        cands.push(Rule::AfterLast(d));
        for nth in 0..=4 {
            cands.push(Rule::NthToken(d, nth));
        }
    }
    for (l, r) in BRACKET_PAIRS {
        cands.push(Rule::Between(l, r));
    }
    // 长度与切片规则按示例反推
    let mut lens: Vec<usize> = examples
        .iter()
        .map(|(_, _, dst)| dst.chars().count())
        .collect();
    lens.sort_unstable();
    lens.dedup();
    for n in &lens {
        if *n > 0 && *n < 60 {
            cands.push(Rule::Left(*n));
            cands.push(Rule::Right(*n));
        }
    }
    if let Some((_, src0, dst0)) = examples.first() {
        let dlen = dst0.chars().count();
        if dlen > 0 && dlen < 60 {
            if let Some(byte_pos) = src0.find(dst0.as_str()) {
                let start_char = src0[..byte_pos].chars().count();
                if start_char > 0 {
                    cands.push(Rule::Slice(start_char, dlen));
                }
            }
        }
    }
    if let Some(mask) = infer_mask_rule(examples) {
        cands.push(mask);
    }
    if let Some(a) = aux_col {
        cands.push(Rule::Join(a));
        for d in [' ', '-', '/', '_', ',', '，', ':', '：'] {
            cands.push(Rule::Concat(d, a));
        }
    }
    cands
}

fn infer_rule(
    sheet: &Sheet,
    examples: &[(u32, String, String)],
    aux_col: Option<u32>,
    multi_cols: &[u32],
) -> Option<Rule> {
    let cands = build_base_candidates(examples, aux_col);
    if let Some(rule) = cands.iter().find(|rule| {
        examples
            .iter()
            .all(|(row, src, want)| rule.apply(src, sheet, *row) == *want)
    }) {
        return Some(rule.clone());
    }
    // 尝试多列自由拼接模板（如 A + B + C + 固定符号）
    if let Some(mc) = infer_multi_col_template(sheet, examples, multi_cols) {
        return Some(mc);
    }
    // 尝试剥离公共前后缀后匹配核心提取规则（如 "员工:张三"、"zhangsan@corp.com"）
    let dst_refs: Vec<&str> = examples.iter().map(|(_, _, d)| d.as_str()).collect();
    let (prefix_cands, suffix_cands): (Vec<String>, Vec<String>) = if examples.len() >= 2 {
        let cp = common_prefix(&dst_refs);
        let cs = common_suffix(&dst_refs);
        (
            if cp.is_empty() { vec![String::new()] } else { vec![cp, String::new()] },
            if cs.is_empty() { vec![String::new()] } else { vec![cs, String::new()] },
        )
    } else {
        // 单示例：尝试在 dst 中定位 src 或基础提取结果作为核心，左右残余作为固定前后缀
        let (row0, src0, dst0) = &examples[0];
        let mut found = None;
        for base in &cands {
            let mid = base.apply(src0, sheet, *row0);
            if mid.is_empty() || mid.len() >= dst0.len() {
                continue;
            }
            if let Some(pos) = dst0.find(&mid) {
                let pfx = dst0[..pos].to_string();
                let sfx = dst0[pos + mid.len()..].to_string();
                if !pfx.is_empty() || !sfx.is_empty() {
                    found = Some(Rule::Template {
                        prefix: pfx,
                        inner: Box::new(base.clone()),
                        suffix: sfx,
                    });
                    break;
                }
            }
        }
        return found;
    };
    for pfx in &prefix_cands {
        for sfx in &suffix_cands {
            if pfx.is_empty() && sfx.is_empty() {
                continue;
            }
            let stripped: Option<Vec<(u32, String, String)>> = examples
                .iter()
                .map(|(row, src, dst)| {
                    let mid = dst.strip_prefix(pfx.as_str())?.strip_suffix(sfx.as_str())?;
                    Some((*row, src.clone(), mid.to_string()))
                })
                .collect();
            let Some(stripped) = stripped else { continue };
            if stripped.iter().any(|(_, _, m)| m.is_empty()) {
                continue;
            }
            let sub_cands = build_base_candidates(&stripped, aux_col);
            if let Some(inner) = sub_cands.into_iter().find(|rule| {
                stripped
                    .iter()
                    .all(|(row, src, want)| rule.apply(src, sheet, *row) == *want)
            }) {
                return Some(Rule::Template {
                    prefix: pfx.clone(),
                    inner: Box::new(inner),
                    suffix: sfx.clone(),
                });
            }
        }
    }
    None
}

/// 用 `dst_col` 里已有的示例反推规则，并填到 `r1..=r2` 的空目标格里。
pub fn flash_fill(
    sheet: &mut Sheet,
    src_col: u32,
    dst_col: u32,
    r1: u32,
    r2: u32,
    aux_col: Option<u32>,
) -> Result<FlashFill, String> {
    if src_col == dst_col {
        return Err("源列和目标列不能是同一列".into());
    }
    let mut examples: Vec<(u32, String, String)> = Vec::new();
    for r in r1..=r2 {
        let dst = sheet.value(r, dst_col).display();
        if dst.trim().is_empty() {
            continue;
        }
        let src = sheet.value(r, src_col).display();
        if src.trim().is_empty() {
            continue;
        }
        examples.push((r, src, dst));
    }
    if examples.is_empty() {
        return Err("先在目标列写一个示例（比如从「张三-研发」里取出「研发」）".into());
    }
    let mut multi_cols = vec![src_col];
    if let Some(a) = aux_col {
        if a != src_col && a != dst_col {
            multi_cols.push(a);
        }
    }
    let rule = infer_rule(sheet, &examples, aux_col, &multi_cols).ok_or_else(|| {
        format!(
            "从 {} 条示例里推不出规则（示例：{:?}）：换一个更规整的示例试试",
            examples.len(),
            examples.first().map(|(_, s, d)| format!("{s} → {d}"))
        )
    })?;
    let mut filled = 0usize;
    for r in r1..=r2 {
        let src = sheet.value(r, src_col).display();
        if src.trim().is_empty() {
            continue;
        }
        if !sheet.value(r, dst_col).display().trim().is_empty() {
            continue;
        }
        let out = rule.apply(&src, sheet, r);
        if out.is_empty() {
            continue;
        }
        sheet.set(
            r,
            dst_col,
            Cell {
                value: Value::Text(out.into()),
                formula: None,
            },
        );
        filled += 1;
    }
    Ok(FlashFill { rule, filled })
}

/// Ctrl+E 一键自动快速填充：以当前目标列 `dst_col` 和锚点行 `anchor_row` 自动探测相邻数据列和连续行范围，
/// 推断规则并填充所有空白目标格，返回 `(FlashFill, 实际写入坐标列表)` 供调用方纳入撤销与重算。
pub fn auto_flash_fill(
    sheet: &mut Sheet,
    dst_col: u32,
    anchor_row: u32,
) -> Result<(FlashFill, Vec<(u32, u32)>), String> {
    // 收集紧邻左侧（最多 4 列）或紧邻右侧的连续非空列作为候选源列
    let mut neighbor_cols: Vec<u32> = Vec::new();
    let mut c = dst_col;
    while c > 0 && neighbor_cols.len() < 4 {
        c -= 1;
        let has_near = !sheet.value(anchor_row, c).display().trim().is_empty()
            || (anchor_row > 0 && !sheet.value(anchor_row - 1, c).display().trim().is_empty())
            || !sheet.value(anchor_row + 1, c).display().trim().is_empty();
        if has_near {
            neighbor_cols.push(c);
        } else {
            break;
        }
    }
    neighbor_cols.reverse();
    if neighbor_cols.is_empty() && dst_col + 1 < 16_384 {
        let right = dst_col + 1;
        let has_near = !sheet.value(anchor_row, right).display().trim().is_empty()
            || (anchor_row > 0 && !sheet.value(anchor_row - 1, right).display().trim().is_empty())
            || !sheet.value(anchor_row + 1, right).display().trim().is_empty();
        if has_near {
            neighbor_cols.push(right);
        }
    }
    if neighbor_cols.is_empty() {
        return Err("相邻列没有可用于快速填充的源数据".into());
    }
    let primary_src = *neighbor_cols.last().unwrap();
    // 沿相邻源列上下扩展连续数据区间（上限 50,000 行）
    let row_has_src = |sh: &Sheet, r: u32| {
        neighbor_cols
            .iter()
            .any(|&col| !sh.value(r, col).display().trim().is_empty())
    };
    let mut r1 = anchor_row;
    while r1 > 0 && anchor_row - r1 < 50_000 && row_has_src(sheet, r1 - 1) {
        r1 -= 1;
    }
    let mut r2 = anchor_row;
    while r2 + 1 < 1_048_576 && r2 - r1 < 50_000 && row_has_src(sheet, r2 + 1) {
        r2 += 1;
    }
    if r2 <= r1 {
        return Err("连续数据不足两行，无法执行快速填充".into());
    }
    // 若首行看起来像表头（所有源列都是文本，且下方存在示例而首行目标列为空或表头），检查是否应跳过首行
    let mut examples: Vec<(u32, String, String)> = Vec::new();
    for r in r1..=r2 {
        let dst = sheet.value(r, dst_col).display();
        if dst.trim().is_empty() {
            continue;
        }
        let src = sheet.value(r, primary_src).display();
        if src.trim().is_empty() {
            continue;
        }
        examples.push((r, src, dst));
    }
    if examples.is_empty() {
        return Err("请先在当前列输入至少 1 个示例后再按 Ctrl+E".into());
    }
    // 尝试每个相邻源列（优先紧邻列）及多列组合
    let mut matched: Option<(u32, Rule)> = None;
    for &sc in neighbor_cols.iter().rev() {
        let ex: Vec<(u32, String, String)> = examples
            .iter()
            .map(|(r, _, dst)| (*r, sheet.value(*r, sc).display(), dst.clone()))
            .filter(|(_, s, _)| !s.trim().is_empty())
            .collect();
        if ex.is_empty() {
            continue;
        }
        let aux = neighbor_cols.iter().copied().find(|&c| c != sc);
        if let Some(rule) = infer_rule(sheet, &ex, aux, &neighbor_cols) {
            matched = Some((sc, rule));
            break;
        }
    }
    // 若含表头（首行有示例但推不出规则），去掉首行重试
    if matched.is_none() && examples.len() >= 2 && examples[0].0 == r1 {
        let trimmed = &examples[1..];
        for &sc in neighbor_cols.iter().rev() {
            let ex: Vec<(u32, String, String)> = trimmed
                .iter()
                .map(|(r, _, dst)| (*r, sheet.value(*r, sc).display(), dst.clone()))
                .filter(|(_, s, _)| !s.trim().is_empty())
                .collect();
            let aux = neighbor_cols.iter().copied().find(|&c| c != sc);
            if let Some(rule) = infer_rule(sheet, &ex, aux, &neighbor_cols) {
                r1 += 1;
                matched = Some((sc, rule));
                break;
            }
        }
    }
    let Some((src_col, rule)) = matched else {
        return Err("未能从现有示例中识别出明确规律，请多写 1 个示例后重试".into());
    };
    let mut written = Vec::new();
    for r in r1..=r2 {
        if !sheet.value(r, dst_col).display().trim().is_empty() {
            continue;
        }
        let src = sheet.value(r, src_col).display();
        if src.trim().is_empty() && !matches!(rule, Rule::MultiConcat(_)) {
            continue;
        }
        let out = rule.apply(&src, sheet, r);
        if out.is_empty() {
            continue;
        }
        sheet.set(
            r,
            dst_col,
            Cell {
                value: Value::Text(out.into()),
                formula: None,
            },
        );
        written.push((r, dst_col));
    }
    let filled = written.len();
    Ok((FlashFill { rule, filled }, written))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with(rows: &[(&str, &str, &str)]) -> Sheet {
        let mut sh = Sheet::new("S");
        for (i, (a, b, c)) in rows.iter().enumerate() {
            let r = i as u32;
            sh.set(r, 0, Cell { value: Value::Text((*a).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Text((*b).into()), formula: None });
            sh.set(r, 2, Cell { value: Value::Text((*c).into()), formula: None });
        }
        sh
    }

    #[test]
    fn learns_suffix_after_dash() {
        let mut sh = sheet_with(&[
            ("张三-研发部", "研发部", ""),
            ("李四-销售部", "", ""),
            ("王五-研发部", "", ""),
        ]);
        let out = flash_fill(&mut sh, 0, 1, 0, 2, None).unwrap();
        assert_eq!(out.rule, Rule::AfterFirst('-'));
        assert_eq!(out.filled, 2);
        assert_eq!(sh.value(1, 1).display(), "销售部");
        assert_eq!(sh.value(2, 1).display(), "研发部");
    }

    #[test]
    fn learns_digits_only() {
        let mut sh = sheet_with(&[
            ("订单 A-1001", "1001", ""),
            ("订单 B-2002", "", ""),
        ]);
        let out = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap();
        assert_eq!(out.rule, Rule::Digits);
        assert_eq!(sh.value(1, 1).display(), "2002");
    }

    #[test]
    fn learns_concat_from_aux_column() {
        let mut sh = Sheet::new("S");
        sh.set(0, 0, Cell { value: Value::Text("张".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("三".into()), formula: None });
        sh.set(0, 2, Cell { value: Value::Text("张三".into()), formula: None });
        sh.set(1, 0, Cell { value: Value::Text("李".into()), formula: None });
        sh.set(1, 1, Cell { value: Value::Text("四".into()), formula: None });
        let out = flash_fill(&mut sh, 0, 2, 0, 1, Some(1)).unwrap();
        assert_eq!(out.rule, Rule::Join(1), "示例「张三」没有分隔符 → 直接拼接");
        assert_eq!(sh.value(1, 2).display(), "李四");
    }

    #[test]
    fn refuses_without_examples() {
        let mut sh = sheet_with(&[("甲", "", ""), ("乙", "", "")]);
        let err = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap_err();
        assert!(err.contains("示例"), "{err}");
    }

    #[test]
    fn refuses_when_no_rule_matches() {
        let mut sh = sheet_with(&[("abc", "xyz", ""), ("abd", "", "")]);
        let err = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap_err();
        assert!(err.contains("推不出"), "{err}");
    }

    #[test]
    fn does_not_overwrite_existing_targets() {
        let mut sh = sheet_with(&[
            ("A-1", "1", ""),
            ("B-2", "2", ""),
            ("C-3", "", ""),
        ]);
        let out = flash_fill(&mut sh, 0, 1, 0, 2, None).unwrap();
        assert_eq!(out.filled, 1, "只填空格");
        assert_eq!(sh.value(1, 1).display(), "2", "已有内容不动");
        assert_eq!(sh.value(2, 1).display(), "3");
    }

    #[test]
    fn learns_advanced_templates_masks_and_multi_col() {
        // 1) 模板包装：zhangsan -> zhangsan@corp.com
        let mut sh = sheet_with(&[
            ("zhangsan", "zhangsan@corp.com", ""),
            ("lisi", "", ""),
        ]);
        let out = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap();
        assert_eq!(out.filled, 1);
        assert_eq!(sh.value(1, 1).display(), "lisi@corp.com");

        // 2) 手机号脱敏：13812345678 -> 138****5678
        let mut sh2 = sheet_with(&[
            ("13812345678", "138****5678", ""),
            ("13987654321", "", ""),
        ]);
        let out2 = flash_fill(&mut sh2, 0, 1, 0, 1, None).unwrap();
        assert_eq!(out2.filled, 1);
        assert_eq!(sh2.value(1, 1).display(), "139****4321");

        // 3) Ctrl+E 多列模板自动拼接：A="张", B="三", C="研发" -> D="张三(研发)"
        let mut sh3 = Sheet::new("S");
        sh3.set(0, 0, Cell { value: Value::Text("张".into()), formula: None });
        sh3.set(0, 1, Cell { value: Value::Text("三".into()), formula: None });
        sh3.set(0, 2, Cell { value: Value::Text("研发".into()), formula: None });
        sh3.set(0, 3, Cell { value: Value::Text("张三(研发)".into()), formula: None });
        sh3.set(1, 0, Cell { value: Value::Text("李".into()), formula: None });
        sh3.set(1, 1, Cell { value: Value::Text("四".into()), formula: None });
        sh3.set(1, 2, Cell { value: Value::Text("财务".into()), formula: None });
        let (out3, written) = auto_flash_fill(&mut sh3, 3, 1).unwrap();
        assert_eq!(out3.filled, 1);
        assert_eq!(written, vec![(1, 3)]);
        assert_eq!(sh3.value(1, 3).display(), "李四(财务)");
    }
}
