//! 最小 PDF 生成器：够画表格就行。
//!
//! - 中文字体走 TrueType 嵌入（CIDFontType2 + Identity-H），文字用字形号下发，
//!   所以不依赖阅读器里有没有装字体。字体文件整份嵌入（不做子集），实现简单、
//!   不会出现「某些字突然变方框」。
//! - 内容流用路径（矩形/线）与文字两种原语，够画网格、填色、单元格文字、页眉页脚。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// 嵌入的字体
pub struct Font {
    data: Vec<u8>,
    /// 字符 → 字形号
    cmap: HashMap<char, u16>,
    /// 字形号 → 宽度（1/1000 em）
    widths: Vec<u16>,
    pub units_per_em: u16,
    ascender: i16,
    descender: i16,
    bbox: [i16; 4],
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

impl Font {
    /// 从 TTF 读出来（不支持 TTC 集合，调用方传单体字体）
    pub fn load(path: &Path) -> Result<Font> {
        let data = std::fs::read(path).with_context(|| format!("读字体失败 {}", path.display()))?;
        Font::parse(data)
    }

    /// 从内存里的 TTF 字节解析（内嵌子集字体走这条）
    pub fn parse(data: Vec<u8>) -> Result<Font> {
        anyhow::ensure!(data.len() > 12, "字体文件太小");
        anyhow::ensure!(
            &data[0..4] == b"\x00\x01\x00\x00" || &data[0..4] == b"true",
            "不是 TrueType 单体字体（TTC 请先拆开）"
        );
        let n_tables = be16(&data, 4) as usize;
        let mut tables: HashMap<[u8; 4], (usize, usize)> = HashMap::new();
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
            data,
            cmap,
            widths,
            units_per_em,
            ascender,
            descender,
            bbox: [x_min, y_min, x_max, y_max],
        })
    }

    pub fn gid(&self, ch: char) -> u16 {
        self.cmap.get(&ch).copied().unwrap_or(0)
    }

    pub fn width_of(&self, ch: char, size: f32) -> f32 {
        let g = self.gid(ch) as usize;
        let w = *self.widths.get(g).unwrap_or(&500) as f32;
        w / 1000.0 * size
    }

    pub fn text_width(&self, s: &str, size: f32) -> f32 {
        s.chars().map(|c| self.width_of(c, size)).sum()
    }

    /// 字形宽度（em 的千分比）；字体里没有这个字返回 None
    pub fn advance(&self, ch: char) -> Option<f32> {
        let g = *self.cmap.get(&ch)? as usize;
        if g == 0 {
            return None;
        }
        let w = *self.widths.get(g)? as f32;
        if w <= 0.0 {
            return None;
        }
        Some(w / 1000.0)
    }

    /// 把 cmap/hmtx 摘出来做成可共享的度量闭包（打印排版用，避免借用冲突）
    pub fn metric(&self) -> std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync> {
        let cmap = self.cmap.clone();
        let widths = self.widths.clone();
        std::sync::Arc::new(move |ch: char, size: f32| {
            let g = *cmap.get(&ch).unwrap_or(&0) as usize;
            let w = *widths.get(g).unwrap_or(&500) as f32;
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
}

impl Page {
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height, ops: String::new() }
    }

    pub fn rect_fill(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: [u8; 3]) {
        self.ops.push_str(&format!(
            "{:.3} {:.3} {:.3} rg {:.2} {:.2} {:.2} {:.2} re f\n",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
            x,
            y,
            w,
            h
        ));
    }

    pub fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, rgb: [u8; 3]) {
        self.ops.push_str(&format!(
            "{:.3} {:.3} {:.3} RG {w:.2} w {x1:.2} {y1:.2} m {x2:.2} {y2:.2} l S\n",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        ));
    }

    /// 文字：`hex` 是各字符字形号的 2 字节大端拼接
    pub fn text(&mut self, x: f32, baseline: f32, size: f32, hex: &str, rgb: [u8; 3]) {
        self.ops.push_str(&format!(
            "BT {:.3} {:.3} {:.3} rg /F1 {size:.2} Tf 1 0 0 1 {x:.2} {baseline:.2} Tm <{hex}> Tj ET\n",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        ));
    }

    /// 带样式的文字：粗体用描边模拟，斜体是文本矩阵错切，rotation 为逆时针角度。
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
        let rad = rotation.to_radians();
        let (sin, cos) = (rad.sin(), rad.cos());
        let skew = if italic { 0.21 } else { 0.0 };
        // [a b c d e f]：c 同时承担旋转的第二基与斜体错切
        let a = cos;
        let b = sin;
        let c = -sin + skew * cos;
        let d = cos;
        let stroke = if bold {
            format!("{:.3} {:.3} {:.3} RG {:.3} w 2 Tr ", rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, size * 0.035)
        } else {
            String::new()
        };
        self.ops.push_str(&format!(
            "BT {:.3} {:.3} {:.3} rg {stroke}/F1 {size:.2} Tf {a:.4} {b:.4} {c:.4} {d:.4} {x:.2} {baseline:.2} Tm <{hex}> Tj ET
",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0
        ));
    }

    /// 画一张已注册的图（名字来自 `Doc::add_image`）
    pub fn image(&mut self, name: &str, x: f32, y: f32, w: f32, h: f32) {
        self.ops.push_str(&format!(
            "q {w:.2} 0 0 {h:.2} {x:.2} {y:.2} cm /{name} Do Q
"
        ));
    }

    /// 任意折线/多边形：闭合时用于饼图扇区填充
    pub fn poly(&mut self, pts: &[(f32, f32)], rgb: [u8; 3], fill: bool) {
        if pts.len() < 2 {
            return;
        }
        self.ops.push_str(&format!(
            "{:.3} {:.3} {:.3} rg 0 0 0 RG {:.2} {:.2} m ",
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
            pts[0].0,
            pts[0].1
        ));
        for p in &pts[1..] {
            self.ops.push_str(&format!("{:.2} {:.2} l ", p.0, p.1));
        }
        self.ops.push_str(if fill { "h f
" } else { "S
" });
    }

    /// 把字符串转成字形号 hex
    pub fn glyph_hex(font: &Font, s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 4);
        for ch in s.chars() {
            out.push_str(&format!("{:04X}", font.gid(ch)));
        }
        out
    }
}

/// 一份 PDF
pub struct Doc {
    pub pages: Vec<Page>,
    font: Font,
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
        Self { pages: Vec::new(), font, images: Vec::new() }
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
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        let mut offsets: Vec<usize> = Vec::new();
        let mut obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: Vec<u8>| {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(b"\nendobj\n");
        };

        // 1 catalog, 2 pages, 3 Type0, 4 CIDFont, 5 descriptor, 6 fontfile,
        // 7 ToUnicode；页面对象从 8 开始。
        let n_pages = self.pages.len().max(1);
        let page_ids: Vec<usize> = (0..n_pages).map(|i| 8 + i * 2).collect();
        let kids: Vec<String> = page_ids.iter().map(|id| format!("{id} 0 R")).collect();

        obj(
            &mut out,
            &mut offsets,
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        );
        obj(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Pages /Count {n_pages} /Kids [{}] >>",
                kids.join(" ")
            )
            .into_bytes(),
        );
        let upm = self.font.units_per_em.max(1) as f32;
        obj(
            &mut out,
            &mut offsets,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /Embedded /Encoding /Identity-H /DescendantFonts [4 0 R] /ToUnicode 7 0 R >>".to_vec(),
        );
        // W 数组：所有字形宽度
        let mut w = String::from("[ 0 [");
        for x in self.font.widths.iter() {
            w.push_str(&format!("{x} "));
        }
        w.push_str("] ]");
        obj(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Embedded /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 5 0 R /DW 1000 /W {w} /CIDToGIDMap /Identity >>"
            )
            .into_bytes(),
        );
        obj(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /FontDescriptor /FontName /Embedded /Flags 4 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {:.0} /Descent {:.0} /CapHeight {:.0} /StemV 80 /FontFile2 6 0 R >>",
                self.font.bbox[0] as f32 * 1000.0 / upm,
                self.font.bbox[1] as f32 * 1000.0 / upm,
                self.font.bbox[2] as f32 * 1000.0 / upm,
                self.font.bbox[3] as f32 * 1000.0 / upm,
                self.font.ascender as f32 * 1000.0 / upm,
                self.font.descender as f32 * 1000.0 / upm,
                self.font.ascender as f32 * 1000.0 / upm * 0.7,
            )
            .into_bytes(),
        );
        {
            let mut body: Vec<u8> = format!(
                "<< /Length {} /Length1 {} >>\nstream\n",
                self.font.data.len(),
                self.font.data.len()
            )
            .into_bytes();
            body.extend_from_slice(&self.font.data);
            body.extend_from_slice(b"\nendstream");
            obj(&mut out, &mut offsets, body);
        }

        // ToUnicode：让 pdftotext / 辅助技术能读回字形对应的 Unicode。
        // 没有它，Identity-H 字形号只能被当成乱码。
        let mut gid_to_char: Vec<(u16, char)> = self
            .font
            .cmap
            .iter()
            .map(|(ch, gid)| (*gid, *ch))
            .filter(|(_, ch)| !ch.is_control())
            .collect();
        gid_to_char.sort_unstable_by_key(|(gid, _)| *gid);
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        for chunk in gid_to_char.chunks(100) {
            cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
            for (gid, ch) in chunk {
                let mut buf = [0u16; 2];
                let encoded = ch.encode_utf16(&mut buf);
                let hex: String = encoded.iter().map(|u| format!("{u:04X}")).collect();
                cmap.push_str(&format!("<{gid:04X}> <{hex}>\n"));
            }
            cmap.push_str("endbfchar\n");
        }
        cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        {
            let mut body = format!("<< /Length {} >>\nstream\n", cmap.len()).into_bytes();
            body.extend_from_slice(cmap.as_bytes());
            body.extend_from_slice(b"endstream");
            obj(&mut out, &mut offsets, body);
        }

        let image_base = 8 + n_pages * 2;
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
        for (i, p) in self.pages.iter().enumerate() {
            let content_id = page_ids[i] + 1;
            let body = format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << /F1 3 0 R >>{xobjects} >> /Contents {content_id} 0 R >>",
                p.width, p.height
            );
            obj(&mut out, &mut offsets, body.into_bytes());
            let stream = p.ops.as_bytes();
            let mut c = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
            c.extend_from_slice(stream);
            c.extend_from_slice(b"endstream");
            obj(&mut out, &mut offsets, c);
        }
        if self.pages.is_empty() {
            // 至少给一张空白页，避免生成非法 PDF
            let body = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> >>".to_vec();
            obj(&mut out, &mut offsets, body);
        }

        for im in self.images.iter() {
            let filter = if im.dct {
                "/Filter /DCTDecode".to_string()
            } else {
                "/Filter /FlateDecode".to_string()
            };
            let cs = if im.colors == 1 { "/DeviceGray" } else { "/DeviceRGB" };
            let mut body = format!(
                "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {cs} /BitsPerComponent 8 {filter} /Length {} >>
stream
",
                im.width,
                im.height,
                im.data.len()
            )
            .into_bytes();
            body.extend_from_slice(&im.data);
            body.extend_from_slice(b"
endstream");
            obj(&mut out, &mut offsets, body);
        }

        let xref_at = out.len();
        let count = offsets.len() + 1;
        out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in offsets.iter() {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"
            )
            .as_bytes(),
        );
        std::fs::write(path, out)?;
        Ok(())
    }
}
