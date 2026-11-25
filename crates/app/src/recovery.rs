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
}

impl RecoveryEntry {
    pub fn snapshot_path(&self) -> PathBuf {
        recovery_dir().join(format!("{}.xlsx", self.token))
    }

    pub fn meta_path(&self) -> PathBuf {
        recovery_dir().join(format!("{}.meta", self.token))
    }

    fn encode_meta(&self) -> String {
        let orig = self.original_path.as_deref().unwrap_or("").replace('\n', " ");
        let title = self.title.replace('\n', " ");
        format!(
            "token={}\ntitle={}\noriginal_path={}\nsaved_unix_secs={}\npid={}\ndoc_uid={}\n",
            self.token, title, orig, self.saved_unix_secs, self.pid, self.doc_uid
        )
    }

    fn decode_meta(text: &str) -> Option<Self> {
        let mut token = None;
        let mut title = String::new();
        let mut original_path = None;
        let mut saved_unix_secs = 0u64;
        let mut pid = 0u32;
        let mut doc_uid = 0u64;
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
    // Check via OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) + GetExitCodeProcess
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

#[cfg(not(windows))]
fn is_process_alive(pid: u32) -> bool {
    pid == std::process::id()
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
    let _ = std::fs::remove_file(&meta);
    std::fs::rename(&tmp_meta, &meta)?;
    Ok(())
}

/// 删除指定 token 的恢复草稿与元数据。
pub fn remove_recovery(token: &str) {
    let dir = recovery_dir();
    let _ = std::fs::remove_file(dir.join(format!("{token}.xlsx")));
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

pub fn build_entry(doc_uid: u64, title: &str, original_path: Option<&Path>) -> RecoveryEntry {
    let pid = std::process::id();
    RecoveryEntry {
        token: make_token(pid, doc_uid),
        title: title.to_string(),
        original_path: original_path.map(|p| p.display().to_string()),
        saved_unix_secs: now_unix_secs(),
        pid,
        doc_uid,
    }
}

enum WorkerCmd {
    Save(RecoveryEntry, Workbook),
    Remove(String),
}

/// 常驻轻量后台写盘线程：把恢复草稿序列化与压缩移出界面线程。
pub struct RecoveryWorker {
    tx: mpsc::Sender<WorkerCmd>,
}

impl RecoveryWorker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<WorkerCmd>();
        let _ = std::thread::Builder::new()
            .name("cellan-recovery".into())
            .spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        WorkerCmd::Save(entry, wb) => {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                let _ = write_recovery_snapshot(&entry, &wb);
                            }));
                        }
                        WorkerCmd::Remove(token) => {
                            remove_recovery(&token);
                        }
                    }
                }
            });
        Self { tx }
    }

    pub fn enqueue_save(&self, entry: RecoveryEntry, wb: Workbook) {
        let _ = self.tx.send(WorkerCmd::Save(entry, wb));
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
