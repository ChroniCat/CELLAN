//! QA-06.2：动态数组溢出块与本机 Excel 16.0 实算对拍 + 全量重算/增量重算一致。
//!
//! 夹具 `work/qa06a/qa06a.xlsx`（`scripts/qa06_array_fixture.py`）：
//! ```text
//! B1:B5 = 3,1,2,5,4      C1:C5 = b,a,c,e,d
//! 数组公式锚点 E1 / E10 / E20 …（跨 9 行以上，溢出块互不覆盖）
//! ```
//! `crates/core/tests/fixtures/qa06a/blocks.tsv` 由 `scripts/qa06_array_probe.ps1` 用 Excel 实算后导出：
//! `anchor<TAB>formula<TAB>rows<TAB>cols<TAB>v(0,0)<TAB>…`
//!
//! 本机 Excel 16.0 (Build 20326) 不支持 `TAKE`/`DROP`/`HSTACK`/`VSTACK`（返回 `#NAME?`），
//! 这几条在矩阵里标记 SKIP，不作为通过也不作为失败。

use sheetcore::formula::{self, deps};
use sheetcore::{Cell, Value, Workbook};

const SKIP_MARK: &str = "#NAME?";

fn num(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn f(formula: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(formula.into()) }
}

fn book(blocks: &[(u32, String)]) -> Workbook {
    let mut wb = Workbook::new_blank();
    let s = &mut wb.sheets[0];
    for (i, v) in [3.0, 1.0, 2.0, 5.0, 4.0].iter().enumerate() {
        s.set(i as u32, 1, num(*v));
    }
    for (i, v) in ["b", "a", "c", "e", "d"].iter().enumerate() {
        s.set(i as u32, 2, text(v));
    }
    for (row, formula) in blocks {
        // E 列 = 第 4 列，行号 1 基
        s.set(row - 1, 4, f(formula));
    }
    wb
}

fn render(v: &Value) -> String {
    match v {
        Value::Number(n) => {
            if n.fract().abs() < f64::EPSILON && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        Value::Text(s) => s.to_string(),
        Value::Bool(b) => {
            if *b {
                "TRUE".into()
            } else {
                "FALSE".into()
            }
        }
        Value::Empty => String::new(),
        Value::Error(e) => format!("{e:?}"),
    }
}

fn err_text(e: &sheetcore::formula::ErrKind) -> &'static str {
    use sheetcore::formula::ErrKind::*;
    match e {
        Na => "#N/A",
        Value => "#VALUE!",
        Div0 => "#DIV/0!",
        Ref => "#REF!",
        Name => "#NAME?",
        Num => "#NUM!",
        Null => "#NULL!",
        _ => "#?",
    }
}

struct Block {
    anchor: String,
    formula: String,
    rows: usize,
    cols: usize,
    values: Vec<String>,
}

fn parse_blocks() -> Vec<Block> {
    let tsv = include_str!("fixtures/qa06a/blocks.tsv").trim_start_matches('\u{feff}');
    let mut out = Vec::new();
    for line in tsv.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 4 {
            continue;
        }
        let rows: usize = cols[2].parse().unwrap_or(0);
        let cols_n: usize = cols[3].parse().unwrap_or(0);
        out.push(Block {
            anchor: cols[0].to_string(),
            formula: cols[1].to_string(),
            rows,
            cols: cols_n,
            values: cols[4..].iter().map(|s| s.to_string()).collect(),
        });
    }
    out
}

#[test]
fn qa06_2_spill_blocks_match_excel() {
    let blocks = parse_blocks();
    assert!(blocks.len() >= 10, "数组块太少：{}", blocks.len());

    let anchors: Vec<(u32, String)> = blocks
        .iter()
        .map(|b| {
            let row: u32 = b.anchor[1..].parse().unwrap();
            (row, b.formula.clone())
        })
        .collect();
    let mut wb = book(&anchors);
    formula::recalc_all(&mut wb);

    let mut checked = 0usize;
    let mut skipped = Vec::new();
    let mut diffs = Vec::new();

    for b in &blocks {
        if b.values.len() == 1 && b.values[0] == SKIP_MARK {
            skipped.push(format!("{} {}", b.anchor, b.formula));
            continue;
        }
        let row: u32 = b.anchor[1..].parse().unwrap();
        for r in 0..b.rows {
            for c in 0..b.cols {
                let v = wb.sheets[0].value(row - 1 + r as u32, 4 + c as u32);
                let got = match &v {
                    Value::Error(e) => err_text(e).to_string(),
                    other => render(other),
                };
                let want = &b.values[r * b.cols + c];
                checked += 1;
                if &got != want {
                    // 数字按容差再判一次
                    let ok = match (&v, want.parse::<f64>()) {
                        (Value::Number(n), Ok(e)) => (*n - e).abs() <= 1e-9 * e.abs().max(1.0),
                        _ => false,
                    };
                    if !ok {
                        diffs.push(format!(
                            "{} {} 格({},{}) 本引擎 {:?} != Excel {:?}",
                            b.anchor, b.formula, r, c, got, want
                        ));
                    }
                }
            }
        }
    }

    assert!(checked >= 30, "核对格数太少：{checked}");
    assert!(
        diffs.is_empty(),
        "与 Excel 不一致 {} 格：\n{}",
        diffs.len(),
        diffs.join("\n")
    );
    eprintln!("[qa06a] 核对 {checked} 格；跳过 {} 条：{:?}", skipped.len(), skipped);
}

#[test]
fn qa06_2_incremental_matches_full_recalc() {
    let blocks = parse_blocks();
    let anchors: Vec<(u32, String)> = blocks
        .iter()
        .map(|b| {
            let row: u32 = b.anchor[1..].parse().unwrap();
            (row, b.formula.clone())
        })
        .collect();

    // 全量：B1 已经是 10
    let mut full = book(&anchors);
    full.sheets[0].set(0, 1, num(10.0));
    formula::recalc_all(&mut full);

    // 增量：先按 B1=3 全量算一遍，再把 B1 改成 10 只重算受影响格
    let mut inc = book(&anchors);
    formula::recalc_all(&mut inc);
    inc.sheets[0].set(0, 1, num(10.0));
    let deps = deps::Deps::build(&inc);
    let n = formula::recalc_dirty(&mut inc, &deps, &[(0, 1, 0)]);
    assert!(n > 0, "增量重算没有命中任何公式");

    let mut diffs = Vec::new();
    let mut cells = 0usize;
    for row in 0..130u32 {
        for col in 0..8u32 {
            let a = full.sheets[0].value(row, col);
            let b = inc.sheets[0].value(row, col);
            cells += 1;
            if render(&a) != render(&b) {
                diffs.push(format!("({row},{col}) 全量 {:?} != 增量 {:?}", render(&a), render(&b)));
            }
        }
    }
    assert!(
        diffs.is_empty(),
        "增量与全量不一致 {} 格：\n{}",
        diffs.len(),
        diffs.join("\n")
    );
    eprintln!("[qa06a] 增量重算 {n} 格，{cells} 格与全量一致");
}
