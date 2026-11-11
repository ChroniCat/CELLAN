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
        if app.keytips {
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
        }
        if resp.clicked() {
            if i == 0 {
                app.backstage = true;
                app.backstage_page = 0;
            } else {
                app.ribbon_tab = i;
                app.ribbon_collapsed = false;
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
            }
        }
    }
}

/// 当前页签的功能区主体。
pub fn body(app: &mut SheetApp, ui: &mut egui::Ui) {
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
        n if n < app.ribbon.tabs.len() => Some(app.ribbon.tabs[n].clone()),
        _ => None,
    };
    let Some(tab) = tab else {
        return;
    };
    ui_kit::reset_groups();
    for g in &tab.groups {
        group(app, ui, g);
    }
    // 原 group_l 的语义：点分组右下角斜箭头 → 打开单元格格式
    if app.fmt_pending {
        app.fmt_pending = false;
        app.open_format_dialog();
    }
    popup(app, ui);
}

fn group(app: &mut SheetApp, ui: &mut egui::Ui, g: &Group) {
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
}

fn nodes(app: &mut SheetApp, ui: &mut egui::Ui, list: &[Node]) {
    for n in list {
        node(app, ui, n);
    }
}

fn node(app: &mut SheetApp, ui: &mut egui::Ui, n: &Node) {
    match n {
        Node::Button(c) => button(app, ui, c, false),
        Node::Toggle(c) => button(app, ui, c, true),
        Node::Split { ctl, menu } => split(app, ui, ctl, menu),
        Node::Menu(m) => menu(app, ui, m),
        Node::DropDown { ctl, items } => dropdown(app, ui, ctl, items),
        Node::ComboBox { ctl, items, width } => combo(app, ui, ctl, items, *width),
        Node::EditBox {
            ctl,
            hint,
            password,
            width,
        } => edit(app, ui, ctl, hint, *password, *width),
        Node::Label(l) => label(app, ui, l),
        Node::Box { vertical, children } => {
            if *vertical {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(2.0);
                    nodes(app, ui, children);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    nodes(app, ui, children);
                });
            }
        }
        Node::Separator => ui_kit::hairline(ui),
    }
}

// ---------------------------------------------------------------- 控件

fn tip_of(c: &Ctl) -> String {
    match (&c.screentip, &c.supertip) {
        (Some(a), Some(b)) => format!("{a}\n{b}"),
        (Some(a), None) => a.clone(),
        (None, Some(b)) => b.clone(),
        (None, None) => String::new(),
    }
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

fn button(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, toggle: bool) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    let label = st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
    let tip = st.tip.clone().unwrap_or_else(|| tip_of(c));
    let icon = c.image.as_deref().and_then(icon_of);
    let on = toggle && st.checked;

    let resp = ui.add_enabled_ui(st.enabled, |ui| {
        // Excel 内置的字体类开关：字本身就是图标
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
        // 颜色按钮：图标/字母 + 底部色条
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
                let w = measure(ui, &label, 12.0).max(28.0) + 14.0;
                let w = w.max(42.0);
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
                    let ic = icon.unwrap_or(Icon::Grid);
                    ui_kit::small_on(ui, ic, on, &tip)
                } else {
                    let w = 16.0 + 4.0 + measure(ui, &label, 12.0) + 10.0;
                    let ic = icon.unwrap_or(Icon::Grid);
                    ui_kit::labeled_w_on(ui, ic, &label, w.max(24.0), on, &tip)
                }
            }
        }
    });
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
    let mut open = false;
    let mut pos = egui::pos2(0.0, 0.0);
    let main_clicked;

    match ctl.size {
        Size::Large => {
            let w = (measure(ui, &label, 12.0).max(28.0) + 14.0).max(42.0);
            let (r, ar) = ui_kit::big_sized_split(ui, icon, &label, Vec2::new(w, 76.0));
            main_clicked = r.clicked();
            if ar.clicked() {
                open = true;
                pos = egui::pos2(ar.rect.left(), ar.rect.bottom());
            }
        }
        Size::Normal => {
            let r = ui.add_enabled_ui(st.enabled, |ui| {
                if label.is_empty() {
                    ui_kit::small_on(ui, icon, on, &tip)
                } else {
                    let w = 16.0 + 4.0 + measure(ui, &label, 12.0) + 10.0;
                    ui_kit::labeled_w_on(ui, icon, &label, w.max(24.0), on, &tip)
                }
            });
            main_clicked = r.inner.clicked();
            // Excel 的 split 右侧那条窄箭头
            let (rect, ar) = ui.allocate_exact_size(Vec2::new(12.0, 24.0), Sense::click());
            if ar.hovered() {
                ui.painter()
                    .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            ui_kit::caret(ui, rect.center(), ui.visuals().text_color());
            if ar.clicked() {
                open = true;
                pos = rect.left_bottom();
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
    let clicked = ui.add_enabled_ui(st.enabled, |ui| {
        let w = if let Some(ic) = icon {
            let w = 16.0 + 4.0 + measure(ui, &label, 12.0) + 16.0;
            ui_kit::labeled_w(ui, ic, &label, w.max(24.0), &tip)
        } else {
            let w = measure(ui, &label, 12.0) + 26.0;
            ui_kit::labeled_w(ui, Icon::Grid, &label, w.max(24.0), &tip)
        };
        w
    });
    if clicked.inner.clicked() {
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Menu(m.clone()),
            pos: clicked.inner.rect.left_bottom(),
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
    let w = c
        .label
        .as_ref()
        .map(|_| 0.0)
        .unwrap_or(0.0);
    let _ = w;
    let clicked = ui.add_enabled_ui(st.enabled, |ui| {
        let width = 96.0_f32.max(measure(ui, &text, 12.0) + 30.0);
        let (rect, r) = ui.allocate_exact_size(Vec2::new(width, 21.0), Sense::click());
        let hover = r.hovered();
        ui.painter().rect(
            rect,
            2.0,
            Color32::WHITE,
            egui::Stroke::new(1.0, ui_kit::BORDER),
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
            ui.visuals().text_color(),
        );
        ui_kit::caret(
            ui,
            egui::pos2(rect.right() - 8.0, rect.center().y),
            Color32::from_rgb(68, 68, 68),
        );
        r.on_hover_text(tip)
    });
    if clicked.inner.clicked() {
        app.ribbon_popup = Some(Popup {
            kind: PopupKind::Items(items.to_vec()),
            pos: clicked.inner.rect.left_bottom(),
        });
    }
}

fn combo(app: &mut SheetApp, ui: &mut egui::Ui, c: &Ctl, items: &[Item], width: f32) {
    // 字体、字号是 Excel 的「可编辑组合框」，外观单独做
    match c.id.as_str() {
        "Font" => return font_combo(app, ui),
        "FontSize" => return size_combo(app, ui),
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
    ui.add_enabled_ui(st.enabled, |ui| {
        ui.spacing_mut().interact_size.y = 21.0;
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
    ui.add_enabled_ui(st.enabled, |ui| {
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
    let chip = l.style.as_deref() == Some("chip");
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        if chip {
            // 表格样式缩略：五段主题绿，和 Excel 的表样式预览同一路数
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
            let mut t = egui::RichText::new(&text).size(l.size);
            t = t.color(ui_kit::TEXT_WEAK);
            ui.label(t);
            return;
        }
        let mut t = egui::RichText::new(&text).size(l.size);
        if mono {
            t = t.monospace();
        }
        if weak {
            t = t.color(ui_kit::TEXT_WEAK);
        } else if mono {
            t = t.color(ui_kit::TEXT);
        }
        ui.label(t);
    });
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
                        for it in &m.items {
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
                                    let text =
                                        format!("{} {}", if st.checked { "✓" } else { "　" }, label);
                                    let btn =
                                        egui::Button::new(egui::RichText::new(text).size(12.0))
                                            .min_size(Vec2::new(160.0, 22.0))
                                            .frame(false);
                                    let r = ui.add_enabled(st.enabled, btn);
                                    if r.clicked() {
                                        fire = Some(c.action().to_string());
                                        close = true;
                                    }
                                }
                                MenuNode::Menu(sub) => {
                                    let label = sub.ctl.label.clone().unwrap_or_default();
                                    let r = ui.add(
                                        egui::Button::new(egui::RichText::new(format!("{label} ▶")).size(12.0))
                                            .min_size(Vec2::new(160.0, 22.0))
                                            .frame(false),
                                    );
                                    if r.clicked() {
                                        app.ribbon_popup = Some(Popup {
                                            kind: PopupKind::Menu(sub.clone()),
                                            pos: egui::pos2(r.rect.right(), r.rect.top()),
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
    if close
        || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        || (ctx.input(|i| i.pointer.any_click())
            && !ctx
                .input(|i| i.pointer.interact_pos())
                .map(|p| rect.contains(p))
                .unwrap_or(false))
    {
        app.ribbon_popup = None;
    }
}

// ---------------------------------------------------------------- 字体/字号框

fn font_combo(app: &mut SheetApp, ui: &mut egui::Ui) {
    let cur = app.current_style();
    ui.spacing_mut().interact_size.y = 21.0;
    let fid = egui::Id::new("font_edit");
    if !ui.memory(|m| m.has_focus(fid)) {
        app.font_buf = cur.font.clone();
    }
    let mut lost = false;
    let mut arrow_pos = egui::pos2(0.0, 0.0);
    let mut arrow_clicked = false;
    {
        let (crect, _) = ui.allocate_exact_size(Vec2::new(160.0, 21.0), Sense::hover());
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
        let fr = child
            .add_sized(
                Vec2::new(text_rect.width(), 18.0),
                egui::TextEdit::singleline(&mut app.font_buf)
                    .id(fid)
                    .frame(false)
                    .hint_text("默认"),
            )
            .on_hover_text("字体名，回车应用");
        lost = fr.lost_focus();
        let ar = ui.interact(arrow_rect, egui::Id::new("font_arrow"), Sense::click());
        arrow_pos = egui::pos2(crect.left(), crect.bottom());
        let c = arrow_rect.center();
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x - 4.0, c.y - 2.0),
                egui::pos2(c.x + 4.0, c.y - 2.0),
                egui::pos2(c.x, c.y + 3.0),
            ],
            Color32::from_rgb(68, 68, 68),
            egui::Stroke::NONE,
        ));
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

fn size_combo(app: &mut SheetApp, ui: &mut egui::Ui) {
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
        let (crect, _) = ui.allocate_exact_size(Vec2::new(52.0, 21.0), Sense::hover());
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
                Vec2::new(text_rect.width(), 18.0),
                egui::TextEdit::singleline(&mut app.size_buf)
                    .id(sid)
                    .frame(false),
            )
            .on_hover_text("字号（磅），回车应用");
        lost = sr.lost_focus();
        let ar = ui.interact(arrow_rect, egui::Id::new("size_arrow"), Sense::click());
        arrow_pos = egui::pos2(crect.left(), crect.bottom());
        let c = arrow_rect.center();
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x - 4.0, c.y - 2.0),
                egui::pos2(c.x + 4.0, c.y - 2.0),
                egui::pos2(c.x, c.y + 3.0),
            ],
            Color32::from_rgb(68, 68, 68),
            egui::Stroke::NONE,
        ));
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
        "TraceIn" => Icon::TraceIn,
        "TraceOut" => Icon::TraceOut,
        _ => return None,
    })
}
