//! Independent numeric expressions can be split across cores without duplicating
//! Calc's memoized dependencies, volatile values or lookup indexes.
use super::{bin_scalar, deps::CellKey, BinOp, Calc, FValue, Node, RefExpr, Workbook};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn literal(wb: &Workbook, sheet: usize, node: &Node) -> Option<f64> {
    match node {
        Node::Number(value) => Some(*value),
        Node::Ref(reference) if reference.sheet.is_none() && reference.sheet2.is_none()
            && reference.book.is_none() => {
            let cell = wb.sheet(sheet).get(reference.row.try_into().ok()?, reference.col.try_into().ok()?)?;
            match (&cell.formula, &cell.value) {
                (None, crate::Value::Number(value)) => Some(*value),
                _ => None,
            }
        }
        _ => None,
    }
}

fn numeric_binary(wb: &Workbook, sheet: usize, node: &Node) -> Option<Result<f64, super::ErrKind>> {
    let Node::Binary(op @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow),
                     lhs, rhs) = node else { return None };
    let lhs = FValue::Number(literal(wb, sheet, lhs)?);
    let rhs = FValue::Number(literal(wb, sheet, rhs)?);
    match bin_scalar(*op, &lhs, &rhs) {
        FValue::Number(n) => Some(Ok(n)),
        FValue::Error(e) => Some(Err(e)),
        _ => None,
    }
}

fn literal_area(wb: &Workbook, sheet: usize, a: &RefExpr, b: &RefExpr) -> bool {
    if a.book.is_some() || b.book.is_some() || a.sheet2.is_some() || b.sheet2.is_some()
        || a.row.min(b.row) < 0 || a.col.min(b.col) < 0
        || a.row.max(b.row) >= 1_048_576 || a.col.max(b.col) >= 16_384 { return false; }
    if let (Some(first), Some(last)) = (&a.sheet, &b.sheet)
        && !first.eq_ignore_ascii_case(last) { return false; }
    let sheet = match a.sheet.as_deref().or(b.sheet.as_deref()) {
        Some(name) => match wb.sheets.iter().find(|s| s.name.eq_ignore_ascii_case(name)) {
            Some(found) => found, None => return false,
        },
        None => wb.sheet(sheet),
    };
    let (top, bottom) = (a.row.min(b.row) as u32, a.row.max(b.row) as u32);
    let (left, right) = (a.col.min(b.col) as u32, a.col.max(b.col) as u32);
    // Wide ranges are better served by the main Calc's shared aggregate/lookup indexes.
    u64::from(bottom - top + 1) * u64::from(right - left + 1) <= 16
        && (top..=bottom).all(|r| (left..=right).all(|c| sheet.get(r,c).is_none_or(|v| v.formula.is_none())))
}

fn independent(wb: &Workbook, sheet: usize, node: &Node) -> bool {
    match node {
        Node::Number(_) | Node::Text(_) | Node::Bool(_) | Node::Empty | Node::Err(_) => true,
        Node::Ref(a) => literal_area(wb, sheet, a, a),
        Node::Range(a,b) => literal_area(wb, sheet, a, b),
        Node::Unary(_, v) | Node::Percent(v) => independent(wb, sheet, v),
        Node::Binary(_, a,b) => independent(wb, sheet, a) && independent(wb, sheet, b),
        Node::Func(name, args) if matches!(name.as_str(), "IF" | "IFERROR" | "ABS" | "ROUND" |
            "ROUNDUP" | "ROUNDDOWN" | "INT" | "MOD" | "TRUNC" | "SIGN" | "SQRT" |
            "SUM" | "MIN" | "MAX" | "AVERAGE" | "COUNT" | "COUNTA") =>
            args.iter().all(|v| independent(wb, sheet, v)),
        _ => false,
    }
}

fn evaluate_cell<'a>(wb: &'a Workbook, (sheet,row,col): CellKey, worker: &mut Option<Calc<'a>>) -> Option<crate::Value> {
    let formula = wb.sheet(sheet).get(row, col)?.formula.as_deref()?;
    let source = formula.strip_prefix('=').unwrap_or(formula);
    if let Some(template) = super::compiled::scalar_at(source, row, col) {
        let result = template.evaluate(|r,c| match wb.sheet(sheet).get(r,c) {
            Some(cell) if cell.formula.is_some() => None,
            Some(cell) => Some(super::to_fvalue(&cell.value)),
            None => Some(FValue::Empty),
        })?;
        return Some(super::from_fvalue(&result));
    }
    super::compiled::with_node(source, row, col, |node| {
        if let Some(value) = numeric_binary(wb, sheet, node) {
            return Some(value.map_or_else(crate::Value::Error, crate::Value::Number));
        }
        if !independent(wb, sheet, node) { return None; }
        let calc = worker.get_or_insert_with(|| Calc::new(wb));
        calc.cur = (row,col);
        calc.cur_sheet = sheet;
        let value = calc.eval_node(node, sheet);
        if matches!(value, FValue::Array(_) | FValue::Lambda(_)) { return None; }
        Some(super::from_fvalue(&value))
    }).ok().flatten()
}

pub(super) struct Batch {
    pub values: Vec<(usize, u32, u32, FValue)>,
    pub pending: Vec<usize>,
}

pub(super) fn evaluate(wb: &Workbook, keys: &[CellKey], cancel: Option<&AtomicBool>,
                       done: Option<&AtomicUsize>) -> Option<Batch> {
    const MIN_PER_WORKER: usize = 8192;
    if keys.len() < MIN_PER_WORKER * 2 { return None; }
    let workers = std::thread::available_parallelism().ok()?.get()
        .min(8).min(keys.len() / MIN_PER_WORKER);
    if workers < 2 { return None; }
    // Avoid spinning up workers for dependency chains or mostly unsupported formulas.
    // Vary the offset inside each stratum so a repeating column pattern isn't sampled at one phase.
    const SAMPLES: usize = 16;
    let stride = keys.len() / SAMPLES;
    let mut probe = None;
    let eligible = (0..SAMPLES).filter(|&i| {
        let slot = i * stride + (i * 37 + 17) % stride;
        evaluate_cell(wb, keys[slot], &mut probe).is_some()
    }).count();
    if eligible < SAMPLES / 2 { return None; }
    let batches = std::thread::scope(|scope| {
        let jobs: Vec<_> = keys.chunks(keys.len().div_ceil(workers)).map(|chunk| {
            scope.spawn(move || {
                let mut worker = None;
                let mut values = Vec::with_capacity(chunk.len());
                let mut completed = 0;
                for (i, &key) in chunk.iter().enumerate() {
                    if i % 256 == 0 && cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) { return None; }
                    let value = evaluate_cell(wb, key, &mut worker);
                    completed += usize::from(value.is_some());
                    values.push(value);
                    if (i + 1) % 256 == 0 {
                        if let Some(done) = done { done.fetch_add(completed, Ordering::Relaxed); }
                        completed = 0;
                    }
                }
                Some(values)
            })
        }).collect();
        jobs.into_iter().map(|job| job.join().expect("numeric calculation worker")).collect::<Option<Vec<_>>>()
    })?;
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) { return None; }
    let mut pending = Vec::new();
    let values = keys.iter().zip(batches.into_iter().flatten()).enumerate().map(|(i, (&(s, r, c), value))| {
        let value = match value {
            Some(result) => super::to_fvalue(&result),
            None => { pending.push(i); FValue::Empty }
        };
        (s, r, c, value)
    }).collect();
    if let Some(done) = done { done.store(keys.len() - pending.len(), Ordering::Relaxed); }
    Some(Batch { values, pending })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric(wb: &Workbook, (sheet,row,col): CellKey) -> Option<Result<f64, crate::formula::ErrKind>> {
        let src = wb.sheet(sheet).get(row,col)?.formula.as_deref()?;
        let node = super::super::compile_at(src.trim_start_matches('='),row,col).ok()?;
        numeric_binary(wb,sheet,&node)
    }

    #[test]
    fn supported_arithmetic_matches_calc_and_qualified_inputs_fall_back() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(2.0), formula: None });
        for src in ["=$A$1+$A1", "=A$1-4", "=A1*0", "=A1/0", "=A1^3", "=1E308*1E308"] {
            wb.sheet_mut(0).set(0, 1, Cell { value: Value::Empty, formula: Some(src.into()) });
            let result = numeric(&wb, (0, 0, 1)).unwrap().map_or_else(FValue::Error, FValue::Number);
            assert_eq!(result, super::super::eval_cell(&wb, 0, 0, 1), "{src}");
        }
        wb.sheet_mut(0).set(1, 0, Cell { value: Value::Bool(true), formula: None });
        wb.sheet_mut(0).set(2, 0, Cell { value: Value::Error(super::super::ErrKind::Ref), formula: None });
        for src in ["=Sheet1!A1+1", "='[1]Sheet1'!A1+1", "=A2+2", "=A3+1", "=D8*1",
                    "=A1#+1", "=NamedRate+1", "=OFFSET(A1,0,0)+1", "=B1+1", "=A1&1"] {
            wb.sheet_mut(0).set(0, 1, Cell { value: Value::Empty, formula: Some(src.into()) });
            assert!(numeric(&wb, (0, 0, 1)).is_none(), "{src} must use Calc");
        }
    }

    #[test]
    fn compound_parallel_expressions_match_calc_and_reject_computed_inputs() {
        use crate::{Cell, Sheet, Value};
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(0,0,Cell { value: Value::Number(5.),formula: None });
        wb.sheets.push(Sheet::new("Data"));
        wb.sheet_mut(1).set(0,1,Cell { value: Value::Number(4.), formula: None });
        for source in ["=IF(A1>0,ROUND((A1+2)*1.5,0),0)","=SUM(Data!A1:B1)*2", "=IFERROR(1/0,17)",
                       "=IF(FALSE,1/0,3)", "=A1>2", "=IF(FALSE,1,\"hi\")","=@A1"] {
            wb.sheet_mut(0).set(2,0,Cell { value: Value::Empty,formula: Some(source.into()) });
            let actual = evaluate_cell(&wb,(0,2,0),&mut None);
            if source == "=@A1" { assert!(actual.is_none()); }
            else { assert_eq!(actual,Some(super::super::from_fvalue(&super::super::eval_cell(&wb,0,2,0))),"{source}"); }
        }
        wb.sheet_mut(1).set(0,1,Cell { value: Value::Number(999.),formula: Some("=RAND()".into()) });
        for source in ["=SUM(Data!A1:B1)*2","=IF(TRUE,A1,RAND())","=SUM(A1:A300)","=INDIRECT(\"A1\")"] {
            wb.sheet_mut(0).set(2,0,Cell { value: Value::Empty,formula: Some(source.into()) });
            assert!(evaluate_cell(&wb,(0,2,0),&mut None).is_none(),"{source}");
        }
    }

    #[test]
    fn page_copy_for_cached_results_keeps_original_formula_allocation() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        let formula: std::sync::Arc<str> = "=3+4".into();
        wb.sheet_mut(0).set(0, 0, Cell { value: Value::Empty, formula: Some(formula.clone()) });
        let snapshot = wb.clone();
        super::super::recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).value(0, 0), Value::Number(7.0));
        assert_eq!(snapshot.sheet(0).value(0, 0), Value::Empty);
        assert!(std::sync::Arc::ptr_eq(&formula, wb.sheet(0).get(0, 0).unwrap().formula.as_ref().unwrap()));
        assert!(std::sync::Arc::ptr_eq(&formula, snapshot.sheet(0).get(0, 0).unwrap().formula.as_ref().unwrap()));
    }

    #[test]
    fn already_cancelled_calculation_preserves_the_visible_spill() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(0, 0, Cell { value: Value::Empty, formula: Some("=SEQUENCE(3,1)".into()) });
        super::super::recalc_all(&mut wb);
        let cancel = AtomicBool::new(true);
        assert!(!super::super::recalc_all_force_impl(&mut wb, Some(&cancel), None));
        assert_eq!(wb.sheet(0).value(2, 0), Value::Number(3.0));
        assert_eq!(wb.sheet(0).spill_shapes.get(&crate::CellRef::new(0,0)), Some(&(3,1)));
    }

    #[test]
    fn large_mixed_batch_reuses_numeric_results_and_preserves_spill_passes() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        let mut keys = Vec::new();
        for row in 0..16_384 {
            wb.sheet_mut(0).set(row, 0, Cell { value: Value::Number(row as f64), formula: None });
            wb.sheet_mut(0).set(row, 1, Cell { value: Value::Empty, formula: Some(format!("=3/A{}", row + 1).into()) });
            keys.push((0, row, 1));
        }
        if std::thread::available_parallelism().is_ok_and(|n| n.get() > 1) {
            let values = evaluate(&wb, &keys, None, None).unwrap();
            assert!(values.pending.is_empty());
            assert!(matches!(values.values[0].3, FValue::Error(_)));
            assert_eq!(values.values[5].3, FValue::Number(0.6));
        }
        wb.sheet_mut(0).set(37, 0, Cell { value: Value::Empty, formula: Some("=8+4".into()) });
        if std::thread::available_parallelism().is_ok_and(|n| n.get() > 1) {
            let batch = evaluate(&wb, &keys, None, None).unwrap();
            assert_eq!(batch.pending, [37]);
        }
        for (c, src) in [(2, "=SUM(B2:B3)"), (3, "=SEQUENCE(2,1,10)"),
                          (4, "=D2+1"), (5, "=SUM(D1#)"), (6, "=C1+E1")] {
            wb.sheet_mut(0).set(0, c, Cell { value: Value::Empty, formula: Some(src.into()) });
        }
        wb.sheets.push(crate::Sheet::new("Summary"));
        wb.sheet_mut(1).set(0, 0, Cell { value: Value::Empty, formula: Some("=SUM(Sheet1!B2:B3,Sheet1!G1)".into()) });
        super::super::recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).value(37, 1), Value::Number(0.25));
        assert_eq!(wb.sheet(0).value(0, 2), Value::Number(4.5));
        assert_eq!(wb.sheet(0).value(0, 4), Value::Number(12.0));
        assert_eq!(wb.sheet(0).value(0, 5), Value::Number(21.0));
        assert_eq!(wb.sheet(1).value(0, 0), Value::Number(21.0));
    }
}
