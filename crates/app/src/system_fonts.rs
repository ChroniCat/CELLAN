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

fn fonts_dir() -> &'static std::path::Path {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let win = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        std::path::PathBuf::from(win).join("Fonts")
    })
    .as_path()
}

/// Windows 只读零拷贝映射系统字体（`CreateFileW` + `CreateFileMappingW` + `MapViewOfFile`），
/// 避免把 39.5 MB 的 `msyh.ttc`/`msyhbd.ttc` 全量读进进程堆内存；映射视图在关闭文件与映射句柄后仍由内核保持有效。
#[cfg(windows)]
fn map_file_readonly(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::os::windows::ffi::OsStrExt;
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_ALL: u32 = 0x0000_0001 | 0x0000_0002 | 0x0000_0004;
    const OPEN_EXISTING: u32 = 3;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    const PAGE_READONLY: u32 = 0x02;
    const FILE_MAP_READ: u32 = 0x0004;
    const INVALID_HANDLE_VALUE: *mut core::ffi::c_void = (-1isize) as *mut core::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *const core::ffi::c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
        fn GetFileSizeEx(h_file: *mut core::ffi::c_void, lp_file_size: *mut i64) -> i32;
        fn CreateFileMappingW(
            h_file: *mut core::ffi::c_void,
            lp_file_mapping_attributes: *const core::ffi::c_void,
            fl_protect: u32,
            dw_maximum_size_high: u32,
            dw_maximum_size_low: u32,
            lp_name: *const u16,
        ) -> *mut core::ffi::c_void;
        fn MapViewOfFile(
            h_file_mapping_object: *mut core::ffi::c_void,
            dw_desired_access: u32,
            dw_file_offset_high: u32,
            dw_file_offset_low: u32,
            dw_number_of_bytes_to_map: usize,
        ) -> *const u8;
        fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let hfile = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_ALL,
            core::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            core::ptr::null_mut(),
        );
        if hfile.is_null() || hfile == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut size: i64 = 0;
        if GetFileSizeEx(hfile, &mut size) == 0 || size <= 0 || (size as u64) > (isize::MAX as u64)
        {
            CloseHandle(hfile);
            return None;
        }
        let hmap = CreateFileMappingW(hfile, core::ptr::null(), PAGE_READONLY, 0, 0, core::ptr::null());
        if hmap.is_null() {
            CloseHandle(hfile);
            return None;
        }
        let ptr = MapViewOfFile(hmap, FILE_MAP_READ, 0, 0, size as usize);
        // 内核对已映射视图保持引用计数，关闭句柄不影响只读视图生命周期。
        CloseHandle(hmap);
        CloseHandle(hfile);
        if ptr.is_null() {
            return None;
        }
        Some(core::slice::from_raw_parts(ptr, size as usize))
    }
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
// filename so aliases, TTC faces and the UI fallback all share one mapping/allocation.
pub(super) fn file_bytes(file: &'static str) -> Option<&'static [u8]> {
    static FILES: OnceLock<Mutex<HashMap<&'static str, &'static [u8]>>> = OnceLock::new();
    let table = FILES.get_or_init(Default::default);
    if let Ok(files) = table.lock() {
        if let Some(&bytes) = files.get(file) {
            return Some(bytes);
        }
    }
    let path = fonts_dir().join(file);
    #[cfg(windows)]
    let mapped = map_file_readonly(&path);
    #[cfg(not(windows))]
    let mapped: Option<&'static [u8]> = None;

    let bytes: &'static [u8] = match mapped {
        Some(s) => s,
        None => {
            let raw = std::fs::read(&path).ok()?;
            Box::leak(raw.into_boxed_slice())
        }
    };
    if let Ok(mut files) = table.lock() {
        if let Some(&existing) = files.get(file) {
            return Some(existing);
        }
        files.insert(file, bytes);
    }
    Some(bytes)
}

/// Windows 原生中文字体（`msyh.ttc` / `msyhbd.ttc`）只读映射与 face 校验结果，
/// 供首帧后由后台线程异步准备，避免冷启动阻塞 39.5 MB CJK 字体加载与解析。
pub(crate) struct UiCjkFonts {
    pub msyh_ui: Option<std::sync::Arc<FontData>>,
    pub msyh_ui_b: Option<std::sync::Arc<FontData>>,
}

static UI_CJK_FONTS: OnceLock<UiCjkFonts> = OnceLock::new();
static UI_CJK_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn load_ttf_face(file: &'static str, idx: u32) -> Option<std::sync::Arc<FontData>> {
    let bytes = file_bytes(file)?;
    let n = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    let index = if idx < n { idx } else { 0 };
    ttf_parser::Face::parse(bytes, index).ok()?;
    Some(std::sync::Arc::new(FontData {
        font: std::borrow::Cow::Borrowed(bytes),
        index,
        tweak: Default::default(),
    }))
}

pub(crate) fn ui_cjk_cached() -> Option<&'static UiCjkFonts> {
    UI_CJK_FONTS.get()
}

pub(crate) fn load_ui_cjk_sync() -> &'static UiCjkFonts {
    UI_CJK_FONTS.get_or_init(|| UiCjkFonts {
        // msyh.ttc / msyhbd.ttc 的 1 号 face 是专门优化过 UI 行高与字面的 Microsoft YaHei UI / Bold
        msyh_ui: load_ttf_face("msyh.ttc", 1),
        msyh_ui_b: load_ttf_face("msyhbd.ttc", 1),
    })
}

pub(crate) fn request_ui_cjk_async(ctx: &egui::Context) {
    if UI_CJK_FONTS.get().is_some() {
        return;
    }
    if UI_CJK_STARTED
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }
    let wake = ctx.clone();
    std::thread::spawn(move || {
        let loaded = load_ui_cjk_sync();
        if loaded.msyh_ui.is_some() || loaded.msyh_ui_b.is_some() {
            READY_TO_INSTALL.store(true, std::sync::atomic::Ordering::Release);
            wake.request_repaint();
        }
    });
}

pub(crate) fn bump_generation() {
    ACTIVE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
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

static ACTIVE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static READY_TO_INSTALL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TRIED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static ASYNC_LOADER: OnceLock<std::sync::mpsc::Sender<(Vec<String>, egui::Context)>> = OnceLock::new();

pub fn generation() -> u64 { ACTIVE_GENERATION.load(std::sync::atomic::Ordering::Acquire) }

/// 后台异步加载工作簿引用的系统字体（避免在 UI 主线程同步读盘 20MB+ `.ttc`/`.ttf`）。
/// 已尝试过的字体（含不存在的系统字体）记入 `TRIED`，避免每 30 帧重复触发文件 I/O。
pub fn request_async(names: &[String], ctx: &egui::Context) {
    let mut to_load = Vec::new();
    {
        let Ok(mut tried) = TRIED.get_or_init(|| Mutex::new(HashSet::new())).lock() else {
            return;
        };
        let already = loaded().lock().ok();
        for n in names {
            let key = norm(n);
            if key.is_empty() {
                continue;
            }
            if already.as_ref().is_some_and(|m| m.contains_key(&key)) {
                tried.insert(key);
                continue;
            }
            if tried.insert(key) {
                to_load.push(n.clone());
            }
        }
    }
    if to_load.is_empty() {
        return;
    }
    let tx = ASYNC_LOADER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<(Vec<String>, egui::Context)>();
        std::thread::spawn(move || {
            while let Ok((batch, wake)) = rx.recv() {
                if ensure(&batch) {
                    READY_TO_INSTALL.store(true, std::sync::atomic::Ordering::Release);
                    wake.request_repaint();
                }
            }
        });
        tx
    });
    let _ = tx.send((to_load, ctx.clone()));
}

/// 检查后台线程是否已完成新字体字节加载并等待挂载到 `egui::Context`。
pub fn take_ready() -> bool {
    READY_TO_INSTALL.swap(false, std::sync::atomic::Ordering::AcqRel)
}

pub fn begin_frame() {
    let active = ACTIVE.get_or_init(|| Mutex::new(HashSet::new()));
    let pending = PENDING.get_or_init(|| Mutex::new(HashSet::new()));
    let (Ok(mut a), Ok(mut p)) = (active.lock(), pending.lock()) else { return };
    let mut changed = false;
    for k in p.drain() { changed |= a.insert(k); }
    if changed { ACTIVE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release); }
}

/// 返回是否新增了字体（调用方需要重设 FontDefinitions）。
pub fn ensure(names: &[String]) -> bool {
    let log = crate::debug_flags::font_log();
    if log {
        eprintln!("[fonts] ensure enter {} names", names.len());
    }
    // 先在锁外完成磁盘读取与 TTC face 解析，缩短全局字体表持锁时间
    let mut newly_loaded: Vec<(String, Loaded, Option<Loaded>)> = Vec::new();
    for n in names {
        let key = norm(n);
        if key.is_empty() {
            continue;
        }
        let already = loaded()
            .lock()
            .ok()
            .is_some_and(|m| m.contains_key(&key));
        if already {
            if let Ok(mut t) = TRIED.get_or_init(|| Mutex::new(HashSet::new())).lock() {
                t.insert(key);
            }
            continue;
        }
        if let Ok(mut t) = TRIED.get_or_init(|| Mutex::new(HashSet::new())).lock() {
            t.insert(key.clone());
        }
        if let Some((data, index)) = load(n) {
            if log {
                eprintln!("[fonts] insert {} face {}", n, index);
            }
            let bold = load_bold(n).map(|(bd, bi)| Loaded { data: bd, index: bi });
            newly_loaded.push((key, Loaded { data, index }, bold));
        }
    }
    if newly_loaded.is_empty() {
        return false;
    }
    let mut map = match loaded().lock() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mut changed = false;
    for (key, reg, bold) in newly_loaded {
        if !map.contains_key(&key) {
            map.insert(key.clone(), reg);
            changed = true;
        }
        if let Some(bold_face) = bold {
            if let Ok(mut b) = BOLD_LOADED.get_or_init(|| Mutex::new(HashMap::new())).lock() {
                if !b.contains_key(&key) {
                    b.insert(key.clone(), bold_face);
                    changed = true;
                    if let Ok(mut fonts) = pending().lock() {
                        fonts.insert(format!("bold:{key}"));
                    }
                }
            }
        }
    }
    if log {
        eprintln!("[fonts] ensure changed={} size={}", changed, map.len());
    }
    changed
}

pub fn apply(fonts: &mut egui::FontDefinitions) {
    let log = crate::debug_flags::font_log();
    let map = match loaded().lock() {
        Ok(m) => m,
        Err(e) => {
            if log {
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
        if log {
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
