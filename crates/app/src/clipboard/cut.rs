//! Source-to-destination geometry shared by ordinary and sparse cuts.
use super::Clip;
use sheetcore::{CellRef, Workbook};

pub(super) struct CutRange {
    sheet: usize,
    source_name: String,
    destination_name: String,
    rect: (u32, u32, u32, u32),
    dest: (u32, u32),
    rows: Option<Vec<u32>>,
}

impl CutRange {
    pub(super) fn new(clip: &Clip, wb: &Workbook, si: usize, dest: (u32, u32)) -> Self {
        Self {
            sheet: clip.sheet,
            source_name: wb.sheet(clip.sheet).name.clone(),
            destination_name: wb.sheet(si).name.clone(),
            rect: clip.rect,
            dest,
            rows: clip.source_rows.clone(),
        }
    }

    fn rewrite(&self, f: &str, own: &str, formula_moves: bool) -> String {
        sheetcore::refs::relocate_moved_rows(
            f,
            own,
            &self.source_name,
            self.rect,
            &self.destination_name,
            i64::from(self.dest.0) - i64::from(self.rect.0),
            i64::from(self.dest.1) - i64::from(self.rect.1),
            self.rows.as_deref(),
            formula_moves,
        )
    }

    pub(super) fn moved_formula(&self, f: &str) -> String {
        self.rewrite(f, &self.source_name, true)
    }

    pub(super) fn external_formulas(
        &self,
        wb: &Workbook,
        dest_sheet: usize,
        end: (u32, u32),
    ) -> Vec<(usize, u32, u32, String)> {
        let mut out = Vec::new();
        for (si, sh) in wb.sheets.iter().enumerate() {
            for (CellRef { row, col }, cell) in sh.formula_cells() {
                let Some(f) = &cell.formula else { continue };
                if si == self.sheet
                    && (self.rect.0..=self.rect.2).contains(&row)
                    && (self.rect.1..=self.rect.3).contains(&col)
                    && self
                        .rows
                        .as_ref()
                        .is_none_or(|v| v.binary_search(&row).is_ok())
                {
                    continue;
                }
                if si == dest_sheet
                    && (self.dest.0..=end.0).contains(&row)
                    && (self.dest.1..=end.1).contains(&col)
                {
                    continue;
                }
                let new = self.rewrite(f, &sh.name, false);
                if new != f.as_ref() {
                    out.push((si, row, col, new));
                }
            }
        }
        out
    }

    pub(super) fn workbook_references(&self, wb: &Workbook) -> sheetcore::undo::MoveReferences {
        sheetcore::undo::MoveReferences::plan(wb, |f, own| self.rewrite(f, own, false))
    }
}
