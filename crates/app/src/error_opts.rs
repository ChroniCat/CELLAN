//! 错误检查选项的持久化：写在 `%LOCALAPPDATA%\SheetsNative\errorcheck.ini`。
//!
//! 和 Excel 一样是「应用级」设置（不是跟文件走）：背景检查开关、标记颜色、规则开关。

#[derive(Clone, Debug, PartialEq)]
pub struct ErrorCheckOptions {
    /// 背景错误检查（绿三角）
    pub background: bool,
    /// 检查公式错误（#DIV/0! 这类）
    pub check_errors: bool,
    /// 检查「数字以文本形式存储」
    pub check_text: bool,
    /// 标记颜色
    pub color: [u8; 3],
}

impl Default for ErrorCheckOptions {
    fn default() -> Self {
        Self {
            background: true,
            check_errors: true,
            check_text: true,
            color: [40, 160, 40],
        }
    }
}

fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    Some(std::path::Path::new(&base).join("SheetsNative").join("errorcheck.ini"))
}

impl ErrorCheckOptions {
    pub fn load() -> Self {
        let mut me = Self::default();
        let Some(p) = path() else { return me };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return me;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "background" => me.background = v == "1",
                "check_errors" => me.check_errors = v == "1",
                "check_text" => me.check_text = v == "1",
                "color" => {
                    let parts: Vec<u8> = v
                        .split(',')
                        .filter_map(|x| x.trim().parse::<u8>().ok())
                        .collect();
                    if parts.len() == 3 {
                        me.color = [parts[0], parts[1], parts[2]];
                    }
                }
                _ => {}
            }
        }
        me
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = format!(
            "background={}\ncheck_errors={}\ncheck_text={}\ncolor={},{},{}\n",
            self.background as u8,
            self.check_errors as u8,
            self.check_text as u8,
            self.color[0],
            self.color[1],
            self.color[2]
        );
        let _ = std::fs::write(&p, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    #[test]
    fn roundtrip_through_file() {
        // QA-03.1：环境目录和恢复交给 TestEnv，不再手写 set_var + 手工还原。
        let env = TestEnv::new("error-opts");
        let mut opts = ErrorCheckOptions::default();
        opts.background = false;
        opts.check_text = false;
        opts.color = [200, 0, 0];
        opts.save();
        let back = ErrorCheckOptions::load();
        assert!(!back.background);
        assert!(back.check_errors);
        assert!(!back.check_text);
        assert_eq!(back.color, [200, 0, 0]);
        let file = env.profile().join("SheetsNative").join("errorcheck.ini");
        assert!(file.is_file(), "选项必须写在隔离 profile 下：{}", file.display());
    }
}
