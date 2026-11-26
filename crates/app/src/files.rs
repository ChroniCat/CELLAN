//! 左侧文件树：快速访问 + 最近文件 + 异步目录浏览。
//!
//! 约束（踩过）：断开的网络盘 `exists()` 会把主线程卡住，窗口出来了却白屏。
//! 所以这里只在后台线程里读目录，主线程只管画已经拿到的内容。

use crate::ui_kit;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const ROW_H: f32 = 22.0;
const INDENT: f32 = 13.0;

/// 目录里最多列这么多项，再多就提示用过滤框
const MAX_ROWS: usize = 600;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub name_lower: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

enum Act {
    Open,
    Reveal,
    CopyPath,
    Refresh,
    AddRoot,
}

pub struct FileTree {
    roots: Vec<PathBuf>,
    children: HashMap<PathBuf, Arc<Vec<Entry>>>,
    expanded: HashSet<PathBuf>,
    loading: HashSet<PathBuf>,
    tx: Sender<(PathBuf, Option<Arc<Vec<Entry>>>)>,
    rx: Receiver<(PathBuf, Option<Arc<Vec<Entry>>>)>,
    quick_rx: Option<Receiver<Vec<(String, PathBuf)>>>,
    pub error: Option<String>,
    /// 用户手动输入的 UNC / 路径
    pub manual: String,
    /// 过滤框内容
    pub filter: String,
    /// 当前打开的文档（高亮 + 快速访问里给个入口）
    pub current: Option<PathBuf>,
    /// 最近打开（由 app 灌进来，避免每帧读盘）
    pub recent: Vec<PathBuf>,
    /// 快速访问（桌面/文档/下载）
    quick: Vec<(String, PathBuf)>,
    selected: Option<PathBuf>,
    started: bool,
}

fn read_dir_entries(path: &Path) -> Option<Arc<Vec<Entry>>> {
    let mut out = Vec::new();
    {
        let rd = std::fs::read_dir(path).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('$')
                || name.starts_with(".~")
                || name.starts_with("~$")
                || name == ".cellan-share"
                || name == "System Volume Information"
            {
                continue;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name_lower = name.to_lowercase();
            out.push(Entry {
                name,
                name_lower,
                path: p,
                is_dir,
            });
        }
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name_lower.cmp(&b.name_lower))
    });
    Some(Arc::new(out))
}

fn roots_store() -> PathBuf {
    crate::update::app_dir().join("roots.txt")
}

fn load_roots() -> Vec<PathBuf> {
    std::fs::read_to_string(roots_store())
        .map(|t| {
            t.lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn save_roots(roots: &[PathBuf]) {
    let p = roots_store();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = roots
        .iter()
        .filter(|r| r.parent().is_some()) // 盘符不用存
        .map(|r| format!("{}\n", r.display()))
        .collect();
    let _ = std::fs::write(p, text);
}

/// 用户目录下的常用位置（只做字符串拼装，不在主线程碰网络路径）
fn home_quick() -> Vec<(String, PathBuf)> {
    let Ok(home) = std::env::var("USERPROFILE") else {
        return Vec::new();
    };
    let h = PathBuf::from(home);
    let mut out = Vec::new();
    for (label, sub) in [
        ("桌面", "Desktop"),
        ("文档", "Documents"),
        ("下载", "Downloads"),
    ] {
        let p = h.join(sub);
        if p.is_dir() {
            out.push((label.to_string(), p));
        }
    }
    out
}

impl FileTree {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let (quick_tx, quick_rx) = channel();
        std::thread::spawn(move || {
            let _ = quick_tx.send(home_quick());
        });
        // 不能在这里探测盘符（断开的网络盘会卡住首帧），先列字母，读取全在后台
        let mut roots: Vec<PathBuf> = (b'A'..=b'Z')
            .map(|c| PathBuf::from(format!("{}:\\", c as char)))
            .collect();
        // 上次用过的 UNC 路径排在最前
        for r in load_roots().into_iter().rev() {
            roots.retain(|x| x != &r);
            roots.insert(0, r);
        }
        Self {
            roots,
            children: HashMap::new(),
            expanded: HashSet::new(),
            loading: HashSet::new(),
            tx,
            rx,
            quick_rx: Some(quick_rx),
            error: None,
            manual: String::new(),
            filter: String::new(),
            current: None,
            recent: Vec::new(),
            quick: Vec::new(),
            selected: None,
            started: false,
        }
    }

    pub fn set_current(&mut self, path: Option<PathBuf>) {
        self.current = path;
    }

    pub fn set_recent(&mut self, list: Vec<PathBuf>) {
        self.recent = list;
    }

    /// 打开某个文件时把它的祖先目录展开，切过去就能看到
    pub fn locate(&mut self, path: &Path) {
        let mut dir = path.parent().map(|p| p.to_path_buf());
        if crate::debug_flags::tree_log() {
            eprintln!("[tree] locate {}", path.display());
        }
        while let Some(d) = dir {
            self.expanded.insert(d.clone());
            self.request(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
        self.selected = Some(path.to_path_buf());
    }

    fn request(&mut self, path: PathBuf) {
        if self.children.contains_key(&path) || self.loading.contains(&path) {
            return;
        }
        self.loading.insert(path.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let entries = read_dir_entries(&path);
            let _ = tx.send((path, entries));
        });
    }

    fn poll_quick(&mut self) {
        if let Some(rx) = &self.quick_rx {
            match rx.try_recv() {
                Ok(q) => {
                    self.quick = q;
                    self.quick_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.quick_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    pub fn pump(&mut self) {
        self.poll_quick();
        while let Ok((path, entries)) = self.rx.try_recv() {
            self.loading.remove(&path);
            match entries {
                Some(list) => {
                    self.children.insert(path, list);
                }
                None => {
                    // 读不到（盘符不存在 / 权限不足）：根节点直接移除，子节点记为空目录
                    let is_root = path.parent().is_none();
                    if is_root {
                        self.roots.retain(|r| r != &path);
                    } else {
                        self.children.insert(path, Arc::new(Vec::new()));
                    }
                }
            }
        }
    }

    pub fn add_root(&mut self, path: PathBuf) {
        self.expanded.insert(path.clone());
        if !self.roots.contains(&path) {
            self.roots.insert(0, path.clone());
            save_roots(&self.roots);
        }
        self.request(path);
    }

    // ---------------------------------------------------------------- 过滤

    fn hit(&self, e: &Entry, filter_lower: &str) -> bool {
        if filter_lower.is_empty() {
            return true;
        }
        if e.name_lower.contains(filter_lower) {
            return true;
        }
        e.is_dir && self.subtree_hit(&e.path, filter_lower, 0)
    }

    fn subtree_hit(&self, path: &Path, f: &str, depth: usize) -> bool {
        if depth > 6 {
            return false;
        }
        let Some(list) = self.children.get(path) else {
            return false;
        };
        for e in list.iter() {
            if e.name_lower.contains(f) {
                return true;
            }
            if e.is_dir && self.subtree_hit(&e.path, f, depth + 1) {
                return true;
            }
        }
        false
    }

    fn filtering(&self) -> bool {
        !self.filter.trim().is_empty()
    }

    // ---------------------------------------------------------------- 绘制

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        self.poll_quick();
        let mut open: Option<PathBuf> = None;

        // 标题
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("文件").size(12.5).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("⟳")
                    .on_hover_text("刷新已展开的目录")
                    .clicked()
                {
                    let keys: Vec<PathBuf> = self.children.keys().cloned().collect();
                    for k in keys {
                        self.children.remove(&k);
                        if self.expanded.contains(&k) {
                            self.request(k);
                        }
                    }
                }
            });
        });

        // 过滤
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("过滤文件名")
                .desired_width(ui.available_width()),
        );

        // 快速访问
        let mut quick_all = self.quick.clone();
        if let Some(d) = self.current.as_ref().and_then(|p| p.parent()) {
            if !quick_all.iter().any(|(_, p)| p == d) {
                quick_all.insert(0, ("当前文件".to_string(), d.to_path_buf()));
            }
        }
        if !quick_all.is_empty() {
            section(ui, "快速访问");
            for (label, path) in &quick_all {
                let mut act = None;
                let r = self.row(ui, 0, RowKind::Dir(false), label, false, &mut act);
                if matches!(act, Some(Act::Open)) || r.clicked() {
                    self.add_root(path.clone());
                }
            }
        }

        // 最近文件
        let recent = self.recent.clone();
        if !recent.is_empty() && !self.filtering() {
            section(ui, "最近");
            for p in recent.iter().take(6) {
                let name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.display().to_string());
                let is_cur = self.current.as_ref() == Some(p);
                let mut act = None;
                let r = self.row(ui, 0, RowKind::File(ext_of(p)), &name, is_cur, &mut act);
                match act {
                    Some(Act::Open) => open = Some(p.clone()),
                    Some(Act::Reveal) => reveal(p),
                    Some(Act::CopyPath) => copy(ui, p),
                    _ => {}
                }
                if r.double_clicked() {
                    open = Some(p.clone());
                }
                r.on_hover_text(p.display().to_string());
            }
        }

        // 手动路径
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.manual)
                    .hint_text(r"\\192.0.2.41\公共盘")
                    .desired_width(ui.available_width() - 46.0),
            );
            let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.small_button("转到").clicked() || submit {
                let p = PathBuf::from(self.manual.trim());
                if p.as_os_str().is_empty() {
                    self.error = Some("请输入路径".into());
                } else {
                    self.error = None;
                    self.add_root(p);
                }
            }
        });
        if let Some(e) = self.error.clone() {
            ui.colored_label(Color32::from_rgb(180, 40, 40), e);
        }
        ui_kit::hairline(ui);

        if !self.started {
            self.started = true;
            for r in self.roots.clone() {
                self.request(r);
            }
        }

        // 树本体
        let filter_lower = self.filter.trim().to_lowercase();
        let mut flat: Vec<(PathBuf, bool)> = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let roots = self.roots.clone();
                for r in roots {
                    let mut act = None;
                    self.node_ui(ui, &r, true, 0, &filter_lower, &mut open, &mut act, &mut flat);
                    self.apply(ui, &r, act, &mut open, true);
                }
                if self.filtering() && flat.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("没有匹配的文件（只搜已展开的目录）")
                            .size(11.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            });

        // 键盘：上下移动、Enter 打开、左右折叠展开
        self.keys(ui, &flat, &mut open);
        open
    }

    fn apply(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        act: Option<Act>,
        open: &mut Option<PathBuf>,
        is_dir: bool,
    ) {
        let Some(act) = act else { return };
        match act {
            Act::Open => {
                if is_dir {
                    self.toggle(path);
                } else {
                    *open = Some(path.to_path_buf());
                }
            }
            Act::Reveal => reveal(path),
            Act::CopyPath => copy(ui, path),
            Act::Refresh => {
                self.children.remove(path);
                if self.expanded.contains(path) {
                    self.request(path.to_path_buf());
                }
            }
            Act::AddRoot => self.add_root(path.to_path_buf()),
        }
    }

    fn toggle(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_path_buf());
            self.request(path.to_path_buf());
        }
    }

    fn keys(&mut self, ui: &mut egui::Ui, flat: &[(PathBuf, bool)], open: &mut Option<PathBuf>) {
        if !ui.ui_contains_pointer() || ui.ctx().wants_keyboard_input() || flat.is_empty() {
            return;
        }
        let idx = self
            .selected
            .as_ref()
            .and_then(|s| flat.iter().position(|(p, _)| p == s));
        let (down, up, enter, right, left) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowLeft),
            )
        });
        if down || up {
            let next = match (idx, up) {
                (Some(i), true) => i.saturating_sub(1),
                (Some(i), false) => (i + 1).min(flat.len() - 1),
                (None, _) => 0,
            };
            self.selected = Some(flat[next].0.clone());
        }
        if let Some(i) = idx {
            let (p, is_dir) = flat[i].clone();
            if right && is_dir && !self.expanded.contains(&p) {
                self.toggle(&p);
            }
            if left && is_dir && self.expanded.contains(&p) {
                self.toggle(&p);
            }
            if enter {
                if is_dir {
                    self.toggle(&p);
                } else {
                    *open = Some(p);
                }
            }
        }
    }

    fn node_ui(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        is_dir: bool,
        depth: usize,
        filter_lower: &str,
        open: &mut Option<PathBuf>,
        act: &mut Option<Act>,
        flat: &mut Vec<(PathBuf, bool)>,
    ) {
        let label = if depth == 0 {
            path.display().to_string()
        } else {
            path.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string())
        };

        if is_dir {
            let expanded = self.expanded.contains(path);
            let loading = self.loading.contains(path);
            let kind = if loading {
                RowKind::Loading
            } else {
                RowKind::Dir(expanded)
            };
            let cur = self.current.as_ref().and_then(|p| p.parent()) == Some(path);
            flat.push((path.to_path_buf(), true));
            let resp = self.row(ui, depth, kind, &label, cur, act);
            if resp.clicked() || resp.double_clicked() {
                self.toggle(path);
            }
            if depth <= 1 {
                resp.on_hover_text(format!("{}", path.display()));
            }
            if expanded && crate::debug_flags::tree_log() {
                eprintln!(
                    "[tree] 展开 {} 子项={}",
                    path.display(),
                    self.children.get(path).map(|v| v.len()).unwrap_or(0)
                );
            }
            if expanded {
                if let Some(children) = self.children.get(path).map(Arc::clone) {
                    let mut shown = 0usize;
                    for c in children.iter() {
                        if !self.hit(c, filter_lower) {
                            continue;
                        }
                        if shown == MAX_ROWS {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}……（共 {} 项，用过滤框收窄）",
                                    "   ".repeat(depth + 1),
                                    children.len()
                                ))
                                .size(10.5)
                                .color(ui.visuals().weak_text_color()),
                            );
                            break;
                        }
                        shown += 1;
                        let mut sub = None;
                        self.node_ui(
                            ui,
                            &c.path,
                            c.is_dir,
                            depth + 1,
                            filter_lower,
                            open,
                            &mut sub,
                            flat,
                        );
                        self.apply(ui, &c.path, sub, open, c.is_dir);
                    }
                }
            }
        } else {
            let ext = ext_of(path);
            let editable = matches!(ext.as_str(), "xlsx" | "xlsm" | "xls" | "ods" | "csv" | "tsv");
            if !editable {
                return; // 文件树只列能打开的表
            }
            let is_cur = self.current.as_deref() == Some(path);
            flat.push((path.to_path_buf(), false));
            let resp = self.row(ui, depth, RowKind::File(ext), &label, is_cur, act);
            if resp.clicked() {
                self.selected = Some(path.to_path_buf());
            }
            if resp.double_clicked() {
                *open = Some(path.to_path_buf());
            }
            resp.on_hover_text(path.display().to_string());
        }
    }

    /// 一行：背景 + 缩进 + 箭头 + 图标 + 文字 + 右键菜单
    fn row(
        &self,
        ui: &mut egui::Ui,
        depth: usize,
        kind: RowKind,
        label: &str,
        active: bool,
        act: &mut Option<Act>,
    ) -> Response {
        let w = ui.available_width().max(60.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
        let v = ui.visuals().clone();
        if active {
            ui.painter().rect_filled(rect, 4.0, v.selection.bg_fill);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect, 4.0, v.widgets.hovered.weak_bg_fill);
        }
        let clip = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let cy = rect.center().y;
        let mut x = rect.left() + 4.0 + depth as f32 * INDENT;

        match &kind {
            RowKind::Dir(open) => chevron(&clip, Pos2::new(x + 5.0, cy), *open, v.weak_text_color()),
            RowKind::Loading => {
                clip.text(
                    Pos2::new(x, cy),
                    Align2::LEFT_CENTER,
                    "…",
                    FontId::proportional(12.0),
                    v.weak_text_color(),
                );
            }
            _ => {}
        }
        x += 15.0;

        match &kind {
            RowKind::Dir(_) | RowKind::QuickFolder => folder_icon(&clip, Pos2::new(x, cy)),
            RowKind::File(ext) => file_icon(&clip, Pos2::new(x, cy), ext),
            RowKind::Loading => {}
        }
        x += 20.0;

        clip.text(
            Pos2::new(x, cy),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(12.5),
            v.widgets.noninteractive.fg_stroke.color,
        );
        if crate::debug_flags::tree_log() {
            let kind_tag = match &kind {
                RowKind::Dir(o) => {
                    if *o {
                        "dir+"
                    } else {
                        "dir"
                    }
                }
                RowKind::QuickFolder => "quick",
                RowKind::File(_) => "file",
                RowKind::Loading => "load",
            };
            eprintln!(
                "[tree] y={:.1} h={:.1} x={:.1} d={} {} {}",
                rect.top(),
                rect.height(),
                x,
                depth,
                kind_tag,
                label
            );
        }

        let mut pending: Option<Act> = None;
        resp.clone().context_menu(|ui| {
            if ui.button("打开").clicked() {
                pending = Some(Act::Open);
                ui.close_menu();
            }
            if ui.button("在资源管理器中显示").clicked() {
                pending = Some(Act::Reveal);
                ui.close_menu();
            }
            if ui.button("复制路径").clicked() {
                pending = Some(Act::CopyPath);
                ui.close_menu();
            }
            ui.separator();
            if ui.button("刷新").clicked() {
                pending = Some(Act::Refresh);
                ui.close_menu();
            }
            if ui.button("加入根目录").clicked() {
                pending = Some(Act::AddRoot);
                ui.close_menu();
            }
        });
        if pending.is_some() {
            *act = pending;
        }
        resp
    }
}

#[derive(Clone)]
enum RowKind {
    Dir(bool),
    QuickFolder,
    File(String),
    Loading,
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn chevron(p: &egui::Painter, c: Pos2, open: bool, color: Color32) {
    let s = 3.2;
    let pts = if open {
        vec![
            Pos2::new(c.x - s, c.y - s * 0.7),
            Pos2::new(c.x + s, c.y - s * 0.7),
            Pos2::new(c.x, c.y + s * 1.1),
        ]
    } else {
        vec![
            Pos2::new(c.x - s * 0.7, c.y - s),
            Pos2::new(c.x - s * 0.7, c.y + s),
            Pos2::new(c.x + s * 1.1, c.y),
        ]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

fn folder_icon(p: &egui::Painter, c: Pos2) {
    let body = Rect::from_center_size(Pos2::new(c.x + 1.0, c.y + 1.5), Vec2::new(14.0, 10.0));
    let tab = Rect::from_min_size(Pos2::new(body.left(), body.top() - 2.5), Vec2::new(6.0, 3.0));
    p.rect_filled(tab, 1.0, Color32::from_rgb(229, 176, 74));
    p.rect_filled(body, 2.0, Color32::from_rgb(240, 190, 96));
    p.rect_stroke(
        body,
        2.0,
        Stroke::new(1.0, Color32::from_rgb(198, 148, 58)),
        egui::StrokeKind::Inside,
    );
}

fn file_icon(p: &egui::Painter, c: Pos2, ext: &str) {
    let (fill, ink, letter) = match ext {
        "xlsx" | "xlsm" => (Color32::from_rgb(33, 115, 70), Color32::WHITE, "X"),
        "xls" => (Color32::from_rgb(46, 125, 82), Color32::WHITE, "X"),
        "csv" | "tsv" => (Color32::from_rgb(70, 110, 90), Color32::WHITE, "C"),
        _ => (Color32::from_rgb(120, 128, 140), Color32::WHITE, "?"),
    };
    let r = Rect::from_center_size(c, Vec2::new(13.0, 15.0));
    p.rect_filled(r, 2.0, fill);
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        letter,
        FontId::proportional(9.0),
        ink,
    );
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(3.0);
    ui.label(
        egui::RichText::new(title)
            .size(10.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
}

fn reveal(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
}

fn copy(ui: &egui::Ui, path: &Path) {
    ui.ctx().copy_text(path.display().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_dirs_first_and_hides_system_entries() {
        let dir = crate::test_env::scratch("tree-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.xlsx"), b"x").unwrap();
        std::fs::write(dir.join("A.txt"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("$RECYCLE.BIN")).unwrap();

        let entries = read_dir_entries(&dir).expect("应该能读到目录");
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["sub", "A.txt", "b.xlsx"]);
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn missing_path_returns_none() {
        let p = std::env::temp_dir().join("sheets_definitely_missing_dir_xyz");
        assert!(read_dir_entries(&p).is_none());
    }

    #[test]
    fn filter_matches_loaded_subtree() {
        let dir = crate::test_env::scratch("tree-filter");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("深/层")).unwrap();
        std::fs::write(dir.join("深/层/报表.xlsx"), b"x").unwrap();
        std::fs::write(dir.join("readme.txt"), b"x").unwrap();

        let mut t = FileTree::new();
        t.filter = "报表".into();
        t.children.insert(dir.clone(), read_dir_entries(&dir).unwrap());
        t.children
            .insert(dir.join("深"), read_dir_entries(&dir.join("深")).unwrap());
        t.children
            .insert(dir.join("深/层"), read_dir_entries(&dir.join("深/层")).unwrap());

        let deep = Entry {
            name: "深".into(),
            name_lower: "深".into(),
            path: dir.join("深"),
            is_dir: true,
        };
        assert!(t.hit(&deep, "报表"), "子孙里有命中，父目录要留着");
        let other = Entry {
            name: "readme.txt".into(),
            name_lower: "readme.txt".into(),
            path: dir.join("readme.txt"),
            is_dir: false,
        };
        assert!(!t.hit(&other, "报表"), "不匹配的文件要滤掉");
    }
}
