//! Independent numeric expressions can be split across cores without duplicating
//! Calc's memoized dependencies, volatile values or lookup indexes.
use super::{bin_scalar, compile_at, deps::CellKey, BinOp, FValue, Node, Workbook};
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

fn numeric(wb: &Workbook, (sheet, row, col): CellKey) -> Option<Result<f64, super::ErrKind>> {
    let formula = wb.sheet(sheet).get(row, col)?.formula.as_deref()?;
    let node = compile_at(formula.strip_prefix('=').unwrap_or(formula), row, col).ok()?;
    let Node::Binary(op @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow),
                     lhs, rhs) = node.as_ref() else { return None };
    let lhs = FValue::Number(literal(wb, sheet, lhs)?);
    let rhs = FValue::Number(literal(wb, sheet, rhs)?);
    match bin_scalar(*op, &lhs, &rhs) {
        FValue::Number(n) => Some(Ok(n)),
        FValue::Error(e) => Some(Err(e)),
        _ => None,
    }
}

pub(super) fn evaluate(wb: &Workbook, keys: &[CellKey], cancel: Option<&AtomicBool>,
                       done: Option<&AtomicUsize>) -> Option<Vec<(usize, u32, u32, FValue)>> {
    const MIN_PER_WORKER: usize = 8192;
    if keys.len() < MIN_PER_WORKER * 2 { return None; }
    let workers = std::thread::available_parallelism().ok()?.get()
        .min(8).min(keys.len() / MIN_PER_WORKER);
    if workers < 2 { return None; }
    // Quickly bypass mixed sheets. Each worker still validates every expression.
    for &key in keys.iter().step_by((keys.len() / 16).max(1)) { let _ = numeric(wb, key)?; }
    let unsupported = AtomicBool::new(false);
    let batches = std::thread::scope(|scope| {
        let jobs: Vec<_> = keys.chunks(keys.len().div_ceil(workers)).map(|chunk| {
            let unsupported = &unsupported;
            scope.spawn(move || {
                let mut values = Vec::with_capacity(chunk.len());
                for (i, &key) in chunk.iter().enumerate() {
                    if i % 256 == 0 && (unsupported.load(Ordering::Relaxed)
                        || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))) { return None; }
                    let Some(value) = numeric(wb, key) else {
                        unsupported.store(true, Ordering::Relaxed);
                        return None;
                    };
                    values.push(value);
                    if (i + 1) % 256 == 0 {
                        if let Some(done) = done { done.fetch_add(256, Ordering::Relaxed); }
                    }
                }
                Some(values)
            })
        }).collect();
        jobs.into_iter().map(|job| job.join().expect("numeric calculation worker")).collect::<Option<Vec<_>>>()
    })?;
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) { return None; }
    if let Some(done) = done { done.store(keys.len(), Ordering::Relaxed); }
    Some(keys.iter().zip(batches.into_iter().flatten()).map(|(&(s, r, c), value)|
        (s, r, c, value.map_or_else(FValue::Error, FValue::Number))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn large_numeric_batch_falls_back_when_one_input_is_a_formula() {
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
            assert!(matches!(values[0].3, FValue::Error(_)));
            assert_eq!(values[5].3, FValue::Number(0.6));
        }
        wb.sheet_mut(0).set(37, 0, Cell { value: Value::Empty, formula: Some("=8+4".into()) });
        assert!(evaluate(&wb, &keys, None, None).is_none());
        super::super::recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).value(37, 1), Value::Number(0.25));
    }
}
