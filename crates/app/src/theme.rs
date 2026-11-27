//! 应用主题与运行时调色板（浅色 / 护眼 / 深色 / 高对比）。
//!
//! 通过全局原子槽存储当前主题，使 `ui_kit` / 表格渲染 / 对话框在不改动数百处函数签名的前提下
//! 实时读取当前主题配色。

use eframe::egui::{self, Color32, Stroke};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum AppTheme {
    #[default]
    Light = 0,
    EyeCare = 1,
    Dark = 2,
    HighContrast = 3,
}

impl AppTheme {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::EyeCare,
            2 => Self::Dark,
            3 => Self::HighContrast,
            _ => Self::Light,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "eyecare" | "eye_care" | "eye-care" | "护眼" => Self::EyeCare,
            "dark" | "深色" => Self::Dark,
            "high_contrast" | "high-contrast" | "hc" | "高对比" => Self::HighContrast,
            _ => Self::Light,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::EyeCare => "eyecare",
            Self::Dark => "dark",
            Self::HighContrast => "high_contrast",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "浅色（默认）",
            Self::EyeCare => "护眼豆沙绿",
            Self::Dark => "深色夜间",
            Self::HighContrast => "高对比度",
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark | Self::HighContrast)
    }

    pub const ALL: [Self; 4] = [Self::Light, Self::EyeCare, Self::Dark, Self::HighContrast];
}

#[derive(Clone, Copy, Debug)]
pub struct ThemePalette {
    pub theme: AppTheme,
    pub ribbon_bg: Color32,
    pub page_bg: Color32,
    pub card_bg: Color32,
    pub card_border: Color32,
    pub title_bg: Color32,
    pub status_bg: Color32,
    pub hover: Color32,
    pub hover_on_page: Color32,
    pub pressed: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_tint: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub text_muted: Color32,
    pub grid_bg: Color32,
    pub grid_line: Color32,
    pub header_bg: Color32,
    pub header_sel: Color32,
    pub header_text: Color32,
    pub default_cell_text: Color32,
    pub dialog_bg: Color32,
    pub dialog_scrim: Color32,
    pub warn_bg: Color32,
    pub warn_fg: Color32,
    pub err_bg: Color32,
    pub err_fg: Color32,
    pub info_bg: Color32,
    pub info_fg: Color32,
}

impl ThemePalette {
    /// Subtle separators for application chrome. High contrast keeps the full theme stroke.
    pub fn chrome_separator(self) -> Color32 {
        if self.theme == AppTheme::HighContrast {
            self.card_border
        } else {
            self.page_bg.lerp_to_gamma(self.card_border, 0.6)
        }
    }
}

#[cfg(not(test))]
static ACTIVE_THEME: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
thread_local! {
    static TEST_ACTIVE_THEME: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

pub fn current_theme() -> AppTheme {
    #[cfg(not(test))]
    {
        AppTheme::from_u8(ACTIVE_THEME.load(Ordering::Relaxed))
    }
    #[cfg(test)]
    {
        TEST_ACTIVE_THEME.with(|c| AppTheme::from_u8(c.get()))
    }
}

pub fn set_current_theme(theme: AppTheme) {
    #[cfg(not(test))]
    {
        ACTIVE_THEME.store(theme as u8, Ordering::Relaxed);
    }
    #[cfg(test)]
    {
        TEST_ACTIVE_THEME.with(|c| c.set(theme as u8));
    }
}

pub fn palette_for(theme: AppTheme) -> ThemePalette {
    match theme {
        AppTheme::Light => ThemePalette {
            theme,
            ribbon_bg: Color32::WHITE,
            page_bg: Color32::from_rgb(245, 246, 248),
            card_bg: Color32::WHITE,
            card_border: Color32::from_rgb(224, 227, 232),
            title_bg: Color32::from_rgb(240, 243, 248),
            status_bg: Color32::from_rgb(245, 246, 248),
            hover: Color32::from_rgb(241, 243, 246),
            hover_on_page: Color32::from_rgb(233, 236, 242),
            pressed: Color32::from_rgb(225, 229, 236),
            accent: crate::ui_kit::ACCENT,
            accent_hover: Color32::from_rgb(21, 108, 70),
            accent_tint: Color32::from_rgb(234, 246, 240),
            text: Color32::from_rgb(31, 35, 41),
            text_weak: Color32::from_rgb(100, 106, 115),
            text_muted: Color32::from_rgb(143, 149, 158),
            grid_bg: Color32::WHITE,
            grid_line: Color32::from_rgb(228, 231, 236),
            header_bg: Color32::from_rgb(247, 248, 250),
            header_sel: Color32::from_rgb(226, 242, 234),
            header_text: Color32::from_rgb(78, 86, 98),
            default_cell_text: Color32::from_rgb(24, 28, 34),
            dialog_bg: Color32::WHITE,
            dialog_scrim: Color32::from_black_alpha(40),
            warn_bg: Color32::from_rgb(255, 249, 240),
            warn_fg: Color32::from_rgb(154, 96, 14),
            err_bg: Color32::from_rgb(254, 242, 242),
            err_fg: Color32::from_rgb(185, 28, 28),
            info_bg: Color32::from_rgb(239, 246, 255),
            info_fg: Color32::from_rgb(29, 78, 216),
        },
        AppTheme::EyeCare => ThemePalette {
            theme,
            ribbon_bg: Color32::from_rgb(232, 244, 232),
            page_bg: Color32::from_rgb(221, 236, 221),
            card_bg: Color32::from_rgb(236, 246, 236),
            card_border: Color32::from_rgb(188, 212, 189),
            title_bg: Color32::from_rgb(214, 232, 215),
            status_bg: Color32::from_rgb(221, 236, 221),
            hover: Color32::from_rgb(209, 229, 211),
            hover_on_page: Color32::from_rgb(202, 224, 204),
            pressed: Color32::from_rgb(190, 216, 193),
            accent: Color32::from_rgb(24, 118, 76),
            accent_hover: Color32::from_rgb(19, 98, 63),
            accent_tint: Color32::from_rgb(203, 232, 212),
            text: Color32::from_rgb(30, 44, 34),
            text_weak: Color32::from_rgb(76, 98, 82),
            text_muted: Color32::from_rgb(112, 134, 118),
            grid_bg: Color32::from_rgb(236, 247, 236),
            grid_line: Color32::from_rgb(201, 222, 202),
            header_bg: Color32::from_rgb(225, 239, 225),
            header_sel: Color32::from_rgb(195, 227, 204),
            header_text: Color32::from_rgb(58, 78, 63),
            default_cell_text: Color32::from_rgb(26, 40, 30),
            dialog_bg: Color32::from_rgb(236, 246, 236),
            dialog_scrim: Color32::from_rgba_unmultiplied(15, 32, 20, 42),
            warn_bg: Color32::from_rgb(252, 244, 216),
            warn_fg: Color32::from_rgb(142, 88, 12),
            err_bg: Color32::from_rgb(252, 234, 234),
            err_fg: Color32::from_rgb(176, 28, 28),
            info_bg: Color32::from_rgb(224, 240, 248),
            info_fg: Color32::from_rgb(24, 82, 152),
        },
        AppTheme::Dark => ThemePalette {
            theme,
            ribbon_bg: Color32::from_rgb(32, 35, 42),
            page_bg: Color32::from_rgb(24, 27, 33),
            card_bg: Color32::from_rgb(36, 40, 48),
            card_border: Color32::from_rgb(56, 62, 74),
            title_bg: Color32::from_rgb(22, 25, 31),
            status_bg: Color32::from_rgb(26, 29, 36),
            hover: Color32::from_rgb(48, 53, 64),
            hover_on_page: Color32::from_rgb(44, 49, 60),
            pressed: Color32::from_rgb(58, 64, 78),
            accent: Color32::from_rgb(46, 178, 118),
            accent_hover: Color32::from_rgb(60, 196, 134),
            accent_tint: Color32::from_rgb(28, 62, 48),
            text: Color32::from_rgb(232, 236, 242),
            text_weak: Color32::from_rgb(164, 172, 186),
            text_muted: Color32::from_rgb(120, 128, 142),
            grid_bg: Color32::from_rgb(28, 31, 38),
            grid_line: Color32::from_rgb(48, 53, 64),
            header_bg: Color32::from_rgb(36, 40, 48),
            header_sel: Color32::from_rgb(34, 74, 56),
            header_text: Color32::from_rgb(196, 204, 216),
            default_cell_text: Color32::from_rgb(236, 240, 246),
            dialog_bg: Color32::from_rgb(34, 38, 46),
            dialog_scrim: Color32::from_rgba_unmultiplied(0, 0, 0, 120),
            warn_bg: Color32::from_rgb(64, 48, 18),
            warn_fg: Color32::from_rgb(252, 211, 77),
            err_bg: Color32::from_rgb(72, 26, 28),
            err_fg: Color32::from_rgb(252, 165, 165),
            info_bg: Color32::from_rgb(24, 46, 76),
            info_fg: Color32::from_rgb(147, 197, 253),
        },
        AppTheme::HighContrast => ThemePalette {
            theme,
            ribbon_bg: Color32::BLACK,
            page_bg: Color32::BLACK,
            card_bg: Color32::from_rgb(12, 12, 12),
            card_border: Color32::from_rgb(240, 240, 240),
            title_bg: Color32::BLACK,
            status_bg: Color32::BLACK,
            hover: Color32::from_rgb(36, 36, 36),
            hover_on_page: Color32::from_rgb(36, 36, 36),
            pressed: Color32::from_rgb(64, 64, 64),
            accent: Color32::from_rgb(255, 230, 0),
            accent_hover: Color32::from_rgb(255, 245, 90),
            accent_tint: Color32::from_rgb(48, 42, 0),
            text: Color32::WHITE,
            text_weak: Color32::from_rgb(225, 225, 225),
            text_muted: Color32::from_rgb(190, 190, 190),
            grid_bg: Color32::BLACK,
            grid_line: Color32::from_rgb(130, 130, 130),
            header_bg: Color32::from_rgb(18, 18, 18),
            header_sel: Color32::from_rgb(60, 54, 0),
            header_text: Color32::WHITE,
            default_cell_text: Color32::WHITE,
            dialog_bg: Color32::BLACK,
            dialog_scrim: Color32::from_rgba_unmultiplied(0, 0, 0, 165),
            warn_bg: Color32::from_rgb(46, 36, 0),
            warn_fg: Color32::from_rgb(255, 230, 0),
            err_bg: Color32::from_rgb(64, 0, 0),
            err_fg: Color32::from_rgb(255, 110, 110),
            info_bg: Color32::from_rgb(0, 32, 68),
            info_fg: Color32::from_rgb(120, 215, 255),
        },
    }
}

#[inline]
pub fn palette() -> ThemePalette {
    palette_for(current_theme())
}

/// 将指定主题同步到 `egui::Context` 的 `Visuals` 与全局运行时调色板。
pub fn apply_app_theme(ctx: &egui::Context, theme: AppTheme) {
    set_current_theme(theme);
    let p = palette_for(theme);
    let egui_theme = if theme.is_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.style_mut_of(egui_theme, |s| {
        let v = &mut s.visuals;
        v.dark_mode = theme.is_dark();
        v.panel_fill = p.page_bg;
        v.window_fill = p.dialog_bg;
        v.faint_bg_color = p.hover;
        v.extreme_bg_color = p.grid_bg;
        // 浅色/护眼主题不强写 override_text_color（保留控件各自语义色），深色/高对比主题确保默认文本清晰可见。
        v.override_text_color = if theme.is_dark() { Some(p.text) } else { None };
        match theme {
            AppTheme::Light => {
                v.selection.bg_fill = Color32::from_rgb(186, 224, 204);
                v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(20, 74, 45));
                v.text_cursor.stroke = Stroke::new(1.25, p.text);
                v.hyperlink_color = Color32::from_rgb(5, 99, 193);
            }
            AppTheme::EyeCare => {
                v.selection.bg_fill = Color32::from_rgb(176, 218, 186);
                v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(20, 74, 45));
                v.text_cursor.stroke = Stroke::new(1.25, p.text);
                v.hyperlink_color = Color32::from_rgb(14, 92, 168);
            }
            AppTheme::Dark => {
                v.selection.bg_fill = Color32::from_rgb(44, 96, 148);
                v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
                v.text_cursor.stroke = Stroke::new(1.25, Color32::WHITE);
                v.hyperlink_color = Color32::from_rgb(120, 186, 255);
            }
            AppTheme::HighContrast => {
                v.selection.bg_fill = Color32::from_rgb(0, 90, 180);
                v.selection.stroke = Stroke::new(1.5, Color32::WHITE);
                v.text_cursor.stroke = Stroke::new(1.5, Color32::from_rgb(255, 230, 0));
                v.hyperlink_color = Color32::from_rgb(120, 225, 255);
            }
        }
        v.widgets.noninteractive.bg_fill = p.card_bg;
        v.widgets.noninteractive.weak_bg_fill = p.card_bg;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.card_border);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.inactive.bg_fill = if theme == AppTheme::Light {
            Color32::from_rgb(245, 247, 250)
        } else {
            p.card_bg
        };
        v.widgets.inactive.weak_bg_fill = p.card_bg;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.card_border);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
        v.widgets.hovered.bg_fill = p.hover;
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
        v.widgets.active.bg_fill = p.pressed;
        v.widgets.active.weak_bg_fill = p.pressed;
        v.widgets.active.bg_stroke = Stroke::new(1.5, p.accent);
        v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.active.corner_radius = egui::CornerRadius::same(6);
        v.widgets.open = v.widgets.active;
        v.window_stroke = Stroke::new(1.0, p.card_border);
    });
    ctx.set_theme(if theme.is_dark() {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
}
