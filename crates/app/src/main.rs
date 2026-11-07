mod files;
mod net;

use eframe::egui;

mod chart_view;
mod icons;
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

/// 不区分大小写地把 needle 换成 repl（保留原文其余部分）
fn replace_ignore_case(hay: &str, needle: &str, repl: &str, case_sensitive: bool) -> String {
    if needle.is_empty() {
        return hay.to_string();
    }
    if case_sensitive {
        return hay.replace(needle, repl);
    }
    let lower = hay.to_lowercase();
    let nl = needle.to_lowercase();
    let mut out = String::with_capacity(hay.len());
    let mut i = 0usize;
    while let Some(p) = lower[i..].find(&nl) {
        let at = i + p;
        out.push_str(&hay[i..at]);
        out.push_str(repl);
        i = at + nl.len();
    }
    out.push_str(&hay[i..]);
    out
}

/// 内部剪贴板：整块格子 + 样式号
#[derive(Clone)]
struct Clip {
    sheet: usize,
    rect: (u32, u32, u32, u32),
    cells: Vec<Vec<(sheetcore::model::Cell, u16)>>,
    cut: bool,
}

#[derive(Clone)]
struct DataDlg {
    tab: u8,
    /// (列, 是否升序, 是否启用)
    keys: Vec<(u32, bool, bool)>,
    header: bool,
    delim: String,
}

#[derive(Clone, Default)]
struct FindState {
    open: bool,
    find: String,
    replace: String,
    match_case: bool,
    whole_cell: bool,
    hits: usize,
}

#[derive(Default, Clone)]
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
    /// 一帧里多个面板的绘制记录
    dump_accum: String,
    /// 条件格式统计缓存（按 doc_gen 失效）
    cf_stats: sheetcore::cf::CfStats,
    cf_stats_gen: u64,
    /// 条件格式对话框输入
    cf_v1: String,
    cf_v2: String,
    /// 数据验证对话框草稿（Some 时显示）
    dv_edit: Option<sheetcore::dv::DataValidation>,
    /// 序列下拉：正在展开的单元格
    dv_open: Option<(u32, u32)>,
    /// 批注编辑：(行, 列, 文本)
    note_edit: Option<(u32, u32, String)>,
    /// 筛选下拉：列号 + 各选项是否勾选
    filter_popup: Option<(u32, Vec<(String, bool)>)>,
    /// 显示分页虚线
    show_pages: bool,
    /// 撤销/重做
    undo: sheetcore::undo::UndoStack,
    /// 编辑租约（客户端看到的）
    locks: sheetcore::sync::LockTable,
    /// 等待授权中的编辑（初始输入）
    pending_edit: Option<String>,
    /// 我当前持有的格
    my_lock: Option<(usize, u32, u32)>,
    /// 上次续期
    lock_renew: std::time::Instant,
    /// 单调时钟原点
    t0: std::time::Instant,
    /// 测试钩子用过没有
    probe_done: bool,
    style_done: bool,
    struct_done: bool,
    macro_done: bool,
    a_done: bool,
    b5_done: bool,
    b6_done: bool,
    c2_done: bool,
    b3_done: bool,
    b1_done: bool,
    /// 远端做过的结构改动（本地换文件后要重新套用）
    struct_history: Vec<(usize, sheetcore::structural::Structural)>,
    /// 已套用到当前工作簿的历史条数
    history_planted: usize,
    /// 上次套用时工作簿的 epoch
    history_last_epoch: u64,
    /// 工作簿被整体替换的次数（Loaded/Recalced 各算一次）
    wb_epoch: u64,
    probe_at: std::time::Instant,
    lock_first_at: Option<std::time::Instant>,
    /// 内部剪贴板（带格式，跨表可用）
    clip: Option<Clip>,
    /// 查找替换
    find: FindState,
    /// 缩放
    zoom: f32,
    /// 工作表重命名对话框
    rename_sheet: Option<(usize, String)>,
    /// 填充柄拖拽的终点（行或列，取决于方向）
    fill_drag: Option<(usize, u32, u32)>,
    /// 数据工具对话框：0=排序 1=分列
    data_dlg: Option<DataDlg>,
    /// 条件格式管理窗口
    cf_mgr: bool,
    /// 打印预览窗口
    print_preview: bool,
    /// 预览第几页（0 起）
    preview_page: usize,
    /// 图片纹理缓存（部件名 → 纹理；None 表示解不出来）
    img_cache: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    /// 成员列表窗口
    members_open: bool,
    /// Ctrl+Shift+Enter 的下一次提交按数组公式处理
    cse_pending: bool,
    /// 圈释无效数据
    circle_invalid: bool,
    /// 无效格集合（(行,列)）
    invalid_cells: std::collections::HashSet<(u32, u32)>,
    /// 无效集合是按哪一版算的
    invalid_gen: u64,
    /// 宏窗口：模块列表 + 选中项
    macro_win: Option<(Vec<sheetcore::vba_src::VbaModule>, usize)>,
    /// 新建图表草稿
    chart_edit: Option<sheetcore::charts::Chart>,
    /// 图表管理窗口
    chart_mgr: bool,
    /// 透视表草稿
    pivot_edit: Option<sheetcore::pivot::PivotSpec>,
    /// 透视表是否生成真正的 pivotTable 部件
    pivot_real: bool,
    /// 并排比较的第二个工作簿 + 表序号 + 滚动
    compare: Option<(sheetcore::model::Workbook, usize, Vec2)>,
    /// 同步滚动
    compare_sync: bool,
    /// 用独立窗口显示比较文件（否则同窗左右分栏）
    compare_window: bool,
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
            sel: {
                let mut se = (0u32, 0u32);
                if let Ok(v) = std::env::var("SC_SEL") {
                    if let Some((a, b)) = v.split_once(',') {
                        if let (Ok(r), Ok(c)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                            se = (r, c);
                        }
                    }
                }
                se
            },
            scroll: Vec2::ZERO,
            sel_end: {
                let mut se = (0u32, 0u32);
                if let Ok(v) = std::env::var("SC_SEL") {
                    if let Some((a, b)) = v.split_once(',') {
                        if let (Ok(r), Ok(c)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                            se = (r, c);
                        }
                    }
                }
                se
            },
            last_view: Vec2::new(800.0, 600.0),
            dump_once: true,
            dump_accum: String::new(),
            cf_stats: sheetcore::cf::CfStats::default(),
            cf_stats_gen: u64::MAX,
            cf_v1: "100".into(),
            cf_v2: "0".into(),
            dv_edit: None,
            dv_open: None,
            note_edit: None,
            filter_popup: None,
            show_pages: false,
            undo: sheetcore::undo::UndoStack::new(),
            locks: sheetcore::sync::LockTable::new(20_000),
            pending_edit: None,
            my_lock: None,
            lock_renew: std::time::Instant::now(),
            t0: std::time::Instant::now(),
            probe_done: false,
            style_done: false,
            struct_done: false,
            macro_done: false,
            a_done: false,
            b5_done: false,
            b6_done: false,
            c2_done: false,
            b3_done: false,
            b1_done: false,
            struct_history: Vec::new(),
            history_planted: 0,
            history_last_epoch: u64::MAX,
            wb_epoch: 0,
            probe_at: std::time::Instant::now(),
            lock_first_at: None,
            clip: None,
            find: FindState::default(),
            zoom: 1.0,
            rename_sheet: None,
            fill_drag: None,
            data_dlg: None,
            cf_mgr: false,
            print_preview: false,
            preview_page: 0,
            img_cache: std::collections::HashMap::new(),
            members_open: false,
            cse_pending: false,
            circle_invalid: false,
            invalid_cells: std::collections::HashSet::new(),
            invalid_gen: u64::MAX,
            macro_win: None,
            chart_edit: None,
            chart_mgr: false,
            pivot_edit: None,
            pivot_real: true,
            compare: {
                let mut c = None;
                if let Ok(p) = std::env::var("SC_COMPARE") {
                    if let Ok(wb) = sheetcore::xlsx::load(std::path::Path::new(&p)) {
                        c = Some((wb, 0, Vec2::ZERO));
                    }
                }
                c
            },
            compare_sync: true,
            compare_window: std::env::var("SC_WIN").is_ok(),
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
                    self.wb_epoch += 1;
                    self.deps = sheetcore::formula::deps::Deps::empty();
                    self.sheet_idx = 0;
                    // 调试：SC_SEL=r,c 时保留启动时的选区，便于无界面验证
                    if std::env::var("SC_SEL").is_err() {
                        self.sel = (0, 0);
                    }
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
                    self.wb_epoch += 1;
                    // 整份替换后条件格式统计必须重算，否则色阶/图标/数据条全空
                    self.cf_stats_gen = u64::MAX;
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
                .add_filter("Excel 工作簿", &["xlsx"])
                .add_filter("Excel 启用宏的工作簿", &["xlsm"])
                .set_file_name(if self.wb.has_vba {
                    format!("{}.xlsm", self.wb.title())
                } else {
                    format!("{}.xlsx", self.wb.title())
                })
                .save_file(),
        };
        let Some(path) = path else { return };

        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if self.wb.has_vba && ext == "xlsx" {
            self.status = "该文件含 VBA 宏，存成 .xlsx 会丢掉宏；已改存为 .xlsm".into();
        }
        let path = if self.wb.has_vba && ext == "xlsx" {
            path.with_extension("xlsm")
        } else {
            path
        };
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

    /// 无界面测试钩子：SC_LOCK=r,c 占住某格；SC_LOCK_TRY=r,c 反复尝试
    fn lock_probe(&mut self, ctx: &egui::Context) {
        let (Ok(kind), Ok(cell)) = (
            std::env::var("SC_LOCK").or_else(|_| std::env::var("SC_LOCK_TRY")),
            std::env::var("SC_LOCK_CELL"),
        ) else {
            return;
        };
        let _ = kind;
        let parts: Vec<u32> = cell.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if parts.len() != 2 {
            return;
        }
        if self.net.is_none() || self.open_rx.is_some() {
            return;
        }
        if let Some(_) = self.my_lock {
            // 测试钩子：占住 N 秒后主动放锁
            let hold: u64 = std::env::var("SC_LOCK_RELEASE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(u64::MAX);
            if self.lock_first_at.unwrap().elapsed().as_secs() >= hold {
                self.release_lock();
                if std::env::var("SC_LOCK").is_ok() {
                    eprintln!("[lock] released");
                    self.probe_done = true;
                }
            }
            return;
        }
        if self.probe_done {
            return;
        }
        if self.lock_first_at.is_none() {
            self.lock_first_at = Some(std::time::Instant::now());
        }
        let delay: u64 = std::env::var("SC_LOCK_DELAY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if self.lock_first_at.unwrap().elapsed().as_secs() < delay {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
            return;
        }
        if self.probe_at.elapsed().as_secs() < 2 {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
            return;
        }
        self.probe_at = std::time::Instant::now();
        self.sel = (parts[0], parts[1]);
        self.begin_edit(None);
        self.editing = None; // 只验租约，不开编辑框
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    /// 无界面自检：选区转表格 + 结构化引用 + 存盘
    fn b1_probe(&mut self) {
        if std::env::var("SC_B1").is_err() || self.open_rx.is_some() || self.b1_done {
            return;
        }
        self.b1_done = true;
        self.sel = (0, 0);
        self.sel_end = (5, 2);
        self.make_table();
        eprintln!("[B1] {}", self.status);
        let t = self.wb.sheet(self.sheet_idx).tables.first().cloned();
        eprintln!("[B1] 表格={:?}", t.as_ref().map(|x| (x.name.clone(), x.range, x.columns.clone())));
        let name = t.map(|x| x.name).unwrap_or_else(|| "Table1".into());
        let si = self.sheet_idx;
        let v = sheetcore::formula::eval_formula(&self.wb, si, &format!("=SUM({name}[金额])"));
        eprintln!("[B1] SUM({name}[金额]) = {v:?}");
        let out = std::path::PathBuf::from("F:/Finance/work/b1_out.xlsx");
        let _ = std::fs::remove_file(&out);
        match sheetcore::xlsx::save(&out, &self.wb) {
            Ok(_) => eprintln!("[B1] 存盘后表格={:?}", sheetcore::table::load(&out)),
            Err(e) => eprintln!("[B1] 保存失败 {e:#}"),
        }
    }

    /// 无界面自检：Ctrl+Shift+Enter 数组公式 → 存盘带 t="array"
    fn b3_probe(&mut self) {
        if std::env::var("SC_B3").is_err() || self.open_rx.is_some() || self.b3_done {
            return;
        }
        self.b3_done = true;
        self.sel = (1, 1);
        self.sel_end = (1, 3);
        self.begin_edit(Some("=SUM(A1:A3*C1:C3)".to_string()));
        self.cse_pending = true;
        self.commit_edit(None);
        eprintln!("[B3] 状态={} 数组={:?}", self.status, self.wb.sheet(self.sheet_idx).arrays);
        let out = std::path::PathBuf::from("F:/Finance/work/b3_out.xlsx");
        let _ = std::fs::remove_file(&out);
        match sheetcore::xlsx::save(&out, &self.wb) {
            Ok(_) => {
                let found = sheetcore::arr::scan(&out);
                eprintln!("[B3] 存盘后数组={found:?}");
                let b2 = self.wb.sheet(self.sheet_idx).get(1, 1).map(|c| c.value.display());
                let c2 = self.wb.sheet(self.sheet_idx).get(1, 2).map(|c| c.value.display());
                eprintln!("[B3] B2={b2:?} C2={c2:?}");
            }
            Err(e) => eprintln!("[B3] 保存失败 {e:#}"),
        }
    }

    /// 无界面自检：成员列表内容（谁在线、在哪个格）
    fn c2_probe(&mut self) {
        if std::env::var("SC_C2").is_err() || self.net.is_none() {
            return;
        }
        if self.t0.elapsed().as_secs() < 6 || self.c2_done {
            return;
        }
        self.c2_done = true;
        let mut members: Vec<String> = Vec::new();
        for (site, name, _) in self.peers_list.iter() {
            let pos = self.remote.get(site).map(|(_, _, r, c)| format!("({r},{c})"));
            members.push(format!("{name}#{site}@{pos:?}"));
        }
        eprintln!("[C2] 成员 {} 人: {:?}", members.len(), members);
    }

    /// 无界面自检：圈释无效 + 输入提示
    fn b6_probe(&mut self, ctx: &egui::Context) {
        if std::env::var("SC_B6").is_err() || self.open_rx.is_some() || self.b6_done {
            return;
        }
        self.b6_done = true;
        self.circle_invalid = true;
        self.invalid_gen = u64::MAX;
        self.compute_invalid();
        let mut cells: Vec<(u32, u32)> = self.invalid_cells.iter().copied().collect();
        cells.sort();
        eprintln!("[B6] 无效格 {:?}", cells);
        // 选中 A2 看提示
        self.sel = (1, 0);
        self.sel_end = (1, 0);
        let dv = sheetcore::dv::find(&self.wb.sheet(self.sheet_idx).dv, 1, 0);
        eprintln!(
            "[B6] A2 提示 = {:?} / {:?}",
            dv.as_ref().map(|d| d.prompt_title.clone()),
            dv.as_ref().map(|d| d.prompt_msg.clone())
        );
        let _ = ctx;
    }

    /// 无界面自检：条件格式管理器（描述文本 + 删规则 + 调顺序）
    fn b5_probe(&mut self) {
        if std::env::var("SC_B5").is_err() || self.open_rx.is_some() || self.b5_done {
            return;
        }
        self.b5_done = true;
        let sheet = self.wb.sheet(self.sheet_idx);
        let before: Vec<String> = sheet
            .cf
            .blocks
            .iter()
            .flat_map(|b| b.rules.iter().map(|r| r.kind.describe()))
            .collect();
        eprintln!("[B5] 原有 {} 条规则: {:?}", before.len(), before);
        // 删第一条规则
        {
            let sheet = self.wb.sheet_mut(self.sheet_idx);
            if let Some(b) = sheet.cf.blocks.first_mut() {
                if !b.rules.is_empty() {
                    b.rules.remove(0);
                }
            }
            sheet.cf.blocks.retain(|b| !b.rules.is_empty());
            sheet.cf_dirty = true;
        }
        let after: Vec<String> = self
            .wb
            .sheet(self.sheet_idx)
            .cf
            .blocks
            .iter()
            .flat_map(|b| b.rules.iter().map(|r| r.kind.describe()))
            .collect();
        eprintln!("[B5] 删除后 {} 条: {:?}", after.len(), after);
        // 调顺序
        let n = self.wb.sheet(self.sheet_idx).cf.blocks.len();
        if n >= 2 {
            let first_range = self.wb.sheet(self.sheet_idx).cf.blocks[0].ranges.clone();
            self.wb.sheet_mut(self.sheet_idx).cf.blocks.swap(0, 1);
            let new_first = self.wb.sheet(self.sheet_idx).cf.blocks[0].ranges.clone();
            eprintln!("[B5] 交换后首块区域 {:?} -> 原第二块 {:?}", first_range, new_first);
        }
    }

    /// 无界面自检：A 组（剪贴板/查找替换/工作表）
    fn a_group_probe(&mut self, ctx: &egui::Context) {
        if std::env::var("SC_A_TEST").is_err() || self.open_rx.is_some() || self.a_done {
            return;
        }
        self.a_done = true;
        // 剪贴板：A1:B2 -> D1
        self.sel = (0, 0);
        self.sel_end = (1, 1);
        self.clip_copy(false, ctx);
        self.sel = (0, 3);
        self.sel_end = (0, 3);
        self.clip_paste(false, None);
        let pasted = format!(
            "{}/{}/{}/{}",
            self.wb.sheet(self.sheet_idx).get(0, 3).map(|c| c.value.display()).unwrap_or_default(),
            self.wb.sheet(self.sheet_idx).get(0, 4).map(|c| c.value.display()).unwrap_or_default(),
            self.wb.sheet(self.sheet_idx).get(1, 3).map(|c| c.value.display()).unwrap_or_default(),
            self.wb.sheet(self.sheet_idx).get(1, 4).map(|c| c.value.display()).unwrap_or_default()
        );
        // 查找替换
        self.find.find = "研发".into();
        self.find.replace = "技术".into();
        self.replace_all();
        let replaced = self.wb.sheet(self.sheet_idx).get(1, 1).map(|c| c.value.display()).unwrap_or_default();
        let status_find = self.status.clone();
        // 工作表
        let before = self.wb.sheets.len();
        self.add_sheet();
        let new_name = self.wb.sheets.last().map(|s| s.name.clone()).unwrap_or_default();
        self.wb.sheets.last_mut().unwrap().name = "新表".into();
        self.move_sheet(self.wb.sheets.len() - 1, -1);
        let moved_to = self.sheet_idx;
        // 删掉刚建的那张（按名字找），别把原表删了
        if let Some(i) = self.wb.sheets.iter().position(|s| s.name == "新表") {
            self.delete_sheet(i);
        }
        // 数据工具：排序 / 删除重复 / 分列（先回到第一张表，前面动过工作表）
        self.sheet_idx = 0;
        {
            let (r1, c1, r2, c2) = (0u32, 0u32, 5u32, 2u32);
            self.sel = (r1, c1);
            self.sel_end = (r2, c2);
            let dlg = DataDlg {
                tab: 0,
                keys: vec![(1, true, true), (2, false, true)],
                header: true,
                delim: ",".into(),
            };
            self.run_sort(&dlg);
            let order: Vec<String> = (1..=5)
                .map(|r| self.wb.sheet(self.sheet_idx).get(r, 1).map(|c| c.value.display()).unwrap_or_default())
                .collect();
            eprintln!("[B2] 排序后部门列={order:?}");
            self.run_dedup();
            eprintln!("[B2] 去重状态={}", self.status);
            // 分列：D 列写 "甲,1" 再拆
            self.wb.sheet_mut(self.sheet_idx).set(0, 3, sheetcore::model::Cell {
                value: sheetcore::model::Value::Text("甲,1".into()), formula: None });
            self.sel = (0, 3);
            self.sel_end = (0, 3);
            self.run_split(',');
            let a = self.wb.sheet(self.sheet_idx).get(0, 3).map(|c| c.value.display()).unwrap_or_default();
            let b = self.wb.sheet(self.sheet_idx).get(0, 4).map(|c| c.value.display()).unwrap_or_default();
            eprintln!("[B2] 分列后 D1={a} E1={b}");
        }
        // 填充：1,2 拉到第 6 行应为 3,4,5,6
        self.wb.sheet_mut(self.sheet_idx).set(8, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(1.0), formula: None });
        self.wb.sheet_mut(self.sheet_idx).set(9, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(2.0), formula: None });
        self.sel = (8, 0);
        self.sel_end = (9, 0);
        self.fill_to(13, true);
        let filled: Vec<String> = (8..=13)
            .map(|r| self.wb.sheet(self.sheet_idx).get(r, 0).map(|c| c.value.display()).unwrap_or_default())
            .collect();
        self.autofit_col(0);
        let w = self.wb.sheet(self.sheet_idx).cols.size(0);
        eprintln!("[A] 填充={filled:?} 列宽={w:.0} 粘贴={pasted} 替换后B2={replaced} ({status_find}) 表数 {before}->{} 新表={new_name} 移动后位置={moved_to}",
            self.wb.sheets.len()
        );
    }

    /// 无界面自检：打开宏窗口并打印读到的模块
    fn macro_probe(&mut self) {
        if std::env::var("SC_MACRO").is_err() || self.open_rx.is_some() || self.macro_done {
            return;
        }
        self.macro_done = true;
        self.open_macro_view();
        if let Some((mods, _)) = &self.macro_win {
            for m in mods {
                eprintln!(
                    "[macro] {} [{}] {} 行，首行={:?}",
                    m.name,
                    m.kind,
                    m.lines,
                    m.source.lines().next().unwrap_or("")
                );
            }
        } else {
            eprintln!("[macro] 没有读到模块（状态={}）", self.status);
        }
    }

    /// 无界面自检：在顶部插两行，验证协作同步
    fn struct_probe(&mut self) {
        if std::env::var("SC_STRUCT_TEST").is_err() || self.open_rx.is_some() || self.struct_done {
            return;
        }
        self.struct_done = true;
        self.structural(sheetcore::structural::Structural::InsertRows { at: 0, n: 2 });
        eprintln!("[struct] 已插入 2 行");
    }

    /// 无界面自检：给 B2 加粗并广播，用来验格式同步
    fn style_probe(&mut self) {
        if std::env::var("SC_STYLE_TEST").is_err() || self.open_rx.is_some() || self.style_done {
            return;
        }
        self.style_done = true;
        self.sel = (1, 1);
        self.sel_end = (1, 1);
        self.apply_style(|st| st.bold = true);
        eprintln!("[style] 已加粗 B2 并广播");
    }

    /// 无界面自检：编辑 → 撤销 → 重做 → 格式撤销
    fn undo_probe(&mut self) {
        if std::env::var("SC_UNDO_TEST").is_err() || self.open_rx.is_some() || self.probe_done {
            return;
        }
        self.probe_done = true;
        let (r, c) = (1u32, 1u32);
        let orig = self
            .wb
            .sheet(self.sheet_idx)
            .get(r, c)
            .map(|x| x.value.display())
            .unwrap_or_default();
        // 1) 编辑
        self.sel = (r, c);
        self.editing = Some(EditState { buf: "999".into() });
        self.commit_edit(None);
        let after_edit = self.wb.sheet(self.sheet_idx).get(r, c).unwrap().value.display();
        // 2) 撤销
        self.do_undo(false);
        let after_undo = self.wb.sheet(self.sheet_idx).get(r, c).unwrap().value.display();
        // 3) 重做
        self.do_undo(true);
        let after_redo = self.wb.sheet(self.sheet_idx).get(r, c).unwrap().value.display();
        // 4) 格式：加粗后撤销
        self.sel = (r, c);
        self.apply_style(|st| st.bold = !st.bold);
        let bold_after = self.wb.styles.get(self.wb.sheet(self.sheet_idx).style_of(r, c)).bold;
        self.do_undo(false);
        let bold_undo = self.wb.styles.get(self.wb.sheet(self.sheet_idx).style_of(r, c)).bold;
        // 5) 清除后撤销
        self.sel = (r, c);
        self.wb
            .sheet_mut(self.sheet_idx)
            .set(r, c, Cell { value: Value::Empty, formula: None });
        self.undo.push(sheetcore::undo::Action {
            label: "清除".into(),
            snap_before: None,
            snap_after: None,
            by_site: 0,
            deltas: vec![sheetcore::undo::CellDelta {
                sheet: self.sheet_idx,
                row: r,
                col: c,
                before: Cell { value: Value::Number(7.0), formula: None },
                after: Cell { value: Value::Empty, formula: None },
                before_style: 0,
                after_style: 0,
            }],
            focus: (self.sheet_idx, r, c),
        });
        self.do_undo(false);
        let clear_undo = self.wb.sheet(self.sheet_idx).get(r, c).unwrap().value.display();
        eprintln!(
            "[undo] 原值={orig} 编辑后={after_edit} 撤销后={after_undo} 重做后={after_redo} 加粗={bold_after} 撤销后={bold_undo} 清除撤销={clear_undo} 深度={:?}",
            self.undo.depth()
        );
    }

    /// 把远端做过的结构改动套到当前工作簿上。
    /// 文件每次重新读入（Loaded/Recalced）都会得到「未改结构」的版本，
    /// 所以换过一次就要从头再套一遍。
    fn sync_history(&mut self) {
        if self.struct_history.is_empty() {
            return;
        }
        if self.wb_epoch != self.history_last_epoch {
            self.history_planted = 0;
            self.history_last_epoch = self.wb_epoch;
        }
        if self.history_planted >= self.struct_history.len() {
            return;
        }
        let items: Vec<(usize, sheetcore::structural::Structural)> =
            self.struct_history[self.history_planted..].to_vec();
        for (sheet, op) in items.iter() {
            if *sheet < self.wb.sheets.len() {
                // 格子由快照给（已经是新坐标），这里只搬合并/条件格式等结构
                self.wb
                    .sheet_mut(*sheet)
                    .apply_structural_structure_only(op);
            }
        }
        self.history_planted = self.struct_history.len();
        self.doc_gen = self.doc_gen.wrapping_add(1);
    }

    fn lock_now(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    /// 这个格子现在能不能由我编辑
    fn lock_holder(&self, row: u32, col: u32) -> Option<String> {
        let site = self.net.as_ref().map(|n| n.site);
        let Some(site) = site else { return None };
        if self.locks.is_locked_by_other(self.sheet_idx, row, col, site) {
            self.locks
                .holder(self.sheet_idx, row, col)
                .map(|h| h.name.clone())
        } else {
            None
        }
    }

    /// 开始编辑：没联网直接开；联网先申请租约，批准后才开
    fn begin_edit(&mut self, initial: Option<String>) {
        let (r, c) = self.sel;
        if let Some(holder) = self.lock_holder(r, c) {
            self.status = format!("{} 正在编辑 {}，请稍后", holder, cell_name(r, c));
            return;
        }
        let Some(net) = self.net.as_ref() else {
            let buf = initial.unwrap_or_else(|| {
                match self.wb.sheet(self.sheet_idx).get(r, c) {
                    Some(cell) => cell
                        .formula
                        .as_deref()
                        .map(|f| f.to_string())
                        .unwrap_or_else(|| cell.value.display()),
                    None => String::new(),
                }
            });
            self.editing = Some(EditState { buf });
            return;
        };
        let name = self.my_name.clone();
        net.request_lock(self.sheet_idx, r, c, &name);
        self.pending_edit = Some(initial.unwrap_or_default());
        self.status = format!("正在申请编辑 {}…", cell_name(r, c));
    }

    /// 编辑结束/放弃，交还租约
    fn release_lock(&mut self) {
        if let Some((sheet, row, col)) = self.my_lock.take() {
            if let Some(net) = self.net.as_ref() {
                net.release_lock(sheet, row, col);
            }
            let site = self.net.as_ref().map(|n| n.site).unwrap_or(0);
            self.locks.release(sheet, row, col, site);
        }
        self.pending_edit = None;
    }

    /// 编辑中定期续期，防止长时间思考时被判定超时
    fn renew_lock_if_needed(&mut self) {
        if self.my_lock.is_none() {
            return;
        }
        if self.lock_renew.elapsed().as_secs() < 8 {
            return;
        }
        self.lock_renew = std::time::Instant::now();
        if let (Some(net), Some((sheet, row, col))) = (self.net.as_ref(), self.my_lock) {
            let name = self.my_name.clone();
            net.request_lock(sheet, row, col, &name);
        }
    }


    /// 记一步撤销：先 capture，改完 finish
    fn txn_begin(&self, keys: &[(u32, u32)]) -> Vec<sheetcore::undo::CellDelta> {
        sheetcore::undo::capture(self.wb.sheet(self.sheet_idx), self.sheet_idx, keys)
    }

    fn txn_end(&mut self, label: &str, before: Vec<sheetcore::undo::CellDelta>) {
        let deltas = sheetcore::undo::finish(before, self.wb.sheet(self.sheet_idx));
        if deltas.is_empty() {
            return;
        }
        let focus = (self.sheet_idx, self.sel.0, self.sel.1);
        self.undo.push(sheetcore::undo::Action {
            label: label.to_string(),
            deltas,
            focus,
            snap_before: None,
            snap_after: None,
            by_site: 0,
        });
    }


    // ===== 剪贴板 =====
    fn clip_copy(&mut self, cut: bool, ctx: &egui::Context) {
        let (r1, c1, r2, c2) = self.sel_rect();
        if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
            self.status = "选区太大（超过 20 万格），不复制".into();
            return;
        }
        let si = self.sheet_idx;
        let mut cells = Vec::new();
        let mut tsv = String::new();
        for r in r1..=r2 {
            let mut row = Vec::new();
            let mut line = Vec::new();
            for c in c1..=c2 {
                let cell = self.wb.sheet(si).get(r, c).cloned().unwrap_or(sheetcore::model::Cell {
                    value: sheetcore::model::Value::Empty,
                    formula: None,
                });
                let st = self.wb.sheet(si).style_of(r, c);
                line.push(cell.value.display());
                row.push((cell, st));
            }
            tsv.push_str(&line.join("\t"));
            tsv.push('\n');
            cells.push(row);
        }
        ctx.copy_text(tsv);
        self.clip = Some(Clip { sheet: si, rect: (r1, c1, r2, c2), cells, cut });
        self.status = format!(
            "已{} {:?}（{} 行 × {} 列）",
            if cut { "剪切" } else { "复制" },
            (r1, c1, r2, c2),
            r2 - r1 + 1,
            c2 - c1 + 1
        );
    }

    /// values_only=true 时只贴值（不贴公式和格式）
    fn clip_paste(&mut self, values_only: bool, text: Option<String>) {
        let (r, c) = self.sel;
        let si = self.sheet_idx;
        let mut tsv_paste = false;
        let mut rows: Vec<Vec<(sheetcore::model::Cell, u16)>> = Vec::new();
        let mut cut_src: Option<(usize, (u32, u32, u32, u32))> = None;

        if let Some(clip) = self.clip.clone() {
            // 系统剪贴板里的内容跟内部剪贴板不一致时优先用系统的
            if let Some(t) = &text {
                if !t.trim_end().is_empty() && !self.clip_matches(t, &clip) {
                    rows = Self::parse_tsv(t);
                    tsv_paste = true;
                }
            }
            if !tsv_paste {
                let (sr1, sc1, _, _) = clip.rect;
                let dr = r as i64 - sr1 as i64;
                let dc = c as i64 - sc1 as i64;
                for (i, row) in clip.cells.iter().enumerate() {
                    let mut out = Vec::new();
                    for (j, (cell, st)) in row.iter().enumerate() {
                        let mut cell = cell.clone();
                        if !values_only {
                            if let Some(f) = &cell.formula {
                                let src_sheet = clip.sheet;
                                let name = self.wb.sheet(src_sheet).name.clone();
                                let _ = name;
                                let sheet_name = self.wb.sheet(si).name.clone();
                                let nf = sheetcore::refs::translate(f, dr, dc + (j as i64 - j as i64), &sheet_name);
                                cell.formula = Some(nf.into_boxed_str());
                            }
                        } else {
                            cell.formula = None;
                        }
                        out.push((cell, if values_only { 0 } else { *st }));
                    }
                    rows.push(out);
                }
                if clip.cut {
                    cut_src = Some((clip.sheet, clip.rect));
                }
            }
        } else if let Some(t) = text {
            rows = Self::parse_tsv(&t);
            tsv_paste = true;
        } else {
            self.status = "剪贴板是空的".into();
            return;
        }
        if rows.is_empty() {
            return;
        }

        // 写回去（可撤销）
        let h = rows.len() as u32;
        let w = rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        let keys = sheetcore::undo::rect_keys(r, c, r + h.saturating_sub(1), c + w.saturating_sub(1));
        let txn = self.txn_begin(&keys);
        for (i, row) in rows.iter().enumerate() {
            for (j, (cell, st)) in row.iter().enumerate() {
                let rr = r + i as u32;
                let cc = c + j as u32;
                self.wb.sheet_mut(si).set(rr, cc, cell.clone());
                if !values_only && !tsv_paste {
                    self.wb.sheet_mut(si).set_style(rr, cc, *st);
                }
            }
        }
        self.txn_end(if values_only { "粘贴数值" } else { "粘贴" }, txn);
        // 剪切：清空源区域
        if let Some((src_sheet, (sr1, sc1, sr2, sc2))) = cut_src {
            for rr in sr1..=sr2 {
                for cc in sc1..=sc2 {
                    self.wb.sheet_mut(src_sheet).set(rr, cc, sheetcore::model::Cell {
                        value: sheetcore::model::Value::Empty,
                        formula: None,
                    });
                }
            }
            self.clip = None;
        }
        // 重算 + 广播
        let mut touched: Vec<(usize, u32, u32)> = Vec::new();
        for rr in r..r + h {
            for cc in c..c + w {
                touched.push((si, rr, cc));
            }
        }
        self.recalc_and_broadcast(&touched);
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已粘贴 {h} 行 × {w} 列");
    }

    fn clip_matches(&self, text: &str, clip: &Clip) -> bool {
        let mut expect = String::new();
        for row in clip.cells.iter() {
            let mut line = Vec::new();
            for (cell, _) in row.iter() {
                line.push(cell.value.display());
            }
            expect.push_str(&line.join("\t"));
            expect.push('\n');
        }
        expect == text
    }

    fn parse_tsv(text: &str) -> Vec<Vec<(sheetcore::model::Cell, u16)>> {
        let mut rows = Vec::new();
        for line in text.lines() {
            if line.is_empty() && rows.is_empty() {
                continue;
            }
            let mut row = Vec::new();
            for field in line.split('\t') {
                let cell = if field.starts_with('=') {
                    sheetcore::model::Cell {
                        value: sheetcore::model::Value::Text(field.into()),
                        formula: Some(field.into()),
                    }
                } else {
                    sheetcore::model::Cell {
                        value: sheetcore::model::Value::parse_input(field),
                        formula: None,
                    }
                };
                row.push((cell, 0u16));
            }
            rows.push(row);
        }
        rows
    }

    fn recalc_and_broadcast(&mut self, keys: &[(usize, u32, u32)]) {
        for k in keys {
            self.deps.update_cell(&self.wb, *k);
        }
        let deps = std::mem::replace(&mut self.deps, sheetcore::formula::deps::Deps::empty());
        sheetcore::formula::recalc_dirty(&mut self.wb, &deps, keys);
        self.deps = deps;
        if let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref()) {
            for (si, r, c) in keys.iter() {
                if let Some(cell) = self.wb.sheet(*si).get(*r, *c).cloned() {
                    let op = peer.local_set(*si, *r, *c, &cell);
                    net.broadcast_op(op);
                }
            }
        }
    }

    /// 填充柄：把源区域按线性序列/复制的方式填到目标区域
    fn fill_to(&mut self, target_row: u32, vertical: bool) {
        let (r1, c1, r2, c2) = self.sel_rect();
        if vertical {
            if target_row <= r2 {
                return;
            }
            let src_h = r2 - r1 + 1;
            let keys = sheetcore::undo::rect_keys(r1, c1, target_row, c2);
            let txn = self.txn_begin(&keys);
            for c in c1..=c2 {
                // 取源列的数字，判断是不是等差
                let nums: Vec<Option<f64>> = (r1..=r2)
                    .map(|r| match self.wb.sheet(self.sheet_idx).get(r, c).map(|x| x.value.clone()) {
                        Some(sheetcore::model::Value::Number(n)) => Some(n),
                        _ => None,
                    })
                    .collect();
                let all_num = nums.iter().all(|x| x.is_some());
                let step = if all_num && src_h >= 2 {
                    let a = nums[0].unwrap();
                    let b = nums[1].unwrap();
                    Some(b - a)
                } else {
                    None
                };
                for r in r2 + 1..=target_row {
                    let cell = if let Some(st) = step {
                        let base = nums[0].unwrap();
                        let idx = (r - r1) as f64;
                        sheetcore::model::Cell {
                            value: sheetcore::model::Value::Number(base + st * idx),
                            formula: None,
                        }
                    } else {
                        let src_r = r1 + (r - r2 - 1) % src_h;
                        self.wb
                            .sheet(self.sheet_idx)
                            .get(src_r, c)
                            .cloned()
                            .unwrap_or(sheetcore::model::Cell {
                                value: sheetcore::model::Value::Empty,
                                formula: None,
                            })
                    };
                    self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
                }
            }
            self.txn_end("填充", txn);
            self.sel_end = (target_row, c2);
        } else {
            let target_col = target_row; // 横向时传列号
            if target_col <= c2 {
                return;
            }
            let src_w = c2 - c1 + 1;
            let keys = sheetcore::undo::rect_keys(r1, c1, r2, target_col);
            let txn = self.txn_begin(&keys);
            for r in r1..=r2 {
                let nums: Vec<Option<f64>> = (c1..=c2)
                    .map(|c| match self.wb.sheet(self.sheet_idx).get(r, c).map(|x| x.value.clone()) {
                        Some(sheetcore::model::Value::Number(n)) => Some(n),
                        _ => None,
                    })
                    .collect();
                let all_num = nums.iter().all(|x| x.is_some());
                let step = if all_num && src_w >= 2 {
                    Some(nums[1].unwrap() - nums[0].unwrap())
                } else {
                    None
                };
                for c in c2 + 1..=target_col {
                    let cell = if let Some(st) = step {
                        let base = nums[0].unwrap();
                        let idx = (c - c1) as f64;
                        sheetcore::model::Cell {
                            value: sheetcore::model::Value::Number(base + st * idx),
                            formula: None,
                        }
                    } else {
                        let src_c = c1 + (c - c2 - 1) % src_w;
                        self.wb
                            .sheet(self.sheet_idx)
                            .get(r, src_c)
                            .cloned()
                            .unwrap_or(sheetcore::model::Cell {
                                value: sheetcore::model::Value::Empty,
                                formula: None,
                            })
                    };
                    self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
                }
            }
            self.txn_end("填充", txn);
            self.sel_end = (r2, target_col);
        }
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        let (a, b, c_, d) = self.sel_rect();
        let keys = sheetcore::undo::rect_keys(a, b, c_, d)
            .into_iter()
            .map(|(r, c)| (self.sheet_idx, r, c))
            .collect::<Vec<_>>();
        self.recalc_and_broadcast(&keys);
        self.status = "已填充".into();
    }

    /// 按内容自适应列宽
    fn autofit_col(&mut self, col: u32) {
        let mut width: f32 = 40.0;
        let (_, _, r2, _) = self.wb.sheet(self.sheet_idx).used_bounds().unwrap_or((0, 0, 0, 0));
        for r in 0..=r2.min(2000) {
            let Some(cell) = self.wb.sheet(self.sheet_idx).get(r, col) else { continue };
            let text = cell.value.display();
            if text.is_empty() {
                continue;
            }
            let size = {
                let st = self.wb.styles.get(self.wb.sheet(self.sheet_idx).style_of(r, col));
                if st.size > 0.0 { st.size } else { 11.0 }
            };
            let w: f32 = text
                .chars()
                .map(|ch| if (ch as u32) > 0x2e80 { size } else { size * 0.62 })
                .sum();
            width = width.max(w + 12.0);
        }
        let width = width.clamp(24.0, 400.0);
        self.wb.sheet_mut(self.sheet_idx).cols.set(col, width);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("{} 列宽已自适应：{:.0}px", cell_name(0, col).trim_end_matches('1'), width);
    }

    // ===== 数据工具 =====
    fn open_data_dlg(&mut self, tab: u8) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let header = sheetcore::sort_filter::has_header(self.wb.sheet(self.sheet_idx), r1, c1, r2, c2);
        let keys = (c1..=c2).map(|c| (c, true, c == c1)).collect();
        self.data_dlg = Some(DataDlg { tab, keys, header, delim: ",".into() });
    }

    fn run_sort(&mut self, dlg: &DataDlg) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let keys: Vec<sheetcore::data_tools::SortKey> = dlg
            .keys
            .iter()
            .filter(|(_, _, on)| *on)
            .map(|(c, asc, _)| sheetcore::data_tools::SortKey {
                col: *c,
                asc: *asc,
                ignore_case: true,
            })
            .collect();
        if keys.is_empty() {
            self.status = "至少选一列排序".into();
            return;
        }
        let txn = self.txn_begin(&sheetcore::undo::rect_keys(r1, c1, r2, c2));
        let moved = sheetcore::data_tools::sort_multi(
            self.wb.sheet_mut(self.sheet_idx),
            (r1, c1, r2, c2),
            &keys,
            dlg.header,
        );
        self.txn_end("多级排序", txn);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已按 {} 个键排序（{} 行移动）", keys.len(), moved);
    }

    fn run_dedup(&mut self) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let header = sheetcore::sort_filter::has_header(self.wb.sheet(self.sheet_idx), r1, c1, r2, c2);
        let txn = self.txn_begin(&sheetcore::undo::rect_keys(r1, c1, r2, c2));
        let removed = sheetcore::data_tools::remove_duplicates(
            self.wb.sheet_mut(self.sheet_idx),
            (r1, c1, r2, c2),
            &[],
            header,
        );
        self.txn_end("删除重复", txn);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = if removed == 0 {
            "没有重复行".into()
        } else {
            format!("删除了 {removed} 行重复")
        };
    }

    fn run_split(&mut self, delim: char) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let txn = self.txn_begin(&sheetcore::undo::rect_keys(r1, c1, r2, c2));
        let n = sheetcore::data_tools::text_to_columns(
            self.wb.sheet_mut(self.sheet_idx),
            (r1, c1, r2, c2),
            delim,
        );
        self.txn_end("分列", txn);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = if n <= 1 {
            "没有找到分隔符".into()
        } else {
            format!("已分成 {n} 列")
        };
    }

    /// 打印预览：按页面设置分页，把每一页画成一张纸
    fn print_preview_window(&mut self, ctx: &egui::Context) {
        if !self.print_preview {
            return;
        }
        let mut open = true;
        let mut goto_page: Option<usize> = None;
        let mut do_print = false;
        let mut do_export = false;
        // 先把要画的东西取出来，别在闭包里借 self
        let (layout, cols, rows, texts, page_no, total) = {
            let sheet = self.wb.sheet(self.sheet_idx);
            let layout = sheetcore::print::paginate(sheet, &sheet.page);
            let total = layout.pages.len().max(1);
            let page_no = self.preview_page.min(total - 1);
            let span = layout
                .pages
                .get(page_no)
                .cloned()
                .unwrap_or(sheetcore::print::PageSpan { rows: (0, 0), cols: (0, 0) });
            let mut texts: Vec<(u32, u32, String)> = Vec::new();
            for r in span.rows.0..=span.rows.1 {
                for c in span.cols.0..=span.cols.1 {
                    if let Some(cell) = sheet.get(r, c) {
                        let t = cell.value.display();
                        if !t.is_empty() {
                            texts.push((r, c, t));
                        }
                    }
                }
            }
            (
                layout,
                sheet.cols.clone(),
                sheet.rows.clone(),
                texts,
                page_no,
                total,
            )
        };
        let span = layout
            .pages
            .get(page_no)
            .cloned()
            .unwrap_or(sheetcore::print::PageSpan { rows: (0, 0), cols: (0, 0) });
        let (pw, ph) = layout.page_size;
        let (print_w, print_h) = layout.printable;
        let page_h = 560.0f32;
        let page_w = page_h * pw / ph;
        let scale_preview = page_h / ph;
        let margins = self.wb.sheet(self.sheet_idx).page.margins;
        let scale_txt = layout.scale;

        egui::Window::new("打印预览")
            .open(&mut open)
            .default_width(page_w + 280.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("◀").clicked() && page_no > 0 {
                        goto_page = Some(page_no - 1);
                    }
                    ui.label(format!("第 {} / {} 页", page_no + 1, total));
                    if ui.button("▶").clicked() && page_no + 1 < total {
                        goto_page = Some(page_no + 1);
                    }
                    ui.separator();
                    ui.label(format!(
                        "纸张 {pw:.0}×{ph:.0}pt 缩放 {:.0}%",
                        scale_txt * 100.0
                    ));
                    ui.separator();
                    if ui.button("打印…").clicked() {
                        do_print = true;
                    }
                    if ui.button("导出 PDF…").clicked() {
                        do_export = true;
                    }
                });
                ui.separator();
                egui::ScrollArea::both().show(ui, |ui| {
                    let (resp, painter) =
                        ui.allocate_painter(egui::vec2(page_w, page_h), egui::Sense::hover());
                    let page_rect = resp.rect;
                    painter.rect_filled(page_rect, 2.0, egui::Color32::WHITE);
                    painter.rect_stroke(
                        page_rect,
                        2.0,
                        egui::Stroke::new(1.0, egui::Color32::from_gray(150)),
                        egui::StrokeKind::Inside,
                    );
                    let inner = egui::Rect::from_min_max(
                        page_rect.min
                            + egui::vec2(
                                margins[0] * 72.0 * scale_preview,
                                margins[2] * 72.0 * scale_preview,
                            ),
                        page_rect.max
                            - egui::vec2(
                                margins[1] * 72.0 * scale_preview,
                                margins[3] * 72.0 * scale_preview,
                            ),
                    );
                    painter.rect_stroke(
                        inner,
                        0.0,
                        egui::Stroke::new(0.8, egui::Color32::from_gray(205)),
                        egui::StrokeKind::Inside,
                    );

                    // 一页内容的原始尺寸（96 DPI 像素）
                    let mut content_w = 0.0f32;
                    for c in span.cols.0..=span.cols.1 {
                        content_w += cols.size(c);
                    }
                    let mut content_h = 0.0f32;
                    for r in span.rows.0..=span.rows.1 {
                        content_h += rows.size(r);
                    }
                    let fit = (print_w * scale_preview / content_w.max(1.0))
                        .min(print_h * scale_preview / content_h.max(1.0));
                    let cell_scale = fit * scale_txt;

                    let mut y = inner.min.y;
                    for r in span.rows.0..=span.rows.1 {
                        let h = rows.size(r) * cell_scale;
                        if y > inner.max.y + 1.0 {
                            break;
                        }
                        let mut x = inner.min.x;
                        painter.line_segment(
                            [egui::pos2(inner.min.x, y), egui::pos2(inner.max.x, y)],
                            egui::Stroke::new(0.4, egui::Color32::from_gray(225)),
                        );
                        for c in span.cols.0..=span.cols.1 {
                            let w = cols.size(c) * cell_scale;
                            if x > inner.max.x + 1.0 {
                                break;
                            }
                            if let Some((_, _, t)) =
                                texts.iter().find(|(rr, cc, _)| *rr == r && *cc == c)
                            {
                                let fs = (11.0 * cell_scale * 1.05).clamp(4.0, 15.0);
                                let per = (fs * 0.62).max(1.0);
                                let max_chars = ((w - 2.0).max(1.0) / per) as usize + 1;
                                let clipped: String = t.chars().take(max_chars).collect();
                                painter.text(
                                    egui::pos2(x + 1.0, y + 1.0),
                                    egui::Align2::LEFT_TOP,
                                    clipped,
                                    egui::FontId::proportional(fs),
                                    egui::Color32::BLACK,
                                );
                            }
                            painter.line_segment(
                                [egui::pos2(x, y), egui::pos2(x, (y + h).min(inner.max.y))],
                                egui::Stroke::new(0.4, egui::Color32::from_gray(225)),
                            );
                            x += w;
                        }
                        y += h;
                    }
                    if std::env::var("SC_PREVIEW").is_ok() {
                        eprintln!(
                            "[preview] page={}/{} rows={:?} cols={:?} scale={:.2} paper={:.0}x{:.0} cells={}",
                            page_no + 1,
                            total,
                            span.rows,
                            span.cols,
                            scale_txt,
                            pw,
                            ph,
                            texts.len()
                        );
                    }
                });
            });
        if let Some(p) = goto_page {
            self.preview_page = p;
        }
        if do_export {
            self.export_pdf_dialog();
        }
        if do_print {
            self.print_to_printer();
        }
        self.print_preview = open;
    }

    /// 真打印：先导出 PDF，再交给系统默认处理程序打印
    fn print_to_printer(&mut self) {
        let path = std::env::temp_dir().join(format!(
            "sheetcore-print-{}.pdf",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ));
        let font = std::path::Path::new("C:/Windows/Fonts/simhei.ttf");
        match sheetcore::print::export_pdf(&self.wb, self.sheet_idx, &path, font) {
            Ok(_) => {
                let p = path.to_string_lossy().to_string();
                let r = std::process::Command::new("powershell")
                    .args([
                        "-NoProfile",
                        "-Command",
                        &format!("Start-Process -FilePath '{p}' -Verb Print"),
                    ])
                    .spawn();
                self.status = match r {
                    Ok(_) => format!("已交给系统打印：{p}"),
                    Err(e) => format!("打印失败：{e}"),
                };
            }
            Err(e) => self.status = format!("生成打印文件失败：{e:#}"),
        }
    }

    fn export_pdf_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.pdf", self.wb.title()))
            .add_filter("PDF", &["pdf"])
            .save_file()
        else {
            return;
        };
        let font = std::path::Path::new("C:/Windows/Fonts/simhei.ttf");
        match sheetcore::print::export_pdf(&self.wb, self.sheet_idx, &path, font) {
            Ok(_) => self.status = format!("已导出 {}", path.display()),
            Err(e) => self.status = format!("导出失败：{e:#}"),
        }
    }

    /// 解一张图片并缓存纹理（解不出来就记 None，不反复重试）
    fn img_texture(
        &mut self,
        key: &str,
        part: &str,
        ctx: &egui::Context,
    ) -> Option<egui::TextureHandle> {
        if let Some(v) = self.img_cache.get(key) {
            return v.clone();
        }
        let loaded = (|| {
            let path = self.wb.path.as_ref()?;
            let bytes = sheetcore::pic::read_part(path, part)?;
            let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            Some(ctx.load_texture(key.to_string(), color, egui::TextureOptions::LINEAR))
        })();
        self.img_cache.insert(key.to_string(), loaded.clone());
        loaded
    }

    /// 把选区转成 Excel 表格对象（ListObject），首行当列名
    fn make_table(&mut self) {
        let (r1, c1, r2, c2) = self.sel_rect();
        if r2 - r1 < 1 || c2 == c1 {
            self.status = "表格至少要两行两列（首行是列名）".into();
            return;
        }
        let mut names: Vec<String> = Vec::new();
        for c in c1..=c2 {
            let v = self
                .wb
                .sheet(self.sheet_idx)
                .get(r1, c)
                .map(|x| x.value.display())
                .unwrap_or_default();
            names.push(if v.trim().is_empty() {
                format!("列{}", c - c1 + 1)
            } else {
                v
            });
        }
        // 名字不能重复，Excel 也不允许
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for i in 0..names.len() {
            let mut base = names[i].clone();
            let mut k = 2;
            while seen.contains(&base.to_lowercase()) {
                base = format!("{}_{k}", names[i]);
                k += 1;
            }
            seen.insert(base.to_lowercase());
            names[i] = base;
        }
        let existing: std::collections::HashSet<String> = self
            .wb
            .sheet(self.sheet_idx)
            .tables
            .iter()
            .map(|t| t.name.to_lowercase())
            .collect();
        let mut n = 1usize;
        while existing.contains(&format!("table{n}")) {
            n += 1;
        }
        let name = format!("Table{n}");
        let t = sheetcore::table::Table {
            name: name.clone(),
            display: name.clone(),
            range: (r1, c1, r2, c2),
            columns: names,
            header_row: true,
            totals_row: false,
        };
        self.wb.sheet_mut(self.sheet_idx).tables.push(t);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已创建表格 {}（{}）", name, sheetcore::table::a1((r1, c1, r2, c2)));
    }

    /// 在线成员：谁在哪儿，点一下跳过去
    fn members_window(&mut self, ctx: &egui::Context) {
        if !self.members_open {
            return;
        }
        let mut open = true;
        let mut jump: Option<(u32, u32)> = None;
        egui::Window::new("在线成员")
            .open(&mut open)
            .default_width(260.0)
            .show(ctx, |ui| {
                let me = self.net.as_ref().map(|n| n.site);
                if self.peers_list.is_empty() && self.net.is_none() {
                    ui.label("当前没有共享");
                }
                for (site, name, color) in self.peers_list.clone() {
                    let is_me = Some(site) == me;
                    ui.horizontal(|ui| {
                        let c = match color % 7 {
                            0 => egui::Color32::from_rgb(230, 81, 0),
                            1 => egui::Color32::from_rgb(0, 121, 107),
                            2 => egui::Color32::from_rgb(106, 27, 154),
                            3 => egui::Color32::from_rgb(2, 119, 189),
                            4 => egui::Color32::from_rgb(198, 40, 40),
                            5 => egui::Color32::from_rgb(46, 125, 50),
                            _ => egui::Color32::from_rgb(255, 143, 0),
                        };
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 4.5, c);
                        let label = if is_me {
                            format!("{name}（我）")
                        } else {
                            name.clone()
                        };
                        ui.label(label);
                        // 远端光标位置
                        let pos = self
                            .remote
                            .get(&site)
                            .map(|(_, _, r, c)| (*r, *c))
                            .or(if is_me { Some(self.sel) } else { None });
                        if let Some((r, c)) = pos {
                            if !is_me {
                                if ui
                                    .small_button(format!("{}", cell_name(r, c)))
                                    .on_hover_text("跳到他所在的位置")
                                    .clicked()
                                {
                                    jump = Some((r, c));
                                }
                            } else {
                                ui.label(egui::RichText::new(cell_name(r, c)).weak());
                            }
                        }
                    });
                }
                ui.separator();
                ui.label(
                    egui::RichText::new("位置来自对方的选区广播，没动过的人不显示")
                        .weak()
                        .size(10.0),
                );
            });
        if let Some((r, c)) = jump {
            self.sel = (r, c);
            self.sel_end = (r, c);
            self.ensure_visible();
            self.status = format!("已跳到 {}", cell_name(r, c));
        }
        self.members_open = open;
    }

    /// 算出当前表里不符合数据验证的格子（上限 5 万格，避免卡死）
    fn compute_invalid(&mut self) {
        if self.invalid_gen == self.doc_gen {
            return;
        }
        self.invalid_gen = self.doc_gen;
        self.invalid_cells.clear();
        let si = self.sheet_idx;
        let dv_list = self.wb.sheet(si).dv.clone();
        if dv_list.is_empty() {
            return;
        }
        let mut budget = 50_000usize;
        let mut calc = sheetcore::formula::Calc::new(&self.wb);
        for dv in dv_list.iter() {
            for (r1, c1, r2, c2) in dv.ranges.iter().copied() {
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        if budget == 0 {
                            return;
                        }
                        budget -= 1;
                        let sheet = self.wb.sheet(si);
                        let Some(cell) = sheet.get(r, c) else { continue };
                        if matches!(cell.value, sheetcore::model::Value::Empty) {
                            continue;
                        }
                        let ok = sheetcore::dv::validate(dv, &cell.value, sheet, r, c, |f, rr, cc| {
                            let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), rr, cc);
                            match sheetcore::formula::parse(&shifted) {
                                Ok(node) => matches!(
                                    calc.eval_node(&node, si),
                                    sheetcore::formula::FValue::Bool(true)
                                ),
                                Err(_) => true,
                            }
                        });
                        if !ok {
                            self.invalid_cells.insert((r, c));
                        }
                    }
                }
            }
        }
    }

    /// 条件格式管理器：看规则、调顺序、删除、定位
    fn cf_manager(&mut self, ctx: &egui::Context) {
        if !self.cf_mgr {
            return;
        }
        let mut open = true;
        let mut del: Option<(usize, usize)> = None;
        let mut up: Option<usize> = None;
        let mut down: Option<usize> = None;
        let mut goto: Option<(u32, u32, u32, u32)> = None;
        let blocks = self.wb.sheet(self.sheet_idx).cf.blocks.clone();
        egui::Window::new("条件格式管理")
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                if blocks.is_empty() {
                    ui.label("这张表还没有条件格式规则");
                }
                for (bi, blk) in blocks.iter().enumerate() {
                    let range_text = blk
                        .ranges
                        .iter()
                        .map(|(r1, c1, r2, c2)| {
                            format!(
                                "{}{}:{}{}",
                                sheetcore::model::col_label(*c1),
                                r1 + 1,
                                sheetcore::model::col_label(*c2),
                                r2 + 1
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("区域 {range_text}")).strong());
                        if ui.small_button("定位").clicked() {
                            if let Some(r) = blk.ranges.first() {
                                goto = Some(*r);
                            }
                        }
                        if ui.small_button("上移").clicked() {
                            up = Some(bi);
                        }
                        if ui.small_button("下移").clicked() {
                            down = Some(bi);
                        }
                    });
                    for (ri, rule) in blk.rules.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("  · {}", rule.kind.describe()));
                            if rule.stop {
                                ui.label(egui::RichText::new("(命中即停)").weak());
                            }
                            if ui.small_button("删除").clicked() {
                                del = Some((bi, ri));
                            }
                        });
                    }
                    ui.separator();
                }
            });
        if let Some((bi, ri)) = del {
            let sheet = self.wb.sheet_mut(self.sheet_idx);
            if let Some(blk) = sheet.cf.blocks.get_mut(bi) {
                if ri < blk.rules.len() {
                    blk.rules.remove(ri);
                }
                if blk.rules.is_empty() {
                    sheet.cf.blocks.remove(bi);
                }
            }
            sheet.cf_dirty = true;
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.status = "已删除一条条件格式规则".into();
        }
        if let Some(bi) = up {
            if bi > 0 {
                self.wb.sheet_mut(self.sheet_idx).cf.blocks.swap(bi, bi - 1);
                self.wb.sheet_mut(self.sheet_idx).cf_dirty = true;
                self.doc_gen = self.doc_gen.wrapping_add(1);
                self.status = "规则已上移（优先级更高）".into();
            }
        }
        if let Some(bi) = down {
            let n = self.wb.sheet(self.sheet_idx).cf.blocks.len();
            if bi + 1 < n {
                self.wb.sheet_mut(self.sheet_idx).cf.blocks.swap(bi, bi + 1);
                self.wb.sheet_mut(self.sheet_idx).cf_dirty = true;
                self.doc_gen = self.doc_gen.wrapping_add(1);
                self.status = "规则已下移".into();
            }
        }
        if let Some((r1, c1, _, _)) = goto {
            self.sel = (r1, c1);
            self.sel_end = (r1, c1);
            self.ensure_visible();
        }
        self.cf_mgr = open;
    }

    fn data_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.data_dlg.take() else { return };
        let mut open = true;
        let mut run = false;
        let title = if dlg.tab == 0 { "多级排序" } else { "分列" };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                if dlg.tab == 0 {
                    ui.checkbox(&mut dlg.header, "首行是标题");
                    ui.separator();
                    ui.label("按顺序勾选排序列（靠前的优先）：");
                    for (c, asc, on) in dlg.keys.iter_mut() {
                        ui.horizontal(|ui| {
                            ui.checkbox(on, format!("{} 列", cell_name(0, *c).trim_end_matches('1')));
                            egui::ComboBox::from_id_salt(format!("ord{c}"))
                                .selected_text(if *asc { "升序" } else { "降序" })
                                .width(70.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(asc, true, "升序");
                                    ui.selectable_value(asc, false, "降序");
                                });
                        });
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.label("分隔符");
                        ui.add(egui::TextEdit::singleline(&mut dlg.delim).desired_width(60.0));
                    });
                    ui.label(egui::RichText::new("按分隔符拆到右边相邻列，右侧内容整体右移").weak());
                }
                ui.separator();
                if ui.button("执行").clicked() {
                    run = true;
                }
            });
        if run {
            if dlg.tab == 0 {
                self.run_sort(&dlg);
            } else {
                let ch = dlg.delim.chars().next().unwrap_or(',');
                self.run_split(ch);
            }
        } else if open {
            self.data_dlg = Some(dlg);
        }
    }

    // ===== 查找替换 =====
    fn find_next(&mut self, backwards: bool) {
        if self.find.find.is_empty() {
            return;
        }
        let si = self.sheet_idx;
        let needle = if self.find.match_case {
            self.find.find.clone()
        } else {
            self.find.find.to_lowercase()
        };
        let (mut r, mut c) = self.sel;
        let max_r = 1_048_575u32;
        let max_c = 16_383u32;
        let total = (max_r as u64 + 1) * (max_c as u64 + 1);
        let mut steps = 0u64;
        loop {
            steps += 1;
            if steps > total.min(4_000_000) {
                self.status = "没找到".into();
                return;
            }
            if backwards {
                if c == 0 {
                    if r == 0 {
                        r = max_r;
                        c = max_c;
                    } else {
                        r -= 1;
                        c = max_c;
                    }
                } else {
                    c -= 1;
                }
            } else {
                if c >= max_c {
                    c = 0;
                    r = if r >= max_r { 0 } else { r + 1 };
                } else {
                    c += 1;
                }
            }
            let Some(cell) = self.wb.sheet(si).get(r, c) else { continue };
            let text = cell.value.display();
            let hay = if self.find.match_case { text.clone() } else { text.to_lowercase() };
            let hit = if self.find.whole_cell { hay == needle } else { hay.contains(&needle) };
            if hit {
                self.sel = (r, c);
                self.sel_end = (r, c);
                self.ensure_visible();
                self.find.hits += 1;
                self.status = format!("找到 {}（第 {} 处）", cell_name(r, c), self.find.hits);
                return;
            }
            if r == self.sel.0 && c == self.sel.1 && steps > 2 {
                self.status = "没找到".into();
                return;
            }
        }
    }

    fn replace_current(&mut self) {
        let (r, c) = self.sel;
        let si = self.sheet_idx;
        let Some(cell) = self.wb.sheet(si).get(r, c).cloned() else { return };
        let text = cell.value.display();
        let (hay, needle) = if self.find.match_case {
            (text.clone(), self.find.find.clone())
        } else {
            (text.to_lowercase(), self.find.find.to_lowercase())
        };
        if !hay.contains(&needle) {
            self.find_next(false);
            return;
        }
        let new_text = if self.find.whole_cell {
            self.find.replace.clone()
        } else {
            replace_ignore_case(&text, &self.find.find, &self.find.replace, self.find.match_case)
        };
        let keys = vec![(si, r, c)];
        let txn = self.txn_begin(&[(r, c)]);
        self.wb
            .sheet_mut(si)
            .set(r, c, sheetcore::model::Cell { value: sheetcore::model::Value::parse_input(&new_text), formula: None });
        self.txn_end("替换", txn);
        self.recalc_and_broadcast(&keys);
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.find_next(false);
    }

    fn replace_all(&mut self) {
        let si = self.sheet_idx;
        let keys: Vec<(u32, u32)> = self
            .wb
            .sheet(si)
            .iter()
            .filter(|(_, cell)| matches!(cell.value, sheetcore::model::Value::Text(_)))
            .map(|(k, _)| (k.row, k.col))
            .collect();
        let needle = if self.find.match_case {
            self.find.find.clone()
        } else {
            self.find.find.to_lowercase()
        };
        let mut changed = 0usize;
        let mut touched = Vec::new();
        for (r, c) in keys {
            let Some(cell) = self.wb.sheet(si).get(r, c).cloned() else { continue };
            let text = cell.value.display();
            let hay = if self.find.match_case { text.clone() } else { text.to_lowercase() };
            let hit = if self.find.whole_cell { hay == needle } else { hay.contains(&needle) };
            if !hit {
                continue;
            }
            let new_text = if self.find.whole_cell {
                self.find.replace.clone()
            } else {
                replace_ignore_case(&text, &self.find.find, &self.find.replace, self.find.match_case)
            };
            self.wb.sheet_mut(si).set(
                r,
                c,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::parse_input(&new_text),
                    formula: None,
                },
            );
            touched.push((si, r, c));
            changed += 1;
        }
        if changed > 0 {
            self.recalc_and_broadcast(&touched);
            self.doc_gen = self.doc_gen.wrapping_add(1);
        }
        self.status = format!("替换了 {changed} 处");
    }

    // ===== 工作表 =====
    fn add_sheet(&mut self) {
        let n = self.wb.sheets.len() + 1;
        let mut name = format!("Sheet{n}");
        let mut k = n;
        while self.wb.sheets.iter().any(|s| s.name == name) {
            k += 1;
            name = format!("Sheet{k}");
        }
        self.wb.sheets.push(sheetcore::model::Sheet::new(&name));
        self.wb.dirty = true;
        self.sheet_idx = self.wb.sheets.len() - 1;
        self.status = format!("已新建 {name}");
    }

    fn delete_sheet(&mut self, idx: usize) {
        if self.wb.sheets.len() <= 1 {
            self.status = "至少保留一张工作表".into();
            return;
        }
        let name = self.wb.sheets[idx].name.clone();
        self.wb.sheets.remove(idx);
        self.wb.dirty = true;
        self.sheet_idx = self.sheet_idx.min(self.wb.sheets.len() - 1);
        self.status = format!("已删除 {name}");
    }

    fn move_sheet(&mut self, idx: usize, delta: i32) {
        let to = idx as i32 + delta;
        if to < 0 || to as usize >= self.wb.sheets.len() {
            return;
        }
        self.wb.sheets.swap(idx, to as usize);
        self.wb.dirty = true;
        self.sheet_idx = to as usize;
    }

    /// 撤销 / 重做
    fn do_undo(&mut self, redo: bool) {
        if self.editing.is_some() {
            return;
        }
        // 协作时：只回退仍然是我改成的样子的格子
        let mut mine: std::collections::HashSet<(usize, u32, u32)> = std::collections::HashSet::new();
        let site = self.net.as_ref().map(|n| n.site);
        if let Some(_site) = site {
            if let Some(peer) = self.peer.as_ref() {
                for (k, op) in peer.cells.iter() {
                    if op.site == site.unwrap() {
                        mine.insert(*k);
                    }
                }
            }
        }
        let guard = |sheet: usize, row: u32, col: u32, _cur: &sheetcore::model::Cell| {
            mine.is_empty() || mine.contains(&(sheet, row, col))
        };
        let applied = if redo {
            self.undo.redo(&mut self.wb.sheets, guard)
        } else {
            self.undo.undo(&mut self.wb.sheets, guard)
        };
        let Some(applied) = applied else {
            self.status = if redo { "没有可重做的操作".into() } else { "没有可撤销的操作".into() };
            return;
        };
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        // 重算受影响格子
        let mut keys: Vec<(usize, u32, u32)> = applied.touched.clone();
        keys.sort();
        keys.dedup();
        for k in keys.iter() {
            self.deps.update_cell(&self.wb, *k);
        }
        sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &keys);
        // 协作广播
        if let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref()) {
            for (sheet, row, col) in applied.touched.iter() {
                if *sheet != self.sheet_idx {
                    continue;
                }
                if let Some(cell) = self.wb.sheet(*sheet).get(*row, *col).cloned() {
                    let op = peer.local_set(*sheet, *row, *col, &cell);
                    net.broadcast_op(op);
                }
            }
        }
        // 回到出事的格
        let (sheet, row, col) = applied.focus;
        self.sheet_idx = sheet.min(self.wb.sheets.len().saturating_sub(1));
        self.sel = (row, col);
        self.sel_end = (row, col);
        self.ensure_visible();
        self.status = if redo {
            format!("重做：{}", applied.label)
        } else {
            format!("撤销：{}", applied.label)
        };
        if applied.skipped > 0 {
            self.status.push_str(&format!("（{} 格已被别人改动，跳过）", applied.skipped));
        }
    }

    fn commit_edit(&mut self, advance: Option<(i32, i32)>) {
        if let Some(ed) = self.editing.take() {
            self.release_lock();
            let (r, c) = self.sel;
            // 数据验证：不合规就不写入，编辑框留着让用户改（Excel 的「重试」）
            let mut rejected: Option<String> = None;
            {
                let sheet = self.wb.sheet(self.sheet_idx);
                if let Some(dv) = sheetcore::dv::find(&sheet.dv, r, c) {
                    let probe = if ed.buf.starts_with('=') {
                        Value::Text(ed.buf.as_str().into())
                    } else {
                        Value::parse_input(&ed.buf)
                    };
                    let mut calc = sheetcore::formula::Calc::new(&self.wb);
                    let ok = sheetcore::dv::validate(dv, &probe, sheet, r, c, |f, rr, cc| {
                        let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), rr, cc);
                        match sheetcore::formula::parse(&shifted) {
                            Ok(node) => matches!(
                                calc.eval_node(&node, self.sheet_idx),
                                sheetcore::formula::FValue::Bool(true)
                            ),
                            Err(_) => true,
                        }
                    });
                    if !ok {
                        rejected = Some(sheetcore::dv::error_text(dv));
                    }
                }
            }
            if let Some(msg) = rejected {
                self.status = msg;
                let mut again = ed.clone();
                again.buf = ed.buf.clone();
                self.editing = Some(again);
                return;
            }
            let cell = if ed.buf.starts_with('=') {
                Cell { value: Value::Text(ed.buf.as_str().into()), formula: Some(ed.buf.as_str().into()) }
            } else {
                Cell { value: Value::parse_input(&ed.buf), formula: None }
            };
            let cell_clone = cell.clone();
            let txn = self.txn_begin(&[(r, c)]);
            self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
            self.txn_end("编辑单元格", txn);
            self.wb.dirty = true;
            let key = (self.sheet_idx, r, c);
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.last_edit = Some(std::time::Instant::now());
            self.deps.update_cell(&self.wb, key);
            sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &[key]);
            // Ctrl+Shift+Enter：按选区把这条公式登记成数组公式（保存时写 t="array" ref）
            if self.cse_pending {
                self.cse_pending = false;
                let (r1, c1, r2, c2) = self.sel_rect();
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 1 && ed.buf.starts_with('=') {
                    let sheet = self.wb.sheet_mut(self.sheet_idx);
                    sheet.arrays.retain(|(a, _)| *a != sheetcore::model::CellRef::new(r1, c1));
                    sheet.arrays.push((
                        sheetcore::model::CellRef::new(r1, c1),
                        (r1, c1, r2, c2),
                    ));
                    let rect = (r1, c1, r2, c2);
                    self.status = format!(
                        "已按数组公式写入 {}{}:{}{}",
                        sheetcore::model::col_label(c1),
                        r1 + 1,
                        sheetcore::model::col_label(c2),
                        r2 + 1
                    );
                    let _ = rect;
                }
            }
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
        let (cols, mut rows) = {
            let s = self.wb.sheet(self.sheet_idx);
            (s.cols.clone(), s.rows.clone())
        };
        // 被筛选隐藏的行 = 行高 0（和 Excel 一样，不是删数据）
        {
            let s = self.wb.sheet(self.sheet_idx);
            for r in s.hidden_rows.iter() {
                rows.set(*r, 0.0);
            }
        }
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
                self.release_lock();
            }
            return;
        }
        let (redo_key, undo_key) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::Y),
                i.modifiers.command && !i.modifiers.shift && i.key_pressed(Key::Z),
            )
        });
        let (copy_k, cut_k, paste_k, paste_v_k, find_k, repl_k) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::C),
                i.modifiers.command && i.key_pressed(Key::X),
                i.modifiers.command && !i.modifiers.shift && i.key_pressed(Key::V),
                i.modifiers.command && i.modifiers.shift && i.key_pressed(Key::V),
                i.modifiers.command && i.key_pressed(Key::F),
                i.modifiers.command && i.key_pressed(Key::H),
            )
        });
        if copy_k {
            self.clip_copy(false, ctx);
            return;
        }
        if cut_k {
            self.clip_copy(true, ctx);
            return;
        }
        if paste_k || paste_v_k {
            let text = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Paste(t) => Some(t.clone()),
                    _ => None,
                })
            });
            // 系统剪贴板没有新粘贴事件时，用内部剪贴板
            self.clip_paste(paste_v_k, text);
            return;
        }
        if find_k || repl_k {
            self.find.open = true;
            if repl_k {
                self.find.replace.clear();
            }
            return;
        }
        if undo_key || redo_key {
            self.do_undo(redo_key);
            return;
        }
        let (mut dr, mut dc) = (0i64, 0i64);
        let (mut commit, mut start_edit, mut clear, mut cse) = (false, false, false, false);
        ctx.input(|i| {
            if i.key_pressed(Key::ArrowDown) { dr += 1 }
            if i.key_pressed(Key::ArrowUp) { dr -= 1 }
            if i.key_pressed(Key::ArrowRight) { dc += 1 }
            if i.key_pressed(Key::ArrowLeft) { dc -= 1 }
            if i.key_pressed(Key::Enter) {
                commit = true;
                dr = 1;
                cse = i.modifiers.command && i.modifiers.shift;
            }
            if i.key_pressed(Key::Tab) { commit = true; dc = 1 }
            if i.key_pressed(Key::F2) { start_edit = true }
            if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) { clear = true }
        });

        if commit && cse {
            self.cse_pending = true;
        }
        if clear {
            let (r, c) = self.sel;
            if let Some(holder) = self.lock_holder(r, c) {
                self.status = format!("{} 正在编辑 {}，请稍后", holder, cell_name(r, c));
                return;
            }
            let (r1, c1, r2, c2) = self.sel_rect();
            let keys = sheetcore::undo::rect_keys(r1, c1, r2, c2);
            let txn = self.txn_begin(&keys);
            for rr in r1..=r2 {
                for cc in c1..=c2 {
                    self.wb.sheet_mut(self.sheet_idx)
                        .set(rr, cc, Cell { value: Value::Empty, formula: None });
                }
            }
            self.txn_end("清除内容", txn);
            let (r, c) = self.sel;
            let _ = (r, c);
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
            self.begin_edit(None);
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
                self.begin_edit(Some(t));
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
        let keys = sheetcore::undo::rect_keys(r1, c1, r2, c2);
        let txn = self.txn_begin(&keys);
        let mut wire: Vec<(u32, u32, sheetcore::style::CellStyle)> = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                let old = self.wb.sheet(self.sheet_idx).style_of(r, c);
                let mut st = self.wb.styles.get(old).clone();
                f(&mut st, r, c);
                let id = self.wb.styles.intern(st.clone());
                self.wb.sheet_mut(self.sheet_idx).set_style(r, c, id);
                wire.push((r, c, st));
            }
        }
        if let Some(net) = self.net.as_ref() {
            net.broadcast_style(self.sheet_idx, wire);
        }
        self.txn_end("设置格式", txn);
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
        if let Some(net) = self.net.as_ref() {
            net.broadcast_merge(self.sheet_idx, (r1, c1, r2, c2), on);
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

            // 插入
            ui.menu_button("文件", |ui| {
                ui.set_min_width(150.0);
                if ui.button("打印预览").clicked() {
                    self.preview_page = 0;
                    self.print_preview = true;
                    ui.close_menu();
                }
                if ui.button("导出 PDF…").clicked() {
                    self.export_pdf_dialog();
                    ui.close_menu();
                }
            });
            ui.menu_button("数据", |ui| {
                ui.set_min_width(150.0);
                if ui.button("多级排序…").clicked() {
                    self.open_data_dlg(0);
                    ui.close_menu();
                }
                if ui.button("删除重复…").clicked() {
                    self.run_dedup();
                    ui.close_menu();
                }
                if ui.button("分列…").clicked() {
                    self.open_data_dlg(1);
                    ui.close_menu();
                }
            });
            ui.menu_button("插入", |ui| {
                ui.set_min_width(180.0);
                if ui.button("表格（把选区转成表格）").clicked() {
                    self.make_table();
                    ui.close_menu();
                }
                ui.separator();
                ui.set_min_width(150.0);
                let (r1, _c1, r2, _c2) = self.sel_rect();
                let (rr, cc) = self.sel;
                if ui.button(format!("在上方插入 {} 行", r2 - r1 + 1)).clicked() {
                    self.structural(sheetcore::structural::Structural::InsertRows { at: r1, n: r2 - r1 + 1 });
                    ui.close_menu();
                }
                if ui.button(format!("在下方插入 {} 行", r2 - r1 + 1)).clicked() {
                    self.structural(sheetcore::structural::Structural::InsertRows { at: r2 + 1, n: r2 - r1 + 1 });
                    ui.close_menu();
                }
                if ui
                    .add_enabled(r2 > r1 || r1 > 0, egui::Button::new(format!("删除 {} 行", r2 - r1 + 1)))
                    .clicked()
                {
                    self.structural(sheetcore::structural::Structural::DeleteRows { at: r1, n: r2 - r1 + 1 });
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("在左侧插入 1 列").clicked() {
                    self.structural(sheetcore::structural::Structural::InsertCols { at: cc, n: 1 });
                    ui.close_menu();
                }
                if ui.button("在右侧插入 1 列").clicked() {
                    self.structural(sheetcore::structural::Structural::InsertCols { at: cc + 1, n: 1 });
                    ui.close_menu();
                }
                if ui.button(format!("删除 {} 列", cc + 1)).clicked() {
                    self.structural(sheetcore::structural::Structural::DeleteCols { at: cc, n: 1 });
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("柱状图（按选区）").clicked() {
                    self.insert_chart_from_selection();
                    ui.close_menu();
                }
                ui.label(egui::RichText::new("首行=系列名，首列=分类").weak());
                ui.separator();
                if ui.button("图表管理…").clicked() {
                    self.chart_mgr = true;
                    self.reload_charts();
                    ui.close_menu();
                }
                if ui.button("数据透视表…").clicked() {
                    let (r, c) = self.sel;
                    let (r1, c1, r2, c2) =
                        sheetcore::sort_filter::current_region(self.wb.sheet(self.sheet_idx), r, c);
                    if c2 <= c1 || r2 <= r1 {
                        self.status = "当前区域太小，选一个带表头的表格".into();
                    } else {
                        self.pivot_edit = Some(sheetcore::pivot::PivotSpec {
                            source: (r1, c1, r2, c2),
                            row_field: c1,
                            col_field: None,
                            value_field: c2,
                            agg: sheetcore::pivot::Agg::Sum,
                        });
                    }
                    ui.close_menu();
                }
            });
            ui.separator();

            // 页面布局
            self.toolbar_page(ui);
            ui.separator();

            // 数据（排序筛选）
            self.toolbar_data(ui);
            ui.separator();

            // 视图（冻结窗格）
            self.toolbar_view(ui);
            ui.separator();

            // 审阅（批注）
            self.toolbar_notes(ui);
            ui.separator();

            // 数据验证
            self.toolbar_dv(ui);
            ui.separator();

            // 条件格式
            self.toolbar_cf(ui);
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


    /// 取（或新建）一个 dxf 索引；所有表保持同一份 dxfs，保证索引一致
    fn cf_dxf(&mut self, bold: bool, color: Option<[u8; 3]>, fill: Option<[u8; 3]>) -> usize {
        let want = sheetcore::cf::Dxf { bold, italic: false, color, fill };
        let mut idx = None;
        if let Some(s0) = self.wb.sheets.first() {
            if let Some(i) = s0.cf.dxfs.iter().position(|d| *d == want) {
                idx = Some(i);
            }
        }
        match idx {
            Some(i) => i,
            None => {
                let i = self.wb.sheets.first().map(|s| s.cf.dxfs.len()).unwrap_or(0);
                for sh in self.wb.sheets.iter_mut() {
                    sh.cf.dxfs.push(want.clone());
                }
                i
            }
        }
    }

    /// 给选区加一条条件格式规则
    fn add_cf(&mut self, kind: sheetcore::cf::CfKind) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let sheet = self.wb.sheet_mut(self.sheet_idx);
        sheet.cf.blocks.push(sheetcore::cf::CfBlock {
            ranges: vec![(r1, c1, r2, c2)],
            rules: vec![sheetcore::cf::CfRule { kind, stop: false }],
        });
        sheet.cf_dirty = true;
        self.wb.dirty = true;
        self.cf_stats_gen = u64::MAX;
        self.status = "已添加条件格式".into();
    }

    /// 清除与选区相交的条件格式
    fn clear_cf(&mut self) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let sheet = self.wb.sheet_mut(self.sheet_idx);
        let before = sheet.cf.blocks.len();
        sheet.cf.blocks.retain(|b| {
            !b.ranges
                .iter()
                .any(|(a, bb, cc, d)| *a <= r2 && *cc >= r1 && *bb <= c2 && *d >= c1)
        });
        if sheet.cf.blocks.len() != before {
            sheet.cf_dirty = true;
            self.wb.dirty = true;
            self.cf_stats_gen = u64::MAX;
            self.status = "已清除所选范围的条件格式".into();
        } else {
            self.status = "所选范围没有条件格式".into();
        }
    }

    /// 条件格式菜单
    fn toolbar_cf(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("条件格式", |ui| {
            ui.set_min_width(260.0);
            ui.menu_button("突出显示单元格规则", |ui| {
                ui.horizontal(|ui| {
                    ui.label("大于");
                    ui.add(egui::TextEdit::singleline(&mut self.cf_v1).desired_width(60.0));
                    if ui.button("应用").clicked() {
                        let v = self.cf_v1.clone();
                        let d = self.cf_dxf(false, Some([156, 0, 6]), Some([255, 199, 206]));
                        self.add_cf(sheetcore::cf::CfKind::CellIs {
                            op: sheetcore::cf::CfOp::Greater,
                            a: v,
                            b: String::new(),
                            dxf: d,
                        });
                        ui.close_menu();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("小于");
                    ui.add(egui::TextEdit::singleline(&mut self.cf_v1).desired_width(60.0));
                    if ui.button("应用").clicked() {
                        let v = self.cf_v1.clone();
                        let d = self.cf_dxf(false, Some([156, 0, 6]), Some([255, 199, 206]));
                        self.add_cf(sheetcore::cf::CfKind::CellIs {
                            op: sheetcore::cf::CfOp::Less,
                            a: v,
                            b: String::new(),
                            dxf: d,
                        });
                        ui.close_menu();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("等于");
                    ui.add(egui::TextEdit::singleline(&mut self.cf_v1).desired_width(60.0));
                    if ui.button("应用").clicked() {
                        let v = self.cf_v1.clone();
                        let d = self.cf_dxf(false, Some([156, 0, 6]), Some([255, 199, 206]));
                        self.add_cf(sheetcore::cf::CfKind::CellIs {
                            op: sheetcore::cf::CfOp::Equal,
                            a: v,
                            b: String::new(),
                            dxf: d,
                        });
                        ui.close_menu();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("介于");
                    ui.add(egui::TextEdit::singleline(&mut self.cf_v1).desired_width(50.0));
                    ui.label("与");
                    ui.add(egui::TextEdit::singleline(&mut self.cf_v2).desired_width(50.0));
                    if ui.button("应用").clicked() {
                        let (a, b) = (self.cf_v1.clone(), self.cf_v2.clone());
                        let d = self.cf_dxf(false, Some([156, 0, 6]), Some([255, 199, 206]));
                        self.add_cf(sheetcore::cf::CfKind::CellIs {
                            op: sheetcore::cf::CfOp::Between,
                            a,
                            b,
                            dxf: d,
                        });
                        ui.close_menu();
                    }
                });
            });
            ui.menu_button("项目选取规则", |ui| {
                if ui.button("前 10 项").clicked() {
                    let d = self.cf_dxf(false, Some([156, 0, 6]), Some([255, 199, 206]));
                    self.add_cf(sheetcore::cf::CfKind::Top { top: true, n: 10, percent: false, dxf: d });
                    ui.close_menu();
                }
                if ui.button("后 10 项").clicked() {
                    let d = self.cf_dxf(false, Some([156, 0, 6]), Some([255, 199, 206]));
                    self.add_cf(sheetcore::cf::CfKind::Top { top: false, n: 10, percent: false, dxf: d });
                    ui.close_menu();
                }
                if ui.button("高于平均值").clicked() {
                    let d = self.cf_dxf(false, Some([0, 97, 0]), Some([198, 239, 206]));
                    self.add_cf(sheetcore::cf::CfKind::Avg { above: true, dxf: d });
                    ui.close_menu();
                }
                if ui.button("低于平均值").clicked() {
                    let d = self.cf_dxf(false, Some([156, 0, 6]), Some([255, 199, 206]));
                    self.add_cf(sheetcore::cf::CfKind::Avg { above: false, dxf: d });
                    ui.close_menu();
                }
            });
            ui.menu_button("数据条", |ui| {
                for (label, rgb) in [
                    ("蓝色", [99u8, 142u8, 198u8]),
                    ("绿色", [99, 190, 123]),
                    ("红色", [248, 105, 107]),
                    ("橙色", [255, 166, 76]),
                ] {
                    if ui.button(label).clicked() {
                        self.add_cf(sheetcore::cf::CfKind::Bar { color: rgb });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("图标集", |ui| {
                for (label, set) in [
                    ("三色交通灯", "3TrafficLights1"),
                    ("三色箭头", "3Arrows"),
                    ("五色箭头", "5Arrows"),
                    ("四色等级", "4Rating"),
                    ("五色等级", "5Rating"),
                    ("三色旗帜", "3Flags"),
                    ("三色符号", "3Symbols"),
                    ("四分之一圆", "5Quarters"),
                ] {
                    if ui.button(label).clicked() {
                        let levels = set.chars().next().unwrap().to_digit(10).unwrap_or(3) as usize;
                        let mut th = vec![None];
                        for i in 1..levels {
                            th.push(Some((i as f64) * 100.0 / levels as f64));
                        }
                        self.add_cf(sheetcore::cf::CfKind::IconSet {
                            set: set.to_string(),
                            reverse: false,
                            thresholds: th,
                            show_value: true,
                        });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("色阶", |ui| {
                if ui.button("绿-黄-红").clicked() {
                    self.add_cf(sheetcore::cf::CfKind::Scale {
                        points: vec![
                            (None, [99, 190, 123]),
                            (Some(50.0), [255, 235, 132]),
                            (None, [248, 105, 107]),
                        ],
                    });
                    ui.close_menu();
                }
                if ui.button("红-黄-绿").clicked() {
                    self.add_cf(sheetcore::cf::CfKind::Scale {
                        points: vec![
                            (None, [248, 105, 107]),
                            (Some(50.0), [255, 235, 132]),
                            (None, [99, 190, 123]),
                        ],
                    });
                    ui.close_menu();
                }
                if ui.button("蓝-白-红").clicked() {
                    self.add_cf(sheetcore::cf::CfKind::Scale {
                        points: vec![
                            (None, [93, 135, 202]),
                            (Some(50.0), [255, 255, 255]),
                            (None, [248, 105, 107]),
                        ],
                    });
                    ui.close_menu();
                }
            });
            ui.separator();
            if ui.button("清除所选范围的条件格式").clicked() {
                self.clear_cf();
                ui.close_menu();
            }
            ui.separator();
            let n = self.wb.sheet(self.sheet_idx).cf.blocks.len();
            ui.label(format!("当前工作表有 {n} 条规则"));
        });
    }


    /// 数据验证菜单
    fn toolbar_dv(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("数据验证", |ui| {
            if ui.button("圈释无效数据").clicked() {
                self.circle_invalid = true;
                self.invalid_gen = u64::MAX;
                self.compute_invalid();
                self.status = format!("已圈出 {} 个不合规的格子", self.invalid_cells.len());
                ui.close_menu();
            }
            if ui.button("清除圈释").clicked() {
                self.circle_invalid = false;
                self.invalid_cells.clear();
                ui.close_menu();
            }
            ui.separator();
            ui.set_min_width(200.0);
            if ui.button("设置…").clicked() {
                let (r1, c1, r2, c2) = self.sel_rect();
                let cur = sheetcore::dv::find(&self.wb.sheet(self.sheet_idx).dv, r1, c1).cloned();
                let mut draft = cur.unwrap_or_default();
                draft.ranges = vec![(r1, c1, r2, c2)];
                self.dv_edit = Some(draft);
                ui.close_menu();
            }
            if ui.button("清除所选范围的验证").clicked() {
                let (r1, c1, r2, c2) = self.sel_rect();
                let sheet = self.wb.sheet_mut(self.sheet_idx);
                let before = sheet.dv.len();
                sheet.dv.retain(|d| {
                    !d.ranges
                        .iter()
                        .any(|(a, b, cc, d2)| *a <= r2 && *cc >= r1 && *b <= c2 && *d2 >= c1)
                });
                if sheet.dv.len() != before {
                    sheet.dv_dirty = true;
                    self.wb.dirty = true;
                    self.status = "已清除数据验证".into();
                }
                ui.close_menu();
            }
            ui.separator();
            let n = self.wb.sheet(self.sheet_idx).dv.len();
            ui.label(format!("当前工作表 {n} 条验证"));
        });
    }

    /// 数据验证对话框
    fn dv_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.dv_edit.take() else { return };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("数据验证")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("dv_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("允许");
                    egui::ComboBox::from_id_salt("dv_kind")
                        .selected_text(draft.kind.label())
                        .show_ui(ui, |ui| {
                            for k in [
                                sheetcore::dv::DvKind::Any,
                                sheetcore::dv::DvKind::Whole,
                                sheetcore::dv::DvKind::Decimal,
                                sheetcore::dv::DvKind::List,
                                sheetcore::dv::DvKind::Date,
                                sheetcore::dv::DvKind::Time,
                                sheetcore::dv::DvKind::TextLength,
                                sheetcore::dv::DvKind::Custom,
                            ] {
                                if ui.selectable_label(draft.kind == k, k.label()).clicked() {
                                    draft.kind = k;
                                }
                            }
                        });
                    ui.end_row();

                    if matches!(
                        draft.kind,
                        sheetcore::dv::DvKind::Whole
                            | sheetcore::dv::DvKind::Decimal
                            | sheetcore::dv::DvKind::Date
                            | sheetcore::dv::DvKind::Time
                            | sheetcore::dv::DvKind::TextLength
                    ) {
                        ui.label("数据");
                        egui::ComboBox::from_id_salt("dv_op")
                            .selected_text(draft.op.label())
                            .show_ui(ui, |ui| {
                                for op in [
                                    sheetcore::dv::DvOp::Between,
                                    sheetcore::dv::DvOp::NotBetween,
                                    sheetcore::dv::DvOp::Equal,
                                    sheetcore::dv::DvOp::NotEqual,
                                    sheetcore::dv::DvOp::Greater,
                                    sheetcore::dv::DvOp::Less,
                                    sheetcore::dv::DvOp::GreaterEq,
                                    sheetcore::dv::DvOp::LessEq,
                                ] {
                                    if ui.selectable_label(draft.op == op, op.label()).clicked() {
                                        draft.op = op;
                                    }
                                }
                            });
                        ui.end_row();
                        ui.label(if draft.op.needs_pair() { "最小值" } else { "值" });
                        ui.add(egui::TextEdit::singleline(&mut draft.f1).desired_width(160.0));
                        ui.end_row();
                        if draft.op.needs_pair() {
                            ui.label("最大值");
                            ui.add(egui::TextEdit::singleline(&mut draft.f2).desired_width(160.0));
                            ui.end_row();
                        }
                    } else if draft.kind == sheetcore::dv::DvKind::List {
                        ui.label("来源");
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.f1)
                                .hint_text("甲,乙,丙 或 $A$1:$A$5")
                                .desired_width(220.0),
                        );
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut draft.show_dropdown, "显示下拉箭头");
                        ui.end_row();
                    } else if draft.kind == sheetcore::dv::DvKind::Custom {
                        ui.label("公式");
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.f1)
                                .hint_text("例如 MOD(A1,2)=0")
                                .desired_width(220.0),
                        );
                        ui.end_row();
                    }

                    ui.label("忽略空值");
                    ui.checkbox(&mut draft.allow_blank, "");
                    ui.end_row();
                    ui.label("出错标题");
                    ui.add(egui::TextEdit::singleline(&mut draft.error_title).desired_width(200.0));
                    ui.end_row();
                    ui.label("出错说明");
                    ui.add(egui::TextEdit::singleline(&mut draft.error_msg).desired_width(200.0));
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("确定").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if apply {
            let (r1, c1, r2, c2) = (draft.ranges[0].0, draft.ranges[0].1, draft.ranges[0].2, draft.ranges[0].3);
            let sheet = self.wb.sheet_mut(self.sheet_idx);
            sheet.dv.retain(|d| {
                !d.ranges
                    .iter()
                    .any(|(a, b, cc, d2)| *a <= r2 && *cc >= r1 && *b <= c2 && *d2 >= c1)
            });
            if draft.kind != sheetcore::dv::DvKind::Any {
                draft.ranges = vec![(r1, c1, r2, c2)];
                sheet.dv.push(draft.clone());
            }
            sheet.dv_dirty = true;
            self.wb.dirty = true;
            self.status = "已设置数据验证".into();
        } else if !cancel && open {
            self.dv_edit = Some(draft);
        }
    }


    /// 审阅菜单：新建/编辑/删除批注
    fn toolbar_notes(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("审阅", |ui| {
            ui.set_min_width(180.0);
            if ui
                .add_enabled(self.wb.has_vba, egui::Button::new("导出宏部件…"))
                .on_hover_text("把 vbaProject.bin 另存出来")
                .clicked()
            {
                self.export_macro_part();
                ui.close_menu();
            }
            if ui
                .add_enabled(self.wb.has_vba, egui::Button::new("查看宏代码…"))
                .on_hover_text("只读展示 VBA 源码，不执行")
                .clicked()
            {
                self.open_macro_view();
                ui.close_menu();
            }
            ui.separator();
            let (r, c) = self.sel;
            let cur = self
                .wb
                .sheet(self.sheet_idx)
                .notes
                .get(&sheetcore::model::CellRef::new(r, c))
                .cloned();
            let label = if cur.is_some() { "编辑批注…" } else { "新建批注…" };
            if ui.button(label).clicked() {
                let text = cur.as_ref().map(|n| n.text.clone()).unwrap_or_default();
                self.note_edit = Some((r, c, text));
                ui.close_menu();
            }
            if ui
                .add_enabled(cur.is_some(), egui::Button::new("删除批注"))
                .clicked()
            {
                let sheet = self.wb.sheet_mut(self.sheet_idx);
                sheet.notes.remove(&sheetcore::model::CellRef::new(r, c));
                sheet.notes_dirty = true;
                self.wb.dirty = true;
                self.status = "已删除批注".into();
                ui.close_menu();
            }
            ui.separator();
            let n = self.wb.sheet(self.sheet_idx).notes.len();
            ui.label(format!("本表 {n} 条批注"));
        });
    }

    /// 批注编辑窗口
    fn note_dialog(&mut self, ctx: &egui::Context) {
        let Some((r, c, mut text)) = self.note_edit.take() else { return };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let name = format!("批注 {}{}", cell_name(r, c), "");
        egui::Window::new(name)
            .collapsible(false)
            .resizable(true)
            .default_width(280.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text("写点什么…"),
                );
                ui.horizontal(|ui| {
                    if ui.button("确定").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if apply {
            let sheet = self.wb.sheet_mut(self.sheet_idx);
            let key = sheetcore::model::CellRef::new(r, c);
            if text.trim().is_empty() {
                sheet.notes.remove(&key);
            } else {
                sheet.notes.insert(
                    key,
                    sheetcore::notes::Note {
                        r,
                        c,
                        text: text.clone(),
                        author: "Hermes".into(),
                    },
                );
            }
            sheet.notes_dirty = true;
            self.wb.dirty = true;
            self.status = "已保存批注".into();
        } else if !cancel && open {
            self.note_edit = Some((r, c, text));
        }
    }

    /// 画一个单元格：底色、条件格式、文字、边框、批注标记。
    /// 冻结窗格要复用它，所以单独拎出来。
    fn paint_cell(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        r: u32,
        c: u32,
        note: Option<&sheetcore::notes::Note>,
        ov: Option<&sheetcore::cf::Overlay>,
        dumping: bool,
        dump: &mut Vec<String>,
    ) {
        let sid = self.wb.sheet(self.sheet_idx).style_of(r, c);
        let st = self.wb.styles.get(sid);
        if self.sel == (r, c) && st.fill.is_none() {
            painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(26, 115, 232, 40));
        }
        if let Some(fill) = st.fill {
            painter.rect_filled(rect, 0.0, Color32::from_rgb(fill[0], fill[1], fill[2]));
            if dumping {
                dump.push(format!("fill ({},{}) rect=({:.0},{:.0},{:.0},{:.0}) rgb={:?}", r, c, rect.min.x, rect.min.y, rect.max.x, rect.max.y, fill));
            }
        }
        if let Some(ov) = ov {
            if let Some(f) = ov.fill {
                painter.rect_filled(rect, 0.0, Color32::from_rgb(f[0], f[1], f[2]));
                if dumping {
                    dump.push(format!("cf-fill ({},{}) rgb={:?}", r, c, f));
                }
            }
            if let Some((t, col)) = ov.bar {
                let w = (rect.width() * t).max(0.0);
                if w > 0.5 {
                    painter.rect_filled(
                        Rect::from_min_size(rect.min, Vec2::new(w, rect.height())),
                        0.0,
                        Color32::from_rgba_unmultiplied(col[0], col[1], col[2], 150),
                    );
                    if dumping {
                        dump.push(format!("cf-bar ({},{}) w={:.0} rgb={:?}", r, c, w, col));
                    }
                }
            }
        }
        if let Some(cell) = self.wb.sheet(self.sheet_idx).get(r, c) {
            let txt = sheetcore::style::format_value(&cell.value, &st.format);
            if !txt.is_empty() {
                let is_num = matches!(cell.value, Value::Number(_));
                let cf_bold = ov.map(|o| o.bold).unwrap_or(false);
                let base_color = ov.and_then(|o| o.color).unwrap_or(st.color);
                let font = FontId::proportional(if st.bold || cf_bold {
                    st.size + 0.6
                } else {
                    st.size
                });
                let color = Color32::from_rgb(base_color[0], base_color[1], base_color[2]);
                let align = if st.halign == 2 || (st.halign == 0 && !is_num) {
                    Align2::CENTER_CENTER
                } else if st.halign == 3 || (st.halign == 0 && is_num) {
                    Align2::RIGHT_CENTER
                } else {
                    Align2::LEFT_CENTER
                };
                let icon_shift = if ov.and_then(|o| o.icon.as_ref()).is_some() {
                    18.0
                } else {
                    0.0
                };
                let tx = match align {
                    Align2::CENTER_CENTER => rect.center().x + icon_shift / 2.0,
                    Align2::RIGHT_CENTER => rect.max.x - 4.0,
                    _ => rect.min.x + 4.0 + icon_shift,
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
                    dump.push(format!("text ({},{}) {:?} at=({:.0},{:.0}) rgb={:?} size={:.1} bold={}", r, c, txt, tx, ty, base_color, st.size, st.bold));
                }
            }
        }
        // 别人正在编辑：虚线框 + 角标
        if let Some(site) = self.net.as_ref().map(|n| n.site) {
            if self.locks.is_locked_by_other(self.sheet_idx, r, c, site) {
                let color = egui::Color32::from_rgb(230, 120, 30);
                let dash = egui::Stroke::new(1.5, color);
                painter.add(egui::Shape::dashed_line(
                    &[
                        rect.left_top(),
                        rect.right_top(),
                        rect.right_bottom(),
                        rect.left_bottom(),
                        rect.left_top(),
                    ],
                    dash,
                    5.0,
                    3.0,
                ));
                if dumping {
                    dump.push(format!(
                        "locked ({r},{c}) by={:?}",
                        self.locks.holder(self.sheet_idx, r, c).map(|h| h.name.clone())
                    ));
                }
            }
        }
        if let Some(ov) = ov {
            if let Some(mark) = &ov.icon {
                icons::draw_icon(painter, rect, mark);
                if dumping {
                    dump.push(format!("icon ({},{}) {:?} idx={}/{}", r, c, mark.style, mark.idx, mark.levels));
                }
            }
        }
        if let Some(n) = note {
            let (rx, ry) = (rect.right(), rect.top());
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(rx - 7.0, ry),
                    egui::pos2(rx, ry),
                    egui::pos2(rx, ry + 7.0),
                ],
                Color32::from_rgb(192, 0, 0),
                egui::Stroke::NONE,
            ));
            if dumping {
                dump.push(format!("note ({},{}) 作者={} 文字={:?}", r, c, n.author, n.text));
            }
        }
        let bd = &st.border;
        if bd[0].style != 0 || bd[1].style != 0 || bd[2].style != 0 || bd[3].style != 0 {
            let (l, t, rr, bb) = (rect.left(), rect.top(), rect.right(), rect.bottom());
            if dumping {
                dump.push(format!("border ({},{}) rect=({:.0},{:.0},{:.0},{:.0}) 左{:?} 右{:?} 上{:?} 下{:?}", r, c, l, t, rr, bb, bd[0].style, bd[1].style, bd[2].style, bd[3].style));
            }
            draw_edge(painter, egui::pos2(l, t), egui::pos2(l, bb), &bd[0]);
            draw_edge(painter, egui::pos2(rr, t), egui::pos2(rr, bb), &bd[1]);
            draw_edge(painter, egui::pos2(l, t), egui::pos2(rr, t), &bd[2]);
            draw_edge(painter, egui::pos2(l, bb), egui::pos2(rr, bb), &bd[3]);
        }
    }


    /// 视图菜单：冻结窗格
    fn toolbar_view(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("视图", |ui| {
            ui.set_min_width(190.0);
            let cur = self.wb.sheet(self.sheet_idx).freeze;
            if ui.button("冻结首行").clicked() {
                self.set_freeze(Some((1, 0)));
                ui.close_menu();
            }
            if ui.button("冻结首列").clicked() {
                self.set_freeze(Some((0, 1)));
                ui.close_menu();
            }
            let (r, c) = self.sel;
            if ui
                .button(format!("冻结到当前单元格（{}{}）", cell_name(r, c), ""))
                .clicked()
            {
                self.set_freeze(Some((r, c)));
                ui.close_menu();
            }
            ui.separator();
            if ui
                .add_enabled(cur.is_some(), egui::Button::new("取消冻结"))
                .clicked()
            {
                self.set_freeze(None);
                ui.close_menu();
            }
            ui.separator();
            ui.label(match cur {
                Some((r, c)) => format!("当前冻结 {r} 行 / {c} 列"),
                None => "未冻结".into(),
            });
        });
    }

    fn set_freeze(&mut self, v: Option<(u32, u32)>) {
        let sheet = self.wb.sheet_mut(self.sheet_idx);
        sheet.freeze = match v {
            Some((r, c)) if r == 0 && c == 0 => None,
            other => other,
        };
        sheet.freeze_dirty = true;
        self.wb.dirty = true;
        self.scroll = Vec2::ZERO;
        self.status = match v {
            Some((r, c)) if r > 0 || c > 0 => format!("已冻结 {r} 行 / {c} 列"),
            _ => "已取消冻结".to_string(),
        };
    }


    /// 数据菜单：排序与筛选
    fn toolbar_data(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("数据", |ui| {
            ui.set_min_width(200.0);
            let (r, c) = self.sel;
            if ui.button("升序排序").clicked() {
                self.do_sort(c, true);
                ui.close_menu();
            }
            if ui.button("降序排序").clicked() {
                self.do_sort(c, false);
                ui.close_menu();
            }
            ui.separator();
            if ui.button("筛选（当前区域）").clicked() {
                self.start_filter();
                ui.close_menu();
            }
            if ui
                .add_enabled(
                    self.wb.sheet(self.sheet_idx).filter.is_some(),
                    egui::Button::new("清除筛选"),
                )
                .clicked()
            {
                let sheet = self.wb.sheet_mut(self.sheet_idx);
                sheet.filter = None;
                sheet.hidden_rows.clear();
                sheet.filter_dirty = true;
                self.wb.dirty = true;
                self.status = "已清除筛选".into();
                ui.close_menu();
            }
            ui.separator();
            let n = match self.wb.sheet(self.sheet_idx).filter.as_ref() {
                Some(f) => {
                    let (a, b, c2, d) = f.range;
                    format!(
                        "筛选区域 {}-{}，隐藏 {} 行",
                        format!("{}{}", cell_name(a, b), ""),
                        format!("{}{}", cell_name(c2, d), ""),
                        self.wb.sheet(self.sheet_idx).hidden_rows.len()
                    )
                }
                None => "未启用筛选".into(),
            };
            ui.label(n);
        });
    }

    fn do_sort(&mut self, col: u32, asc: bool) {
        let (r, c) = self.sel;
        let (r1, c1, r2, c2) = sheetcore::sort_filter::current_region(self.wb.sheet(self.sheet_idx), r, c);
        let keys = sheetcore::undo::rect_keys(r1, c1, r2, c2);
        let txn = self.txn_begin(&keys);
        let res = sheetcore::sort_filter::sort_region(
            self.wb.sheet_mut(self.sheet_idx),
            (r1, c1, r2, c2),
            col,
            asc,
        );
        match res {
            Ok(()) => {
                self.txn_end("排序", txn);
                self.wb.dirty = true;
                self.doc_gen = self.doc_gen.wrapping_add(1);
                self.status = format!("已按 {} 列{}排序", cell_name(0, col), if asc { "升序" } else { "降序" });
            }
            Err(e) => self.status = e,
        }
    }

    fn start_filter(&mut self) {
        let (r, c) = self.sel;
        let (r1, c1, r2, c2) = sheetcore::sort_filter::current_region(self.wb.sheet(self.sheet_idx), r, c);
        if r2 <= r1 {
            self.status = "区域太小，无法筛选".into();
            return;
        }
        let sheet = self.wb.sheet_mut(self.sheet_idx);
        sheet.filter = Some(sheetcore::sort_filter::Filter::new(r1, c1, r2, c2));
        sheet.filter_dirty = true;
        self.wb.dirty = true;
        self.status = "已启用筛选：点表头的箭头选择要显示的值".into();
    }

    /// 表头筛选下拉
    fn filter_dialog(&mut self, ctx: &egui::Context) {
        let Some((col, mut items)) = self.filter_popup.take() else { return };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(format!("筛选 {} 列", cell_name(0, col)))
            .collapsible(false)
            .resizable(true)
            .default_width(220.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("全选").clicked() {
                        for it in items.iter_mut() {
                            it.1 = true;
                        }
                    }
                    if ui.small_button("全不选").clicked() {
                        for it in items.iter_mut() {
                            it.1 = false;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                    for it in items.iter_mut() {
                        ui.checkbox(&mut it.1, if it.0.is_empty() { "（空白）" } else { it.0.as_str() });
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("确定").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if apply {
            let allowed: std::collections::HashSet<String> =
                items.iter().filter(|(_, on)| *on).map(|(v, _)| v.clone()).collect();
            // 先改规则，再（不可变借用下）重算隐藏行，最后写回
            if let Some(f) = self.wb.sheet_mut(self.sheet_idx).filter.as_mut() {
                f.allows.insert(col, allowed.clone());
            }
            let hidden = {
                let sheet = self.wb.sheet(self.sheet_idx);
                sheet
                    .filter
                    .as_ref()
                    .map(|f| f.hidden_rows(sheet))
                    .unwrap_or_default()
            };
            {
                let sheet = self.wb.sheet_mut(self.sheet_idx);
                sheet.hidden_rows = hidden;
                sheet.filter_dirty = true;
            }
            self.wb.dirty = true;
            self.status = format!("{} 列已筛选", cell_name(0, col));
        } else if !cancel && open {
            self.filter_popup = Some((col, items));
        }
    }


    /// 页面布局菜单
    fn toolbar_page(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("页面布局", |ui| {
            ui.set_min_width(220.0);
            let ps0 = self.wb.sheet(self.sheet_idx).page.clone();
            ui.menu_button(format!("纸张：{}", ps0.paper_label()), |ui| {
                for (code, name) in [(9u16, "A4"), (8, "A3"), (10, "A5"), (1, "Letter"), (4, "Legal")] {
                    if ui.selectable_label(ps0.paper == code, name).clicked() {
                        self.set_page(|p| p.paper = code);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button(if ps0.landscape { "方向：横向" } else { "方向：纵向" }, |ui| {
                if ui.selectable_label(!ps0.landscape, "纵向").clicked() {
                    self.set_page(|p| p.landscape = false);
                    ui.close_menu();
                }
                if ui.selectable_label(ps0.landscape, "横向").clicked() {
                    self.set_page(|p| p.landscape = true);
                    ui.close_menu();
                }
            });
            ui.menu_button(format!("缩放：{}", if ps0.fit_to_page { "调整为 1 页宽".to_string() } else { format!("{}%", ps0.scale) }), |ui| {
                if ui.selectable_label(ps0.fit_to_page, "调整为 1 页宽").clicked() {
                    self.set_page(|p| {
                        p.fit_to_page = true;
                        p.fit_width = 1;
                        p.fit_height = 0;
                    });
                    ui.close_menu();
                }
                for pct in [100u16, 85, 75, 50] {
                    if ui.selectable_label(!ps0.fit_to_page && ps0.scale == pct, format!("{pct}%")).clicked() {
                        self.set_page(move |p| {
                            p.fit_to_page = false;
                            p.scale = pct;
                        });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("边距", |ui| {
                for (name, m) in [
                    ("常规", [0.7f32, 0.7, 0.75, 0.75, 0.3, 0.3]),
                    ("窄", [0.25, 0.25, 0.75, 0.75, 0.3, 0.3]),
                    ("宽", [1.0, 1.0, 1.0, 1.0, 0.5, 0.5]),
                ] {
                    if ui.button(name).clicked() {
                        self.set_page(move |p| p.margins = m);
                        ui.close_menu();
                    }
                }
            });
            ui.separator();
            if ui.checkbox(&mut { ps0.gridlines }, "打印网格线").clicked() {
                let v = !ps0.gridlines;
                self.set_page(move |p| p.gridlines = v);
                ui.close_menu();
            }
            if ui.checkbox(&mut { ps0.headings }, "打印行号列标").clicked() {
                let v = !ps0.headings;
                self.set_page(move |p| p.headings = v);
                ui.close_menu();
            }
            ui.separator();
            let (r1, c1, r2, c2) = self.sel_rect();
            if ui.button("把选区设为打印区域").clicked() {
                self.set_page(move |p| p.print_area = Some((r1, c1, r2, c2)));
                ui.close_menu();
            }
            if ui
                .add_enabled(ps0.print_area.is_some(), egui::Button::new("取消打印区域"))
                .clicked()
            {
                self.set_page(|p| p.print_area = None);
                ui.close_menu();
            }
            let (sr, _sc) = self.sel;
            if ui.button(format!("把第 {} 行设为顶端标题行", sr + 1)).clicked() {
                self.set_page(move |p| p.title_rows = Some((0, sr)));
                ui.close_menu();
            }
            if ui
                .add_enabled(ps0.title_rows.is_some(), egui::Button::new("取消标题行"))
                .clicked()
            {
                self.set_page(|p| p.title_rows = None);
                ui.close_menu();
            }
            ui.separator();
            if ui.checkbox(&mut self.show_pages, "显示分页虚线").clicked() {
                ui.close_menu();
            }
            ui.separator();
            ui.separator();
            if ui.button("导出 PDF…").clicked() {
                self.export_pdf();
                ui.close_menu();
            }
            if ui.button("打印预览").clicked() {
                self.show_pages = true;
                self.status = "分页虚线已显示（按当前页面设置）".into();
                ui.close_menu();
            }
            if ui.button("并排比较…").clicked() {
                self.open_compare();
                ui.close_menu();
            }
            if ui
                .add_enabled(self.compare.is_some(), egui::Button::new("关闭并排比较"))
                .clicked()
            {
                self.compare = None;
                self.compare_window = false;
                self.status = "已关闭并排比较".into();
                ui.close_menu();
            }
            if self.compare.is_some() {
                ui.checkbox(&mut self.compare_sync, "同步纵向滚动");
                if ui
                    .checkbox(&mut self.compare_window, "在独立窗口中显示")
                    .changed()
                {
                    ui.close_menu();
                }
            }
            ui.label(format!(
                "页数约 {}×{}",
                sheetcore::page::page_breaks(self.wb.sheet(self.sheet_idx)).0.len() + 1,
                sheetcore::page::page_breaks(self.wb.sheet(self.sheet_idx)).1.len() + 1
            ));
        });
    }

    /// 用选区插入一张柱状图（首行标题、首列分类），写进文件后重新读图表
    fn insert_chart_from_selection(&mut self) {
        let (r1, c1, r2, c2) = self.sel_rect();
        if r2 <= r1 || c2 <= c1 {
            self.status = "先选中一块数据（首行为系列名，首列为分类）".into();
            return;
        }
        if self.wb.path.is_none() {
            self.status = "请先保存文件再插入图表".into();
            return;
        }
        let name = self.wb.sheet(self.sheet_idx).name.clone();
        let ch = sheetcore::chart_write::chart_from_selection(
            self.wb.sheet(self.sheet_idx),
            &name,
            (r1, c1, r2, c2),
            "图表",
            sheetcore::charts::ChartKind::Column,
        );
        self.chart_edit = Some(ch);
    }

    /// 真的写文件
    fn commit_chart(&mut self, ch: sheetcore::charts::Chart) {
        let Some(path) = self.wb.path.clone() else {
            self.status = "请先保存文件再插入图表".into();
            return;
        };
        let name = self.wb.sheet(self.sheet_idx).name.clone();
        if let Err(e) = sheetcore::xlsx::save(&path, &self.wb) {
            self.status = format!("保存失败：{e}");
            return;
        }
        match sheetcore::chart_write::insert_chart(&path, &name, &ch) {
            Ok(()) => {
                self.reload_charts();
                self.status = format!("已插入{}（{} 个系列）", ch.kind.label(), ch.series.len());
            }
            Err(e) => self.status = format!("插入图表失败：{e}"),
        }
    }

    fn reload_charts(&mut self) {
        if let Some(path) = self.wb.path.clone() {
            if let Ok(all) = sheetcore::charts::load_charts_all(&path, &self.wb.sheets) {
                for (idx, sh) in self.wb.sheets.iter_mut().enumerate() {
                    sh.charts = all.get(&(idx as u32)).cloned().unwrap_or_default();
                }
            }
        }
    }


    /// 把比较工作簿临时换进状态里画一帧，再换回来
    fn draw_compare_pane(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(mut cmp) = self.compare.take() else { return };
        let (cmp_wb, cmp_sheet, cmp_scroll) = (std::mem::take(&mut cmp.0), cmp.1, cmp.2);
        let saved_wb = std::mem::replace(&mut self.wb, cmp_wb);
        let saved_sheet = self.sheet_idx;
        let saved_scroll = self.scroll;
        let saved_sel = self.sel;
        let saved_sel_end = self.sel_end;
        self.sheet_idx = cmp_sheet;
        self.scroll = cmp_scroll;
        self.sel = (0, 0);
        self.sel_end = (0, 0);
        if self.compare_sync {
            self.scroll.y = saved_scroll.y;
        }
        self.draw_grid_pane(ui, ctx, true);
        let used_scroll = self.scroll;
        cmp.0 = std::mem::replace(&mut self.wb, saved_wb);
        cmp.2 = used_scroll;
        self.sheet_idx = saved_sheet;
        self.scroll = saved_scroll;
        self.sel = saved_sel;
        self.sel_end = saved_sel_end;
        self.cf_stats_gen = u64::MAX;
        self.compare = Some(cmp);
    }

    /// 独立窗口里的比较视图
    fn show_compare_window(&mut self, ctx: &egui::Context) {
        if self.compare.is_none() || !self.compare_window {
            return;
        }
        let title = format!(
            "比较 - {}",
            self.compare.as_ref().map(|c| c.0.title()).unwrap_or_default()
        );
        let vp = egui::ViewportId::from_hash_of("sheet_compare");
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([420.0, 300.0]);
        let mut closed = false;
        if std::env::var("SC_DUMP").is_ok() {
            eprintln!("[win] 打开独立比较窗口");
        }
        ctx.show_viewport_immediate(vp, builder, |ctx, _class| {
            egui::TopBottomPanel::top("cmp_bar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("只读视图").small(),
                    );
                    if self.compare.as_ref().map(|c| c.0.sheets.len() > 1).unwrap_or(false) {
                        let mut pick = self.compare.as_ref().map(|c| c.1).unwrap_or(0);
                        let names: Vec<String> = self
                            .compare
                            .as_ref()
                            .map(|c| c.0.sheets.iter().map(|s| s.name.clone()).collect())
                            .unwrap_or_default();
                        egui::ComboBox::from_id_salt("cmp_sheet")
                            .selected_text(names.get(pick).cloned().unwrap_or_default())
                            .show_ui(ui, |ui| {
                                for (i, n) in names.iter().enumerate() {
                                    if ui.selectable_label(pick == i, n).clicked() {
                                        pick = i;
                                    }
                                }
                            });
                        if let Some(c) = self.compare.as_mut() {
                            c.1 = pick;
                        }
                    }
                    let mut sync = self.compare_sync;
                    if ui.checkbox(&mut sync, "同步纵向滚动").changed() {
                        self.compare_sync = sync;
                    }
                });
            });
            egui::CentralPanel::default().show(ctx, |ui| {
                self.draw_compare_pane(ui, ctx);
            });
            if ctx.input(|i| i.viewport().close_requested()) {
                closed = true;
            }
        });
        if closed {
            self.compare_window = false;
            self.compare = None;
            self.status = "已关闭比较窗口".into();
        }
    }

    /// 选一个文件放到右半边
    fn open_compare(&mut self) {
        let picked = rfd::FileDialog::new()
            .add_filter("Excel 文件", &["xlsx", "xlsm", "xls"])
            .set_title("选择要并排比较的文件")
            .pick_file();
        let Some(path) = picked else { return };
        match sheetcore::xlsx::load(&path) {
            Ok(wb) => {
                let n = wb.sheets.len();
                self.compare = Some((wb, 0, Vec2::ZERO));
                self.status = format!("并排比较：{}（{n} 张表，只读）", path.display());
            }
            Err(e) => self.status = format!("打开失败：{e}"),
        }
    }

    fn set_page<F: Fn(&mut sheetcore::page::PageSetup)>(&mut self, f: F) {
        let sheet = self.wb.sheet_mut(self.sheet_idx);
        f(&mut sheet.page);
        sheet.page_dirty = true;
        self.wb.dirty = true;
    }



    /// 插入/删除行列：结构 + 撤销 + 协作广播
    fn structural(&mut self, op: sheetcore::structural::Structural) {
        let idx = self.sheet_idx;
        let cell_count = self.wb.sheet(idx).len();
        // 表太大就不记快照（记了也没内存），明确告诉用户不能撤销
        let snapshot = if cell_count <= 200_000 {
            Some(sheetcore::undo::snapshot_sheet(self.wb.sheet(idx), idx))
        } else {
            None
        };
        sheetcore::structural::apply_structural_wb(&mut self.wb, idx, &op);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        let after = if snapshot.is_some() {
            Some(sheetcore::undo::snapshot_sheet(self.wb.sheet(idx), idx))
        } else {
            None
        };
        let label = op.label();
        if let (Some(before), Some(after)) = (snapshot, after) {
            self.undo.push(sheetcore::undo::Action {
                label: label.clone(),
                deltas: Vec::new(),
                focus: (idx, self.sel.0, self.sel.1),
                snap_before: Some(before),
                snap_after: Some(after),
                by_site: 0,
            });
        }
        // 整表重算（结构变了，依赖图也得重建）
        sheetcore::formula::recalc_all(&mut self.wb);
        self.deps = sheetcore::formula::deps::Deps::build(&self.wb);
        if let Some(net) = self.net.as_ref() {
            net.broadcast_structural(idx, &op);
        }
        self.status = if self.undo.can_undo() {
            format!("{label}（可 Ctrl+Z 撤销）")
        } else {
            format!("{label}（表太大，本次不可撤销）")
        };
    }

    /// 读宏代码，开窗口
    fn open_macro_view(&mut self) {
        let Some(path) = self.wb.path.clone() else {
            self.status = "先保存文件".into();
            return;
        };
        let Some(bytes) = sheetcore::vba::read_vba(&path) else {
            self.status = "这个文件里没有 VBA 宏".into();
            return;
        };
        match sheetcore::vba_src::read_modules(&bytes) {
            Ok(mods) => {
                self.status = format!("读到 {} 个宏模块（只读）", mods.len());
                self.macro_win = Some((mods, 0));
            }
            Err(e) => self.status = format!("解析宏失败：{e:#}"),
        }
    }

    /// 工作表重命名
    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((idx, mut name)) = self.rename_sheet.take() else { return };
        let mut open = true;
        let mut ok = false;
        egui::Window::new("重命名工作表")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("名称");
                    let r = ui.text_edit_singleline(&mut name);
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        ok = true;
                    }
                });
                if ui.button("确定").clicked() {
                    ok = true;
                }
            });
        if ok && !name.trim().is_empty() && idx < self.wb.sheets.len() {
            self.wb.sheets[idx].name = name.trim().to_string();
            self.wb.dirty = true;
            self.status = format!("已重命名为 {}", name.trim());
        } else if open {
            self.rename_sheet = Some((idx, name));
        }
    }

    /// 查找/替换窗口
    fn find_dialog(&mut self, ctx: &egui::Context) {
        if !self.find.open {
            return;
        }
        let mut open = true;
        let mut do_find = false;
        let mut do_prev = false;
        let mut do_repl = false;
        let mut do_all = false;
        egui::Window::new("查找与替换")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .show(ctx, |ui| {
                egui::Grid::new("find_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("查找");
                    let r = ui.text_edit_singleline(&mut self.find.find);
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        do_find = true;
                    }
                    ui.end_row();
                    ui.label("替换为");
                    ui.text_edit_singleline(&mut self.find.replace);
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.find.match_case, "区分大小写");
                    ui.checkbox(&mut self.find.whole_cell, "整格匹配");
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("查找下一个").clicked() {
                        do_find = true;
                    }
                    if ui.button("上一个").clicked() {
                        do_prev = true;
                    }
                    if ui.button("替换").clicked() {
                        do_repl = true;
                    }
                    if ui.button("全部替换").clicked() {
                        do_all = true;
                    }
                });
                ui.label(egui::RichText::new(format!("已命中 {} 处", self.find.hits)).weak());
            });
        if do_find {
            self.find_next(false);
        }
        if do_prev {
            self.find_next(true);
        }
        if do_repl {
            self.replace_current();
        }
        if do_all {
            self.replace_all();
        }
        self.find.open = open;
    }

    fn macro_window(&mut self, ctx: &egui::Context) {
        let Some((mods, mut sel)) = self.macro_win.take() else { return };
        let mut open = true;
        egui::Window::new("宏代码（只读，不执行）")
            .open(&mut open)
            .default_size([720.0, 480.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (i, m) in mods.iter().enumerate() {
                        if ui.selectable_label(sel == i, format!("{}（{} 行）", m.name, m.lines)).clicked() {
                            sel = i;
                        }
                    }
                });
                ui.separator();
                if let Some(m) = mods.get(sel) {
                    ui.label(
                        egui::RichText::new(format!("{} · {}", m.name, m.kind))
                            .strong()
                            .size(12.0),
                    );
                    egui::ScrollArea::both().auto_shrink([false, false]).max_height(360.0).show(
                        ui,
                        |ui: &mut egui::Ui| {
                            let mut text = m.source.clone();
                            ui.add(
                                egui::TextEdit::multiline(&mut text)
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        },
                    );
                    if ui.button("复制当前模块").clicked() {
                        ctx.copy_text(m.source.clone());
                        self.status = format!("已复制 {}", m.name);
                    }
                } else {
                    ui.label("没有可显示的模块");
                }
            });
        if open {
            self.macro_win = Some((mods, sel));
        }
    }

    /// 导出当前工作表为 PDF
    fn export_pdf(&mut self) {
        let Some(out) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{}.pdf", self.wb.sheet(self.sheet_idx).name))
            .save_file()
        else {
            return;
        };
        let font = std::path::Path::new("C:/Windows/Fonts/simhei.ttf");
        if !font.exists() {
            self.status = "找不到 C:/Windows/Fonts/simhei.ttf，无法导出 PDF".into();
            return;
        }
        match sheetcore::print::export_pdf(&self.wb, self.sheet_idx, &out, font) {
            Ok(n) => self.status = format!("已导出 {n} 页到 {}", out.display()),
            Err(e) => self.status = format!("导出失败：{e:#}"),
        }
    }

    /// 把 vbaProject.bin 导出来
    fn export_macro_part(&mut self) {
        let Some(src) = self.wb.path.clone() else {
            self.status = "先保存文件".into();
            return;
        };
        let Some(bytes) = sheetcore::vba::read_vba(&src) else {
            self.status = "这个文件里没有 VBA 宏".into();
            return;
        };
        let Some(out) = rfd::FileDialog::new()
            .set_file_name("vbaProject.bin")
            .add_filter("VBA 工程", &["bin"])
            .save_file()
        else {
            return;
        };
        match std::fs::write(&out, &bytes) {
            Ok(()) => {
                self.status = format!("已导出 {} 字节到 {}", bytes.len(), out.display());
            }
            Err(e) => self.status = format!("导出失败：{e}"),
        }
    }

    /// 新建/编辑图表的对话框
    fn chart_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut ch) = self.chart_edit.take() else { return };
        let mut open = true;
        let mut ok = false;
        let mut cancel = false;
        egui::Window::new("插入图表")
            .collapsible(false)
            .resizable(false)
            .default_width(280.0)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("chart_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("图表类型");
                    egui::ComboBox::from_id_salt("chart_kind")
                        .selected_text(ch.kind.label())
                        .show_ui(ui, |ui| {
                            for k in [
                                sheetcore::charts::ChartKind::Column,
                                sheetcore::charts::ChartKind::Bar,
                                sheetcore::charts::ChartKind::BarStacked,
                                sheetcore::charts::ChartKind::Line,
                                sheetcore::charts::ChartKind::Area,
                                sheetcore::charts::ChartKind::Pie,
                                sheetcore::charts::ChartKind::Doughnut,
                                sheetcore::charts::ChartKind::Scatter,
                            ] {
                                if ui.selectable_label(ch.kind == k, k.label()).clicked() {
                                    ch.kind = k;
                                }
                            }
                        });
                    ui.end_row();
                    ui.label("标题");
                    ui.text_edit_singleline(&mut ch.title);
                    ui.end_row();
                    ui.label("数据标签");
                    ui.checkbox(&mut ch.show_labels, "显示数值");
                    ui.end_row();

                    ui.label("图例");
                    egui::ComboBox::from_id_salt("legend_pos")
                        .selected_text(match ch.legend_pos.as_str() {
                            "t" => "顶部",
                            "b" => "底部",
                            "l" => "左侧",
                            "r" => "右侧",
                            _ => "无",
                        })
                        .show_ui(ui, |ui| {
                            for (k, label) in [("r", "右侧"), ("t", "顶部"), ("b", "底部"), ("l", "左侧")] {
                                if ui.selectable_label(ch.legend_pos == k, label).clicked() {
                                    ch.legend_pos = k.to_string();
                                    ch.legend = true;
                                }
                            }
                        });
                    ui.end_row();

                    ui.label("系列");
                    ui.label(format!("{} 个", ch.series.len()));
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("插入").clicked() {
                        ok = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if ok {
            self.commit_chart(ch);
        } else if !cancel && open {
            self.chart_edit = Some(ch);
        }
    }

    /// 图表管理：改类型、删除
    fn chart_manager(&mut self, ctx: &egui::Context) {
        if !self.chart_mgr {
            return;
        }
        let mut open = true;
        let mut del: Option<usize> = None;
        let mut retype: Option<(usize, sheetcore::charts::ChartKind)> = None;
        let charts = self.wb.sheet(self.sheet_idx).charts.clone();
        egui::Window::new("图表管理")
            .collapsible(false)
            .default_width(340.0)
            .open(&mut open)
            .show(ctx, |ui| {
                if charts.is_empty() {
                    ui.label("这张表还没有图表");
                }
                for (i, ch) in charts.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{}. {}（{} 系列，锚点 {}-{}）",
                            i + 1,
                            if ch.title.is_empty() { "图表" } else { &ch.title },
                            ch.series.len(),
                            cell_name(ch.anchor.0, ch.anchor.1),
                            cell_name(ch.anchor.2, ch.anchor.3)
                        ));
                        egui::ComboBox::from_id_salt(format!("ret{i}"))
                            .selected_text(ch.kind.label())
                            .width(96.0)
                            .show_ui(ui, |ui| {
                                for k in [
                                    sheetcore::charts::ChartKind::Column,
                                    sheetcore::charts::ChartKind::Bar,
                                    sheetcore::charts::ChartKind::BarStacked,
                                    sheetcore::charts::ChartKind::Line,
                                    sheetcore::charts::ChartKind::Area,
                                    sheetcore::charts::ChartKind::Pie,
                                    sheetcore::charts::ChartKind::Doughnut,
                                    sheetcore::charts::ChartKind::Scatter,
                                ] {
                                    if ui.selectable_label(ch.kind == k, k.label()).clicked() {
                                        retype = Some((i, k));
                                    }
                                }
                            });
                        if ui.button("删除").clicked() {
                            del = Some(i);
                        }
                    });
                }
            });
        if let Some((i, k)) = retype {
            if let Some(ch) = charts.get(i) {
                let mut nc = ch.clone();
                nc.kind = k;
                if let Some(path) = self.wb.path.clone() {
                    match sheetcore::chart_write::replace_chart(&path, &ch.part, &nc) {
                        Ok(()) => {
                            self.reload_charts();
                            self.status = format!("已改成{}", k.label());
                        }
                        Err(e) => self.status = format!("改类型失败：{e}"),
                    }
                }
            }
        }
        if let Some(i) = del {
            if let Some(ch) = charts.get(i) {
                if let Some(path) = self.wb.path.clone() {
                    match sheetcore::chart_write::delete_chart(&path, &ch.drawing, &ch.part) {
                        Ok(()) => {
                            self.reload_charts();
                            self.status = "已删除图表".into();
                        }
                        Err(e) => self.status = format!("删除失败：{e}"),
                    }
                }
            }
        }
        if !open {
            self.chart_mgr = false;
        }
    }

    /// 数据透视表对话框
    fn pivot_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut spec) = self.pivot_edit.take() else { return };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        // 源区域表头
        let (sr1, sc1, _, sc2) = spec.source;
        let fields: Vec<(u32, String)> = (sc1..=sc2)
            .map(|c| {
                (
                    c,
                    self.wb
                        .sheet(self.sheet_idx)
                        .get(sr1, c)
                        .map(|x| x.value.display())
                        .unwrap_or_else(|| cell_name(0, c)),
                )
            })
            .collect();
        let fname = |c: u32| {
            fields
                .iter()
                .find(|(cc, _)| *cc == c)
                .map(|(_, n)| n.clone())
                .unwrap_or_default()
        };
        egui::Window::new("数据透视表")
            .collapsible(false)
            .resizable(false)
            .default_width(300.0)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("pivot_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("源区域");
                    ui.label(format!(
                        "{}-{}（{} 行）",
                        cell_name(spec.source.0, spec.source.1),
                        cell_name(spec.source.2, spec.source.3),
                        spec.source.2 - spec.source.0
                    ));
                    ui.end_row();

                    ui.label("行字段");
                    egui::ComboBox::from_id_salt("pivot_row")
                        .selected_text(fname(spec.row_field))
                        .show_ui(ui, |ui| {
                            for (c, n) in fields.iter() {
                                if ui.selectable_label(spec.row_field == *c, n).clicked() {
                                    spec.row_field = *c;
                                }
                            }
                        });
                    ui.end_row();

                    ui.label("列字段");
                    egui::ComboBox::from_id_salt("pivot_col")
                        .selected_text(match spec.col_field {
                            Some(c) => fname(c),
                            None => "（无）".into(),
                        })
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(spec.col_field.is_none(), "（无）").clicked() {
                                spec.col_field = None;
                            }
                            for (c, n) in fields.iter() {
                                if ui.selectable_label(spec.col_field == Some(*c), n).clicked() {
                                    spec.col_field = Some(*c);
                                }
                            }
                        });
                    ui.end_row();

                    ui.label("值字段");
                    egui::ComboBox::from_id_salt("pivot_val")
                        .selected_text(fname(spec.value_field))
                        .show_ui(ui, |ui| {
                            for (c, n) in fields.iter() {
                                if ui.selectable_label(spec.value_field == *c, n).clicked() {
                                    spec.value_field = *c;
                                }
                            }
                        });
                    ui.end_row();

                    ui.label("汇总方式");
                    egui::ComboBox::from_id_salt("pivot_agg")
                        .selected_text(spec.agg.label())
                        .show_ui(ui, |ui| {
                            for a in [
                                sheetcore::pivot::Agg::Sum,
                                sheetcore::pivot::Agg::Count,
                                sheetcore::pivot::Agg::Avg,
                                sheetcore::pivot::Agg::Max,
                                sheetcore::pivot::Agg::Min,
                            ] {
                                if ui.selectable_label(spec.agg == a, a.label()).clicked() {
                                    spec.agg = a;
                                }
                            }
                        });
                    ui.end_row();

                    ui.checkbox(&mut self.pivot_real, "生成可刷新的透视表");
                    ui.end_row();
                    ui.label("");
                    ui.label(
                        egui::RichText::new("关掉则只粘贴数值；打开后 Excel 里能拖字段")
                            .weak()
                            .size(10.0),
                    );
                    ui.end_row();

                    ui.label("输出到");
                    let (r, c) = self.sel;
                    ui.label(format!("{}（当前选中格）", cell_name(r, c)));
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("生成").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if apply {
            let res = sheetcore::pivot::compute(self.wb.sheet(self.sheet_idx), &spec);
            let at = self.sel;
            let rect = sheetcore::pivot::write_to(self.wb.sheet_mut(self.sheet_idx), at, &res);
            if self.pivot_real {
                let vname = self
                    .wb
                    .sheet(self.sheet_idx)
                    .get(spec.source.0, spec.value_field)
                    .map(|c| c.value.display())
                    .unwrap_or_default();
                self.wb.pivots_pending.push((self.sheet_idx, spec.clone(), res.clone(), rect, vname));
            }
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.status = format!(
                "透视表已生成：{} 行 × {} 列（{}）",
                rect.2 - rect.0 + 1,
                rect.3 - rect.1 + 1,
                spec.agg.label()
            );
        } else if !cancel && open {
            self.pivot_edit = Some(spec);
        }
    }


    /// 主入口：没有比较对象时整块画；有就左右各一半
    fn draw_grid(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.compare.is_none() || self.compare_window {
            self.draw_grid_pane(ui, ctx, false);
            self.flush_dump();
            return;
        }
        let avail = ui.available_size();
        let half = (avail.x - 8.0) / 2.0;
        ui.horizontal_top(|ui| {
            ui.allocate_ui(Vec2::new(half, avail.y), |ui| {
                self.draw_grid_pane(ui, ctx, false);
            });
            ui.separator();
            ui.allocate_ui(Vec2::new(half, avail.y), |ui| {
                self.draw_compare_pane(ui, ctx);
            });
        });
        self.flush_dump();
    }

    /// 把这一帧两个面板的绘制记录写出去（只写一次）
    fn flush_dump(&mut self) {
        if self.dump_accum.is_empty() {
            return;
        }
        // 可选：等到第 N 秒再落盘（验证「运行一段时间后」的状态）
        if let Ok(v) = std::env::var("SC_DUMP_AT") {
            if let Ok(secs) = v.parse::<u64>() {
                if self.t0.elapsed().as_secs() < secs {
                    // 还没到时候：丢掉这一帧的记录，否则文件里会混进历史帧
                    self.dump_accum.clear();
                    return;
                }
            }
        }
        if let Ok(path) = std::env::var("SC_DUMP") {
            eprintln!(
                "[dbg] t={}s 写 dump: (0,0)={:?} (2,0)={:?} 格数={}",
                self.t0.elapsed().as_secs(),
                self.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
                self.wb.sheet(0).get(2, 0).map(|c| c.value.display()),
                self.wb.sheet(0).len()
            );
            let _ = std::fs::write(path, &self.dump_accum);
            self.dump_once = false;
            self.dump_accum.clear();
        }
    }

    fn draw_grid_pane(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, readonly: bool) {
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
        let (cols, mut rows) = {
            let s = self.wb.sheet(self.sheet_idx);
            (s.cols.clone(), s.rows.clone())
        };
        // 被筛选隐藏的行 = 行高 0（和 Excel 一样，不是删数据）
        {
            let s = self.wb.sheet(self.sheet_idx);
            for r in s.hidden_rows.iter() {
                rows.set(*r, 0.0);
            }
        }
        let max_x = (cols.pos(16_384) - avail.x).max(0.0);
        let max_y = (rows.pos(1_048_576) - avail.y).max(0.0);
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);

        let (off_x, off_y) = (self.scroll.x, self.scroll.y);
        // 冻结窗格：滚动区域整体平移出冻结条
        let (fy, fx) = self.wb.sheet(self.sheet_idx).freeze.unwrap_or((0, 0));
        let (fy, fx) = (fy.min(200), fx.min(50));
        let frozen_w = cols.pos(fx);
        let frozen_h = rows.pos(fy);
        let body_x = origin.x + HEAD_W + frozen_w;
        let body_y = origin.y + HEAD_H + frozen_h;
        let first_col = cols.index_at(off_x).max(fx);
        let first_row = rows.index_at(off_y).max(fy);
        if std::env::var("SC_DBG_GRID").is_ok() {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < 3 || n % 120 == 0 {
                eprintln!("[grid] f={n} avail={:?} scroll={:?} cols.custom={} rows.custom={} first=({},{})",
                    avail, (off_x, off_y), cols.len(), rows.len(), first_col, first_row);
            }
        }
        let gx = |c: u32| body_x + cols.pos(c) - off_x;
        let gy = |r: u32| body_y + rows.pos(r) - off_y;
        // 冻结区里的格子用固定坐标
        let px_of = |c: u32| {
            if c < fx {
                origin.x + HEAD_W + cols.pos(c)
            } else {
                gx(c)
            }
        };
        let py_of = |r: u32| {
            if r < fy {
                origin.y + HEAD_H + rows.pos(r)
            } else {
                gy(r)
            }
        };

        let mut dump: Vec<String> = Vec::new();
        let dumping = std::env::var("SC_DUMP").is_ok()
            && self.dump_once
            && self.open_rx.is_none()
            && self.wb.sheets.iter().any(|s| s.len() > 0);
        if dumping {
            dump.push(format!("pane readonly={readonly} rect=({:.0},{:.0},{:.0},{:.0})", resp.rect.min.x, resp.rect.min.y, resp.rect.max.x, resp.rect.max.y));
            dump.push(format!("avail={:?} scroll=({:.0},{:.0}) first=({},{}) sel={:?} dv={} cf={}", avail, off_x, off_y, first_col, first_row, self.sel, self.wb.sheet(self.sheet_idx).dv.len(), self.wb.sheet(self.sheet_idx).cf.blocks.len()));
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
        // 冻结列的标题（固定不动）
        for c in 0..fx {
            let x = origin.x + HEAD_W + cols.pos(c);
            let w = cols.size(c);
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
        for r in 0..fy {
            let y = origin.y + HEAD_H + rows.pos(r);
            let h = rows.size(r);
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
        let mut r = first_row;
        guard = 0;
        while gy(r) < origin.y + avail.y && guard < 8192 {
            guard += 1;
            let y = gy(r);
            let h = rows.size(r);
            if h < 1.0 {
                r += 1;
                continue;
            }
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

        // 条件格式叠加（只算可视范围）
        let si = self.sheet_idx;
        if self.cf_stats_gen != self.doc_gen {
            let mut st = sheetcore::cf::CfStats::default();
            {
                let sheet = self.wb.sheet(si);
                sheetcore::cf::compute_stats(&sheet.cf, sheet, &mut st);
            }
            self.cf_stats = st;
            self.cf_stats_gen = self.doc_gen;
        }
        let mut cf_map: std::collections::HashMap<(u32, u32), sheetcore::cf::Overlay> =
            std::collections::HashMap::new();
        if !self.wb.sheet(si).cf.blocks.is_empty() {
            let sheet = self.wb.sheet(si);
            let stats = &self.cf_stats;
            let cf = &sheet.cf;
            let mut calc = sheetcore::formula::Calc::new(&self.wb);
            let mut rr = first_row;
            let mut g1 = 0;
            while g1 < 200 {
                g1 += 1;
                let mut cc = first_col;
                let mut g2 = 0;
                while g2 < 60 {
                    g2 += 1;
                    let ov = sheetcore::cf::overlay_at(cf, stats, sheet, rr, cc, |f, r0, c0| {
                        let shifted =
                            sheetcore::cf::shift_formula(f.trim_start_matches('='), r0, c0);
                        match sheetcore::formula::parse(&shifted) {
                            Ok(node) => matches!(
                                calc.eval_node(&node, si),
                                sheetcore::formula::FValue::Bool(true)
                            ),
                            Err(_) => false,
                        }
                    });
                    if !ov.is_empty() {
                        cf_map.insert((rr, cc), ov);
                    }
                    cc += 1;
                }
                rr += 1;
            }
        }

        if dumping {
            dump.push(format!("cfmap {} 项", cf_map.len()));
        }

        // 单元格
        let merges = self.wb.sheet(self.sheet_idx).merges.clone();
        let notes = self.wb.sheet(self.sheet_idx).notes.clone();
        let mut pending_filter: Option<(u32, Vec<(String, bool)>)> = None;
        let mut r = first_row;
        guard = 0;
        while gy(r) < origin.y + avail.y && guard < 8192 {
            guard += 1;
            let y = gy(r);
            let rh = rows.size(r);
            if rh < 1.0 {
                r += 1;
                continue;
            }
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
                self.paint_cell(
                    &painter,
                    rect,
                    r,
                    c,
                    notes.get(&sheetcore::model::CellRef::new(r, c)),
                    cf_map.get(&(r, c)),
                    dumping,
                    &mut dump,
                );
                // 筛选区域表头上的下拉箭头
                if let Some(f) = self.wb.sheet(self.sheet_idx).filter.clone() {
                    let (fr1, fc1, _, fc2) = f.range;
                    if r == fr1 && c >= fc1 && c <= fc2 {
                        let btn = Rect::from_center_size(
                            egui::pos2(rect.right() - 8.0, rect.center().y),
                            Vec2::new(14.0, 14.0),
                        );
                        painter.rect_filled(btn, 2.0, Color32::from_rgb(236, 239, 244));
                        painter.text(
                            btn.center(),
                            Align2::CENTER_CENTER,
                            "▾",
                            FontId::proportional(10.0),
                            Color32::from_rgb(70, 70, 70),
                        );
                        if dumping {
                            dump.push(format!("filter-arrow ({},{})", r, c));
                        }
                        if resp.clicked() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                if btn.contains(pos) {
                                    let sheet = self.wb.sheet(self.sheet_idx);
                                    let vals = sheetcore::sort_filter::unique_values(sheet, f.range, c);
                                    let allowed = f.allows.get(&c).cloned();
                                    let items: Vec<(String, bool)> = vals
                                        .into_iter()
                                        .map(|v| {
                                            let on = allowed.as_ref().map(|a| a.contains(&v)).unwrap_or(true);
                                            (v, on)
                                        })
                                        .collect();
                                    pending_filter = Some((c, items));
                                }
                            }
                        }
                    }
                }
                c += 1;
            }
            r += 1;
        }

        if let Some(pf) = pending_filter.take() {
            self.filter_popup = Some(pf);
        }

        // 冻结区单元格（覆盖在主体之上）
        if fx > 0 || fy > 0 {
            let clip_left = Rect::from_min_max(
                egui::pos2(origin.x + HEAD_W, origin.y + HEAD_H),
                egui::pos2(body_x, origin.y + avail.y),
            );
            let clip_top = Rect::from_min_max(
                egui::pos2(body_x, origin.y + HEAD_H),
                egui::pos2(origin.x + avail.x, body_y),
            );
            let clip_corner = Rect::from_min_max(
                egui::pos2(origin.x + HEAD_W, origin.y + HEAD_H),
                egui::pos2(body_x, body_y),
            );
            if fx > 0 {
                let p2 = painter.with_clip_rect(clip_left);
                let mut rr = first_row;
                let mut g = 0;
                while gy(rr) < origin.y + avail.y && g < 4000 {
                    g += 1;
                    let y = gy(rr);
                    let h = rows.size(rr);
                    if h >= 1.0 && y + h > origin.y + HEAD_H {
                        for cc in 0..fx {
                            let rect = Rect::from_min_size(
                                egui::pos2(origin.x + HEAD_W + cols.pos(cc), y),
                                Vec2::new(cols.size(cc), h),
                            );
                            self.paint_cell(
                                &p2,
                                rect,
                                rr,
                                cc,
                                notes.get(&sheetcore::model::CellRef::new(rr, cc)),
                                cf_map.get(&(rr, cc)),
                                dumping,
                                &mut dump,
                            );
                        }
                    }
                    rr += 1;
                }
            }
            if fy > 0 {
                let p2 = painter.with_clip_rect(clip_top);
                let mut cc = first_col;
                let mut g = 0;
                while gx(cc) < origin.x + avail.x && g < 2000 {
                    g += 1;
                    let x = gx(cc);
                    let w = cols.size(cc);
                    for rr in 0..fy {
                        let rect = Rect::from_min_size(
                            egui::pos2(x, origin.y + HEAD_H + rows.pos(rr)),
                            Vec2::new(w, rows.size(rr)),
                        );
                        self.paint_cell(
                            &p2,
                            rect,
                            rr,
                            cc,
                            notes.get(&sheetcore::model::CellRef::new(rr, cc)),
                            cf_map.get(&(rr, cc)),
                            dumping,
                            &mut dump,
                        );
                    }
                    cc += 1;
                }
            }
            let p3 = painter.with_clip_rect(clip_corner);
            for rr in 0..fy {
                for cc in 0..fx {
                    let rect = Rect::from_min_size(
                        egui::pos2(
                            origin.x + HEAD_W + cols.pos(cc),
                            origin.y + HEAD_H + rows.pos(rr),
                        ),
                        Vec2::new(cols.size(cc), rows.size(rr)),
                    );
                    self.paint_cell(
                        &p3,
                        rect,
                        rr,
                        cc,
                        notes.get(&sheetcore::model::CellRef::new(rr, cc)),
                        cf_map.get(&(rr, cc)),
                        dumping,
                        &mut dump,
                    );
                }
            }
            // 冻结边界加一条深一点的线，和普通网格线区分开
            let edge = Color32::from_rgb(160, 160, 160);
            if fx > 0 {
                painter.line_segment(
                    [
                        egui::pos2(body_x - 0.5, origin.y + HEAD_H),
                        egui::pos2(body_x - 0.5, origin.y + avail.y),
                    ],
                    Stroke::new(1.6, edge),
                );
            }
            if fy > 0 {
                painter.line_segment(
                    [
                        egui::pos2(origin.x + HEAD_W, body_y - 0.5),
                        egui::pos2(origin.x + avail.x, body_y - 0.5),
                    ],
                    Stroke::new(1.6, edge),
                );
            }
            if dumping {
                dump.push(format!("freeze rows={fy} cols={fx} body=({:.0},{:.0})", body_x, body_y));
            }
        }

        // 图片（在单元格之上、图表之下）
        let pics = self.wb.sheet(self.sheet_idx).pictures.clone();
        for (i, pic) in pics.iter().enumerate() {
            let (r1, c1, r2, c2) = pic.rect;
            let rect = egui::Rect::from_min_max(
                egui::pos2(px_of(c1), py_of(r1)),
                egui::pos2(px_of(c2), py_of(r2)),
            );
            let clip = egui::Rect::from_min_max(
                egui::pos2(origin.x + HEAD_W, origin.y + HEAD_H),
                egui::pos2(origin.x + avail.x, origin.y + avail.y),
            );
            if !rect.intersects(clip) {
                continue;
            }
            let key = format!("{}#{i}", pic.part);
            let tex = self.img_texture(&key, &pic.part, ctx);
            let p2 = painter.with_clip_rect(clip);
            match tex {
                Some(t) => {
                    p2.image(
                        t.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    if dumping {
                        dump.push(format!("pic {} at=({},{},{},{})", pic.part, r1, c1, r2, c2));
                    }
                }
                None => {
                    p2.rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(1.0, Color32::from_rgb(180, 120, 120)),
                        egui::StrokeKind::Inside,
                    );
                    if dumping {
                        dump.push(format!("pic-broken {} at=({},{},{},{})", pic.part, r1, c1, r2, c2));
                    }
                }
            }
        }

        // 图表（浮在单元格之上）
        let charts = self.wb.sheet(self.sheet_idx).charts.clone();
        for ch in charts.iter() {
            let (r1, c1, r2, c2) = ch.anchor;
            let top_left = egui::pos2(px_of(c1), py_of(r1));
            let bottom_right = egui::pos2(px_of(c2), py_of(r2));
            let rect = egui::Rect::from_min_max(top_left, bottom_right);
            let clip = egui::Rect::from_min_max(
                egui::pos2(origin.x + HEAD_W, origin.y + HEAD_H),
                egui::pos2(origin.x + avail.x, origin.y + avail.y),
            );
            if rect.intersects(clip) {
                let p2 = painter.with_clip_rect(clip);
                chart_view::paint_chart(&p2, rect, ch);
                if dumping {
                    dump.push(format!(
                        "chart {:?} 标题={:?} 系列={} 锚点=({},{},{},{})",
                        ch.kind, ch.title, ch.series.len(), r1, c1, r2, c2
                    ));
                }
            }
        }

        // 分页虚线（打印预览用）
        if self.show_pages {
            let (xs, ys) = sheetcore::page::page_breaks(self.wb.sheet(self.sheet_idx));
            let dash = Color32::from_rgb(120, 140, 180);
            let (r0, c0) = (first_row, first_col);
            let (pr1, pc1, pr2, pc2) = self
                .wb
                .sheet(self.sheet_idx)
                .page
                .print_area
                .unwrap_or((r0, c0, r0 + 500, c0 + 40));
            let x_off = cols.pos(pc1);
            let y_off = rows.pos(pr1);
            for x in xs.iter() {
                let px = gx(pc1) + x;
                if px >= body_x && px <= origin.x + avail.x {
                    let mut y = py_of(pr1).max(origin.y + HEAD_H) + 0.0;
                    while y < origin.y + avail.y {
                        let y2 = (y + 4.0).min(origin.y + avail.y);
                        painter.line_segment(
                            [egui::pos2(px, y), egui::pos2(px, y2)],
                            Stroke::new(1.0, dash),
                        );
                        y += 7.0;
                    }
                }
            }
            for y in ys.iter() {
                let py = gy(pr1) + y;
                if py >= body_y && py <= origin.y + avail.y {
                    let mut x = px_of(pc1).max(origin.x + HEAD_W);
                    while x < origin.x + avail.x {
                        let x2 = (x + 4.0).min(origin.x + avail.x);
                        painter.line_segment(
                            [egui::pos2(x, py), egui::pos2(x2, py)],
                            Stroke::new(1.0, dash),
                        );
                        x += 7.0;
                    }
                }
            }
            let _ = (pr2, pc2, x_off, y_off);
            if dumping {
                dump.push(format!("page-breaks x={} y={}", xs.len(), ys.len()));
            }
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
        let sx = px_of(sc1);
        let sy = py_of(sr1);
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

        // 圈释无效数据：红椭圆
        if self.circle_invalid && !self.invalid_cells.is_empty() {
            let cells: Vec<(u32, u32)> = self
                .invalid_cells
                .iter()
                .copied()
                .filter(|(r, c)| {
                    let x = gx(*c);
                    let y = gy(*r);
                    x + cols.size(*c) > origin.x + HEAD_W
                        && y + rows.size(*r) > origin.y + HEAD_H
                        && x < origin.x + avail.x
                        && y < origin.y + avail.y
                })
                .collect();
            for (r, c) in cells {
                let rect = Rect::from_min_size(
                    egui::pos2(gx(c) + 1.0, gy(r) + 1.0),
                    Vec2::new(cols.size(c) - 2.0, rows.size(r) - 2.0),
                );
                let center = rect.center();
                let radius = Vec2::new(rect.width() * 0.52, rect.height() * 0.46);
                let mut pts = Vec::new();
                for i in 0..=24 {
                    let a = i as f32 / 24.0 * std::f32::consts::TAU;
                    pts.push(egui::pos2(
                        center.x + radius.x * a.cos(),
                        center.y + radius.y * a.sin(),
                    ));
                }
                painter.add(egui::Shape::line(
                    pts,
                    Stroke::new(1.5, Color32::from_rgb(214, 48, 49)),
                ));
            }
        }

        // 输入提示：选中带提示的验证格时，弹一个黄框
        if let Some(dv) = sheetcore::dv::find(&self.wb.sheet(self.sheet_idx).dv, self.sel.0, self.sel.1) {
            if !dv.prompt_msg.is_empty() || !dv.prompt_title.is_empty() {
                let text = if dv.prompt_title.is_empty() {
                    dv.prompt_msg.clone()
                } else if dv.prompt_msg.is_empty() {
                    dv.prompt_title.clone()
                } else {
                    format!("{}\n{}", dv.prompt_title, dv.prompt_msg)
                };
                let w = 190.0f32.min(text.chars().count() as f32 * 12.0 + 16.0).max(80.0);
                let top = sy + sh + 4.0;
                let box_rect = Rect::from_min_size(egui::pos2(sx, top), Vec2::new(w, 34.0));
                painter.rect_filled(box_rect, 3.0, Color32::from_rgb(255, 249, 196));
                painter.rect_stroke(
                    box_rect,
                    3.0,
                    Stroke::new(1.0, Color32::from_rgb(214, 190, 90)),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    box_rect.min + Vec2::new(6.0, 5.0),
                    Align2::LEFT_TOP,
                    text,
                    FontId::proportional(11.0),
                    Color32::from_rgb(80, 70, 20),
                );
                if dumping {
                    dump.push(format!("dv-prompt at ({},{})", self.sel.0, self.sel.1));
                }
            }
        }

        // 填充柄：选区右下角的小方块，拖到底就填充
        {
            let hx = sx + sw;
            let hy = sy + sh;
            if hx > origin.x + HEAD_W && hy > origin.y + HEAD_H {
                let handle = Rect::from_min_size(egui::pos2(hx - 4.0, hy - 4.0), Vec2::splat(7.0));
                painter.rect_filled(handle, 1.0, Color32::from_rgb(26, 115, 232));
                let grab = Rect::from_min_size(egui::pos2(hx - 7.0, hy - 7.0), Vec2::splat(14.0));
                let resp = ui.interact(
                    grab,
                    egui::Id::new(("fill_handle", self.sheet_idx)),
                    egui::Sense::drag(),
                );
                if !readonly && resp.dragged() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let rr = rows.index_at(off_y + p.y - (origin.y + HEAD_H));
                        let cc = cols.index_at(off_x + p.x - (origin.x + HEAD_W));
                        let vertical = (p.y - hy).abs() > (p.x - hx).abs();
                        self.fill_drag = Some((if vertical { 0 } else { 1 }, rr, cc));
                        let preview = if vertical && rr > sr2 {
                            Some(Rect::from_min_max(
                                egui::pos2(sx, sy),
                                egui::pos2(sx + sw, py_of(rr + 1)),
                            ))
                        } else if !vertical && cc > sc2 {
                            Some(Rect::from_min_max(
                                egui::pos2(sx, sy),
                                egui::pos2(px_of(cc + 1), sy + sh),
                            ))
                        } else {
                            None
                        };
                        if let Some(pv) = preview {
                            painter.rect_stroke(
                                pv,
                                0.0,
                                Stroke::new(1.0, Color32::from_rgb(26, 115, 232)),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if dumping {
                            dump.push(format!("fill drag to ({rr},{cc})"));
                        }
                    }
                }
                if !readonly && resp.drag_stopped() {
                    if let Some((dir, rr, cc)) = self.fill_drag.take() {
                        if dir == 0 && rr > sr2 {
                            self.fill_to(rr, true);
                        } else if dir == 1 && cc > sc2 {
                            self.fill_to(cc, false);
                        }
                    }
                }
            }
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
        if !readonly && resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let dx = pos.x - (origin.x + HEAD_W);
                let dy = pos.y - (origin.y + HEAD_H);
                if dx >= 0.0 && dy >= 0.0 {
                    let c = if fx > 0 && pos.x < body_x {
                        cols.index_at(pos.x - (origin.x + HEAD_W))
                    } else {
                        cols.index_at(off_x + dx)
                    };
                    let r = if fy > 0 && pos.y < body_y {
                        rows.index_at(pos.y - (origin.y + HEAD_H))
                    } else {
                        rows.index_at(off_y + dy)
                    };
                    self.sel_end = (r, c);
                }
            }
        }

        // 点表头：整列 / 整行 / 全选
        if !readonly && resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let in_head_x = pos.x < origin.x + HEAD_W;
                let in_head_y = pos.y < origin.y + HEAD_H;
                if in_head_x && in_head_y {
                    let (_, _, r2, c2) = self.wb.sheet(self.sheet_idx).used_bounds().unwrap_or((0, 0, 0, 0));
                    self.sel = (0, 0);
                    self.sel_end = (r2, c2);
                    self.status = "已选中全表".into();
                } else if in_head_y {
                    let c = cols.index_at(off_x + pos.x - (origin.x + HEAD_W));
                    // 双击列右边界 = 自适应列宽
                    let edge = origin.x + HEAD_W + cols.pos(c + 1) - off_x;
                    if resp.double_clicked() && (pos.x - edge).abs() < 5.0 {
                        self.autofit_col(c);
                        return;
                    }
                    let (_, _, r2, _) = self.wb.sheet(self.sheet_idx).used_bounds().unwrap_or((0, 0, 0, 0));
                    self.sel = (0, c);
                    self.sel_end = (r2, c);
                    self.status = format!("已选中 {} 列", cell_name(0, c).trim_end_matches('1'));
                } else if in_head_x {
                    let r = rows.index_at(off_y + pos.y - (origin.y + HEAD_H));
                    let (_, _, _, c2) = self.wb.sheet(self.sheet_idx).used_bounds().unwrap_or((0, 0, 0, 0));
                    self.sel = (r, 0);
                    self.sel_end = (r, c2);
                    self.status = format!("已选中第 {} 行", r + 1);
                }
            }
        }

        // 点击选中
        if !readonly && resp.clicked() {
            // 点到别人正在编辑的格子，直接告诉他
            if let Some(pos) = resp.interact_pointer_pos() {
                if pos.x > origin.x + HEAD_W && pos.y > origin.y + HEAD_H {
                    let cc = cols.index_at(off_x + pos.x - (origin.x + HEAD_W));
                    let rr = rows.index_at(off_y + pos.y - (origin.y + HEAD_H));
                    if let Some(holder) = self.lock_holder(rr, cc) {
                        self.status = format!(
                            "{} 正在编辑 {}，同一格只能一个人改",
                            holder,
                            cell_name(rr, cc)
                        );
                    }
                }
            }
            if let Some(pos) = resp.interact_pointer_pos() {
                let dx = pos.x - (origin.x + HEAD_W);
                let dy = pos.y - (origin.y + HEAD_H);
                if dx >= 0.0 && dy >= 0.0 {
                    let c = if fx > 0 && pos.x < body_x {
                        cols.index_at(pos.x - (origin.x + HEAD_W))
                    } else {
                        cols.index_at(off_x + dx)
                    };
                    let r = if fy > 0 && pos.y < body_y {
                        rows.index_at(pos.y - (origin.y + HEAD_H))
                    } else {
                        rows.index_at(off_y + dy)
                    };
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

        // 序列下拉：选中格有 list 验证时画箭头
        let sel_dv = {
            let sheet = self.wb.sheet(self.sheet_idx);
            sheetcore::dv::find(&sheet.dv, self.sel.0, self.sel.1).cloned()
        };
        let mut open_list = false;
        if let Some(dv) = &sel_dv {
            if dv.kind == sheetcore::dv::DvKind::List && dv.show_dropdown {
                let bx = sx + sw - 16.0;
                let by = sy + sh / 2.0;
                let btn = Rect::from_center_size(egui::pos2(bx, by), Vec2::new(14.0, 16.0));
                painter.rect_filled(btn, 2.0, Color32::from_rgb(236, 239, 244));
                painter.text(
                    btn.center(),
                    Align2::CENTER_CENTER,
                    "▾",
                    FontId::proportional(11.0),
                    Color32::from_rgb(70, 70, 70),
                );
                if dumping {
                    dump.push(format!("dv-arrow ({},{}) at=({:.0},{:.0}) 选项数={}", self.sel.0, self.sel.1, btn.center().x, btn.center().y, sheetcore::dv::list_items(dv, self.wb.sheet(self.sheet_idx)).len()));
                }
                if !readonly && resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        if btn.contains(pos) {
                            open_list = true;
                        }
                    }
                }
            }
        }
        if open_list {
            self.dv_open = Some(self.sel);
        }

        // 序列下拉列表
        if let Some((dr, dc)) = self.dv_open {
            if let Some(dv) = {
                let sheet = self.wb.sheet(self.sheet_idx);
                sheetcore::dv::find(&sheet.dv, dr, dc).cloned()
            } {
                let items = {
                    let sheet = self.wb.sheet(self.sheet_idx);
                    sheetcore::dv::list_items(&dv, sheet)
                };
                if !items.is_empty() {
                    let pos = egui::pos2(gx(dc), gy(dr) + rows.size(dr));
                    let mut picked: Option<String> = None;
                    let mut close = false;
                    egui::Area::new(egui::Id::new("dv_list"))
                        .fixed_pos(pos)
                        .order(egui::Order::Foreground)
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_min_width(120.0);
                                for it in items.iter() {
                                    if ui.button(it).clicked() {
                                        picked = Some(it.clone());
                                    }
                                }
                            });
                        });
                    if let Some(v) = picked {
                        let cell = Cell {
                            value: Value::parse_input(&v),
                            formula: None,
                        };
                        let key = (self.sheet_idx, dr, dc);
                        self.wb.sheet_mut(self.sheet_idx).set(dr, dc, cell.clone());
                        self.wb.dirty = true;
                        self.doc_gen = self.doc_gen.wrapping_add(1);
                        self.deps.update_cell(&self.wb, key);
                        sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &[key]);
                        close = true;
                    }
                    if ctx.input(|i| i.pointer.any_click()) && !ctx.input(|i| i.pointer.hover_pos().map(|p| p.y < pos.y + 200.0).unwrap_or(false)) {
                        close = true;
                    }
                    if close {
                        self.dv_open = None;
                    }
                } else {
                    self.dv_open = None;
                }
            } else {
                self.dv_open = None;
            }
        }

        // 编辑框（只读面板不画）
        if let Some(mut ed) = if readonly { None } else { self.editing.take() } {
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

        // 悬停显示批注
        if let Some(pos) = resp.hover_pos() {
            if pos.x > origin.x + HEAD_W && pos.y > origin.y + HEAD_H {
                let hc = cols.index_at(off_x + pos.x - (origin.x + HEAD_W));
                let hr = rows.index_at(off_y + pos.y - (origin.y + HEAD_H));
                if let Some(n) = notes.get(&sheetcore::model::CellRef::new(hr, hc)) {
                    egui::show_tooltip_at_pointer(
                        ctx,
                        egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("note_tip")),
                        egui::Id::new("note_tip_body"),
                        |ui| {
                            ui.set_max_width(260.0);
                            ui.label(egui::RichText::new(format!("{}（{}）", cell_name(hr, hc), n.author)).strong());
                            ui.label(&n.text);
                        },
                    );
                }
            }
        }

        if !dump.is_empty() {
            self.dump_accum.push_str(&dump.join("
"));
            self.dump_accum.push_str("
");
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
                self.undo.set_site(site);
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
                self.undo.set_site(site);
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
                crate::net::NetEvent::LockOk { site, name, sheet, row, col } => {
                    let my_site = self.net.as_ref().map(|n| n.site).unwrap_or(0);
                    self.locks
                        .request(sheet, row, col, site, &name, self.lock_now());
                    if site == my_site {
                        self.my_lock = Some((sheet, row, col));
                        if std::env::var("SC_LOCK").is_ok() || std::env::var("SC_LOCK_TRY").is_ok() {
                            eprintln!("[lock] granted site={site} cell=({row},{col})");
                            self.probe_done = true;
                        }
                        self.lock_renew = std::time::Instant::now();
                        // 等到授权了才真正开编辑框
                        if let Some(initial) = self.pending_edit.take() {
                            let (r, c) = (row, col);
                            let buf = if initial.is_empty() {
                                match self.wb.sheet(sheet).get(r, c) {
                                    Some(cell) => cell
                                        .formula
                                        .as_deref()
                                        .map(|f| f.to_string())
                                        .unwrap_or_else(|| cell.value.display()),
                                    None => String::new(),
                                }
                            } else {
                                initial
                            };
                            self.editing = Some(EditState { buf });
                            self.status = format!("正在编辑 {}", cell_name(r, c));
                        }
                    } else {
                        self.status = format!("{} 开始编辑 {}", name, cell_name(row, col));
                    }
                }
                crate::net::NetEvent::StructuralHistory(items) => {
                    // 先存着：文件还在后台读/重算，这会儿改会被随后的整份替换冲掉
                    self.struct_history.extend(items);
                }
                crate::net::NetEvent::Structural { sheet, op } => {
                    if sheet < self.wb.sheets.len() {
                        sheetcore::structural::apply_structural_wb(&mut self.wb, sheet, &op);
                        self.wb.dirty = true;
                        self.doc_gen = self.doc_gen.wrapping_add(1);
                        sheetcore::formula::recalc_all(&mut self.wb);
                        self.deps = sheetcore::formula::deps::Deps::build(&self.wb);
                        self.status = format!("收到远端改动：{}", op.label());
                    }
                }
                crate::net::NetEvent::Styles(items) => {
                    for (sheet, row, col, st) in items {
                        if sheet >= self.wb.sheets.len() {
                            continue;
                        }
                        let id = self.wb.styles.intern(st);
                        self.wb.sheet_mut(sheet).set_style(row, col, id);
                    }
                    self.doc_gen = self.doc_gen.wrapping_add(1);
                }
                crate::net::NetEvent::Merge { sheet, rect, on, .. } => {
                    if sheet < self.wb.sheets.len() {
                        let sh = self.wb.sheet_mut(sheet);
                        if on {
                            sh.merge_range(rect.0, rect.1, rect.2, rect.3);
                        } else {
                            sh.unmerge_range(rect.0, rect.1, rect.2, rect.3);
                        }
                        self.doc_gen = self.doc_gen.wrapping_add(1);
                    }
                }
                crate::net::NetEvent::LockDeny { holder_name, row, col, .. } => {
                    self.pending_edit = None;
                    if std::env::var("SC_LOCK").is_ok() || std::env::var("SC_LOCK_TRY").is_ok() {
                        eprintln!("[lock] denied by {holder_name} cell=({row},{col})");
                        if std::env::var("SC_LOCK_RETRY").is_err() {
                            self.probe_done = true;
                        } else {
                            self.probe_at = std::time::Instant::now()
                                - std::time::Duration::from_secs(2);
                        }
                    }
                    self.status = format!(
                        "{} 正在编辑 {}，同一格只能一个人改",
                        holder_name,
                        cell_name(row, col)
                    );
                }
                crate::net::NetEvent::LockRelease { site, sheet, row, col } => {
                    self.locks.release(sheet, row, col, site);
                    let my_site = self.net.as_ref().map(|n| n.site).unwrap_or(0);
                    if site == my_site {
                        self.my_lock = None;
                    }
                }
                net::NetEvent::Connected => {
                    eprintln!("[net] connected");
                    self.status = "已连接".into();
                }
                net::NetEvent::Welcome { peers } => {
                    // 握手完成前发出去的光标会被丢掉；清一下，下一帧重发
                    self.last_cursor = None;
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
                    if std::env::var("SC_C2").is_ok() {
                        eprintln!("[C2] 收到光标 site={} sheet={} ({},{}) 我={} 当前表={}", c.site, c.sheet, c.row, c.col, me, self.sheet_idx);
                    }
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
        self.sync_history();
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
        // 有 dump 待写时也要保持刷新，否则窗口静止就永远等不到那一帧
        if std::env::var("SC_DUMP").is_ok() && self.dump_once {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // 协作或后台重算时保持刷新
        if self.net.is_some() || self.busy || self.save_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.dv_dialog(ctx);
        self.note_dialog(ctx);
        self.filter_dialog(ctx);
        self.pivot_dialog(ctx);
        self.renew_lock_if_needed();
        self.lock_probe(ctx);
        self.undo_probe();
        self.style_probe();
        self.struct_probe();
        self.macro_probe();
        self.a_group_probe(ctx);
        self.b5_probe();
        self.b6_probe(ctx);
        self.c2_probe();
        self.b3_probe();
        self.b1_probe();
        self.chart_dialog(ctx);
        self.show_compare_window(ctx);
        self.chart_manager(ctx);
        self.macro_window(ctx);
        self.find_dialog(ctx);
        self.data_dialog(ctx);
        self.cf_manager(ctx);
        if std::env::var("SC_PREVIEW").is_ok() && self.open_rx.is_none() {
            self.print_preview = true;
        }
        self.print_preview_window(ctx);
        self.members_window(ctx);
        if self.circle_invalid {
            self.compute_invalid();
        }
        self.rename_dialog(ctx);
        // 缩放：Ctrl+滚轮
        let (ctrl, wheel) = ctx.input(|i| (i.modifiers.command, i.raw_scroll_delta.y));
        if ctrl && wheel.abs() > 0.5 {
            let z = (self.zoom * (1.0 + wheel * 0.0015)).clamp(0.5, 2.5);
            if (z - self.zoom).abs() > 0.001 {
                self.zoom = z;
                ctx.set_zoom_factor(z);
            }
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
                let (cu, cr) = (self.undo.can_undo(), self.undo.can_redo());
                if ui
                    .add_enabled(cu, egui::Button::new("↶").small())
                    .on_hover_text(
                        self.undo
                            .undo_label()
                            .map(|l| format!("撤销 {l}（Ctrl+Z）"))
                            .unwrap_or_else(|| "撤销（Ctrl+Z）".into()),
                    )
                    .clicked()
                {
                    self.do_undo(false);
                }
                if ui
                    .add_enabled(cr, egui::Button::new("↷").small())
                    .on_hover_text(
                        self.undo
                            .redo_label()
                            .map(|l| format!("重做 {l}（Ctrl+Y）"))
                            .unwrap_or_else(|| "重做（Ctrl+Y）".into()),
                    )
                    .clicked()
                {
                    self.do_undo(true);
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
                    let online = ui.colored_label(
                        egui::Color32::from_rgb(46, 125, 50),
                        format!("协作中 · 在线 {} ▾", self.peers_list.len()),
                    );
                    if online.interact(egui::Sense::click()).clicked() {
                        self.members_open = true;
                    }
                }
                ui.separator();
                ui.label(format!("{}{}", self.wb.title(), if self.wb.dirty { " *" } else { "" }));
                if self.wb.has_vba {
                    ui.colored_label(
                        egui::Color32::from_rgb(180, 110, 0),
                        "含宏（保留不执行）",
                    );
                }
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
                let mut goto: Option<usize> = None;
                let mut del: Option<usize> = None;
                let mut ren: Option<(usize, String)> = None;
                let mut mv: Option<(usize, i32)> = None;
                for i in 0..self.wb.sheets.len() {
                    let name = self.wb.sheets[i].name.clone();
                    let resp = ui.selectable_label(self.sheet_idx == i, name.clone());
                    if resp.clicked() {
                        self.commit_edit(None);
                        goto = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("重命名…").clicked() {
                            ren = Some((i, name.clone()));
                            ui.close_menu();
                        }
                        if ui.button("左移").clicked() {
                            mv = Some((i, -1));
                            ui.close_menu();
                        }
                        if ui.button("右移").clicked() {
                            mv = Some((i, 1));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("删除").clicked() {
                            del = Some(i);
                            ui.close_menu();
                        }
                    });
                }
                if ui.button("＋").on_hover_text("新建工作表").clicked() {
                    self.add_sheet();
                }
                ui.separator();
                ui.label("缩放");
                let mut z = self.zoom;
                if ui
                    .add(egui::Slider::new(&mut z, 0.5..=2.5).suffix("×").fixed_decimals(2))
                    .changed()
                {
                    self.zoom = z;
                    ctx.set_zoom_factor(z);
                }
                if ui.button("100%").clicked() {
                    self.zoom = 1.0;
                    ctx.set_zoom_factor(1.0);
                }
                if let Some(i) = goto {
                    self.sheet_idx = i;
                    self.sel = (0, 0);
                }
                if let Some((i, n)) = ren {
                    self.rename_sheet = Some((i, n));
                }
                if let Some((i, d)) = mv {
                    self.move_sheet(i, d);
                }
                if let Some(i) = del {
                    self.delete_sheet(i);
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
    // 渲染后端：--renderer=wgpu|glow|auto，默认 auto（先 wgpu，失败退 glow）
    let mut want = std::env::var("SC_RENDERER").unwrap_or_else(|_| "auto".into());
    for a in std::env::args().skip(1) {
        if let Some(v) = a.strip_prefix("--renderer=") {
            want = v.to_string();
        }
    }
    let mut opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("Sheets Native (v1.6)"),
        ..Default::default()
    };
    // 候选顺序：auto 先试 wgpu（无 GPU 的远程桌面也能走 WARP 软件适配器），不行再退 glow
    let mut candidates: Vec<&str> = match want.as_str() {
        "glow" => vec!["glow"],
        "wgpu" => vec!["wgpu"],
        _ => vec!["wgpu", "glow"],
    };
    // 没编进来的后端直接跳过
    candidates.retain(|c| match *c {
        "wgpu" => cfg!(feature = "wgpu"),
        "glow" => cfg!(feature = "glow"),
        _ => false,
    });
    if candidates.is_empty() {
        candidates.push("glow");
    }
    eprintln!("[renderer] 候选 {:?}（请求 {want}）", candidates);
    let mut attempt = 0usize;
    let result = loop {
        let which = candidates[attempt.min(candidates.len() - 1)];
        #[cfg(feature = "wgpu")]
        {
            opts.renderer = if which == "wgpu" {
                eframe::Renderer::Wgpu
            } else {
                eframe::Renderer::Glow
            };
        }
        #[cfg(not(feature = "wgpu"))]
        {
            let _ = which;
        }
        eprintln!("[renderer] 尝试 {which}");
        // 自检用：强制某个后端失败，验证回退路径
        if std::env::var("SC_FORCE_FAIL").map(|v| v == which).unwrap_or(false) {
            let e = eframe::Error::AppCreation(Box::new(std::io::Error::other("forced")));
            eprintln!("[renderer] {which} 启动失败: forced");
            if attempt + 1 >= candidates.len() {
                break Err(e);
            }
            attempt += 1;
            continue;
        }
        let r = run_app(&mut opts, initial.clone(), auto_host, auto_join.clone(), auto_pass.clone());
        match r {
            Ok(()) => break Ok(()),
            Err(e) => {
                eprintln!("[renderer] {which} 启动失败: {e}");
                if attempt + 1 >= candidates.len() {
                    break Err(e);
                }
                attempt += 1;
            }
        }
    };
    result
}

fn run_app(
    opts: &mut eframe::NativeOptions,
    initial: Option<PathBuf>,
    auto_host: bool,
    auto_join: Option<String>,
    auto_pass: Option<String>,
) -> Result<(), eframe::Error> {
    eframe::run_native(
        "sheets-native",
        opts.clone(),
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
