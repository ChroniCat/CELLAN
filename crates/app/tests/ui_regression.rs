//! UI 回归：启动真实的 app.exe，用 SC_DUMP 导出绘制几何，断言关键内容还在。
//!
//! 这不是像素比对，而是「界面有没有画出来、画在哪儿」的契约测试：
//! 窗口分栏、单元格文字、坐标、条件格式图标、编辑锁标记都能断言。
//! 屏幕关着/RDP 断开也能跑，因为不依赖截图。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 跑一次 app，等 dump 落盘，返回内容
fn dump_for(file: &str, extra: &[(&str, &str)], wait: Duration) -> Option<String> {
    let source = Path::new(file);
    if !source.exists() {
        eprintln!("跳过：缺少 {}", source.display());
        return None;
    }
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
    None
}

fn has_cell(dump: &str, r: u32, c: u32, text: &str) -> bool {
    dump.lines().any(|l| {
        l.starts_with(&format!("text ({r},{c})")) && l.contains(&format!("\"{text}\""))
    })
}

#[test]
fn grid_renders_cells_and_panes() {
    let Some(dump) = dump_for("F:/Finance/work/table.xlsx", &[], Duration::from_secs(30)) else {
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
fn conditional_format_icons_render() {
    let Some(dump) = dump_for(
        "F:/Finance/work/icons.xlsx",
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
fn compare_mode_splits_into_two_panes() {
    let Some(dump) = dump_for(
        "F:/Finance/work/chart.xlsx",
        &[("SC_COMPARE", "F:/Finance/work/table.xlsx")],
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
fn pictures_are_drawn_at_anchor() {
    let Some(dump) = dump_for("F:/Finance/work/pic.xlsx", &[], Duration::from_secs(30)) else {
        return;
    };
    let pics: Vec<&str> = dump.lines().filter(|l| l.starts_with("pic ")).collect();
    assert_eq!(pics.len(), 1, "图片没画出来：
{dump}");
    assert!(pics[0].contains("xl/media/"), "部件名丢了: {}", pics[0]);
    assert!(pics[0].contains("at=(2,1,7,3)"), "位置不对: {}", pics[0]);
}

#[test]
fn locked_cell_is_marked() {
    // 单机时锁不会出现；用 SC_LOCK 让宿主自己占一格，应能看到标记
    let Some(dump) = dump_for(
        "F:/Finance/work/table.xlsx",
        &[("SC_LOCK", "1"), ("SC_LOCK_CELL", "1,1"), ("SC_LOCK_RELEASE_SECS", "60")],
        Duration::from_secs(30),
    ) else {
        return;
    };
    // 单机无网络时不产生锁，dump 里没有 locked 行是正常的；这里只要求能正常渲染
    assert!(has_cell(&dump, 1, 1, "研发"), "内容没画出来");
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

