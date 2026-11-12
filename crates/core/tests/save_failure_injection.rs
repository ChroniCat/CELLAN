//! C13.1 保存子步骤故障注入：子步骤失败时必须让整次保存失败。
//!
//! `save_atomic` 先写同目录临时文件再 rename；子步骤是在临时文件上打补丁，
//! 一旦某步失败却只 eprintln，临时文件就会带着缺口被 rename 成目标文件——
//! 用户看到“已保存”，实际丢了宏/样式/图表/保护/页面设置。
//!
//! 这里用 `SC_FAIL_SAVE_STEP=名称` 让指定子步骤返回错误，断言：
//!   1. save_atomic 返回 Err（可定位到步骤名）；
//!   2. 目标文件 SHA256 不变（没有半成品覆盖原文件）；
//!   3. 清掉注入后重试成功，文件恢复可读。

use sheetcore::model::{Cell, Value, Workbook};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 进程级环境变量，串行跑（cargo test -- --test-threads=1 或本锁）
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "sheetcore-c13-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn hash(path: &Path) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(std::fs::read(path).unwrap());
    h.finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets[0].set(
        0,
        0,
        Cell { value: Value::Text("金额".into()), formula: None },
    );
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(1.0), formula: None });
    wb
}

/// 注入后保存必须失败；失败时目标文件一个字节都不能变。
#[test]
fn injected_step_failures_do_not_report_success() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("inject");
    let path = dir.join("book.xlsx");

    let mut wb = book();
    sheetcore::xlsx::save_atomic(&path, &wb).expect("基线保存");
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(2.0), formula: None });
    let before = hash(&path);

    // 这些子步骤在“已有基准文件 + 普通工作簿”的保存里都会被执行
    let steps = [
        "merge-sheet-data",
        "layout",
        "protection",
        "page-setup",
        "fixups-migrate-sheet-rels",
        "styles-count",
        "shared-strings-rel",
        "content-types",
        "prune-content-types",
        "outline",
        "scenarios",
        "ignored-errors",
        "tab-colors",
        "hyperlinks",
        "sparkline",
        "textbox",
        "sheet-states",
        "rich",
        "fixups-drawing-rels",
        "fixups-dedupe-rels",
        "fixups-prune-orphans",
        "fixups-sheet-refs",
        "fixups-workbook-rels",
        "fixups-drawing-props",
        "fixups-content-types",
        "restore-unedited-sheets",
        "rehome-sheets",
        "workbook-skeleton",
        "base-sheet-rels",
        "fixups-content-types-2",
        "fixups-prune-orphans-2",
    ];
    let mut swallowed = Vec::new();
    for step in steps {
        // SAFETY: 整个测试持有 ENV_LOCK，没有其它线程读写这个变量
        unsafe { std::env::set_var("SC_FAIL_SAVE_STEP", step) };
        let result = sheetcore::xlsx::save_atomic(&path, &wb);
        unsafe { std::env::remove_var("SC_FAIL_SAVE_STEP") };
        match result {
            Err(e) => {
                let msg = format!("{e:#}");
                assert!(
                    msg.contains(step),
                    "错误里必须点名失败的子步骤 {step}，实际：{msg}"
                );
            }
            Ok(()) => swallowed.push(step),
        }
        assert_eq!(hash(&path), before, "注入 {step} 失败时原文件不得被改写");
    }

    // 不允许任何一步被吞掉
    assert!(
        swallowed.is_empty(),
        "这些保存子步骤失败后仍回报成功（原文件已被替换）：{swallowed:?}"
    );

    // 故障解除后重试成功，内容正确
    sheetcore::xlsx::save_atomic(&path, &wb).expect("故障解除后应保存成功");
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(1, 0), Value::Number(2.0));
    let _ = std::fs::remove_dir_all(dir);
}


/// 有条件触发的子步骤：只有工作簿里真的有对应部件时才会执行。
/// 这里用一份“什么东西都带一点”的合成工作簿把它们真正跑起来。
#[test]
fn injected_failures_in_conditional_steps_abort_the_save() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("conditional");
    let path = dir.join("rich.xlsx");

    let mut wb = book();
    wb.sheets[0].set(
        0,
        1,
        Cell { value: Value::Text("数量".into()), formula: None },
    );
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(5.0), formula: None });
    wb.sheets[0].tables.push(sheetcore::table::Table {
        name: "Sales".into(),
        display: "Sales".into(),
        range: (0, 0, 1, 1),
        columns: vec!["金额".into(), "数量".into()],
        header_row: true,
        totals_row: false,
    });
    wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
        ranges: vec![(1, 0, 1, 0)],
        kind: sheetcore::dv::DvKind::Whole,
        f1: "1".into(),
        f2: "9".into(),
        ..Default::default()
    });
    wb.sources.push(sheetcore::external::Source {
        name: "导入1".into(),
        kind: sheetcore::external::Kind::Internal {
            sheet: "数据".into(),
            rect: (0, 0, 1, 1),
        },
        at: (4, 0),
        rows: 2,
        cols: 2,
    });
    wb.links.push(sheetcore::xlinks::ExternalBook {
        index: 1,
        target: "外部.xlsx".into(),
        sheets: vec!["Sheet1".into()],
        ..Default::default()
    });

    wb.sheets[0].tables_dirty = true;
    wb.pivot_shifts.push(("数据".into(), 0, 1, 0, true));

    sheetcore::xlsx::save_atomic(&path, &wb).expect("基线保存");
    let before = hash(&path);
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(6.0), formula: None });

    let steps = [
        "table-attach",
        "cf-patch",
        "sources",
        "xlink-parts",
        "xlink-cache",
        "table-sync",
        "pivot-shifts",
    ];
    let mut swallowed = Vec::new();
    for step in steps {
        // SAFETY: 整个测试持有 ENV_LOCK
        unsafe { std::env::set_var("SC_FAIL_SAVE_STEP", step) };
        let result = sheetcore::xlsx::save_atomic(&path, &wb);
        unsafe { std::env::remove_var("SC_FAIL_SAVE_STEP") };
        match result {
            Err(e) => assert!(
                format!("{e:#}").contains(step),
                "错误里必须点名失败的子步骤 {step}"
            ),
            Ok(()) => swallowed.push(step),
        }
        assert_eq!(hash(&path), before, "注入 {step} 失败时原文件不得被改写");
    }
    assert!(swallowed.is_empty(), "这些子步骤失败后仍回报成功：{swallowed:?}");

    sheetcore::xlsx::save_atomic(&path, &wb).expect("故障解除后应保存成功");
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(1, 1), Value::Number(6.0));
    let _ = std::fs::remove_dir_all(dir);
}


/// 宏工作簿：VBA 部件写不回去时必须让保存失败，不能“成功”地丢掉宏。
#[test]
fn injected_vba_restore_failure_aborts_saving_a_macro_workbook() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("vba");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/real_macro.xlsm");
    let path = dir.join("macro.xlsm");
    std::fs::copy(&src, &path).expect("复制宏工作簿样本");
    let mut wb = sheetcore::xlsx::load(&path).expect("读取宏工作簿");
    wb.path = Some(path.clone());
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(7.0), formula: None });
    let before = hash(&path);

    // SAFETY: 整个测试持有 ENV_LOCK
    unsafe { std::env::set_var("SC_FAIL_SAVE_STEP", "vba") };
    let result = sheetcore::xlsx::save_atomic(&path, &wb);
    unsafe { std::env::remove_var("SC_FAIL_SAVE_STEP") };
    let err = format!("{:#}", result.expect_err("宏恢复失败时保存必须报错"));
    assert!(err.contains("vba"), "错误里必须点名 vba 步骤：{err}");
    assert_eq!(hash(&path), before, "失败时原宏工作簿不得被改写");

    sheetcore::xlsx::save_atomic(&path, &wb).expect("故障解除后应保存成功");
    assert!(
        sheetcore::vba::read_vba(&path).is_some(),
        "正常保存后宏部件必须还在"
    );
    let _ = std::fs::remove_dir_all(dir);
}


/// 图表：锚点/引用回写失败时必须中止保存（不能“成功”地把图弄丢或弄错位）。
#[test]
fn injected_chart_failures_abort_the_save() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("chart");
    let path = dir.join("chart.xlsx");

    let mut wb = book();
    wb.sheets[0].set(0, 1, Cell { value: Value::Text("数量".into()), formula: None });
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(2.0), formula: None });
    wb.sheets[0].set(2, 0, Cell { value: Value::Number(3.0), formula: None });
    wb.sheets[0].set(2, 1, Cell { value: Value::Number(4.0), formula: None });
    sheetcore::xlsx::save_atomic(&path, &wb).expect("基线保存");

    let chart = sheetcore::chart_write::chart_from_selection(
        &wb.sheets[0],
        "数据",
        (0, 0, 2, 1),
        "数量图",
        sheetcore::charts::ChartKind::Column,
    );
    sheetcore::chart_write::insert_chart(&path, "数据", &chart).expect("插入图表");

    let mut wb2 = sheetcore::xlsx::load(&path).expect("读回带图表的文件");
    wb2.path = Some(path.clone());
    assert_eq!(wb2.sheets[0].charts.len(), 1, "读回应有一张图");
    // 让锚点与引用两个回写步骤都真的执行
    wb2.sheets[0].charts_dirty = true;
    {
        let ch = &mut wb2.sheets[0].charts[0];
        assert!(!ch.drawing.is_empty(), "图表应有 drawing 部件");
        ch.ref_fixes.push(("A1".into(), "A1".into()));
    }
    wb2.sheets[0].set(2, 1, Cell { value: Value::Number(9.0), formula: None });

    let before = hash(&path);
    let mut swallowed = Vec::new();
    for step in ["chart-anchor", "chart-refs"] {
        // SAFETY: 整个测试持有 ENV_LOCK
        unsafe { std::env::set_var("SC_FAIL_SAVE_STEP", step) };
        let result = sheetcore::xlsx::save_atomic(&path, &wb2);
        unsafe { std::env::remove_var("SC_FAIL_SAVE_STEP") };
        match result {
            Err(e) => assert!(
                format!("{e:#}").contains(step),
                "错误里必须点名失败的子步骤 {step}"
            ),
            Ok(()) => swallowed.push(step),
        }
        assert_eq!(hash(&path), before, "注入 {step} 失败时原文件不得被改写");
    }
    assert!(swallowed.is_empty(), "图表子步骤失败后仍回报成功：{swallowed:?}");

    sheetcore::xlsx::save_atomic(&path, &wb2).expect("故障解除后应保存成功");
    assert_eq!(sheetcore::charts::load_charts(&path, "数据").unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}


/// 目标文件不可替换（只读属性）：必须报错、保留原文件与半成品临时文件，清掉后能重试。
#[test]
fn readonly_target_is_not_silently_replaced() {
    let dir = scratch("readonly");
    let path = dir.join("ro.xlsx");
    let mut wb = book();
    sheetcore::xlsx::save_atomic(&path, &wb).expect("基线保存");
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(8.0), formula: None });
    let before = hash(&path);

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms.clone()).unwrap();

    let err = format!(
        "{:#}",
        sheetcore::xlsx::save_atomic(&path, &wb).expect_err("只读目标必须报错")
    );
    assert!(err.contains("替换"), "错误要说明替换失败并给出临时文件位置：{err}");
    assert_eq!(hash(&path), before, "原文件不得被改写");
    let leftover = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().contains(".saving-"));
    assert!(
        leftover.is_some(),
        "替换失败时半成品必须留在临时文件里，不能删掉后假装没发生"
    );

    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let n = entry.file_name().to_string_lossy().to_string();
        if n.contains(".saving-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    sheetcore::xlsx::save_atomic(&path, &wb).expect("清掉只读后应能重试成功");
    assert_eq!(
        sheetcore::xlsx::load(&path).unwrap().sheets[0].value(1, 0),
        Value::Number(8.0)
    );
    let _ = std::fs::remove_dir_all(dir);
}
