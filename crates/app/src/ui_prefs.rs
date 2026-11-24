//! 界面偏好：写在 `%LOCALAPPDATA%\CELLAN\ui.ini`。
//!
//! 和错误检查选项一样是应用级设置（不跟文件走），只放「下次打开还想保持」的开关。

use sheetcore::zhconv::Variant;

pub fn variant_to_u8(v: Variant) -> u8 {
    match v {
        Variant::Generic => 0,
        Variant::Taiwan => 1,
        Variant::HongKong => 2,
    }
}

pub fn variant_from_u8(n: u8) -> Variant {
    match n {
        1 => Variant::Taiwan,
        2 => Variant::HongKong,
        _ => Variant::Generic,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiPrefs {
    /// 上次用的简繁变体（0 通用 / 1 台湾 / 2 香港）
    pub zh_variant: u8,
    /// 校对结果按类型分组显示
    pub proof_group: bool,
    /// 状态栏统计项位掩码：1 平均、2 计数、4 数值计数、8 求和、16 最小、32 最大
    pub stat_mask: u8,
    /// 本机在协作/修订里显示的名字；空表示用系统默认名
    pub user_name: String,
    /// WPS 聚光灯（阅读模式）开关
    pub spotlight: bool,
    /// 自动将已提交的编辑写回已有文件。
    pub autosave: bool,
    /// 最近一次自动探测成功的图形后端（`wgpu` / `glow`；空表示尚未探测或已重置）
    pub gpu_backend: String,
    /// 与 `gpu_backend` 绑定的轻量环境特征（OS-架构 | 主机名 | console/rdp 会话类型）
    pub gpu_backend_env: String,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            zh_variant: 0,
            proof_group: true,
            stat_mask: 0b00_1011,
            user_name: String::new(),
            spotlight: false,
            autosave: false,
            gpu_backend: String::new(),
            gpu_backend_env: String::new(),
        }
    }
}

pub fn normalize_gpu_backend(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "wgpu" => Some("wgpu"),
        "glow" => Some("glow"),
        _ => None,
    }
}

fn sanitize_ini_value(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect()
}

/// 生成与持久化图形后端绑定的轻量环境特征（零外设/零 COM 调用）。
/// 区分本机桌面（`console`）与远程桌面（`rdp`）以及主机名，环境变化时自动重探。
pub fn gpu_env_fingerprint() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let session = std::env::var("SESSIONNAME").unwrap_or_default();
    gpu_env_fingerprint_from(std::env::consts::OS, std::env::consts::ARCH, &host, &session)
}

pub fn gpu_env_fingerprint_from(os: &str, arch: &str, host: &str, session: &str) -> String {
    let clean_host: String = host
        .trim()
        .chars()
        .filter(|c| !c.is_control() && *c != '|')
        .take(48)
        .collect();
    let sess_lower = session.trim().to_ascii_lowercase();
    let sess_kind = if sess_lower.starts_with("rdp") {
        "rdp"
    } else if sess_lower.is_empty() || sess_lower == "console" {
        "console"
    } else {
        "other"
    };
    format!("{os}-{arch}|{clean_host}|{sess_kind}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendPlan {
    pub candidates: Vec<&'static str>,
    pub is_auto: bool,
    pub used_cache: bool,
}

/// 解析图形后端请求：命令行 `--renderer=` 优先，其次环境变量 `SC_BACKEND`，再次兼容 `SC_RENDERER`，默认 `"auto"`。
pub fn resolve_backend_request(
    cli_renderer: Option<&str>,
    sc_backend: Option<&str>,
    sc_renderer: Option<&str>,
) -> String {
    for raw in [cli_renderer, sc_backend, sc_renderer].into_iter().flatten() {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_ascii_lowercase();
        }
    }
    "auto".to_string()
}

/// 计算启动时尝试的图形后端顺序：
/// - 显式指定 `wgpu` / `glow` 时严格尊重覆盖，不依赖缓存；
/// - `auto` 模式下优先命中已验证缓存后端（避免重复双窗口冷启动）；
/// - 无有效缓存时，Windows 默认优先 `wgpu`（DX12），初始化失败再自动回退 `glow`（OpenGL 3.3）。
pub fn plan_gpu_backends(
    request: &str,
    cached: Option<&str>,
    has_wgpu: bool,
    has_glow: bool,
    is_windows: bool,
) -> BackendPlan {
    let req = request.trim().to_ascii_lowercase();
    let (mut candidates, is_auto, mut used_cache) = match req.as_str() {
        "wgpu" => (vec!["wgpu"], false, false),
        "glow" => (vec!["glow"], false, false),
        _ => {
            let default_order: Vec<&'static str> = if is_windows {
                vec!["wgpu", "glow"]
            } else {
                vec!["glow", "wgpu"]
            };
            if let Some(hit) = cached.and_then(normalize_gpu_backend) {
                let mut ordered = vec![hit];
                for b in default_order {
                    if b != hit {
                        ordered.push(b);
                    }
                }
                (ordered, true, true)
            } else {
                (default_order, true, false)
            }
        }
    };
    let cached_first = candidates.first().copied();
    candidates.retain(|c| match *c {
        "wgpu" => has_wgpu,
        "glow" => has_glow,
        _ => false,
    });
    if candidates.is_empty() {
        if has_wgpu {
            candidates.push("wgpu");
        } else {
            candidates.push("glow");
        }
    }
    if used_cache && candidates.first().copied() != cached_first {
        used_cache = false;
    }
    BackendPlan {
        candidates,
        is_auto,
        used_cache,
    }
}

/// 在自动探测模式下持久化已验证成功的图形后端（保留其它 ui.ini 偏好）。
pub fn persist_probed_gpu_backend(backend: &str, env_fp: &str) {
    let Some(norm_backend) = normalize_gpu_backend(backend) else {
        return;
    };
    let mut prefs = UiPrefs::load();
    if prefs.cached_gpu_backend(env_fp) == Some(norm_backend) && prefs.gpu_backend_env == env_fp {
        return;
    }
    prefs.set_gpu_backend(norm_backend, env_fp);
    prefs.save();
}

/// 已缓存后端启动失败时清除记录，确保回退与下次冷启动重新探测。
pub fn reset_cached_gpu_backend() {
    let mut prefs = UiPrefs::load();
    if prefs.gpu_backend.is_empty() && prefs.gpu_backend_env.is_empty() {
        return;
    }
    prefs.clear_gpu_backend();
    prefs.save();
}

fn path() -> Option<std::path::PathBuf> {
    Some(crate::update::app_dir_opt()?.join("ui.ini"))
}

impl UiPrefs {
    /// 若缓存的图形后端有效且与当前环境特征匹配，返回 `"wgpu"` 或 `"glow"`。
    pub fn cached_gpu_backend(&self, current_env: &str) -> Option<&'static str> {
        let backend = normalize_gpu_backend(&self.gpu_backend)?;
        if !self.gpu_backend_env.is_empty() && self.gpu_backend_env != current_env {
            return None;
        }
        Some(backend)
    }

    pub fn set_gpu_backend(&mut self, backend: &str, current_env: &str) {
        if let Some(b) = normalize_gpu_backend(backend) {
            self.gpu_backend = b.to_string();
            self.gpu_backend_env = sanitize_ini_value(current_env);
        }
    }

    pub fn clear_gpu_backend(&mut self) {
        self.gpu_backend.clear();
        self.gpu_backend_env.clear();
    }

    /// 从指定文件读（测试用，不碰环境变量）
    pub fn load_from(p: &std::path::Path) -> Self {
        let mut me = Self::default();
        let Ok(text) = std::fs::read_to_string(p) else {
            return me;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "zh_variant" => {
                    if let Ok(n) = v.parse::<u8>() {
                        me.zh_variant = n.min(2);
                    }
                }
                "proof_group" => me.proof_group = v == "1",
                "stat_mask" => {
                    if let Ok(n) = v.parse::<u8>() {
                        me.stat_mask = n & 0b11_1111;
                    }
                }
                "user_name" => me.user_name = v.to_string(),
                "spotlight" => me.spotlight = v == "1",
                "autosave" => me.autosave = v == "1",
                "gpu_backend" => {
                    me.gpu_backend = normalize_gpu_backend(v).unwrap_or("").to_string();
                }
                "gpu_backend_env" => {
                    me.gpu_backend_env = sanitize_ini_value(v);
                }
                _ => {}
            }
        }
        me
    }

    pub fn load() -> Self {
        match path() {
            Some(p) => Self::load_from(&p),
            None => Self::default(),
        }
    }

    /// 写到指定文件（测试用）
    pub fn save_to(&self, p: &std::path::Path) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = format!(
            "zh_variant={}\nproof_group={}\nstat_mask={}\nuser_name={}\nspotlight={}\nautosave={}\ngpu_backend={}\ngpu_backend_env={}\n",
            self.zh_variant,
            self.proof_group as u8,
            self.stat_mask,
            sanitize_ini_value(&self.user_name),
            self.spotlight as u8,
            self.autosave as u8,
            normalize_gpu_backend(&self.gpu_backend).unwrap_or(""),
            sanitize_ini_value(&self.gpu_backend_env),
        );
        let _ = std::fs::write(p, text);
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            self.save_to(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_roundtrip() {
        for v in [Variant::Generic, Variant::Taiwan, Variant::HongKong] {
            assert_eq!(variant_from_u8(variant_to_u8(v)), v);
        }
    }

    #[test]
    fn prefs_roundtrip_via_file() {
        // 走纯路径接口：不动 LOCALAPPDATA，避免和其它用到它的测试并行时打架
        let dir = crate::test_env::scratch("ui-prefs");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ui.ini");
        let p = UiPrefs {
            zh_variant: 2,
            proof_group: false,
            stat_mask: 0b10_1000,
            user_name: "张三".into(),
            spotlight: true,
            autosave: true,
            gpu_backend: "wgpu".into(),
            gpu_backend_env: "windows-x86_64|DEVBOX|console".into(),
        };
        p.save_to(&file);
        let got = UiPrefs::load_from(&file);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got.zh_variant, 2);
        assert!(!got.proof_group);
        assert_eq!(got.stat_mask, 0b10_1000);
        assert_eq!(got.user_name, "张三");
        assert!(got.spotlight);
        assert!(got.autosave);
        assert_eq!(got.gpu_backend, "wgpu");
        assert_eq!(got.gpu_backend_env, "windows-x86_64|DEVBOX|console");
    }

    #[test]
    fn gpu_backend_windows_default_prefers_wgpu_with_glow_fallback() {
        let win = plan_gpu_backends("auto", None, true, true, true);
        assert_eq!(win.candidates, vec!["wgpu", "glow"]);
        assert!(win.is_auto);
        assert!(!win.used_cache);

        let non_win = plan_gpu_backends("auto", None, true, true, false);
        assert_eq!(non_win.candidates, vec!["glow", "wgpu"]);
        assert!(non_win.is_auto);
        assert!(!non_win.used_cache);

        let glow_only = plan_gpu_backends("auto", Some("wgpu"), false, true, true);
        assert_eq!(glow_only.candidates, vec!["glow"]);
        assert!(!glow_only.used_cache);

        let wgpu_only = plan_gpu_backends("auto", Some("glow"), true, false, true);
        assert_eq!(wgpu_only.candidates, vec!["wgpu"]);
        assert!(!wgpu_only.used_cache);
    }

    #[test]
    fn gpu_backend_env_and_cli_overrides_take_precedence() {
        assert_eq!(resolve_backend_request(None, None, None), "auto");
        assert_eq!(resolve_backend_request(None, None, Some("glow")), "glow");
        assert_eq!(resolve_backend_request(None, Some("wgpu"), Some("glow")), "wgpu");
        assert_eq!(resolve_backend_request(Some("glow"), Some("wgpu"), Some("wgpu")), "glow");

        let env_glow = plan_gpu_backends("glow", Some("wgpu"), true, true, true);
        assert_eq!(env_glow.candidates, vec!["glow"]);
        assert!(!env_glow.is_auto);
        assert!(!env_glow.used_cache);

        let env_wgpu = plan_gpu_backends("wgpu", Some("glow"), true, true, true);
        assert_eq!(env_wgpu.candidates, vec!["wgpu"]);
        assert!(!env_wgpu.is_auto);
        assert!(!env_wgpu.used_cache);
    }

    #[test]
    fn gpu_backend_persistence_env_invalidation_and_fallback_reset() {
        let _env = crate::test_env::TestEnv::new("gpu-backend-persist");
        let fp_console = gpu_env_fingerprint_from("windows", "x86_64", "WORKSTATION", "Console");
        let fp_rdp_1 = gpu_env_fingerprint_from("windows", "x86_64", "WORKSTATION", "RDP-Tcp#1");
        let fp_rdp_42 = gpu_env_fingerprint_from("windows", "x86_64", "WORKSTATION", "RDP-Tcp#42");
        assert_eq!(fp_rdp_1, fp_rdp_42, "RDP 会话编号变化不应导致重复探测");
        assert_ne!(fp_console, fp_rdp_1, "本机桌面与 RDP 会话应区分缓存环境");

        // 保留其它用户偏好
        let mut init = UiPrefs::default();
        init.user_name = "李四".into();
        init.spotlight = true;
        init.save();

        persist_probed_gpu_backend("glow", &fp_console);
        let loaded = UiPrefs::load();
        assert_eq!(loaded.user_name, "李四");
        assert!(loaded.spotlight);
        assert_eq!(loaded.cached_gpu_backend(&fp_console), Some("glow"));
        assert_eq!(loaded.cached_gpu_backend(&fp_rdp_1), None);

        let hit_plan = plan_gpu_backends("auto", loaded.cached_gpu_backend(&fp_console), true, true, true);
        assert_eq!(hit_plan.candidates, vec!["glow", "wgpu"]);
        assert!(hit_plan.used_cache);

        let rdp_plan = plan_gpu_backends("auto", loaded.cached_gpu_backend(&fp_rdp_1), true, true, true);
        assert_eq!(rdp_plan.candidates, vec!["wgpu", "glow"]);
        assert!(!rdp_plan.used_cache);

        // 模拟缓存后端失效后的回退清除
        reset_cached_gpu_backend();
        let after_reset = UiPrefs::load();
        assert_eq!(after_reset.user_name, "李四");
        assert!(after_reset.spotlight);
        assert!(after_reset.gpu_backend.is_empty());
        assert_eq!(after_reset.cached_gpu_backend(&fp_console), None);
    }
}

// ---------------------------------------------------------------- 工作表视图

/// 一个保存下来的工作表视图（缩放 + 冻结 + 选区）
#[derive(Clone, Debug, PartialEq)]
pub struct SavedView {
    pub name: String,
    pub sheet: usize,
    pub zoom: f32,
    pub freeze: Option<(u32, u32)>,
    pub sel: (u32, u32),
}

fn views_path(key: &str) -> Option<std::path::PathBuf> {
    // 文件名用「路径的十六进制摘要」，避免中文/空格/超长路径出问题
    let mut h: u64 = 1469598103934665603;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    Some(
        crate::update::app_dir_opt()?
            .join("views")
            .join(format!("{h:016x}.ini")),
    )
}

impl SavedView {
    fn to_line(&self) -> String {
        let fz = match self.freeze {
            Some((r, c)) => format!("{r},{c}"),
            None => "-".into(),
        };
        format!(
            "{}|{}|{:.3}|{}|{},{}",
            self.name, self.sheet, self.zoom, fz, self.sel.0, self.sel.1
        )
    }
    fn from_line(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() != 5 {
            return None;
        }
        let freeze = if parts[3] == "-" {
            None
        } else {
            let (a, b) = parts[3].split_once(',')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        };
        let (sr, sc) = parts[4].split_once(',')?;
        Some(Self {
            name: parts[0].to_string(),
            sheet: parts[1].parse().ok()?,
            zoom: parts[2].parse().ok()?,
            freeze,
            sel: (sr.parse().ok()?, sc.parse().ok()?),
        })
    }
}

/// 读某个工作簿保存的视图
pub fn load_views(key: &str) -> Vec<SavedView> {
    let Some(p) = views_path(key) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(p) else {
        return Vec::new();
    };
    text.lines().filter_map(SavedView::from_line).collect()
}

/// 写某个工作簿的视图（空列表 = 删文件）
pub fn save_views(key: &str, views: &[SavedView]) {
    let Some(p) = views_path(key) else {
        return;
    };
    if views.is_empty() {
        let _ = std::fs::remove_file(p);
        return;
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = views.iter().map(|v| format!("{}\n", v.to_line())).collect();
    let _ = std::fs::write(p, text);
}

#[cfg(test)]
mod view_tests {
    use super::*;

    #[test]
    fn saved_view_line_roundtrip() {
        let v = SavedView {
            name: "看一季度".into(),
            sheet: 2,
            zoom: 1.25,
            freeze: Some((1, 2)),
            sel: (10, 3),
        };
        let line = v.to_line();
        assert_eq!(SavedView::from_line(&line).as_ref(), Some(&v));
        let none = SavedView {
            freeze: None,
            ..v.clone()
        };
        assert_eq!(SavedView::from_line(&none.to_line()), Some(none));
    }

    #[test]
    fn views_file_roundtrip() {
        let dir = crate::test_env::scratch("views");
        std::fs::create_dir_all(&dir).unwrap();
        // 直接测文件格式（不经过 LOCALAPPDATA）
        let file = dir.join("v.ini");
        let views = vec![
            SavedView {
                name: "A".into(),
                sheet: 0,
                zoom: 1.0,
                freeze: None,
                sel: (0, 0),
            },
            SavedView {
                name: "B".into(),
                sheet: 1,
                zoom: 1.5,
                freeze: Some((0, 1)),
                sel: (3, 4),
            },
        ];
        let text: String = views.iter().map(|v| format!("{}\n", v.to_line())).collect();
        std::fs::write(&file, &text).unwrap();
        let back: Vec<SavedView> = std::fs::read_to_string(&file)
            .unwrap()
            .lines()
            .filter_map(SavedView::from_line)
            .collect();
        assert_eq!(back, views);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
