mod files;
mod net;

use eframe::egui;
use files::FileTree;
use sheetcore::sync::{Cursor, Peer};
use egui::{Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2};
use sheetcore::{cell_name, col_label, Cell, Value, Workbook};
use std::collections::HashMap;
use std::path::PathBuf;

const ROW_H: f32 = 22.0;
const COL_W: f32 = 104.0;
const HEAD_W: f32 = 52.0;
const HEAD_H: f32 = 22.0;

#[derive(Default)]
struct EditState {
    buf: String,
}

struct SheetApp {
    wb: Workbook,
    sheet_idx: usize,
    sel: (u32, u32), // row, col
    scroll: Vec2,
    /// 选区右下角（拖选/Shift 点选用；普通点击时等于 sel）
    sel_end: (u32, u32),
    /// 上一帧表格可视尺寸，供 ensure_visible 用
    last_view: Vec2,
    /// 只 dump 一帧
    dump_once: bool,
    editing: Option<EditState>,
    status: String,
    tree: FileTree,
    // ---- 协作 ----
    net: Option<net::Net>,
    peer: Option<Peer>,
    remote: HashMap<u32, (String, u8, u32, u32)>,
    peers_list: Vec<(u32, String, u8)>,
    join_addr: String,
    room_password: String,
    my_name: String,
    last_cursor: Option<(u32, u32)>,
    joined_doc: bool,
    /// 依赖图：编辑后只重算受影响的公式
    deps: sheetcore::formula::deps::Deps,
    /// 后台打开/重算的通道
    open_rx: Option<std::sync::mpsc::Receiver<OpenMsg>>,
    /// 文档代次：后台算完时如果有编辑，就丢弃结果
    doc_gen: u64,
    /// 后台重算进行中
    busy: bool,
    /// 打开期间有编辑，需要找个空闲时刻补一次全量重算
    needs_full: bool,
    /// 最后一次编辑的时间，用于判断是否空闲
    last_edit: Option<std::time::Instant>,
    /// 后台保存的通道
    save_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// 等文件加载完再执行的共享/加入动作
    pending_host: bool,
    pending_join: Option<String>,
}

enum OpenMsg {
    /// 文件已读入（带的是上次保存的计算结果，先显示）
    Loaded(Box<Workbook>),
    /// 全量重算完成
    Recalced(Box<Workbook>, Box<sheetcore::formula::deps::Deps>),
    Failed(String),
}

impl SheetApp {
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let mut app = Self {
            wb: Workbook::new_blank(),
            sheet_idx: 0,
            sel: (0, 0),
            scroll: Vec2::ZERO,
            sel_end: (0, 0),
            last_view: Vec2::new(800.0, 600.0),
            dump_once: true,
            editing: None,
            status: "就绪".into(),
            tree: FileTree::new(),
            net: None,
            peer: None,
            remote: HashMap::new(),
            peers_list: Vec::new(),
            join_addr: String::new(),
            room_password: String::new(),
            my_name: default_name(),
            last_cursor: None,
            joined_doc: false,
            deps: sheetcore::formula::deps::Deps::empty(),
            open_rx: None,
            doc_gen: 0,
            busy: false,
            needs_full: false,
            last_edit: None,
            save_rx: None,
            pending_host: false,
            pending_join: None,
        };
        if let Some(p) = initial {
            app.open_path(p);
        }
        app
    }

    fn open(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Excel", &["xlsx", "xlsm", "xls", "ods"])
            .pick_file()
        {
            self.open_path(path);
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.open_rx = Some(rx);
        self.busy = true;
        self.status = format!("正在打开 {}…", path.display());
        std::thread::spawn(move || {
            match sheetcore::xlsx::load(&path) {
                Ok(wb) => {
                    // 先把读到的内容发回去，界面立刻能画（值是文件里缓存的结果）
                    let _ = tx.send(OpenMsg::Loaded(Box::new(wb.clone())));
                    let mut wb2 = wb;
                    sheetcore::formula::recalc_all(&mut wb2);
                    let deps = sheetcore::formula::deps::Deps::build(&wb2);
                    let _ = tx.send(OpenMsg::Recalced(Box::new(wb2), Box::new(deps)));
                }
                Err(e) => {
                    let _ = tx.send(OpenMsg::Failed(format!("打开失败: {e:#}")));
                }
            }
        });
    }

    /// 处理后台打开/重算的结果
    fn poll_open(&mut self) {
        let mut msgs: Vec<OpenMsg> = Vec::new();
        if let Some(rx) = &self.open_rx {
            while let Ok(m) = rx.try_recv() {
                msgs.push(m);
            }
        }
        let mut finished = false;
        for msg in msgs {
            match msg {
                OpenMsg::Loaded(wb) => {
                    self.wb = *wb;
                    self.deps = sheetcore::formula::deps::Deps::empty();
                    self.sheet_idx = 0;
                    self.sel = (0, 0);
                    self.scroll = Vec2::ZERO;
                    self.editing = None;
                    self.doc_gen = 0;
                    let n: usize = self.wb.sheets.iter().map(|s| s.len()).sum();
                    self.status = format!("已打开（{n} 个非空格），正在后台重算…");
                    eprintln!("[open] 已显示缓存值 {n} 格");
                    if self.pending_host {
                        self.pending_host = false;
                        self.start_host();
                    } else if let Some(addr) = self.pending_join.take() {
                        self.join_addr = addr;
                        self.join_shared();
                    }
                }
                OpenMsg::Recalced(wb, deps) => {
                    if self.doc_gen != 0 {
                        // 期间用户改过：保留用户结果，等空闲再补一次全量
                        self.needs_full = true;
                        self.busy = false;
                        self.deps = sheetcore::formula::deps::Deps::build(&self.wb);
                        self.status = "已打开；因期间有编辑，稍后会自动补算一次".into();
                        eprintln!("[open] 丢弃后台结果（期间有编辑），等空闲补算");
                        finished = true;
                        continue;
                    }
                    self.wb = *wb;
                    self.deps = *deps;
                    self.busy = false;
                    self.needs_full = false;
                    finished = true;
                    let n: usize = self.wb.sheets.iter().map(|s| s.len()).sum();
                    self.status = format!("已打开并重算完成（{n} 个非空格）");
                    eprintln!("[open] 后台重算完成 {n} 格");
                }
                OpenMsg::Failed(e) => {
                    self.status = e;
                    self.busy = false;
                    finished = true;
                }
            }
        }
        if finished {
            self.open_rx = None;
        }
    }

    fn save(&mut self, force_dialog: bool) {
        if self.save_rx.is_some() {
            self.status = "上一次保存还没完成…".into();
            return;
        }
        let path = match (&self.wb.path, force_dialog) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("Excel", &["xlsx"])
                .set_file_name(self.wb.title())
                .save_file(),
        };
        let Some(path) = path else { return };

        let mut wb = self.wb.clone();
        wb.path = Some(path.clone());
        let shown = path.display().to_string();
        self.status = format!("正在保存 {shown}…");
        self.wb.dirty = false;
        let (tx, rx) = std::sync::mpsc::channel();
        self.save_rx = Some(rx);
        std::thread::spawn(move || {
            // 先写临时文件，成功再替换，避免写一半断电把原文件毁了
            let tmp = path.with_extension("xlsx.tmp");
            let res = match sheetcore::xlsx::save(&tmp, &wb) {
                Ok(()) => match std::fs::rename(&tmp, &path) {
                    Ok(()) => Ok(shown),
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        Err(format!("替换文件失败: {e}"))
                    }
                },
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    Err(format!("保存失败: {e:#}"))
                }
            };
            let _ = tx.send(res);
        });
    }

    /// 轮询后台保存结果
    fn poll_save(&mut self) {
        let mut done = None;
        if let Some(rx) = &self.save_rx {
            if let Ok(r) = rx.try_recv() {
                done = Some(r);
            }
        }
        if let Some(r) = done {
            self.save_rx = None;
            match r {
                Ok(p) => {
                    self.status = format!("已保存 {p}");
                    eprintln!("[save] 完成 {p}");
                }
                Err(e) => {
                    self.status = e.clone();
                    self.wb.dirty = true;
                    eprintln!("[save] {e}");
                }
            }
        }
    }

    /// 提交当前编辑：写回单元格、增量重算、广播给协作者
    fn commit_edit(&mut self, advance: Option<(i32, i32)>) {
        if let Some(ed) = self.editing.take() {
            let (r, c) = self.sel;
            let cell = if ed.buf.starts_with('=') {
                Cell { value: Value::Text(ed.buf.as_str().into()), formula: Some(ed.buf.as_str().into()) }
            } else {
                Cell { value: Value::parse_input(&ed.buf), formula: None }
            };
            let cell_clone = cell.clone();
            self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
            self.wb.dirty = true;
            let key = (self.sheet_idx, r, c);
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.last_edit = Some(std::time::Instant::now());
            self.deps.update_cell(&self.wb, key);
            sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &[key]);
            if let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref()) {
                let op = peer.local_set(self.sheet_idx, r, c, &cell_clone);
                net.broadcast_op(op);
            }
        }
        if let Some((dr, dc)) = advance {
            let nr = (self.sel.0 as i64 + dr as i64).max(0) as u32;
            let nc = (self.sel.1 as i64 + dc as i64).max(0) as u32;
            self.sel = (nr, nc);
            self.ensure_visible();
        }
    }

    /// 让选区留在可视范围内（列宽行高不统一，只能按实际位置算）
    fn ensure_visible(&mut self) {
        let (r, c) = self.sel;
        let (cols, rows) = {
            let s = self.wb.sheet(self.sheet_idx);
            (s.cols.clone(), s.rows.clone())
        };
        let view_w = (self.last_view.x - HEAD_W).max(64.0);
        let view_h = (self.last_view.y - HEAD_H).max(44.0);
        let top = rows.pos(r);
        let bot = top + rows.size(r);
        if top < self.scroll.y {
            self.scroll.y = top;
        } else if bot > self.scroll.y + view_h {
            self.scroll.y = bot - view_h;
        }
        let left = cols.pos(c);
        let right = left + cols.size(c);
        if left < self.scroll.x {
            self.scroll.x = left;
        } else if right > self.scroll.x + view_w {
            self.scroll.x = right - view_w;
        }
    }

    /// 键盘：方向键移动、回车/制表符提交并移动、直接键入进入编辑
    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.editing.is_some() {
            // 编辑框内的按键交给 TextEdit 自己处理
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.editing = None;
            }
            return;
        }
        let (mut dr, mut dc) = (0i64, 0i64);
        let (mut commit, mut start_edit, mut clear) = (false, false, false);
        ctx.input(|i| {
            if i.key_pressed(Key::ArrowDown) { dr += 1 }
            if i.key_pressed(Key::ArrowUp) { dr -= 1 }
            if i.key_pressed(Key::ArrowRight) { dc += 1 }
            if i.key_pressed(Key::ArrowLeft) { dc -= 1 }
            if i.key_pressed(Key::Enter) { commit = true; dr = 1 }
            if i.key_pressed(Key::Tab) { commit = true; dc = 1 }
            if i.key_pressed(Key::F2) { start_edit = true }
            if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) { clear = true }
        });

        if clear {
            let (r, c) = self.sel;
            self.wb.sheet_mut(self.sheet_idx).set(r, c, Cell { value: Value::Empty, formula: None });
            self.wb.dirty = true;
            let key = (self.sheet_idx, r, c);
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.deps.update_cell(&self.wb, key);
            sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &[key]);
            if let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref()) {
                let op = peer.local_set(self.sheet_idx, r, c, &Cell { value: Value::Empty, formula: None });
                net.broadcast_op(op);
            }
        }
        if commit || dr != 0 || dc != 0 {
            let nr = (self.sel.0 as i64 + dr).max(0) as u32;
            let nc = (self.sel.1 as i64 + dc).max(0) as u32;
            self.sel = (nr, nc);
            self.ensure_visible();
        }
        if start_edit {
            let (r, c) = self.sel;
            let buf = match self.wb.sheet(self.sheet_idx).get(r, c) {
                Some(cell) => cell.formula.as_deref().map(|f| f.to_string()).unwrap_or_else(|| cell.value.display()),
                None => String::new(),
            };
            self.editing = Some(EditState { buf });
        }
        // 直接打字就开始编辑
        if self.editing.is_none() {
            let typed = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Text(t) if !t.is_empty() && !t.chars().next().unwrap().is_control() => {
                        Some(t.clone())
                    }
                    _ => None,
                })
            });
            if let Some(t) = typed {
                self.editing = Some(EditState { buf: t });
            }
        }
    }

    /// 画表格：行列标题、单元格、选区、远端光标

    /// 规范化后的选区 (r1, c1, r2, c2)
    fn sel_rect(&self) -> (u32, u32, u32, u32) {
        let (r1, r2) = (self.sel.0.min(self.sel_end.0), self.sel.0.max(self.sel_end.0));
        let (c1, c2) = (self.sel.1.min(self.sel_end.1), self.sel.1.max(self.sel_end.1));
        (r1, c1, r2, c2)
    }

    /// 选区左上角的样式（工具栏按钮显示当前状态）
    fn current_style(&self) -> sheetcore::style::CellStyle {
        let id = self.wb.sheet(self.sheet_idx).style_of(self.sel.0, self.sel.1);
        self.wb.styles.get(id).clone()
    }

    /// 对选区每个格子套用样式改动；只影响显示，不触发重算
    fn apply_style<F: Fn(&mut sheetcore::style::CellStyle)>(&mut self, f: F) {
        self.apply_style_xy(move |st, _r, _c| f(st));
    }

    /// 同上，但闭包能拿到格子坐标（画边框要用）
    fn apply_style_xy<F: Fn(&mut sheetcore::style::CellStyle, u32, u32)>(&mut self, f: F) {
        let (r1, c1, r2, c2) = self.sel_rect();
        if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
            self.status = "选区太大（超过 20 万格），不给整块刷格式".into();
            return;
        }
        for r in r1..=r2 {
            for c in c1..=c2 {
                let old = self.wb.sheet(self.sheet_idx).style_of(r, c);
                let mut st = self.wb.styles.get(old).clone();
                f(&mut st, r, c);
                let id = self.wb.styles.intern(st);
                self.wb.sheet_mut(self.sheet_idx).set_style(r, c, id);
            }
        }
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
    }

    /// 只给选区外沿加边框（内部各格不重复画）
    fn outer_border(&mut self, style: u8, color: [u8; 3]) {
        let (r1, c1, r2, c2) = self.sel_rect();
        self.apply_style_xy(move |st, r, c| {
            let mut b = st.border;
            // 左 右 上 下
            if c == c1 {
                b[0] = sheetcore::style::Border { style, color };
            }
            if c == c2 {
                b[1] = sheetcore::style::Border { style, color };
            }
            if r == r1 {
                b[2] = sheetcore::style::Border { style, color };
            }
            if r == r2 {
                b[3] = sheetcore::style::Border { style, color };
            }
            st.border = b;
        });
    }

    fn toggle_merge(&mut self, on: bool) {
        let (r1, c1, r2, c2) = self.sel_rect();
        {
            let sheet = self.wb.sheet_mut(self.sheet_idx);
            if on {
                sheet.merge_range(r1, c1, r2, c2);
            } else {
                sheet.unmerge_range(r1, c1, r2, c2);
            }
        }
        self.wb.dirty = true;
        self.status = if on { "已合并选区".into() } else { "已取消合并".into() };
    }

    /// 格式工具栏（第二行）
    fn toolbar_format(&mut self, ui: &mut egui::Ui) {
        let cur = self.current_style();
        ui.horizontal_wrapped(|ui| {
            // 加粗 / 斜体
            if ui
                .selectable_label(cur.bold, egui::RichText::new("B").strong())
                .on_hover_text("加粗")
                .clicked()
            {
                let v = !cur.bold;
                self.apply_style(move |s| s.bold = v);
            }
            if ui
                .selectable_label(cur.italic, egui::RichText::new("I").italics())
                .on_hover_text("斜体")
                .clicked()
            {
                let v = !cur.italic;
                self.apply_style(move |s| s.italic = v);
            }
            ui.separator();

            // 字号
            let mut size = cur.size;
            ui.label("字号");
            if ui
                .add(egui::DragValue::new(&mut size).speed(0.5).range(6.0..=72.0))
                .changed()
            {
                self.apply_style(move |s| s.size = size);
            }
            ui.separator();

            // 字色
            let mut fg = cur.color;
            if ui.color_edit_button_srgb(&mut fg).on_hover_text("字体颜色").changed() {
                self.apply_style(move |s| s.color = fg);
            }
            // 填充色（None 时用白色占位，点"无填充"清掉）
            let mut bg = cur.fill.unwrap_or([255, 255, 255]);
            if ui
                .color_edit_button_srgb(&mut bg)
                .on_hover_text("填充颜色")
                .changed()
            {
                self.apply_style(move |s| s.fill = Some(bg));
            }
            if ui.small_button("无填充").clicked() {
                self.apply_style(|s| s.fill = None);
            }
            ui.separator();

            // 对齐
            for (label, val, tip) in [("左", 1u8, "左对齐"), ("中", 2, "居中"), ("右", 3, "右对齐")] {
                if ui.selectable_label(cur.halign == val, label).on_hover_text(tip).clicked() {
                    self.apply_style(move |s| s.halign = val);
                }
            }
            ui.separator();

            // 数字格式
            let fmt_label = match cur.format.as_str() {
                "" => "常规",
                "#,##0.00" => "数值",
                "¥#,##0.00" => "货币",
                "0.0%" => "百分比",
                "yyyy-mm-dd" => "日期",
                other => other,
            };
            egui::ComboBox::from_id_salt("numfmt")
                .selected_text(fmt_label.to_string())
                .width(84.0)
                .show_ui(ui, |ui| {
                    for (label, code) in [
                        ("常规", ""),
                        ("数值", "#,##0.00"),
                        ("货币", "¥#,##0.00"),
                        ("百分比", "0.0%"),
                        ("日期", "yyyy-mm-dd"),
                    ] {
                        if ui.selectable_label(cur.format == code, label).clicked() {
                            let c = code.to_string();
                            self.apply_style(move |s| s.format = c.clone());
                        }
                    }
                });
            ui.separator();

            // 边框
            egui::ComboBox::from_id_salt("borders")
                .selected_text("边框")
                .width(64.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(false, "无边框").clicked() {
                        self.apply_style(|s| s.clear_borders());
                    }
                    if ui.selectable_label(false, "全部框线").clicked() {
                        self.apply_style(|s| s.set_all_borders(1, [128, 128, 128]));
                    }
                    if ui.selectable_label(false, "外边框").clicked() {
                        self.outer_border(1, [0, 0, 0]);
                    }
                    if ui.selectable_label(false, "粗外框").clicked() {
                        self.outer_border(3, [0, 0, 0]);
                    }
                });
            ui.separator();

            // 合并
            let (r1, c1, r2, c2) = self.sel_rect();
            let multi = r1 != r2 || c1 != c2;
            let merged = self
                .wb
                .sheet(self.sheet_idx)
                .merge_at(r1, c1)
                .is_some();
            if ui
                .add_enabled(multi, egui::Button::new("合并"))
                .on_hover_text("合并选中区域")
                .clicked()
            {
                self.toggle_merge(true);
            }
            if ui
                .add_enabled(merged, egui::Button::new("拆分"))
                .on_hover_text("取消合并")
                .clicked()
            {
                self.toggle_merge(false);
            }

            if multi {
                let n = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
                ui.separator();
                ui.weak(format!("选中 {n} 格"));
            }
        });
    }

    fn draw_grid(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let avail = ui.available_size();
        self.last_view = avail;
        let (resp, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
        let origin = resp.rect.min;
        painter.rect_filled(resp.rect, 0.0, Color32::WHITE);

        // 滚轮
        if resp.hovered() {
            let d = ui.input(|i| i.raw_scroll_delta);
            self.scroll -= d;
        }

        // 版式快照：列宽/行高随文件而变，位置不能再按「等宽 × 索引」算
        let (cols, rows) = {
            let s = self.wb.sheet(self.sheet_idx);
            (s.cols.clone(), s.rows.clone())
        };
        let max_x = (cols.pos(16_384) - avail.x).max(0.0);
        let max_y = (rows.pos(1_048_576) - avail.y).max(0.0);
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);

        let (off_x, off_y) = (self.scroll.x, self.scroll.y);
        let first_col = cols.index_at(off_x);
        let first_row = rows.index_at(off_y);
        if std::env::var("SC_DBG_GRID").is_ok() {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < 3 || n % 120 == 0 {
                eprintln!("[grid] f={n} avail={:?} scroll={:?} cols.custom={} rows.custom={} first=({},{})",
                    avail, (off_x, off_y), cols.len(), rows.len(), first_col, first_row);
            }
        }
        let gx = |c: u32| origin.x + HEAD_W + cols.pos(c) - off_x;
        let gy = |r: u32| origin.y + HEAD_H + rows.pos(r) - off_y;

        let mut dump: Vec<String> = Vec::new();
        let dumping = std::env::var("SC_DUMP").is_ok() && self.dump_once;
        if dumping {
            self.dump_once = false;
            dump.push(format!("avail={:?} scroll=({:.0},{:.0}) first=({},{})", avail, off_x, off_y, first_col, first_row));
        }

        let text_color = Color32::from_rgb(24, 24, 24);
        let grid_line = Color32::from_rgb(228, 228, 228);
        let head_bg = Color32::from_rgb(244, 245, 247);

        // 列标题
        painter.rect_filled(
            Rect::from_min_size(origin, Vec2::new(avail.x, HEAD_H)),
            0.0,
            head_bg,
        );
        let mut c = first_col;
        let mut guard = 0;
        while gx(c) < origin.x + avail.x && guard < 4096 {
            guard += 1;
            let x = gx(c);
            let w = cols.size(c);
            if x + w > origin.x + HEAD_W {
                painter.text(
                    egui::pos2(x + w / 2.0, origin.y + HEAD_H / 2.0),
                    Align2::CENTER_CENTER,
                    col_label(c),
                    FontId::proportional(12.0),
                    Color32::from_rgb(90, 90, 90),
                );
                painter.line_segment(
                    [egui::pos2(x, origin.y), egui::pos2(x, origin.y + HEAD_H + avail.y)],
                    Stroke::new(1.0, grid_line),
                );
            }
            c += 1;
        }

        // 行标题
        let mut r = first_row;
        guard = 0;
        while gy(r) < origin.y + avail.y && guard < 8192 {
            guard += 1;
            let y = gy(r);
            let h = rows.size(r);
            if y + h > origin.y + HEAD_H {
                painter.text(
                    egui::pos2(origin.x + HEAD_W / 2.0, y + h / 2.0),
                    Align2::CENTER_CENTER,
                    (r + 1).to_string(),
                    FontId::proportional(12.0),
                    Color32::from_rgb(90, 90, 90),
                );
                painter.line_segment(
                    [egui::pos2(origin.x, y), egui::pos2(origin.x + avail.x, y)],
                    Stroke::new(1.0, grid_line),
                );
            }
            r += 1;
        }

        // 单元格
        let merges = self.wb.sheet(self.sheet_idx).merges.clone();
        let mut r = first_row;
        guard = 0;
        while gy(r) < origin.y + avail.y && guard < 8192 {
            guard += 1;
            let y = gy(r);
            let rh = rows.size(r);
            if y + rh < origin.y + HEAD_H {
                r += 1;
                continue;
            }
            let mut c = first_col;
            let mut cguard = 0;
            while gx(c) < origin.x + avail.x && cguard < 4096 {
                cguard += 1;
                let x = gx(c);
                let cw = cols.size(c);
                if x + cw < origin.x + HEAD_W {
                    c += 1;
                    continue;
                }
                let mut rect = Rect::from_min_size(egui::pos2(x, y), Vec2::new(cw, rh));
                // 合并区域：只画左上角，整块按合并尺寸绘制
                let merge = merges
                    .iter()
                    .find(|(r1, c1, r2, c2)| r >= *r1 && r <= *r2 && c >= *c1 && c <= *c2)
                    .copied();
                if let Some((mr1, mc1, mr2, mc2)) = merge {
                    if (r, c) != (mr1, mc1) {
                        c += 1;
                        continue;
                    }
                    rect.set_width((cols.pos(mc2 + 1) - cols.pos(mc1)).max(cw));
                    rect.set_height((rows.pos(mr2 + 1) - rows.pos(mr1)).max(rh));
                }
                let sid = self.wb.sheet(self.sheet_idx).style_of(r, c);
                let st = self.wb.styles.get(sid);
                // 选中底色先画，自带填充后画（否则会把真实颜色染偏）
                if self.sel == (r, c) && st.fill.is_none() {
                    painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(26, 115, 232, 40));
                }
                if let Some(fill) = st.fill {
                    painter.rect_filled(rect, 0.0, Color32::from_rgb(fill[0], fill[1], fill[2]));
                    if dumping {
                        dump.push(format!("fill ({},{}) rect=({:.0},{:.0},{:.0},{:.0}) rgb={:?}", r, c, rect.min.x, rect.min.y, rect.max.x, rect.max.y, fill));
                    }
                }
                if let Some(cell) = self.wb.sheet(self.sheet_idx).get(r, c) {
                    let txt = sheetcore::style::format_value(&cell.value, &st.format);
                    if !txt.is_empty() {
                        let is_num = matches!(cell.value, Value::Number(_));
                        let font = FontId::proportional(if st.bold { st.size + 0.6 } else { st.size });
                        let color = Color32::from_rgb(st.color[0], st.color[1], st.color[2]);
                        let align = if st.halign == 2 || (st.halign == 0 && !is_num) {
                            Align2::CENTER_CENTER
                        } else if st.halign == 3 || (st.halign == 0 && is_num) {
                            Align2::RIGHT_CENTER
                        } else {
                            Align2::LEFT_CENTER
                        };
                        let tx = match align {
                            Align2::CENTER_CENTER => rect.center().x,
                            Align2::RIGHT_CENTER => rect.max.x - 4.0,
                            _ => rect.min.x + 4.0,
                        };
                        let ty = if st.valign == 1 {
                            rect.min.y + 4.0
                        } else if st.valign == 3 {
                            rect.max.y - 4.0
                        } else {
                            rect.center().y
                        };
                        painter.text(egui::pos2(tx, ty), align, txt.clone(), font, color);
                        if dumping {
                            dump.push(format!("text ({},{}) {:?} at=({:.0},{:.0}) rgb={:?} size={:.1}", r, c, txt, tx, ty, st.color, st.size));
                        }
                    }
                }
                // 边框（合并区域按整块画）
                let bd = &st.border;
                if bd[0].style != 0 || bd[1].style != 0 || bd[2].style != 0 || bd[3].style != 0 {
                    let (l, t, rr, bb) = (rect.left(), rect.top(), rect.right(), rect.bottom());
                    if dumping {
                        dump.push(format!("border ({},{}) rect=({:.0},{:.0},{:.0},{:.0}) 左{:?} 右{:?} 上{:?} 下{:?}", r, c, l, t, rr, bb, bd[0].style, bd[1].style, bd[2].style, bd[3].style));
                    }
                    draw_edge(&painter, egui::pos2(l, t), egui::pos2(l, bb), &bd[0]);
                    draw_edge(&painter, egui::pos2(rr, t), egui::pos2(rr, bb), &bd[1]);
                    draw_edge(&painter, egui::pos2(l, t), egui::pos2(rr, t), &bd[2]);
                    draw_edge(&painter, egui::pos2(l, bb), egui::pos2(rr, bb), &bd[3]);
                }
                c += 1;
            }
            r += 1;
        }

        // 框选范围底色
        let (qr1, qc1, qr2, qc2) = self.sel_rect();
        if (qr1, qc1) != (qr2, qc2) {
            let x0 = gx(qc1);
            let y0 = gy(qr1);
            let w = cols.pos(qc2 + 1) - cols.pos(qc1);
            let h = rows.pos(qr2 + 1) - rows.pos(qr1);
            painter.rect_filled(
                Rect::from_min_size(egui::pos2(x0, y0), Vec2::new(w, h)),
                0.0,
                Color32::from_rgba_unmultiplied(26, 115, 232, 28),
            );
            painter.rect_stroke(
                Rect::from_min_size(egui::pos2(x0, y0), Vec2::new(w, h)),
                0.0,
                Stroke::new(1.5, Color32::from_rgb(26, 115, 232)),
                egui::StrokeKind::Inside,
            );
        }

        // 选区边框（合并区域按整块画）
        let (sr1, sc1, sr2, sc2) = merges
            .iter()
            .find(|(r1, c1, r2, c2)| self.sel.0 >= *r1 && self.sel.0 <= *r2 && self.sel.1 >= *c1 && self.sel.1 <= *c2)
            .copied()
            .unwrap_or((self.sel.0, self.sel.1, self.sel.0, self.sel.1));
        let sx = gx(sc1);
        let sy = gy(sr1);
        let sw = cols.pos(sc2 + 1) - cols.pos(sc1);
        let sh = rows.pos(sr2 + 1) - rows.pos(sr1);
        if sx + sw > origin.x + HEAD_W && sy + sh > origin.y + HEAD_H {
            painter.rect_stroke(
                Rect::from_min_size(egui::pos2(sx, sy), Vec2::new(sw, sh)),
                0.0,
                Stroke::new(2.0, Color32::from_rgb(26, 115, 232)),
                egui::StrokeKind::Inside,
            );
        }

        // 远端光标
        for (name, color, rr, cc) in self.remote.values() {
            let x = gx(*cc);
            let y = gy(*rr);
            if x + cols.size(*cc) < origin.x + HEAD_W || y + rows.size(*rr) < origin.y + HEAD_H {
                continue;
            }
            if x > origin.x + avail.x || y > origin.y + avail.y {
                continue;
            }
            let col = match color % 7 {
                0 => Color32::from_rgb(230, 81, 0),
                1 => Color32::from_rgb(0, 121, 107),
                2 => Color32::from_rgb(106, 27, 154),
                3 => Color32::from_rgb(2, 119, 189),
                4 => Color32::from_rgb(198, 40, 40),
                5 => Color32::from_rgb(46, 125, 50),
                _ => Color32::from_rgb(255, 143, 0),
            };
            painter.rect_stroke(
                Rect::from_min_size(egui::pos2(x, y), Vec2::new(cols.size(*cc), rows.size(*rr))),
                0.0,
                Stroke::new(2.0, col),
                egui::StrokeKind::Inside,
            );
            painter.text(
                egui::pos2(x + 2.0, y - 2.0),
                Align2::LEFT_BOTTOM,
                name,
                FontId::proportional(10.0),
                col,
            );
        }

        // 拖拽框选
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let dx = pos.x - (origin.x + HEAD_W);
                let dy = pos.y - (origin.y + HEAD_H);
                if dx >= 0.0 && dy >= 0.0 {
                    let c = cols.index_at(off_x + dx);
                    let r = rows.index_at(off_y + dy);
                    self.sel_end = (r, c);
                }
            }
        }

        // 点击选中
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let dx = pos.x - (origin.x + HEAD_W);
                let dy = pos.y - (origin.y + HEAD_H);
                if dx >= 0.0 && dy >= 0.0 {
                    let c = cols.index_at(off_x + dx);
                    let r = rows.index_at(off_y + dy);
                    if self.editing.is_some() {
                        self.commit_edit(None);
                    }
                    self.sel = match merges
                        .iter()
                        .find(|(r1, c1, r2, c2)| r >= *r1 && r <= *r2 && c >= *c1 && c <= *c2)
                    {
                        Some((mr1, mc1, _, _)) => (*mr1, *mc1),
                        None => (r, c),
                    };
                    self.sel_end = self.sel;
                }
            }
        }

        if !dump.is_empty() {
            if let Ok(path) = std::env::var("SC_DUMP") {
                let _ = std::fs::write(path, dump.join("
"));
            }
        }

        // 编辑框
        if let Some(mut ed) = self.editing.take() {
            let rect = Rect::from_min_size(
                egui::pos2(sx, sy),
                Vec2::new(sw.max(160.0), sh.max(rows.size(sr1))),
            );
            let out = ui.put(rect, egui::TextEdit::singleline(&mut ed.buf));
            out.request_focus();
            if ctx.input(|i| i.key_pressed(Key::Enter)) {
                self.commit_edit(Some((1, 0)));
            } else if ctx.input(|i| i.key_pressed(Key::Escape)) {
                // 放弃编辑
            } else if ctx.input(|i| i.key_pressed(Key::Tab)) {
                self.commit_edit(Some((0, 1)));
            } else {
                self.editing = Some(ed);
            }
        }
    }

    fn start_host(&mut self) {
        let site = rand_site();
        let port = net::DEFAULT_PORT;
        match net::host(site, self.my_name.clone(), self.wb.title(), port, self.room_password.clone()) {
            Ok(n) => {
                let mut peer = Peer::new(site);
                // 把所有工作表的内容都放进初始快照
                let mut all: Vec<(usize, u32, u32, sheetcore::Cell)> = Vec::new();
                for (si, sheet) in self.wb.sheets.iter().enumerate() {
                    for (k, v) in sheet.iter() {
                        all.push((si, k.row, k.col, v.clone()));
                    }
                }
                for (si, r, c, cell) in all {
                    peer.local_set(si, r, c, &cell);
                }
                self.peers_list = vec![(site, self.my_name.clone(), (site % 7) as u8)];
                // 把已有内容塞进宿主的快照存储，后来者才能拿到全量
                for op in peer.snapshot() {
                    n.broadcast_op(op);
                }
                self.peer = Some(peer);
                self.net = Some(n);
                self.status = format!("已共享，同事可加入：{}:{}", local_ip_hint(), port);
            eprintln!("[net] {}", self.status);
            }
            Err(e) => self.status = format!("共享失败: {e}"),
        }
    }

    fn join_shared(&mut self) {
        let site = rand_site();
        let addr = self.join_addr.trim().to_string();
        if addr.is_empty() {
            self.status = "请填写宿主地址，例如 192.168.1.20:7878".into();
            return;
        }
        match net::join(site, self.my_name.clone(), self.wb.title(), &addr, self.room_password.clone()) {
            Ok(n) => {
                self.peer = Some(Peer::new(site));
                self.peers_list = vec![(site, self.my_name.clone(), (site % 7) as u8)];
                self.net = Some(n);
                self.status = format!("正在连接 {addr} …");
            }
            Err(e) => self.status = format!("连接失败: {e}"),
        }
    }

    fn poll_net(&mut self) {
        let mut events = Vec::new();
        if let Some(n) = self.net.as_ref() {
            while let Ok(e) = n.events.try_recv() {
                events.push(e);
            }
        }
        if events.is_empty() {
            return;
        }
        for e in events {
            match e {
                net::NetEvent::Connected => {
                    eprintln!("[net] connected");
                    self.status = "已连接".into();
                }
                net::NetEvent::Welcome { peers } => {
                    let me = self.peer.as_ref().map(|p| p.site).unwrap_or(0);
                    self.peers_list = peers
                        .into_iter()
                        .filter(|(s, _, _)| *s != me)
                        .collect();
                    let my_name = self.my_name.clone();
                    self.peers_list.push((me, my_name, (me % 7) as u8));
                    self.status = format!("已加入，在线 {} 人", self.peers_list.len());
                }
                net::NetEvent::PeerJoined(site, name, color) => {
                    eprintln!("[net] peer joined: {name}#{site}");
                    self.peers_list.retain(|(s, _, _)| *s != site);
                    self.peers_list.push((site, name, color));
                    self.status = format!("在线 {} 人", self.peers_list.len());
                }
                net::NetEvent::PeerLeft(site) => {
                    self.peers_list.retain(|(s, _, _)| *s != site);
                    self.remote.remove(&site);
                    self.status = format!("有人离开，在线 {} 人", self.peers_list.len());
                }
                net::NetEvent::Ops(ops) => {
                    eprintln!("[net] ops: {}", ops.len());
                    let is_snapshot = !self.joined_doc;
                    if is_snapshot {
                        // 第一次收到的是宿主全量快照，本地旧内容要清掉
                        self.joined_doc = true;
                        self.wb.sheet_mut(self.sheet_idx).clear();
                    }
                    let mut touched: Vec<(usize, u32, u32)> = Vec::new();
                    if let Some(peer) = self.peer.as_mut() {
                        for op in ops {
                            let key = (op.sheet, op.row, op.col);
                            if peer.apply(op) {
                                touched.push(key);
                            }
                        }
                    }
                    if !touched.is_empty() {
                        if is_snapshot {
                            if let Some(peer) = self.peer.as_ref() {
                                peer.apply_all_to_workbook(&mut self.wb);
                            }
                            sheetcore::formula::recalc_all(&mut self.wb);
                            self.deps = sheetcore::formula::deps::Deps::build(&self.wb);
                        } else {
                            if let Some(peer) = self.peer.as_ref() {
                                let keys: Vec<(usize, u32, u32)> =
                                    touched.iter().map(|(si, r, c)| (*si, *r, *c)).collect();
                                peer.apply_list_to_workbook(&mut self.wb, &keys);
                            }
                            let seeds: Vec<sheetcore::formula::deps::CellKey> =
                                touched.iter().map(|(si, r, c)| (*si, *r, *c)).collect();
                            for k in &seeds {
                                self.deps.update_cell(&self.wb, *k);
                            }
                            sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &seeds);
                        }
                        self.wb.dirty = true;
                    }
                }
                net::NetEvent::Cursor(c) => {
                    let me = self.peer.as_ref().map(|p| p.site).unwrap_or(0);
                    if c.site != me && c.sheet == self.sheet_idx {
                        self.remote.insert(c.site, (c.name, c.color, c.row, c.col));
                    }
                }
                net::NetEvent::Disconnected(msg) => {
                    self.status = format!("{msg}，正在自动重连…");
                    eprintln!("[net] disconnected: {msg}");
                }
                net::NetEvent::Denied(reason) => {
                    eprintln!("[net] denied: {reason}");
                    self.status = format!("加入失败：{reason}");
                    self.net = None;
                    self.peer = None;
                    self.peers_list.clear();
                }
                net::NetEvent::Error(msg) => self.status = msg,
            }
        }
    }
}

fn rand_site() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
    (nanos % 90_000) + 1
}

fn default_name() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "用户".into())
}

fn local_ip_hint() -> String {
    // 取本机在局域网里的地址（UDP connect 技巧，不会真的发包）
    use std::net::UdpSocket;
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(a) = s.local_addr() {
                return a.ip().to_string();
            }
        }
    }
    "本机IP".into()
}

impl eframe::App for SheetApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_net();
        self.poll_open();
        self.poll_save();
        // 打开期间被编辑打断的全量重算，等用户停手 3 秒后补上
        if self.needs_full
            && !self.busy
            && self
                .last_edit
                .map(|t| t.elapsed().as_secs_f32() > 3.0)
                .unwrap_or(true)
        {
            let wb_now = self.wb.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.open_rx = Some(rx);
            self.busy = true;
            self.needs_full = false;
            self.doc_gen = 0;
            self.status = "空闲，正在后台补算…".into();
            std::thread::spawn(move || {
                let mut wb2 = wb_now;
                sheetcore::formula::recalc_all(&mut wb2);
                let d = sheetcore::formula::deps::Deps::build(&wb2);
                let _ = tx.send(OpenMsg::Recalced(Box::new(wb2), Box::new(d)));
            });
        }
        // 协作或后台重算时保持刷新
        if self.net.is_some() || self.busy || self.save_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("打开").clicked() {
                    self.open();
                }
                if ui.button("保存").clicked() {
                    self.save(false);
                }
                if ui.button("另存为").clicked() {
                    self.save(true);
                }
                ui.separator();
                if self.net.is_none() {
                    if ui.button("共享").on_hover_text("把当前文件共享到局域网").clicked() {
                        self.start_host();
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.join_addr)
                            .hint_text("192.168.1.20:7878")
                            .desired_width(150.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.room_password)
                            .hint_text("口令(可空)")
                            .desired_width(90.0)
                            .password(true),
                    );
                    if ui.button("加入").clicked() {
                        self.join_shared();
                    }
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(46, 125, 50),
                        format!("协作中 · 在线 {}", self.peers_list.len()),
                    );
                }
                ui.separator();
                ui.label(format!("{}{}", self.wb.title(), if self.wb.dirty { " *" } else { "" }));
                ui.separator();
                ui.label(self.status.clone());
            });
            self.toolbar_format(ui);
        });

        egui::TopBottomPanel::top("formula").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (r, c) = self.sel;
                ui.add_sized([64.0, 20.0], egui::Label::new(cell_name(r, c)));
                ui.separator();
                let cur = match self.wb.sheet(self.sheet_idx).get(r, c) {
                    Some(cell) => cell.formula.as_deref().map(|f| f.to_string()).unwrap_or_else(|| cell.value.display()),
                    None => String::new(),
                };
                ui.label(if cur.is_empty() { " ".to_string() } else { cur });
            });
        });

        egui::TopBottomPanel::bottom("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for i in 0..self.wb.sheets.len() {
                    let name = self.wb.sheets[i].name.clone();
                    if ui.selectable_label(self.sheet_idx == i, name).clicked() {
                        self.commit_edit(None);
                        self.sheet_idx = i;
                        self.sel = (0, 0);
                    }
                }
            });
        });

        self.tree.pump();
        // 选区变化时把光标位置广播出去
        if let (Some(net), Some(peer)) = (self.net.as_ref(), self.peer.as_ref()) {
            if self.last_cursor != Some(self.sel) {
                self.last_cursor = Some(self.sel);
                net.broadcast_cursor(Cursor {
                    sheet: self.sheet_idx,
                    site: peer.site,
                    name: self.my_name.clone(),
                    color: (peer.site % 7) as u8,
                    row: self.sel.0,
                    col: self.sel.1,
                });
            }
        }
        let mut to_open: Option<PathBuf> = None;
        egui::SidePanel::left("files")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                to_open = self.tree.ui(ui);
            });
        if let Some(p) = to_open {
            self.open_path(p);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                self.handle_keys(ctx);
                self.draw_grid(ui, ctx);
            });
    }
}

fn install_cjk_font(ctx: &egui::Context) {
    if std::env::var("SHEETS_NO_CJK").is_ok() {
        return;
    }
    // egui 自带字体没有中文，加载系统字体，否则界面上的中文是方块
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    // 优先用内置子集字体（GB2312 常用字，约 2.8 MB），避免加载 16 MB 全量字体
    const EMBEDDED: &[u8] = include_bytes!("../../../assets/cjk-subset.ttf");
    {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_static(EMBEDDED).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().insert(0, "cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }

    #[allow(unreachable_code)]
    for path in CANDIDATES {
        let Ok(data) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_owned(data).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().insert(0, "cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut initial: Option<PathBuf> = None;
    let mut auto_host = false;
    let mut auto_join: Option<String> = None;
    let mut auto_pass: Option<String> = None;
    for a in args {
        if a == "--host" {
            auto_host = true;
        } else if let Some(addr) = a.strip_prefix("--join=") {
            auto_join = Some(addr.to_string());
        } else if let Some(pw) = a.strip_prefix("--pass=") {
            auto_pass = Some(pw.to_string());
        } else if !a.starts_with("--") {
            initial = Some(PathBuf::from(a));
        }
    }
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("Sheets Native (v1.6)"),
        ..Default::default()
    };
    eframe::run_native(
        "sheets-native",
        opts,
        {
            let initial = initial.clone();
            let auto_join = auto_join.clone();
            Box::new(move |cc| {
                let mut app = SheetApp::new(cc, initial.clone());
                if let Some(pw) = auto_pass.clone() {
                    app.room_password = pw;
                }
                // 打开文件是异步的：共享/加入要等表读进来再执行，
                // 否则宿主快照是空的（客户端只会收到 0 条操作）
                if initial.is_some() {
                    app.pending_host = auto_host;
                    app.pending_join = auto_join.clone();
                } else if auto_host {
                    app.start_host();
                } else if let Some(addr) = auto_join.clone() {
                    app.join_addr = addr;
                    app.join_shared();
                }
                Ok(Box::new(app))
            })
        },
    )
}

/// 画一条边框线（支持双线/虚线/点线）
fn draw_edge(painter: &egui::Painter, a: egui::Pos2, b: egui::Pos2, bd: &sheetcore::style::Border) {
    if bd.style == 0 {
        return;
    }
    let color = egui::Color32::from_rgb(bd.color[0], bd.color[1], bd.color[2]);
    let w = bd.px().max(0.8);
    let len = (b - a).length();
    if len < 0.5 {
        return;
    }
    match bd.style {
        4 => {
            // 双线：两条细线
            let off = egui::vec2(0.0, 1.2);
            painter.line_segment([a - off, b - off], egui::Stroke::new(1.0, color));
            painter.line_segment([a + off, b + off], egui::Stroke::new(1.0, color));
        }
        5 | 6 => {
            // 虚线/点线：等距短段（返回 ()，与其它分支一致）
            let (dash, gap) = if bd.style == 5 { (4.0, 3.0) } else { (1.5, 2.5) };
            let dir = (b - a) / len;
            let mut t = 0.0;
            while t < len {
                let e = (t + dash).min(len);
                painter.line_segment([a + dir * t, a + dir * e], egui::Stroke::new(w, color));
                t = e + gap;
            }
        }
        _ => {
            painter.line_segment([a, b], egui::Stroke::new(w, color));
        }
    }
}
