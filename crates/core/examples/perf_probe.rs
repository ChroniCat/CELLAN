//! QA-07 性能基线：打开 / 重算 / 改一格 / 保存 的端到端耗时与内存峰值。
//!
//! 用法（release）：
//!   cargo run --release -p sheetcore --example perf_probe -- --suite
//!   cargo run --release -p sheetcore --example perf_probe -- --one 100000 values
//!
//! JSON 中 save_ms 为首次原子保存，save2_ms 为载入、编辑后原地原子保存。
//! save_used_delta / save2_used_delta 取自实际保存回执。两个阶段均包含完整包校验。
use sheetcore::{Cell, Value, Workbook};
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

/// 采样线程：每 20ms 读一次工作集，记录峰值（保存/重算这类瞬时峰值靠首尾读数测不到）
struct PeakSampler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    peak: std::sync::Arc<std::sync::atomic::AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PeakSampler {
    fn start() -> Self {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let peak = std::sync::Arc::new(AtomicU64::new((rss_mb() * 1024.0) as u64));
        let (s2, p2) = (stop.clone(), peak.clone());
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                let cur = (rss_mb() * 1024.0) as u64;
                p2.fetch_max(cur, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        Self { stop, peak, handle: Some(handle) }
    }
    fn finish(mut self) -> f64 {
        use std::sync::atomic::Ordering;
        self.stop_sampling();
        rss_mb().max(self.peak.load(Ordering::Relaxed) as f64 / 1024.0)
    }

    fn stop_sampling(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for PeakSampler {
    fn drop(&mut self) { self.stop_sampling(); }
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// 平铺数值：cells 个非空格，1000 列宽
fn build_values(cells: usize) -> Workbook {
    let mut wb = Workbook::new_blank();
    let cols = 1000u32;
    for i in 0..cells {
        let r = (i as u32) / cols;
        let c = (i as u32) % cols;
        wb.sheets[0].set(
            r,
            c,
            Cell {
                value: Value::Number((i % 997) as f64),
                formula: None,
            },
        );
    }
    wb
}

/// Both fixtures have identical dimensions. In a chain every B depends on the previous B;
/// in the historical fixture each B depends only on A in the same row.
fn build_formulas(cells: usize, chain: bool) -> Workbook {
    let mut wb = Workbook::new_blank();
    for r in 0..cells as u32 {
        wb.sheets[0].set(
            r,
            0,
            Cell {
                value: Value::Number(r as f64),
                formula: None,
            },
        );
        wb.sheets[0].set(
            r,
            1,
            Cell {
                // A real saved workbook already has valid caches. The later save should
                // measure this edit, not correction of every placeholder formula result.
                value: Value::Number(f64::from(r) + 1.0),
                formula: Some(if chain && r > 0 {
                    format!("=B{r}+1")
                } else {
                    format!("=A{}+1", r + 1)
                }.into()),
            },
        );
    }
    wb
}

/// 稀疏远端：只有 1000 个值，但落在 1,000,000 行范围内
fn build_sparse(filled: usize) -> Workbook {
    let mut wb = Workbook::new_blank();
    // 行号必须落在 Excel 上限 1,048,576 之内（早期版本写成 i*999，
    // 1 万个值就跑到 999 万行，模型/渲染按最大行分配内存，直接把 RSS 顶到 2GB）
    let step = (1_048_576u32 / filled.max(1) as u32).max(1);
    for i in 0..filled {
        let r = (i as u32) * step;
        wb.sheets[0].set(
            r,
            3,
            Cell {
                value: Value::Number(i as f64),
                formula: None,
            },
        );
    }
    wb
}

fn build_mixed(rows: usize, source_rows: u32) -> Workbook {
    let mut wb = Workbook::new_blank();
    let mut report = sheetcore::Sheet::new("Report");
    let mut sums = [0.0; 16];
    for r in 0..source_rows {
        let source = &mut wb.sheets[0];
        let amount = 1.0 + (r % 19) as f64;
        sums[(r % 16) as usize] += amount;
        source.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        source.set(r, 1, Cell { value: Value::Text(format!("组{}", r % 16).into()), formula: None });
        source.set(r, 2, Cell { value: Value::Number(amount), formula: None });
    }
    for r in 0..rows as u32 {
        let (formula, cache) = if r % 2 == 0 {
            (format!("=VLOOKUP({},Sheet1!$A$1:$C${source_rows},3,FALSE)", r % source_rows), 1.0 + (r % source_rows % 19) as f64)
        } else {
            (format!("=SUMIFS(Sheet1!$C$1:$C${source_rows},Sheet1!$B$1:$B${source_rows},\"组{}\")", r % 16), sums[(r % 16) as usize])
        };
        report.set(r, 0, Cell { value: Value::Number(cache), formula: Some(formula.into()) });
    }
    wb.sheets.push(report);
    wb
}

fn suite_one(kind: &str, cells: usize, dir: &std::path::Path) -> String {
    let filename = format!("qa07-{kind}-{cells}.xlsx");
    let scratch = tempfile::tempdir_in(dir).expect("创建计算样本临时目录失败");
    let path = scratch.path().join(&filename);
    let t = Instant::now();
    let source_rows = if kind == "mixed" {
        let n: u32 = std::env::var("SC_PERF_SOURCE_ROWS").unwrap_or_else(|_| "256".into()).parse().expect("SC_PERF_SOURCE_ROWS must be numeric");
        assert!((1..=1_048_576).contains(&n), "source size exceeds Excel limits");
        n
    } else { 0 };
    let built = match kind {
        "values" => build_values(cells),
        "chain" => build_formulas(cells, true),
        "independent" => build_formulas(cells, false),
        "mixed" => build_mixed(cells, source_rows),
        "spill" => {
            let mut wb = build_formulas(cells, false);
            wb.sheets[0].set(0, 5, Cell { value: Value::Empty, formula: Some("=SEQUENCE(3,1)".into()) });
            sheetcore::formula::recalc_all(&mut wb);
            wb
        }
        "row_heights" => {
            let mut wb = build_formulas(cells, false);
            wb.sheets[0].rows.set_many((0..cells as u32).map(|r| (r, 24.0 + (r % 3) as f32)));
            wb
        }
        "sparse" => build_sparse(cells),
        _ => panic!("未知样本类型 {kind}"),
    };
    let build_ms = ms(t.elapsed());
    let nonempty_cells = built.sheets.iter().map(|s| s.len()).sum::<usize>();
    let rss_build = rss_mb();
    let (pages, slabs, slots) = built.sheets[0].cells_stats();

    let t = Instant::now();
    let sampler = PeakSampler::start();
    let save_used_delta = match sheetcore::xlsx::save_atomic_with_receipt(&path, &built) {
        Ok(receipt) => receipt.used_cell_delta,
        Err(e) => return format!("{{\"kind\":\"{kind}\",\"cells\":{cells},\"error\":\"save {e}\"}}"),
    };
    let save_ms = ms(t.elapsed());
    let peak_save = sampler.finish();
    let rss_save = rss_mb();
    drop(built);

    let t = Instant::now();
    let loaded = match sheetcore::xlsx::load(&path) {
        Ok(w) => w,
        Err(e) => return format!("{{\"kind\":\"{kind}\",\"cells\":{cells},\"error\":\"load {e}\"}}"),
    };
    let load_ms = ms(t.elapsed());
    let rss_load = rss_mb();

    let t = Instant::now();
    let mut wb = loaded;
    sheetcore::formula::recalc_all(&mut wb);
    let recalc_ms = ms(t.elapsed());

    // Dependency graph setup is a distinct stage from editing a value in an open workbook.
    let t = Instant::now();
    let deps = sheetcore::formula::deps::Deps::build(&wb);
    let deps_ms = ms(t.elapsed());
    let t = Instant::now();
    let cell = wb.sheets[0].get(0, 0).cloned().unwrap_or(Cell {
        value: Value::Number(0.0),
        formula: None,
    });
    let cur = match cell.value {
        Value::Number(n) => n,
        _ => 0.0,
    };
    wb.sheets[0].set(
        0,
        0,
        Cell {
            value: Value::Number(cur + 1.0),
            formula: None,
        },
    );
    let edit_formulas = sheetcore::formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    let edit_ms = ms(t.elapsed());
    if matches!(kind, "chain" | "independent") && cells > 0 {
        let expected = if kind == "chain" { cells } else { 1 };
        assert_eq!(edit_formulas, expected);
        let last = cells as u32 - 1;
        let last_value = if kind == "chain" || cells == 1 { cells + 1 } else { cells };
        assert_eq!(wb.sheets[0].value(last, 1), Value::Number(last_value as f64));
    }

    // Keep the pristine GUI fixture; its copy is outside both measured save stages.
    let pristine = dir.join(filename);
    std::fs::copy(&path, &pristine).expect("保留初始样本失败");
    let sampler = PeakSampler::start();
    let t = Instant::now();
    let save2_used_delta = match sheetcore::xlsx::save_atomic_with_receipt(&path, &wb) {
        Ok(receipt) => receipt.used_cell_delta,
        Err(e) => return format!("{{\"kind\":\"{kind}\",\"cells\":{cells},\"error\":\"save2 {e}\"}}"),
    };
    let save2_ms = ms(t.elapsed());
    let peak_save2 = sampler.finish();
    let rss = rss_mb();
    let size = std::fs::metadata(&pristine).map(|m| m.len()).unwrap_or(0) as f64 / 1_048_576.0;

    format!(
        "{{\"kind\":\"{kind}\",\"cells\":{cells},\"source_rows\":{source_rows},\"nonempty_cells\":{nonempty_cells},\"build_ms\":{build_ms:.1},\"save_ms\":{save_ms:.1},\
         \"load_ms\":{load_ms:.1},\"recalc_ms\":{recalc_ms:.1},\"deps_ms\":{deps_ms:.1},\
         \"edit_ms\":{edit_ms:.3},\"edit_formulas\":{edit_formulas},\
         \"save_used_delta\":{save_used_delta},\"save2_used_delta\":{save2_used_delta},\"peak_save2_mb\":{peak_save2:.0},\
         \"save2_ms\":{save2_ms:.1},\"file_mb\":{size:.2},\"rss_mb\":{rss:.1},\
         \"rss_build_mb\":{rss_build:.1},\"rss_save_mb\":{rss_save:.1},\"rss_load_mb\":{rss_load:.1},         \"pages\":{pages},\"slabs\":{slabs},\"slots\":{slots},\"peak_save_mb\":{peak_save:.0}}}"

    )
}

/// Two numeric columns with a header. The pristine file is also the Excel COM input.
fn measure_operations(rows: u32, dir: &std::path::Path) {
    use sheetcore::{data_tools::CompactSort, sort_filter::Filter, structural::{Structural, apply_structural_wb}};
    assert!((50_001..=1_048_574).contains(&rows));
    let mut wb = Workbook::new_blank();
    for (col, title) in ["Key", "Group"].into_iter().enumerate() {
        wb.sheets[0].set(0, col as u32, Cell { value: Value::Text(title.into()), formula: None });
    }
    for row in 1..=rows {
        let key = rows+1-row;
        for (col, value) in [key,key % 2].into_iter().enumerate() {
            wb.sheets[0].set(row,col as u32,Cell { value: Value::Number(f64::from(value)), formula: None });
        }
    }
    let path = dir.join(format!("qa07-operations-{rows}.xlsx"));
    let t = Instant::now();
    sheetcore::xlsx::save_atomic_with_receipt(&path,&wb).unwrap();
    let first_save_ms = ms(t.elapsed());
    drop(wb);
    let t = Instant::now();
    let mut wb = sheetcore::xlsx::load(&path).unwrap();
    let open_ms = ms(t.elapsed());
    let sampler = PeakSampler::start();
    let t = Instant::now();
    let sort = CompactSort::plan(&wb.sheets[0],0,(1,0,rows,1),0,true,false).unwrap().unwrap();
    let mut count = 0;
    for d in sort.deltas() {
        wb.sheets[0].set(d.row,d.col,d.after);
        count += 1;
    }
    let sort_ms = ms(t.elapsed());
    let sort_peak_mb = sampler.finish();
    assert_eq!(wb.sheets[0].value(1,0),Value::Number(1.0));
    assert_eq!(wb.sheets[0].value(rows,0),Value::Number(f64::from(rows)));
    let at = rows/2+1;
    let op = Structural::InsertRows { at, n: 1 };
    let t = Instant::now();
    let before = sheetcore::undo::snapshot_structure(&wb,0,&op);
    apply_structural_wb(&mut wb,0,&op);
    let after = before.capture_after(&wb);
    let mut undo = sheetcore::undo::UndoStack::new();
    undo.push_structure(op.label(),before,after,(0,at,0));
    let insert_ms = ms(t.elapsed());
    assert!(wb.sheets[0].get(at,0).is_none());
    let t = Instant::now();
    let mut filter = Filter::new(0,0,rows+1,1);
    filter.allows.insert(1,["1".to_owned()].into_iter().collect());
    let hidden = filter.hidden_rows(&wb.sheets[0]);
    let visible = rows+1-hidden.len() as u32;
    assert_eq!(visible,rows.div_ceil(2));
    wb.sheets[0].hidden_rows = hidden.into();
    wb.sheets[0].filter = Some(filter);
    let filter_ms = ms(t.elapsed());
    println!("{}", serde_json::json!({ "kind":"operations", "rows":rows, "file":path,
        "first_save_ms":first_save_ms, "open_ms":open_ms, "sort_ms":sort_ms, "sort_peak_mb":sort_peak_mb,
        "changed_cells":count, "insert_with_undo_ms":insert_ms, "filter_ms":filter_ms,"visible_data_rows":visible }));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("SC_PERF_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    std::fs::create_dir_all(&dir).ok();
    if args.first().is_some_and(|s| s == "--operations") {
        measure_operations(args.get(1).map_or(1_000_000, |n| n.parse().expect("invalid row count")), &dir);
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("--one") {
        let cells: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(10_000);
        let kind = args.get(2).map(|s| s.as_str()).unwrap_or("values");
        println!("{}", suite_one(kind, cells, &dir));
        return;
    }
    // 套件：每类样本测 1 万 / 10 万 / 100 万。
    println!(
        "# QA-07 perf_probe suite  dir={}  size_of::<Cell>()={}  PAGE_ROWS={}",
        dir.display(),
        std::mem::size_of::<Cell>(),
        sheetcore::store::PAGE_ROWS
    );
    for (kind, sizes) in [
        ("values", vec![10_000usize, 100_000, 1_000_000]),
        ("chain", vec![10_000, 100_000, 1_000_000]),
        ("independent", vec![10_000, 100_000, 1_000_000]),
        ("sparse", vec![10_000, 100_000, 1_000_000]),
        ("mixed", vec![10_000, 100_000, 1_000_000]),
        ("spill", vec![10_000, 100_000, 1_000_000]),
        ("row_heights", vec![10_000, 100_000, 1_000_000]),
    ] {
        for n in sizes {
            println!("{}", suite_one(kind, n, &dir));
        }
    }
}
