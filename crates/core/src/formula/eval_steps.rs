//! 公式求值（Evaluate Formula）：逐步拆解并高亮展示下一次求值的子表达式，支持单步求值、步入引用与重新开始。

use crate::formula::{eval_formula, from_fvalue, parse, FValue, Node, RefExpr};
use crate::model::{cell_name, Value, Workbook};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalStep {
    /// 当前待求值公式的完整展示文本（含前导 `=` 或最终值）
    pub expr: String,
    /// 下一步将被求值子表达式在 `expr` 中的字节区间 `(start, end)`（下划线高亮）
    pub highlight: Option<(usize, usize)>,
    /// 若当前高亮子表达式是可直接步入的公式单元格引用，给出其 `(sheet, row, col)`
    pub step_in_target: Option<(usize, u32, u32)>,
    /// 是否已求值到最终结果
    pub done: bool,
}

#[derive(Clone, Debug)]
struct Frame {
    sheet: usize,
    row: u32,
    col: u32,
    original_formula: String,
    node: Node,
    done: bool,
}

/// 交互式“公式求值”状态机（支持嵌套步入/步出引用公式）。
#[derive(Clone, Debug)]
pub struct FormulaEvaluator {
    frames: Vec<Frame>,
}

impl FormulaEvaluator {
    pub fn new(wb: &Workbook, sheet: usize, row: u32, col: u32, formula: &str) -> Self {
        let raw = formula.trim();
        let body = raw.strip_prefix('=').unwrap_or(raw);
        let expanded = crate::table::expand_structured(wb, sheet, body);
        let src = expanded.as_deref().unwrap_or(body);
        let (node, done) = match parse(src) {
            Ok(n) => {
                let is_lit = is_terminal(&n);
                (n, is_lit)
            }
            Err(e) => (Node::Err(e), true),
        };
        Self {
            frames: vec![Frame {
                sheet,
                row,
                col,
                original_formula: if raw.starts_with('=') {
                    raw.to_string()
                } else {
                    format!("={raw}")
                },
                node,
                done,
            }],
        }
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    pub fn active_cell(&self) -> (usize, u32, u32) {
        let f = self.frames.last().expect("evaluator has at least one frame");
        (f.sheet, f.row, f.col)
    }

    pub fn restart(&mut self, wb: &Workbook) {
        let root = self.frames[0].clone();
        *self = Self::new(wb, root.sheet, root.row, root.col, &root.original_formula);
    }

    /// 返回当前步骤的展示文本、下划线高亮范围及是否可步入。
    pub fn current(&self, wb: &Workbook) -> EvalStep {
        let frame = self.frames.last().expect("frame");
        if frame.done || is_terminal(&frame.node) {
            return EvalStep {
                expr: render_terminal(&frame.node),
                highlight: None,
                step_in_target: None,
                done: true,
            };
        }
        let target_path = find_next_redex(&frame.node, wb, frame.sheet);
        let mut out = String::from("=");
        let mut highlight = None;
        let mut step_in_target = None;
        render_node(
            &frame.node,
            &[],
            target_path.as_deref(),
            wb,
            frame.sheet,
            &mut out,
            &mut highlight,
            &mut step_in_target,
        );
        EvalStep {
            expr: out,
            highlight,
            step_in_target,
            done: false,
        }
    }

    /// 单步求值当前高亮的子表达式。返回 `true` 表示有推进。
    pub fn step(&mut self, wb: &Workbook) -> bool {
        let frame = self.frames.last_mut().expect("frame");
        if frame.done || is_terminal(&frame.node) {
            frame.done = true;
            return false;
        }
        let Some(path) = find_next_redex(&frame.node, wb, frame.sheet) else {
            // Fallback: evaluate whole node directly
            let s = format!("={}", render_plain(&frame.node));
            let val = eval_formula(wb, frame.sheet, &s);
            frame.node = fvalue_to_node(&val);
            frame.done = true;
            return true;
        };
        let sheet = frame.sheet;
        replace_at_path(&mut frame.node, &path, wb, sheet);
        if is_terminal(&frame.node) {
            frame.done = true;
        }
        true
    }

    /// 若当前高亮节点是含公式的单元格引用，则步入该单元格公式。
    pub fn step_in(&mut self, wb: &Workbook) -> bool {
        let cur = self.current(wb);
        let Some((si, r, c)) = cur.step_in_target else {
            return false;
        };
        let Some(f) = wb
            .sheets
            .get(si)
            .and_then(|s| s.formula_arc_of(r, c))
        else {
            return false;
        };
        let sub = Self::new(wb, si, r, c, &f);
        if let Some(frame) = sub.frames.into_iter().next() {
            self.frames.push(frame);
            return true;
        }
        false
    }

    /// 步出当前子帧，将结果写回父帧的当前高亮引用。
    pub fn step_out(&mut self, wb: &Workbook) -> bool {
        if self.frames.len() <= 1 {
            return false;
        }
        self.frames.pop();
        self.step(wb);
        true
    }
}

fn is_terminal(node: &Node) -> bool {
    matches!(
        node,
        Node::Empty | Node::Number(_) | Node::Text(_) | Node::Bool(_) | Node::Err(_) | Node::Array(..)
    )
}

fn fvalue_to_node(v: &FValue) -> Node {
    match v {
        FValue::Empty => Node::Empty,
        FValue::Number(n) => Node::Number(*n),
        FValue::Text(s) => Node::Text(s.clone()),
        FValue::Bool(b) => Node::Bool(*b),
        FValue::Error(e) => Node::Err(*e),
        FValue::Array(a) => {
            if a.rows == 1 && a.cols == 1 {
                fvalue_to_node(&a.data[0])
            } else {
                let vals: Vec<Value> = a.data.iter().map(from_fvalue).collect();
                Node::Array(a.rows, a.cols, vals)
            }
        }
        FValue::Lambda(_) => Node::Text("#LAMBDA".into()),
    }
}

fn render_terminal(node: &Node) -> String {
    match node {
        Node::Empty => "0".into(),
        Node::Number(n) => Value::Number(*n).display(),
        Node::Text(s) => format!("\"{}\"", s.replace('"', "\"\"")),
        Node::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        Node::Err(e) => e.text().into(),
        Node::Array(rows, cols, vals) => {
            let mut s = String::from("{");
            for r in 0..*rows {
                if r > 0 {
                    s.push(';');
                }
                for c in 0..*cols {
                    if c > 0 {
                        s.push(',');
                    }
                    if let Some(v) = vals.get(r * *cols + c) {
                        match v {
                            Value::Text(t) => s.push_str(&format!("\"{}\"", t.replace('"', "\"\""))),
                            _ => s.push_str(&v.display()),
                        }
                    }
                }
            }
            s.push('}');
            s
        }
        _ => render_plain(node),
    }
}

fn render_ref(r: &RefExpr) -> String {
    let mut s = String::new();
    if let Some(sh) = &r.sheet {
        if let Some(sh2) = &r.sheet2 {
            s.push_str(&format!("'{sh}:{sh2}'!"));
        } else if sh.chars().any(|c| !c.is_alphanumeric() && c != '_') {
            s.push_str(&format!("'{}'!", sh.replace('\'', "''")));
        } else {
            s.push_str(sh);
            s.push('!');
        }
    }
    if r.col_abs {
        s.push('$');
    }
    s.push_str(&crate::model::col_label(r.col.max(0) as u32));
    if r.row_abs {
        s.push('$');
    }
    s.push_str(&(r.row.max(0) + 1).to_string());
    s
}

fn render_plain(node: &Node) -> String {
    match node {
        Node::Empty => String::new(),
        Node::Number(n) => Value::Number(*n).display(),
        Node::Text(s) => format!("\"{}\"", s.replace('"', "\"\"")),
        Node::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        Node::Err(e) => e.text().into(),
        Node::Array(..) => render_terminal(node),
        Node::Ref(r) => render_ref(r),
        Node::Spill(r) => format!("{}#", render_ref(r)),
        Node::Range(a, b) => format!("{}:{}", render_ref(a), render_ref(b)),
        Node::Name(n) => n.clone(),
        Node::ScopedName { book, sheet, name } => {
            let mut out = String::new();
            if let Some(b) = book {
                out.push('[');
                out.push_str(b);
                out.push(']');
            }
            if let Some(s) = sheet {
                out.push_str(s);
            }
            out.push('!');
            out.push_str(name);
            out
        }
        Node::Implicit(x) => format!("@{}", render_plain(x)),
        Node::Unary(neg, x) => {
            if *neg {
                format!("-{}", render_plain(x))
            } else {
                format!("+{}", render_plain(x))
            }
        }
        Node::Percent(x) => format!("{}%", render_plain(x)),
        Node::Binary(op, l, r) => {
            let op_s = match op {
                crate::formula::BinOp::Add => "+",
                crate::formula::BinOp::Sub => "-",
                crate::formula::BinOp::Mul => "*",
                crate::formula::BinOp::Div => "/",
                crate::formula::BinOp::Pow => "^",
                crate::formula::BinOp::Concat => "&",
                crate::formula::BinOp::Eq => "=",
                crate::formula::BinOp::Ne => "<>",
                crate::formula::BinOp::Lt => "<",
                crate::formula::BinOp::Le => "<=",
                crate::formula::BinOp::Gt => ">",
                crate::formula::BinOp::Ge => ">=",
            };
            format!("({}{op_s}{})", render_plain(l), render_plain(r))
        }
        Node::Func(name, args) => {
            let mut s = format!("{}(", name.as_str());
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&render_plain(a));
            }
            s.push(')');
            s
        }
    }
}

fn resolve_sheet(wb: &Workbook, name: Option<&str>, cur: usize) -> Option<usize> {
    match name {
        None => Some(cur),
        Some(n) => wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(n)),
    }
}

/// Find the leftmost-innermost evaluable sub-expression (respecting `IF` short-circuiting).
fn find_next_redex(node: &Node, wb: &Workbook, sheet: usize) -> Option<Vec<usize>> {
    let mut path = Vec::new();
    if find_redex_inner(node, wb, sheet, &mut path) {
        Some(path)
    } else {
        None
    }
}

fn find_redex_inner(node: &Node, wb: &Workbook, sheet: usize, path: &mut Vec<usize>) -> bool {
    match node {
        Node::Empty | Node::Number(_) | Node::Text(_) | Node::Bool(_) | Node::Err(_) | Node::Array(..) => {
            false
        }
        Node::Ref(_) | Node::Spill(_) | Node::Range(..) | Node::Name(_) | Node::ScopedName { .. } => {
            true
        }
        Node::Implicit(x) | Node::Unary(_, x) | Node::Percent(x) => {
            path.push(0);
            if find_redex_inner(x, wb, sheet, path) {
                return true;
            }
            path.pop();
            true
        }
        Node::Binary(_, l, r) => {
            path.push(0);
            if find_redex_inner(l, wb, sheet, path) {
                return true;
            }
            path.pop();
            path.push(1);
            if find_redex_inner(r, wb, sheet, path) {
                return true;
            }
            path.pop();
            true
        }
        Node::Func(name, args) => {
            let upper = name.as_str().to_ascii_uppercase();
            // Functions that take raw range/reference args evaluate as a whole once non-range args are ready.
            let ref_preserving = matches!(
                upper.as_str(),
                "SUM"
                    | "AVERAGE"
                    | "COUNT"
                    | "COUNTA"
                    | "MIN"
                    | "MAX"
                    | "SUMIF"
                    | "SUMIFS"
                    | "COUNTIF"
                    | "COUNTIFS"
                    | "VLOOKUP"
                    | "HLOOKUP"
                    | "XLOOKUP"
                    | "INDEX"
                    | "MATCH"
                    | "SUBTOTAL"
                    | "AGGREGATE"
                    | "OFFSET"
                    | "ROW"
                    | "COLUMN"
                    | "ROWS"
                    | "COLUMNS"
                    | "ISREF"
            );
            if upper == "IF" && !args.is_empty() {
                path.push(0);
                if find_redex_inner(&args[0], wb, sheet, path) {
                    return true;
                }
                path.pop();
                // Condition is now literal: only descend into the taken branch!
                let cond_true = match &args[0] {
                    Node::Bool(b) => *b,
                    Node::Number(n) => *n != 0.0,
                    _ => {
                        let s = format!("={}", render_plain(&args[0]));
                        matches!(from_fvalue(&eval_formula(wb, sheet, &s)), Value::Bool(true))
                    }
                };
                let branch_idx = if cond_true { 1 } else { 2 };
                if let Some(branch) = args.get(branch_idx) {
                    path.push(branch_idx);
                    if find_redex_inner(branch, wb, sheet, path) {
                        return true;
                    }
                    path.pop();
                }
                return true;
            }
            for (i, a) in args.iter().enumerate() {
                if ref_preserving && matches!(a, Node::Range(..) | Node::Spill(_)) {
                    continue;
                }
                if matches!(upper.as_str(), "ROW" | "COLUMN" | "ROWS" | "COLUMNS" | "OFFSET" | "ISREF")
                    && i == 0
                    && matches!(a, Node::Ref(_))
                {
                    continue;
                }
                path.push(i);
                if find_redex_inner(a, wb, sheet, path) {
                    return true;
                }
                path.pop();
            }
            true
        }
    }
}

fn replace_at_path(node: &mut Node, path: &[usize], wb: &Workbook, sheet: usize) {
    if path.is_empty() {
        let s = format!("={}", render_plain(node));
        let val = eval_formula(wb, sheet, &s);
        *node = fvalue_to_node(&val);
        return;
    }
    let idx = path[0];
    let rest = &path[1..];
    match node {
        Node::Implicit(x) | Node::Unary(_, x) | Node::Percent(x) => {
            replace_at_path(x, rest, wb, sheet);
        }
        Node::Binary(_, l, r) => {
            if idx == 0 {
                replace_at_path(l, rest, wb, sheet);
            } else {
                replace_at_path(r, rest, wb, sheet);
            }
        }
        Node::Func(_, args) => {
            if let Some(a) = args.get_mut(idx) {
                replace_at_path(a, rest, wb, sheet);
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    node: &Node,
    cur_path: &[usize],
    target_path: Option<&[usize]>,
    wb: &Workbook,
    sheet: usize,
    out: &mut String,
    highlight: &mut Option<(usize, usize)>,
    step_in_target: &mut Option<(usize, u32, u32)>,
) {
    let is_target = target_path == Some(cur_path);
    let start = out.len();
    match node {
        Node::Empty | Node::Number(_) | Node::Text(_) | Node::Bool(_) | Node::Err(_) | Node::Array(..) => {
            out.push_str(&render_terminal(node));
        }
        Node::Ref(r) => {
            out.push_str(&render_ref(r));
            if is_target && r.book.is_none() && r.sheet2.is_none() {
                if let Some(si) = resolve_sheet(wb, r.sheet.as_deref(), sheet) {
                    let row = r.row.max(0) as u32;
                    let col = r.col.max(0) as u32;
                    if wb.sheets.get(si).and_then(|s| s.formula_arc_of(row, col)).is_some() {
                        *step_in_target = Some((si, row, col));
                    }
                }
            }
        }
        Node::Spill(r) => {
            out.push_str(&render_ref(r));
            out.push('#');
        }
        Node::Range(a, b) => {
            out.push_str(&render_ref(a));
            out.push(':');
            out.push_str(&render_ref(b));
        }
        Node::Name(n) => out.push_str(n),
        Node::ScopedName { book, sheet: s, name } => {
            if let Some(b) = book {
                out.push('[');
                out.push_str(b);
                out.push(']');
            }
            if let Some(sh) = s {
                out.push_str(sh);
            }
            out.push('!');
            out.push_str(name);
        }
        Node::Implicit(x) => {
            out.push('@');
            let mut p = cur_path.to_vec();
            p.push(0);
            render_node(x, &p, target_path, wb, sheet, out, highlight, step_in_target);
        }
        Node::Unary(neg, x) => {
            out.push(if *neg { '-' } else { '+' });
            let mut p = cur_path.to_vec();
            p.push(0);
            render_node(x, &p, target_path, wb, sheet, out, highlight, step_in_target);
        }
        Node::Percent(x) => {
            let mut p = cur_path.to_vec();
            p.push(0);
            render_node(x, &p, target_path, wb, sheet, out, highlight, step_in_target);
            out.push('%');
        }
        Node::Binary(op, l, r) => {
            let op_s = match op {
                crate::formula::BinOp::Add => "+",
                crate::formula::BinOp::Sub => "-",
                crate::formula::BinOp::Mul => "*",
                crate::formula::BinOp::Div => "/",
                crate::formula::BinOp::Pow => "^",
                crate::formula::BinOp::Concat => "&",
                crate::formula::BinOp::Eq => "=",
                crate::formula::BinOp::Ne => "<>",
                crate::formula::BinOp::Lt => "<",
                crate::formula::BinOp::Le => "<=",
                crate::formula::BinOp::Gt => ">",
                crate::formula::BinOp::Ge => ">=",
            };
            let need_paren = !cur_path.is_empty();
            if need_paren {
                out.push('(');
            }
            let mut pl = cur_path.to_vec();
            pl.push(0);
            render_node(l, &pl, target_path, wb, sheet, out, highlight, step_in_target);
            out.push_str(op_s);
            let mut pr = cur_path.to_vec();
            pr.push(1);
            render_node(r, &pr, target_path, wb, sheet, out, highlight, step_in_target);
            if need_paren {
                out.push(')');
            }
        }
        Node::Func(name, args) => {
            out.push_str(name.as_str());
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let mut pa = cur_path.to_vec();
                pa.push(i);
                render_node(a, &pa, target_path, wb, sheet, out, highlight, step_in_target);
            }
            out.push(')');
        }
    }
    if is_target {
        *highlight = Some((start, out.len()));
    }
}

/// Format an active cell label for the Evaluate Formula dialog header (e.g. `Sheet1!C5`).
pub fn eval_cell_label(wb: &Workbook, sheet: usize, row: u32, col: u32) -> String {
    let sname = wb.sheets.get(sheet).map(|s| s.name.as_str()).unwrap_or("Sheet1");
    format!("{sname}!{}", cell_name(row, col))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    #[test]
    fn steps_through_nested_formula_and_short_circuits_if() {
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(10.0), formula: None });
        wb.sheet_mut(0).set(0, 1, Cell { value: Value::Number(5.0), formula: Some("=A1/2".into()) });
        let mut ev = FormulaEvaluator::new(&wb, 0, 0, 2, "=IF(A1>5, B1*3, 1/0)");

        let s0 = ev.current(&wb);
        assert!(!s0.done);
        let (h1, h2) = s0.highlight.unwrap();
        assert_eq!(&s0.expr[h1..h2], "A1");

        assert!(ev.step(&wb)); // A1 -> 10
        assert!(ev.step(&wb)); // 10>5 -> TRUE
        let s_b1 = ev.current(&wb);
        let (b1, b2) = s_b1.highlight.unwrap();
        assert_eq!(&s_b1.expr[b1..b2], "B1");
        assert_eq!(s_b1.step_in_target, Some((0, 0, 1)));

        // Step into B1 (=A1/2)
        assert!(ev.step_in(&wb));
        assert_eq!(ev.depth(), 2);
        assert!(ev.step(&wb)); // A1 -> 10
        assert!(ev.step(&wb)); // 10/2 -> 5
        assert!(ev.current(&wb).done);
        assert_eq!(ev.current(&wb).expr, "5");

        // Step out back to parent
        assert!(ev.step_out(&wb));
        assert_eq!(ev.depth(), 1);
        while ev.step(&wb) {}
        let done = ev.current(&wb);
        assert!(done.done);
        assert_eq!(done.expr, "15");
    }
}
