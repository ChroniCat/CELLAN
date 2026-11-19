//! 单实例 + 把文件转交给已有窗口。
//!
//! 做法：第一个实例在回环地址上监听一个固定端口。
//! 后来的实例连上去把文件路径发过来，自己退出——用户看到的就是
//! "双击第二个文件，第一扇窗口把它打开了"，而不是又冒出十几个窗口。
//!
//! 用 TCP 回环而不是命名互斥量：不需要 FFI，进程退出端口自动释放，
//! 也不会像互斥量那样在异常退出后留下"幽灵实例"。

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
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

pub enum Role {
    /// 我是第一个实例；别人的"你打开的是不是这个文件"询问从这里进来
    Primary(Receiver<PathBuf>),
    /// 已经有实例在跑，而且它开的正是这个文件：把注意力交给它，自己退出
    FocusedExisting,
    /// 端口被占但没人认领这个文件（或占端口的是别的程序）：照常独立运行
    Standalone,
}

/// 试着自己当主实例；如果不是，就问已在运行的那个"你开的是不是这个文件"
pub fn claim(
    forward: Option<PathBuf>,
    current: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
) -> Role {
    match TcpListener::bind(("127.0.0.1", port())) {
        Ok(listener) => {
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let tx = tx.clone();
                    let current = current.clone();
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
                        let path = PathBuf::from(line.trim());
                        // 直接在后台线程比对：界面在最小化/被遮挡时不重绘，
                        // 让 UI 线程回话会超时
                        let mine = current.lock().ok().and_then(|c| c.clone());
                        let same = match (mine, std::fs::canonicalize(&path)) {
                            (Some(cur), Ok(p2)) => std::fs::canonicalize(&cur)
                                .map(|p1| p1 == p2)
                                .unwrap_or(false),
                            _ => false,
                        };
                        let _ = writeln!(writer, "{}", if same { "yes" } else { "no" });
                        let _ = writer.flush();
                        if same {
                            let _ = tx.send(path);
                        }
                    });
                }
            });
            Role::Primary(rx)
        }
        Err(_) => {
            let Some(path) = forward else { return Role::Standalone };
            let Ok(mut s) = TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", port()).parse().unwrap(),
                Duration::from_millis(600),
            ) else {
                return Role::Standalone;
            };
            let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
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

/// 注册为 .xlsx 的打开方式（当前用户，不需要管理员）
pub fn register_association(exe: &std::path::Path) -> std::io::Result<()> {
    let exe = exe.display().to_string();
    let exe_name = exe
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("LANCell.exe");
    let prog_id = "LANCell.Workbook";
    let run = |args: &[&str]| -> std::io::Result<()> {
        let out = std::process::Command::new("reg")
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
    run(&["add", &format!("HKCU\\Software\\Classes\\{prog_id}"), "/ve", "/d", "Excel 工作表（LANCell）", "/f"])?;
    run(&["add", &format!("HKCU\\Software\\Classes\\{prog_id}\\DefaultIcon"), "/ve", "/d", &format!("{exe},0"), "/f"])?;
    run(&["add", &format!("HKCU\\Software\\Classes\\{prog_id}\\shell\\open\\command"), "/ve", "/d", &format!("\"{exe}\" \"%1\""), "/f"])?;
    // 挂到 .xlsx / .xlsm 上：出现在"打开方式"列表里
    for ext in ["xlsx", "xlsm"] {
        run(&["add", &format!("HKCU\\Software\\Classes\\.{ext}\\OpenWithProgids"), "/v", prog_id, "/t", "REG_NONE", "/d", "", "/f"])?;
        run(&["add", &format!("HKCU\\Software\\Classes\\Applications\\{exe_name}\\shell\\open\\command"), "/ve", "/d", &format!("\"{exe}\" \"%1\""), "/f"])?;
    }
    // 让资源管理器立刻认到新关联
    let _ = std::process::Command::new("ie4uinit.exe").arg("-show").spawn();
    Ok(())
}

/// 把 .xlsx 直接指到我们（Windows 10 起系统仍会要求用户确认一次默认应用）
pub fn set_default(exe: &std::path::Path) -> std::io::Result<()> {
    register_association(exe)?;
    let prog_id = "LANCell.Workbook";
    for ext in ["xlsx", "xlsm"] {
        let _ = std::process::Command::new("reg")
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
