//! 测试环境守卫：一次测试一个专用目录（QA-03.1）。
//!
//! 测试不得读写真实用户目录（`%LOCALAPPDATA%\SheetsNative`）或系统临时目录，也不得继承
//! 开发机上残留的 `SC_*` 探针变量。守卫在构造时：
//!
//! 1. 取 `ENV_LOCK`，让所有改进程环境变量的测试串行；
//! 2. 把 `TEMP`/`TMP` 指向 `<base>/<pid>-<n>-<tag>`，`LOCALAPPDATA` 指向其 `profile/`；
//! 3. 清空全部继承的 `SC_*`；
//! 4. Drop 时逐项恢复原值，并且只在目录确实位于本次运行的 base 之下时才删除它。
//!
//! 需要探针变量的测试必须用 [`TestEnv::sc`] 显式设置（allowlist），Drop 时一并清掉。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::MutexGuard;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 一次测试的隔离环境。存活期间 TEMP/TMP/LOCALAPPDATA 都指向它自己的目录。
pub struct TestEnv {
    root: PathBuf,
    base: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

fn sc_names() -> Vec<OsString> {
    std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| k.to_string_lossy().starts_with("SC_"))
        .collect()
}

fn set_var(key: &str, value: &Path) {
    // SAFETY: 全程持有 ENV_LOCK，本进程内没有其它线程同时读写这些变量。
    unsafe { std::env::set_var(key, value) };
}

impl TestEnv {
    pub fn new(tag: &str) -> Self {
        let lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join("sheets-native-tests");
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let safe: String = tag
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let root = base.join(format!("{}-{}-{}", std::process::id(), n, safe));
        std::fs::create_dir_all(root.join("profile"))
            .unwrap_or_else(|e| panic!("建测试目录失败 {}: {e}", root.display()));

        for key in sc_names() {
            // SAFETY: 同上，ENV_LOCK 已持有。
            unsafe { std::env::remove_var(&key) };
        }
        let saved = ["TEMP", "TMP", "LOCALAPPDATA"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect::<Vec<_>>();
        for (key, _) in saved.iter() {
            set_var(key, &root);
        }
        set_var("LOCALAPPDATA", &root.join("profile"));

        Self { root, base, saved, _lock: lock }
    }

    /// 本次测试的根目录（TEMP/TMP 指向这里）
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 模拟 `%LOCALAPPDATA%` 的目录（recent.txt / ui.ini / views 都在这里）
    pub fn profile(&self) -> PathBuf {
        self.root.join("profile")
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// 显式允许一个 `SC_*` 探针变量（只在本次测试期间有效，Drop 后不会残留）
    pub fn sc(&self, name: &str, value: &str) {
        assert!(name.starts_with("SC_"), "只允许 SC_* 探针：{name}");
        // SAFETY: 持有 ENV_LOCK。
        unsafe { std::env::set_var(name, value) };
    }
}

/// 只求「互不覆盖」的测试夹具目录：不改进程环境变量，路径在 TEMP 下的
/// `sheets-native-tests/`，按 pid + 序号 + 标签唯一。用于会生成工作簿/日志、
/// 但不需要隔离 LOCALAPPDATA 的测试；固定目录名在两次测试进程并行时会互相踩。
pub fn scratch(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join("sheets-native-tests");
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let safe: String = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = base.join(format!("scratch-{}-{}-{}", std::process::id(), n, safe));
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("建测试目录失败 {}: {e}", dir.display()));
    dir
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (key, value) in self.saved.iter() {
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        for key in sc_names() {
            // SAFETY: 仍持有 ENV_LOCK（字段 drop 顺序：先执行本函数，再释放 _lock）。
            unsafe { std::env::remove_var(&key) };
        }
        // 只删自己建的目录：必须是 base 的子路径，且不等于 base 本身。
        if self.root.starts_with(&self.base) && self.root != self.base {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentinel(tag: &str) -> (OsString, OsString, OsString) {
        let root = std::env::temp_dir().join(format!("sn-sentinel-{tag}"));
        let _ = std::fs::create_dir_all(&root);
        (
            root.clone().into_os_string(),
            root.clone().into_os_string(),
            root.join("profile").into_os_string(),
        )
    }

    #[test]
    fn isolates_temp_and_restores_after_drop() {
        // 先把三个变量写成哨兵值，守卫必须原样恢复它们（而不是恢复成开发机真实目录）。
        let (temp, tmp, la) = sentinel("restore");
        {
            let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: 持有 ENV_LOCK
            unsafe {
                std::env::set_var("TEMP", &temp);
                std::env::set_var("TMP", &tmp);
                std::env::set_var("LOCALAPPDATA", &la);
            }
        }

        let root;
        {
            let env = TestEnv::new("restore");
            root = env.root().to_path_buf();
            assert_eq!(std::env::temp_dir(), root, "TEMP/TMP 必须指向本次运行目录");
            assert_eq!(
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
                Some(env.profile()),
                "LOCALAPPDATA 必须在本次运行目录下"
            );
            std::fs::write(env.path("probe.txt"), b"x").unwrap();
            assert!(env.path("probe.txt").is_file());
        }
        assert!(!root.exists(), "Drop 后必须删掉本次运行目录：{}", root.display());
        assert_eq!(std::env::var_os("TEMP"), Some(temp), "TEMP 必须恢复");
        assert_eq!(std::env::var_os("TMP"), Some(tmp), "TMP 必须恢复");
        assert_eq!(std::env::var_os("LOCALAPPDATA"), Some(la), "LOCALAPPDATA 必须恢复");
    }

    #[test]
    fn clears_inherited_probe_vars_and_does_not_restore_them() {
        {
            let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: 持有 ENV_LOCK
            unsafe {
                std::env::set_var("SC_B1", "F:/Finance/work/should-never-run");
                std::env::set_var("SC_DUMP", "C:/somewhere/else.txt");
            }
        }
        {
            let env = TestEnv::new("sc-clear");
            assert!(std::env::var_os("SC_B1").is_none(), "继承的 SC_B1 必须清掉");
            assert!(std::env::var_os("SC_DUMP").is_none(), "继承的 SC_DUMP 必须清掉");
            env.sc("SC_DUMP", "allowed.txt");
            assert_eq!(std::env::var("SC_DUMP").unwrap(), "allowed.txt");
        }
        assert!(std::env::var_os("SC_B1").is_none(), "SC_* 不得在测试后残留");
        assert!(std::env::var_os("SC_DUMP").is_none(), "SC_* 不得在测试后残留");
    }

    #[test]
    fn each_guard_gets_its_own_directory() {
        let a = TestEnv::new("unique-a");
        let a_root = a.root().to_path_buf();
        drop(a);
        let b = TestEnv::new("unique-b");
        assert_ne!(a_root, b.root());
        assert!(!a_root.exists());
    }

    #[test]
    fn drop_only_removes_the_guard_own_directory() {
        let env = TestEnv::new("base-check");
        let base = env.base.clone();
        let root = env.root().to_path_buf();
        std::fs::create_dir_all(&base).unwrap();
        let marker = base.join("shared-marker.txt");
        std::fs::write(&marker, b"keep").unwrap();
        drop(env);
        assert!(!root.exists(), "本次运行目录要删");
        assert!(marker.is_file(), "共享 base 与其它运行的文件不能被删");
        let _ = std::fs::remove_file(&marker);
    }
}
