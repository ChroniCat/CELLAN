//! 通用功能区渲染：XML 里的结构 → egui 控件。
//!
//! 只认 MS-CUSTOMUI 的控件种类；具体长什么样沿用 `ui_kit` 里照 Excel 实测
//! 尺寸做的那套：大按钮 76 高、常规按钮 24 高、分组 102 高。
//! 少数 Excel 内置控件（字体框、字号框、B/I/U、颜色按钮）按 id 给出专用外观，
//! 这正是 idMso 语义（Font/FontSize/Bold/FillColor…）。

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Vec2};

use super::{Ctl, Group, Item, LabelCtl, Menu, MenuNode, Node, Size};
use crate::ui_icons::Icon;
use crate::{ui_kit, SheetApp};

/// 弹出层内容：菜单或下拉项。
#[derive(Clone, Debug)]
pub enum PopupKind {
    Menu(Menu),
    Items(Vec<Item>),
}

#[derive(Clone, Debug)]
pub struct Popup {
    pub kind: PopupKind,
    pub pos: egui::Pos2,
    /// 打开时刻：同一帧的这次点击不能立刻把弹层关掉
    pub opened_at: f64,
}

/// 页签行（文件…帮助）。上下文页签由调用方按需追加。
pub fn tabs(app: &mut SheetApp, ui: &mut egui::Ui) {
    let tabs: Vec<(usize, String, String)> = app
        .ribbon
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.label.clone(), t.keytip.clone()))
        .collect();
    for (i, label, keytip) in tabs {
        let resp = ui_kit::tab_ex(ui, &label, app.ribbon_tab == i && i != 0, i == 0);
        if app.keytips && app.keytip_tab.is_none() {
            if let Some(ch) = keytip.chars().next() {
                let r = resp.rect;
                let b = Rect::from_center_size(
                    egui::pos2(r.center().x, r.top() + 6.0),
                    Vec2::new(13.0, 11.0),
                );
                ui.painter()
                    .rect_filled(b, 2.0, Color32::from_rgb(68, 68, 68));
                ui.painter().text(
                    b.center(),
                    Align2::CENTER_CENTER,
                    ch.to_string(),
                    FontId::proportional(8.5),
                    Color32::WHITE,
                );
            }
        }
        if resp.double_clicked() {
            app.ribbon_collapsed = !app.ribbon_collapsed;
            app.ribbon_popup = None;
        }
        if resp.clicked() {
            if i == 0 {
                app.backstage = true;
                app.backstage_page = 0;
            } else {
                app.ribbon_tab = i;
                app.ribbon_collapsed = false;
                app.ribbon_popup = None;
            }
        }
    }
}

/// 上下文页签（表设计 / 图表工具）：选中表格或图表时才出现。
pub fn context_tabs(app: &mut SheetApp, ui: &mut egui::Ui) {
    if app.table_at_sel().is_some() {
        if let Some(c) = app
            .ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetTableTools")
        {
            let label = c.tab.label.clone();
            let r = ui_kit::tab_ex(ui, &label, app.ribbon_tab == 100, false);
            if r.clicked() {
                app.ribbon_tab = 100;
                app.ribbon_collapsed = false;
                app.ribbon_popup = None;
            }
        }
    }
    if app.chart_at_sel().is_some() {
        if let Some(c) = app
            .ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetChartTools")
        {
            let label = c.tab.label.clone();
            let r = ui_kit::tab_ex(ui, &label, app.ribbon_tab == 101, false);
            if r.clicked() {
                app.ribbon_tab = 101;
                app.ribbon_collapsed = false;
                app.ribbon_popup = None;
            }
        }
    }
    // 选中带迷你图的单元格才出现「迷你图工具」
    if app.spark_at_sel().is_some() {
        if let Some(c) = app
            .ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetSparklineTools")
        {
            let label = c.tab.label.clone();
            let r = ui_kit::tab_ex(ui, &label, app.ribbon_tab == 102, false);
            if r.clicked() {
                app.ribbon_tab = 102;
                app.ribbon_collapsed = false;
                app.ribbon_popup = None;
            }
        }
    }
}

/// 当前页签的功能区主体。
pub fn body(app: &mut SheetApp, ui: &mut egui::Ui) {
    // 上下文页签的宿主对象没了就退回「开始」（和 Excel 一样）
    // 文件还在后台打开时，选区/表格都还没就位，先别把上下文页签踢掉
    if app.tasks.open_rx.is_none() {
        if app.ribbon_tab == 100 && app.table_at_sel().is_none() {
            if std::env::var("SC_RIBBON_LOG").is_ok() {
                eprintln!(
                    "[ribbon] 上下文页签退回开始 sel={:?} 表={}",
                    app.sel,
                    app.wb.sheet(app.sheet_idx).tables.len()
                );
            }
            app.ribbon_tab = 1;
        }
        if app.ribbon_tab == 101 && app.chart_at_sel().is_none() {
            app.ribbon_tab = 1;
        }
        if app.ribbon_tab == 102 && app.spark_at_sel().is_none() {
            app.ribbon_tab = 1;
        }
    }
    let tab = match app.ribbon_tab {
        100 => app
            .ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetTableTools")
            .map(|c| c.tab.clone()),
        101 => app
            .ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetChartTools")
            .map(|c| c.tab.clone()),
        102 => app
            .ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetSparklineTools")
            .map(|c| c.tab.clone()),
        n if n < app.ribbon.tabs.len() => Some(app.ribbon.tabs[n].clone()),
        _ => None,
    };
    let tab = match tab {
        Some(t) => t,
        None => {
            app.ribbon_tab = 1;
            match app.ribbon.tabs.get(1) {
                Some(t) => t.clone(),
                None => return,
            }
        }
    };
    ui_kit::reset_groups();
    // UI-13.2：窄窗口把放不下的分组折叠成「组名 ▾」按钮，点开仍能执行全部命令。
    // 宽度取上一帧实测值；首帧先完整画一遍，第二帧起稳定。
    let avail = (ui.clip_rect().width() - 24.0).max(200.0);
    let mut used = 0.0f32;
    let mut i = 0usize;
    while i < tab.groups.len() {
        let g = &tab.groups[i];
        let known = app.ribbon_group_w.get(&g.id).copied();
        let w = known.unwrap_or(160.0);
        let compact_w = compact_width(g);
        // UI-13.2 二级压缩：整组放不下时，先把大按钮收成小图标网格；
        // 小图标也放不下，才退化成「组名 ▾」/「更多 ▾」。
        let full_fits = known.is_some() && used + w + 60.0 <= avail;
        let compact_fits = known.is_some()
            && compact_w + 12.0 <= avail - used
            && compact_w + 12.0 < w
            && !compact_needs_text(&g.children);
        if std::env::var("SC_PANEL_LOG").is_ok() {
            eprintln!(
                "[layout] {:14} known={known:?} w={w:.0} used={used:.0} avail={avail:.0} compact={compact_w:.0}"
                , g.label
            );
        }
        if known.is_some() && !full_fits && compact_fits {
            let x0 = ui.cursor().min.x;
            group_compact(app, ui, g);
            used += (ui.cursor().min.x - x0).max(24.0);
            i += 1;
            continue;
        }
        // 给「更多 ▾」预留 60pt：不留的话最后会画到裁切区外（760×420 实测溢出 11px）
        if known.is_some() && used + w + 60.0 > avail && used > 0.0 {
            if avail - used >= 92.0 {
                ui.menu_button(format!("{} ▾", g.label), |ui| {
                    nodes(app, ui, &g.children);
                });
                used += 84.0;
                i += 1;
                continue;
            }
            // 连「组名 ▾」都放不下：剩下的组统一进「更多」。
            // 极端窄（首组就超宽）时把「更多」钉在裁切区右侧，别让它画到窗口外。
            let mut more = |ui: &mut egui::Ui| {
                ui.menu_button("更多 ▾", |ui| {
                    for h in &tab.groups[i..] {
                        ui.menu_button(&h.label, |ui| {
                            nodes(app, ui, &h.children);
                        });
                    }
                });
            };
            let clip = ui.clip_rect();
            if clip.right() - ui.cursor().min.x >= 60.0 {
                more(ui);
            } else {
                let rect = egui::Rect::from_min_size(
                    egui::pos2((clip.right() - 58.0).max(clip.left()), ui.cursor().min.y),
                    egui::Vec2::new(56.0, 22.0),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| more(ui));
            }
            break;
        }
        let x0 = ui.cursor().min.x;
        group(app, ui, g);
        let w = (ui.cursor().min.x - x0).max(24.0);
        app.ribbon_group_w.insert(g.id.clone(), w);
        used += w;
        i += 1;
    }
    // 原 group_l 的语义：点分组右下角斜箭头 → 打开单元格格式
    if app.fmt_pending {
        app.fmt_pending = false;
        app.open_format_dialog();
    }
    popup(app, ui);
}

static DBG_GROUPS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn group(app: &mut SheetApp, ui: &mut egui::Ui, g: &Group) {
    let dbg = *DBG_GROUPS.get_or_init(|| std::env::var("SC_PANEL_LOG").is_ok());
    let before = if dbg { ui.min_rect() } else { Rect::NOTHING };
    match &g.launcher {
        Some(l) => {
            let hit = ui_kit::group_l(ui, &g.label, &g.id, |ui| nodes(app, ui, &g.children));
            if hit {
                let action = l.action().to_string();
                app.ribbon_action(&action, ui.ctx());
            }
        }
        None => {
            ui_kit::group(ui, &g.label, |ui| nodes(app, ui, &g.children));
        }
    }
    if dbg {
        let after = ui.min_rect();
        eprintln!("[group] {:22} h={:.0}", g.label, after.height() - before.height());
    }
}

fn nodes(app: &mut SheetApp, ui: &mut egui::Ui, list: &[Node]) {
    for n in list {
        node(app, ui, n);
    }
}

/// UI-13.2：计一个分组里真正可点击的控件数（忽略说明文字和空白）。
fn compact_leaf_count(list: &[Node]) -> usize {
    list.iter()
        .map(|n| match n {
            Node::Button(_)
            | Node::Toggle(_)
            | Node::Split { .. }
            | Node::Menu(_)
            | Node::DropDown { .. }
            | Node::Gallery { .. }
            | Node::ComboBox { .. }
            | Node::EditBox { .. }
            | Node::Slider { .. } => 1,
            Node::Box { children, .. } => compact_leaf_count(children),
            Node::Label(_) | Node::Spacer(_) | Node::Separator => 0,
        })
        .sum()
}

/// 小图标网格的目标宽度：按控件数选 3/4 列，避免又占满整条功能区。
fn compact_width(g: &Group) -> f32 {
    let n = compact_leaf_count(&g.children);
    let cols: f32 = if n >= 6 { 4.0 } else { 3.0 };
    (cols * 52.0 + 10.0).min(210.0)
}

/// 压缩态把长标签收成图标；若控件本身没有图标，会变成空白按钮。
/// 这种组不要硬压，交给「组名 ▾」菜单，保证文字完整。
fn compact_needs_text(list: &[Node]) -> bool {
    let no_icon_long = |c: &Ctl| {
        let long = c
            .label
            .as_deref()
            .map(|s| s.chars().count() > 2)
            .unwrap_or(false);
        // 超过 5 个汉字的命令即使有图标也不要压成纯图标：截断或只剩图标
        // 都会让命令难以辨认；这类组退到「组名 ▾」保证文字可读。
        let very_long = c
            .label
            .as_deref()
            .map(|s| s.chars().count() > 5)
            .unwrap_or(false);
        (c.image.is_none() && long) || very_long
    };
    list.iter().any(|n| match n {
        Node::Button(c) | Node::Toggle(c) => no_icon_long(c),
        Node::Split { ctl, .. } => no_icon_long(ctl),
        Node::Menu(m) => no_icon_long(&m.ctl),
        Node::DropDown { ctl, .. }
        | Node::Gallery { ctl, .. }
        | Node::ComboBox { ctl, .. }
        | Node::EditBox { ctl, .. }
        | Node::Slider { ctl, .. } => no_icon_long(ctl),
        Node::Box { children, .. } => compact_needs_text(children),
        Node::Label(_) | Node::Spacer(_) | Node::Separator => false,
    })
}

/// Excel 的二级压缩：分组内的大按钮改成 24px 小按钮，排成 3–4 列的小网格。
/// 组名仍然在底部，命令一个不少；真正放不下时才由调用方退化成「组名 ▾」。
fn group_compact(app: &mut SheetApp, ui: &mut egui::Ui, g: &Group) {
    let w = compact_width(g);
    let x0 = ui.cursor().min.x;
    let mut leaves = Vec::new();
    compact_leaves(&g.children, &mut leaves);
    let cols = if leaves.len() >= 6 { 4 } else { 3 };
    let body = |app: &mut SheetApp, ui: &mut egui::Ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(w, 76.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(w);
                ui.set_max_width(w);
                for row in leaves.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
                        for n in row {
                            node_compact(app, ui, n);
                        }
                    });
                }
            },
        );
    };
    match &g.launcher {
        Some(l) => {
            // 紧凑组也要保留右下角的对话框启动器，否则“格式单元格”会丢。
            let hit = ui_kit::group_l(ui, &g.label, &g.id, |ui| body(app, ui));
            if hit {
                let action = l.action().to_string();
                app.ribbon_action(&action, ui.ctx());
            }
        }
        None => {
            let _ = ui_kit::group(ui, &g.label, |ui| body(app, ui));
        }
    }
    if std::env::var("SC_PANEL_LOG").is_ok() {
        eprintln!(
            "[compact] {:20} leaves={} target_w={w:.0} actual={:.0} h={:.0}",
            g.label,
            compact_leaf_count(&g.children),
            ui.cursor().min.x - x0,
            ui.min_rect().height()
        );
    }
}

fn compact_leaves<'a>(list: &'a [Node], out: &mut Vec<&'a Node>) {
    for n in list {
        match n {
            Node::Box { children, .. } => compact_leaves(children, out),
            Node::Button(_)
            | Node::Toggle(_)
            | Node::Split { .. }
            | Node::Menu(_)
            | Node::DropDown { .. }
            | Node::Gallery { .. }
            | Node::ComboBox { .. }
            | Node::EditBox { .. }
            | Node::Slider { .. } => out.push(n),
            Node::Label(_) | Node::Spacer(_) | Node::Separator => {}
        }
    }
}

fn node_compact(app: &mut SheetApp, ui: &mut egui::Ui, n: &Node) {
    // 压缩态是“小图标”，不是把大按钮等比缩小：标签超过两个汉字就只画图标，
    // 完整文字放进 screentip，避免 52px 宽按钮把“自动求和”截成“自动”。
    let icon_ctl = |c: &Ctl| -> Ctl {
        let mut c = c.clone();
        c.size = Size::Normal;
        c.width = Some(28.0);
        if let Some(label) = c.label.take() {
            if label.chars().count() > 2 {
                if c.screentip.is_none() && c.supertip.is_none() {
                    c.screentip = Some(label);
                }
            } else {
                c.label = Some(label);
            }
        }
        c
    };
    match n {
        Node::Button(c) => {
            let c = icon_ctl(c);
            button(app, ui, &c, false);
        }
        Node::Toggle(c) => {
            let c = icon_ctl(c);
            button(app, ui, &c, true);
        }
        Node::Split { ctl, menu } => {
            let mut ctl = icon_ctl(ctl);
            ctl.width = Some(28.0);
            split(app, ui, &ctl, menu);
        }
        Node::Menu(m) => {
            let mut m = m.clone();
            m.ctl.size = Size::Normal;
            m.ctl.width = Some(28.0);
            if let Some(label) = m.ctl.label.take() {
                if label.chars().count() > 2 {
                    if m.ctl.screentip.is_none() && m.ctl.supertip.is_none() {
                        m.ctl.screentip = Some(label);
                    }
                } else {
                    m.ctl.label = Some(label);
                }
            }
            menu(app, ui, &m);
        }
        Node::DropDown { ctl, items } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            ctl.width = Some(ctl.width.unwrap_or(84.0).min(84.0));
            dropdown(app, ui, &ctl, items);
        }
        Node::Gallery { ctl, items, .. } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            ctl.width = Some(ctl.width.unwrap_or(100.0).min(100.0));
            gallery(app, ui, &ctl, items);
        }
        Node::ComboBox { ctl, items, width } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            combo(app, ui, &ctl, items, (*width).min(90.0));
        }
        Node::EditBox {
            ctl,
            hint,
            password,
            width,
        } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            edit(app, ui, &ctl, hint, *password, (*width).min(90.0));
        }
        Node::Slider { ctl, min, max } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            slider(app, ui, &ctl, *min, *max);
        }
        Node::Box { .. } | Node::Label(_) | Node::Spacer(_) | Node::Separator => {}
    }
}

fn node(app: &mut SheetApp, ui: &mut egui::Ui, n: &Node) {
    match n {
        Node::Button(c) => button(app, ui, c, false),
        Node::Toggle(c) => button(app, ui, c, true),
        Node::Split { ctl, menu } => split(app, ui, ctl, menu),
        Node::Menu(m) => menu(app, ui, m),
        Node::DropDown { ctl, items } => dropdown(app, ui, ctl, items),
        Node::Gallery { ctl, items, .. } => gallery(app, ui, ctl, items),
        Node::ComboBox { ctl, items, width } => combo(app, ui, ctl, items, *width),
        Node::EditBox {
            ctl,
            hint,
            password,
            width,
        } => edit(app, ui, ctl, hint, *password, *width),
        Node::Label(l) => label(app, ui, l),
        Node::Slider { ctl, min, max } => slider(app, ui, ctl, *min, *max),
        Node::Box {
            vertical,
            top,
            gap,
            children,
        } => {
            if *vertical {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = *gap;
                    if *top > 0.0 {
                        ui.add_space(*top);
                    }
                    nodes(app, ui, children);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    nodes(app, ui, children);
                });
            }
        }
        Node::Spacer(w) => {
            ui.add_space(*w);
        }
        Node::Separator => ui_kit::hairline(ui),
    }
}

// ---------------------------------------------------------------- 控件

fn on_enabled(c: &Ctl, st: &super::CtlState) -> bool {
    c.enabled && st.enabled
}

fn tip_of(c: &Ctl) -> String {
    let mut s = match (&c.screentip, &c.supertip) {
        (Some(a), Some(b)) => format!("{a}\n{b}"),
        (Some(a), None) => a.clone(),
        (None, Some(b)) => b.clone(),
        (None, None) => String::new(),
    };
    // 命令表里登记的快捷键直接写进 tooltip，避免帮助文字与实际按键不符
    if let Some(cmd) = crate::command::by_action(c.action()) {
        s = if s.is_empty() {
            cmd.tip()
        } else {
            format!("{s} ({})", cmd.shortcut())
        };
    }
    s
}

fn measure(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            FontId::proportional(size),
            Color32::PLACEHOLDER,
        )
        .size()
        .x
}

fn draw_keytip(ui: &egui::Ui, key: &str, rect: Rect) {
    if key.is_empty() {
        return;
    }
    if std::env::var("SC_KEYTIP_LOG").is_ok() {
        eprintln!("[keytip] draw {key} at {rect:?}");
    }
    let w = (key.chars().count() as f32 * 5.0 + 6.0).max(13.0);
    let b = Rect::from_min_size(
        egui::pos2(rect.right() - w - 2.0, rect.top() + 2.0),
        Vec2::new(w, 11.0),
    );
    ui.painter()
        .rect_filled(b, 2.0, Color32::from_rgb(68, 68, 68));
    ui.painter().text(
        b.center(),
        Align2::CENTER_CENTER,
        key.to_string(),
        FontId::proportional(8.5),
        Color32::WHITE,
    );
}

fn button(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, toggle: bool) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    let label = st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
    let tip = st.tip.clone().unwrap_or_else(|| tip_of(c));
    let tip_a11y = tip.clone();
    let icon = c.image.as_deref().and_then(icon_of);
    let on = toggle && st.checked;
    let enabled = on_enabled(c, &st);

    let resp = ui.add_enabled_ui(enabled, |ui| {
        if matches!(c.id.as_str(), "Bold" | "Italic" | "Underline") {
            return ui_kit::text_toggle(
                ui,
                if c.id == "Bold" {
                    "B"
                } else if c.id == "Italic" {
                    "I"
                } else {
                    "U"
                },
                on,
                &tip,
                c.id == "Bold",
                c.id == "Italic",
                c.id == "Underline",
            );
        }
        if matches!(c.id.as_str(), "FillColor" | "FontColor") {
            let col = st.swatch.unwrap_or(Color32::from_rgb(128, 128, 128));
            return ui_kit::color_button(
                ui,
                if c.id == "FontColor" { Some("A") } else { None },
                if c.id == "FontColor" { None } else { icon },
                col,
                &tip,
            );
        }
        match c.size {
            Size::Large => {
                let w = c.width.unwrap_or(42.0);
                let ic = icon.unwrap_or(Icon::Grid);
                let r = ui_kit::big_sized(ui, ic, &label, Vec2::new(w, 76.0));
                if !tip.is_empty() {
                    r.on_hover_text(tip)
                } else {
                    r
                }
            }
            Size::Normal => {
                if label.is_empty() {
                    ui_kit::small_on(ui, icon.unwrap_or(Icon::Grid), on, &tip)
                } else {
                    let w = c.width.unwrap_or_else(|| {
                        (16.0 + 4.0 + measure(ui, &label, 12.0) + 10.0).max(24.0)
                    });
                    ui_kit::labeled_w_on(ui, icon.unwrap_or(Icon::Grid), &label, w, on, &tip)
                }
            }
        }
    });
    // UI-24.2：功能区按钮在 UIA 里要有名字/角色/禁用状态
    {
        let name = if tip_a11y.is_empty() { label.clone() } else { tip_a11y };
        ui_kit::a11y_name(
            &resp.inner,
            &name,
            enabled,
            if toggle { Some(on) } else { None },
        );
    }
    if app.keytips && app.keytip_tab.is_some() && enabled {
        if let Some(k) = &c.keytip {
            if app.keytip_prefix.is_empty() || k.starts_with(&app.keytip_prefix) {
                draw_keytip(ui, k, resp.inner.rect);
            }
        }
    }
    if resp.inner.double_clicked() && c.action() == "FormatPainter" {
        app.painter_double = true;
    }
    if resp.inner.clicked() {
        let action = c.action().to_string();
        app.ribbon_action(&action, ui.ctx());
    }
}

fn split(app: &mut SheetApp, ui: &mut egui::Ui, ctl: &Ctl, menu_def: &Menu) {
    let st = app.ctl_state(&ctl.id);
    if !st.visible {
        return;
    }
    let label = st.label.clone().or_else(|| ctl.label.clone()).unwrap_or_default();
    let tip = st.tip.clone().unwrap_or_else(|| tip_of(ctl));
    let icon = ctl.image.as_deref().and_then(icon_of).unwrap_or(Icon::Grid);
    let on = st.checked;
    let enabled = on_enabled(ctl, &st);
    let mut open = false;
    let mut pos = egui::pos2(0.0, 0.0);
    let mut keytip_rect: Option<Rect> = None;
    let main_clicked;

    match ctl.size {
        Size::Large => {
            let w = ctl.width.unwrap_or(42.0);
            let (r, ar) = ui_kit::big_sized_split(ui, icon, &label, Vec2::new(w, 76.0));
            // UI-24.2：大号拆分按钮的主体和箭头是两个点击区，各自要有名字
            if !label.is_empty() {
                let mut main = r.rect;
                main.max.y = main.max.y.min(ar.rect.top());
                ui_kit::a11y_name_rect(&r, &label, enabled, main, ui.ctx());
            }
            ui_kit::a11y_name(&ar, &format!("{label} 选项"), enabled, None);
            keytip_rect = Some(r.rect);
            main_clicked = enabled && r.clicked();
            if ar.clicked() && enabled {
                open = true;
                pos = egui::pos2(ar.rect.left(), ar.rect.bottom());
            }
        }
        Size::Normal => {
            let aw = ctl.arrow.unwrap_or(15.0);
            let (mr, ar) = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let main = ui.add_enabled_ui(enabled, |ui| {
                        match ctl.id.as_str() {
                            "Bold" => ui_kit::text_toggle(ui, "B", on, &tip, true, false, false),
                            "Italic" => ui_kit::text_toggle(ui, "I", on, &tip, false, true, false),
                            "Underline" => {
                                ui_kit::text_toggle(ui, "U", on, &tip, false, false, true)
                            }
                            "FillColor" | "FontColor" => {
                                let col = st.swatch.unwrap_or(Color32::from_rgb(128, 128, 128));
                                ui_kit::color_button(
                                    ui,
                                    if ctl.id == "FontColor" { Some("A") } else { None },
                                    icon_of(if ctl.id == "FontColor" { "FontColor" } else { "Fill" }),
                                    col,
                                    &tip,
                                )
                            }
                            _ if label.is_empty() => ui_kit::small_on(ui, icon, on, &tip),
                            _ => {
                                let w = ctl.width.unwrap_or_else(|| {
                                    (16.0 + 4.0 + measure(ui, &label, 12.0) + 10.0).max(24.0)
                                });
                                ui_kit::labeled_w_on(ui, icon, &label, w, on, &tip)
                            }
                        }
                    });
                    let ar = ui
                        .scope(|ui| {
                            if aw <= 18.0 {
                                ui.spacing_mut().button_padding.x = 2.0;
                            }
                            ui_kit::arrow_glyph_button(
                                ui,
                                Vec2::new(aw, 24.0),
                                Color32::from_rgb(68, 68, 68),
                            )
                        })
                        .inner;
                    (main.inner, ar)
                })
                .inner;
            keytip_rect = Some(mr.rect);
            main_clicked = mr.clicked();
            if ar.clicked() && enabled {
                open = true;
                pos = ar.rect.left_bottom();
            }
        }
    }

    if app.keytips && app.keytip_tab.is_some() && enabled {
        if let (Some(k), Some(r)) = (&ctl.keytip, keytip_rect) {
            if app.keytip_prefix.is_empty() || k.starts_with(&app.keytip_prefix) {
                draw_keytip(ui, k, r);
            }
        }
    }
    if main_clicked {
        let action = ctl.action().to_string();
        app.ribbon_action(&action, ui.ctx());
    }
    if open {
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Menu(menu_def.clone()),
            pos,
            opened_at: ui.input(|i| i.time),
        });
    }
}

fn menu(app: &mut SheetApp, ui: &mut egui::Ui, m: &Menu) {
    let st = app.ctl_state(&m.ctl.id);
    if !st.visible {
        return;
    }
    let label = st.label.clone().or_else(|| m.ctl.label.clone()).unwrap_or_default();
    let tip = tip_of(&m.ctl);
    let icon = m.ctl.image.as_deref().and_then(icon_of);
    if m.ctl.size == Size::Large {
        let w = m.ctl.width.unwrap_or(65.0);
        let ic = m.ctl.image.as_deref().and_then(icon_of).unwrap_or(Icon::Grid);
        let (r, ar) = ui_kit::big_sized_split(ui, ic, &label, Vec2::new(w, 76.0));
        {
            let mut main = r.rect;
            main.max.y = main.max.y.min(ar.rect.top());
            ui_kit::a11y_name_rect(&r, &label, on_enabled(&m.ctl, &st), main, ui.ctx());
        }
        ui_kit::a11y_name(&ar, &format!("{label} 选项"), on_enabled(&m.ctl, &st), None);
        if (r.clicked() || ar.clicked()) && on_enabled(&m.ctl, &st) {
            app.ribbon_popup = Some(Popup {
                kind: PopupKind::Menu(m.clone()),
                pos: egui::pos2(ar.rect.left(), ar.rect.bottom()),
                opened_at: ui.input(|i| i.time),
            });
        }
        return;
    }
    let clicked = ui.add_enabled_ui(on_enabled(&m.ctl, &st), |ui| {
        let w = m.ctl.width.unwrap_or_else(|| {
            if icon.is_some() {
                (16.0 + 4.0 + measure(ui, &label, 12.0) + 16.0).max(24.0)
            } else {
                (measure(ui, &label, 12.0) + 26.0).max(24.0)
            }
        });
        if let Some(ic) = icon {
            ui_kit::labeled_w(ui, ic, &label, w, &tip)
        } else {
            ui.add(
                egui::Button::new(egui::RichText::new(&label).size(12.0))
                    .min_size(Vec2::new(w, 24.0)),
            )
        }
    });
    if clicked.inner.clicked() {
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Menu(m.clone()),
            pos: clicked.inner.rect.left_bottom(),
            opened_at: ui.input(|i| i.time),
        });
    }
}

fn dropdown(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, items: &[Item]) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    // Excel 的数字格式是「可输入的下拉」，原来用 egui ComboBox 调过对齐，别再自绘
    if c.id == "NumberFormat" {
        let cur = st
            .text
            .clone()
            .or_else(|| items.first().map(|i| i.label.clone()))
            .unwrap_or_default();
        let mut picked: Option<String> = None;
        ui.add_enabled_ui(st.enabled, |ui| {
            ui.spacing_mut().interact_size.y = 21.0;
            egui::ComboBox::from_id_salt("numfmt")
                .selected_text(cur)
                .width(c.width.unwrap_or(96.0))
                .height(24.0)
                .show_ui(ui, |ui| {
                    for it in items {
                        if ui.selectable_label(false, &it.label).clicked() {
                            picked = Some(it.id.clone());
                        }
                    }
                });
        });
        if let Some(p) = picked {
            app.ribbon_action(&p, ui.ctx());
        }
        return;
    }
    let text = st
        .text
        .clone()
        .or_else(|| c.label.clone())
        .or_else(|| items.first().map(|i| i.label.clone()))
        .unwrap_or_default();
    let tip = tip_of(c);
    let w = c
        .label
        .as_ref()
        .map(|_| 0.0)
        .unwrap_or(0.0);
    let _ = w;
    let clicked = ui.add_enabled_ui(on_enabled(c, &st), |ui| {
        let width = c
            .width
            .unwrap_or_else(|| 96.0_f32.max(measure(ui, &text, 12.0) + 30.0));
        let (rect, r) = ui.allocate_exact_size(Vec2::new(width, 21.0), Sense::click());
        let enabled = ui.is_enabled();
        let hover = r.hovered() && enabled;
        ui.painter().rect(
            rect,
            2.0,
            if enabled { ui.visuals().extreme_bg_color } else { ui_kit::HOVER },
            egui::Stroke::new(
                1.0,
                if enabled { ui_kit::BORDER } else { Color32::from_rgb(224, 224, 224) },
            ),
            egui::StrokeKind::Inside,
        );
        if hover {
            ui.painter()
                .rect_filled(rect.shrink(1.0), 1.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        ui.painter().text(
            egui::pos2(rect.left() + 5.0, rect.center().y),
            Align2::LEFT_CENTER,
            &text,
            FontId::proportional(12.0),
            if enabled { ui.visuals().text_color() } else { ui_kit::DISABLED },
        );
        ui_kit::caret(
            ui,
            egui::pos2(rect.right() - 8.0, rect.center().y),
            if enabled { Color32::from_rgb(68, 68, 68) } else { ui_kit::DISABLED },
        );
        r.on_hover_text(tip.clone())
    });
    // UI-24.2：自绘下拉在 UIA 里要有名字（当前值 + 说明）
    {
        let name = if tip.is_empty() {
            format!("{text}（下拉）")
        } else {
            format!("{text}，{tip}")
        };
        ui_kit::a11y_name(&clicked.inner, &name, on_enabled(c, &st), None);
    }
    if clicked.inner.clicked() {
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Items(items.to_vec()),
            pos: clicked.inner.rect.left_bottom(),
            opened_at: ui.input(|i| i.time),
        });
    }
}

fn combo(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, items: &[Item], width: f32) {
    // 字体、字号是 Excel 的「可编辑组合框」，外观单独做
    match c.id.as_str() {
        "Font" => return font_combo(app, ui, width),
        "FontSize" => return size_combo(app, ui, width),
        _ => {}
    }
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    let cur = st
        .text
        .clone()
        .or_else(|| items.first().map(|i| i.label.clone()))
        .unwrap_or_default();
    let mut picked: Option<String> = None;
    ui.add_enabled_ui(on_enabled(c, &st), |ui| {
        egui::ComboBox::from_id_salt(&c.id)
            .selected_text(cur)
            .width(width)
            .show_ui(ui, |ui| {
                for it in items {
                    if ui.selectable_label(false, &it.label).clicked() {
                        picked = Some(it.id.clone());
                    }
                }
            });
    });
    if let Some(p) = picked {
        app.ribbon_action(&p, ui.ctx());
    }
}

fn edit(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    c: &Ctl,
    hint: &str,
    password: bool,
    width: f32,
) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    ui.add_enabled_ui(on_enabled(c, &st), |ui| {
        ui.spacing_mut().interact_size.y = 21.0;
        let buf: &mut String = match c.id.as_str() {
            "JoinAddress" => &mut app.join_addr,
            "RoomPassword" => &mut app.room_password,
            _ => return,
        };
        ui.add(
            egui::TextEdit::singleline(buf)
                .hint_text(hint)
                .desired_width(width)
                .password(password),
        )
        .on_hover_text(tip_of(c));
    });
}

fn label(app: &mut SheetApp, ui: &mut egui::Ui, l: &LabelCtl) {
    let st = app.ctl_state(&l.id);
    if !st.visible {
        return;
    }
    let text = st.text.clone().or_else(|| l.label.clone()).unwrap_or_default();
    let weak = l.style.as_deref() == Some("weak");
    let mono = l.style.as_deref() == Some("mono");

    if l.style.as_deref() == Some("chip") {
        // 表格样式缩略：五段主题绿，和 Excel 的表样式预览同一路数
        ui.vertical(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(120.0, 22.0), Sense::hover());
            let painter = ui.painter();
            let mut x = r.left();
            for col in [
                Color32::from_rgb(21, 96, 58),
                Color32::from_rgb(33, 122, 76),
                Color32::from_rgb(59, 156, 104),
                Color32::from_rgb(160, 212, 183),
                Color32::from_rgb(226, 240, 231),
            ] {
                painter.rect_filled(
                    Rect::from_min_size(egui::pos2(x, r.top()), Vec2::new(24.0, 22.0)),
                    0.0,
                    col,
                );
                x += 24.0;
            }
            ui.label(egui::RichText::new(&text).size(l.size).color(ui_kit::TEXT_WEAK));
        });
        return;
    }

    if let Some(w) = l.width {
        // 页面布局那三行「宽度:/高度:/缩放:」是定宽标签 + 定宽下拉
        ui.add_sized(
            [w, 21.0],
            egui::Label::new(egui::RichText::new(&text).size(l.size)),
        );
        return;
    }

    let mut t = egui::RichText::new(&text).size(l.size);
    if mono {
        t = t.monospace().color(ui_kit::TEXT);
    }
    if weak {
        t = t.weak();
    }
    ui.label(t);
}

// ---------------------------------------------------------------- 弹出层

fn popup(app: &mut SheetApp, ui: &mut egui::Ui) {
    let Some(p) = app.ribbon_popup.clone() else {
        return;
    };
    let ctx = ui.ctx().clone();
    let mut close = false;
    let mut fire: Option<String> = None;
    let area = egui::Area::new(egui::Id::new("ribbon_popup"))
        .fixed_pos(p.pos)
        .order(egui::Order::Foreground)
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(180.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                match &p.kind {
                    PopupKind::Menu(m) => {
                        // UI-11.4：「最近使用的函数」按真实调用记录生成，不再用 XML 里写死的五项
                        if m.ctl.id == "RecentFnMenu" {
                            for name in crate::fnrecent::menu_items() {
                                let r = ui.add(
                                    egui::Button::new(egui::RichText::new(&name).size(12.0))
                                        .min_size(Vec2::new(160.0, 22.0))
                                        .frame(false),
                                );
                                if r.clicked() {
                                    fire = Some(format!("InsertFn:{name}"));
                                    close = true;
                                }
                            }
                        }
                        for it in m.items.iter().filter(|_| m.ctl.id != "RecentFnMenu") {
                            match it {
                                MenuNode::Separator => {
                                    ui.separator();
                                }
                                MenuNode::Button(c) => {
                                    let st = app.ctl_state(&c.id);
                                    if !st.visible {
                                        continue;
                                    }
                                    let label =
                                        st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
                                    let btn =
                                        egui::Button::new(egui::RichText::new(&label).size(12.0))
                                            .min_size(Vec2::new(160.0, 22.0))
                                            .frame(false);
                                    let r = ui.add_enabled(st.enabled, btn);
                                    if r.clicked() {
                                        fire = Some(c.action().to_string());
                                        close = true;
                                    }
                                }
                                MenuNode::Toggle(c) => {
                                    let st = app.ctl_state(&c.id);
                                    if !st.visible {
                                        continue;
                                    }
                                    let label =
                                        st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
                                    let r = ui_kit::check_row(ui, &label, st.checked, st.enabled, 200.0);
                                    if r.clicked() {
                                        fire = Some(c.action().to_string());
                                        close = true;
                                    }
                                }
                                MenuNode::Menu(sub) => {
                                    let label = sub.ctl.label.clone().unwrap_or_default();
                                    let r = ui.add(
                                        egui::Button::new(egui::RichText::new(label).size(12.0))
                                            .min_size(Vec2::new(160.0, 22.0))
                                            .frame(false),
                                    );
                                    ui_kit::caret_right(
                                        ui,
                                        egui::pos2(r.rect.right() - 9.0, r.rect.center().y),
                                        Color32::from_rgb(68, 68, 68),
                                    );
                                    if r.clicked() {
                                        app.ribbon_popup = Some(Popup {
                                            kind: PopupKind::Menu(sub.clone()),
                                            pos: egui::pos2(r.rect.right(), r.rect.top()),
                                            opened_at: ui.input(|i| i.time),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    PopupKind::Items(items) => {
                        for it in items {
                            let r = ui.add(
                                egui::Button::new(egui::RichText::new(&it.label).size(12.0))
                                    .min_size(Vec2::new(160.0, 22.0))
                                    .frame(false),
                            );
                            if r.clicked() {
                                fire = Some(it.id.clone());
                                close = true;
                            }
                        }
                    }
                }
            });
        });
    let rect = area.response.rect;
    if let Some(a) = fire {
        app.ribbon_action(&a, &ctx);
    }
    let now = ctx.input(|i| i.time);
    let fresh = now - p.opened_at < 0.08;
    if close
        || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        || (!fresh
            && ctx.input(|i| i.pointer.any_click())
            && !ctx
                .input(|i| i.pointer.interact_pos())
                .map(|p| rect.contains(p))
                .unwrap_or(false))
    {
        app.ribbon_popup = None;
    }
}

// ---------------------------------------------------------------- 字体/字号框

fn font_combo(app: &mut SheetApp, ui: &mut egui::Ui, width: f32) {
    let cur = app.current_style();
    ui.spacing_mut().interact_size.y = 21.0;
    let fid = egui::Id::new("font_edit");
    // UI-24.2：框里要显示真实的字体名（默认字体时以前是空串，读屏读不到）
    let shown_font = if cur.font.is_empty() {
        "思源黑体".to_string()
    } else {
        cur.font.clone()
    };
    if !ui.memory(|m| m.has_focus(fid)) {
        app.font_buf = shown_font;
    }
    let mut lost = false;
    let mut arrow_pos = egui::pos2(0.0, 0.0);
    let mut arrow_clicked = false;
    {
        let (crect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
        ui.painter().rect(
            crect,
            2.0,
            Color32::WHITE,
            egui::Stroke::new(1.0, ui_kit::BORDER),
            egui::StrokeKind::Inside,
        );
        let arrow_rect = Rect::from_min_max(
            egui::pos2(crect.right() - 16.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 1.0, crect.bottom() - 1.0),
        );
        let text_rect = Rect::from_min_max(
            egui::pos2(crect.left() + 5.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 18.0, crect.bottom() - 1.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let focused = ui.memory(|m| m.has_focus(fid));
        let fr = child
            .add_sized(
                Vec2::new(text_rect.width(), 20.0),
                egui::TextEdit::singleline(&mut app.font_buf)
                    .id(fid)
                    .frame(false)
                    // 空字体时不再用灰色 hint（Excel 框里永远是具体的字体名）
                    .hint_text(""),
            )
            .on_hover_text("字体名，回车应用");
        if !focused && app.font_buf.is_empty() {
            ui.painter().text(
                egui::pos2(text_rect.left(), text_rect.center().y),
                egui::Align2::LEFT_CENTER,
                "思源黑体",
                egui::FontId::proportional(12.0),
                ui_kit::TEXT,
            );
        }
        lost = fr.lost_focus();
        // UI-24.2：输入框在 UIA 里要有名字，否则读屏只念「编辑 思源黑体」
        let _ = ui.ctx().accesskit_node_builder(fid, |node| {
            node.set_label("字体".to_string());
        });
        let ar = ui.interact(arrow_rect, egui::Id::new("font_arrow"), Sense::click());
        ui_kit::a11y_name(&ar, "字体 下拉", true, None);
        arrow_pos = egui::pos2(crect.left(), crect.bottom());
        let c = arrow_rect.center();
        ui_kit::caret(ui, c, Color32::from_rgb(68, 68, 68));
        if ar.clicked() {
            arrow_clicked = true;
        }
    }
    if arrow_clicked {
        app.font_menu = !app.font_menu;
        app.size_menu = false;
    }
    if lost && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let pick = app.font_buf.trim().to_string();
        app.apply_style(move |s| s.font = pick.clone());
    }
    if app.font_menu {
        let ctx = ui.ctx().clone();
        let mut pick: Option<String> = None;
        let mut close = false;
        egui::Area::new(egui::Id::new("font_list"))
            .fixed_pos(arrow_pos)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(170.0);
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for name in sheetcore::style::FONT_CHOICES {
                                let label = if name.is_empty() { "默认" } else { *name };
                                let preview = egui::RichText::new(label);
                                let preview = match sheetcore::style::font_class(name) {
                                    sheetcore::style::FontClass::Serif => preview
                                        .family(egui::FontFamily::Name("cjk-serif".into())),
                                    sheetcore::style::FontClass::Mono => {
                                        preview.family(egui::FontFamily::Name("mono".into()))
                                    }
                                    _ => preview,
                                };
                                if ui.selectable_label(cur.font == *name, preview).clicked() {
                                    pick = Some(name.to_string());
                                    close = true;
                                }
                            }
                        });
                });
            });
        if let Some(p) = pick {
            app.apply_style(move |s| s.font = p.clone());
        }
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.font_menu = false;
        }
    }
}

fn size_combo(app: &mut SheetApp, ui: &mut egui::Ui, width: f32) {
    let cur = app.current_style();
    ui.spacing_mut().interact_size.y = 21.0;
    let sid = egui::Id::new("size_edit");
    if !ui.memory(|m| m.has_focus(sid)) {
        app.size_buf = format!("{:.0}", cur.size);
    }
    let mut lost = false;
    let mut arrow_pos = egui::pos2(0.0, 0.0);
    let mut arrow_clicked = false;
    {
        let (crect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
        ui.painter().rect(
            crect,
            2.0,
            Color32::WHITE,
            egui::Stroke::new(1.0, ui_kit::BORDER),
            egui::StrokeKind::Inside,
        );
        let arrow_rect = Rect::from_min_max(
            egui::pos2(crect.right() - 16.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 1.0, crect.bottom() - 1.0),
        );
        let text_rect = Rect::from_min_max(
            egui::pos2(crect.left() + 5.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 18.0, crect.bottom() - 1.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let sr = child
            .add_sized(
                Vec2::new(text_rect.width(), 20.0),
                egui::TextEdit::singleline(&mut app.size_buf)
                    .id(sid)
                    .frame(false),
            )
            .on_hover_text("字号（磅），回车应用");
        lost = sr.lost_focus();
        let _ = ui.ctx().accesskit_node_builder(sid, |node| {
            node.set_label("字号".to_string());
        });
        let ar = ui.interact(arrow_rect, egui::Id::new("size_arrow"), Sense::click());
        ui_kit::a11y_name(&ar, "字号 下拉", true, None);
        arrow_pos = egui::pos2(crect.left(), crect.bottom());
        let c = arrow_rect.center();
        ui_kit::caret(ui, c, Color32::from_rgb(68, 68, 68));
        if ar.clicked() {
            arrow_clicked = true;
        }
    }
    if arrow_clicked {
        app.size_menu = !app.size_menu;
        app.font_menu = false;
    }
    if lost && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        if let Ok(v) = app.size_buf.trim().parse::<f32>() {
            let v = v.clamp(6.0, 72.0);
            app.apply_style(move |s| s.size = v);
        }
    }
    if app.size_menu {
        let ctx = ui.ctx().clone();
        let mut pick: Option<f32> = None;
        let mut close = false;
        egui::Area::new(egui::Id::new("size_list"))
            .fixed_pos(arrow_pos)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(90.0);
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for size in [
                                9.0f32, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0, 28.0,
                                36.0, 48.0, 72.0,
                            ] {
                                if ui
                                    .selectable_label(
                                        (cur.size - size).abs() < 0.01,
                                        format!("{size:.0}"),
                                    )
                                    .clicked()
                                {
                                    pick = Some(size);
                                    close = true;
                                }
                            }
                        });
                });
            });
        if let Some(p) = pick {
            app.apply_style(move |s| s.size = p);
        }
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.size_menu = false;
        }
    }
}

fn slider(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, min: f32, max: f32) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    let mut z = app.zoom;
    ui.spacing_mut().slider_width = c.width.unwrap_or(120.0);
    let r = ui.add_enabled(
        on_enabled(c, &st),
        egui::Slider::new(&mut z, min..=max).show_value(false),
    );
    if r.changed() {
        app.zoom = z.clamp(min, max);
        ui.ctx().set_zoom_factor(app.zoom);
    }
}

fn gallery(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, items: &[Item]) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    let w = c.width.unwrap_or(349.0);
    let enabled = on_enabled(c, &st);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 65.0), Sense::hover());
    let arrow_w = 24.0;
    let area = Rect::from_min_max(rect.min, egui::pos2(rect.right() - arrow_w, rect.bottom()));
    let n = items.len().max(1) as f32;
    let tw = area.width() / n;
    let mut fire: Option<String> = None;
    for (i, it) in items.iter().enumerate() {
        let r = Rect::from_min_size(
            egui::pos2(area.left() + i as f32 * tw + 1.0, area.top() + 1.0),
            Vec2::new(tw - 2.0, 63.0),
        );
        let resp = ui.interact(r, ui.id().with(("gal", &c.id, i)), Sense::click());
        ui_kit::a11y_name(&resp, &it.label, enabled, None);
        let (bg, fg, bold) = style_preview(&it.id);
        let bg = if resp.hovered() { bg.gamma_multiply(0.94) } else { bg };
        ui.painter().rect(
            r,
            2.0,
            bg,
            egui::Stroke::new(1.0, ui_kit::BORDER),
            egui::StrokeKind::Inside,
        );
        // 像 Excel 的样式缩略图：一块样例底纹 + Aa + 两条模拟网格线
        if bg != Color32::WHITE {
            let band = Rect::from_min_max(r.min, egui::pos2(r.right(), r.top() + 6.0));
            ui.painter().rect_filled(band, 0.0, bg.gamma_multiply(0.85));
        }
        let fs = if bold { 13.0 } else { 12.0 };
        let pos = egui::pos2(r.center().x, r.top() + 22.0);
        ui.painter().text(
            pos,
            Align2::CENTER_CENTER,
            "Aa",
            FontId::proportional(fs),
            fg,
        );
        if bold {
            ui.painter().text(
                pos + Vec2::new(0.5, 0.0),
                Align2::CENTER_CENTER,
                "Aa",
                FontId::proportional(fs),
                fg,
            );
        }
        let line = egui::Stroke::new(1.0, ui_kit::BORDER);
        for k in 0..2 {
            let y = r.top() + 40.0 + k as f32 * 9.0;
            ui.painter().line_segment(
                [egui::pos2(r.left() + 6.0, y), egui::pos2(r.right() - 6.0, y)],
                line,
            );
        }
        ui.painter().text(
            egui::pos2(r.center().x, r.bottom() - 8.0),
            Align2::CENTER_CENTER,
            it.label.clone(),
            FontId::proportional(9.0),
            ui_kit::TEXT_WEAK,
        );
        if resp.clicked() && enabled {
            fire = Some(it.id.clone());
        }
    }
    let arrow_rect = Rect::from_min_max(egui::pos2(rect.right() - arrow_w, rect.top()), rect.max);
    let ar = ui.interact(arrow_rect, ui.id().with(("galarrow", &c.id)), Sense::click());
    {
        let base = c
            .label
            .clone()
            .or_else(|| st.tip.clone())
            .unwrap_or_else(|| tip_of(c));
        let base = if base.is_empty() { c.id.clone() } else { base };
        ui_kit::a11y_name(&ar, &format!("{base} 展开"), enabled, None);
    }
    if ar.hovered() {
        ui.painter().rect_filled(arrow_rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    ui_kit::caret(
        ui,
        egui::pos2(rect.right() - arrow_w / 2.0, rect.center().y),
        ui.visuals().text_color(),
    );
    if ar.clicked() && enabled {
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Items(items.to_vec()),
            pos: egui::pos2(rect.right() - arrow_w, rect.top()),
            opened_at: ui.input(|i| i.time),
        });
    }
    if let Some(a) = fire {
        app.ribbon_action(&a, ui.ctx());
    }
}

fn style_preview(id: &str) -> (Color32, Color32, bool) {
    match id {
        "StyleTitle" => (Color32::WHITE, Color32::from_rgb(31, 78, 121), true),
        "StyleHeading" => (
            Color32::from_rgb(217, 226, 243),
            Color32::from_rgb(31, 56, 100),
            true,
        ),
        "StyleGood" => (Color32::from_rgb(198, 239, 206), Color32::from_rgb(0, 97, 0), false),
        "StyleBad" => (Color32::from_rgb(255, 199, 206), Color32::from_rgb(156, 0, 6), false),
        _ => (Color32::WHITE, ui_kit::TEXT, false),
    }
}

pub fn icon_of(name: &str) -> Option<Icon> {
    Some(match name {
        "Open" => Icon::Open,
        "Save" => Icon::Save,
        "SaveAs" => Icon::SaveAs,
        "Print" => Icon::Print,
        "Undo" => Icon::Undo,
        "Redo" => Icon::Redo,
        "Paste" => Icon::Paste,
        "Cut" => Icon::Cut,
        "Copy" => Icon::Copy,
        "Brush" => Icon::Brush,
        "TextDirection" => Icon::TextDir,
        "HeaderFooter" => Icon::HeaderFooter,
        "GroupRows" => Icon::GroupRows,
        "UngroupRows" => Icon::UngroupRows,
        "ShowDetail" => Icon::ShowDetail,
        "HideDetail" => Icon::HideDetail,
        "Validation" => Icon::Validation,
        "SplitColumns" => Icon::SplitColumns,
        "RemoveDup" => Icon::RemoveDup,
        "Consolidate" => Icon::Consolidate,
        "ImportCsv" => Icon::ImportCsv,
        "ImportBook" => Icon::ImportBook,
        "WhatIf" => Icon::WhatIf,
        "OutlineLevels" => Icon::OutlineLevels,
        "GoalSeek" => Icon::GoalSeek,
        "Eye" => Icon::Eye,
        "EyeOff" => Icon::EyeOff,
        "PageLayout" => Icon::PageLayout,
        "PageBreak" => Icon::PageBreak,
        "NormalView" => Icon::NormalView,
        "ShowHeadings" => Icon::ShowHeadings,
        "FormulaBar" => Icon::FormulaBar,
        "TextSpill" => Icon::TextSpill,
        "ViewPane" => Icon::ViewPane,
        "FormulaView" => Icon::FormulaView,
        "PivotRefresh" => Icon::PivotRefresh,
        "FolderPlus" => Icon::FolderPlus,
        "AppDefault" => Icon::AppDefault,
        "AppAdd" => Icon::AppAdd,
        "PdfExport" => Icon::PdfExport,
        "HostTakeover" => Icon::HostTakeover,
        "UpdateCheck" => Icon::UpdateCheck,
        "FormatCells" => Icon::FormatCells,
        "TextVerticalDraw" => Icon::TextVerticalDraw,
        "TextBoxDraw" => Icon::TextBoxDraw,
        "CalcSheet" => Icon::CalcSheet,
        "SheetRef" => Icon::SheetRef,
        "FilterReapply" => Icon::FilterReapply,
        "SortDialog" => Icon::SortDialog,
        "ShowChanges" => Icon::ShowChanges,
        "SheetViewSwitch" => Icon::SheetViewSwitch,
        "Zoom100" => Icon::Zoom100,
        "ZoomDialog" => Icon::ZoomDialog,
        "ZoomIn" => Icon::ZoomIn,
        "Unfreeze" => Icon::Unfreeze,
        "TableToRange" => Icon::TableToRange,
        "TableFillCalculated" => Icon::Fill,
        "SparkEdit" => Icon::SparkEdit,
        "ChartEdit" => Icon::ChartEdit,
        "PivotOptions" => Icon::PivotOptions,
        "ChartList" => Icon::ChartList,
        "Spelling" => Icon::Spelling,
        "CommentPrev" => Icon::CommentPrev,
        "CommentNext" => Icon::CommentNext,
        "ShowComment" => Icon::ShowComment,
        "A11y" => Icon::A11y,
        "Online" => Icon::Online,
        "AutoShare" => Icon::AutoShare,
        "Help" => Icon::Help,
        "About" => Icon::About,
        "Orientation" => Icon::Orientation,
        "PaperSize" => Icon::PaperSize,
        "PrintGrid" => Icon::PrintGrid,
        "PrintHeadings" => Icon::PrintHeadings,
        "PrintArea" => Icon::PrintArea,
        "ErrorCheck" => Icon::ErrorCheck,
        "NameTag" => Icon::NameTag,
        "WatchPane" => Icon::WatchPane,
        "FreezeRow" => Icon::FreezeRow,
        "FreezeCol" => Icon::FreezeCol,
        "Pin" => Icon::Pin,
        "NewView" => Icon::NewView,
        "ZoomSelection" => Icon::ZoomSelection,
        "Scenario" => Icon::Scenario,
        "WorkbookLinks" => Icon::DocLink,
        "DocLink" => Icon::DocLink,
        "TextVertical" => Icon::TextDir,
        "Fill" => Icon::Fill,
        "FontColor" => Icon::FontColor,
        "AlignL" => Icon::AlignL,
        "AlignC" => Icon::AlignC,
        "AlignR" => Icon::AlignR,
        "Top" => Icon::Top,
        "Middle" => Icon::Middle,
        "Bottom" => Icon::Bottom,
        "Wrap" => Icon::Wrap,
        "Merge" => Icon::Merge,
        "Borders" => Icon::Border,
        "Currency" => Icon::Currency,
        "Percent" => Icon::Percent,
        "Comma" => Icon::Comma,
        "SortAsc" => Icon::SortAsc,
        "SortDesc" => Icon::SortDesc,
        "Filter" => Icon::Filter,
        "Find" => Icon::Find,
        "Chart" => Icon::Chart,
        "Image" => Icon::Image,
        "Table" => Icon::Table,
        "Comment" => Icon::Comment,
        "Pivot" => Icon::Pivot,
        "Link" => Icon::Link,
        "Revision" => Icon::Revision,
        "Zoom" => Icon::Zoom,
        "Calc" => Icon::Calc,
        "Sheet" => Icon::Sheet,
        "Share" => Icon::Share,
        "Fx" => Icon::Fx,
        "Sum" => Icon::Sum,
        "Grid" => Icon::Grid,
        "Close" => Icon::Close,
        "Plus" => Icon::Plus,
        "Refresh" => Icon::Refresh,
        "DecInc" => Icon::DecInc,
        "DecDec" => Icon::DecDec,
        "FontGrow" => Icon::FontGrow,
        "FontShrink" => Icon::FontShrink,
        "Freeze" => Icon::Freeze,
        "Lock" => Icon::Lock,
        "Cf" => Icon::Cf,
        "Eraser" => Icon::Eraser,
        "ClearFmt" => Icon::ClearFmt,
        "IndentInc" => Icon::IndentInc,
        "IndentDec" => Icon::IndentDec,
        "Symbol" => Icon::Symbol,
        "ZhS" => Icon::ZhS,
        "ZhT" => Icon::ZhT,
        "RichText" => Icon::RichText,
        "TraceIn" => Icon::TraceIn,
        "TraceOut" => Icon::TraceOut,
        "SparkLine" => Icon::SparkLine,
        "SparkColumn" => Icon::SparkColumn,
        "SparkLoss" => Icon::SparkLoss,
        "TextBox" => Icon::TextBox,
        _ => return None,
    })
}

#[cfg(test)]
mod compact_tests {
    use super::*;

    #[test]
    fn compact_alignment_group_has_a_bounded_grid() {
        let ribbon = crate::ribbon::load();
        let home = ribbon
            .tabs
            .iter()
            .find(|t| t.label == "开始")
            .expect("Home tab");
        let g = home
            .groups
            .iter()
            .find(|g| g.label == "对齐方式")
            .expect("alignment group");
        let n = compact_leaf_count(&g.children);
        assert!(n >= 8, "对齐方式应有足够多的命令，实际 {n}");
        let w = compact_width(g);
        assert!((72.0..=210.0).contains(&w), "紧凑组宽度越界: {w}");
        let mut leaves = Vec::new();
        compact_leaves(&g.children, &mut leaves);
        assert_eq!(leaves.len(), n, "扁平化后控件数必须一致");
    }
}
