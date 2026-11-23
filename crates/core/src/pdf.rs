//! 最小 PDF 生成器：够画表格就行。
//!
//! - 中文字体走 TrueType 嵌入（CIDFontType2 + Identity-H），文字用字形号下发，
//!   所以不依赖阅读器里有没有装字体。字体文件整份嵌入（不做子集），实现简单、
//!   不会出现「某些字突然变方框」。
//! - 内容流用路径（矩形/线）与文字两种原语，够画网格、填色、单元格文字、页眉页脚。

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
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
    /// 字符 → 字形号
    cmap: HashMap<char, u16>,
    /// 字形号 → 宽度（1/1000 em）
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

impl Font {
    /// 从 TTF 读出来（不支持 TTC 集合，调用方传单体字体）
    pub fn load(path: &Path) -> Result<Font> {
        let data = std::fs::read(path).with_context(|| format!("读字体失败 {}", path.display()))?;
        Font::parse(data)
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
        let parsed = Self::parse_bytes(FontBytes::Static(data))?;
        if let Ok(mut guard) = cache.lock() {
            guard.insert(key, parsed.clone());
        }
        Ok(parsed)
    }

    /// 从内存里的 TTF 字节解析（内嵌子集字体走这条）
    pub fn parse(data: Vec<u8>) -> Result<Font> {
        Self::parse_bytes(FontBytes::Owned(data))
    }

    fn parse_bytes(data: FontBytes) -> Result<Font> {
        anyhow::ensure!(data.len() > 12, "字体文件太小");
        anyhow::ensure!(
            &data[0..4] == b"\x00\x01\x00\x00" || &data[0..4] == b"true",
            "不是 TrueType 单体字体（TTC 请先拆开）"
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
            let adv = be16(&data, hmtx + idx * 4);
            widths[gid] = ((adv as u32 * 1000) / units_per_em.max(1) as u32) as u16;
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

    /// 字体自身的纵向度量（in em）：ascender、descender（都取正值）。
    /// 行高/基线要按它算，不能拿「字号 × 常数」估。
    pub fn em_metrics(&self) -> (f32, f32) {
        let upm = self.units_per_em.max(1) as f32;
        (self.inner.ascender as f32 / upm, (-self.inner.descender as f32) / upm)
    }

    pub fn gid(&self, ch: char) -> u16 {
        self.inner.cmap.get(&ch).copied().unwrap_or(0)
    }

    pub fn width_of(&self, ch: char, size: f32) -> f32 {
        let g = self.gid(ch) as usize;
        let w = *self.inner.widths.get(g).unwrap_or(&500) as f32;
        w / 1000.0 * size
    }

    pub fn text_width(&self, s: &str, size: f32) -> f32 {
        s.chars().map(|c| self.width_of(c, size)).sum()
    }

    /// 字形宽度（em 的千分比）；字体里没有这个字返回 None
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

    /// 把 cmap/hmtx 摘出来做成可共享的度量闭包（打印排版用，O(1) 共享内部 Arc）
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
    let n = be16(data, cmap_off + 2) as usize;
    let mut best: Option<usize> = None;
    let mut best_score = 0;
    for i in 0..n {
        let rec = cmap_off + 4 + i * 8;
        let platform = be16(data, rec);
        let encoding = be16(data, rec + 2);
        let sub = cmap_off + be32(data, rec + 4) as usize;
        let format = be16(data, sub);
        // 优先 Windows BMP / Unicode BMP；format 12 覆盖更广
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
            // format 4：BMP 分段映射
            let seg_x2 = be16(data, sub + 6) as usize;
            let seg = seg_x2 / 2;
            let end_base = sub + 14;
            let start_base = end_base + seg_x2 + 2;
            let delta_base = start_base + seg_x2;
            let range_base = delta_base + seg_x2;
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
                    if gid != 0 {
                        if let Some(ch) = char::from_u32(c) {
                            map.insert(ch, gid);
                        }
                    }
                }
            }
        }
        12 => {
            // format 12：分组
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
                    if let Some(ch) = char::from_u32(c) {
                        map.insert(ch, (start_gid + (c - start)) as u16);
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
    /// 本页用到的常规/粗体字形号（写 PDF 时按它裁字体）
    used: HashSet<u16>,
    used_bold: HashSet<u16>,
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

/// 内容流里的字形号是 4 位十六进制一串；记下来供字体裁剪
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

/// Flate 压缩（PDF 流）；压缩失败就原样返回，导出不能因此失败
fn compress_stream(data: &[u8]) -> (Vec<u8>, bool) {
    use std::io::Write;
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    if enc.write_all(data).is_ok() {
        if let Ok(v) = enc.finish() {
            return (v, true);
        }
    }
    (data.to_vec(), false)
}

/// 生成按实际使用字形裁剪的 ToUnicode CMap：消除全量 2 万字 CJK 映射的数百 KB 拼接开销，
/// 同时保留 pdftotext / 辅助技术提取全文所需的精确映射。
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

/// 按实际使用字形生成紧凑的 `/W` 宽度数组（连续 gid 自动合并区间），避免每份 PDF 格式化 2 万个宽度。
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

impl Page {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            ops: String::with_capacity(4096),
            used: HashSet::new(),
            used_bold: HashSet::new(),
        }
    }

    pub fn rect_fill(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: [u8; 3]) {
        let _ = writeln!(
            self.ops,
            "{:.3} {:.3} {:.3} rg {:.2} {:.2} {:.2} {:.2} re f",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
            x,
            y,
            w,
            h
        );
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

    /// 文字：`hex` 是各字符字形号的 2 字节大端拼接
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

    /// 带样式的文字：斜体是文本矩阵错切，rotation 为逆时针角度。
    /// `face` 是字体资源名（"F1" 常规 / "F2" 真粗体）。
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
        note_glyphs(hex, if face == "F2" { &mut self.used_bold } else { &mut self.used });
        let rad = rotation.to_radians();
        let (sin, cos) = (rad.sin(), rad.cos());
        let skew = if italic { 0.21 } else { 0.0 };
        let a = cos;
        let b = sin;
        let c = -sin + skew * cos;
        let d = cos;
        let _ = writeln!(
            self.ops,
            "BT {:.3} {:.3} {:.3} rg /{face} {size:.2} Tf {a:.4} {b:.4} {c:.4} {d:.4} {x:.2} {baseline:.2} Tm <{hex}> Tj ET",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        );
    }

    /// 带样式的文字：斜体是文本矩阵错切，rotation 为逆时针角度。
    /// 兼容入口：粗体仍走描边（没有第二条字体时的退化路径）。
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
        note_glyphs(hex, &mut self.used);
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
        let _ = write!(self.ops, "BT {r0:.3} {g0:.3} {b0:.3} rg ");
        if bold {
            let _ = write!(self.ops, "{r0:.3} {g0:.3} {b0:.3} RG {:.3} w 2 Tr ", size * 0.035);
        }
        let _ = writeln!(
            self.ops,
            "/F1 {size:.2} Tf {a:.4} {b:.4} {c:.4} {d:.4} {x:.2} {baseline:.2} Tm <{hex}> Tj ET"
        );
    }

    /// 画一张已注册的图（名字来自 `Doc::add_image`）
    pub fn image(&mut self, name: &str, x: f32, y: f32, w: f32, h: f32) {
        let _ = writeln!(self.ops, "q {w:.2} 0 0 {h:.2} {x:.2} {y:.2} cm /{name} Do Q");
    }

    /// 任意折线/多边形：闭合时用于饼图扇区填充
    pub fn poly(&mut self, pts: &[(f32, f32)], rgb: [u8; 3], fill: bool) {
        if pts.len() < 2 {
            return;
        }
        let _ = write!(
            self.ops,
            "{:.3} {:.3} {:.3} rg 0 0 0 RG {:.2} {:.2} m ",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
            pts[0].0,
            pts[0].1
        );
        for p in &pts[1..] {
            let _ = write!(self.ops, "{:.2} {:.2} l ", p.0, p.1);
        }
        self.ops.push_str(if fill { "h f\n" } else { "S\n" });
    }

    /// 把字符串转成字形号 hex（零 format! 堆分配）
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

/// 一份 PDF
pub struct Doc {
    pub pages: Vec<Page>,
    font: Font,
    /// 真粗体字体（可选）。没有时 `text_styled` 仍用描边模拟。
    bold: Option<Font>,
    /// 已注册的图片：名字 → 要写进 PDF 的 XObject
    pub images: Vec<PdfImage>,
}

/// 一张要嵌进 PDF 的位图
#[derive(Clone, Debug)]
pub struct PdfImage {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// 1 = 灰度，3 = RGB
    pub colors: u8,
    /// DCT（JPEG 原始字节）或 Flate（压缩后的 RGB 原始字节）
    pub dct: bool,
    pub data: Vec<u8>,
}

impl Doc {
    pub fn new(font: Font) -> Self {
        Self { pages: Vec::new(), font, bold: None, images: Vec::new() }
    }

    /// 挂上真粗体字体：粗体文字改用 /F2 的真 700 字形，不再描边。
    pub fn set_bold_font(&mut self, font: Font) {
        self.bold = Some(font);
    }

    pub fn bold_font(&self) -> Option<&Font> {
        self.bold.as_ref()
    }

    /// 取实际下发字形号用的字体（粗体且挂了真粗体时用 bold）
    pub fn font_for(&self, bold: bool) -> &Font {
        match (bold, self.bold.as_ref()) {
            (true, Some(f)) => f,
            _ => &self.font,
        }
    }

    /// 注册一张图，返回内容流里用的名字（同名只注册一次）
    pub fn add_image(&mut self, img: PdfImage) -> String {
        if let Some(old) = self.images.iter().find(|x| x.name == img.name) {
            let _ = old;
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
        // 按实际用到的字形裁字体（gid 不变）。
        let mut used: HashSet<u16> = HashSet::new();
        let mut used_bold: HashSet<u16> = HashSet::new();
        for p in &self.pages {
            used.extend(p.used.iter().copied());
            used_bold.extend(p.used_bold.iter().copied());
        }
        let reg_bytes: &[u8] = &self.font.inner.data;
        let regular_sub = crate::font_subset::subset(reg_bytes, &used)
            .filter(|v| v.len() < reg_bytes.len());
        let regular_slice: &[u8] = regular_sub.as_deref().unwrap_or(reg_bytes);
        let bold_sub = self.bold.as_ref().and_then(|bf| {
            let b_bytes: &[u8] = &bf.inner.data;
            crate::font_subset::subset(b_bytes, &used_bold).filter(|v| v.len() < b_bytes.len())
        });
        let mut out: Vec<u8> = Vec::with_capacity(65_536 + self.pages.len() * 4096);
        out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        let mut offsets: Vec<usize> = Vec::with_capacity(16 + self.pages.len() * 2 + self.images.len());
        let mut obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
            offsets.push(out.len());
            let _ = std::io::Write::write_fmt(out, format_args!("{} 0 obj\n", offsets.len()));
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };

        // 1 catalog, 2 pages, 3 Type0, 4 CIDFont, 5 descriptor, 6 fontfile,
        // 7 ToUnicode；页面对象从 8 开始。
        let n_pages = self.pages.len().max(1);
        let page_ids: Vec<usize> = (0..n_pages).map(|i| 8 + i * 2).collect();
        let mut kids = String::with_capacity(n_pages * 8);
        for (idx, id) in page_ids.iter().enumerate() {
            if idx > 0 {
                kids.push(' ');
            }
            let _ = write!(kids, "{id} 0 R");
        }

        obj(
            &mut out,
            &mut offsets,
            b"<< /Type /Catalog /Pages 2 0 R >>",
        );
        obj(
            &mut out,
            &mut offsets,
            format!("<< /Type /Pages /Count {n_pages} /Kids [{kids}] >>").as_bytes(),
        );
        let upm = self.font.units_per_em.max(1) as f32;
        obj(
            &mut out,
            &mut offsets,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /Embedded /Encoding /Identity-H /DescendantFonts [4 0 R] /ToUnicode 7 0 R >>",
        );
        let w = build_widths_array(&self.font.inner.widths, &used);
        obj(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Embedded /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 5 0 R /DW 1000 /W {w} /CIDToGIDMap /Identity >>"
            )
            .as_bytes(),
        );
        obj(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /FontDescriptor /FontName /Embedded /Flags 4 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {:.0} /Descent {:.0} /CapHeight {:.0} /StemV 80 /FontFile2 6 0 R >>",
                self.font.inner.bbox[0] as f32 * 1000.0 / upm,
                self.font.inner.bbox[1] as f32 * 1000.0 / upm,
                self.font.inner.bbox[2] as f32 * 1000.0 / upm,
                self.font.inner.bbox[3] as f32 * 1000.0 / upm,
                self.font.inner.ascender as f32 * 1000.0 / upm,
                self.font.inner.descender as f32 * 1000.0 / upm,
                self.font.inner.ascender as f32 * 1000.0 / upm * 0.7,
            )
            .as_bytes(),
        );
        {
            let (packed, _) = compress_stream(regular_slice);
            let mut body: Vec<u8> = format!(
                "<< /Length {} /Length1 {} /Filter /FlateDecode >>\nstream\n",
                packed.len(),
                regular_slice.len()
            )
            .into_bytes();
            body.extend_from_slice(&packed);
            body.extend_from_slice(b"\nendstream");
            obj(&mut out, &mut offsets, &body);
        }

        let cmap = tounicode_cmap(&self.font, &used);
        {
            let mut body = format!("<< /Length {} >>\nstream\n", cmap.len()).into_bytes();
            body.extend_from_slice(cmap.as_bytes());
            body.extend_from_slice(b"endstream");
            obj(&mut out, &mut offsets, &body);
        }

        let image_base = 8 + n_pages * 2;
        // 粗体字体对象排在页面与图片之后；页面资源要提前知道它的对象号。
        // 常规字体固定占 1..7，每页 2 个对象，图片各 1 个，空文档还有 1 个占位页对象。
        let blank_page = usize::from(self.pages.is_empty());
        let bold_base = image_base + self.images.len() + blank_page;
        let xobjects: String = if self.images.is_empty() {
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
        let fonts_res = if self.bold.is_some() {
            format!("/Font << /F1 3 0 R /F2 {bold_base} 0 R >>")
        } else {
            "/Font << /F1 3 0 R >>".to_string()
        };
        for (i, p) in self.pages.iter().enumerate() {
            let content_id = page_ids[i] + 1;
            let body = format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << {fonts_res}{xobjects} >> /Contents {content_id} 0 R >>",
                p.width, p.height
            );
            obj(&mut out, &mut offsets, body.as_bytes());
            let stream = p.ops.as_bytes();
            // 大页内容流（>= 32KB）启用 Flate 压缩显著缩减多页大表导出体积与写盘耗时；
            // 小型单页保留明文流以兼容结构测试与快速直写。
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
            // 至少给一张空白页，避免生成非法 PDF
            let body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> >>";
            obj(&mut out, &mut offsets, body);
        }

        for im in self.images.iter() {
            let filter = if im.dct {
                "/Filter /DCTDecode"
            } else {
                "/Filter /FlateDecode"
            };
            let cs = if im.colors == 1 { "/DeviceGray" } else { "/DeviceRGB" };
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

        // 真粗体字体：与常规字体同样的 5 个对象（Type0 / CIDFont / 描述符 / FontFile2 / ToUnicode）
        if let Some(bf) = &self.bold {
            let id0 = bold_base;
            let upm = bf.units_per_em.max(1) as f32;
            obj(
                &mut out,
                &mut offsets,
                format!(
                    "<< /Type /Font /Subtype /Type0 /BaseFont /EmbeddedBold /Encoding /Identity-H /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
                    id0 + 1,
                    id0 + 4
                )
                .as_bytes(),
            );
            let w = build_widths_array(&bf.inner.widths, &used_bold);
            obj(
                &mut out,
                &mut offsets,
                format!(
                    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /EmbeddedBold /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {} 0 R /DW 1000 /W {w} /CIDToGIDMap /Identity >>",
                    id0 + 2
                )
                .as_bytes(),
            );
            obj(
                &mut out,
                &mut offsets,
                format!(
                    "<< /Type /FontDescriptor /FontName /EmbeddedBold /Flags 4 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {:.0} /Descent {:.0} /CapHeight {:.0} /StemV 140 /FontFile2 {} 0 R >>",
                    bf.inner.bbox[0] as f32 * 1000.0 / upm,
                    bf.inner.bbox[1] as f32 * 1000.0 / upm,
                    bf.inner.bbox[2] as f32 * 1000.0 / upm,
                    bf.inner.bbox[3] as f32 * 1000.0 / upm,
                    bf.inner.ascender as f32 * 1000.0 / upm,
                    bf.inner.descender as f32 * 1000.0 / upm,
                    bf.inner.ascender as f32 * 1000.0 / upm * 0.7,
                    id0 + 3
                )
                .as_bytes(),
            );
            let bold_slice: &[u8] = bold_sub.as_deref().unwrap_or(&bf.inner.data);
            let (packed, _) = compress_stream(bold_slice);
            let mut body: Vec<u8> = format!(
                "<< /Length {} /Length1 {} /Filter /FlateDecode >>\nstream\n",
                packed.len(),
                bold_slice.len()
            )
            .into_bytes();
            body.extend_from_slice(&packed);
            body.extend_from_slice(b"\nendstream");
            obj(&mut out, &mut offsets, &body);
            let cmap = tounicode_cmap(bf, &used_bold);
            let mut body = format!("<< /Length {} >>\nstream\n", cmap.len()).into_bytes();
            body.extend_from_slice(cmap.as_bytes());
            body.extend_from_slice(b"endstream");
            obj(&mut out, &mut offsets, &body);
        }

        let xref_at = out.len();
        let count = offsets.len() + 1;
        let _ = std::io::Write::write_fmt(&mut out, format_args!("xref\n0 {count}\n0000000000 65535 f \n"));
        for off in offsets.iter() {
            let _ = std::io::Write::write_fmt(&mut out, format_args!("{off:010} 00000 n \n"));
        }
        let _ = std::io::Write::write_fmt(
            &mut out,
            format_args!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"),
        );
        std::fs::write(path, out)?;
        Ok(())
    }
}
