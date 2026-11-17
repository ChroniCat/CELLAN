//! 按实际用到的字形裁剪 TrueType 字体（只重写 glyf/loca，字形号不变）。
//!
//! PDF 里 CID=字形号（Identity-H），所以裁剪**不能重排 gid**：这里保留 numGlyphs、
//! cmap、hmtx 原样，只把没用到字形的 glyf 数据删掉（loca 指向同一偏移＝空字形）。
//! 复合字形会把依赖的部件一起带上。这样 ToUnicode/Widths 都不用动，
//! 嵌入体积从「整个 GB2312 子集」降到「文档实际用字」。

use std::collections::HashSet;

fn be16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}
fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn tables(data: &[u8]) -> Option<Vec<([u8; 4], usize, usize)>> {
    if data.len() < 12 {
        return None;
    }
    let n = be16(data, 4) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = 12 + i * 16;
        if off + 16 > data.len() {
            return None;
        }
        let tag = [data[off], data[off + 1], data[off + 2], data[off + 3]];
        let toff = be32(data, off + 8) as usize;
        let tlen = be32(data, off + 12) as usize;
        if toff.checked_add(tlen)? > data.len() {
            return None;
        }
        out.push((tag, toff, tlen));
    }
    Some(out)
}

fn loca_offsets(data: &[u8], loca: (usize, usize), n: usize, long: bool) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let v = if long {
            let off = loca.0 + i * 4;
            if off + 4 > loca.0 + loca.1 {
                return None;
            }
            be32(data, off)
        } else {
            let off = loca.0 + i * 2;
            if off + 2 > loca.0 + loca.1 {
                return None;
            }
            be16(data, off) as u32 * 2
        };
        out.push(v);
    }
    Some(out)
}

/// 把复合字形引用的部件 gid 补进集合
fn add_components(data: &[u8], glyf: (usize, usize), loca: &[u32], gid: u16, set: &mut HashSet<u16>) {
    let mut queue = vec![gid];
    while let Some(g) = queue.pop() {
        let i = g as usize;
        if i + 1 >= loca.len() {
            continue;
        }
        let (a, b) = (loca[i] as usize, loca[i + 1] as usize);
        if b <= a || b > glyf.1 {
            continue;
        }
        let start = glyf.0 + a;
        let end = glyf.0 + b;
        if start + 10 > end {
            continue;
        }
        let ncont = i16::from_be_bytes([data[start], data[start + 1]]);
        if ncont >= 0 {
            continue;
        }
        let mut p = start + 10;
        loop {
            if p + 4 > end {
                break;
            }
            let flags = be16(data, p);
            let comp = be16(data, p + 2);
            p += 4;
            p += if flags & 0x0001 != 0 { 4 } else { 2 }; // ARG_1_AND_2_ARE_WORDS
            if flags & 0x0008 != 0 {
                p += 2; // WE_HAVE_A_SCALE
            } else if flags & 0x0040 != 0 {
                p += 4; // X_AND_Y_SCALE
            } else if flags & 0x0080 != 0 {
                p += 8; // TWO_BY_TWO
            }
            if set.insert(comp) {
                queue.push(comp);
            }
            if flags & 0x0020 == 0 {
                break; // MORE_COMPONENTS
            }
        }
    }
}

/// 用 `used` 里的字形号裁一份新 TTF；输入不合法/裁不动就返回 None，调用方保留原字体。
pub fn subset(data: &[u8], used: &HashSet<u16>) -> Option<Vec<u8>> {
    let recs = tables(data)?;
    let find = |tag: &[u8; 4]| recs.iter().find(|r| &r.0 == tag).map(|r| (r.1, r.2));
    let head = find(b"head")?;
    let maxp = find(b"maxp")?;
    let loca = find(b"loca")?;
    let glyf = find(b"glyf")?;
    let n = be16(data, maxp.0 + 4) as usize;
    if n == 0 || n > 65535 {
        return None;
    }
    let long = be16(data, head.0 + 50) == 1;
    let offs = loca_offsets(data, loca, n, long)?;

    let mut set: HashSet<u16> = used.iter().copied().filter(|g| (*g as usize) < n).collect();
    set.insert(0);
    let seeds: Vec<u16> = set.iter().copied().collect();
    for g in seeds {
        add_components(data, glyf, &offs, g, &mut set);
    }

    // 重写 glyf：用到的字形按 gid 顺序紧凑排（4 字节对齐），其余 gid 长度 0
    let mut new_glyf: Vec<u8> = Vec::new();
    let mut new_offs: Vec<u32> = vec![0; n + 1];
    for gid in 0..n {
        let (a, b) = (offs[gid] as usize, offs[gid + 1] as usize);
        if set.contains(&(gid as u16)) && b > a && b <= glyf.1 {
            // 对齐后才是这张字形的真正起点；loca 必须指到这里，不能指到补零前
            while new_glyf.len() % 4 != 0 {
                new_glyf.push(0);
            }
            new_offs[gid] = new_glyf.len() as u32;
            new_glyf.extend_from_slice(&data[glyf.0 + a..glyf.0 + b]);
        }
        new_offs[gid + 1] = new_glyf.len() as u32;
    }
    while new_glyf.len() % 4 != 0 {
        new_glyf.push(0);
    }
    new_offs[n] = new_glyf.len() as u32;

    let mut new_loca: Vec<u8> = Vec::with_capacity((n + 1) * 4);
    for o in &new_offs {
        new_loca.extend_from_slice(&o.to_be_bytes());
    }
    let mut new_head = data[head.0..head.0 + head.1].to_vec();
    if new_head.len() < 54 {
        return None;
    }
    new_head[8..12].copy_from_slice(&[0, 0, 0, 0]); // checkSumAdjustment
    new_head[50..52].copy_from_slice(&1i16.to_be_bytes()); // indexToLocFormat = long

    // 白名单：PDF 渲染需要的表；GSUB/GPOS/GDEF/kern 这类排版表不要（CJK 里很大）
    let keep: [&[u8; 4]; 14] = [
        b"head", b"hhea", b"maxp", b"hmtx", b"cmap", b"glyf", b"loca", b"name", b"OS/2",
        b"post", b"cvt ", b"fpgm", b"prep", b"gasp",
    ];
    let mut out_recs: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for r in &recs {
        if !keep.iter().any(|k| *k == &r.0) {
            continue;
        }
        let body = match &r.0 {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            _ => data[r.1..r.1 + r.2].to_vec(),
        };
        out_recs.push((r.0, body));
    }
    if !out_recs.iter().any(|r| &r.0 == b"glyf") || !out_recs.iter().any(|r| &r.0 == b"loca") {
        return None;
    }

    // 组装 sfnt
    let count = out_recs.len() as u16;
    // searchRange/entrySelector/rangeShift 必须按新的表数重算：
    // 沿用原值会让按二分查表的解析器找不到 cmap/glyf（文字能提取但字形空白）。
    let mut pow2 = 1u16;
    let mut log2 = 0u16;
    while pow2 * 2 <= count {
        pow2 *= 2;
        log2 += 1;
    }
    let search_range = pow2 * 16;
    let range_shift = count * 16 - search_range;
    let mut out = Vec::with_capacity(new_glyf.len() + 4096);
    out.extend_from_slice(&data[0..4]);
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&log2.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    let mut head_off = 12 + out_recs.len() * 16;
    let mut body_off = head_off;
    for (_, b) in &out_recs {
        head_off += 0;
        body_off += b.len() + (4 - b.len() % 4) % 4;
    }
    let _ = head_off;
    let mut cur = 12 + out_recs.len() * 16;
    let mut dir: Vec<u8> = Vec::with_capacity(out_recs.len() * 16);
    let mut bodies: Vec<u8> = Vec::new();
    for (tag, b) in &out_recs {
        dir.extend_from_slice(tag);
        dir.extend_from_slice(&0u32.to_be_bytes()); // checksum 不参与 PDF 解析，置 0
        dir.extend_from_slice(&(cur as u32).to_be_bytes());
        dir.extend_from_slice(&(b.len() as u32).to_be_bytes());
        bodies.extend_from_slice(b);
        while bodies.len() % 4 != 0 {
            bodies.push(0);
        }
        cur = 12 + out_recs.len() * 16 + bodies.len();
    }
    out.extend_from_slice(&dir);
    out.extend_from_slice(&bodies);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sans() -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/subset/NotoSansSC-Regular.subset.ttf"),
        )
        .expect("内嵌字体应存在")
    }

    #[test]
    fn subset_drops_unused_glyf_but_keeps_gids() {
        let data = sans();
        let mut used = HashSet::new();
        for ch in "销售汇总数量金额".chars() {
            // 用 ttf-parser 查 gid（和 PDF 里用的是同一套）
            let face = ttf_parser::Face::parse(&data, 0).unwrap();
            if let Some(g) = face.glyph_index(ch) {
                used.insert(g.0);
            }
        }
        let small = subset(&data, &used).expect("应能裁剪");
        assert!(small.len() * 3 < data.len(), "{} → {}", data.len(), small.len());
        // gid 不变：同样的字符在裁剪后的字体里仍映射到同一字形号
        let before = ttf_parser::Face::parse(&data, 0).unwrap();
        let after = ttf_parser::Face::parse(&small, 0).unwrap();
        for ch in "销售汇总数量金额".chars() {
            assert_eq!(
                before.glyph_index(ch).map(|g| g.0),
                after.glyph_index(ch).map(|g| g.0),
                "{ch} 的 gid 变了"
            );
        }
        assert_eq!(after.number_of_glyphs(), before.number_of_glyphs());
    }

    #[test]
    fn composite_components_are_kept() {
        let data = sans();
        let face = ttf_parser::Face::parse(&data, 0).unwrap();
        // é 通常是复合字形；没有就跳过（不同字体子集可能不同）
        let Some(g) = face.glyph_index('é') else { return };
        let mut used = HashSet::new();
        used.insert(g.0);
        let small = subset(&data, &used).expect("应能裁剪");
        let after = ttf_parser::Face::parse(&small, 0).unwrap();
        assert_eq!(after.glyph_index('é').map(|x| x.0), Some(g.0));
        // 复合字形的部件必须还在，否则读出来是空框：直接确认该字有轮廓
        let mut outline = 0;
        let mut builder = CountingOutline { n: &mut outline };
        after.outline_glyph(g, &mut builder);
        assert!(outline > 0, "é 轮廓丢失（部件没带上）");
    }

    struct CountingOutline<'a> {
        n: &'a mut usize,
    }
    impl ttf_parser::OutlineBuilder for CountingOutline<'_> {
        fn move_to(&mut self, _: f32, _: f32) {
            *self.n += 1;
        }
        fn line_to(&mut self, _: f32, _: f32) {
            *self.n += 1;
        }
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
            *self.n += 1;
        }
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
            *self.n += 1;
        }
        fn close(&mut self) {}
    }
}
