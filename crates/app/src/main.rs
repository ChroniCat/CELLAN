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
                    let n: usize = self.wb.sheets.iter().map(|s| s.cells.len()).sum();
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
                    let n: usize = self.wb.sheets.iter().map(|s| s.cells.len()).sum();
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
                Cell { value: Value::Text(ed.buf.clone()), formula: Some(ed.buf.clone()) }
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

    /// 让选区留在可视范围内
    fn ensure_visible(&mut self) {
        let (r, c) = self.sel;
        let top = (self.scroll.y / ROW_H) as u32;
        let left = (self.scroll.x / COL_W) as u32;
        let rows_on_screen = 30u32;
        let cols_on_screen = 10u32;
        if r < top {
            self.scroll.y = r as f32 * ROW_H;
        } else if r >= top + rows_on_screen {
            self.scroll.y = (r + 1 - rows_on_screen) as f32 * ROW_H;
        }
        if c < left {
            self.scroll.x = c as f32 * COL_W;
        } else if c >= left + cols_on_screen {
            self.scroll.x = (c + 1 - cols_on_screen) as f32 * COL_W;
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
                Some(cell) => cell.formula.clone().unwrap_or_else(|| cell.value.display()),
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
    fn draw_grid(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let avail = ui.available_size();
        let (resp, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
        let origin = resp.rect.min;
        painter.rect_filled(resp.rect, 0.0, Color32::WHITE);

        let grid_w = HEAD_W + avail.x;
        let grid_h = HEAD_H + avail.y;

        // 滚轮
        if resp.hovered() {
            let d = ui.input(|i| i.raw_scroll_delta);
            self.scroll -= d;
        }
        self.scroll.x = self.scroll.x.clamp(0.0, 4000.0 * COL_W);
        self.scroll.y = self.scroll.y.clamp(0.0, 1_048_576.0 * ROW_H);

        let first_col = (self.scroll.x / COL_W).floor().max(0.0) as u32;
        let first_row = (self.scroll.y / ROW_H).floor().max(0.0) as u32;
        let off_x = first_col as f32 * COL_W - self.scroll.x;
        let off_y = first_row as f32 * ROW_H - self.scroll.y;
        let n_cols = ((avail.x - HEAD_W) / COL_W).ceil().max(1.0) as u32 + 1;
        let n_rows = ((avail.y - HEAD_H) / ROW_H).ceil().max(1.0) as u32 + 1;

        let text_color = Color32::from_rgb(24, 24, 24);
        let grid_line = Color32::from_rgb(228, 228, 228);
        let head_bg = Color32::from_rgb(244, 245, 247);

        // 列标题
        painter.rect_filled(
            Rect::from_min_size(origin, Vec2::new(avail.x, HEAD_H)),
            0.0,
            head_bg,
        );
        for i in 0..n_cols {
            let c = first_col + i;
            let x = origin.x + HEAD_W + off_x + i as f32 * COL_W;
            if x + COL_W < origin.x {
                continue;
            }
            painter.text(
                egui::pos2(x + COL_W / 2.0, origin.y + HEAD_H / 2.0),
                Align2::CENTER_CENTER,
                col_label(c),
                FontId::proportional(12.0),
                Color32::from_rgb(90, 90, 90),
            );
            painter.line_segment(
                [egui::pos2(x, origin.y), egui::pos2(x, origin.y + grid_h)],
                Stroke::new(1.0, grid_line),
            );
        }

        // 行标题
        for i in 0..n_rows {
            let r = first_row + i;
            let y = origin.y + HEAD_H + off_y + i as f32 * ROW_H;
            if y + ROW_H > origin.y + HEAD_H {
                painter.text(
                    egui::pos2(origin.x + HEAD_W / 2.0, y + ROW_H / 2.0),
                    Align2::CENTER_CENTER,
                    (r + 1).to_string(),
                    FontId::proportional(12.0),
                    Color32::from_rgb(90, 90, 90),
                );
            }
            painter.line_segment(
                [egui::pos2(origin.x, y), egui::pos2(origin.x + avail.x, y)],
                Stroke::new(1.0, grid_line),
            );
        }

        // 单元格
        let sheet = self.wb.sheet(self.sheet_idx);
        for i in 0..n_rows {
            let r = first_row + i;
            let y = origin.y + HEAD_H + off_y + i as f32 * ROW_H;
            if y + ROW_H < origin.y + HEAD_H || y > origin.y + avail.y {
                continue;
            }
            for j in 0..n_cols {
                let c = first_col + j;
                let x = origin.x + HEAD_W + off_x + j as f32 * COL_W;
                if x + COL_W < origin.x + HEAD_W || x > origin.x + avail.x {
                    continue;
                }
                let rect = Rect::from_min_size(egui::pos2(x, y), Vec2::new(COL_W, ROW_H));
                if self.sel == (r, c) {
                    painter.rect_filled(rect, 0.0, Color32::from_rgb(224, 238, 252));
                }
                if let Some(cell) = sheet.get(r, c) {
                    let txt = cell.value.display();
                    if !txt.is_empty() {
                        let is_num = matches!(cell.value, Value::Number(_));
                        if is_num {
                            painter.text(
                                egui::pos2(x + COL_W - 4.0, y + ROW_H / 2.0),
                                Align2::RIGHT_CENTER,
                                txt,
                                FontId::proportional(13.0),
                                text_color,
                            );
                        } else {
                            painter.text(
                                egui::pos2(x + 4.0, y + ROW_H / 2.0),
                                Align2::LEFT_CENTER,
                                txt,
                                FontId::proportional(13.0),
                                text_color,
                            );
                        }
                    }
                }
            }
        }

        // 选区边框
        let sx = origin.x + HEAD_W + off_x + (self.sel.1 - first_col) as f32 * COL_W;
        let sy = origin.y + HEAD_H + off_y + (self.sel.0 - first_row) as f32 * ROW_H;
        if sx >= origin.x + HEAD_W - COL_W && sy >= origin.y + HEAD_H - ROW_H {
            painter.rect_stroke(
                Rect::from_min_size(egui::pos2(sx, sy), Vec2::new(COL_W, ROW_H)),
                0.0,
                Stroke::new(2.0, Color32::from_rgb(26, 115, 232)),
                egui::StrokeKind::Inside,
            );
        }

        // 远端光标
        for (name, color, r, c) in self.remote.values() {
            if *r < first_row || *c < first_col {
                continue;
            }
            let x = origin.x + HEAD_W + off_x + (*c - first_col) as f32 * COL_W;
            let y = origin.y + HEAD_H + off_y + (*r - first_row) as f32 * ROW_H;
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
                Rect::from_min_size(egui::pos2(x, y), Vec2::new(COL_W, ROW_H)),
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

        // 点击选中
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let dx = pos.x - (origin.x + HEAD_W + off_x);
                let dy = pos.y - (origin.y + HEAD_H + off_y);
                if dx >= 0.0 && dy >= 0.0 {
                    let c = first_col + (dx / COL_W).floor() as u32;
                    let r = first_row + (dy / ROW_H).floor() as u32;
                    if self.editing.is_some() {
                        self.commit_edit(None);
                    }
                    self.sel = (r, c);
                }
            }
        }

        // 编辑框
        if let Some(mut ed) = self.editing.take() {
            let rect = Rect::from_min_size(egui::pos2(sx, sy), Vec2::new(COL_W.max(160.0), ROW_H));
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
                    for (k, v) in sheet.cells.iter() {
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
                        self.wb.sheet_mut(self.sheet_idx).cells.clear();
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
        });

        egui::TopBottomPanel::top("formula").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (r, c) = self.sel;
                ui.add_sized([64.0, 20.0], egui::Label::new(cell_name(r, c)));
                ui.separator();
                let cur = match self.wb.sheet(self.sheet_idx).get(r, c) {
                    Some(cell) => cell.formula.clone().unwrap_or_else(|| cell.value.display()),
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
            .with_title("Sheets Native (v1.2)"),
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
