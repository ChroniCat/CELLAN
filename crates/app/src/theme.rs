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

    /// Subtle secondary surface inside floating cards/panels (headers, inset containers, idle inputs).
    pub fn surface_subtle(self) -> Color32 {
        match self.theme {
            AppTheme::Light => Color32::from_rgb(248, 249, 252),
            AppTheme::EyeCare => Color32::from_rgb(230, 241, 231),
            AppTheme::Dark => Color32::from_rgb(28, 32, 40),
            AppTheme::HighContrast => Color32::from_rgb(18, 18, 18),
        }
    }

    /// Theme-aware floating card shadow color.
    pub fn shadow_color(self) -> Color32 {
        match self.theme {
            AppTheme::Light => Color32::from_black_alpha(14),
            AppTheme::EyeCare => Color32::from_rgba_unmultiplied(15, 32, 20, 16),
            AppTheme::Dark => Color32::from_black_alpha(48),
            AppTheme::HighContrast => Color32::TRANSPARENT,
        }
    }

    /// Soft accent outline for active badges, user bubbles, and callout banners.
    pub fn accent_border(self) -> Color32 {
        if self.theme == AppTheme::HighContrast {
            self.accent
        } else {
            self.card_border.lerp_to_gamma(self.accent, 0.45)
        }
    }

    /// Semantic callout border for warning banners/cards.
    pub fn warn_border(self) -> Color32 {
        if self.theme == AppTheme::HighContrast {
            self.warn_fg
        } else {
            self.warn_bg.lerp_to_gamma(self.warn_fg, 0.45)
        }
    }

    /// Semantic callout border for error banners/cards.
    pub fn err_border(self) -> Color32 {
        if self.theme == AppTheme::HighContrast {
            self.err_fg
        } else {
            self.err_bg.lerp_to_gamma(self.err_fg, 0.42)
        }
    }

    /// Semantic callout border for informational badges/banners.
    #[allow(dead_code)]
    pub fn info_border(self) -> Color32 {
        if self.theme == AppTheme::HighContrast {
            self.info_fg
        } else {
            self.info_bg.lerp_to_gamma(self.info_fg, 0.38)
        }
    }

    /// Accessible brand navy/blue ink for assistant accents and context badges across all themes.
    pub fn brand_ink(self) -> Color32 {
        match self.theme {
            AppTheme::Light => crate::ui_kit::BRAND_NAVY,
            AppTheme::EyeCare => Color32::from_rgb(24, 82, 152),
            AppTheme::Dark => Color32::from_rgb(122, 174, 255),
            AppTheme::HighContrast => self.info_fg,
        }
    }

    /// High-contrast foreground color for text placed on solid `self.accent` fills.
    pub fn on_accent(self) -> Color32 {
        match self.theme {
            AppTheme::Light | AppTheme::EyeCare => Color32::WHITE,
            AppTheme::Dark => Color32::from_rgb(10, 24, 18),
            AppTheme::HighContrast => Color32::BLACK,
        }
    }

    /// High-contrast foreground color for text placed on solid `self.err_fg` fills.
    pub fn on_err(self) -> Color32 {
        if self.theme.is_dark() {
            Color32::from_rgb(24, 16, 16)
        } else {
            Color32::WHITE
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
            page_bg: Color32::from_rgb(243, 245, 248),
            card_bg: Color32::WHITE,
            card_border: Color32::from_rgb(218, 223, 231),
            title_bg: Color32::from_rgb(243, 245, 248),
            status_bg: Color32::from_rgb(245, 247, 250),
            hover: Color32::from_rgb(238, 242, 247),
            hover_on_page: Color32::from_rgb(231, 236, 243),
            pressed: Color32::from_rgb(222, 228, 237),
            accent: crate::ui_kit::ACCENT,
            accent_hover: Color32::from_rgb(24, 102, 62),
            accent_tint: Color32::from_rgb(232, 245, 238),
            text: Color32::from_rgb(26, 31, 39),
            text_weak: Color32::from_rgb(92, 100, 112),
            text_muted: Color32::from_rgb(138, 146, 158),
            grid_bg: Color32::WHITE,
            grid_line: Color32::from_rgb(226, 230, 236),
            header_bg: Color32::from_rgb(247, 248, 251),
            header_sel: Color32::from_rgb(224, 241, 232),
            header_text: Color32::from_rgb(76, 84, 96),
            default_cell_text: Color32::from_rgb(22, 26, 33),
            dialog_bg: Color32::WHITE,
            dialog_scrim: Color32::from_black_alpha(38),
            warn_bg: Color32::from_rgb(255, 248, 235),
            warn_fg: Color32::from_rgb(154, 88, 12),
            err_bg: Color32::from_rgb(254, 242, 242),
            err_fg: Color32::from_rgb(185, 28, 28),
            info_bg: Color32::from_rgb(238, 245, 255),
            info_fg: Color32::from_rgb(29, 78, 216),
        },
        AppTheme::EyeCare => ThemePalette {
            theme,
            ribbon_bg: Color32::from_rgb(236, 245, 236),
            page_bg: Color32::from_rgb(224, 237, 225),
            card_bg: Color32::from_rgb(239, 247, 239),
            card_border: Color32::from_rgb(189, 212, 192),
            title_bg: Color32::from_rgb(224, 237, 225),
            status_bg: Color32::from_rgb(226, 238, 227),
            hover: Color32::from_rgb(212, 230, 214),
            hover_on_page: Color32::from_rgb(205, 225, 207),
            pressed: Color32::from_rgb(193, 217, 196),
            accent: Color32::from_rgb(24, 118, 76),
            accent_hover: Color32::from_rgb(19, 98, 63),
            accent_tint: Color32::from_rgb(206, 234, 215),
            text: Color32::from_rgb(26, 40, 30),
            text_weak: Color32::from_rgb(72, 94, 78),
            text_muted: Color32::from_rgb(108, 130, 114),
            grid_bg: Color32::from_rgb(238, 248, 238),
            grid_line: Color32::from_rgb(202, 223, 204),
            header_bg: Color32::from_rgb(227, 240, 228),
            header_sel: Color32::from_rgb(197, 228, 206),
            header_text: Color32::from_rgb(56, 76, 61),
            default_cell_text: Color32::from_rgb(24, 38, 28),
            dialog_bg: Color32::from_rgb(239, 247, 239),
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
            ribbon_bg: Color32::from_rgb(31, 35, 43),
            page_bg: Color32::from_rgb(22, 25, 31),
            card_bg: Color32::from_rgb(33, 37, 46),
            card_border: Color32::from_rgb(55, 62, 76),
            title_bg: Color32::from_rgb(22, 25, 31),
            status_bg: Color32::from_rgb(25, 28, 35),
            hover: Color32::from_rgb(46, 52, 64),
            hover_on_page: Color32::from_rgb(39, 44, 55),
            pressed: Color32::from_rgb(56, 63, 78),
            accent: Color32::from_rgb(48, 184, 122),
            accent_hover: Color32::from_rgb(64, 202, 138),
            accent_tint: Color32::from_rgb(26, 58, 45),
            text: Color32::from_rgb(236, 240, 246),
            text_weak: Color32::from_rgb(164, 174, 190),
            text_muted: Color32::from_rgb(118, 128, 144),
            grid_bg: Color32::from_rgb(26, 29, 36),
            grid_line: Color32::from_rgb(46, 52, 63),
            header_bg: Color32::from_rgb(34, 38, 47),
            header_sel: Color32::from_rgb(32, 70, 54),
            header_text: Color32::from_rgb(194, 203, 216),
            default_cell_text: Color32::from_rgb(238, 242, 248),
            dialog_bg: Color32::from_rgb(33, 37, 46),
            dialog_scrim: Color32::from_rgba_unmultiplied(0, 0, 0, 128),
            warn_bg: Color32::from_rgb(62, 46, 18),
            warn_fg: Color32::from_rgb(252, 211, 77),
            err_bg: Color32::from_rgb(68, 26, 29),
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
            p.surface_subtle()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rel_lum(c: Color32) -> f64 {
        let ch = |v: u8| {
            let s = v as f64 / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (la, lb) = (rel_lum(a), rel_lum(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn all_themes_meet_wcag_contrast_and_surface_separation() {
        let ctx = egui::Context::default();
        for theme in AppTheme::ALL {
            apply_app_theme(&ctx, theme);
            let p = palette_for(theme);
            assert!(
                contrast(p.text, p.card_bg) >= 7.0,
                "{theme:?} text/card_bg contrast too low"
            );
            assert!(
                contrast(p.text, p.dialog_bg) >= 7.0,
                "{theme:?} text/dialog_bg contrast too low"
            );
            assert!(
                contrast(p.text, p.surface_subtle()) >= 7.0,
                "{theme:?} text/surface_subtle contrast too low"
            );
            assert!(
                contrast(p.text_weak, p.card_bg) >= 4.5,
                "{theme:?} text_weak/card_bg contrast too low"
            );
            assert!(
                contrast(p.on_accent(), p.accent) >= 4.5,
                "{theme:?} on_accent/accent contrast too low"
            );
            assert!(
                contrast(p.on_err(), p.err_fg) >= 4.5,
                "{theme:?} on_err/err_fg contrast too low"
            );
            assert!(
                contrast(p.brand_ink(), p.card_bg) >= 4.5,
                "{theme:?} brand_ink/card_bg contrast too low"
            );
            let st = ctx.style();
            assert!(
                contrast(st.visuals.selection.bg_fill, st.visuals.selection.stroke.color) >= 4.5,
                "{theme:?} selection contrast too low"
            );
        }
        apply_app_theme(&ctx, AppTheme::Light);
    }
}
