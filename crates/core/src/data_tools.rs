//! 数据工具：多级排序、删除重复、分列。
//!
//! 和 Excel 一样：排序前整块备份（按原始行序），落位后写回；删除重复保留第一次出现；
//! 分列把一格按分隔符拆到右边相邻的列，右边的原有内容会被推开。

use crate::model::{Cell, Sheet, Value};

/// 文本排序方式（Excel 的“排序依据”）
#[derive(Clone, Debug, PartialEq, Default)]
pub enum SortMode {
    /// 码位/字典序（旧行为）
    #[default]
    Default,
    /// 中文拼音序：走系统 zh-CN 排序表，和 Excel 在中文 Windows 上一致
    Pinyin,
    /// 自定义序列（高/中/低）：列表里靠前的排前面，不在列表里的按拼音排在后面
    Custom(Vec<String>),
}

/// 一个排序键
#[derive(Clone, Debug, PartialEq)]
pub struct SortKey {
    pub col: u32,
    pub asc: bool,
    /// true 时大小写不敏感
    pub ignore_case: bool,
    pub mode: SortMode,
}

impl SortKey {
    /// 文本列默认：大小写不敏感、字典序
    pub fn text(col: u32, asc: bool) -> Self {
        Self {
            col,
            asc,
            ignore_case: true,
            mode: SortMode::Default,
        }
    }
}

/// 比较两个格子（空值永远排最后）
fn cmp_cell(
    a: &Option<Cell>,
    b: &Option<Cell>,
    ignore_case: bool,
    mode: &SortMode,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let av = a.as_ref().map(|c| c.value.clone()).unwrap_or(Value::Empty);
    let bv = b.as_ref().map(|c| c.value.clone()).unwrap_or(Value::Empty);
    match (&av, &bv) {
        (Value::Empty, Value::Empty) => Ordering::Equal,
        (Value::Empty, _) => Ordering::Greater,
        (_, Value::Empty) => Ordering::Less,
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(_), _) => Ordering::Less,
        (_, Value::Number(_)) => Ordering::Greater,
        _ => {
            let (x, y) = (av.display(), bv.display());
            let (x, y) = if ignore_case {
                (x.to_lowercase(), y.to_lowercase())
            } else {
                (x, y)
            };
            match mode {
                SortMode::Default => x.cmp(&y),
                SortMode::Pinyin => crate::collate::cmp_pinyin(&x, &y),
                SortMode::Custom(list) => {
                    let rank = |v: &str| -> Option<usize> {
                        let t = v.trim();
                        list.iter().position(|it| it.trim().eq_ignore_ascii_case(t))
                    };
                    match (rank(&x), rank(&y)) {
                        (Some(i), Some(j)) => i.cmp(&j),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => crate::collate::cmp_pinyin(&x, &y),
                    }
                }
            }
        }
    }
}

/// 多级排序；返回实际移动的行数。调用方须重建依赖图并重算工作簿。
/// 不可排序的数组/合并区保持原样并返回 0；UI 可先调用 validate_sort_region 显示原因。
pub fn sort_multi(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    keys: &[SortKey],
    header: bool,
) -> usize {
    let (r1, c1, r2, c2) = region;
    let r1 = if header { r1 + 1 } else { r1 };
    if r2 <= r1 || keys.is_empty() {
        return 0;
    }
    if crate::sort_filter::validate_sort_region(sheet, (r1, c1, r2, c2)).is_err() {
        return 0;
    }
    // 整块备份（原样，含样式）
    let mut rows: Vec<(u32, Vec<(Cell, u16)>)> = Vec::new();
    for r in r1..=r2 {
        let mut line = Vec::new();
        for c in c1..=c2 {
            let cell = sheet.get(r, c).cloned().unwrap_or(Cell {
                value: Value::Empty,
                formula: None,
            });
            line.push((cell, sheet.style_of(r, c)));
        }
        rows.push((r, line));
    }
    let before: Vec<Vec<(Cell, u16)>> = rows.iter().map(|(_, l)| l.clone()).collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&i, &j| {
        for k in keys {
            let ci = (k.col - c1) as usize;
            if ci >= before[i].len() {
                continue;
            }
            let a = Some(before[i][ci].0.clone());
            let b = Some(before[j][ci].0.clone());
            let mut ord = cmp_cell(&a, &b, k.ignore_case, &k.mode);
            if !k.asc {
                ord = ord.reverse();
            }
            // 空值不参与反向：空永远最后
            let a_empty = matches!(before[i][ci].0.value, Value::Empty);
            let b_empty = matches!(before[j][ci].0.value, Value::Empty);
            if a_empty != b_empty {
                ord = if a_empty {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Less
                };
            }
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
    let mut moved = 0;
    for (target_r, &src_i) in order.iter().enumerate() {
        let r = r1 + target_r as u32;
        for (k, (cell, st)) in before[src_i].iter().enumerate() {
            let c = c1 + k as u32;
            sheet.set(r, c, crate::sort_filter::sorted_cell(cell, rows[src_i].0, r, &sheet.name));
            sheet.set_style(r, c, *st);
        }
        if order[target_r] != target_r {
            moved += 1;
        }
    }
    let _ = c2;
    moved
}

/// 删除重复行（按指定列判断），保留第一次出现；返回删除的行数
/// 只统计重复行数，不改表（去重预览）。
pub fn count_duplicates(
    sheet: &Sheet,
    region: (u32, u32, u32, u32),
    cols: &[u32],
    header: bool,
) -> usize {
    let (mut r1, c1, r2, c2) = region;
    if header && r2 > r1 {
        r1 += 1;
    }
    if r2 < r1 {
        return 0;
    }
    let use_cols: Vec<u32> = if cols.is_empty() {
        (c1..=c2).collect()
    } else {
        cols.to_vec()
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dup = 0usize;
    for r in r1..=r2 {
        let key: String = use_cols
            .iter()
            .map(|c| sheet.get(r, *c).map(|x| x.value.display()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\u{1}");
        if !seen.insert(key) {
            dup += 1;
        }
    }
    dup
}

pub fn remove_duplicates(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    cols: &[u32],
    header: bool,
) -> usize {
    let (r1, c1, r2, c2) = region;
    let r1 = if header && r2 > r1 { r1 + 1 } else { r1 };
    if r2 < r1 {
        return 0;
    }
    let use_cols: Vec<u32> = if cols.is_empty() {
        (c1..=c2).collect()
    } else {
        cols.to_vec()
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut keep: Vec<u32> = Vec::new();
    for r in r1..=r2 {
        let key: String = use_cols
            .iter()
            .map(|c| sheet.get(r, *c).map(|x| x.value.display()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\u{1}");
        if seen.insert(key) {
            keep.push(r);
        }
    }
    let removed = (r2 - r1 + 1) as usize - keep.len();
    if removed == 0 {
        return 0;
    }
    // 把保留的行依次搬到顶部，再清掉尾部
    let mut snapshot: Vec<Vec<(Cell, u16)>> = Vec::new();
    for r in keep.iter() {
        let mut line = Vec::new();
        for c in c1..=c2 {
            let cell = sheet.get(*r, c).cloned().unwrap_or(Cell {
                value: Value::Empty,
                formula: None,
            });
            line.push((cell, sheet.style_of(*r, c)));
        }
        snapshot.push(line);
    }
    for (i, line) in snapshot.iter().enumerate() {
        let r = r1 + i as u32;
        for (k, (cell, st)) in line.iter().enumerate() {
            let c = c1 + k as u32;
            sheet.set(r, c, cell.clone());
            sheet.set_style(r, c, *st);
        }
    }
    for r in r1 + snapshot.len() as u32..=r2 {
        for c in c1..=c2 {
            sheet.set(r, c, Cell { value: Value::Empty, formula: None });
        }
    }
    removed
}

/// 分列：按分隔符把每格拆到右边相邻列（右侧内容整体右移腾位置）
/// 分列选项（Excel“分列”向导里的那些开关）
#[derive(Clone, Debug)]
pub struct SplitOpts {
    pub delim: char,
    /// 连续分隔符当一个（“a,,b” 或 “a   b” 不产生空列）
    pub merge_consecutive: bool,
    /// 双引号包起来的字段里不拆，`""` 表示一个引号
    pub quote: bool,
    /// 识别数字/日期；前导零（00123）保留为文本
    pub detect_types: bool,
    pub date_system: crate::model::DateSystem,
    /// 固定宽度模式：每列占几个字符（按字符数，不按字节）；None = 按分隔符拆
    pub fixed_widths: Option<Vec<usize>>,
}

impl Default for SplitOpts {
    fn default() -> Self {
        Self {
            delim: ',',
            merge_consecutive: false,
            quote: true,
            detect_types: true,
            date_system: crate::model::DateSystem::D1900,
            fixed_widths: None,
        }
    }
}

/// 分列结果
#[derive(Clone, Debug, Default)]
pub struct SplitReport {
    /// 写出去的格子数
    pub written: usize,
    /// 会覆盖掉右边原有非空格子的个数（执行前用于提示）
    pub overwritten: usize,
    /// 拆出来的最大列数
    pub width: usize,
    /// 识别成日期的格子（调用方补日期格式）
    pub date_cells: Vec<(u32, u32)>,
}

/// 按选项拆一行文本
pub fn split_fields(line: &str, opts: &SplitOpts) -> Vec<String> {
    if let Some(widths) = &opts.fixed_widths {
        return split_fixed(line, widths);
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if opts.quote && ch == '"' {
            if in_quote && chars.peek() == Some(&'"') {
                cur.push('"');
                chars.next();
            } else {
                in_quote = !in_quote;
            }
            continue;
        }
        if !in_quote && ch == opts.delim {
            if opts.merge_consecutive && cur.trim().is_empty() {
                cur.clear();
                continue;
            }
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(ch);
    }
    out.push(cur);
    if opts.merge_consecutive {
        out.retain(|x| !x.trim().is_empty());
    }
    out
}

fn detect_value(text: &str, opts: &SplitOpts) -> (Value, bool) {
    let t = text.trim();
    if !opts.detect_types || t.is_empty() {
        return (Value::Text(text.to_string().into()), false);
    }
    // 前导零（00123、007）当文本，避免变成 123
    if t.len() > 1 && t.starts_with('0') && t.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return (Value::Text(text.to_string().into()), false);
    }
    if let Ok(n) = t.parse::<f64>() {
        return (Value::Number(n), false);
    }
    // 日期：2026-03-05 / 2026/3/5
    let norm = t.replace('/', "-");
    let parts: Vec<&str> = norm.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(m), Ok(d)) = (
            parts[0].parse::<i32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            if (1900..=9999).contains(&y) && parts[0].len() == 4 {
                if let Some(serial) = crate::formula::date_to_serial_in(opts.date_system, y, m, d) {
                    return (Value::Number(serial), true);
                }
            }
        }
    }
    (Value::Text(text.to_string().into()), false)
}

/// 分列：把区域每一行按分隔符拆开写回，右边原有的内容整体右移。
///
/// `dry_run` 为真时只统计会覆盖多少非空格子，不改表。
pub fn text_to_columns_ex(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    opts: &SplitOpts,
    dry_run: bool,
) -> SplitReport {
    let (r1, c1, r2, c2) = region;
    let mut report = SplitReport::default();
    let mut rows: Vec<(u32, Vec<String>)> = Vec::new();
    for r in r1..=r2 {
        let text = sheet
            .get(r, c1)
            .map(|x| x.value.display())
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let fields = split_fields(&text, opts);
        report.width = report.width.max(fields.len());
        rows.push((r, fields));
    }
    let shift = report.width.saturating_sub(1) as u32;
    if dry_run {
        for (r, fields) in &rows {
            for (i, _) in fields.iter().enumerate().skip(1) {
                let c = c1 + i as u32;
                let occupied = sheet
                    .get(*r, c)
                    .map(|x| !matches!(x.value, Value::Empty) && !x.value.display().is_empty())
                    .unwrap_or(false);
                if occupied {
                    report.overwritten += 1;
                }
            }
        }
        return report;
    }
    // 右侧被推开的列：整块只搬一次，从最右往左
    if shift > 0 {
        let last = sheet.used_bounds().map(|b| b.3).unwrap_or(c2).max(c2);
        for cc in (c1 + 1..=last).rev() {
            for rr in r1..=r2 {
                if let Some(cell) = sheet.get(rr, cc).cloned() {
                    sheet.set(rr, cc + shift, cell);
                    let st = sheet.style_of(rr, cc);
                    sheet.set_style(rr, cc + shift, st);
                }
            }
        }
    }
    for (r, fields) in &rows {
        for (i, f) in fields.iter().enumerate() {
            let c = c1 + i as u32;
            let (v, is_date) = detect_value(f, opts);
            if is_date {
                report.date_cells.push((*r, c));
            }
            sheet.set(*r, c, Cell { value: v, formula: None });
            report.written += 1;
        }
    }
    report
}

/// 兼容旧调用：单分隔符、不合并、不识别类型
pub fn text_to_columns(sheet: &mut Sheet, region: (u32, u32, u32, u32), delim: char) -> usize {
    let opts = SplitOpts {
        delim,
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        date_system: crate::model::DateSystem::D1900,
        fixed_widths: None,
    };
    text_to_columns_ex(sheet, region, &opts, false).width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with(rows: &[&[&str]]) -> Sheet {
        let mut s = Sheet::new("T");
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                s.set(
                    r as u32,
                    c as u32,
                    Cell { value: Value::parse_input(v), formula: None },
                );
            }
        }
        s
    }

    fn col(s: &Sheet, c: u32, r1: u32, r2: u32) -> Vec<String> {
        (r1..=r2)
            .map(|r| s.get(r, c).map(|x| x.value.display()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn multi_level_sort() {
        let mut s = sheet_with(&[
            &["部门", "姓名", "金额"],
            &["研发", "甲", "300"],
            &["销售", "乙", "100"],
            &["研发", "丙", "200"],
        ]);
        let moved = sort_multi(
            &mut s,
            (0, 0, 3, 2),
            &[
                SortKey::text(0, true),
                SortKey::text(2, false),
            ],
            true,
        );
        assert!(moved > 0);
        assert_eq!(col(&s, 0, 1, 3), vec!["研发", "研发", "销售"]);
        // 同部门内按金额降序
        assert_eq!(col(&s, 2, 1, 3), vec!["300", "200", "100"]);
    }

    #[test]
    fn sort_puts_empty_last() {
        let mut s = sheet_with(&[&["3"], &[""], &["1"]]);
        sort_multi(&mut s, (0, 0, 2, 0), &[SortKey::text(0, true)], false);
        assert_eq!(col(&s, 0, 0, 2), vec!["1", "3", ""]);
    }

    #[test]
    fn remove_duplicates_keeps_first() {
        let mut s = sheet_with(&[
            &["a", "1"],
            &["b", "2"],
            &["a", "1"],
            &["c", "3"],
            &["a", "1"],
        ]);
        let removed = remove_duplicates(&mut s, (0, 0, 4, 1), &[0, 1], false);
        assert_eq!(removed, 2);
        assert_eq!(col(&s, 0, 0, 2), vec!["a", "b", "c"]);
        assert_eq!(col(&s, 0, 3, 4), vec!["", ""]);
    }

    #[test]
    fn text_to_columns_splits_and_shifts() {
        let mut s = sheet_with(&[&["甲,100", "备注A"], &["乙,200", "备注B"]]);
        let n = text_to_columns(&mut s, (0, 0, 1, 1), ',');
        assert_eq!(n, 2);
        assert_eq!(col(&s, 0, 0, 1), vec!["甲", "乙"]);
        assert_eq!(col(&s, 1, 0, 1), vec!["100", "200"]);
        // 原来的备注被推到右边
        assert_eq!(col(&s, 2, 0, 1), vec!["备注A", "备注B"]);
    }

    fn col_text(s: &Sheet, c: u32, n: u32) -> Vec<String> {
        (0..n)
            .map(|r| s.get(r, c).map(|x| x.value.display()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn pinyin_sort_matches_chinese_windows_order() {
        let mut s = Sheet::new("S");
        for (i, n) in ["张三", "李四", "王五", "赵六", "陈七"].iter().enumerate() {
            s.set(i as u32, 0, Cell { value: Value::Text((*n).into()), formula: None });
        }
        let mut k = SortKey::text(0, true);
        k.mode = SortMode::Pinyin;
        sort_multi(&mut s, (0, 0, 4, 0), &[k], false);
        let got = col_text(&s, 0, 5);
        assert_eq!(got, vec!["陈七", "李四", "王五", "张三", "赵六"], "拼音序：{got:?}");
    }

    #[test]
    fn custom_sequence_orders_by_list_position() {
        let mut s = Sheet::new("S");
        for (i, n) in ["低", "高", "中", "未知", "高"].iter().enumerate() {
            s.set(i as u32, 0, Cell { value: Value::Text((*n).into()), formula: None });
        }
        let mut k = SortKey::text(0, true);
        k.mode = SortMode::Custom(vec!["高".into(), "中".into(), "低".into()]);
        sort_multi(&mut s, (0, 0, 4, 0), &[k], false);
        let got = col_text(&s, 0, 5);
        assert_eq!(got, vec!["高", "高", "中", "低", "未知"], "自定义序列：{got:?}");
    }


    #[test]
    fn split_respects_quotes_and_consecutive_delims() {
        let opts = SplitOpts { delim: ',', merge_consecutive: false, quote: true, ..Default::default() };
        assert_eq!(
            split_fields(r#"甲,"乙,丙",丁"#, &opts),
            vec!["甲", "乙,丙", "丁"]
        );
        assert_eq!(split_fields(r#""他说""好""#, &opts), vec![r#"他说"好"#]);
        let opts2 = SplitOpts { delim: ' ', merge_consecutive: true, quote: false, ..Default::default() };
        assert_eq!(split_fields("甲   乙  丙", &opts2), vec!["甲", "乙", "丙"]);
    }

    #[test]
    fn split_keeps_leading_zeros_and_detects_dates() {
        let mut s = Sheet::new("S");
        s.set(0, 0, Cell { value: Value::Text("00123".into()), formula: None });
        s.set(1, 0, Cell { value: Value::Text("2026-03-05".into()), formula: None });
        s.set(2, 0, Cell { value: Value::Text("12.50".into()), formula: None });
        let opts = SplitOpts::default();
        let rep = text_to_columns_ex(&mut s, (0, 0, 2, 0), &opts, false);
        match &s.get(0, 0).unwrap().value {
            Value::Text(t) => assert_eq!(t.as_ref(), "00123", "前导零必须保留为文本"),
            v => panic!("00123 不应变成数字：{v:?}"),
        }
        match &s.get(1, 0).unwrap().value {
            Value::Number(n) => assert!((*n - 46086.0).abs() < 0.001, "2026-03-05 的序列值：{n}"),
            v => panic!("日期应识别为日期序列：{v:?}"),
        }
        assert_eq!(rep.date_cells, vec![(1, 0)]);
        assert!(matches!(s.get(2, 0).unwrap().value, Value::Number(_)));
    }

    #[test]
    fn split_warns_before_overwriting_non_empty_cells() {
        let mut s = Sheet::new("S");
        s.set(0, 0, Cell { value: Value::Text("甲,乙".into()), formula: None });
        s.set(0, 1, Cell { value: Value::Text("原有".into()), formula: None });
        s.set(0, 2, Cell { value: Value::Text("也有".into()), formula: None });
        let opts = SplitOpts::default();
        let dry = text_to_columns_ex(&mut s, (0, 0, 0, 0), &opts, true);
        assert_eq!(dry.overwritten, 1, "只有紧邻的一格被覆盖，后面的整体右移");
        assert_eq!(dry.written, 0);
        assert_eq!(s.get(0, 1).unwrap().value.display(), "原有", "试算不能改表");
        let rep = text_to_columns_ex(&mut s, (0, 0, 0, 0), &opts, false);
        assert_eq!(rep.width, 2);
        assert_eq!(s.get(0, 1).unwrap().value.display(), "乙");
        assert_eq!(s.get(0, 2).unwrap().value.display(), "原有", "原有内容右移一列");
        assert_eq!(s.get(0, 3).unwrap().value.display(), "也有");
    }

}

/// 固定宽度分列：按字符数切（Excel 的「固定宽度」也是按显示宽度，这里按字符数，
/// 中日韩全角字符一个算一个，够用且可预测）。空段保留成空串，行为与 Excel 一致。
pub fn split_fixed(line: &str, widths: &[usize]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut at = 0usize;
    for &w in widths {
        if w == 0 {
            continue;
        }
        let end = (at + w).min(chars.len());
        out.push(chars[at..end].iter().collect::<String>());
        at = end;
        if at >= chars.len() {
            break;
        }
    }
    // Excel 里最后一列的右边界是「文本末尾」，断点之后的剩余部分留在最后一列
    if at < chars.len() {
        let tail: String = chars[at..].iter().collect();
        match out.last_mut() {
            Some(last) => last.push_str(&tail),
            None => out.push(tail),
        }
    }
    out
}

/// 「10,20,30」→ [10,20,30]；有任何非正数或非数字就返回 None。
pub fn parse_fixed_widths(s: &str) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    for part in s.split([',', '，', ' ']).filter(|p| !p.trim().is_empty()) {
        let v: usize = part.trim().parse().ok()?;
        if v == 0 {
            return None;
        }
        out.push(v);
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod fixed_width_tests {
    use super::*;

    #[test]
    fn fixed_width_splits_by_char_count() {
        // 断点之后的剩余部分并入最后一列（Excel 的最后一列右边界到文本末尾）
        assert_eq!(split_fixed("ABCDEFG", &[3, 2]), vec!["ABC", "DEFG"]);
        assert_eq!(split_fixed("甲1乙2丙", &[2, 2]), vec!["甲1", "乙2丙"]);
        assert_eq!(split_fixed("AB", &[4, 4]), vec!["AB"], "短于宽度时不补空格");
        assert_eq!(split_fixed("", &[3]), vec![""]);
    }

    #[test]
    fn fixed_width_parse_rejects_zero_and_garbage() {
        assert_eq!(parse_fixed_widths("10,20,15"), Some(vec![10, 20, 15]));
        assert_eq!(parse_fixed_widths(" 10 ， 20 "), Some(vec![10, 20]));
        assert_eq!(parse_fixed_widths("10,0"), None);
        assert_eq!(parse_fixed_widths("10,x"), None);
        assert_eq!(parse_fixed_widths(""), None);
    }

    #[test]
    fn fixed_width_mode_bypasses_delimiters() {
        let opts = SplitOpts {
            fixed_widths: Some(vec![3, 3]),
            ..Default::default()
        };
        assert_eq!(split_fields("AB,CD,EF", &opts), vec!["AB,", "CD,EF"]);
    }
}
