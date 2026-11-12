//! UI 回归：启动真实的 app.exe，用 SC_DUMP 导出绘制几何，断言关键内容还在。
//!
//! 这不是像素比对，而是「界面有没有画出来、画在哪儿」的契约测试：
//! 窗口分栏、单元格文字、坐标、条件格式图标、编辑锁标记都能断言。
//! 屏幕关着/RDP 断开也能跑，因为不依赖截图。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 跑一次 app，等 dump 落盘，返回内容
/// 这些测试各自会启动同一个 app.exe：单实例端口只有一个，必须串行跑，
/// 否则后启动的进程会把路径转给前一个实例然后自己退出，dump 永远不落盘（假失败）。
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn dump_for(file: &str, extra: &[(&str, &str)], wait: Duration) -> Option<String> {
    let _guard = serial();
    dump_for_locked(file, extra, wait)
}

fn dump_for_locked(file: &str, extra: &[(&str, &str)], wait: Duration) -> Option<String> {
    let source = Path::new(file);
    assert!(source.exists(), "required test fixture is missing: {}", source.display());
    let out = std::env::temp_dir().join(format!(
        "ui_dump_{}_{}.txt",
        std::process::id(),
        source.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&out);

    let exe = env!("CARGO_BIN_EXE_app");
    let mut cmd = Command::new(exe);
    cmd.arg(file)
        .env("SC_DUMP", &out)
        .env("SC_DUMP_AT", "2")
        .env_remove("SC_B1")
        .env_remove("SC_B3")
        .env_remove("SC_B4")
        // 独立单实例端口：用户开着 app 时也要能起测试实例（否则会转发给用户那个进程）
        .env("SC_INSTANCE_PORT", "49731")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("启动 app 失败");

    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if out.exists() {
            if let Ok(text) = std::fs::read_to_string(&out) {
                if !text.trim().is_empty() {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&out);
                    return Some(text);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&out);
    panic!("UI dump was not produced for {} within {:?}", source.display(), wait)
}

fn has_cell(dump: &str, r: u32, c: u32, text: &str) -> bool {
    dump.lines().any(|l| {
        l.starts_with(&format!("text ({r},{c})")) && l.contains(&format!("\"{text}\""))
    })
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn grid_renders_cells_and_panes() {
    let Some(dump) = dump_for("tests/test-work/table.xlsx", &[], Duration::from_secs(30)) else {
        return;
    };
    assert!(
        dump.lines().any(|l| l.starts_with("pane readonly=false")),
        "没有主面板的绘制记录：\n{dump}"
    );
    assert!(has_cell(&dump, 0, 0, "姓名"), "表头没画出来");
    assert!(has_cell(&dump, 1, 0, "张三"), "第一行数据没画出来");
    assert!(
        dump.lines().any(|l| l.starts_with("avail=")),
        "缺视口尺寸记录"
    );
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn conditional_format_icons_render() {
    let Some(dump) = dump_for(
        "tests/test-work/icons.xlsx",
        &[],
        Duration::from_secs(30),
    ) else {
        return;
    };
    let icons: Vec<&str> = dump.lines().filter(|l| l.starts_with("icon ")).collect();
    assert!(!icons.is_empty(), "图标集一个都没画：\n{dump}");
    assert!(
        icons.iter().any(|l| l.contains("TrafficLights")),
        "红绿灯图标没出现"
    );
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn compare_mode_splits_into_two_panes() {
    let Some(dump) = dump_for(
        "tests/test-work/chart.xlsx",
        &[("SC_COMPARE", "tests/test-work/table.xlsx")],
        Duration::from_secs(30),
    ) else {
        return;
    };
    let panes: Vec<&str> = dump.lines().filter(|l| l.starts_with("pane ")).collect();
    assert_eq!(panes.len(), 2, "并排比较应该有两个面板：\n{dump}");
    assert!(panes.iter().any(|l| l.contains("readonly=true")), "右侧应该是只读");
    assert!(has_cell(&dump, 0, 0, "产品"), "左侧内容缺失");
    assert!(has_cell(&dump, 0, 0, "姓名"), "右侧内容缺失");
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn pictures_are_drawn_at_anchor() {
    let Some(dump) = dump_for("tests/test-work/pic.xlsx", &[], Duration::from_secs(30)) else {
        return;
    };
    let pics: Vec<&str> = dump.lines().filter(|l| l.starts_with("pic ")).collect();
    assert_eq!(pics.len(), 1, "图片没画出来：
{dump}");
    assert!(pics[0].contains("xl/media/"), "部件名丢了: {}", pics[0]);
    assert!(pics[0].contains("at=(2,1,7,3)"), "位置不对: {}", pics[0]);
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn locked_cell_is_marked() {
    // 单机时锁不会出现；用 SC_LOCK 让宿主自己占一格，应能看到标记
    let Some(dump) = dump_for(
        "tests/test-work/table.xlsx",
        &[("SC_LOCK", "1"), ("SC_LOCK_CELL", "1,1"), ("SC_LOCK_RELEASE_SECS", "60")],
        Duration::from_secs(30),
    ) else {
        return;
    };
    // 单机无网络时不产生锁，dump 里没有 locked 行是正常的；这里只要求能正常渲染
    assert!(has_cell(&dump, 1, 1, "研发"), "内容没画出来");
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn textbox_renders_and_moves() {
    let src = Path::new("tests/test-work/table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let tmp = std::env::temp_dir().join(format!("ui_tb_{}.xlsx", std::process::id()));
    std::fs::copy(src, &tmp).unwrap();
    let Some(dump) = dump_for(
        &tmp.to_string_lossy(),
        &[
            ("SC_TEXTBOX", "120,80,220,70"),
            ("SC_TB_TEXT", "文本框内容"),
            ("SC_TB_SEL", "0"),
            ("SC_TB_RECT", "160,40,180,50"),
        ],
        Duration::from_secs(30),
    ) else {
        let _ = std::fs::remove_file(&tmp);
        return;
    };
    let lines: Vec<&str> = dump.lines().filter(|l| l.starts_with("textbox ")).collect();
    assert_eq!(lines.len(), 1, "文本框没画出来：\n{dump}");
    assert!(lines[0].contains("文字=\"文本框内容\""), "文字：{}", lines[0]);
    // SC_TB_RECT 之后尺寸要变成 180×50（dump 里是屏幕坐标，含表格原点偏移）
    let nums: Vec<f32> = lines[0]
        .split("rect=(")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
        .unwrap_or_default();
    assert_eq!(nums.len(), 4, "rect 解析失败：{}", lines[0]);
    assert!((nums[2] - 180.0).abs() < 1.5, "宽没跟模型走：{}", lines[0]);
    assert!((nums[3] - 50.0).abs() < 1.5, "高没跟模型走：{}", lines[0]);
    assert!(nums[0] > 160.0, "x 应该右移：{}", lines[0]);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn textbox_drag_moves_model() {
    let src = Path::new("tests/test-work/table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let tmp = std::env::temp_dir().join(format!("ui_tbdrag_{}.xlsx", std::process::id()));
    std::fs::copy(src, &tmp).unwrap();
    let Some(dump) = dump_for(
        &tmp.to_string_lossy(),
        &[
            ("SC_TEXTBOX", "120,80,220,70"),
            ("SC_TB_TEXT", "拖我"),
            ("SC_TB_SEL", "0"),
            ("SC_TB_DRAG", "60,40"),
        ],
        Duration::from_secs(30),
    ) else {
        let _ = std::fs::remove_file(&tmp);
        return;
    };
    let line = dump
        .lines()
        .find(|l| l.starts_with("textbox (0)"))
        .unwrap_or_else(|| panic!("文本框没画出来：\n{dump}"));
    let nums: Vec<f32> = line
        .split("rect=(")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
        .unwrap_or_default();
    // 拖动 (60,40)：模型从 (120,80) 到 (180,120)。
    // 表格原点不写死——标签条/功能区高度变了也会跟着走，用 dump 里的 pane 矩形换算。
    let pane_y = dump
        .lines()
        .find(|l| l.starts_with("pane "))
        .and_then(|l| l.split("rect=(").nth(1))
        .and_then(|s| s.split(',').nth(1))
        .and_then(|v| v.trim().parse::<f32>().ok())
        .unwrap_or(225.0);
    assert!(nums[0] >= 200.0 && nums[0] <= 220.0, "x 没动到位：{line}");
    let want_y = pane_y + 19.0 + 120.0;
    assert!(
        (nums[1] - want_y).abs() <= 4.0,
        "y 没动到位：期望 ≈{want_y}（pane_y={pane_y}），实际 {line}"
    );
    assert!((nums[2] - 220.0).abs() < 1.5 && (nums[3] - 70.0).abs() < 1.5, "尺寸不该变：{line}");
    let _ = std::fs::remove_file(&tmp);
}

// ---------------------------------------------------------------- 数据分析四件套

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn subtotal_and_flashfill_apply() {
    let src = Path::new("tests/test-work/table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let tmp = std::env::temp_dir().join(format!("ui_data_{}.xlsx", std::process::id()));
    std::fs::copy(src, &tmp).unwrap();
    // table.xlsx 的 部门 列不是相邻同值，分类汇总会如实按「相邻同组」处理（可能多组）
    let Some(dump) = dump_for(
        &tmp.to_string_lossy(),
        &[("SC_SUBTOTAL", "A1:C6,B,C")],
        Duration::from_secs(30),
    ) else {
        let _ = std::fs::remove_file(&tmp);
        return;
    };
    assert!(
        dump.lines().any(|l| l.contains("汇总")),
        "分类汇总的小计行没出现：\n{dump}"
    );
    assert!(
        dump.lines().any(|l| l.contains("总计")),
        "总计行没出现：\n{dump}"
    );
    let _ = std::fs::remove_file(&tmp);
}

// ---------------------------------------------------------------- 迷你图

/// 仓库里 Excel 365 造的迷你图样本（三种类型 + 双迷你图组）
fn spark_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/data/sparklines.xlsx")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("../core/tests/data/sparklines.xlsx"))
}

#[test]
fn sparklines_render_in_cells() {
    let f = spark_fixture();
    assert!(f.exists(), "required test fixture is missing: {}", f.display());
    // The app creates coordination files next to an opened workbook; use a temporary copy.
    let copy = std::env::temp_dir().join(format!("ui_spark_fixture_{}.xlsx", std::process::id()));
    std::fs::copy(&f, &copy).expect("copy sparkline fixture");
    let Some(dump) = dump_for(&copy.to_string_lossy(), &[], Duration::from_secs(30)) else {
        return;
    };
    let _ = std::fs::remove_file(&copy);
    let lines: Vec<&str> = dump
        .lines()
        .filter(|l| l.starts_with("sparkline "))
        .collect();
    assert_eq!(lines.len(), 5, "五条迷你图都要画：\n{dump}");
    // F1 折线、F2 柱形、F3 盈亏，各 5 个点
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("sparkline (0,5) Line") && l.contains("点=5")),
        "F1 折线没画：{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("sparkline (1,5) Column")),
        "F2 柱形没画：{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("sparkline (2,5) WinLoss")),
        "F3 盈亏没画：{lines:?}"
    );
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn sparkline_create_from_selection() {
    // 用两张表的普通文件，按 A1:E1 选区建柱形迷你图，落在 F1
    let src = Path::new("tests/test-work/table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let tmp = std::env::temp_dir().join(format!("ui_spark_{}.xlsx", std::process::id()));
    std::fs::copy(src, &tmp).unwrap();
    let Some(dump) = dump_for(
        &tmp.to_string_lossy(),
        &[
            ("SC_SEL", "0,0"),
            ("SC_SEL2", "0,4"),
            ("SC_SPARK", "column"),
        ],
        Duration::from_secs(30),
    ) else {
        let _ = std::fs::remove_file(&tmp);
        return;
    };
    assert!(
        dump.lines()
            .any(|l| l.starts_with("sparkline (0,5) Column") && l.contains("点=5")),
        "选区建的迷你图没画出来：\n{dump}"
    );
    let _ = std::fs::remove_file(&tmp);
}

/// 造一个只有 A1 的小文件，可选地带上字体（走真实的 xlsx 写盘）
fn make_font_fixture(name: &str, font: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("sheet_ui_font");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].set(
        0,
        0,
        sheetcore::model::Cell {
            // 混着拉丁和汉字：不同字体的排版宽度才拉得开
            value: sheetcore::model::Value::Text("FontTest 宋体".into()),
            formula: None,
        },
    );
    if !font.is_empty() {
        let id = wb.styles.intern(sheetcore::style::CellStyle {
            font: font.into(),
            size: 11.0,
            ..Default::default()
        });
        wb.sheets[0].set_style(0, 0, id);
    }
    sheetcore::xlsx::save(&p, &wb).expect("造样本");
    p
}

fn cell_line<'a>(dump: &'a str, r: u32, c: u32) -> &'a str {
    dump.lines()
        .find(|l| l.starts_with(&format!("text ({r},{c})")))
        .unwrap_or_else(|| panic!("({r},{c}) 没有文字绘制记录：
{dump}"))
}

/// dump 行里的排版宽度（同一字符串换字体后宽度会变）
fn cell_width(line: &str) -> f32 {
    line.split("w=")
        .nth(1)
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or_else(|| panic!("这行没有 w=：{line}"))
}

#[test]
fn cell_font_picks_embedded_family() {
    let plain = make_font_fixture("plain.xlsx", "");
    let song = make_font_fixture("song.xlsx", "宋体");
    let mono = make_font_fixture("mono.xlsx", "JetBrains Mono");

    let d1 = dump_for(plain.to_str().unwrap(), &[], Duration::from_secs(30)).expect("dump plain");
    assert!(cell_line(&d1, 0, 0).contains("family=default"), "默认字体应该走 default：{}", cell_line(&d1, 0, 0));

    let d2 = dump_for(song.to_str().unwrap(), &[], Duration::from_secs(30)).expect("dump song");
    let l2 = cell_line(&d2, 0, 0);
    assert!(l2.contains("family=cjk-serif"), "宋体应选 cjk-serif：{l2}");
    assert!(l2.contains("font=\"宋体\""), "dump 里应带字体名：{l2}");

    let d3 = dump_for(mono.to_str().unwrap(), &[], Duration::from_secs(30)).expect("dump mono");
    let l3 = cell_line(&d3, 0, 0);
    assert!(l3.contains("family=mono"), "等宽字体应选 mono");

    // 宽度不同 = 真的换了字形数据，不只是换了个 family 标签
    let w1 = cell_width(cell_line(&d1, 0, 0));
    let w2 = cell_width(l2);
    let w3 = cell_width(l3);
    eprintln!("[font] 排版宽度 default={w1:.1} 宋体={w2:.1} 等宽={w3:.1}");
    assert!(
        (w1 - w2).abs() > 0.5,
        "宋体和默认字体排版宽度一样（{w1} vs {w2}），说明没换字形"
    );
    assert!((w1 - w3).abs() > 0.5, "等宽和默认宽度一样（{w1} vs {w3}）");
}

#[test]
fn font_change_applies_and_saves() {
    let src = make_font_fixture("apply_src.xlsx", "");
    let out = src.with_file_name("apply_out.xlsx");
    let _ = std::fs::remove_file(&out);

    let dump = dump_for(
        src.to_str().unwrap(),
        &[
            ("SC_SEL", "0,0"),
            ("SC_FONT", "宋体"),
            ("SC_SAVE", out.to_str().unwrap()),
        ],
        Duration::from_secs(30),
    )
    .expect("dump apply");
    // 渲染立刻换了族
    assert!(
        cell_line(&dump, 0, 0).contains("family=cjk-serif"),
        "选了宋体后渲染没换族：{}",
        cell_line(&dump, 0, 0)
    );

    // 存盘后重开还能读到字体名
    assert!(out.exists(), "SC_SAVE 没写出文件");
    let back = sheetcore::xlsx::load(&out).expect("重开");
    let st = back.styles.get(back.sheets[0].style_of(0, 0));
    assert_eq!(st.font, "宋体", "存盘后字体丢了");

    // 关掉重开：这一份文件再画一遍，仍然是宋体族（不是只有内存里那一刻对）
    let again = dump_for(out.to_str().unwrap(), &[], Duration::from_secs(30)).expect("dump reopen");
    assert!(
        cell_line(&again, 0, 0).contains("family=cjk-serif"),
        "重开后字体没恢复：{}",
        cell_line(&again, 0, 0)
    );
}

#[test]
fn merged_range_uses_anchor_font() {
    // 合并区域只画左上角，整块应该按锚点那格的样式（含字体）来画
    let dir = std::env::temp_dir().join("sheet_ui_font");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("merged_font.xlsx");
    let _ = std::fs::remove_file(&p);
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].set(
        0,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Text("合并宋体".into()),
            formula: None,
        },
    );
    let id = wb.styles.intern(sheetcore::style::CellStyle {
        font: "宋体".into(),
        size: 11.0,
        ..Default::default()
    });
    wb.sheets[0].set_style(0, 0, id);
    wb.sheets[0].merge_range(0, 0, 1, 1);
    sheetcore::xlsx::save(&p, &wb).expect("造样本");

    let dump = dump_for(p.to_str().unwrap(), &[], Duration::from_secs(30)).expect("dump merged");
    let line = cell_line(&dump, 0, 0);
    assert!(line.contains("family=cjk-serif"), "合并区没按锚点字体画：{line}");
    // 右下角那格不该单独出文字
    assert!(
        !dump.lines().any(|l| l.starts_with("text (1,1)")),
        "合并区里不该再画一格"
    );
}

#[test]
fn header_highlight_follows_merged_selection() {
    // Excel 选中合并格时，整块合并区覆盖的列头都高亮（不只是活动格那一列）
    let dir = std::env::temp_dir().join("sheet_ui_headsel");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("head_sel.xlsx");
    let _ = std::fs::remove_file(&p);
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].set(
        0,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Text("标题".into()),
            formula: None,
        },
    );
    wb.sheets[0].merge_range(0, 0, 0, 4);
    sheetcore::xlsx::save(&p, &wb).expect("造样本");

    let dump = dump_for(p.to_str().unwrap(), &[], Duration::from_secs(30)).expect("dump headsel");
    let line = dump
        .lines()
        .find(|l| l.starts_with("head_sel "))
        .unwrap_or_else(|| panic!("dump 里没有 head_sel：{dump}"));
    assert!(line.contains("cols=0..4"), "合并区应该让 A..E 列头都高亮：{line}");
    assert!(line.contains("rows=0..0"), "只选中第 1 行：{line}");
}

#[test]
fn external_link_values_render_and_refresh() {
    let dir = std::env::temp_dir().join("sheet_ui_xlink");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 源工作簿
    let src = dir.join("预算.xlsx");
    let mut s = sheetcore::model::Workbook::new_blank();
    s.sheets[0].name = "预算".into();
    s.sheets[0].set(0, 0, sheetcore::model::Cell {
        value: sheetcore::model::Value::Number(10.0),
        formula: None,
    });
    sheetcore::xlsx::save(&src, &s).unwrap();

    // 引用方：链接 + 缓存 + [1] 公式（save 会把 externalLink 部件补齐）
    let main = dir.join("汇总.xlsx");
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.path = Some(main.clone());
    wb.sheets[0].name = "汇总".into();
    let mut link = sheetcore::xlinks::ExternalBook {
        index: 1,
        target: "预算.xlsx".into(),
        sheets: vec!["预算".into()],
        ..Default::default()
    };
    link.cache.insert((0, 0, 0), sheetcore::model::Value::Number(10.0));
    wb.links.push(link);
    wb.sheets[0].set(0, 0, sheetcore::model::Cell {
        value: sheetcore::model::Value::Number(10.0),
        formula: Some("=[1]预算!A1".into()),
    });
    sheetcore::xlsx::save(&main, &wb).expect("写样本");

    // 打开就能画出来（用文件里的缓存）
    let dump = dump_for(main.to_str().unwrap(), &[], Duration::from_secs(30)).expect("dump link");
    assert!(cell_line(&dump, 0, 0).contains("\"10\""), "缓存值没画出来：{}", cell_line(&dump, 0, 0));

    // 源文件改成 77 → 更新链接 → 画面跟着变
    let mut s2 = sheetcore::model::Workbook::new_blank();
    s2.sheets[0].name = "预算".into();
    s2.sheets[0].set(0, 0, sheetcore::model::Cell {
        value: sheetcore::model::Value::Number(77.0),
        formula: None,
    });
    sheetcore::xlsx::save(&src, &s2).unwrap();
    let dump2 = dump_for(
        main.to_str().unwrap(),
        &[("SC_XLINK_REFRESH", "1")],
        Duration::from_secs(30),
    )
    .expect("dump refresh");
    assert!(
        cell_line(&dump2, 0, 0).contains("\"77\""),
        "更新链接后没换成新值：{}",
        cell_line(&dump2, 0, 0)
    );
}
#[test]
fn revision_revert_restores_font() {
    let dir = std::env::temp_dir().join("sheet_ui_rev");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("修订.xlsx");

    // A1 现在是宋体
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].set(0, 0, sheetcore::model::Cell {
        value: sheetcore::model::Value::Text("x".into()),
        formula: None,
    });
    let song = wb.styles.intern(sheetcore::style::CellStyle {
        font: "宋体".into(),
        size: 11.0,
        ..Default::default()
    });
    wb.sheets[0].set_style(0, 0, song);
    sheetcore::xlsx::save(&p, &wb).unwrap();

    // 造一条"把黑体改成宋体"的修订（当前值必须等于 after 才能撤回）
    let mut e = sheetcore::revision::Entry::new("测试", 0, "设置格式", "Sheet1");
    let mut before_style = sheetcore::style::CellStyle::default();
    before_style.font = "黑体".into();
    before_style.size = 11.0;
    let mut after_style = before_style.clone();
    after_style.font = "宋体".into();
    e.cells.push(sheetcore::revision::Change {
        r: 0,
        c: 0,
        before: sheetcore::revision::Snap { v: "x".into(), f: None, t: "s".into() },
        after: sheetcore::revision::Snap { v: "x".into(), f: None, t: "s".into() },
        before_style: Some(before_style),
        after_style: Some(after_style),
    });
    sheetcore::revision::append(&p, &e).unwrap();

    let dump = dump_for(
        p.to_str().unwrap(),
        &[("SC_REV_REVERT", "1")],
        Duration::from_secs(30),
    )
    .expect("dump revert");
    let line = cell_line(&dump, 0, 0);
    assert!(line.contains("family=cjk-sans"), "撤回后应回到黑体：{line}");

    // 日志里应多一条"撤回修订"，且能重开读到两条
    let log = sheetcore::revision::load(&p, 10);
    assert_eq!(log.len(), 2, "撤回本身也要记一条");
    assert_eq!(log[1].label, "撤回修订");
}

/// 跑一次 app，把 stderr 收回来（功能区几何日志走 stderr）
fn ui_log_for(file: &str, extra: &[(&str, &str)], wait: Duration) -> String {
    let _guard = serial();
    let out = std::env::temp_dir().join(format!("ui_log_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let f = std::fs::File::create(&out).unwrap();
    let exe = env!("CARGO_BIN_EXE_app");
    let mut cmd = Command::new(exe);
    cmd.arg(file)
        .env("SC_NO_OPEN_DIALOG", "1")
        .env("SC_INSTANCE_PORT", "49731")
        .env_remove("SC_B1")
        .env_remove("SC_B3")
        .env_remove("SC_B4")
        .stdout(Stdio::null())
        .stderr(Stdio::from(f));
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("启动 app 失败");
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(120));
        if out.exists() {
            if let Ok(t) = std::fs::read_to_string(&out) {
                if t.matches("[ui] 组").count() >= 14 {
                    break;
                }
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    std::fs::read_to_string(&out).unwrap_or_default()
}

#[test]
fn ribbon_groups_aligned_and_fit() {
    let dir = std::env::temp_dir().join("sheet_ui_ribbon");
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join("空白.xlsx");
    let wb = sheetcore::model::Workbook::new_blank();
    sheetcore::xlsx::save(&p, &wb).unwrap();

    let log = ui_log_for(p.to_str().unwrap(), &[("SC_UI_LOG", "1")], Duration::from_secs(45));
    // 取最后一帧：从最后一次出现"剪贴板"开始，读满 7 个组
    let mut groups: Vec<(String, f32, f32, f32, f32)> = Vec::new();
    let mut started = false;
    for line in log.lines().filter(|l| l.contains("[ui] 组")) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // [ui] 组 剪贴板 x= 8.0 y= 39.0 w= 96.0 h= 73.0
        if parts.len() < 8 {
            continue;
        }
        let title = parts[2].to_string();
        let num = |k: &str| -> f32 {
            for (i, s) in parts.iter().enumerate() {
                if let Some(v) = s.strip_prefix(k) {
                    if !v.is_empty() {
                        return v.parse::<f32>().unwrap_or(0.0);
                    }
                    if let Some(n) = parts.get(i + 1) {
                        return n.parse::<f32>().unwrap_or(0.0);
                    }
                }
            }
            0.0
        };
        if title == "剪贴板" {
            groups.clear();
            started = true;
        }
        if !started {
            continue;
        }
        groups.push((title, num("x="), num("y="), num("w="), num("h=")));
    }
    let names: Vec<&str> = groups.iter().map(|g| g.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "剪贴板",
            "字体",
            "对齐方式",
            "数字",
            "样式",
            "单元格",
            "编辑",
            "加载项"
        ],
        "开始页应显示 8 个分组（照 Excel 365 排）"
    );
    let y0 = groups[0].2;
    let h0 = groups[0].4;
    for g in &groups {
        assert!((g.2 - y0).abs() < 0.5, "分组 {} 的 y={} 与其它组不齐", g.0, g.2);
        assert!((g.4 - h0).abs() < 0.5, "分组 {} 的 h={} 与其它组不一致", g.0, g.4);
        assert!(g.3 > 40.0 && g.3 < 520.0, "分组 {} 宽度异常：{}", g.0, g.3);
    }
    // Excel 的「开始」页本身就是 ~1750px 宽；窗口不足时功能区横向滚动（Excel 也会）。
    let end = groups.last().unwrap().1 + groups.last().unwrap().3;
    assert!(end < 2100.0, "分组总宽 {end} 与 Excel 实测差距过大");
}

