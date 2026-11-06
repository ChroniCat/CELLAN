//! 测模型内存占用：建 N 行 × 5 列（数字/文本/公式混合），比较私有内存增量
use sheetcore::{Cell, Value, Workbook};

fn private_mb() -> f64 {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut pmc: PROCESS_MEMORY_COUNTERS = MaybeUninit::zeroed().assume_init();
        let ok = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        );
        if ok == 0 {
            return 0.0;
        }
        pmc.PagefileUsage as f64 / 1048576.0
    }
}

fn main() {
    println!("size_of::<Cell>()  = {} 字节", std::mem::size_of::<Cell>());
    println!("size_of::<Value>() = {} 字节", std::mem::size_of::<Value>());

    let n: u32 = 200_000;
    let base = private_mb();
    let mut wb = Workbook::new_blank();
    for r in 0..n {
        wb.sheets[0].set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        wb.sheets[0].set(r, 1, Cell {
            value: Value::Text(format!("客户{:04}", r % 1000).into()),
            formula: None,
        });
        wb.sheets[0].set(r, 2, Cell { value: Value::Number((r % 500) as f64 * 1.5), formula: None });
        wb.sheets[0].set(r, 3, Cell {
            value: Value::Text(format!("产品{:03}", r % 300).into()),
            formula: None,
        });
        wb.sheets[0].set(r, 4, Cell {
            value: Value::Number(0.0),
            formula: Some(format!("=A{}*C{}", r + 1, r + 1).into()),
        });
    }
    let after = private_mb();
    let cells = (n * 5) as f64;
    let delta = after - base;
    println!(
        "{} 万个单元格：私有内存 {:.1} → {:.1} MB（+{:.1} MB，{:.0} 字节/格）",
        cells / 10000.0,
        base,
        after,
        delta,
        delta * 1048576.0 / cells
    );
    std::hint::black_box(&wb);
}
