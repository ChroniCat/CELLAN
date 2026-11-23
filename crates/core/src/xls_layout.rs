//! 老式 .xls（BIFF8）的行列尺寸。
//!
//! calamine 能读 .xls 的单元格值和合并区，但不给列宽/行高，也不给样式。
//! 这里只做一件事：从 CFB 的 `Workbook` 流里把 COLINFO / ROW 记录抠出来，
//! 让 20 年前的表在界面上不被挤成默认宽度。
//!
//! 只解析需要的记录，遇到不认识的直接跳过——BIFF 是「类型 + 长度」的流，
//! 跳过是安全的。

use std::path::Path;

/// 一张表的尺寸
#[derive(Clone, Debug, Default)]
pub struct SheetSizes {
    /// (起列, 止列, 像素宽) —— 起止都含，0-based
    pub cols: Vec<(u32, u32, f32)>,
    /// (行, 像素高)
    pub rows: Vec<(u32, f32)>,
}

#[derive(Clone, Debug, Default)]
pub struct XlsSizes {
    /// 按表顺序；没有尺寸信息的表是空结构
    pub sheets: Vec<SheetSizes>,
}

const BOF: u16 = 0x0809;
const BOUNDSHEET: u16 = 0x0085;
const COLINFO: u16 = 0x007D;
const ROW: u16 = 0x0208;

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

/// COLINFO uses 0x0100 for the default formatting of future columns.
pub(crate) fn physical_columns(body: &[u8]) -> Option<(u32, u32)> {
    let first = u16le(body, 0)?;
    let last = u16le(body, 2)?;
    (first <= last && first < 256 && last <= 256)
        .then_some((u32::from(first), u32::from(last.min(255))))
}

fn u32le(b: &[u8], at: usize) -> Option<u32> {
    let mut v = [0u8; 4];
    for i in 0..4 {
        v[i] = *b.get(at + i)?;
    }
    Some(u32::from_le_bytes(v))
}

/// 解析 `Workbook` 流。公开出来是为了能用内存里的字节做测试。
pub fn parse(stream: &[u8]) -> XlsSizes {
    // 第一遍：BOUNDSHEET 给出每张表的 BOF 偏移（也就是表顺序）
    let mut bounds: Vec<(u32, String)> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body_end = (i + 4 + len).min(stream.len());
        let body = &stream[i + 4..body_end];
        if typ == BOUNDSHEET && body.len() >= 8 {
            let pos = u32le(body, 0).unwrap_or(0);
            let cch = body[6] as usize;
            let flags = body[7];
            let raw = &body[8..(8 + if flags & 1 == 0 { cch } else { cch * 2 }).min(body.len())];
            let name = if flags & 1 == 0 {
                String::from_utf8_lossy(raw).to_string()
            } else {
                let units: Vec<u16> = raw
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                    .collect();
                String::from_utf16_lossy(&units)
            };
            bounds.push((pos, name));
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }
    bounds.sort_by_key(|(p, _)| *p);

    let mut out = XlsSizes {
        sheets: vec![SheetSizes::default(); bounds.len()],
    };

    // 第二遍：按当前所在表分配 COLINFO / ROW
    let mut cur: Option<usize> = None;
    let mut next_bound = 0usize;
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        while next_bound < bounds.len() && (i as u32) >= bounds[next_bound].0 {
            cur = Some(next_bound);
            next_bound += 1;
        }
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body_end = (i + 4 + len).min(stream.len());
        let body = &stream[i + 4..body_end];
        if let Some(si) = cur {
            match typ {
                COLINFO if body.len() >= 6 => {
                    let chars = u16le(body, 4).unwrap() as f32 / 256.0;
                    if let Some((first, last)) = physical_columns(body).filter(|_| chars > 0.0) {
                        let px = crate::layout::col_width_to_px(chars);
                        if (px - crate::layout::DEFAULT_COL_W).abs() > 0.5 {
                            out.sheets[si].cols.push((first, last, px));
                        }
                    }
                }
                ROW if body.len() >= 8 => {
                    // ROW: rw(0) colMic(2) colMac(4) miyRw(6) ... 行高在偏移 6
                    let r = u16le(body, 0).unwrap() as u32;
                    let twips = u16le(body, 6).unwrap();
                    // 255 twips 是 Excel 的默认行高（12.75pt），不必记
                    if twips != 0 && twips != 0x00FF {
                        let px = crate::layout::pt_to_px(twips as f32 / 20.0);
                        if (px - crate::layout::DEFAULT_ROW_H).abs() > 0.5 {
                            out.sheets[si].rows.push((r, px));
                        }
                    }
                }
                _ => {}
            }
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }
    out
}

/// 读一个 .xls 文件的行列尺寸；不是 CFB（也就是新格式）就返回 None
pub fn read(path: &Path) -> Option<XlsSizes> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"PK") || bytes.len() < 512 {
        return None;
    }
    let c = crate::cfb::Compound::open(bytes).ok()?;
    let stream = c
        .stream_by_path("Workbook")
        .or_else(|| c.stream_by_path("Book"))?;
    Some(parse(&stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(typ: u16, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&typ.to_le_bytes());
        v.extend_from_slice(&(body.len() as u16).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    fn boundsheet(offset: u32, name: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&offset.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.push(name.len() as u8);
        b.push(0); // 8-bit 名字
        b.extend_from_slice(name.as_bytes());
        rec(BOUNDSHEET, &b)
    }

    #[test]
    fn reads_colinfo_and_row_per_sheet() {
        let mut stream = Vec::new();
        // globals
        stream.extend(rec(BOF, &[0u8; 4]));
        stream.extend(boundsheet(200, "预算"));
        stream.extend(boundsheet(400, "汇总"));
        stream.extend(rec(0x000A, &[]));
        // 表 1：偏移 200
        while stream.len() < 200 {
            stream.push(0);
        }
        stream.extend(rec(BOF, &[0u8; 4]));
        let mut ci = Vec::new();
        ci.extend_from_slice(&0u16.to_le_bytes());
        ci.extend_from_slice(&2u16.to_le_bytes());
        ci.extend_from_slice(&(16u16 * 256).to_le_bytes()); // 16 字符宽
        ci.extend_from_slice(&[0u8; 6]);
        stream.extend(rec(COLINFO, &ci));
        let mut row = Vec::new();
        row.extend_from_slice(&1u16.to_le_bytes()); // rw
        row.extend_from_slice(&0u16.to_le_bytes()); // colMic
        row.extend_from_slice(&0u16.to_le_bytes()); // colMac
        row.extend_from_slice(&600u16.to_le_bytes()); // miyRw = 30pt
        row.extend_from_slice(&[0u8; 10]);
        stream.extend(rec(ROW, &row));
        stream.extend(rec(0x000A, &[]));
        // 表 2：偏移 400
        while stream.len() < 400 {
            stream.push(0);
        }
        stream.extend(rec(BOF, &[0u8; 4]));
        stream.extend(rec(0x000A, &[]));

        let out = parse(&stream);
        assert_eq!(out.sheets.len(), 2);
        assert_eq!(out.sheets[0].cols.len(), 1);
        assert_eq!(out.sheets[0].cols[0].0, 0);
        assert_eq!(out.sheets[0].cols[0].1, 2);
        assert!((out.sheets[0].cols[0].2 - 133.0).abs() < 2.0); // 16 字符 × 8 + 5
        assert_eq!(out.sheets[0].rows.len(), 1);
        assert!((out.sheets[0].rows[0].1 - 40.0).abs() < 1.5);
        assert!(out.sheets[1].cols.is_empty());
    }
}
