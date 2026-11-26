//! 通用零依赖位图解码器（供 PDF 嵌入与矢量打印通道使用）：
//! - JPEG：原始字节零拷贝直通 `DCTDecode`（自动解析 SOF0..SOF15 宽高与色彩分量）；
//! - PNG：完整支持全部 5 种色彩类型（灰度/RGB/索引调色板 PLTE+tRNS/灰度+Alpha/RGBA）、
//!   1/2/4/8/16 位色深、全部 5 种滤波与 Adam7 隔行扫描；
//! - BMP：支持 24-bit BGR、32-bit BGRA（白底合成）、8-bit 调色板、正/倒序扫描线；
//! - GIF：纯 Rust LZW 解码器（GIF87a/89a 全局/局部调色板、隔行扫描、透明色白底合成）；
//! - 自动按魔数探测格式，避免工作簿里扩展名误标导致丢图。

use crate::pdf::PdfImage;
use std::io::Read;

pub fn make(name: &str, ext: &str, bytes: &[u8]) -> Option<PdfImage> {
    // 优先按魔数嗅探，容错扩展名不符的文件
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        return png(name, bytes);
    }
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        return jpeg(name, bytes);
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return gif(name, bytes);
    }
    if bytes.len() >= 26 && &bytes[..2] == b"BM" {
        return bmp(name, bytes);
    }
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => jpeg(name, bytes),
        "png" => png(name, bytes),
        "bmp" | "dib" => bmp(name, bytes),
        "gif" => gif(name, bytes),
        _ => None,
    }
}

/// 按归一化裁剪比例 `[left, top, right, bottom]` 裁剪 RGB8 像素数据，物理移除被裁掉的像素。
pub fn crop_rgb8(w: u32, h: u32, rgb: &[u8], crop: [f32; 4]) -> (u32, u32, Vec<u8>) {
    if w == 0 || h == 0 || rgb.len() < (w as usize) * (h as usize) * 3 {
        return (w, h, rgb.to_vec());
    }
    let u0 = crop[0].clamp(0.0, 0.9);
    let v0 = crop[1].clamp(0.0, 0.9);
    let u1 = (1.0 - crop[2]).clamp((u0 + 0.02).min(1.0), 1.0);
    let v1 = (1.0 - crop[3]).clamp((v0 + 0.02).min(1.0), 1.0);
    let x0 = ((u0 * w as f32).round() as u32).min(w.saturating_sub(1));
    let x1 = ((u1 * w as f32).round() as u32).clamp(x0 + 1, w);
    let y0 = ((v0 * h as f32).round() as u32).min(h.saturating_sub(1));
    let y1 = ((v1 * h as f32).round() as u32).clamp(y0 + 1, h);
    let cw = x1 - x0;
    let ch = y1 - y0;
    let mut out = Vec::with_capacity((cw as usize) * (ch as usize) * 3);
    let stride = w as usize * 3;
    let row_start = x0 as usize * 3;
    let row_end = x1 as usize * 3;
    for y in y0..y1 {
        let base = y as usize * stride;
        out.extend_from_slice(&rgb[base + row_start..base + row_end]);
    }
    (cw, ch, out)
}

/// 生成供 PDF 嵌入的图像对象；若带裁剪且格式可像素解码（PNG/BMP/GIF），直接物理裁掉被裁区域并返回 `baked_crop = true`。
/// 对 JPEG 直通流返回 `baked_crop = false`，由 PDF 裁剪路径 + 变换矩阵在渲染层精确裁切。
pub fn make_with_crop(name: &str, ext: &str, bytes: &[u8], crop: [f32; 4]) -> Option<(PdfImage, bool)> {
    if crop.iter().any(|&v| v > 1e-4)
        && let Some((w, h, rgb)) = decode_rgb8(ext, bytes)
    {
        let (cw, ch, cropped) = crop_rgb8(w, h, &rgb, crop);
        return pack_rgb(name, cw, ch, &cropped).map(|img| (img, true));
    }
    make(name, ext, bytes).map(|img| (img, false))
}

/// 将任意常见图片解码为 RGB8（`w * h * 3`）供原生打印或软件渲染复用
pub fn decode_rgb8(ext: &str, bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        return decode_png_rgb(bytes);
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return decode_gif_rgb(bytes);
    }
    if bytes.len() >= 26 && &bytes[..2] == b"BM" {
        return decode_bmp_rgb(bytes);
    }
    match ext.to_ascii_lowercase().as_str() {
        "png" => decode_png_rgb(bytes),
        "bmp" | "dib" => decode_bmp_rgb(bytes),
        "gif" => decode_gif_rgb(bytes),
        _ => None,
    }
}

fn pack_rgb(name: &str, w: u32, h: u32, rgb: &[u8]) -> Option<PdfImage> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut enc, rgb).ok()?;
    let data = enc.finish().ok()?;
    Some(PdfImage {
        name: name.to_string(),
        width: w,
        height: h,
        colors: 3,
        dct: false,
        data,
    })
}

fn jpeg(name: &str, b: &[u8]) -> Option<PdfImage> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        while i + 1 < b.len() && b[i + 1] == 0xFF {
            i += 1;
        }
        if i + 1 >= b.len() {
            break;
        }
        let marker = b[i + 1];
        if marker == 0xD8 || marker == 0xD9 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if i + 4 > b.len() {
            break;
        }
        let len = ((b[i + 2] as usize) << 8) | b[i + 3] as usize;
        if len < 2 {
            break;
        }
        // SOF0..SOF15，排除 DHT(C4)/JPG(C8)/DAC(CC)
        if (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC
            && i + 9 < b.len()
        {
            let h = ((b[i + 5] as u32) << 8) | b[i + 6] as u32;
            let w = ((b[i + 7] as u32) << 8) | b[i + 8] as u32;
            let comps = b[i + 9];
            if w == 0 || h == 0 {
                return None;
            }
            let colors = if comps == 1 { 1 } else { 3 };
            return Some(PdfImage {
                name: name.to_string(),
                width: w,
                height: h,
                colors,
                dct: true,
                data: b.to_vec(),
            });
        }
        i += 2 + len;
    }
    None
}

fn png(name: &str, b: &[u8]) -> Option<PdfImage> {
    let (w, h, rgb) = decode_png_rgb(b)?;
    pack_rgb(name, w, h, &rgb)
}

fn blend_white(c: u8, alpha: u8) -> u8 {
    let a = alpha as u16;
    ((c as u16 * a + 255 * (255 - a)) / 255) as u8
}

fn decode_png_rgb(b: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if b.len() < 33 || &b[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8;
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0u32, 0u32, 0u8, 0u8, 0u8);
    let mut idat: Vec<u8> = Vec::new();
    let mut plte: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();

    while i + 8 <= b.len() {
        let len = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
        let tag = &b[i + 4..i + 8];
        let start = i + 8;
        let end = start.checked_add(len)?.min(b.len());
        let data = &b[start..end];
        match tag {
            b"IHDR" if data.len() >= 13 => {
                w = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                h = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                depth = data[8];
                ctype = data[9];
                interlace = data[12];
            }
            b"PLTE" => {
                for ch in data.chunks_exact(3) {
                    plte.push([ch[0], ch[1], ch[2]]);
                }
            }
            b"tRNS" => {
                trns.extend_from_slice(data);
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        i = start.checked_add(len)?.checked_add(4)?;
    }
    if w == 0 || h == 0 || (w as u64 * h as u64) > 64_000_000 {
        return None;
    }
    if !matches!(depth, 1 | 2 | 4 | 8 | 16) || !matches!(ctype, 0 | 2 | 3 | 4 | 6) {
        return None;
    }
    if ctype == 3 && plte.is_empty() {
        return None;
    }

    let mut z = flate2::read::ZlibDecoder::new(&idat[..]);
    let mut raw = Vec::new();
    z.read_to_end(&mut raw).ok()?;

    let spp: usize = match ctype {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        6 => 4,
        _ => return None,
    };
    let bpp = ((spp * depth as usize) + 7) / 8;

    let mut out = vec![255u8; w as usize * h as usize * 3];

    let decode_pass = |raw_slice: &[u8],
                       pass_w: usize,
                       pass_h: usize,
                       x0: usize,
                       y0: usize,
                       dx: usize,
                       dy: usize,
                       out: &mut [u8]|
     -> Option<usize> {
        if pass_w == 0 || pass_h == 0 {
            return Some(0);
        }
        let stride = (pass_w * spp * depth as usize + 7) / 8;
        let needed = (stride + 1) * pass_h;
        if raw_slice.len() < needed {
            return None;
        }
        let mut prev = vec![0u8; stride];
        let mut cur = vec![0u8; stride];
        for row in 0..pass_h {
            let base = row * (stride + 1);
            let filter = raw_slice[base];
            let line = &raw_slice[base + 1..base + 1 + stride];
            for x in 0..stride {
                let a = if x >= bpp { cur[x - bpp] } else { 0 };
                let bb = prev[x];
                let c = if x >= bpp { prev[x - bpp] } else { 0 };
                let v = line[x];
                cur[x] = match filter {
                    0 => v,
                    1 => v.wrapping_add(a),
                    2 => v.wrapping_add(bb),
                    3 => v.wrapping_add(((a as u16 + bb as u16) / 2) as u8),
                    4 => v.wrapping_add(paeth(a, bb, c)),
                    _ => return None,
                };
            }
            let dy_pos = y0 + row * dy;
            for col in 0..pass_w {
                let dx_pos = x0 + col * dx;
                let (r, g, b_ch) =
                    extract_png_pixel(&cur, col, depth, ctype, &plte, &trns);
                let dst = (dy_pos * w as usize + dx_pos) * 3;
                out[dst] = r;
                out[dst + 1] = g;
                out[dst + 2] = b_ch;
            }
            std::mem::swap(&mut prev, &mut cur);
        }
        Some(needed)
    };

    if interlace == 0 {
        decode_pass(&raw, w as usize, h as usize, 0, 0, 1, 1, &mut out)?;
    } else if interlace == 1 {
        const ADAM7: [(usize, usize, usize, usize); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        let mut pos = 0usize;
        for &(x0, y0, dx, dy) in &ADAM7 {
            let pw = if w as usize > x0 {
                (w as usize - x0 + dx - 1) / dx
            } else {
                0
            };
            let ph = if h as usize > y0 {
                (h as usize - y0 + dy - 1) / dy
            } else {
                0
            };
            let consumed = decode_pass(&raw[pos..], pw, ph, x0, y0, dx, dy, &mut out)?;
            pos += consumed;
        }
    } else {
        return None;
    }
    Some((w, h, out))
}

fn extract_png_pixel(
    row: &[u8],
    col: usize,
    depth: u8,
    ctype: u8,
    plte: &[[u8; 3]],
    trns: &[u8],
) -> (u8, u8, u8) {
    match ctype {
        3 => {
            // 索引调色板（支持 1/2/4/8 位）
            let idx = sample_subbyte(row, col, depth) as usize;
            let [r, g, b] = plte.get(idx).copied().unwrap_or([255, 255, 255]);
            let a = trns.get(idx).copied().unwrap_or(255);
            if a < 255 {
                (blend_white(r, a), blend_white(g, a), blend_white(b, a))
            } else {
                (r, g, b)
            }
        }
        0 => {
            // 灰度（1/2/4/8/16 位）
            let v = if depth == 16 {
                row.get(col * 2).copied().unwrap_or(0)
            } else if depth == 8 {
                row.get(col).copied().unwrap_or(0)
            } else {
                let raw = sample_subbyte(row, col, depth);
                let max = (1u16 << depth) - 1;
                ((raw as u16 * 255) / max) as u8
            };
            if trns.len() >= 2 && depth == 8 && v == trns[1] {
                (255, 255, 255)
            } else {
                (v, v, v)
            }
        }
        4 => {
            // 灰度 + Alpha（8/16 位）
            let (g, a) = if depth == 16 {
                (
                    row.get(col * 4).copied().unwrap_or(0),
                    row.get(col * 4 + 2).copied().unwrap_or(255),
                )
            } else {
                (
                    row.get(col * 2).copied().unwrap_or(0),
                    row.get(col * 2 + 1).copied().unwrap_or(255),
                )
            };
            let v = blend_white(g, a);
            (v, v, v)
        }
        2 => {
            // RGB（8/16 位）
            if depth == 16 {
                let off = col * 6;
                (
                    row.get(off).copied().unwrap_or(0),
                    row.get(off + 2).copied().unwrap_or(0),
                    row.get(off + 4).copied().unwrap_or(0),
                )
            } else {
                let off = col * 3;
                let (r, g, b) = (
                    row.get(off).copied().unwrap_or(0),
                    row.get(off + 1).copied().unwrap_or(0),
                    row.get(off + 2).copied().unwrap_or(0),
                );
                if trns.len() >= 6 && r == trns[1] && g == trns[3] && b == trns[5] {
                    (255, 255, 255)
                } else {
                    (r, g, b)
                }
            }
        }
        6 => {
            // RGBA（8/16 位）
            let (r, g, b, a) = if depth == 16 {
                let off = col * 8;
                (
                    row.get(off).copied().unwrap_or(0),
                    row.get(off + 2).copied().unwrap_or(0),
                    row.get(off + 4).copied().unwrap_or(0),
                    row.get(off + 6).copied().unwrap_or(255),
                )
            } else {
                let off = col * 4;
                (
                    row.get(off).copied().unwrap_or(0),
                    row.get(off + 1).copied().unwrap_or(0),
                    row.get(off + 2).copied().unwrap_or(0),
                    row.get(off + 3).copied().unwrap_or(255),
                )
            };
            (blend_white(r, a), blend_white(g, a), blend_white(b, a))
        }
        _ => (255, 255, 255),
    }
}

#[inline]
fn sample_subbyte(row: &[u8], col: usize, depth: u8) -> u8 {
    match depth {
        8 => row.get(col).copied().unwrap_or(0),
        4 => {
            let byte = row.get(col / 2).copied().unwrap_or(0);
            if col % 2 == 0 { byte >> 4 } else { byte & 0x0F }
        }
        2 => {
            let byte = row.get(col / 4).copied().unwrap_or(0);
            (byte >> (6 - (col % 4) * 2)) & 0x03
        }
        1 => {
            let byte = row.get(col / 8).copied().unwrap_or(0);
            (byte >> (7 - (col % 8))) & 0x01
        }
        _ => 0,
    }
}

fn bmp(name: &str, b: &[u8]) -> Option<PdfImage> {
    let (w, h, rgb) = decode_bmp_rgb(b)?;
    pack_rgb(name, w, h, &rgb)
}

fn decode_bmp_rgb(b: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if b.len() < 54 || &b[..2] != b"BM" {
        return None;
    }
    let pixel_off = u32::from_le_bytes([b[10], b[11], b[12], b[13]]) as usize;
    let dib_size = u32::from_le_bytes([b[14], b[15], b[16], b[17]]) as usize;
    if dib_size < 40 || 14 + dib_size > b.len() {
        return None;
    }
    let w = i32::from_le_bytes([b[18], b[19], b[20], b[21]]);
    let h_signed = i32::from_le_bytes([b[22], b[23], b[24], b[25]]);
    let bpp = u16::from_le_bytes([b[28], b[29]]);
    let compression = u32::from_le_bytes([b[30], b[31], b[32], b[33]]);
    if w <= 0 || h_signed == 0 || (compression != 0 && compression != 3) {
        return None;
    }
    let (w, h) = (w as usize, h_signed.unsigned_abs() as usize);
    let top_down = h_signed < 0;
    if (w as u64 * h as u64) > 64_000_000 {
        return None;
    }

    let palette_off = 14 + dib_size;
    let row_stride = ((w * bpp as usize + 31) / 32) * 4;
    if pixel_off + row_stride * h > b.len() {
        return None;
    }
    let mut out = vec![255u8; w * h * 3];
    for y in 0..h {
        let src_y = if top_down { y } else { h - 1 - y };
        let row = &b[pixel_off + src_y * row_stride..pixel_off + (src_y + 1) * row_stride];
        for x in 0..w {
            let (r, g, b_ch) = match bpp {
                24 => (row[x * 3 + 2], row[x * 3 + 1], row[x * 3]),
                32 => {
                    let (bb, gg, rr, aa) = (row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]);
                    // 很多 32 位 BMP 的 alpha 字节全 0 表示不透明；若全 0 直接用 RGB
                    if aa == 0 {
                        (rr, gg, bb)
                    } else {
                        (blend_white(rr, aa), blend_white(gg, aa), blend_white(bb, aa))
                    }
                }
                8 => {
                    let idx = row[x] as usize;
                    let poff = palette_off + idx * 4;
                    if poff + 3 <= b.len() {
                        (b[poff + 2], b[poff + 1], b[poff])
                    } else {
                        (255, 255, 255)
                    }
                }
                _ => return None,
            };
            let dst = (y * w + x) * 3;
            out[dst] = r;
            out[dst + 1] = g;
            out[dst + 2] = b_ch;
        }
    }
    Some((w as u32, h as u32, out))
}

fn gif(name: &str, b: &[u8]) -> Option<PdfImage> {
    let (w, h, rgb) = decode_gif_rgb(b)?;
    pack_rgb(name, w, h, &rgb)
}

fn decode_gif_rgb(b: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if b.len() < 14 || (&b[..6] != b"GIF87a" && &b[..6] != b"GIF89a") {
        return None;
    }
    let canvas_w = u16::from_le_bytes([b[6], b[7]]) as usize;
    let canvas_h = u16::from_le_bytes([b[8], b[9]]) as usize;
    if canvas_w == 0 || canvas_h == 0 || (canvas_w * canvas_h) > 64_000_000 {
        return None;
    }
    let packed = b[10];
    let has_gct = (packed & 0x80) != 0;
    let gct_size = 1usize << ((packed & 0x07) + 1);
    let mut pos = 13usize;
    let gct: Vec<[u8; 3]> = if has_gct {
        if pos + gct_size * 3 > b.len() {
            return None;
        }
        let tbl: Vec<[u8; 3]> = b[pos..pos + gct_size * 3]
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        pos += gct_size * 3;
        tbl
    } else {
        Vec::new()
    };

    let mut transp_idx: Option<u8> = None;
    while pos < b.len() {
        match b[pos] {
            0x21 => {
                // 扩展块
                if pos + 2 >= b.len() {
                    break;
                }
                let label = b[pos + 1];
                pos += 2;
                if label == 0xF9 && pos + 5 <= b.len() && b[pos] == 4 {
                    let gce_flags = b[pos + 1];
                    if (gce_flags & 1) != 0 {
                        transp_idx = Some(b[pos + 4]);
                    }
                }
                // 跳过子块链
                while pos < b.len() {
                    let sz = b[pos] as usize;
                    pos += 1;
                    if sz == 0 {
                        break;
                    }
                    pos = pos.saturating_add(sz);
                }
            }
            0x2C => {
                // 图像描述符（解码首帧）
                if pos + 10 > b.len() {
                    return None;
                }
                let left = u16::from_le_bytes([b[pos + 1], b[pos + 2]]) as usize;
                let top = u16::from_le_bytes([b[pos + 3], b[pos + 4]]) as usize;
                let iw = u16::from_le_bytes([b[pos + 5], b[pos + 6]]) as usize;
                let ih = u16::from_le_bytes([b[pos + 7], b[pos + 8]]) as usize;
                let flags = b[pos + 9];
                pos += 10;
                let interlaced = (flags & 0x40) != 0;
                let lct: Option<Vec<[u8; 3]>> = if (flags & 0x80) != 0 {
                    let n = 1usize << ((flags & 0x07) + 1);
                    if pos + n * 3 > b.len() {
                        return None;
                    }
                    let t: Vec<[u8; 3]> = b[pos..pos + n * 3]
                        .chunks_exact(3)
                        .map(|c| [c[0], c[1], c[2]])
                        .collect();
                    pos += n * 3;
                    Some(t)
                } else {
                    None
                };
                let palette = lct.as_deref().unwrap_or(&gct);
                if pos >= b.len() {
                    return None;
                }
                let min_code_size = b[pos];
                pos += 1;
                let mut lzw_data = Vec::new();
                while pos < b.len() {
                    let sz = b[pos] as usize;
                    pos += 1;
                    if sz == 0 {
                        break;
                    }
                    let end = (pos + sz).min(b.len());
                    lzw_data.extend_from_slice(&b[pos..end]);
                    pos = end;
                }
                let indices = gif_lzw_decode(min_code_size, &lzw_data, iw * ih)?;
                let mut out = vec![255u8; canvas_w * canvas_h * 3];
                let mut row_map = Vec::with_capacity(ih);
                if interlaced {
                    for &(start, step) in &[(0, 8), (4, 8), (2, 4), (1, 2)] {
                        let mut y = start;
                        while y < ih {
                            row_map.push(y);
                            y += step;
                        }
                    }
                } else {
                    row_map.extend(0..ih);
                }
                for (src_row, &dst_rel_y) in row_map.iter().enumerate() {
                    let cy = top + dst_rel_y;
                    if cy >= canvas_h {
                        continue;
                    }
                    for x in 0..iw {
                        let cx = left + x;
                        if cx >= canvas_w {
                            continue;
                        }
                        let idx = indices.get(src_row * iw + x).copied().unwrap_or(0);
                        if transp_idx == Some(idx) {
                            continue;
                        }
                        let [r, g, bl] = palette.get(idx as usize).copied().unwrap_or([255, 255, 255]);
                        let dst = (cy * canvas_w + cx) * 3;
                        out[dst] = r;
                        out[dst + 1] = g;
                        out[dst + 2] = bl;
                    }
                }
                return Some((canvas_w as u32, canvas_h as u32, out));
            }
            0x3B => break,
            _ => pos += 1,
        }
    }
    None
}

fn gif_lzw_decode(min_code_size: u8, data: &[u8], expected: usize) -> Option<Vec<u8>> {
    if !(2..=8).contains(&min_code_size) {
        return None;
    }
    let clear_code = 1u16 << min_code_size;
    let eoi_code = clear_code + 1;
    let mut code_size = min_code_size as u32 + 1;
    let mut next_code = eoi_code + 1;
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    for i in 0..clear_code {
        suffix[i as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(expected);
    let mut stack = Vec::with_capacity(4096);
    let mut bit_buf: u32 = 0;
    let mut bits_in_buf: u32 = 0;
    let mut byte_idx = 0usize;
    let mut prev_code: Option<u16> = None;
    let mut first_char: u8 = 0;

    loop {
        while bits_in_buf < code_size && byte_idx < data.len() {
            bit_buf |= (data[byte_idx] as u32) << bits_in_buf;
            bits_in_buf += 8;
            byte_idx += 1;
        }
        if bits_in_buf < code_size {
            break;
        }
        let code = (bit_buf & ((1u32 << code_size) - 1)) as u16;
        bit_buf >>= code_size;
        bits_in_buf -= code_size;

        if code == clear_code {
            code_size = min_code_size as u32 + 1;
            next_code = eoi_code + 1;
            prev_code = None;
            continue;
        }
        if code == eoi_code {
            break;
        }
        let mut cur = code;
        stack.clear();
        if cur >= next_code {
            let Some(pc) = prev_code else { break };
            stack.push(first_char);
            cur = pc;
        }
        while cur >= clear_code {
            if stack.len() >= 4096 {
                return None;
            }
            stack.push(suffix[cur as usize]);
            cur = prefix[cur as usize];
        }
        first_char = suffix[cur as usize];
        stack.push(first_char);
        for &b in stack.iter().rev() {
            out.push(b);
            if out.len() >= expected {
                return Some(out);
            }
        }
        if let Some(pc) = prev_code
            && next_code < 4096
        {
            prefix[next_code as usize] = pc;
            suffix[next_code as usize] = first_char;
            next_code += 1;
            if next_code == (1u16 << code_size) && code_size < 12 {
                code_size += 1;
            }
        }
        prev_code = Some(code);
    }
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}
