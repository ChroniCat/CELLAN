//! 老式 .xls 的兜底读取：calamine 打不开的文件（比如我们自己写的）用这个。
//!
//! 只取「单元格值和表名」，样式仍由 `xls_style` 负责。支持 BIFF8 的
//! LABEL / NUMBER / RK / MULRK / LABEL / LABELSST / BOOLERR / FORMULA(缓存值)。

use crate::model::{Cell, Sheet, Value, Workbook};
use anyhow::{bail, Result};
use std::path::Path;

const BOUNDSHEET: u16 = 0x0085;
const LABELSST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MULRK: u16 = 0x00BD;
const BOOLERR: u16 = 0x0205;
const FORMULA: u16 = 0x0006;
const SST: u16 = 0x00FC;
const CONTINUE: u16 = 0x003C;
const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;

fn u16le(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}
fn u32le(b: &[u8], i: usize) -> Option<u32> {
    let mut v = [0u8; 4];
    for k in 0..4 {
        v[k] = *b.get(i + k)?;
    }
    Some(u32::from_le_bytes(v))
}

/// XLUnicodeString：cch(2) + flags(1) + 字符；返回值与消耗的字节数
fn read_str(b: &[u8], at: usize) -> Option<(String, usize)> {
    let cch = u16le(b, at)? as usize;
    let flags = *b.get(at + 2)?;
    let wide = flags & 1 != 0;
    let bytes = if wide { cch * 2 } else { cch };
    let raw = b.get(at + 3..at + 3 + bytes)?;
    let s = if wide {
        let units: Vec<u16> = raw
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).to_string()
    };
    Some((s, 3 + bytes))
}

fn rk_value(rk: u32) -> f64 {
    let cents = rk & 1 != 0;
    let int = rk & 2 != 0;
    let mut v = if int {
        (rk as i32 >> 2) as f64
    } else {
        f64::from_bits(((rk as u64) & 0xFFFF_FFFC) << 32)
    };
    if cents {
        v /= 100.0;
    }
    v
}

/// 解析 `Workbook` 流里的表名与单元格值
pub struct SheetData {
    pub name: String,
    pub cells: Vec<(u32, u32, Cell)>,
    pub merges: Vec<(u32, u32, u32, u32)>,
}

/// BIFF8 错误码 → ErrKind（与 xls_write::biff_error_code 互为反函数）
fn err_kind_from_biff(code: u8) -> Option<crate::formula::ErrKind> {
    use crate::formula::ErrKind as K;
    Some(match code {
        0x00 => K::Null,
        0x07 => K::Div0,
        0x0F => K::Value,
        0x17 => K::Ref,
        0x1D => K::Name,
        0x24 => K::Num,
        0x2A => K::Na,
        _ => return None,
    })
}

pub fn read_stream(stream: &[u8]) -> Result<Vec<SheetData>> {
    // 表名 + BOF 偏移
    let mut bounds: Vec<(u32, String)> = Vec::new();
    let mut sst: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        let t = u16le(stream, i).unwrap();
        let l = u16le(stream, i + 2).unwrap() as usize;
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + l).min(stream.len())];
        if t == BOUNDSHEET && body.len() >= 8 {
            let pos = u32le(body, 0).unwrap_or(0);
            let cch = body[6] as usize;
            let flags = body[7];
            let raw = &body[8..(8 + if flags & 1 == 0 { cch } else { cch * 2 }).min(body.len())];
            let name = if flags & 1 == 0 {
                String::from_utf8_lossy(raw).to_string()
            } else {
                let u: Vec<u16> = raw
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                    .collect();
                String::from_utf16_lossy(&u)
            };
            bounds.push((pos, name));
        }
        if t == SST {
            sst = parse_sst(stream, i + 4, l);
        }
        let n = (i + 4 + l).min(stream.len());
        if n <= i {
            break;
        }
        i = n;
    }
    bounds.sort_by_key(|(p, _)| *p);
    if bounds.is_empty() {
        bail!("没有 BOUNDSHEET");
    }

    let mut out: Vec<SheetData> = bounds
        .iter()
        .map(|(_, n)| SheetData {
            name: n.clone(),
            cells: Vec::new(),
            merges: Vec::new(),
        })
        .collect();

    let mut cur: Option<usize> = None;
    let mut next_bound = 0usize;
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        while next_bound < bounds.len() && (i as u32) >= bounds[next_bound].0 {
            cur = Some(next_bound);
            next_bound += 1;
        }
        let t = u16le(stream, i).unwrap();
        let l = u16le(stream, i + 2).unwrap() as usize;
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + l).min(stream.len())];
        if let Some(si) = cur {
            let mut push = |r: u32, c: u32, v: Value| {
                if v != Value::Empty {
                    out[si].cells.push((
                        r,
                        c,
                        Cell {
                            value: v,
                            formula: None,
                        },
                    ));
                }
            };
            match t {
                NUMBER if body.len() >= 14 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c = u16le(body, 2).unwrap() as u32;
                    let mut b8 = [0u8; 8];
                    b8.copy_from_slice(&body[6..14]);
                    push(r, c, Value::Number(f64::from_le_bytes(b8)));
                }
                RK if body.len() >= 10 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c = u16le(body, 2).unwrap() as u32;
                    let rk = u32le(body, 6).unwrap();
                    push(r, c, Value::Number(rk_value(rk)));
                }
                MULRK if body.len() >= 6 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c1 = u16le(body, 2).unwrap() as u32;
                    let n = (body.len().saturating_sub(6)) / 6;
                    for k in 0..n {
                        if let Some(rk) = u32le(body, 4 + k * 6 + 2) {
                            push(r, c1 + k as u32, Value::Number(rk_value(rk)));
                        }
                    }
                }
                LABEL if body.len() >= 8 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c = u16le(body, 2).unwrap() as u32;
                    if let Some((s, _)) = read_str(body, 6) {
                        push(r, c, Value::Text(s.into()));
                    }
                }
                LABELSST if body.len() >= 10 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c = u16le(body, 2).unwrap() as u32;
                    let idx = u32le(body, 6).unwrap() as usize;
                    if let Some(s) = sst.get(idx) {
                        push(r, c, Value::Text(s.clone().into()));
                    }
                }
                BOOLERR if body.len() >= 8 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c = u16le(body, 2).unwrap() as u32;
                    if body[7] == 0 {
                        push(r, c, Value::Bool(body[6] != 0));
                    } else {
                        // fError=1：#NULL!/#DIV/0!/... 第 2 字节是 BIFF 错误码
                        if let Some(k) = err_kind_from_biff(body[6]) {
                            push(r, c, Value::Error(k));
                        }
                    }
                }
                FORMULA if body.len() >= 14 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c = u16le(body, 2).unwrap() as u32;
                    // 缓存结果：前两字节 0xFFFF 表示特殊值。两种布局都要认——
                    // MS-XLS 写在 num 的开头（body[6..8]），Excel 实际文件里也见过
                    // 写在 num 末尾（body[12..14]）的，旧实现只认后者还读错了类型字节。
                    let special = if body[6] == 0xFF && body[7] == 0xFF {
                        Some((body[8], body[9]))
                    } else if body[12] == 0xFF && body[13] == 0xFF {
                        Some((body[6], body[8]))
                    } else {
                        None
                    };
                    if let Some((ty, val)) = special {
                        match ty {
                            // 0 = 字符串结果（值在随后的 STRING 记录里，这里不抢着写）
                            0 => {}
                            1 => push(r, c, Value::Bool(val != 0)),
                            2 => {
                                if let Some(k) = err_kind_from_biff(val) {
                                    push(r, c, Value::Error(k));
                                }
                            }
                            3 => push(r, c, Value::Empty),
                            _ => {}
                        }
                    } else {
                        let mut b8 = [0u8; 8];
                        b8.copy_from_slice(&body[6..14]);
                        push(r, c, Value::Number(f64::from_le_bytes(b8)));
                    }
                }
                0x00E5 if body.len() >= 2 => {
                    let n = u16le(body, 0).unwrap_or(0) as usize;
                    for k in 0..n {
                        let at = 2 + k * 8;
                        if at + 8 > body.len() {
                            break;
                        }
                        out[si].merges.push((
                            u16le(body, at).unwrap() as u32,
                            u16le(body, at + 4).unwrap() as u32,
                            u16le(body, at + 2).unwrap() as u32,
                            u16le(body, at + 6).unwrap() as u32,
                        ));
                    }
                }
                _ => {}
            }
        }
        let n = (i + 4 + l).min(stream.len());
        if n <= i {
            break;
        }
        i = n;
    }
    Ok(out)
}

/// SST：total(4) unique(4) 然后逐条字符串；CONTINUE 记录里的字符串可能跨段，
/// 这里按「同一段内完整」处理（我们自己写出的文件不用 SST，Excel 的分段另有兜底）。
fn parse_sst(stream: &[u8], start: usize, len: usize) -> Vec<String> {
    let mut body: Vec<u8> = stream[start..(start + len).min(stream.len())].to_vec();
    let mut i = start + len;
    while i + 4 <= stream.len() {
        let t = u16le(stream, i).unwrap();
        if t != CONTINUE {
            break;
        }
        let l = u16le(stream, i + 2).unwrap() as usize;
        body.extend_from_slice(&stream[(i + 4).min(stream.len())..(i + 4 + l).min(stream.len())]);
        i += 4 + l;
    }
    let mut out = Vec::new();
    let unique = u32le(&body, 4).unwrap_or(0) as usize;
    let mut at = 8usize;
    while out.len() < unique && at + 3 <= body.len() {
        match read_str(&body, at) {
            Some((s, used)) => {
                out.push(s);
                at += used;
            }
            None => break,
        }
    }
    out
}

/// 读一个 .xls 文件（CFB → Workbook 流 → BIFF）
pub fn load(path: &Path) -> Result<Workbook> {
    let bytes = std::fs::read(path)?;
    let cf = crate::cfb::Compound::open(bytes)?;
    let stream = cf
        .stream_by_path("Workbook")
        .or_else(|| cf.stream_by_path("Book"))
        .ok_or_else(|| anyhow::anyhow!("没有 Workbook 流"))?;
    let parsed = read_stream(&stream)?;

    let mut wb = Workbook::new_blank();
    wb.path = Some(path.to_path_buf());
    wb.sheets.clear();
    for sd in parsed {
        let mut sh = Sheet::new(sd.name);
        for (r, c, cell) in sd.cells {
            sh.set(r, c, cell);
        }
        sh.merges = sd.merges;
        wb.sheets.push(sh);
    }
    if wb.sheets.is_empty() {
        bail!("文件里没有工作表");
    }
    Ok(wb)
}
