//! 出版级矢量 PDF 1.7 生成器：
//! - TrueType 单体字体 + **纯 Rust `.ttc` TrueType Collection 无损拆包器**（直接支持 Windows 微软雅黑 / 宋体 / Cambria 等系统字体集合）；
//! - **多字体 Unicode 回退链**（`/F1` 常规、`/F2` 真粗体、`/F3` 系统 CJK 常规回退、`/F4` 系统 CJK 粗体回退）：
//!   生僻汉字、繁日韩、财务与统计符号自动无缝路由到系统全字库并按实有字形精准裁剪，彻底消除豆腐块；
//! - 完整矢量路径原语：裁剪区（`W n`）、13 种 Excel 边框虚实线型（`d`）、半透明填充与水印（`ExtGState`）、
//!   描边/填充多边形、斜体/旋转文本、PDF 文档元信息（`/Info`）与多工作表侧边栏书签目录（`/Outlines`）。

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

enum FontBytes {
    Owned(Vec<u8>),
    Static(&'static [u8]),
}

impl std::ops::Deref for FontBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v.as_slice(),
            Self::Static(s) => s,
        }
    }
}

struct FontInner {
    data: FontBytes,
    cmap: HashMap<char, u16>,
    widths: Vec<u16>,
    ascender: i16,
    descender: i16,
    bbox: [i16; 4],
}

/// 嵌入的字体（内部 `Arc` 共享，克隆为 O(1)）
#[derive(Clone)]
pub struct Font {
    inner: Arc<FontInner>,
    pub units_per_em: u16,
}

fn be16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}
fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn bei16(b: &[u8], off: usize) -> i16 {
    i16::from_be_bytes([b[off], b[off + 1]])
}

static STATIC_FONT_CACHE: OnceLock<Mutex<HashMap<(usize, usize), Font>>> = OnceLock::new();

/// 纯 Rust TrueType Collection (`.ttc`) 无损拆包：把第 `face_index` 号字体重新打包成合法单体 TTF，
/// 供 PDF 嵌入与 `font_subset::subset` 裁剪复用。
pub fn extract_ttc_face(data: &[u8], face_index: u32) -> Result<Vec<u8>> {
    anyhow::ensure!(data.len() >= 16 && &data[0..4] == b"ttcf", "不是 TTC 字体集合");
    let num_fonts = be32(data, 8);
    anyhow::ensure!(num_fonts > 0, "TTC 集合为空");
    let idx = face_index.min(num_fonts - 1) as usize;
    let offset_pos = 12 + idx * 4;
    anyhow::ensure!(offset_pos + 4 <= data.len(), "TTC 偏移表越界");
    let font_off = be32(data, offset_pos) as usize;
    anyhow::ensure!(font_off + 12 <= data.len(), "TTC 子字体头越界");
    let sfnt_tag = &data[font_off..font_off + 4];
    anyhow::ensure!(
        sfnt_tag == b"\x00\x01\x00\x00" || sfnt_tag == b"true",
        "TTC 第 {idx} 号子字体不是 TrueType 轮廓（不支持 CFF/OTTO）"
    );
    let n_tables = be16(data, font_off + 4) as usize;
    anyhow::ensure!(font_off + 12 + n_tables * 16 <= data.len(), "TTC 表目录越界");

    let mut recs: Vec<([u8; 4], u32, usize, usize)> = Vec::with_capacity(n_tables);
    let mut total_body = 0usize;
    for i in 0..n_tables {
        let p = font_off + 12 + i * 16;
        let tag = [data[p], data[p + 1], data[p + 2], data[p + 3]];
        let csum = be32(data, p + 4);
        let toff = be32(data, p + 8) as usize;
        let tlen = be32(data, p + 12) as usize;
        anyhow::ensure!(
            toff.checked_add(tlen).is_some_and(|e| e <= data.len()),
            "TTC 子表 {:?} 越界",
            std::str::from_utf8(&tag).unwrap_or("????")
        );
        recs.push((tag, csum, toff, tlen));
        total_body += (tlen + 3) & !3;
    }

    let count = recs.len() as u16;
    let mut pow2 = 1u16;
    let mut log2 = 0u16;
    while pow2 * 2 <= count {
        pow2 *= 2;
        log2 += 1;
    }
    let search_range = pow2 * 16;
    let range_shift = count * 16 - search_range;

    let dir_end = 12 + recs.len() * 16;
    let mut out = Vec::with_capacity(dir_end + total_body);
    out.extend_from_slice(sfnt_tag);
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&log2.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    let mut cur_off = dir_end;
    for &(tag, csum, _, tlen) in &recs {
        out.extend_from_slice(&tag);
        out.extend_from_slice(&csum.to_be_bytes());
        out.extend_from_slice(&(cur_off as u32).to_be_bytes());
        out.extend_from_slice(&(tlen as u32).to_be_bytes());
        cur_off += (tlen + 3) & !3;
    }
    for &(_, _, toff, tlen) in &recs {
        out.extend_from_slice(&data[toff..toff + tlen]);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    Ok(out)
}

impl Font {
    /// 从 TTF 或 TTC 文件加载（若是 `.ttc` 集合自动拆出第 0 号字面）
    pub fn load(path: &Path) -> Result<Font> {
        Self::load_face(path, 0)
    }

    /// 从 TTF 或 TTC 文件加载指定索引的子字面
    pub fn load_face(path: &Path, face_index: u32) -> Result<Font> {
        let data = std::fs::read(path).with_context(|| format!("读字体失败 {}", path.display()))?;
        Self::parse_collection(data, face_index)
    }

    /// 从静态内嵌 TTF 字节零拷贝解析并全局缓存，重复导出 PDF / 打印时 0 分配复用。
    pub fn parse_static(data: &'static [u8]) -> Result<Font> {
        let key = (data.as_ptr() as usize, data.len());
        let cache = STATIC_FONT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = cache.lock() {
            if let Some(cached) = guard.get(&key) {
                return Ok(cached.clone());
            }
        }
        let parsed = if data.len() >= 4 && &data[0..4] == b"ttcf" {
            let unpacked = extract_ttc_face(data, 0)?;
            Self::parse_bytes(FontBytes::Owned(unpacked))?
        } else {
            Self::parse_bytes(FontBytes::Static(data))?
        };
        if let Ok(mut guard) = cache.lock() {
            guard.insert(key, parsed.clone());
        }
        Ok(parsed)
    }

    /// 从内存里的 TTF/TTC 字节解析（若是 TTC 自动拆出第 0 号字面）
    pub fn parse(data: Vec<u8>) -> Result<Font> {
        Self::parse_collection(data, 0)
    }

    /// 从内存里的 TTF/TTC 字节解析指定字面
    pub fn parse_collection(data: Vec<u8>, face_index: u32) -> Result<Font> {
        if data.len() >= 4 && &data[0..4] == b"ttcf" {
            let unpacked = extract_ttc_face(&data, face_index)?;
            return Self::parse_bytes(FontBytes::Owned(unpacked));
        }
        Self::parse_bytes(FontBytes::Owned(data))
    }

    fn parse_bytes(data: FontBytes) -> Result<Font> {
        anyhow::ensure!(data.len() > 12, "字体文件太小");
        anyhow::ensure!(
            &data[0..4] == b"\x00\x01\x00\x00" || &data[0..4] == b"true",
            "不是 TrueType 字体"
        );
        let n_tables = be16(&data, 4) as usize;
        let mut tables: HashMap<[u8; 4], (usize, usize)> = HashMap::with_capacity(n_tables);
        for i in 0..n_tables {
            let off = 12 + i * 16;
            if off + 16 > data.len() {
                break;
            }
            let tag = [data[off], data[off + 1], data[off + 2], data[off + 3]];
            let toff = be32(&data, off + 8) as usize;
            let tlen = be32(&data, off + 12) as usize;
            tables.insert(tag, (toff, tlen));
        }
        let head = tables.get(b"head").context("缺少 head 表")?.0;
        let hhea = tables.get(b"hhea").context("缺少 hhea 表")?.0;
        let maxp = tables.get(b"maxp").context("缺少 maxp 表")?.0;
        let hmtx = tables.get(b"hmtx").context("缺少 hmtx 表")?.0;
        let cmap_off = tables.get(b"cmap").context("缺少 cmap 表")?.0;

        let units_per_em = be16(&data, head + 18);
        let ascender = bei16(&data, hhea + 4);
        let descender = bei16(&data, hhea + 6);
        let x_min = bei16(&data, head + 36);
        let y_min = bei16(&data, head + 38);
        let x_max = bei16(&data, head + 40);
        let y_max = bei16(&data, head + 42);
        let n_glyphs = be16(&data, maxp + 4) as usize;
        let n_hmetrics = be16(&data, hhea + 34) as usize;

        let mut widths = vec![0u16; n_glyphs];
        for gid in 0..n_glyphs {
            let idx = gid.min(n_hmetrics.saturating_sub(1));
            if hmtx + idx * 4 + 2 <= data.len() {
                let adv = be16(&data, hmtx + idx * 4);
                widths[gid] = ((adv as u32 * 1000) / units_per_em.max(1) as u32) as u16;
            }
        }

        let cmap = parse_cmap(&data, cmap_off);
        Ok(Font {
            inner: Arc::new(FontInner {
                data,
                cmap,
                widths,
                ascender,
                descender,
                bbox: [x_min, y_min, x_max, y_max],
            }),
            units_per_em,
        })
    }

    pub fn em_metrics(&self) -> (f32, f32) {
        let upm = self.units_per_em.max(1) as f32;
        (
            self.inner.ascender as f32 / upm,
            (-self.inner.descender as f32) / upm,
        )
    }

    pub fn gid(&self, ch: char) -> u16 {
        self.inner.cmap.get(&ch).copied().unwrap_or(0)
    }

    pub fn has_char(&self, ch: char) -> bool {
        self.gid(ch) != 0
    }

    pub fn width_of(&self, ch: char, size: f32) -> f32 {
        let g = self.gid(ch) as usize;
        let w = *self.inner.widths.get(g).unwrap_or(&500) as f32;
        w / 1000.0 * size
    }

    pub fn text_width(&self, s: &str, size: f32) -> f32 {
        s.chars().map(|c| self.width_of(c, size)).sum()
    }

    pub fn advance(&self, ch: char) -> Option<f32> {
        let g = *self.inner.cmap.get(&ch)? as usize;
        if g == 0 {
            return None;
        }
        let w = *self.inner.widths.get(g)? as f32;
        if w <= 0.0 {
            return None;
        }
        Some(w / 1000.0)
    }

    pub fn metric(&self) -> std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync> {
        let inner = self.inner.clone();
        std::sync::Arc::new(move |ch: char, size: f32| {
            let g = *inner.cmap.get(&ch).unwrap_or(&0) as usize;
            let w = *inner.widths.get(g).unwrap_or(&500) as f32;
            w / 1000.0 * size
        })
    }
}

fn parse_cmap(data: &[u8], cmap_off: usize) -> HashMap<char, u16> {
    let mut map = HashMap::new();
    if cmap_off + 4 > data.len() {
        return map;
    }
    let n = be16(data, cmap_off + 2) as usize;
    let mut best: Option<usize> = None;
    let mut best_score = 0;
    for i in 0..n {
        let rec = cmap_off + 4 + i * 8;
        if rec + 8 > data.len() {
            break;
        }
        let platform = be16(data, rec);
        let encoding = be16(data, rec + 2);
        let sub = cmap_off + be32(data, rec + 4) as usize;
        if sub + 2 > data.len() {
            continue;
        }
        let format = be16(data, sub);
        let score = match (platform, encoding, format) {
            (3, 10, 12) => 5,
            (3, 1, 4) => 4,
            (0, _, 12) => 3,
            (0, _, 4) => 2,
            (_, _, 12) => 2,
            (_, _, 4) => 1,
            _ => 0,
        };
        if score > best_score {
            best_score = score;
            best = Some(sub);
        }
    }
    let Some(sub) = best else { return map };
    match be16(data, sub) {
        4 => {
            if sub + 14 > data.len() {
                return map;
            }
            let seg_x2 = be16(data, sub + 6) as usize;
            let seg = seg_x2 / 2;
            let end_base = sub + 14;
            let start_base = end_base + seg_x2 + 2;
            let delta_base = start_base + seg_x2;
            let range_base = delta_base + seg_x2;
            if range_base + seg_x2 > data.len() {
                return map;
            }
            for i in 0..seg {
                let end = be16(data, end_base + i * 2) as u32;
                let start = be16(data, start_base + i * 2) as u32;
                if start == 0xffff {
                    continue;
                }
                let delta = bei16(data, delta_base + i * 2);
                let ro = be16(data, range_base + i * 2) as usize;
                for c in start..=end.min(start + 0xffff) {
                    let gid = if ro == 0 {
                        (c as i32 + delta as i32) as u16
                    } else {
                        let addr = range_base + i * 2 + ro + (c - start) as usize * 2;
                        if addr + 2 > data.len() {
                            continue;
                        }
                        let g = be16(data, addr);
                        if g == 0 {
                            0
                        } else {
                            (g as i32 + delta as i32) as u16
                        }
                    };
                    if gid != 0
                        && let Some(ch) = char::from_u32(c)
                    {
                        map.insert(ch, gid);
                    }
                }
            }
        }
        12 => {
            if sub + 16 > data.len() {
                return map;
            }
            let n_groups = be32(data, sub + 12) as usize;
            for i in 0..n_groups {
                let g = sub + 16 + i * 12;
                if g + 12 > data.len() {
                    break;
                }
                let start = be32(data, g);
                let end = be32(data, g + 4);
                let start_gid = be32(data, g + 8);
                for c in start..=end.min(start + 0xffff) {
                    let gid = start_gid + (c - start);
                    if gid <= u16::MAX as u32
                        && let Some(ch) = char::from_u32(c)
                    {
                        map.insert(ch, gid as u16);
                    }
                }
            }
        }
        _ => {}
    }
    map
}

/// 一页的内容流（单位 pt，原点在左下）
pub struct Page {
    pub width: f32,
    pub height: f32,
    pub ops: String,
    used: HashSet<u16>,
    used_bold: HashSet<u16>,
    used_fb: HashSet<u16>,
    used_fb_bold: HashSet<u16>,
    /// 本页使用的透明度状态（百分比 1..=99 -> 资源名 `/GSaXX`）
    alphas: BTreeMap<u8, f32>,
}

#[inline]
fn hex_nibble(b: u8) -> Option<u16> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u16),
        b'A'..=b'F' => Some((b - b'A' + 10) as u16),
        b'a'..=b'f' => Some((b - b'a' + 10) as u16),
        _ => None,
    }
}

fn note_glyphs(hex: &str, set: &mut HashSet<u16>) {
    let b = hex.as_bytes();
    let mut i = 0;
    while i + 4 <= b.len() {
        if let (Some(n0), Some(n1), Some(n2), Some(n3)) = (
            hex_nibble(b[i]),
            hex_nibble(b[i + 1]),
            hex_nibble(b[i + 2]),
            hex_nibble(b[i + 3]),
        ) {
            set.insert((n0 << 12) | (n1 << 8) | (n2 << 4) | n3);
        }
        i += 4;
    }
}

fn compress_stream(data: &[u8]) -> (Vec<u8>, bool) {
    use std::io::Write;
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    if enc.write_all(data).is_ok()
        && let Ok(v) = enc.finish()
    {
        return (v, true);
    }
    (data.to_vec(), false)
}

fn tounicode_cmap(font: &Font, used: &HashSet<u16>) -> String {
    let mut gid_to_char: Vec<(u16, char)> = if used.is_empty() {
        font.inner
            .cmap
            .iter()
            .filter(|(ch, _)| !ch.is_control())
            .take(1)
            .map(|(ch, gid)| (*gid, *ch))
            .collect()
    } else {
        font.inner
            .cmap
            .iter()
            .filter(|(ch, gid)| !ch.is_control() && used.contains(gid))
            .map(|(ch, gid)| (*gid, *ch))
            .collect()
    };
    if gid_to_char.is_empty() {
        gid_to_char.push((0, ' '));
    }
    gid_to_char.sort_unstable_by_key(|(gid, _)| *gid);
    gid_to_char.dedup_by_key(|(gid, _)| *gid);
    let mut cmap = String::with_capacity(256 + gid_to_char.len() * 20);
    cmap.push_str(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in gid_to_char.chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for &(gid, ch) in chunk {
            let mut buf = [0u16; 2];
            let encoded = ch.encode_utf16(&mut buf);
            let _ = write!(cmap, "<{gid:04X}> <");
            for u in encoded {
                let _ = write!(cmap, "{u:04X}");
            }
            cmap.push_str(">\n");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap
}

fn build_widths_array(widths: &[u16], used: &HashSet<u16>) -> String {
    if used.is_empty() {
        let w0 = widths.first().copied().unwrap_or(1000);
        return format!("[ 0 [{w0}] ]");
    }
    let mut gids: Vec<u16> = used.iter().copied().collect();
    gids.sort_unstable();
    let mut out = String::with_capacity(32 + gids.len() * 10);
    out.push_str("[ ");
    let mut i = 0usize;
    while i < gids.len() {
        let start = gids[i];
        let mut end = i;
        while end + 1 < gids.len() && gids[end + 1] == gids[end].saturating_add(1) {
            end += 1;
        }
        let _ = write!(out, "{start} [");
        for &g in &gids[i..=end] {
            let w = widths.get(g as usize).copied().unwrap_or(1000);
            let _ = write!(out, "{w} ");
        }
        out.push_str("] ");
        i = end + 1;
    }
    out.push(']');
    out
}

/// 将 UTF-8 文本编码为带 UTF-16BE BOM 的 PDF 十六进制字符串（书签与元信息支持完整中文）
fn pdf_utf16be_hex(s: &str) -> String {
    let mut out = String::with_capacity(6 + s.len() * 4);
    out.push_str("<FEFF");
    for u in s.encode_utf16() {
        let _ = write!(out, "{u:04X}");
    }
    out.push('>');
    out
}

impl Page {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            ops: String::with_capacity(4096),
            used: HashSet::new(),
            used_bold: HashSet::new(),
            used_fb: HashSet::new(),
            used_fb_bold: HashSet::new(),
            alphas: BTreeMap::new(),
        }
    }

    fn note_face(&mut self, face: &str, hex: &str) {
        let set = match face {
            "F2" => &mut self.used_bold,
            "F3" => &mut self.used_fb,
            "F4" => &mut self.used_fb_bold,
            _ => &mut self.used,
        };
        note_glyphs(hex, set);
    }

    fn alpha_state(&mut self, alpha: f32) -> Option<u8> {
        let a = alpha.clamp(0.01, 1.0);
        if a >= 0.995 {
            return None;
        }
        let key = (a * 100.0).round().clamp(1.0, 99.0) as u8;
        self.alphas.entry(key).or_insert(key as f32 / 100.0);
        Some(key)
    }

    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let _ = writeln!(self.ops, "q {x:.2} {y:.2} {w:.2} {h:.2} re W n");
    }

    pub fn pop_clip(&mut self) {
        self.ops.push_str("Q\n");
    }

    pub fn rect_fill(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: [u8; 3]) {
        let _ = writeln!(
            self.ops,
            "{:.3} {:.3} {:.3} rg {x:.2} {y:.2} {w:.2} {h:.2} re f",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        );
    }

    pub fn rect_fill_alpha(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: [u8; 3], alpha: f32) {
        if let Some(k) = self.alpha_state(alpha) {
            let _ = writeln!(
                self.ops,
                "q /GSa{k} gs {:.3} {:.3} {:.3} rg {x:.2} {y:.2} {w:.2} {h:.2} re f Q",
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            );
        } else {
            self.rect_fill(x, y, w, h, rgb);
        }
    }

    pub fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, rgb: [u8; 3]) {
        let _ = writeln!(
            self.ops,
            "{:.3} {:.3} {:.3} RG {w:.2} w {x1:.2} {y1:.2} m {x2:.2} {y2:.2} l S",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        );
    }

    /// 带虚实线型的矢量直线（`dash` 为空表示实线；支持全部 13 种 Excel 边框样式）
    pub fn line_styled(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        w: f32,
        rgb: [u8; 3],
        dash: &[f32],
    ) {
        if dash.is_empty() {
            self.line(x1, y1, x2, y2, w, rgb);
            return;
        }
        let _ = write!(self.ops, "q [");
        for (i, d) in dash.iter().enumerate() {
            if i > 0 {
                self.ops.push(' ');
            }
            let _ = write!(self.ops, "{d:.2}");
        }
        let _ = writeln!(
            self.ops,
            "] 0 d {:.3} {:.3} {:.3} RG {w:.2} w {x1:.2} {y1:.2} m {x2:.2} {y2:.2} l S Q",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        );
    }

    pub fn text(&mut self, x: f32, baseline: f32, size: f32, hex: &str, rgb: [u8; 3]) {
        note_glyphs(hex, &mut self.used);
        let _ = writeln!(
            self.ops,
            "BT {:.3} {:.3} {:.3} rg /F1 {size:.2} Tf 1 0 0 1 {x:.2} {baseline:.2} Tm <{hex}> Tj ET",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        );
    }

    pub fn text_styled_face(
        &mut self,
        face: &str,
        x: f32,
        baseline: f32,
        size: f32,
        hex: &str,
        rgb: [u8; 3],
        italic: bool,
        rotation: f32,
    ) {
        self.text_styled_face_alpha(face, x, baseline, size, hex, rgb, italic, rotation, 1.0);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_styled_face_alpha(
        &mut self,
        face: &str,
        x: f32,
        baseline: f32,
        size: f32,
        hex: &str,
        rgb: [u8; 3],
        italic: bool,
        rotation: f32,
        alpha: f32,
    ) {
        self.note_face(face, hex);
        let rad = rotation.to_radians();
        let (sin, cos) = (rad.sin(), rad.cos());
        let skew = if italic { 0.21 } else { 0.0 };
        let a = cos;
        let b = sin;
        let c = -sin + skew * cos;
        let d = cos;
        let gs = self.alpha_state(alpha);
        if let Some(k) = gs {
            let _ = write!(self.ops, "q /GSa{k} gs ");
        }
        let _ = write!(
            self.ops,
            "BT {:.3} {:.3} {:.3} rg /{face} {size:.2} Tf {a:.4} {b:.4} {c:.4} {d:.4} {x:.2} {baseline:.2} Tm <{hex}> Tj ET",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        );
        if gs.is_some() {
            self.ops.push_str(" Q\n");
        } else {
            self.ops.push('\n');
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_styled(
        &mut self,
        x: f32,
        baseline: f32,
        size: f32,
        hex: &str,
        rgb: [u8; 3],
        bold: bool,
        italic: bool,
        rotation: f32,
    ) {
        self.text_styled_on("F1", x, baseline, size, hex, rgb, bold, italic, rotation, 1.0);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_styled_on(
        &mut self,
        face: &str,
        x: f32,
        baseline: f32,
        size: f32,
        hex: &str,
        rgb: [u8; 3],
        synthetic_bold: bool,
        italic: bool,
        rotation: f32,
        alpha: f32,
    ) {
        self.note_face(face, hex);
        let rad = rotation.to_radians();
        let (sin, cos) = (rad.sin(), rad.cos());
        let skew = if italic { 0.21 } else { 0.0 };
        let a = cos;
        let b = sin;
        let c = -sin + skew * cos;
        let d = cos;
        let (r0, g0, b0) = (
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        );
        let gs = self.alpha_state(alpha);
        if let Some(k) = gs {
            let _ = write!(self.ops, "q /GSa{k} gs ");
        }
        let _ = write!(self.ops, "BT {r0:.3} {g0:.3} {b0:.3} rg ");
        if synthetic_bold {
            let _ = write!(
                self.ops,
                "{r0:.3} {g0:.3} {b0:.3} RG {:.3} w 2 Tr ",
                size * 0.035
            );
        }
        let _ = write!(
            self.ops,
            "/{face} {size:.2} Tf {a:.4} {b:.4} {c:.4} {d:.4} {x:.2} {baseline:.2} Tm <{hex}> Tj ET"
        );
        if gs.is_some() {
            self.ops.push_str(" Q\n");
        } else {
            self.ops.push('\n');
        }
    }

    pub fn image(&mut self, name: &str, x: f32, y: f32, w: f32, h: f32) {
        let _ = writeln!(self.ops, "q {w:.2} 0 0 {h:.2} {x:.2} {y:.2} cm /{name} Do Q");
    }

    /// 绘制带可选裁剪（对未在像素层裁切的流如 JPEG）与顺时针旋转角度（度）的图片。
    pub fn image_transformed(
        &mut self,
        name: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        crop: Option<[f32; 4]>,
        rot_deg: f32,
    ) {
        let rot = rot_deg.rem_euclid(360.0);
        let has_rot = rot > 0.01 && (360.0 - rot) > 0.01;
        if crop.is_none() && !has_rot {
            self.image(name, x, y, w, h);
            return;
        }
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        // 屏幕/OOXML 顺时针角度对应 PDF（y 向上）负角度
        let theta = -rot.to_radians();
        let (raw_s, raw_c) = theta.sin_cos();
        let clean = |v: f32| if v.abs() < 1e-5 { 0.0 } else { v };
        let s = clean(raw_s);
        let c = clean(raw_c);
        if let Some(cr) = crop {
            let u0 = cr[0].clamp(0.0, 0.9);
            let v0 = cr[1].clamp(0.0, 0.9);
            let u1 = (1.0 - cr[2]).clamp((u0 + 0.02).min(1.0), 1.0);
            let v1 = (1.0 - cr[3]).clamp((v0 + 0.02).min(1.0), 1.0);
            let full_w = w / (u1 - u0);
            let full_h = h / (v1 - v0);
            let ox = -u0 * full_w;
            let oy = -(1.0 - v1) * full_h;
            let tx = cx - 0.5 * w * c + 0.5 * h * s;
            let ty = cy - 0.5 * w * s - 0.5 * h * c;
            let _ = writeln!(
                self.ops,
                "q {c:.5} {s:.5} {:.5} {c:.5} {tx:.2} {ty:.2} cm 0 0 {w:.2} {h:.2} re W n {full_w:.2} 0 0 {full_h:.2} {ox:.2} {oy:.2} cm /{name} Do Q",
                clean(-s),
            );
        } else {
            let a = clean(w * c);
            let b = clean(w * s);
            let cm_c = clean(-h * s);
            let d = clean(h * c);
            let e = cx - 0.5 * a - 0.5 * cm_c;
            let f = cy - 0.5 * b - 0.5 * d;
            let _ = writeln!(
                self.ops,
                "q {a:.4} {b:.4} {cm_c:.4} {d:.4} {e:.2} {f:.2} cm /{name} Do Q"
            );
        }
    }

    /// 任意折线/多边形：填充时用 `rg + h f`，描边时用 `RG + S`（修复旧版描边写死黑色 bug）
    pub fn poly(&mut self, pts: &[(f32, f32)], rgb: [u8; 3], fill: bool) {
        self.poly_styled(pts, 1.2, rgb, &[], fill, 1.0);
    }

    pub fn poly_styled(
        &mut self,
        pts: &[(f32, f32)],
        lw: f32,
        rgb: [u8; 3],
        dash: &[f32],
        fill: bool,
        alpha: f32,
    ) {
        if pts.len() < 2 {
            return;
        }
        let gs = self.alpha_state(alpha);
        let wrap = gs.is_some() || !dash.is_empty();
        if wrap {
            self.ops.push_str("q ");
            if let Some(k) = gs {
                let _ = write!(self.ops, "/GSa{k} gs ");
            }
            if !dash.is_empty() && !fill {
                self.ops.push('[');
                for (i, d) in dash.iter().enumerate() {
                    if i > 0 {
                        self.ops.push(' ');
                    }
                    let _ = write!(self.ops, "{d:.2}");
                }
                self.ops.push_str("] 0 d ");
            }
        }
        let (r, g, b) = (
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        );
        if fill {
            let _ = write!(self.ops, "{r:.3} {g:.3} {b:.3} rg {:.2} {:.2} m ", pts[0].0, pts[0].1);
        } else {
            let _ = write!(
                self.ops,
                "{r:.3} {g:.3} {b:.3} RG {lw:.2} w 1 J 1 j {:.2} {:.2} m ",
                pts[0].0, pts[0].1
            );
        }
        for p in &pts[1..] {
            let _ = write!(self.ops, "{:.2} {:.2} l ", p.0, p.1);
        }
        self.ops.push_str(if fill { "h f" } else { "S" });
        if wrap {
            self.ops.push_str(" Q\n");
        } else {
            self.ops.push('\n');
        }
    }

    pub fn glyph_hex(font: &Font, s: &str) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = Vec::with_capacity(s.len() * 4);
        for ch in s.chars() {
            let g = font.gid(ch);
            out.push(HEX[((g >> 12) & 0xF) as usize]);
            out.push(HEX[((g >> 8) & 0xF) as usize]);
            out.push(HEX[((g >> 4) & 0xF) as usize]);
            out.push(HEX[(g & 0xF) as usize]);
        }
        // SAFETY: `HEX` only contains ASCII hex digits.
        unsafe { String::from_utf8_unchecked(out) }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DocMeta {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub creator: String,
}

#[derive(Clone, Debug)]
pub struct Bookmark {
    pub title: String,
    pub page_index: usize,
}

/// 一份 PDF 文档（支持常规/粗体 + 系统 CJK 回退字体链 + 书签目录 + 元信息 + 透明度）
pub struct Doc {
    pub pages: Vec<Page>,
    font: Font,
    bold: Option<Font>,
    fallback: Option<Font>,
    fallback_bold: Option<Font>,
    pub images: Vec<PdfImage>,
    pub meta: DocMeta,
    pub bookmarks: Vec<Bookmark>,
}

#[derive(Clone, Debug)]
pub struct PdfImage {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub colors: u8,
    pub dct: bool,
    pub data: Vec<u8>,
}

impl Doc {
    pub fn new(font: Font) -> Self {
        Self {
            pages: Vec::new(),
            font,
            bold: None,
            fallback: None,
            fallback_bold: None,
            images: Vec::new(),
            meta: DocMeta {
                creator: "CELLAN Spreadsheet Engine".into(),
                ..Default::default()
            },
            bookmarks: Vec::new(),
        }
    }

    pub fn set_bold_font(&mut self, font: Font) {
        self.bold = Some(font);
    }

    pub fn set_fallback_font(&mut self, font: Font) {
        self.fallback = Some(font);
    }

    pub fn set_fallback_bold_font(&mut self, font: Font) {
        self.fallback_bold = Some(font);
    }

    pub fn set_metadata(
        &mut self,
        title: impl Into<String>,
        author: impl Into<String>,
        subject: impl Into<String>,
    ) {
        self.meta.title = title.into();
        self.meta.author = author.into();
        self.meta.subject = subject.into();
    }

    pub fn add_bookmark(&mut self, title: impl Into<String>, page_index: usize) {
        self.bookmarks.push(Bookmark {
            title: title.into(),
            page_index,
        });
    }

    pub fn bold_font(&self) -> Option<&Font> {
        self.bold.as_ref()
    }

    pub fn font_for(&self, bold: bool) -> &Font {
        match (bold, self.bold.as_ref()) {
            (true, Some(f)) => f,
            _ => &self.font,
        }
    }

    /// 智能多字体回退分段绘制：对主字体缺失的生僻字/繁日韩/符号自动路由至系统 CJK 回退字库，
    /// 旋转与斜体文本按精确旋转向量步进，杜绝豆腐块。
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_auto(
        &mut self,
        x: f32,
        baseline: f32,
        size: f32,
        text: &str,
        rgb: [u8; 3],
        bold: bool,
        italic: bool,
        rotation: f32,
        alpha: f32,
    ) {
        if text.is_empty() {
            return;
        }
        let has_bold = self.bold.is_some();
        let has_fb = self.fallback.is_some();
        let has_fb_bold = self.fallback_bold.is_some();

        // 决定每个字符走哪条字体槽（0=F1, 1=F2, 2=F3, 3=F4）及是否需要描边模拟粗体
        let mut runs: Vec<(u8, bool, String, f32)> = Vec::new();
        for ch in text.chars() {
            let (slot, synth_bold, adv) = if bold {
                if has_bold && self.bold.as_ref().unwrap().has_char(ch) {
                    (1u8, false, self.bold.as_ref().unwrap().width_of(ch, size))
                } else if !has_bold && self.font.has_char(ch) {
                    (0u8, true, self.font.width_of(ch, size))
                } else if has_fb_bold && self.fallback_bold.as_ref().unwrap().has_char(ch) {
                    (3u8, false, self.fallback_bold.as_ref().unwrap().width_of(ch, size))
                } else if has_fb && self.fallback.as_ref().unwrap().has_char(ch) {
                    (2u8, true, self.fallback.as_ref().unwrap().width_of(ch, size))
                } else if has_bold {
                    (1u8, false, self.bold.as_ref().unwrap().width_of(ch, size))
                } else {
                    (0u8, true, self.font.width_of(ch, size))
                }
            } else if self.font.has_char(ch) {
                (0u8, false, self.font.width_of(ch, size))
            } else if has_fb && self.fallback.as_ref().unwrap().has_char(ch) {
                (2u8, false, self.fallback.as_ref().unwrap().width_of(ch, size))
            } else {
                (0u8, false, self.font.width_of(ch, size))
            };
            if let Some(last) = runs.last_mut()
                && last.0 == slot
                && last.1 == synth_bold
            {
                last.2.push(ch);
                last.3 += adv;
            } else {
                let mut s = String::new();
                s.push(ch);
                runs.push((slot, synth_bold, s, adv));
            }
        }

        let rad = rotation.to_radians();
        let (dx_u, dy_u) = (rad.cos(), rad.sin());
        let mut pen = 0.0f32;
        for (slot, synth_bold, seg, adv) in runs {
            let (face, hex) = match slot {
                1 => ("F2", Page::glyph_hex(self.bold.as_ref().unwrap(), &seg)),
                2 => ("F3", Page::glyph_hex(self.fallback.as_ref().unwrap(), &seg)),
                3 => ("F4", Page::glyph_hex(self.fallback_bold.as_ref().unwrap(), &seg)),
                _ => ("F1", Page::glyph_hex(&self.font, &seg)),
            };
            let rx = x + pen * dx_u;
            let ry = baseline + pen * dy_u;
            let page = self.pages.last_mut().expect("先 add_page");
            if synth_bold {
                page.text_styled_on(face, rx, ry, size, &hex, rgb, true, italic, rotation, alpha);
            } else {
                page.text_styled_face_alpha(face, rx, ry, size, &hex, rgb, italic, rotation, alpha);
            }
            pen += adv;
        }
    }

    pub fn add_image(&mut self, img: PdfImage) -> String {
        if self.images.iter().any(|x| x.name == img.name) {
            return img.name;
        }
        let name = img.name.clone();
        self.images.push(img);
        name
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    pub fn add_page(&mut self, w: f32, h: f32) {
        self.pages.push(Page::new(w, h));
    }

    pub fn page_mut(&mut self) -> &mut Page {
        self.pages.last_mut().expect("先 add_page")
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut used: HashSet<u16> = HashSet::new();
        let mut used_bold: HashSet<u16> = HashSet::new();
        let mut used_fb: HashSet<u16> = HashSet::new();
        let mut used_fb_bold: HashSet<u16> = HashSet::new();
        let mut all_alphas: BTreeMap<u8, f32> = BTreeMap::new();
        for p in &self.pages {
            used.extend(p.used.iter().copied());
            used_bold.extend(p.used_bold.iter().copied());
            used_fb.extend(p.used_fb.iter().copied());
            used_fb_bold.extend(p.used_fb_bold.iter().copied());
            for (&k, &v) in &p.alphas {
                all_alphas.insert(k, v);
            }
        }

        let reg_bytes: &[u8] = &self.font.inner.data;
        let regular_sub =
            crate::font_subset::subset(reg_bytes, &used).filter(|v| v.len() < reg_bytes.len());
        let regular_slice: &[u8] = regular_sub.as_deref().unwrap_or(reg_bytes);

        let emit_fb = self.fallback.is_some() && !used_fb.is_empty();
        let emit_fb_bold = self.fallback_bold.is_some() && !used_fb_bold.is_empty();

        let mut out: Vec<u8> = Vec::with_capacity(65_536 + self.pages.len() * 4096);
        out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        let mut offsets: Vec<usize> = Vec::with_capacity(24 + self.pages.len() * 2 + self.images.len());
        let obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
            offsets.push(out.len());
            let _ = std::io::Write::write_fmt(out, format_args!("{} 0 obj\n", offsets.len()));
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };

        // 对象编号规划（严格保持 1..7 兼容已有结构测试）：
        // 1 Catalog, 2 Pages, 3..7 F1 Regular,
        // 8..8+2*n_pages-1 Pages & Contents (或空页 1 个占位),
        // image_base.. images,
        // bold_base.. F2 (5 objs if present),
        // fb_base.. F3 (5 objs if emit_fb),
        // fb_bold_base.. F4 (5 objs if emit_fb_bold),
        // gs_base.. ExtGState (1 per alpha key),
        // outlines_base.. Outlines (1 root + N items if bookmarks non-empty),
        // info_id Info dict
        let n_pages = self.pages.len().max(1);
        let page_ids: Vec<usize> = (0..n_pages).map(|i| 8 + i * 2).collect();
        let mut kids = String::with_capacity(n_pages * 8);
        for (idx, id) in page_ids.iter().enumerate() {
            if idx > 0 {
                kids.push(' ');
            }
            let _ = write!(kids, "{id} 0 R");
        }

        let blank_page = usize::from(self.pages.is_empty());
        let image_base = 8 + self.pages.len() * 2 + blank_page;
        let bold_base = image_base + self.images.len();
        let fb_base = bold_base + if self.bold.is_some() { 5 } else { 0 };
        let fb_bold_base = fb_base + if emit_fb { 5 } else { 0 };
        let gs_base = fb_bold_base + if emit_fb_bold { 5 } else { 0 };
        let gs_keys: Vec<u8> = all_alphas.keys().copied().collect();
        let outlines_base = gs_base + gs_keys.len();
        let valid_bms: Vec<&Bookmark> = self
            .bookmarks
            .iter()
            .filter(|b| b.page_index < self.pages.len())
            .collect();
        let has_outlines = !valid_bms.is_empty();
        let info_id = outlines_base + if has_outlines { 1 + valid_bms.len() } else { 0 };

        // 1: Catalog
        let catalog = if has_outlines {
            format!("<< /Type /Catalog /Pages 2 0 R /Outlines {outlines_base} 0 R /PageMode /UseOutlines >>")
        } else {
            "<< /Type /Catalog /Pages 2 0 R >>".to_string()
        };
        obj(&mut out, &mut offsets, catalog.as_bytes());

        // 2: Pages
        obj(
            &mut out,
            &mut offsets,
            format!("<< /Type /Pages /Count {n_pages} /Kids [{kids}] >>").as_bytes(),
        );

        // 3..7: F1 Regular
        let emit_font_objs = |out: &mut Vec<u8>,
                              offsets: &mut Vec<usize>,
                              id0: usize,
                              base_name: &str,
                              stem_v: u32,
                              f: &Font,
                              f_used: &HashSet<u16>,
                              f_slice: &[u8]| {
            let upm = f.units_per_em.max(1) as f32;
            obj(
                out,
                offsets,
                format!(
                    "<< /Type /Font /Subtype /Type0 /BaseFont /{base_name} /Encoding /Identity-H /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
                    id0 + 1,
                    id0 + 4
                )
                .as_bytes(),
            );
            let w = build_widths_array(&f.inner.widths, f_used);
            obj(
                out,
                offsets,
                format!(
                    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{base_name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {} 0 R /DW 1000 /W {w} /CIDToGIDMap /Identity >>",
                    id0 + 2
                )
                .as_bytes(),
            );
            obj(
                out,
                offsets,
                format!(
                    "<< /Type /FontDescriptor /FontName /{base_name} /Flags 4 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {:.0} /Descent {:.0} /CapHeight {:.0} /StemV {stem_v} /FontFile2 {} 0 R >>",
                    f.inner.bbox[0] as f32 * 1000.0 / upm,
                    f.inner.bbox[1] as f32 * 1000.0 / upm,
                    f.inner.bbox[2] as f32 * 1000.0 / upm,
                    f.inner.bbox[3] as f32 * 1000.0 / upm,
                    f.inner.ascender as f32 * 1000.0 / upm,
                    f.inner.descender as f32 * 1000.0 / upm,
                    f.inner.ascender as f32 * 1000.0 / upm * 0.7,
                    id0 + 3
                )
                .as_bytes(),
            );
            let (packed, _) = compress_stream(f_slice);
            let mut body: Vec<u8> = format!(
                "<< /Length {} /Length1 {} /Filter /FlateDecode >>\nstream\n",
                packed.len(),
                f_slice.len()
            )
            .into_bytes();
            body.extend_from_slice(&packed);
            body.extend_from_slice(b"\nendstream");
            obj(out, offsets, &body);

            let cmap = tounicode_cmap(f, f_used);
            let mut cbody = format!("<< /Length {} >>\nstream\n", cmap.len()).into_bytes();
            cbody.extend_from_slice(cmap.as_bytes());
            cbody.extend_from_slice(b"endstream");
            obj(out, offsets, &cbody);
        };

        emit_font_objs(&mut out, &mut offsets, 3, "Embedded", 80, &self.font, &used, regular_slice);

        // 资源字典片段
        let xobjects = if self.images.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = self
                .images
                .iter()
                .enumerate()
                .map(|(i, im)| format!("/{} {} 0 R", im.name, image_base + i))
                .collect();
            format!(" /XObject << {} >>", items.join(" "))
        };
        let ext_gstates = if gs_keys.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = gs_keys
                .iter()
                .enumerate()
                .map(|(i, k)| format!("/GSa{k} {} 0 R", gs_base + i))
                .collect();
            format!(" /ExtGState << {} >>", items.join(" "))
        };
        let mut fonts_res = String::from("/Font << /F1 3 0 R");
        if self.bold.is_some() {
            let _ = write!(fonts_res, " /F2 {bold_base} 0 R");
        }
        if emit_fb {
            let _ = write!(fonts_res, " /F3 {fb_base} 0 R");
        }
        if emit_fb_bold {
            let _ = write!(fonts_res, " /F4 {fb_bold_base} 0 R");
        }
        fonts_res.push_str(" >>");

        // Pages & Contents
        for (i, p) in self.pages.iter().enumerate() {
            let content_id = page_ids[i] + 1;
            let body = format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << {fonts_res}{xobjects}{ext_gstates} >> /Contents {content_id} 0 R >>",
                p.width, p.height
            );
            obj(&mut out, &mut offsets, body.as_bytes());
            let stream = p.ops.as_bytes();
            if stream.len() >= 32 * 1024 {
                let (packed, ok) = compress_stream(stream);
                if ok && packed.len() < stream.len() {
                    let mut c = format!(
                        "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                        packed.len()
                    )
                    .into_bytes();
                    c.extend_from_slice(&packed);
                    c.extend_from_slice(b"\nendstream");
                    obj(&mut out, &mut offsets, &c);
                    continue;
                }
            }
            let mut c = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
            c.extend_from_slice(stream);
            c.extend_from_slice(b"endstream");
            obj(&mut out, &mut offsets, &c);
        }
        if self.pages.is_empty() {
            let body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> >>";
            obj(&mut out, &mut offsets, body);
        }

        // Images
        for im in &self.images {
            let filter = if im.dct {
                "/Filter /DCTDecode"
            } else {
                "/Filter /FlateDecode"
            };
            let cs = if im.colors == 1 {
                "/DeviceGray"
            } else {
                "/DeviceRGB"
            };
            let mut body = format!(
                "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {cs} /BitsPerComponent 8 {filter} /Length {} >>\nstream\n",
                im.width,
                im.height,
                im.data.len()
            )
            .into_bytes();
            body.extend_from_slice(&im.data);
            body.extend_from_slice(b"\nendstream");
            obj(&mut out, &mut offsets, &body);
        }

        // F2 Bold
        if let Some(bf) = &self.bold {
            let b_bytes: &[u8] = &bf.inner.data;
            let b_sub = crate::font_subset::subset(b_bytes, &used_bold)
                .filter(|v| v.len() < b_bytes.len());
            let b_slice: &[u8] = b_sub.as_deref().unwrap_or(b_bytes);
            emit_font_objs(
                &mut out,
                &mut offsets,
                bold_base,
                "EmbeddedBold",
                140,
                bf,
                &used_bold,
                b_slice,
            );
        }

        // F3 Fallback Regular
        if emit_fb && let Some(fb) = &self.fallback {
            let fb_bytes: &[u8] = &fb.inner.data;
            let fb_sub = crate::font_subset::subset(fb_bytes, &used_fb)
                .filter(|v| v.len() < fb_bytes.len());
            let fb_slice: &[u8] = fb_sub.as_deref().unwrap_or(fb_bytes);
            emit_font_objs(
                &mut out,
                &mut offsets,
                fb_base,
                "EmbeddedFallback",
                80,
                fb,
                &used_fb,
                fb_slice,
            );
        }

        // F4 Fallback Bold
        if emit_fb_bold && let Some(fbb) = &self.fallback_bold {
            let fbb_bytes: &[u8] = &fbb.inner.data;
            let fbb_sub = crate::font_subset::subset(fbb_bytes, &used_fb_bold)
                .filter(|v| v.len() < fbb_bytes.len());
            let fbb_slice: &[u8] = fbb_sub.as_deref().unwrap_or(fbb_bytes);
            emit_font_objs(
                &mut out,
                &mut offsets,
                fb_bold_base,
                "EmbeddedFallbackBold",
                140,
                fbb,
                &used_fb_bold,
                fbb_slice,
            );
        }

        // ExtGState 透明度字典
        for &k in &gs_keys {
            let a = all_alphas[&k];
            let body = format!("<< /Type /ExtGState /ca {a:.2} /CA {a:.2} >>");
            obj(&mut out, &mut offsets, body.as_bytes());
        }

        // PDF 书签目录 Outlines
        if has_outlines {
            let first_item = outlines_base + 1;
            let last_item = outlines_base + valid_bms.len();
            let root = format!(
                "<< /Type /Outlines /First {first_item} 0 R /Last {last_item} 0 R /Count {} >>",
                valid_bms.len()
            );
            obj(&mut out, &mut offsets, root.as_bytes());
            for (i, bm) in valid_bms.iter().enumerate() {
                let item_id = first_item + i;
                let dest_page = page_ids[bm.page_index];
                let mut dict = format!(
                    "<< /Title {} /Parent {outlines_base} 0 R /Dest [{dest_page} 0 R /Fit]",
                    pdf_utf16be_hex(&bm.title)
                );
                if i > 0 {
                    let _ = write!(dict, " /Prev {} 0 R", item_id - 1);
                }
                if i + 1 < valid_bms.len() {
                    let _ = write!(dict, " /Next {} 0 R", item_id + 1);
                }
                dict.push_str(" >>");
                obj(&mut out, &mut offsets, dict.as_bytes());
            }
        }

        // Info 元信息字典
        let mut info = String::from("<< /Producer ");
        info.push_str(&pdf_utf16be_hex("CELLAN Vector PDF Engine"));
        if !self.meta.creator.is_empty() {
            info.push_str(" /Creator ");
            info.push_str(&pdf_utf16be_hex(&self.meta.creator));
        }
        if !self.meta.title.is_empty() {
            info.push_str(" /Title ");
            info.push_str(&pdf_utf16be_hex(&self.meta.title));
        }
        if !self.meta.author.is_empty() {
            info.push_str(" /Author ");
            info.push_str(&pdf_utf16be_hex(&self.meta.author));
        }
        if !self.meta.subject.is_empty() {
            info.push_str(" /Subject ");
            info.push_str(&pdf_utf16be_hex(&self.meta.subject));
        }
        info.push_str(" >>");
        obj(&mut out, &mut offsets, info.as_bytes());

        let xref_at = out.len();
        let count = offsets.len() + 1;
        let _ = std::io::Write::write_fmt(
            &mut out,
            format_args!("xref\n0 {count}\n0000000000 65535 f \n"),
        );
        for off in offsets.iter() {
            let _ = std::io::Write::write_fmt(&mut out, format_args!("{off:010} 00000 n \n"));
        }
        let _ = std::io::Write::write_fmt(
            &mut out,
            format_args!(
                "trailer\n<< /Size {count} /Root 1 0 R /Info {info_id} 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"
            ),
        );
        std::fs::write(path, out)?;
        Ok(())
    }
}
