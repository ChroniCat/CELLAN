//! 单实例 + 把文件转交给已有窗口（多标签并入 / 后台标签去重 / 前台唤醒）。
//!
//! 做法：第一个实例在回环地址上监听一个固定端口。
//! 后来的实例连上去把文件路径发过来，自己退出——用户看到的就是
//! "双击第 2 个表格直接在当前窗口新开标签页，重复双击已打开文件则聚焦到原标签页"，
//! 而不是冒出一堆独立进程窗口。
//!
//! 用 TCP 回环而不是命名互斥量：进程退出端口自动释放，
//! 也不会在异常退出后留下"幽灵实例"。

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 只监听 127.0.0.1，外部连不进来
pub const PORT: u16 = 48_765;

/// 实际使用的端口：`SC_INSTANCE_PORT` 可覆盖（UI 回归测试用它避开用户正在跑的实例）
pub fn port() -> u16 {
    std::env::var("SC_INSTANCE_PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(PORT)
}

#[derive(Default)]
pub struct InstanceState {
    /// 当前窗口所有标签页已打开或正在打开的文件路径（含活动标签与后台 Parked 标签）
    pub open_files: Vec<PathBuf>,
    /// 当前窗口是否允许并入新标签页（实时协作 `net.is_some()` 时禁止切走活动标签）
    pub accept_new_tab: bool,
    /// 主窗口 egui Context，用于在最小化/空闲时立即唤醒处理转发请求
    pub wake_ctx: Option<eframe::egui::Context>,
}

pub enum Role {
    /// 我是第一个实例；别人转发进来的文件路径从这里收
    Primary(Receiver<PathBuf>),
    /// 已经有实例在跑并接管了打开/聚焦请求：本进程可以直接退出
    FocusedExisting,
    /// 无法并入已有实例（或无需转发）：照常独立运行
    Standalone,
}

/// 比较两个路径是否指向同一个本地文件（优先 canonicalize，Windows 回退忽略大小写比对）
pub fn same_file_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return ca == cb;
    }
    #[cfg(windows)]
    {
        return a
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy());
    }
    #[cfg(not(windows))]
    false
}

/// 试着自己当主实例；如果已有实例在跑，尝试把文件并入该窗口的多标签或切换到对应标签页
pub fn claim(forward: Option<PathBuf>, state: Arc<Mutex<InstanceState>>) -> Role {
    claim_with_port(port(), forward, state)
}

pub fn claim_with_port(
    bind_port: u16,
    forward: Option<PathBuf>,
    state: Arc<Mutex<InstanceState>>,
) -> Role {
    match TcpListener::bind(("127.0.0.1", bind_port)) {
        Ok(listener) => {
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let tx = tx.clone();
                    let state = state.clone();
                    std::thread::spawn(move || {
                        let mut writer = match stream.try_clone() {
                            Ok(w) => w,
                            Err(_) => return,
                        };
                        let mut reader = BufReader::new(stream);
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_err() {
                            return;
                        }
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            return;
                        }
                        let path = PathBuf::from(trimmed);
                        // 直接在后台线程比对并判定：界面最小化/被遮挡时不重绘，
                        // 不能等 UI 线程回复再决定，否则发送方会超时
                        let (accept, wake) = state
                            .lock()
                            .map(|st| {
                                let already_open =
                                    st.open_files.iter().any(|p| same_file_path(p, &path));
                                (already_open || st.accept_new_tab, st.wake_ctx.clone())
                            })
                            .unwrap_or((false, None));
                        // The GUI receiver may have been dropped while an old
                        // clipboard owner finishes in the background.
                        let accept = accept && tx.send(path).is_ok();
                        let _ = writeln!(writer, "{}", if accept { "yes" } else { "no" });
                        let _ = writer.flush();
                        if accept {
                            if let Some(ctx) = wake {
                                ctx.request_repaint();
                            }
                        }
                    });
                }
            });
            Role::Primary(rx)
        }
        Err(_) => {
            let Some(path) = forward else {
                return Role::Standalone;
            };
            let Ok(mut s) = TcpStream::connect_timeout(
                &format!("127.0.0.1:{bind_port}").parse().unwrap(),
                Duration::from_millis(600),
            ) else {
                return Role::Standalone;
            };
            let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
            #[cfg(windows)]
            unsafe {
                // 允许已运行的主实例窗口主动置顶到前台，不受 Windows 前台焦点锁限制
                #[link(name = "user32")]
                unsafe extern "system" {
                    fn AllowSetForegroundWindow(dw_process_id: u32) -> i32;
                }
                const ASFW_ANY: u32 = 0xFFFF_FFFF;
                let _ = AllowSetForegroundWindow(ASFW_ANY);
            }
            if writeln!(s, "{}", path.display()).is_err() {
                return Role::Standalone;
            }
            let _ = s.flush();
            let mut answer = String::new();
            let mut reader = BufReader::new(s);
            if reader.read_line(&mut answer).is_ok() && answer.trim() == "yes" {
                Role::FocusedExisting
            } else {
                Role::Standalone
            }
        }
    }
}

/// 注册为 .xlsx/.xlsm/.xls/.csv 的打开方式（当前用户，不需要管理员）
pub fn register_association(exe: &std::path::Path) -> std::io::Result<()> {
    let exe = exe.display().to_string();
    let exe_name = exe
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("CELLAN.exe");
    let prog_id = "CELLAN.Workbook";
    let run = |args: &[&str]| -> std::io::Result<()> {
        let out = crate::hidden_console_cmd("reg")
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).to_string(),
            ));
        }
        Ok(())
    };
    // ProgID：说明这是什么样的文件、用什么命令打开
    run(&["add", &format!("HKCU\\Software\\Classes\\{prog_id}"), "/ve", "/d", "电子表格工作簿（CELLAN）", "/f"])?;
    run(&["add", &format!("HKCU\\Software\\Classes\\{prog_id}\\DefaultIcon"), "/ve", "/d", &format!("{exe},0"), "/f"])?;
    run(&["add", &format!("HKCU\\Software\\Classes\\{prog_id}\\shell\\open\\command"), "/ve", "/d", &format!("\"{exe}\" \"%1\""), "/f"])?;
    // 挂到常见表格扩展名上：出现在"打开方式"列表里
    for ext in ["xlsx", "xlsm", "xls", "csv"] {
        run(&["add", &format!("HKCU\\Software\\Classes\\.{ext}\\OpenWithProgids"), "/v", prog_id, "/t", "REG_NONE", "/d", "", "/f"])?;
        run(&["add", &format!("HKCU\\Software\\Classes\\Applications\\{exe_name}\\shell\\open\\command"), "/ve", "/d", &format!("\"{exe}\" \"%1\""), "/f"])?;
    }
    // 让资源管理器立刻认到新关联
    let _ = std::process::Command::new("ie4uinit.exe").arg("-show").spawn();
    Ok(())
}

/// 把 .xlsx/.xlsm/.xls/.csv 直接指到我们（Windows 10 起系统仍会要求用户确认一次默认应用）
pub fn set_default(exe: &std::path::Path) -> std::io::Result<()> {
    register_association(exe)?;
    let prog_id = "CELLAN.Workbook";
    for ext in ["xlsx", "xlsm", "xls", "csv"] {
        let _ = crate::hidden_console_cmd("reg")
            .args([
                "add",
                &format!("HKCU\\Software\\Classes\\.{ext}"),
                "/ve",
                "/d",
                prog_id,
                "/f",
            ])
            .output();
    }
    let _ = std::process::Command::new("ie4uinit.exe").arg("-show").spawn();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reserve_loopback_port() -> u16 {
        let sock = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        sock.local_addr().unwrap().port()
    }

    #[test]
    fn handoff_declines_when_gui_closed_and_clipboard_is_finishing() {
        let port = reserve_loopback_port();
        let state = InstanceState { accept_new_tab: true, ..Default::default() };
        let Role::Primary(rx) = claim_with_port(port, None, Arc::new(Mutex::new(state))) else {
            panic!("expected the primary instance");
        };
        drop(rx);
        let second = claim_with_port(port, Some(PathBuf::from("second.xlsx")), Default::default());
        assert!(matches!(second, Role::Standalone));
    }

    #[test]
    fn handoff_accepts_new_file_when_tab_merging_enabled() {
        let port = reserve_loopback_port();
        let state = Arc::new(Mutex::new(InstanceState {
            open_files: vec![PathBuf::from(r"C:\work\first.xlsx")],
            accept_new_tab: true,
            wake_ctx: None,
        }));
        let Role::Primary(rx) = claim_with_port(port, None, state) else {
            panic!("first instance must become Primary");
        };

        let second = PathBuf::from(r"C:\work\second.xls");
        assert!(matches!(
            claim_with_port(port, Some(second.clone()), Arc::new(Mutex::new(InstanceState::default()))),
            Role::FocusedExisting
        ));
        let received = rx.recv_timeout(Duration::from_secs(2)).expect("forwarded file");
        assert_eq!(received, second);
    }

    #[test]
    fn collab_blocks_unrelated_handoff_but_focuses_parked_tab_file() {
        let port = reserve_loopback_port();
        let parked_doc = PathBuf::from(r"C:\work\parked_budget.xlsx");
        let state = Arc::new(Mutex::new(InstanceState {
            open_files: vec![
                PathBuf::from(r"C:\work\active_collab.xlsx"),
                parked_doc.clone(),
            ],
            accept_new_tab: false,
            wake_ctx: None,
        }));
        let Role::Primary(rx) = claim_with_port(port, None, state) else {
            panic!("first instance must become Primary");
        };

        // 协作中双击第三个无关文件：不得切走当前协作标签，应独立启动
        let third = PathBuf::from(r"C:\work\third.csv");
        assert!(matches!(
            claim_with_port(port, Some(third), Arc::new(Mutex::new(InstanceState::default()))),
            Role::Standalone
        ));
        assert!(rx.try_recv().is_err());

        // 协作中重复双击已在后台标签打开的文件：仍需唤醒原窗口防止双开损坏
        assert!(matches!(
            claim_with_port(port, Some(parked_doc.clone()), Arc::new(Mutex::new(InstanceState::default()))),
            Role::FocusedExisting
        ));
        let received = rx.recv_timeout(Duration::from_secs(2)).expect("parked duplicate forwarded");
        assert_eq!(received, parked_doc);
    }
}
