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

/// 第 2 排「文件 ▾」绿色下拉胶囊按钮（独立渲染以接入自动保存与快速访问栏）。
pub fn tab_file(app: &mut SheetApp, ui: &mut egui::Ui) {
    let Some(t) = app.ribbon.tabs.first() else { return };
    let label = format!("{}   ", t.label);
    let keytip = t.keytip.clone();
    let resp = ui_kit::tab_ex(ui, &label, false, true);
    ui_kit::caret(
        ui,
        egui::pos2(resp.rect.right() - 11.0, resp.rect.center().y + 0.5),
        ui_kit::ACCENT,
    );
    if app.keytips && app.keytip_tab.is_none() {
        if let Some(ch) = keytip.chars().next() {
            let r = resp.rect;
            let b = Rect::from_center_size(egui::pos2(r.center().x, r.top() + 6.0), Vec2::new(13.0, 11.0));
            ui.painter().rect_filled(b, 2.0, Color32::from_rgb(68, 68, 68));
            ui.painter().text(b.center(), Align2::CENTER_CENTER, ch.to_string(), FontId::proportional(8.5), Color32::WHITE);
        }
    }
    if resp.clicked() {
        app.file_menu_anchor = Some(resp.rect.left_bottom());
        app.file_menu_open = !app.file_menu_open;
        app.ribbon_popup = None;
        app.ribbon_sub = None;
    }
}

/// 零堆分配借用保护：绘制期间暂移只读 `Ribbon` 到栈字段，利用结构体字段拆分借用消除每帧 `Tab::clone()`，
/// 并在作用域结束（含提前 `return`）时由 `Drop` 无条件归还至 `app.ribbon`。
struct RibbonGuard<'a> {
    app: &'a mut SheetApp,
    ribbon: super::Ribbon,
}

impl<'a> RibbonGuard<'a> {
    #[inline]
    fn new(app: &'a mut SheetApp) -> Self {
        let ribbon = std::mem::take(&mut app.ribbon);
        Self { app, ribbon }
    }
}

impl Drop for RibbonGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.app.ribbon = std::mem::take(&mut self.ribbon);
    }
}

/// 页签行（开始…帮助）。上下文页签由调用方按需追加。
pub fn tabs(app: &mut SheetApp, ui: &mut egui::Ui) {
    let guard = RibbonGuard::new(app);
    let ribbon = &guard.ribbon;
    let app = &mut *guard.app;
    for (i, t) in ribbon.tabs.iter().enumerate().skip(1) {
        let resp = ui_kit::tab_ex(ui, &t.label, app.ribbon_tab == i, false);
        if app.keytips && app.keytip_tab.is_none() {
            if let Some(ch) = t.keytip.chars().next() {
                let r = resp.rect;
                let b = Rect::from_center_size(
                    egui::pos2(r.center().x, r.top() + 6.0),
                    Vec2::new(13.0, 11.0),
                );
                ui.painter()
                    .rect_filled(b, 2.0, Color32::from_rgb(68, 68, 68));
                let mut buf = [0u8; 4];
                ui.painter().text(
                    b.center(),
                    Align2::CENTER_CENTER,
                    ch.encode_utf8(&mut buf),
                    FontId::proportional(8.5),
                    Color32::WHITE,
                );
            }
        }
        if resp.double_clicked() {
            app.ribbon_collapsed = !app.ribbon_collapsed;
            app.ribbon_popup = None;
            app.ribbon_sub = None;
        }
        if resp.clicked() {
            app.file_menu_open = false;
            app.ribbon_tab = i;
            app.ribbon_collapsed = false;
            app.ribbon_popup = None;
            app.ribbon_sub = None;
        }
    }
}

/// 上下文页签（表设计 / 图表工具）：选中表格或图表时才出现。
pub fn context_tabs(app: &mut SheetApp, ui: &mut egui::Ui) {
    let guard = RibbonGuard::new(app);
    let ribbon = &guard.ribbon;
    let app = &mut *guard.app;
    if app.table_at_sel().is_some() {
        if let Some(c) = ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetTableTools")
        {
            let r = ui_kit::tab_ex(ui, &c.tab.label, app.ribbon_tab == 100, false);
            if r.clicked() {
                app.ribbon_tab = 100;
                app.ribbon_collapsed = false;
                app.ribbon_popup = None;
            }
        }
    }
    if app.chart_at_sel().is_some() {
        if let Some(c) = ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetChartTools")
        {
            let r = ui_kit::tab_ex(ui, &c.tab.label, app.ribbon_tab == 101, false);
            if r.clicked() {
                app.ribbon_tab = 101;
                app.ribbon_collapsed = false;
                app.ribbon_popup = None;
            }
        }
    }
    // 选中带迷你图的单元格才出现「迷你图工具」
    if app.spark_at_sel().is_some() {
        if let Some(c) = ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetSparklineTools")
        {
            let r = ui_kit::tab_ex(ui, &c.tab.label, app.ribbon_tab == 102, false);
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
    let guard = RibbonGuard::new(app);
    let ribbon = &guard.ribbon;
    let app = &mut *guard.app;
    let tab = match app.ribbon_tab {
        100 => ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetTableTools")
            .map(|c| &c.tab),
        101 => ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetChartTools")
            .map(|c| &c.tab),
        102 => ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetSparklineTools")
            .map(|c| &c.tab),
        n if n < ribbon.tabs.len() => Some(&ribbon.tabs[n]),
        _ => None,
    };
    let tab = match tab {
        Some(t) => t,
        None => {
            app.ribbon_tab = 1;
            match ribbon.tabs.get(1) {
                Some(t) => t,
                None => return,
            }
        }
    };
    ui_kit::reset_groups();
    // UI-13.2：窄窗口把放不下的分组折叠成「组名 ▾」按钮，点开仍能执行全部命令。
    // 宽度取上一帧实测值；首帧先完整画一遍，第二帧起稳定。
    let avail = (ui.clip_rect().width() - 24.0).max(200.0);
    // WPS 两级响应式布局：
    // - >= 1720px：完整展开（含完整样式库、长中文标签）
    // - 1280..1720px（标准 1440px 屏）：折叠样式库、关键命令展示精简 2 字中文标签、保持全部 8 组可见
    // - < 1280px：纯图标紧凑模式
    let mid_tier = avail >= 1280.0 && avail < 1720.0;
    app.ribbon_compact = avail < 1280.0;
    RIBBON_MID.with(|c| c.set(mid_tier));
    ui_kit::set_group_pad(if mid_tier { 5.0 } else { 8.0 });
    let mut used = 0.0f32;
    let mut i = 0usize;
    while i < tab.groups.len() {
        let g = &tab.groups[i];
        // 完整/中档/紧凑三种布局的组宽分开缓存，避免切窗宽时复用错宽度
        let cache_key = if app.ribbon_compact {
            format!("{}#c", g.id)
        } else if mid_tier {
            format!("{}#m", g.id)
        } else {
            g.id.clone()
        };
        let known = app.ribbon_group_w.get(&cache_key).copied();
        let w = known.unwrap_or(160.0);
        let compact_w = compact_width(g);
        // UI-13.2 二级压缩：整组放不下时，先把大按钮收成小图标网格；
        // 小图标也放不下，才退化成「组名 ▾」/「更多 ▾」。
        // 「更多 ▾」的 60pt 只在后面还有组时才需要预留：最后一组放得下就照常画，
        // 否则悬浮卡片收窄 28px 后，1440 宽下 58px 的「加载项」会被白白挤进「更多」。
        let reserve = if i + 1 < tab.groups.len() { 60.0 } else { 0.0 };
        let full_fits = known.is_some() && used + w + reserve <= avail;
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
        if known.is_some() && used + w + reserve > avail && used > 0.0 {
            if avail - used >= 92.0 {
                menu_chevron(ui, &g.label, |ui| {
                    nodes(app, ui, &g.children);
                });
                used += 84.0;
                i += 1;
                continue;
            }
            // 连「组名 ▾」都放不下：剩下的组统一进「更多」。
            // 极端窄（首组就超宽）时把「更多」钉在裁切区右侧，别让它画到窗口外。
            let mut more = |ui: &mut egui::Ui| {
                menu_chevron(ui, "更多", |ui| {
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
        app.ribbon_group_w.insert(cache_key, w);
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
            let hit = ui_kit::group_l(ui, &g.label, &g.id, |ui| nodes_auto(app, ui, &g.children));
            if hit {
                let action = l.action().to_string();
                app.ribbon_action(&action, ui.ctx());
            }
        }
        None => {
            ui_kit::group(ui, &g.label, |ui| nodes_auto(app, ui, &g.children));
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

thread_local! {
    static RIBBON_MID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn mid_short_label(raw: &str) -> Option<&'static str> {
    Some(match raw {
        "自动换行" => "换行",
        "合并后居中" => "合并",
        "自动求和" => "求和",
        "条件格式" => "条件",
        "套用表格格式" => "表样",
        "排序和筛选" => "筛选",
        "查找和选择" => "查找",
        "冻结窗格" => "冻结",
        "格式刷" => "格式刷",
        _ => return None,
    })
}

/// 功能区组专用：支持 full / mid(1280..1720 精简中文标签) / narrow(<1280 纯图标)
fn nodes_auto(app: &mut SheetApp, ui: &mut egui::Ui, list: &[Node]) {
    let mid = !app.ribbon_compact && RIBBON_MID.with(|c| c.get());
    for n in list {
        if app.ribbon_compact {
            node_narrow(app, ui, n);
        } else if mid {
            node_mid(app, ui, n);
        } else {
            node(app, ui, n);
        }
    }
}

fn node_mid(app: &mut SheetApp, ui: &mut egui::Ui, n: &Node) {
    let shorten_ctl = |c: &Ctl| -> (Ctl, Option<String>) {
        let mut c2 = c.clone();
        let mut full = None;
        if let Some(lbl) = c2.label.as_deref() {
            if let Some(short) = mid_short_label(lbl) {
                full = Some(lbl.to_string());
                if c2.screentip.is_none() {
                    c2.screentip = Some(lbl.to_string());
                }
                c2.label = Some(short.to_string());
                c2.width = None;
            }
        }
        (c2, full)
    };
    match n {
        Node::Button(c) => {
            let (mut c2, full) = shorten_ctl(c);
            if matches!(c2.id.as_str(), "TextDirection") {
                c2.width = None;
            }
            button_a11y(app, ui, &c2, false, full.as_deref());
        }
        Node::Toggle(c) => {
            let (c2, full) = shorten_ctl(c);
            button_a11y(app, ui, &c2, true, full.as_deref());
        }
        Node::Split { ctl, menu: m } => {
            let (mut c2, full) = shorten_ctl(ctl);
            if ctl.id == "AutoSum" {
                c2.width = None;
                c2.arrow = Some(15.0);
            }
            split_a11y(app, ui, &c2, m, full.as_deref());
        }
        Node::Menu(m) => {
            let (mut c2, full) = shorten_ctl(&m.ctl);
            if matches!(c2.id.as_str(), "FillMenu" | "ClearMenu" | "TextDirection") {
                c2.width = None;
            }
            let mut m2 = m.clone();
            m2.ctl = c2;
            menu_a11y(app, ui, &m2, full.as_deref());
        }
        Node::Gallery { ctl, items, .. } => gallery_compact(app, ui, ctl, items),
        Node::ComboBox { ctl, items, width } => {
            let w2 = if ctl.id == "Font" {
                (*width).min(106.0)
            } else if ctl.id == "FontSize" {
                (*width).min(46.0)
            } else {
                *width
            };
            combo(app, ui, ctl, items, w2);
        }
        Node::DropDown { ctl, items } => {
            let mut c2 = ctl.clone();
            if c2.id == "NumberFormat" {
                c2.width = Some(106.0);
            }
            dropdown(app, ui, &c2, items);
        }
        Node::Spacer(w) => {
            ui.add_space((*w * 0.35).max(1.0));
        }
        Node::Box { vertical, top, gap, children } => {
            if *vertical {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = *gap;
                    if *top > 0.0 {
                        ui.add_space(*top);
                    }
                    nodes_auto(app, ui, children);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    nodes_auto(app, ui, children);
                });
            }
        }
        _ => node(app, ui, n),
    }
}

fn node_narrow(app: &mut SheetApp, ui: &mut egui::Ui, n: &Node) {
    match n {
        Node::Button(c) | Node::Toggle(c)
            if c.size != Size::Large && c.label.is_some() && c.image.is_some() =>
        {
            let mut c2 = c.clone();
            c2.label = None;
            c2.width = Some(24.0);
            let toggle = matches!(n, Node::Toggle(_));
            button(app, ui, &c2, toggle);
        }
        Node::Split { ctl, menu } if ctl.size != Size::Large && ctl.image.is_some() => {
            let mut c2 = ctl.clone();
            c2.label = None;
            c2.width = Some(24.0);
            c2.arrow = Some(15.0);
            split(app, ui, &c2, menu);
        }
        Node::Menu(m) if m.ctl.size != Size::Large && m.ctl.image.is_some() => {
            let mut m2 = m.clone();
            m2.ctl.label = None;
            m2.ctl.width = Some(24.0);
            menu(app, ui, &m2);
        }
        // 样式库整条 349px，1440 下必须收成「单元格样式 ▾」下拉
        Node::Gallery { ctl, items, .. } => gallery_compact(app, ui, ctl, items),
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
                    nodes_auto(app, ui, children);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    nodes_auto(app, ui, children);
                });
            }
        }
        _ => node(app, ui, n),
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
    button_a11y(app, ui, c, toggle, None);
}

fn button_a11y(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    c: &Ctl,
    toggle: bool,
    a11y_full: Option<&str>,
) {
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
                let tw = if label.is_empty() { 0.0 } else { measure(ui, &label, 12.0) };
                let w = c.width.unwrap_or(44.0).max(tw + 14.0);
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
                    let min_w = (16.0 + 5.0 + measure(ui, &label, 12.0) + 12.0).max(24.0);
                    let w = c.width.unwrap_or(min_w).max(min_w);
                    ui_kit::labeled_w_on(ui, icon.unwrap_or(Icon::Grid), &label, w, on, &tip)
                }
            }
        }
    });
    // UI-24.2：功能区按钮在 UIA 里要有名字/角色/禁用状态
    {
        let name = if tip_a11y.is_empty() {
            a11y_full.unwrap_or(&label).to_string()
        } else {
            tip_a11y
        };
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
    split_a11y(app, ui, ctl, menu_def, None);
}

fn split_a11y(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    ctl: &Ctl,
    menu_def: &Menu,
    a11y_full: Option<&str>,
) {
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
            let tw = if label.is_empty() { 0.0 } else { measure(ui, &label, 12.0) };
            let w = ctl.width.unwrap_or(44.0).max(tw + 14.0);
            let (r, ar) = ui_kit::big_sized_split(ui, icon, &label, Vec2::new(w, 76.0));
            // UI-24.2：大号拆分按钮的主体和箭头是两个点击区，各自要有名字
            let a11y_lbl = a11y_full.unwrap_or(&label);
            if !a11y_lbl.is_empty() {
                let mut main = r.rect;
                main.max.y = main.max.y.min(ar.rect.top());
                ui_kit::a11y_name_rect(&r, a11y_lbl, enabled, main, ui.ctx());
            }
            ui_kit::a11y_name(&ar, &format!("{a11y_lbl} 选项"), enabled, None);
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
                                let min_w =
                                    (16.0 + 5.0 + measure(ui, &label, 12.0) + 10.0).max(24.0);
                                let w = ctl.width.unwrap_or(min_w).max(min_w);
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
        app.ribbon_sub = None;
        if ctl.id == "FillColor" || ctl.id == "FontColor" {
            let want_font = ctl.id == "FontColor";
            app.color_popup_pos = Some(pos);
            app.color_popup = if app.color_popup == Some(want_font) { None } else { Some(want_font) };
            app.ribbon_popup = None;
        } else {
            app.ribbon_popup = Some(Popup {
                kind: PopupKind::Menu(menu_def.clone()),
                pos,
                opened_at: ui.input(|i| i.time),
            });
        }
    }
}

fn menu(app: &mut SheetApp, ui: &mut egui::Ui, m: &Menu) {
    menu_a11y(app, ui, m, None);
}

fn menu_a11y(app: &mut SheetApp, ui: &mut egui::Ui, m: &Menu, a11y_full: Option<&str>) {
    let st = app.ctl_state(&m.ctl.id);
    if !st.visible {
        return;
    }
    let label = st.label.clone().or_else(|| m.ctl.label.clone()).unwrap_or_default();
    let a11y_lbl = a11y_full.unwrap_or(&label);
    let tip = tip_of(&m.ctl);
    let icon = m.ctl.image.as_deref().and_then(icon_of);
    if m.ctl.size == Size::Large {
        let tw = if label.is_empty() { 0.0 } else { measure(ui, &label, 12.0) };
        let min_large = if a11y_full.is_some() { 46.0 } else { 62.0 };
        let w = m.ctl.width.unwrap_or(min_large).max(tw + 14.0);
        let ic = m.ctl.image.as_deref().and_then(icon_of).unwrap_or(Icon::Grid);
        let (r, ar) = ui_kit::big_sized_split(ui, ic, &label, Vec2::new(w, 76.0));
        {
            let mut main = r.rect;
            main.max.y = main.max.y.min(ar.rect.top());
            ui_kit::a11y_name_rect(&r, a11y_lbl, on_enabled(&m.ctl, &st), main, ui.ctx());
        }
        ui_kit::a11y_name(&ar, &format!("{a11y_lbl} 选项"), on_enabled(&m.ctl, &st), None);
        if (r.clicked() || ar.clicked()) && on_enabled(&m.ctl, &st) {
            app.ribbon_sub = None;
            app.ribbon_popup = Some(Popup {
                kind: PopupKind::Menu(m.clone()),
                pos: egui::pos2(ar.rect.left(), ar.rect.bottom()),
                opened_at: ui.input(|i| i.time),
            });
        }
        return;
    }
    let clicked = ui.add_enabled_ui(on_enabled(&m.ctl, &st), |ui| {
        let enabled = on_enabled(&m.ctl, &st);
        let min_w = if label.is_empty() {
            28.0
        } else if icon.is_some() {
            (16.0 + 5.0 + measure(ui, &label, 12.0) + 22.0).max(28.0)
        } else {
            (measure(ui, &label, 12.0) + 24.0).max(28.0)
        };
        let w = m.ctl.width.unwrap_or(min_w).max(min_w);
        if let Some(ic) = icon {
            let r = ui_kit::labeled_w(ui, ic, &label, w, &tip);
            ui_kit::caret(
                ui,
                egui::pos2(r.rect.right() - 7.0, r.rect.center().y + 0.5),
                if enabled { Color32::from_rgb(100, 116, 139) } else { ui_kit::DISABLED },
            );
            r
        } else {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::click());
            let bg = ui_kit::state_bg(
                enabled,
                false,
                resp.hovered(),
                resp.is_pointer_button_down_on(),
                ui.visuals().dark_mode,
            );
            if bg != Color32::TRANSPARENT {
                ui.painter().rect_filled(rect, 4.0, bg);
            }
            let fg = if enabled { ui_kit::TEXT } else { ui_kit::DISABLED };
            ui.painter().text(
                egui::pos2(rect.left() + 8.0, rect.center().y),
                Align2::LEFT_CENTER,
                &label,
                FontId::proportional(12.0),
                fg,
            );
            ui_kit::caret(
                ui,
                egui::pos2(rect.right() - 8.0, rect.center().y + 0.5),
                if enabled { Color32::from_rgb(100, 116, 139) } else { ui_kit::DISABLED },
            );
            ui_kit::a11y_name(&resp, if tip.is_empty() { &label } else { &tip }, enabled, None);
            resp.on_hover_text(&tip)
        }
    });
    if clicked.inner.clicked() {
        app.ribbon_sub = None;
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
    let text = st
        .text
        .clone()
        .or_else(|| c.label.clone())
        .or_else(|| items.first().map(|i| i.label.clone()))
        .unwrap_or_default();
    let tip = tip_of(c);
    let clicked = ui.add_enabled_ui(on_enabled(c, &st), |ui| {
        let width = c
            .width
            .unwrap_or_else(|| 96.0_f32.max(measure(ui, &text, 12.0) + 30.0));
        let (rect, r) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
        let enabled = ui.is_enabled();
        let hover = r.hovered() && enabled;
        ui.painter().rect(
            rect,
            5.0,
            if !enabled {
                ui_kit::HOVER
            } else if hover {
                Color32::from_rgb(244, 247, 251)
            } else {
                Color32::from_rgb(250, 251, 253)
            },
            egui::Stroke::new(
                1.0,
                if !enabled {
                    Color32::from_rgb(226, 232, 240)
                } else if hover {
                    Color32::from_rgb(176, 188, 204)
                } else {
                    ui_kit::CARD_BORDER
                },
            ),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 7.0, rect.center().y),
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
        app.ribbon_sub = None;
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
    let tip = tip_of(c);
    let enabled = on_enabled(c, &st);
    let clicked = ui.add_enabled_ui(enabled, |ui| {
        let w = c.width.unwrap_or(width).max(52.0);
        let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::click());
        let hover = r.hovered() && enabled;
        ui.painter().rect(
            rect,
            5.0,
            if !enabled {
                ui_kit::HOVER
            } else if hover {
                Color32::from_rgb(244, 247, 251)
            } else {
                Color32::from_rgb(250, 251, 253)
            },
            egui::Stroke::new(
                1.0,
                if !enabled {
                    Color32::from_rgb(226, 232, 240)
                } else if hover {
                    Color32::from_rgb(176, 188, 204)
                } else {
                    ui_kit::CARD_BORDER
                },
            ),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 7.0, rect.center().y),
            Align2::LEFT_CENTER,
            &cur,
            FontId::proportional(12.0),
            if enabled { ui_kit::TEXT } else { ui_kit::DISABLED },
        );
        ui_kit::caret(
            ui,
            egui::pos2(rect.right() - 8.0, rect.center().y),
            if enabled { Color32::from_rgb(68, 68, 68) } else { ui_kit::DISABLED },
        );
        r.on_hover_text(tip.clone())
    });
    let name = if tip.is_empty() {
        format!("{cur}（下拉）")
    } else {
        format!("{cur}，{tip}")
    };
    ui_kit::a11y_name(&clicked.inner, &name, enabled, None);
    if clicked.inner.clicked() {
        app.ribbon_sub = None;
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Items(items.to_vec()),
            pos: clicked.inner.rect.left_bottom(),
            opened_at: ui.input(|i| i.time),
        });
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
        let _ = ui.add(
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

    if l.id == "AreaLabel" {
        ui.add_space(12.0);
        let cw = 76.0_f32.max(measure(ui, &text, 12.0) + 18.0);
        let (cr, _) = ui.allocate_exact_size(Vec2::new(cw, 46.0), Sense::hover());
        ui.painter().rect(
            cr,
            6.0,
            Color32::from_rgb(248, 250, 252),
            egui::Stroke::new(1.0, ui_kit::CARD_BORDER),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            egui::pos2(cr.center().x, cr.top() + 13.0),
            Align2::CENTER_CENTER,
            "当前区域",
            FontId::proportional(10.0),
            ui_kit::TEXT_WEAK,
        );
        ui.painter().text(
            egui::pos2(cr.center().x, cr.bottom() - 13.0),
            Align2::CENTER_CENTER,
            &text,
            FontId::monospace(12.0),
            ui_kit::BRAND_NAVY,
        );
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

fn menu_shortcut(action: &str) -> &'static str {
    match action {
        "Paste" => "Ctrl+V",
        "PasteValues" => "Ctrl+Shift+V",
        "PasteSpecial" => "Ctrl+Alt+V",
        "Copy" => "Ctrl+C",
        "Cut" => "Ctrl+X",
        "FillDown" => "Ctrl+D",
        "FillRight" => "Ctrl+R",
        "FlashFill" => "Ctrl+E",
        "FindOpen" => "Ctrl+F",
        "ReplaceOpen" => "Ctrl+H",
        "GotoOpen" => "Ctrl+G",
        "FormatCellsDialog" => "Ctrl+1",
        "Filter" => "Ctrl+Shift+L",
        "AutoSum" => "Alt+=",
        "InsertTable" | "FormatAsTable" => "Ctrl+T",
        "Hyperlink" => "Ctrl+K",
        _ => "",
    }
}

fn menu_swatch(action: &str) -> Option<Color32> {
    Some(match action {
        "CfBarBlue" => Color32::from_rgb(99, 142, 198),
        "CfBarGreen" => Color32::from_rgb(99, 190, 123),
        "CfBarRed" => Color32::from_rgb(248, 105, 107),
        "CfBarOrange" => Color32::from_rgb(255, 166, 76),
        "CfBarPurple" => Color32::from_rgb(147, 112, 219),
        "CfBarTeal" => Color32::from_rgb(45, 178, 160),
        "TabColorNone" => Color32::WHITE,
        "TabColorRed" => Color32::from_rgb(192, 0, 0),
        "TabColorOrange" => Color32::from_rgb(237, 125, 49),
        "TabColorYellow" => Color32::from_rgb(255, 192, 0),
        "TabColorGreen" => Color32::from_rgb(112, 173, 71),
        "TabColorTeal" => Color32::from_rgb(0, 176, 176),
        "TabColorBlue" => Color32::from_rgb(68, 114, 196),
        "TabColorPurple" => Color32::from_rgb(112, 48, 160),
        "TabColorGray" => Color32::from_rgb(128, 128, 128),
        _ => return None,
    })
}

fn menu_has_gutter(m: &Menu) -> bool {
    m.items.iter().any(|it| match it {
        MenuNode::Button(c) => c.image.is_some() || menu_swatch(c.action()).is_some(),
        MenuNode::Menu(sub) => sub.ctl.image.is_some(),
        _ => false,
    })
}

fn measure_menu_width(app: &SheetApp, ui: &egui::Ui, m: &Menu, min_w: f32) -> f32 {
    let gutter = menu_has_gutter(m);
    let left_pad = if gutter { 30.0 } else { 10.0 };
    let mut max_w = if m.ctl.id == "PasteMenu" {
        min_w.max(216.0)
    } else {
        min_w
    };
    if m.ctl.id == "RecentFnMenu" {
        for name in crate::fnrecent::menu_items() {
            max_w = max_w.max(left_pad + measure(ui, &name, 12.0) + 14.0);
        }
        return max_w.clamp(min_w, 340.0);
    }
    for it in &m.items {
        match it {
            MenuNode::Separator => {}
            MenuNode::Button(c) => {
                let st = app.ctl_state(&c.id);
                if !st.visible {
                    continue;
                }
                let label = st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
                let sc = menu_shortcut(c.action());
                let sc_w = if sc.is_empty() {
                    0.0
                } else {
                    measure(ui, sc, 11.0) + 18.0
                };
                max_w = max_w.max(left_pad + measure(ui, &label, 12.0) + sc_w + 14.0);
            }
            MenuNode::Toggle(c) => {
                let st = app.ctl_state(&c.id);
                if !st.visible {
                    continue;
                }
                let label = st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
                max_w = max_w.max(28.0 + measure(ui, &label, 12.0) + 16.0);
            }
            MenuNode::Menu(sub) => {
                let label = sub.ctl.label.as_deref().unwrap_or("");
                max_w = max_w.max(left_pad + measure(ui, label, 12.0) + 26.0);
            }
        }
    }
    max_w.clamp(min_w, 340.0)
}

fn measure_items_width(ui: &egui::Ui, items: &[Item]) -> f32 {
    let is_styles = items.first().is_some_and(|i| i.id.starts_with("Style"));
    let mut max_w: f32 = 188.0;
    for it in items {
        let w = if is_styles {
            40.0 + measure(ui, &it.label, 12.0) + 18.0
        } else {
            24.0 + measure(ui, &it.label, 12.0)
        };
        max_w = max_w.max(w);
    }
    max_w.clamp(188.0, 320.0)
}

fn popup_menu_row(
    ui: &mut egui::Ui,
    label: &str,
    icon: Option<Icon>,
    swatch: Option<Color32>,
    shortcut: &str,
    has_sub: bool,
    active_sub: bool,
    enabled: bool,
    row_w: f32,
    icon_gutter: bool,
) -> egui::Response {
    let text_w = measure(ui, label, 12.0);
    let sc_w = if shortcut.is_empty() {
        0.0
    } else {
        measure(ui, shortcut, 11.0) + 16.0
    };
    let left_pad = if icon_gutter { 30.0 } else { 10.0 };
    let right_pad = if has_sub { 24.0 } else { 12.0 };
    let w = (left_pad + text_w + sc_w + right_pad).max(row_w);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 27.0), sense);
    if enabled && resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 5.0, ui_kit::PRESSED);
    } else if enabled && (resp.hovered() || active_sub) {
        ui.painter().rect_filled(rect, 5.0, ui_kit::HOVER);
    }
    let col = if !enabled {
        ui_kit::DISABLED
    } else {
        ui.visuals().text_color()
    };
    if icon_gutter {
        if let Some(rgb) = swatch {
            let sw = Rect::from_center_size(
                egui::pos2(rect.left() + 15.0, rect.center().y),
                Vec2::new(13.0, 13.0),
            );
            ui.painter().rect_filled(sw, 2.5, rgb);
            ui.painter().rect_stroke(
                sw,
                2.5,
                egui::Stroke::new(1.0, Color32::from_rgb(160, 170, 184)),
                egui::StrokeKind::Inside,
            );
        } else if let Some(ic) = icon {
            let ir = Rect::from_center_size(
                egui::pos2(rect.left() + 15.0, rect.center().y),
                Vec2::splat(16.0),
            );
            crate::ui_icons::draw_excel(ui.painter(), ir, ic, ui_kit::DISABLED, enabled);
        }
    }
    ui.painter().text(
        egui::pos2(rect.left() + left_pad, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        col,
    );
    if has_sub {
        ui_kit::caret_right(
            ui,
            egui::pos2(rect.right() - 10.0, rect.center().y),
            Color32::from_rgb(100, 116, 139),
        );
    } else if !shortcut.is_empty() {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            shortcut,
            FontId::proportional(11.0),
            Color32::from_rgb(130, 142, 158),
        );
    }
    ui_kit::a11y_name(&resp, label, enabled, None);
    resp
}

fn paste_visual_grid(
    app: &SheetApp,
    ui: &mut egui::Ui,
    row_w: f32,
    fire: &mut Option<String>,
    close: &mut bool,
) {
    let paste_on = app.ctl_state("Paste").enabled;
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("粘贴选项")
                .size(11.0)
                .color(ui_kit::TEXT_WEAK),
        );
    });
    ui.add_space(3.0);
    let rows: [[(Icon, &str, &str); 5]; 2] = [
        [
            (Icon::Paste, "Paste", "粘贴 (Ctrl+V)"),
            (Icon::PasteValues123, "PasteValues", "粘贴为数值 (Ctrl+Shift+V)"),
            (Icon::Fx, "PasteFormulas", "仅粘贴公式"),
            (Icon::Transpose, "PasteTranspose", "转置粘贴 (行列互换)"),
            (Icon::Brush, "PasteFormats", "仅粘贴格式"),
        ],
        [
            (Icon::Percent, "PasteValuesNumFmt", "值和数字格式"),
            (Icon::CellStyle, "PasteValuesSourceFmt", "值和源格式"),
            (Icon::NoBorder, "PasteNoBorders", "无边框粘贴"),
            (Icon::ColWidth, "PasteColWidths", "保留源列宽"),
            (Icon::PasteLink, "PasteLink", "粘贴链接"),
        ],
    ];
    for row in rows {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            for (ic, act, tip) in row {
                let (tr, resp) = ui.allocate_exact_size(
                    Vec2::new(38.0, 32.0),
                    if paste_on { Sense::click() } else { Sense::hover() },
                );
                let fill = if paste_on && resp.is_pointer_button_down_on() {
                    ui_kit::PRESSED
                } else if paste_on && resp.hovered() {
                    ui_kit::HOVER
                } else {
                    Color32::from_rgb(248, 250, 252)
                };
                let stroke_col = if paste_on && resp.hovered() {
                    ui_kit::ACCENT
                } else {
                    Color32::from_rgb(226, 232, 240)
                };
                ui.painter().rect(
                    tr,
                    5.0,
                    fill,
                    egui::Stroke::new(1.0, stroke_col),
                    egui::StrokeKind::Inside,
                );
                crate::ui_icons::draw_excel(
                    ui.painter(),
                    Rect::from_center_size(tr.center(), Vec2::splat(19.0)),
                    ic,
                    ui_kit::DISABLED,
                    paste_on,
                );
                ui_kit::a11y_name(&resp, &format!("快捷：{tip}"), paste_on, None);
                if resp.on_hover_text(tip).clicked() && paste_on {
                    *fire = Some(act.to_string());
                    *close = true;
                }
            }
        });
        ui.add_space(3.0);
    }
    ui.add_space(2.0);
    let (sr, _) = ui.allocate_exact_size(Vec2::new(row_w.max(206.0), 1.0), Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(sr.left() + 6.0, sr.center().y),
            egui::pos2(sr.right() - 6.0, sr.center().y),
        ],
        egui::Stroke::new(1.0, Color32::from_rgb(232, 236, 242)),
    );
    ui.add_space(2.0);
}

fn border_preset_icon(ui: &egui::Ui, row: Rect, action: &str) {
    let origin = egui::pos2(row.left() + 7.0, row.center().y - 8.0);
    let fg = ui.visuals().text_color();
    for y in 0..2 {
        for x in 0..2 {
            let cell = Rect::from_min_size(origin + Vec2::new(x as f32 * 8.0, y as f32 * 8.0), Vec2::splat(8.0));
            let edges = super::actions::preset_edges(action, [x == 0, x == 1, y == 0, y == 1], 1);
            for ((a,b), style) in [
                (cell.left_top(),cell.left_bottom()), (cell.right_top(),cell.right_bottom()),
                (cell.left_top(),cell.right_top()), (cell.left_bottom(),cell.right_bottom()),
            ].into_iter().zip(edges) {
                let id = style.unwrap_or(0);
                let color = if id == 0 { Color32::from_gray(172) } else { fg };
                crate::border_lines::paint(ui.painter(), a, b, if id == 0 { 6 } else { id }, color, 0.8);
            }
        }
    }
}

fn activate_border_pen(app: &mut SheetApp) {
    use crate::BorderBrush::{Grid, Outline};
    if !matches!(app.border_brush, Some(Grid | Outline)) {
        app.set_border_brush(Outline);
    }
}

fn draw_menu_nodes(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    menu_id: &str,
    items: &[MenuNode],
    min_w: f32,
    icon_gutter: bool,
    active_sub_id: Option<&str>,
    fire: &mut Option<String>,
    close: &mut bool,
    want_sub: &mut Option<(String, Menu, egui::Pos2)>,
    hover_non_sub: &mut bool,
) {
    if menu_id == "BorderColorMenu" {
        let mut auto = false;
        let mut picked = None;
        crate::wps_chrome::paint_wps_color_palette(ui, Some(app.last_border_color), &app.recent_colors,
            Some("自动"), &mut auto, &mut picked);
        if let Some(color) = picked.or_else(|| auto.then_some([0; 3])) {
            app.last_border_color = color;
            if !auto { app.push_recent_color(color); }
            activate_border_pen(app);
            *close = true;
        }
        return;
    }
    if menu_id == "BorderLineMenu" {
        for &(id, label) in crate::border_lines::STYLES {
            let row = popup_menu_row(ui, label, None, None, "", false, id == app.border_brush_style, true, min_w, false);
            let end = egui::pos2(row.rect.right() - 9.0, row.rect.center().y);
            crate::border_lines::paint(ui.painter(), end - Vec2::new(68.0, 0.0), end, id,
                Color32::from_rgb(app.last_border_color[0],app.last_border_color[1],app.last_border_color[2]), 1.0);
            if row.clicked() {
                app.border_brush_style = id;
                activate_border_pen(app);
                *close = true;
            }
        }
        return;
    }
    if menu_id == "BordersMenu" { ui.strong("边框"); }
    if menu_id == "RecentFnMenu" {
        for name in crate::fnrecent::menu_items() {
            let r = popup_menu_row(ui, &name, None, None, "", false, false, true, min_w, icon_gutter);
            if r.hovered() {
                *hover_non_sub = true;
            }
            if r.clicked() {
                *fire = Some(format!("InsertFn:{name}"));
                *close = true;
            }
        }
        return;
    }
    for it in items {
        match it {
            MenuNode::Separator => {
                ui.add_space(2.0);
                let (sr, _) = ui.allocate_exact_size(Vec2::new(min_w, 1.0), Sense::hover());
                ui.painter().line_segment(
                    [
                        egui::pos2(sr.left() + 6.0, sr.center().y),
                        egui::pos2(sr.right() - 6.0, sr.center().y),
                    ],
                    egui::Stroke::new(1.0, Color32::from_rgb(232, 236, 242)),
                );
                ui.add_space(2.0);
            }
            MenuNode::Button(c) => {
                if menu_id == "BordersMenu" && c.id == "BorderDraw" { ui.strong("绘制边框"); }
                let st = app.ctl_state(&c.id);
                if !st.visible {
                    continue;
                }
                let label = st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
                let icon = c.image.as_deref().and_then(icon_of);
                let sw = menu_swatch(c.action());
                let sc = menu_shortcut(c.action());
                let checked = matches!((c.action(), app.border_brush),
                    ("BorderDraw", Some(crate::BorderBrush::Outline))
                    | ("BorderDrawGrid", Some(crate::BorderBrush::Grid))
                    | ("BorderErase", Some(crate::BorderBrush::Erase)));
                let r = popup_menu_row(ui, &label, icon, sw, sc, false, checked, st.enabled, min_w, icon_gutter);
                if menu_id == "BordersMenu" && c.image.is_none() {
                    border_preset_icon(ui, r.rect, c.action());
                }
                if r.hovered() {
                    *hover_non_sub = true;
                }
                if r.clicked() && st.enabled {
                    *fire = Some(c.action().to_string());
                    *close = true;
                }
            }
            MenuNode::Toggle(c) => {
                let st = app.ctl_state(&c.id);
                if !st.visible {
                    continue;
                }
                let label = st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
                let r = ui_kit::check_row(ui, &label, st.checked, st.enabled, min_w);
                if r.hovered() {
                    *hover_non_sub = true;
                }
                if r.clicked() && st.enabled {
                    *fire = Some(c.action().to_string());
                    *close = true;
                }
            }
            MenuNode::Menu(sub) => {
                let label = sub.ctl.label.clone().unwrap_or_default();
                let icon = sub.ctl.image.as_deref().and_then(icon_of);
                let is_open = active_sub_id == Some(sub.ctl.id.as_str());
                let r = popup_menu_row(ui, &label, icon, None, "", true, is_open, true, min_w, icon_gutter);
                if r.hovered() || r.clicked() {
                    *want_sub = Some((
                        menu_id.to_string(),
                        sub.clone(),
                        egui::pos2(r.rect.right() + 6.0, r.rect.top() - 4.0),
                    ));
                }
            }
        }
    }
}

fn popup(app: &mut SheetApp, ui: &mut egui::Ui) {
    let Some(p) = app.ribbon_popup.clone() else {
        app.ribbon_sub = None;
        return;
    };
    let ctx = ui.ctx().clone();
    let screen = ctx.screen_rect();
    let mut pos = p.pos;
    let (inner_w, has_scroll, est_h) = match &p.kind {
        PopupKind::Menu(m) => {
            let gutter = menu_has_gutter(m);
            let min_w = if m.ctl.id == "PasteMenu" {
                216.0
            } else if gutter {
                192.0
            } else {
                176.0
            };
            let w = measure_menu_width(app, ui, m, min_w);
            if m.ctl.id == "BordersMenu" {
                let needed = m.items.iter().map(|it| if matches!(it, MenuNode::Separator) { 7.0 } else { 29.0 }).sum::<f32>() + 64.0;
                let available = (screen.height() - 42.0).max(80.0);
                (w, needed > available, needed.min(available))
            } else {
                let scroll = m.items.len() > 13;
                let extra = if m.ctl.id == "PasteMenu" { 104.0 } else { 0.0 };
                let h = (m.items.len() as f32 * 28.0 + 18.0 + extra).min(460.0);
                (w, scroll, h)
            }
        }
        PopupKind::Items(items) => {
            let w = measure_items_width(ui, items);
            let scroll = items.len() > 10;
            let h = (items.len() as f32 * 28.0 + 16.0).min(360.0);
            (w, scroll, h)
        }
    };
    let frame_inner_w = inner_w + if has_scroll { 14.0 } else { 0.0 };
    let est_w = frame_inner_w + 12.0;
    pos.x = pos.x.clamp(4.0, (screen.right() - est_w - 6.0).max(4.0));
    pos.y = pos.y.clamp(28.0, (screen.bottom() - est_h - 6.0).max(28.0));

    let mut close = false;
    let mut fire: Option<String> = None;
    let mut want_sub: Option<(String, Menu, egui::Pos2)> = None;
    let mut hover_non_sub = false;
    let active_sub_id = app.ribbon_sub.as_ref().map(|(_, s, _)| s.ctl.id.clone());

    let area = egui::Area::new(egui::Id::new("ribbon_popup"))
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(&ctx, |ui| {
            ui_kit::popup_frame(ui.style()).show(ui, |ui| {
                ui.set_min_width(frame_inner_w);
                ui.set_max_width(frame_inner_w);
                ui.spacing_mut().item_spacing.y = 2.0;
                match &p.kind {
                    PopupKind::Menu(m) => {
                        let gutter = menu_has_gutter(m);
                        if m.ctl.id == "PasteMenu" {
                            paste_visual_grid(app, ui, inner_w, &mut fire, &mut close);
                        }
                        let mut show_rows = |ui: &mut egui::Ui| {
                            draw_menu_nodes(
                                app,
                                ui,
                                &m.ctl.id,
                                &m.items,
                                inner_w,
                                gutter,
                                active_sub_id.as_deref(),
                                &mut fire,
                                &mut close,
                                &mut want_sub,
                                &mut hover_non_sub,
                            );
                        };
                        if has_scroll {
                            egui::ScrollArea::vertical().max_height(if m.ctl.id == "BordersMenu" { est_h - 14.0 } else { 390.0 }).show(ui, show_rows);
                        } else {
                            show_rows(ui);
                        }
                    }
                    PopupKind::Items(items) => {
                        let is_styles = items.first().is_some_and(|i| i.id.starts_with("Style"));
                        let mut show_items = |ui: &mut egui::Ui| {
                            for it in items {
                                if is_styles {
                                    let (bg, fg, bold) = style_preview(&it.id);
                                    let (rect, r) =
                                        ui.allocate_exact_size(Vec2::new(inner_w, 27.0), Sense::click());
                                    if r.is_pointer_button_down_on() {
                                        ui.painter().rect_filled(rect, 5.0, ui_kit::PRESSED);
                                    } else if r.hovered() {
                                        ui.painter().rect_filled(rect, 5.0, ui_kit::HOVER);
                                    }
                                    let sw = Rect::from_center_size(
                                        egui::pos2(rect.left() + 20.0, rect.center().y),
                                        Vec2::new(26.0, 18.0),
                                    );
                                    ui.painter().rect(
                                        sw,
                                        3.0,
                                        bg,
                                        egui::Stroke::new(1.0, Color32::from_rgb(198, 206, 218)),
                                        egui::StrokeKind::Inside,
                                    );
                                    ui.painter().text(
                                        sw.center(),
                                        Align2::CENTER_CENTER,
                                        "Aa",
                                        FontId::proportional(if bold { 11.0 } else { 10.5 }),
                                        fg,
                                    );
                                    ui.painter().text(
                                        egui::pos2(rect.left() + 40.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        &it.label,
                                        FontId::proportional(12.0),
                                        ui.visuals().text_color(),
                                    );
                                    ui_kit::a11y_name(&r, &it.label, true, None);
                                    if r.clicked() {
                                        fire = Some(it.id.clone());
                                        close = true;
                                    }
                                } else {
                                    let r = ui_kit::popup_item(ui, &it.label, true, inner_w);
                                    if r.clicked() {
                                        fire = Some(it.id.clone());
                                        close = true;
                                    }
                                }
                            }
                        };
                        if has_scroll {
                            egui::ScrollArea::vertical().max_height(340.0).show(ui, show_items);
                        } else {
                            show_items(ui);
                        }
                    }
                }
            });
        });
    let rect = area.response.rect;

    if let Some((pid, sub, spos)) = want_sub {
        app.ribbon_sub = Some((pid, sub, spos));
    }

    let mut sub_rect: Option<Rect> = None;
    if let Some((pid, sub, mut spos)) = app.ribbon_sub.clone() {
        let sub_gutter = menu_has_gutter(&sub);
        let sub_inner_w = if sub.ctl.id == "BorderColorMenu" { 218.0 } else { measure_menu_width(app, ui, &sub, if sub_gutter { 186.0 } else { 170.0 }) };
        let desired_h = match sub.ctl.id.as_str() {
            "BorderLineMenu" => 29.0 * crate::border_lines::STYLES.len() as f32 + 14.0,
            "BorderColorMenu" => 238.0,
            _ => (sub.items.len() as f32 * 28.0 + 16.0).min(390.0),
        };
        let sub_h = desired_h.min((screen.height() - 42.0).max(80.0));
        let sub_scroll = sub.items.len() > 13 || desired_h > sub_h;
        let sub_frame_w = sub_inner_w + if sub_scroll { 14.0 } else { 0.0 };
        let sub_outer_w = sub_frame_w + 12.0;
        if p.opened_at == 0.0 {
            spos = egui::pos2(rect.right() + 2.0, rect.top() + 58.0);
        }
        if spos.x < rect.right() && spos.x + sub_outer_w > rect.left() {
            spos.x = if rect.right() + sub_outer_w <= screen.right() - 6.0 {
                rect.right() + 2.0
            } else {
                (rect.left() - sub_outer_w - 2.0).max(4.0)
            };
        } else if spos.x + sub_outer_w > screen.right() - 6.0 {
            spos.x = (rect.left() - sub_outer_w - 2.0).max(4.0);
        }
        spos.x = spos.x.clamp(4.0, (screen.right() - sub_outer_w - 4.0).max(4.0));
        spos.y = spos.y.clamp(28.0, (screen.bottom() - sub_h - 6.0).max(28.0));
        let mut dummy_sub: Option<(String, Menu, egui::Pos2)> = None;
        let mut dummy_hover = false;
        let sub_area = egui::Area::new(egui::Id::new("ribbon_sub_popup"))
            .fixed_pos(spos)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                ui_kit::popup_frame(ui.style()).show(ui, |ui| {
                    ui.set_min_width(sub_frame_w);
                    ui.set_max_width(sub_frame_w);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let mut show_sub = |ui: &mut egui::Ui| {
                        draw_menu_nodes(
                            app,
                            ui,
                            &sub.ctl.id,
                            &sub.items,
                            sub_inner_w,
                            sub_gutter,
                            None,
                            &mut fire,
                            &mut close,
                            &mut dummy_sub,
                            &mut dummy_hover,
                        );
                    };
                    if sub_scroll {
                        egui::ScrollArea::vertical().max_height(sub_h - 16.0).show(ui, show_sub);
                    } else {
                        show_sub(ui);
                    }
                });
            });
        sub_rect = Some(sub_area.response.rect);
        let ptr_pos = ctx.input(|i| i.pointer.hover_pos());
        if hover_non_sub
            && !ptr_pos.is_some_and(|pt| sub_area.response.rect.contains(pt))
            && matches!(&p.kind, PopupKind::Menu(m) if m.ctl.id == pid)
        {
            app.ribbon_sub = None;
        }
    }

    if let Some(a) = fire {
        app.ribbon_action(&a, &ctx);
    }
    let now = ctx.input(|i| i.time);
    let fresh = now - p.opened_at < 0.08;
    let in_popup = ctx
        .input(|i| i.pointer.interact_pos())
        .map(|pt| rect.contains(pt) || sub_rect.is_some_and(|sr| sr.contains(pt)))
        .unwrap_or(false);
    if close
        || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        || (!fresh && ctx.input(|i| i.pointer.any_click()) && !in_popup)
    {
        app.ribbon_popup = None;
        app.ribbon_sub = None;
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
            5.0,
            Color32::from_rgb(250, 251, 253),
            egui::Stroke::new(1.0, ui_kit::CARD_BORDER),
            egui::StrokeKind::Inside,
        );
        let arrow_rect = Rect::from_min_max(
            egui::pos2(crect.right() - 18.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 1.0, crect.bottom() - 1.0),
        );
        let text_rect = Rect::from_min_max(
            egui::pos2(crect.left() + 7.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 19.0, crect.bottom() - 1.0),
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
        let area = egui::Area::new(egui::Id::new("font_list"))
            .fixed_pos(egui::pos2(arrow_pos.x, arrow_pos.y + 3.0))
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                ui_kit::popup_frame(ui.style()).show(ui, |ui| {
                    ui.set_min_width(176.0);
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
        let outside = !arrow_clicked
            && ctx.input(|i| i.pointer.any_click())
            && !ctx
                .input(|i| i.pointer.interact_pos())
                .map(|p| area.response.rect.contains(p))
                .unwrap_or(false);
        if close || outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
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
            5.0,
            Color32::from_rgb(250, 251, 253),
            egui::Stroke::new(1.0, ui_kit::CARD_BORDER),
            egui::StrokeKind::Inside,
        );
        let arrow_rect = Rect::from_min_max(
            egui::pos2(crect.right() - 18.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 1.0, crect.bottom() - 1.0),
        );
        let text_rect = Rect::from_min_max(
            egui::pos2(crect.left() + 7.0, crect.top() + 1.0),
            egui::pos2(crect.right() - 19.0, crect.bottom() - 1.0),
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
        let area = egui::Area::new(egui::Id::new("size_list"))
            .fixed_pos(egui::pos2(arrow_pos.x, arrow_pos.y + 3.0))
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                ui_kit::popup_frame(ui.style()).show(ui, |ui| {
                    ui.set_min_width(92.0);
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
        let outside = !arrow_clicked
            && ctx.input(|i| i.pointer.any_click())
            && !ctx
                .input(|i| i.pointer.interact_pos())
                .map(|p| area.response.rect.contains(p))
                .unwrap_or(false);
        if close || outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
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
    let w = c.width.unwrap_or(330.0).min(330.0);
    let enabled = on_enabled(c, &st);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 65.0), Sense::hover());
    let arrow_w = 24.0;
    let area = Rect::from_min_max(rect.min, egui::pos2(rect.right() - arrow_w, rect.bottom()));
    let inline_items = &items[..items.len().min(5)];
    let n = inline_items.len().max(1) as f32;
    let tw = area.width() / n;
    let mut fire: Option<String> = None;
    for (i, it) in inline_items.iter().enumerate() {
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
        app.ribbon_sub = None;
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

/// 「组名 ▾」「更多 ▾」折叠按钮。内嵌字体子集里没有 U+25BE，直接写「▾」会显示成方块，
/// 所以文字后面留空，再手画一个和「单元格样式」下拉同款的 V 形箭头。
fn menu_chevron(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    let ir = ui.menu_button(format!("{label}    "), add_contents);
    let rect = ir.response.rect;
    let col = ui.visuals().text_color();
    let cx = rect.right() - 11.0;
    let cy = rect.center().y;
    let stk = egui::Stroke::new(1.1, col);
    ui.painter()
        .line_segment([egui::pos2(cx - 3.0, cy - 1.5), egui::pos2(cx, cy + 1.5)], stk);
    ui.painter()
        .line_segment([egui::pos2(cx, cy + 1.5), egui::pos2(cx + 3.0, cy - 1.5)], stk);
}

/// 紧凑模式的样式库：整条 349px 的缩略图放不下时，收成与「条件格式 / 套用表格格式」同高的
/// 76px 大号拆分按钮（不再是一条悬浮在顶边的 24px 裸文字按钮），三枚样式命令整齐并排。
fn gallery_compact(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, items: &[Item]) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    let full_label = c
        .label
        .clone()
        .or_else(|| st.label.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "单元格样式".to_string());
    let vis_label = if RIBBON_MID.with(|m| m.get()) && full_label == "单元格样式" {
        "样式".to_string()
    } else {
        full_label.clone()
    };
    let enabled = on_enabled(c, &st);
    let tw = measure(ui, &vis_label, 12.0);
    let w = (tw + 14.0).max(if vis_label != full_label { 46.0 } else { 62.0 });
    let ic = c.image.as_deref().and_then(icon_of).unwrap_or(Icon::CellStyle);
    let (r, ar) = ui.add_enabled_ui(enabled, |ui| {
        ui_kit::big_sized_split(ui, ic, &vis_label, Vec2::new(w, 76.0))
    }).inner;
    {
        let mut main = r.rect;
        main.max.y = main.max.y.min(ar.rect.top());
        ui_kit::a11y_name_rect(&r, &full_label, enabled, main, ui.ctx());
    }
    ui_kit::a11y_name(&ar, &format!("{full_label} 展开"), enabled, None);
    if (r.clicked() || ar.clicked()) && enabled {
        app.ribbon_sub = None;
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Items(items.to_vec()),
            pos: egui::pos2(r.rect.left(), ar.rect.bottom()),
            opened_at: ui.input(|i| i.time),
        });
    }
}

fn style_preview(id: &str) -> (Color32, Color32, bool) {
    crate::wps_menus::style_preset_visuals(id)
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
        "NewFile" => Icon::NewFile,
        "MacroRun" => Icon::MacroRun,
        "MacroExport" => Icon::Macro,
        "VisualBasicEditor" => Icon::Code,
        "Compare" => Icon::Compare,
        "SyncScroll" => Icon::SyncScroll,
        "ValidationCircleInvalid" => Icon::InvalidCircle,
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
        "PasteValues123" => Icon::PasteValues123,
        "StrikeThrough" => Icon::StrikeThrough,
        "Spotlight" => Icon::Spotlight,
        "SplitPanes" => Icon::SplitPanes,
        "HighlightDup" => Icon::HighlightDup,
        "RejectDup" => Icon::RejectDup,
        "MergeSame" => Icon::MergeSame,
        "InsertCell" => Icon::InsertCell,
        "DeleteCell" => Icon::DeleteCell,
        "CellStyle" => Icon::CellStyle,
        "FnBook" => Icon::FnBook,
        "Transpose" => Icon::Transpose,
        "PasteLink" => Icon::PasteLink,
        "PastePic" => Icon::PastePic,
        "NoBorder" => Icon::NoBorder,
        "ColWidth" => Icon::ColWidth,
        _ => return None,
    })
}

#[cfg(test)]
mod compact_tests {
    use super::*;

    #[test]
    fn border_menu_pen_choices_enter_draw_mode_preserve_grid_and_exit_on_escape() {
        use crate::BorderBrush::{Erase, Grid, Outline};
        use egui::{Event, Modifiers, PointerButton, RawInput};

        let _env = crate::test_env::TestEnv::new("ribbon-border-pen");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 480.0));
        let pos = egui::pos2(25.0, 18.0);
        for menu in ["BorderLineMenu", "BorderColorMenu"] {
            for before in [None, Some(Erase), Some(Outline), Some(Grid)] {
                app.border_brush = before;
                app.border_brush_style = 3;
                app.last_border_color = [96, 30, 40];
                let mut close = false;
                for press in [None, Some(true), Some(false)] {
                    let mut events = vec![Event::PointerMoved(pos)];
                    if let Some(pressed) = press {
                        events.push(Event::PointerButton {
                            pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE,
                        });
                    }
                    let input = RawInput { events, screen_rect: Some(screen), ..Default::default() };
                    let _ = ctx.run(input, |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            draw_menu_nodes(&mut app, ui, menu, &[], 190.0, false, None,
                                &mut None, &mut close, &mut None, &mut false);
                        });
                    });
                }
                assert!(close, "choosing a pen should close {menu}");
                if menu == "BorderLineMenu" {
                    assert_eq!(app.border_brush_style, crate::border_lines::STYLES[0].0);
                } else {
                    assert_eq!(app.last_border_color, [0; 3]);
                }
                let expected = if before == Some(Grid) { Grid } else { Outline };
                assert!(app.border_brush == Some(expected));
                let escape = Event::Key {
                    key: egui::Key::Escape, physical_key: None, pressed: true,
                    repeat: false, modifiers: Modifiers::NONE,
                };
                let input = RawInput { events: vec![escape], screen_rect: Some(screen), ..Default::default() };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
                });
                assert!(app.border_brush.is_none());
            }
        }
    }

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

    /// 功能区里每个 imageMso 都要有专属图标。
    /// 漏映射时绘制端会退回同一个蓝色网格（`unwrap_or(Icon::Grid)`），
    /// 用户看到的就是「几个功能共用一枚占位图」，这种缺失必须在测试里挡住。
    #[test]
    fn every_ribbon_image_resolves_to_an_icon() {
        let xml = include_str!("../../assets/ribbon/customUI.xml");
        let mut missing = Vec::new();
        for part in xml.split("imageMso=\"").skip(1) {
            let name = part.split('"').next().unwrap_or_default();
            // 加粗/斜体/下划线是文字按钮，本来就没有图形图标
            if matches!(name, "Bold" | "Italic" | "Underline") {
                continue;
            }
            if icon_of(name).is_none() {
                missing.push(name.to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "这些 imageMso 没映射图标，会退化成同一个网格占位图：{missing:?}"
        );
    }
}
