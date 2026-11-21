//! UI-14.1 系统字体加载与回退。
//!
//! 只按需加载工作簿里真正出现的字体名：优先在 Windows Fonts 目录找到同名/别名文件，
//! 用 ttf-parser 在 TTC 里按 family 名选对 face；找不到就回落到内嵌的四族。
//! 不做全目录扫描，避免启动时读几百 MB 字体。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use eframe::egui::FontData;

pub fn family_key(name: &str) -> String {
    format!("sys:{}", norm(name))
}

pub fn norm(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Excel 常见字体 → Windows 字体文件。注册表名的代码页不可靠，这里用确定映射；
/// 其余字体用英文名去 Fonts 目录找同名 .ttf/.ttc。
fn file_candidates(name: &str) -> Vec<&'static str> {
    match norm(name).as_str() {
        "宋体" | "simsun" | "新宋体" | "nsimsun" => vec!["simsun.ttc"],
        "微软雅黑" | "microsoftyahei" | "microsoftyaheiui" => vec!["msyh.ttc", "msyhbd.ttc"],
        "黑体" | "simhei" => vec!["simhei.ttf"],
        "楷体" | "楷体_gb2312" | "kaiti" => vec!["simkai.ttf"],
        "仿宋" | "fangsong" => vec!["simfang.ttf"],
        "等线" | "dengxian" => vec!["Deng.ttf"],
        "等线light" | "dengxianlight" => vec!["Dengl.ttf"],
        "arial" => vec!["arial.ttf"],
        "calibri" => vec!["calibri.ttf"],
        "timesnewroman" | "times" => vec!["times.ttf"],
        "cambria" => vec!["cambria.ttc"],
        "consolas" => vec!["consola.ttf"],
        "segoeui" => vec!["segoeui.ttf"],
        "verdana" => vec!["verdana.ttf"],
        "tahoma" => vec!["tahoma.ttf"],
        "georgia" => vec!["georgia.ttf"],
        "couriernew" => vec!["cour.ttf"],
        _ => vec![],
    }
}

/// 真实粗体文件；没有粗体字面的字体（宋体/黑体）返回空，由调用方决定是否合成。
fn bold_file_candidates(name: &str) -> Vec<&'static str> {
    match norm(name).as_str() {
        "微软雅黑" | "microsoftyahei" | "microsoftyaheiui" => vec!["msyhbd.ttc"],
        "等线" | "dengxian" => vec!["Dengb.ttf"],
        "arial" => vec!["arialbd.ttf"],
        "calibri" => vec!["calibrib.ttf"],
        "timesnewroman" | "times" => vec!["timesbd.ttf"],
        "cambria" => vec!["cambriab.ttf"],
        "consolas" => vec!["consolab.ttf"],
        "segoeui" => vec!["segoeuib.ttf"],
        "verdana" => vec!["verdanab.ttf"],
        "tahoma" => vec!["tahomabd.ttf"],
        "georgia" => vec!["georgiab.ttf"],
        "couriernew" => vec!["courbd.ttf"],
        _ => vec![],
    }
}

fn fonts_dir() -> std::path::PathBuf {
    let win = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    std::path::PathBuf::from(win).join("Fonts")
}

/// 在 TTC/TTF 里按 family 名找 face；找不到返回 0 号 face。
fn face_index(data: &[u8], want: &str) -> Option<u32> {
    let n = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    let want = norm(want);
    for i in 0..n {
        let Ok(face) = ttf_parser::Face::parse(data, i) else { continue };
        for name in face.names() {
            if !name.is_unicode() {
                continue;
            }
            if let Some(s) = name.to_string() {
                let s = norm(&s);
                // 注册名常带 “Regular/Bold”，family 名本身是前缀
                if s == want || s.starts_with(&want) {
                    return Some(i);
                }
            }
        }
    }
    if n > 0 { Some(0) } else { None }
}

// epaint retains borrowed bytes for the lifetime of its font faces. Cache by
// filename so aliases, TTC faces and the UI fallback all share one allocation.
pub(super) fn file_bytes(file: &'static str) -> Option<&'static [u8]> {
    static FILES: OnceLock<Mutex<HashMap<&'static str, &'static [u8]>>> = OnceLock::new();
    let mut files = FILES.get_or_init(Default::default).lock().ok()?;
    if let Some(&bytes) = files.get(file) {
        return Some(bytes);
    }
    let bytes = std::fs::read(fonts_dir().join(file)).ok()?;
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    files.insert(file, bytes);
    Some(bytes)
}

pub fn load(name: &str) -> Option<(&'static [u8], u32)> {
    load_from(&file_candidates(name), name)
}

pub fn load_bold(name: &str) -> Option<(&'static [u8], u32)> {
    load_from(&bold_file_candidates(name), name)
}

fn load_from(files: &[&'static str], name: &str) -> Option<(&'static [u8], u32)> {
    for &file in files {
        if let Some(data) = file_bytes(file) {
            let idx = face_index(data, name).unwrap_or(0);
            return Some((data, idx));
        }
    }
    None
}

struct Loaded {
    data: &'static [u8],
    index: u32,
}

static LOADED: OnceLock<Mutex<HashMap<String, Loaded>>> = OnceLock::new();
/// 已写入 FontDefinitions 但 egui 要到下一帧才生效的字体。
static BOLD_LOADED: OnceLock<Mutex<HashMap<String, Loaded>>> = OnceLock::new();
static PENDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
/// 当前帧真正可用的系统字体（egui 已装载）。
static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn loaded() -> &'static Mutex<HashMap<String, Loaded>> {
    LOADED.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn is_bold_loaded(name: &str) -> bool {
    ACTIVE
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|m| m.contains(&format!("bold:{}", norm(name))))
        .unwrap_or(false)
}

pub fn bold_family_key(name: &str) -> String {
    format!("sysbold:{}", norm(name))
}

pub fn is_loaded(name: &str) -> bool {
    ACTIVE
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|m| m.contains(&norm(name)))
        .unwrap_or(false)
}

/// 每帧开头调用：上一帧 `set_fonts` 的字体在这一帧真正可用。
fn pending() -> &'static Mutex<HashSet<String>> {
    PENDING.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn begin_frame() {
    let active = ACTIVE.get_or_init(|| Mutex::new(HashSet::new()));
    let pending = PENDING.get_or_init(|| Mutex::new(HashSet::new()));
    let (Ok(mut a), Ok(mut p)) = (active.lock(), pending.lock()) else { return };
    for k in p.drain() {
        a.insert(k);
    }
}

/// 返回是否新增了字体（调用方需要重设 FontDefinitions）。
pub fn ensure(names: &[String]) -> bool {
    if std::env::var("SC_FONT_LOG").is_ok() {
        eprintln!("[fonts] ensure enter {} names", names.len());
    }
    let mut map = match loaded().lock() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mut changed = false;
    for n in names {
        let key = norm(n);
        if key.is_empty() || map.contains_key(&key) {
            continue;
        }
        if let Some((data, index)) = load(n) {
            if std::env::var("SC_FONT_LOG").is_ok() {
                eprintln!("[fonts] insert {} face {}", n, index);
            }
            map.insert(key, Loaded { data, index });
            changed = true;
        }
        if let Some((data, index)) = load_bold(n) {
            if let Ok(mut b) = BOLD_LOADED.get_or_init(|| Mutex::new(HashMap::new())).lock() {
                if !b.contains_key(&norm(n)) {
                    b.insert(norm(n), Loaded { data, index });
                    changed = true;
                    pending().lock().map(|mut p| p.insert(format!("bold:{}", norm(n))));
                }
            }
        }
    }
    if std::env::var("SC_FONT_LOG").is_ok() {
        eprintln!("[fonts] ensure changed={} size={}", changed, map.len());
    }
    changed
}

pub fn apply(fonts: &mut egui::FontDefinitions) {
    let map = match loaded().lock() {
        Ok(m) => m,
        Err(e) => {
            if std::env::var("SC_FONT_LOG").is_ok() {
                eprintln!("[fonts] lock poisoned: {e}");
            }
            return;
        }
    };
    for (name, f) in map.iter() {
        let key = format!("sys:{name}");
        fonts.font_data.insert(
            key.clone(),
            FontData { font: std::borrow::Cow::Borrowed(f.data), index: f.index, tweak: Default::default() }.into(),
        );
        if std::env::var("SC_FONT_LOG").is_ok() {
            eprintln!("[fonts] apply {:?} index={}", key, f.index);
        }
        let mut chain = vec![key.clone()];
        for fb in ["sys-ui-msyh", "noto-sans"] {
            if fonts.font_data.contains_key(fb) && !chain.iter().any(|x| x == fb) {
                chain.push(fb.to_owned());
            }
        }
        fonts
            .families
            .insert(egui::FontFamily::Name(key.clone().into()), chain);
        let _ = pending().lock().map(|mut p| p.insert(name.clone()));
    }
    if let Ok(bold) = BOLD_LOADED.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        for (name, f) in bold.iter() {
            let key = format!("sysbold:{name}");
            fonts.font_data.insert(
                key.clone(),
                FontData { font: std::borrow::Cow::Borrowed(f.data), index: f.index, tweak: Default::default() }.into(),
            );
            let mut chain = vec![key.clone()];
            for fb in ["sys-ui-msyh-b", "noto-sans-bold", "sys-ui-msyh", "noto-sans"] {
                if fonts.font_data.contains_key(fb) && !chain.iter().any(|x| x == fb) {
                    chain.push(fb.to_owned());
                }
            }
            fonts
                .families
                .insert(egui::FontFamily::Name(key.clone().into()), chain);
            let _ = pending().lock().map(|mut p| p.insert(format!("bold:{name}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_common_chinese_and_latin_fonts() {
        let (data, idx) = load("宋体").expect("本机应有 simsun.ttc");
        assert!(!data.is_empty());
        assert_eq!(face_index(&data, "宋体"), Some(idx));
        let (data, idx) = load("微软雅黑").expect("本机应有 msyh.ttc");
        assert_eq!(face_index(&data, "微软雅黑"), Some(idx));
        let (data, idx) = load("Arial").expect("本机应有 arial.ttf");
        assert_eq!(face_index(&data, "Arial"), Some(idx));
    }

    #[test]
    fn loads_real_bold_face_when_present() {
        let (data, idx) = load_bold("微软雅黑").expect("本机应有 msyhbd.ttc");
        assert_eq!(face_index(&data, "微软雅黑"), Some(idx));
        assert!(load_bold("宋体").is_none(), "宋体没有粗体字面");
    }

    #[test]
    fn unknown_font_falls_back() {
        assert!(load("绝对不存在的字体 XYZ").is_none());
        assert!(!is_loaded("绝对不存在的字体 XYZ"));
    }

    #[test]
    fn ensure_is_idempotent() {
        assert!(ensure(&["宋体".to_string()]));
        let mut d = egui::FontDefinitions::default();
        apply(&mut d);
        begin_frame();
        assert!(is_loaded("宋体"));
        assert!(!ensure(&["宋体".to_string()]));
    }

    #[test]
    fn latin_system_font_chains_cjk_fallback() {
        let _ = ensure(&["Arial".to_string()]);
        let mut d = egui::FontDefinitions::default();
        d.font_data
            .insert("noto-sans".to_owned(), egui::FontData::from_static(&[]).into());
        apply(&mut d);
        let fam = d
            .families
            .get(&egui::FontFamily::Name("sys:arial".into()))
            .expect("sys:arial 必须存在");
        assert_eq!(fam.first().map(String::as_str), Some("sys:arial"));
        assert!(
            fam.iter().any(|k| k == "noto-sans"),
            "西文系统字体族必须串接中文回退字库以防保存重开后汉字变成问号，实际: {fam:?}"
        );
    }
}
