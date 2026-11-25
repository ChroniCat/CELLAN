//! Windows 原生桌面集成辅助：
//! 1. 查询系统滚轮滚动行数（`SPI_GETWHEELSCROLLLINES`）；
//! 2. 为无边框窗口提供 Win11 最大化按钮贴靠布局（Snap Layouts：`WM_NCHITTEST -> HTMAXBUTTON`）子类化钩子；
//! 3. 多显示器窗口位置合法性校验（防止外接屏拔出后窗口恢复到屏外）；
//! 4. 原生 `winspool.drv` 快速枚举本机打印机（替代阻塞主线程的 PowerShell 子进程）。

use eframe::egui;

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};

/// Windows 滚轮系统设置：`u32::MAX`（`WHEEL_PAGESCROLL`）表示一次滚一屏；否则表示滚动行数（默认 3）。
pub fn system_wheel_scroll_lines() -> u32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETWHEELSCROLLLINES,
        };
        let mut lines: u32 = 3;
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                (&mut lines as *mut u32).cast(),
                0,
            )
        };
        if ok != 0 {
            return lines;
        }
    }
    3
}

/// 返回当前主显示器的 DPI 缩放倍数（1.0 = 96 DPI，1.5 = 144 DPI，2.0 = 192 DPI）。
#[cfg(windows)]
pub fn primary_dpi_scale() -> f32 {
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetDeviceCaps(hdc: *mut core::ffi::c_void, index: i32) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetDC(hwnd: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn ReleaseDC(hwnd: *mut core::ffi::c_void, hdc: *mut core::ffi::c_void) -> i32;
    }
    const LOGPIXELSX: i32 = 88;
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return 1.0;
        }
        let dpi = GetDeviceCaps(hdc, LOGPIXELSX);
        ReleaseDC(std::ptr::null_mut(), hdc);
        if dpi >= 48 && dpi <= 960 {
            dpi as f32 / 96.0
        } else {
            1.0
        }
    }
}

/// 校验恢复的窗口外框逻辑坐标是否落在当前可用显示器工作区内（与物理像素做 DPI 换算，
/// 避免高 DPI 笔记本拔掉外接屏后按物理边界误放行导致窗口开到屏幕外）。
pub fn validate_window_position(x: f32, y: f32, w: f32, h: f32) -> Option<(f32, f32)> {
    if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() || w < 200.0 || h < 200.0 {
        return None;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::RECT;
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        };
        let scale = primary_dpi_scale().max(0.5);
        let px = (x * scale).round() as i32;
        let py = (y * scale).round() as i32;
        let pw = (w * scale).round() as i32;
        let ph = (h * scale).round() as i32;
        let rect = RECT {
            left: px,
            top: py,
            right: px.saturating_add(pw),
            bottom: py.saturating_add(ph),
        };
        let mon = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) };
        if !mon.is_null() {
            let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(mon, &mut info) } != 0 {
                let wa = info.rcWork;
                let margin_x = (120.0 * scale).round() as i32;
                let margin_y = (80.0 * scale).round() as i32;
                let min_x = wa.left.saturating_sub(pw / 2);
                let max_x = wa.right.saturating_sub(margin_x).max(wa.left);
                let max_y = wa.bottom.saturating_sub(margin_y).max(wa.top);
                // 若原窗口与最近显示器工作区完全不相交（例如外接副屏已拔出），则整窗拉回工作区内。
                let intersects = rect.right > wa.left + margin_x
                    && rect.left < wa.right - margin_x
                    && rect.bottom > wa.top + margin_y
                    && rect.top < wa.bottom - margin_y;
                let (cx, cy) = if intersects {
                    (px.clamp(min_x, max_x), py.clamp(wa.top, max_y))
                } else {
                    let fit_x = wa.right.saturating_sub(pw).max(wa.left);
                    let fit_y = wa.bottom.saturating_sub(ph).max(wa.top);
                    (px.clamp(wa.left, fit_x), py.clamp(wa.top, fit_y))
                };
                return Some((cx as f32 / scale, cy as f32 / scale));
            }
        }
    }
    Some((x.max(0.0), y.max(0.0)))
}

// -----------------------------------------------------------------------------
// Win11 Snap Layouts (`WM_NCHITTEST` -> `HTMAXBUTTON`)
// -----------------------------------------------------------------------------

#[cfg(windows)]
static ORIG_WNDPROC: AtomicIsize = AtomicIsize::new(0);
#[cfg(windows)]
static SUBCLASSED_HWND: AtomicIsize = AtomicIsize::new(0);
#[cfg(windows)]
static MAX_BTN_L: AtomicI32 = AtomicI32::new(-1);
#[cfg(windows)]
static MAX_BTN_T: AtomicI32 = AtomicI32::new(-1);
#[cfg(windows)]
static MAX_BTN_R: AtomicI32 = AtomicI32::new(-1);
#[cfg(windows)]
static MAX_BTN_B: AtomicI32 = AtomicI32::new(-1);
#[cfg(windows)]
static MAX_BTN_PRESSED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static MAX_BTN_CLICK_PENDING: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static MAX_BTN_HOVERED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static MAX_BTN_SEQ: AtomicU32 = AtomicU32::new(0);

/// 每帧由标题栏最大化按钮上报当前逻辑矩形（转为客户区物理像素），供 `WM_NCHITTEST` 识别。
pub fn set_maximize_button_rect(rect: Option<egui::Rect>, ppp: f32) {
    #[cfg(windows)]
    {
        if let Some(r) = rect {
            MAX_BTN_L.store((r.min.x * ppp).round() as i32, Ordering::Relaxed);
            MAX_BTN_T.store((r.min.y * ppp).round() as i32, Ordering::Relaxed);
            MAX_BTN_R.store((r.max.x * ppp).round() as i32, Ordering::Relaxed);
            MAX_BTN_B.store((r.max.y * ppp).round() as i32, Ordering::Relaxed);
            MAX_BTN_SEQ.fetch_add(1, Ordering::Relaxed);
        } else {
            MAX_BTN_L.store(-1, Ordering::Relaxed);
            MAX_BTN_R.store(-1, Ordering::Relaxed);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (rect, ppp);
    }
}

/// 当 Win11 Snap Layout 钩子截获 `HTMAXBUTTON` 上的单击时返回 `true`，交由应用切换最大化状态。
pub fn take_maximize_button_click() -> bool {
    #[cfg(windows)]
    {
        MAX_BTN_CLICK_PENDING.swap(false, Ordering::AcqRel)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn maximize_button_nc_hovered() -> bool {
    #[cfg(windows)]
    {
        MAX_BTN_HOVERED.load(Ordering::Relaxed)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 安装 Win32 窗口过程子类化，启用无边框窗口的 Win11 贴靠布局浮层（悬停最大化按钮弹出 Snap Layouts）。
pub fn install_snap_layouts(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = cc.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = win32.hwnd.get() as windows_sys::Win32::Foundation::HWND;
        if hwnd.is_null() {
            return;
        }
        if SUBCLASSED_HWND.swap(hwnd as isize, Ordering::AcqRel) == hwnd as isize {
            return;
        }
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, GWLP_WNDPROC,
                SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
                WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
            };
            // Ensure WS_MAXIMIZEBOX is present so Windows 11 enables Snap Layouts on HTMAXBUTTON
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let want = style | (WS_MAXIMIZEBOX as isize) | (WS_MINIMIZEBOX as isize);
            if want != style {
                SetWindowLongPtrW(hwnd, GWL_STYLE, want);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, snap_wndproc as *const () as isize);
            ORIG_WNDPROC.store(prev, Ordering::Release);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cc;
    }
}

#[cfg(windows)]
unsafe extern "system" fn snap_wndproc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, HTMAXBUTTON, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
        WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    };

    if msg == WM_NCHITTEST {
        let l = MAX_BTN_L.load(Ordering::Relaxed);
        let r = MAX_BTN_R.load(Ordering::Relaxed);
        if l >= 0 && r > l {
            let t = MAX_BTN_T.load(Ordering::Relaxed);
            let b = MAX_BTN_B.load(Ordering::Relaxed);
            let sx = (lparam & 0xFFFF) as i16 as i32;
            let sy = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut pt = POINT { x: sx, y: sy };
            if unsafe { ScreenToClient(hwnd, &mut pt) } != 0
                && pt.x >= l
                && pt.x <= r
                && pt.y >= t
                && pt.y <= b
            {
                MAX_BTN_HOVERED.store(true, Ordering::Relaxed);
                return HTMAXBUTTON as isize;
            }
        }
        MAX_BTN_HOVERED.store(false, Ordering::Relaxed);
    } else if msg == WM_NCMOUSEMOVE && wparam == HTMAXBUTTON as usize {
        MAX_BTN_HOVERED.store(true, Ordering::Relaxed);
        return 0;
    } else if msg == WM_NCMOUSELEAVE {
        MAX_BTN_HOVERED.store(false, Ordering::Relaxed);
        MAX_BTN_PRESSED.store(false, Ordering::Relaxed);
    } else if msg == WM_NCLBUTTONDOWN && wparam == HTMAXBUTTON as usize {
        MAX_BTN_PRESSED.store(true, Ordering::Relaxed);
        return 0;
    } else if msg == WM_NCLBUTTONUP && wparam == HTMAXBUTTON as usize {
        if MAX_BTN_PRESSED.swap(false, Ordering::AcqRel) {
            MAX_BTN_CLICK_PENDING.store(true, Ordering::Release);
        }
        return 0;
    }

    let orig = ORIG_WNDPROC.load(Ordering::Acquire);
    if orig != 0 {
        let proc_fn: windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC =
            unsafe { std::mem::transmute(orig) };
        unsafe { CallWindowProcW(proc_fn, hwnd, msg, wparam, lparam) }
    } else {
        0
    }
}

/// 快速枚举本机可用打印机（Windows 下直接调用 `winspool.drv` 的 `EnumPrintersW` + `GetDefaultPrinterW`，
/// 避免阻塞界面线程启动 PowerShell）。返回 `(所有打印机名称, 默认打印机名称)`。
pub fn enumerate_printers_fast() -> (Vec<String>, String) {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct PrinterInfo4W {
            p_printer_name: *const u16,
            p_server_name: *const u16,
            attributes: u32,
        }
        #[link(name = "winspool")]
        unsafe extern "system" {
            fn EnumPrintersW(
                flags: u32,
                name: *const u16,
                level: u32,
                p_printer_enum: *mut u8,
                cb_buf: u32,
                pcb_needed: *mut u32,
                pc_returned: *mut u32,
            ) -> i32;
            fn GetDefaultPrinterW(psz_buffer: *mut u16, pcch_buffer: *mut u32) -> i32;
        }
        const PRINTER_ENUM_LOCAL: u32 = 0x0000_0002;
        const PRINTER_ENUM_CONNECTIONS: u32 = 0x0000_0004;

        let mut default_name = String::new();
        let mut def_len: u32 = 0;
        unsafe {
            let _ = GetDefaultPrinterW(std::ptr::null_mut(), &mut def_len);
            if def_len > 1 && def_len < 4096 {
                let mut buf = vec![0u16; def_len as usize];
                if GetDefaultPrinterW(buf.as_mut_ptr(), &mut def_len) != 0 {
                    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    default_name = String::from_utf16_lossy(&buf[..end]);
                }
            }
        }

        let mut needed: u32 = 0;
        let mut returned: u32 = 0;
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        unsafe {
            let _ = EnumPrintersW(
                flags,
                std::ptr::null(),
                4,
                std::ptr::null_mut(),
                0,
                &mut needed,
                &mut returned,
            );
        }
        let mut list = Vec::new();
        if needed > 0 && needed < 16 * 1024 * 1024 {
            let mut raw = vec![0u8; needed as usize];
            let ok = unsafe {
                EnumPrintersW(
                    flags,
                    std::ptr::null(),
                    4,
                    raw.as_mut_ptr(),
                    needed,
                    &mut needed,
                    &mut returned,
                )
            };
            if ok != 0 && returned > 0 {
                let infos = raw.as_ptr() as *const PrinterInfo4W;
                for i in 0..returned as usize {
                    let ptr = unsafe { (*infos.add(i)).p_printer_name };
                    if !ptr.is_null() {
                        let mut len = 0usize;
                        while unsafe { *ptr.add(len) } != 0 && len < 1024 {
                            len += 1;
                        }
                        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) });
                        if !s.is_empty() && !list.contains(&s) {
                            list.push(s);
                        }
                    }
                }
            }
        }
        if !default_name.is_empty() && !list.contains(&default_name) {
            list.insert(0, default_name.clone());
        } else if default_name.is_empty()
            && let Some(first) = list.first()
        {
            default_name = first.clone();
        }
        return (list, default_name);
    }
    #[cfg(not(windows))]
    {
        (Vec::new(), String::new())
    }
}
