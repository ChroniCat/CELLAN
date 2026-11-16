//! QA-07 性能基线：打开 / 重算 / 改一格 / 保存 的端到端耗时与内存峰值。
//!
//! 用法（release）：
//!   cargo run --release -p sheetcore --example perf_probe -- --suite
//!   cargo run --release -p sheetcore --example perf_probe -- --one 100000 values
//!
//! 输出每行一个 JSON，字段：kind/cells/build_ms/save_ms/load_ms/recalc_ms/edit_ms/save2_ms/rss_mb
use sheetcore::{Cell, Value, Workbook};
use std::time::Instant;

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

/// 公式链：A 列数值，B.. 每行 = 上一列 + 1，链长 = cells
fn build_chain(cells: usize) -> Workbook {
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
                value: Value::Number(0.0),
                formula: Some(format!("=A{}+1", r + 1).into()),
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

fn suite_one(kind: &str, cells: usize, wb: Workbook, dir: &std::path::Path) -> String {
    let path = dir.join(format!("qa07-{kind}-{cells}.xlsx"));
    let t = Instant::now();
    let built = match kind {
        "values" => build_values(cells),
        "chain" => build_chain(cells),
        "sparse" => build_sparse(cells),
        _ => panic!("未知样本类型 {kind}"),
    };
    let build_ms = ms(t.elapsed());
    let rss_build = rss_mb();
    let (pages, slabs, slots) = built.sheets[0].cells_stats();
    drop(wb);

    let t = Instant::now();
    if let Err(e) = sheetcore::xlsx::save(&path, &built) {
        return format!("{{\"kind\":\"{kind}\",\"cells\":{cells},\"error\":\"save {e}\"}}");
    }
    let save_ms = ms(t.elapsed());
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

    // 改一格：A1 加 1，再全量重算（模拟用户编辑后的连锁）
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
    sheetcore::formula::recalc_all(&mut wb);
    let edit_ms = ms(t.elapsed());

    let t = Instant::now();
    let out2 = dir.join(format!("qa07-{kind}-{cells}-edited.xlsx"));
    if let Err(e) = sheetcore::xlsx::save(&out2, &wb) {
        return format!("{{\"kind\":\"{kind}\",\"cells\":{cells},\"error\":\"save2 {e}\"}}");
    }
    let save2_ms = ms(t.elapsed());
    let rss = rss_mb();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1_048_576.0;
    let _ = std::fs::remove_file(&out2);

    format!(
        "{{\"kind\":\"{kind}\",\"cells\":{cells},\"build_ms\":{build_ms:.1},\"save_ms\":{save_ms:.1},\
         \"load_ms\":{load_ms:.1},\"recalc_ms\":{recalc_ms:.1},\"edit_ms\":{edit_ms:.1},\
         \"save2_ms\":{save2_ms:.1},\"file_mb\":{size:.2},\"rss_mb\":{rss:.1},\
         \"rss_build_mb\":{rss_build:.1},\"rss_save_mb\":{rss_save:.1},\"rss_load_mb\":{rss_load:.1},         \"pages\":{pages},\"slabs\":{slabs},\"slots\":{slots}}}"

    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("SC_PERF_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    std::fs::create_dir_all(&dir).ok();
    if args.first().map(|s| s.as_str()) == Some("--one") {
        let cells: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(10_000);
        let kind = args.get(2).map(|s| s.as_str()).unwrap_or("values");
        println!("{}", suite_one(kind, cells, Workbook::new_blank(), &dir));
        return;
    }
    // 套件：三类样本 × 三档规模（1 万 / 10 万 / 100 万）
    println!(
        "# QA-07 perf_probe suite  dir={}  size_of::<Cell>()={}  PAGE_ROWS={}",
        dir.display(),
        std::mem::size_of::<Cell>(),
        sheetcore::store::PAGE_ROWS
    );
    for (kind, sizes) in [
        ("values", vec![10_000usize, 100_000, 1_000_000]),
        ("chain", vec![10_000, 100_000, 1_000_000]),
        ("sparse", vec![10_000, 100_000, 1_000_000]),
    ] {
        for n in sizes {
            println!("{}", suite_one(kind, n, Workbook::new_blank(), &dir));
        }
    }
}
