//! C01.3 FORMULA 缓存值的真实布局（MS-XLS Xnum）与增量/全量一致性。
//!
//! 这里不经过我们自己的写端，直接按规范字节造 BIFF 记录喂给 `read_stream`，
//! 避免「读写两端一起错、互相自洽」的假通过。

use sheetcore::formula::{self, ErrKind};
use sheetcore::xls_read::read_stream;
use sheetcore::{Cell, Value, Workbook};

fn rec(t: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + body.len());
    v.extend_from_slice(&t.to_le_bytes());
    v.extend_from_slice(&(body.len() as u16).to_le_bytes());
    v.extend_from_slice(body);
    v
}

fn formula_body(r: u16, c: u16, num: [u8; 8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&r.to_le_bytes());
    b.extend_from_slice(&c.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // ixfe
    b.extend_from_slice(&num);
    b.extend_from_slice(&0u16.to_le_bytes()); // grbit
    b.extend_from_slice(&0u32.to_le_bytes()); // chn
    b.extend_from_slice(&0u16.to_le_bytes()); // cce
    b
}

fn string_body(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut b = Vec::new();
    b.extend_from_slice(&(units.len() as u16).to_le_bytes());
    b.push(1); // UTF-16LE
    for u in units {
        b.extend_from_slice(&u.to_le_bytes());
    }
    b
}

/// 造一条只有 BOUNDSHEET + BOF + 若干记录 + EOF 的流
fn stream(records: &[(u16, Vec<u8>)]) -> Vec<u8> {
    // BOUNDSHEET 记录固定 13 字节，所以 BOF 从 13 开始
    let mut bs = Vec::new();
    bs.extend_from_slice(&13u32.to_le_bytes()); // BOF 的偏移
    bs.extend_from_slice(&0u16.to_le_bytes()); // grbit
    bs.push(1); // cch
    bs.push(0); // 压缩（ASCII）
    bs.push(b'S');
    let mut out = rec(0x0085, &bs);
    out.extend_from_slice(&rec(0x0809, &[0u8; 16]));
    for (t, b) in records {
        out.extend_from_slice(&rec(*t, b));
    }
    out.extend_from_slice(&rec(0x000A, &[]));
    out
}

const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;

fn special(ty: u8, val: u8) -> [u8; 8] {
    let mut n = [0u8; 8];
    n[0] = ty;
    n[2] = val;
    n[6] = 0xFF;
    n[7] = 0xFF;
    n
}

fn cells(records: &[(u16, Vec<u8>)]) -> Vec<(u32, u32, Value)> {
    let sheets = read_stream(&stream(records)).expect("读流失败");
    sheets[0].cells.iter().map(|(r, c, cell)| (*r, *c, cell.value.clone())).collect()
}

#[test]
fn a_plain_number_whose_low_word_is_ffff_survives() {
    // f64::from_bits(0x3ff000000000ffff) = 1.0000000000145517
    // 小端字节 ff ff 00 00 00 00 f0 3f —— 低 16 位正好是 0xFFFF
    let n = f64::from_bits(0x3ff000000000_ffff);
    assert_eq!(n.to_le_bytes(), [0xff, 0xff, 0, 0, 0, 0, 0xf0, 0x3f]);
    let got = cells(&[(FORMULA, formula_body(0, 0, n.to_le_bytes()))]);
    assert_eq!(got, vec![(0, 0, Value::Number(n))], "低 16 位是 0xFFFF 的普通数值被当成特殊值丢了");
}

#[test]
fn special_results_use_the_high_word_marker() {
    let got = cells(&[
        (FORMULA, formula_body(0, 0, special(2, 0x07))), // #DIV/0!
        (FORMULA, formula_body(1, 0, special(2, 0x2A))), // #N/A
        (FORMULA, formula_body(2, 0, special(1, 1))),     // TRUE
        (FORMULA, formula_body(3, 0, special(1, 0))),     // FALSE
        (FORMULA, formula_body(4, 0, special(3, 0))),     // 空
        (FORMULA, formula_body(5, 0, 42.5f64.to_le_bytes())),
        (FORMULA, formula_body(6, 0, 0.0f64.to_le_bytes())),
    ]);
    assert_eq!(got[0].2, Value::Error(ErrKind::Div0));
    assert_eq!(got[1].2, Value::Error(ErrKind::Na));
    assert_eq!(got[2].2, Value::Bool(true));
    assert_eq!(got[3].2, Value::Bool(false));
    assert!(!got.iter().any(|(r, _, _)| *r == 4), "空结果不该落格");
    // 空结果不落格，所以后面的下标要按实际长度取
    assert_eq!(got.len(), 6);
    assert_eq!(got[4], (5, 0, Value::Number(42.5)));
    assert_eq!(got[5], (6, 0, Value::Number(0.0)));
}

#[test]
fn a_string_result_is_read_from_the_following_string_record() {
    let got = cells(&[
        (FORMULA, formula_body(0, 0, special(0, 0))),
        (STRING, string_body("文本")),
    ]);
    assert_eq!(got, vec![(0, 0, Value::Text("文本".into()))], "STRING 记录没有接上 FORMULA");
}

#[test]
fn our_own_xls_roundtrip_covers_every_cached_type() {
    let dir = std::env::temp_dir().join(format!("c013-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cached.xls");

    let tricky = f64::from_bits(0x3ff000000000_ffff);
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "缓存".into();
    let mut put = |r: u32, v: Value, f: &str| {
        wb.sheets[0].set(r, 0, Cell { value: v, formula: Some(f.into()) });
    };
    put(0, Value::Number(tricky), "=1/1");
    put(1, Value::Error(ErrKind::Div0), "=1/0");
    put(2, Value::Bool(true), "=1=1");
    put(3, Value::Text("结果".into()), "=\"结果\"");
    put(4, Value::Empty, "=\"\"");

    sheetcore::xls_write::write(&path, &wb).expect("写 .xls 失败");
    let back = sheetcore::xls_read::load(&path).expect("读 .xls 失败");
    let s = &back.sheets[0];
    assert_eq!(s.value(0, 0), Value::Number(tricky), "低 16 位 0xFFFF 的数值存盘后丢了");
    assert_eq!(s.value(1, 0), Value::Error(ErrKind::Div0));
    assert_eq!(s.value(2, 0), Value::Bool(true));
    assert_eq!(s.value(3, 0), Value::Text("结果".into()));
}

// ---------------------------------------------------------------- 增量 vs 全量

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "汇总".into();
    let mut other = sheetcore::model::Sheet::new("数据");
    // 七种传统错误 + SPILL
    let defs = [
        "=1/0", "=NA()", "=SQRT(-1)", "=NOPE()", "=\"a\"+1", "=#REF!", "=SUM(A1 A2)",
    ];
    for (i, f) in defs.iter().enumerate() {
        other.set(i as u32, 0, Cell { value: Value::Empty, formula: Some((*f).into()) });
    }
    // SPILL：A9 溢出，B9 被占
    other.set(8, 0, Cell { value: Value::Empty, formula: Some("=SEQUENCE(2,1)".into()) });
    other.set(8, 1, Cell { value: Value::Number(1.0), formula: None });
    // 依赖链：跨表引用错误值
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=数据!A1".into()) });
    wb.sheets[0].set(1, 0, Cell { value: Value::Empty, formula: Some("=A1&\"x\"".into()) });
    wb.sheets[0].set(2, 0, Cell { value: Value::Empty, formula: Some("=IFERROR(A1,7)".into()) });
    wb.sheets[0].set(3, 0, Cell { value: Value::Empty, formula: Some("=SUM(A1,2)".into()) });
    wb.sheets.push(other);
    formula::recalc_all(&mut wb);
    wb
}

fn snapshot(wb: &Workbook) -> Vec<(usize, u32, u32, Value)> {
    let mut out = Vec::new();
    for (si, s) in wb.sheets.iter().enumerate() {
        for (cref, cell) in s.iter() {
            if cell.formula.is_some() {
                out.push((si, cref.row, cref.col, cell.value.clone()));
            }
        }
    }
    out.sort_by_key(|(si, r, c, _)| (*si, *r, *c));
    out
}

#[test]
fn full_recalc_gives_the_expected_three_stage_behavior() {
    let mut wb = book();
    // 阶段 1：全算
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Div0));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Error(ErrKind::Div0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(7.0), "IFERROR 应抓住跨表错误");
    assert_eq!(wb.sheets[0].value(3, 0), Value::Error(ErrKind::Div0), "SUM 不能吞错误");
    // 阶段 2：只改一个无关输入（B1 在 数据!B1 之外的地方）
    wb.sheets[1].set(20, 5, Cell { value: Value::Number(2.0), formula: None });
    let deps = formula::deps::Deps::build(&wb);
    formula::recalc_dirty(&mut wb, &deps, &[(1, 20, 5)]);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Div0), "增量后跨表错误退化了");
    assert_eq!(wb.sheets[0].value(3, 0), Value::Error(ErrKind::Div0));
    // 阶段 3：再全算，仍应一致
    let before = snapshot(&wb);
    formula::recalc_all(&mut wb);
    assert_eq!(snapshot(&wb), before, "再次全算改变了结果");
}

#[test]
fn every_incremental_step_matches_a_clone_full_recalc() {
    let mut wb = book();
    // 逐格改一批输入，每改一次：增量算 vs 克隆后全算，逐格比类型与数值
    let edits: &[(usize, u32, u32, f64)] = &[
        (1, 20, 0, 2.0),
        (1, 21, 0, -1.0),
        (0, 30, 30, 5.0),
        (1, 8, 1, 9.0),  // 清掉 SPILL 障碍
        (1, 8, 0, 0.0),  // 公式列被数值覆盖
    ];
    for (step, (si, r, c, v)) in edits.iter().enumerate() {
        wb.sheets[*si].set(*r, *c, Cell { value: Value::Number(*v), formula: None });
        let deps = formula::deps::Deps::build(&wb);
        formula::recalc_dirty(&mut wb, &deps, &[(*si, *r, *c)]);
        let mut clone = wb.clone();
        formula::recalc_all(&mut clone);
        assert_eq!(
            snapshot(&clone),
            snapshot(&wb),
            "第 {step} 步（{si}!{}）增量与全量不一致",
            sheetcore::model::cell_name(*r, *c)
        );
    }
}

#[test]
fn fixing_the_error_source_clears_dependents_incrementally() {
    let mut wb = book();
    wb.sheets[1].set(0, 1, Cell { value: Value::Number(4.0), formula: None });
    wb.sheets[1].set(0, 0, Cell { value: Value::Empty, formula: Some("=1/B1".into()) });
    let deps = formula::deps::Deps::build(&wb);
    formula::recalc_dirty(&mut wb, &deps, &[(1, 0, 0)]);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(0.25));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Text("0.25x".into()));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(2.25));
}

#[test]
fn requirement_three_stage_case_stays_div0() {
    // core-requirements.md C01 表格第一行：A1=1/0，B1=1，C1=SUM(A1,B1)
    // 全算 → 改 B1=2 增量 → 再全算，C1 必须始终是 #DIV/0!（旧行为会变成 2）
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=1/0".into()) });
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set(0, 2, Cell { value: Value::Empty, formula: Some("=SUM(A1,B1)".into()) });

    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Error(ErrKind::Div0), "全算阶段 1");

    wb.sheets[0].set(0, 1, Cell { value: Value::Number(2.0), formula: None });
    let deps = formula::deps::Deps::build(&wb);
    formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 1)]);
    assert_eq!(
        wb.sheets[0].value(0, 2),
        Value::Error(ErrKind::Div0),
        "只改 B1 做增量后 C1 退化了：{:?}",
        wb.sheets[0].value(0, 2)
    );
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Div0), "A1 在增量后变了");

    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Error(ErrKind::Div0), "再全算又变了");
}
