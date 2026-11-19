//! 应用级命令表（UI-02.3）。
//!
//! 功能区的按钮、开始页上的入口、菜单项和快捷键都从这里取同一份定义：
//!
//! - `action`：与 ribbon XML 的 `onAction` 同名，点击与按键走同一个执行函数；
//! - `key`/`shift`：快捷键（唯一登记处，避免帮助文字与实际按键不符）；
//! - `enabled`/`disabled_hint`：唯一的可用性谓词，按钮灰掉的同时快捷键也失效；
//! - `tip`：tooltip 文本，自动带上快捷键。
//!
//! # 键盘优先级
//!
//! 应用级快捷键按 [`Layer`] 分层，高优先级层先消费：
//!
//! 1. `Dialog`：`ui_kit::window` 画的模态对话框。键盘属于它，应用级命令一律不执行
//!    （否则会在对话框背后换掉文档或再弹一个文件对话框）。
//! 2. `CellEdit`：单元格编辑中。文档/外壳类命令先提交有效编辑再执行（与 Excel
//!    「先落编辑再执行命令」一致，草稿不会因为一个快捷键消失）；撤销/重做让给
//!    编辑框自己（TextEdit 的文本撤销）。
//! 3. `Start`：开始页，没有打开的文档。只有新建/打开可用，保存/打印不可用。
//! 4. `Grid`：网格空闲，全部按 `enabled` 执行。
//!
//! 非模态文本框（名称框、查找框、对话框之外的 TextEdit）不算一层：egui 的 TextEdit
//! 只消费文本键与方向键，Ctrl 组合键本来就归应用，这与 Excel 一致。

use eframe::egui;

use crate::SheetApp;

/// 键盘焦点层级（见模块文档的优先级说明）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    /// 开始页：没有打开的文档
    Start,
    /// `ui_kit::window` 模态对话框
    Dialog,
    /// 单元格内编辑中
    CellEdit,
    /// 网格空闲
    Grid,
}

pub struct Command {
    /// 与 ribbon XML 的 onAction 同名
    pub action: &'static str,
    /// 菜单/提示里的中文名
    pub label: &'static str,
    pub key: egui::Key,
    /// 是否要求 Shift（Ctrl+Shift+S 与 Ctrl+S 靠这个区分）
    pub shift: bool,
    /// 开始页是否可用
    pub on_start: bool,
    /// 单元格编辑中是否可用：可用的会先提交草稿再执行
    pub in_cell_edit: bool,
    /// 可用性谓词：按钮与快捷键共用
    pub enabled: fn(&SheetApp) -> bool,
}

impl Command {
    /// "Ctrl+S" / "Ctrl+Shift+S"
    pub fn shortcut(&self) -> String {
        if self.shift {
            format!("Ctrl+Shift+{:?}", self.key)
        } else {
            format!("Ctrl+{:?}", self.key)
        }
    }

    /// 带快捷键的 tooltip 首行，例如 "保存 (Ctrl+S)"
    pub fn tip(&self) -> String {
        format!("{} ({})", self.label, self.shortcut())
    }
}

// ---------------------------------------------------------------- 谓词

fn can_save(app: &SheetApp) -> bool {
    !app.show_start && app.tasks.saving.is_none() && app.tasks.open_rx.is_none()
}

fn can_shell(app: &SheetApp) -> bool {
    app.tasks.saving.is_none() && app.tasks.open_rx.is_none()
}

fn can_print(app: &SheetApp) -> bool {
    !app.show_start
}

fn can_undo(app: &SheetApp) -> bool {
    app.undo.can_undo()
}

fn can_redo(app: &SheetApp) -> bool {
    app.undo.can_redo()
}

/// 唯一命令表。新增快捷键必须登记在这里，`handle_command_keys` 只认这张表。
pub const COMMANDS: &[Command] = &[
    Command {
        action: "FileSave",
        label: "保存",
        key: egui::Key::S,
        shift: false,
        on_start: false,
        in_cell_edit: true,
        enabled: can_save,
    },
    Command {
        action: "FileSaveAs",
        label: "另存为",
        key: egui::Key::S,
        shift: true,
        on_start: false,
        in_cell_edit: true,
        enabled: can_save,
    },
    Command {
        action: "FileOpen",
        label: "打开",
        key: egui::Key::O,
        shift: false,
        on_start: true,
        in_cell_edit: true,
        enabled: can_shell,
    },
    Command {
        action: "NewTab",
        label: "新建",
        key: egui::Key::N,
        shift: false,
        on_start: true,
        in_cell_edit: true,
        enabled: can_shell,
    },
    Command {
        action: "CloseTab",
        label: "关闭工作簿",
        key: egui::Key::W,
        shift: false,
        on_start: false,
        in_cell_edit: true,
        enabled: can_save,
    },
    Command {
        action: "PrintPreview",
        label: "打印预览",
        key: egui::Key::P,
        shift: false,
        on_start: false,
        in_cell_edit: true,
        enabled: can_print,
    },
    Command {
        action: "Undo",
        label: "撤销",
        key: egui::Key::Z,
        shift: false,
        on_start: false,
        in_cell_edit: false,
        enabled: can_undo,
    },
    Command {
        action: "Redo",
        label: "重做",
        key: egui::Key::Y,
        shift: false,
        on_start: false,
        in_cell_edit: false,
        enabled: can_redo,
    },
];

/// 按 action 名查表。
pub fn by_action(action: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.action == action)
}

/// 功能区控件 id → 命令 action。多数控件 id 与 action 同名，
/// 只有打印预览一个动作挂了三个位置的按钮。
pub fn action_for_ctl(id: &str) -> &str {
    match id {
        "FilePrintPreview" | "PagePrintPreview" | "ViewPrintPreview" => "PrintPreview",
        _ => id,
    }
}

impl SheetApp {
    /// 当前键盘层级，优先级见模块文档。
    pub fn key_layer(&self, ctx: &egui::Context) -> Layer {
        if self.show_start {
            return Layer::Start;
        }
        if crate::ui_kit::take_dialog_shown(ctx) {
            return Layer::Dialog;
        }
        if self.editing.is_some() {
            return Layer::CellEdit;
        }
        Layer::Grid
    }

    /// 这一层是否允许执行该命令。
    pub fn command_allowed(&self, cmd: &Command, layer: Layer) -> bool {
        match layer {
            Layer::Start => cmd.on_start,
            Layer::Dialog => false,
            Layer::CellEdit => cmd.in_cell_edit,
            Layer::Grid => true,
        }
    }

    /// 命令此刻是否可用（功能区按钮与快捷键共用这一个判断）。
    pub fn command_enabled(&self, action: &str) -> bool {
        by_action(action).is_some_and(|c| (c.enabled)(self))
    }

    /// 不可用的原因，用于 tooltip 与状态栏；可用时返回 None。
    pub fn disabled_hint(&self, action: &str) -> Option<&'static str> {
        if self.command_enabled(action) {
            return None;
        }
        match action {
            "FileSave" | "FileSaveAs" | "CloseTab" if self.tasks.saving.is_some() => {
                Some("正在保存，完成后再试")
            }
            "FileSave" | "FileSaveAs" | "CloseTab" if self.tasks.open_rx.is_some() => {
                Some("正在加载，完成后再试")
            }
            "FileSave" | "FileSaveAs" | "CloseTab" | "PrintPreview" if self.show_start => {
                Some("先打开一个工作簿")
            }
            "FileOpen" | "NewTab" if self.tasks.open_rx.is_some() => Some("正在加载，完成后再试"),
            "Undo" => Some("没有可撤销的操作"),
            "Redo" => Some("没有可重做的操作"),
            _ => None,
        }
    }

    /// 执行一个登记过的命令；功能区、开始页、菜单与快捷键都走这里。
    /// 返回 false 表示不是登记命令（或此刻不可用），调用方可继续原有分支。
    pub fn command_exec(&mut self, action: &str, ctx: &egui::Context) -> bool {
        let Some(cmd) = by_action(action) else {
            return false;
        };
        if !(cmd.enabled)(self) {
            if let Some(hint) = self.disabled_hint(action) {
                self.status = hint.into();
            }
            return true;
        }
        // 编辑中先提交：不因为一个快捷键丢掉没提交的草稿；校验不通过就停下
        if cmd.in_cell_edit {
            self.commit_edit(None);
            if self.editing.is_some() {
                return true;
            }
        }
        match action {
            "FileSave" => self.save(false),
            "FileSaveAs" => self.save(true),
            "FileOpen" => self.open_dialog(),
            "NewTab" => self.new_document(),
            "CloseTab" => {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            "PrintPreview" => {
                self.preview_page = 0;
                self.print_preview = true;
            }
            "Undo" => self.do_undo(false),
            "Redo" => self.do_undo(true),
            _ => return false,
        }
        let _ = ctx;
        true
    }

    /// 应用级快捷键分发（开始页也调用；网格层继续走 `handle_keys`）。
    /// 返回 true 表示这一帧的按键已被命令表消费。
    pub fn handle_command_keys(&mut self, ctx: &egui::Context) -> bool {
        let layer = self.key_layer(ctx);
        for cmd in COMMANDS {
            let hit = ctx.input(|i| {
                i.modifiers.command
                    && !i.modifiers.alt
                    && i.key_pressed(cmd.key)
                    && i.modifiers.shift == cmd.shift
            });
            if !hit {
                continue;
            }
            if !self.command_allowed(cmd, layer) {
                return true;
            }
            self.command_exec(cmd.action, ctx);
            return true;
        }
        false
    }
}
