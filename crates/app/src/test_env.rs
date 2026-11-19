//! 测试环境守卫：一次测试一个专用目录（QA-03.1）。
//!
//! 测试不得读写真实用户目录（`%LOCALAPPDATA%\CELLAN`）或系统临时目录，也不得继承
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
    /// 由 [`EnvTxn`] 移交的「进入测试前的原值」：随守卫一起还原。
    extra: Vec<(String, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

/// 测试根目录：**不能**用 `std::env::temp_dir()`——并行测试里别人随时会把 TEMP
/// 换成自己的隔离目录，路径就会嵌套到别人的根下面，别人一 drop 就整个被删。
/// 固定在仓库 `target/test-tmp/`，与进程环境无关。
pub fn run_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        // 用 parent() 逐级上退，不要写 "../../"：路径里留 ".." 会和
        // `std::env::temp_dir()` 的规范路径比不相等（路径带不带 .. 语义相同但字节不同）。
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or_else(|| Path::new("."))
            .join("target")
            .join("test-tmp");
        std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("建测试根失败 {}: {e}", p.display()));
        p
    })
    .clone()
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
        Self::building(crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()), tag)
    }

    /// 用调用方已经持有的 `ENV_LOCK` 构造。测试要「先写哨兵值再构造守卫」时必须走这里：
    /// 先锁、再写、再把同一个 guard 交进来，中间没有放锁窗口，也就没有别的测试线程
    /// 插进来改 TEMP/TMP/LOCALAPPDATA 的竞态。
    pub fn building(lock: MutexGuard<'static, ()>, tag: &str) -> Self {
        let base = run_root();
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

        if std::env::var_os("SC_ENV_TRACE").is_some() {
            eprintln!("[env-new] {:?} root={} TEMP={:?}", std::thread::current().id(), root.display(), std::env::var_os("TEMP"));
        }
        Self { root, base, saved, extra: Vec::new(), _lock: lock }
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

    /// 进入测试前的环境快照（测试要核对「Drop 后是否还原」，必须用这份，
    /// 不能在锁外自己读——锁外读到的可能是别的测试正在用的临时根）。
    pub fn entry_value(&self, key: &str) -> Option<OsString> {
        self.extra.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.clone())
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
    let base = run_root();
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
        // 先还原「构造时看到的」（可能是测试自己写的哨兵），最后用 extra 覆盖成
        // 「进入测试前的原值」——顺序反了就会把进程留在哨兵路径上。
        for (key, value) in self.saved.iter() {
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        for (key, value) in self.extra.iter() {
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

    /// 一次「改进程环境变量」的完整事务，全程 RAII：
    /// 先在锁内保存原值，再交给 [`TestEnv`] 随守卫一起还原——测试 panic 也不会把
    /// 进程环境留在哨兵值上。**保存原值必须在拿到锁之后**，否则可能存到别的测试的临时根。
    struct EnvTxn {
        guard: Option<MutexGuard<'static, ()>>,
        saved: Option<Vec<(String, Option<OsString>)>>,
    }

    impl EnvTxn {
        fn begin(keys: &[&'static str]) -> Self {
            let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = keys.iter().map(|k| (k.to_string(), std::env::var_os(k))).collect();
            Self { guard: Some(guard), saved: Some(saved) }
        }

        fn set(&self, key: &'static str, value: &str) {
            // SAFETY: 本事务持有 ENV_LOCK
            unsafe { std::env::set_var(key, value) };
        }

        /// 写哨兵值并交出守卫；原值挂到 TestEnv 上，Drop 时按「先 extra 后三项」还原。
        fn env(mut self, tag: &str, sentinels: &[(&'static str, PathBuf)]) -> TestEnv {
            let guard = self.guard.take().expect("事务已交出");
            for (k, v) in sentinels {
                // SAFETY: 仍持有 ENV_LOCK
                unsafe { std::env::set_var(k, v) };
            }
            let mut e = TestEnv::building(guard, tag);
            e.extra = self.saved.take().unwrap_or_default();
            e
        }
    }

    impl Drop for EnvTxn {
        fn drop(&mut self) {
            // 只有还没交给 TestEnv 的情况才需要在这里还原（panic 早退也走这里）。
            if let Some(saved) = self.saved.take() {
                for (k, v) in saved {
                    match v {
                        Some(v) => unsafe { std::env::set_var(&k, v) },
                        None => unsafe { std::env::remove_var(&k) },
                    }
                }
            }
        }
    }

    #[test]
    fn isolates_temp_and_restores_after_drop() {
        // 入口原值由事务在锁内取；这里不能在锁外先读（可能读到别的测试正在用的根）。
        let (temp, tmp, la) = sentinel("restore");
        let txn = EnvTxn::begin(&["TEMP", "TMP", "LOCALAPPDATA"]);
        let env = txn.env(
            "restore",
            &[
                ("TEMP", PathBuf::from(&temp)),
                ("TMP", PathBuf::from(&tmp)),
                ("LOCALAPPDATA", PathBuf::from(&la)),
            ],
        );
        let root = env.root().to_path_buf();
        assert_eq!(std::env::temp_dir(), root, "TEMP/TMP 必须指向本次运行目录");
        assert_eq!(
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            Some(env.profile()),
            "LOCALAPPDATA 必须在本次运行目录下"
        );
        std::fs::write(env.path("probe.txt"), b"x").unwrap();
        assert!(env.path("probe.txt").is_file());
        // 锁内取得的入口快照，作为 Drop 后的比对基准
        let want = (
            env.entry_value("TEMP"),
            env.entry_value("TMP"),
            env.entry_value("LOCALAPPDATA"),
        );
        drop(env);

        assert!(!root.exists(), "Drop 后必须删掉本次运行目录：{}", root.display());
        let after = {
            let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            (
                std::env::var_os("TEMP"),
                std::env::var_os("TMP"),
                std::env::var_os("LOCALAPPDATA"),
            )
        };
        assert_eq!(after, want, "TEMP/TMP/LOCALAPPDATA 必须还原成进入测试前的值");
    }

    #[test]
    fn clears_inherited_probe_vars_and_does_not_restore_them() {
        let txn = EnvTxn::begin(&["SC_B1", "SC_DUMP"]);
        txn.set("SC_B1", "F:/Finance/work/should-never-run");
        txn.set("SC_DUMP", "C:/somewhere/else.txt");
        let env = txn.env("sc-clear", &[]);
        assert!(std::env::var_os("SC_B1").is_none(), "继承的 SC_B1 必须清掉");
        assert!(std::env::var_os("SC_DUMP").is_none(), "继承的 SC_DUMP 必须清掉");
        env.sc("SC_DUMP", "allowed.txt");
        assert_eq!(std::env::var("SC_DUMP").unwrap(), "allowed.txt");
        drop(env);

        let leftovers = {
            let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            (std::env::var_os("SC_B1"), std::env::var_os("SC_DUMP"))
        };
        assert!(leftovers.0.is_none(), "SC_* 不得在测试后残留");
        assert!(leftovers.1.is_none(), "SC_* 不得在测试后残留");
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

    #[test]
    fn each_guard_gets_its_own_directory() {
        let a = TestEnv::new("unique-a");
        let a_root = a.root().to_path_buf();
        drop(a);
        let b = TestEnv::new("unique-b");
        assert_ne!(a_root, b.root());
        assert!(!a_root.exists());
    }
}
