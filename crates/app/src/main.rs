mod files;
mod net;
mod office_clipboard;
mod recent;
mod update;
mod session_lock;
mod single_instance;

use eframe::egui;

mod chart_view;
mod icons;
mod ui_icons;
mod ui_kit;

use ui_icons::Icon;
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

struct FmtState {
    font: String,
    bold: bool,
    italic: bool,
    size: f32,
    color: [u8; 3],
    fill: Option<[u8; 3]>,
    halign: u8,
    valign: u8,
    wrap: bool,
    format: String,
    border: [bool; 4],
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
    /// 跨表取数对话框：输入文本
    link_dlg: Option<String>,
    /// 运行宏对话框：宏名
    macro_run_dlg: Option<String>,
    macro_ran: bool,
    /// 占用卡心跳时间
    lock_beat: Option<std::time::Instant>,
    /// 已经为哪个文件尝试过自动发现（避免反复重连）
    auto_join_tried: Option<PathBuf>,
    /// 占用卡上写着的编辑者（没连上时显示在标题栏）
    remote_editor: Option<String>,
    last_title: String,
    /// 别的进程问"你开的是不是这个文件"
    open_requests: Option<std::sync::mpsc::Receiver<PathBuf>>,
    shared_current_file: Option<std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>>,
    /// 没带文件启动时显示最近文件
    show_start: bool,
    confirm_close_once: bool,
    tabs_done: bool,
    paste_done: bool,
    pending_tab_switch: Option<usize>,
    update_done: bool,
    /// 历史版本窗口：(工作簿路径, 版本列表)
    history_view: Option<(PathBuf, Vec<sheetcore::backup::Version>)>,
    /// 筛选下拉：列号 + 各选项是否勾选
    filter_popup: Option<(u32, Vec<(String, bool)>)>,
    /// 显示分页虚线
    show_pages: bool,
    /// 撤销/重做
    undo: sheetcore::undo::UndoStack,
    /// 标签页：与 active_tab 一一对应，Active 那个的数据在 self 上
    tabs: Vec<TabSlot>,
    active_tab: usize,
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
    d3_done: bool,
    b4_done: bool,
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
    /// 功能区当前页签：0 文件 1 开始 2 插入 3 公式 4 数据 5 视图
    ribbon_tab: usize,
    /// 单元格格式对话框
    fmt_dlg: bool,
    fmt_state: FmtState,
    /// 外部链接对话框
    xlink_dlg: bool,
    /// 修订记录（新的在后）
    rev: Vec<sheetcore::revision::Entry>,
    rev_open: bool,
    /// 文件列表（左侧）
    tree_open: bool,
    /// 自截图：SC_SHOT=<png>，抓自己的 framebuffer（RDP 断着也能出图）
    shot_path: Option<PathBuf>,
    shot_frames: u32,
    /// 标签拖拽排序
    tab_drag: Option<usize>,
    /// 名称框
    name_buf: String,
    name_edit: bool,
    /// 工作表重命名对话框
    rename_sheet: Option<(usize, String)>,
    /// 填充柄拖拽的终点（行或列，取决于方向）
    fill_drag: Option<(usize, u32, u32)>,
    /// 数据工具对话框：0=排序 1=分列
    data_dlg: Option<DataDlg>,
    /// 条件格式管理窗口
    cf_mgr: bool,
    /// 上次导入的外部数据（路径, 起始行, 起始列），刷新用
    last_import: Option<(std::path::PathBuf, u32, u32)>,
    /// 房间断了（可以接任宿主）
    room_lost: bool,
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
    /// 图表数据区域文本（如 A1:C10）
    chart_src: String,
    /// 点了「重新读取」
    chart_reload_src: bool,
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
    /// 正在写出的目标路径（保存成功后把文档路径切过去）
    save_target: Option<PathBuf>,
    /// 等文件加载完再执行的共享/加入动作
    pending_host: bool,
    pending_join: Option<String>,
}

/// 一个被挂起（非活动）的文档：切走时把它整份存这里，切回来再装回去
struct ParkedDoc {
    title: String,
    wb: Workbook,
    undo: sheetcore::undo::UndoStack,
    deps: sheetcore::formula::deps::Deps,
    sel: (u32, u32),
    sel_end: (u32, u32),
    sheet_idx: usize,
    scroll: Vec2,
    zoom: f32,
    doc_gen: u64,
    cf_stats_gen: u64,
}

/// 标签页槽位：一个是当前活动的，其余挂起
enum TabSlot {
    Active,
    Parked(Box<ParkedDoc>),
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
        install_fonts(&cc.egui_ctx);
        ui_kit::apply_theme(&cc.egui_ctx);
        // 截图时关掉淡入动画，否则会抓到窗口半透明的中间帧
        if std::env::var("SC_SHOT").is_ok() {
            cc.egui_ctx.style_mut(|s| s.animation_time = 0.0);
        }
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
            link_dlg: None,
            macro_run_dlg: None,
            macro_ran: false,
            lock_beat: None,
            auto_join_tried: None,
            remote_editor: None,
            last_title: String::new(),
            open_requests: None,
            shared_current_file: None,
            show_start: false,
            confirm_close_once: false,
            tabs_done: false,
            paste_done: false,
            pending_tab_switch: None,
            update_done: false,
            history_view: None,
            filter_popup: None,
            show_pages: false,
            undo: sheetcore::undo::UndoStack::new(),
            tabs: vec![TabSlot::Active],
            active_tab: 0,
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
            d3_done: false,
            b4_done: false,
            struct_history: Vec::new(),
            history_planted: 0,
            history_last_epoch: u64::MAX,
            wb_epoch: 0,
            probe_at: std::time::Instant::now(),
            lock_first_at: None,
            clip: None,
            find: FindState::default(),
            zoom: 1.0,
            ribbon_tab: 1,
            fmt_dlg: false,
            fmt_state: FmtState::default(),
            xlink_dlg: false,
            rev: Vec::new(),
            rev_open: false,
            tree_open: true,
            shot_path: std::env::var("SC_SHOT").ok().map(PathBuf::from),
            shot_frames: 0,
            tab_drag: None,
            name_buf: String::new(),
            name_edit: false,
            rename_sheet: None,
            fill_drag: None,
            data_dlg: None,
            cf_mgr: false,
            last_import: None,
            room_lost: false,
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
            chart_src: String::new(),
            chart_reload_src: false,
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
            save_target: None,
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
                    if let Some(p) = self.wb.path.clone() {
                        recent::touch(&p);
                    }
                    self.rev = self
                        .wb
                        .path
                        .as_ref()
                        .map(|p| sheetcore::revision::load(p, 300))
                        .unwrap_or_default();
                    let n: usize = self.wb.sheets.iter().map(|s| s.len()).sum();
                    self.status = format!("已打开（{n} 个非空格），正在后台重算…");
                    eprintln!("[open] 已显示缓存值 {n} 格");
                    // 无界面验收钩子：SC_FONT=字体名 把字体套到当前选区（走真实的 apply_style，
                    // 含样式去重、撤销事务、协作广播），SC_SAVE=路径 立刻另存一份
                    // 无界面验收：SC_XLINK_REFRESH=1 打开后立刻更新外部链接并重算
                    if std::env::var("SC_XLINK_REFRESH").is_ok() && !self.wb.links.is_empty() {
                        let rep = sheetcore::xlinks::refresh(&mut self.wb);
                        sheetcore::formula::recalc_all(&mut self.wb);
                        // 让还在路上的后台重算作废，别拿旧快照盖掉刷新结果
                        self.doc_gen = self.doc_gen.wrapping_add(1);
                        eprintln!(
                            "[xlink] 更新 {} 个，失败 {:?}，A1={:?}",
                            rep.ok,
                            rep.failed,
                            self.wb.sheets[0].get(0, 0).map(|c| c.value.display())
                        );
                    }
                    // 无界面验收：SC_OPEN_DIALOG=format/revision/xlink 直接开对话框截图
                    match std::env::var("SC_OPEN_DIALOG").as_deref() {
                        Ok("format") => self.open_format_dialog(),
                        Ok("revision") => self.rev_open = true,
                        Ok("xlink") => self.xlink_dlg = true,
                        Ok("find") => self.find.open = true,
                        Ok("cf") => self.cf_mgr = true,
                        Ok("chart") => {
                            self.chart_mgr = true;
                            self.reload_charts();
                        }
                        _ => {}
                    }
                    // 无界面验收：SC_REV_REVERT=N 撤回倒数第 N 条修订
                    if let Ok(v) = std::env::var("SC_REV_REVERT") {
                        if let Ok(n) = v.parse::<usize>() {
                            let idx = self.rev.len().saturating_sub(n.max(1));
                            self.revert_revision(idx);
                            eprintln!("[rev] 撤回倒数第 {n} 条（索引 {idx}），共 {} 条", self.rev.len());
                        }
                    }
                    if let Ok(font) = std::env::var("SC_FONT") {
                        let font = font.trim().to_string();
                        self.apply_style(move |s| s.font = font.clone());
                        eprintln!("[font] 选区已套用字体 {:?}", self.current_style().font);
                    }
                    if let Ok(out) = std::env::var("SC_SAVE") {
                        let target = PathBuf::from(&out);
                        // 基准沿用打开时的原文件（和另存为的语义一致），不覆盖 wb.path
                        let wb = self.wb.clone();
                        match sheetcore::xlsx::save_atomic(&target, &wb) {
                            Ok(()) => eprintln!("[save] 测试钩子已存 {}", target.display()),
                            Err(e) => eprintln!("[save] 测试钩子失败 {e:#}"),
                        }
                    }
                    if self.pending_host {
                        self.pending_host = false;
                        self.start_host();
                    } else if let Some(addr) = self.pending_join.take() {
                        self.join_addr = addr;
                        self.join_shared();
                    } else {
                        // 没指定角色：看看文件旁边有没有同事留下的占用卡
                        self.try_auto_join();
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
        // 基准路径必须指向「打开时的原文件」：umya 从它继承样式、合并、数据验证、
        // 图片这些我们不在模型里重建的结构。另存到新路径时把 path 改成目标路径，
        // 就会写出一份只剩单元格值的文件（字体/格式全丢）。
        wb.path = self.wb.path.clone().filter(|p| p.exists());
        let shown = path.display().to_string();
        self.save_target = Some(path.clone());
        self.status = format!("正在保存 {shown}…");
        self.wb.dirty = false;
        let (tx, rx) = std::sync::mpsc::channel();
        self.save_rx = Some(rx);
        std::thread::spawn(move || {
            // 先写临时文件，成功再替换，避免写一半断电把原文件毁了
            let tmp = path.with_extension("xlsx.tmp");
            let res = match sheetcore::xlsx::save_atomic(&tmp, &wb) {
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
                    if let Some(t) = self.save_target.take() {
                        self.wb.path = Some(t);
                    }
                    self.status = format!("已保存 {p}");
                    eprintln!("[save] 完成 {p}");
                }
                Err(e) => {
                    self.save_target = None;
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

    /// 无界面自检：导入 CSV + 刷新
    fn b4_probe(&mut self) {
        if std::env::var("SC_B4").is_err() || self.open_rx.is_some() || self.b4_done {
            return;
        }
        self.b4_done = true;
        self.sel = (0, 0);
        self.sel_end = (0, 0);
        self.import_csv_from(std::path::Path::new("F:/Finance/work/import.csv"));
        eprintln!("[B4] {}", self.status);
        for r in 0..4 {
            let row: Vec<String> = (0..3)
                .map(|c| self.wb.sheet(self.sheet_idx).get(r, c).map(|x| x.value.display()).unwrap_or_default())
                .collect();
            eprintln!("[B4] 第{}行 {:?}", r + 1, row);
        }
        // 改文件再刷新
        let _ = std::fs::write("F:/Finance/work/import.csv", "姓名,部门,金额
赵六,财务,900
");
        self.refresh_import();
        let v = self.wb.sheet(self.sheet_idx).get(1, 0).map(|x| x.value.display()).unwrap_or_default();
        eprintln!("[B4] 刷新后 A2={v} 状态={}", self.status);
        eprintln!("[B4] 数据源 {} 个", self.wb.sources.len());

        // 存盘再重开：数据源应当还在，刷新能力才谈得上"持久"
        let out = std::path::PathBuf::from("F:/Finance/work/b4_out.xlsx");
        let _ = std::fs::remove_file(&out);
        if let Err(e) = sheetcore::xlsx::save_atomic(&out, &self.wb) {
            eprintln!("[B4] 存盘失败: {e:#}");
            return;
        }
        // 挂查询表（必须在文件写完之后，和手动流程一致）
        self.wb.path = Some(out.clone());
        self.attach_query_tables();
        eprintln!("[B4] {}", self.status);
        match sheetcore::xlsx::load(&out) {
            Ok(back) => eprintln!(
                "[B4] 重开后数据源={} 首个={:?}",
                back.sources.len(),
                back.sources.first().map(|s| format!("{:?} {}", s.kind, s.rows * s.cols))
            ),
            Err(e) => eprintln!("[B4] 重开失败: {e:#}"),
        }
    }

    /// 无界面自检：图表改颜色/轴标题 → 写盘 → 重读
    fn d3_probe(&mut self) {
        if std::env::var("SC_D3").is_err() || self.open_rx.is_some() || self.d3_done {
            return;
        }
        self.d3_done = true;
        let charts = self.wb.sheet(self.sheet_idx).charts.clone();
        let Some(mut ch) = charts.first().cloned() else {
            eprintln!("[D3] 当前表没有图表");
            return;
        };
        ch.axis_x = "季度".into();
        ch.axis_y = "销量".into();
        if let Some(s0) = ch.series.first_mut() {
            s0.color = Some([0xE0, 0x4A, 0x2F]);
        }
        ch.y_min = Some(0.0);
        ch.y_max = Some(600.0);
        eprintln!("[D3] 改前 part={:?} 系列={}", ch.part, ch.series.len());
        self.commit_chart(ch);
        eprintln!("[D3] 提交状态={}", self.status);
        if let Some(path) = self.wb.path.clone() {
            match sheetcore::charts::load_charts(&path, &self.wb.sheet(self.sheet_idx).name) {
                Ok(list) => {
                    for c in list.iter().take(1) {
                        eprintln!(
                            "[D3] 重读 轴X={:?} 轴Y={:?} ymax={:?} 首色={:?}",
                            c.axis_x, c.axis_y, c.y_max,
                            c.series.first().and_then(|s| s.color)
                        );
                    }
                }
                Err(e) => eprintln!("[D3] 重读失败 {e:#}"),
            }
        }
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
        match sheetcore::xlsx::save_atomic(&out, &self.wb) {
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
        match sheetcore::xlsx::save_atomic(&out, &self.wb) {
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
                        .map(|f| sheetcore::xlinks::display_formula(&self.wb, f))
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
        self.record_revision(label, &deltas);
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


    /// 远端改动也进日志（只展示，不给撤回）
    fn record_remote_ops(
        &mut self,
        ops: &[sheetcore::sync::Op],
        befores: Vec<Option<sheetcore::model::Cell>>,
    ) {
        let Some(path) = self.wb.path.clone() else {
            return;
        };
        for (op, before) in ops.iter().zip(befores.into_iter()) {
            let Some(sheet) = self.wb.sheets.get(op.sheet) else {
                continue;
            };
            let sheet_name = sheet.name.clone();
            let after = sheet.get(op.row, op.col).cloned();
            let (Some(b), Some(a)) = (before, after) else {
                continue;
            };
            let who = self
                .peers_list
                .iter()
                .find(|(s, _, _)| *s == op.site)
                .map(|(_, n, _)| n.clone())
                .unwrap_or_else(|| format!("同事#{}", op.site));
            let mut e = sheetcore::revision::Entry::new(&who, op.site, "远端编辑", &sheet_name);
            e.remote = true;
            e.cells.push(sheetcore::revision::Change {
                r: op.row,
                c: op.col,
                before: sheetcore::revision::Snap::from_cell(&b),
                after: sheetcore::revision::Snap::from_cell(&a),
                before_style: None,
                after_style: None,
            });
            let _ = sheetcore::revision::append(&path, &e);
            self.rev.push(e);
            if self.rev.len() > 600 {
                self.rev.remove(0);
            }
        }
    }

    /// 把一次动作写进修订日志（本地动作）
    fn record_revision(&mut self, label: &str, deltas: &[sheetcore::undo::CellDelta]) {
        let Some(path) = self.wb.path.clone() else {
            return; // 没保存过的新文件还不知道日志放哪
        };
        let sheet = self.wb.sheet(self.sheet_idx).name.clone();
        let mut e = sheetcore::revision::Entry::new(&self.my_name, 0, label, &sheet);
        let total = deltas.len();
        let mut only_style = true;
        for d in deltas.iter().take(sheetcore::revision::MAX_CELLS) {
            if d.before.value != d.after.value || d.before.formula != d.after.formula {
                only_style = false;
            }
            let (bs, as_) = if d.before_style != d.after_style {
                (
                    Some(self.wb.styles.get(d.before_style).clone()),
                    Some(self.wb.styles.get(d.after_style).clone()),
                )
            } else {
                (None, None)
            };
            e.cells.push(sheetcore::revision::Change {
                r: d.row,
                c: d.col,
                before: sheetcore::revision::Snap::from_cell(&d.before),
                after: sheetcore::revision::Snap::from_cell(&d.after),
                before_style: bs,
                after_style: as_,
            });
        }
        if only_style && total > 0 {
            e.note = "格式变更".into();
        }
        if total > sheetcore::revision::MAX_CELLS {
            e.note = format!("共 {total} 格，只记前 {} 格", sheetcore::revision::MAX_CELLS);
        }
        if let Err(err) = sheetcore::revision::append(&path, &e) {
            eprintln!("[rev] 写日志失败: {err}");
        }
        self.rev.push(e);
        if self.rev.len() > 600 {
            self.rev.remove(0);
        }
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

        ui_kit::window(ctx, "打印预览", 420.0)
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

    /// 导入 CSV/TSV 到当前选区左上角
    fn import_csv(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV / TSV", &["csv", "tsv", "txt"])
            .pick_file()
        else {
            return;
        };
        self.import_csv_from(&path);
    }

    fn import_csv_from(&mut self, path: &std::path::Path) {
        let text = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(e) => {
                self.status = format!("读不了 {}：{e}", path.display());
                return;
            }
        };
        // 分隔符要记下来：查询表连接里写错分隔符，Excel 刷新会读成一整列
        let first = text.lines().next().unwrap_or("");
        let tabs = first.matches('\t').count();
        let commas = first.matches(',').count();
        let delim = if tabs > commas { '\t' } else { ',' };
        let rows = sheetcore::csv::parse_auto(&text);
        if rows.is_empty() {
            self.status = "这个文件里没有可导入的内容".into();
            return;
        }
        let (r0, c0) = self.sel;
        let max_r = rows.len() as u32;
        let max_c = rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        let mut keys: Vec<(u32, u32)> = Vec::new();
        for r in 0..max_r {
            for c in 0..max_c {
                keys.push((r0 + r, c0 + c));
            }
        }
        let txn = self.txn_begin(&keys);
        for (i, row) in rows.iter().enumerate() {
            for (j, val) in row.iter().enumerate() {
                let cell = if val.is_empty() {
                    sheetcore::model::Cell {
                        value: sheetcore::model::Value::Empty,
                        formula: None,
                    }
                } else {
                    sheetcore::model::Cell {
                        value: sheetcore::model::Value::parse_input(val),
                        formula: None,
                    }
                };
                self.wb.sheet_mut(self.sheet_idx).set(r0 + i as u32, c0 + j as u32, cell);
            }
        }
        self.txn_end("导入 CSV", txn);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        // 登记成数据源：换台机器/重开文件还能刷新
        let size = (rows.len() as u32, max_c);
        let stored_path = sheetcore::external::store_path(&self.wb, path);
        let key = (stored_path.clone(), r0, c0);
        let existing = self.wb.sources.iter_mut().find(|src| match &src.kind {
            sheetcore::external::Kind::Csv { path: p, .. } => {
                (p.clone(), src.at.0, src.at.1) == key
            }
            _ => false,
        });
        let src = sheetcore::external::Source {
            name: String::new(),
            kind: sheetcore::external::Kind::Csv {
                // 和文件放一起就存相对路径，换机器也能刷新
                path: stored_path.clone(),
                delim,
                header: false,
            },
            at: (r0, c0),
            rows: size.0,
            cols: size.1,
        };
        match existing {
            Some(slot) => {
                slot.rows = size.0;
                slot.cols = size.1;
            }
            None => {
                let n = self.wb.sources.len() + 1;
                let mut src = src;
                src.name = format!("_sc_src_{n}");
                self.wb.sources.push(src);
            }
        }
        self.last_import = Some((path.to_path_buf(), r0, c0));
        self.status = format!(
            "已导入 {} 行 × {} 列（{}）",
            rows.len(),
            max_c,
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    /// 刷新数据：优先按登记过的数据源重读，没有就退回"上次导入"
    fn refresh_import(&mut self) {
        if self.wb.sources.is_empty() {
            match self.last_import.clone() {
                Some((path, r, c)) => {
                    self.sel = (r, c);
                    self.sel_end = (r, c);
                    self.import_csv_from(&path);
                }
                None => self.status = "还没有导入过外部数据".into(),
            }
            return;
        }
        let idx = self.sheet_idx;
        let sources = self.wb.sources.clone();
        let mut ok = 0;
        let mut fail = 0;
        for mut src in sources {
            match sheetcore::external::refresh(&mut self.wb, idx, &src) {
                Ok((rows, cols)) => {
                    ok += 1;
                    // 行数变了就更新记录，下次保存写回去
                    if let Some(slot) = self.wb.sources.iter_mut().find(|s| s.name == src.name) {
                        slot.rows = rows;
                        slot.cols = cols;
                    }
                    src.rows = rows;
                }
                Err(_) => fail += 1,
            }
        }
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.wb.dirty = true;
        self.status = if fail == 0 {
            format!("已刷新 {ok} 个数据源")
        } else {
            format!("刷新完成：成功 {ok}，失败 {fail}")
        };
    }


    /// 把从 Excel 复制来的块贴进来：值 + 格式 + 合并
    fn paste_office_block(&mut self, block: &office_clipboard::OfficeBlock) -> bool {
        if block.is_empty() {
            return false;
        }
        let (r0, c0) = self.sel;
        let cols = block.cols() as u32;
        let rows = block.rows.len() as u32;
        if (rows as u64) * (cols as u64) > 200_000 {
            self.status = "粘贴区域太大（超过 20 万格），改用纯文本粘贴".into();
            return false;
        }

        let keys = sheetcore::undo::rect_keys(r0, c0, r0 + rows - 1, c0 + cols - 1);
        let txn = self.txn_begin(&keys);
        let mut merges: Vec<(u32, u32, u32, u32)> = Vec::new();
        let si = self.sheet_idx;

        for (ri, row) in block.rows.iter().enumerate() {
            let mut ci: u32 = 0;
            for cell in row {
                let r = r0 + ri as u32;
                let c = c0 + ci;
                // 值：带公式的按缓存值类型走，之后统一重算
                let text = cell.text.trim();
                let value = if cell.formula.is_some() {
                    match cell.kind {
                        0 => text
                            .replace(',', "")
                            .parse::<f64>()
                            .map(Value::Number)
                            .unwrap_or(Value::Empty),
                        2 => Value::Bool(text == "1" || text.eq_ignore_ascii_case("true")),
                        _ => {
                            if text.is_empty() {
                                Value::Empty
                            } else {
                                Value::Text(text.into())
                            }
                        }
                    }
                } else if text.is_empty() {
                    Value::Empty
                } else if let Ok(n) = text.replace(',', "").parse::<f64>() {
                    Value::Number(n)
                } else {
                    Value::Text(text.into())
                };
                self.wb.sheet_mut(si).set(
                    r,
                    c,
                    Cell { value, formula: cell.formula.as_deref().map(|f| f.into()) },
                );

                // 格式
                let base_id = self.wb.sheet(si).style_of(r, c);
                let mut st = self.wb.styles.get(base_id).clone();
                let mut touched = false;
                if cell.bold != st.bold {
                    st.bold = cell.bold;
                    touched = true;
                }
                if cell.italic != st.italic {
                    st.italic = cell.italic;
                    touched = true;
                }
                if let Some(fill) = cell.fill {
                    st.fill = Some(fill);
                    touched = true;
                }
                if let Some(color) = cell.color {
                    st.color = color;
                    touched = true;
                }
                if cell.halign != 0 && cell.halign != st.halign {
                    st.halign = cell.halign;
                    touched = true;
                }
                if let Some(bc) = cell.border {
                    for side in st.border.iter_mut() {
                        if side.style == 0 {
                            side.style = 1;
                            side.color = bc;
                        }
                    }
                    touched = true;
                }
                // XML Spreadsheet 还带了字体名/字号/数字格式，HTML 那条路没有
                if !cell.font.is_empty() {
                    let f = sheetcore::style::normalize_font(&cell.font);
                    if f != st.font {
                        st.font = f;
                        touched = true;
                    }
                }
                if cell.size > 0.0 && (cell.size - st.size).abs() > 0.01 {
                    st.size = cell.size;
                    touched = true;
                }
                if !cell.num_format.is_empty() && cell.num_format != st.format {
                    st.format = cell.num_format.clone();
                    touched = true;
                }
                if touched {
                    let id = self.wb.styles.intern(st.clone());
                    self.wb.sheet_mut(si).set_style(r, c, id);
                }

                // 合并
                if cell.colspan > 1 || cell.rowspan > 1 {
                    merges.push((r, c, r + cell.rowspan - 1, c + cell.colspan - 1));
                }
                ci += cell.colspan.max(1);
            }
        }

        if !merges.is_empty() {
            let sheet = self.wb.sheet_mut(si);
            for m in merges {
                if !sheet.merges.iter().any(|x| x.0 == m.0 && x.1 == m.1 && x.2 == m.2 && x.3 == m.3) {
                    sheet.merges.push(m);
                }
            }
            sheet.merges_dirty = true;
        }

        // 公式要立刻算出来（Excel 复制过来的是 R1C1 折算后的 A1）
        let fkeys: Vec<(usize, u32, u32)> = (0..rows)
            .flat_map(|ri| (0..cols).map(move |ci| (r0 + ri, c0 + ci)))
            .filter(|(r, c)| {
                self.wb
                    .sheet(si)
                    .get(*r, *c)
                    .map(|x| x.formula.is_some())
                    .unwrap_or(false)
            })
            .map(|(r, c)| (si, r, c))
            .collect();
        if !fkeys.is_empty() {
            for k in &fkeys {
                self.deps.update_cell(&self.wb, *k);
            }
            sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &fkeys);
        }

        self.txn_end("从 Excel 粘贴", txn);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.sel_end = (r0 + rows - 1, c0 + cols - 1);
        self.status = format!("已粘贴 Excel 区域 {rows} 行 × {cols} 列（含格式与合并）");
        eprintln!("[paste] office 块 {rows}x{cols}");
        true
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
        ui_kit::window(ctx, "在线成员", 260.0)
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
        ui_kit::window(ctx, "条件格式管理", 460.0)
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
        ui_kit::window(ctx, title, 420.0)
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

    /// 复制工作表：插到原表后面，名字自动去重
    fn duplicate_sheet(&mut self, i: usize) {
        let mut copy = self.wb.sheets[i].clone();
        let base = copy.name.clone();
        let mut n = 2;
        let mut name = format!("{base} ({n})");
        while self.wb.sheets.iter().any(|s| s.name == name) {
            n += 1;
            name = format!("{base} ({n})");
        }
        copy.name = name;
        copy.spilled.clear();
        self.wb.sheets.insert(i + 1, copy);
        self.sheet_idx = i + 1;
        self.wb.dirty = true;
    }

    /// 已用区域的右下角
    fn used_end(&self) -> (u32, u32) {
        let sheet = self.wb.sheet(self.sheet_idx);
        let (mut mr, mut mc) = (0u32, 0u32);
        for (k, cell) in sheet.iter() {
            if cell.is_blank() {
                continue;
            }
            mr = mr.max(k.row);
            mc = mc.max(k.col);
        }
        (mr, mc)
    }

    /// Ctrl+方向：沿方向跳过空白，停在连续数据块的边缘（Excel 语义）
    fn jump_edge(&self, dr: i64, dc: i64) -> (u32, u32) {
        let sheet = self.wb.sheet(self.sheet_idx);
        let (mut r, mut c) = (self.sel.0 as i64, self.sel.1 as i64);
        let occupied = |r: i64, c: i64| -> bool {
            r >= 0
                && c >= 0
                && sheet
                    .get(r as u32, c as u32)
                    .map(|x| !x.is_blank())
                    .unwrap_or(false)
        };
        // 先跳过空白
        for _ in 0..4096 {
            let (nr, nc) = (r + dr, c + dc);
            if nr < 0 || nc < 0 {
                break;
            }
            if occupied(nr, nc) {
                break;
            }
            r = nr;
            c = nc;
        }
        // 再走到数据块末尾
        if occupied(r + dr, c + dc) {
            for _ in 0..4096 {
                let (nr, nc) = (r + dr, c + dc);
                if !occupied(nr, nc) {
                    break;
                }
                r = nr;
                c = nc;
            }
        }
        (r.max(0) as u32, c.max(0) as u32)
    }

    /// 打开单元格格式对话框，先读选区左上角的样式
    fn open_format_dialog(&mut self) {
        let (r, c) = self.sel;
        let id = self.wb.sheet(self.sheet_idx).style_of(r, c);
        let st = self.wb.styles.get(id).clone();
        self.fmt_state = FmtState {
            font: st.font.clone(),
            bold: st.bold,
            italic: st.italic,
            size: st.size,
            color: st.color,
            fill: st.fill,
            halign: st.halign,
            valign: st.valign,
            wrap: st.wrap,
            format: st.format,
            border: [
                st.border[0].style != 0,
                st.border[1].style != 0,
                st.border[2].style != 0,
                st.border[3].style != 0,
            ],
        };
        self.fmt_dlg = true;
    }

    /// 名称框：支持 A1、A1:B2、以及定义名称
    fn goto_name(&mut self) {
        let text = self.name_buf.trim().to_string();
        if text.is_empty() {
            return;
        }
        let up = text.to_ascii_uppercase();
        let parts: Vec<&str> = up.split(':').collect();
        if parts.len() == 2 {
            if let (Some(a), Some(b)) = (
                sheetcore::sparse::parse_ref(parts[0]),
                sheetcore::sparse::parse_ref(parts[1]),
            ) {
                self.commit_edit(None);
                self.sel = a;
                self.sel_end = b;
                self.ensure_visible();
                return;
            }
        } else if let Some(a) = sheetcore::sparse::parse_ref(parts[0]) {
            self.commit_edit(None);
            self.sel = a;
            self.sel_end = a;
            self.ensure_visible();
            return;
        }
        let hit = self
            .wb
            .names
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(&text))
            .map(|(_, sh, body)| (*sh, body.clone()));
        if let Some((_sh, body)) = hit {
            let body = body.trim_start_matches('=').to_string();
            let tail = body.rsplit('!').next().unwrap_or(&body).replace('$', "");
            if let Some(pos) = sheetcore::sparse::parse_ref(&tail) {
                if let Some((sheet, _)) = body.rsplit_once('!') {
                    let nm = sheet.trim_matches(|c| c == '\'');
                    if let Some(idx) = self
                        .wb
                        .sheets
                        .iter()
                        .position(|s| s.name.eq_ignore_ascii_case(nm))
                    {
                        self.sheet_idx = idx;
                    }
                }
                self.commit_edit(None);
                self.sel = pos;
                self.sel_end = pos;
                self.ensure_visible();
                return;
            }
        }
        self.status = format!("找不到名称：{text}");
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
                // 手输的 [预算.xlsx]Sheet!A1：先把工作簿登记成链接（能读到就带缓存），
                // 再折回 Excel 认的 [N] 形式
                let mut text = ed.buf.trim().to_string();
                if sheetcore::xlinks::is_external_formula(&text) {
                    let added = sheetcore::xlinks::absorb_formula(&mut self.wb, &text);
                    if added > 0 {
                        self.status = format!("已登记 {added} 个外部工作簿");
                    }
                    text = sheetcore::xlinks::raw_formula(&self.wb, &text);
                }
                Cell { value: Value::Text(text.as_str().into()), formula: Some(text.as_str().into()) }
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
            // 从 Excel 复制过来的块：HTML 里有格式与合并，优先走这条
            if paste_k && !paste_v_k {
                if let Some(block) = office_clipboard::read_table() {
                    if self.paste_office_block(&block) {
                        return;
                    }
                }
            }
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
        // Ctrl+方向：跳到数据边缘；Ctrl+Home/End：回到 A1 / 已用区域末尾
        let (ctrl, home, end, pgup, pgdn) = ctx.input(|i| {
            (
                i.modifiers.command,
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::PageUp),
                i.key_pressed(Key::PageDown),
            )
        });
        if ctrl && (home || end) {
            self.commit_edit(None);
            let (r, c) = if home {
                (0u32, 0u32)
            } else {
                self.used_end()
            };
            self.sel = (r, c);
            self.sel_end = (r, c);
            self.ensure_visible();
            return;
        }
        if pgup || pgdn {
            self.commit_edit(None);
            let step = 20u32;
            let (r, c) = self.sel;
            let r = if pgdn { r.saturating_add(step) } else { r.saturating_sub(step) };
            self.sel = (r, c);
            self.sel_end = (r, c);
            self.ensure_visible();
            return;
        }
        if ctrl && (ctx.input(|i| i.key_pressed(Key::ArrowUp))
            || ctx.input(|i| i.key_pressed(Key::ArrowDown))
            || ctx.input(|i| i.key_pressed(Key::ArrowLeft))
            || ctx.input(|i| i.key_pressed(Key::ArrowRight)))
        {
            let (dr, dc) = ctx.input(|i| {
                (
                    if i.key_pressed(Key::ArrowDown) { 1i64 } else if i.key_pressed(Key::ArrowUp) { -1 } else { 0 },
                    if i.key_pressed(Key::ArrowRight) { 1i64 } else if i.key_pressed(Key::ArrowLeft) { -1 } else { 0 },
                )
            });
            self.commit_edit(None);
            let (r, c) = self.jump_edge(dr, dc);
            self.sel = (r, c);
            self.sel_end = (r, c);
            self.ensure_visible();
            return;
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::Num1)) {
            self.open_format_dialog();
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
    // ===== 功能区（Excel 式分组）=====

    fn ribbon_file(&mut self, ui: &mut egui::Ui) {
        ui_kit::group(ui, "文件", |ui| {
            if ui_kit::big(ui, Icon::Open, "打开").clicked() {
                self.open();
            }
            if ui_kit::big(ui, Icon::Save, "保存").clicked() {
                self.save(false);
            }
            if ui_kit::big(ui, Icon::SaveAs, "另存为").clicked() {
                self.save(true);
            }
        });

        ui_kit::group(ui, "打印", |ui| {
            if ui_kit::big(ui, Icon::Print, "打印预览").clicked() {
                self.preview_page = 0;
                self.print_preview = true;
            }
            ui.vertical(|ui| {
                if ui_kit::pill(ui, "导出 PDF…", "把当前表导出成 PDF").clicked() {
                    self.export_pdf_dialog();
                }
                if ui_kit::pill(ui, "历史版本…", "从自动备份恢复").clicked() {
                    if let Some(p) = self.wb.path.clone() {
                        self.history_view = Some((p.clone(), sheetcore::backup::list(&p)));
                    } else {
                        self.status = "先保存文件，才有历史版本".into();
                    }
                }
            });
        });

        ui_kit::group(ui, "编辑", |ui| {
            let (cu, cr) = (self.undo.can_undo(), self.undo.can_redo());
            ui.vertical(|ui| {
                ui.add_enabled_ui(cu, |ui| {
                    if ui_kit::small(ui, Icon::Undo, "撤销（Ctrl+Z）").clicked() {
                        self.do_undo(false);
                    }
                });
                ui.add_enabled_ui(cr, |ui| {
                    if ui_kit::small(ui, Icon::Redo, "重做（Ctrl+Y）").clicked() {
                        self.do_undo(true);
                    }
                });
            });
        });

        ui_kit::group(ui, "标签", |ui| {
            ui.vertical(|ui| {
                if ui_kit::pill(ui, "新建标签", "开一个空白工作簿").clicked() {
                    self.open_in_new_tab(PathBuf::new());
                }
                if ui_kit::pill(ui, "打开到新标签…", "").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Excel 工作簿", &["xlsx", "xlsm", "csv"])
                        .pick_file()
                    {
                        self.open_in_new_tab(p);
                    }
                }
                if ui_kit::pill(ui, "关闭当前标签", "").clicked() {
                    let idx = self.active_tab;
                    self.close_tab(idx);
                }
            });
        });

        ui_kit::group(ui, "协作", |ui| {
            if self.net.is_none() {
                if ui_kit::big(ui, Icon::Share, "共享").on_hover_text("把当前文件共享到局域网").clicked() {
                    self.start_host();
                }
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.join_addr)
                            .hint_text("192.168.1.20:7878")
                            .desired_width(132.0),
                    );
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.room_password)
                                .hint_text("口令")
                                .desired_width(64.0)
                                .password(true),
                        );
                        if ui_kit::pill(ui, "加入", "加入同事共享的房间").clicked() {
                            self.join_shared();
                        }
                    });
                });
            } else {
                let online = ui_kit::big_sized(
                    ui,
                    Icon::Share,
                    &format!("在线 {}", self.peers_list.len()),
                    Vec2::new(66.0, 50.0),
                );
                if online.clicked() {
                    self.members_open = true;
                }
                if self.room_lost {
                    if ui_kit::pill(ui, "接任宿主", "宿主已离线：用我手里这份继续开房间").clicked() {
                        self.promote_to_host();
                    }
                }
            }
        });

        ui_kit::group(ui, "更多", |ui| {
            ui.vertical(|ui| {
                if ui_kit::pill(ui, "检查更新…", "").clicked() {
                    self.check_update();
                }
                if ui_kit::pill(ui, "加入“打开方式”", "注册 .xlsx/.xlsm（当前用户）").clicked() {
                    self.register_file_association(false);
                }
                if ui_kit::pill(ui, "设为默认打开方式", "").clicked() {
                    self.register_file_association(true);
                }
            });
        });
    }

    fn ribbon_home(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let cur = self.current_style();

        ui_kit::group(ui, "剪贴板", |ui| {
            if ui_kit::big(ui, Icon::Paste, "粘贴").clicked() {
                self.clip_paste(false, None);
            }
            ui.vertical(|ui| {
                if ui_kit::small(ui, Icon::Cut, "剪切（Ctrl+X）").clicked() {
                    self.clip_copy(true, &ctx);
                }
                if ui_kit::small(ui, Icon::Copy, "复制（Ctrl+C）").clicked() {
                    self.clip_copy(false, &ctx);
                }
            });
        });

        ui_kit::group(ui, "字体", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("font_pick")
                        .selected_text(if cur.font.is_empty() {
                            "默认".to_string()
                        } else {
                            cur.font.clone()
                        })
                        .width(112.0)
                        .show_ui(ui, |ui| {
                            for name in sheetcore::style::FONT_CHOICES {
                                let label = if name.is_empty() { "默认" } else { *name };
                                let preview = egui::RichText::new(label);
                                let preview = match sheetcore::style::font_class(name) {
                                    sheetcore::style::FontClass::Serif => preview.family(
                                        egui::FontFamily::Name("cjk-serif".into()),
                                    ),
                                    sheetcore::style::FontClass::Mono => preview
                                        .family(egui::FontFamily::Name("mono".into())),
                                    _ => preview,
                                };
                                if ui.selectable_label(cur.font == *name, preview).clicked() {
                                    let pick = name.to_string();
                                    self.apply_style(move |s| s.font = pick.clone());
                                }
                            }
                        });
                    let mut size = cur.size;
                    let w = ui.add_sized(
                        [40.0, 20.0],
                        egui::DragValue::new(&mut size)
                            .speed(0.5)
                            .range(6.0..=72.0)
                            .fixed_decimals(0),
                    );
                    if w.changed() {
                        self.apply_style(move |s| s.size = size);
                    }
                });
                ui.horizontal(|ui| {
                    if ui_kit::text_toggle(ui, "B", cur.bold, "加粗（Ctrl+B）", true, false).clicked() {
                        let v = !cur.bold;
                        self.apply_style(move |s| s.bold = v);
                    }
                    if ui_kit::text_toggle(ui, "I", cur.italic, "斜体（Ctrl+I）", false, true).clicked() {
                        let v = !cur.italic;
                        self.apply_style(move |s| s.italic = v);
                    }
                    ui.add_space(2.0);
                    let mut fg = cur.color;
                    if ui
                        .color_edit_button_srgb(&mut fg)
                        .on_hover_text("字体颜色")
                        .changed()
                    {
                        self.apply_style(move |s| s.color = fg);
                    }
                    let mut bg = cur.fill.unwrap_or([255, 255, 255]);
                    if ui
                        .color_edit_button_srgb(&mut bg)
                        .on_hover_text("填充颜色")
                        .changed()
                    {
                        self.apply_style(move |s| s.fill = Some(bg));
                    }
                    if ui_kit::small(ui, Icon::Fill, "清除填充").clicked() {
                        self.apply_style(|s| s.fill = None);
                    }
                });
            });
        });

        ui_kit::group(ui, "对齐", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    for (ic, val, tip) in [
                        (Icon::AlignL, 1u8, "左对齐"),
                        (Icon::AlignC, 2, "居中"),
                        (Icon::AlignR, 3, "右对齐"),
                    ] {
                        if ui_kit::small_on(ui, ic, cur.halign == val, tip).clicked() {
                            self.apply_style(move |s| s.halign = val);
                        }
                    }
                    if ui_kit::small_on(ui, Icon::Wrap, cur.wrap, "自动换行").clicked() {
                        let v = !cur.wrap;
                        self.apply_style(move |s| s.wrap = v);
                    }
                });
                ui.horizontal(|ui| {
                    for (ic, val, tip) in [
                        (Icon::Top, 1u8, "顶端对齐"),
                        (Icon::Middle, 2, "垂直居中"),
                        (Icon::Bottom, 3, "底端对齐"),
                    ] {
                        if ui_kit::small_on(ui, ic, cur.valign == val, tip).clicked() {
                            self.apply_style(move |s| s.valign = val);
                        }
                    }
                    let (r1, c1, r2, c2) = self.sel_rect();
                    let merged = self
                        .wb
                        .sheet(self.sheet_idx)
                        .merges
                        .iter()
                        .any(|m| m.0 == r1 && m.1 == c1 && m.2 == r2 && m.3 == c2);
                    if ui_kit::small_on(ui, Icon::Merge, merged, "合并/取消合并").clicked() {
                        self.toggle_merge(!merged);
                    }
                });
            });
        });

        ui_kit::group(ui, "数字", |ui| {
            ui.vertical(|ui| {
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
                    .width(86.0)
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
                ui.horizontal(|ui| {
                    if ui_kit::small(ui, Icon::Currency, "货币格式").clicked() {
                        self.apply_style(|s| s.format = "¥#,##0.00".into());
                    }
                    if ui_kit::small(ui, Icon::Percent, "百分比格式").clicked() {
                        self.apply_style(|s| s.format = "0.0%".into());
                    }
                    if ui_kit::small(ui, Icon::Comma, "千分位").clicked() {
                        self.apply_style(|s| s.format = "#,##0.00".into());
                    }
                });
            });
        });

        ui_kit::group(ui, "样式", |ui| {
            if ui_kit::big(ui, Icon::Border, "全部框线").clicked() {
                self.apply_style(|s| s.set_all_borders(1, [128, 128, 128]));
            }
            ui.vertical(|ui| {
                ui.menu_button("外框", |ui| {
                    if ui.button("细外框").clicked() {
                        self.outer_border(1, [0, 0, 0]);
                        ui.close_menu();
                    }
                    if ui.button("粗外框").clicked() {
                        self.outer_border(3, [0, 0, 0]);
                        ui.close_menu();
                    }
                });
                if ui_kit::pill(ui, "无边框", "").clicked() {
                    self.apply_style(|s| s.clear_borders());
                }
            });
        });

        ui_kit::group(ui, "单元格", |ui| {
            ui.vertical(|ui| {
                let (r1, _c1, r2, _c2) = self.sel_rect();
                let cc = self.sel.1;
                ui.horizontal(|ui| {
                    if ui_kit::pill(ui, "插入行", "").clicked() {
                        self.structural(sheetcore::structural::Structural::InsertRows {
                            at: r1,
                            n: r2 - r1 + 1,
                        });
                    }
                    if ui_kit::pill(ui, "删除行", "").clicked() {
                        self.structural(sheetcore::structural::Structural::DeleteRows {
                            at: r1,
                            n: r2 - r1 + 1,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui_kit::pill(ui, "插入列", "").clicked() {
                        self.structural(sheetcore::structural::Structural::InsertCols { at: cc, n: 1 });
                    }
                    if ui_kit::pill(ui, "删除列", "").clicked() {
                        self.structural(sheetcore::structural::Structural::DeleteCols { at: cc, n: 1 });
                    }
                });
            });
        });

        ui_kit::group(ui, "编辑", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if ui_kit::small(ui, Icon::Find, "查找与替换（Ctrl+F）").clicked() {
                        self.find.open = true;
                    }
                    if ui_kit::small(ui, Icon::Filter, "筛选").clicked() {
                        self.start_filter();
                    }
                });
                ui.horizontal(|ui| {
                    if ui_kit::pill(ui, "升序", "").clicked() {
                        let c = self.sel.1;
                        self.do_sort(c, true);
                    }
                    if ui_kit::pill(ui, "降序", "").clicked() {
                        let c = self.sel.1;
                        self.do_sort(c, false);
                    }
                });
            });
        });
    }

    fn ribbon_insert(&mut self, ui: &mut egui::Ui) {
        ui_kit::group(ui, "图表", |ui| {
            if ui_kit::big(ui, Icon::Chart, "柱状图").on_hover_text("按选区插一张柱状图").clicked() {
                self.insert_chart_from_selection();
            }
            ui.vertical(|ui| {
                if ui_kit::pill(ui, "图表管理…", "").clicked() {
                    self.chart_mgr = true;
                    self.reload_charts();
                }
                if ui_kit::pill(ui, "透视表…", "把选区做成数据透视表").clicked() {
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
                }
            });
        });

        ui_kit::group(ui, "插图", |ui| {
            if ui_kit::big(ui, Icon::Image, "图片").clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("图片", &["png", "jpg", "jpeg", "gif", "bmp"])
                    .pick_file()
                {
                    match (self.wb.path.clone(), std::fs::read(&p)) {
                        (Some(out), Ok(bytes)) => {
                            let ext = p
                                .extension()
                                .map(|e| e.to_string_lossy().to_string())
                                .unwrap_or_else(|| "png".into());
                            let sheet_name = self.wb.sheet(self.sheet_idx).name.clone();
                            let (r, c) = self.sel;
                            match sheetcore::pic_write::insert_picture(
                                &out, &sheet_name, &bytes, &ext, c, r,
                            ) {
                                Ok(_) => {
                                    if let Some(list) =
                                        sheetcore::pic::load(&out).remove(&sheet_name)
                                    {
                                        self.wb.sheet_mut(self.sheet_idx).pictures = list;
                                    }
                                    self.status = "已插入图片".into();
                                }
                                Err(e) => self.status = format!("插入图片失败：{e:#}"),
                            }
                        }
                        (None, _) => self.status = "请先保存文件，再插入图片".into(),
                        (_, Err(e)) => self.status = format!("读图片失败：{e}"),
                    }
                }
            }
        });

        ui_kit::group(ui, "表格", |ui| {
            if ui_kit::big(ui, Icon::Table, "表格").on_hover_text("把选区转成表格对象").clicked() {
                self.make_table();
            }
            if ui_kit::big(ui, Icon::Comment, "批注").clicked() {
                let (r, c) = self.sel;
                let cur = self
                    .wb
                    .sheet(self.sheet_idx)
                    .notes
                    .get(&sheetcore::model::CellRef::new(r, c))
                    .map(|n| n.text.clone())
                    .unwrap_or_default();
                self.note_edit = Some((r, c, cur));
            }
        });
    }

    fn ribbon_formula(&mut self, ui: &mut egui::Ui) {
        ui_kit::group(ui, "计算", |ui| {
            if ui_kit::big(ui, Icon::Calc, "重算全部").clicked() {
                sheetcore::formula::recalc_all(&mut self.wb);
                self.status = "已重算".to_string();
            }
        });
        ui_kit::group(ui, "查找", |ui| {
            if ui_kit::big(ui, Icon::Find, "查找与替换").clicked() {
                self.find.open = true;
            }
        });
        ui_kit::group(ui, "区域", |ui| {
            ui.vertical(|ui| {
                let (r1, c1, r2, c2) = self.sel_rect();
                ui.label(
                    egui::RichText::new(format!(
                        "{}:{}",
                        cell_name(r1, c1),
                        cell_name(r2, c2)
                    ))
                    .monospace()
                    .size(11.5),
                );
                ui.label(
                    egui::RichText::new("fx 里显示公式，选中外部引用会标出文件名")
                        .size(10.0)
                        .weak(),
                );
            });
        });
    }

    fn ribbon_data(&mut self, ui: &mut egui::Ui) {
        ui_kit::group(ui, "格式", |ui| {
            if ui_kit::big(ui, Icon::Filter, "条件格式").clicked() {
                self.cf_mgr = true;
            }
        });
        ui_kit::group(ui, "链接", |ui| {
            let links = self.wb.links.len();
            let label = if links > 0 {
                format!("链接 {links}")
            } else {
                "链接".to_string()
            };
            if ui_kit::big(ui, Icon::Link, &label)
                .on_hover_text("跨工作簿引用：查看、更新缓存")
                .clicked()
            {
                self.xlink_dlg = true;
            }
        });
        ui_kit::group(ui, "排序与筛选", |ui| {
            ui.vertical(|ui| {
                if ui_kit::pill(ui, "多级排序…", "").clicked() {
                    self.open_data_dlg(0);
                }
                if ui_kit::pill(ui, "筛选", "").clicked() {
                    self.start_filter();
                }
                if ui_kit::pill(ui, "删除重复…", "").clicked() {
                    self.run_dedup();
                }
            });
        });
        ui_kit::group(ui, "获取数据", |ui| {
            ui.vertical(|ui| {
                if ui_kit::pill(ui, "导入 CSV/TSV…", "").clicked() {
                    self.import_csv();
                }
                if ui_kit::pill(ui, "从另一张表取数…", "").clicked() {
                    self.link_dlg = Some(String::new());
                }
                if ui_kit::pill(ui, "分列…", "").clicked() {
                    self.open_data_dlg(1);
                }
                if ui_kit::pill(ui, "刷新导入", "").clicked() {
                    self.refresh_import();
                }
            });
        });
    }

    fn ribbon_view(&mut self, ui: &mut egui::Ui) {
        ui_kit::group(ui, "显示", |ui| {
            if ui_kit::big(ui, Icon::Print, "打印预览").clicked() {
                self.preview_page = 0;
                self.print_preview = true;
            }
            ui.vertical(|ui| {
                if ui_kit::pill(ui, "100%", "回到实际大小").clicked() {
                    self.zoom = 1.0;
                    ui.ctx().set_zoom_factor(1.0);
                }
                let on = self.tree_open;
                if ui_kit::pill(ui, if on { "隐藏文件列表" } else { "显示文件列表" }, "左侧文件树")
                    .clicked()
                {
                    self.tree_open = !on;
                }
            });
        });
        ui_kit::group(ui, "缩放", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if ui_kit::small(ui, Icon::Zoom, "放大").clicked() {
                        self.zoom = (self.zoom + 0.1).min(2.5);
                        ui.ctx().set_zoom_factor(self.zoom);
                    }
                    ui.label(format!("{:.0}%", self.zoom * 100.0));
                });
                let mut z = self.zoom;
                if ui
                    .add(egui::Slider::new(&mut z, 0.5..=2.5).show_value(false))
                    .changed()
                {
                    self.zoom = z;
                    ui.ctx().set_zoom_factor(z);
                }
            });
        });
        ui_kit::group(ui, "修订", |ui| {
            if ui_kit::big(ui, Icon::Revision, "修订记录").clicked() {
                self.rev_open = true;
            }
        });
    }



    /// 取（或新建）一个 dxf 索引；所有表保持同一份 dxfs，保证索引一致
    fn cf_dxf(&mut self, bold: bool, color: Option<[u8; 3]>, fill: Option<[u8; 3]>) -> usize {
        let want = sheetcore::cf::Dxf { bold, italic: false, color, fill, font: None };
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
        ui_kit::window(ctx, "数据验证", 420.0)
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
            if ui.button("修订记录…").on_hover_text("谁在什么时候改了哪格，可撤回").clicked() {
                self.rev_open = true;
                ui.close_menu();
            }
            ui.separator();
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
                .on_hover_text("只读展示 VBA 源码")
                .clicked()
            {
                self.open_macro_view();
                ui.close_menu();
            }
            if ui
                .add_enabled(self.wb.has_vba, egui::Button::new("运行宏…"))
                .on_hover_text("交给本机 Excel 执行（需要装了 Excel），跑完结果存回文件")
                .clicked()
            {
                self.macro_run_dlg = Some(String::new());
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
        ui_kit::window(ctx, name, 280.0)
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
        // 边框：在填充之上、文字之下。每格画自己四条边，相邻格共享边重画一次也无妨。
        for (i, b) in st.border.iter().enumerate() {
            if b.is_none() {
                continue;
            }
            let col = Color32::from_rgb(b.color[0], b.color[1], b.color[2]);
            let (a, z) = match i {
                0 => (rect.left_top(), rect.left_bottom()),
                1 => (rect.right_top(), rect.right_bottom()),
                2 => (rect.left_top(), rect.right_top()),
                _ => (rect.left_bottom(), rect.right_bottom()),
            };
            painter.line_segment([a, z], Stroke::new(b.px(), col));
            if dumping {
                dump.push(format!("border ({},{}) i={} w={:.1} rgb={:?}", r, c, i, b.px(), b.color));
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
                // 条件格式里指定的字体优先，其次单元格样式；未知字体回退默认
                let font_name = ov.and_then(|o| o.font.as_deref()).unwrap_or(st.font.as_str());
                let family = cell_family(font_name);
                let font = FontId::new(
                    if st.bold || cf_bold {
                        st.size + 0.6
                    } else {
                        st.size
                    },
                    family.clone(),
                );
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
                if dumping {
                    // 同一个字符串在不同 family 下的排版宽度不同：这是「真的换了字体数据，
                    // 不只是换了个标签」的直接证据
                    let w = painter
                        .layout_no_wrap(txt.clone(), font.clone(), color)
                        .size()
                        .x;
                    dump.push(format!(
                        "text ({},{}) {:?} at=({:.0},{:.0}) rgb={:?} size={:.1} bold={} font={:?} family={} w={:.1}",
                        r,
                        c,
                        txt,
                        tx,
                        ty,
                        base_color,
                        st.size,
                        st.bold,
                        font_name,
                        sheetcore::style::font_class(font_name).key(),
                        w
                    ));
                }
                // 多行/自动换行要按单元格宽度排版并裁到格子里，
                // 否则长地址会溢出到右边的发票号上（Excel 会裁掉）。
                let multiline = st.wrap || txt.contains("
");
                if multiline {
                    let max_w = (rect.width() - 8.0).max(8.0);
                    let galley = painter.layout(txt.clone(), font.clone(), color, max_w);
                    let h = galley.size().y;
                    let top = if st.valign == 1 {
                        rect.min.y + 3.0
                    } else if st.valign == 3 {
                        (rect.max.y - 3.0 - h).max(rect.min.y + 2.0)
                    } else {
                        (rect.center().y - h / 2.0).max(rect.min.y + 2.0)
                    };
                    let x = match align {
                        Align2::CENTER_CENTER => rect.center().x - galley.size().x / 2.0,
                        Align2::RIGHT_CENTER => rect.max.x - 4.0 - galley.size().x,
                        _ => rect.min.x + 4.0,
                    };
                    let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
                    clipped.galley(egui::pos2(x, top), galley, color);
                } else {
                    painter.text(egui::pos2(tx, ty), align, txt.clone(), font, color);
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



    /// 从另一张表取数：输入 "表名!A1:C10"，结果贴到当前选区左上角，
    /// 并登记成可刷新的数据源
    fn link_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut text) = self.link_dlg.take() else { return };
        let mut open = true;
        let mut apply = false;
        ui_kit::window(ctx, "从另一张表取数", 320.0)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("格式：表名!A1:C10");
                ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .desired_width(f32::INFINITY)
                        .hint_text("销售!A1:C100"),
                );
                ui.horizontal(|ui| {
                    if ui.button("确定").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        ui.close_menu();
                    }
                });
            });
        if apply {
            let spec = text.trim().to_string();
            if let Some((sheet_name, range)) = spec.split_once('!') {
                if let (Some(r1c1), Some(r2c2)) = (
                    sheetcore::sparse::parse_ref(range.split(':').next().unwrap_or("A1")),
                    sheetcore::sparse::parse_ref(range.split(':').nth(1).unwrap_or(range)),
                ) {
                    let rect = (r1c1.0, r1c1.1, r2c2.0, r2c2.1);
                    let at = self.sel;
                    let n = self.wb.sources.len() + 1;
                    let src = sheetcore::external::Source {
                        name: format!("_sc_src_{n}"),
                        kind: sheetcore::external::Kind::Internal {
                            sheet: sheet_name.trim().to_string(),
                            rect,
                        },
                        at,
                        rows: 0,
                        cols: 0,
                    };
                    let idx = self.sheet_idx;
                    match sheetcore::external::refresh(&mut self.wb, idx, &src) {
                        Ok((rows, cols)) => {
                            self.wb.sources.push(sheetcore::external::Source {
                                rows,
                                cols,
                                ..src
                            });
                            self.wb.dirty = true;
                            self.doc_gen = self.doc_gen.wrapping_add(1);
                            self.status = format!(
                                "已从 {} 取数 {} 行 × {} 列（可刷新）",
                                sheet_name.trim(), rows, cols
                            );
                        }
                        Err(e) => self.status = format!("取数失败：{e}"),
                    }
                } else {
                    self.status = "区间写法看不懂，像 A1:C10".into();
                }
            } else {
                self.status = "格式要写成 表名!A1:C10".into();
            }
            return;
        }
        if open && !apply {
            self.link_dlg = Some(text);
        }
    }


    /// 用本机 Excel 跑宏。
    ///
    /// 我们不做 VBA 解释器（那是一个编译器的工程量），但用户的宏本来就在 Excel 里
    /// 能跑：这里把文件交给本机 Excel 的 COM 接口执行，跑完把结果存回同一个文件。
    fn run_macro_via_excel(&mut self, macro_name: &str) {
        let Some(path) = self.wb.path.clone() else {
            self.status = "先保存文件，再运行宏".into();
            return;
        };
        if macro_name.trim().is_empty() {
            self.status = "宏名不能为空（形如 Module1.MyMacro）".into();
            return;
        }
        let ps = format!(
            "$ErrorActionPreference='Stop'; $xl=New-Object -ComObject Excel.Application; \
             $xl.Visible=$false; $xl.DisplayAlerts=$false; \
             try {{ $wb=$xl.Workbooks.Open('{p}'); $xl.Run('{m}'); $wb.Save(); $wb.Close($false); \
             Write-Output 'OK' }} finally {{ $xl.Quit() }}",
            p = path.to_string_lossy().replace('\'', "''"),
            m = macro_name.replace('\'', "''")
        );
        match std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if text.contains("OK") {
                    self.status = format!("宏 {macro_name} 执行完毕");
                    if let Ok(fresh) = sheetcore::xlsx::load(&path) {
                        self.wb = fresh;
                        self.doc_gen = self.doc_gen.wrapping_add(1);
                    }
                } else {
                    let err = String::from_utf8_lossy(&out.stderr).to_string();
                    let low = err.to_lowercase();
                    // COM 起不来通常是没装 Excel：给一句能懂的话，别丢一串 HRESULT
                    let no_excel = low.contains("80040154")
                        || low.contains("class not registered")
                        || err.contains("无法创建")
                        || err.contains("ActiveX");
                    self.status = if no_excel {
                        "这台机器没有装 Excel，宏没法运行（可以把宏代码导出到装了 Excel 的机器上跑）".into()
                    } else {
                        format!("宏执行失败：{}", err.lines().next().unwrap_or("未知错误"))
                    };
                }
            }
            Err(e) => self.status = format!("调不起 PowerShell：{e}"),
        }
    }

    /// 运行宏的输入框
    fn macro_run_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut text) = self.macro_run_dlg.take() else { return };
        let mut open = true;
        let mut apply = false;
        ui_kit::window(ctx, "运行宏（借助本机 Excel）", 320.0)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("填宏名，例如 Module1.MyMacro");
                ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .desired_width(f32::INFINITY)
                        .hint_text("Module1.MyMacro"),
                );
                ui.horizontal(|ui| {
                    if ui.button("运行").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        ui.close_menu();
                    }
                });
            });
        if apply {
            self.run_macro_via_excel(&text);
            return;
        }
        if open {
            self.macro_run_dlg = Some(text);
        }
    }


    /// 自检：用本机 Excel 跑一个宏
    fn macro_run_probe(&mut self) {
        let Ok(name) = std::env::var("SC_RUNMACRO") else { return };
        if self.open_rx.is_some() || self.macro_ran {
            return;
        }
        self.macro_ran = true;
        self.run_macro_via_excel(&name);
        eprintln!("[MACRO] {}", self.status);
        if let Some(p) = self.wb.path.clone() {
            if let Ok(back) = sheetcore::xlsx::load(&p) {
                eprintln!(
                    "[MACRO] A1={}",
                    back.sheet(0).get(0, 0).map(|c| c.value.display()).unwrap_or_default()
                );
            }
        }
    }


    /// 把导入的 CSV 挂成 Excel 原生查询表（数据 → 查询和连接 里能看到）
    fn attach_query_tables(&mut self) {
        let Some(path) = self.wb.path.clone() else {
            self.status = "先保存文件，才能挂查询表".into();
            return;
        };
        let mut ok = 0;
        let mut fail = 0;
        for src in self.wb.sources.clone() {
            if let sheetcore::external::Kind::Csv { path: csv, header, delim } = &src.kind {
                let real = sheetcore::external::resolve_path(&self.wb, csv);
                if sheetcore::qtable::attach_csv(
                    &path,
                    &self.wb.sheet(self.sheet_idx).name,
                    src.at,
                    src.rows.max(1),
                    src.cols.max(1),
                    &real,
                    *header,
                    *delim,
                )
                .is_ok()
                {
                    ok += 1;
                } else {
                    fail += 1;
                }
            }
        }
        self.status = if fail == 0 {
            format!("已挂 {ok} 个 Excel 查询表（数据 → 查询和连接）")
        } else {
            format!("挂查询表：成功 {ok}，失败 {fail}")
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
        ui_kit::window(ctx, format!("筛选 {} 列", cell_name(0, col)), 220.0)
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
        self.chart_src = sheetcore::table::a1((r1, c1, r2, c2));
        self.chart_edit = Some(ch);
    }

    /// 真的写文件
    fn commit_chart(&mut self, ch: sheetcore::charts::Chart) {
        let Some(path) = self.wb.path.clone() else {
            self.status = "请先保存文件再插入图表".into();
            return;
        };
        let name = self.wb.sheet(self.sheet_idx).name.clone();
        if let Err(e) = sheetcore::xlsx::save_atomic(&path, &self.wb) {
            self.status = format!("保存失败：{e}");
            return;
        }
        // 已有部件就替换，否则新建（改图表不能变成又插一个）
        let res = if ch.part.is_empty() {
            sheetcore::chart_write::insert_chart(&path, &name, &ch)
        } else {
            sheetcore::chart_write::replace_chart(&path, &ch.part, &ch)
        };
        match res {
            Ok(()) => {
                self.reload_charts();
                self.status = if ch.part.is_empty() {
                    format!("已插入{}（{} 个系列）", ch.kind.label(), ch.series.len())
                } else {
                    format!("已更新{}（{} 个系列）", ch.kind.label(), ch.series.len())
                };
            }
            Err(e) => self.status = format!("写图表失败：{e}"),
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
    /// 修订记录：谁、什么时候、改了哪格；点一条能跳过去或者撤回
    fn revision_panel(&mut self, ctx: &egui::Context) {
        if !self.rev_open {
            return;
        }
        let mut open = self.rev_open;
        let mut jump: Option<(String, u32, u32)> = None;
        let mut revert: Option<usize> = None;
        let mut reload = false;
        let entries = self.rev.clone();
        ui_kit::window(ctx, "修订记录", 720.0)
            .open(&mut open)
            .resizable(true)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -30.0])
            .default_width(720.0)
            .min_width(560.0)
            .frame(
                egui::Frame::window(&ctx.style()).inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 8,
                    bottom: 10,
                }),
            )
            .show(ctx, |ui| {
                ui.set_width(700.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{} 条", entries.len())).strong());
                    if ui.small_button("重新读取").clicked() {
                        reload = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(".~sheetflow-history/<文件名>/revisions.jsonl")
                                .size(10.0)
                                .weak(),
                        );
                    });
                });
                ui.add_space(4.0);
                // 表头
                ui.horizontal(|ui| {
                    dlg_col(ui, 62.0, |ui| {
                        ui.label(egui::RichText::new("时间").size(10.5).weak());
                    });
                    dlg_col(ui, 92.0, |ui| {
                        ui.label(egui::RichText::new("用户").size(10.5).weak());
                    });
                    dlg_col(ui, 76.0, |ui| {
                        ui.label(egui::RichText::new("动作").size(10.5).weak());
                    });
                    dlg_col(ui, 132.0, |ui| {
                        ui.label(egui::RichText::new("位置").size(10.5).weak());
                    });
                    dlg_col(ui, 240.0, |ui| {
                        ui.label(egui::RichText::new("改动").size(10.5).weak());
                    });
                });
                ui.separator();
                if entries.is_empty() {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("还没有记录。改几格再回来看。").weak());
                    ui.add_space(10.0);
                }
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, e) in entries.iter().enumerate().rev() {
                            ui.horizontal(|ui| {
                                dlg_col(ui, 62.0, |ui| {
                                    ui.label(
                                        egui::RichText::new(rel_time(e.ts)).size(11.0).weak(),
                                    );
                                });
                                dlg_col(ui, 92.0, |ui| {
                                    ui.label(egui::RichText::new(&e.who).strong().size(12.0));
                                });
                                dlg_col(ui, 76.0, |ui| {
                                    ui.label(egui::RichText::new(&e.label).size(12.0));
                                });
                                dlg_col(ui, 132.0, |ui| {
                                    let pos = format!("{}!{}", e.sheet, e.range());
                                    if ui.link(pos).clicked() {
                                        if let Some(ch) = e.cells.first() {
                                            jump = Some((e.sheet.clone(), ch.r, ch.c));
                                        }
                                    }
                                });
                                dlg_col(ui, 240.0, |ui| {
                                    ui.label(
                                        egui::RichText::new(e.delta_text())
                                            .size(11.0)
                                            .weak(),
                                    );
                                });
                                if !e.remote && !e.cells.is_empty() {
                                    if ui.small_button("撤回").clicked() {
                                        revert = Some(i);
                                    }
                                }
                            });
                            ui.separator();
                        }
                    });
            });
        if reload {
            if let Some(p) = self.wb.path.clone() {
                self.rev = sheetcore::revision::load(&p, 300);
            }
        }
        if let Some((sheet, r, c)) = jump {
            if let Some(si) = self.wb.sheets.iter().position(|s| s.name == sheet) {
                self.sheet_idx = si;
                self.sel = (r, c);
                self.sel_end = (r, c);
                self.ensure_visible();
                self.doc_gen = self.doc_gen.wrapping_add(1);
            }
        }
        if let Some(i) = revert {
            self.revert_revision(i);
        }
        if !open {
            self.rev_open = false;
        }
    }

    /// 撤回一条本地修订：先确认这些格子还是它改成的样子，再按 before 写回
    fn revert_revision(&mut self, idx: usize) {
        let Some(e) = self.rev.get(idx).cloned() else {
            return;
        };
        let Some(si) = self.wb.sheets.iter().position(|s| s.name == e.sheet) else {
            self.status = "那张工作表已经不在了".into();
            return;
        };
        if e.cells.is_empty() {
            self.status = "这条没有可撤回的格子".into();
            return;
        }
        let conflict = e
            .cells
            .iter()
            .filter(|ch| !ch.after.same(self.wb.sheet(si).get(ch.r, ch.c)))
            .count();
        if conflict > 0 {
            self.status = format!("有 {conflict} 格之后又被改过，不能整条撤回");
            return;
        }
        self.sheet_idx = si;
        let keys: Vec<(u32, u32)> = e.cells.iter().map(|c| (c.r, c.c)).collect();
        let txn = self.txn_begin(&keys);
        for ch in &e.cells {
            self.wb.sheet_mut(si).set(ch.r, ch.c, ch.before.to_cell());
            if let Some(st) = ch.before_style.clone() {
                let id = self.wb.styles.intern(st);
                self.wb.sheet_mut(si).set_style(ch.r, ch.c, id);
            }
        }
        self.txn_end("撤回修订", txn);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        let fkeys: Vec<(usize, u32, u32)> = keys.iter().map(|(r, c)| (si, *r, *c)).collect();
        for k in &fkeys {
            self.deps.update_cell(&self.wb, *k);
        }
        sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &fkeys);
        self.status = format!("已撤回 {} 的「{}」", e.who, e.label);
    }

    /// 外部链接：看看有哪些、缓存多少格、需不需要更新
    fn xlink_dialog(&mut self, ctx: &egui::Context) {
        if !self.xlink_dlg {
            return;
        }
        let mut open = self.xlink_dlg;
        let mut do_refresh = false;
        let mut forget: Option<usize> = None;
        ui_kit::window(ctx, "外部链接", 460.0)
            .open(&mut open)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                if self.wb.links.is_empty() {
                    ui.label("这个工作簿没有跨工作簿引用。");
                    ui.weak("在单元格里写 =[预算.xlsx]Sheet1!A1 会自动登记成链接。");
                } else {
                    egui::Grid::new("xlink_grid")
                        .striped(true)
                        .num_columns(4)
                        .show(ui, |ui| {
                            ui.strong("文件");
                            ui.strong("表");
                            ui.strong("缓存");
                            ui.strong("");
                            ui.end_row();
                            for (i, l) in self.wb.links.iter().enumerate() {
                                ui.label(sheetcore::xlinks::label(l))
                                    .on_hover_text(if l.target.is_empty() {
                                        l.abs.clone().unwrap_or_default()
                                    } else {
                                        l.target.clone()
                                    });
                                ui.label(if l.sheets.is_empty() {
                                    "—".to_string()
                                } else {
                                    l.sheets.join("、")
                                });
                                ui.label(format!("{} 格", l.cache.len()));
                                if ui.small_button("断开").clicked() {
                                    forget = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button("全部更新")
                        .on_hover_text("按相对路径（其次绝对路径）重读目标文件，并重算")
                        .clicked()
                    {
                        do_refresh = true;
                    }
                    if ui.button("关闭").clicked() {
                        self.xlink_dlg = false;
                    }
                });
                ui.weak("更新后保存，缓存值会写回文件；别人打开时即使不更新链接也看到新数字。");
            });
        if let Some(i) = forget {
            if i < self.wb.links.len() {
                let name = sheetcore::xlinks::label(&self.wb.links[i]);
                self.wb.links.remove(i);
                for (k, l) in self.wb.links.iter_mut().enumerate() {
                    l.index = k + 1;
                }
                self.status = format!("已断开 {name}（公式保留，取不到值会显示 #REF!）");
            }
        }
        if do_refresh {
            let rep = sheetcore::xlinks::refresh(&mut self.wb);
            sheetcore::formula::recalc_all(&mut self.wb);
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.wb.dirty = true;
            self.status = if rep.failed.is_empty() {
                format!("已更新 {} 个外部链接", rep.ok)
            } else {
                format!(
                    "更新 {} 个，失败 {} 个：{}",
                    rep.ok,
                    rep.failed.len(),
                    rep.failed.iter().map(|(n, e)| format!("{n}（{e}）")).collect::<Vec<_>>().join("，")
                )
            };
        }
        if !open {
            self.xlink_dlg = false;
        }
    }

    fn format_dialog(&mut self, ctx: &egui::Context) {
        if !self.fmt_dlg {
            return;
        }
        let mut open = self.fmt_dlg;
        let mut apply = false;
        let mut cancel = false;
        let st = self.fmt_state.clone();
        ui_kit::window(ctx, "设置单元格格式", 420.0)
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -20.0])
            .frame(
                egui::Frame::window(&ctx.style()).inner_margin(egui::Margin {
                    left: 16,
                    right: 16,
                    top: 10,
                    bottom: 12,
                }),
            )
            .show(ctx, |ui| {
                ui.set_width(452.0);
                ui.spacing_mut().item_spacing = egui::Vec2::new(8.0, 7.0);
                ui.spacing_mut().button_padding = egui::Vec2::new(10.0, 4.0);

                dlg_section(ui, "字体");
                egui::Grid::new("fmt_g_font")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        dlg_cell_label(ui, "字体");
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("fmt_dlg_font")
                                .selected_text(if self.fmt_state.font.is_empty() {
                                    "默认".to_string()
                                } else {
                                    self.fmt_state.font.clone()
                                })
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for name in sheetcore::style::FONT_CHOICES {
                                        let label = if name.is_empty() { "默认" } else { *name };
                                        if ui
                                            .selectable_label(
                                                self.fmt_state.font == *name,
                                                label,
                                            )
                                            .clicked()
                                        {
                                            self.fmt_state.font = name.to_string();
                                        }
                                    }
                                });
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("字号").size(11.5).weak());
                            ui.add_sized(
                                [46.0, 22.0],
                                egui::DragValue::new(&mut self.fmt_state.size)
                                    .range(6.0..=72.0)
                                    .fixed_decimals(0),
                            );
                        });
                        ui.end_row();

                        dlg_cell_label(ui, "字形");
                        ui.horizontal(|ui| {
                            if ui_kit::text_toggle(
                                ui,
                                "B",
                                self.fmt_state.bold,
                                "粗体",
                                true,
                                false,
                            )
                            .clicked()
                            {
                                self.fmt_state.bold = !self.fmt_state.bold;
                            }
                            if ui_kit::text_toggle(
                                ui,
                                "I",
                                self.fmt_state.italic,
                                "斜体",
                                false,
                                true,
                            )
                            .clicked()
                            {
                                self.fmt_state.italic = !self.fmt_state.italic;
                            }
                            ui.add_space(10.0);
                            ui.label(egui::RichText::new("颜色").size(11.5).weak());
                            ui.color_edit_button_srgb(&mut self.fmt_state.color)
                                .on_hover_text("字体颜色");
                            ui.add_space(10.0);
                            let mut has_fill = self.fmt_state.fill.is_some();
                            if ui.checkbox(&mut has_fill, "填充").changed() {
                                self.fmt_state.fill =
                                    if has_fill { Some([255, 255, 0]) } else { None };
                            }
                            if let Some(f) = self.fmt_state.fill.as_mut() {
                                ui.color_edit_button_srgb(f).on_hover_text("底色");
                            }
                        });
                        ui.end_row();
                    });

                dlg_section(ui, "对齐");
                egui::Grid::new("fmt_g_align")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        dlg_cell_label(ui, "水平");
                        ui.horizontal(|ui| {
                            for (label, v) in
                                [("常规", 0u8), ("左", 1), ("中", 2), ("右", 3)]
                            {
                                ui.selectable_value(&mut self.fmt_state.halign, v, label);
                            }
                            ui.add_space(12.0);
                            ui.label(egui::RichText::new("垂直").size(11.5).weak());
                            for (label, v) in
                                [("常规", 0u8), ("上", 1), ("中", 2), ("下", 3)]
                            {
                                ui.selectable_value(&mut self.fmt_state.valign, v, label);
                            }
                        });
                        ui.end_row();

                        dlg_cell_label(ui, "文本");
                        let mut wrap = self.fmt_state.wrap;
                        if ui.checkbox(&mut wrap, "自动换行").changed() {
                            self.fmt_state.wrap = wrap;
                        }
                        ui.end_row();
                    });

                dlg_section(ui, "数字");
                egui::Grid::new("fmt_g_num")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        dlg_cell_label(ui, "格式");
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("fmt_dlg_num")
                                .selected_text(if self.fmt_state.format.is_empty() {
                                    "常规".to_string()
                                } else {
                                    self.fmt_state.format.clone()
                                })
                                .width(160.0)
                                .show_ui(ui, |ui| {
                                    for (label, code) in [
                                        ("常规", ""),
                                        ("0", "0"),
                                        ("0.00", "0.00"),
                                        ("#,##0", "#,##0"),
                                        ("#,##0.00", "#,##0.00"),
                                        ("0%", "0%"),
                                        ("0.00%", "0.00%"),
                                        ("yyyy-mm-dd", "yyyy-mm-dd"),
                                        ("yyyy/mm/dd", "yyyy/mm/dd"),
                                    ] {
                                        if ui
                                            .selectable_label(
                                                self.fmt_state.format == code,
                                                label,
                                            )
                                            .clicked()
                                        {
                                            self.fmt_state.format = code.to_string();
                                        }
                                    }
                                });
                            let sample = sheetcore::style::format_value(
                                &sheetcore::model::Value::Number(1234.5678),
                                &self.fmt_state.format,
                            );
                            ui.label(
                                egui::RichText::new(format!("示例  {sample}"))
                                    .monospace()
                                    .size(11.5)
                                    .weak(),
                            );
                        });
                        ui.end_row();
                    });

                dlg_section(ui, "边框");
                egui::Grid::new("fmt_g_border")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        dlg_cell_label(ui, "线条");
                        ui.horizontal(|ui| {
                            for (label, idx) in
                                [("左", 0usize), ("右", 1), ("上", 2), ("下", 3)]
                            {
                                let on = self.fmt_state.border[idx];
                                if ui.selectable_label(on, label).clicked() {
                                    self.fmt_state.border[idx] = !on;
                                }
                            }
                            ui.add_space(8.0);
                            if ui_kit::pill(ui, "外框", "四周都画").clicked() {
                                self.fmt_state.border = [true; 4];
                            }
                            if ui_kit::pill(ui, "无", "去掉所有边框").clicked() {
                                self.fmt_state.border = [false; 4];
                            }
                        });
                        ui.end_row();

                        dlg_cell_label(ui, "合并");
                        ui.horizontal(|ui| {
                            if ui_kit::pill(ui, "合并单元格", "").clicked() {
                                self.toggle_merge(true);
                            }
                            if ui_kit::pill(ui, "取消合并", "").clicked() {
                                self.toggle_merge(false);
                            }
                        });
                        ui.end_row();
                    });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.button("取消").clicked() {
                                cancel = true;
                            }
                            if ui_kit::primary_button(ui, "确定").clicked() {
                                apply = true;
                            }
                        },
                    );
                });
            });
        if apply {
            let borders = st.border;
            let (r1, c1, r2, c2) = self.sel_rect();
            self.apply_style_xy(move |s, r, c| {
                for (i, on) in borders.iter().enumerate() {
                    s.border[i].style = if *on { 1 } else { 0 };
                }
                // 外框：内部边不画
                if borders[0] && c > c1 {
                    s.border[0].style = 0;
                }
                if borders[1] && c < c2 {
                    s.border[1].style = 0;
                }
                if borders[2] && r > r1 {
                    s.border[2].style = 0;
                }
                if borders[3] && r < r2 {
                    s.border[3].style = 0;
                }
            });
            self.apply_style(move |s| {
                s.font = st.font.clone();
                s.bold = st.bold;
                s.italic = st.italic;
                s.size = st.size;
                s.color = st.color;
                s.fill = st.fill;
                s.halign = st.halign;
                s.valign = st.valign;
                s.wrap = st.wrap;
                s.format = st.format.clone();
            });
            self.fmt_dlg = false;
            self.status = "已应用格式".into();
        }
        if cancel || !open {
            self.fmt_dlg = false;
        }
    }

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((idx, mut name)) = self.rename_sheet.take() else { return };
        let mut open = true;
        let mut ok = false;
        ui_kit::window(ctx, "重命名工作表", 420.0)
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
        ui_kit::window(ctx, "查找与替换", 380.0)
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui_kit::field(ui, "查找", |ui| {
                    let r = ui.add_sized(
                        [ui.available_width(), 22.0],
                        egui::TextEdit::singleline(&mut self.find.find),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        do_find = true;
                    }
                });
                ui_kit::field(ui, "替换为", |ui| {
                    ui.add_sized(
                        [ui.available_width(), 22.0],
                        egui::TextEdit::singleline(&mut self.find.replace),
                    );
                });
                ui.horizontal(|ui| {
                    ui.add_space(68.0);
                    ui.checkbox(&mut self.find.match_case, "区分大小写");
                    ui.checkbox(&mut self.find.whole_cell, "整格匹配");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("命中 {} 处", self.find.hits))
                                .size(11.0)
                                .weak(),
                        );
                    });
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("全部替换").clicked() {
                        do_all = true;
                    }
                    if ui.button("替换").clicked() {
                        do_repl = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            self.find.open = false;
                        }
                        if ui_kit::primary_button(ui, "查找下一个").clicked() {
                            do_find = true;
                        }
                        if ui.button("上一个").clicked() {
                            do_prev = true;
                        }
                    });
                });
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
        ui_kit::window(ctx, "宏代码（只读）", 420.0)
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
        ui_kit::window(ctx, "插入图表", 280.0)
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

                    ui.label("X 轴标题");
                    ui.text_edit_singleline(&mut ch.axis_x);
                    ui.end_row();
                    ui.label("Y 轴标题");
                    ui.text_edit_singleline(&mut ch.axis_y);
                    ui.end_row();
                    ui.label("Y 轴范围");
                    ui.horizontal(|ui| {
                        let mut auto = ch.y_min.is_none() && ch.y_max.is_none();
                        if ui.checkbox(&mut auto, "自动").changed() {
                            if auto {
                                ch.y_min = None;
                                ch.y_max = None;
                            } else {
                                ch.y_min = Some(0.0);
                                ch.y_max = Some(100.0);
                            }
                        }
                        if !auto {
                            let mut lo = ch.y_min.unwrap_or(0.0);
                            let mut hi = ch.y_max.unwrap_or(0.0);
                            ui.add(egui::DragValue::new(&mut lo).speed(1.0));
                            ui.label("到");
                            ui.add(egui::DragValue::new(&mut hi).speed(1.0));
                            ch.y_min = Some(lo);
                            ch.y_max = Some(hi);
                        }
                    });
                    ui.end_row();
                    ui.label("数据区域");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.chart_src);
                        if ui.small_button("重新读取").clicked() {
                            self.chart_reload_src = true;
                        }
                    });
                    ui.end_row();

                    ui.label("系列");
                    ui.label(format!("{} 个", ch.series.len()));
                    ui.end_row();
                });
                if !ch.series.is_empty() {
                    ui.separator();
                    ui.label("系列颜色");
                    for srs in ch.series.iter_mut() {
                        ui.horizontal(|ui| {
                            let mut c = srs.color.unwrap_or([0x4F, 0x81, 0xBD]);
                            if ui.color_edit_button_srgb(&mut c).changed() {
                                srs.color = Some(c);
                            }
                            ui.label(&srs.name);
                        });
                    }
                }
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
        if self.chart_reload_src {
            self.chart_reload_src = false;
            if let Some(rect) = sheetcore::table::parse_a1(&self.chart_src) {
                let name = self.wb.sheet(self.sheet_idx).name.clone();
                let mut fresh = sheetcore::chart_write::chart_from_selection(
                    self.wb.sheet(self.sheet_idx),
                    &name,
                    rect,
                    &ch.title,
                    ch.kind,
                );
                fresh.anchor = ch.anchor;
                fresh.legend = ch.legend;
                fresh.legend_pos = ch.legend_pos.clone();
                fresh.show_labels = ch.show_labels;
                fresh.axis_x = ch.axis_x.clone();
                fresh.axis_y = ch.axis_y.clone();
                fresh.y_min = ch.y_min;
                fresh.y_max = ch.y_max;
                fresh.part = ch.part.clone();
                fresh.drawing = ch.drawing.clone();
                fresh.series.iter_mut().enumerate().for_each(|(i, s2)| {
                    s2.color = ch.series.get(i).and_then(|x| x.color);
                });
                ch = fresh;
                self.status = format!("已按 {} 重新读取数据", self.chart_src);
            } else {
                self.status = "数据区域格式不对，写 A1:C10 这样".into();
            }
        }
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
        ui_kit::window(ctx, "图表管理", 340.0)
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
        ui_kit::window(ctx, "数据透视表", 300.0)
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

        // 右键菜单（选区操作）
        resp.context_menu(|ui| {
            let (r1, c1, r2, c2) = self.sel_rect();
            let rows = r2 - r1 + 1;
            if ui.button("剪切").clicked() {
                self.clip_copy(true, ui.ctx());
                ui.close_menu();
            }
            if ui.button("复制").clicked() {
                self.clip_copy(false, ui.ctx());
                ui.close_menu();
            }
            if ui.button("粘贴").clicked() {
                self.clip_paste(false, None);
                ui.close_menu();
            }
            ui.separator();
            if ui.button(format!("插入 {rows} 行")).clicked() {
                self.structural(sheetcore::structural::Structural::InsertRows { at: r1, n: rows });
                ui.close_menu();
            }
            if ui.button(format!("删除 {rows} 行")).clicked() {
                self.structural(sheetcore::structural::Structural::DeleteRows { at: r1, n: rows });
                ui.close_menu();
            }
            if ui.button("插入列").clicked() {
                self.structural(sheetcore::structural::Structural::InsertCols { at: c1, n: 1 });
                ui.close_menu();
            }
            if ui.button("删除列").clicked() {
                self.structural(sheetcore::structural::Structural::DeleteCols { at: c1, n: 1 });
                ui.close_menu();
            }
            ui.separator();
            if ui.button("冻结到当前单元格").clicked() {
                self.set_freeze(Some((r1, c1)));
                ui.close_menu();
            }
            if ui.button("取消冻结").clicked() {
                self.set_freeze(None);
                ui.close_menu();
            }
        });
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
                FontId::new(12.0, family_named("latin")),
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
                    FontId::new(12.0, family_named("latin")),
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
                FontId::new(12.0, family_named("latin")),
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
                    FontId::new(12.0, family_named("latin")),
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

    /// 宿主没了：以当前手里的数据接任，同事用新地址重新加入
    fn promote_to_host(&mut self) {
        // 只有客户端能接任；先彻底丢掉旧连接（含重连线程）
        if self.join_addr.is_empty() {
            return;
        }
        self.net = None;
        self.peer = None;
        self.remote.clear();
        self.locks = sheetcore::sync::LockTable::default();
        self.pending_edit = None;
        self.my_lock = None;
        self.room_lost = false;
        self.start_host();
        if self.net.is_some() {
            self.status = format!(
                "已接任宿主，同事请用这个地址重新加入：{}:{}",
                local_ip_hint(),
                net::DEFAULT_PORT
            );
            eprintln!("[net] {}{}", "接任宿主 ", self.status);
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
                // 在文件旁边放占用卡：同事打开同一个文件就能自动找到我
                if let Some(p) = self.wb.path.clone() {
                    let addr = format!("{}:{}", local_ip_hint(), port);
                    let _ = session_lock::write(&p, &addr, &self.my_name);
                    self.lock_beat = Some(std::time::Instant::now());
                }
                self.status = format!("已共享，同事可加入：{}:{}", local_ip_hint(), port);
            eprintln!("[net] {}", self.status);
            }
            Err(e) => self.status = format!("共享失败: {e}"),
        }
    }


    /// 打开文件后自动发现同编人：读文件旁边的占用卡，新鲜就加入
    fn try_auto_join(&mut self) {
        let Some(path) = self.wb.path.clone() else { return };
        if self.net.is_some() || self.auto_join_tried.as_ref() == Some(&path) {
            return;
        }
        self.auto_join_tried = Some(path.clone());

        let Some(info) = session_lock::read(&path) else { return };
        if session_lock::is_mine(&info) {
            return;
        }
        if !session_lock::is_fresh(&info) {
            // 上一任编辑者已经不在了：我接管，把卡换成我的地址
            let who = if info.user.is_empty() { info.machine.clone() } else { info.user.clone() };
            eprintln!("[net] 占用卡已过期（{}），接管共享", who);
            self.status = format!("{} 已退出，已接管共享", who);
            self.start_host();
            return;
        }
        let who = if info.user.is_empty() { info.machine.clone() } else { info.user.clone() };
        self.status = format!("发现 {} 正在编辑这个文件，正在加入…", who);
        eprintln!("[net] 发现占用卡：{} @ {}", who, info.host);
        let site = rand_site();
        match net::join(site, self.my_name.clone(), self.wb.title(), &info.host, self.room_password.clone()) {
            Ok(n) => {
                self.peer = Some(Peer::new(site));
                self.peers_list = vec![(site, self.my_name.clone(), (site % 7) as u8)];
                self.net = Some(n);
                self.undo.set_site(site);
                self.status = format!("已加入 {} 的编辑会话（{}）", who, info.host);
                eprintln!("[net] {}", self.status);
            }
            Err(e) => {
                self.status = format!(
                    "{} 正在编辑，但连不上 {}（{}）。可能不在同一网段，或对方已退出",
                    who, info.host, e
                );
            }
        }
    }

    /// 宿主心跳：每 5 秒刷新占用卡
    fn beat_lock(&mut self) {
        let hosting = self.net.as_ref().map(|n| n.hosting).unwrap_or(false);
        if !hosting {
            return;
        }
        let Some(path) = self.wb.path.clone() else { return };
        let due = self.lock_beat.map(|t| t.elapsed().as_secs() >= 5).unwrap_or(true);
        if !due {
            return;
        }
        let addr = format!("{}:{}", local_ip_hint(), net::DEFAULT_PORT);
        let _ = session_lock::write(&path, &addr, &self.my_name);
        self.lock_beat = Some(std::time::Instant::now());
    }


    /// 每 2 秒做的事：宿主刷心跳；客户端找一找"谁在编辑"
    fn tick_session(&mut self) {
        let due = self
            .lock_beat
            .map(|t| t.elapsed().as_secs() >= 2)
            .unwrap_or(true);
        if !due && self.remote_editor.is_some() {
            return;
        }
        let hosting = self.net.as_ref().map(|n| n.hosting).unwrap_or(false);
        if hosting {
            self.beat_lock();
            self.remote_editor = None;
            return;
        }
        // 没连上时，读一眼占用卡：让别人看见"谁在编辑"
        let Some(path) = self.wb.path.clone() else {
            self.remote_editor = None;
            return;
        };
        self.remote_editor = session_lock::read(&path).and_then(|info| {
            if session_lock::is_mine(&info) || !session_lock::is_fresh(&info) {
                None
            } else if info.user.is_empty() {
                Some(info.machine)
            } else {
                Some(info.user)
            }
        });
        self.lock_beat = Some(std::time::Instant::now());
    }

    /// 窗口标题：文件名 + 未保存 + 谁在编辑
    fn update_window_title(&mut self, ctx: &egui::Context) {
        let name = self
            .wb
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名.xlsx".into());
        let mut title = name;
        if self.wb.dirty {
            title.push_str(" *");
        }
        let hosting = self.net.as_ref().map(|n| n.hosting).unwrap_or(false);
        let connected = self.net.is_some();
        if connected {
            let total = self.peers_list.len().max(1);
            // 同一台机器上两个进程名字一样，按站点号区分才靠得住
            let my_site = self.net.as_ref().map(|n| n.site).unwrap_or(0);
            let names: Vec<String> = self
                .peers_list
                .iter()
                .filter(|(site, _, _)| *site != my_site)
                .map(|(_, n, _)| n.clone())
                .collect();
            if total > 1 {
                title.push_str(&format!(" · {} 人协作（{}）", total, names.join("、")));
            } else if hosting {
                title.push_str(" · 已共享，等待同事加入");
            } else {
                title.push_str(" · 已加入协作会话");
            }
        } else if let Some(who) = &self.remote_editor {
            title.push_str(&format!(" · {who} 正在编辑"));
        }
        title.push_str(" - Sheets Native");
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            eprintln!("[title] {title}");
            self.last_title = title;
        }
    }


    /// 别的进程重复打开了同一个文件：后台线程已经比对过并让它退出，
    /// 这里只负责给用户一个提示
    fn poll_open_requests(&mut self) {
        if let Some(cur) = &self.shared_current_file {
            if let Ok(mut slot) = cur.lock() {
                *slot = self.wb.path.clone();
            }
        }
        let Some(rx) = &self.open_requests else { return };
        while let Ok(path) = rx.try_recv() {
            self.status = format!(
                "{} 已经在这个窗口打开了",
                path.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
            eprintln!("[single] 同一文件重复打开，已切到现有窗口");
        }
    }

    /// 注册文件关联（当前用户）
    fn register_file_association(&mut self, make_default: bool) {
        let Ok(exe) = std::env::current_exe() else {
            self.status = "拿不到自己的路径，没法注册关联".into();
            return;
        };
        let res = if make_default {
            single_instance::set_default(&exe)
        } else {
            single_instance::register_association(&exe)
        };
        self.status = match res {
            Ok(_) if make_default => {
                "已登记为 .xlsx/.xlsm 的默认打开方式；Windows 可能还会问你一次是否允许".into()
            }
            Ok(_) => "已加入 .xlsx/.xlsm 的“打开方式”列表，右键即可选本程序".into(),
            Err(e) => format!("注册关联失败：{e}"),
        };
        eprintln!("[assoc] {}", self.status);
    }



    /// 自检：多标签
    fn tabs_probe(&mut self) {
        if let Some(idx) = self.pending_tab_switch {
            if self.open_rx.is_none() {
                self.pending_tab_switch = None;
                eprintln!("[tabs] 切回前 标签数={} 活动={}", self.tabs.len(), self.active_tab);
                self.switch_tab(idx);
                eprintln!(
                    "[tabs] 切回后 活动={} 文档={:?} 标题={}",
                    self.active_tab,
                    self.wb.path,
                    self.tab_title(self.active_tab)
                );
            }
        }
        if self.tabs_done {
            return;
        }
        if let Ok(p) = std::env::var("SC_TABS") {
            if self.open_rx.is_some() || p.is_empty() {
                return;
            }
            self.tabs_done = true;
            eprintln!("[tabs] 当前标签 {} 文档 {:?}", self.active_tab, self.wb.path);
            self.open_in_new_tab(PathBuf::from(p));
            if let Ok(idx) = std::env::var("SC_SWITCH_TAB") {
                self.pending_tab_switch = idx.parse().ok();
            }
            return;
        }
        if let Ok(idx) = std::env::var("SC_SWITCH_TAB") {
            if self.open_rx.is_some() {
                return;
            }
            self.tabs_done = true;
            let i: usize = idx.parse().unwrap_or(0);
            eprintln!("[tabs] 切换前 标签数={} 活动={} 文档={:?}", self.tabs.len(), self.active_tab, self.wb.path);
            self.switch_tab(i);
            eprintln!("[tabs] 切换后 活动={} 文档={:?} 标题={}", self.active_tab, self.wb.path, self.tab_title(self.active_tab));
        }
    }


    /// 自检：读系统剪贴板里的 HTML 表格并粘贴
    fn paste_probe(&mut self) {
        if std::env::var("SC_PASTE_HTML").is_err() || self.paste_done || self.open_rx.is_some() {
            return;
        }
        self.paste_done = true;
        self.sel = (0, 0);
        self.sel_end = (0, 0);
        match office_clipboard::read_table() {
            Some(block) => {
                eprintln!("[paste] 读到表格 {} 行 × {} 列", block.rows.len(), block.cols());
                let ok = self.paste_office_block(&block);
                let sh = self.wb.sheet(self.sheet_idx);
                let a1 = sh.get(0, 0).map(|c| c.value.display()).unwrap_or_default();
                let st = sh.style_of(0, 0);
                let cs = self.wb.styles.get(st);
                eprintln!(
                    "[paste] 成功={ok} A1={a1:?} 粗体={} 填充={:?} 合并数={}",
                    cs.bold,
                    cs.fill,
                    sh.merges.len()
                );
                for (r, c) in [(0u32, 0u32), (0, 1), (1, 0), (1, 1)] {
                    if let Some(x) = sh.get(r, c) {
                        let sid = sh.style_of(r, c);
                        let st = self.wb.styles.get(sid);
                        eprintln!(
                            "[paste] ({r},{c}) 值={:?} 公式={:?} 字体={:?} 字号={} 格式={:?}",
                            x.value.display(),
                            x.formula,
                            st.font,
                            st.size,
                            st.format
                        );
                    }
                }
            }
            None => eprintln!("[paste] 剪贴板里没有 HTML 表格"),
        }
        if std::env::var("SC_PASTE_CHECK").is_ok() {
            // 改一下被引用的格子，看粘进来的公式是不是活的
            let key = (self.sheet_idx, 0u32, 0u32);
            if let Some(c) = self.wb.sheet(self.sheet_idx).get(0, 0).cloned() {
                let mut c2 = c;
                c2.value = Value::Number(5.0);
                self.wb.sheet_mut(self.sheet_idx).set(0, 0, c2);
                self.deps.update_cell(&self.wb, key);
                sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &[key]);
                let b1 = self
                    .wb
                    .sheet(self.sheet_idx)
                    .get(0, 1)
                    .map(|x| x.value.display())
                    .unwrap_or_default();
                eprintln!("[paste] A1=5 后 B1={b1:?}（应 7）");
            }
        }
    }

    /// 启动页：没指定文件时列出最近打开的
    fn start_page(&mut self, ctx: &egui::Context) {
        if !self.show_start {
            return;
        }
        let mut open_wanted: Option<PathBuf> = None;
        let mut close = false;
        ui_kit::window(ctx, "开始", 460.0)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("Sheets Native");
                ui.label(
                    egui::RichText::new(format!("版本 {}", update::VERSION)).weak(),
                );
                ui.separator();
                let list = recent::load();
                if list.is_empty() {
                    ui.label("还没有打开过文件。");
                } else {
                    ui.label(egui::RichText::new("最近打开").strong());
                    for p in list {
                        let name = p
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(format!("{name}\n{dir}")).size(13.0),
                                )
                                .min_size(egui::vec2(420.0, 34.0)),
                            )
                            .clicked()
                        {
                            open_wanted = Some(p.clone());
                        }
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("打开文件…").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("Excel 工作簿", &["xlsx", "xlsm", "csv"])
                            .pick_file()
                        {
                            open_wanted = Some(p);
                        }
                    }
                    if ui.button("新建空白").clicked() {
                        close = true;
                    }
                });
            });
        if let Some(p) = open_wanted {
            self.show_start = false;
            self.open_path(p);
        } else if close {
            self.show_start = false;
        }
    }

    /// 历史版本窗口
    fn history_window(&mut self, ctx: &egui::Context) {
        let Some((file, versions)) = self.history_view.clone() else { return };
        let mut open = true;
        let mut restore: Option<PathBuf> = None;
        let mut close = false;
        ui_kit::window(ctx, "历史版本", 560.0)
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}（最多保留 {} 份，每次覆盖前自动留存）",
                        file.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                        sheetcore::backup::KEEP
                    ))
                    .weak(),
                );
                ui.separator();
                if versions.is_empty() {
                    ui.label("还没有历史版本——先保存一次试试。");
                }
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for v in &versions {
                        ui.horizontal(|ui| {
                            let kb = v.bytes as f64 / 1024.0;
                            ui.label(
                                egui::RichText::new(format!("{}  ·  {kb:.0} KB", v.stamp)).monospace(),
                            );
                            if ui.button("打开这一版").clicked() {
                                restore = Some(v.path.clone());
                            }
                        });
                    }
                });
                ui.separator();
                if ui.button("关闭").clicked() {
                    close = true;
                }
            });
        if let Some(p) = restore {
            self.status = "已打开历史版本（另存为新文件即可保留）".into();
            self.open_path(p);
            self.history_view = None;
            return;
        }
        if !open || close {
            self.history_view = None;
        }
    }


    /// 从共享目录检查更新：有新版就下到本地，关窗时替换
    fn check_update(&mut self) {
        let dir = match update::dir() {
            Some(d) => d,
            None => {
                let Some(p) = rfd::FileDialog::new()
                    .set_title("选择发版目录（放 manifest.txt 和新版 exe）")
                    .pick_folder()
                else {
                    return;
                };
                if let Err(e) = update::set_dir(&p) {
                    self.status = format!("记住发版目录失败：{e}");
                    return;
                }
                p
            }
        };
        match update::check(&dir) {
            Ok(None) => self.status = format!("已是最新版本（{}）", update::VERSION),
            Ok(Some(remote)) => match update::stage(&remote) {
                Ok(staged) => match update::apply_on_exit(&staged) {
                    Ok(_) => {
                        self.status = format!(
                            "发现新版本 {}，已下载并校验；关闭窗口后自动替换并重启",
                            remote.version
                        );
                    }
                    Err(e) => self.status = format!("准备替换脚本失败：{e}"),
                },
                Err(e) => self.status = format!("下载更新失败：{e}"),
            },
            Err(e) => self.status = format!("检查更新失败：{e}"),
        }
        eprintln!("[update] {}", self.status);
    }


    /// 自检：启动后跑一次检查更新
    fn update_probe(&mut self) {
        if std::env::var("SC_UPDATE_CHECK").is_err() || self.update_done {
            return;
        }
        self.update_done = true;
        self.check_update();
    }


    /// 把当前文档从 self 上"取下来"
    fn park_current(&mut self) -> Box<ParkedDoc> {
        let title = self
            .wb
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名".into());
        Box::new(ParkedDoc {
            title,
            wb: std::mem::replace(&mut self.wb, Workbook::new_blank()),
            undo: std::mem::replace(&mut self.undo, sheetcore::undo::UndoStack::new()),
            deps: std::mem::replace(&mut self.deps, sheetcore::formula::deps::Deps::empty()),
            sel: self.sel,
            sel_end: self.sel_end,
            sheet_idx: self.sheet_idx,
            scroll: self.scroll,
            zoom: self.zoom,
            doc_gen: self.doc_gen,
            cf_stats_gen: self.cf_stats_gen,
        })
    }

    /// 把一份挂起的文档装到 self 上
    fn activate_parked(&mut self, doc: Box<ParkedDoc>) {
        self.wb = doc.wb;
        self.undo = doc.undo;
        self.deps = doc.deps;
        self.sel = doc.sel;
        self.sel_end = doc.sel_end;
        self.sheet_idx = doc.sheet_idx;
        self.scroll = doc.scroll;
        self.zoom = doc.zoom;
        self.doc_gen = doc.doc_gen;
        self.cf_stats_gen = doc.cf_stats_gen;
        self.wb_epoch = self.wb_epoch.wrapping_add(1);
        self.editing = None;
        self.cf_stats_gen = u64::MAX;
        self.needs_full = false;
    }

    /// 切到某个标签
    fn switch_tab(&mut self, idx: usize) {
        if idx == self.active_tab || idx >= self.tabs.len() {
            return;
        }
        if self.net.is_some() {
            self.status = "这个标签正在协作中，先断开（或等同事改完）再切换".into();
            return;
        }
        let parked = self.park_current();
        self.tabs[self.active_tab] = TabSlot::Parked(parked);
        let target = std::mem::replace(&mut self.tabs[idx], TabSlot::Active);
        self.active_tab = idx;
        if let TabSlot::Parked(doc) = target {
            self.activate_parked(doc);
        }
        let title = self.tab_title(idx);
        self.status = format!("已切到 {title}");
    }

    fn tab_title(&self, idx: usize) -> String {
        match self.tabs.get(idx) {
            Some(TabSlot::Active) => self
                .wb
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "未命名".into()),
            Some(TabSlot::Parked(p)) => p.title.clone(),
            None => String::new(),
        }
    }

    /// 在新标签里打开文件
    fn open_in_new_tab(&mut self, path: PathBuf) {
        if self.net.is_some() {
            self.status = "当前标签正在协作中，先断开再开新标签".into();
            return;
        }
        let parked = self.park_current();
        self.tabs[self.active_tab] = TabSlot::Parked(parked);
        self.tabs.push(TabSlot::Active);
        self.active_tab = self.tabs.len() - 1;
        self.reset_editor_state();
        if path.as_os_str().is_empty() {
            self.wb = Workbook::new_blank();
            self.status = "已新建标签".into();
        } else {
            self.open_path(path);
        }
    }

    /// 关掉某个标签
    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || self.tabs.len() == 1 {
            return;
        }
        if idx == self.active_tab {
            if self.net.is_some() {
                self.status = "这个标签正在协作中，先断开再关闭".into();
                return;
            }
            if self.wb.dirty && !self.confirm_discard() {
                return;
            }
            self.tabs.remove(idx);
            if self.active_tab > idx || self.active_tab >= self.tabs.len() {
                self.active_tab = self.active_tab.saturating_sub(1);
            }
            // 装载新活动标签
            let target = std::mem::replace(&mut self.tabs[self.active_tab], TabSlot::Active);
            if let TabSlot::Parked(doc) = target {
                self.activate_parked(doc);
            }
        } else {
            self.tabs.remove(idx);
            if self.active_tab > idx {
                self.active_tab -= 1;
            }
        }
    }

    /// 关标签前的未保存确认（暂时用状态栏提示 + 二次点击）
    fn confirm_discard(&mut self) -> bool {
        if self.confirm_close_once {
            self.confirm_close_once = false;
            return true;
        }
        self.confirm_close_once = true;
        self.status = "这个文件有未保存的改动，再点一次 × 确认丢弃".into();
        false
    }

    /// 打开一个新文档时把编辑态清干净
    fn reset_editor_state(&mut self) {
        self.sel = (0, 0);
        self.sel_end = (0, 0);
        self.sheet_idx = 0;
        self.scroll = Vec2::ZERO;
        self.editing = None;
        self.doc_gen = 0;
        self.cf_stats_gen = u64::MAX;
        self.undo = sheetcore::undo::UndoStack::new();
        self.deps = sheetcore::formula::deps::Deps::empty();
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
                    let before_ops: Vec<Option<sheetcore::model::Cell>> = if is_snapshot {
                        Vec::new()
                    } else {
                        ops.iter()
                            .map(|op| {
                                self.wb
                                    .sheets
                                    .get(op.sheet)
                                    .and_then(|s| s.get(op.row, op.col))
                                    .cloned()
                            })
                            .collect()
                    };
                    let ops_snapshot = if is_snapshot { Vec::new() } else { ops.clone() };
                    let mut touched: Vec<(usize, u32, u32)> = Vec::new();
                    if let Some(peer) = self.peer.as_mut() {
                        for op in ops {
                            let key = (op.sheet, op.row, op.col);
                            if peer.apply(op) {
                                touched.push(key);
                            }
                        }
                    }
                    if !ops_snapshot.is_empty() {
                        self.record_remote_ops(&ops_snapshot, before_ops);
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
                    self.status = format!("{msg}，正在自动重连…（也可以点「接任宿主」继续协作）");
                    self.room_lost = true;
                    eprintln!("[net] disconnected: {msg}");
                    if std::env::var("SC_PROMOTE").is_ok() {
                        self.promote_to_host();
                    }
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

/// 修订记录里的时间：只显示相对量，省掉时区换算
fn rel_time(ts: u64) -> String {
    let now = sheetcore::revision::now_ms();
    let d = now.saturating_sub(ts) / 1000;
    match d {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分钟前", d / 60),
        3600..=86399 => format!("{} 小时前", d / 3600),
        _ => format!("{} 天前", d / 86400),
    }
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

impl Drop for SheetApp {
    fn drop(&mut self) {
        // 退出时把占用卡撤掉，同事下次打开就不会去找一个已经消失的地址
        let hosting = self.net.as_ref().map(|n| n.hosting).unwrap_or(false);
        if hosting {
            if let Some(p) = self.wb.path.clone() {
                session_lock::remove(&p);
                eprintln!("[net] 已撤下占用卡");
            }
        }
    }
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
        self.xlink_dialog(ctx);
        self.revision_panel(ctx);
        self.note_dialog(ctx);
        self.link_dialog(ctx);
        self.macro_run_dialog(ctx);
        self.macro_run_probe();
        self.tick_session();
        self.poll_open_requests();
        self.tabs_probe();
        self.paste_probe();
        self.update_probe();
        self.start_page(ctx);
        self.history_window(ctx);
        self.update_window_title(ctx);
        self.filter_dialog(ctx);
        self.pivot_dialog(ctx);
        self.renew_lock_if_needed();
        self.beat_lock();
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
        self.d3_probe();
        self.b4_probe();
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
        self.format_dialog(ctx);
        // 缩放：Ctrl+滚轮
        let (ctrl, wheel) = ctx.input(|i| (i.modifiers.command, i.raw_scroll_delta.y));
        if ctrl && wheel.abs() > 0.5 {
            let z = (self.zoom * (1.0 + wheel * 0.0015)).clamp(0.5, 2.5);
            if (z - self.zoom).abs() > 0.001 {
                self.zoom = z;
                ctx.set_zoom_factor(z);
            }
        }

        egui::TopBottomPanel::top("ribbon")
            .frame(egui::Frame::NONE.fill(ctx.style().visuals.panel_fill))
            .show(ctx, |ui| {
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    for (i, name) in
                        ["文件", "开始", "插入", "公式", "数据", "视图"].iter().enumerate()
                    {
                        if ui_kit::tab(ui, name, self.ribbon_tab == i).clicked() {
                            self.ribbon_tab = i;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        if self.wb.has_vba {
                            ui.label(
                                egui::RichText::new("含宏（保留不执行）")
                                    .size(10.5)
                                    .color(egui::Color32::from_rgb(180, 110, 0)),
                            );
                        }
                        if self.net.is_some() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "协作 · {} 人",
                                    self.peers_list.len()
                                ))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(46, 125, 50)),
                            );
                        }
                    });
                });
                ui.add_space(3.0);
                egui::ScrollArea::horizontal()
                    .id_salt("ribbon_scroll")
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.add_space(8.0);
                            match self.ribbon_tab {
                                0 => self.ribbon_file(ui),
                                1 => self.ribbon_home(ui),
                                2 => self.ribbon_insert(ui),
                                3 => self.ribbon_formula(ui),
                                4 => self.ribbon_data(ui),
                                _ => self.ribbon_view(ui),
                            }
                        });
                    });
                ui.add_space(4.0);
            });
        egui::TopBottomPanel::top("formula").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (r, c) = self.sel;
                if !self.name_edit {
                    self.name_buf = cell_name(r, c);
                }
                let nb = ui.add_sized(
                    [84.0, 22.0],
                    egui::TextEdit::singleline(&mut self.name_buf)
                        .hint_text("A1")
                        .font(egui::TextStyle::Monospace),
                );
                if nb.gained_focus() {
                    self.name_edit = true;
                }
                if nb.lost_focus() {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    self.name_edit = false;
                    if enter {
                        self.goto_name();
                    }
                }
                ui.label(egui::RichText::new("fx").italics());
                ui.separator();
                if self.editing.is_some() {
                    let mut commit = false;
                    let mut cancel = false;
                    if let Some(ed) = self.editing.as_mut() {
                        let te = ui.add_sized(
                            [ui.available_width(), 20.0],
                            egui::TextEdit::singleline(&mut ed.buf).font(egui::TextStyle::Monospace),
                        );
                        if te.lost_focus() {
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                commit = true;
                            } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                cancel = true;
                            }
                        }
                    }
                    if commit {
                        self.commit_edit(Some((1, 0)));
                    } else if cancel {
                        self.editing = None;
                        self.release_lock();
                    }
                } else {
                    let cur = match self.wb.sheet(self.sheet_idx).get(r, c) {
                        Some(cell) => cell
                            .formula
                            .as_deref()
                            .map(|f| sheetcore::xlinks::display_formula(&self.wb, f))
                            .unwrap_or_else(|| cell.value.display()),
                        None => String::new(),
                    };
                    let resp = ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), 22.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(cur)
                                        .family(egui::FontFamily::Monospace)
                                        .size(12.5),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .interact(egui::Sense::click())
                        },
                    )
                    .inner;
                    if resp.clicked() {
                        self.begin_edit(None);
                    }
                }
            });
        });

        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (r1, c1, r2, c2) = self.sel_rect();
                let cells = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
                ui.label(format!("{cells} 格"));
                if !self.wb.links.is_empty() {
                    ui.separator();
                    ui.label(format!("外部链接 {}", self.wb.links.len()))
                        .on_hover_text("数据 → 编辑链接…");
                }
                if cells > 1 && cells <= 200_000 {
                    let (mut count, mut sum, mut num) = (0u64, 0f64, 0u64);
                    let sheet = self.wb.sheet(self.sheet_idx);
                    for rr in r1..=r2 {
                        for cc in c1..=c2 {
                            if let Some(cell) = sheet.get(rr, cc) {
                                if !cell.is_blank() {
                                    count += 1;
                                    if let sheetcore::model::Value::Number(v) = &cell.value {
                                        sum += *v;
                                        num += 1;
                                    }
                                }
                            }
                        }
                    }
                    ui.separator();
                    ui.label(format!("计数 {count}"));
                    if num > 0 {
                        ui.separator();
                        ui.label(format!("求和 {sum:.2}"));
                        ui.separator();
                        ui.label(format!("平均 {:.2}", sum / num as f64));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.net.is_some() {
                        ui.label(egui::RichText::new("协作中").small().weak());
                        ui.separator();
                    }
                    ui.label(egui::RichText::new(self.status.clone()).small().weak());
                });
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
                    let resp = ui_kit::sheet_tab(ui, &name, self.sheet_idx == i);
                    if resp.drag_started() {
                        self.tab_drag = Some(i);
                    }
                    if let Some(from) = self.tab_drag {
                        if from != i && resp.hovered() && ui.input(|i| i.pointer.any_down()) {
                            self.wb.sheets.swap(from, i);
                            if self.sheet_idx == from {
                                self.sheet_idx = i;
                            } else if self.sheet_idx == i {
                                self.sheet_idx = from;
                            }
                            self.tab_drag = Some(i);
                        }
                    }
                    if resp.drag_stopped() {
                        self.tab_drag = None;
                    }
                    if resp.clicked() {
                        self.commit_edit(None);
                        goto = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("新建工作表").clicked() {
                            self.add_sheet();
                            ui.close_menu();
                        }
                        if ui.button("复制本表").clicked() {
                            self.duplicate_sheet(i);
                            ui.close_menu();
                        }
                        ui.separator();
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
                ui.add_space(4.0);
                if ui.button("＋").on_hover_text("新建工作表").clicked() {
                    self.add_sheet();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    if ui
                        .add(egui::Button::new(format!("{:.0}%", self.zoom * 100.0)).frame(false))
                        .on_hover_text("点击回到 100%")
                        .clicked()
                    {
                        self.zoom = 1.0;
                        ctx.set_zoom_factor(1.0);
                    }
                    let mut z = self.zoom;
                    if ui
                        .add_sized(
                            [110.0, 18.0],
                            egui::Slider::new(&mut z, 0.5..=2.5).show_value(false),
                        )
                        .changed()
                    {
                        self.zoom = z;
                        ctx.set_zoom_factor(z);
                    }
                    if ui.small_button("−").on_hover_text("缩小").clicked() {
                        self.zoom = (self.zoom - 0.1).max(0.5);
                        ctx.set_zoom_factor(self.zoom);
                    }
                    if ui.small_button("+").on_hover_text("放大").clicked() {
                        self.zoom = (self.zoom + 0.1).min(2.5);
                        ctx.set_zoom_factor(self.zoom);
                    }
                });
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
        if self.tree_open {
            egui::SidePanel::left("files")
                .resizable(true)
                .default_width(232.0)
                .width_range(180.0..=420.0)
                .show(ctx, |ui| {
                    to_open = self.tree.ui(ui);
                });
        }
        if let Some(p) = to_open {
            self.open_path(p);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                self.handle_keys(ctx);
                self.draw_grid(ui, ctx);
            });

        // 自截图：等界面稳定两帧再抓，避免抓到第一帧的半成品
        if let Some(path) = self.shot_path.clone() {
            self.shot_frames += 1;
            let at = std::env::var("SC_SHOT_FRAMES")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(8);
            if self.shot_frames == at {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            let shot = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                match save_color_image(&path, &img) {
                    Ok(()) => eprintln!("[shot] 已保存 {} ({}×{})", path.display(), img.size[0], img.size[1]),
                    Err(e) => eprintln!("[shot] 保存失败: {e:#}"),
                }
                self.shot_path = None;
                if std::env::var("SC_SHOT_EXIT").is_ok() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            } else {
                ctx.request_repaint();
            }
        }
    }
}

/// 对话框里定宽的一列（时间/用户/位置这类需要对齐）
fn dlg_col(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::Vec2::new(w, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        add,
    );
}

/// 对话框的小节标题（细线 + 强调色小字）
fn dlg_section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text)
            .size(10.5)
            .strong()
            .color(ui_kit::accent(ui.visuals())),
    );
    ui.separator();
}

/// 对话框里的字段名：固定宽度，保证多行控件对成一列
fn dlg_cell_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::Vec2::new(42.0, 22.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(egui::RichText::new(text).size(11.5).weak());
        },
    );
}

/// 把 egui 的 framebuffer 存成 PNG（image 已经带 png feature）
fn save_color_image(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let (w, h) = (img.size[0] as u32, img.size[1] as u32);
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    for px in &img.pixels {
        let [r, g, b, a] = px.to_array();
        buf.extend_from_slice(&[r, g, b, a]);
    }
    image::save_buffer(path, &buf, w, h, image::ColorType::Rgba8)?;
    Ok(())
}

/// 内嵌字体（SIL OFL 1.1）：思源黑体、思源宋体、Inter、JetBrains Mono。
///
/// 原始 ttf 不入库，子集由 scripts/subset_fonts.py 重建（GB2312 + 拉丁 + 常用符号）。
const FONT_SANS: &[u8] = include_bytes!("../../../assets/fonts/subset/NotoSansSC-Regular.subset.ttf");
const FONT_SERIF: &[u8] = include_bytes!("../../../assets/fonts/subset/NotoSerifSC-Regular.subset.ttf");
const FONT_LATIN: &[u8] = include_bytes!("../../../assets/fonts/subset/Inter-Regular.subset.ttf");
const FONT_MONO: &[u8] = include_bytes!("../../../assets/fonts/subset/JetBrainsMono-Regular.subset.ttf");

/// 自定义 family 名（和 `sheetcore::style::FontClass::key()` 一一对应）
fn family_named(key: &str) -> egui::FontFamily {
    egui::FontFamily::Name(key.into())
}

/// 单元格字体族：按 style.font 选内嵌 family，未知字体回退默认
fn cell_family(name: &str) -> egui::FontFamily {
    family_named(sheetcore::style::font_class(name).key())
}

/// 注册四套内嵌字体，并把自定义 family 都绑上——
/// 少绑一个，遇到该字体的格子 egui 会直接 panic（FontFamily not bound）。
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("noto-sans".to_owned(), egui::FontData::from_static(FONT_SANS).into());
    fonts
        .font_data
        .insert("noto-serif".to_owned(), egui::FontData::from_static(FONT_SERIF).into());
    fonts
        .font_data
        .insert("inter".to_owned(), egui::FontData::from_static(FONT_LATIN).into());
    fonts
        .font_data
        .insert("mono".to_owned(), egui::FontData::from_static(FONT_MONO).into());

    // 界面默认：拉丁走 Inter，中文回退思源黑体，再往后是 egui 自带的字体/emoji
    let prop = fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    prop.insert(0, "noto-sans".to_owned());
    prop.insert(0, "inter".to_owned());
    let mono = fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default();
    mono.insert(0, "noto-sans".to_owned());
    mono.insert(0, "mono".to_owned());

    // 单元格可选的 family
    let sans = vec!["inter".to_owned(), "noto-sans".to_owned()];
    fonts.families.insert(family_named("default"), sans.clone());
    fonts.families.insert(family_named("cjk-sans"), sans);
    // 宋体连拉丁也用宋体字形，和黑体一眼能分开
    fonts
        .families
        .insert(family_named("cjk-serif"), vec!["noto-serif".to_owned()]);
    fonts.families.insert(
        family_named("latin"),
        vec!["inter".to_owned(), "noto-sans".to_owned()],
    );
    fonts.families.insert(
        family_named("mono"),
        vec!["mono".to_owned(), "noto-sans".to_owned()],
    );

    ctx.set_fonts(fonts);
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
    // 命令行注册关联：打包脚本/安装器可以直接调用
    if std::env::args().any(|a| a == "--register-assoc") {
        let Ok(exe) = std::env::current_exe() else { return Ok(()) };
        match single_instance::register_association(&exe) {
            Ok(_) => println!("已加入“打开方式”列表"),
            Err(e) => println!("注册失败：{e}"),
        }
        return Ok(());
    }
    if std::env::args().any(|a| a == "--set-default") {
        let Ok(exe) = std::env::current_exe() else { return Ok(()) };
        match single_instance::set_default(&exe) {
            Ok(_) => println!("已设为默认打开方式"),
            Err(e) => println!("设置失败：{e}"),
        }
        return Ok(());
    }

    // 单实例：同一个文件重复双击时，把注意力交给已经打开的窗口
    // 用 Rc<RefCell<..>> 是因为渲染后端重试时闭包会被构造多次
    let current_file: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let role = single_instance::claim(initial.clone(), current_file.clone());
    let open_requests = match role {
        single_instance::Role::FocusedExisting => {
            println!("已在此窗口中打开");
            return Ok(());
        }
        single_instance::Role::Primary(rx) => Some(rx),
        single_instance::Role::Standalone => None,
    };
    let open_requests = std::rc::Rc::new(std::cell::RefCell::new(open_requests));

    // 渲染后端：--renderer=wgpu|glow|auto，默认 auto（先 wgpu，失败退 glow）
    let mut want = std::env::var("SC_RENDERER").unwrap_or_else(|_| "auto".into());
    for a in std::env::args().skip(1) {
        if let Some(v) = a.strip_prefix("--renderer=") {
            want = v.to_string();
        }
    }
    let mut opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([760.0, 420.0])
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
        #[cfg(all(feature = "wgpu", feature = "glow"))]
        {
            opts.renderer = if which == "wgpu" {
                eframe::Renderer::Wgpu
            } else {
                eframe::Renderer::Glow
            };
        }
        #[cfg(all(feature = "wgpu", not(feature = "glow")))]
        {
            let _ = which;
            opts.renderer = eframe::Renderer::Wgpu;
        }
        #[cfg(all(not(feature = "wgpu"), feature = "glow"))]
        {
            let _ = which;
            opts.renderer = eframe::Renderer::Glow;
        }
        #[cfg(not(any(feature = "wgpu", feature = "glow")))]
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
        let r = run_app(
            &mut opts,
            initial.clone(),
            auto_host,
            auto_join.clone(),
            auto_pass.clone(),
            open_requests.clone(),
            current_file.clone(),
        );
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
    open_requests: std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Receiver<PathBuf>>>>,
    current_file: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
) -> Result<(), eframe::Error> {
    eframe::run_native(
        "sheets-native",
        opts.clone(),
        {
            let initial = initial.clone();
            let auto_join = auto_join.clone();
            let open_requests = open_requests.clone();
            Box::new(move |cc| {
                let mut app = SheetApp::new(cc, initial.clone());
                app.open_requests = open_requests.borrow_mut().take();
                app.show_start = initial.is_none();
                app.shared_current_file = Some(current_file.clone());
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
