//! Net retained heap for dependency chains, dense cell styles, and a workbook snapshot.
use sheetcore::{Cell, Value, Workbook, formula::deps::Deps};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

struct CountingAllocator;
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let ptr = unsafe { System.realloc(ptr, layout, size) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(size as isize - layout.size() as isize, Relaxed);
        }
        ptr
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn main() {
    for rows in [10_000, 20_000] {
        let mut wb = Workbook::new_blank();
        for row in 1..=rows {
            wb.sheets[0].set(row, 0, Cell {
                value: Value::Number(0.0),
                formula: Some(format!("=A{row}+1").into()),
            });
        }
        // Drop the worker and its thread-local parser caches before sampling.
        let before = LIVE_BYTES.load(Relaxed);
        let deps = std::thread::scope(|s| s.spawn(|| Deps::build(&wb)).join().unwrap());
        let retained = LIVE_BYTES.load(Relaxed) - before;
        println!("{rows} formulas: {retained} bytes, {:.1} bytes/formula", retained as f64 / f64::from(rows));
        assert_eq!(deps.closure(&[(0, 0, 0)]).len(), rows as usize);
    }
    let mut wb = Workbook::new_blank();
    let before = LIVE_BYTES.load(Relaxed);
    for row in 0..100_000 {
        for col in 0..4 {
            wb.sheets[0].set_style_loaded(row, col, (col + 1) as u16);
        }
    }
    let styled = LIVE_BYTES.load(Relaxed);
    let snapshot = wb.clone();
    let cloned = LIVE_BYTES.load(Relaxed);
    println!("400000 explicit styles: {} bytes, clone: {} bytes", styled - before, cloned - styled);
    std::hint::black_box(snapshot);
}
