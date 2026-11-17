//! UI-22.1：命令使用频率。
//!
//! 存 `%LOCALAPPDATA%\SheetsNative\cmdusage.txt`，一行 `action<TAB>count`。
//! 命令搜索结果按「用得多的在前、同频按名字」排，重启后仍然有效。

use std::collections::HashMap;
use std::path::PathBuf;

fn store() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let d = PathBuf::from(base).join("SheetsNative");
    let _ = std::fs::create_dir_all(&d);
    d.join("cmdusage.txt")
}

/// 读取 action → 次数；坏行直接跳过，不让统计文件把命令面板弄坏。
pub fn load() -> HashMap<String, u32> {
    let Ok(text) = std::fs::read_to_string(store()) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some((action, n)) = line.rsplit_once('\t') else {
            continue;
        };
        if action.is_empty() {
            continue;
        }
        if let Ok(n) = n.trim().parse::<u32>() {
            out.insert(action.to_string(), n);
        }
    }
    out
}

fn save(map: &HashMap<String, u32>) {
    let mut rows: Vec<(&String, &u32)> = map.iter().filter(|(_, n)| **n > 0).collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut text = String::new();
    for (action, n) in rows {
        text.push_str(action);
        text.push('\t');
        text.push_str(&n.to_string());
        text.push('\n');
    }
    let _ = std::fs::write(store(), text);
}

/// 记一次执行；文件写失败不影响命令本身。
pub fn touch(action: &str) {
    if action.trim().is_empty() {
        return;
    }
    let mut map = load();
    *map.entry(action.to_string()).or_insert(0) += 1;
    save(&map);
}

/// 按使用次数降序、同频按名字排；没记录的排后面。
pub fn rank<T: AsRef<str>>(actions: &mut [T], usage: &HashMap<String, u32>) {
    actions.sort_by(|a, b| {
        let ca = usage.get(a.as_ref()).copied().unwrap_or(0);
        let cb = usage.get(b.as_ref()).copied().unwrap_or(0);
        cb.cmp(&ca).then_with(|| a.as_ref().cmp(b.as_ref()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_survive_reload_and_rank_desc() {
        let _env = crate::test_env::TestEnv::new("ui221-cmdusage");
        touch("Bold");
        touch("Bold");
        touch("CenterAlign");
        let m = load();
        assert_eq!(m.get("Bold"), Some(&2));
        assert_eq!(m.get("CenterAlign"), Some(&1));
        let mut acts = vec!["FreezePanes", "CenterAlign", "Bold"];
        rank(&mut acts, &m);
        assert_eq!(acts, vec!["Bold", "CenterAlign", "FreezePanes"]);
    }

    #[test]
    fn touch_is_isolated_by_localappdata() {
        let _env = crate::test_env::TestEnv::new("ui221-iso");
        touch("Paste");
        assert_eq!(load().get("Paste"), Some(&1));
        // 换一个隔离目录后读不到上一个目录的计数
        drop(_env);
        let _env2 = crate::test_env::TestEnv::new("ui221-iso2");
        assert!(load().get("Paste").is_none());
    }
}
