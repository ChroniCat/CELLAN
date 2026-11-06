use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

pub struct FileTree {
    roots: Vec<PathBuf>,
    children: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
    loading: HashSet<PathBuf>,
    tx: Sender<(PathBuf, Option<Vec<Entry>>)>,
    rx: Receiver<(PathBuf, Option<Vec<Entry>>)>,
    pub error: Option<String>,
    /// 用户手动输入的 UNC / 路径
    pub manual: String,
    started: bool,
}

fn read_dir_entries(path: &Path) -> Option<Vec<Entry>> {
    let mut out = Vec::new();
    {
        let rd = std::fs::read_dir(path).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('$') || name == "System Volume Information" {
                continue;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            out.push(Entry { name, path: p, is_dir });
        }
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Some(out)
}

impl FileTree {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        // 注意：不能在这里探测盘符是否存在——断开的网络盘 exists() 会把主线程卡住，
        // 表现就是窗口出来了但首帧不画（白屏）。先列字母，读取全部放后台线程。
        let roots = (b'A'..=b'Z')
            .map(|c| PathBuf::from(format!("{}:\\", c as char)))
            .collect();
        Self {
            roots,
            children: HashMap::new(),
            expanded: HashSet::new(),
            loading: HashSet::new(),
            tx,
            rx,
            error: None,
            manual: String::new(),
            started: false,
        }
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

    pub fn pump(&mut self) {
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
                        self.children.insert(path, Vec::new());
                    }
                }
            }
        }
    }

    pub fn add_root(&mut self, path: PathBuf) {
        self.expanded.insert(path.clone());
        if !self.roots.contains(&path) {
            self.roots.insert(0, path.clone());
        }
        self.request(path);
    }

    /// 返回被双击打开的文件
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        let mut open: Option<PathBuf> = None;

        ui.horizontal(|ui| {
            ui.strong("文件");
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

        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.manual)
                    .hint_text("\\\\192.0.2.41\\公共盘新")
                    .desired_width(150.0),
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
            ui.colored_label(egui::Color32::from_rgb(180, 40, 40), e);
        }
        ui.separator();

        // 首帧之后再发起根目录读取，保证窗口先画出来
        if !self.started {
            self.started = true;
            for r in self.roots.clone() {
                self.request(r);
            }
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let roots = self.roots.clone();
                for r in roots {
                    self.node_ui(ui, &r, true, 0, &mut open);
                }
            });
        open
    }

    fn node_ui(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        is_dir: bool,
        depth: usize,
        open: &mut Option<PathBuf>,
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
            let arrow = if loading {
                "…"
            } else if expanded {
                "▾"
            } else {
                "▸"
            };
            let text = format!("{}{} {}", "   ".repeat(depth), arrow, label);
            let resp = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
            if resp.clicked() {
                if expanded {
                    self.expanded.remove(path);
                } else {
                    self.expanded.insert(path.to_path_buf());
                    self.request(path.to_path_buf());
                }
            }
            if expanded {
                if let Some(children) = self.children.get(path).cloned() {
                    for c in children.iter().take(400) {
                        self.node_ui(ui, &c.path, c.is_dir, depth + 1, open);
                    }
                }
            }
        } else {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let editable = matches!(ext.as_str(), "xlsx" | "xlsm" | "xls" | "ods");
            let text = format!("{}    {}", "   ".repeat(depth), label);
            let mut rich = egui::RichText::new(text);
            if !editable {
                rich = rich.color(ui.visuals().weak_text_color());
            }
            let resp = ui.add(egui::Label::new(rich).sense(egui::Sense::click()));
            if resp.double_clicked() && editable {
                *open = Some(path.to_path_buf());
            }
            if resp.hovered() {
                resp.on_hover_text(path.display().to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_dirs_first_and_hides_system_entries() {
        let dir = std::env::temp_dir().join("sheets_tree_test");
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
}
