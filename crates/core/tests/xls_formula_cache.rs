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

// ------------------------------------------------------- 七种错误：真实局部 dirty 路径
//
// 这里刻意**不含任何数组/溢出**：`recalc_dirty` 一旦发现 spilled/arrays 就会整本回退全算，
// 那样测到的只是「全量 vs 全量」。局部路径必须用没有溢出源的簿子来验。

fn error_book() -> Workbook {
    let mut wb = Workbook::new_blank(); // 0 = 汇总
    wb.sheets[0].name = "汇总".into();
    let mut data = sheetcore::model::Sheet::new("数据"); // 1 = 数据

    // A1..A7：七种传统错误，互不依赖
    let defs = [
        "=1/0",          // #DIV/0!
        "=NA()",         // #N/A
        "=SQRT(-1)",     // #NUM!
        "=NOPE()",       // #NAME?
        "=\"a\"+1",      // #VALUE!
        "=#REF!",        // #REF!
        "=SUM(A1 A9)",   // #NULL!（空格是交集，A1 与 A9 不相交）
    ];
    for (i, f) in defs.iter().enumerate() {
        data.set(i as u32, 0, Cell { value: Value::Empty, formula: Some((*f).into()) });
    }
    // A9 是一条真实的错误链：B1=0 → 1/B1 = #DIV/0!
    data.set(0, 1, Cell { value: Value::Number(0.0), formula: None });
    data.set(8, 0, Cell { value: Value::Empty, formula: Some("=1/B1".into()) });

    // 跨表依赖链
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=数据!A9".into()) });
    wb.sheets[0].set(1, 0, Cell { value: Value::Empty, formula: Some("=A1&\"x\"".into()) });
    wb.sheets[0].set(2, 0, Cell { value: Value::Empty, formula: Some("=IFERROR(A1,7)".into()) });
    wb.sheets[0].set(3, 0, Cell { value: Value::Empty, formula: Some("=SUM(A1,2)".into()) });
    wb.sheets.push(data);
    formula::recalc_all(&mut wb);
    wb
}

fn total_formulas(wb: &Workbook) -> usize {
    wb.sheets.iter().map(|s| s.iter().filter(|(_, c)| c.formula.is_some()).count()).sum()
}

/// 所有非空格（含溢出子格），逐格记类型与值
fn snapshot(wb: &Workbook) -> Vec<(usize, u32, u32, Value)> {
    let mut out = Vec::new();
    for (si, s) in wb.sheets.iter().enumerate() {
        for (cref, cell) in s.iter() {
            if cell.formula.is_some() || cell.value != Value::Empty {
                out.push((si, cref.row, cref.col, cell.value.clone()));
            }
        }
    }
    out.sort_by_key(|(si, r, c, _)| (*si, *r, *c));
    out
}

#[test]
fn seven_errors_are_typed_after_a_full_recalc() {
    let wb = error_book();
    let d = &wb.sheets[1];
    let want = [
        Value::Error(ErrKind::Div0),
        Value::Error(ErrKind::Na),
        Value::Error(ErrKind::Num),
        Value::Error(ErrKind::Name),
        Value::Error(ErrKind::Value),
        Value::Error(ErrKind::Ref),
        Value::Error(ErrKind::Null), // 断言类型，不靠「可能等价于 Value」的推测
    ];
    for (i, w) in want.iter().enumerate() {
        assert_eq!(&d.value(i as u32, 0), w, "第 {i} 种错误类型不对");
    }
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Div0));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Error(ErrKind::Div0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(7.0), "IFERROR 应抓住跨表错误");
    assert_eq!(wb.sheets[0].value(3, 0), Value::Error(ErrKind::Div0), "SUM 不能吞错误");
}

#[test]
fn requirement_three_stage_case_stays_div0() {
    // core-requirements.md C01 表格第一行：A1=1/0，B1=1，C1=SUM(A1,B1)
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=1/0".into()) });
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set(0, 2, Cell { value: Value::Empty, formula: Some("=SUM(A1,B1)".into()) });

    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Error(ErrKind::Div0), "全算阶段 1");

    wb.sheets[0].set(0, 1, Cell { value: Value::Number(2.0), formula: None });
    let deps = formula::deps::Deps::build(&wb);
    let n = formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 1)]);
    assert_eq!(n, 1, "这里应是只算 C1 的局部路径，实际算了 {n} 格");
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

#[test]
fn a_local_recalc_matches_a_full_recalc_over_the_whole_book() {
    let mut wb = error_book();
    let total = total_formulas(&wb);
    // 改真实依赖：数据!B1 = 4 → A9 从 #DIV/0! 变 0.25，跨表链跟着变
    for (step, v) in [4.0f64, 8.0, 0.0].into_iter().enumerate() {
        wb.sheets[1].set(0, 1, Cell { value: Value::Number(v), formula: None });
        let deps = formula::deps::Deps::build(&wb);
        let n = formula::recalc_dirty(&mut wb, &deps, &[(1, 0, 1)]);
        // 依赖锥就是 数据!A9 + 汇总!A1..D1 这 5 格；精确值比「小于总数」更能说明范围
        assert_eq!(n, 5, "第 {step} 步的脏集合不对（总公式 {total} 格），说明不是局部路径");
        let mut clone = wb.clone();
        formula::recalc_all(&mut clone);
        assert_eq!(snapshot(&clone), snapshot(&wb), "第 {step} 步局部与全量不一致");
    }
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Div0), "B1=0 时应回到 #DIV/0!");
}

#[test]
fn a_local_recalc_does_not_touch_an_unrelated_cached_cell() {
    let mut wb = error_book();
    // Z31 = 1+1，但缓存被故意写成 999：局部路径不该碰它，全量回退会把它改回 2
    wb.sheets[0].set(30, 25, Cell { value: Value::Number(999.0), formula: Some("=1+1".into()) });
    wb.sheets[1].set(0, 1, Cell { value: Value::Number(4.0), formula: None });
    let deps = formula::deps::Deps::build(&wb);
    let n = formula::recalc_dirty(&mut wb, &deps, &[(1, 0, 1)]);
    assert_eq!(n, 5, "脏集合应为 5 格");
    assert_eq!(
        wb.sheets[0].value(30, 25),
        Value::Number(999.0),
        "无关格的缓存被重算成 2 = 实际走了全量"
    );
    assert_eq!(wb.sheets[1].value(8, 0), Value::Number(0.25), "范围内的依赖没更新");
}

#[test]
fn fixing_the_error_source_clears_dependents_incrementally() {
    let mut wb = error_book();
    assert_eq!(wb.sheets[1].value(8, 0), Value::Error(ErrKind::Div0));
    wb.sheets[1].set(0, 1, Cell { value: Value::Number(4.0), formula: None });
    let deps = formula::deps::Deps::build(&wb);
    let n = formula::recalc_dirty(&mut wb, &deps, &[(1, 0, 1)]);
    assert!(n < total_formulas(&wb), "走了全量回退");
    assert_eq!(wb.sheets[1].value(8, 0), Value::Number(0.25));
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(0.25));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Text("0.25x".into()));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(2.25));
}

// -------------------------------------------------------------------- SPILL 单独验
//
// 有溢出时 `recalc_dirty` 会整本回退全算（返回总公式数）。这条路径单独测，
// 不混进上面的局部矩阵，也不假装它是增量。

#[test]
fn a_blocked_spill_reports_spill_and_recovers_after_the_blocker_is_removed() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(8, 0, Cell { value: Value::Empty, formula: Some("=SEQUENCE(2,1)".into()) });
    wb.sheets[0].set(9, 0, Cell { value: Value::Number(7.0), formula: None }); // A10 真占用
    formula::recalc_all(&mut wb);
    assert_eq!(
        wb.sheets[0].value(8, 0),
        Value::Error(ErrKind::Spill),
        "A10 被占时 A9 应是 #SPILL!，实际 {:?}",
        wb.sheets[0].value(8, 0)
    );
    assert_eq!(wb.sheets[0].value(9, 0), Value::Number(7.0), "占用格不能被溢出覆盖");

    // 删掉障碍，再算：锚点与子格都要回来
    wb.sheets[0].set(9, 0, Cell::default());
    let deps = formula::deps::Deps::build(&wb);
    let n = formula::recalc_dirty(&mut wb, &deps, &[(0, 9, 0)]);
    assert_eq!(n, total_formulas(&wb), "有溢出时按预期走全量回退，返回的是总公式数");
    assert_eq!(wb.sheets[0].value(8, 0), Value::Number(1.0), "锚点没恢复");
    assert_eq!(wb.sheets[0].value(9, 0), Value::Number(2.0), "溢出子格没恢复");
    let mut clone = wb.clone();
    formula::recalc_all(&mut clone);
    assert_eq!(snapshot(&clone), snapshot(&wb), "溢出恢复后与全量重算逐格不一致");
}
