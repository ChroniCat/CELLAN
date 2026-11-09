//! 公式引擎对照：拿 Excel 存进文件的缓存值，和本引擎重算的结果逐格比。
//!
//! 用法：verify_formulas <xlsx> [报告.csv]
//!
//! Excel 保存时会把每个公式算好的结果一起写进文件。所以只要读出来、先记下缓存值，
//! 再整份重算，就能知道"我们和 Excel 差在哪"——这是唯一客观的兼容性指标。

use std::collections::HashMap;
use std::io::Write;

/// Excel 缓存里的错误值是内部短名：#Div0/#NA/#Value/#Name/#Ref/#Num/#Null
fn canon_err(s: &str) -> &str {
    match s.trim() {
        "#Div0" => "#DIV/0!",
        "#NA" => "#N/A",
        "#Value" => "#VALUE!",
        "#Name" => "#NAME?",
        "#Ref" => "#REF!",
        "#Num" => "#NUM!",
        "#Null" => "#NULL!",
        "#GettingData" => "#GETTING_DATA",
        other => other,
    }
}

fn is_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

/// 数值比较：相对误差 1e-9，或者两边都是 0
fn num_close(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let diff = (a - b).abs();
    let scale = a.abs().max(b.abs()).max(1.0);
    diff / scale < 1e-9
}

/// 从公式里取最外层函数名（"=SUM(A1:B2)+3" → "SUM"）
fn outer_function(formula: &str) -> String {
    let f = formula.trim_start_matches('=');
    // 找到第一个 '(' 之前的标识符
    let mut name = String::new();
    for ch in f.chars() {
        if ch.is_ascii_alphabetic() || ch == '.' || ch == '_' {
            name.push(ch.to_ascii_uppercase());
        } else if ch == '(' {
            break;
        } else if !name.is_empty() {
            // 说明不是以函数开头（比如 A1+SUM(...)）
            return "(表达式)".into();
        }
    }
    if name.is_empty() {
        "(表达式)".into()
    } else {
        name
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("用法: verify_formulas <xlsx> [报告.csv]");
        return;
    };
    let report_path = args.get(2).cloned();

    let t0 = std::time::Instant::now();
    let mut wb = match sheetcore::xlsx::load(std::path::Path::new(path)) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("读入失败: {e:#}");
            return;
        }
    };
    eprintln!("读入 {:.2}s：{} 张表", t0.elapsed().as_secs_f64(), wb.sheets.len());

    // 1) 先记下 Excel 的缓存值
    type Key = (usize, u32, u32);
    let mut cached: HashMap<Key, String> = HashMap::new();
    let mut formulas = 0usize;
    for (si, sheet) in wb.sheets.iter().enumerate() {
        for (k, cell) in sheet.iter() {
            if cell.formula.is_some() {
                formulas += 1;
                cached.insert((si, k.row, k.col), cell.value.display());
            }
        }
    }
    eprintln!("公式格 {formulas} 个，缓存值已记录（{:.2}s）", t0.elapsed().as_secs_f64());

    // 2) 整份重算
    let t1 = std::time::Instant::now();
    sheetcore::formula::recalc_all(&mut wb);
    eprintln!("重算 {:.2}s", t1.elapsed().as_secs_f64());

    // 3) 逐格对比
    // 循环引用的格子：Excel 缓存值是历史快照，本引擎按循环处理，不参与比对
    let cycles = sheetcore::formula::take_cycle_cells();
    let mut circular = 0usize;
    let mut same = 0usize;
    let mut close = 0usize;
    let mut diff = 0usize;
    let mut missing = 0usize;
    let mut no_cache = 0usize;
    let mut by_func: HashMap<String, usize> = HashMap::new();
    let mut samples: Vec<String> = Vec::new();
    let mut csv = report_path
        .as_ref()
        .and_then(|p| std::fs::File::create(p).ok())
        .map(std::io::BufWriter::new);

    if let Some(f) = csv.as_mut() {
        let _ = writeln!(f, "工作表,单元格,公式,Excel缓存值,本引擎值,函数");
    }

    for (si, sheet) in wb.sheets.iter().enumerate() {
        for (k, cell) in sheet.iter() {
            let Some(f) = &cell.formula else { continue };
            let Some(expect) = cached.get(&(si, k.row, k.col)) else {
                missing += 1;
                continue;
            };
            if cycles.contains(&(si, k.row, k.col)) {
                circular += 1;
                continue;
            }
            let got_raw = cell.value.display();
            let expect_x = canon_err(expect).to_string();
            let got_x = canon_err(&got_raw).to_string();
            let (expect, got) = (&expect_x, &got_x);
            // 文件里没写缓存值（常见于脚本生成的工作簿）就没法对照，单独计数
            if expect.trim().is_empty() && !got.trim().is_empty() {
                no_cache += 1;
                continue;
            }
            let ok = match (is_number(expect), is_number(&got)) {
                (Some(a), Some(b)) => {
                    if num_close(a, b) {
                        if expect == got {
                            same += 1;
                        } else {
                            close += 1;
                        }
                        true
                    } else {
                        false
                    }
                }
                _ => {
                    if expect == got {
                        same += 1;
                        true
                    } else {
                        false
                    }
                }
            };
            if !ok {
                diff += 1;
                let func = outer_function(f);
                *by_func.entry(func.clone()).or_insert(0) += 1;
                if samples.len() < 40 {
                    samples.push(format!(
                        "{}!{}{} | {f} | Excel={expect:?} 我们={got:?} | {func}",
                        sheet.name,
                        sheetcore::col_label(k.col),
                        k.row + 1
                    ));
                }
                if let Some(w) = csv.as_mut() {
                    let _ = writeln!(
                        w,
                        "{},{}{},{},{},{},{}",
                        sheet.name,
                        sheetcore::col_label(k.col),
                        k.row + 1,
                        f.replace(',', "，"),
                        expect.replace(',', "，"),
                        got.replace(',', "，"),
                        func
                    );
                }
            }
        }
    }

    let total = same + close + diff;
    println!();
    println!("=== 公式对照结果 ===");
    println!("公式格         {formulas}");
    println!("参与比较       {total}");
    println!("无缓存值       {no_cache}（文件里没存 Excel 结果，无法对照）");
    println!("完全一致       {same}");
    println!("数值一致(容差) {close}");
    println!("不一致         {diff}");
    println!("循环引用       {circular}（Excel 缓存是历史快照，单列不参与比例）");
    if total > 0 {
        println!(
            "一致率         {:.4}%",
            (same + close) as f64 / total as f64 * 100.0
        );
    }
    if !by_func.is_empty() {
        let mut list: Vec<(String, usize)> = by_func.into_iter().collect();
        list.sort_by(|a, b| b.1.cmp(&a.1));
        println!();
        println!("--- 不一致按函数分布（前 15）---");
        for (f, n) in list.iter().take(15) {
            println!("{f:<16} {n}");
        }
    }
    if !samples.is_empty() {
        println!();
        println!("--- 样例（前 {}）---", samples.len());
        for s in samples {
            println!("{s}");
        }
    }
    if let Some(p) = report_path {
        println!();
        println!("完整清单已写入 {p}");
    }
}
