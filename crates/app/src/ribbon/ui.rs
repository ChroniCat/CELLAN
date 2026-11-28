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
    let owned_label;
    let label: &str = if t.label == "文件" {
        "文件   "
    } else {
        owned_label = format!("{}   ", t.label);
        &owned_label
    };
    let keytip_ch = if app.keytips && app.keytip_tab.is_none() {
        t.keytip.chars().next()
    } else {
        None
    };
    let layout = ui_kit::layout_tab(ui, label, true);
    let resp = ui_kit::tab_ex(ui, layout, false, true);
    ui_kit::a11y_name(&resp, t.label.trim(), true, Some(app.file_menu_open));
    ui_kit::focus_ring(ui, &resp, resp.rect, 4.0);
    ui_kit::caret(
        ui,
        egui::pos2(resp.rect.right() - 11.0, resp.rect.center().y + 0.5),
        crate::theme::palette().accent,
    );
    if let Some(ch) = keytip_ch {
        let r = resp.rect;
        let b = Rect::from_center_size(egui::pos2(r.center().x, r.top() + 7.0), Vec2::new(15.0, 14.0));
        ui.painter().rect_filled(b, 2.0, Color32::from_rgb(68, 68, 68));
        let mut buf = [0u8; 4];
        ui.painter().text(b.center(), Align2::CENTER_CENTER, ch.encode_utf8(&mut buf), FontId::proportional(11.0), Color32::WHITE);
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

/// Lay out ordinary and contextual tabs in the space left by the two toolbars.
pub fn tabs(app: &mut SheetApp, ui: &mut egui::Ui) {
    let guard = RibbonGuard::new(app);
    let ribbon = &guard.ribbon;
    let app = &mut *guard.app;
    let mut tabs: Vec<_> = ribbon.tabs.iter().enumerate().skip(1).collect();
    let pivot = app.active_pivot().is_some();
    for (id, index, present) in [
        ("TabSetTableTools", 100, app.table_at_sel().is_some()),
        ("TabSetChartTools", 101, app.chart_at_sel().is_some()),
        ("TabSetSparklineTools", 102, app.spark_at_sel().is_some()),
        ("TabSetPivotAnalyze", 103, pivot),
        ("TabSetPivotDesign", 104, pivot),
    ] {
        if present {
            if let Some(context) = ribbon.context.iter().find(|c| c.set_id == id) {
                tabs.push((index, &context.tab));
            }
        }
    }

    let layouts: Vec<_> = tabs.iter()
        .map(|(index, tab)| ui_kit::layout_tab(ui, &tab.label, *index == app.ribbon_tab))
        .collect();
    let available = ui.available_width();
    let overflow = layouts.iter().map(|(_, width)| width).sum::<f32>() > available;
    let active_width = tabs.iter().position(|(i, _)| *i == app.ribbon_tab).map_or(0.0, |i| layouts[i].1);
    let mut remaining = available - 28.0 - active_width;
    let mut hidden = Vec::new();
    for ((i, tab), layout) in tabs.into_iter().zip(layouts) {
        let (_, width) = &layout;
        let active = i == app.ribbon_tab;
        if overflow && ((active && available < width + 28.0) || (!active && *width > remaining)) {
            hidden.push((i, tab));
            continue;
        }
        if !active {
            remaining -= width;
        }
        let resp = ui.push_id(i, |ui| ui_kit::tab_ex(ui, layout, active, false)).inner;
        ui_kit::a11y_role(&resp, &tab.label, egui::accesskit::Role::Tab, Some(active));
        ui_kit::tab_focus_ring(ui, &resp, resp.rect.shrink2(Vec2::new(2.0, 3.0)), 5.0, active);
        if app.keytips && app.keytip_tab.is_none() {
            if let Some(ch) = tab.keytip.chars().next() {
                let b = Rect::from_center_size(egui::pos2(resp.rect.center().x, resp.rect.top() + 7.0), Vec2::new(15.0, 14.0));
                ui.painter().rect_filled(b, 2.0, Color32::from_gray(68));
                let mut buf = [0u8; 4];
                ui.painter().text(b.center(), Align2::CENTER_CENTER, ch.encode_utf8(&mut buf), FontId::proportional(11.0), Color32::WHITE);
            }
        }
        if resp.clicked() {
            let prior = resp.id.with("collapsed_before_click");
            let collapse = if resp.double_clicked() {
                !ui.data(|d| d.get_temp::<bool>(prior)).unwrap_or(app.ribbon_collapsed)
            } else {
                ui.data_mut(|d| d.insert_temp(prior, app.ribbon_collapsed));
                false
            };
            select_tab(app, i, collapse);
        }
    }
    let menu_id = ui.make_persistent_id("tab_overflow");
    if hidden.is_empty() {
        if ui.memory(|m| m.is_popup_open(menu_id)) {
            ui.memory_mut(|m| m.close_popup());
        }
        return;
    }
    let (_, rect) = ui.allocate_space(Vec2::new(28.0, 24.0));
    let resp = ui.interact(rect, menu_id.with("button"), Sense::click());
    let pal = crate::theme::palette();
    let active_hidden = hidden.iter().find(|(i, _)| *i == app.ribbon_tab);
    let title = active_hidden.map(|(_, tab)| format!("更多选项卡，当前页为{}", tab.label));
    let title = title.as_deref().unwrap_or("更多选项卡");
    let resp = resp.on_hover_text(title);
    ui_kit::a11y_name(&resp, title, true, None);
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(menu_id));
    }
    let (selected_fill, selected_ink) = ui_kit::active_tab_colors(pal);
    let is_hc = pal.theme == crate::theme::AppTheme::HighContrast;
    let ov_hov = ui_kit::anim_hover(
        ui.ctx(),
        resp.id.with("ov_hov"),
        active_hidden.is_none() && (resp.hovered() || ui.memory(|m| m.is_popup_open(menu_id))),
    );
    let ov_act = ui_kit::anim_toggle(ui.ctx(), resp.id.with("ov_act"), active_hidden.is_some());
    if active_hidden.is_some() {
        if is_hc {
            ui.painter().rect_filled(rect, 4.0, selected_fill);
        } else {
            ui.painter().rect(
                rect,
                4.0,
                selected_fill,
                egui::Stroke::new(1.0, pal.chrome_separator()),
                egui::StrokeKind::Inside,
            );
        }
        let bar_y = if is_hc { rect.bottom() + 1.0 } else { rect.bottom() - 1.5 };
        let bar_w = egui::lerp(8.0_f32..=16.0_f32, ov_act);
        let bar = Rect::from_center_size(egui::pos2(rect.center().x, bar_y), Vec2::new(bar_w, 2.0));
        let bar_col = ui_kit::lerp_color(Color32::TRANSPARENT, pal.accent, ov_act);
        if bar_col != Color32::TRANSPARENT {
            ui.painter().rect_filled(bar, 1.0, bar_col);
        }
    } else {
        let bg = ui_kit::lerp_color(Color32::TRANSPARENT, pal.hover_on_page, ov_hov);
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, 4.0, bg);
        }
    }
    ui_kit::caret(ui, rect.center(), if active_hidden.is_some() { selected_ink } else { pal.text });
    ui_kit::tab_focus_ring(ui, &resp, rect, 4.0, active_hidden.is_some());
    egui::popup_below_widget(ui, menu_id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_width(132.0);
        let (popup_fill, popup_ink) = ui_kit::active_dialog_tab_colors(pal);
        ui.visuals_mut().selection.bg_fill = popup_fill;
        ui.visuals_mut().selection.stroke.color = popup_ink;
        for (i, tab) in hidden {
            let active = i == app.ribbon_tab;
            let mut label = egui::RichText::new(&tab.label);
            if active {
                label = label.font(ui_kit::medium_in(ui.ctx(), 12.0)).color(popup_ink);
            }
            if ui.selectable_label(active, label).clicked() {
                select_tab(app, i, false);
                ui.memory_mut(|m| m.close_popup());
            }
        }
    });
}

fn select_tab(app: &mut SheetApp, index: usize, collapsed: bool) {
    app.ribbon_tab = index;
    app.ribbon_collapsed = collapsed;
    app.file_menu_open = false;
    app.ribbon_popup = None;
    app.ribbon_sub = None;
}

#[derive(Clone, Copy)]
struct RibbonFrameCtx<'a> {
    cur: &'a sheetcore::style::CellStyle,
    a11y: bool,
    mid: bool,
}

/// 当前页签的功能区主体。
pub fn body(app: &mut SheetApp, ui: &mut egui::Ui) {
    let commands = egui::UiStackInfo::default().with_tag(crate::ui_icons::RIBBON_COMMANDS);
    ui.scope_builder(egui::UiBuilder::new().ui_stack_info(commands), |ui| command_groups(app, ui));
}

fn command_groups(app: &mut SheetApp, ui: &mut egui::Ui) {
    // 上下文页签的宿主对象没了就退回「开始」（和 Excel 一样）
    // 文件还在后台打开时，选区/表格都还没就位，先别把上下文页签踢掉
    if app.tasks.open_rx.is_none() {
        if app.ribbon_tab == 100 && app.table_at_sel().is_none() {
            if crate::debug_flags::ribbon_log() {
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
        if (app.ribbon_tab == 103 || app.ribbon_tab == 104) && app.active_pivot().is_none() {
            app.ribbon_tab = 1;
        }
    }
    let cur_style = app.current_style();
    let a11y_active = ui_kit::is_accesskit_active(ui.ctx());
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
        103 => ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetPivotAnalyze")
            .map(|c| &c.tab),
        104 => ribbon
            .context
            .iter()
            .find(|c| c.set_id == "TabSetPivotDesign")
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
    let rc = RibbonFrameCtx {
        cur: &cur_style,
        a11y: a11y_active,
        mid: mid_tier,
    };
    ui_kit::set_group_pad(if mid_tier || app.ribbon_compact { 5.0 } else { 8.0 });
    let panel_log = crate::debug_flags::panel_log();
    let mut used = 0.0f32;
    let mut i = 0usize;
    while i < tab.groups.len() {
        let g = &tab.groups[i];
        // 完整/中档/紧凑三种布局的组宽分开缓存，使用解析期预计算 key，零堆分配。
        let cache_key: &str = if app.ribbon_compact {
            &g.id_compact
        } else if mid_tier {
            &g.id_mid
        } else {
            &g.id
        };
        let known = app.ribbon_group_w.get(cache_key).copied();
        let w = known.unwrap_or(160.0);
        let compact_w = g.compact_w;
        // UI-13.2 二级压缩：整组放不下时，先把大按钮收成小图标网格；
        // 小图标也放不下，才退化成「组名 ▾」/「更多 ▾」。
        // 「更多 ▾」的 60pt 只在后面还有组时才需要预留：最后一组放得下就照常画，
        // 否则悬浮卡片收窄 28px 后，1440 宽下 58px 的「加载项」会被白白挤进「更多」。
        let reserve = if i + 1 < tab.groups.len() { 60.0 } else { 0.0 };
        let full_fits = known.is_some() && used + w + reserve <= avail;
        let compact_fits = known.is_some()
            && compact_w + 12.0 <= avail - used
            && compact_w + 12.0 < w
            && !g.compact_needs_text;
        if panel_log {
            eprintln!(
                "[layout] {:14} known={known:?} w={w:.0} used={used:.0} avail={avail:.0} compact={compact_w:.0}"
                , g.label
            );
        }
        if known.is_some() && !full_fits && compact_fits {
            let x0 = ui.cursor().min.x;
            group_compact(app, ui, rc, g);
            used += (ui.cursor().min.x - x0).max(24.0);
            i += 1;
            continue;
        }
        // 给「更多 ▾」预留 60pt：不留的话最后会画到裁切区外（760×420 实测溢出 11px）
        if known.is_some() && used + w + reserve > avail && used > 0.0 {
            if avail - used >= 92.0 {
                menu_chevron(ui, &g.label, |ui| {
                    nodes(app, ui, rc, &g.children);
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
                            nodes(app, ui, rc, &h.children);
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
        group(app, ui, rc, g, panel_log);
        let w = (ui.cursor().min.x - x0).max(24.0);
        if known != Some(w) {
            app.ribbon_group_w.insert(cache_key.to_owned(), w);
        }
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

fn group(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, g: &Group, dbg: bool) {
    let before = if dbg { ui.min_rect() } else { Rect::NOTHING };
    match &g.launcher {
        Some(l) => {
            let hit = ui_kit::group_l(ui, &g.label, &g.id, |ui| nodes_auto(app, ui, rc, &g.children));
            if hit {
                let action = l.action().to_string();
                app.ribbon_action(&action, ui.ctx());
            }
        }
        None => {
            ui_kit::group(ui, &g.label, |ui| nodes_auto(app, ui, rc, &g.children));
        }
    }
    if dbg {
        let after = ui.min_rect();
        eprintln!("[group] {:22} h={:.0}", g.label, after.height() - before.height());
    }
}

fn nodes(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, list: &[Node]) {
    for n in list {
        node(app, ui, rc, n);
    }
}

/// 功能区组专用：支持 full / mid(1280..1720 精简中文标签，零克隆预计算视图) / narrow(<1280 纯图标)
fn nodes_auto(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, list: &[Node]) {
    let mid = !app.ribbon_compact && rc.mid;
    for n in list {
        if app.ribbon_compact {
            node_narrow(app, ui, rc, n);
        } else if mid {
            node_mid(app, ui, rc, n);
        } else {
            node(app, ui, rc, n);
        }
    }
}

fn node_mid(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, n: &Node) {
    match n {
        Node::Button(c) => {
            let (c2, full) = c.mid_view();
            button_a11y(app, ui, rc, c2, false, full);
        }
        Node::Toggle(c) => {
            let (c2, full) = c.mid_view();
            button_a11y(app, ui, rc, c2, true, full);
        }
        Node::Split { ctl, menu: m } => {
            let (c2, full) = ctl.mid_view();
            split_a11y(app, ui, rc, c2, m, full);
        }
        Node::Menu(m) => {
            let (c2, full) = m.ctl.mid_view();
            menu_a11y(app, ui, rc, c2, m, full);
        }
        Node::Gallery { ctl, items, .. } => gallery_compact(app, ui, rc, ctl, items),
        Node::ComboBox { ctl, items, width } => {
            let w2 = if ctl.id == "Font" {
                (*width).min(106.0)
            } else if ctl.id == "FontSize" {
                (*width).min(46.0)
            } else {
                *width
            };
            combo(app, ui, rc, ctl, items, w2);
        }
        Node::DropDown { ctl, items } => {
            let (c2, _) = ctl.mid_view();
            dropdown(app, ui, rc, c2, items);
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
                    nodes_auto(app, ui, rc, children);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    nodes_auto(app, ui, rc, children);
                });
            }
        }
        _ => node(app, ui, rc, n),
    }
}

fn node_narrow(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, n: &Node) {
    let icon_only = |c: &Ctl| {
        let mut compact = c.clone();
        let label = compact.label.take();
        if compact.screentip.is_none() {
            compact.screentip = label;
        }
        compact.width = Some(24.0);
        compact
    };
    match n {
        Node::Button(c) | Node::Toggle(c)
            if c.size != Size::Large && c.label.is_some() && c.image.is_some() =>
        {
            let toggle = matches!(n, Node::Toggle(_));
            button_a11y(app, ui, rc, &icon_only(c), toggle, c.label.as_deref());
        }
        Node::Split { ctl, menu } if ctl.size != Size::Large && ctl.image.is_some() => {
            let mut c2 = icon_only(ctl);
            c2.arrow = Some(24.0);
            split_a11y(app, ui, rc, &c2, menu, ctl.label.as_deref());
        }
        Node::Menu(m) if m.ctl.size != Size::Large && m.ctl.image.is_some() => {
            menu_a11y(app, ui, rc, &icon_only(&m.ctl), m, m.ctl.label.as_deref());
        }
        // 样式库整条 349px，1440 下必须收成「单元格样式 ▾」下拉
        Node::Gallery { ctl, items, .. } => gallery_compact(app, ui, rc, ctl, items),
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
                    nodes_auto(app, ui, rc, children);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    nodes_auto(app, ui, rc, children);
                });
            }
        }
        _ => node(app, ui, rc, n),
    }
}

/// Excel 的二级压缩：分组内的大按钮改成 24px 小按钮，排成 3–4 列的小网格。
/// 组名仍然在底部，命令一个不少；真正放不下时才由调用方退化成「组名 ▾」。
fn group_compact(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, g: &Group) {
    let w = g.compact_w;
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
                            node_compact(app, ui, rc, n);
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
    if crate::debug_flags::panel_log() {
        eprintln!(
            "[compact] {:20} leaves={} target_w={w:.0} actual={:.0} h={:.0}",
            g.label,
            leaves.len(),
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

fn node_compact(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, n: &Node) {
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
            button(app, ui, rc, &c, false);
        }
        Node::Toggle(c) => {
            let c = icon_ctl(c);
            button(app, ui, rc, &c, true);
        }
        Node::Split { ctl, menu } => {
            let mut ctl = icon_ctl(ctl);
            ctl.width = Some(28.0);
            split(app, ui, rc, &ctl, menu);
        }
        Node::Menu(m) => {
            let c = icon_ctl(&m.ctl);
            menu_a11y(app, ui, rc, &c, m, None);
        }
        Node::DropDown { ctl, items } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            ctl.width = Some(ctl.width.unwrap_or(84.0).min(84.0));
            dropdown(app, ui, rc, &ctl, items);
        }
        Node::Gallery { ctl, items, .. } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            ctl.width = Some(ctl.width.unwrap_or(100.0).min(100.0));
            gallery(app, ui, rc, &ctl, items);
        }
        Node::ComboBox { ctl, items, width } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            combo(app, ui, rc, &ctl, items, (*width).min(90.0));
        }
        Node::EditBox {
            ctl,
            hint,
            password,
            width,
        } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            edit(app, ui, rc, &ctl, hint, *password, (*width).min(90.0));
        }
        Node::Slider { ctl, min, max } => {
            let mut ctl = ctl.clone();
            ctl.size = Size::Normal;
            slider(app, ui, rc, &ctl, *min, *max);
        }
        Node::Box { .. } | Node::Label(_) | Node::Spacer(_) | Node::Separator => {}
    }
}

fn node(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, n: &Node) {
    match n {
        Node::Button(c) => button(app, ui, rc, c, false),
        Node::Toggle(c) => button(app, ui, rc, c, true),
        Node::Split { ctl, menu } => split(app, ui, rc, ctl, menu),
        Node::Menu(m) => menu(app, ui, rc, m),
        Node::DropDown { ctl, items } => dropdown(app, ui, rc, ctl, items),
        Node::Gallery { ctl, items, .. } => gallery(app, ui, rc, ctl, items),
        Node::ComboBox { ctl, items, width } => combo(app, ui, rc, ctl, items, *width),
        Node::EditBox {
            ctl,
            hint,
            password,
            width,
        } => edit(app, ui, rc, ctl, hint, *password, *width),
        Node::Label(l) => label(app, ui, rc, l),
        Node::Slider { ctl, min, max } => slider(app, ui, rc, ctl, *min, *max),
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
                    nodes(app, ui, rc, children);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    nodes(app, ui, rc, children);
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

#[inline]
fn layout_text(ui: &egui::Ui, text: &str, size: f32) -> std::sync::Arc<egui::Galley> {
    let col = if ui.is_enabled() {
        ui_kit::text_color(ui.visuals())
    } else {
        ui_kit::disabled_color(crate::theme::palette())
    };
    ui.painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(size), col)
}

fn measure(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    layout_text(ui, text, size).size().x
}

fn draw_keytip(ui: &egui::Ui, key: &str, rect: Rect) {
    if key.is_empty() {
        return;
    }
    if crate::debug_flags::keytip_log() {
        eprintln!("[keytip] draw {key} at {rect:?}");
    }
    let w = (key.chars().count() as f32 * 6.0 + 6.0).max(15.0);
    let b = Rect::from_min_size(
        egui::pos2(rect.right() - w - 2.0, rect.top() + 2.0),
        Vec2::new(w, 14.0),
    );
    ui.painter()
        .rect_filled(b, 2.0, Color32::from_rgb(68, 68, 68));
    ui.painter().text(
        b.center(),
        Align2::CENTER_CENTER,
        key,
        FontId::proportional(11.0),
        Color32::WHITE,
    );
}

#[inline]
fn attach_lazy_tip(resp: egui::Response, st_tip: Option<&str>, c: &Ctl) -> egui::Response {
    if resp.hovered() || resp.contains_pointer() {
        let tip = st_tip.map(str::to_owned).unwrap_or_else(|| tip_of(c));
        if !tip.is_empty() {
            return resp.on_hover_text(tip);
        }
    }
    resp
}

fn button(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, c: &Ctl, toggle: bool) {
    button_a11y(app, ui, rc, c, toggle, None);
}

fn button_a11y(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    c: &Ctl,
    toggle: bool,
    a11y_full: Option<&str>,
) {
    let st = app.ctl_state_with_style(&c.id, rc.cur);
    if !st.visible {
        return;
    }
    let label: &str = st.label.as_deref().or(c.label.as_deref()).unwrap_or("");
    let icon = c.image.as_deref().and_then(icon_of);
    let on = toggle && st.checked;
    let enabled = on_enabled(c, &st);

    let resp = ui.add_enabled_ui(enabled, |ui| {
        if matches!(c.id.as_str(), "Bold" | "Italic" | "Underline") {
            let r = ui_kit::text_toggle(
                ui,
                if c.id == "Bold" {
                    "B"
                } else if c.id == "Italic" {
                    "I"
                } else {
                    "U"
                },
                on,
                "",
                c.id == "Bold",
                c.id == "Italic",
                c.id == "Underline",
            );
            return attach_lazy_tip(r, st.tip.as_deref(), c);
        }
        if matches!(c.id.as_str(), "FillColor" | "FontColor") {
            let col = st.swatch.unwrap_or(Color32::from_rgb(128, 128, 128));
            let r = ui_kit::color_button(
                ui,
                if c.id == "FontColor" { Icon::FontColor } else { Icon::Fill },
                col,
                "",
            );
            return attach_lazy_tip(r, st.tip.as_deref(), c);
        }
        match c.size {
            Size::Large => {
                let galley = (!label.is_empty()).then(|| layout_text(ui, label, 12.0));
                let tw = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
                let w = c.width.unwrap_or(44.0).max(tw + 14.0);
                let ic = icon.unwrap_or(Icon::Grid);
                let r = ui_kit::big_sized_galley(ui, ic, galley, Vec2::new(w, 76.0));
                attach_lazy_tip(r, st.tip.as_deref(), c)
            }
            Size::Normal => {
                if label.is_empty() {
                    let r = ui_kit::small_on(ui, icon.unwrap_or(Icon::Grid), on, "");
                    ui_kit::focus_ring(ui, &r, r.rect, 3.0);
                    attach_lazy_tip(r, st.tip.as_deref(), c)
                } else {
                    let galley = layout_text(ui, label, 12.0);
                    let min_w = (16.0 + 5.0 + galley.size().x + 12.0).max(24.0);
                    let w = c.width.unwrap_or(min_w).max(min_w);
                    let r = ui_kit::labeled_w_on_galley(
                        ui,
                        icon.unwrap_or(Icon::Grid),
                        Some(galley),
                        w,
                        on,
                        "",
                    );
                    ui_kit::focus_ring(ui, &r, r.rect, 3.0);
                    attach_lazy_tip(r, st.tip.as_deref(), c)
                }
            }
        }
    });
    // UI-24.2：功能区按钮在 UIA 里要有名字/角色/禁用状态（未启用 AccessKit 时零分配跳过）
    if rc.a11y {
        let tip = st.tip.as_deref().map(str::to_owned).unwrap_or_else(|| tip_of(c));
        let name = a11y_full.or_else(|| (!label.is_empty()).then_some(label)).unwrap_or(&tip);
        ui_kit::a11y_name(
            &resp.inner,
            name,
            enabled,
            if toggle { Some(on) } else { None },
        );
        if !tip.is_empty() && name != tip {
            let _ = ui.ctx().accesskit_node_builder(resp.inner.id, |node| node.set_description(tip));
        }
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

fn split(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    ctl: &Ctl,
    menu_def: &Menu,
) {
    split_a11y(app, ui, rc, ctl, menu_def, None);
}

fn split_a11y(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    ctl: &Ctl,
    menu_def: &Menu,
    a11y_full: Option<&str>,
) {
    let st = app.ctl_state_with_style(&ctl.id, rc.cur);
    if !st.visible {
        return;
    }
    let label: &str = st.label.as_deref().or(ctl.label.as_deref()).unwrap_or("");
    let icon = ctl.image.as_deref().and_then(icon_of).unwrap_or(Icon::Grid);
    let on = st.checked;
    let enabled = on_enabled(ctl, &st);
    let mut open = false;
    let mut pos = egui::pos2(0.0, 0.0);
    let mut keytip_rect: Option<Rect> = None;
    let main_clicked;

    match ctl.size {
        Size::Large => {
            let galley = (!label.is_empty()).then(|| layout_text(ui, label, 12.0));
            let tw = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
            let w = ctl.width.unwrap_or(44.0).max(tw + 14.0);
            let (r, ar) = ui.add_enabled_ui(enabled, |ui| {
                ui_kit::big_sized_split_galley(ui, icon, galley, Vec2::new(w, 76.0))
            }).inner;
            // UI-24.2：大号拆分按钮的主体和箭头是两个点击区，各自要有名字
            if rc.a11y {
                let a11y_lbl = a11y_full.unwrap_or(label);
                if !a11y_lbl.is_empty() {
                    let mut main = r.rect;
                    main.max.y = main.max.y.min(ar.rect.top());
                    ui_kit::a11y_name_rect(&r, a11y_lbl, enabled, main, ui.ctx());
                }
                ui_kit::a11y_name(&ar, &format!("{a11y_lbl} 选项"), enabled, None);
            }
            keytip_rect = Some(r.rect);
            main_clicked = enabled && r.clicked();
            if ar.clicked() && enabled {
                open = true;
                pos = egui::pos2(ar.rect.left(), ar.rect.bottom());
            }
        }
        Size::Normal => {
            let aw = ctl.arrow.unwrap_or(24.0).max(24.0);
            let (mr, ar) = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let main = ui.add_enabled_ui(enabled, |ui| {
                        let r = match ctl.id.as_str() {
                            "Bold" => ui_kit::text_toggle(ui, "B", on, "", true, false, false),
                            "Italic" => ui_kit::text_toggle(ui, "I", on, "", false, true, false),
                            "Underline" => {
                                ui_kit::text_toggle(ui, "U", on, "", false, false, true)
                            }
                            "FillColor" | "FontColor" => {
                                let col = st.swatch.unwrap_or(Color32::from_rgb(128, 128, 128));
                                ui_kit::color_button(
                                    ui,
                                    icon,
                                    col,
                                    "",
                                )
                            }
                            _ if label.is_empty() => ui_kit::small_on(ui, icon, on, ""),
                            _ => {
                                let galley = layout_text(ui, label, 12.0);
                                let min_w = (16.0 + 5.0 + galley.size().x + 10.0).max(24.0);
                                let w = ctl.width.unwrap_or(min_w).max(min_w);
                                ui_kit::labeled_w_on_galley(ui, icon, Some(galley), w, on, "")
                            }
                        };
                        if !matches!(ctl.id.as_str(), "Bold" | "Italic" | "Underline" | "FillColor" | "FontColor") {
                            ui_kit::focus_ring(ui, &r, r.rect, 3.0);
                        }
                        attach_lazy_tip(r, st.tip.as_deref(), ctl)
                    });
                    let ar = ui
                        .add_enabled_ui(enabled, |ui| {
                            let r = ui_kit::arrow_glyph_button(
                                ui,
                                Vec2::new(aw, 24.0),
                                crate::theme::palette().text_weak,
                            );
                            ui_kit::focus_ring(ui, &r, r.rect, 3.0);
                            r
                        })
                        .inner;
                    (main.inner, ar)
                })
                .inner;
            keytip_rect = Some(mr.rect);
            if rc.a11y || ar.hovered() {
                let name = a11y_full.filter(|s| !s.is_empty())
                    .or_else(|| (!label.is_empty()).then_some(label))
                    .or(ctl.screentip.as_deref()).unwrap_or(ctl.action());
                let options = format!("{name} 选项");
                if rc.a11y {
                    ui_kit::a11y_name(&mr, name, enabled, (ctl.id == "Underline").then_some(on));
                    ui_kit::a11y_name(&ar, &options, enabled, None);
                }
                if ar.hovered() {
                    ar.clone().on_hover_text(options);
                }
            }
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

fn menu(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, m: &Menu) {
    menu_a11y(app, ui, rc, &m.ctl, m, None);
}

fn menu_a11y(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    ctl: &Ctl,
    menu_def: &Menu,
    a11y_full: Option<&str>,
) {
    let st = app.ctl_state_with_style(&ctl.id, rc.cur);
    if !st.visible {
        return;
    }
    let label: &str = st.label.as_deref().or(ctl.label.as_deref()).unwrap_or("");
    let enabled = on_enabled(ctl, &st);
    let icon = ctl.image.as_deref().and_then(icon_of);
    if ctl.size == Size::Large {
        let galley = (!label.is_empty()).then(|| layout_text(ui, label, 12.0));
        let tw = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
        let min_large = if a11y_full.is_some() { 46.0 } else { 62.0 };
        let w = ctl.width.unwrap_or(min_large).max(tw + 14.0);
        let ic = icon.unwrap_or(Icon::Grid);
        let (r, ar) = ui.add_enabled_ui(enabled, |ui| {
            ui_kit::big_sized_split_galley(ui, ic, galley, Vec2::new(w, 76.0))
        }).inner;
        if rc.a11y {
            let a11y_lbl = a11y_full.unwrap_or(label);
            let mut main = r.rect;
            main.max.y = main.max.y.min(ar.rect.top());
            ui_kit::a11y_name_rect(&r, a11y_lbl, enabled, main, ui.ctx());
            ui_kit::a11y_name(&ar, &format!("{a11y_lbl} 选项"), enabled, None);
        }
        if (r.clicked() || ar.clicked()) && enabled {
            app.ribbon_sub = None;
            app.ribbon_popup = Some(Popup {
                kind: PopupKind::Menu(menu_def.clone()),
                pos: egui::pos2(ar.rect.left(), ar.rect.bottom()),
                opened_at: ui.input(|i| i.time),
            });
        }
        return;
    }
    let clicked = ui.add_enabled_ui(enabled, |ui| {
        let galley = (!label.is_empty()).then(|| layout_text(ui, label, 12.0));
        let tw = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
        let min_w = if label.is_empty() {
            28.0
        } else if icon.is_some() {
            (16.0 + 5.0 + tw + 22.0).max(28.0)
        } else {
            (tw + 24.0).max(28.0)
        };
        let w = ctl.width.unwrap_or(min_w).max(min_w);
        let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::click());
        let pal = crate::theme::palette();
        let bg = ui_kit::anim_state_bg(
            ui.ctx(),
            r.id,
            enabled,
            false,
            r.hovered(),
            r.is_pointer_button_down_on(),
            pal.theme.is_dark(),
        );
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, 4.0, bg);
        }
        let dis = ui_kit::disabled_color(pal);
        let ink = if enabled { pal.text } else { dis };
        let mut content = rect.shrink2(Vec2::new(if icon.is_some() { 6.0 } else { 8.0 }, 0.0));
        content.max.x = rect.right() - 14.0;
        if let Some(ic) = icon {
            let glyph = if galley.is_some() {
                Rect::from_center_size(content.left_center() + Vec2::new(8.0, 0.0), Vec2::splat(16.0))
            } else {
                Rect::from_center_size(content.center(), Vec2::splat(14.0))
            };
            crate::ui_icons::draw_control(ui, glyph, ic, ink, enabled);
            content.min.x = glyph.right() + 5.0;
        }
        if let Some(galley) = galley {
            let pos = Align2::LEFT_CENTER.anchor_size(content.left_center(), galley.size()).min;
            ui.painter_at(content).galley(pos, galley, ink);
        }
        ui_kit::caret(ui, egui::pos2(rect.right() - 7.0, rect.center().y + 0.5),
            if enabled { pal.text_weak } else { dis });
        attach_lazy_tip(r, st.tip.as_deref(), ctl)
    });
    if rc.a11y {
        let name = a11y_full.or_else(|| (!label.is_empty()).then_some(label))
            .or(ctl.screentip.as_deref()).unwrap_or(ctl.action());
        ui_kit::a11y_name(&clicked.inner, name, enabled, None);
    }
    ui_kit::focus_ring(ui, &clicked.inner, clicked.inner.rect, 4.0);
    if clicked.inner.clicked() {
        app.ribbon_sub = None;
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Menu(menu_def.clone()),
            pos: clicked.inner.rect.left_bottom(),
            opened_at: ui.input(|i| i.time),
        });
    }
}

/// Value choices sit on a quiet, crisp resting input surface (`grid_bg` + soft 1px `chrome_separator`
/// border) so combos and dropdowns read as clear controls without visual clutter.
fn paint_value_selector(ui: &egui::Ui, resp: &egui::Response, popup: Option<&Popup>) {
    let rect = resp.rect;
    let open = popup.is_some_and(|p| matches!(p.kind, PopupKind::Items(_)) && p.pos.distance_sq(rect.left_bottom()) < 1.0);
    let pal = crate::theme::palette();
    let hov_t = ui_kit::anim_hover(ui.ctx(), resp.id.with("val_sel_hov"), resp.hovered() && !open);
    let bg = if open || resp.is_pointer_button_down_on() {
        pal.pressed
    } else {
        ui_kit::lerp_color(pal.grid_bg, pal.hover, hov_t)
    };
    let border_col = if open || resp.has_focus() {
        pal.accent
    } else {
        ui_kit::lerp_color(pal.chrome_separator(), pal.card_border, hov_t)
    };
    ui.painter().rect(
        rect,
        4.0,
        bg,
        egui::Stroke::new(1.0, border_col),
        egui::StrokeKind::Inside,
    );
}

fn dropdown(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    c: &Ctl,
    items: &[Item],
) {
    let st = app.ctl_state_with_style(&c.id, rc.cur);
    if !st.visible {
        return;
    }
    let text: &str = st
        .text
        .as_deref()
        .or(c.label.as_deref())
        .or_else(|| items.first().map(|i| i.label.as_str()))
        .unwrap_or("");
    let enabled = on_enabled(c, &st);
    let clicked = ui.add_enabled_ui(enabled, |ui| {
        let galley = layout_text(ui, text, 12.0);
        let width = c
            .width
            .unwrap_or_else(|| 96.0_f32.max(galley.size().x + 30.0));
        let (rect, r) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
        let enabled = ui.is_enabled();
        let pal = crate::theme::palette();
        paint_value_selector(ui, &r, app.ribbon_popup.as_ref());
        let pos = Align2::LEFT_CENTER
            .anchor_size(egui::pos2(rect.left() + 7.0, rect.center().y), galley.size())
            .min;
        let dis = ui_kit::disabled_color(pal);
        ui.painter().galley(
            pos,
            galley,
            if enabled { pal.text } else { dis },
        );
        ui_kit::caret(
            ui,
            egui::pos2(rect.right() - 8.0, rect.center().y),
            if enabled { pal.text_weak } else { dis },
        );
        ui_kit::focus_ring(ui, &r, rect, 4.0);
        attach_lazy_tip(r, st.tip.as_deref(), c)
    });
    // UI-24.2：自绘下拉在 UIA 里要有名字（当前值 + 说明）
    if rc.a11y {
        let tip = tip_of(c);
        let name = if tip.is_empty() {
            format!("{text}（下拉）")
        } else {
            format!("{text}，{tip}")
        };
        ui_kit::a11y_name(&clicked.inner, &name, enabled, None);
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

fn combo(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    c: &Ctl,
    items: &[Item],
    width: f32,
) {
    // 字体、字号是 Excel 的「可编辑组合框」，外观单独做
    match c.id.as_str() {
        "Font" => return font_combo(app, ui, rc, width),
        "FontSize" => return size_combo(app, ui, rc, width),
        _ => {}
    }
    let st = app.ctl_state_with_style(&c.id, rc.cur);
    if !st.visible {
        return;
    }
    let cur: &str = st
        .text
        .as_deref()
        .or_else(|| items.first().map(|i| i.label.as_str()))
        .unwrap_or("");
    let enabled = on_enabled(c, &st);
    let clicked = ui.add_enabled_ui(enabled, |ui| {
        let w = c.width.unwrap_or(width).max(52.0);
        let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::click());
        let pal = crate::theme::palette();
        paint_value_selector(ui, &r, app.ribbon_popup.as_ref());
        let dis = ui_kit::disabled_color(pal);
        ui.painter().text(
            egui::pos2(rect.left() + 7.0, rect.center().y),
            Align2::LEFT_CENTER,
            cur,
            FontId::proportional(12.0),
            if enabled { pal.text } else { dis },
        );
        ui_kit::caret(
            ui,
            egui::pos2(rect.right() - 8.0, rect.center().y),
            if enabled { pal.text_weak } else { dis },
        );
        ui_kit::focus_ring(ui, &r, rect, 4.0);
        attach_lazy_tip(r, st.tip.as_deref(), c)
    });
    if rc.a11y {
        let tip = tip_of(c);
        let name = if tip.is_empty() {
            format!("{cur}（下拉）")
        } else {
            format!("{cur}，{tip}")
        };
        ui_kit::a11y_name(&clicked.inner, &name, enabled, None);
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

fn edit(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    c: &Ctl,
    hint: &str,
    password: bool,
    width: f32,
) {
    let st = app.ctl_state_with_style(&c.id, rc.cur);
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
        let resp = ui.add(
            egui::TextEdit::singleline(buf)
                .hint_text(hint)
                .desired_width(width)
                .password(password),
        );
        let _ = attach_lazy_tip(resp, st.tip.as_deref(), c);
    });
}

fn label(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, l: &LabelCtl) {
    let st = app.ctl_state_with_style(&l.id, rc.cur);
    if !st.visible {
        return;
    }
    let text: &str = st.text.as_deref().or(l.label.as_deref()).unwrap_or("");
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
            ui.label(egui::RichText::new(text).size(l.size).color(crate::theme::palette().text_weak));
        });
        return;
    }

    if l.id == "AreaLabel" {
        let pal = crate::theme::palette();
        ui.add_space(12.0);
        let cw = 76.0_f32.max(measure(ui, text, 12.0) + 18.0);
        let (cr, _) = ui.allocate_exact_size(Vec2::new(cw, 46.0), Sense::hover());
        ui.painter().rect(
            cr,
            6.0,
            pal.card_bg,
            egui::Stroke::new(1.0, pal.card_border),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            egui::pos2(cr.center().x, cr.top() + 13.0),
            Align2::CENTER_CENTER,
            "当前区域",
            FontId::proportional(10.0),
            pal.text_weak,
        );
        ui.painter().text(
            egui::pos2(cr.center().x, cr.bottom() - 13.0),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(12.0),
            pal.info_fg,
        );
        return;
    }

    if let Some(w) = l.width {
        // 页面布局那三行「宽度:/高度:/缩放:」是定宽标签 + 定宽下拉
        ui.add_sized(
            [w, 21.0],
            egui::Label::new(egui::RichText::new(text).size(l.size)),
        );
        return;
    }

    let mut t = egui::RichText::new(text).size(l.size);
    if mono {
        t = t.monospace().color(crate::theme::palette().text);
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
    let pal = crate::theme::palette();
    let dis = ui_kit::disabled_color(pal);
    let ink = if enabled { pal.text } else { dis };
    let secondary = if enabled { pal.text_weak } else { dis };
    let sc_g = (!shortcut.is_empty()).then(|| ui.painter().layout_no_wrap(
        shortcut.to_owned(), FontId::proportional(11.0), secondary,
    ));
    let sc_w = sc_g.as_ref().map(|g| g.size().x + 16.0).unwrap_or(0.0);
    let left_pad = if icon_gutter { 30.0 } else { 10.0 };
    let right_pad = if has_sub { 24.0 } else { 12.0 };
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), FontId::proportional(12.0), ink);
    job.wrap = egui::text::TextWrapping::truncate_at_width((row_w - left_pad - right_pad - sc_w).max(1.0));
    let label_g = ui.fonts(|f| f.layout_job(job));
    let clipped = label_g.elided;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(row_w, 27.0), sense);
    let hov_t = ui_kit::anim_hover(ui.ctx(), resp.id.with("pmr_hov"), enabled && resp.hovered() && !active_sub);
    let sub_t = ui_kit::anim_hover(ui.ctx(), resp.id.with("pmr_sub"), enabled && active_sub);
    let bg = if enabled && resp.is_pointer_button_down_on() {
        pal.pressed
    } else {
        let base = ui_kit::lerp_color(Color32::TRANSPARENT, pal.hover, hov_t);
        ui_kit::lerp_color(base, pal.accent_tint, sub_t)
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 4.0, bg);
    }
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
            crate::ui_icons::draw_excel(ui.painter(), ir, ic, dis, enabled);
        }
    }
    let lpos = Align2::LEFT_CENTER
        .anchor_size(egui::pos2(rect.left() + left_pad, rect.center().y), label_g.size())
        .min;
    ui.painter().galley(lpos, label_g, ink);
    if has_sub {
        ui_kit::caret_right(
            ui,
            egui::pos2(rect.right() - 10.0, rect.center().y),
            secondary,
        );
    } else if let Some(sg) = sc_g {
        let spos = Align2::RIGHT_CENTER
            .anchor_size(egui::pos2(rect.right() - 10.0, rect.center().y), sg.size())
            .min;
        ui.painter().galley(spos, sg, secondary);
    }
    ui_kit::a11y_name(&resp, label, enabled, None);
    ui_kit::focus_ring(ui, &resp, rect, 4.0);
    if resp.gained_focus() { resp.scroll_to_me(None); }
    if clipped { resp.on_hover_text(label) } else { resp }
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
                .color(crate::theme::palette().text_weak),
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
                    Vec2::new(((row_w - 22.0) / 5.0).clamp(1.0, 38.0), 32.0),
                    if paste_on { Sense::click() } else { Sense::hover() },
                );
                let pal = crate::theme::palette();
                let hov_t = ui_kit::anim_hover(ui.ctx(), resp.id.with("pv_hov"), paste_on && resp.hovered());
                let fill = if paste_on && resp.is_pointer_button_down_on() {
                    pal.pressed
                } else {
                    ui_kit::lerp_color(pal.card_bg, pal.hover, hov_t)
                };
                let stroke_col = ui_kit::lerp_color(pal.card_border, pal.accent, hov_t);
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
                    ui_kit::disabled_color(pal),
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
    let (sr, _) = ui.allocate_exact_size(Vec2::new(row_w, 1.0), Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(sr.left() + 6.0, sr.center().y),
            egui::pos2(sr.right() - 6.0, sr.center().y),
        ],
        egui::Stroke::new(1.0, crate::theme::palette().card_border),
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
        let pal = crate::theme::palette();
        let color = Color32::from_rgb(app.last_border_color[0], app.last_border_color[1], app.last_border_color[2]);
        for &(id, label) in crate::border_lines::STYLES {
            let row = popup_menu_row(ui, label, None, None, "", false, id == app.border_brush_style, true, min_w, false);
            let end = egui::pos2(row.rect.right() - 13.0, row.rect.center().y);
            let start = end - Vec2::new(62.0, 0.0);
            let preview = Rect::from_two_pos(start, end).expand2(Vec2::new(4.0, 6.0));
            let background = if egui::Rgba::from(color).intensity() > 0.25 {
                Color32::from_gray(45)
            } else {
                Color32::from_gray(246)
            };
            ui.painter().rect(preview, 2.0, background, egui::Stroke::new(1.0, pal.card_border), egui::StrokeKind::Inside);
            crate::border_lines::paint(ui.painter(), start, end, id, color, 1.0);
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
                    egui::Stroke::new(1.0, crate::theme::palette().card_border),
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
                let enabled = on_enabled(c, &st);
                let r = popup_menu_row(ui, &label, icon, sw, sc, false, checked, enabled, min_w, icon_gutter);
                if menu_id == "BordersMenu" && c.image.is_none() {
                    border_preset_icon(ui, r.rect, c.action());
                }
                if r.hovered() {
                    *hover_non_sub = true;
                }
                if r.clicked() && enabled {
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
                let enabled = on_enabled(c, &st);
                let r = ui_kit::check_row(ui, &label, st.checked, enabled, min_w);
                if r.hovered() {
                    *hover_non_sub = true;
                }
                if r.clicked() && enabled {
                    *fire = Some(c.action().to_string());
                    *close = true;
                }
            }
            MenuNode::Menu(sub) => {
                let st = app.ctl_state(&sub.ctl.id);
                if !st.visible {
                    continue;
                }
                let label = st.label.as_deref().or(sub.ctl.label.as_deref()).unwrap_or("");
                let icon = sub.ctl.image.as_deref().and_then(icon_of);
                let is_open = active_sub_id == Some(sub.ctl.id.as_str());
                let enabled = on_enabled(&sub.ctl, &st);
                let r = popup_menu_row(ui, label, icon, None, "", true, is_open, enabled, min_w, icon_gutter);
                if enabled && (r.hovered() || r.clicked()) {
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
    let mut bounds = screen.shrink(5.0);
    bounds.min.y = (screen.top() + 28.0).min(bounds.bottom());
    let frame = ui_kit::popup_frame(ui.style());
    let margin = frame.total_margin().sum();
    let max_width = (bounds.width() - margin.x).max(1.0);
    let max_height = (bounds.height() - margin.y).clamp(1.0, 390.0);
    let (width, scroll_key) = match &p.kind {
        PopupKind::Menu(m) => {
            let min_w = if m.ctl.id == "PasteMenu" { 216.0 } else if menu_has_gutter(m) { 192.0 } else { 176.0 };
            (measure_menu_width(app, ui, m, min_w), m.ctl.id.as_str())
        }
        PopupKind::Items(items) => (measure_items_width(ui, items), items.first().map_or("", |i| i.id.as_str())),
    };
    let frame_width = width.min(max_width);

    let mut close = false;
    let mut fire: Option<String> = None;
    let mut want_sub: Option<(String, Menu, egui::Pos2)> = None;
    let mut hover_non_sub = false;
    let active_sub_id = app.ribbon_sub.as_ref().map(|(_, s, _)| s.ctl.id.clone());

    let pop_id = egui::Id::new("ribbon_popup");
    let enter = ui_kit::anim_enter(&ctx, pop_id);
    let draw_pos = egui::pos2(p.pos.x, p.pos.y + (1.0 - enter) * 3.0);
    let area = egui::Area::new(pop_id)
        .fixed_pos(draw_pos)
        .constrain_to(bounds)
        .order(egui::Order::Foreground)
        .show(&ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(frame_width);
                ui.set_max_height(max_height);
                ui.spacing_mut().item_spacing.y = 2.0;
                egui::ScrollArea::vertical().id_salt(scroll_key).max_height(max_height).show(ui, |ui| {
                    let inner_w = ui.available_width().min(frame_width);
                    match &p.kind {
                        PopupKind::Menu(m) => {
                            let gutter = menu_has_gutter(m);
                            if m.ctl.id == "PasteMenu" {
                                paste_visual_grid(app, ui, inner_w, &mut fire, &mut close);
                            }
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
                        }
                        PopupKind::Items(items) => {
                            let is_styles = items.first().is_some_and(|i| i.id.starts_with("Style"));
                            for it in items {
                                if is_styles {
                                    let (bg, fg, bold) = style_preview(&it.id);
                                    let (rect, r) =
                                        ui.allocate_exact_size(Vec2::new(inner_w, 27.0), Sense::click());
                                    let pal = crate::theme::palette();
                                    let row_bg = ui_kit::anim_state_bg(
                                        ui.ctx(),
                                        r.id,
                                        true,
                                        false,
                                        r.hovered(),
                                        r.is_pointer_button_down_on(),
                                        pal.theme.is_dark(),
                                    );
                                    if row_bg != Color32::TRANSPARENT {
                                        ui.painter().rect_filled(rect, 4.0, row_bg);
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
                                        if bold {
                                            ui_kit::ui_bold_in(ui.ctx(), 11.0)
                                        } else {
                                            FontId::proportional(11.0)
                                        },
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
                        }
                    }
                });
            });
        });
    let rect = area.response.rect;

    if let Some((pid, sub, spos)) = want_sub {
        app.ribbon_sub = Some((pid, sub, spos));
    }

    let mut sub_rect: Option<Rect> = None;
    if let Some((pid, sub, mut spos)) = app.ribbon_sub.clone() {
        let sub_gutter = menu_has_gutter(&sub);
        let width = if sub.ctl.id == "BorderColorMenu" { 218.0 } else {
            measure_menu_width(app, ui, &sub, if sub_gutter { 186.0 } else { 170.0 })
        }.min(max_width);
        let sub_outer_w = width + margin.x;
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
        let mut dummy_sub: Option<(String, Menu, egui::Pos2)> = None;
        let mut dummy_hover = false;
        let sub_pop_id = egui::Id::new("ribbon_sub_popup");
        let sub_enter = ui_kit::anim_enter(&ctx, sub_pop_id.with(&sub.ctl.id));
        let sub_area = egui::Area::new(sub_pop_id)
            .fixed_pos(egui::pos2(spos.x, spos.y + (1.0 - sub_enter) * 3.0))
            .constrain_to(bounds)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(width);
                    ui.set_max_height(max_height);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    egui::ScrollArea::vertical().id_salt(&sub.ctl.id).max_height(max_height).show(ui, |ui| {
                        let sub_inner_w = ui.available_width().min(width);
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
                    });
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

fn font_combo(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, width: f32) {
    let cur = rc.cur;
    ui.spacing_mut().interact_size.y = 21.0;
    let fid = egui::Id::new("font_edit");
    // UI-24.2：框里要显示真实的字体名（默认字体时以前是空串，读屏读不到）
    let shown_font: &str = if cur.font.is_empty() {
        "思源黑体"
    } else {
        cur.font.as_str()
    };
    let focused = ui.memory(|m| m.has_focus(fid));
    if !focused && app.font_buf != shown_font {
        app.font_buf.clear();
        app.font_buf.push_str(shown_font);
    }
    let mut lost = false;
    let mut arrow_pos = egui::pos2(0.0, 0.0);
    let mut arrow_clicked = false;
    {
        let pal = crate::theme::palette();
        let (crect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
        let hovered = ui.rect_contains_pointer(crect);
        let active = focused || app.font_menu;
        let hov_t = ui_kit::anim_hover(ui.ctx(), fid.with("combo_hov"), hovered && !active);
        let stroke_col = if active {
            pal.accent
        } else {
            ui_kit::lerp_color(pal.chrome_separator(), pal.card_border, hov_t)
        };
        let fill_col = if active {
            pal.grid_bg
        } else {
            ui_kit::lerp_color(pal.grid_bg, pal.hover, hov_t)
        };
        ui.painter().rect(
            crect,
            4.0,
            fill_col,
            egui::Stroke::new(1.0, stroke_col),
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
        let mut fr = child.add_sized(
            Vec2::new(text_rect.width(), 20.0),
            egui::TextEdit::singleline(&mut app.font_buf)
                .id(fid)
                .frame(false)
                // 空字体时不再用灰色 hint（Excel 框里永远是具体的字体名）
                .hint_text(""),
        );
        if fr.hovered() || fr.contains_pointer() {
            fr = fr.on_hover_text("字体名，回车应用");
        }
        if !focused && app.font_buf.is_empty() {
            ui.painter().text(
                egui::pos2(text_rect.left(), text_rect.center().y),
                egui::Align2::LEFT_CENTER,
                "思源黑体",
                egui::FontId::proportional(12.0),
                crate::theme::palette().text,
            );
        }
        lost = fr.lost_focus();
        // UI-24.2：输入框在 UIA 里要有名字，否则读屏只念「编辑 思源黑体」
        if rc.a11y {
            let _ = ui.ctx().accesskit_node_builder(fid, |node| {
                node.set_label("字体".to_string());
            });
        }
        let ar = ui.interact(arrow_rect, egui::Id::new("font_arrow"), Sense::click());
        if rc.a11y {
            ui_kit::a11y_name(&ar, "字体 下拉", true, None);
        }
        arrow_pos = egui::pos2(crect.left(), crect.bottom());
        let c = arrow_rect.center();
        ui_kit::caret(ui, c, crate::theme::palette().text_weak);
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
        let font_list_id = egui::Id::new("font_list");
        let font_enter = ui_kit::anim_enter(&ctx, font_list_id);
        let area = egui::Area::new(font_list_id)
            .fixed_pos(egui::pos2(arrow_pos.x, arrow_pos.y + 3.0 + (1.0 - font_enter) * 3.0))
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

fn size_combo(app: &mut SheetApp, ui: &mut egui::Ui, rc: RibbonFrameCtx<'_>, width: f32) {
    let cur = rc.cur;
    ui.spacing_mut().interact_size.y = 21.0;
    let sid = egui::Id::new("size_edit");
    if !ui.memory(|m| m.has_focus(sid)) {
        let want_i = cur.size.round() as i32;
        if app.size_buf.parse::<i32>().ok() != Some(want_i) {
            app.size_buf.clear();
            let _ = std::fmt::Write::write_fmt(&mut app.size_buf, format_args!("{want_i}"));
        }
    }
    let mut lost = false;
    let mut arrow_pos = egui::pos2(0.0, 0.0);
    let mut arrow_clicked = false;
    {
        let pal = crate::theme::palette();
        let (crect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
        let hovered = ui.rect_contains_pointer(crect);
        let size_focused = ui.memory(|m| m.has_focus(sid));
        let active = size_focused || app.size_menu;
        let hov_t = ui_kit::anim_hover(ui.ctx(), sid.with("combo_hov"), hovered && !active);
        let stroke_col = if active {
            pal.accent
        } else {
            ui_kit::lerp_color(pal.chrome_separator(), pal.card_border, hov_t)
        };
        let fill_col = if active {
            pal.grid_bg
        } else {
            ui_kit::lerp_color(pal.grid_bg, pal.hover, hov_t)
        };
        ui.painter().rect(
            crect,
            4.0,
            fill_col,
            egui::Stroke::new(1.0, stroke_col),
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
        let mut sr = child.add_sized(
            Vec2::new(text_rect.width(), 20.0),
            egui::TextEdit::singleline(&mut app.size_buf)
                .id(sid)
                .frame(false),
        );
        if sr.hovered() || sr.contains_pointer() {
            sr = sr.on_hover_text("字号（磅），回车应用");
        }
        lost = sr.lost_focus();
        if rc.a11y {
            let _ = ui.ctx().accesskit_node_builder(sid, |node| {
                node.set_label("字号".to_string());
            });
        }
        let ar = ui.interact(arrow_rect, egui::Id::new("size_arrow"), Sense::click());
        if rc.a11y {
            ui_kit::a11y_name(&ar, "字号 下拉", true, None);
        }
        arrow_pos = egui::pos2(crect.left(), crect.bottom());
        let c = arrow_rect.center();
        ui_kit::caret(ui, c, crate::theme::palette().text_weak);
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
        let size_list_id = egui::Id::new("size_list");
        let size_enter = ui_kit::anim_enter(&ctx, size_list_id);
        let area = egui::Area::new(size_list_id)
            .fixed_pos(egui::pos2(arrow_pos.x, arrow_pos.y + 3.0 + (1.0 - size_enter) * 3.0))
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

fn slider(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    c: &Ctl,
    min: f32,
    max: f32,
) {
    let st = app.ctl_state_with_style(&c.id, rc.cur);
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
        app.set_zoom_interactive(z.clamp(min, max), ui.ctx());
    }
}

fn gallery(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    c: &Ctl,
    items: &[Item],
) {
    let st = app.ctl_state_with_style(&c.id, rc.cur);
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
        if rc.a11y {
            ui_kit::a11y_name(&resp, &it.label, enabled, None);
        }
        let pal = crate::theme::palette();
        let (bg, fg, bold) = style_preview(&it.id);
        let bg = if resp.hovered() { bg.gamma_multiply(0.94) } else { bg };
        ui.painter().rect(
            r,
            2.0,
            bg,
            egui::Stroke::new(1.0, pal.card_border),
            egui::StrokeKind::Inside,
        );
        // 像 Excel 的样式缩略图：一块样例底纹 + Aa + 两条模拟网格线
        if bg != Color32::WHITE {
            let band = Rect::from_min_max(r.min, egui::pos2(r.right(), r.top() + 6.0));
            ui.painter().rect_filled(band, 0.0, bg.gamma_multiply(0.85));
        }
        let pos = egui::pos2(r.center().x, r.top() + 22.0);
        let font = if bold {
            ui_kit::ui_bold_in(ui.ctx(), 13.0)
        } else {
            FontId::proportional(12.0)
        };
        ui.painter().text(pos, Align2::CENTER_CENTER, "Aa", font, fg);
        let line = egui::Stroke::new(1.0, pal.chrome_separator());
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
            &it.label,
            FontId::proportional(11.0),
            pal.text_weak,
        );
        if resp.clicked() && enabled {
            fire = Some(it.id.clone());
        }
    }
    let arrow_rect = Rect::from_min_max(egui::pos2(rect.right() - arrow_w, rect.top()), rect.max);
    let ar = ui.interact(arrow_rect, ui.id().with(("galarrow", &c.id)), Sense::click());
    if rc.a11y {
        let tip_buf;
        let base: &str = if let Some(l) = c.label.as_deref().filter(|s| !s.is_empty()) {
            l
        } else if let Some(t) = st.tip.as_deref().filter(|s| !s.is_empty()) {
            t
        } else {
            tip_buf = tip_of(c);
            if tip_buf.is_empty() { &c.id } else { &tip_buf }
        };
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
    let mut bounds = ui.available_rect_before_wrap();
    bounds.min.y += 12.0;
    bounds.max.y = bounds.top() + 24.0;
    let mut row = ui.new_child(egui::UiBuilder::new().id_salt("collapsed_groups").max_rect(bounds)
        .layout(egui::Layout::left_to_right(egui::Align::Center)));
    row.spacing_mut().interact_size.y = 24.0;
    let pal = crate::theme::palette();
    if pal.theme != crate::theme::AppTheme::HighContrast {
        let inactive = &mut row.visuals_mut().widgets.inactive;
        inactive.weak_bg_fill = Color32::TRANSPARENT;
        inactive.bg_stroke = egui::Stroke::NONE;
    }
    let response = row.menu_button(format!("{label}    "), add_contents).response;
    let rect = response.rect;
    ui_kit::focus_ring(&row, &response, rect, 4.0);
    ui_kit::caret(&mut row, egui::pos2(rect.right() - 9.0, rect.center().y), pal.text_weak);
    ui.advance_cursor_after_rect(rect);
}

/// 紧凑模式的样式库：整条 349px 的缩略图放不下时，收成与「条件格式 / 套用表格格式」同高的
/// 76px 大号拆分按钮（不再是一条悬浮在顶边的 24px 裸文字按钮），三枚样式命令整齐并排。
fn gallery_compact(
    app: &mut SheetApp,
    ui: &mut egui::Ui,
    rc: RibbonFrameCtx<'_>,
    c: &Ctl,
    items: &[Item],
) {
    let st = app.ctl_state_with_style(&c.id, rc.cur);
    if !st.visible {
        return;
    }
    let full_label: &str = c
        .label
        .as_deref()
        .or(st.label.as_deref())
        .filter(|l| !l.is_empty())
        .unwrap_or("单元格样式");
    let vis_label: &str = if rc.mid && full_label == "单元格样式" {
        "样式"
    } else {
        full_label
    };
    let enabled = on_enabled(c, &st);
    let ic = c.image.as_deref().and_then(icon_of).unwrap_or(Icon::CellStyle);
    let (r, ar) = ui
        .add_enabled_ui(enabled, |ui| {
            let galley = layout_text(ui, vis_label, 12.0);
            let w = (galley.size().x + 14.0).max(if vis_label != full_label { 46.0 } else { 62.0 });
            ui_kit::big_sized_split_galley(ui, ic, Some(galley), Vec2::new(w, 76.0))
        })
        .inner;
    if rc.a11y {
        let mut main = r.rect;
        main.max.y = main.max.y.min(ar.rect.top());
        ui_kit::a11y_name_rect(&r, full_label, enabled, main, ui.ctx());
        ui_kit::a11y_name(&ar, &format!("{full_label} 展开"), enabled, None);
    }
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
    fn value_selector_distinguishes_idle_open_and_high_contrast() {
        use crate::theme::{palette_for, set_current_theme, AppTheme};
        let original = crate::theme::current_theme();
        for theme in [AppTheme::Light, AppTheme::Dark, AppTheme::HighContrast] {
            set_current_theme(theme);
            let pal = palette_for(theme);
            let ctx = egui::Context::default();
            for open in [false, true] {
                let mut selector = Rect::NOTHING;
                let frame = ctx.run(Default::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let (rect, response) = ui.allocate_exact_size(Vec2::new(96.0, 24.0), Sense::click());
                        selector = rect;
                        let popup = open.then(|| Popup {
                            kind: PopupKind::Items(Vec::new()), pos: rect.left_bottom(), opened_at: 0.0,
                        });
                        paint_value_selector(ui, &response, popup.as_ref());
                    });
                });
                let face = frame.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(face) if face.rect == selector => Some(face),
                    _ => None,
                }).expect("value selector should paint a control surface");
                assert_eq!(face.fill, if open { pal.pressed } else { pal.grid_bg });
                let expected_stroke = if open { pal.accent } else { pal.chrome_separator() };
                assert_eq!(face.stroke, egui::Stroke::new(1.0, expected_stroke));
            }
        }
        set_current_theme(original);
    }

    #[test]
    fn ribbon_watch_and_calculated_column_are_clickable_above_the_group_caption() {
        let _env = crate::test_env::TestEnv::new("ribbon-command-columns");
        for (tab, command, caption) in [(4, "监视窗口", "公式审核"), (100, "填充计算列", "工具")] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut app = SheetApp::new_with_context(&ctx, None);
            app.show_start = false;
            app.ribbon_tab = tab;
            if tab == 100 {
                let sheet = app.wb.sheet_mut(0);
                sheet.tables.push(sheetcore::table::Table {
                    name: "Sales".into(), display: "Sales".into(), range: (0, 0, 2, 0),
                    columns: vec!["Total".into()], column_ids: vec![1], header_row: true, ..Default::default()
                });
                sheet.set(1, 0, sheetcore::model::Cell {
                    value: sheetcore::model::Value::Number(3.0), formula: Some("=1+2".into()),
                });
                app.sel = (1, 0);
                app.sel_end = app.sel;
            }
            let mut frame = eframe::Frame::_new_kittest();
            let mut advance = |app: &mut SheetApp, events| {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 850.0))),
                    events, ..Default::default()
                };
                ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame))
            };
            let mut hit = None;
            let mut caption_top = None;
            for _ in 0..4 {
                let out = advance(&mut app, vec![]);
                for (_, node) in out.platform_output.accesskit_update.unwrap().nodes {
                    if node.label() == Some(command) { hit = node.bounds(); }
                }
                caption_top = out.shapes.iter().find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == caption => Some(t.pos.y),
                    _ => None,
                });
            }
            let hit = hit.expect("command needs a named hit region");
            assert!(hit.y1 as f32 <= caption_top.expect("group caption") && hit.y1 - hit.y0 >= 23.0);
            let pos = egui::pos2((hit.x0 + hit.x1) as f32 / 2.0, (hit.y0 + hit.y1) as f32 / 2.0);
            for pressed in [true, false] {
                advance(&mut app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
                }]);
            }
            if tab == 4 { assert!(app.watch_open); }
            else { assert_eq!(app.wb.sheet(0).formula_of(2, 0), Some("=1+2")); }
        }
    }

    #[test]
    fn short_window_ribbon_popups_scroll_to_the_last_command_inside_the_frame() {
        let _env = crate::test_env::TestEnv::new("ribbon-short-popup");
        for items in [false, true] {
            let ctx = egui::Context::default();
            let mut app = SheetApp::new_with_context(&ctx, None);
            let target;
            let mut number_code = None;
            let kind = if items {
                let options = app.ribbon.find_dropdown("NumberFormat").unwrap();
                let last = options.last().unwrap();
                target = last.label.clone();
                number_code = Some(last.id.strip_prefix("fmt:").unwrap().to_owned());
                PopupKind::Items(options)
            } else {
                let menu = app.ribbon.find_menu("PasteMenu").unwrap();
                target = "选择性粘贴…".to_owned();
                let cell = sheetcore::model::Cell { value: sheetcore::model::Value::Number(1.0), formula: None };
                app.clip = Some(crate::Clip {
                    deferred: None, sheet: 0, rect: (0, 0, 0, 0), cells: vec![vec![(cell, 0)]],
                    merges: vec![], cut: false, source_rows: None, source_cols: None,
                });
                PopupKind::Menu(menu)
            };
            let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(506.0, 320.0));
            app.ribbon_popup = Some(Popup { kind, pos: screen.right_bottom(), opened_at: 0.0 });
            let mut time = 0.0;
            let mut render = |app: &mut SheetApp, events| {
                time += 1.0 / 30.0;
                ctx.run(egui::RawInput { time: Some(time), screen_rect: Some(screen), events, ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| popup(app, ui));
                })
            };
            let position = |out: &egui::FullOutput| out.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == target => {
                    let pos = t.pos + t.galley.size() * 0.5;
                    s.clip_rect.contains(pos).then_some(pos)
                }
                _ => None,
            });
            for _ in 0..4 { render(&mut app, vec![]); }
            let area = ctx.memory(|m| m.area_rect(egui::Id::new("ribbon_popup"))).unwrap();
            assert!(screen.shrink(4.0).contains_rect(area), "popup clipped: {area:?}");
            assert!(position(&render(&mut app, vec![])).is_none(), "last row should initially be below the scroll viewport");
            let pos = area.center();
            render(&mut app, vec![egui::Event::PointerMoved(pos), egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -850.0), modifiers: egui::Modifiers::NONE,
            }]);
            for _ in 0..16 { render(&mut app, vec![]); }
            let last = position(&render(&mut app, vec![])).expect("last command must scroll into view");
            for pressed in [true, false] {
                render(&mut app, vec![egui::Event::PointerMoved(last), egui::Event::PointerButton {
                    pos: last, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
                }]);
            }
            assert!(app.ribbon_popup.is_none());
            if let Some(code) = number_code {
                assert_eq!(app.current_style().format, code);
            } else {
                assert!(app.paste_special);
            }
        }
    }

    #[test]
    fn ribbon_color_split_swatch_matches_main_action_and_arrow_only_opens_choices() {
        let _env = crate::test_env::TestEnv::new("ribbon-color-split");
        let chosen = [42, 146, 78];
        let previous = [78, 96, 152];
        for enabled in [true, false] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut app = SheetApp::new_with_context(&ctx, None);
            app.apply_fill_color(Some(previous));
            app.last_fill_color = chosen;
            let ctl = Ctl {
                id: "FillColor".into(), image: Some("Fill".into()),
                screentip: Some("填充颜色".into()), arrow: Some(15.0), enabled,
                ..Default::default()
            };
            let menu = Menu { ctl: Ctl::default(), items: Vec::new() };
            let frame = |app: &mut SheetApp, events| {
                ctx.run(egui::RawInput { events, ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let cur = app.current_style();
                        let rc = RibbonFrameCtx { cur: &cur, mid: false, a11y: true };
                        split(app, ui, rc, &ctl, &menu);
                    });
                })
            };
            let output = frame(&mut app, vec![]);
            if enabled {
                let swatch = Color32::from_rgb(chosen[0], chosen[1], chosen[2]);
                assert_eq!(output.shapes.iter().filter(|s| matches!(
                    &s.shape, egui::Shape::Rect(r) if r.fill == swatch
                )).count(), 1);
            }
            let nodes = output.platform_output.accesskit_update.unwrap().nodes;
            let button = |name| {
                let node = &nodes.iter().find(|(_, node)| node.label() == Some(name)).unwrap().1;
                assert_eq!(node.is_disabled(), !enabled);
                let b = node.bounds().unwrap();
                Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32))
            };
            let main = button("填充颜色");
            let arrow = button("填充颜色 选项");
            assert!(main.right() <= arrow.left() && arrow.width() >= 24.0 && arrow.height() >= 24.0);
            let click = |app: &mut SheetApp, pos| {
                for pressed in [true, false] {
                    frame(app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
                    }]);
                }
            };
            click(&mut app, arrow.center());
            assert_eq!(app.color_popup, enabled.then_some(false));
            assert_eq!(app.current_style().fill, Some(previous));
            app.color_popup = None;
            click(&mut app, main.center());
            assert_eq!(app.current_style().fill, Some(if enabled { chosen } else { previous }));
            assert!(app.color_popup.is_none());
        }
    }

    #[test]
    fn contextual_tab_keeps_its_selected_surface_when_only_overflow_fits() {
        let _env = crate::test_env::TestEnv::new("ribbon-context-tab-surface");
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheet_mut(0).tables.push(sheetcore::table::Table {
            name: "Sales".into(), display: "Sales".into(), range: (0, 0, 2, 0),
            columns: vec!["Total".into()], column_ids: vec![1], header_row: true, ..Default::default()
        });
        app.ribbon_tab = 100;
        let label = app.ribbon.context.iter().find(|c| c.set_id == "TabSetTableTools").unwrap().tab.label.clone();
        let overflow_name = format!("更多选项卡，当前页为{label}");
        for theme in crate::theme::AppTheme::ALL {
            crate::theme::apply_app_theme(&ctx, theme);
            let pal = crate::theme::palette();
            let (fill, ink) = ui_kit::active_tab_colors(pal);
            for width in [1440.0, 48.0] {
                let out = ctx.run(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, 100.0))),
                    ..Default::default()
                }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| { ui.horizontal(|ui| tabs(&mut app, ui)); });
                });
                let selected = out.shapes.iter().find_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.fill == fill && r.rect.height() >= 18.0 => Some(r.rect),
                    _ => None,
                }).expect("the selected tab or its overflow trigger needs a full surface");
                if width > 100.0 {
                    assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t)
                        if t.galley.job.text == label && t.override_text_color == Some(ink)
                        && selected.contains(t.pos + 0.5 * t.galley.size()))));
                } else {
                    assert_eq!(selected.width(), 28.0);
                    assert!(out.platform_output.accesskit_update.unwrap().nodes.iter()
                        .any(|(_, node)| node.label() == Some(overflow_name.as_str())));
                }
                assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r)
                    if r.fill == pal.accent && r.rect.width() >= 14.0 && r.rect.height() <= 2.5)));
            }
        }
    }

    #[test]
    fn ribbon_overflow_selects_hidden_tab_and_double_click_collapses_it() {
        let _env = crate::test_env::TestEnv::new("ribbon-tab-overflow");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.ribbon_tab = 1;
        let last = app.ribbon.tabs.len() - 1;
        let last_label = app.ribbon.tabs[last].label.clone();
        let mut menu_button = egui::Pos2::ZERO;
        let mut time = 0.0;
        let mut frame = |app: &mut SheetApp, click: Option<(egui::Pos2, bool)>| {
            time += if click.is_none() { 1.0 } else { 0.016 };
            let mut events = Vec::new();
            if let Some((pos, pressed)) = click {
                events.push(egui::Event::PointerMoved(pos));
                events.push(egui::Event::PointerButton {
                    pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
                });
            }
            let out = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(420.0, 440.0))),
                events, time: Some(time), ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let bounds = Rect::from_min_size(ui.cursor().min, Vec2::new(156.0, ui_kit::TAB_H));
                    let mut row = ui.new_child(egui::UiBuilder::new()
                        .max_rect(bounds)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)));
                    row.spacing_mut().item_spacing.x = 0.0;
                    tabs(app, &mut row);
                    let button_id = row.make_persistent_id("tab_overflow").with("button");
                    let rect = ctx.read_response(button_id).expect("overflow button").rect;
                    assert!(rect.right() <= bounds.right() + 1.0);
                    menu_button = rect.center();
                });
            });
            let label_pos = out.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == last_label => Some(t.pos + t.galley.size() / 2.0),
                _ => None,
            });
            (menu_button, label_pos)
        };

        let (menu, hidden) = frame(&mut app, None);
        assert!(hidden.is_none());
        frame(&mut app, Some((menu, true)));
        frame(&mut app, Some((menu, false)));
        let (_, target) = frame(&mut app, None);
        let target = target.expect("last tab is in the overflow popup");
        frame(&mut app, Some((target, true)));
        frame(&mut app, Some((target, false)));
        assert_eq!(app.ribbon_tab, last);
        let (_, active) = frame(&mut app, None);
        let active = active.expect("selected tab is promoted into the visible strip");
        for pressed in [true, false, true, false] {
            frame(&mut app, Some((active, pressed)));
        }
        assert!(app.ribbon_collapsed);
    }

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
        let mut leaves = Vec::new();
        compact_leaves(&g.children, &mut leaves);
        let n = leaves.len();
        assert!(n >= 8, "对齐方式应有足够多的命令，实际 {n}");
        let w = g.compact_w;
        assert!((72.0..=210.0).contains(&w), "紧凑组宽度越界: {w}");
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
