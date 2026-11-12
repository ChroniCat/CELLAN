//! Excel 原始功能区图标（Office imageMso 导出，32px 与 16px 两套）。
use crate::ui_icons::Icon;
use eframe::egui;
use eframe::egui::{Color32, Context, TextureHandle, TextureOptions};

/// 返回 (纹理名, 32px 字节, 16px 字节)；没有位图的矢量图标返回 None。
pub fn png(icon: Icon) -> Option<(&'static str, &'static [u8], &'static [u8])> {
    Some(match icon {
        Icon::AlignC => ("AlignC", include_bytes!("../../../assets/icons/excel/AlignC.png"), include_bytes!("../../../assets/icons/excel/AlignC_16.png")),
        Icon::AlignL => ("AlignL", include_bytes!("../../../assets/icons/excel/AlignL.png"), include_bytes!("../../../assets/icons/excel/AlignL_16.png")),
        Icon::AlignR => ("AlignR", include_bytes!("../../../assets/icons/excel/AlignR.png"), include_bytes!("../../../assets/icons/excel/AlignR_16.png")),
        Icon::Border => ("Border", include_bytes!("../../../assets/icons/excel/Border.png"), include_bytes!("../../../assets/icons/excel/Border_16.png")),
        Icon::Bottom => ("Bottom", include_bytes!("../../../assets/icons/excel/Bottom.png"), include_bytes!("../../../assets/icons/excel/Bottom_16.png")),
        Icon::Brush => ("Brush", include_bytes!("../../../assets/icons/excel/Brush.png"), include_bytes!("../../../assets/icons/excel/Brush_16.png")),
        Icon::Cf => ("Cf", include_bytes!("../../../assets/icons/excel/Cf.png"), include_bytes!("../../../assets/icons/excel/Cf_16.png")),
        Icon::Chart => ("Chart", include_bytes!("../../../assets/icons/excel/Chart.png"), include_bytes!("../../../assets/icons/excel/Chart_16.png")),
        Icon::ClearFmt => ("ClearFmt", include_bytes!("../../../assets/icons/excel/ClearFmt.png"), include_bytes!("../../../assets/icons/excel/ClearFmt_16.png")),
        Icon::Close => ("Close", include_bytes!("../../../assets/icons/excel/Close.png"), include_bytes!("../../../assets/icons/excel/Close_16.png")),
        Icon::Comma => ("Comma", include_bytes!("../../../assets/icons/excel/Comma.png"), include_bytes!("../../../assets/icons/excel/Comma_16.png")),
        Icon::Comment => ("Comment", include_bytes!("../../../assets/icons/excel/Comment.png"), include_bytes!("../../../assets/icons/excel/Comment_16.png")),
        Icon::Copy => ("Copy", include_bytes!("../../../assets/icons/excel/Copy.png"), include_bytes!("../../../assets/icons/excel/Copy_16.png")),
        Icon::Currency => ("Currency", include_bytes!("../../../assets/icons/excel/Currency.png"), include_bytes!("../../../assets/icons/excel/Currency_16.png")),
        Icon::Cut => ("Cut", include_bytes!("../../../assets/icons/excel/Cut.png"), include_bytes!("../../../assets/icons/excel/Cut_16.png")),
        Icon::Eraser => ("Eraser", include_bytes!("../../../assets/icons/excel/Eraser.png"), include_bytes!("../../../assets/icons/excel/Eraser_16.png")),
        Icon::Fill => ("Fill", include_bytes!("../../../assets/icons/excel/Fill.png"), include_bytes!("../../../assets/icons/excel/Fill_16.png")),
        Icon::Filter => ("Filter", include_bytes!("../../../assets/icons/excel/Filter.png"), include_bytes!("../../../assets/icons/excel/Filter_16.png")),
        Icon::Find => ("Find", include_bytes!("../../../assets/icons/excel/Find.png"), include_bytes!("../../../assets/icons/excel/Find_16.png")),
        Icon::FontColor => ("FontColor", include_bytes!("../../../assets/icons/excel/FontColor.png"), include_bytes!("../../../assets/icons/excel/FontColor_16.png")),
        Icon::FontGrow => ("FontGrow", include_bytes!("../../../assets/icons/excel/FontGrow.png"), include_bytes!("../../../assets/icons/excel/FontGrow_16.png")),
        Icon::FontShrink => ("FontShrink", include_bytes!("../../../assets/icons/excel/FontShrink.png"), include_bytes!("../../../assets/icons/excel/FontShrink_16.png")),
        Icon::Freeze => ("Freeze", include_bytes!("../../../assets/icons/excel/Freeze.png"), include_bytes!("../../../assets/icons/excel/Freeze_16.png")),
        Icon::Grid => ("Grid", include_bytes!("../../../assets/icons/excel/Grid.png"), include_bytes!("../../../assets/icons/excel/Grid_16.png")),
        Icon::Image => ("Image", include_bytes!("../../../assets/icons/excel/Image.png"), include_bytes!("../../../assets/icons/excel/Image_16.png")),
        Icon::IndentDec => ("IndentDec", include_bytes!("../../../assets/icons/excel/IndentDec.png"), include_bytes!("../../../assets/icons/excel/IndentDec_16.png")),
        Icon::IndentInc => ("IndentInc", include_bytes!("../../../assets/icons/excel/IndentInc.png"), include_bytes!("../../../assets/icons/excel/IndentInc_16.png")),
        Icon::Link => ("Link", include_bytes!("../../../assets/icons/excel/Link.png"), include_bytes!("../../../assets/icons/excel/Link_16.png")),
        Icon::Lock => ("Lock", include_bytes!("../../../assets/icons/excel/Lock.png"), include_bytes!("../../../assets/icons/excel/Lock_16.png")),
        Icon::Merge => ("Merge", include_bytes!("../../../assets/icons/excel/Merge.png"), include_bytes!("../../../assets/icons/excel/Merge_16.png")),
        Icon::Middle => ("Middle", include_bytes!("../../../assets/icons/excel/Middle.png"), include_bytes!("../../../assets/icons/excel/Middle_16.png")),
        Icon::Open => ("Open", include_bytes!("../../../assets/icons/excel/Open.png"), include_bytes!("../../../assets/icons/excel/Open_16.png")),
        Icon::Paste => ("Paste", include_bytes!("../../../assets/icons/excel/Paste.png"), include_bytes!("../../../assets/icons/excel/Paste_16.png")),
        Icon::Percent => ("Percent", include_bytes!("../../../assets/icons/excel/Percent.png"), include_bytes!("../../../assets/icons/excel/Percent_16.png")),
        Icon::Pivot => ("Pivot", include_bytes!("../../../assets/icons/excel/Pivot.png"), include_bytes!("../../../assets/icons/excel/Pivot_16.png")),
        Icon::Print => ("Print", include_bytes!("../../../assets/icons/excel/Print.png"), include_bytes!("../../../assets/icons/excel/Print_16.png")),
        Icon::Refresh => ("Refresh", include_bytes!("../../../assets/icons/excel/Refresh.png"), include_bytes!("../../../assets/icons/excel/Refresh_16.png")),
        Icon::SaveAs => ("SaveAs", include_bytes!("../../../assets/icons/excel/SaveAs.png"), include_bytes!("../../../assets/icons/excel/SaveAs_16.png")),
        // Share 的 imageMso 导出只剩一个橙色圆点（内部图形没导出来），
        // 改用矢量画法（三个点 + 连线），任何 DPI 下都清晰。
        Icon::SortAsc => ("SortAsc", include_bytes!("../../../assets/icons/excel/SortAsc.png"), include_bytes!("../../../assets/icons/excel/SortAsc_16.png")),
        Icon::SortDesc => ("SortDesc", include_bytes!("../../../assets/icons/excel/SortDesc.png"), include_bytes!("../../../assets/icons/excel/SortDesc_16.png")),
        Icon::Sum => ("Sum", include_bytes!("../../../assets/icons/excel/Sum.png"), include_bytes!("../../../assets/icons/excel/Sum_16.png")),
        Icon::Symbol => ("Symbol", include_bytes!("../../../assets/icons/excel/Symbol.png"), include_bytes!("../../../assets/icons/excel/Symbol_16.png")),
        Icon::Table => ("Table", include_bytes!("../../../assets/icons/excel/Table.png"), include_bytes!("../../../assets/icons/excel/Table_16.png")),
        Icon::Top => ("Top", include_bytes!("../../../assets/icons/excel/Top.png"), include_bytes!("../../../assets/icons/excel/Top_16.png")),
        Icon::Wrap => ("Wrap", include_bytes!("../../../assets/icons/excel/Wrap.png"), include_bytes!("../../../assets/icons/excel/Wrap_16.png")),
        Icon::Zoom => ("Zoom", include_bytes!("../../../assets/icons/excel/Zoom.png"), include_bytes!("../../../assets/icons/excel/Zoom_16.png")),
        _ => return None,
    })
}

fn texture(ctx: &Context, name: &'static str, small: bool, bytes: &'static [u8]) -> TextureHandle {
    let id = egui::Id::new(("excel-icon", name, small));
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return t;
    }
    let img = image::load_from_memory(bytes).expect("excel icon png").to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let label = if small { format!("{name}@16") } else { name.to_string() };
    let t = ctx.load_texture(label, color, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

/// 画一张位图图标；小尺寸用原生 16px 版本，避免 32px 缩小的发虚。
pub fn draw(p: &egui::Painter, rect: egui::Rect, icon: Icon, enabled: bool) -> bool {
    if let Some((name, big, small)) = png(icon) {
        let small_mode = rect.width() <= 20.0;
        let bytes = if small_mode { small } else { big };
        let t = texture(p.ctx(), name, small_mode, bytes);
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
