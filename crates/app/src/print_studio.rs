//! 出版级打印与预览工作室（超越 Excel / WPS）：
//! - 全矢量 GPU 实时预览（完整呈现 13 种边框、富文本、斜体/粗体/下划线/删除线/旋转、
//!   条件格式图标、迷你图、折线/柱形/饼图、真实高清图片与矢量水印）；
//! - 单页 / 双页对开对比视图 + 左侧页面缩略图导航 + 纸面直接拖拽页边距实时标尺；
//! - 原生 Windows GDI 硬件矢量打印（整本工作簿 / 当前表 / 选区 / 非连续页码 `"1,3-5,8"` + 系统打印机属性）；
//! - 零豆腐块多字体回退链 PDF（自动加载 Windows `.ttc` CJK 字体 + 整簿侧边栏 `/Outlines` 书签目录）；
//! - 150 / 300 DPI 高清 PNG 出版图导出 + 可视化左/中/右三区页眉页脚设计器 + 水印工作室。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use eframe::egui::{self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use sheetcore::page::{PageSetup, PAPER_SIZES};
use sheetcore::print::{
    comment_summary_pages, header_parts_with_ctx, page_ops_with, paginate, parse_page_ranges,
    HeaderContext, Layout, Out, PdfFontPack,
};

use crate::{native_print, ui_kit, SheetApp, FONT_SANS, FONT_SANS_BOLD};

/// 可视化三区页眉页脚设计器草稿
#[derive(Clone, Debug, Default)]
pub(crate) struct HfDraft {
    pub sheet_id: u64,
    /// 0 = 常规，1 = 首页，2 = 偶数页
    pub tab: u8,
    /// 当前焦点区（0..=2 页眉左/中/右，3..=5 页脚左/中/右）
    pub active_zone: u8,
    pub different_first: bool,
    pub different_odd_even: bool,
    pub header: [String; 3],
    pub footer: [String; 3],
    pub first_header: [String; 3],
    pub first_footer: [String; 3],
    pub even_header: [String; 3],
    pub even_footer: [String; 3],
}

/// 打印工作室持久 UI 状态
pub(crate) struct PrintStudioState {
    /// 打印范围作用域：0 = 当前工作表，1 = 整个工作簿（所有可见表），2 = 仅当前选区
    pub scope: u8,
    /// 智能自定义页码范围串（如 `"1, 3-5, 8"`；空串表示全部）
    pub page_range_text: String,
    /// 双面打印：0 = 打印机默认，1 = 单面，2 = 双面长边翻转，3 = 双面短边翻转
    pub duplex: u8,
    /// 逐份打印
    pub collate: bool,
    /// 双页对开视图
    pub facing_pages: bool,
    /// 左侧缩略图导航条
    pub show_thumbnails: bool,
    /// 纸面可拖拽页边距参考线
    pub show_margin_guides: bool,
    /// 高清 PNG 导出 DPI（150 或 300）
    pub png_dpi: u32,
    /// 水印配置面板展开开关
    pub watermark_open: bool,
    /// 系统原厂打印机属性对话框返回的 DEVMODEW 缓冲
    pub devmode_override: Option<Vec<u8>>,
    /// 预览中嵌入图片的 GPU 纹理缓存
    pub preview_textures: HashMap<String, egui::TextureHandle>,
    /// 可视化三区页眉页脚设计器草稿
    pub hf_draft: Option<HfDraft>,
}

impl Default for PrintStudioState {
    fn default() -> Self {
        Self {
            scope: 0,
            page_range_text: String::new(),
            duplex: 0,
            collate: true,
            facing_pages: false,
            show_thumbnails: true,
            show_margin_guides: true,
            png_dpi: 300,
            watermark_open: false,
            devmode_override: None,
            preview_textures: HashMap::new(),
            hf_draft: None,
        }
    }
}

/// 将 Excel 页眉/页脚字符串拆为 `(左区, 中区, 右区)`（无标记默认落入中区）
pub(crate) fn split_hf_zones(raw: &str) -> [String; 3] {
    let mut out = [String::new(), String::new(), String::new()];
    let mut zone = 1usize;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            match chars.peek().copied() {
                Some('L' | 'l') => {
                    chars.next();
                    zone = 0;
                    continue;
                }
                Some('C' | 'c') => {
                    chars.next();
                    zone = 1;
                    continue;
                }
                Some('R' | 'r') => {
                    chars.next();
                    zone = 2;
                    continue;
                }
                _ => {}
            }
        }
        out[zone].push(ch);
    }
    out
}

/// 将 `(左区, 中区, 右区)` 拼回标准 OOXML `&L...&C...&R...` 字符串
pub(crate) fn compose_hf_zones(zones: &[String; 3]) -> String {
    let l = zones[0].trim();
    let c = zones[1].trim();
    let r = zones[2].trim();
    if l.is_empty() && c.is_empty() && r.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    if !l.is_empty() {
        s.push_str("&L");
        s.push_str(l);
    }
    if !c.is_empty() {
        s.push_str("&C");
        s.push_str(c);
    }
    if !r.is_empty() {
        s.push_str("&R");
        s.push_str(r);
    }
    s
}

/// 进程级缓存 Windows 系统 CJK `.ttc`/`.ttf` 回退字体，用于 PDF 零豆腐块多字体回退链（`/F3` `/F4`）
fn cached_system_cjk_fallback() -> Option<(sheetcore::pdf::Font, Option<sheetcore::pdf::Font>)> {
    static FALLBACK: OnceLock<Option<(sheetcore::pdf::Font, Option<sheetcore::pdf::Font>)>> =
        OnceLock::new();
    FALLBACK
        .get_or_init(|| {
            let win = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
            let dir = PathBuf::from(win).join("Fonts");
            let reg_candidates = ["msyh.ttc", "simsun.ttc", "Deng.ttf", "simhei.ttf"];
            let bold_candidates = ["msyhbd.ttc", "Dengb.ttf"];
            let mut reg = None;
            for name in reg_candidates {
                let p = dir.join(name);
                if p.exists()
                    && let Ok(f) = sheetcore::pdf::Font::load(&p)
                {
                    reg = Some(f);
                    break;
                }
            }
            let reg = reg?;
            let mut bold = None;
            for name in bold_candidates {
                let p = dir.join(name);
                if p.exists()
                    && let Ok(f) = sheetcore::pdf::Font::load(&p)
                {
                    bold = Some(f);
                    break;
                }
            }
            Some((reg, bold))
        })
        .clone()
}

/// 组装完整的出版级 PDF 字体包：内嵌 `NotoSansSC Regular/Bold` + 自动系统 CJK `.ttc` 回退链
pub(crate) fn build_pdf_font_pack() -> anyhow::Result<PdfFontPack> {
    let regular = sheetcore::pdf::Font::parse_static(FONT_SANS)?;
    let bold = sheetcore::pdf::Font::parse_static(FONT_SANS_BOLD).ok();
    let (fallback, fallback_bold) = match cached_system_cjk_fallback() {
        Some((f, fb)) => (Some(f), fb),
        None => (None, None),
    };
    Ok(PdfFontPack {
        regular,
        bold,
        fallback,
        fallback_bold,
    })
}

/// 构建当前工作簿与工作表的页眉页脚上下文（供页面布局视图和页眉设计器实时预览）
pub(crate) fn make_header_ctx(wb: &sheetcore::Workbook, sheet_idx: usize) -> HeaderContext {
    HeaderContext::for_workbook(wb, sheet_idx)
}

/// 将 `sheetcore::print::Out` 矢量显示列表完整绘制到 `egui::Painter` 纸张矩形上
/// （预览、缩略图、双页对开 100% 复用同一管线）
pub(crate) fn paint_out_on_egui(
    ctx: &egui::Context,
    painter: &egui::Painter,
    page_rect: Rect,
    ph_pt: f32,
    ops: &Out,
    textures: &mut HashMap<String, egui::TextureHandle>,
    is_thumbnail: bool,
) {
    let s_pt = page_rect.height() / ph_pt.max(1.0);
    let to_xy = |x: f32, y: f32| -> Pos2 {
        egui::pos2(
            page_rect.min.x + x * s_pt,
            page_rect.min.y + (ph_pt - y) * s_pt,
        )
    };

    // 1. 实心矩形
    for &(x, y, w, h, rgb) in &ops.rects {
        let a = to_xy(x, y + h);
        let b = to_xy(x + w, y);
        painter.rect_filled(
            Rect::from_min_max(a, b),
            0.0,
            Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
        );
    }

    // 2. 半透明矩形（数据条 / 透明填充）
    for &(x, y, w, h, rgb, alpha) in &ops.alpha_rects {
        let a = to_xy(x, y + h);
        let b = to_xy(x + w, y);
        let a8 = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        painter.rect_filled(
            Rect::from_min_max(a, b),
            0.0,
            Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], a8),
        );
    }

    // 3. 填充多边形（饼图扇区、雷达面积、条件格式图标、批注红三角）
    for (pts, rgb) in &ops.polys {
        if pts.len() < 3 {
            continue;
        }
        let col = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        let mut mesh = egui::Mesh::default();
        for &(px, py) in pts {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: to_xy(px, py),
                uv: egui::epaint::WHITE_UV,
                color: col,
            });
        }
        for i in 1..(pts.len() - 1) {
            mesh.indices.push(0);
            mesh.indices.push(i as u32);
            mesh.indices.push((i + 1) as u32);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    // 4. 真实图片（按内容指纹缓存 TextureHandle，完整还原裁剪比例与顺时针旋转角度）
    for (idx, (x, y, w, h, name, bytes, _ext)) in ops.images.iter().enumerate() {
        let (crop, rot_deg) = ops
            .image_transforms
            .get(idx)
            .copied()
            .unwrap_or(([0.0; 4], 0.0));
        let key = format!("print_img:{name}:{}", bytes.len());
        if !textures.contains_key(&key) {
            if textures.len() > 48 {
                textures.clear();
            }
            if let Ok(img) = image::load_from_memory(bytes) {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let color_img = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                let handle = ctx.load_texture(key.clone(), color_img, egui::TextureOptions::LINEAR);
                textures.insert(key.clone(), handle);
            }
        }
        let dest = Rect::from_min_max(to_xy(*x, *y + *h), to_xy(*x + *w, *y));
        if let Some(tex) = textures.get(&key) {
            let u0 = crop[0].clamp(0.0, 0.9);
            let v0 = crop[1].clamp(0.0, 0.9);
            let u1 = (1.0 - crop[2]).clamp((u0 + 0.02).min(1.0), 1.0);
            let v1 = (1.0 - crop[3]).clamp((v0 + 0.02).min(1.0), 1.0);
            let uv = Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u1, v1));
            let rot = rot_deg.rem_euclid(360.0);
            if rot > 0.01 && (360.0 - rot) > 0.01 {
                let rad = rot.to_radians();
                let (sin_a, cos_a) = rad.sin_cos();
                let c = dest.center();
                let rot_pt = |pt: Pos2| -> Pos2 {
                    let dx = pt.x - c.x;
                    let dy = pt.y - c.y;
                    Pos2::new(c.x + dx * cos_a - dy * sin_a, c.y + dx * sin_a + dy * cos_a)
                };
                let mut mesh = egui::Mesh::with_texture(tex.id());
                mesh.vertices.push(egui::epaint::Vertex { pos: rot_pt(dest.left_top()), uv: uv.left_top(), color: Color32::WHITE });
                mesh.vertices.push(egui::epaint::Vertex { pos: rot_pt(dest.right_top()), uv: uv.right_top(), color: Color32::WHITE });
                mesh.vertices.push(egui::epaint::Vertex { pos: rot_pt(dest.right_bottom()), uv: uv.right_bottom(), color: Color32::WHITE });
                mesh.vertices.push(egui::epaint::Vertex { pos: rot_pt(dest.left_bottom()), uv: uv.left_bottom(), color: Color32::WHITE });
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(egui::Shape::mesh(mesh));
            } else {
                painter.image(tex.id(), dest, uv, Color32::WHITE);
            }
        } else {
            painter.rect_stroke(
                dest,
                0.0,
                Stroke::new(0.8f32, Color32::from_gray(170)),
                egui::StrokeKind::Inside,
            );
        }
    }

    // 5. 实线（网格线、实线/双线边框、图表坐标轴）
    for &(x1, y1, x2, y2, lw, rgb) in &ops.lines {
        painter.line_segment(
            [to_xy(x1, y1), to_xy(x2, y2)],
            Stroke::new(
                (lw * s_pt).max(0.35),
                Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
            ),
        );
    }

    // 6. 虚线 / 点线 / 点划线（13 种 OOXML 边框 & 参考线）
    for sl in &ops.styled_lines {
        let p0 = to_xy(sl.x1, sl.y1);
        let p1 = to_xy(sl.x2, sl.y2);
        let stroke = Stroke::new(
            (sl.width * s_pt).max(0.4),
            Color32::from_rgb(sl.color[0], sl.color[1], sl.color[2]),
        );
        if sl.dash.is_empty() || is_thumbnail {
            painter.line_segment([p0, p1], stroke);
        } else {
            let d0 = (sl.dash[0] * s_pt).max(1.2);
            let d1 = (sl.dash.get(1).copied().unwrap_or(sl.dash[0]) * s_pt).max(1.0);
            painter.add(egui::Shape::dashed_line(&[p0, p1], stroke, d0, d1));
        }
    }

    // 7. 描边折线（折线图、散点连线、迷你图）
    for sp in &ops.stroke_polys {
        if sp.pts.len() < 2 {
            continue;
        }
        let pts: Vec<Pos2> = sp.pts.iter().map(|&(px, py)| to_xy(px, py)).collect();
        let stroke = Stroke::new(
            (sp.width * s_pt).max(0.5),
            Color32::from_rgb(sp.color[0], sp.color[1], sp.color[2]),
        );
        if sp.dash.is_empty() || is_thumbnail {
            painter.add(egui::Shape::line(pts, stroke));
        } else {
            let d0 = (sp.dash[0] * s_pt).max(1.5);
            let d1 = (sp.dash.get(1).copied().unwrap_or(sp.dash[0]) * s_pt).max(1.2);
            painter.add(egui::Shape::dashed_line(&pts, stroke, d0, d1));
        }
    }

    // 8. 矢量文字（含精准裁剪、粗体/斜体、下划线/双下划线/删除线、旋转角）
    for t in &ops.texts {
        if t.text.is_empty() {
            continue;
        }
        let px_size = (t.size * s_pt).max(if is_thumbnail { 1.5 } else { 3.0 });
        if is_thumbnail && px_size < 1.8 {
            continue;
        }
        let col = Color32::from_rgb(t.color[0], t.color[1], t.color[2]);
        let target_painter = match t.clip {
            Some([cx, cy, cw, ch]) => {
                let cr = Rect::from_min_max(to_xy(cx, cy + ch), to_xy(cx + cw, cy))
                    .intersect(page_rect);
                if cr.width() <= 0.2 || cr.height() <= 0.2 {
                    continue;
                }
                painter.with_clip_rect(cr)
            }
            None => painter.with_clip_rect(page_rect),
        };
        let fam = if t.bold {
            crate::family_named("default-bold")
        } else {
            crate::family_named("default")
        };
        let mut job = egui::text::LayoutJob::default();
        job.append(
            &t.text,
            0.0,
            egui::TextFormat {
                font_id: FontId::new(px_size, fam),
                color: col,
                italics: t.italic,
                underline: if t.underline && !t.double_underline {
                    Stroke::new((px_size * 0.065).max(0.5), col)
                } else {
                    Stroke::NONE
                },
                strikethrough: if t.strikethrough {
                    Stroke::new((px_size * 0.065).max(0.5), col)
                } else {
                    Stroke::NONE
                },
                ..Default::default()
            },
        );
        let galley = target_painter.layout_job(job);
        let base = to_xy(t.x, t.baseline);
        let top_left = egui::pos2(base.x, base.y - galley.size().y * 0.84);
        if t.rotation.abs() > 0.5 {
            let angle = -t.rotation.to_radians();
            target_painter.add(egui::Shape::Text(
                egui::epaint::TextShape::new(top_left, galley, col).with_angle(angle),
            ));
        } else {
            let gw = galley.size().x;
            target_painter.galley(top_left, galley, col);
            if t.double_underline {
                let lw = (px_size * 0.055).max(0.5);
                let y1 = base.y + (px_size * 0.08).max(0.8);
                let y2 = y1 + (lw * 2.1).max(1.4);
                target_painter.line_segment(
                    [egui::pos2(base.x, y1), egui::pos2(base.x + gw, y1)],
                    Stroke::new(lw, col),
                );
                target_painter.line_segment(
                    [egui::pos2(base.x, y2), egui::pos2(base.x + gw, y2)],
                    Stroke::new(lw, col),
                );
            }
        }
    }

    // 9. 半透明斜角 / 平铺矢量水印
    if !ops.watermarks.is_empty() {
        let wp = painter.with_clip_rect(page_rect);
        for wm in &ops.watermarks {
            let px_size = (wm.size * s_pt).max(5.0);
            let a8 = (wm.opacity.clamp(0.02, 0.9) * 255.0).round() as u8;
            let col = Color32::from_rgba_unmultiplied(wm.color[0], wm.color[1], wm.color[2], a8);
            let galley = wp.layout_no_wrap(
                wm.text.clone(),
                FontId::new(px_size, crate::family_named("default-bold")),
                col,
            );
            let angle = -wm.angle_deg.to_radians();
            let (sa, ca) = angle.sin_cos();
            let hx = galley.size().x * 0.5;
            let hy = galley.size().y * 0.5;
            let center = to_xy(wm.x, wm.y);
            let pos = egui::pos2(
                center.x - (hx * ca - hy * sa),
                center.y - (hx * sa + hy * ca),
            );
            wp.add(egui::Shape::Text(
                egui::epaint::TextShape::new(pos, galley, col).with_angle(angle),
            ));
        }
    }
}

/// 组装某一作用域（当前表 / 整本工作簿 / 仅选区）的有效 `PageSetup` 列表
fn effective_setup_for_scope(
    sheet: &sheetcore::Sheet,
    base_setup: &PageSetup,
    scope: u8,
    sel_rect: (u32, u32, u32, u32),
) -> PageSetup {
    let mut s = base_setup.clone();
    if scope == 2 {
        s.print_area = Some(sel_rect);
    }
    let _ = sheet;
    s
}

/// 生成预览页面索引列表（整合 `print_range` + 智能自定义串 `"1,3-5,8"`）
fn selected_page_indices(
    print_range: u8,
    preview_page: usize,
    print_from: u32,
    print_to: u32,
    range_text: &str,
    total: usize,
) -> Vec<usize> {
    let total = total.max(1);
    match print_range {
        1 => vec![preview_page.min(total - 1)],
        2 => {
            if !range_text.trim().is_empty() {
                parse_page_ranges(range_text, total)
            } else {
                let lo = print_from.min(print_to) as usize;
                let hi = print_from.max(print_to) as usize;
                if hi < 1 || lo > total {
                    Vec::new()
                } else {
                    let a = lo.max(1) - 1;
                    let b = hi.min(total) - 1;
                    (a..=b).collect()
                }
            }
        }
        _ => (0..total).collect(),
    }
}

impl SheetApp {
    /// 获取当前预览页（支持缓存加速，包含对象/批注/迷你图版本防脏）
    fn cached_preview_layout_and_ops(
        &mut self,
        setup: &PageSetup,
        req_page: usize,
    ) -> (std::sync::Arc<Layout>, std::sync::Arc<Out>, usize) {
        let sheet = self.wb.sheet(self.sheet_idx);
        let extra_fp = (sheet.pictures.len() as u64)
            .wrapping_mul(131)
            .wrapping_add(sheet.charts.len() as u64 * 17)
            .wrapping_add(sheet.notes.len() as u64 * 257)
            .wrapping_add(sheet.sparklines.len() as u64 * 1021)
            .wrapping_add(sheet.rich.len() as u64 * 4099)
            .wrapping_add(self.doc_gen.wrapping_mul(31));
        let cache_key = (
            self.doc_uid,
            sheet.identity,
            sheet.cell_version().clone(),
            sheet.display_style_revision(),
            sheet.rows.revision(),
            sheet.cols.revision(),
            sheet.hidden_rows.revision(),
            sheet.hidden_rows_user.revision(),
            sheet.hidden_cols.revision(),
            sheet.merges_revision() ^ extra_fp,
            setup.clone(),
            self.wb.path.clone(),
        );
        let hit = self
            .normal_page_breaks
            .print_preview
            .as_ref()
            .is_some_and(|c| c.key == cache_key);
        if !hit {
            let built = std::sync::Arc::new(paginate(sheet, setup));
            self.normal_page_breaks.print_preview =
                Some(crate::grid_render_cache::PrintPreviewCache {
                    key: cache_key,
                    layout: built,
                    page_ops: None,
                });
        }
        let layout = self
            .normal_page_breaks
            .print_preview
            .as_ref()
            .unwrap()
            .layout
            .clone();
        let total = layout.pages.len().max(1);
        let page_no = req_page.min(total - 1);
        let cached_ops = self
            .normal_page_breaks
            .print_preview
            .as_ref()
            .and_then(|c| c.page_ops.as_ref())
            .filter(|(p, _)| *p == page_no)
            .map(|(_, ops)| ops.clone());
        let ops = match cached_ops {
            Some(o) => o,
            None => {
                let built = std::sync::Arc::new(page_ops_with(
                    &self.wb,
                    self.sheet_idx,
                    &layout,
                    page_no,
                    self.wb.path.as_deref(),
                ));
                if let Some(cache) = self.normal_page_breaks.print_preview.as_mut() {
                    cache.page_ops = Some((page_no, built.clone()));
                }
                built
            }
        };
        (layout, ops, total)
    }

    /// 原生 Windows GDI 硬件矢量打印（支持当前工作表 / 整个工作簿 / 选区 + 非连续页码 + 双面/逐份/黑白）
    pub(crate) fn print_to_printer_native(&mut self) {
        let scope = self.print_studio.scope;
        let sel_rect = self.sel_rect();
        let copies = self.print_copies.clamp(1, 999) as u16;
        let printer = self.page_setup_printer.clone();
        let duplex = self.print_studio.duplex;
        let collate = self.print_studio.collate;
        let devmode_override = self.print_studio.devmode_override.clone();
        let print_range = self.print_range;
        let preview_page = self.preview_page;
        let print_from = self.print_from;
        let print_to = self.print_to;
        let range_text = self.print_studio.page_range_text.clone();
        let wb_snap = self.wb.clone();
        let si = self.sheet_idx;
        let doc_title = wb_snap.title();
        let color = !wb_snap.sheet(si).page.black_and_white;

        let run_print = move || -> Result<String, String> {
            let source = wb_snap.path.as_deref();
            let mut rendered: Vec<(f32, f32, u16, bool, Out)> = Vec::new();
            let sheet_indices: Vec<usize> = if scope == 1 {
                wb_snap
                    .sheets
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.hidden)
                    .map(|(i, _)| i)
                    .collect()
            } else {
                vec![si]
            };

            for idx in sheet_indices {
                let sh = &wb_snap.sheets[idx];
                let setup = effective_setup_for_scope(sh, &sh.page, scope, sel_rect);
                let layout = paginate(sh, &setup);
                let (pw, ph) = layout.page_size;
                let page_list = if scope == 1 {
                    (0..layout.pages.len()).collect::<Vec<_>>()
                } else {
                    selected_page_indices(
                        print_range,
                        preview_page,
                        print_from,
                        print_to,
                        &range_text,
                        layout.pages.len(),
                    )
                };
                if scope != 1 && print_range == 2 && page_list.is_empty() {
                    return Err(format!(
                        "自定义页码范围无效或超出总页数（共 {} 页）",
                        layout.pages.len().max(1)
                    ));
                }
                let mut sheet_renderer =
                    sheetcore::print::SheetPageRenderer::new(&wb_snap, idx);
                for pi in page_list {
                    let out = sheet_renderer.page_ops(&layout, pi, source);
                    rendered.push((pw, ph, setup.paper, setup.landscape, out));
                }
                if scope == 1 || print_range == 0 {
                    for cp in comment_summary_pages(&wb_snap, idx, &layout) {
                        rendered.push((pw, ph, setup.paper, setup.landscape, cp));
                    }
                }
            }

            if rendered.is_empty() {
                return Err("当前范围没有可打印内容".into());
            }
            let jobs: Vec<native_print::PrintPageJob<'_>> = rendered
                .iter()
                .map(|(pw, ph, paper, land, out)| native_print::PrintPageJob {
                    width_pt: *pw,
                    height_pt: *ph,
                    paper_code: *paper,
                    landscape: *land,
                    out,
                })
                .collect();
            let opts = native_print::NativePrintOptions {
                printer,
                copies,
                duplex,
                collate,
                color,
                devmode_override,
            };
            native_print::print_pages_gdi(&doc_title, &jobs, &opts).map_err(|e| format!("{e:#}"))
        };

        if cfg!(test) || std::env::var_os("SC_PRINT_EXIT").is_some() {
            match run_print() {
                Ok(msg) => self.status = msg,
                Err(err) => self.status = format!("打印失败：{err}"),
            }
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_print_rx = Some(rx);
        self.status = "正在通过原生矢量驱动提交打印任务…".into();
        std::thread::spawn(move || {
            let _ = tx.send(run_print());
        });
    }

    /// 导出整本工作簿（所有可见表 + PDF 书签目录 `/Outlines`）
    pub(crate) fn export_workbook_pdf_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}_整簿.pdf", self.wb.title()))
            .add_filter("PDF 文档", &["pdf"])
            .save_file()
        else {
            return;
        };
        self.export_workbook_pdf_to(&path);
    }

    /// 导出整本工作簿到指定 PDF 路径
    pub(crate) fn export_workbook_pdf_to(&mut self, path: &Path) {
        match build_pdf_font_pack() {
            Ok(pack) => {
                let async_eligible = !cfg!(test)
                    && std::env::var_os("SC_EXPORT_PDF").is_none()
                    && std::env::var_os("SC_EXPORT_PDF_EXIT").is_none();
                if async_eligible {
                    let snapshot = self.wb.clone();
                    let out_path = path.to_path_buf();
                    self.status = format!("正在后台导出整簿 PDF {}…", out_path.display());
                    self.tasks.start_workbook_pdf_export(snapshot, out_path, pack);
                    return;
                }
                match sheetcore::print::export_workbook_pdf_with_pack(
                    &self.wb,
                    path,
                    pack,
                    self.wb.path.as_deref(),
                ) {
                    Ok(n) => {
                        self.status = format!("已导出整簿 PDF（共 {n} 页，含书签目录）：{}", path.display());
                    }
                    Err(e) => {
                        self.status = format!("整簿导出失败：{e:#}");
                    }
                }
            }
            Err(e) => self.status = format!("导出失败：字体初始化失败 {e:#}"),
        }
    }

    /// 打印工作室按当前作用域 & 自定义页码范围导出 PDF
    pub(crate) fn export_pdf_from_studio_dialog(&mut self) {
        if self.print_studio.scope == 1 {
            self.export_workbook_pdf_dialog();
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.pdf", self.wb.title()))
            .add_filter("PDF 文档", &["pdf"])
            .save_file()
        else {
            return;
        };
        self.export_pdf_from_studio_to(&path);
    }

    pub(crate) fn export_pdf_from_studio_to(&mut self, path: &Path) {
        if self.print_studio.scope == 1 {
            self.export_workbook_pdf_to(path);
            return;
        }
        let mut wb_snap = self.wb.clone();
        let si = self.sheet_idx;
        if self.print_studio.scope == 2 {
            wb_snap.sheets[si].page.print_area = Some(self.sel_rect());
        }
        let total = paginate(&wb_snap.sheets[si], &wb_snap.sheets[si].page)
            .pages
            .len()
            .max(1);
        let indices = if self.print_range == 0 {
            None
        } else {
            let list = selected_page_indices(
                self.print_range,
                self.preview_page,
                self.print_from,
                self.print_to,
                &self.print_studio.page_range_text,
                total,
            );
            if list.is_empty() {
                self.status = format!("导出失败：自定义页码范围无效或超出总页数（共 {total} 页）");
                return;
            }
            Some(list)
        };
        let pack = match build_pdf_font_pack() {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("导出失败：字体初始化失败 {e:#}");
                return;
            }
        };
        let async_eligible = !cfg!(test)
            && std::env::var_os("SC_EXPORT_PDF").is_none()
            && std::env::var_os("SC_EXPORT_PDF_EXIT").is_none();
        if async_eligible {
            self.status = format!("正在后台导出 PDF {}…", path.display());
            self.tasks
                .start_pdf_export_pack(wb_snap, si, path.to_path_buf(), pack, indices);
            return;
        }
        match sheetcore::print::export_pdf_sheet_with_pack(
            &wb_snap,
            si,
            path,
            pack,
            wb_snap.path.as_deref(),
            indices.as_deref(),
        ) {
            Ok(n) => self.status = format!("已导出 PDF（共 {n} 页）：{}", path.display()),
            Err(e) => self.status = format!("导出失败：{e:#}"),
        }
    }

    /// 导出当前预览页为 150/300 DPI 出版级高清 PNG
    pub(crate) fn export_current_page_png_dialog(&mut self) {
        let page_no = self.preview_page + 1;
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}_第{}页.png", self.wb.title(), page_no))
            .add_filter("PNG 高清图片", &["png"])
            .save_file()
        else {
            return;
        };
        let dpi = self.print_studio.png_dpi;
        let sheet = self.wb.sheet(self.sheet_idx);
        let setup = effective_setup_for_scope(sheet, &sheet.page, self.print_studio.scope, self.sel_rect());
        let layout = paginate(sheet, &setup);
        let pi = self.preview_page.min(layout.pages.len().saturating_sub(1));
        let out = page_ops_with(&self.wb, self.sheet_idx, &layout, pi, self.wb.path.as_deref());
        let (pw, ph) = layout.page_size;
        match native_print::render_out_to_png_bytes(pw, ph, dpi, &out)
            .and_then(|bytes| std::fs::write(&path, bytes).map_err(anyhow::Error::from))
        {
            Ok(()) => {
                self.status = format!("已导出 {dpi} DPI 高清 PNG：{}", path.display());
            }
            Err(e) => {
                self.status = format!("PNG 导出失败：{e:#}");
            }
        }
    }

    /// 超越 Excel / WPS 的一体化打印与出版工作室 (`Ctrl+P`)
    pub(crate) fn draw_print_studio(&mut self, ctx: &egui::Context) {
        if !self.print_preview {
            return;
        }
        let mut open = true;
        let mut do_print = false;
        let mut do_export_pdf = false;
        let mut do_export_png = false;
        let mut do_page_setup = false;
        let mut do_refresh_printers = false;
        let mut do_printer_props = false;
        let mut goto_page: Option<usize> = None;

        if self.page_setup_printers.is_empty() && self.bg_printers_rx.is_none() {
            self.refresh_printers();
        }
        let printing_now = self.bg_print_rx.is_some();

        let base_setup = {
            let sheet = self.wb.sheet(self.sheet_idx);
            if self.page_setup_open && self.page_setup_sheet == sheet.identity {
                self.page_setup_draft.clone()
            } else {
                sheet.page.clone()
            }
        };
        let eff_setup = effective_setup_for_scope(
            self.wb.sheet(self.sheet_idx),
            &base_setup,
            self.print_studio.scope,
            self.sel_rect(),
        );
        let (layout, ops_cur, total) =
            self.cached_preview_layout_and_ops(&eff_setup, self.preview_page);
        let page_no = self.preview_page.min(total - 1);
        let (pw, ph) = layout.page_size;
        let margins = eff_setup.margins;

        // 双页对开时准备下一页绘制指令
        let ops_next: Option<(usize, Out)> = if self.print_studio.facing_pages && page_no + 1 < total {
            let next_i = page_no + 1;
            Some((
                next_i,
                page_ops_with(
                    &self.wb,
                    self.sheet_idx,
                    &layout,
                    next_i,
                    self.wb.path.as_deref(),
                ),
            ))
        } else {
            None
        };

        let visible_sheets_count = self.wb.sheets.iter().filter(|s| !s.hidden).count().max(1);
        let mut margin_edit: Option<(usize, f32)> = None;

        ui_kit::window(ctx, "打印与出版工作室", 1080.0)
            .open(&mut open)
            .default_size([1080.0, 680.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // ==================== 左侧控制面板（296px，可独立滚动） ====================
                    ui.vertical(|ui| {
                        ui.set_width(296.0);
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 5.0);

                        // 主操作栏：原生打印 / 导出 PDF / 高清 PNG
                        ui.horizontal(|ui| {
                            if printing_now {
                                ui.spinner();
                                ui.label(egui::RichText::new("打印提交中…").size(12.0));
                            } else if ui_kit::primary_button(ui, "立即打印").clicked() {
                                do_print = true;
                            }
                            if ui_kit::secondary_button(ui, "导出 PDF…").clicked() {
                                do_export_pdf = true;
                            }
                            if ui_kit::secondary_button(ui, "PNG…")
                                .on_hover_text("将当前页导出为高分辨率 PNG 出版图")
                                .clicked()
                            {
                                do_export_png = true;
                            }
                        });

                        // 超越 Excel：智能可读性预警（自动缩放过小时给出友好建议与一键修复）
                        if layout.scale < 0.65 {
                            egui::Frame::NONE
                                .fill(Color32::from_rgb(255, 247, 237))
                                .stroke(Stroke::new(1.0f32, Color32::from_rgb(251, 146, 60)))
                                .corner_radius(4.0)
                                .inner_margin(egui::Margin::symmetric(8, 5))
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "⚠ 当前缩放 {:.0}%，字号偏小",
                                                layout.scale * 100.0
                                            ))
                                            .size(11.5)
                                            .color(Color32::from_rgb(154, 52, 18)),
                                        );
                                        if !base_setup.landscape
                                            && ui.small_button("一键切横向").clicked()
                                        {
                                            self.set_page(|p| p.landscape = true);
                                        }
                                    });
                                });
                        }

                        ui.separator();

                        egui::ScrollArea::vertical()
                            .id_salt("print_studio_controls")
                            .max_height((ui.available_height() - 6.0).max(320.0))
                            .show(ui, |ui| {
                                // 1. 打印机与硬件属性
                                crate::dlg_section(ui, "打印机与作业");
                                ui.horizontal(|ui| {
                                    let cur_prn = if self.page_setup_printer.is_empty() {
                                        "系统默认打印机"
                                    } else {
                                        self.page_setup_printer.as_str()
                                    };
                                    egui::ComboBox::from_id_salt("studio_printer")
                                        .width(172.0)
                                        .selected_text(cur_prn)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.page_setup_printer,
                                                String::new(),
                                                "系统默认打印机",
                                            );
                                            for p in &self.page_setup_printers {
                                                ui.selectable_value(
                                                    &mut self.page_setup_printer,
                                                    p.clone(),
                                                    p,
                                                );
                                            }
                                        });
                                    if ui
                                        .small_button("刷新")
                                        .on_hover_text("重新枚举系统打印机")
                                        .clicked()
                                    {
                                        do_refresh_printers = true;
                                    }
                                    if ui
                                        .small_button("属性…")
                                        .on_hover_text("打开系统原厂打印机首选项对话框")
                                        .clicked()
                                    {
                                        do_printer_props = true;
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("份数：");
                                    let mut cp = self.print_copies as f64;
                                    if ui_kit::spin(ui, &mut cp, 1.0, 999.0, 1.0, 56.0) {
                                        self.print_copies = cp as u32;
                                    }
                                    ui.checkbox(&mut self.print_studio.collate, "逐份打印");
                                });
                                ui.horizontal(|ui| {
                                    ui.label("双面：");
                                    let dup_lbl = match self.print_studio.duplex {
                                        1 => "单面打印",
                                        2 => "双面（长边翻转）",
                                        3 => "双面（短边翻转）",
                                        _ => "跟随打印机默认",
                                    };
                                    egui::ComboBox::from_id_salt("studio_duplex")
                                        .width(160.0)
                                        .selected_text(dup_lbl)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.print_studio.duplex,
                                                0,
                                                "跟随打印机默认",
                                            );
                                            ui.selectable_value(
                                                &mut self.print_studio.duplex,
                                                1,
                                                "单面打印",
                                            );
                                            ui.selectable_value(
                                                &mut self.print_studio.duplex,
                                                2,
                                                "双面（长边翻转）",
                                            );
                                            ui.selectable_value(
                                                &mut self.print_studio.duplex,
                                                3,
                                                "双面（短边翻转）",
                                            );
                                        });
                                });

                                // 2. 打印作用域与自定义页码范围
                                crate::dlg_section(ui, "打印范围");
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.print_studio.scope, 0, "当前工作表");
                                    ui.selectable_value(
                                        &mut self.print_studio.scope,
                                        1,
                                        format!("整个工作簿({visible_sheets_count})"),
                                    );
                                    ui.selectable_value(&mut self.print_studio.scope, 2, "仅选区");
                                });
                                if self.print_studio.scope != 1 {
                                    ui.horizontal(|ui| {
                                        ui.label("页码：");
                                        ui.selectable_value(&mut self.print_range, 0, "全部");
                                        ui.selectable_value(&mut self.print_range, 1, "当前页");
                                        ui.selectable_value(&mut self.print_range, 2, "自定义");
                                    });
                                    if self.print_range == 2 {
                                        ui.horizontal(|ui| {
                                            ui.add(
                                                egui::TextEdit::singleline(
                                                    &mut self.print_studio.page_range_text,
                                                )
                                                .desired_width(185.0)
                                                .hint_text("如 1, 3-5, 8（留空按起止页）"),
                                            );
                                            let cnt = selected_page_indices(
                                                self.print_range,
                                                page_no,
                                                self.print_from,
                                                self.print_to,
                                                &self.print_studio.page_range_text,
                                                total,
                                            )
                                            .len();
                                            ui.label(
                                                egui::RichText::new(format!("共 {cnt} 页"))
                                                    .size(11.0)
                                                    .weak(),
                                            );
                                        });
                                    }
                                } else {
                                    ui.label(
                                        egui::RichText::new(
                                            "导出 PDF 时将自动生成各工作表侧边栏书签目录",
                                        )
                                        .size(11.0)
                                        .weak(),
                                    );
                                }

                                // 3. 纸张、方向与页边距
                                crate::dlg_section(ui, "纸张与版面（实时生效）");
                                ui.horizontal(|ui| {
                                    ui.label("方向：");
                                    if ui.selectable_label(!base_setup.landscape, "纵向").clicked() {
                                        self.set_page(|p| p.landscape = false);
                                    }
                                    if ui.selectable_label(base_setup.landscape, "横向").clicked() {
                                        self.set_page(|p| p.landscape = true);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("纸张：");
                                    egui::ComboBox::from_id_salt("studio_paper")
                                        .width(196.0)
                                        .selected_text(base_setup.paper_label())
                                        .show_ui(ui, |ui| {
                                            for &(code, label) in PAPER_SIZES {
                                                if ui
                                                    .selectable_label(base_setup.paper == code, label)
                                                    .clicked()
                                                {
                                                    self.set_page(|p| p.paper = code);
                                                }
                                            }
                                        });
                                });
                                ui.horizontal(|ui| {
                                    ui.label("边距：");
                                    let is_narrow = (margins[0] - 0.25).abs() < 0.05;
                                    let is_wide = (margins[0] - 1.0).abs() < 0.05;
                                    if ui
                                        .selectable_label(!is_narrow && !is_wide, "常规")
                                        .clicked()
                                    {
                                        self.set_page(|p| {
                                            p.margins = [0.7, 0.7, 0.75, 0.75, 0.3, 0.3];
                                        });
                                    }
                                    if ui.selectable_label(is_narrow, "窄").clicked() {
                                        self.set_page(|p| {
                                            p.margins = [0.25, 0.25, 0.75, 0.75, 0.3, 0.3];
                                        });
                                    }
                                    if ui.selectable_label(is_wide, "宽").clicked() {
                                        self.set_page(|p| {
                                            p.margins = [1.0, 1.0, 1.0, 1.0, 0.5, 0.5];
                                        });
                                    }
                                });

                                // 4. 缩放与分页顺序
                                crate::dlg_section(ui, "一键适应缩放");
                                let fit_mode = if !base_setup.fit_to_page {
                                    0u8
                                } else if base_setup.fit_width == 1 && base_setup.fit_height == 1 {
                                    1
                                } else if base_setup.fit_width == 1 && base_setup.fit_height == 0 {
                                    2
                                } else if base_setup.fit_width == 0 && base_setup.fit_height == 1 {
                                    3
                                } else {
                                    4
                                };
                                for (m, lbl) in [
                                    (0u8, "无缩放（按实际比例）"),
                                    (1, "调整为一页 (1×1)"),
                                    (2, "所有列缩为一页宽"),
                                    (3, "所有行缩为一页高"),
                                ] {
                                    if ui.selectable_label(fit_mode == m, lbl).clicked() {
                                        self.set_page(|p| match m {
                                            1 => {
                                                p.fit_to_page = true;
                                                p.fit_width = 1;
                                                p.fit_height = 1;
                                            }
                                            2 => {
                                                p.fit_to_page = true;
                                                p.fit_width = 1;
                                                p.fit_height = 0;
                                            }
                                            3 => {
                                                p.fit_to_page = true;
                                                p.fit_width = 0;
                                                p.fit_height = 1;
                                            }
                                            _ => {
                                                p.fit_to_page = false;
                                                p.fit_width = 0;
                                                p.fit_height = 0;
                                            }
                                        });
                                    }
                                }
                                if fit_mode == 0 {
                                    ui.horizontal(|ui| {
                                        ui.label("比例：");
                                        let mut sc = base_setup.scale as f64;
                                        if ui_kit::spin(ui, &mut sc, 10.0, 400.0, 5.0, 62.0) {
                                            self.set_page(|p| p.scale = sc as u16);
                                        }
                                        ui.label("%");
                                    });
                                }

                                // 5. 打印元素与财务专业质量选项
                                crate::dlg_section(ui, "打印元素与输出质量");
                                ui.horizontal(|ui| {
                                    let mut g = base_setup.gridlines;
                                    if ui.checkbox(&mut g, "网格线").changed() {
                                        self.set_page(|p| p.gridlines = g);
                                    }
                                    let mut h = base_setup.headings;
                                    if ui.checkbox(&mut h, "行号列标").changed() {
                                        self.set_page(|p| p.headings = h);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    let mut ch = base_setup.center_h;
                                    if ui.checkbox(&mut ch, "水平居中").changed() {
                                        self.set_page(|p| p.center_h = ch);
                                    }
                                    let mut cv = base_setup.center_v;
                                    if ui.checkbox(&mut cv, "垂直居中").changed() {
                                        self.set_page(|p| p.center_v = cv);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    let mut bw = base_setup.black_and_white;
                                    if ui.checkbox(&mut bw, "黑白单色").changed() {
                                        self.set_page(|p| p.black_and_white = bw);
                                    }
                                    let mut dr = base_setup.draft;
                                    if ui.checkbox(&mut dr, "草稿模式").changed() {
                                        self.set_page(|p| p.draft = dr);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("页序：");
                                    if ui
                                        .selectable_label(!base_setup.page_order_over_then_down, "先列后行")
                                        .clicked()
                                    {
                                        self.set_page(|p| p.page_order_over_then_down = false);
                                    }
                                    if ui
                                        .selectable_label(base_setup.page_order_over_then_down, "先行后列")
                                        .clicked()
                                    {
                                        self.set_page(|p| p.page_order_over_then_down = true);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("批注：");
                                    let c_lbl = match base_setup.print_comments {
                                        1 => "工作表末尾汇总",
                                        2 => "如工作表所示",
                                        _ => "不打印",
                                    };
                                    egui::ComboBox::from_id_salt("studio_comments")
                                        .width(126.0)
                                        .selected_text(c_lbl)
                                        .show_ui(ui, |ui| {
                                            for (v, t) in [
                                                (0u8, "不打印"),
                                                (1, "工作表末尾汇总"),
                                                (2, "如工作表所示"),
                                            ] {
                                                if ui
                                                    .selectable_label(base_setup.print_comments == v, t)
                                                    .clicked()
                                                {
                                                    self.set_page(|p| p.print_comments = v);
                                                }
                                            }
                                        });
                                });
                                ui.horizontal(|ui| {
                                    ui.label("错误值：");
                                    let e_lbl = match base_setup.print_errors {
                                        1 => "空白",
                                        2 => "--",
                                        3 => "#N/A",
                                        _ => "原样显示",
                                    };
                                    egui::ComboBox::from_id_salt("studio_errors")
                                        .width(104.0)
                                        .selected_text(e_lbl)
                                        .show_ui(ui, |ui| {
                                            for (v, t) in [
                                                (0u8, "原样显示"),
                                                (1, "空白"),
                                                (2, "--"),
                                                (3, "#N/A"),
                                            ] {
                                                if ui
                                                    .selectable_label(base_setup.print_errors == v, t)
                                                    .clicked()
                                                {
                                                    self.set_page(|p| p.print_errors = v);
                                                }
                                            }
                                        });
                                });

                                // 6. 超越 Excel：原生矢量水印工作室
                                ui.separator();
                                ui.horizontal(|ui| {
                                    let wm_active = base_setup.watermark.is_active();
                                    let hdr = if wm_active {
                                        format!("水印工作室（{}）", base_setup.watermark.text)
                                    } else {
                                        "水印工作室（未启用）".into()
                                    };
                                    if ui
                                        .selectable_label(self.print_studio.watermark_open, hdr)
                                        .clicked()
                                    {
                                        self.print_studio.watermark_open =
                                            !self.print_studio.watermark_open;
                                    }
                                });
                                if self.print_studio.watermark_open {
                                    let mut wm = base_setup.watermark.clone();
                                    let mut changed = false;
                                    ui.horizontal_wrapped(|ui| {
                                        for preset in ["机密", "草稿", "内部审阅", "已复核", "清除"] {
                                            if ui.small_button(preset).clicked() {
                                                wm.text = if preset == "清除" {
                                                    String::new()
                                                } else {
                                                    preset.into()
                                                };
                                                changed = true;
                                            }
                                        }
                                    });
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut wm.text)
                                                .desired_width(245.0)
                                                .hint_text("输入自定义水印文字…"),
                                        )
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                    ui.horizontal(|ui| {
                                        ui.label("字号");
                                        changed |= ui
                                            .add(egui::Slider::new(&mut wm.font_size, 20.0..=92.0))
                                            .changed();
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("角度");
                                        changed |= ui
                                            .add(egui::Slider::new(&mut wm.angle_deg, -60.0..=60.0).suffix("°"))
                                            .changed();
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("透明");
                                        let mut pct = wm.opacity * 100.0;
                                        if ui
                                            .add(egui::Slider::new(&mut pct, 5.0..=55.0).suffix("%"))
                                            .changed()
                                        {
                                            wm.opacity = pct / 100.0;
                                            changed = true;
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        changed |= ui.checkbox(&mut wm.tiled, "平铺满页").changed();
                                        for (lbl, c) in [
                                            ("灰", [148u8, 163, 184]),
                                            ("红", [220, 38, 38]),
                                            ("蓝", [37, 99, 235]),
                                        ] {
                                            if ui.selectable_label(wm.color == c, lbl).clicked() {
                                                wm.color = c;
                                                changed = true;
                                            }
                                        }
                                    });
                                    if changed {
                                        self.set_page(move |p| p.watermark = wm.clone());
                                    }
                                }

                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.label("PNG 分辨率：");
                                    ui.selectable_value(&mut self.print_studio.png_dpi, 150, "150 DPI");
                                    ui.selectable_value(&mut self.print_studio.png_dpi, 300, "300 DPI");
                                });
                                ui.horizontal(|ui| {
                                    if ui_kit::secondary_button(ui, "页眉/页脚设计…").clicked() {
                                        self.hf_dlg = true;
                                        self.print_studio.hf_draft = None;
                                    }
                                    if ui_kit::secondary_button(ui, "高级页面设置…").clicked() {
                                        do_page_setup = true;
                                    }
                                });
                            });
                    });

                    ui.separator();

                    // ==================== 右侧出版级实时纸张画布 ====================
                    ui.vertical(|ui| {
                        let avail_w = ui.available_width().max(420.0);
                        ui.set_width(avail_w);

                        // 顶部画布工具条
                        ui.horizontal(|ui| {
                            ui.toggle_value(&mut self.print_studio.show_thumbnails, "缩略图");
                            ui.toggle_value(&mut self.print_studio.facing_pages, "双页对开");
                            ui.toggle_value(&mut self.print_studio.show_margin_guides, "拖拽边距");
                            ui.separator();
                            if ui_kit::secondary_button(ui, "<").clicked() && page_no > 0 {
                                let step = if self.print_studio.facing_pages { 2 } else { 1 };
                                goto_page = Some(page_no.saturating_sub(step));
                            }
                            ui.label(format!("第 {} / {} 页", page_no + 1, total));
                            if ui_kit::secondary_button(ui, ">").clicked() && page_no + 1 < total {
                                let step = if self.print_studio.facing_pages { 2 } else { 1 };
                                goto_page = Some((page_no + step).min(total - 1));
                            }
                            ui.separator();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · 缩放 {:.0}%",
                                    eff_setup.paper_label(),
                                    layout.scale * 100.0
                                ))
                                .size(11.5)
                                .weak(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(
                                    egui::Slider::new(&mut self.preview_zoom, 0.45..=2.2)
                                        .show_value(false),
                                );
                                if ui.small_button("100%").clicked() {
                                    self.preview_zoom = 1.0;
                                }
                            });
                        });
                        ui.add_space(4.0);

                        let canvas_h = (ui.available_height() - 4.0).max(460.0);
                        ui.horizontal_top(|ui| {
                            // 可选左侧缩略图导航栏（最多展示前 24 页，点击直达）
                            if self.print_studio.show_thumbnails && total > 1 {
                                egui::Frame::NONE
                                    .fill(Color32::from_rgb(241, 245, 249))
                                    .corner_radius(6.0)
                                    .inner_margin(egui::Margin::same(6))
                                    .show(ui, |ui| {
                                        ui.set_width(96.0);
                                        ui.set_min_height(canvas_h);
                                        egui::ScrollArea::vertical()
                                            .id_salt("studio_thumbs")
                                            .max_height(canvas_h)
                                            .show(ui, |ui| {
                                                let max_show = total.min(24);
                                                let th_w = 78.0;
                                                let th_h = (th_w * ph / pw.max(1.0)).clamp(44.0, 120.0);
                                                for ti in 0..max_show {
                                                    let active = ti == page_no
                                                        || (self.print_studio.facing_pages
                                                            && ti == page_no + 1);
                                                    let (r, p) = ui.allocate_painter(
                                                        Vec2::new(th_w, th_h),
                                                        Sense::click(),
                                                    );
                                                    p.rect_filled(r.rect, 2.0, Color32::WHITE);
                                                    let t_ops = if ti == page_no {
                                                        None
                                                    } else {
                                                        Some(page_ops_with(
                                                            &self.wb,
                                                            self.sheet_idx,
                                                            &layout,
                                                            ti,
                                                            self.wb.path.as_deref(),
                                                        ))
                                                    };
                                                    let ops_ref: &Out = t_ops
                                                        .as_ref()
                                                        .unwrap_or_else(|| ops_cur.as_ref());
                                                    paint_out_on_egui(
                                                        ctx,
                                                        &p,
                                                        r.rect,
                                                        ph,
                                                        ops_ref,
                                                        &mut self.print_studio.preview_textures,
                                                        true,
                                                    );
                                                    let stroke = if active {
                                                        Stroke::new(2.0f32, ui_kit::ACCENT)
                                                    } else {
                                                        Stroke::new(1.0f32, Color32::from_gray(190))
                                                    };
                                                    p.rect_stroke(
                                                        r.rect,
                                                        2.0,
                                                        stroke,
                                                        egui::StrokeKind::Inside,
                                                    );
                                                    if r.clicked() {
                                                        goto_page = Some(ti);
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(format!("第 {} 页", ti + 1))
                                                            .size(10.0)
                                                            .color(if active {
                                                                ui_kit::ACCENT
                                                            } else {
                                                                Color32::from_gray(100)
                                                            }),
                                                    );
                                                    ui.add_space(4.0);
                                                }
                                            });
                                    });
                                ui.add_space(4.0);
                            }

                            // 主纸张预览台（单页或双页对开）
                            egui::Frame::NONE
                                .fill(Color32::from_rgb(226, 232, 240))
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::same(14))
                                .show(ui, |ui| {
                                    let desk_w = ui.available_width().max(260.0);
                                    ui.set_min_size(Vec2::new(desk_w, canvas_h));
                                    egui::ScrollArea::both()
                                        .id_salt("studio_paper_desk")
                                        .max_height(canvas_h)
                                        .show(ui, |ui| {
                                            let base_h = if self.print_studio.facing_pages && ops_next.is_some() {
                                                410.0
                                            } else {
                                                510.0
                                            };
                                            let page_h = base_h * self.preview_zoom.clamp(0.45, 2.2);
                                            let page_w = page_h * pw / ph.max(1.0);
                                            let show_guides = self.print_studio.show_margin_guides;

                                            ui.horizontal_centered(|ui| {
                                                // 第 1 张纸（带可拖拽页边距标尺线）
                                                let (resp, painter) = ui.allocate_painter(
                                                    Vec2::new(page_w, page_h),
                                                    Sense::hover(),
                                                );
                                                let pr = resp.rect;
                                                painter.rect_filled(
                                                    pr.translate(Vec2::new(4.0, 5.0)),
                                                    2.0,
                                                    Color32::from_black_alpha(30),
                                                );
                                                painter.rect_filled(pr, 2.0, Color32::WHITE);
                                                paint_out_on_egui(
                                                    ctx,
                                                    &painter,
                                                    pr,
                                                    ph,
                                                    &ops_cur,
                                                    &mut self.print_studio.preview_textures,
                                                    false,
                                                );
                                                painter.rect_stroke(
                                                    pr,
                                                    2.0,
                                                    Stroke::new(1.0f32, Color32::from_gray(165)),
                                                    egui::StrokeKind::Inside,
                                                );

                                                // 纸面直接拖拽 4 条页边距参考线（左/右/上/下）
                                                if show_guides {
                                                    let s_pt = page_h / ph.max(1.0);
                                                    let lx = pr.min.x + margins[0] * 72.0 * s_pt;
                                                    let rx = pr.max.x - margins[1] * 72.0 * s_pt;
                                                    let ty = pr.min.y + margins[2] * 72.0 * s_pt;
                                                    let by = pr.max.y - margins[3] * 72.0 * s_pt;
                                                    let guide_col = Color32::from_rgba_unmultiplied(37, 99, 235, 165);
                                                    let guide_stroke = Stroke::new(1.0f32, guide_col);
                                                    painter.add(egui::Shape::dashed_line(
                                                        &[Pos2::new(lx, pr.min.y), Pos2::new(lx, pr.max.y)],
                                                        guide_stroke,
                                                        4.0,
                                                        3.0,
                                                    ));
                                                    painter.add(egui::Shape::dashed_line(
                                                        &[Pos2::new(rx, pr.min.y), Pos2::new(rx, pr.max.y)],
                                                        guide_stroke,
                                                        4.0,
                                                        3.0,
                                                    ));
                                                    painter.add(egui::Shape::dashed_line(
                                                        &[Pos2::new(pr.min.x, ty), Pos2::new(pr.max.x, ty)],
                                                        guide_stroke,
                                                        4.0,
                                                        3.0,
                                                    ));
                                                    painter.add(egui::Shape::dashed_line(
                                                        &[Pos2::new(pr.min.x, by), Pos2::new(pr.max.x, by)],
                                                        guide_stroke,
                                                        4.0,
                                                        3.0,
                                                    ));

                                                    let handles = [
                                                        (0usize, Rect::from_min_max(Pos2::new(lx - 4.0, pr.min.y), Pos2::new(lx + 4.0, pr.max.y)), true, "左边距"),
                                                        (1usize, Rect::from_min_max(Pos2::new(rx - 4.0, pr.min.y), Pos2::new(rx + 4.0, pr.max.y)), true, "右边距"),
                                                        (2usize, Rect::from_min_max(Pos2::new(pr.min.x, ty - 4.0), Pos2::new(pr.max.x, ty + 4.0)), false, "上边距"),
                                                        (3usize, Rect::from_min_max(Pos2::new(pr.min.x, by - 4.0), Pos2::new(pr.max.x, by + 4.0)), false, "下边距"),
                                                    ];
                                                    for (idx, hrect, horiz, label) in handles {
                                                        let hr = ui.interact(
                                                            hrect,
                                                            egui::Id::new(("studio_margin_drag", idx)),
                                                            Sense::drag(),
                                                        );
                                                        if hr.hovered() || hr.dragged() {
                                                            ctx.set_cursor_icon(if horiz {
                                                                egui::CursorIcon::ResizeHorizontal
                                                            } else {
                                                                egui::CursorIcon::ResizeVertical
                                                            });
                                                        }
                                                        if hr.dragged()
                                                            && let Some(pos) = hr.interact_pointer_pos()
                                                        {
                                                            let inches = match idx {
                                                                0 => ((pos.x - pr.min.x) / (72.0 * s_pt)).clamp(0.0, 2.5),
                                                                1 => ((pr.max.x - pos.x) / (72.0 * s_pt)).clamp(0.0, 2.5),
                                                                2 => ((pos.y - pr.min.y) / (72.0 * s_pt)).clamp(0.0, 2.5),
                                                                _ => ((pr.max.y - pos.y) / (72.0 * s_pt)).clamp(0.0, 2.5),
                                                            };
                                                            margin_edit = Some((idx, inches));
                                                            painter.text(
                                                                pos + Vec2::new(10.0, -10.0),
                                                                Align2::LEFT_BOTTOM,
                                                                format!("{label}: {:.1} mm", inches * 25.4),
                                                                FontId::proportional(11.5),
                                                                Color32::from_rgb(30, 64, 175),
                                                            );
                                                        }
                                                    }
                                                }

                                                // 双页对开：第 2 张纸
                                                if let Some((_nidx, ref next_ops)) = ops_next {
                                                    ui.add_space(16.0);
                                                    let (r2, p2) = ui.allocate_painter(
                                                        Vec2::new(page_w, page_h),
                                                        Sense::hover(),
                                                    );
                                                    let pr2 = r2.rect;
                                                    p2.rect_filled(
                                                        pr2.translate(Vec2::new(4.0, 5.0)),
                                                        2.0,
                                                        Color32::from_black_alpha(30),
                                                    );
                                                    p2.rect_filled(pr2, 2.0, Color32::WHITE);
                                                    paint_out_on_egui(
                                                        ctx,
                                                        &p2,
                                                        pr2,
                                                        ph,
                                                        next_ops,
                                                        &mut self.print_studio.preview_textures,
                                                        false,
                                                    );
                                                    p2.rect_stroke(
                                                        pr2,
                                                        2.0,
                                                        Stroke::new(1.0f32, Color32::from_gray(165)),
                                                        egui::StrokeKind::Inside,
                                                    );
                                                }
                                            });
                                        });
                                });
                        });
                    });
                });
            });

        if let Some((idx, inches)) = margin_edit {
            self.set_page(move |p| p.margins[idx] = inches);
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            open = false;
        }
        if let Some(p) = goto_page {
            self.preview_page = p;
        }
        if do_refresh_printers {
            self.page_setup_printers.clear();
            self.refresh_printers();
        }
        if do_printer_props {
            let prn = self.page_setup_printer.clone();
            if let Some(dm) = native_print::open_printer_properties_dialog(
                &prn,
                self.print_studio.devmode_override.as_deref(),
            ) {
                self.print_studio.devmode_override = Some(dm);
                self.status = "已保存打印机原厂首选项".into();
            }
        }
        if do_print {
            self.print_to_printer_native();
        }
        if do_export_pdf {
            self.export_pdf_from_studio_dialog();
        }
        if do_export_png {
            self.export_current_page_png_dialog();
        }
        if do_page_setup {
            self.open_page_setup();
        }
        self.print_preview = open;
    }

    /// 可视化三区页眉与页脚设计器（左 / 中 / 右独立输入 + 一键令牌芯片 + 首页/奇偶页差异 + 实时预览）
    pub(crate) fn draw_modern_header_footer_dialog(&mut self, ctx: &egui::Context) {
        if !self.hf_dlg {
            self.print_studio.hf_draft = None;
            return;
        }
        let sheet_id = self.wb.sheet(self.sheet_idx).identity;
        let need_init = self
            .print_studio
            .hf_draft
            .as_ref()
            .is_none_or(|d| d.sheet_id != sheet_id);
        if need_init {
            let p = &self.wb.sheet(self.sheet_idx).page;
            self.print_studio.hf_draft = Some(HfDraft {
                sheet_id,
                tab: 0,
                active_zone: 1,
                different_first: p.different_first_page,
                different_odd_even: p.different_odd_even,
                header: split_hf_zones(&p.header),
                footer: split_hf_zones(&p.footer),
                first_header: split_hf_zones(&p.first_header),
                first_footer: split_hf_zones(&p.first_footer),
                even_header: split_hf_zones(&p.even_header),
                even_footer: split_hf_zones(&p.even_footer),
            });
        }
        let hctx = make_header_ctx(&self.wb, self.sheet_idx);
        let mut draft = self.print_studio.hf_draft.clone().unwrap();
        let mut open = true;
        let mut ok = false;

        ui_kit::window(ctx, "页眉和页脚设计器", 620.0)
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut draft.different_first, "首页不同");
                    ui.checkbox(&mut draft.different_odd_even, "奇偶页不同");
                    ui.separator();
                    ui.selectable_value(&mut draft.tab, 0, "常规页");
                    if draft.different_first {
                        ui.selectable_value(&mut draft.tab, 1, "首页");
                    } else if draft.tab == 1 {
                        draft.tab = 0;
                    }
                    if draft.different_odd_even {
                        ui.selectable_value(&mut draft.tab, 2, "偶数页");
                    } else if draft.tab == 2 {
                        draft.tab = 0;
                    }
                });
                ui.separator();

                // 一键财务预设
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("快捷预设：").size(11.5).weak());
                    if ui.small_button("标准页码（第 1 页 / 共 N 页）").clicked() {
                        draft.footer = [String::new(), "第 &P 页 / 共 &N 页".into(), String::new()];
                    }
                    if ui.small_button("财务审计归档").clicked() {
                        draft.header = ["&F".into(), "&A".into(), "制表日期：&D".into()];
                        draft.footer = [
                            "内部财务报表".into(),
                            "第 &P 页，共 &N 页".into(),
                            "&T".into(),
                        ];
                    }
                    if ui.small_button("全部清空").clicked() {
                        draft.header = Default::default();
                        draft.footer = Default::default();
                    }
                });

                // 令牌插入芯片栏
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("插入动态字段：").size(11.5).weak());
                    for (lbl, tok) in [
                        ("页码 &P", "&P"),
                        ("总页数 &N", "&N"),
                        ("日期 &D", "&D"),
                        ("时间 &T", "&T"),
                        ("文件名 &F", "&F"),
                        ("工作表 &A", "&A"),
                        ("路径 &Z", "&Z"),
                    ] {
                        if ui.small_button(lbl).clicked() {
                            let (h, f) = match draft.tab {
                                1 => (&mut draft.first_header, &mut draft.first_footer),
                                2 => (&mut draft.even_header, &mut draft.even_footer),
                                _ => (&mut draft.header, &mut draft.footer),
                            };
                            match draft.active_zone {
                                0..=2 => h[draft.active_zone as usize].push_str(tok),
                                3..=5 => f[(draft.active_zone - 3) as usize].push_str(tok),
                                _ => h[1].push_str(tok),
                            }
                        }
                    }
                });
                ui.add_space(4.0);

                let (hdr, ftr) = match draft.tab {
                    1 => (&mut draft.first_header, &mut draft.first_footer),
                    2 => (&mut draft.even_header, &mut draft.even_footer),
                    _ => (&mut draft.header, &mut draft.footer),
                };

                crate::dlg_section(ui, "页眉（左 / 中 / 右）");
                ui.columns(3, |cols| {
                    for (i, title) in ["左区", "中区", "右区"].iter().enumerate() {
                        cols[i].label(egui::RichText::new(*title).size(11.0).weak());
                        let r = cols[i].add(
                            egui::TextEdit::singleline(&mut hdr[i])
                                .desired_width(f32::INFINITY)
                                .hint_text(*title),
                        );
                        if r.gained_focus() || r.has_focus() {
                            draft.active_zone = i as u8;
                        }
                    }
                });

                crate::dlg_section(ui, "页脚（左 / 中 / 右）");
                ui.columns(3, |cols| {
                    for (i, title) in ["左区", "中区", "右区"].iter().enumerate() {
                        cols[i].label(egui::RichText::new(*title).size(11.0).weak());
                        let r = cols[i].add(
                            egui::TextEdit::singleline(&mut ftr[i])
                                .desired_width(f32::INFINITY)
                                .hint_text(*title),
                        );
                        if r.gained_focus() || r.has_focus() {
                            draft.active_zone = (3 + i) as u8;
                        }
                    }
                });

                // 实时效果预览条
                ui.add_space(6.0);
                let raw_h = compose_hf_zones(hdr);
                let raw_f = compose_hf_zones(ftr);
                let (hl, hc, hr) = header_parts_with_ctx(&raw_h, 1, 5, Some(&hctx));
                let (fl, fc, fr) = header_parts_with_ctx(&raw_f, 1, 5, Some(&hctx));
                egui::Frame::NONE
                    .fill(Color32::from_rgb(248, 250, 252))
                    .stroke(Stroke::new(1.0f32, Color32::from_gray(215)))
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("实时效果预览（模拟第 1 / 5 页）").size(10.5).weak());
                        ui.columns(3, |c| {
                            c[0].label(egui::RichText::new(&hl).size(11.5));
                            c[1].vertical_centered(|u| {
                                u.label(egui::RichText::new(&hc).size(11.5));
                            });
                            c[2].with_layout(egui::Layout::right_to_left(egui::Align::Center), |u| {
                                u.label(egui::RichText::new(&hr).size(11.5));
                            });
                        });
                        ui.separator();
                        ui.columns(3, |c| {
                            c[0].label(egui::RichText::new(&fl).size(11.5));
                            c[1].vertical_centered(|u| {
                                u.label(egui::RichText::new(&fc).size(11.5));
                            });
                            c[2].with_layout(egui::Layout::right_to_left(egui::Align::Center), |u| {
                                u.label(egui::RichText::new(&fr).size(11.5));
                            });
                        });
                    });

                ui_kit::footer(ui, |ui| {
                    if ui_kit::secondary_button(ui, "取消").clicked() {
                        open = false;
                    }
                    if ui_kit::primary_button(ui, "确定").clicked() {
                        ok = true;
                    }
                });
            });

        self.print_studio.hf_draft = Some(draft.clone());
        if ok {
            let sh = self.wb.sheet_mut(self.sheet_idx);
            sh.page.different_first_page = draft.different_first;
            sh.page.different_odd_even = draft.different_odd_even;
            sh.page.header = compose_hf_zones(&draft.header);
            sh.page.footer = compose_hf_zones(&draft.footer);
            sh.page.first_header = compose_hf_zones(&draft.first_header);
            sh.page.first_footer = compose_hf_zones(&draft.first_footer);
            sh.page.even_header = compose_hf_zones(&draft.even_header);
            sh.page.even_footer = compose_hf_zones(&draft.even_footer);
            sh.page_dirty = true;
            self.wb.dirty = true;
            self.hf_dlg = false;
            self.print_studio.hf_draft = None;
            self.status = "已更新页眉页脚".into();
        } else if !open {
            self.hf_dlg = false;
            self.print_studio.hf_draft = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;
    use sheetcore::model::{Cell, Value};

    #[test]
    fn hf_three_zone_split_and_compose_roundtrip() {
        let raw = "&L财务部&C资产负债表 &A&R第 &P 页 / 共 &N 页";
        let z = split_hf_zones(raw);
        assert_eq!(z[0], "财务部");
        assert_eq!(z[1], "资产负债表 &A");
        assert_eq!(z[2], "第 &P 页 / 共 &N 页");
        assert_eq!(compose_hf_zones(&z), raw);

        // 无 &L/&C/&R 前缀默认居中
        let plain = split_hf_zones("第 &P 页");
        assert_eq!(plain, ["", "第 &P 页", ""]);
        assert_eq!(compose_hf_zones(&plain), "&C第 &P 页");
    }

    #[test]
    fn pdf_font_pack_loads_embedded_and_optional_cjk_fallback() {
        let pack = build_pdf_font_pack().expect("embedded fonts parse");
        assert!(pack.bold.is_some(), "embedded bold font should be present");
        #[cfg(windows)]
        {
            // Windows 机器通常自带 msyh.ttc / simsun.ttc / Deng.ttf
            assert!(
                pack.fallback.is_some(),
                "Windows CJK .ttc/.ttf fallback font should load cleanly"
            );
        }
    }

    #[test]
    fn print_studio_renders_single_facing_and_exports_workbook_pdf() {
        let _env = TestEnv::new("print_studio_smoke");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheets[0].name = "利润表".into();
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Text("营业收入".into()), formula: None });
        app.wb.sheets[0].set(0, 1, Cell { value: Value::Number(123456.78), formula: None });
        app.wb.sheets[0].page.watermark.text = "内部审阅".into();
        app.wb.sheets.push(sheetcore::Sheet::new("现金流量表"));
        app.wb.sheets[1].set(0, 0, Cell { value: Value::Text("经营现金流".into()), formula: None });

        app.print_preview = true;
        app.print_studio.facing_pages = true;
        app.print_studio.show_thumbnails = true;
        app.print_studio.show_margin_guides = true;
        let _ = ctx.run(egui::RawInput::default(), |c| {
            app.draw_print_studio(c);
        });
        assert!(app.print_preview);

        // 验证整簿 PDF 导出生成合法 PDF 与书签目录（TestEnv 已隔离 TEMP）
        let out_pdf = std::env::temp_dir().join("workbook_studio.pdf");
        app.export_workbook_pdf_to(&out_pdf);
        let bytes = std::fs::read(&out_pdf).expect("workbook pdf written");
        assert!(bytes.starts_with(b"%PDF-1."));
        let txt = String::from_utf8_lossy(&bytes);
        assert!(txt.contains("/Outlines"), "workbook PDF must contain /Outlines bookmarks");
    }

    #[test]
    fn page_break_reset_all_clears_manual_breaks() {
        let _env = TestEnv::new("pb_reset_all");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheets[0].page.row_breaks = vec![5, 10];
        app.wb.sheets[0].page.col_breaks = vec![3];
        app.ribbon_action("PageBreakResetAll", &ctx);
        assert!(app.wb.sheets[0].page.row_breaks.is_empty());
        assert!(app.wb.sheets[0].page.col_breaks.is_empty());
    }

    #[test]
    fn modern_header_footer_dialog_renders_and_initializes_draft() {
        let _env = TestEnv::new("modern_hf_dlg");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheets[0].page.header = "&L左&C中&R右".into();
        app.hf_dlg = true;
        let _ = ctx.run(egui::RawInput::default(), |c| {
            app.draw_modern_header_footer_dialog(c);
        });
        let draft = app.print_studio.hf_draft.as_ref().expect("draft initialized");
        assert_eq!(draft.header, ["左", "中", "右"]);
    }
}
