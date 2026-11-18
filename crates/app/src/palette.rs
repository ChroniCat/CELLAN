//! UI-22 命令搜索：把功能区里所有可执行控件摊平成一张表，按名称/路径过滤后执行。
//! 不另建命令清单，避免和 customUI.xml 漂移。

use crate::ribbon::{Menu, MenuNode, Node};
use crate::SheetApp;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub action: String,
    pub label: String,
    pub group: String,
    pub tab: String,
}

impl AsRef<str> for Entry {
    /// 使用频率按 action 记，不按显示名（改名不丢历史）
    fn as_ref(&self) -> &str {
        &self.action
    }
}

impl Entry {
    pub fn path(&self) -> String {
        format!("{} › {}", self.tab, self.group)
    }
    pub fn haystack(&self) -> String {
        format!("{} {} {} {}", self.label, self.action, self.group, self.tab)
    }
}

pub fn entries(app: &SheetApp) -> Vec<Entry> {
    let mut out = Vec::new();
    for tab in &app.ribbon.tabs {
        for g in &tab.groups {
            for n in &g.children {
                walk(app, n, &tab.label, &g.label, &mut out);
            }
        }
    }
    // 同一动作只留第一条（功能区里可能有重复入口）
    let mut seen = std::collections::HashSet::new();
    out.retain(|e| seen.insert(e.action.clone()));
    out
}

fn walk(app: &SheetApp, n: &Node, tab: &str, group: &str, out: &mut Vec<Entry>) {
    match n {
        Node::Button(c) | Node::Toggle(c) | Node::Split { ctl: c, .. } => push(app, c, tab, group, out),
        Node::Menu(m) => menu(app, m, tab, group, out),
        Node::DropDown { ctl, .. }
        | Node::Gallery { ctl, .. }
        | Node::ComboBox { ctl, .. }
        | Node::Slider { ctl, .. } => push(app, ctl, tab, group, out),
        Node::EditBox { ctl, .. } => push(app, ctl, tab, group, out),
        Node::Box { children, .. } => {
            for c in children {
                walk(app, c, tab, group, out);
            }
        }
        _ => {}
    }
}

fn menu(app: &SheetApp, m: &Menu, tab: &str, group: &str, out: &mut Vec<Entry>) {
    push(app, &m.ctl, tab, group, out);
    for it in &m.items {
        match it {
            MenuNode::Button(c) | MenuNode::Toggle(c) => push(app, c, tab, group, out),
            MenuNode::Menu(sub) => menu(app, sub, tab, group, out),
            MenuNode::Separator => {}
        }
    }
}

fn push(app: &SheetApp, c: &crate::ribbon::Ctl, tab: &str, group: &str, out: &mut Vec<Entry>) {
    let st = app.ctl_state(&c.id);
    if !st.visible {
        return;
    }
    let label = st.label.clone().or_else(|| c.label.clone()).unwrap_or_default();
    if label.trim().is_empty() {
        return;
    }
    out.push(Entry {
        action: c.action().to_string(),
        label,
        group: group.to_string(),
        tab: tab.to_string(),
    });
}

/// 过滤：空格分词，全部命中才算（中文按子串，英文不区分大小写）。
pub fn filter(entries: &[Entry], query: &str) -> Vec<Entry> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    entries
        .iter()
        .filter(|e| {
            let hay = e.haystack().to_lowercase();
            terms.iter().all(|t| hay.contains(t))
        })
        .cloned()
        .collect()
}

/// 方向键移动命中下标：空结果固定在 0，越界钳制，不环绕。
pub fn step_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (current as isize + delta).clamp(0, len as isize - 1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        vec![
            Entry { action: "Bold".into(), label: "加粗".into(), group: "字体".into(), tab: "开始".into() },
            Entry { action: "CenterAlign".into(), label: "居中".into(), group: "对齐方式".into(), tab: "开始".into() },
            Entry { action: "FreezePanes".into(), label: "冻结窗格".into(), group: "窗口".into(), tab: "视图".into() },
        ]
    }

    #[test]
    fn filters_by_label_path_and_action() {
        assert_eq!(filter(&sample(), "加粗").len(), 1);
        assert_eq!(filter(&sample(), "开始 居中")[0].action, "CenterAlign");
        assert_eq!(filter(&sample(), "freeze")[0].label, "冻结窗格");
        assert_eq!(filter(&sample(), "视图 窗口").len(), 1);
        assert_eq!(filter(&sample(), "").len(), 3);
        assert!(filter(&sample(), "不存在").is_empty());
    }

    #[test]
    fn arrow_index_clamps_and_empty_stays_zero() {
        assert_eq!(step_index(0, 4, 1), 1);
        assert_eq!(step_index(0, 4, -1), 0);
        assert_eq!(step_index(3, 4, 1), 3);
        assert_eq!(step_index(0, 0, 1), 0);
        assert_eq!(step_index(0, 0, -1), 0);
    }
}
