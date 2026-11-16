//! UI-29.1 设计 token：间距、圆角、描边、字号、图标光学尺寸。
//!
//! 为什么单独成文件：此前这些数字散落在 `ui_kit.rs` / `ribbon/ui.rs` / `main.rs`
//! 的各处字面量里，改一处间距要全局搜，也没法在评审里说「这套 UI 用的是 4 的倍数栅格」。
//! 这里的值全部是 **逻辑点**（egui point），DPI 缩放由 `pixels_per_point` 统一乘，
//! 所以 100%/150%/200% 下物理尺寸必须严格按倍率走 —— 有测试守着这一点。

/// 间距：4 的倍数栅格，2 是半分（图标与文字之间这类必须更紧的地方）
pub mod space {
    /// 半分：图标和标签之间、分隔线两侧
    pub const HAIR: f32 = 2.0;
    /// 1×：控件内部 padding
    pub const XS: f32 = 4.0;
    /// 1.5×：图标与文字、分组内相邻控件
    pub const M: f32 = 6.0;
    /// 2×：控件之间、行与行之间
    pub const L: f32 = 8.0;
    /// 3×：分组之间、对话框内容边距
    pub const XL: f32 = 12.0;
    /// 4×：对话框分节
    pub const XXL: f32 = 16.0;
    /// 6×：窗口级留白
    pub const XXXL: f32 = 24.0;
    /// 分组名与命令区之间（Excel 实测 7pt：6 + 1 条基线间隙）
    pub const GROUP_LABEL_GAP: f32 = 7.0;
    /// 分组左内边距（Excel 实测 6pt）
    pub const GROUP_PAD_X: f32 = 6.0;
    /// 分组顶部内边距（Excel 实测 4pt）
    pub const GROUP_PAD_TOP: f32 = 4.0;
}

/// 圆角：Office 的控件基本是 2（小控件）/3（输入框）/4（对话框）
pub mod radius {
    pub const SM: f32 = 2.0;
    pub const MD: f32 = 3.0;
    pub const LG: f32 = 4.0;
}

/// 描边：1px 常规线；1.5px 焦点圈（在 100% 下能看见又不糊）
pub mod stroke {
    pub const HAIR: f32 = 1.0;
    pub const FOCUS: f32 = 1.5;
}

/// 字号（逻辑点）：正文 12、次要 11、标题 13
pub mod text {
    pub const SMALL: f32 = 11.0;
    pub const BODY: f32 = 12.0;
    pub const TITLE: f32 = 13.0;
}

/// 控件尺寸（逻辑点）：按钮/工作表标签这类固定盒子
pub mod size {
    /// 对话框主/次按钮的最小宽度（Excel 是 75×24）
    pub const BTN_MIN_W: f32 = 75.0;
    /// 标准按钮高度
    pub const BTN_H: f32 = 24.0;
    /// 工作表标签高度（Excel 365 实测 32）
    pub const SHEET_TAB_H: f32 = 32.0;
    /// 工作表标签左右留白（字宽 + 26）
    pub const SHEET_TAB_PAD: f32 = 26.0;
}

/// 图标光学尺寸：16/20/24/32 四档，UI-29.2 逐个尺寸验证清晰度
pub mod icon {
    /// 分组对话框启动器那种斜箭头
    pub const XS: f32 = 12.0;
    pub const S: f32 = 16.0;
    pub const M: f32 = 20.0;
    pub const L: f32 = 24.0;
    pub const XL: f32 = 32.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// token 是契约：改了值必须同步 `ui29-tokens.md` 和测量基线
    #[test]
    fn tokens_are_stable() {
        assert_eq!(space::HAIR, 2.0);
        assert_eq!(space::XS, 4.0);
        assert_eq!(space::M, 6.0);
        assert_eq!(space::L, 8.0);
        assert_eq!(space::XL, 12.0);
        assert_eq!(space::XXL, 16.0);
        assert_eq!(space::XXXL, 24.0);
        // 间距必须是 2 的整数倍（栅格自证）
        for v in [
            space::HAIR,
            space::XS,
            space::M,
            space::L,
            space::XL,
            space::XXL,
            space::XXXL,
        ] {
            assert_eq!((v / 2.0).fract(), 0.0, "间距 {v} 不在 2pt 栅格上");
        }
        assert_eq!(radius::SM, 2.0);
        assert_eq!(radius::MD, 3.0);
        assert_eq!(radius::LG, 4.0);
        assert_eq!(stroke::HAIR, 1.0);
        assert_eq!(stroke::FOCUS, 1.5);
        assert_eq!(text::SMALL, 11.0);
        assert_eq!(text::BODY, 12.0);
        assert_eq!(text::TITLE, 13.0);
        assert_eq!(size::BTN_MIN_W, 75.0);
        assert_eq!(size::BTN_H, 24.0);
        assert_eq!(size::SHEET_TAB_H, 32.0);
        assert_eq!(size::SHEET_TAB_PAD, 26.0);
        assert_eq!(icon::XS, 12.0);
        assert_eq!(icon::S, 16.0);
        assert_eq!(icon::M, 20.0);
        assert_eq!(icon::L, 24.0);
        assert_eq!(icon::XL, 32.0);
    }
}
