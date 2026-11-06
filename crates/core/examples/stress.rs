//! 大文件压测：生成 → 打开 → 重算 → 保存，全程计时
use sheetcore::{xlsx, Cell, Value, Workbook};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().cloned().unwrap_or_else(|| "bench".into());

    if mode == "gen" {
        // gen <路径> <行数> [公式列数]
        let path = &args[1];
        let rows: u32 = args[2].parse().unwrap();
        let with_formula = args.get(3).map(|s| s == "f").unwrap_or(false);
        let t = Instant::now();
        let mut wb = Workbook::new_blank();
        for r in 0..rows {
            wb.sheets[0].set(r, 0, Cell { value: Value::Number(r as f64 + 1.0), formula: None });
            wb.sheets[0].set(r, 1, Cell { value: Value::Text(format!("客户{}", r % 1000).into()), formula: None });
            wb.sheets[0].set(r, 2, Cell { value: Value::Number((r % 500) as f64 * 1.5), formula: None });
            wb.sheets[0].set(r, 3, Cell { value: Value::Number((r % 37) as f64), formula: None });
            if with_formula {
                wb.sheets[0].set(r, 4, Cell {
                    value: Value::Number(0.0),
                    formula: Some(format!("=C{0}*D{0}", r + 1).into()),
                });
                wb.sheets[0].set(r, 5, Cell {
                    value: Value::Number(0.0),
                    formula: Some(format!("=IF(E{0}>100,E{0}*0.9,E{0})", r + 1).into()),
                });
            } else {
                wb.sheets[0].set(r, 4, Cell { value: Value::Number(r as f64 * 0.5), formula: None });
            }
        }
        let gen_time = t.elapsed().as_secs_f64();
        let t2 = Instant::now();
        if let Err(e) = xlsx::save(std::path::Path::new(path), &wb) {
            eprintln!("保存失败: {e}");
            std::process::exit(1);
        }
        let save_time = t2.elapsed().as_secs_f64();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        println!(
            "生成 {rows} 行 ({:.1} 万单元格)  建表 {:.2}s  写盘 {:.2}s  文件 {:.1} MB",
            rows as f64 * 5.0 / 10000.0,
            gen_time,
            save_time,
            size as f64 / 1048576.0
        );
        return;
    }

    // edit <路径>：模拟用户改格子，测增量重算耗时
    if mode == "edit" {
        let path = &args[1];
        let mut wb = xlsx::load(std::path::Path::new(path)).expect("load");
        let t = Instant::now();
        sheetcore::formula::recalc_all(&mut wb);
        println!("首次全量重算 {:.2}s", t.elapsed().as_secs_f64());
        let t = Instant::now();
        let deps = sheetcore::formula::deps::Deps::build(&wb);
        println!("建依赖图 {:.2}s，{} 条公式", t.elapsed().as_secs_f64(), deps.refs.len());

        // 1. 改明细里的数量（只影响本行几条公式）
        let mut worst = 0.0f64;
        for i in 0..20u32 {
            let r = (i * 731) % 15000;
            wb.sheets[1].set(r, 3, Cell { value: Value::Number((i as f64 + 1.0) * 7.0), formula: None });
            let t = Instant::now();
            let n = sheetcore::formula::recalc_dirty(&mut wb, &deps, &[(1, r, 3)]);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            worst = worst.max(ms);
            if i < 5 {
                println!("  改 明细!D{:<6} 重算 {:>4} 格  {:>6.1} ms", r + 1, n, ms);
            }
        }
        println!("20 次编辑：最慢 {:.1} ms", worst);

        // 2. 改基础数据里的数量（会被明细的 VLOOKUP 引用）
        let t = Instant::now();
        wb.sheets[0].set(100, 3, Cell { value: Value::Number(999.0), formula: None });
        let n = sheetcore::formula::recalc_dirty(&mut wb, &deps, &[(0, 100, 3)]);
        println!("改 基础数据!D101：重算 {n} 格，{:.1} ms", t.elapsed().as_secs_f64() * 1000.0);

        // 正确性校验：增量算完的结果，必须和「整表重算」完全一致
        let snapshot: Vec<Vec<(u32, u32, String)>> = wb
            .sheets
            .iter()
            .map(|s| s.iter().map(|(k, c)| (k.row, k.col, format!("{:?}", c.value))).collect())
            .collect();
        sheetcore::formula::recalc_all(&mut wb);
        let mut diffs = 0usize;
        for (si, sheet) in wb.sheets.iter().enumerate() {
            for (k, c) in sheet.iter() {
                let before = snapshot[si]
                    .iter()
                    .find(|(r, col, _)| *r == k.row && *col == k.col)
                    .map(|(_, _, v)| v.clone());
                if let Some(b) = before {
                    if b != format!("{:?}", c.value) {
                        if diffs < 5 {
                            println!("  差异 R{}C{}: 增量={} 全量={:?}", k.row + 1, k.col + 1, b, c.value);
                        }
                        diffs += 1;
                    }
                }
            }
        }
        println!("一致性校验：{diffs} 处差异（0 表示增量结果与全量重算完全一致）");
        return;
    }

    // collab <路径> [操作数]：模拟远端批量操作，测增量重算
    if mode == "collab" {
        use sheetcore::sync::{Peer, ValueWire};
        let path = &args[1];
        let n_ops: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
        let mut wb = xlsx::load(std::path::Path::new(path)).expect("load");
        sheetcore::formula::recalc_all(&mut wb);
        let mut deps = sheetcore::formula::deps::Deps::build(&wb);

        // 模拟「别人」发来的操作：站点号 7，时间戳递增
        let mut peer = Peer::new(1);
        let mut touched: Vec<(u32, u32)> = Vec::new();
        for i in 0..n_ops {
            let r = (i * 37) % 15000;
            let op = sheetcore::sync::Op {
                sheet: 1,
                row: r,
                col: 3,
                value: ValueWire::Number((i as f64 + 1.0) * 3.0),
                formula: None,
                ts: i as u64 + 1,
                site: 7,
            };
            if peer.apply(op) {
                touched.push((r, 3));
            }
        }
        let t = Instant::now();
        let keys: Vec<(usize, u32, u32)> = touched.iter().map(|(r, c)| (1usize, *r, *c)).collect();
        peer.apply_list_to_workbook(&mut wb, &keys);
        let seeds: Vec<sheetcore::formula::deps::CellKey> = keys.clone();
        for k in &seeds {
            deps.update_cell(&wb, *k);
        }
        let cells = sheetcore::formula::recalc_dirty(&mut wb, &deps, &seeds);
        let dt = t.elapsed().as_secs_f64();
        println!(
            "远端批量 {n_ops} 条操作 → 重算 {cells} 格，{:.0} ms（全量重算要 ~17000 ms）",
            dt * 1000.0
        );
        let _ = ValueWire::Number(0.0);
        return;
    }

    // bench <路径>
    let path = &args[1];
    let t = Instant::now();
    let mut wb = match xlsx::load(std::path::Path::new(path)) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("打开失败: {e}");
            std::process::exit(1);
        }
    };
    let open_time = t.elapsed().as_secs_f64();
    let cells: usize = wb.sheets.iter().map(|s| s.len()).sum();

    let t2 = Instant::now();
    sheetcore::formula::recalc_all(&mut wb);
    let recalc_time = t2.elapsed().as_secs_f64();

    let out = format!("{path}.out.xlsx");
    let t3 = Instant::now();
    if let Err(e) = xlsx::save(std::path::Path::new(&out), &wb) {
        eprintln!("保存失败: {e}");
        std::process::exit(1);
    }
    let save_time = t3.elapsed().as_secs_f64();

    println!(
        "{cells} 个单元格  打开 {:.2}s  重算 {:.2}s  另存 {:.2}s",
        open_time, recalc_time, save_time
    );
}
