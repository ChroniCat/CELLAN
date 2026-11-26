//! 热路径只读 `SC_*` 诊断环境变量与 `app_dir` 缓存。
//!
//! - 生产构建（`#[cfg(not(test))]`）使用 `OnceLock`，整进程仅扫一次环境变量，热路径零系统调用；
//! - 测试构建（`#[cfg(test)]`）按 `test_env::env_gen()` 代数计数器同步，环境变量未变化时同样零系统调用、
//!   零堆分配，`TestEnv::new` / `TestEnv::sc` / `Drop` 改动环境变量后自动失效重读。

use std::collections::HashMap;
use std::path::PathBuf;

pub struct DebugFlags {
    pub has_any_sc: bool,
    pub ui_log: bool,
    pub ui_log_2: bool,
    pub panel_log: bool,
    pub ribbon_log: bool,
    pub ribbon_state: Option<String>,
    pub keytip_log: bool,
    pub font_log: bool,
    pub formula_log: bool,
    pub dump: bool,
    pub hit_log: bool,
    pub scroll_dbg: bool,
    pub dbg_grid: bool,
    pub clip_log: bool,
    pub palette_log: bool,
    pub page_log: bool,
    pub wiz_log: bool,
    pub tree_log: bool,
    pub auto_share_enabled: bool,
    pub auto_share_forced: bool,
    pub fnrecent_record: bool,
    pub shot: bool,
    raw: HashMap<String, String>,
}

impl DebugFlags {
    fn load_from_env() -> Self {
        let mut raw = HashMap::new();
        for (k, v) in std::env::vars_os() {
            let Some(ks) = k.to_str() else { continue };
            if !ks.starts_with("SC_") {
                continue;
            }
            // SC_HOST_ADDR 在部分集成测试中直接通过 std::env::set_var 改写，不纳入缓存。
            if ks == "SC_HOST_ADDR" {
                continue;
            }
            if let Ok(vs) = v.into_string() {
                raw.insert(ks.to_owned(), vs);
            } else {
                raw.insert(ks.to_owned(), String::new());
            }
        }
        let has_any_sc = !raw.is_empty();
        if !has_any_sc {
            return Self {
                has_any_sc: false,
                ui_log: false,
                ui_log_2: false,
                panel_log: false,
                ribbon_log: false,
                ribbon_state: None,
                keytip_log: false,
                font_log: false,
                formula_log: false,
                dump: false,
                hit_log: false,
                scroll_dbg: false,
                dbg_grid: false,
                clip_log: false,
                palette_log: false,
                page_log: false,
                wiz_log: false,
                tree_log: false,
                auto_share_enabled: true,
                auto_share_forced: false,
                fnrecent_record: false,
                shot: false,
                raw,
            };
        }
        let ui_log_val = raw.get("SC_UI_LOG");
        let auto_share_val = raw.get("SC_AUTO_SHARE").map(String::as_str);
        Self {
            has_any_sc: true,
            ui_log: ui_log_val.is_some(),
            ui_log_2: ui_log_val.map(|v| v == "2").unwrap_or(false),
            panel_log: raw.contains_key("SC_PANEL_LOG"),
            ribbon_log: raw.contains_key("SC_RIBBON_LOG"),
            ribbon_state: raw.get("SC_RIBBON_STATE").cloned(),
            keytip_log: raw.contains_key("SC_KEYTIP_LOG"),
            font_log: raw.contains_key("SC_FONT_LOG"),
            formula_log: raw.contains_key("SC_FORMULA_LOG"),
            dump: raw.contains_key("SC_DUMP"),
            hit_log: raw.contains_key("SC_HIT_LOG"),
            scroll_dbg: raw.contains_key("SC_SCROLL_DBG"),
            dbg_grid: raw.contains_key("SC_DBG_GRID"),
            clip_log: raw.contains_key("SC_CLIP_LOG"),
            palette_log: raw.contains_key("SC_PALETTE_LOG"),
            page_log: raw.contains_key("SC_PAGE_LOG"),
            wiz_log: raw.contains_key("SC_WIZ_LOG"),
            tree_log: raw.contains_key("SC_TREE_LOG"),
            auto_share_enabled: auto_share_val != Some("0"),
            auto_share_forced: auto_share_val == Some("1"),
            fnrecent_record: raw.contains_key("SC_FNRECENT_RECORD"),
            shot: raw.contains_key("SC_SHOT"),
            raw,
        }
    }

    #[inline]
    pub fn get(&self, key: &str) -> Option<&str> {
        if !self.has_any_sc {
            return None;
        }
        self.raw.get(key).map(String::as_str)
    }

    #[inline]
    pub fn contains(&self, key: &str) -> bool {
        self.has_any_sc && self.raw.contains_key(key)
    }
}

#[cfg(not(test))]
#[inline]
pub fn with<R>(f: impl FnOnce(&DebugFlags) -> R) -> R {
    static FLAGS: std::sync::OnceLock<DebugFlags> = std::sync::OnceLock::new();
    f(FLAGS.get_or_init(DebugFlags::load_from_env))
}

#[cfg(test)]
#[inline]
pub fn with<R>(f: impl FnOnce(&DebugFlags) -> R) -> R {
    use std::cell::RefCell;
    thread_local! {
        static CACHE: RefCell<(u64, Option<DebugFlags>)> = const { RefCell::new((u64::MAX, None)) };
    }
    let env_g = crate::test_env::env_gen();
    CACHE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.0 != env_g || slot.1.is_none() {
            slot.1 = Some(DebugFlags::load_from_env());
            slot.0 = env_g;
        }
        f(slot.1.as_ref().unwrap())
    })
}

#[inline]
pub fn has_any_sc() -> bool {
    with(|f| f.has_any_sc)
}

#[inline]
pub fn is_set(key: &str) -> bool {
    with(|f| f.contains(key))
}

#[inline]
pub fn var(key: &str) -> Option<String> {
    with(|f| f.get(key).map(str::to_owned))
}

#[inline]
pub fn var_eq(key: &str, expected: &str) -> bool {
    with(|f| f.get(key) == Some(expected))
}

#[inline]
pub fn ui_log() -> bool {
    with(|f| f.ui_log)
}

#[inline]
pub fn ui_log_2() -> bool {
    with(|f| f.ui_log_2)
}

#[inline]
pub fn panel_log() -> bool {
    with(|f| f.panel_log)
}

#[inline]
pub fn ribbon_log() -> bool {
    with(|f| f.ribbon_log)
}

#[inline]
pub fn ribbon_state_matches(id: &str) -> bool {
    with(|f| f.ribbon_state.as_deref() == Some(id))
}

#[inline]
pub fn keytip_log() -> bool {
    with(|f| f.keytip_log)
}

#[inline]
pub fn font_log() -> bool {
    with(|f| f.font_log)
}

#[inline]
pub fn formula_log() -> bool {
    with(|f| f.formula_log)
}

#[inline]
pub fn dump() -> bool {
    with(|f| f.dump)
}

#[inline]
pub fn hit_log() -> bool {
    with(|f| f.hit_log)
}

#[inline]
pub fn scroll_dbg() -> bool {
    with(|f| f.scroll_dbg)
}

#[inline]
pub fn dbg_grid() -> bool {
    with(|f| f.dbg_grid)
}

#[inline]
pub fn clip_log() -> bool {
    with(|f| f.clip_log)
}

#[inline]
pub fn palette_log() -> bool {
    with(|f| f.palette_log)
}

#[inline]
pub fn page_log() -> bool {
    with(|f| f.page_log)
}

#[inline]
pub fn wiz_log() -> bool {
    with(|f| f.wiz_log)
}

#[inline]
pub fn tree_log() -> bool {
    with(|f| f.tree_log)
}

#[inline]
pub fn auto_share_enabled() -> bool {
    with(|f| f.auto_share_enabled)
}

#[inline]
pub fn auto_share_forced() -> bool {
    with(|f| f.auto_share_forced)
}

#[inline]
pub fn fnrecent_record() -> bool {
    with(|f| f.fnrecent_record)
}

#[inline]
pub fn shot() -> bool {
    with(|f| f.shot)
}

/// 缓存应用配置目录 `%LOCALAPPDATA%\CELLAN`（生产 `OnceLock`，测试按 `env_gen` 同步）。
#[cfg(not(test))]
pub fn app_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(crate::update::app_dir).clone()
}

#[cfg(test)]
pub fn app_dir() -> PathBuf {
    use std::cell::RefCell;
    thread_local! {
        static DIR_CACHE: RefCell<(u64, Option<PathBuf>)> = const { RefCell::new((u64::MAX, None)) };
    }
    let env_g = crate::test_env::env_gen();
    DIR_CACHE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.0 != env_g || slot.1.is_none() {
            slot.1 = Some(crate::update::app_dir());
            slot.0 = env_g;
        }
        slot.1.as_ref().unwrap().clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    #[test]
    fn debug_flags_tracks_test_env_changes_and_app_dir() {
        let env = TestEnv::new("dbg-flags");
        assert!(!has_any_sc());
        assert!(!ui_log());
        assert!(auto_share_enabled());
        assert_eq!(app_dir(), env.profile().join("CELLAN"));

        env.sc("SC_UI_LOG", "2");
        env.sc("SC_AUTO_SHARE", "0");
        env.sc("SC_RIBBON_STATE", "Bold");
        assert!(has_any_sc());
        assert!(ui_log());
        assert!(ui_log_2());
        assert!(!auto_share_enabled());
        assert!(ribbon_state_matches("Bold"));
        assert!(!ribbon_state_matches("Italic"));
        assert_eq!(var("SC_UI_LOG").as_deref(), Some("2"));

        drop(env);
        let env2 = TestEnv::new("dbg-flags-2");
        assert!(!has_any_sc());
        assert!(!ui_log());
        assert!(auto_share_enabled());
        assert_eq!(app_dir(), env2.profile().join("CELLAN"));
    }
}
