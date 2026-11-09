//! Copy offsets and structural shifts intentionally have different `$` semantics.
use sheetcore::refs::{self, Axis, Shift};

fn shift(axis: Axis, at: u32, n: u32, delete: bool) -> Shift {
    Shift {
        axis,
        at,
        n,
        delete,
        target_sheet: "Sheet1".into(),
        own_sheet: "Sheet1".into(),
        translate: None,
    }
}

#[test]
fn copy_preserves_each_absolute_axis() {
    assert_eq!(
        refs::translate("=$A$1+A$1+$A1+A1", 1, 1, "Sheet1"),
        "=$A$1+B$1+$A2+B2",
    );
    assert_eq!(
        refs::translate("=SUM($A1:B$4)+SUM(C$2:$D5)", 2, 3, "Sheet1"),
        "=SUM($A3:E$4)+SUM(F$2:$D7)",
    );
}

#[test]
fn copy_moves_qualified_relative_refs_without_changing_sheet_names() {
    assert_eq!(
        refs::translate("=Other!A1+'我的 表'!$B2+'Joe''s sheet'!C$3+A1", 2, 1, "Sheet1"),
        "=Other!B3+'我的 表'!$B4+'Joe''s sheet'!D$3+B3",
    );
    assert_eq!(
        refs::translate("='[A1.xlsx]Sheet 2'!$A1+[B2.xlsx]Data!B$2", 1, 1, "Sheet1"),
        "='[A1.xlsx]Sheet 2'!$A2+[B2.xlsx]Data!C$2",
    );
}

#[test]
fn copy_whole_axes_preserves_locks_and_ignores_other_axis_offset() {
    assert_eq!(
        refs::translate("=SUM(A:C)+SUM($A:C)+SUM(A:$C)+SUM($A:$C)", -99, 1, "Sheet1"),
        "=SUM(B:D)+SUM($A:D)+SUM(B:$C)+SUM($A:$C)",
    );
    assert_eq!(
        refs::translate("=SUM(1:3)+SUM($1:3)+SUM(1:$3)+SUM($1:$3)", 1, -99, "Sheet1"),
        "=SUM(2:4)+SUM($1:4)+SUM(2:$3)+SUM($1:$3)",
    );
}

#[test]
fn copying_past_grid_edges_returns_ref_only_for_unlocked_axes() {
    assert_eq!(
        refs::translate("=$A$1+A$1+$A1+A1", -1, -1, "Sheet1"),
        "=$A$1+#REF!+#REF!+#REF!",
    );
    assert_eq!(
        refs::translate("=$XFD$1048576+XFD$1048576+$XFD1048576+XFD1048576", 1, 1, "Sheet1"),
        "=$XFD$1048576+#REF!+#REF!+#REF!",
    );
    assert_eq!(refs::translate("=SUM(A:C)+SUM($A:$C)", 0, -1, "Sheet1"), "=SUM(#REF!)+SUM($A:$C)");
    assert_eq!(refs::translate("=SUM(1:3)+SUM($1:$3)", -1, 0, "Sheet1"), "=SUM(#REF!)+SUM($1:$3)");
}

#[test]
fn copy_leaves_strings_functions_names_and_table_columns_unchanged() {
    assert_eq!(
        refs::translate("=IF(A1>0,\"A1 \"\"$B$2\"\"\",LOG10(B2))+A1_name+A1税率+税率A1+Table1[A1]+SUM(1,2)", 1, 1, "Sheet1"),
        "=IF(B2>0,\"A1 \"\"$B$2\"\"\",LOG10(C3))+A1_name+A1税率+税率A1+Table1[A1]+SUM(1,2)",
    );
}

#[test]
fn structural_edits_still_move_absolute_refs_only_on_target_sheet() {
    assert_eq!(
        refs::rewrite("=$A$1+A$1+$A1+A1+Other!A1+B1", &shift(Axis::Row, 0, 1, false)),
        "=$A$2+A$2+$A2+A2+Other!A1+B2",
    );
    assert_eq!(
        refs::rewrite("=SUM($A:$C)+SUM($1:$3)", &shift(Axis::Col, 0, 1, false)),
        "=SUM($B:$D)+SUM($1:$3)",
    );
    assert_eq!(
        refs::rewrite("=SUM($A:$C)+SUM($1:$3)", &shift(Axis::Row, 0, 1, false)),
        "=SUM($A:$C)+SUM($2:$4)",
    );
}

#[test]
fn deleting_entire_first_range_returns_ref_but_partial_ranges_shrink() {
    let rows = shift(Axis::Row, 0, 1, true);
    assert_eq!(refs::rewrite("=SUM(A1:A1)+SUM($1:$1)", &rows), "=SUM(#REF!)+SUM(#REF!)");
    assert_eq!(refs::rewrite("=SUM(A1:A3)+SUM(1:3)", &rows), "=SUM(A1:A2)+SUM(1:2)");
    let cols = shift(Axis::Col, 0, 1, true);
    assert_eq!(refs::rewrite("=SUM(A1:A2)+SUM($A:$A)", &cols), "=SUM(#REF!)+SUM(#REF!)");
    assert_eq!(refs::rewrite("=SUM(A1:C2)+SUM(A:C)", &cols), "=SUM(A1:B2)+SUM(A:B)");
}
