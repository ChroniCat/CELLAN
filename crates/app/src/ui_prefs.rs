//! 界面偏好：写在 `%LOCALAPPDATA%\SheetsNative\ui.ini`。
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
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            zh_variant: 0,
            proof_group: true,
        }
    }
}

fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    Some(std::path::Path::new(&base).join("SheetsNative").join("ui.ini"))
}

impl UiPrefs {
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
            "zh_variant={}\nproof_group={}\n",
            self.zh_variant, self.proof_group as u8
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
        let dir = std::env::temp_dir().join(format!("sn_ui_prefs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ui.ini");
        let p = UiPrefs {
            zh_variant: 2,
            proof_group: false,
        };
        p.save_to(&file);
        let got = UiPrefs::load_from(&file);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got.zh_variant, 2);
        assert!(!got.proof_group);
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
    let base = std::env::var("LOCALAPPDATA").ok()?;
    // 文件名用「路径的十六进制摘要」，避免中文/空格/超长路径出问题
    let mut h: u64 = 1469598103934665603;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    Some(
        std::path::Path::new(&base)
            .join("SheetsNative")
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
        let dir = std::env::temp_dir().join(format!("sn_views_{}", std::process::id()));
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
