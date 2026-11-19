//! UI-11.4：函数「最近使用」记录。
//!
//! 存 `%LOCALAPPDATA%\CELLAN\fnrecent.txt`，一行一个函数名，新的在上面。
//! 只有真的在公式里用过的函数才进榜（Excel「最近使用的函数」同款语义），
//! XML 里那五项只在还没有任何记录时兜底，免得菜单一开始是空的。

use std::path::PathBuf;

pub const MAX: usize = 10;

/// 还没有任何使用记录时的兜底菜单（与功能区 XML 原来的五项一致）。
pub const DEFAULTS: [&str; 5] = ["SUM", "IF", "VLOOKUP", "COUNTIF", "SUMIF"];

fn store() -> PathBuf {
    crate::update::app_dir().join("fnrecent.txt")
}

pub fn load() -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(store()) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_ascii_uppercase())
        .take(MAX)
        .collect()
}

/// 菜单项：有记录用记录，没记录用兜底五项。
pub fn menu_items() -> Vec<String> {
    let list = load();
    if list.is_empty() {
        DEFAULTS.iter().map(|s| s.to_string()).collect()
    } else {
        list
    }
}

/// 把一个函数提到最前（公式提交时调用）。
pub fn record(name: &str) {
    let name = name.trim().to_ascii_uppercase();
    if name.is_empty() {
        return;
    }
    let mut list = load();
    list.retain(|n| n != &name);
    list.insert(0, name);
    list.truncate(MAX);
    let body: String = list.iter().map(|n| format!("{n}\n")).collect();
    let _ = std::fs::write(store(), body);
}

/// 从一段公式里抽出所有「函数调用」名（`NAME(`，允许名字和括号之间有空格），
/// 只认引擎真实现了的函数，按出现顺序去重。
pub fn formula_functions(text: &str) -> Vec<String> {
    let known: std::collections::HashSet<&'static str> =
        sheetcore::formula::functions::implemented().into_iter().collect();
    let b: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let boundary = i == 0
            || !(b[i - 1].is_alphanumeric() || matches!(b[i - 1], '_' | '.' | '$'));
        if !boundary || !(b[i].is_ascii_alphabetic() || b[i] == '_') {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], '_' | '.')) {
            i += 1;
        }
        let mut j = i;
        while j < b.len() && b[j] == ' ' {
            j += 1;
        }
        if j < b.len() && b[j] == '(' {
            let name = b[s..i].iter().collect::<String>().to_ascii_uppercase();
            if known.contains(name.as_str()) && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

pub fn record_formula(text: &str) {
    for n in formula_functions(text) {
        record(&n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    #[test]
    fn record_moves_to_front_and_dedupes() {
        let _env = TestEnv::new("fnrecent");
        record("SUM");
        record("IF");
        record("SUM");
        let list = load();
        assert_eq!(list, vec!["SUM".to_string(), "IF".to_string()]);
    }

    #[test]
    fn store_lives_under_isolated_profile() {
        let env = TestEnv::new("fnrecent-store");
        record("AVERAGE");
        let store = env.profile().join("CELLAN").join("fnrecent.txt");
        assert!(store.is_file(), "fnrecent.txt 必须落在本次运行目录");
        assert!(std::fs::read_to_string(&store).unwrap().contains("AVERAGE"));
    }

    #[test]
    fn formula_scan_only_keeps_real_calls_in_order() {
        let got = formula_functions("=sum(A1:A3)+IF(B1>0, VLOOKUP(C1,D:E,2,0), 0)");
        assert_eq!(got, vec!["SUM", "IF", "VLOOKUP"], "按出现顺序、去重、只认已实现函数");
        // 名字不是函数（后面没有括号）、未实现的函数名都不收
        let got = formula_functions("=NOTAFUNCTIONX(A1)+sum (B1)+MYMACRO(1)");
        assert_eq!(got, vec!["SUM"], "空格允许，引擎没实现的宏名不收");
    }

    #[test]
    fn menu_falls_back_to_defaults_until_something_is_recorded() {
        let _env = TestEnv::new("fnrecent-menu");
        assert_eq!(menu_items(), DEFAULTS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        record("XLOOKUP");
        assert_eq!(menu_items().first().map(|s| s.as_str()), Some("XLOOKUP"));
    }
}
