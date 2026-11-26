//! Plan large sorts and duplicate removal on a read-only snapshot. Recheck the
//! document and the full selection's permissions before applying one undoable edit.

use super::SheetApp;
use sheetcore::{Sheet, data_tools, sort_filter};
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[cfg(test)]
fn sort_apps(env: &crate::test_env::TestEnv) -> (SheetApp,SheetApp) {
    use sheetcore::{Cell,Value};
    use sheetcore::sync::{Peer,Cursor};
    let mut host = SheetApp::new_with_context(&eframe::egui::Context::default(),None);
    let mut guest = SheetApp::new_with_context(&eframe::egui::Context::default(),None);
    host.wb.path = Some(env.path("host.xlsx"));
    guest.wb.path = Some(env.path("guest.xlsx"));
    let number = |n| Cell { value: Value::Number(n), formula: None };
    let (net, port) = crate::net::host_for_test(1,"H","doc","sort");
    let mut peer = Peer::new(1);
    for (r,n) in [(0,2.),(1,1.)] {
        host.wb.sheet_mut(0).set(r,0,number(n));
        peer.seed_cell(0,r,0,&number(n));
    }
    let mut fmt = sheetcore::style::CellStyle::default();
    fmt.bold = true;
    let bold = host.wb.styles.intern(fmt);
    host.wb.sheet_mut(0).set_style(0,0,bold);
    net.seed_snapshot(&peer,&host.wb,Cursor { site:1, name:"H".into(), color:1, sheet:0, row:0, col:0, end_row: None, end_col: None });
    host.peer = Some(peer);
    host.net = Some(net);
    host.undo.set_site(1);
    guest.net = Some(crate::net::join(2,"G".into(),"doc".into(),&format!("127.0.0.1:{port}"),"sort".into()).unwrap());
    guest.peer = Some(Peer::new(2));
    guest.undo.set_site(2);
    until(&mut host,&mut guest,|_,g| !g.in_snapshot && g.wb.sheet(0).value(0,0)==Value::Number(2.));
    (host,guest)
}

#[cfg(test)]
fn until(h: &mut SheetApp,g: &mut SheetApp,mut done: impl FnMut(&SheetApp,&SheetApp)-> bool) {
    let start=std::time::Instant::now();
    while !done(h,g) {
        h.poll_net(); g.poll_net();
        assert!(start.elapsed().as_secs()<10, "H={} G={}",h.status,g.status);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn guest_structural_denial_keeps_coordinates_and_next_sort_converges() {
    use sheetcore::{Value, structural::Structural};
    let env = crate::test_env::TestEnv::new("ordered_structure_denial");
    let (mut host, mut guest) = sort_apps(&env);
    // The authoritative lease has not reached the guest's UI when it requests the insert.
    host.net.as_ref().unwrap().request_lock(0,1,0,"H");
    guest.structural(Structural::InsertRows { at:0,n:1 });
    assert!(guest.net.as_ref().unwrap().pending_structure());
    assert_eq!(guest.wb.sheet(0).value(0,0),Value::Number(2.));
    let start = std::time::Instant::now();
    while guest.net.as_ref().unwrap().pending_structure() {
        guest.poll_net();
        assert!(start.elapsed().as_secs() < 5);
        std::thread::yield_now();
    }
    assert_eq!(guest.undo.depth().0,0);
    assert_eq!(guest.net.as_ref().unwrap().structure_seen(),0);
    host.net.as_ref().unwrap().release_lock(0,1,0);
    until(&mut host,&mut guest,|_,g| g.locks.is_empty());
    guest.sort_region((0,0,1,0),0,true);
    until(&mut host,&mut guest,|h,g| h.wb.sheet(0).value(0,0)==Value::Number(1.)
        && g.net.as_ref().unwrap().pending_sort_ranges().is_empty());
    guest.structural(Structural::InsertRows { at:0,n:1 });
    until(&mut host,&mut guest,|h,g| h.wb.sheet(0).value(1,0)==Value::Number(1.)
        && g.wb.sheet(0).value(1,0)==Value::Number(1.));
    for app in [&host,&guest] {
        assert_eq!(app.net.as_ref().unwrap().structure_seen(),1);
        assert_eq!(app.wb.sheet(0).value(2,0),Value::Number(2.));
    }
    assert_eq!(guest.undo.depth().0,2);
}

#[test]
fn future_sort_epoch_repairs_the_guest_without_disconnecting() {
    use sheetcore::Value;
    let env = crate::test_env::TestEnv::new("future_sort_epoch");
    let (mut host,mut guest) = sort_apps(&env);
    guest.net.as_ref().unwrap().note_structure(1);
    guest.sort_region((0,0,1,0),0,true);
    until(&mut host,&mut guest,|_,g| g.net.as_ref().unwrap().structure_seen()==0
        && g.net.as_ref().unwrap().pending_sort_ranges().is_empty());
    assert_eq!(guest.wb.sheet(0).value(0,0),Value::Number(2.));
    assert_eq!(guest.undo.depth().0,0);
    assert!(!guest.room_lost);
    guest.sort_region((0,0,1,0),0,true);
    until(&mut host,&mut guest,|h,g| h.wb.sheet(0).value(0,0)==Value::Number(1.)
        && g.net.as_ref().unwrap().pending_sort_ranges().is_empty());
    assert_eq!(guest.wb.sheet(0).value(1,0),Value::Number(2.));
}

#[test]
fn rejected_sort_crossing_a_structural_frame_repairs_moved_cells_and_history() {
    use sheetcore::Value;
    for locked in [true,false] {
        let env = crate::test_env::TestEnv::new(if locked {"sort_locked_shift"} else {"sort_fallback_shift"});
        let (mut host,mut guest) = sort_apps(&env);
        let (paused,wait) = mpsc::channel();
        let (release,resume) = mpsc::channel();
        *host.net.as_ref().unwrap().before_sort_reply.lock().unwrap() = Some(Box::new(move || {
            paused.send(()).unwrap(); resume.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        }));
        if locked {
            host.net.as_ref().unwrap().request_lock(0,0,0,"H");
        } else {
            // Force a full-data retry after the host verifies a different source.
            guest.wb.sheet_mut(0).set(0,0,sheetcore::Cell { value: Value::Number(8.),formula: None });
        }
        guest.sort_region((0,0,1,0),0,true);
        wait.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        host.structural(sheetcore::structural::Structural::InsertRows { at:0, n:1 });
        assert_eq!(host.wb.sheet(0).value(1,0),Value::Number(2.));
        release.send(()).unwrap();
        until(&mut host,&mut guest,|_,g| g.wb.sheet(0).value(2,0)==Value::Number(1.)
            && g.undo.depth().0==0 && g.net.as_ref().unwrap().pending_sort_ranges().is_empty());
        for row in 0..3 {
            assert_eq!(guest.wb.sheet(0).value(row,0),host.wb.sheet(0).value(row,0));
            assert_eq!(guest.wb.styles.get(guest.wb.sheet(0).style_of(row,0)),
                       host.wb.styles.get(host.wb.sheet(0).style_of(row,0)));
        }
    }
}

#[test]
fn disconnected_sort_is_not_replayed_as_cells_but_a_later_ordinary_edit_is() {
    use sheetcore::{Cell,Value};
    let env = crate::test_env::TestEnv::new("sort_disconnect_repair");
    let (mut host,mut guest)=sort_apps(&env);
    let (paused,wait)=mpsc::channel();
    let (release,resume)=mpsc::channel();
    *host.net.as_ref().unwrap().before_sort_reply.lock().unwrap()=Some(Box::new(move || {
        paused.send(()).unwrap(); resume.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    }));
    host.net.as_ref().unwrap().request_lock(0,0,0,"H");
    guest.sort_region((0,0,1,0),0,true);
    wait.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    guest.net.as_ref().unwrap().disconnect_socket_for_test();
    guest.wb.sheet_mut(0).set(3,0,Cell {value:Value::Number(77.),formula:None});
    guest.broadcast_cells(&[(0,3,0)]);
    release.send(()).unwrap();
    until(&mut host,&mut guest,|h,g| g.undo.depth().0==0 && !g.in_snapshot
        && h.wb.sheet(0).value(3,0)==Value::Number(77.) && g.wb.sheet(0).value(1,0)==Value::Number(1.));
    for app in [&host,&guest] {
        assert_eq!(app.wb.sheet(0).value(0,0),Value::Number(2.));
        assert_eq!(app.wb.sheet(0).value(1,0),Value::Number(1.));
        assert_eq!(app.wb.sheet(0).value(3,0),Value::Number(77.));
    }
}

#[test]
fn sort_ready_and_optimistic_style_follow_host_order_on_the_writing_guest() {
    use sheetcore::Value;
    let env = crate::test_env::TestEnv::new("sort_optimistic_style");
    let (mut host,mut guest) = sort_apps(&env);
    host.sort_region((0,0,1,0),0,true);
    let guest_net = guest.net.as_ref().unwrap();
    // Capture the pre-sort model. The worker can finish, but the guest has not
    // yet applied its validated result when the user changes the target format.
    loop {
        let event=guest_net.events.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        if let crate::net::NetEvent::Sort(plan,reply) = event {
            let sh=guest.wb.sheet(0);
            let target=crate::net_sort::SortTarget { document:guest.doc_uid,sheet:0,
                identity:sh.identity,coordinates:sh.coordinate_revision() };
            guest_net.check_sort(target,plan,sh.search_snapshot(),guest.wb.styles.clone(),reply);
            break;
        }
    }
    let mut italic=sheetcore::style::CellStyle::default_dark();
    italic.italic=true;
    let id=guest.wb.styles.intern(italic.clone());
    guest.wb.sheet_mut(0).set_style(1,0,id);
    guest.broadcast_cell_styles(&[(0,1,0)]);
    until(&mut host,&mut guest,|h,g| g.wb.sheet(0).value(1,0)==Value::Number(2.)
        && [&h,&g].iter().all(|app| app.wb.styles.get(app.wb.sheet(0).style_of(1,0))==&italic));
}

#[test]
fn negotiated_sort_and_guarded_undo_converge_through_both_apps() {
    use sheetcore::{Cell,Value};
    let env = crate::test_env::TestEnv::new("sort_app_roundtrip");
    let (mut host,mut guest) = sort_apps(&env);
    let number = |n| Cell { value: Value::Number(n), formula: None };
    guest.sort_region((0,0,1,0),0,true);
    until(&mut host,&mut guest,|h,_| h.wb.sheet(0).value(1,0)==Value::Number(2.));
    assert!(host.wb.styles.get(host.wb.sheet(0).style_of(1,0)).bold);
    assert!(host.rev.iter().any(|e| e.remote && e.label=="远端排序"));
    assert!(guest.apply_undo(false));
    until(&mut host,&mut guest,|h,_| h.wb.sheet(0).value(1,0)==Value::Number(1.));
    assert!(guest.apply_undo(true));
    until(&mut host,&mut guest,|h,_| h.wb.sheet(0).value(1,0)==Value::Number(2.));
    host.wb.sheet_mut(0).set(0,0,number(99.));
    host.broadcast_cells(&[(0,0,0)]);
    until(&mut host,&mut guest,|_,g| g.wb.sheet(0).value(0,0)==Value::Number(99.));
    assert!(!guest.apply_undo(false), "a newer remote cell is skipped");
    until(&mut host,&mut guest,|h,_| h.wb.sheet(0).value(1,0)==Value::Number(1.));
    for app in [&host,&guest] { assert_eq!(app.wb.sheet(0).value(0,0),Value::Number(99.)); }
    // A different local cached result must not be permuted into the host's data.
    guest.wb.sheet_mut(0).set(1,0,number(777.));
    host.sort_region((0,0,1,0),0,true);
    until(&mut host,&mut guest,|_,g| g.wb.sheet(0).value(1,0)==Value::Number(99.));
    assert_eq!(guest.wb.sheet(0).value(0,0),Value::Number(1.));
    // The host grants a lease just after the guest's last event poll. Reject
    // the entire next sort; a newer optimistic edit at another cell must survive.
    host.editing = Some(super::EditState::new("1".into()));
    host.edit_sheet = Some(0);
    host.edit_sel = Some((0,0));
    host.net.as_ref().unwrap().request_lock(0,0,0,"H");
    let depth = guest.undo.depth().0;
    guest.sort_region((0,0,1,0),0,false);
    assert_eq!(guest.wb.sheet(0).value(0,0),Value::Number(99.));
    guest.wb.sheet_mut(0).set(1,0,number(55.));
    guest.broadcast_cells(&[(0,1,0)]);
    until(&mut host,&mut guest,|h,g| h.wb.sheet(0).value(1,0)==Value::Number(55.)
        && g.wb.sheet(0).value(0,0)==Value::Number(1.));
    assert_eq!(guest.wb.sheet(0).value(1,0),Value::Number(55.));
    assert_eq!(host.wb.sheet(0).value(0,0),Value::Number(1.));
    assert_eq!(guest.undo.depth().0,depth, "a rejected sort leaves no undo action");
}

#[test]
fn leaving_room_preserves_peer_value_when_undoing_and_redoing_my_edit() {
    use sheetcore::{Cell,Value};
    let env = crate::test_env::TestEnv::new("undo_after_leaving_room");
    let (mut host,mut guest) = sort_apps(&env);
    let number = |n| Cell { value: Value::Number(n), formula: None };
    let saved = sheetcore::undo::capture(guest.wb.sheet(0),0,&[(0,0),(1,0)]);
    guest.wb.sheet_mut(0).set(0,0,number(8.));
    guest.wb.sheet_mut(0).set(1,0,number(9.));
    guest.record_deltas("输入", sheetcore::undo::finish(saved,guest.wb.sheet(0)));
    guest.broadcast_cells(&[(0,0,0),(0,1,0)]);
    until(&mut host,&mut guest,|h,_| h.wb.sheet(0).value(1,0)==Value::Number(9.));
    host.wb.sheet_mut(0).set(0,0,number(2.)); // matches the old value of the guest's edit
    host.broadcast_cells(&[(0,0,0)]);
    until(&mut host,&mut guest,|_,g| g.wb.sheet(0).value(0,0)==Value::Number(2.));
    guest.leave_session_at(None);
    assert!(!guest.apply_undo(false));
    assert!(guest.status.contains("1 格"));
    assert_eq!(guest.wb.sheet(0).value(1,0),Value::Number(1.));
    assert!(guest.apply_undo(true));
    assert_eq!(guest.wb.sheet(0).value(1,0),Value::Number(9.));
    assert_eq!(guest.wb.sheet(0).value(0,0),Value::Number(2.));
}

#[test]
fn dense_multi_sort_uses_the_stable_multi_key_permutation_for_undo() {
    use sheetcore::{Cell,Value};
    let _env = crate::test_env::TestEnv::new("sort_compact_multi");
    let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(),None);
    let sh = app.wb.sheet_mut(0);
    for row in 0..1001 {
        for col in 0..101 {
            let n = match col { 0 => row % 2, 1 => row % 5, _ => row };
            sh.set(row,col,Cell { value: Value::Number(n as f64),formula: None });
        }
    }
    let rect = (0,0,1000,100);
    let keys = vec![data_tools::SortKey::text(0,true),data_tools::SortKey::text(1,false)];
    let data_tools::MultiSortPlan::Compact(compact) =
        data_tools::plan_large_sort_multi(sh,0,rect,&keys,false).unwrap() else { panic!("expected compact history"); };
    let (_,_,order) = compact.permutation();
    assert_eq!(&order[..3], &[4,14,24]);
    drop(compact);
    app.run_reorder(rect,Operation::Multi { keys,header:false,horizontal:false,styles:None });
    let start = std::time::Instant::now();
    while app.tasks.reorder.is_some() {
        app.poll_reorder();
        assert!(start.elapsed().as_secs()<30);
        std::thread::yield_now();
    }
    assert_eq!(app.wb.sheet(0).value(0,2),Value::Number(4.));
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheet(0).value(0,2),Value::Number(0.));
    assert!(app.apply_undo(true));
    assert_eq!(app.wb.sheet(0).value(1,2),Value::Number(14.));
}

#[test]
fn dense_numeric_sort_keeps_revision_count_and_replays_through_app() {
    let env = crate::test_env::TestEnv::new("dense_numeric_sort_history");
    let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    app.wb.path = Some(env.path("numbers.xlsx"));
    let sheet = &mut app.wb.sheets[0];
    for row in 0..1000 {
        for col in 0..101 {
            sheet.set(row, col, sheetcore::Cell {
                value: sheetcore::Value::Number((1000 - row + col) as f64), formula: None,
            });
        }
    }
    app.sort_region((0, 0, 999, 100), 0, true);
    // A concurrent same-sheet edit and a sheet-tab switch must survive the prepared swap.
    app.wb.sheet_mut(0).set(1001,0,sheetcore::Cell { value: sheetcore::Value::Number(81.), formula: None });
    app.wb.sheets.push(Sheet::new("Other"));
    assert!(app.switch_sheet_view(1));
    let start = std::time::Instant::now();
    while app.tasks.reorder.is_some() {
        app.poll_reorder();
        assert!(start.elapsed().as_secs() < 30);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(app.sheet_idx, 1);
    assert_eq!(app.wb.sheet(0).value(1001,0),sheetcore::Value::Number(81.));
    assert_eq!(app.wb.sheet(1).value(0,0),sheetcore::Value::Empty);
    let log = app.rev.last().unwrap();
    assert_eq!(log.cells.len(), sheetcore::revision::MAX_CELLS);
    assert!(log.note.contains("101000"));
    assert_eq!(app.wb.sheets[0].value(0, 0), sheetcore::Value::Number(1.0));
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheets[0].value(0, 0), sheetcore::Value::Number(1000.0));
    assert!(app.apply_undo(true));
    assert_eq!(app.wb.sheets[0].value(0, 0), sheetcore::Value::Number(1.0));
    // A later writer restored the original value; redo must still exclude this cell.
    app.wb.sheet_mut(0).set(0,0,sheetcore::Cell { value: sheetcore::Value::Number(1000.), formula: None });
    assert!(!app.apply_undo(false));
    assert!(app.status.contains("1 格"));
    assert!(app.apply_undo(true));
    assert_eq!(app.wb.sheet(0).value(0,0),sheetcore::Value::Number(1000.));
}

#[test]
fn pending_sort_does_not_cross_room_boundaries() {
    let _env = crate::test_env::TestEnv::new("sort_room_change");
    let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    let number = |n| sheetcore::Cell { value: sheetcore::Value::Number(n), formula: None };
    app.wb.sheets[0].set(0,0,number(2.));
    app.wb.sheets[0].set(1,0,number(1.));
    for joining in [true,false] {
        app.sort_region((0,0,20_000,5),0,true);
        assert!(app.tasks.reorder.is_some());
        if joining {
            app.net = Some(crate::net::host_for_test(91,"test","sort-room","test").0);
        } else {
            app.leave_session_at(None);
        }
        let start = std::time::Instant::now();
        while app.tasks.reorder.is_some() {
            app.poll_reorder();
            assert!(start.elapsed().as_secs() < 30);
            std::thread::yield_now();
        }
        assert_eq!(app.wb.sheets[0].get(0,0).as_deref(),Some(&number(2.)));
        assert_eq!(app.undo.depth(),(0,0));
    }
}

type Rect = (u32, u32, u32, u32);

pub(super) enum Operation {
    Rows {
        col: u32,
        asc: bool,
    },
    Columns {
        row: u32,
        asc: bool,
    },
    Multi {
        keys: Vec<data_tools::SortKey>,
        header: bool,
        horizontal: bool,
        styles: Option<std::sync::Arc<sheetcore::style::StyleTable>>,
    },
    Dedup {
        cols: Vec<u32>,
        header: bool,
        total_data_rows: usize,
    },
}

impl Operation {
    fn label(&self) -> &'static str {
        match self {
            Self::Rows { .. } => "排序",
            Self::Columns { .. } => "按行排序",
            Self::Multi { .. } => "多级排序",
            Self::Dedup { .. } => "删除重复",
        }
    }

    fn plan(
        &self,
        sheet: &Sheet,
        index: usize,
        rect: Rect,
    ) -> Result<data_tools::RowChanges, String> {
        let (r1, c1, r2, c2) = rect;
        match self {
            Self::Rows { col, asc } => {
                let header = sort_filter::has_header(sheet, r1, c1, r2, c2);
                sort_filter::plan_sort_axis(
                    sheet,
                    index,
                    (r1 + u32::from(header), c1, r2, c2),
                    *col,
                    *asc,
                    false,
                )
            }
            Self::Columns { row, asc } => {
                sort_filter::validate_sort_region(sheet, rect)?;
                let header = sort_filter::has_header_col(sheet, r1, c1, r2, c2);
                sort_filter::plan_sort_axis(
                    sheet,
                    index,
                    (r1, c1 + u32::from(header), r2, c2),
                    *row,
                    *asc,
                    true,
                )
            }
            Self::Multi { keys, header, horizontal, styles } => {
                sort_filter::validate_sort_region(sheet, rect)?;
                data_tools::plan_sort_multi_with_styles(
                    sheet,
                    styles.as_deref(),
                    index,
                    rect,
                    keys,
                    *header,
                    *horizontal,
                )
            }
            Self::Dedup { cols, header, .. } => {
                sort_filter::validate_sort_region(sheet, rect)?;
                Ok(data_tools::plan_remove_duplicates(
                    sheet, index, rect, cols, *header,
                ))
            }
        }
    }

    fn completed(&self, count: usize) -> String {
        match self {
            Self::Rows { col, asc } => format!(
                "已按 {} 列{}排序",
                sheetcore::model::cell_name(0, *col),
                if *asc { "升序" } else { "降序" }
            ),
            Self::Columns { row, .. } => format!("已按第 {} 行排序（重排列）", row + 1),
            Self::Multi { keys, horizontal, .. } => {
                if *horizontal {
                    format!("已按 {} 个行键水平排序（{} 列移动）", keys.len(), count)
                } else {
                    format!("已按 {} 个键排序（{} 行移动）", keys.len(), count)
                }
            }
            Self::Dedup { .. } if count == 0 => "没有重复行".into(),
            Self::Dedup { total_data_rows, .. } => {
                let kept = total_data_rows.saturating_sub(count);
                format!("已删除了 {count} 行重复，保留了 {kept} 个唯一值")
            }
        }
    }
}

pub(crate) const ASYNC_REORDER_CELLS: u64 = 20_000;

type PrebuiltSortItem = (
    u32,
    u32,
    sheetcore::sync::ValueWire,
    Option<std::sync::Arc<str>>,
    Option<sheetcore::style::CellStyle>,
);

pub(crate) struct Pending {
    sheet: usize,
    target: super::sheet_edit_target::SheetEditTarget,
    rect: Rect,
    operation: std::sync::Arc<Operation>,
    rx: Receiver<Result<(Sheet, Sheet, PreparedChanges), String>>,
}

enum PreparedChanges {
    Cells(data_tools::RowChanges),
    CowSort {
        order_perm: data_tools::LinePermutation,
        moved_lines: usize,
        total: usize,
        sample: Vec<sheetcore::undo::CellDelta>,
        batch: Option<sheetcore::undo::PreparedCowBatch>,
    },
    Permutation {
        order: data_tools::CompactSort,
        note_edit: sheetcore::undo::NoteUndoEdit,
        total: usize,
        sample: Vec<sheetcore::undo::CellDelta>,
        collab: Option<Vec<PrebuiltSortItem>>,
    },
}

impl SheetApp {
    /// The undo guard checks the cell operation's site even for a format-only edit.
    /// Send every touched cell, including empty clears, and only the formats that changed.
    pub(super) fn broadcast_sheet_deltas(
        &mut self,
        sheet: usize,
        deltas: &[sheetcore::undo::CellDelta],
    ) {
        if self.net.is_none() {
            return;
        }
        for chunk in deltas.chunks(sheetcore::sync::LIVE_OP_BATCH) {
            let keys: Vec<_> = chunk.iter().map(|d| (sheet, d.row, d.col)).collect();
            self.broadcast_cells(&keys);
            let styles: Vec<_> = chunk
                .iter()
                .filter(|d| d.before_style != d.after_style)
                .map(|d| (d.row, d.col, self.wb.styles.get(d.after_style).clone()))
                .collect();
            if let Some(net) = &self.net {
                net.broadcast_style(sheet, styles);
            }
        }
    }

    pub(super) fn confirm_own_styles<'a>(&mut self, items: impl Iterator<Item=(usize,u32,u32,&'a sheetcore::style::CellStyle)>) {
        let mut changed=false;
        for (index,row,col,style) in items {
            let Some(sheet) = self.wb.sheets.get_mut(index) else { continue };
            if self.wb.styles.get(sheet.style_of(row,col)) != style {
                let id = self.wb.styles.intern(style.clone());
                sheet.set_style(row,col,id);
                changed=true;
            }
        }
        if changed { self.doc_gen = self.doc_gen.wrapping_add(1); self.wb.dirty=true; }
    }

    pub(super) fn repair_rejected_sort(&mut self, sheet: usize, first_ts: u64, items: Vec<sheetcore::sync::SortRepair>) {
        self.undo.reject_sort(first_ts);
        let Some(peer) = &mut self.peer else { return };
        if self.wb.sheets.get(sheet).is_none() { return; }
        let mut touched = Vec::new();
        let mut styles_changed = false;
        for item in items {
            let key = (sheet,item.row,item.col);
            if peer.cells.get(&key).is_some_and(|op| op.site == peer.site && op.ts == item.rejected_ts) {
                peer.force_cell(sheet,item.row,item.col,item.cell);
                touched.push(key);
            }
            // Style changes travel independently of the cell LWW operation.
            if let Some((expected,previous)) = item.style {
                let current = self.wb.sheet(sheet).style_of(item.row,item.col);
                if self.wb.styles.get(current) == &expected {
                    let sid = previous.map_or(0,|s| self.wb.styles.intern(s));
                    self.wb.sheet_mut(sheet).set_style(item.row,item.col,sid);
                    styles_changed = true;
                }
            }
        }
        peer.apply_list_to_workbook(&mut self.wb,&touched);
        if !touched.is_empty() { self.fast_recalc_cells(&touched); }
        if !touched.is_empty() || styles_changed { self.doc_gen = self.doc_gen.wrapping_add(1); }
        self.status = "排序区域有协作者正在编辑，本次排序已撤回".into();
    }

    pub(super) fn apply_remote_sort(&mut self, batch: &crate::net_sort::SortBatch) {
        let sheet = batch.plan.sheet;
        let site = batch.plan.site;
        let cells_only = self.only_shared_cell_edits() && batch.styles.is_empty()
            && self.net.as_ref().is_some_and(|n| !n.hosting);
        let Some(peer) = self.peer.as_mut() else { return };
        let Some(sh) = self.wb.sheets.get_mut(sheet) else { return };
        let mut changes = 0;
        let mut sample = Vec::new();
        let mut seeds = Vec::new();
        for op in &batch.ops {
            if !peer.apply_shared(op.clone()) { continue; }
            let cell = sheetcore::Cell { value: (&op.value).into(), formula: op.formula.as_deref().map(Into::into) };
            if sample.len() < sheetcore::revision::MAX_CELLS {
                let old = sh.get(op.row,op.col).map(|c| c.into_owned()).unwrap_or_default();
                sample.push(sheetcore::revision::Change { r: op.row, c: op.col,
                    before: sheetcore::revision::Snap::from_cell(&old), after: sheetcore::revision::Snap::from_cell(&cell),
                    before_style: None, after_style: None });
            }
            sh.set(op.row,op.col,cell);
            if changes < 2_000 { seeds.push((sheet,op.row,op.col)); }
            changes += 1;
        }
        let mut style_intern_cache: std::collections::HashMap<
            u64,
            Vec<(sheetcore::style::CellStyle, u16)>,
        > = std::collections::HashMap::new();
        for (r, c, style) in &batch.styles {
            let h = sheetcore::sync::cell_style_hash(style);
            let id = if let Some(found) = style_intern_cache
                .get(&h)
                .and_then(|b| b.iter().find(|(s, _)| s == style))
                .map(|(_, id)| *id)
            {
                found
            } else {
                let id = self.wb.styles.intern(style.clone());
                style_intern_cache
                    .entry(h)
                    .or_default()
                    .push((style.clone(), id));
                id
            };
            sh.set_style(*r, *c, id);
        }
        if changes == 0 && batch.styles.is_empty() { return; }
        let name = sh.name.clone();
        if changes > 2_000 { self.recalc_all_and_deps_adaptive(); } else { self.fast_recalc_cells(&seeds); }
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        if cells_only { self.shared_cell_generation = Some((self.doc_uid,self.doc_gen)); }
        if let Some(path) = self.wb.path.clone() {
            let mut entry = sheetcore::revision::Entry::new(&self.peer_display_name(site),site,"远端排序",&name);
            entry.remote = true;
            entry.cells = sample;
            entry.note = format!("变更 {changes} 格，格式 {} 格",batch.styles.len());
            self.add_revision(&path,entry);
        }
    }

    pub(super) fn broadcast_line_permutation(
        &mut self, sheet: usize, (region, horizontal, order): data_tools::LinePermutation,
        cells: impl Iterator<Item=(u32,u32,bool)>,
    ) -> Option<u64> {
        let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref()) else { return None };
        let sh = self.wb.sheet(sheet);
        let blank = sheetcore::Cell::default();
        let mut ops = Vec::new();
        let mut styles = Vec::new();
        let mut style_cache: std::collections::HashMap<u16, sheetcore::style::CellStyle> =
            std::collections::HashMap::new();
        for (r,c,style) in cells {
            let cell = sh.get(r, c);
            ops.push(peer.local_set_shared(sheet, r, c, cell.as_deref().unwrap_or(&blank)));
            if style {
                let sid = sh.style_of(r, c);
                let st = style_cache
                    .entry(sid)
                    .or_insert_with(|| self.wb.styles.get(sid).clone())
                    .clone();
                styles.push((r, c, st));
            }
        }
        let Some(first) = ops.first() else { return None };
        let plan = sheetcore::sync::SortPermutation { structural_revision: net.structure_seen(), sheet, name: sh.name.clone(), region, horizontal,
            order, first_ts: first.ts, site: peer.site, digest: [0;32] };
        if plan.valid() {
            let first_ts = plan.first_ts;
            net.broadcast_sort(crate::net_sort::SortBatch::new(plan,ops,styles));
            Some(first_ts)
        } else {
            for chunk in ops.chunks(sheetcore::sync::LIVE_OP_BATCH) { net.broadcast_ops(chunk.iter().map(|o| (**o).clone()).collect()); }
            for chunk in styles.chunks(sheetcore::sync::STYLE_PALETTE_CHUNK_ITEMS) { net.broadcast_style(sheet,chunk.to_vec()); }
            None
        }
    }

    fn broadcast_prebuilt_permutation(
        &mut self,
        sheet: usize,
        (region, horizontal, order): data_tools::LinePermutation,
        items: Vec<PrebuiltSortItem>,
    ) -> Option<u64> {
        let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref()) else {
            return None;
        };
        if items.is_empty() {
            return None;
        }
        let sh_name = self.wb.sheet(sheet).name.clone();
        let site = peer.site;
        let mut ops = Vec::with_capacity(items.len());
        let mut styles = Vec::new();
        for (row, col, value, formula, style) in items {
            peer.clock = peer.clock.saturating_add(1);
            let op = std::sync::Arc::new(sheetcore::sync::Op {
                sheet,
                row,
                col,
                value,
                formula,
                ts: peer.clock,
                site,
            });
            peer.cells.insert((sheet, row, col), op.clone());
            ops.push(op);
            if let Some(st) = style {
                styles.push((row, col, st));
            }
        }
        let first_ts = ops[0].ts;
        let plan = sheetcore::sync::SortPermutation {
            structural_revision: net.structure_seen(),
            sheet,
            name: sh_name,
            region,
            horizontal,
            order,
            first_ts,
            site,
            digest: [0; 32],
        };
        if plan.valid() {
            net.broadcast_sort(crate::net_sort::SortBatch::new(plan, ops, styles));
            Some(first_ts)
        } else {
            for chunk in ops.chunks(sheetcore::sync::LIVE_OP_BATCH) {
                net.broadcast_ops(chunk.iter().map(|o| (**o).clone()).collect());
            }
            for chunk in styles.chunks(sheetcore::sync::STYLE_PALETTE_CHUNK_ITEMS) {
                net.broadcast_style(sheet, chunk.to_vec());
            }
            None
        }
    }

    #[inline]
    fn rect_has_locked_cell(&self, sheet: usize, rect: Rect) -> bool {
        crate::column_clip::rect_has_any_locked(&self.wb, self.wb.sheet(sheet), rect)
    }

    fn preflight_reorder(&mut self, sheet: usize, rect: Rect, operation: &Operation) -> bool {
        let (r1, c1, r2, c2) = rect;
        let label = operation.label();
        let op = matches!(operation, Operation::Rows { .. })
            .then_some(sheetcore::protection::ProtOp::Sort);
        if !self.write_preflight_on_op(sheet, &[], label, op) {
            return false;
        }
        if op.is_none()
            && self.wb.sheet(sheet).protection.is_some()
            && self.rect_has_locked_cell(sheet, rect)
        {
            self.status = format!("工作表受保护：目标区域含锁定单元格，不能{label}");
            return false;
        }
        let conflict = self
            .net
            .as_ref()
            .and_then(|n| n.host_lock_conflict_rect(sheet, rect))
            .map(|(_, name, r, c)| (name, r, c))
            .or_else(|| {
                let site = self.net.as_ref().map_or(0, |n| n.site);
                self.locks
                    .conflicts_rect(sheet, r1, c1, r2, c2, site, self.lock_now())
                    .map(|((r, c), h)| (h.name.clone(), r, c))
            });
        if let Some((name, r, c)) = conflict {
            self.status = format!(
                "{name} 正在编辑 {}，不能{label}",
                sheetcore::model::cell_name(r, c)
            );
            return false;
        }
        true
    }

    pub(super) fn run_reorder(&mut self, rect: Rect, operation: Operation) {
        if self.tasks.reorder.is_some() {
            self.status = "正在计算上一项数据操作，请稍候".into();
            return;
        }
        if !self.preflight_reorder(self.sheet_idx, rect, &operation) {
            return;
        }
        let sheet = self.wb.sheet(self.sheet_idx);
        let area = u64::from(rect.2 - rect.0 + 1) * u64::from(rect.3 - rect.1 + 1);
        if area <= ASYNC_REORDER_CELLS {
            let changes = operation.plan(sheet, self.sheet_idx, rect);
            self.finish_reorder(&operation, changes, None, (self.sheet_idx,self.sel.0,self.sel.1));
            return;
        }
        let snapshot = sheet.reorder_snapshot();
        let collab_palette = self
            .net
            .is_some()
            .then(|| std::sync::Arc::new(self.wb.styles.clone()));
        let index = self.sheet_idx;
        let focus = (index, self.sel.0, self.sel.1);
        self.status = format!("正在计算{}…", operation.label());
        let (tx, rx) = mpsc::channel();
        let op = std::sync::Arc::new(operation);
        let worker_op = op.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let mut prepared = snapshot.clone();
                let sort = match worker_op.as_ref() {
                    Operation::Rows { col, asc } => {
                        let (r1,c1,r2,c2) = rect;
                        let header = sort_filter::has_header(&snapshot,r1,c1,r2,c2);
                        data_tools::CompactSort::plan(&snapshot,index,(r1 + u32::from(header),c1,r2,c2),*col,*asc,false)?
                    }
                    Operation::Columns { row, asc } => {
                        sort_filter::validate_sort_region(&snapshot,rect)?;
                        let (r1,c1,r2,c2) = rect;
                        let header = sort_filter::has_header_col(&snapshot,r1,c1,r2,c2);
                        data_tools::CompactSort::plan(&snapshot,index,(r1,c1 + u32::from(header),r2,c2),*row,*asc,true)?
                    }
                    Operation::Multi { keys, header, horizontal, styles } => {
                        sort_filter::validate_sort_region(&snapshot, rect)?;
                        let needs_rich = *horizontal
                            || styles.is_some()
                            || keys.iter().any(|k| {
                                !k.ignore_case
                                    || matches!(
                                        k.mode,
                                        data_tools::SortMode::FillColor(_)
                                            | data_tools::SortMode::FontColor(_)
                                    )
                            });
                        if needs_rich {
                            let changes = data_tools::plan_sort_multi_with_styles(
                                &snapshot,
                                styles.as_deref(),
                                index,
                                rect,
                                keys,
                                *header,
                                *horizontal,
                            )?;
                            changes.apply(&mut prepared);
                            return Ok((snapshot, prepared, PreparedChanges::Cells(changes)));
                        }
                        match data_tools::plan_large_sort_multi(&snapshot,index,rect,keys,*header)? {
                            data_tools::MultiSortPlan::Compact(order) => Some(order),
                            data_tools::MultiSortPlan::Cells(changes) => {
                                changes.apply(&mut prepared);
                                return Ok((snapshot,prepared,PreparedChanges::Cells(changes)));
                            }
                        }
                    }
                    Operation::Dedup { .. } => None,
                };
                if let Some(order) = sort {
                    if collab_palette.is_none() {
                        let (total, sample) = order.apply_to_sheet_with_sample(
                            &mut prepared,
                            sheetcore::revision::MAX_CELLS,
                        );
                        let moved_lines = order.moved_lines();
                        let order_perm = order.permutation();
                        let notes_changed =
                            snapshot.notes != prepared.notes || snapshot.threads != prepared.threads;
                        let batch = (total > 0 || notes_changed).then(|| {
                            sheetcore::undo::PreparedCowBatch::from_sheets(
                                worker_op.label(),
                                index,
                                &snapshot,
                                &prepared,
                                focus,
                                0,
                            )
                        }).flatten();
                        drop(order);
                        return Ok((
                            snapshot,
                            prepared,
                            PreparedChanges::CowSort {
                                order_perm,
                                moved_lines,
                                total,
                                sample,
                                batch,
                            },
                        ));
                    }
                    let note_edit = order.plan_notes(&snapshot);
                    let (total, sample) = order.apply_to_sheet_with_sample(
                        &mut prepared,
                        sheetcore::revision::MAX_CELLS,
                    );
                    let mut collab = collab_palette.as_ref().map(|_| Vec::with_capacity(total));
                    let mut style_cache: std::collections::HashMap<u16, sheetcore::style::CellStyle> =
                        std::collections::HashMap::new();
                    if let (Some(out), Some(pal)) = (collab.as_mut(), collab_palette.as_ref()) {
                        for d in order.deltas() {
                            let effective_st = if d.before_style != d.after_style {
                                let sid = prepared.style_of(d.row, d.col);
                                Some(
                                    style_cache
                                        .entry(sid)
                                        .or_insert_with(|| pal.get(sid).clone())
                                        .clone(),
                                )
                            } else {
                                None
                            };
                            let val_wire = (&d.after.value).into();
                            out.push((d.row, d.col, val_wire, d.after.formula, effective_st));
                        }
                    }
                    return Ok((snapshot, prepared, PreparedChanges::Permutation { order, note_edit, total, sample, collab }));
                }
                let changes = worker_op.plan(&snapshot, index, rect)?;
                changes.apply(&mut prepared);
                Ok((snapshot, prepared, PreparedChanges::Cells(changes)))
            })();
            let _ = tx.send(result);
        });
        self.tasks.reorder = Some(Pending {
            sheet: index,
            target: super::sheet_edit_target::SheetEditTarget::current(self,self.sel),
            rect,
            operation: op,
            rx,
        });
    }

    pub(super) fn poll_reorder(&mut self) {
        let Some(pending) = &self.tasks.reorder else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            result => result,
        };
        let pending = self.tasks.reorder.take().unwrap();
        let (source, mut prepared, changes) = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                self.status = error;
                return;
            }
            Err(_) => {
                self.status = "数据操作未完成，请重试".into();
                return;
            }
        };
        if !pending.target.valid(self) {
            self.status = "目标表或协作状态已改变，请重新执行数据操作".into();
            return;
        }
        let current = self.wb.sheet(pending.sheet);
        let whole_snapshot_matches = current.matches_reorder_source(&source);
        if !whole_snapshot_matches && !current.same_sort_region(&source, pending.rect) {
            self.status = "排序区域已变更，请重新执行数据操作".into();
            return;
        }
        let focus = (pending.sheet, pending.target.at.0, pending.target.at.1);
        if self.preflight_reorder(pending.sheet, pending.rect, &pending.operation) {
            if !whole_snapshot_matches {
                let (r1,c1,r2,c2) = pending.rect;
                self.wb.sheet(pending.sheet).rebase_row_changes(&source, &mut prepared,
                    |k| (r1..=r2).contains(&k.row) && (c1..=c2).contains(&k.col));
            }
            match changes {
                PreparedChanges::Cells(changes) => {
                    self.finish_reorder(&pending.operation, Ok(changes), Some(&mut prepared), focus);
                }
                PreparedChanges::CowSort {
                    order_perm,
                    moved_lines,
                    total,
                    sample,
                    batch,
                } => {
                    let operation = &pending.operation;
                    self.status = operation.completed(moved_lines);
                    if total == 0 && batch.is_none() {
                        std::thread::spawn(move || drop((source, prepared)));
                        return;
                    }
                    self.wb.sheet_mut(pending.sheet).take_row_changes(&mut prepared);
                    self.wb.dirty = true;
                    self.doc_gen = self.doc_gen.wrapping_add(1);
                    self.recalc_all_and_deps_adaptive();
                    self.recalc_filter_hidden_on(pending.sheet);
                    self.record_revision_sample(operation.label(), &sample, total);
                    if let Some(batch) = batch {
                        self.undo.push_prepared_cow_batch(batch);
                        self.undo.tag_sorted_cells(order_perm);
                    }
                    self.tasks.last_edit = Some(std::time::Instant::now());
                    std::thread::spawn(move || drop((source, prepared)));
                }
                PreparedChanges::Permutation { order, note_edit, total, sample, collab } => {
                    let operation = &pending.operation;
                    self.status = operation.completed(order.moved_lines());
                    if total == 0 && note_edit.is_empty() { return; }
                    self.wb.sheet_mut(pending.sheet).take_row_changes(&mut prepared);
                    self.wb.dirty = true;
                    self.doc_gen = self.doc_gen.wrapping_add(1);
                    self.recalc_all_and_deps_adaptive();
                    self.recalc_filter_hidden_on(pending.sheet);
                    let ts = if self.net.is_some() {
                        if whole_snapshot_matches {
                            if let Some(prebuilt) = collab {
                                self.broadcast_prebuilt_permutation(
                                    pending.sheet,
                                    order.permutation(),
                                    prebuilt,
                                )
                            } else {
                                self.broadcast_line_permutation(
                                    pending.sheet,
                                    order.permutation(),
                                    order.deltas().map(|d| (d.row, d.col, d.before_style != d.after_style)),
                                )
                            }
                        } else {
                            self.broadcast_line_permutation(
                                pending.sheet,
                                order.permutation(),
                                order.deltas().map(|d| (d.row, d.col, d.before_style != d.after_style)),
                            )
                        }
                    } else {
                        None
                    };
                    if !note_edit.is_empty() {
                        self.broadcast_note_undo_edit(&note_edit);
                    }
                    self.record_revision_sample(operation.label(), &sample, total);
                    self.undo.push_compact_sort_with_notes(
                        operation.label(),
                        order,
                        (!note_edit.is_empty()).then_some(note_edit),
                        focus,
                    );
                    if let Some(ts) = ts { self.undo.track_sort(ts,None); }
                    self.tasks.last_edit = Some(std::time::Instant::now());
                }
            }
        }
    }

    fn finish_reorder(
        &mut self,
        operation: &Operation,
        changes: Result<data_tools::RowChanges, String>,
        prepared: Option<&mut Sheet>,
        focus: (usize,u32,u32),
    ) {
        let mut changes = match changes {
            Err(error) => {
                self.status = error;
                return;
            }
            Ok(changes) => changes,
        };
        let message = operation.completed(changes.count);
        if !changes.deltas.is_empty() || !changes.notes.is_empty() {
            let sheet = self.wb.sheet_mut(focus.0);
            if let Some(prepared) = prepared {
                sheet.take_row_changes(prepared);
            } else {
                changes.apply(sheet);
            }
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.recalc_all_and_deps_adaptive();
            self.recalc_filter_hidden_on(focus.0);
            let ts = if self.net.is_some() && !changes.deltas.is_empty() && let Some(order) = changes.permutation.clone() {
                self.broadcast_line_permutation(focus.0, order,
                    changes.deltas.iter().map(|d| (d.row,d.col,d.before_style != d.after_style)))
            } else {
                if !changes.deltas.is_empty() {
                    self.broadcast_sheet_deltas(focus.0, &changes.deltas);
                }
                None
            };
            let note_edit = std::mem::take(&mut changes.notes);
            if !note_edit.is_empty() {
                self.broadcast_note_undo_edit(&note_edit);
            }
            let had_deltas = !changes.deltas.is_empty();
            if had_deltas {
                self.record_deltas_at(operation.label(), changes.deltas, None, focus);
                if !note_edit.is_empty() {
                    self.undo.attach_notes_to_last(note_edit);
                }
            } else if !note_edit.is_empty() {
                self.undo.push_with_notes(
                    sheetcore::undo::Action {
                        label: operation.label().to_string(),
                        merges: None,
                        deltas: Vec::new(),
                        focus,
                        snap_before: None,
                        snap_after: None,
                        by_site: 0,
                        pictures: None,
                        shapes: None,
                        refresh_sizes: None,
                    },
                    Some(note_edit),
                );
            }
            if let Some(order) = changes.permutation.take() { self.undo.tag_sorted_cells(order); }
            if let Some(ts) = ts { self.undo.track_sort(ts,None); }
            self.tasks.last_edit = Some(std::time::Instant::now());
        }
        self.status = message;
    }
}
