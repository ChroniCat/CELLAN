use crate::{SheetApp, test_env::TestEnv};
use eframe::egui;
use sheetcore::{
    Cell, Sheet, Value,
    charts::{Chart, Series},
};

fn number(n: f64) -> Cell {
    Cell {
        value: Value::Number(n),
        formula: None,
    }
}
fn formula(f: &str) -> Cell {
    Cell {
        value: Value::Empty,
        formula: Some(f.into()),
    }
}

fn paste(
    app: &mut SheetApp,
    source: (u32, u32, u32, u32),
    to_sheet: usize,
    dest: (u32, u32),
    cut: bool,
) {
    crate::clear_test_clipboard();
    app.sel = (source.0, source.1);
    app.sel_end = (source.2, source.3);
    app.clip_copy(cut, &egui::Context::default());
    let text = app.clip_stamp.clone();
    app.switch_sheet_view(to_sheet);
    app.sel = dest;
    app.sel_end = dest;
    app.clip_paste(false, text);
}

#[test]
fn sparse_and_dense_pastes_preserve_filtered_values_and_styles_with_one_undo() {
    let _env = TestEnv::new("sparse-paste-filtered-dest");
    for height in [4096, 4097, 16_385] {
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.wb.sheets.push(Sheet::new("Dest"));
        let st = app.wb.styles.intern(sheetcore::style::CellStyle {
            bold: true,
            ..Default::default()
        });
        app.wb.sheet_mut(0).set(0, 0, number(11.0));
        app.wb.sheet_mut(0).set(1, 0, formula("=A1*2"));
        app.wb.sheet_mut(0).set(2, 0, number(33.0));
        let sh = app.wb.sheet_mut(1);
        sh.hidden_rows.insert(1);
        sh.set(1, 1, number(999.0));
        sh.set_style(1, 1, st);
        sh.set(height, 1, number(777.0)); // displaced logical tail is also covered
        app.recalc_all_and_deps_adaptive();
        paste(&mut app, (0, 0, height - 1, 0), 1, (0, 1), false);
        let sh = app.wb.sheet(1);
        assert_eq!(sh.value(1, 1), Value::Number(999.0));
        assert_eq!(sh.style_of(1, 1), st);
        assert_eq!(sh.formula_of(2, 1), Some("=B2*2"));
        assert_eq!(sh.value(3, 1), Value::Number(33.0));
        assert_eq!(sh.value(height, 1), Value::Empty);
        app.do_undo(false);
        assert_eq!(app.wb.sheet(1).value(height, 1), Value::Number(777.0));
        assert_eq!(app.wb.sheet(1).value(0, 1), Value::Empty);
        app.do_undo(true);
        assert_eq!(app.wb.sheet(1).value(1, 1), Value::Number(999.0));
    }
}

#[test]
fn dense_and_sparse_cuts_follow_moved_cells_and_retain_outside_references() {
    let _env = TestEnv::new("sparse-cut-references");
    for (height, dest_sheet) in [(4096, 0), (4097, 0), (4097, 1)] {
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.wb.sheets.push(Sheet::new("Dest"));
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, number(11.0));
        sh.set(1, 0, formula("=D1"));
        sh.set(2, 0, formula("=A1*2"));
        sh.set(0, 3, number(22.0));
        sh.set(0, 4, number(33.0));
        sh.set(0, 5, formula("=A1"));
        app.recalc_all_and_deps_adaptive();
        paste(&mut app, (0, 0, height - 1, 0), dest_sheet, (0, 1), true);
        let sh = app.wb.sheet(dest_sheet);
        assert_eq!(sh.value(1, 1), Value::Number(22.0));
        assert_eq!(sh.value(2, 1), Value::Number(22.0));
        assert_eq!(app.wb.sheet(0).value(0, 5), Value::Number(11.0));
        assert_eq!(
            app.wb.sheet(0).formula_of(0, 5),
            Some(if dest_sheet == 0 { "=B1" } else { "=Dest!B1" })
        );
        app.do_undo(false);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(11.0));
        assert_eq!(app.wb.sheet(0).formula_of(0, 5), Some("=A1"));
        app.do_undo(true);
        assert_eq!(app.wb.sheet(dest_sheet).value(1, 1), Value::Number(22.0));
    }
}

#[test]
fn a_cut_restores_names_chart_series_and_formula_values_in_one_step() {
    let env = TestEnv::new("cut-workbook-refs");
    let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
    app.wb
        .names
        .push(("Amount".into(), None, "Sheet1!$A$1".into()));
    let sh = app.wb.sheet_mut(0);
    sh.set(0, 0, number(11.0));
    sh.set(0, 1, number(22.0));
    sh.set(0, 5, formula("=Amount"));
    sh.charts.push(Chart {
        series: vec![Series {
            val_ref: "Sheet1!$A$1".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    app.wb.names_dirty = true;
    app.wb.sheet_mut(0).charts_dirty = true;
    let path = env.path("cut-chart.xlsx");
    sheetcore::xlsx::save_atomic(&path, &app.wb).unwrap();
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    assert!(!app.wb.sheet(0).charts[0].part.is_empty());
    app.recalc_all_and_deps_adaptive();
    paste(&mut app, (0, 0, 0, 0), 0, (0, 1), true);
    for (redo, expected) in [
        (None, "Sheet1!$B$1"),
        (Some(false), "Sheet1!$A$1"),
        (Some(true), "Sheet1!$B$1"),
    ] {
        if let Some(redo) = redo {
            app.do_undo(redo);
        }
        assert_eq!(app.wb.names[0].2, expected);
        assert_eq!(app.wb.sheet(0).charts[0].series[0].val_ref, expected);
        assert_eq!(app.wb.sheet(0).value(0, 5), Value::Number(11.0));
        sheetcore::xlsx::save_atomic_with_receipt(&path, &app.wb)
            .unwrap()
            .apply_to(&mut app.wb);
        let saved = sheetcore::xlsx::load(&path).unwrap();
        assert_eq!(saved.sheet(0).charts[0].series[0].val_ref, expected);
        assert_eq!(
            saved
                .names
                .iter()
                .find(|(name, _, _)| name == "Amount")
                .unwrap()
                .2,
            expected
        );
    }
}

#[test]
fn filtered_cut_rewrites_visible_cells_once_and_leaves_hidden_formulas_in_place() {
    let _env = TestEnv::new("filtered-cut-outside-refs");
    for end in [3, 4100] {
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let sh = app.wb.sheet_mut(0);
        sh.set(1, 0, number(11.0));
        sh.set(2, 0, formula("=A4"));
        sh.set(3, 0, number(33.0));
        sh.set(0, 5, formula("=A4+A2"));
        sh.hidden_rows.insert(2);
        app.recalc_all_and_deps_adaptive();
        paste(&mut app, (1, 0, end, 0), 0, (3, 0), true); // old A2 -> A4; old A4 -> A5
        let sh = app.wb.sheet(0);
        assert_eq!(sh.formula_of(0, 5), Some("=A5+A4"));
        assert_eq!(sh.formula_of(2, 0), Some("=A5"));
        assert_eq!(sh.value(0, 5), Value::Number(44.0));
        app.do_undo(false);
        assert_eq!(app.wb.sheet(0).formula_of(2, 0), Some("=A4"));
        assert_eq!(app.wb.sheet(0).value(3, 0), Value::Number(33.0));
    }
}

#[test]
fn external_sparse_paste_maps_filtered_rows_and_rejects_overflow_atomically() {
    let _env = TestEnv::new("sparse-office-filtered-dest");
    let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
    let xml = r#"<Workbook xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet><Table><Row><Cell><Data ss:Type="Number">11</Data></Cell></Row><Row><Cell><Data ss:Type="Number">22</Data></Cell></Row></Table></Worksheet></Workbook>"#;
    let block = sheetcore::clipboard_xmlss::parse_sparse_ext(xml, false).unwrap();
    let sh = app.wb.sheet_mut(0);
    sh.set(1, 0, number(999.0));
    sh.hidden_rows.insert(1);
    assert!(app.paste_sparse_xmlss_ext(&block, true));
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(999.0));
    assert_eq!(app.wb.sheet(0).value(2, 0), Value::Number(22.0));
    app.do_undo(false);
    let last = sheetcore::sync::MAX_ROWS - 1;
    app.wb.sheet_mut(0).hidden_rows.insert(last);
    app.sel = (last - 1, 0);
    let history = app.undo.depth();
    assert!(app.paste_sparse_xmlss_ext(&block, true));
    assert_eq!(app.undo.depth(), history);
    assert_eq!(app.wb.sheet(0).value(last - 1, 0), Value::Empty);
}

#[test]
fn a_full_column_cannot_fit_into_a_filtered_destination() {
    let _env = TestEnv::new("filtered-column-destination");
    let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
    app.wb.sheets.push(Sheet::new("Dest"));
    app.wb.sheet_mut(0).set(1, 0, number(22.0));
    app.wb.sheet_mut(1).hidden_rows.insert(1);
    app.wb.sheet_mut(1).set(1, 1, number(999.0));
    let history = app.undo.depth();
    paste(
        &mut app,
        (0, 0, sheetcore::sync::MAX_ROWS - 1, 0),
        1,
        (0, 1),
        false,
    );
    assert_eq!(app.undo.depth(), history);
    assert_eq!(app.wb.sheet(1).value(1, 1), Value::Number(999.0));
}

#[test]
fn cut_reference_conflicts_leave_values_history_and_clipboard_intact() {
    let _env = TestEnv::new("cut-reference-conflicts");
    let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
    app.wb
        .names
        .push(("Amount".into(), None, "Sheet1!$A$1".into()));
    let sh = app.wb.sheet_mut(0);
    sh.set(0, 0, number(11.0));
    sh.set(1, 0, number(12.0));
    sh.set(0, 1, number(22.0));
    app.net = Some(crate::net::host_for_test(91, "host", "cut", "pw").0);
    let history = app.undo.depth();
    paste(&mut app, (0, 0, 1, 0), 0, (0, 1), true);
    assert!(app.status.contains("协作协议"));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(11.0));
    assert_eq!(app.undo.depth(), history);
    assert!(app.clip.as_ref().is_some_and(|clip| clip.cut));
    let net = app.net.take();
    app.clip_paste(false, app.clip_stamp.clone());
    let history = app.undo.depth();
    assert_eq!(app.wb.names[0].2, "Sheet1!$B$1");
    app.net = net;
    assert!(!app.apply_undo(false));
    assert_eq!(app.undo.depth(), history);
    app.net = None;
    app.wb.sheet_mut(0).set(1, 1, number(77.0)); // conflict on another row of the same cut
    assert!(!app.apply_undo(false));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(11.0));
    assert_eq!(app.undo.depth(), history);
    app.wb.sheet_mut(0).set(1, 1, number(12.0));
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.names[0].2, "Sheet1!$A$1");
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(22.0));
}

#[test]
fn many_isolated_visible_rows_keep_hidden_and_inherited_formats() {
    let _env = TestEnv::new("filtered-row-style-runs");
    let mut sh = Sheet::new("Filtered");
    for row in 0..20_000 {
        sh.row_styles.insert(row, 1);
    }
    let visible: Vec<_> = (0..20_000).step_by(2).map(|r| (r, r)).collect();
    sh.clear_styles_in_row_runs(0, 0, &visible);
    for row in [0, 17_998, 19_998] {
        assert_eq!(sh.style_of(row, 0), 0);
        assert_eq!(sh.style_of(row + 1, 0), 1);
    }
}

#[test]
fn eager_cut_does_not_clear_peer_edits_and_ignores_formula_cache_changes() {
    let _env = TestEnv::new("eager-cut-source");
    let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
    let ctx = egui::Context::default();
    let mut peer = sheetcore::sync::Peer::new(22);
    app.peer = Some(sheetcore::sync::Peer::new(11));
    app.net = Some(crate::net::host_for_test(11, "host", "eager-cut", "pw").0);
    app.wb.sheet_mut(0).set(0, 0, number(11.0));
    app.sel = (0, 0);
    app.sel_end = app.sel;
    app.clip_copy(true, &ctx);
    let op = peer.local_set(0, 0, 0, &number(99.0));
    let local = app.peer.as_mut().unwrap();
    assert!(local.apply(op));
    local.apply_list_to_workbook(&mut app.wb, &[(0, 0, 0)]);
    app.sel = (0, 1);
    app.sel_end = app.sel;
    let history = app.undo.depth();
    app.clip_paste(false, app.clip_stamp.clone());
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(99.0));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty);
    assert_eq!(app.undo.depth(), history);
    assert!(app.clip.as_ref().is_some_and(|c| c.cut));
    assert!(app.status.contains("剪切源已更改"));

    app.net = None;
    let sh = app.wb.sheet_mut(0);
    sh.set(0, 0, formula("=D1"));
    sh.set(0, 3, number(1.0));
    app.recalc_all_and_deps_adaptive();
    app.sel = (0, 0);
    app.sel_end = app.sel;
    app.clip_copy(true, &ctx);
    app.wb.sheet_mut(0).set(0, 3, number(2.0));
    app.recalc_all_and_deps_adaptive();
    app.sel = (0, 1);
    app.sel_end = app.sel;
    app.clip_paste(false, app.clip_stamp.clone());
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty);
}

#[test]
fn filtered_eager_cut_validates_visible_notes_and_explicit_styles_before_clearing() {
    let _env = TestEnv::new("eager-cut-style-and-notes");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        bold: true,
        ..Default::default()
    });
    for change_note in [false, true] {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, number(11.0));
        sh.set(2, 0, number(33.0));
        sh.hidden_rows.insert(1);
        app.sel = (0, 0);
        app.sel_end = (2, 0);
        app.clip_copy(true, &ctx);
        let sh = app.wb.sheet_mut(0);
        if change_note {
            sh.notes.insert(
                sheetcore::CellRef::new(2, 0),
                sheetcore::notes::Note {
                    r: 2,
                    c: 0,
                    text: "new note".into(),
                    author: "peer".into(),
                },
            );
        } else {
            sh.set_style(2, 0, sid);
        }
        app.sel = (5, 1);
        app.sel_end = app.sel;
        app.clip_paste(false, app.clip_stamp.clone());
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(11.0));
        assert_eq!(app.wb.sheet(0).value(5, 1), Value::Empty);
        assert!(app.status.contains("剪切源已更改"));
    }
}
