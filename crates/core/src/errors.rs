//! 错误检查：扫描表里的错误值（公式算出来的 #DIV/0! 之类）和「数字存为文本」。
//!
//! 单元格里的错误在模型层就是一段文本（`Value::Text("#DIV/0!")`），所以这里按文本识别。

use crate::model::{Sheet, Value};

/// 错误类型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    Div0,
    Value,
    Ref,
    Name,
    Num,
    Na,
    Null,
    Spill,
    NumberAsText,
}

impl IssueKind {
    /// 单元格里显示的文本（NumberAsText 没有错误文本，返回空）
    pub fn error_text(&self) -> &'static str {
        match self {
            IssueKind::Div0 => "#DIV/0!",
            IssueKind::Value => "#VALUE!",
            IssueKind::Ref => "#REF!",
            IssueKind::Name => "#NAME?",
            IssueKind::Num => "#NUM!",
            IssueKind::Na => "#N/A",
            IssueKind::Null => "#NULL!",
            IssueKind::Spill => "#SPILL!",
            IssueKind::NumberAsText => "",
        }
    }

    /// 中文说明（错误检查对话框里那一列）
    pub fn detail(&self) -> &'static str {
        match self {
            IssueKind::Div0 => "除以零：除数是 0 或空单元格",
            IssueKind::Value => "值错误：运算数的类型不对",
            IssueKind::Ref => "引用无效：公式引用的单元格被删除",
            IssueKind::Name => "名称错误：函数名或区域名认不出来",
            IssueKind::Num => "数字错误：数值运算的结果无效",
            IssueKind::Na => "找不到匹配值：查找函数没有命中",
            IssueKind::Null => "空交集：两个区域没有公共单元格",
            IssueKind::Spill => "溢出区域被占用",
            IssueKind::NumberAsText => "数字存为文本：这个单元格里是文本，但看起来像数字",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub row: u32,
    pub col: u32,
    pub kind: IssueKind,
    /// 单元格当前文本（对话框里显示的「单元格内容」）
    pub text: String,
}

/// 把错误文本认成 IssueKind
pub fn error_kind(s: &str) -> Option<IssueKind> {
    // 注意：模型里的错误是 calamine 的 Debug 简写（`#Div0`/`#NA`/`#Name`…），
    // 不是 Excel 的显示文本；两种都要认，否则扫不到任何错误。
    match s.trim() {
        "#DIV/0!" | "#Div0" => Some(IssueKind::Div0),
        "#VALUE!" | "#Value" => Some(IssueKind::Value),
        "#REF!" | "#Ref" => Some(IssueKind::Ref),
        "#NAME?" | "#Name" => Some(IssueKind::Name),
        "#NUM!" | "#Num" => Some(IssueKind::Num),
        "#N/A" | "#NA" | "#Na" => Some(IssueKind::Na),
        "#NULL!" | "#Null" => Some(IssueKind::Null),
        "#SPILL!" | "#Spill" => Some(IssueKind::Spill),
        _ => None,
    }
}

/// 「看起来是数字的文本」：纯数字（可带正负号、小数、科学计数、千分位、百分号）
pub fn looks_numeric(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    // 纯数字最省事
    if t.parse::<f64>().is_ok() {
        return true;
    }
    // 千分位 / 百分号 / 前后空格
    let core = t.replace(',', "");
    let core = core.strip_suffix('%').unwrap_or(&core).trim();
    if core.parse::<f64>().is_ok() {
        return true;
    }
    // ¥ $ € £ 前缀
    let core = core
        .trim_start_matches(['¥', '$', '€', '£', '￥'])
        .trim();
    core.parse::<f64>().is_ok()
}

/// 扫一张表：返回按行列排好序的问题清单
pub fn scan(sheet: &Sheet) -> Vec<Issue> {
    let mut out = Vec::new();
    for (cref, cell) in sheet.iter() {
        if sheet.ignored_errors.contains(&(cref.row, cref.col)) {
            continue;
        }
        let Value::Text(t) = &cell.value else {
            continue;
        };
        if let Some(kind) = error_kind(t) {
            out.push(Issue {
                row: cref.row,
                col: cref.col,
                kind,
                text: kind.error_text().to_string(),
            });
        } else if looks_numeric(t) {
            out.push(Issue {
                row: cref.row,
                col: cref.col,
                kind: IssueKind::NumberAsText,
                text: t.to_string(),
            });
        }
    }
    out.sort_by_key(|i| (i.row, i.col));
    out
}

/// 带工作簿的扫描：除了错误值，还能认出「单元格格式是文本(@)、值是数字」的格子
/// （我们的加载器会把看起来像数字的文本直接转成数字，所以只能靠格式判断）。
pub fn scan_wb(wb: &crate::model::Workbook, si: usize) -> Vec<Issue> {
    let mut out = Vec::new();
    visit_issues(wb, si, &std::sync::atomic::AtomicBool::new(false), |key, cell, kind| {
        let text = if kind == IssueKind::NumberAsText { cell.value().display() } else { kind.error_text().into() };
        out.push(Issue { row: key.row, col: key.col, kind, text });
    });
    out.sort_by_key(|i| (i.row, i.col));
    out
}

/// Background marks only need coordinates; avoid a sortable list of display strings.
pub fn mark_positions(wb: &crate::Workbook, si: usize, check_text: bool, check_errors: bool,
    stop: &std::sync::atomic::AtomicBool) -> std::collections::HashSet<(u32,u32)> {
    let mut out = std::collections::HashSet::new();
    visit_issues(wb, si, stop, |key, _, kind| {
        let enabled = if kind == IssueKind::NumberAsText { check_text } else { check_errors };
        if enabled {
            out.insert((key.row, key.col));
        }
    });
    out
}

fn visit_issues(wb: &crate::Workbook, si:usize, stop:&std::sync::atomic::AtomicBool,
    mut visit: impl FnMut(crate::CellRef, crate::store::CellView<'_>, IssueKind)) {
    let sheet = wb.sheet(si);
    for (i, (key, cell)) in sheet.stored_cells().enumerate() {
        if i % 256 == 0 && stop.load(std::sync::atomic::Ordering::Relaxed) { return; }
        if sheet.ignored_errors.contains(&(key.row, key.col)) { continue; }
        let kind = match cell.value().as_ref() {
            Value::Text(t) => error_kind(t),
            Value::Number(_) if wb.styles.get(sheet.style_of(key.row,key.col)).format.trim() == "@" => Some(IssueKind::NumberAsText),
            _ => None,
        };
        if let Some(kind) = kind { visit(key,cell,kind); }
    }
}

/// 找循环引用：在「公式格 → 它引用的公式格」这张图上做 DFS，遇到回边就把环上的格子标出来。
/// 公式太多（> 20000）时直接返回空，避免每次编辑都全量解析。
pub fn find_cycles(wb: &crate::model::Workbook, si: usize) -> Vec<(u32, u32)> {
    find_cycles_cancellable(wb, si, &std::sync::atomic::AtomicBool::new(false))
}

/// A cancelled automatic scan discards all partial results.
pub fn find_cycles_cancellable(wb: &crate::Workbook, si: usize, stop: &std::sync::atomic::AtomicBool) -> Vec<(u32,u32)> {
    use std::sync::atomic::Ordering;
    use std::collections::HashMap;
    let sheet = wb.sheet(si);
    let mut formulas: Vec<(u32, u32)> = sheet
        .formula_cells()
        .take(20_001)
        .map(|(k, _)| (k.row, k.col))
        .collect();
    if formulas.is_empty() || formulas.len() > 20_000 {
        return Vec::new();
    }
    formulas.sort_unstable();
    let idx: HashMap<(u32, u32), usize> = formulas
        .iter()
        .enumerate()
        .map(|(i, k)| (*k, i))
        .collect();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); formulas.len()];
    for (i, (r, c)) in formulas.iter().enumerate() {
        if stop.load(Ordering::Relaxed) { return Vec::new(); }
        let Some(f) = sheet.get(*r, *c).and_then(|x| x.formula.clone()) else {
            continue;
        };
        for rect in crate::formula::deps::refs_of(wb, si, &f) {
            if stop.load(Ordering::Relaxed) { return Vec::new(); }
            if rect.sheet != si {
                continue;
            }
            let (r1, c1) = (rect.r1.max(0) as u32, rect.c1.max(0) as u32);
            let (r2, c2) = (rect.r2.max(0) as u32, rect.c2.max(0) as u32);
            let area = (r2.saturating_sub(r1) as u64 + 1) * (c2.saturating_sub(c1) as u64 + 1);
            if area <= 64 {
                for rr in r1..=r2 {
                    for cc in c1..=c2 {
                        if let Some(j) = idx.get(&(rr, cc)) {
                            adj[i].push(*j);
                        }
                    }
                }
            } else {
                // 大范围或整列引用：按公式列表测试矩形包含（O(formulas)），避免 20000 × 100000 = 20 亿次内层循环卡死主线程
                for (j, &(fr, fc)) in formulas.iter().enumerate() {
                    if fr >= r1 && fr <= r2 && fc >= c1 && fc <= c2 {
                        adj[i].push(j);
                    }
                }
            }
        }
    }
    let n = formulas.len();
    let mut color = vec![0u8; n]; // 0 未访问 / 1 在栈上 / 2 已完成
    let mut cyc = vec![false; n];
    for s0 in 0..n {
        if color[s0] != 0 {
            continue;
        }
        color[s0] = 1;
        let mut stack: Vec<usize> = vec![s0];
        let mut next: Vec<usize> = vec![0];
        while let Some(&u) = stack.last() {
            if stop.load(Ordering::Relaxed) { return Vec::new(); }
            let k = *next.last().unwrap();
            if k < adj[u].len() {
                *next.last_mut().unwrap() += 1;
                let v = adj[u][k];
                if color[v] == 1 {
                    if let Some(pos) = stack.iter().position(|x| *x == v) {
                        for &w in &stack[pos..] {
                            cyc[w] = true;
                        }
                    }
                } else if color[v] == 0 {
                    color[v] = 1;
                    stack.push(v);
                    next.push(0);
                }
            } else {
                color[u] = 2;
                stack.pop();
                next.pop();
            }
        }
    }
    let mut out: Vec<(u32, u32)> = formulas
        .iter()
        .enumerate()
        .filter(|(i, _)| cyc[*i])
        .map(|(_, k)| *k)
        .collect();
    out.sort();
    out
}

/// 只扫错误值（不含「数字存为文本」）——画绿色小三角时用
pub fn scan_errors(sheet: &Sheet) -> Vec<Issue> {
    scan(sheet)
        .into_iter()
        .filter(|i| i.kind != IssueKind::NumberAsText)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Sheet};

    fn sheet_with(cells: &[(u32, u32, &str)]) -> Sheet {
        let mut s = Sheet::new("S");
        for (r, c, t) in cells {
            s.set(
                *r,
                *c,
                Cell {
                    value: Value::Text((*t).into()),
                    formula: None,
                },
            );
        }
        s
    }

    #[test]
    fn finds_error_texts() {
        let s = sheet_with(&[(0, 0, "#DIV/0!"), (1, 1, "#N/A"), (2, 2, "正常")]);
        let got = scan(&s);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].kind, IssueKind::Div0);
        assert_eq!(got[1].kind, IssueKind::Na);
    }

    #[test]
    fn finds_number_as_text() {
        let s = sheet_with(&[(0, 0, "123"), (1, 0, "1,200.5"), (2, 0, "12%"), (3, 0, "abc")]);
        let got = scan(&s)
            .into_iter()
            .filter(|i| i.kind == IssueKind::NumberAsText)
            .count();
        assert_eq!(got, 3);
    }

    #[test]
    fn finds_circular_references() {
        let mut s = Sheet::new("S");
        let mut wb = crate::model::Workbook::default();
        wb.sheets.push(s.clone());
        s.set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("A1+1".into()),
            },
        );
        s.set(
            0,
            1,
            Cell {
                value: Value::Empty,
                formula: Some("C1".into()),
            },
        );
        s.set(
            0,
            2,
            Cell {
                value: Value::Empty,
                formula: Some("B1".into()),
            },
        );
        wb.sheets[0] = s;
        let got = find_cycles(&wb, 0);
        assert_eq!(got, vec![(0, 0), (0, 1), (0, 2)], "{got:?}");
    }

    #[test]
    fn ignored_cells_are_skipped() {
        let mut s = sheet_with(&[(0, 0, "#DIV/0!"), (1, 0, "#N/A")]);
        s.ignored_errors.insert((0, 0));
        let got = scan(&s);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].row, 1);
    }

    #[test]
    fn numeric_check_rejects_noise() {
        assert!(!looks_numeric(""));
        assert!(!looks_numeric("N/A"));
        assert!(!looks_numeric("1-2"));
        assert!(looks_numeric("-3.5e2"));
        assert!(looks_numeric("¥ 88"));
    }
}
