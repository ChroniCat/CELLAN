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


fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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
                column_ids: Vec::new(),
        name: "Sales".into(),
        display: "Sales".into(),
        range: (0, 0, 1, 1),
        columns: vec!["金额".into(), "数量".into()],
        header_row: true,
        totals_row: false,
        ..Default::default()
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
        target_sheet: None,
        rows: 2,
        cols: 2,
    csv_opts: None,
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
    let _env = env_guard();
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
    assert!(err.contains("替换"), "错误要说明替换失败：{err}");
    assert_eq!(hash(&path), before, "原文件不得被改写");
    let leftover = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().contains(".saving-"));
    assert!(
        leftover.is_none(),
        "替换失败时不得在原文件目录遗留 .saving-* 大体积临时文件"
    );

    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
    sheetcore::xlsx::save_atomic(&path, &wb).expect("清掉只读后应能重试成功");
    assert_eq!(
        sheetcore::xlsx::load(&path).unwrap().sheets[0].value(1, 0),
        Value::Number(8.0)
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// C13 最终产物完整性门禁：写步骤全部成功、但临时文件已经损坏时，绝不能覆盖原文件。
// 注入的是「写完之后把产物弄坏」，与 SC_FAIL_SAVE_STEP（中途返回错误）互补。

#[test]
fn corrupted_temp_package_is_never_committed() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mode in ["truncate", "drop-part", "drop-sheet", "corrupt-xml"] {
        let dir = scratch(mode);
        let path = dir.join("gate.xlsx");
        let mut wb = book();
        sheetcore::xlsx::save_atomic(&path, &wb).expect("先写一份正常文件");
        let before = hash(&path);

        // 改内容 + 让临时文件在写完后损坏
        wb.sheets[0].set(5, 0, Cell { value: Value::Number(999.0), formula: None });
        unsafe { std::env::set_var("SC_CORRUPT_TMP", mode) };
        let err = sheetcore::xlsx::save_atomic(&path, &wb).expect_err("产物损坏必须报错");
        unsafe { std::env::remove_var("SC_CORRUPT_TMP") };

        let msg = format!("{err:#}");
        assert!(msg.contains("完整性校验失败") || msg.contains("未通过完整性校验"), "{mode} 的错误没点明门禁：{msg}");
        assert_eq!(hash(&path), before, "{mode}：原文件被覆盖了");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|x| x.file_name().to_string_lossy().to_string()))
            .filter(|n| n.contains(".saving-") || n.ends_with(".rezipped"))
            .collect();
        assert!(leftovers.is_empty(), "{mode}：失败后留下临时文件 {leftovers:?}");

        // 解除注入后重试必须成功，且新内容真的写进去
        sheetcore::xlsx::save_atomic(&path, &wb).expect("解除注入后应能保存");
        let back = sheetcore::xlsx::load(&path).expect("重新读回");
        assert_eq!(back.sheet(0).get(5, 0).unwrap().value.display(), "999", "{mode}：重试后内容不对");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 门禁不能误伤正常文件：新建、覆盖、另存为都要能通过
#[test]
fn integrity_gate_accepts_every_normal_save() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("gate-ok");
    let path = dir.join("ok.xlsx");
    let mut wb = book();
    sheetcore::xlsx::save_atomic(&path, &wb).expect("首次保存");
    sheetcore::xlsx::verify_saved_package(&path).expect("首次产物应通过门禁");
    wb.sheets[0].set(2, 0, Cell { value: Value::Number(7.0), formula: None });
    sheetcore::xlsx::save_atomic(&path, &wb).expect("覆盖保存");
    sheetcore::xlsx::verify_saved_package(&path).expect("覆盖后的产物应通过门禁");
    let other = dir.join("copy.xlsx");
    sheetcore::xlsx::save_atomic(&other, &wb).expect("另存为");
    sheetcore::xlsx::verify_saved_package(&other).expect("另存产物应通过门禁");
    let _ = std::fs::remove_dir_all(&dir);
}


// ---------------------------------------------------------------------------
// C13 真实文件占用 & 保存阶段进程中断（不真断电）

#[cfg(windows)]
mod occupancy {
    use super::*;

    // 直接声明 FFI：windows-sys 0.52 的这些符号分散在别的 feature 里，为一条测试开
    // 一整套 feature 不值得。共享模式取「只读」，正是 Office 打开工作簿时的行为。
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sa: *mut core::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
        fn CloseHandle(h: *mut core::ffi::c_void) -> i32;
    }
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const OPEN_EXISTING: u32 = 3;

    struct Exclusive(*mut core::ffi::c_void);
    impl Drop for Exclusive {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open_exclusive(path: &Path) -> Exclusive {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let h = unsafe {
            CreateFileW(wide.as_ptr(), GENERIC_READ, FILE_SHARE_READ, std::ptr::null_mut(), OPEN_EXISTING, 0, std::ptr::null_mut())
        };
        assert!(!h.is_null() && h as isize != -1, "独占打开失败");
        Exclusive(h)
    }

    #[test]
    fn exclusive_handle_blocks_replacement_and_retry_succeeds() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("occupied");
        let path = dir.join("occupied.xlsx");
        let mut wb = book();
        sheetcore::xlsx::save_atomic(&path, &wb).expect("先写一份");
        let before = hash(&path);

        let hold = open_exclusive(&path);
        wb.sheets[0].set(3, 0, Cell { value: Value::Number(5.0), formula: None });
        let err = sheetcore::xlsx::save_atomic(&path, &wb).expect_err("被独占时必须报错");
        let msg = format!("{err:#}");
        assert!(msg.contains("替换") && msg.contains("失败"), "错误没说明替换失败：{msg}");
        assert_eq!(hash(&path), before, "被独占时原文件不能变");
        let stray: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|x| x.file_name().to_string_lossy().to_string()))
            .filter(|n| n.contains(".saving-"))
            .collect();
        assert!(stray.is_empty(), "被独占保存失败后不得在原目录残留 .saving-* 垃圾文件：{stray:?}");

        drop(hold);
        sheetcore::xlsx::save_atomic(&path, &wb).expect("解除占用后应能保存");
        let back = sheetcore::xlsx::load(&path).expect("读回");
        assert_eq!(back.sheet(0).get(3, 0).unwrap().value.display(), "5");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 共享盘常见的「能改内容、不能删除/重命名」：rename 被拒后走原地流式覆写。
    /// 覆写前会在本机临时目录留一份回滚副本；成功路径上它不能影响结果，也不能在原目录留下任何文件。
    #[test]
    fn delete_denied_target_is_overwritten_in_place_and_verified() {
        const FILE_SHARE_WRITE: u32 = 0x2;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("delete-denied");
        let path = dir.join("shared.xlsx");
        let mut wb = book();
        sheetcore::xlsx::save_atomic(&path, &wb).expect("先写一份");

        // 别的句柄允许读写共享、但不允许删除：rename 覆盖目标会被拒（os error 5）
        let hold = {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
            let h = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            assert!(!h.is_null() && h as isize != -1, "打开失败");
            Exclusive(h)
        };
        wb.sheets[0].set(4, 0, Cell { value: Value::Number(7.0), formula: None });
        let res = sheetcore::xlsx::save_atomic(&path, &wb);
        drop(hold);
        res.expect("目标只拦删除/重命名时应原地覆写成功");

        let back = sheetcore::xlsx::load(&path).expect("覆写后的文件必须能正常打开");
        assert_eq!(back.sheet(0).get(4, 0).unwrap().value.display(), "7");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|x| x.file_name().to_string_lossy().to_string()))
            .filter(|n| n != "shared.xlsx")
            .collect();
        assert!(leftovers.is_empty(), "原目录不得残留任何中间文件：{leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn killed_during_save_leaves_the_original_intact() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("killed");
    let path = dir.join("killed.xlsx");
    let mut wb = book();
    sheetcore::xlsx::save_atomic(&path, &wb).expect("先写一份");
    let before = hash(&path);

    // 子进程：同一个测试二进制，跑 child_save_worker，卡在“临时文件已写好”的窗口里
    let exe = std::env::current_exe().expect("测试可执行文件");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", "child_save_worker", "--ignored", "--nocapture"])
        .env("SC_WORKER_PATH", &path)
        .env("SC_SAVE_PAUSE_MS", "15000")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("启动保存子进程");
    // 等到临时文件出现（说明已经写完、正卡在替换前），再强杀
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut saw_tmp = false;
    while std::time::Instant::now() < deadline {
        if std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name().to_string_lossy().contains(".saving-")
        }) {
            saw_tmp = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(saw_tmp, "子进程没进入「已写临时文件」阶段");
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(hash(&path), before, "强杀后原文件必须完好");
    let back = sheetcore::xlsx::load(&path).expect("原文件仍可读");
    assert_eq!(back.sheet(0).get(0, 0).unwrap().value.display(), "金额");

    // 清掉残留临时文件后，再次保存必须成功（不是永久卡死）
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.file_name().to_string_lossy().contains(".saving-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    wb.sheets[0].set(4, 0, Cell { value: Value::Number(6.0), formula: None });
    sheetcore::xlsx::save_atomic(&path, &wb).expect("强杀后重试应成功");
    let back = sheetcore::xlsx::load(&path).expect("重试后读回");
    assert_eq!(back.sheet(0).get(4, 0).unwrap().value.display(), "6");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 只被子进程用：加载 → 改一格 → 保存（父进程会在 SC_SAVE_PAUSE_MS 的窗口里杀掉它）
#[test]
#[ignore = "helper process for killed_during_save_leaves_the_original_intact"]
fn child_save_worker() {
    let Ok(path) = std::env::var("SC_WORKER_PATH") else { return };
    let path = PathBuf::from(path);
    let mut wb = sheetcore::xlsx::load(&path).expect("子进程读入");
    wb.sheets[0].set(9, 0, Cell { value: Value::Number(12345.0), formula: None });
    let _ = sheetcore::xlsx::save_atomic(&path, &wb);
}

/// 加密工作簿最终落盘是 CFB 容器（不是 ZIP）：门禁必须认，不能当坏包拒绝
#[test]
fn encrypted_workbook_passes_the_gate_even_though_it_is_cfb() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("encrypted");
    let path = dir.join("secret.xlsx");
    let mut wb = book();
    wb.encryption = Some(sheetcore::crypto::EncryptionSettings::new("pw-123"));
    sheetcore::xlsx::save(&path, &wb).expect("加密保存");
    let head = std::fs::read(&path).unwrap();
    assert_eq!(&head[..8], &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1], "加密后应是 CFB");
    sheetcore::xlsx::verify_saved_package(&path).expect("加密产物必须通过门禁");
    // 且能真的解回来
    let back = sheetcore::xlsx::load_with_password(&path, "pw-123").expect("解密读回");
    assert_eq!(back.sheet(0).get(0, 0).unwrap().value.display(), "金额");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 关系目标解析：'/xl/...' 绝对路径、'../' 向上、外部链接不查
#[test]
fn relationship_targets_are_resolved_the_xml_way() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("rels");
    let path = dir.join("rels.xlsx");
    let wb = book();
    sheetcore::xlsx::save_atomic(&path, &wb).expect("保存");
    sheetcore::xlsx::verify_saved_package(&path).expect("正常文件的相对关系应解析正确");

    // 把工作簿关系改成一个 '../xl/worksheets/sheet1.xml'（等价路径）：必须仍能解析到
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut rels = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("xl/_rels/workbook.xml.rels").unwrap(), &mut rels).unwrap();
    drop(zip);
    let patched = rels.replace("worksheets/sheet1.xml", "../xl/worksheets/sheet1.xml");
    assert_ne!(patched, rels, "夹具里应有 worksheets/sheet1.xml");
    let tmp = dir.join("rels2.xlsx");
    {
        let mut src = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let out = std::fs::File::create(&tmp).unwrap();
        let mut zw = zip::ZipWriter::new(out);
        for i in 0..src.len() {
            let mut e = src.by_index(i).unwrap();
            let name = e.name().to_string();
            zw.start_file(name.clone(), zip::write::SimpleFileOptions::default()).unwrap();
            if name == "xl/_rels/workbook.xml.rels" {
                std::io::Write::write_all(&mut zw, patched.as_bytes()).unwrap();
            } else {
                std::io::copy(&mut e, &mut zw).unwrap();
            }
        }
        zw.finish().unwrap();
    }
    sheetcore::xlsx::verify_saved_package(&tmp).expect("../ 关系必须解析到真实部件，而不是被跳过");
    let _ = std::fs::remove_dir_all(&dir);
}
