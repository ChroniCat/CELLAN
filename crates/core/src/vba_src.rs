//! 从 vbaProject.bin 里把宏「读出来」——只读，不执行。
//!
//! 两级解码：
//! 1. OLE 复合文档（见 cfb.rs）
//! 2. MS-OVBA 的压缩容器（本文件）：每个 chunk 头两字节给出大小与是否压缩，
//!    未压缩就是 4096 字节原文；压缩的用「标志字节 + 8 个 token」的 LZ 变体。

use anyhow::{bail, Result};

use crate::cfb::Compound;

/// 解 MS-OVBA 压缩容器（算法对照 MS-OVBA 2.4.1.3 与 oletools 的实现）
///
/// 两个容易搞错的地方：
/// 1. CopyToken 是 **2 字节**，没有第三个字节；
/// 2. 长度/偏移的位宽不固定：`bits = max(4, ceil(log2(本 chunk 已解出字节数)))`，
///    长度占低 (16 - bits) 位，偏移占高 bits 位，且解出的距离要 +1。
pub fn decompress(src: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(src.len() * 4);
    // 规范要求首字节是 0x01；老的自造 fixture 没有，也容忍一下
    let mut i = if src.first() == Some(&0x01) { 1 } else { 0 };
    while i + 2 <= src.len() {
        let header = u16::from_le_bytes([src[i], src[i + 1]]);
        if (header >> 12) & 0x07 != 0b011 {
            break;
        }
        let chunk_size = (header & 0x0FFF) as usize + 3; // 含 2 字节头
        let compressed = (header >> 15) & 1 == 1;
        let start = i;
        let end = (start + chunk_size).min(src.len());
        i = start + 2;

        if !compressed {
            let take = (i + 4096).min(src.len());
            out.extend_from_slice(&src[i..take]);
            i = end;
            continue;
        }

        let chunk_start_out = out.len();
        while i < end {
            let flags = src[i];
            i += 1;
            for bit in 0..8 {
                if i >= end {
                    break;
                }
                if (flags >> bit) & 1 == 0 {
                    out.push(src[i]);
                    i += 1;
                } else {
                    if i + 2 > end {
                        i = end;
                        break;
                    }
                    let token = u16::from_le_bytes([src[i], src[i + 1]]);
                    i += 2;
                    let diff = out.len() - chunk_start_out;
                    if diff == 0 {
                        return out;
                    }
                    let bits = if diff <= 1 {
                        4usize
                    } else {
                        (32 - (diff as u32 - 1).leading_zeros()) as usize
                    }
                    .max(4);
                    let length_mask: u16 = 0xFFFFu16 >> bits;
                    let length = (token & length_mask) as usize + 3;
                    let offset = (((token & !length_mask) as usize) >> (16 - bits)) + 1;
                    if offset > out.len() {
                        return out;
                    }
                    let mut from = out.len() - offset;
                    for _ in 0..length {
                        let b = out[from];
                        out.push(b);
                        from += 1;
                    }
                }
            }
        }
        i = end;
    }
    out
}

/// 一个模块
#[derive(Clone, Debug, PartialEq)]
pub struct VbaModule {
    pub name: String,
    pub kind: String,
    pub source: String,
    pub lines: usize,
}

/// 读出所有模块（名字 + 源码）
pub fn read_modules(data: &[u8]) -> Result<Vec<VbaModule>> {
    let cfb = Compound::open(data.to_vec())?;
    let project = cfb
        .stream_by_path("PROJECT")
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();

    // PROJECT 里列了模块：Module=xxx / Class=xxx / Document=ThisWorkbook
    let mut listed: Vec<(String, String)> = Vec::new();
    for line in project.lines() {
        let t = line.trim();
        for (key, kind) in [
            ("Module=", "模块"),
            ("Class=", "类模块"),
            ("Document=", "工作表/工作簿"),
            ("BaseClass=", "类"),
            ("Form=", "窗体"),
        ] {
            if let Some(name) = t.strip_prefix(key) {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    listed.push((name, kind.to_string()));
                }
            }
        }
    }

    let dir = cfb
        .stream_by_path("VBA/dir")
        .map(|b| decompress(&b))
        .unwrap_or_default();
    if listed.is_empty() && dir.is_empty() {
        bail!("没有找到 VBA 工程内容（可能是空的 vbaProject.bin）");
    }

    // 目录流里也有模块名与源码偏移：MODULENAME(0x0019) / MODULEOFFSET(0x0031)
    let mut offsets: Vec<(String, u32)> = Vec::new();
    let mut i = 0usize;
    while i + 6 <= dir.len() {
        let id = u16::from_le_bytes([dir[i], dir[i + 1]]);
        let size = u32::from_le_bytes([dir[i + 2], dir[i + 3], dir[i + 4], dir[i + 5]]) as usize;
        let data_at = i + 6;
        if size > dir.len().saturating_sub(data_at) {
            break;
        }
        if id == 0x0019 && size > 0 {
            let name = String::from_utf8_lossy(&dir[data_at..data_at + size])
                .trim()
                .to_string();
            offsets.push((name, 0));
        } else if id == 0x0031 && size == 4 {
            let off = u32::from_le_bytes([
                dir[data_at],
                dir[data_at + 1],
                dir[data_at + 2],
                dir[data_at + 3],
            ]);
            if let Some(last) = offsets.last_mut() {
                last.1 = off;
            }
        }
        i = data_at + size;
    }
    if listed.is_empty() {
        listed = offsets.iter().map(|(n, _)| (n.clone(), "模块".to_string())).collect();
    }

    let mut out = Vec::new();
    for (name, kind) in listed {
        // dir 里解析出来的名字可能是「模块名/流名」，流名才是容器里的条目
        let (disp, stream_name) = match name.split_once('/') {
            Some((a, b)) => {
                let b = b.split("&H").next().unwrap_or(b).trim();
                (a.to_string(), b.to_string())
            }
            None => (name.clone(), name.clone()),
        };
        let raw = cfb
            .stream_by_path(&format!("VBA/{stream_name}"))
            .or_else(|| cfb.stream_by_path(&stream_name))
            .or_else(|| cfb.stream_by_path(&format!("VBA/{disp}")))
            .or_else(|| cfb.stream_by_path(&disp));
        let mut usable = true;
        let source = match raw {
            Some(bytes) => {
                let off = offsets
                    .iter()
                    .find(|(n, _)| n == &name || n == &disp || n == &stream_name)
                    .map(|(_, o)| *o as usize)
                    .unwrap_or(0);
                let slice = if off > 0 && off < bytes.len() {
                    &bytes[off..]
                } else {
                    &bytes[..]
                };
                // 真文件的模块流前面有 PerformanceCache，源码容器不一定从 MODULEOFFSET 开始；
                // 先按记录的偏移试，签名不对就在流里找第一个合法压缩块
                let slice = if is_container(slice) {
                    slice
                } else {
                    match find_container(&bytes) {
                        Some(p) => &bytes[p..],
                        None => slice,
                    }
                };
                let text = decompress(slice);
                let s = String::from_utf8_lossy(&text).to_string();
                // 真实文件里如果压缩 token 解错，会出来一堆控制字符/重复串；
                // 宁可说"解不出来"，也不要把乱码当源码显示出去
                // 去掉压缩容器前面可能带的版本字节
                let clean = s
                    .trim_start_matches(char::from(1))
                    .trim_start_matches('\u{feff}')
                    .to_string();
                // Excel 的模块流把 Attribute VB_Name 行存了两遍，显示时去掉一行
                let mut clean = clean;
                let head: Vec<&str> = clean.lines().take(2).collect();
                if head.len() == 2 && head[0] == head[1] && head[0].starts_with("Attribute VB_Name") {
                    if let Some(pos) = clean.find(char::from(10)) {
                        clean = clean[pos + 1..].to_string();
                    }
                }
                usable = looks_like_source(&clean);
                clean
            }
            None => {
                usable = false;
                String::new()
            }
        };
        let (source, kind) = if usable {
            (source, kind)
        } else if source.is_empty() {
            (String::new(), "无法解析".to_string())
        } else {
            (String::new(), "无法解析（压缩形式不支持）".to_string())
        };
        out.push(VbaModule {
            name: disp,
            kind,
            lines: source.lines().count(),
            source,
        });
    }
    Ok(out)
}


/// 压缩容器：首字节 0x01，紧跟的 chunk header 的 signature 必须是 0b011
fn is_container(b: &[u8]) -> bool {
    b.len() >= 3 && b[0] == 0x01 && ((u16::from_le_bytes([b[1], b[2]]) >> 12) & 0x7) == 3
}

fn find_container(b: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    while i + 3 <= b.len() {
        if b[i] == 0x01 && is_container(&b[i..]) {
            return Some(i);
        }
        i += 1;
    }
    None
}


/// 源码是否像样：得有 Attribute VB_Name，且可打印字符占绝大多数
fn looks_like_source(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s.contains("Attribute VB_Name") && !s.contains("Attribute VB_") {
        // 有的模块（如 ThisWorkbook）也可能只有代码没属性行，用可打印比例兜底
        if s.len() < 8 {
            return false;
        }
    }
    // 解错时典型症状：同一行反复出现，且没有任何过程定义
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut worst = 0usize;
    for line in s.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let c = counts.entry(line).or_insert(0);
        *c += 1;
        worst = worst.max(*c);
    }
    let has_proc = s.contains("Sub ") || s.contains("Function ") || s.contains("Dim ");
    if worst >= 2 && !has_proc {
        return false;
    }
    let total = s.chars().count();
    // 可打印字符（含换行/制表）占比
    let ok = s
        .chars()
        .filter(|c| c.is_whitespace() || !c.is_control())
        .count();
    (ok as f64) / (total as f64) > 0.97
}

/// 是否有宏（能读出模块即算有）
pub fn has_macros(data: &[u8]) -> bool {
    read_modules(data).map(|m| !m.is_empty()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompresses_uncompressed_chunk() {
        // 容器签名 0x01 + 头（size=0x0FFF，未压缩）+ 4096 字节原文
        let text: &[u8] = b"Attribute VB_Name = \"Module1\"\r\nSub Hello()\r\nEnd Sub\r\n";
        let mut buf = vec![0x01];
        let header: u16 = 0x0FFF | (0b011 << 12);
        buf.extend_from_slice(&header.to_le_bytes());
        let mut data = vec![0u8; 4096];
        data[..text.len()].copy_from_slice(text);
        buf.extend_from_slice(&data);
        let out = decompress(&buf);
        assert_eq!(&out[..text.len()], text, "raw chunk mismatch");
    }

    #[test]
    fn decodes_multi_chunk_containers() {
        // 两个未压缩块：文档说容器可以有多块，逐块拼起来
        let mut buf = vec![0x01];
        let header: u16 = 0x0FFF | (0b011 << 12);
        for tag in [b'A', b'B'] {
            buf.extend_from_slice(&header.to_le_bytes());
            let mut data = vec![tag; 4096];
            data[0] = b'X';
            buf.extend_from_slice(&data);
        }
        let out = decompress(&buf);
        assert_eq!(out.len(), 8192, "两块共 8192 字节");
        assert_eq!(out[0], b'X');
        assert_eq!(out[4096], b'X');
        assert_eq!(out[1], b'A');
        assert_eq!(out[4097], b'B');
    }
    #[test]
    fn rejects_non_cfb() {
        assert!(read_modules(b"not an ole file at all").is_err());
    }
}
