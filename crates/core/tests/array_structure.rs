use sheetcore::{Cell, CellRef, Value, Workbook, formula, structural::{apply_structural_wb, Structural}, xlsx};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("native_array_structure_{}_{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> PathBuf { self.0.join("array.xlsx") }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn number(value: f64) -> Cell { Cell { value: Value::Number(value), formula: None } }
fn seed(cse: bool) -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(2, 2, Cell { value: Value::Empty, formula: Some("=SEQUENCE(2,2)".into()) });
    if cse { wb.sheets[0].arrays.push((CellRef::new(2, 2), (2, 2, 3, 3))); }
    wb.sheets[0].set(4, 2, number(99.0));
    wb.sheets[0].set(2, 4, number(77.0));
    formula::recalc_all(&mut wb);
    wb
}

fn assert_array(wb: &Workbook, row: u32, col: u32, cse: bool) {
    for dr in 0..2 { for dc in 0..2 {
        assert_eq!(wb.sheets[0].value(row + dr, col + dc), Value::Number((dr * 2 + dc + 1) as f64));
    } }
    assert_eq!(wb.sheets[0].value(row + 2, col), Value::Number(99.0));
    assert_eq!(wb.sheets[0].value(row, col + 2), Value::Number(77.0));
    if cse {
        assert_eq!(wb.sheets[0].arrays, vec![(CellRef::new(row, col), (row, col, row + 1, col + 1))]);
    }
}

#[test]
fn insert_delete_rows_columns_move_array_metadata_and_survive_reopen() {
    for cse in [false, true] {
        for (op, row, col) in [
            (Structural::InsertRows { at: 0, n: 1 }, 3, 2),
            (Structural::DeleteRows { at: 0, n: 1 }, 1, 2),
            (Structural::InsertCols { at: 0, n: 1 }, 2, 3),
            (Structural::DeleteCols { at: 0, n: 1 }, 2, 1),
        ] {
            let fixture = Fixture::new();
            let path = fixture.path();
            xlsx::save_atomic(&path, &seed(cse)).unwrap();
            let mut wb = xlsx::load(&path).unwrap();
            formula::recalc_all(&mut wb);
            assert_eq!(wb.sheets[0].value(2, 2), Value::Number(1.0), "initial load cse={cse}, op={op:?}");
            apply_structural_wb(&mut wb, 0, &op);
            formula::recalc_all(&mut wb);
            assert_eq!(wb.sheets[0].value(row, col), Value::Number(1.0), "after edit cse={cse}, op={op:?}");
            assert_array(&wb, row, col, cse);
            for _ in 0..2 {
                xlsx::save_atomic(&path, &wb).unwrap();
                let mut reopened = xlsx::load(&path).unwrap();
                formula::recalc_all(&mut reopened);
                assert_eq!(reopened.sheets[0].value(row, col), Value::Number(1.0), "reopen cse={cse}, op={op:?}, xml={}",
                    String::from_utf8_lossy(&sheetcore::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml").unwrap()));
                assert_array(&reopened, row, col, cse);
            }
        }
    }
}

#[test]
fn structure_change_outside_dynamic_array_does_not_create_spill_error() {
    for op in [
        Structural::InsertRows { at: 9, n: 1 }, Structural::DeleteRows { at: 9, n: 1 },
        Structural::InsertCols { at: 9, n: 1 }, Structural::DeleteCols { at: 9, n: 1 },
    ] {
        let mut wb = seed(false);
        apply_structural_wb(&mut wb, 0, &op);
        formula::recalc_all(&mut wb);
        assert_array(&wb, 2, 2, false);
    }
}

#[test]
fn deleting_before_cse_array_keeps_neighboring_user_value_99() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(1, 0, Cell { value: Value::Empty, formula: Some("=SEQUENCE(2)".into()) });
    wb.sheets[0].arrays.push((CellRef::new(1, 0), (1, 0, 2, 0)));
    wb.sheets[0].set(3, 0, number(99.0));
    formula::recalc_all(&mut wb);
    apply_structural_wb(&mut wb, 0, &Structural::DeleteRows { at: 0, n: 1 });
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(1.0));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(2.0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(99.0));
    assert_eq!(wb.sheets[0].arrays, vec![(CellRef::new(0, 0), (0, 0, 1, 0))]);
}

#[test]
fn deleting_cse_anchor_before_first_recalc_removes_only_derived_tail() {
    for (op, user) in [
        (Structural::DeleteRows { at: 2, n: 1 }, (3, 2, 99.0)),
        (Structural::DeleteCols { at: 2, n: 1 }, (2, 3, 77.0)),
    ] {
        let fixture = Fixture::new();
        let path = fixture.path();
        xlsx::save_atomic(&path, &seed(true)).unwrap();
        let mut wb = xlsx::load(&path).unwrap();
        assert!(wb.sheets[0].spilled.is_empty(), "模拟加载后尚未首次重算");
        apply_structural_wb(&mut wb, 0, &op);
        formula::recalc_all(&mut wb);
        assert!(wb.sheets[0].arrays.is_empty());
        assert!(wb.sheets[0].spilled.is_empty());
        assert!(wb.sheets[0].get(2, 2).is_none());
        assert_eq!(wb.sheets[0].value(user.0, user.1), Value::Number(user.2));
        xlsx::save_atomic(&path, &wb).unwrap();
        let reopened = xlsx::load(&path).unwrap();
        assert!(reopened.sheets[0].arrays.is_empty());
        assert_eq!(reopened.sheets[0].value(user.0, user.1), Value::Number(user.2));
        assert!(reopened.sheets[0].get(2, 2).is_none());
    }
}

#[test]
fn structure_only_replay_moves_array_metadata_without_moving_snapshot_cells() {
    let original = seed(true);
    for op in [
        Structural::InsertRows { at: 0, n: 1 }, Structural::DeleteRows { at: 0, n: 1 },
        Structural::InsertCols { at: 0, n: 1 }, Structural::DeleteCols { at: 0, n: 1 },
    ] {
        let mut full = original.clone();
        apply_structural_wb(&mut full, 0, &op);
        let mut metadata = original.sheets[0].clone();
        metadata.apply_structural_structure_only(&op);
        assert_eq!(metadata.arrays, full.sheets[0].arrays);
        assert_eq!(metadata.spilled, full.sheets[0].spilled);
        assert_eq!(metadata.value(2, 2), Value::Number(1.0));
        assert_eq!(metadata.value(4, 2), Value::Number(99.0));
    }
}

#[test]
fn scoped_structural_recalc_updates_names_tables_and_spill_readers() {
    use sheetcore::{Sheet, formula::deps::Deps, table::Table};
    let mut seed = Workbook::new_blank();
    let formula = |f: &str| Cell {formula:Some(f.into()),value:Value::Empty};
    let add = |b: &mut Workbook, name: &str| b.sheets.push(Sheet::new(name));
    for name in ["Readers","Summary","Independent"] { add(&mut seed,name); }
    seed.sheets[0].set(1,0,number(4.));
    seed.sheets[0].set(2,0,number(7.));
    seed.sheets[0].set(2,3,formula("=SEQUENCE(2,1)"));
    seed.sheets[0].tables.push(Table { name:"Data".into(),display:"Data".into(),range:(0,0,2,0),
        columns:vec!["Amount".into()],column_ids:vec![1],header_row:true,..Default::default() });
    seed.names.push(("AmountCells".into(),None,"Sheet1!$A$2:$A$3".into()));
    seed.sheets[1].set(0,0,formula("=SUM(AmountCells)+SUM(Data[Amount])+Sheet1!D4"));
    seed.sheets[2].set(0,0,formula("=Readers!A1*2"));
    seed.sheets[3].set(0,0,formula("=7+3"));
    formula::recalc_all(&mut seed);
    for op in [Structural::InsertRows {at:2,n:1},Structural::DeleteRows {at:1,n:1}] {
        let mut wb = seed.clone();
        let graph = Deps::build_after_recalc(&wb);
        apply_structural_wb(&mut wb,0,&op);
        let mut expected = wb.clone();
        formula::recalc_all(&mut expected);
        // A distinctive cached result proves the independent sheet wasn't evaluated.
        wb.sheets[3].set(0,0,Cell {formula:Some("=7+3".into()),value:Value::Number(900.)});
        let repaired = graph.recalc_structural_sheets(&mut wb,0,&false.into()).unwrap();
        for si in 0..3 {
            assert!(wb.sheets[si].iter().eq(expected.sheets[si].iter()));
            assert_eq!(wb.sheets[si].spilled,expected.sheets[si].spilled);
        }
        assert_eq!(wb.sheets[3].value(0,0),Value::Number(900.));
        assert_eq!(repaired.edit_closure(&[(1,0,0)]),Deps::build_after_recalc(&wb).edit_closure(&[(1,0,0)]));
    }
}
