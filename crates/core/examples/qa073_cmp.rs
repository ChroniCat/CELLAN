//! QA-07.3 保存正确性对拍：两个 xlsx 的全部单元格（值 + 公式文本）必须一致。
//!
//! 用法：cargo run --release -p sheetcore --example qa073_cmp -- <a.xlsx> <b.xlsx>
use sheetcore::{Value, Workbook};
use std::collections::BTreeMap;
use std::path::Path;

fn snap(p: &Path) -> BTreeMap<(usize, u32, u32), (String, String)> {
    let wb: Workbook = sheetcore::xlsx::load(p).unwrap_or_else(|e| panic!("load {}: {e}", p.display()));
    let mut m = BTreeMap::new();
    for (si, sh) in wb.sheets.iter().enumerate() {
        for (cref, cell) in sh.iter() {
            let (row, col) = (cref.row, cref.col);
            let v = match &cell.value {
                Value::Empty => String::new(),
                Value::Number(n) => format!("n:{n:.12}"),
                Value::Text(t) => format!("s:{t}"),
                Value::Bool(b) => format!("b:{b}"),
                Value::Error(e) => format!("e:{e:?}"),
            };
            let f = cell.formula.as_deref().unwrap_or("").to_string();
            m.insert((si, row, col), (v, f));
        }
    }
    m
}

fn main() {
    let a = std::env::args().nth(1).expect("a.xlsx");
    let b = std::env::args().nth(2).expect("b.xlsx");
    let ma = snap(Path::new(&a));
    let mb = snap(Path::new(&b));
    let fcount = |m: &BTreeMap<(usize, u32, u32), (String, String)>| {
        m.values().filter(|(_, f)| !f.is_empty()).count()
    };
    println!(
        "cells {} vs {}, formulas {} vs {}",
        ma.len(),
        mb.len(),
        fcount(&ma),
        fcount(&mb)
    );
    let mut diffs = Vec::new();
    for (k, va) in &ma {
        match mb.get(k) {
            None => diffs.push(format!("missing {k:?} -> {va:?}")),
            Some(vb) if vb != va => diffs.push(format!("diff {k:?}: {va:?} vs {vb:?}")),
            Some(_) => {}
        }
        if diffs.len() > 20 {
            break;
        }
    }
    for (k, vb) in &mb {
        if !ma.contains_key(k) {
            diffs.push(format!("extra {k:?} -> {vb:?}"));
            if diffs.len() > 40 {
                break;
            }
        }
    }
    if diffs.is_empty() {
        println!("RESULT PASS 全部单元格一致");
    } else {
        println!("RESULT FAIL {} 处", diffs.len());
        for d in diffs.iter().take(20) {
            println!("  {d}");
        }
        std::process::exit(1);
    }
}
