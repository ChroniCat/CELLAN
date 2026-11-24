//! Independent scalar expressions split across all cores via the persistent thread pool
//! with dynamic work stealing, direct `FValue` transport, and `scope` support.
use super::{
    BinOp, Calc, FValue, Node, RefExpr, SharedCalcCaches, Workbook, bin_scalar,
    deps::CellKey,
    func_id::FuncId,
    functions::EvalCtx,
    pool,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[inline]
fn cell_has_cached_scalar(
    wb: &Workbook,
    scope: Option<&HashSet<CellKey>>,
    sheet: usize,
    r: u32,
    c: u32,
) -> bool {
    match wb.sheet(sheet).get(r, c) {
        None => true,
        Some(cell) => {
            if cell.formula.is_none() {
                true
            } else {
                scope.is_some_and(|s| !s.contains(&(sheet, r, c)))
                    && !super::callable_cache_candidate(&cell.value)
            }
        }
    }
}

fn literal(
    wb: &Workbook,
    scope: Option<&HashSet<CellKey>>,
    sheet: usize,
    node: &Node,
    dr: i64,
    dc: i64,
) -> Option<f64> {
    match node {
        Node::Number(value) => Some(*value),
        Node::Ref(reference)
            if reference.sheet.is_none()
                && reference.sheet2.is_none()
                && reference.book.is_none() =>
        {
            let r_i = reference.row + if reference.row_abs { 0 } else { dr };
            let c_i = reference.col + if reference.col_abs { 0 } else { dc };
            let r = u32::try_from(r_i).ok()?;
            let c = u32::try_from(c_i).ok()?;
            let cell = wb.sheet(sheet).get(r, c)?;
            if !cell_has_cached_scalar(wb, scope, sheet, r, c) {
                return None;
            }
            match &cell.value {
                crate::Value::Number(value) => Some(*value),
                _ => None,
            }
        }
        _ => None,
    }
}

fn numeric_binary(
    wb: &Workbook,
    scope: Option<&HashSet<CellKey>>,
    sheet: usize,
    node: &Node,
    dr: i64,
    dc: i64,
) -> Option<Result<f64, super::ErrKind>> {
    let Node::Binary(
        op @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow),
        lhs,
        rhs,
    ) = node
    else {
        return None;
    };
    let lhs = FValue::Number(literal(wb, scope, sheet, lhs, dr, dc)?);
    let rhs = FValue::Number(literal(wb, scope, sheet, rhs, dr, dc)?);
    match bin_scalar(*op, &lhs, &rhs) {
        FValue::Number(n) => Some(Ok(n)),
        FValue::Error(e) => Some(Err(e)),
        _ => None,
    }
}

fn literal_area(
    wb: &Workbook,
    scope: Option<&HashSet<CellKey>>,
    sheet: usize,
    a: &RefExpr,
    b: &RefExpr,
    dr: i64,
    dc: i64,
) -> bool {
    if a.book.is_some()
        || b.book.is_some()
        || a.sheet2.is_some()
        || b.sheet2.is_some()
    {
        return false;
    }
    let ar = a.row + if a.row_abs { 0 } else { dr };
    let ac = a.col + if a.col_abs { 0 } else { dc };
    let br = b.row + if b.row_abs { 0 } else { dr };
    let bc = b.col + if b.col_abs { 0 } else { dc };
    if ar.min(br) < 0
        || ac.min(bc) < 0
        || ar.max(br) >= 1_048_576
        || ac.max(bc) >= 16_384
    {
        return false;
    }
    if let (Some(first), Some(last)) = (&a.sheet, &b.sheet)
        && !first.eq_ignore_ascii_case(last)
    {
        return false;
    }
    let target_si = match a.sheet.as_deref().or(b.sheet.as_deref()) {
        Some(name) => match wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(name)) {
            Some(idx) => idx,
            None => return false,
        },
        None => sheet,
    };
    let (top, bottom) = (ar.min(br) as u32, ar.max(br) as u32);
    let (left, right) = (ac.min(bc) as u32, ac.max(bc) as u32);
    // Larger ranges are handled by dependency_batch + SharedCalcCaches.
    u64::from(bottom - top + 1) * u64::from(right - left + 1) <= 16
        && (top..=bottom).all(|r| {
            (left..=right).all(|c| cell_has_cached_scalar(wb, scope, target_si, r, c))
        })
}

fn independent(
    wb: &Workbook,
    scope: Option<&HashSet<CellKey>>,
    sheet: usize,
    node: &Node,
    dr: i64,
    dc: i64,
) -> bool {
    match node {
        Node::Number(_) | Node::Text(_) | Node::Bool(_) | Node::Empty | Node::Err(_) => true,
        Node::Ref(a) => literal_area(wb, scope, sheet, a, a, dr, dc),
        Node::Range(a, b) => literal_area(wb, scope, sheet, a, b, dr, dc),
        Node::Unary(_, v) | Node::Percent(v) => independent(wb, scope, sheet, v, dr, dc),
        Node::Binary(_, a, b) => {
            independent(wb, scope, sheet, a, dr, dc) && independent(wb, scope, sheet, b, dr, dc)
        }
        Node::Func(name, args)
            if matches!(
                name.id(),
                FuncId::If
                    | FuncId::IfError
                    | FuncId::IfNa
                    | FuncId::Abs
                    | FuncId::Round
                    | FuncId::RoundUp
                    | FuncId::RoundDown
                    | FuncId::Int
                    | FuncId::Mod
                    | FuncId::Trunc
                    | FuncId::Sign
                    | FuncId::Sqrt
                    | FuncId::Power
                    | FuncId::Exp
                    | FuncId::Ln
                    | FuncId::Log10
                    | FuncId::Ceiling
                    | FuncId::Floor
                    | FuncId::And
                    | FuncId::Or
                    | FuncId::Not
                    | FuncId::Len
                    | FuncId::Lower
                    | FuncId::Upper
                    | FuncId::Trim
                    | FuncId::Left
                    | FuncId::Right
                    | FuncId::Mid
                    | FuncId::Sum
                    | FuncId::Min
                    | FuncId::Max
                    | FuncId::Average
                    | FuncId::Count
                    | FuncId::CountA
            ) =>
        {
            args.iter().all(|v| independent(wb, scope, sheet, v, dr, dc))
        }
        _ => false,
    }
}

fn evaluate_cell<'a>(
    wb: &'a Workbook,
    scope: Option<&'a HashSet<CellKey>>,
    shared: Option<&'a SharedCalcCaches>,
    ctx: EvalCtx,
    (sheet, row, col): CellKey,
    worker: &mut Option<Calc<'a>>,
) -> Option<FValue> {
    let formula_arc = wb.sheet(sheet).get(row, col)?.formula.as_ref()?;
    super::compiled::with_eval_arc(formula_arc, row, col, |node, dr, dc, kind| match kind {
        super::compiled::ScalarKind::SameSheet => {
            super::compiled::eval_projected_scalar(node, dr, dc, &mut |_, r, c| {
                match wb.sheet(sheet).get(r, c) {
                    Some(_) if !cell_has_cached_scalar(wb, scope, sheet, r, c) => None,
                    Some(cell) => Some(super::to_fvalue(&cell.value)),
                    None => Some(FValue::Empty),
                }
            })
        }
        super::compiled::ScalarKind::CrossSheet => {
            super::compiled::eval_projected_scalar(node, dr, dc, &mut |sh_opt, r, c| {
                let target_si = match sh_opt {
                    Some(name) => wb
                        .sheets
                        .iter()
                        .position(|s| s.name.eq_ignore_ascii_case(name))?,
                    None => sheet,
                };
                match wb.sheet(target_si).get(r, c) {
                    Some(_) if !cell_has_cached_scalar(wb, scope, target_si, r, c) => None,
                    Some(cell) => Some(super::to_fvalue(&cell.value)),
                    None => Some(FValue::Empty),
                }
            })
        }
        super::compiled::ScalarKind::None => {
            if let Some(value) = numeric_binary(wb, scope, sheet, node, dr, dc) {
                return Some(value.map_or_else(FValue::Error, FValue::Number));
            }
            if !independent(wb, scope, sheet, node, dr, dc) {
                return None;
            }
            let calc = worker.get_or_insert_with(|| Calc::new_worker(wb, ctx, scope, shared));
            calc.cur = (row, col);
            calc.cur_sheet = sheet;
            let value = calc.eval_node_shifted(node, sheet, dr, dc);
            if matches!(value, FValue::Array(_) | FValue::Lambda(_)) {
                return None;
            }
            Some(value)
        }
    })
    .ok()
    .flatten()
}

pub(super) struct Batch {
    pub values: Vec<(usize, u32, u32, FValue)>,
    pub pending: Vec<usize>,
}

#[allow(dead_code)]
pub(super) fn evaluate(
    wb: &Workbook,
    keys: &[CellKey],
    cancel: Option<&AtomicBool>,
    done: Option<&AtomicUsize>,
) -> Option<Batch> {
    evaluate_in(
        wb,
        keys,
        None,
        None,
        EvalCtx::for_workbook(wb.date_system),
        cancel,
        done,
    )
}

pub(super) fn evaluate_in<'a>(
    wb: &'a Workbook,
    keys: &[CellKey],
    scope: Option<&'a HashSet<CellKey>>,
    shared: Option<&'a SharedCalcCaches>,
    ctx: EvalCtx,
    cancel: Option<&AtomicBool>,
    done: Option<&AtomicUsize>,
) -> Option<Batch> {
    const MIN_PER_WORKER: usize = 2_048;
    const CHUNK: usize = 512;
    if keys.len() < MIN_PER_WORKER * 2 {
        return None;
    }
    let workers = pool::max_workers().min(keys.len() / MIN_PER_WORKER);
    if workers < 2 {
        return None;
    }
    const SAMPLES: usize = 16;
    let stride = keys.len() / SAMPLES;
    let mut probe = None;
    let eligible = (0..SAMPLES)
        .filter(|&i| {
            let slot = i * stride + (i * 37 + 17) % stride;
            evaluate_cell(wb, scope, shared, ctx, keys[slot], &mut probe).is_some()
        })
        .count();
    if eligible < SAMPLES / 2 {
        return None;
    }
    let num_chunks = keys.len().div_ceil(CHUNK);
    let next_chunk = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let chunk_outputs: Vec<std::sync::Mutex<Vec<Option<FValue>>>> = (0..num_chunks)
        .map(|_| std::sync::Mutex::new(Vec::new()))
        .collect();

    pool::run_parallel(workers, |_| {
        let mut worker = None;
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                aborted.store(true, Ordering::Relaxed);
                return;
            }
            let ci = next_chunk.fetch_add(1, Ordering::Relaxed);
            if ci >= num_chunks {
                return;
            }
            let start = ci * CHUNK;
            let end = (start + CHUNK).min(keys.len());
            let mut vals = Vec::with_capacity(end - start);
            let mut completed = 0usize;
            for (idx, &key) in keys[start..end].iter().enumerate() {
                if idx % 256 == 0 && cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    aborted.store(true, Ordering::Relaxed);
                    return;
                }
                let v = evaluate_cell(wb, scope, shared, ctx, key, &mut worker);
                completed += usize::from(v.is_some());
                vals.push(v);
            }
            if let Some(done) = done {
                done.fetch_add(completed, Ordering::Relaxed);
            }
            *chunk_outputs[ci].lock().unwrap() = vals;
        }
    });

    if aborted.load(Ordering::Relaxed) || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return None;
    }

    let mut pending = Vec::new();
    let mut values = Vec::with_capacity(keys.len());
    let mut slot = 0usize;
    for chunk_mutex in chunk_outputs {
        let vals = chunk_mutex.into_inner().unwrap();
        for v in vals {
            let (s, r, c) = keys[slot];
            let fv = match v {
                Some(res) => res,
                None => {
                    pending.push(slot);
                    FValue::Empty
                }
            };
            values.push((s, r, c, fv));
            slot += 1;
        }
    }
    if let Some(done) = done {
        done.store(keys.len() - pending.len(), Ordering::Relaxed);
    }
    Some(Batch { values, pending })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric(
        wb: &Workbook,
        (sheet, row, col): CellKey,
    ) -> Option<Result<f64, crate::formula::ErrKind>> {
        let src = wb.sheet(sheet).get(row, col)?.formula.as_deref()?;
        let node = super::super::compile_at(src.trim_start_matches('='), row, col).ok()?;
        numeric_binary(wb, None, sheet, &node, 0, 0)
    }

    #[test]
    fn supported_arithmetic_matches_calc_and_qualified_inputs_fall_back() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Number(2.0),
                formula: None,
            },
        );
        for src in [
            "=$A$1+$A1",
            "=A$1-4",
            "=A1*0",
            "=A1/0",
            "=A1^3",
            "=1E308*1E308",
        ] {
            wb.sheet_mut(0).set(
                0,
                1,
                Cell {
                    value: Value::Empty,
                    formula: Some(src.into()),
                },
            );
            let result = numeric(&wb, (0, 0, 1))
                .unwrap()
                .map_or_else(FValue::Error, FValue::Number);
            assert_eq!(result, super::super::eval_cell(&wb, 0, 0, 1), "{src}");
        }
        wb.sheet_mut(0).set(
            1,
            0,
            Cell {
                value: Value::Bool(true),
                formula: None,
            },
        );
        wb.sheet_mut(0).set(
            2,
            0,
            Cell {
                value: Value::Error(super::super::ErrKind::Ref),
                formula: None,
            },
        );
        for src in [
            "=Sheet1!A1+1",
            "='[1]Sheet1'!A1+1",
            "=A2+2",
            "=A3+1",
            "=D8*1",
            "=A1#+1",
            "=NamedRate+1",
            "=OFFSET(A1,0,0)+1",
            "=B1+1",
            "=A1&1",
        ] {
            wb.sheet_mut(0).set(
                0,
                1,
                Cell {
                    value: Value::Empty,
                    formula: Some(src.into()),
                },
            );
            assert!(numeric(&wb, (0, 0, 1)).is_none(), "{src} must use Calc");
        }
    }

    #[test]
    fn compound_parallel_expressions_match_calc_and_reject_computed_inputs() {
        use crate::{Cell, Sheet, Value};
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Number(5.),
                formula: None,
            },
        );
        wb.sheets.push(Sheet::new("Data"));
        wb.sheet_mut(1).set(
            0,
            1,
            Cell {
                value: Value::Number(4.),
                formula: None,
            },
        );
        let ctx = EvalCtx::for_workbook(wb.date_system);
        for source in [
            "=IF(A1>0,ROUND((A1+2)*1.5,0),0)",
            "=SUM(Data!A1:B1)*2",
            "=IFERROR(1/0,17)",
            "=IF(FALSE,1/0,3)",
            "=A1>2",
            "=IF(FALSE,1,\"hi\")",
            "=@A1",
        ] {
            wb.sheet_mut(0).set(
                2,
                0,
                Cell {
                    value: Value::Empty,
                    formula: Some(source.into()),
                },
            );
            let actual = evaluate_cell(&wb, None, None, ctx, (0, 2, 0), &mut None);
            if source == "=@A1" {
                assert!(actual.is_none());
            } else {
                assert_eq!(
                    actual.map(|v| super::super::from_fvalue(&v)),
                    Some(super::super::from_fvalue(&super::super::eval_cell(
                        &wb, 0, 2, 0
                    ))),
                    "{source}"
                );
            }
        }
        wb.sheet_mut(1).set(
            0,
            1,
            Cell {
                value: Value::Number(999.),
                formula: Some("=RAND()".into()),
            },
        );
        for source in [
            "=SUM(Data!A1:B1)*2",
            "=IF(TRUE,A1,RAND())",
            "=SUM(A1:A300)",
            "=INDIRECT(\"A1\")",
        ] {
            wb.sheet_mut(0).set(
                2,
                0,
                Cell {
                    value: Value::Empty,
                    formula: Some(source.into()),
                },
            );
            assert!(
                evaluate_cell(&wb, None, None, ctx, (0, 2, 0), &mut None).is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn page_copy_for_cached_results_keeps_original_formula_allocation() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        let formula: std::sync::Arc<str> = "=3+4".into();
        wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some(formula.clone()),
            },
        );
        let snapshot = wb.clone();
        super::super::recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).value(0, 0), Value::Number(7.0));
        assert_eq!(snapshot.sheet(0).value(0, 0), Value::Empty);
        assert!(std::sync::Arc::ptr_eq(
            &formula,
            wb.sheet(0).get(0, 0).unwrap().formula.as_ref().unwrap()
        ));
        assert!(std::sync::Arc::ptr_eq(
            &formula,
            snapshot
                .sheet(0)
                .get(0, 0)
                .unwrap()
                .formula
                .as_ref()
                .unwrap()
        ));
    }

    #[test]
    fn already_cancelled_calculation_preserves_the_visible_spill() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("=SEQUENCE(3,1)".into()),
            },
        );
        super::super::recalc_all(&mut wb);
        let cancel = AtomicBool::new(true);
        assert!(!super::super::recalc_all_force_impl(
            &mut wb,
            Some(&cancel),
            None
        ));
        assert_eq!(wb.sheet(0).value(2, 0), Value::Number(3.0));
        assert_eq!(
            wb.sheet(0).spill_shapes.get(&crate::CellRef::new(0, 0)),
            Some(&(3, 1))
        );
    }

    #[test]
    fn large_mixed_batch_reuses_numeric_results_and_preserves_spill_passes() {
        use crate::{Cell, Value};
        let mut wb = Workbook::new_blank();
        let mut keys = Vec::new();
        for row in 0..16_384 {
            wb.sheet_mut(0).set(
                row,
                0,
                Cell {
                    value: Value::Number(row as f64),
                    formula: None,
                },
            );
            wb.sheet_mut(0).set(
                row,
                1,
                Cell {
                    value: Value::Empty,
                    formula: Some(format!("=3/A{}", row + 1).into()),
                },
            );
            keys.push((0, row, 1));
        }
        if std::thread::available_parallelism().is_ok_and(|n| n.get() > 1) {
            let values = evaluate(&wb, &keys, None, None).unwrap();
            assert!(values.pending.is_empty());
            assert!(matches!(values.values[0].3, FValue::Error(_)));
            assert_eq!(values.values[5].3, FValue::Number(0.6));
        }
        wb.sheet_mut(0).set(
            37,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("=8+4".into()),
            },
        );
        if std::thread::available_parallelism().is_ok_and(|n| n.get() > 1) {
            let batch = evaluate(&wb, &keys, None, None).unwrap();
            assert_eq!(batch.pending, [37]);
        }
        for (c, src) in [
            (2, "=SUM(B2:B3)"),
            (3, "=SEQUENCE(2,1,10)"),
            (4, "=D2+1"),
            (5, "=SUM(D1#)"),
            (6, "=C1+E1"),
        ] {
            wb.sheet_mut(0).set(
                0,
                c,
                Cell {
                    value: Value::Empty,
                    formula: Some(src.into()),
                },
            );
        }
        wb.sheets.push(crate::Sheet::new("Summary"));
        wb.sheet_mut(1).set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("=SUM(Sheet1!B2:B3,Sheet1!G1)".into()),
            },
        );
        super::super::recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).value(37, 1), Value::Number(0.25));
        assert_eq!(wb.sheet(0).value(0, 2), Value::Number(4.5));
        assert_eq!(wb.sheet(0).value(0, 4), Value::Number(12.0));
        assert_eq!(wb.sheet(0).value(0, 5), Value::Number(21.0));
        assert_eq!(wb.sheet(1).value(0, 0), Value::Number(21.0));
    }
}
