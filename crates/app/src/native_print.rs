//! Windows 原生 GDI 硬件矢量打印驱动 & 高清 PNG 离屏光栅化引擎。
//!
//! 彻底取代旧版 `scripts/print_pdf.ps1`（PowerShell + WinRT 200 DPI 光栅截图），
//! 直接对接 `winspool.drv` 与 `gdi32.dll`：
//! - 毫秒级枚举系统打印机与默认打印机；
//! - 原生弹出系统打印机首选项对话框（`DocumentPropertiesW`），支持纸盒/双面/黑白/份数/逐份；
//! - 直接按物理打印机原生 DPI（600–1200 DPI）发送矢量矩形、实虚边框、多边形、折线、高清图像、
//!   旋转/粗体/斜体/下划线/删除线硬件字体与斜角/平铺水印；
//! - 复用同一套 `render_out_to_hdc` 管线输出 150/300 DPI 高清 PNG 页面，实现“打印 = PNG = PDF = 预览”同源。

use anyhow::{Context, Result};
use sheetcore::print::Out;

/// 打印作业硬件配置（直接写入 Win32 `DEVMODEW`）
#[derive(Clone, Debug)]
pub struct NativePrintOptions {
    /// 打印机名称；为空表示自动选用系统默认打印机
    pub printer: String,
    /// 打印份数（1..=999）
    pub copies: u16,
    /// 双面打印：0 = 打印机默认，1 = 单面，2 = 双面长边翻转，3 = 双面短边翻转
    pub duplex: u8,
    /// 逐份打印（多份多页时 1-2-3, 1-2-3）
    pub collate: bool,
    /// 彩色/黑白硬件模式（false = 强制单色黑白）
    pub color: bool,
    /// 通过系统“打印机属性…”对话框获取的驱动私有 DEVMODE 字节缓冲
    pub devmode_override: Option<Vec<u8>>,
}

impl Default for NativePrintOptions {
    fn default() -> Self {
        Self {
            printer: String::new(),
            copies: 1,
            duplex: 0,
            collate: true,
            color: true,
            devmode_override: None,
        }
    }
}

/// 单页打印载荷（支持整本工作簿跨表混排不同纸张大小与横纵方向）
pub struct PrintPageJob<'a> {
    pub width_pt: f32,
    pub height_pt: f32,
    pub paper_code: u16,
    pub landscape: bool,
    pub out: &'a Out,
}

/// 枚举本机与网络打印机（复用 `win_chrome::enumerate_printers_fast`）
pub fn enumerate_printers() -> (Vec<String>, Option<String>) {
    let (list, def) = crate::win_chrome::enumerate_printers_fast();
    let def_opt = (!def.is_empty()).then_some(def);
    (list, def_opt)
}

#[cfg(windows)]
mod win_gdi {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    pub type HDC = *mut c_void;
    pub type HANDLE = *mut c_void;
    pub type HWND = *mut c_void;
    pub type HGDIOBJ = *mut c_void;
    pub type HBRUSH = *mut c_void;
    pub type HPEN = *mut c_void;
    pub type HFONT = *mut c_void;
    pub type HBITMAP = *mut c_void;
    pub type COLORREF = u32;

    pub const DM_OUT_BUFFER: u32 = 2;
    pub const DM_IN_PROMPT: u32 = 4;
    pub const DM_IN_BUFFER: u32 = 8;

    pub const DM_ORIENTATION: u32 = 0x0000_0001;
    pub const DM_PAPERSIZE: u32 = 0x0000_0002;
    pub const DM_COPIES: u32 = 0x0000_0100;
    pub const DM_COLOR: u32 = 0x0000_0800;
    pub const DM_DUPLEX: u32 = 0x0000_1000;
    pub const DM_COLLATE: u32 = 0x0000_8000;

    pub const DMORIENT_PORTRAIT: i16 = 1;
    pub const DMORIENT_LANDSCAPE: i16 = 2;
    pub const DMCOLOR_MONOCHROME: i16 = 1;
    pub const DMCOLOR_COLOR: i16 = 2;
    pub const DMDUP_SIMPLEX: i16 = 1;
    pub const DMDUP_VERTICAL: i16 = 2;
    pub const DMDUP_HORIZONTAL: i16 = 3;
    pub const DMCOLLATE_FALSE: i16 = 0;
    pub const DMCOLLATE_TRUE: i16 = 1;

    pub const LOGPIXELSX: i32 = 88;
    pub const LOGPIXELSY: i32 = 90;
    pub const PHYSICALOFFSETX: i32 = 112;
    pub const PHYSICALOFFSETY: i32 = 113;

    pub const TRANSPARENT: i32 = 1;
    pub const TA_LEFT: u32 = 0;
    pub const TA_CENTER: u32 = 6;
    pub const TA_BASELINE: u32 = 24;
    pub const PS_SOLID: i32 = 0;
    pub const PS_GEOMETRIC: u32 = 0x0001_0000;
    pub const PS_USERSTYLE: u32 = 7;
    pub const PS_ENDCAP_FLAT: u32 = 0x0000_0200;
    pub const PS_JOIN_MITER: u32 = 0x0000_2000;
    pub const BS_SOLID: u32 = 0;
    pub const NULL_PEN: i32 = 8;
    pub const HALFTONE: i32 = 4;
    pub const DIB_RGB_COLORS: u32 = 0;
    pub const SRCCOPY: u32 = 0x00CC_0020;
    pub const BI_RGB: u32 = 0;
    pub const FW_NORMAL: i32 = 400;
    pub const FW_BOLD: i32 = 700;
    pub const DEFAULT_CHARSET: u32 = 1;
    pub const OUT_TT_PRECIS: u32 = 4;
    pub const CLIP_DEFAULT_PRECIS: u32 = 0;
    pub const CLEARTYPE_QUALITY: u32 = 5;
    pub const DEFAULT_PITCH: u32 = 0;
    pub const FF_SWISS: u32 = 2 << 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct DevModeW {
        pub dm_device_name: [u16; 32],
        pub dm_spec_version: u16,
        pub dm_driver_version: u16,
        pub dm_size: u16,
        pub dm_driver_extra: u16,
        pub dm_fields: u32,
        pub dm_orientation: i16,
        pub dm_paper_size: i16,
        pub dm_paper_length: i16,
        pub dm_paper_width: i16,
        pub dm_scale: i16,
        pub dm_copies: i16,
        pub dm_default_source: i16,
        pub dm_print_quality: i16,
        pub dm_color: i16,
        pub dm_duplex: i16,
        pub dm_y_resolution: i16,
        pub dm_tt_option: i16,
        pub dm_collate: i16,
        pub dm_form_name: [u16; 32],
        pub dm_log_pixels: u16,
        pub dm_bits_per_pel: u32,
        pub dm_pels_width: u32,
        pub dm_pels_height: u32,
        pub dm_display_flags: u32,
        pub dm_display_frequency: u32,
        pub dm_icm_method: u32,
        pub dm_icm_intent: u32,
        pub dm_media_type: u32,
        pub dm_dither_type: u32,
        pub dm_reserved1: u32,
        pub dm_reserved2: u32,
        pub dm_panning_width: u32,
        pub dm_panning_height: u32,
    }

    #[repr(C)]
    pub struct DocInfoW {
        pub cb_size: i32,
        pub lpsz_doc_name: *const u16,
        pub lpsz_output: *const u16,
        pub lpsz_datatype: *const u16,
        pub fw_type: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct RectI {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PointI {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    pub struct LogBrush {
        pub lb_style: u32,
        pub lb_color: COLORREF,
        pub lb_hatch: usize,
    }

    #[repr(C)]
    pub struct BitmapInfoHeader {
        pub bi_size: u32,
        pub bi_width: i32,
        pub bi_height: i32,
        pub bi_planes: u16,
        pub bi_bit_count: u16,
        pub bi_compression: u32,
        pub bi_size_image: u32,
        pub bi_x_pels_per_meter: i32,
        pub bi_y_pels_per_meter: i32,
        pub bi_clr_used: u32,
        pub bi_clr_important: u32,
    }

    #[repr(C)]
    pub struct BitmapInfo {
        pub bmi_header: BitmapInfoHeader,
        pub bmi_colors: [u32; 1],
    }

    #[link(name = "winspool")]
    unsafe extern "system" {
        pub fn OpenPrinterW(
            p_printer_name: *const u16,
            ph_printer: *mut HANDLE,
            p_default: *const c_void,
        ) -> i32;
        pub fn ClosePrinter(h_printer: HANDLE) -> i32;
        pub fn DocumentPropertiesW(
            hwnd: HWND,
            h_printer: HANDLE,
            p_device_name: *const u16,
            p_dev_mode_output: *mut u8,
            p_dev_mode_input: *const u8,
            f_mode: u32,
        ) -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        pub fn CreateDCW(
            pwsz_driver: *const u16,
            pwsz_device: *const u16,
            psz_port: *const u16,
            pdm: *const u8,
        ) -> HDC;
        pub fn ResetDCW(hdc: HDC, lpdm: *const u8) -> HDC;
        pub fn DeleteDC(hdc: HDC) -> i32;
        pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
        pub fn CreateDIBSection(
            hdc: HDC,
            lpbmi: *const BitmapInfo,
            usage: u32,
            ppv_bits: *mut *mut c_void,
            h_section: HANDLE,
            offset: u32,
        ) -> HBITMAP;
        pub fn GdiFlush() -> i32;
        pub fn GetDeviceCaps(hdc: HDC, index: i32) -> i32;
        pub fn StartDocW(hdc: HDC, lpdi: *const DocInfoW) -> i32;
        pub fn EndDoc(hdc: HDC) -> i32;
        pub fn AbortDoc(hdc: HDC) -> i32;
        pub fn StartPage(hdc: HDC) -> i32;
        pub fn EndPage(hdc: HDC) -> i32;
        pub fn SaveDC(hdc: HDC) -> i32;
        pub fn RestoreDC(hdc: HDC, n_saved_dc: i32) -> i32;
        pub fn IntersectClipRect(
            hdc: HDC,
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
        ) -> i32;
        pub fn SetBkMode(hdc: HDC, mode: i32) -> i32;
        pub fn SetTextColor(hdc: HDC, color: COLORREF) -> COLORREF;
        pub fn SetTextAlign(hdc: HDC, align: u32) -> u32;
        pub fn CreateSolidBrush(color: COLORREF) -> HBRUSH;
        pub fn CreatePen(style: i32, width: i32, color: COLORREF) -> HPEN;
        pub fn ExtCreatePen(
            i_pen_style: u32,
            c_width: u32,
            plbrush: *const LogBrush,
            c_style: u32,
            pstyle: *const u32,
        ) -> HPEN;
        pub fn GetStockObject(i: i32) -> HGDIOBJ;
        pub fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
        pub fn DeleteObject(ho: HGDIOBJ) -> i32;
        pub fn MoveToEx(hdc: HDC, x: i32, y: i32, lppt: *mut PointI) -> i32;
        pub fn LineTo(hdc: HDC, x: i32, y: i32) -> i32;
        pub fn Polygon(hdc: HDC, apt: *const PointI, cpt: i32) -> i32;
        pub fn Polyline(hdc: HDC, apt: *const PointI, cpt: i32) -> i32;
        pub fn SetStretchBltMode(hdc: HDC, mode: i32) -> i32;
        pub fn StretchDIBits(
            hdc: HDC,
            x_dest: i32,
            y_dest: i32,
            dest_width: i32,
            dest_height: i32,
            x_src: i32,
            y_src: i32,
            src_width: i32,
            src_height: i32,
            lp_bits: *const c_void,
            lpbmi: *const BitmapInfo,
            i_usage: u32,
            rop: u32,
        ) -> i32;
        pub fn CreateFontW(
            c_height: i32,
            c_width: i32,
            c_escapement: i32,
            c_orientation: i32,
            c_weight: i32,
            b_italic: u32,
            b_underline: u32,
            b_strike_out: u32,
            i_char_set: u32,
            i_out_precision: u32,
            i_clip_precision: u32,
            i_quality: u32,
            i_pitch_and_family: u32,
            psz_face_name: *const u16,
        ) -> HFONT;
        pub fn ExtTextOutW(
            hdc: HDC,
            x: i32,
            y: i32,
            options: u32,
            lprect: *const RectI,
            lp_string: *const u16,
            c: u32,
            lp_dx: *const i32,
        ) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn FillRect(hdc: HDC, lprc: *const RectI, hbr: HBRUSH) -> i32;
    }

    pub fn to_wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    #[inline]
    pub fn colorref(rgb: [u8; 3]) -> COLORREF {
        (rgb[0] as u32) | ((rgb[1] as u32) << 8) | ((rgb[2] as u32) << 16)
    }

    #[inline]
    pub fn blend_over_white(rgb: [u8; 3], alpha: f32) -> [u8; 3] {
        let a = alpha.clamp(0.0, 1.0);
        let inv = 1.0 - a;
        [
            (rgb[0] as f32 * a + 255.0 * inv).round().clamp(0.0, 255.0) as u8,
            (rgb[1] as f32 * a + 255.0 * inv).round().clamp(0.0, 255.0) as u8,
            (rgb[2] as f32 * a + 255.0 * inv).round().clamp(0.0, 255.0) as u8,
        ]
    }

    /// 构造并验证打印机的 `DEVMODEW`（保留驱动私有扩展区 `dmDriverExtra`）
    pub fn build_devmode(
        printer_name: &str,
        paper_code: u16,
        landscape: bool,
        opts: &NativePrintOptions,
    ) -> Option<Vec<u8>> {
        let wname = to_wide(printer_name);
        unsafe {
            let mut hprinter: HANDLE = null_mut();
            if OpenPrinterW(wname.as_ptr(), &mut hprinter, null()) == 0 || hprinter.is_null() {
                return None;
            }
            let sz = DocumentPropertiesW(null_mut(), hprinter, wname.as_ptr(), null_mut(), null(), 0);
            if sz < std::mem::size_of::<DevModeW>() as i32 {
                ClosePrinter(hprinter);
                return None;
            }
            let mut buf = vec![0u8; sz as usize];
            let ok = if let Some(ov) = opts.devmode_override.as_ref().filter(|b| b.len() == buf.len()) {
                buf.copy_from_slice(ov);
                1
            } else {
                DocumentPropertiesW(
                    null_mut(),
                    hprinter,
                    wname.as_ptr(),
                    buf.as_mut_ptr(),
                    null(),
                    DM_OUT_BUFFER,
                )
            };
            if ok < 0 {
                ClosePrinter(hprinter);
                return None;
            }
            let dm = &mut *(buf.as_mut_ptr() as *mut DevModeW);
            dm.dm_fields |= DM_ORIENTATION | DM_PAPERSIZE | DM_COPIES | DM_COLOR | DM_COLLATE;
            dm.dm_orientation = if landscape {
                DMORIENT_LANDSCAPE
            } else {
                DMORIENT_PORTRAIT
            };
            if paper_code > 0 {
                dm.dm_paper_size = paper_code as i16;
            }
            dm.dm_copies = opts.copies.clamp(1, 999) as i16;
            dm.dm_color = if opts.color {
                DMCOLOR_COLOR
            } else {
                DMCOLOR_MONOCHROME
            };
            dm.dm_collate = if opts.collate {
                DMCOLLATE_TRUE
            } else {
                DMCOLLATE_FALSE
            };
            if opts.duplex > 0 {
                dm.dm_fields |= DM_DUPLEX;
                dm.dm_duplex = match opts.duplex {
                    1 => DMDUP_SIMPLEX,
                    2 => DMDUP_VERTICAL,
                    3 => DMDUP_HORIZONTAL,
                    _ => DMDUP_SIMPLEX,
                };
            }
            let mut validated = vec![0u8; sz as usize];
            let v = DocumentPropertiesW(
                null_mut(),
                hprinter,
                wname.as_ptr(),
                validated.as_mut_ptr(),
                buf.as_ptr(),
                DM_IN_BUFFER | DM_OUT_BUFFER,
            );
            ClosePrinter(hprinter);
            if v >= 0 {
                Some(validated)
            } else {
                Some(buf)
            }
        }
    }

    /// 弹出系统打印机原厂属性/首选项对话框（纸盒、双面、缩放、质量）
    pub fn prompt_printer_properties(
        printer_name: &str,
        existing: Option<&[u8]>,
    ) -> Option<Vec<u8>> {
        let wname = to_wide(printer_name);
        unsafe {
            let mut hprinter: HANDLE = null_mut();
            if OpenPrinterW(wname.as_ptr(), &mut hprinter, null()) == 0 || hprinter.is_null() {
                return None;
            }
            let sz = DocumentPropertiesW(null_mut(), hprinter, wname.as_ptr(), null_mut(), null(), 0);
            if sz < std::mem::size_of::<DevModeW>() as i32 {
                ClosePrinter(hprinter);
                return None;
            }
            let mut out_buf = vec![0u8; sz as usize];
            let (in_ptr, flags) = if let Some(prev) = existing.filter(|b| b.len() == sz as usize) {
                (prev.as_ptr(), DM_IN_PROMPT | DM_IN_BUFFER | DM_OUT_BUFFER)
            } else {
                (null(), DM_IN_PROMPT | DM_OUT_BUFFER)
            };
            let ret = DocumentPropertiesW(
                null_mut(),
                hprinter,
                wname.as_ptr(),
                out_buf.as_mut_ptr(),
                in_ptr,
                flags,
            );
            ClosePrinter(hprinter);
            // IDOK == 1
            if ret == 1 {
                Some(out_buf)
            } else {
                None
            }
        }
    }

    fn make_pen(width_px: i32, color: [u8; 3], dash_pt: &[f32], scale_x: f32) -> HPEN {
        let w = width_px.max(1);
        let cref = colorref(color);
        unsafe {
            if dash_pt.is_empty() {
                CreatePen(PS_SOLID, w, cref)
            } else {
                let mut pattern: Vec<u32> = dash_pt
                    .iter()
                    .map(|&d| (d * scale_x).round().max(1.0) as u32)
                    .collect();
                if pattern.len() == 1 {
                    pattern.push(pattern[0]);
                }
                let lb = LogBrush {
                    lb_style: BS_SOLID,
                    lb_color: cref,
                    lb_hatch: 0,
                };
                ExtCreatePen(
                    PS_GEOMETRIC | PS_USERSTYLE | PS_ENDCAP_FLAT | PS_JOIN_MITER,
                    w as u32,
                    &lb,
                    pattern.len() as u32,
                    pattern.as_ptr(),
                )
            }
        }
    }

    /// 核心统一矢量 GDI 渲染器：同时服务于物理打印机 DC 与 300 DPI 离屏 DIBSection PNG 导出
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn render_out_to_hdc(
        hdc: HDC,
        pw_pt: f32,
        ph_pt: f32,
        dpi_x: f32,
        dpi_y: f32,
        off_x: i32,
        off_y: i32,
        out: &Out,
        fill_white_bg: bool,
    ) {
        let sx = dpi_x / 72.0;
        let sy = dpi_y / 72.0;
        let to_x = |x_pt: f32| -> i32 { (x_pt * sx).round() as i32 - off_x };
        let to_y = |y_pt: f32| -> i32 { ((ph_pt - y_pt) * sy).round() as i32 - off_y };

        unsafe {
            if fill_white_bg {
                let bg = CreateSolidBrush(colorref([255, 255, 255]));
                let r = RectI {
                    left: 0,
                    top: 0,
                    right: (pw_pt * sx).round() as i32,
                    bottom: (ph_pt * sy).round() as i32,
                };
                FillRect(hdc, &r, bg);
                DeleteObject(bg);
            }
            SetBkMode(hdc, TRANSPARENT);

            // 1. 实心矩形（单元格背景、图表面板、条形/柱形、表头底色）
            for &(x, y, w, h, rgb) in &out.rects {
                let r = RectI {
                    left: to_x(x),
                    top: to_y(y + h),
                    right: to_x(x + w),
                    bottom: to_y(y),
                };
                if r.right > r.left && r.bottom > r.top {
                    let br = CreateSolidBrush(colorref(rgb));
                    FillRect(hdc, &r, br);
                    DeleteObject(br);
                }
            }

            // 2. 半透明矩形（数据条/面积叠加：与纸白预混合，兼容所有 PostScript/PCL 硬件打印机）
            for &(x, y, w, h, rgb, alpha) in &out.alpha_rects {
                let r = RectI {
                    left: to_x(x),
                    top: to_y(y + h),
                    right: to_x(x + w),
                    bottom: to_y(y),
                };
                if r.right > r.left && r.bottom > r.top {
                    let br = CreateSolidBrush(colorref(blend_over_white(rgb, alpha)));
                    FillRect(hdc, &r, br);
                    DeleteObject(br);
                }
            }

            // 3. 填充多边形（饼图扇区、雷达面积、条件格式矢量图标、批注角标）
            let null_pen = GetStockObject(NULL_PEN);
            for (pts, rgb) in &out.polys {
                if pts.len() < 3 {
                    continue;
                }
                let gpts: Vec<PointI> = pts
                    .iter()
                    .map(|&(px, py)| PointI {
                        x: to_x(px),
                        y: to_y(py),
                    })
                    .collect();
                let br = CreateSolidBrush(colorref(*rgb));
                let old_br = SelectObject(hdc, br);
                let old_pen = SelectObject(hdc, null_pen);
                Polygon(hdc, gpts.as_ptr(), gpts.len() as i32);
                SelectObject(hdc, old_pen);
                SelectObject(hdc, old_br);
                DeleteObject(br);
            }

            // 4. 嵌入图片（PNG 全色型/JPEG/BMP/GIF → 32 位 BGRA DIB → HALFTONE StretchDIBits）
            SetStretchBltMode(hdc, HALFTONE);
            for (x, y, w, h, _name, bytes, _ext) in &out.images {
                if let Ok(img) = image::load_from_memory(bytes) {
                    let rgba = img.to_rgba8();
                    let (iw, ih) = rgba.dimensions();
                    if iw == 0 || ih == 0 {
                        continue;
                    }
                    let mut bgra = Vec::with_capacity((iw * ih * 4) as usize);
                    for p in rgba.pixels() {
                        let a = p[3] as f32 / 255.0;
                        let inv = 1.0 - a;
                        let r = (p[0] as f32 * a + 255.0 * inv).round().clamp(0.0, 255.0) as u8;
                        let g = (p[1] as f32 * a + 255.0 * inv).round().clamp(0.0, 255.0) as u8;
                        let b = (p[2] as f32 * a + 255.0 * inv).round().clamp(0.0, 255.0) as u8;
                        bgra.push(b);
                        bgra.push(g);
                        bgra.push(r);
                        bgra.push(255);
                    }
                    let bmi = BitmapInfo {
                        bmi_header: BitmapInfoHeader {
                            bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                            bi_width: iw as i32,
                            bi_height: -(ih as i32), // top-down DIB
                            bi_planes: 1,
                            bi_bit_count: 32,
                            bi_compression: BI_RGB,
                            bi_size_image: 0,
                            bi_x_pels_per_meter: 0,
                            bi_y_pels_per_meter: 0,
                            bi_clr_used: 0,
                            bi_clr_important: 0,
                        },
                        bmi_colors: [0],
                    };
                    let dx = to_x(*x);
                    let dy = to_y(*y + *h);
                    let dw = (to_x(*x + *w) - dx).max(1);
                    let dh = (to_y(*y) - dy).max(1);
                    StretchDIBits(
                        hdc,
                        dx,
                        dy,
                        dw,
                        dh,
                        0,
                        0,
                        iw as i32,
                        ih as i32,
                        bgra.as_ptr() as *const c_void,
                        &bmi,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    );
                }
            }

            // 5. 实线（网格线、实线边框、双线边框、图表坐标轴、删除线/下划线）
            for &(x1, y1, x2, y2, lw, rgb) in &out.lines {
                let pen = make_pen((lw * sx).round() as i32, rgb, &[], sx);
                let old = SelectObject(hdc, pen);
                MoveToEx(hdc, to_x(x1), to_y(y1), null_mut());
                LineTo(hdc, to_x(x2), to_y(y2));
                SelectObject(hdc, old);
                DeleteObject(pen);
            }

            // 6. 虚线 / 点线 / 点划线边框与参考线
            for sl in &out.styled_lines {
                let pen = make_pen((sl.width * sx).round() as i32, sl.color, &sl.dash, sx);
                let old = SelectObject(hdc, pen);
                MoveToEx(hdc, to_x(sl.x1), to_y(sl.y1), null_mut());
                LineTo(hdc, to_x(sl.x2), to_y(sl.y2));
                SelectObject(hdc, old);
                DeleteObject(pen);
            }

            // 7. 描边折线（折线图、散点连线、迷你图趋势线）
            for sp in &out.stroke_polys {
                if sp.pts.len() < 2 {
                    continue;
                }
                let gpts: Vec<PointI> = sp
                    .pts
                    .iter()
                    .map(|&(px, py)| PointI {
                        x: to_x(px),
                        y: to_y(py),
                    })
                    .collect();
                let pen = make_pen((sp.width * sx).round() as i32, sp.color, &sp.dash, sx);
                let old = SelectObject(hdc, pen);
                Polyline(hdc, gpts.as_ptr(), gpts.len() as i32);
                SelectObject(hdc, old);
                DeleteObject(pen);
            }

            // 8. 硬件矢量文字（含精准裁剪、旋转角度、粗体/斜体/单双下划线/删除线）
            let face_w = to_wide("Microsoft YaHei UI");
            SetTextAlign(hdc, TA_LEFT | TA_BASELINE);
            for t in &out.texts {
                if t.text.is_empty() {
                    continue;
                }
                let saved = if t.clip.is_some() { SaveDC(hdc) } else { 0 };
                if let Some([cx, cy, cw, ch]) = t.clip {
                    IntersectClipRect(hdc, to_x(cx), to_y(cy + ch), to_x(cx + cw), to_y(cy));
                }
                let h_px = -((t.size * sy).round() as i32).max(1);
                let esc = (t.rotation * 10.0).round() as i32;
                let font = CreateFontW(
                    h_px,
                    0,
                    esc,
                    esc,
                    if t.bold { FW_BOLD } else { FW_NORMAL },
                    u32::from(t.italic),
                    u32::from(t.underline && !t.double_underline),
                    u32::from(t.strikethrough),
                    DEFAULT_CHARSET,
                    OUT_TT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    DEFAULT_PITCH | FF_SWISS,
                    face_w.as_ptr(),
                );
                let old_font = SelectObject(hdc, font);
                SetTextColor(hdc, colorref(t.color));
                let wide: Vec<u16> = t.text.encode_utf16().collect();
                let gx = to_x(t.x);
                let gy = to_y(t.baseline);
                ExtTextOutW(
                    hdc,
                    gx,
                    gy,
                    0,
                    null(),
                    wide.as_ptr(),
                    wide.len() as u32,
                    null(),
                );
                if t.double_underline && t.rotation.abs() < 0.5 {
                    let est_w = sheetcore::display::text_width(&t.text, t.size);
                    let lw = (t.size * 0.055 * sy).round().max(1.0) as i32;
                    let pen = make_pen(lw, t.color, &[], sx);
                    let old_p = SelectObject(hdc, pen);
                    let y1 = to_y(t.baseline - t.size * 0.12);
                    let y2 = to_y(t.baseline - t.size * 0.24);
                    let x2 = to_x(t.x + est_w);
                    MoveToEx(hdc, gx, y1, null_mut());
                    LineTo(hdc, x2, y1);
                    MoveToEx(hdc, gx, y2, null_mut());
                    LineTo(hdc, x2, y2);
                    SelectObject(hdc, old_p);
                    DeleteObject(pen);
                }
                SelectObject(hdc, old_font);
                DeleteObject(font);
                if saved > 0 {
                    RestoreDC(hdc, saved);
                }
            }

            // 9. 原生矢量水印（预混合半透明浅色，兼容所有激光与虚拟打印机）
            if !out.watermarks.is_empty() {
                SetTextAlign(hdc, TA_CENTER | TA_BASELINE);
                for wm in &out.watermarks {
                    let h_px = -((wm.size * sy).round() as i32).max(2);
                    let esc = (wm.angle_deg * 10.0).round() as i32;
                    let font = CreateFontW(
                        h_px,
                        0,
                        esc,
                        esc,
                        FW_BOLD,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_TT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        CLEARTYPE_QUALITY,
                        DEFAULT_PITCH | FF_SWISS,
                        face_w.as_ptr(),
                    );
                    let old_font = SelectObject(hdc, font);
                    SetTextColor(hdc, colorref(blend_over_white(wm.color, wm.opacity)));
                    let wide: Vec<u16> = wm.text.encode_utf16().collect();
                    ExtTextOutW(
                        hdc,
                        to_x(wm.x),
                        to_y(wm.y),
                        0,
                        null(),
                        wide.as_ptr(),
                        wide.len() as u32,
                        null(),
                    );
                    SelectObject(hdc, old_font);
                    DeleteObject(font);
                }
            }
        }
    }
}

/// 弹出 Windows 系统原厂打印机首选项对话框，返回用户确认后的 `DEVMODEW` 字节
pub fn open_printer_properties_dialog(
    printer: &str,
    existing: Option<&[u8]>,
) -> Option<Vec<u8>> {
    #[cfg(windows)]
    {
        let resolved = if printer.trim().is_empty() {
            enumerate_printers().1?
        } else {
            printer.to_string()
        };
        win_gdi::prompt_printer_properties(&resolved, existing)
    }
    #[cfg(not(windows))]
    {
        let _ = (printer, existing);
        None
    }
}

/// 通过 Windows 原生 GDI 硬件矢量管线直接打印多页作业（无临时文件、无 PowerShell、支持 600–1200 DPI）
pub fn print_pages_gdi(
    doc_name: &str,
    pages: &[PrintPageJob<'_>],
    opts: &NativePrintOptions,
) -> Result<String> {
    if pages.is_empty() {
        anyhow::bail!("没有可打印的页面");
    }
    #[cfg(windows)]
    {
        use std::ptr::null;
        use win_gdi::*;

        let target_printer = if opts.printer.trim().is_empty() {
            let (list, def) = enumerate_printers();
            def.or_else(|| list.into_iter().next())
                .context("本机未检测到可用打印机")?
        } else {
            opts.printer.clone()
        };

        let first = &pages[0];
        let mut cur_paper = first.paper_code;
        let mut cur_land = first.landscape;
        let devmode = build_devmode(&target_printer, cur_paper, cur_land, opts);

        let driver_w = to_wide("WINSPOOL");
        let printer_w = to_wide(&target_printer);
        let hdc = unsafe {
            CreateDCW(
                driver_w.as_ptr(),
                printer_w.as_ptr(),
                null(),
                devmode.as_ref().map_or(null(), |b| b.as_ptr()),
            )
        };
        if hdc.is_null() {
            anyhow::bail!("无法打开打印机设备上下文：{target_printer}");
        }

        let doc_w = to_wide(doc_name);
        let di = DocInfoW {
            cb_size: std::mem::size_of::<DocInfoW>() as i32,
            lpsz_doc_name: doc_w.as_ptr(),
            lpsz_output: null(),
            lpsz_datatype: null(),
            fw_type: 0,
        };
        let job_id = unsafe { StartDocW(hdc, &di) };
        if job_id <= 0 {
            unsafe { DeleteDC(hdc) };
            anyhow::bail!("打印机拒绝启动作业（可能未就绪或用户取消了输出对话框）");
        }

        // 驱动原生支持 dmCopies 的情况下单次发送即可；若 devmode 构建失败则软件循环份数兜底
        let sw_copies = if devmode.is_some() {
            1usize
        } else {
            opts.copies.clamp(1, 99) as usize
        };

        for _copy in 0..sw_copies {
            for page in pages {
                if page.paper_code != cur_paper || page.landscape != cur_land {
                    cur_paper = page.paper_code;
                    cur_land = page.landscape;
                    if let Some(dm) = build_devmode(&target_printer, cur_paper, cur_land, opts) {
                        unsafe {
                            let _ = ResetDCW(hdc, dm.as_ptr());
                        }
                    }
                }
                let started = unsafe { StartPage(hdc) };
                if started <= 0 {
                    unsafe {
                        AbortDoc(hdc);
                        DeleteDC(hdc);
                    }
                    anyhow::bail!("打印中途失败（StartPage 返回 {started}）");
                }
                let (dpi_x, dpi_y, off_x, off_y) = unsafe {
                    (
                        (GetDeviceCaps(hdc, LOGPIXELSX).max(72)) as f32,
                        (GetDeviceCaps(hdc, LOGPIXELSY).max(72)) as f32,
                        GetDeviceCaps(hdc, PHYSICALOFFSETX).max(0),
                        GetDeviceCaps(hdc, PHYSICALOFFSETY).max(0),
                    )
                };
                unsafe {
                    render_out_to_hdc(
                        hdc,
                        page.width_pt,
                        page.height_pt,
                        dpi_x,
                        dpi_y,
                        off_x,
                        off_y,
                        page.out,
                        false,
                    );
                }
                let ended = unsafe { EndPage(hdc) };
                if ended <= 0 {
                    unsafe {
                        AbortDoc(hdc);
                        DeleteDC(hdc);
                    }
                    anyhow::bail!("页面提交失败（EndPage 返回 {ended}）");
                }
            }
        }

        unsafe {
            EndDoc(hdc);
            DeleteDC(hdc);
        }
        Ok(format!(
            "已通过原生矢量驱动提交打印（{} 份，共 {} 页）→ {}",
            opts.copies.clamp(1, 999),
            pages.len(),
            target_printer
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = (doc_name, opts);
        anyhow::bail!("当前平台暂不支持 Windows GDI 原生硬件打印")
    }
}

/// 将单页矢量显示列表 `Out` 光栅化为指定 DPI（如 150 / 300 DPI）的高清 PNG 字节流
pub fn render_out_to_png_bytes(width_pt: f32, height_pt: f32, dpi: u32, out: &Out) -> Result<Vec<u8>> {
    let dpi = dpi.clamp(72, 600) as f32;
    let px_w = ((width_pt * dpi / 72.0).round() as u32).clamp(16, 12_000);
    let px_h = ((height_pt * dpi / 72.0).round() as u32).clamp(16, 12_000);

    #[cfg(windows)]
    let rgb_buf: Vec<u8> = {
        use std::ffi::c_void;
        use std::ptr::null_mut;
        use win_gdi::*;

        unsafe {
            let hdc = CreateCompatibleDC(null_mut());
            if hdc.is_null() {
                anyhow::bail!("无法创建离屏 GDI 上下文");
            }
            let bmi = BitmapInfo {
                bmi_header: BitmapInfoHeader {
                    bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                    bi_width: px_w as i32,
                    bi_height: -(px_h as i32), // top-down
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: BI_RGB,
                    bi_size_image: 0,
                    bi_x_pels_per_meter: 0,
                    bi_y_pels_per_meter: 0,
                    bi_clr_used: 0,
                    bi_clr_important: 0,
                },
                bmi_colors: [0],
            };
            let mut bits: *mut c_void = null_mut();
            let hbmp = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if hbmp.is_null() || bits.is_null() {
                DeleteDC(hdc);
                anyhow::bail!("无法分配离屏 DIB 位图（{px_w}×{px_h}）");
            }
            let old = SelectObject(hdc, hbmp);
            render_out_to_hdc(hdc, width_pt, height_pt, dpi, dpi, 0, 0, out, true);
            GdiFlush();

            let total_px = (px_w as usize) * (px_h as usize);
            let src = std::slice::from_raw_parts(bits as *const u8, total_px * 4);
            let mut rgb = Vec::with_capacity(total_px * 3);
            for chunk in src.chunks_exact(4) {
                rgb.push(chunk[2]); // R
                rgb.push(chunk[1]); // G
                rgb.push(chunk[0]); // B
            }

            SelectObject(hdc, old);
            DeleteObject(hbmp);
            DeleteDC(hdc);
            rgb
        }
    };

    #[cfg(not(windows))]
    let rgb_buf: Vec<u8> = {
        let _ = out;
        vec![255u8; (px_w as usize) * (px_h as usize) * 3]
    };

    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
    image::ImageEncoder::write_image(
        encoder,
        &rgb_buf,
        px_w,
        px_h,
        image::ExtendedColorType::Rgb8,
    )
    .context("编码 PNG 失败")?;
    Ok(png_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn devmodew_layout_matches_win32_220_bytes() {
        assert_eq!(std::mem::size_of::<win_gdi::DevModeW>(), 220);
    }

    #[test]
    fn render_out_to_png_produces_valid_high_dpi_image() {
        let mut out = Out::default();
        out.rects.push((36.0, 700.0, 200.0, 40.0, [24, 92, 168]));
        out.lines.push((36.0, 680.0, 500.0, 680.0, 1.5, [0, 0, 0]));
        out.styled_lines.push(sheetcore::print::StyledLine {
            x1: 36.0,
            y1: 660.0,
            x2: 500.0,
            y2: 660.0,
            width: 1.0,
            color: [180, 30, 30],
            dash: vec![4.0, 2.0],
        });
        out.texts.push(sheetcore::print::TextOp::simple(
            42.0,
            712.0,
            14.0,
            "CELLAN 原生高清财务报表 123,456.78".into(),
            [255, 255, 255],
            true,
            false,
            0.0,
        ));
        out.watermarks.push(sheetcore::print::WatermarkOp {
            x: 297.0,
            y: 420.0,
            size: 42.0,
            text: "机密文件".into(),
            color: [148, 163, 184],
            angle_deg: 35.0,
            opacity: 0.2,
        });

        let png = render_out_to_png_bytes(595.28, 841.89, 150, &out).expect("render png");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoded = image::load_from_memory(&png).expect("decode png").to_rgb8();
        assert_eq!(decoded.width(), 1240);
        assert_eq!(decoded.height(), 1754);
        // 确认页面并非纯白（包含绘制的蓝底矩形、线条、文字与水印像素）
        let non_white = decoded
            .pixels()
            .filter(|p| p[0] < 250 || p[1] < 250 || p[2] < 250)
            .count();
        assert!(non_white > 10_000, "rendered PNG should contain visible vector elements, got {non_white} pixels");
    }
}
