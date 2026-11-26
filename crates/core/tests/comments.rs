//! 批注：读、改、存
use std::path::Path;

fn load() -> Option<sheetcore::model::Workbook> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02/notes.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    Some(sheetcore::xlsx::load(&p).expect("读入"))
}

#[test]
fn reads_comments() {
    let Some(wb) = load() else { return };
    let sh = wb.sheet(0);
    assert_eq!(sh.notes.len(), 2, "应读到两条批注: {}", sh.notes.len());
    let b2 = sh
        .notes
        .get(&sheetcore::model::CellRef::new(1, 1))
        .expect("B2 应有批注");
    assert_eq!(b2.text, "这行数据要复核");
    assert_eq!(b2.author, "张三");
    let a3 = sh
        .notes
        .get(&sheetcore::model::CellRef::new(2, 0))
        .expect("A3 应有批注");
    assert_eq!(a3.text, "客户确认过\n金额无误");
    assert_eq!(a3.author, "李四");
}

#[test]
fn add_edit_delete_and_save() {
    let Some(mut wb) = load() else { return };
    let out = std::env::current_exe()
        .expect("test executable path")
        .parent()
        .expect("test executable directory")
        .join(format!("notes_edit_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);

    // 新增 C5、修改 B2、删除 A3
    wb.sheets[0].notes.insert(
        sheetcore::model::CellRef::new(4, 2),
        sheetcore::notes::Note { r: 4, c: 2, text: "新增的批注".into(), author: "Hermes".into() },
    );
    if let Some(n) = wb.sheets[0].notes.get_mut(&sheetcore::model::CellRef::new(1, 1)) {
        n.text = "改过的内容".into();
    }
    wb.sheets[0].notes.remove(&sheetcore::model::CellRef::new(2, 0));
    wb.sheets[0].notes_dirty = true;

    sheetcore::xlsx::save(&out, &wb).expect("保存");

    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    let sh = back.sheet(0);
    let get = |r: u32, c: u32| sh.notes.get(&sheetcore::model::CellRef::new(r, c)).map(|n| n.text.clone());
    assert_eq!(get(4, 2), Some("新增的批注".to_string()), "新增的批注应保存");
    assert_eq!(get(1, 1), Some("改过的内容".to_string()), "修改应保存");
    assert_eq!(get(2, 0), None, "删除的批注不应残留");
}

fn temp_xlsx(name: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("comments_p1_{name}_{}_{n}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn mk_thread(r: u32, c: u32, author: &str, text: &str, resolved: bool) -> sheetcore::notes::ThreadedComment {
    sheetcore::notes::ThreadedComment {
        r,
        c,
        resolved,
        entries: vec![
            sheetcore::notes::ThreadEntry {
                id: sheetcore::notes::new_ooxml_guid(),
                author: author.into(),
                timestamp: "2026-11-26T08:00:00Z".into(),
                text: text.into(),
            },
            sheetcore::notes::ThreadEntry {
                id: sheetcore::notes::new_ooxml_guid(),
                author: "复核员".into(),
                timestamp: "2026-11-26T08:05:00Z".into(),
                text: format!("{text}-已复核"),
            },
        ],
    }
}

#[test]
fn legacy_note_edit_preserves_existing_threaded_comments_on_save() {
    use sheetcore::model::{CellRef, Workbook};
    use sheetcore::notes::Note;

    let path = temp_xlsx("mixed_preserve");
    let mut wb = Workbook::new_blank();
    let sh = wb.sheet_mut(0);
    sh.notes.insert(
        CellRef::new(1, 1),
        Note {
            r: 1,
            c: 1,
            text: "原始传统批注".into(),
            author: "张三".into(),
        },
    );
    sh.notes_dirty = true;
    sh.threads.insert(
        CellRef::new(3, 3),
        mk_thread(3, 3, "李四", "线程讨论首条", true),
    );
    sh.threads_dirty = true;
    sheetcore::xlsx::save(&path, &wb).expect("首次保存混合批注");

    // 重新载入后仅修改传统批注 B2（threads_dirty 保持 false），再次保存必须完整保留 D4 线程批注
    let mut loaded = sheetcore::xlsx::load(&path).expect("重新加载");
    assert!(loaded.sheet(0).threads.contains_key(&CellRef::new(3, 3)));
    assert!(!loaded.sheet(0).threads_dirty);
    loaded.sheet_mut(0).notes.get_mut(&CellRef::new(1, 1)).unwrap().text = "仅修改传统批注".into();
    loaded.sheet_mut(0).notes_dirty = true;
    sheetcore::xlsx::save(&path, &loaded).expect("二次保存");

    let back = sheetcore::xlsx::load(&path).expect("二次加载");
    let sh_back = back.sheet(0);
    let b2 = sh_back.notes.get(&CellRef::new(1, 1)).expect("B2 传统批注应保留");
    assert_eq!(b2.text, "仅修改传统批注");
    let d4 = sh_back
        .threads
        .get(&CellRef::new(3, 3))
        .expect("仅改传统批注保存时绝不能丢失同表已有的 Excel 365 线程批注");
    assert!(d4.resolved, "线程解决状态必须保留");
    assert_eq!(d4.entries.len(), 2, "线程全部回复必须保留");
    assert_eq!(d4.entries[0].text, "线程讨论首条");
    assert_eq!(d4.entries[1].text, "线程讨论首条-已复核");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn xls_compatibility_warning_detects_threaded_comments() {
    use sheetcore::model::{CellRef, Workbook};

    let mut wb = Workbook::new_blank();
    wb.sheet_mut(0)
        .threads
        .insert(CellRef::new(0, 0), mk_thread(0, 0, "王五", "仅线程批注", false));
    let warns = sheetcore::xls_write::compatibility_warnings(&wb);
    assert!(
        warns.iter().any(|w| w.contains("批注")),
        "仅有线程批注时 .xls 兼容检查也必须发出批注提醒，实际: {warns:?}"
    );
}

#[test]
fn sort_and_dedup_move_notes_and_threads_with_undo() {
    use sheetcore::data_tools::{plan_remove_duplicates, plan_sort_multi, CompactSort, SortKey};
    use sheetcore::model::{Cell, CellRef, Value, Workbook};
    use sheetcore::notes::Note;
    use sheetcore::sync::SheetMetaKind;
    use sheetcore::undo::{Action, UndoStack};

    // 1) 纵向排序 + Undo/Redo + 协作同步元数据产出
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for (r, v) in [(0u32, 30.0), (1, 10.0), (2, 20.0)] {
            sh.set(r, 0, Cell { value: Value::Number(v), formula: None });
            sh.set(r, 1, Cell { value: Value::Text(format!("row-{v}").into()), formula: None });
        }
        sh.notes.insert(
            CellRef::new(0, 1),
            Note { r: 0, c: 1, text: "注30".into(), author: "甲".into() },
        );
        sh.threads.insert(
            CellRef::new(1, 0),
            mk_thread(1, 0, "乙", "线程10", false),
        );
    }
    let changes = plan_sort_multi(
        wb.sheet(0),
        0,
        (0, 0, 2, 1),
        &[SortKey::text(0, true)],
        false,
    )
    .expect("纵向排序计划");
    assert!(!changes.notes.is_empty(), "排序变更必须包含批注/线程迁移计划");
    changes.apply(wb.sheet_mut(0));
    {
        let sh = wb.sheet(0);
        // 排序后: row0=10 (原row1), row1=20 (原row2), row2=30 (原row0)
        assert_eq!(sh.notes.get(&CellRef::new(0, 1)), None);
        let moved_note = sh.notes.get(&CellRef::new(2, 1)).expect("注30 应随行移至 C? B3 (2,1)");
        assert_eq!((moved_note.r, moved_note.c, moved_note.text.as_str()), (2, 1, "注30"));
        assert!(sh.threads.get(&CellRef::new(1, 0)).is_none());
        let moved_tc = sh.threads.get(&CellRef::new(0, 0)).expect("线程10 应随行移至 A1 (0,0)");
        assert_eq!((moved_tc.r, moved_tc.c), (0, 0));
        assert_eq!(moved_tc.entries[0].text, "线程10");
    }

    let mut undo = UndoStack::new();
    undo.push_with_notes(
        Action {
            label: "排序".into(),
            merges: None,
            deltas: changes.deltas,
            focus: (0, 0, 0),
            snap_before: None,
            snap_after: None,
            by_site: 0,
            pictures: None,
            shapes: None,
            refresh_sizes: None,
        },
        Some(changes.notes),
    );
    undo.undo(&mut wb.sheets, |_, _, _, _| true).expect("撤销排序");
    let sync_after_undo = undo.take_meta_sync();
    assert!(
        sync_after_undo.iter().any(|(_, k)| matches!(k, SheetMetaKind::Note { row: 0, col: 1, text: Some(t), .. } if t == "注30")),
        "撤销排序必须生成 Note 协作同步消息: {sync_after_undo:?}"
    );
    assert!(
        sync_after_undo.iter().any(|(_, k)| matches!(k, SheetMetaKind::Thread { row: 1, col: 0, thread: Some(_) })),
        "撤销排序必须生成 Thread 协作同步消息: {sync_after_undo:?}"
    );
    assert_eq!(wb.sheet(0).notes.get(&CellRef::new(0, 1)).map(|n| n.text.as_str()), Some("注30"));
    assert!(wb.sheet(0).notes.get(&CellRef::new(2, 1)).is_none());
    assert!(wb.sheet(0).threads.get(&CellRef::new(1, 0)).is_some());
    assert!(wb.sheet(0).threads.get(&CellRef::new(0, 0)).is_none());

    undo.redo(&mut wb.sheets, |_, _, _, _| true).expect("重做排序");
    assert_eq!(wb.sheet(0).notes.get(&CellRef::new(2, 1)).map(|n| n.text.as_str()), Some("注30"));
    assert!(wb.sheet(0).threads.get(&CellRef::new(0, 0)).is_some());

    // 2) 横向按行排序：批注与线程随列移动
    let mut wb_h = Workbook::new_blank();
    {
        let sh = wb_h.sheet_mut(0);
        for (c, v) in [(0u32, 30.0), (1, 10.0), (2, 20.0)] {
            sh.set(0, c, Cell { value: Value::Number(v), formula: None });
        }
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "列注30".into(), author: "甲".into() },
        );
        sh.threads.insert(
            CellRef::new(0, 1),
            mk_thread(0, 1, "乙", "列线程10", false),
        );
    }
    sheetcore::sort_filter::sort_range_by_row(wb_h.sheet_mut(0), 0, 0, 0, 2, 0, true).expect("横向排序");
    assert_eq!(wb_h.sheet(0).notes.get(&CellRef::new(0, 2)).map(|n| (n.r, n.c, n.text.as_str())), Some((0, 2, "列注30")));
    assert_eq!(wb_h.sheet(0).threads.get(&CellRef::new(0, 0)).map(|t| (t.r, t.c)), Some((0, 0)));

    // 3) 去重：保留行的批注上移，被删重复行的批注清除
    let mut wb_d = Workbook::new_blank();
    {
        let sh = wb_d.sheet_mut(0);
        for (r, v) in [(0u32, "A"), (1, "A"), (2, "B")] {
            sh.set(r, 0, Cell { value: Value::Text(v.into()), formula: None });
        }
        sh.notes.insert(
            CellRef::new(1, 0),
            Note { r: 1, c: 0, text: "重复行注".into(), author: "甲".into() },
        );
        sh.notes.insert(
            CellRef::new(2, 0),
            Note { r: 2, c: 0, text: "保留B注".into(), author: "乙".into() },
        );
    }
    let dedup = plan_remove_duplicates(wb_d.sheet(0), 0, (0, 0, 2, 0), &[0], false);
    dedup.apply(wb_d.sheet_mut(0));
    assert_eq!(wb_d.sheet(0).notes.get(&CellRef::new(1, 0)).map(|n| (n.r, n.c, n.text.as_str())), Some((1, 0, "保留B注")));
    assert!(wb_d.sheet(0).notes.get(&CellRef::new(2, 0)).is_none());

    // 4) 大表 CompactSort (>100,000 格 NumericSort) 移动批注与线程并可撤销
    let mut wb_big = Workbook::new_blank();
    {
        let sh = wb_big.sheet_mut(0);
        for r in 0..1005u32 {
            for c in 0..101u32 {
                let v = if c == 0 { (1004 - r) as f64 } else { r as f64 };
                sh.set(r, c, Cell { value: Value::Number(v), formula: None });
            }
        }
        sh.notes.insert(
            CellRef::new(0, 5),
            Note { r: 0, c: 5, text: "大表首行批注".into(), author: "大表".into() },
        );
        sh.threads.insert(
            CellRef::new(1004, 2),
            mk_thread(1004, 2, "大表", "大表末行线程", false),
        );
    }
    let compact = CompactSort::plan(wb_big.sheet(0), 0, (0, 0, 1004, 100), 0, true, false)
        .expect("大表排序计划")
        .expect("超过 10 万格应进入 CompactSort");
    let note_edit = compact.plan_notes(wb_big.sheet(0));
    assert!(!note_edit.is_empty());
    compact.apply_to_sheet(wb_big.sheet_mut(0));
    assert_eq!(
        wb_big.sheet(0).notes.get(&CellRef::new(1004, 5)).map(|n| (n.r, n.c, n.text.as_str())),
        Some((1004, 5, "大表首行批注"))
    );
    assert_eq!(
        wb_big.sheet(0).threads.get(&CellRef::new(0, 2)).map(|t| (t.r, t.c)),
        Some((0, 2))
    );
    let mut big_undo = UndoStack::new();
    big_undo.push_compact_sort_with_notes("大表排序", compact, Some(note_edit), (0, 0, 0));
    big_undo.undo(&mut wb_big.sheets, |_, _, _, _| true).expect("撤销大表排序");
    assert_eq!(
        wb_big.sheet(0).notes.get(&CellRef::new(0, 5)).map(|n| n.text.as_str()),
        Some("大表首行批注")
    );
    assert!(wb_big.sheet(0).notes.get(&CellRef::new(1004, 5)).is_none());
    assert!(wb_big.sheet(0).threads.get(&CellRef::new(1004, 2)).is_some());
    assert!(wb_big.sheet(0).threads.get(&CellRef::new(0, 2)).is_none());
}

#[test]
fn meta_edit_and_sheet_snapshot_undo_threads_and_emit_sync() {
    use sheetcore::model::{CellRef, Workbook};
    use sheetcore::notes::Note;
    use sheetcore::sync::SheetMetaKind;
    use sheetcore::undo::{snapshot_sheet, MetaEdit, UndoStack};

    let mut wb = Workbook::new_blank();
    let tc = mk_thread(2, 3, "审阅人", "请补充发票号", false);
    let note = Note { r: 2, c: 3, text: tc.summary_text(), author: "审阅人".into() };
    wb.sheet_mut(0).notes.insert(CellRef::new(2, 3), note.clone());
    wb.sheet_mut(0).threads.insert(CellRef::new(2, 3), tc.clone());

    let mut undo = UndoStack::new();
    undo.push_meta(
        "新建线程批注",
        (0, 2, 3),
        MetaEdit {
            sheet: 0,
            notes: vec![((2, 3), None, Some(note.clone()))],
            threads: vec![((2, 3), None, Some(tc.clone()))],
            ..Default::default()
        },
    );
    undo.undo(&mut wb.sheets, |_, _, _, _| true).expect("撤销线程批注");
    assert!(wb.sheet(0).notes.is_empty());
    assert!(wb.sheet(0).threads.is_empty());
    let sync_undo = undo.take_meta_sync();
    assert!(sync_undo.iter().any(|(_, k)| matches!(k, SheetMetaKind::Note { row: 2, col: 3, text: None, .. })));
    assert!(sync_undo.iter().any(|(_, k)| matches!(k, SheetMetaKind::Thread { row: 2, col: 3, thread: None })));

    undo.redo(&mut wb.sheets, |_, _, _, _| true).expect("重做线程批注");
    assert_eq!(wb.sheet(0).threads.get(&CellRef::new(2, 3)), Some(&tc));
    let sync_redo = undo.take_meta_sync();
    assert!(sync_redo.iter().any(|(_, k)| matches!(k, SheetMetaKind::Thread { row: 2, col: 3, thread: Some(t) } if t == &tc)));

    // SheetSnapshot 删除工作表后恢复，必须还原 notes 与 threads
    let snap = snapshot_sheet(wb.sheet(0), 0);
    let mut restored = sheetcore::model::Sheet::new("Sheet1");
    sheetcore::undo::restore_sheet(&mut restored, &snap);
    assert_eq!(restored.notes.get(&CellRef::new(2, 3)), Some(&note));
    assert_eq!(restored.threads.get(&CellRef::new(2, 3)), Some(&tc));
    assert!(restored.notes_dirty);
    assert!(restored.threads_dirty);
}

#[test]
fn search_comments_matches_notes_threads_and_authors_across_all_apis() {
    use sheetcore::model::{CellRef, Workbook};
    use sheetcore::notes::Note;
    use sheetcore::search::{self, Hit, SearchOpts, SearchOrder};
    use std::sync::atomic::AtomicBool;

    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.notes.insert(
            CellRef::new(1, 1),
            Note {
                r: 1,
                c: 1,
                text: "差额复核待确认".into(),
                author: "王财务".into(),
            },
        );
        sh.threads.insert(
            CellRef::new(3, 2),
            mk_thread(3, 2, "李总监", "同意跨期调整", true),
        );
    }
    let cancel = AtomicBool::new(false);
    let opts = SearchOpts {
        in_comments: true,
        use_wildcards: true,
        ..Default::default()
    };

    // 1) 搜索线程回复正文（含通配符）
    let h_scoped = search::scoped_hits(&wb, Some(0), &[], "*跨期*", &opts, SearchOrder::RowMajor, &cancel);
    assert_eq!(h_scoped, vec![Hit { sheet: 0, row: 3, col: 2 }]);
    assert_eq!(
        search::hits_in_sheet(&wb, 0, "跨期", &opts, SearchOrder::RowMajor),
        vec![Hit { sheet: 0, row: 3, col: 2 }]
    );
    assert_eq!(
        search::hits_in_rect(&wb, 0, (0, 0, 5, 5), "复核员", &opts, SearchOrder::RowMajor),
        vec![Hit { sheet: 0, row: 3, col: 2 }],
        "按线程回复作者搜索必须命中"
    );
    assert!(search::cell_matches(&wb, 0, 3, 2, "已复核", &opts));

    // 2) 按传统批注作者与正文搜索
    assert_eq!(
        search::hits(&wb, None, "王财务", &opts, SearchOrder::RowMajor),
        vec![Hit { sheet: 0, row: 1, col: 1 }]
    );
    assert!(search::cell_matches(&wb, 0, 1, 1, "差额", &opts));
    assert!(!search::cell_matches(&wb, 0, 0, 0, "差额", &opts));
}

#[test]
fn pdf_comment_printing_supports_threads_at_end_and_as_displayed() {
    use sheetcore::model::{Cell, CellRef, Value, Workbook};
    use sheetcore::notes::Note;
    use sheetcore::print::{SheetPageRenderer, comment_summary_pages, paginate};

    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.set(1, 1, Cell { value: Value::Number(1234.5), formula: None });
        sh.set(3, 3, Cell { value: Value::Text("跨期项".into()), formula: None });
        sh.notes.insert(
            CellRef::new(1, 1),
            Note {
                r: 1,
                c: 1,
                text: "传统批注打印测试".into(),
                author: "财务甲".into(),
            },
        );
        sh.threads.insert(
            CellRef::new(3, 3),
            mk_thread(3, 3, "审阅乙", "线程批注打印内容", true),
        );
    }

    // 1) AtEnd (print_comments == 1)：工作表末尾附页必须同时输出传统批注、线程全部回复与已解决标记
    wb.sheet_mut(0).page.print_comments = 1;
    let layout_end = paginate(wb.sheet(0), &wb.sheet(0).page);
    let summary_pages = comment_summary_pages(&wb, 0, &layout_end);
    assert!(!summary_pages.is_empty(), "AtEnd 模式必须生成批注汇总附页");
    let joined_end: String = summary_pages
        .iter()
        .flat_map(|p| p.texts.iter().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined_end.contains("B2") && joined_end.contains("传统批注打印测试"), "附页缺传统批注:\n{joined_end}");
    assert!(joined_end.contains("1234.5"), "附页应附带单元格当前值:\n{joined_end}");
    assert!(
        joined_end.contains("D4")
            && joined_end.contains("已解决")
            && joined_end.contains("线程批注打印内容")
            && joined_end.contains("线程批注打印内容-已复核"),
        "附页必须完整输出线程批注与全部回复:\n{joined_end}"
    );

    // 2) AsDisplayed (print_comments == 2)：正文页绘制红/紫角标及原位批注浮签
    wb.sheet_mut(0).page.print_comments = 2;
    let layout_disp = paginate(wb.sheet(0), &wb.sheet(0).page);
    let mut renderer = SheetPageRenderer::new(&wb, 0);
    let page0 = renderer.page_ops(&layout_disp, 0, None);
    assert!(
        page0.polys.iter().any(|(_, col)| *col == [217, 45, 32]),
        "AsDisplayed 应为传统批注绘制红色角标"
    );
    assert!(
        page0.polys.iter().any(|(_, col)| *col == [99, 102, 241]),
        "AsDisplayed 应为线程批注绘制紫蓝色角标"
    );
    let joined_disp: String = page0
        .texts
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined_disp.contains("传统批注打印测试") && joined_disp.contains("线程批注打印内容"),
        "AsDisplayed 模式应在页面绘制可见原位批注浮签内容:\n{joined_disp}"
    );
}
