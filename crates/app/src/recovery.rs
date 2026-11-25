//! 后台自动恢复草稿（AutoRecover）：定期把未保存的工作簿（含新建未命名工作簿）
//! 异步写入独立恢复区 `%LOCALAPPDATA%\CELLAN\recovery\`，绝不覆盖原文件。
//! 正常保存或显式放弃更改后清理对应草稿；下次启动时自动发现崩溃残留草稿并提示恢复。

use sheetcore::Workbook;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 恢复区里的一条可恢复工作簿元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryEntry {
    pub token: String,
    pub title: String,
    pub original_path: Option<String>,
    pub saved_unix_secs: u64,
    pub pid: u32,
    pub doc_uid: u64,
    /// 快照文件扩展名（`xlsx` 或 `xlsm`，兼容旧版无此字段的 `.meta` 默认 `xlsx`）
    pub ext: String,
}

impl RecoveryEntry {
    pub fn snapshot_path(&self) -> PathBuf {
        let ext = if self.ext.is_empty() { "xlsx" } else { self.ext.as_str() };
        recovery_dir().join(format!("{}.{ext}", self.token))
    }

    pub fn meta_path(&self) -> PathBuf {
        recovery_dir().join(format!("{}.meta", self.token))
    }

    fn encode_meta(&self) -> String {
        let orig = self.original_path.as_deref().unwrap_or("").replace('\n', " ");
        let title = self.title.replace('\n', " ");
        let ext = if self.ext.is_empty() { "xlsx" } else { self.ext.as_str() };
        format!(
            "token={}\ntitle={}\noriginal_path={}\nsaved_unix_secs={}\npid={}\ndoc_uid={}\next={}\n",
            self.token, title, orig, self.saved_unix_secs, self.pid, self.doc_uid, ext
        )
    }

    fn decode_meta(text: &str) -> Option<Self> {
        let mut token = None;
        let mut title = String::new();
        let mut original_path = None;
        let mut saved_unix_secs = 0u64;
        let mut pid = 0u32;
        let mut doc_uid = 0u64;
        let mut ext = String::from("xlsx");
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "token" => token = Some(v.to_string()),
                "title" => title = v.to_string(),
                "original_path" => {
                    if !v.is_empty() {
                        original_path = Some(v.to_string());
                    }
                }
                "saved_unix_secs" => saved_unix_secs = v.trim().parse().unwrap_or(0),
                "pid" => pid = v.trim().parse().unwrap_or(0),
                "doc_uid" => doc_uid = v.trim().parse().unwrap_or(0),
                "ext" => {
                    let e = v.trim();
                    if e == "xlsx" || e == "xlsm" {
                        ext = e.to_string();
                    }
                }
                _ => {}
            }
        }
        Some(Self {
            token: token?,
            title,
            original_path,
            saved_unix_secs,
            pid,
            doc_uid,
            ext,
        })
    }
}

pub fn recovery_dir() -> PathBuf {
    crate::debug_flags::app_dir().join("recovery")
}

pub fn make_token(pid: u32, doc_uid: u64) -> String {
    format!("rec-{pid}-{doc_uid}")
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        let mut same_exe = true;
        if ok != 0 && code == STILL_ACTIVE as u32 {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) != 0 && len > 0 {
                let other_path = String::from_utf16_lossy(&buf[..len as usize]);
                let other_name = Path::new(&other_path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if let Ok(cur_exe) = std::env::current_exe() {
                    let cur_name = cur_exe.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if !cur_name.is_empty() && !other_name.is_empty() {
                        same_exe = other_name.eq_ignore_ascii_case(cur_name);
                    }
                }
            }
        }
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE as u32 && same_exe
    }
}

#[cfg(not(windows))]
fn is_process_alive(pid: u32) -> bool {
    pid == std::process::id()
}

fn replace_file_atomic(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let src_w: Vec<u16> = tmp.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let dst_w: Vec<u16> = dest.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let ok = unsafe {
            MoveFileExW(
                src_w.as_ptr(),
                dst_w.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, dest)
    }
}

/// 同步写入一份恢复草稿（供后台线程与单测调用）。
pub fn write_recovery_snapshot(entry: &RecoveryEntry, wb: &Workbook) -> anyhow::Result<()> {
    let dir = recovery_dir();
    std::fs::create_dir_all(&dir)?;
    let snap = entry.snapshot_path();
    let meta = entry.meta_path();
    sheetcore::xlsx::save_atomic(&snap, wb)?;
    let tmp_meta = dir.join(format!("{}.meta.tmp", entry.token));
    std::fs::write(&tmp_meta, entry.encode_meta())?;
    replace_file_atomic(&tmp_meta, &meta)?;
    Ok(())
}

/// 删除指定 token 的恢复草稿与元数据。
pub fn remove_recovery(token: &str) {
    let dir = recovery_dir();
    let _ = std::fs::remove_file(dir.join(format!("{token}.xlsx")));
    let _ = std::fs::remove_file(dir.join(format!("{token}.xlsm")));
    let _ = std::fs::remove_file(dir.join(format!("{token}.meta")));
    let _ = std::fs::remove_file(dir.join(format!("{token}.meta.json")));
    let _ = std::fs::remove_file(dir.join(format!("{token}.meta.tmp")));
}

pub fn remove_for_doc(doc_uid: u64) {
    remove_recovery(&make_token(std::process::id(), doc_uid));
}

pub fn remove_entry(entry: &RecoveryEntry) {
    remove_recovery(&entry.token);
}

/// 扫描上次异常退出遗留的可恢复草稿（排除当前进程或其他仍在运行的 CELLAN 实例正在写的草稿）。
pub fn scan_orphaned_entries() -> Vec<RecoveryEntry> {
    let dir = recovery_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in rd.flatten() {
        let p = item.path();
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".meta") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Some(entry) = RecoveryEntry::decode_meta(&text) else {
            continue;
        };
        if is_process_alive(entry.pid) {
            continue;
        }
        if entry.snapshot_path().is_file() {
            out.push(entry);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
    out.sort_by(|a, b| b.saved_unix_secs.cmp(&a.saved_unix_secs));
    out
}

pub fn build_entry(
    doc_uid: u64,
    title: &str,
    original_path: Option<&Path>,
    has_vba: bool,
) -> RecoveryEntry {
    let pid = std::process::id();
    let is_xlsm = has_vba
        || original_path
            .and_then(|p| p.extension().and_then(|e| e.to_str()))
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsm"));
    RecoveryEntry {
        token: make_token(pid, doc_uid),
        title: title.to_string(),
        original_path: original_path.map(|p| p.display().to_string()),
        saved_unix_secs: now_unix_secs(),
        pid,
        doc_uid,
        ext: if is_xlsm { "xlsm".into() } else { "xlsx".into() },
    }
}

enum WorkerCmd {
    Save(RecoveryEntry, Workbook, u64),
    Remove(String),
}

pub struct RecoveryResult {
    pub doc_uid: u64,
    pub doc_gen: u64,
    pub err: Option<String>,
}

/// 常驻轻量后台写盘线程：把恢复草稿序列化与压缩移出界面线程。
pub struct RecoveryWorker {
    tx: mpsc::Sender<WorkerCmd>,
    rx: mpsc::Receiver<RecoveryResult>,
}

impl RecoveryWorker {
    pub fn new() -> Self {
        let (tx, rx_cmd) = mpsc::channel::<WorkerCmd>();
        let (tx_res, rx) = mpsc::channel::<RecoveryResult>();
        let _ = std::thread::Builder::new()
            .name("cellan-recovery".into())
            .spawn(move || {
                while let Ok(cmd) = rx_cmd.recv() {
                    match cmd {
                        WorkerCmd::Save(entry, wb, doc_gen) => {
                            let doc_uid = entry.doc_uid;
                            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                write_recovery_snapshot(&entry, &wb)
                            }));
                            let err = match res {
                                Ok(Ok(())) => None,
                                Ok(Err(e)) => Some(format!("{e:#}")),
                                Err(_) => Some("自动恢复写入线程异常".into()),
                            };
                            let _ = tx_res.send(RecoveryResult {
                                doc_uid,
                                doc_gen,
                                err,
                            });
                        }
                        WorkerCmd::Remove(token) => {
                            remove_recovery(&token);
                        }
                    }
                }
            });
        Self { tx, rx }
    }

    pub fn enqueue_save(&self, entry: RecoveryEntry, wb: Workbook, doc_gen: u64) {
        let _ = self.tx.send(WorkerCmd::Save(entry, wb, doc_gen));
    }

    pub fn poll_results(&self) -> Vec<RecoveryResult> {
        let mut out = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            out.push(r);
        }
        out
    }

    pub fn enqueue_remove(&self, token: String) {
        let _ = self.tx.send(WorkerCmd::Remove(token));
    }
}

impl Default for RecoveryWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;
    use sheetcore::{Cell, Value};

    #[test]
    fn recovery_snapshot_and_orphan_scan_isolated_in_test_env() {
        let _env = TestEnv::new("recovery-unit");
        let mut wb = Workbook::new_blank();
        wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Text("未命名草稿".into()),
                formula: None,
            },
        );
        // Simulate a dead process (pid = 0) so orphan scanner picks it up.
        let entry = RecoveryEntry {
            token: "rec-0-42".into(),
            title: "未命名.xlsx".into(),
            original_path: None,
            saved_unix_secs: 1_700_000_000,
            pid: 0,
            doc_uid: 42,
            ext: "xlsx".into(),
        };
        write_recovery_snapshot(&entry, &wb).unwrap();
        let found = scan_orphaned_entries();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].token, "rec-0-42");
        assert!(found[0].original_path.is_none());

        let loaded = sheetcore::xlsx::load(&found[0].snapshot_path()).unwrap();
        assert_eq!(loaded.sheet(0).value(0, 0), Value::Text("未命名草稿".into()));

        remove_recovery(&entry.token);
        assert!(scan_orphaned_entries().is_empty());
    }
}
