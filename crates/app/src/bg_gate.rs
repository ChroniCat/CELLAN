//! Unified `(doc_uid, wb.coord_epoch())` staleness gate for background async workflows.
//!
//! Captures both workbook-wide `coord_epoch` (advanced on any structural insert/delete,
//! partial cell shift, sheet tab add/delete/rename/move, or undo/redo thereof) and,
//! when sheet-scoped, the sheet's `(sheet_idx, identity, coordinate_revision)` so
//! direct sheet-level structural mutations are fenced alongside workbook-level epochs.

use sheetcore::Workbook;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CoordGate {
    pub doc_uid: u64,
    pub coord_epoch: u64,
    pub sheet_idx: Option<usize>,
    pub sheet_identity: Option<u64>,
    pub sheet_coord_rev: Option<u64>,
}

impl CoordGate {
    /// Capture a workbook-level coordinate gate bound to `(doc_uid, wb.coord_epoch())`.
    pub(crate) fn workbook(doc_uid: u64, wb: &Workbook) -> Self {
        Self {
            doc_uid,
            coord_epoch: wb.coord_epoch(),
            sheet_idx: None,
            sheet_identity: None,
            sheet_coord_rev: None,
        }
    }

    /// Capture a sheet-scoped coordinate gate bound to `(doc_uid, wb.coord_epoch())`
    /// plus the target sheet's `(sheet_idx, identity, coordinate_revision())`.
    pub(crate) fn sheet(doc_uid: u64, wb: &Workbook, sheet_idx: usize) -> Option<Self> {
        let sh = wb.sheets.get(sheet_idx)?;
        Some(Self {
            doc_uid,
            coord_epoch: wb.coord_epoch(),
            sheet_idx: Some(sheet_idx),
            sheet_identity: Some(sh.identity),
            sheet_coord_rev: Some(sh.coordinate_revision()),
        })
    }

    /// True if the live workbook still has the same `doc_uid` and `coord_epoch`.
    #[inline]
    pub(crate) fn matches_workbook(&self, doc_uid: u64, wb: &Workbook) -> bool {
        self.doc_uid == doc_uid && self.coord_epoch == wb.coord_epoch()
    }

    /// Returns `Some(sheet_idx)` if the workbook gate still matches and the target
    /// sheet at `sheet_idx` retains the same `identity` and `coordinate_revision()`.
    #[inline]
    pub(crate) fn matches_sheet(&self, doc_uid: u64, wb: &Workbook) -> Option<usize> {
        if !self.matches_workbook(doc_uid, wb) {
            return None;
        }
        let idx = self.sheet_idx?;
        let expected_id = self.sheet_identity?;
        let expected_rev = self.sheet_coord_rev?;
        let sh = wb.sheets.get(idx)?;
        (sh.identity == expected_id && sh.coordinate_revision() == expected_rev).then_some(idx)
    }

    /// True when the gate no longer matches the live workbook/sheet state.
    #[inline]
    pub(crate) fn is_stale(&self, doc_uid: u64, wb: &Workbook) -> bool {
        if self.sheet_idx.is_some() {
            self.matches_sheet(doc_uid, wb).is_none()
        } else {
            !self.matches_workbook(doc_uid, wb)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CoordGate;
    use sheetcore::{
        Cell, Sheet, Value, Workbook,
        sheet_ops,
        structural::{Structural, apply_structural_wb},
    };

    #[test]
    fn phase5_bg_gate_coord_gate_tracks_doc_epoch_identity_and_sheet_rev() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Other"));
        let doc = 42;

        let wb_gate = CoordGate::workbook(doc, &wb);
        let sh_gate = CoordGate::sheet(doc, &wb, 0).expect("sheet 0 exists");
        assert!(CoordGate::sheet(doc, &wb, 99).is_none());

        assert!(wb_gate.matches_workbook(doc, &wb));
        assert!(!wb_gate.is_stale(doc, &wb));
        assert_eq!(wb_gate.matches_sheet(doc, &wb), None);

        assert!(sh_gate.matches_workbook(doc, &wb));
        assert_eq!(sh_gate.matches_sheet(doc, &wb), Some(0));
        assert!(!sh_gate.is_stale(doc, &wb));

        // Ordinary cell edits on either sheet do NOT advance coord_epoch or coordinate_revision.
        wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Number(1.0),
                formula: None,
            },
        );
        wb.sheet_mut(1).set(
            2,
            3,
            Cell {
                value: Value::Text("ok".into()),
                formula: None,
            },
        );
        assert!(wb_gate.matches_workbook(doc, &wb));
        assert_eq!(sh_gate.matches_sheet(doc, &wb), Some(0));
        assert!(!sh_gate.is_stale(doc, &wb));

        // Different doc_uid marks both stale immediately.
        assert!(!wb_gate.matches_workbook(doc + 1, &wb));
        assert!(wb_gate.is_stale(doc + 1, &wb));
        assert_eq!(sh_gate.matches_sheet(doc + 1, &wb), None);
        assert!(sh_gate.is_stale(doc + 1, &wb));

        // Structural edit on ANOTHER sheet advances wb.coord_epoch() and invalidates BOTH gates.
        apply_structural_wb(&mut wb, 1, &Structural::InsertRows { at: 0, n: 1 });
        assert!(!wb_gate.matches_workbook(doc, &wb));
        assert!(wb_gate.is_stale(doc, &wb));
        assert_eq!(sh_gate.matches_sheet(doc, &wb), None);
        assert!(sh_gate.is_stale(doc, &wb));

        // Direct sheet-level structural mutation (without wb.coord_epoch bump) still fences sheet gate.
        let fresh_sheet = CoordGate::sheet(doc, &wb, 0).unwrap();
        wb.sheet_mut(0)
            .apply_structural(&Structural::InsertCols { at: 0, n: 1 });
        assert_eq!(fresh_sheet.matches_sheet(doc, &wb), None);
        assert!(fresh_sheet.is_stale(doc, &wb));

        // Sheet tab rename / move / insert / delete advance wb.coord_epoch().
        for op in ["rename", "move", "insert", "delete"] {
            let g_wb = CoordGate::workbook(doc, &wb);
            let g_sh = CoordGate::sheet(doc, &wb, 0).unwrap();
            match op {
                "rename" => assert!(sheet_ops::rename_sheet(&mut wb, 1, "Renamed")),
                "move" => assert!(sheet_ops::move_sheet(&mut wb, 0, 1)),
                "insert" => assert!(sheet_ops::insert_sheet(&mut wb, 2, Sheet::new("Third"))),
                "delete" => assert!(sheet_ops::delete_sheet(&mut wb, 2)),
                _ => unreachable!(),
            }
            assert!(g_wb.is_stale(doc, &wb), "{op} must advance coord_epoch");
            assert!(g_sh.is_stale(doc, &wb), "{op} must invalidate sheet gate");
        }
    }
}
