//! Excel 原始功能区图标（从 Office 的 imageMso 导出，32×32 RGBA）。
use crate::ui_icons::Icon;
use eframe::egui;
use eframe::egui::{Color32, Context, TextureHandle, TextureOptions};

/// 返回该图标对应的 PNG 字节；矢量图标返回 None。
pub fn png(icon: Icon) -> Option<(&'static str, &'static [u8])> {
    Some(match icon {
        Icon::AlignC => ("AlignC", include_bytes!("../../../assets/icons/excel/AlignC.png")),
        Icon::AlignL => ("AlignL", include_bytes!("../../../assets/icons/excel/AlignL.png")),
        Icon::AlignR => ("AlignR", include_bytes!("../../../assets/icons/excel/AlignR.png")),
        Icon::Border => ("Border", include_bytes!("../../../assets/icons/excel/Border.png")),
        Icon::Bottom => ("Bottom", include_bytes!("../../../assets/icons/excel/Bottom.png")),
        Icon::Brush => ("Brush", include_bytes!("../../../assets/icons/excel/Brush.png")),
        Icon::Cf => ("Cf", include_bytes!("../../../assets/icons/excel/Cf.png")),
        Icon::Chart => ("Chart", include_bytes!("../../../assets/icons/excel/Chart.png")),
        Icon::ClearFmt => ("ClearFmt", include_bytes!("../../../assets/icons/excel/ClearFmt.png")),
        Icon::Close => ("Close", include_bytes!("../../../assets/icons/excel/Close.png")),
        Icon::Comma => ("Comma", include_bytes!("../../../assets/icons/excel/Comma.png")),
        Icon::Comment => ("Comment", include_bytes!("../../../assets/icons/excel/Comment.png")),
        Icon::Copy => ("Copy", include_bytes!("../../../assets/icons/excel/Copy.png")),
        Icon::Currency => ("Currency", include_bytes!("../../../assets/icons/excel/Currency.png")),
        Icon::Cut => ("Cut", include_bytes!("../../../assets/icons/excel/Cut.png")),
        Icon::Eraser => ("Eraser", include_bytes!("../../../assets/icons/excel/Eraser.png")),
        Icon::Fill => ("Fill", include_bytes!("../../../assets/icons/excel/Fill.png")),
        Icon::Filter => ("Filter", include_bytes!("../../../assets/icons/excel/Filter.png")),
        Icon::Find => ("Find", include_bytes!("../../../assets/icons/excel/Find.png")),
        Icon::FontColor => ("FontColor", include_bytes!("../../../assets/icons/excel/FontColor.png")),
        Icon::FontGrow => ("FontGrow", include_bytes!("../../../assets/icons/excel/FontGrow.png")),
        Icon::FontShrink => ("FontShrink", include_bytes!("../../../assets/icons/excel/FontShrink.png")),
        Icon::Freeze => ("Freeze", include_bytes!("../../../assets/icons/excel/Freeze.png")),
        Icon::Grid => ("Grid", include_bytes!("../../../assets/icons/excel/Grid.png")),
        Icon::Image => ("Image", include_bytes!("../../../assets/icons/excel/Image.png")),
        Icon::IndentDec => ("IndentDec", include_bytes!("../../../assets/icons/excel/IndentDec.png")),
        Icon::IndentInc => ("IndentInc", include_bytes!("../../../assets/icons/excel/IndentInc.png")),
        Icon::Link => ("Link", include_bytes!("../../../assets/icons/excel/Link.png")),
        Icon::Lock => ("Lock", include_bytes!("../../../assets/icons/excel/Lock.png")),
        Icon::Merge => ("Merge", include_bytes!("../../../assets/icons/excel/Merge.png")),
        Icon::Middle => ("Middle", include_bytes!("../../../assets/icons/excel/Middle.png")),
        Icon::Open => ("Open", include_bytes!("../../../assets/icons/excel/Open.png")),
        Icon::Paste => ("Paste", include_bytes!("../../../assets/icons/excel/Paste.png")),
        Icon::Percent => ("Percent", include_bytes!("../../../assets/icons/excel/Percent.png")),
        Icon::Pivot => ("Pivot", include_bytes!("../../../assets/icons/excel/Pivot.png")),
        Icon::Print => ("Print", include_bytes!("../../../assets/icons/excel/Print.png")),
        Icon::Redo => ("Redo", include_bytes!("../../../assets/icons/excel/Redo.png")),
        Icon::Refresh => ("Refresh", include_bytes!("../../../assets/icons/excel/Refresh.png")),
        Icon::Save => ("Save", include_bytes!("../../../assets/icons/excel/Save.png")),
        Icon::SaveAs => ("SaveAs", include_bytes!("../../../assets/icons/excel/SaveAs.png")),
        Icon::Share => ("Share", include_bytes!("../../../assets/icons/excel/Share.png")),
        Icon::SortAsc => ("SortAsc", include_bytes!("../../../assets/icons/excel/SortAsc.png")),
        Icon::SortDesc => ("SortDesc", include_bytes!("../../../assets/icons/excel/SortDesc.png")),
        Icon::Sum => ("Sum", include_bytes!("../../../assets/icons/excel/Sum.png")),
        Icon::Symbol => ("Symbol", include_bytes!("../../../assets/icons/excel/Symbol.png")),
        Icon::Table => ("Table", include_bytes!("../../../assets/icons/excel/Table.png")),
        Icon::Top => ("Top", include_bytes!("../../../assets/icons/excel/Top.png")),
        Icon::Undo => ("Undo", include_bytes!("../../../assets/icons/excel/Undo.png")),
        Icon::Wrap => ("Wrap", include_bytes!("../../../assets/icons/excel/Wrap.png")),
        Icon::Zoom => ("Zoom", include_bytes!("../../../assets/icons/excel/Zoom.png")),
        _ => return None,
    })
}

/// 按需解码并缓存纹理。
pub fn texture(ctx: &Context, name: &'static str, bytes: &'static [u8]) -> TextureHandle {
    let id = egui::Id::new(("excel-icon", name));
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return t;
    }
    let img = image::load_from_memory(bytes)
        .expect("excel icon png")
        .to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let t = ctx.load_texture(name, color, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

/// 画一张位图图标。
pub fn draw(p: &egui::Painter, rect: egui::Rect, icon: Icon, enabled: bool) -> bool {
    if let Some((name, bytes)) = png(icon) {
        let t = texture(p.ctx(), name, bytes);
        let tint = if enabled { Color32::WHITE } else { Color32::from_gray(170) };
        p.image(
            t.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            tint,
        );
        true
    } else {
        false
    }
}
