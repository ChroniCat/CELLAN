use sheetcore::{cf, model::DateSystem, page::PageSetup, store::CellVersion, Sheet};

#[derive(Default)]
pub(super) struct PageBreaks {
    source: Option<(u64, Option<(u32, u32, u32, u32)>, u64, u64, PageSetup)>,
    lines: std::sync::Arc<(Vec<f32>, Vec<f32>)>,
}

impl PageBreaks {
    pub fn get(&mut self, sheet: &Sheet) -> std::sync::Arc<(Vec<f32>, Vec<f32>)> {
        // Large explicit print areas are clamped to used bounds by page_breaks.
        let area = sheet.used_bounds();
        let key = (sheet.identity, area, sheet.rows.revision(), sheet.cols.revision());
        if self.source.as_ref().is_none_or(|(id, area, rows, cols, setup)|
            (*id,*area,*rows,*cols) != key || setup != &sheet.page) {
            self.lines = std::sync::Arc::new(sheetcore::page::page_breaks(sheet));
            self.source = Some((key.0, key.1, key.2, key.3, sheet.page.clone()));
        }
        self.lines.clone()
    }
}

#[derive(Default)]
pub(super) struct Rules {
    pub index: cf::RuleIndex,
    pub generation: u64,
    sheet: Option<(u64, CellVersion, DateSystem)>,
    observed: (u64,u64),
    definitions: cf::CfSheet,
    expressions: bool,
}

impl Rules {
    pub fn refresh(&mut self, sh: &Sheet, dates: DateSystem, observed: (u64,u64), forced: bool,
        stats: &mut cf::CfStats) -> bool {
        let local_changed = self.sheet.as_ref().is_none_or(|(id,cells,sys)|
            *id != sh.identity || cells != sh.cell_version() || *sys != dates);
        if !local_changed && !forced && self.observed == observed { return false; }
        let definitions_changed = self.definitions != sh.cf;
        if definitions_changed {
            self.definitions = sh.cf.clone();
            self.index = cf::RuleIndex::new(&sh.cf);
            self.expressions = sh.cf.blocks.iter().flat_map(|b| &b.rules).any(|r| matches!(r.kind, cf::CfKind::Expr { .. }));
        }
        let dirty = local_changed || definitions_changed || self.expressions;
        if dirty {
            if local_changed || definitions_changed || forced { cf::refresh_stats(sh, stats); }
            self.generation = self.generation.wrapping_add(1);
        }
        self.sheet = Some((sh.identity, sh.cell_version().clone(), dates));
        self.observed = observed;
        dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{egui, office_clipboard, Clip, SheetApp};
    use sheetcore::{Cell, Value};
    use std::sync::Arc;

    #[test]
    fn page_lines_follow_clamped_used_bounds_and_axis_changes_without_refreshing_on_value_edits() {
        let mut sheet = Sheet::new("pages");
        sheet.page.print_area = Some((0,0,999_999,2));
        let mut pages = PageBreaks::default();
        let number = |n| Cell { value: Value::Number(n), formula: None };
        sheet.set(0,0,number(1.0));
        let first = pages.get(&sheet);
        sheet.set(0,0,number(2.0));
        assert!(Arc::ptr_eq(&first, &pages.get(&sheet)));
        sheet.set(999,1,number(2.0));
        let extended = pages.get(&sheet);
        assert!(!Arc::ptr_eq(&first, &extended));
        assert_eq!(*extended, sheetcore::page::page_breaks(&sheet));
        sheet.rows.set(8,56.0);
        assert!(!Arc::ptr_eq(&extended,&pages.get(&sheet)));
    }

    #[test]
    fn conditional_rule_inputs_keep_unrelated_generations_and_honour_external_expressions() {
        let mut sh = Sheet::new("rules");
        sh.cf.blocks.push(cf::CfBlock { ranges: vec![(1,2,4,2),(3,5,7,5)], rules: vec![cf::CfRule {
            kind: cf::CfKind::Text { contains: "X".into(), dxf: 0 }, stop: true, priority: 1,
        }] });
        sh.cf.dxfs.push(cf::Dxf { fill: Some([1,2,3]), ..Default::default() });
        sh.set(3,5,Cell { value: Value::Text("X".into()), formula: None });
        let mut rules = Rules::default();
        let mut stats = cf::CfStats::default();
        assert!(rules.refresh(&sh, DateSystem::default(), (1,1),true,&mut stats));
        let gen0 = rules.generation;
        assert!(!rules.refresh(&sh, DateSystem::default(), (2,3),true,&mut stats));
        assert_eq!(rules.generation, gen0);
        let row = rules.index.row(&sh.cf,3);
        assert_eq!(row.overlay_at(&stats,&sh,5,|_,_,_| false).fill,Some([1,2,3]));
        assert!(row.overlay_at(&stats,&sh,4,|_,_,_| false).is_empty());
        sh.cf.blocks[0].rules[0].kind = cf::CfKind::Expr { formula: "Other!B3".into(), dxf: 0 };
        assert!(rules.refresh(&sh,DateSystem::default(),(4,3),false,&mut stats));
        assert!(rules.refresh(&sh,DateSystem::default(),(5,3),false,&mut stats));
    }

    #[test]
    fn full_column_width_paste_uses_copy_time_dimensions_and_late_column_locks() {
        let _env = crate::test_env::TestEnv::new("full-column-widths");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx,None);
        let rect = (0,0,1_048_575,2);
        app.wb.sheets[0].cols.set_many([(0,74.0),(1,90.0),(2,66.0)]);
        let range = Arc::new(office_clipboard::delayed::Range::new(&app.wb,0,rect));
        app.clip = Some(Clip { sheet: 0, rect, deferred: Some(range), cells: vec![], merges: vec![],
            source_rows: None, cut: false });
        app.wb.sheets[0].cols.set(1,160.0);
        app.ps_kind = 5;
        app.sel = (80,6);
        app.paste_special_apply_text(&ctx,None);
        assert_eq!((6..=8).map(|c| app.wb.sheets[0].cols.size(c)).collect::<Vec<_>>(), vec![74.0,90.0,66.0]);
        app.sel = (0,9);
        app.locks.request(0,999_999,10,2,"peer",app.lock_now());
        app.paste_special_apply_text(&ctx,None);
        assert!(app.status.contains("peer"),"{}",app.status);
        assert_eq!(app.wb.sheets[0].cols.size(9),app.wb.sheets[0].cols.size(12));
    }

    #[test]
    fn dense_grid_probe_reports_tessellated_meshes_and_triangles() {
        let _env = crate::test_env::TestEnv::new("grid-mesh-probe");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx,None);
        crate::install_fonts(&ctx);
        let mut st = app.wb.styles.get(0).clone();
        st.fill = Some([215,230,245]);
        let style = app.wb.styles.intern(st);
        for r in 0..36 { for c in 0..14 {
            app.wb.sheets[0].set(r,c,Cell { value: Value::Text("abc".into()), formula: None });
            if (r+c)%2 == 0 { app.wb.sheets[0].set_style(r,c,style); }
        } }
        for frame in 0..5 {
            let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1200.0,800.0))),..Default::default() };
            let start = std::time::Instant::now();
            let output = ctx.run(raw, |ctx| { egui::CentralPanel::default().show(ctx,|ui| app.draw_grid_pane(ui,ctx,false)); });
            let build = start.elapsed();
            let now = std::time::Instant::now();
            let meshes = ctx.tessellate(output.shapes,output.pixels_per_point);
            let tess = now.elapsed();
            let triangles: usize = meshes.iter().map(|m| match &m.primitive {
                egui::epaint::Primitive::Mesh(m) => m.indices.len()/3, _ => 0
            }).sum();
            if frame == 4 {
                eprintln!("grid 1200x800, 36x14 filled cells: build={build:?} tessellation={tess:?}, meshes={}, triangles={triangles}",meshes.len());
                assert!(triangles > 100 && meshes.len() < 250, "short fitting text should batch across cells");
            }
        }
    }
}
