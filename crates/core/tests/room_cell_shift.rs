use sheetcore::{
    Cell, Sheet, Value, Workbook,
    cell_shift_patch::{CellShiftOverlay, CellShiftPatch},
    cells::Shift,
    cf::{CfBlock, CfKind, CfRule},
    dv::DataValidation,
    external::{Kind, Source},
    hyperlinks::Hyperlink,
    model::CellRef,
    notes::{Note, ThreadEntry, ThreadedComment},
    richtext::RichRun,
    structural::CellShift,
    style::CellStyle,
};

fn fixture() -> Workbook {
    let mut wb = Workbook::new_blank();
    let target = &mut wb.sheets[0];
    target.name = "Data".into();
    target.set(
        0,
        1,
        Cell {
            value: Value::Text("Rich".into()),
            formula: None,
        },
    );
    let key = CellRef::new(0, 1);
    target.notes.insert(
        key,
        Note {
            r: 0,
            c: 1,
            text: "note".into(),
            author: "Alice".into(),
        },
    );
    target.threads.insert(
        key,
        ThreadedComment {
            r: 0,
            c: 1,
            resolved: false,
            entries: vec![ThreadEntry {
                id: "id".into(),
                author: "Alice".into(),
                timestamp: "".into(),
                text: "original".into(),
            }],
        },
    );
    target.rich.insert(
        (0, 1),
        vec![RichRun {
            bold: true,
            ..RichRun::plain("Rich")
        }],
    );
    target.hyperlinks.insert(
        (0, 1),
        Hyperlink {
            location: "Data!B1".into(),
            ..Default::default()
        },
    );
    target.merges.push((0, 1, 1, 1));
    let style = wb.styles.intern_with_quote(
        CellStyle {
            italic: true,
            ..CellStyle::default_dark()
        },
        true,
    );
    target.set_style(0, 1, style);
    let mut other = Sheet::new("Summary");
    other.set(
        4,
        0,
        Cell {
            formula: Some("=Data!B1".into()),
            value: Value::Number(9.0),
        },
    );
    other.cf.blocks.push(CfBlock {
        ranges: vec![(1, 0, 3, 0)],
        rules: vec![CfRule {
            kind: CfKind::Expr {
                formula: "Data!B1".into(),
                dxf: 0,
            },
            stop: false,
            priority: 1,
        }],
    });
    other.dv.push(DataValidation {
        ranges: vec![(2, 0, 3, 0)],
        f1: "Data!B1".into(),
        ..Default::default()
    });
    wb.sheets.push(other);
    wb.sources.push(Source {
        name: "incoming".into(),
        kind: Kind::Csv {
            path: "PRIVATE-HOST-PATH".into(),
            delim: ',',
            header: true,
        },
        at: (0, 1),
        target_sheet: Some("Data".into()),
        rows: 2,
        cols: 1,
        csv_opts: None,
    });
    wb
}

#[test]
fn room_shift_replays_all_metadata_styles_and_foreign_references_atomically() {
    let mut host = fixture();
    let baseline = CellShiftOverlay::capture(&host);
    let op = CellShift::new((0, 1, 1, 1), Shift::Horizontal, true);
    let patch = CellShiftPatch::prepare(&host, 0, op).unwrap();
    let json = serde_json::to_string(&patch).unwrap();
    assert!(!json.contains("PRIVATE-HOST-PATH"));
    let patch: CellShiftPatch = serde_json::from_str(&json).unwrap();
    let mut replica = host.clone();
    replica.sources.clear();
    replica.sheets[1].set(
        4,
        0,
        Cell {
            formula: Some("=Data!B1".into()),
            value: Value::Number(100.0),
        },
    );
    let guest_request = CellShiftPatch::prepare(&replica, 0, op).unwrap();
    assert_eq!(guest_request.canonical_forward(&host).unwrap(), patch);
    let mut forged = guest_request.clone();
    forged.cells.clear();
    assert!(forged.canonical_forward(&host).is_none());

    assert!(patch.apply(&mut host, true));
    assert!(patch.apply_committed(&mut replica, true));
    let sh = &host.sheets[0];
    assert_eq!(sh.value(0, 2), Value::Text("Rich".into()));
    assert!(host.styles.quote_of(sh.explicit_style_of(0, 2).unwrap()));
    assert_eq!(sh.rich[&(0, 2)][0].text, "Rich");
    assert_eq!(sh.notes[&CellRef::new(0, 2)].c, 2);
    assert_eq!(sh.threads[&CellRef::new(0, 2)].c, 2);
    assert!(sh.hyperlinks.contains_key(&(0, 2)));
    assert_eq!(sh.merges.as_ref(), &[(0, 2, 1, 2)]);
    assert_eq!(host.sources[0].at, (0, 2));
    assert_eq!(host.sheets[1].dv[0].f1, "Data!C1");
    assert!(
        matches!(&host.sheets[1].cf.blocks[0].rules[0].kind,CfKind::Expr{formula,..} if formula=="Data!C1")
    );
    assert_eq!(
        host.sheets[1].get(4, 0).unwrap().formula.as_deref(),
        Some("=Data!C1")
    );
    assert!(patch.matches_replica(&replica, false));
    assert!(!patch.matches(&replica, false));

    let edited = &mut host.sheets[0]
        .threads
        .get_mut(&CellRef::new(0, 2))
        .unwrap()
        .entries[0]
        .text;
    *edited = "later".into();
    let before_conflict = CellShiftOverlay::capture(&host);
    let epoch = host.sheets[0].coordinate_revision();
    assert!(!patch.apply(&mut host, false));
    assert_eq!(host.sheets[0].coordinate_revision(), epoch);
    assert_eq!(CellShiftOverlay::capture(&host), before_conflict);
    host.sheets[0]
        .threads
        .get_mut(&CellRef::new(0, 2))
        .unwrap()
        .entries[0]
        .text = "original".into();
    assert!(patch.apply(&mut host, false));
    assert_eq!(CellShiftOverlay::capture(&host), baseline);
    assert!(patch.apply(&mut host, true));
    assert_eq!(host.sources[0].at, (0, 2));
}

#[test]
fn room_shift_reconnect_uses_current_metadata_local_source_path_and_local_values() {
    let mut host = fixture();
    host.sheets[0].merges.clear();
    // A partial cut across the destination footprint detaches this source.
    let op = CellShift::new((0, 1, 0, 1), Shift::Horizontal, true);
    let patch = CellShiftPatch::prepare(&host, 0, op).unwrap();
    assert!(patch.apply(&mut host, true));
    assert!(matches!(host.sources[0].kind, Kind::Unbound(_)));
    host.sheets[0].notes.remove(&CellRef::new(0, 2));
    host.sheets[0]
        .threads
        .get_mut(&CellRef::new(0, 2))
        .unwrap()
        .entries[0]
        .text = "late reply".into();
    let overlay = CellShiftOverlay::capture(&host);
    let wire = serde_json::to_string(&overlay).unwrap();
    assert!(!wire.contains("PRIVATE-HOST-PATH"));
    let overlay: CellShiftOverlay = serde_json::from_str(&wire).unwrap();
    let mut joining = fixture();
    if let Kind::Csv { path, .. } = &mut joining.sources[0].kind {
        *path = "GUEST-PATH".into();
    }
    joining.sheets[0].set(
        12,
        4,
        Cell {
            value: Value::Number(53.0),
            formula: None,
        },
    );
    assert!(overlay.apply_to(&mut joining));
    assert_eq!(joining.sheets[0].value(12, 4), Value::Number(53.0));
    assert!(!joining.sheets[0].notes.contains_key(&CellRef::new(0, 2)));
    assert_eq!(
        joining.sheets[0].threads[&CellRef::new(0, 2)].entries[0].text,
        "late reply"
    );
    assert!(
        matches!(&joining.sources[0].kind,Kind::Unbound(inner) if matches!(&**inner,Kind::Csv{path,..} if path=="GUEST-PATH"))
    );
    joining.sources.clear();
    assert!(overlay.apply_to(&mut joining));
    assert!(joining.sources.is_empty());
}

#[test]
fn room_shift_rejects_new_binding_and_rich_text_in_replayed_band_and_keeps_other_edits() {
    let mut wb = fixture();
    let source = wb.sources.remove(0);
    wb.sheets[0].rich.clear();
    let op = CellShift::new((0, 1, 1, 1), Shift::Horizontal, true);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.apply(&mut wb, true));
    wb.sources.push(source);
    assert!(!patch.apply(&mut wb, false));
    wb.sources[0].at = (9, 9);
    wb.sheets[0]
        .rich
        .insert((0, 2), vec![RichRun::plain("Rich")]);
    assert!(!patch.apply(&mut wb, false));
    wb.sheets[0].rich.remove(&(0, 2));
    wb.sheets[0].notes.insert(
        CellRef::new(9, 9),
        Note {
            r: 9,
            c: 9,
            author: "Else".into(),
            text: "kept".into(),
        },
    );
    assert!(patch.apply(&mut wb, false));
    assert_eq!(wb.sheets[0].notes[&CellRef::new(9, 9)].text, "kept");
    assert_eq!(wb.sources[0].at, (9, 9));
}

#[test]
fn room_shift_empty_geometry_invalidates_only_its_target_and_checks_bounds() {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Other"));
    let epoch = [
        wb.sheets[0].coordinate_revision(),
        wb.sheets[1].coordinate_revision(),
    ];
    let op = CellShift::new((4, 4, 5, 4), Shift::Vertical, false);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.is_empty());
    assert!(patch.replay_metadata(&mut wb, true));
    assert_ne!(wb.sheets[0].coordinate_revision(), epoch[0]);
    assert_eq!(wb.sheets[1].coordinate_revision(), epoch[1]);
    let mut invalid = patch;
    invalid.rect.2 = sheetcore::sync::MAX_ROWS;
    assert!(!invalid.wire_valid());
    assert!(!invalid.apply(&mut wb, true));
}

#[test]
fn room_shift_overlay_preserves_extended_formats_and_clears_snapshot_overrides() {
    use sheetcore::style_ext::FillPattern;
    use sheetcore::style_theme::{ThemeRef, XfTheme};
    let mut host = Workbook::new_blank();
    let style = host.styles.intern_with_ext(
        CellStyle {
            bold: true,
            ..CellStyle::default_dark()
        },
        XfTheme {
            font: Some(ThemeRef { idx: 4, tint: 0.25 }),
            fill: None,
        },
        Some(FillPattern {
            kind: "darkGrid".into(),
            fg: None,
            bg: None,
            fg_theme: Some((3, 0.1)),
            bg_theme: None,
            fg_indexed: None,
            bg_indexed: None,
        }),
        true,
    );
    host.sheets[0].set_row_style(3, style);
    host.sheets[0].set_style(3, 1, 0); // explicitly override the inherited row format
    host.sheets[0].set_style(1, 2, style);
    let overlay = CellShiftOverlay::capture(&host);
    let mut guest = Workbook::new_blank();
    guest.styles.intern(CellStyle {
        italic: true,
        ..CellStyle::default_dark()
    });
    guest.sheets[0].set_row_style(3, 1);
    guest.sheets[0].set_style(3, 5, 0); // ordinary snapshot left a stale explicit default
    assert!(overlay.apply_to(&mut guest));
    let sh = &guest.sheets[0];
    assert_eq!(sh.explicit_style_of(3, 5), None);
    assert_eq!(sh.explicit_style_of(3, 1), Some(0));
    let id = sh.explicit_style_of(1, 2).unwrap();
    assert_eq!(sh.style_of(3, 5), id);
    assert_eq!(guest.styles.pattern_of(id), host.styles.pattern_of(style));
    assert_eq!(guest.styles.theme_of(id), host.styles.theme_of(style));
    assert!(guest.styles.quote_of(id));
}

#[test]
fn room_shift_overlay_serializes_large_uniform_styles_as_column_runs() {
    let mut wb = Workbook::new_blank();
    let id = wb.styles.intern(CellStyle {
        bold: true,
        ..CellStyle::default_dark()
    });
    wb.sheets[0].styles.insert_rect(0, 0, 262_143, 8, id);
    let overlay = CellShiftOverlay::capture(&wb);
    assert!(overlay.wire_valid());
    assert!(serde_json::to_vec(&overlay).unwrap().len() < 3_000);
    let mut replica = Workbook::new_blank();
    assert!(overlay.apply_to(&mut replica));
    assert_eq!(replica.sheets[0].styles.len(), 262_144 * 9);
    assert!(
        replica
            .styles
            .get(replica.sheets[0].explicit_style_of(262_143, 8).unwrap())
            .bold
    );
}

#[test]
fn room_shift_internal_binding_resizes_only_its_read_area_and_refuses_a_deleted_source_on_undo() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets.push(Sheet::new("Output"));
    wb.sources.push(Source {
        name: "inside".into(),
        kind: Kind::Internal {
            sheet: "Data".into(),
            rect: (0, 0, 2, 0),
        },
        at: (2, 2),
        target_sheet: Some("Output".into()),
        rows: 3,
        cols: 1,
        csv_opts: None,
    });
    let op = CellShift::new((1, 0, 1, 0), Shift::Vertical, true);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.apply(&mut wb, true));
    let source = wb.sources.pop().unwrap();
    assert_eq!(source.at, (2, 2));
    assert!(matches!(
        source.kind,
        Kind::Internal {
            rect: (0, 0, 3, 0),
            ..
        }
    ));
    assert!(!patch.apply(&mut wb, false));
    wb.sources.push(source);
    assert!(patch.apply(&mut wb, false));
    assert!(matches!(
        wb.sources[0].kind,
        Kind::Internal {
            rect: (0, 0, 2, 0),
            ..
        }
    ));
}

#[test]
fn room_shift_rich_only_changes_have_cell_clocks_and_preserve_a_later_winner() {
    let mut wb = Workbook::new_blank();
    for col in [1, 2] {
        wb.sheets[0].set(
            0,
            col,
            Cell {
                value: Value::Text("same".into()),
                formula: None,
            },
        );
        wb.sheets[0].rich.insert(
            (0, col),
            vec![RichRun {
                bold: col == 1,
                ..RichRun::plain("same")
            }],
        );
    }
    let patch = CellShiftPatch::prepare(
        &wb,
        0,
        CellShift::new((0, 1, 0, 1), Shift::Horizontal, true),
    )
    .unwrap();
    let rich_only = patch
        .cells
        .iter()
        .find(|c| (c.row, c.col) == (0, 2))
        .unwrap();
    assert_eq!(rich_only.before, rich_only.after);
    wb.sheets[0].set(
        0,
        2,
        Cell {
            value: Value::Text("later".into()),
            formula: None,
        },
    );
    wb.sheets[0]
        .rich
        .insert((0, 2), vec![RichRun::plain("later")]);
    assert!(patch.replay_metadata_for_cells(&mut wb, true, |row, col| (row, col) != (0, 2)));
    assert_eq!(wb.sheets[0].rich[&(0, 2)][0].text, "later");
    assert!(!wb.sheets[0].rich.contains_key(&(0, 1)));
}

#[test]
fn room_shift_preserves_merge_order_and_checks_later_intersections() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].merges = vec![(0, 1, 0, 2), (2, 1, 2, 2)].into();
    let op = CellShift::new((0, 1, 0, 1), Shift::Horizontal, true);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.apply(&mut wb, true));
    assert_eq!(
        wb.sheets[0].merges.to_vec(),
        vec![(0, 2, 0, 3), (2, 1, 2, 2)]
    );
    wb.sheets[0].merges.push((0, 0, 0, 1));
    assert!(!patch.apply(&mut wb, false));
    wb.sheets[0].merges.pop();
    assert!(patch.apply(&mut wb, false));
    assert_eq!(
        wb.sheets[0].merges.to_vec(),
        vec![(0, 1, 0, 2), (2, 1, 2, 2)]
    );

    // Some imported files have two merges with a common anchor. The earlier
    // range is the one Excel-compatible lookup chooses at the common point.
    wb.sheets[0].merges.push((0, 1, 0, 3));
    let baseline = wb.sheets[0].merges.clone();
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.apply(&mut wb, true));
    assert_eq!(
        wb.sheets[0].merges.to_vec(),
        vec![(0, 2, 0, 3), (2, 1, 2, 2), (0, 2, 0, 4)]
    );
    assert!(patch.apply(&mut wb, false));
    assert_eq!(wb.sheets[0].merges, baseline);
}
