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
