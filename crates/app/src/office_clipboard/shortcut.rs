//! egui-winit reads Unicode text before reporting a paste shortcut. Route grid
//! shortcuts before that read, while text editors retain the standard winit path.
use eframe::egui;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{
        GetKeyState, VK_CONTROL, VK_INSERT, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
    },
    WindowsAndMessaging::*,
};
use winit::{
    platform::windows::EventLoopBuilderExtWindows,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

#[derive(Default)]
pub struct GridPaste {
    hwnd: Cell<usize>,
    wake: RefCell<Option<egui::Context>>,
    grid: Cell<bool>,
    pending: Cell<Option<bool>>,
    frame_paste: Cell<Option<bool>>,
    ime: Cell<bool>,
}

impl GridPaste {
    pub fn install(self: &Rc<Self>, opts: &mut eframe::NativeOptions) {
        let hook = self.clone();
        opts.event_loop_builder = Some(Box::new(move |builder| {
            builder.with_msg_hook(move |raw| {
                // winit passes the MSG it is about to translate and dispatch.
                let msg = unsafe { &*(raw as *const MSG) };
                hook.handle_message(msg, |vk| unsafe { GetKeyState(i32::from(vk)) < 0 })
            });
        }));
    }

    fn handle_message(&self, msg: &MSG, down: impl Fn(u16) -> bool) -> bool {
        if msg.hwnd.is_null() || msg.hwnd as usize != self.hwnd.get() {
            return false;
        }
        match msg.message {
            WM_IME_STARTCOMPOSITION => {
                self.ime.set(true);
                self.grid.set(false);
            }
            WM_IME_ENDCOMPOSITION | WM_KILLFOCUS => {
                self.ime.set(false);
                self.grid.set(false);
            }
            WM_KEYDOWN if self.grid.get() && !self.ime.get() => {
                let key = msg.wParam as u16;
                if key == VK_V || key == VK_INSERT {
                    if let Some(values_only) = shortcut(
                        key,
                        down(VK_CONTROL),
                        down(VK_SHIFT),
                        down(VK_MENU) || down(VK_LWIN) || down(VK_RWIN),
                    ) {
                        self.pending.set(Some(values_only));
                        if let Some(ctx) = self.wake.borrow().as_ref() {
                            ctx.request_repaint();
                        }
                        return true;
                    }
                }
                // F2, typing or a click may focus an editor before another frame.
                if !matches!(key, VK_CONTROL | VK_SHIFT) {
                    self.grid.set(false);
                }
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_POINTERDOWN | WM_SYSKEYDOWN => {
                self.grid.set(false)
            }
            _ => {}
        }
        false
    }

    pub fn attach(&self, cc: &eframe::CreationContext<'_>) {
        self.grid.set(false);
        self.ime.set(false);
        self.pending.set(None);
        if let Ok(handle) = cc.window_handle() {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                self.hwnd.set(handle.hwnd.get() as usize);
                *self.wake.borrow_mut() = Some(cc.egui_ctx.clone());
            }
        }
    }

    pub fn begin_frame(&self) {
        self.grid.set(false);
        self.frame_paste.set(self.pending.take());
    }

    pub fn end_frame(&self, grid: bool) {
        self.frame_paste.set(None);
        self.grid.set(self.hwnd.get() != 0 && grid);
    }

    pub fn take(&self) -> Option<bool> {
        self.frame_paste.take()
    }
}

fn shortcut(key: u16, ctrl: bool, shift: bool, alternate: bool) -> Option<bool> {
    if alternate {
        return None;
    }
    if key == VK_V && ctrl {
        Some(shift)
    } else if key == VK_INSERT && shift && !ctrl {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_hook_only_intercepts_grid_paste_on_the_root_window() {
        let hook = GridPaste::default();
        hook.hwnd.set(1);
        hook.end_frame(true);
        let mut msg = MSG {
            hwnd: 2usize as _,
            message: WM_KEYDOWN,
            wParam: VK_V as _,
            ..Default::default()
        };
        let ctrl = |vk| vk == VK_CONTROL;
        assert!(!hook.handle_message(&msg, ctrl));
        msg.hwnd = 1usize as _;
        assert!(hook.handle_message(&msg, ctrl));
        hook.begin_frame();
        assert_eq!(hook.take(), Some(false));
        hook.end_frame(false); // text edit or modal dialog
        assert!(!hook.handle_message(&msg, ctrl));
        hook.end_frame(true);
        msg.message = WM_IME_STARTCOMPOSITION;
        hook.handle_message(&msg, ctrl);
        hook.end_frame(true);
        msg.message = WM_KEYDOWN;
        assert!(!hook.handle_message(&msg, ctrl));
        msg.message = WM_IME_ENDCOMPOSITION;
        hook.handle_message(&msg, ctrl);
        hook.end_frame(true);
        msg.message = WM_LBUTTONDOWN;
        hook.handle_message(&msg, ctrl);
        msg.message = WM_KEYDOWN;
        assert!(!hook.handle_message(&msg, ctrl));
        assert_eq!(hook.take(), None);
    }

    #[test]
    fn clipboard_shortcut_modifiers_match_grid_paste() {
        assert_eq!(shortcut(VK_V, true, false, false), Some(false));
        assert_eq!(shortcut(VK_V, true, true, false), Some(true));
        assert_eq!(shortcut(VK_INSERT, false, true, false), Some(false));
        assert_eq!(shortcut(VK_V, true, false, true), None);
        assert_eq!(shortcut(VK_V, false, false, false), None);
        assert_eq!(shortcut(VK_INSERT, true, false, false), None);
    }
}
