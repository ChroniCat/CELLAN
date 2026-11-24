//! Wave 0-4 端到端性能基线套件（覆盖存储/格式、公式引擎、文件 IO/加密/剪贴板/XLS）。
//!
//! 运行（release + offline）：
//!   cargo run --release --offline -p sheetcore --example perf_wave_bench
//!   cargo run --release --offline -p sheetcore --example perf_wave_bench -- --filter date_format
use sheetcore::cf::{CfBlock, CfKind, CfRule, CfSheet, CfStats, Cfvo, refresh_stats};
use sheetcore::clipboard_xmlss;
use sheetcore::crypto::EncryptionSettings;
use sheetcore::data_tools::CompactSort;
use sheetcore::formula::{self, deps::Deps};
use sheetcore::merges::Merges;
use sheetcore::structural::{Structural, apply_structural_wb};
use sheetcore::style::{CellStyle, format_value};
use sheetcore::undo::{Action, UndoStack, capture, finish, rect_keys};
use sheetcore::{Cell, Value, Workbook};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

#[cfg(windows)]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn rss_mb() -> f64 {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
            return c.WorkingSetSize as f64 / 1_048_576.0;
        }
    }
    0.0
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn emit(name: &str, value: serde_json::Value) {
    println!("PERF_WAVE_JSON: {}", serde_json::json!({ "bench": name, "metrics": value }));
}

fn should_run(filter: Option<&str>, name: &str) -> bool {
    filter.is_none_or(|f| name.contains(f))
}

fn bench_date_format_100k() {
    let n = 100_000usize;
    let fmt = "yyyy-mm-dd hh:mm:ss";
    let t = Instant::now();
    let mut total_len = 0usize;
    for i in 0..n {
        let v = Value::Number(45292.25 + (i as f64) * 0.0007);
        let s = format_value(&v, fmt);
        total_len += s.len();
    }
    let elapsed_ms = ms(t.elapsed());
    black_box(total_len);
    emit(
        "date_format_100k",
        serde_json::json!({ "n": n, "ms": (elapsed_ms * 100.0).round() / 100.0, "total_len": total_len }),
    );
}

fn bench_cf_stats_edit_200k() {
    let rows = 200_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Number((r % 10_000) as f64), formula: None });
        }
        sh.cf = CfSheet {
            blocks: vec![CfBlock {
                ranges: vec![(0, 0, rows - 1, 0)],
                rules: vec![
                    CfRule {
                        kind: CfKind::Scale {
                            points: vec![
                                (Cfvo::min(), [248, 105, 107]),
                                (Cfvo::max(), [99, 190, 123]),
                            ],
                        },
                        stop: false,
                        priority: 1,
                    },
                    CfRule {
                        kind: CfKind::Bar {
                            color: [99, 142, 198],
                            min: Cfvo::min(),
                            max: Cfvo::max(),
                        },
                        stop: false,
                        priority: 2,
                    },
                ],
            }],
            ..Default::default()
        };
    }

    let mut cached = CfStats::default();
    let t0 = Instant::now();
    refresh_stats(wb.sheet(0), &mut cached);
    let cold_ms = ms(t0.elapsed());

    wb.sheet_mut(0).set(rows / 2, 0, Cell { value: Value::Number(12345.0), formula: None });

    let t1 = Instant::now();
    let mut fresh = CfStats::default();
    refresh_stats(wb.sheet(0), &mut fresh);
    let fresh_after_edit_ms = ms(t1.elapsed());

    let t2 = Instant::now();
    refresh_stats(wb.sheet(0), &mut cached);
    let reused_after_edit_ms = ms(t2.elapsed());

    assert_eq!(fresh.ranges, cached.ranges);
    emit(
        "cf_stats_edit_200k",
        serde_json::json!({
            "rows": rows,
            "cold_ms": (cold_ms * 100.0).round() / 100.0,
            "fresh_after_edit_ms": (fresh_after_edit_ms * 100.0).round() / 100.0,
            "reused_after_edit_ms": (reused_after_edit_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_merge_query_10k_tall() {
    let mut m = Merges::new();
    let mut initial = Vec::with_capacity(10_001);
    initial.push((0, 0, 100_000, 0)); // single tall merge causing worst-case prefix-max scan
    for r in 0..10_000u32 {
        initial.push((r * 2, 1, r * 2, 2));
    }
    let t_build = Instant::now();
    m.merge_ranges(initial);
    let build_ms = ms(t_build.elapsed());

    let t_q = Instant::now();
    let mut hits = 0usize;
    for i in 0..50_000u32 {
        let r = (i * 2) % 20_000;
        if m.at(r, 1).is_some() {
            hits += 1;
        }
        if m.at(r, 5).is_some() {
            hits += 1;
        }
    }
    let query_50k_ms = ms(t_q.elapsed());
    black_box(hits);

    let mut m2 = Merges::new();
    let base: Vec<_> = (0..2_000u32).map(|r| (r * 2, 1, r * 2, 2)).collect();
    m2.merge_ranges(base);
    let incoming: Vec<_> = (0..2_000u32).map(|r| (r * 2, 4, r * 2, 5)).collect();
    let t_batch = Instant::now();
    m2.merge_ranges(incoming);
    let batch_2k_into_2k_ms = ms(t_batch.elapsed());

    emit(
        "merge_query_10k_tall",
        serde_json::json!({
            "build_10k_ms": (build_ms * 100.0).round() / 100.0,
            "query_100k_lookups_ms": (query_50k_ms * 100.0).round() / 100.0,
            "batch_2k_into_2k_ms": (batch_2k_into_2k_ms * 100.0).round() / 100.0,
            "hits": hits,
        }),
    );
}

fn bench_spilled_set_cow_100k() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..100_000u32 {
            sh.spilled.insert(sheetcore::CellRef { row: r, col: 1 });
        }
    }
    let mut cow_broken = 0usize;
    let t = Instant::now();
    for round in 0..20u32 {
        let snap = wb.sheet(0).clone();
        wb.sheet_mut(0).set(200_000 + round, 0, Cell { value: Value::Number(round as f64), formula: None });
        if !wb.sheet(0).spilled.ptr_eq(&snap.spilled) {
            cow_broken += 1;
        }
        black_box(&snap);
    }
    let cow_20_edits_ms = ms(t.elapsed());
    emit(
        "spilled_set_cow_100k",
        serde_json::json!({
            "spilled_len": wb.sheet(0).spilled.len(),
            "cow_20_edits_ms": (cow_20_edits_ms * 100.0).round() / 100.0,
            "cow_broken_count": cow_broken,
        }),
    );
}

fn bench_undo_paste_500k() {
    let rows = 50_000u32;
    let cols = 10u32;

    // 1) Legacy dense CellDelta path (currently what app paste uses, capped at 500k)
    let mut wb_dense = Workbook::new_blank();
    let mut undo_dense = UndoStack::new();
    let t1 = Instant::now();
    let keys = rect_keys(0, 0, rows - 1, cols - 1);
    let deltas = capture(wb_dense.sheet(0), 0, &keys);
    {
        let sh = wb_dense.sheet_mut(0);
        for r in 0..rows {
            for c in 0..cols {
                sh.set(r, c, Cell { value: Value::Number((r + c) as f64), formula: None });
            }
        }
    }
    let deltas = finish(deltas, wb_dense.sheet(0));
    undo_dense.push(Action {
        label: "粘贴".into(),
        deltas,
        focus: (0, 0, 0),
        snap_before: None,
        snap_after: None,
        by_site: 0,
        shapes: None,
        pictures: None,
        merges: None,
        refresh_sizes: None,
    });
    let dense_500k_ms = ms(t1.elapsed());

    // 2) CoW batch path (implemented in undo.rs but not wired in app yet)
    let mut wb_cow = Workbook::new_blank();
    let mut undo_cow = UndoStack::new();
    let t2 = Instant::now();
    let before = wb_cow.sheet(0).clone();
    {
        let sh = wb_cow.sheet_mut(0);
        for r in 0..rows {
            for c in 0..cols {
                sh.set(r, c, Cell { value: Value::Number((r + c) as f64), formula: None });
            }
        }
    }
    undo_cow.push_cow_batch("粘贴", 0, &before, wb_cow.sheet(0), (0, 0, 0));
    let cow_500k_ms = ms(t2.elapsed());

    emit(
        "undo_paste_500k",
        serde_json::json!({
            "cells": rows * cols,
            "dense_delta_ms": (dense_500k_ms * 100.0).round() / 100.0,
            "cow_batch_ms": (cow_500k_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_sort_200k_x10() {
    let rows = 200_000u32;
    let cols = 10u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Text(format!("SKU-{:06}", rows - r).into()), formula: None });
            for c in 1..cols {
                sh.set(r, c, Cell { value: Value::Number(((r + c * 7) % 10_000) as f64), formula: None });
            }
        }
    }
    let t_plan = Instant::now();
    let plan = CompactSort::plan(wb.sheet(0), 0, (0, 0, rows - 1, cols - 1), 0, true, false)
        .unwrap()
        .unwrap();
    let plan_ms = ms(t_plan.elapsed());

    let mut gather_sheet = wb.sheet(0).clone();
    let t_gather = Instant::now();
    let gather_changed = plan.apply_to_sheet(&mut gather_sheet);
    let apply_gather_ms = ms(t_gather.elapsed());
    assert_eq!(gather_sheet.value(0, 0), Value::Text("SKU-000001".into()));

    let t_apply = Instant::now();
    let mut applied = 0usize;
    for d in plan.deltas() {
        wb.sheet_mut(0).set(d.row, d.col, d.after);
        applied += 1;
    }
    let apply_ms = ms(t_apply.elapsed());
    emit(
        "sort_200k_x10",
        serde_json::json!({
            "rows": rows,
            "cols": cols,
            "plan_ms": (plan_ms * 100.0).round() / 100.0,
            "apply_gather_ms": (apply_gather_ms * 100.0).round() / 100.0,
            "total_gather_ms": ((plan_ms + apply_gather_ms) * 100.0).round() / 100.0,
            "gather_changed": gather_changed,
            "apply_ms": (apply_ms * 100.0).round() / 100.0,
            "total_ms": ((plan_ms + apply_ms) * 100.0).round() / 100.0,
            "deltas": applied,
        }),
    );
}

fn bench_structural_insert_50k_formulas() {
    let rows = 50_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
            sh.set(r, 1, Cell { value: Value::Number((r % 97) as f64), formula: None });
            sh.set(
                r,
                2,
                Cell {
                    value: Value::Number((r + (r % 97)) as f64),
                    formula: Some(format!("=A{}+B{}", r + 1, r + 1).into()),
                },
            );
        }
    }
    let op = Structural::InsertRows { at: rows / 2, n: 1 };
    let t = Instant::now();
    apply_structural_wb(&mut wb, 0, &op);
    let insert_ms = ms(t.elapsed());
    emit(
        "structural_insert_50k_formulas",
        serde_json::json!({
            "formulas": rows,
            "insert_ms": (insert_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_cell_memory_1m_num() {
    let rss_before = rss_mb();
    let rows = 100_000u32;
    let cols = 10u32;
    let t = Instant::now();
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            for c in 0..cols {
                sh.set(r, c, Cell { value: Value::Number((r + c) as f64), formula: None });
            }
        }
    }
    let fill_ms = ms(t.elapsed());
    let rss_after = rss_mb();
    let (pages, slabs, slots) = wb.sheet(0).cells_stats();
    let raw_slots_mb = (slots * std::mem::size_of::<Cell>()) as f64 / 1_048_576.0;
    emit(
        "cell_memory_1m_num",
        serde_json::json!({
            "cells": rows * cols,
            "size_of_cell": std::mem::size_of::<Cell>(),
            "raw_slots_mb": (raw_slots_mb * 100.0).round() / 100.0,
            "fill_ms": (fill_ms * 100.0).round() / 100.0,
            "rss_delta_mb": ((rss_after - rss_before) * 10.0).round() / 10.0,
            "pages": pages,
            "slabs": slabs,
            "slots": slots,
        }),
    );
}

fn bench_deps_make_mut_200k() {
    let rows = 200_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
            sh.set(
                r,
                1,
                Cell {
                    value: Value::Number((r + 1) as f64),
                    formula: Some(format!("=A{}+1", r + 1).into()),
                },
            );
        }
    }
    let t_build = Instant::now();
    let mut deps = Arc::new(Deps::build(&wb));
    let build_ms = ms(t_build.elapsed());

    // Simulate background recalc / save holding an Arc<Deps> clone while user edits 1 formula
    let _bg_guard = Arc::clone(&deps);
    wb.sheet_mut(0).set(0, 1, Cell { value: Value::Empty, formula: Some("=A1+2".into()) });
    let t_edit = Instant::now();
    Arc::make_mut(&mut deps).update_cell(&wb, (0, 0, 1));
    let make_mut_clone_ms = ms(t_edit.elapsed());

    emit(
        "deps_make_mut_200k",
        serde_json::json!({
            "formulas": rows,
            "deps_build_ms": (build_ms * 100.0).round() / 100.0,
            "make_mut_clone_and_update_ms": (make_mut_clone_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_recalc_degraded_name_50k() {
    let rows = 50_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
            sh.set(
                r,
                1,
                Cell {
                    value: Value::Empty,
                    formula: Some(format!("=IF(A{}>0, A{}*2, 0)", r + 1, r + 1).into()),
                },
            );
        }
        // Single undefined-name formula triggers runtime_refs -> requires_full_recalc()
        sh.set(0, 3, Cell { value: Value::Empty, formula: Some("=UndefinedName+1".into()) });
    }
    formula::recalc_all(&mut wb);
    let deps = Deps::build_after_recalc(&wb);
    let forces_full = deps.requires_full_recalc();

    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(42.0), formula: None });
    let t = Instant::now();
    let count = if forces_full {
        formula::recalc_all_force(&mut wb);
        rows as usize + 1
    } else {
        formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)])
    };
    let edit_recalc_ms = ms(t.elapsed());
    emit(
        "recalc_degraded_name_50k",
        serde_json::json!({
            "formulas": rows + 1,
            "requires_full_recalc": forces_full,
            "recalced_cells": count,
            "edit_recalc_ms": (edit_recalc_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_recalc_full_100k_mixed() {
    let source_rows = 10_000u32;
    let formula_rows = 100_000u32;
    let mut wb = Workbook::new_blank();
    let mut report = sheetcore::Sheet::new("Report");
    {
        let src = wb.sheet_mut(0);
        for r in 0..source_rows {
            src.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
            src.set(r, 1, Cell { value: Value::Text(format!("G{}", r % 32).into()), formula: None });
            src.set(r, 2, Cell { value: Value::Number((r % 19 + 1) as f64), formula: None });
        }
    }
    for r in 0..formula_rows {
        let f = match r % 3 {
            0 => format!("=IF(Sheet1!A{}>0, VLOOKUP({},Sheet1!$A$1:$C${source_rows},3,FALSE), 0)", (r % source_rows) + 1, r % source_rows),
            1 => format!("=SUMIFS(Sheet1!$C$1:$C${source_rows},Sheet1!$B$1:$B${source_rows},\"G{}\")", r % 32),
            _ => format!("=Sheet1!C{}*2+1", (r % source_rows) + 1),
        };
        report.set(r, 0, Cell { value: Value::Empty, formula: Some(f.into()) });
    }
    wb.sheets.push(report);

    let t1 = Instant::now();
    formula::recalc_all_force(&mut wb);
    let recalc_ms = ms(t1.elapsed());

    let t2 = Instant::now();
    let _deps = Deps::build_after_recalc(&wb);
    let deps_ms = ms(t2.elapsed());

    emit(
        "recalc_full_100k_mixed",
        serde_json::json!({
            "formulas": formula_rows,
            "recalc_ms": (recalc_ms * 100.0).round() / 100.0,
            "deps_build_ms": (deps_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_recalc_narrow_chains_20x2000() {
    let chains = 20u32;
    let depth = 2_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for c in 0..chains {
            let col = sheetcore::col_label(c);
            sh.set(0, c, Cell { value: Value::Number(1.0), formula: None });
            for r in 1..depth {
                sh.set(
                    r,
                    c,
                    Cell {
                        value: Value::Empty,
                        formula: Some(format!("=IF({col}{r}>0,{col}{r}+1,0)").into()),
                    },
                );
            }
        }
    }
    let t = Instant::now();
    formula::recalc_all_force(&mut wb);
    let recalc_ms = ms(t.elapsed());
    assert_eq!(wb.sheet(0).value(depth - 1, 0), Value::Number(depth as f64));
    emit(
        "recalc_narrow_chains_20x2000",
        serde_json::json!({
            "formulas": chains * (depth - 1),
            "recalc_ms": (recalc_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_countif_text_on_nums_200k() {
    let rows = 200_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Number((r % 100_000) as f64), formula: None });
        }
        for i in 0..50u32 {
            sh.set(
                i,
                2,
                Cell {
                    value: Value::Empty,
                    formula: Some(format!("=COUNTIF($A$1:$A${rows},\"苹果{i}\")").into()),
                },
            );
        }
    }
    let t = Instant::now();
    formula::recalc_all_force(&mut wb);
    let recalc_ms = ms(t.elapsed());
    assert_eq!(wb.sheet(0).value(0, 2), Value::Number(0.0));
    emit(
        "countif_text_on_nums_200k",
        serde_json::json!({
            "rows": rows,
            "formulas": 50,
            "recalc_ms": (recalc_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_cond_index_edit_200k() {
    let rows = 200_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Number((r % 100) as f64), formula: None });
            sh.set(r, 1, Cell { value: Value::Number(1.0), formula: None });
        }
        for i in 0..500u32 {
            sh.set(
                i,
                3,
                Cell {
                    value: Value::Empty,
                    formula: Some(format!("=SUMIFS($B$1:$B${rows},$A$1:$A${rows},\">={}\")", i % 50).into()),
                },
            );
        }
    }
    formula::recalc_all(&mut wb);
    let deps = Deps::build_after_recalc(&wb);
    wb.sheet_mut(0).set(rows / 2, 0, Cell { value: Value::Number(99.0), formula: None });
    let t = Instant::now();
    let n = formula::recalc_dirty(&mut wb, &deps, &[(0, rows / 2, 0)]);
    let edit_ms = ms(t.elapsed());
    emit(
        "cond_index_edit_200k",
        serde_json::json!({
            "rows": rows,
            "recalced": n,
            "edit_ms": (edit_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_xlsx_open_save_delta_200k_x10() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wave_200k_x10.xlsx");
    let rows = 200_000u32;
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..rows {
            sh.set(r, 0, Cell { value: Value::Text(format!("Dept{}", r % 64).into()), formula: None });
            for c in 1..9u32 {
                sh.set(r, c, Cell { value: Value::Number(((r + c) % 10_000) as f64), formula: None });
            }
            if r < 10_000 {
                sh.set(
                    r,
                    9,
                    Cell {
                        value: Value::Number(((r + 1) % 10_000 + (r + 2) % 10_000) as f64),
                        formula: Some(format!("=B{}+C{}", r + 1, r + 1).into()),
                    },
                );
            }
        }
    }

    let t_fresh = Instant::now();
    let r1 = sheetcore::xlsx::save_atomic_with_receipt(&path, &wb).unwrap();
    let fresh_save_ms = ms(t_fresh.elapsed());
    let file_mb = std::fs::metadata(&path).unwrap().len() as f64 / 1_048_576.0;
    drop(wb);

    let t_load = Instant::now();
    let mut loaded = sheetcore::xlsx::load(&path).unwrap();
    let load_ms = ms(t_load.elapsed());

    loaded.sheet_mut(0).set(100_000, 1, Cell { value: Value::Number(99999.0), formula: None });
    let t_delta = Instant::now();
    let r2 = sheetcore::xlsx::save_atomic_with_receipt(&path, &loaded).unwrap();
    let delta_save_ms = ms(t_delta.elapsed());

    emit(
        "xlsx_open_save_delta_200k_x10",
        serde_json::json!({
            "file_mb": (file_mb * 100.0).round() / 100.0,
            "fresh_save_ms": (fresh_save_ms * 100.0).round() / 100.0,
            "load_ms": (load_ms * 100.0).round() / 100.0,
            "delta_save_ms": (delta_save_ms * 100.0).round() / 100.0,
            "fresh_used_delta": r1.used_cell_delta,
            "edit_used_delta": r2.used_cell_delta,
        }),
    );
}

fn bench_xlsx_styles_3000_open_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wave_styles_3000.xlsx");
    let mut wb = Workbook::new_blank();
    let mut style_ids = Vec::with_capacity(3000);
    for i in 0..3000u32 {
        let mut s = CellStyle::default();
        s.fill = Some([(i & 0xFF) as u8, ((i >> 8) & 0xFF) as u8, ((i * 29) & 0xFF) as u8]);
        s.bold = i % 2 == 0;
        s.italic = i % 3 == 0;
        s.format = format!("#,##0.{:0>width$}", 0, width = (i % 4 + 1) as usize);
        style_ids.push(wb.styles.intern(s));
    }
    {
        let sh = wb.sheet_mut(0);
        for r in 0..30_000u32 {
            sh.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
            sh.set_style(r, 0, style_ids[(r as usize) % style_ids.len()]);
        }
    }
    let t_save = Instant::now();
    sheetcore::xlsx::save_atomic_with_receipt(&path, &wb).unwrap();
    let fresh_save_ms = ms(t_save.elapsed());
    drop(wb);

    let t_load = Instant::now();
    let mut loaded = sheetcore::xlsx::load(&path).unwrap();
    let load_ms = ms(t_load.elapsed());

    let mut s = loaded.styles.get(0).clone();
    s.bold = true;
    s.color = [200, 10, 10];
    let new_sid = loaded.styles.intern(s);
    loaded.sheet_mut(0).set_style(0, 0, new_sid);
    let t_delta = Instant::now();
    let r2 = sheetcore::xlsx::save_atomic_with_receipt(&path, &loaded).unwrap();
    let style_delta_save_ms = ms(t_delta.elapsed());

    emit(
        "xlsx_styles_3000_open_save",
        serde_json::json!({
            "styles": loaded.styles.len(),
            "fresh_save_ms": (fresh_save_ms * 100.0).round() / 100.0,
            "load_ms": (load_ms * 100.0).round() / 100.0,
            "style_delta_save_ms": (style_delta_save_ms * 100.0).round() / 100.0,
            "used_delta": r2.used_cell_delta,
        }),
    );
}

fn bench_xlsx_encrypted_50k_x5() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wave_enc_50k_x5.xlsx");
    let pw = "CellanPerf2026!";
    let mut wb = Workbook::new_blank();
    wb.encryption = Some(EncryptionSettings::new(pw));
    {
        let sh = wb.sheet_mut(0);
        for r in 0..50_000u32 {
            for c in 0..5u32 {
                sh.set(r, c, Cell { value: Value::Number((r * 5 + c) as f64), formula: None });
            }
        }
    }
    let t_save1 = Instant::now();
    sheetcore::xlsx::save_atomic_with_receipt(&path, &wb).unwrap();
    let save1_ms = ms(t_save1.elapsed());
    let file_mb = std::fs::metadata(&path).unwrap().len() as f64 / 1_048_576.0;
    drop(wb);

    let t_load = Instant::now();
    let mut loaded = sheetcore::xlsx::load_with_password(&path, pw).unwrap();
    let load_ms = ms(t_load.elapsed());

    loaded.sheet_mut(0).set(100, 0, Cell { value: Value::Number(777.0), formula: None });
    let t_save2 = Instant::now();
    sheetcore::xlsx::save_atomic_with_receipt(&path, &loaded).unwrap();
    let save2_ms = ms(t_save2.elapsed());

    emit(
        "xlsx_encrypted_50k_x5",
        serde_json::json!({
            "file_mb": (file_mb * 100.0).round() / 100.0,
            "save1_ms": (save1_ms * 100.0).round() / 100.0,
            "load_ms": (load_ms * 100.0).round() / 100.0,
            "save2_ms": (save2_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_xmlss_parse_sparse_100k() {
    let mut xml = String::with_capacity(8 * 1024 * 1024);
    xml.push_str(
        "<?xml version=\"1.0\"?><Workbook xmlns=\"urn:schemas-microsoft-com:office:spreadsheet\" xmlns:ss=\"urn:schemas-microsoft-com:office:spreadsheet\"><Worksheet ss:Name=\"S1\"><Table>",
    );
    for r in 0..100_000u32 {
        xml.push_str("<Row><Cell><Data ss:Type=\"Number\">");
        xml.push_str(&r.to_string());
        xml.push_str("</Data></Cell><Cell><Data ss:Type=\"String\">Item</Data></Cell></Row>");
    }
    xml.push_str("</Table></Worksheet></Workbook>");

    let t = Instant::now();
    let parsed = clipboard_xmlss::parse_sparse(&xml).unwrap();
    let parse_ms = ms(t.elapsed());
    emit(
        "xmlss_parse_sparse_100k",
        serde_json::json!({
            "xml_mb": ((xml.len() as f64 / 1_048_576.0) * 100.0).round() / 100.0,
            "rows": parsed.rows,
            "cells": parsed.cells.len(),
            "parse_sparse_ms": (parse_ms * 100.0).round() / 100.0,
        }),
    );
}

fn bench_xls_read_write_30k_x5() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wave_30k_x5.xls");
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..30_000u32 {
            sh.set(r, 0, Cell { value: Value::Text(format!("Row{}", r % 500).into()), formula: None });
            for c in 1..5u32 {
                sh.set(r, c, Cell { value: Value::Number((r + c) as f64), formula: None });
            }
        }
    }
    let t_w = Instant::now();
    sheetcore::xls_write::write(&path, &wb).unwrap();
    let write_ms = ms(t_w.elapsed());
    let file_mb = std::fs::metadata(&path).unwrap().len() as f64 / 1_048_576.0;

    let t_r = Instant::now();
    let loaded = sheetcore::xls_read::load(&path).unwrap();
    let read_ms = ms(t_r.elapsed());
    assert_eq!(loaded.sheet(0).len(), 150_000);

    emit(
        "xls_read_write_30k_x5",
        serde_json::json!({
            "file_mb": (file_mb * 100.0).round() / 100.0,
            "write_ms": (write_ms * 100.0).round() / 100.0,
            "read_ms": (read_ms * 100.0).round() / 100.0,
        }),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let filter = args
        .windows(2)
        .find(|w| w[0] == "--filter")
        .map(|w| w[1].as_str());

    let benches: &[(&str, fn())] = &[
        ("cell_memory_1m_num", bench_cell_memory_1m_num),
        ("date_format_100k", bench_date_format_100k),
        ("cf_stats_edit_200k", bench_cf_stats_edit_200k),
        ("merge_query_10k_tall", bench_merge_query_10k_tall),
        ("spilled_set_cow_100k", bench_spilled_set_cow_100k),
        ("undo_paste_500k", bench_undo_paste_500k),
        ("sort_200k_x10", bench_sort_200k_x10),
        ("structural_insert_50k_formulas", bench_structural_insert_50k_formulas),
        ("deps_make_mut_200k", bench_deps_make_mut_200k),
        ("recalc_degraded_name_50k", bench_recalc_degraded_name_50k),
        ("recalc_full_100k_mixed", bench_recalc_full_100k_mixed),
        ("recalc_narrow_chains_20x2000", bench_recalc_narrow_chains_20x2000),
        ("countif_text_on_nums_200k", bench_countif_text_on_nums_200k),
        ("cond_index_edit_200k", bench_cond_index_edit_200k),
        ("xlsx_open_save_delta_200k_x10", bench_xlsx_open_save_delta_200k_x10),
        ("xlsx_styles_3000_open_save", bench_xlsx_styles_3000_open_save),
        ("xlsx_encrypted_50k_x5", bench_xlsx_encrypted_50k_x5),
        ("xmlss_parse_sparse_100k", bench_xmlss_parse_sparse_100k),
        ("xls_read_write_30k_x5", bench_xls_read_write_30k_x5),
    ];

    for &(name, f) in benches {
        if should_run(filter, name) {
            f();
        }
    }
}
