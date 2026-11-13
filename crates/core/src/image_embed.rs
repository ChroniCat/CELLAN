//! 把 PNG/JPEG 变成 PDF 能直接嵌入的 XObject 数据。
//!
//! - JPEG：原始字节直接 `DCTDecode`，只解析 SOF 取宽高与分量数；
//! - PNG：用 flate2 解 IDAT，按 PNG 的 5 种过滤器还原扫描线，输出 RGB/灰度原始字节，
//!   再压缩成 `FlateDecode`；RGBA 按白底合成（PDF 这条路径不带 alpha）。

use crate::pdf::PdfImage;
use std::io::Read;

pub fn make(name: &str, ext: &str, bytes: &[u8]) -> Option<PdfImage> {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => jpeg(name, bytes),
        "png" => png(name, bytes),
        _ => None,
    }
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
        let marker = b[i + 1];
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = ((b[i + 2] as usize) << 8) | b[i + 3] as usize;
        // SOF0..SOF15，排除 DHT(C4)/JPG(C8)/DAC(CC)
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let h = ((b[i + 5] as u32) << 8) | b[i + 6] as u32;
            let w = ((b[i + 7] as u32) << 8) | b[i + 8] as u32;
            let comps = b[i + 9];
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
    if b.len() < 33 || &b[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8;
    let (mut w, mut h, mut depth, mut ctype) = (0u32, 0u32, 0u8, 0u8);
    let mut idat: Vec<u8> = Vec::new();
    while i + 8 <= b.len() {
        let len = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
        let tag = &b[i + 4..i + 8];
        let data = &b[i + 8..(i + 8 + len).min(b.len())];
        match tag {
            b"IHDR" if data.len() >= 13 => {
                w = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                h = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                depth = data[8];
                ctype = data[9];
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    if depth != 8 || !matches!(ctype, 0 | 2 | 4 | 6) || w == 0 || h == 0 {
        return None;
    }
    let channels = match ctype {
        0 => 1usize,
        4 => 2,
        2 => 3,
        _ => 4,
    };
    let mut z = flate2::read::ZlibDecoder::new(&idat[..]);
    let mut raw = Vec::new();
    z.read_to_end(&mut raw).ok()?;
    let stride = w as usize * channels;
    if raw.len() < (stride + 1) * h as usize {
        return None;
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 3);
    let mut prev = vec![0u8; stride];
    for row in 0..h as usize {
        let base = row * (stride + 1);
        let filter = raw[base];
        let line = &raw[base + 1..base + 1 + stride];
        let mut cur = vec![0u8; stride];
        for x in 0..stride {
            let a = if x >= channels { cur[x - channels] } else { 0 };
            let bb = prev[x];
            let c = if x >= channels { prev[x - channels] } else { 0 };
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
        for px in cur.chunks_exact(channels) {
            match channels {
                1 | 2 => {
                    out.extend_from_slice(&[px[0], px[0], px[0]]);
                }
                3 => out.extend_from_slice(&px[0..3]),
                _ => {
                    // alpha 合成到白底
                    let a = px[3] as u16;
                    for k in 0..3 {
                        let v = (px[k] as u16 * a + 255 * (255 - a)) / 255;
                        out.push(v as u8);
                    }
                }
            }
        }
        prev = cur;
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut enc, &out).ok()?;
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
