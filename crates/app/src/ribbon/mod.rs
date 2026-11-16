//! 功能区：由 MS-CUSTOMUI 形状的 `assets/ribbon/customUI.xml` 声明。
//!
//! 这里只做「读 XML → 模型」；渲染在 `ui`，命令分发在 `actions`。
//! 元素/属性名沿用 [MS-CUSTOMUI]：customUI/ribbon/tabs/tab/group/box/
//! button/toggleButton/splitButton/menu/menuSeparator/dropDown/comboBox/
//! editBox/labelControl/separator/dialogBoxLauncher。
//!
//! 少数 Excel 里本来由宿主（Office）提供的动态能力，用回调状态实现：
//! 控件 id 就是 idMso 名（Bold/Font/MergeCenter…），状态由 `SheetApp::ctl_state`
//! 按 id 给出（enabled/checked/visible/label/text/swatch），等价于 MS-CUSTOMUI
//! 的 getEnabled/getPressed/getVisible/getLabel/getText/getImage。

pub mod actions;
pub mod ui;

use std::collections::HashMap;

/// 顶层文档。
#[derive(Clone, Debug, Default)]
pub struct Ribbon {
    pub tabs: Vec<Tab>,
    /// 上下文页签（表设计 / 图表工具）：选中表格或图表时才出现。
    pub context: Vec<ContextTab>,
}

#[derive(Clone, Debug, Default)]
pub struct Tab {
    pub id: String,
    pub label: String,
    pub keytip: String,
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, Default)]
pub struct ContextTab {
    /// `<tabSet idMso="TabSetTableTools">`
    pub set_id: String,
    pub set_label: String,
    pub tab: Tab,
}

#[derive(Clone, Debug, Default)]
pub struct Group {
    pub id: String,
    pub label: String,
    pub children: Vec<Node>,
    pub launcher: Option<Ctl>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    #[default]
    Normal,
    Large,
}

/// 控件公共属性（MS-CUSTOMUI 的 button/toggleButton/… 共用那一套）。
#[derive(Clone, Debug, Default)]
pub struct Ctl {
    pub id: String,
    pub label: Option<String>,
    pub image: Option<String>,
    pub size: Size,
    /// 显式宽度（像素）。Excel 的组宽是实测调出来的，自动排版会跑偏；
    /// MS-CUSTOMUI 只有 editBox/comboBox 有 sizeString，这里对所有控件都认。
    pub width: Option<f32>,
    /// split 右侧箭头区的宽度（旧版「▼」按 menu_button 走是 28，「tight」是 16）
    pub arrow: Option<f32>,
    pub screentip: Option<String>,
    pub supertip: Option<String>,
    pub keytip: Option<String>,
    /// 没写就按 id 分发。
    pub on_action: Option<String>,
    pub enabled: bool,
}

impl Ctl {
    pub fn action(&self) -> &str {
        self.on_action.as_deref().unwrap_or(&self.id)
    }
}

/// 控件运行态：等价于 MS-CUSTOMUI 的 getEnabled/getVisible/getPressed/
/// getLabel/getText/getImage 回调结果。
#[derive(Clone, Debug)]
pub struct CtlState {
    pub visible: bool,
    pub enabled: bool,
    pub checked: bool,
    /// 动态文字（覆盖 XML 的 label）
    pub label: Option<String>,
    /// 文本控件的当前值（comboBox/editBox/dropDown 的选中项）
    pub text: Option<String>,
    /// 颜色按钮下方的色条（getImage 的动态版）
    pub swatch: Option<eframe::egui::Color32>,
    /// 动态提示（screentip 覆盖）
    pub tip: Option<String>,
}

impl Default for CtlState {
    fn default() -> Self {
        Self {
            visible: true,
            enabled: true,
            checked: false,
            label: None,
            text: None,
            swatch: None,
            tip: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Node {
    Button(Ctl),
    Toggle(Ctl),
    Split { ctl: Ctl, menu: Menu },
    Menu(Menu),
    DropDown { ctl: Ctl, items: Vec<Item> },
    Gallery {
        ctl: Ctl,
        items: Vec<Item>,
        columns: u32,
    },
    ComboBox {
        ctl: Ctl,
        items: Vec<Item>,
        width: f32,
    },
    EditBox {
        ctl: Ctl,
        hint: String,
        password: bool,
        width: f32,
    },
    Label(LabelCtl),
    /// MS-CUSTOMUI 没有滑块；视图>缩放这一格沿用原来的 egui Slider。
    Slider { ctl: Ctl, min: f32, max: f32 },
    Box {
        vertical: bool,
        top: f32,
        gap: f32,
        children: Vec<Node>,
    },
    /// 横向 2px 分隔（旧版行内的 add_space，Excel 用固定间距，MS-CUSTOMUI 没有对应元素）
    Spacer(f32),
    Separator,
}

#[derive(Clone, Debug, Default)]
pub struct Menu {
    pub ctl: Ctl,
    pub items: Vec<MenuNode>,
}

#[derive(Clone, Debug)]
pub enum MenuNode {
    Button(Ctl),
    Toggle(Ctl),
    Menu(Menu),
    Separator,
}

#[derive(Clone, Debug, Default)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub image: Option<String>,
    pub screentip: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LabelCtl {
    pub id: String,
    pub label: Option<String>,
    /// mono / weak / chip
    pub style: Option<String>,
    pub size: f32,
    pub width: Option<f32>,
}

/// 编译进包的文档。解析失败是编译产物的问题，直接炸出来，别静默降级。
pub fn load() -> Ribbon {
    parse(include_str!("../../assets/ribbon/customUI.xml"))
        .unwrap_or_else(|e| panic!("customUI.xml 解析失败：{e}"))
}

// ---------------------------------------------------------------- 解析

#[derive(Debug, Default)]
struct El {
    name: String,
    attrs: HashMap<String, String>,
    children: Vec<El>,
}

fn parse(src: &str) -> Result<Ribbon, String> {
    let root = read_tree(src)?;
    let ribbon = child(&root, "ribbon").ok_or("缺少 <ribbon>")?;
    let mut out = Ribbon::default();
    if let Some(tabs) = child(ribbon, "tabs") {
        for t in tabs.children.iter().filter(|e| e.name == "tab") {
            out.tabs.push(read_tab(t)?);
        }
    }
    if let Some(ctx) = child(ribbon, "contextualTabs") {
        for set in ctx
            .children
            .iter()
            .filter(|e| e.name == "tabSet")
        {
            for t in set.children.iter().filter(|e| e.name == "tab") {
                out.context.push(ContextTab {
                    set_id: set.attrs.get("idMso").cloned().unwrap_or_default(),
                    set_label: set.attrs.get("label").cloned().unwrap_or_default(),
                    tab: read_tab(t)?,
                });
            }
        }
    }
    if out.tabs.is_empty() {
        return Err("没有页签".into());
    }
    Ok(out)
}

impl Ribbon {
    /// 按菜单 id 找菜单（弹出层只存 id + 位置，不复制整棵树）。
    pub fn find_menu(&self, id: &str) -> Option<Menu> {
        fn in_nodes(nodes: &[Node], id: &str) -> Option<Menu> {
            for n in nodes {
                match n {
                    Node::Split { menu, .. } => {
                        if menu.ctl.id == id {
                            return Some(menu.clone());
                        }
                        if let Some(m) = in_menu(menu, id) {
                            return Some(m);
                        }
                    }
                    Node::Menu(m) => {
                        if m.ctl.id == id {
                            return Some(m.clone());
                        }
                        if let Some(x) = in_menu(m, id) {
                            return Some(x);
                        }
                    }
                    Node::Box { children, .. } => {
                        if let Some(m) = in_nodes(children, id) {
                            return Some(m);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        fn in_menu(m: &Menu, id: &str) -> Option<Menu> {
            for i in &m.items {
                if let MenuNode::Menu(sub) = i {
                    if sub.ctl.id == id {
                        return Some(sub.clone());
                    }
                    if let Some(x) = in_menu(sub, id) {
                        return Some(x);
                    }
                }
            }
            None
        }
        for t in &self.tabs {
            for g in &t.groups {
                if let Some(m) = in_nodes(&g.children, id) {
                    return Some(m);
                }
            }
        }
        for c in &self.context {
            for g in &c.tab.groups {
                if let Some(m) = in_nodes(&g.children, id) {
                    return Some(m);
                }
            }
        }
        None
    }

    /// 按控件 id 找下拉项（截图探针与调试用）。
    pub fn find_dropdown(&self, id: &str) -> Option<Vec<Item>> {
        fn walk(nodes: &[Node], id: &str) -> Option<Vec<Item>> {
            for n in nodes {
                match n {
                    Node::DropDown { ctl, items } | Node::ComboBox { ctl, items, .. }
                        if ctl.id == id =>
                    {
                        return Some(items.clone());
                    }
                    Node::Box { children, .. } => {
                        if let Some(v) = walk(children, id) {
                            return Some(v);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        for t in &self.tabs {
            for g in &t.groups {
                if let Some(v) = walk(&g.children, id) {
                    return Some(v);
                }
            }
        }
        None
    }
}

fn read_tab(e: &El) -> Result<Tab, String> {
    let mut tab = Tab {
        id: attr(e, "id").unwrap_or_default(),
        label: attr(e, "label").unwrap_or_default(),
        keytip: attr(e, "keytip").unwrap_or_default(),
        groups: Vec::new(),
    };
    if tab.id.is_empty() {
        return Err("tab 缺 id".into());
    }
    for g in e.children.iter().filter(|c| c.name == "group") {
        tab.groups.push(read_group(g)?);
    }
    Ok(tab)
}

fn read_group(e: &El) -> Result<Group, String> {
    let mut group = Group {
        id: attr(e, "id").unwrap_or_default(),
        label: attr(e, "label").unwrap_or_default(),
        children: Vec::new(),
        launcher: None,
    };
    if group.id.is_empty() {
        return Err(format!("group「{}」缺 id", group.label));
    }
    for c in &e.children {
        match c.name.as_str() {
            "dialogBoxLauncher" => {
                if let Some(b) = child(c, "button") {
                    group.launcher = Some(read_ctl(b));
                }
            }
            _ => {
                if let Some(n) = read_node(c)? {
                    group.children.push(n);
                }
            }
        }
    }
    Ok(group)
}

fn read_node(e: &El) -> Result<Option<Node>, String> {
    Ok(Some(match e.name.as_str() {
        "button" => Node::Button(read_ctl(e)),
        "toggleButton" => Node::Toggle(read_ctl(e)),
        "splitButton" => {
            // 视觉属性（label/image/size）在 splitButton 上，命令与状态在它包着的
            // button/toggleButton 上——照 MS-CUSTOMUI 的结构拆开取。
            let outer = read_ctl(e);
            let mut ctl = child(e, "button")
                .or_else(|| child(e, "toggleButton"))
                .map(read_ctl)
                .unwrap_or_else(|| outer.clone());
            if ctl.label.is_none() {
                ctl.label = outer.label.clone();
            }
            if ctl.image.is_none() {
                ctl.image = outer.image.clone();
            }
            if ctl.screentip.is_none() {
                ctl.screentip = outer.screentip.clone();
            }
            ctl.size = outer.size;
            if ctl.width.is_none() {
                ctl.width = outer.width;
            }
            if ctl.arrow.is_none() {
                ctl.arrow = outer.arrow;
            }
            if ctl.keytip.is_none() {
                ctl.keytip = outer.keytip.clone();
            }
            let menu = child(e, "menu")
                .map(read_menu)
                .transpose()?
                .unwrap_or_else(|| Menu {
                    ctl: Ctl {
                        id: format!("{}Menu", outer.id),
                        ..Default::default()
                    },
                    items: Vec::new(),
                });
            Node::Split { ctl, menu }
        }
        "menu" => Node::Menu(read_menu(e)?),
        "gallery" => Node::Gallery {
            ctl: read_ctl(e),
            items: read_items(e),
            columns: attr(e, "columns")
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
        },
        "dropDown" => Node::DropDown {
            ctl: read_ctl(e),
            items: read_items(e),
        },
        "comboBox" => Node::ComboBox {
            ctl: read_ctl(e),
            items: read_items(e),
            width: attr(e, "sizeString")
                .and_then(|s| s.parse().ok())
                .unwrap_or(120.0),
        },
        "editBox" => Node::EditBox {
            ctl: read_ctl(e),
            hint: attr(e, "hint").unwrap_or_default(),
            password: attr(e, "password").as_deref() == Some("true"),
            width: attr(e, "sizeString")
                .and_then(|s| s.parse().ok())
                .unwrap_or(120.0),
        },
        "labelControl" => Node::Label(LabelCtl {
            id: attr(e, "id").unwrap_or_default(),
            label: attr(e, "label"),
            style: attr(e, "style"),
            size: attr(e, "fontSize")
                .and_then(|s| s.parse().ok())
                .unwrap_or(11.0),
            width: attr(e, "sizeString").and_then(|s| s.parse().ok()),
        }),
        "box" => {
            let vertical = attr(e, "boxStyle").as_deref() == Some("vertical");
            let top = attr(e, "paddingTop")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let gap = attr(e, "gap").and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let mut children = Vec::new();
            for c in &e.children {
                if let Some(n) = read_node(c)? {
                    children.push(n);
                }
            }
            Node::Box {
                vertical,
                top,
                gap,
                children,
            }
        }
        "spacer" => Node::Spacer(
            attr(e, "sizeString")
                .and_then(|s| s.parse().ok())
                .unwrap_or(2.0),
        ),
        "slider" => Node::Slider {
            ctl: read_ctl(e),
            min: attr(e, "min").and_then(|v| v.parse().ok()).unwrap_or(0.5),
            max: attr(e, "max").and_then(|v| v.parse().ok()).unwrap_or(2.5),
        },
        "separator" => Node::Separator,
        // 未知元素不致命：跳过，方便以后按需扩。
        _ => return Ok(None),
    }))
}

fn read_menu(e: &El) -> Result<Menu, String> {
    let mut m = Menu {
        ctl: read_ctl(e),
        items: Vec::new(),
    };
    for c in &e.children {
        match c.name.as_str() {
            "menuSeparator" => m.items.push(MenuNode::Separator),
            "button" => m.items.push(MenuNode::Button(read_ctl(c))),
            "toggleButton" => m.items.push(MenuNode::Toggle(read_ctl(c))),
            "menu" => m.items.push(MenuNode::Menu(read_menu(c)?)),
            _ => {}
        }
    }
    Ok(m)
}

fn read_items(e: &El) -> Vec<Item> {
    e.children
        .iter()
        .filter(|c| c.name == "item")
        .map(|c| Item {
            id: attr(c, "id").unwrap_or_default(),
            label: attr(c, "label").unwrap_or_default(),
            image: attr(c, "imageMso"),
            screentip: attr(c, "screentip"),
        })
        .collect()
}

fn read_ctl(e: &El) -> Ctl {
    Ctl {
        id: attr(e, "id").unwrap_or_default(),
        label: attr(e, "label").filter(|s| !s.is_empty()),
        image: attr(e, "imageMso"),
        size: match attr(e, "size").as_deref() {
            Some("large") => Size::Large,
            _ => Size::Normal,
        },
        width: attr(e, "sizeString")
            .or_else(|| attr(e, "width"))
            .and_then(|s| s.parse().ok()),
        arrow: attr(e, "arrowWidth").and_then(|s| s.parse().ok()),
        screentip: attr(e, "screentip").filter(|s| !s.is_empty()),
        supertip: attr(e, "supertip").filter(|s| !s.is_empty()),
        keytip: attr(e, "keytip").filter(|s| !s.is_empty()),
        on_action: attr(e, "onAction"),
        enabled: attr(e, "enabled").as_deref() != Some("false"),
    }
}

fn attr(e: &El, k: &str) -> Option<String> {
    e.attrs.get(k).cloned()
}

fn child<'a>(e: &'a El, name: &str) -> Option<&'a El> {
    e.children.iter().find(|c| c.name == name)
}

/// 用 quick-xml 先建一棵通用树；文本节点对 ribbon 没意义，丢掉。
fn read_tree(src: &str) -> Result<El, String> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(src);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let mut el = El {
                    name: local(&e),
                    ..Default::default()
                };
                for a in e.attributes().flatten() {
                    let k = a.key.as_ref().to_string();
                    let k = k.split(':').next_back().unwrap_or(&k).to_string();
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    el.attrs.insert(k, v);
                }
                stack.push(el);
            }
            Ok(Event::Empty(e)) => {
                let mut el = El {
                    name: local(&e),
                    ..Default::default()
                };
                for a in e.attributes().flatten() {
                    let k = a.key.as_ref().to_string();
                    let k = k.split(':').next_back().unwrap_or(&k).to_string();
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    el.attrs.insert(k, v);
                }
                match stack.last_mut() {
                    Some(p) => p.children.push(el),
                    None => root = Some(el),
                }
            }
            Ok(Event::End(_)) => {
                let Some(el) = stack.pop() else {
                    return Err("结束标签多余".into());
                };
                match stack.last_mut() {
                    Some(p) => p.children.push(el),
                    None => root = Some(el),
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML 错误：{e}")),
            _ => {}
        }
    }
    root.ok_or_else(|| "空文档".to_string())
}

fn local(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.name().local_name().as_ref().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn all_ids(r: &Ribbon) -> Vec<String> {
        fn walk(nodes: &[Node], out: &mut Vec<String>) {
            for n in nodes {
                match n {
                    Node::Button(c) | Node::Toggle(c) => out.push(c.id.clone()),
                    Node::Split { ctl, menu } => {
                        out.push(ctl.id.clone());
                        menu_ids(menu, out);
                    }
                    Node::Menu(m) => menu_ids(m, out),
                    Node::DropDown { ctl, items } | Node::Gallery { ctl, items, .. } => {
                        out.push(ctl.id.clone());
                        out.extend(items.iter().map(|i| i.id.clone()));
                    }
                    Node::ComboBox { ctl, items, .. } => {
                        out.push(ctl.id.clone());
                        out.extend(items.iter().map(|i| i.id.clone()));
                    }
                    Node::EditBox { ctl, .. } => out.push(ctl.id.clone()),
                    Node::Label(l) => out.push(l.id.clone()),
                    Node::Slider { ctl, .. } => out.push(ctl.id.clone()),
                    Node::Box { children, .. } => walk(children, out),
                    Node::Spacer(_) | Node::Separator => {}
                }
            }
        }
        fn menu_ids(m: &Menu, out: &mut Vec<String>) {
            out.push(m.ctl.id.clone());
            for i in &m.items {
                match i {
                    MenuNode::Button(c) | MenuNode::Toggle(c) => out.push(c.id.clone()),
                    MenuNode::Menu(sub) => menu_ids(sub, out),
                    MenuNode::Separator => {}
                }
            }
        }
        let mut out = Vec::new();
        for t in &r.tabs {
            for g in &t.groups {
                out.push(g.id.clone());
                walk(&g.children, &mut out);
            }
        }
        for c in &r.context {
            out.push(c.tab.id.clone());
            for g in &c.tab.groups {
                out.push(g.id.clone());
                walk(&g.children, &mut out);
            }
        }
        out
    }

    #[test]
    fn parses_and_ids_are_unique() {
        let r = load();
        assert_eq!(r.tabs.len(), 9, "页签数");
        assert_eq!(r.context.len(), 3, "上下文页签数（表设计 / 迷你图 / 图表工具）");
        let ids = all_ids(&r);
        let mut seen = BTreeSet::new();
        for id in &ids {
            assert!(!id.is_empty(), "空 id");
            assert!(seen.insert(id.clone()), "重复 id：{id}");
        }
        assert!(ids.len() > 100, "控件太少：{}", ids.len());
    }

    #[test]
    fn tab_order_matches_excel() {
        let r = load();
        let labels: Vec<_> = r.tabs.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            ["文件", "开始", "插入", "页面布局", "公式", "数据", "审阅", "视图", "帮助"]
        );
        assert_eq!(r.context[0].tab.label, "表设计");
        assert_eq!(r.context[1].tab.label, "迷你图");
        assert_eq!(r.context[2].tab.label, "图表工具");
    }

    /// 从 ribbon_action 的 match 臂里抽出「精确命令名」集合。
    /// 以前是 `region.contains("\"Save\"")` —— 子串匹配会被别的命令名或注释骗过，
    /// 现在按 `"A" | "B" =>` 这种臂逐个取字面量。
    fn dispatched_actions() -> BTreeSet<String> {
        let src = include_str!("actions.rs");
        let region = &src[src.find("pub fn ribbon_action").expect("没有 ribbon_action")..];
        let mut out = BTreeSet::new();
        for line in region.lines() {
            let t = line.trim_start();
            if !t.starts_with('"') {
                continue;
            }
            let Some((lhs, _)) = t.split_once("=>") else { continue };
            for part in lhs.split('|') {
                let p = part.trim();
                if p.starts_with('"') && p.ends_with('"') && p.len() >= 2 {
                    out.insert(p[1..p.len() - 1].to_string());
                }
            }
        }
        out
    }

    /// UI-30：XML 里每个控件动作都必须有**精确**的 dispatch 臂。
    /// 前缀带值的家族（fmt:/InsertFn:/zoom:/fitw:/fith:/scale:）单独认。
    #[test]
    fn every_action_is_dispatched() {
        fn walk(nodes: &[Node], out: &mut Vec<String>) {
            for n in nodes {
                match n {
                    Node::Button(c) | Node::Toggle(c) => out.push(c.action().to_string()),
                    Node::Split { ctl, menu } => {
                        out.push(ctl.action().to_string());
                        walk_menu(menu, out);
                    }
                    Node::Menu(m) => walk_menu(m, out),
                    Node::DropDown { items, .. } | Node::Gallery { items, .. } => {
                        out.extend(items.iter().map(|i| i.id.clone()))
                    }
                    Node::Box { children, .. } => walk(children, out),
                    _ => {}
                }
            }
        }
        fn walk_menu(m: &Menu, out: &mut Vec<String>) {
            for i in &m.items {
                match i {
                    MenuNode::Button(c) | MenuNode::Toggle(c) => out.push(c.action().to_string()),
                    MenuNode::Menu(sub) => walk_menu(sub, out),
                    MenuNode::Separator => {}
                }
            }
        }
        let r = load();
        let mut acts = Vec::new();
        for t in &r.tabs {
            for g in &t.groups {
                walk(&g.children, &mut acts);
            }
        }
        for c in &r.context {
            for g in &c.tab.groups {
                walk(&g.children, &mut acts);
            }
        }
        let arms = dispatched_actions();
        // 自查：解析出来的是精确名，不能被前缀或注释混进来
        assert!(!arms.contains("NoSuchCommand"), "解析把不存在的命令也算进来了");
        assert!(
            crate::command::COMMANDS.iter().any(|c| c.action == "FileSave"),
            "命令表里连保存都没有"
        );
        let mut missing = Vec::new();
        for a in acts {
            let known = arms.contains(&a)
                || crate::command::COMMANDS.iter().any(|c| c.action == a)
                || a.strip_prefix("fmt:").is_some()
                || a.starts_with("InsertFn:")
                || a.starts_with("zoom:")
                || a.starts_with("fitw:")
                || a.starts_with("fith:")
                || a.starts_with("scale:");
            if !known {
                missing.push(a);
            }
        }
        assert!(missing.is_empty(), "这些 XML 动作没有 dispatch 臂：{missing:?}");
    }

    /// UI-30：宏的三个入口必须在「审阅」页签的「宏」组里（原来只在没人调的旧菜单）。
    #[test]
    fn review_tab_exposes_macro_group() {
        let r = load();
        let tab = r
            .tabs
            .iter()
            .find(|t| t.label == "审阅")
            .expect("没有审阅页签");
        let group = tab
            .groups
            .iter()
            .find(|g| g.label == "宏")
            .expect("审阅页签没有「宏」组");
        let mut acts = Vec::new();
        for child in &group.children {
            match child {
                Node::Button(c) | Node::Toggle(c) => acts.push(c.action().to_string()),
                Node::Box { children, .. } => {
                    for sub in children {
                        if let Node::Button(c) | Node::Toggle(c) = sub {
                            acts.push(c.action().to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        for want in ["MacroView", "MacroRun", "MacroExport"] {
            assert!(acts.iter().any(|a| a == want), "宏组里缺 {want}：{acts:?}");
        }
    }

    #[test]
    fn every_group_has_children_or_launcher() {
        let r = load();
        for t in &r.tabs {
            for g in &t.groups {
                assert!(
                    !g.children.is_empty(),
                    "组「{}」是空的（页签 {}）",
                    g.label,
                    t.label
                );
            }
        }
    }
}
