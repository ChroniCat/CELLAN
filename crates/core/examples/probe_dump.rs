//! 读探针 .xls：把 FORMULA 的记号流、ARRAY 记录、全局的 SUPBOOK/EXTERNSHEET/NAME 全打出来。
//! 用来把 Excel 自己写出来的 BIFF 规律抄进实现，而不是猜。

fn u16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn main() -> anyhow::Result<()> {
    let p = std::env::args().nth(1).unwrap();
    let c = sheetcore::cfb::Compound::open(std::fs::read(&p)?)?;
    let s = c.stream_by_path("Workbook").unwrap();

    let mut i = 0usize;
    let mut in_globals = true;
    let mut depth: i32 = 0;
    while i + 4 <= s.len() {
        let t = u16(&s, i);
        let l = u16(&s, i + 2) as usize;
        let body = &s[(i + 4).min(s.len())..(i + 4 + l).min(s.len())];
        match t {
            0x0809 => {
                depth += 1;
                if depth == 1 {
                    in_globals = true;
                    println!("BOF globals {:02x?}", &body[..body.len().min(4)]);
                } else {
                    in_globals = false;
                    println!("BOF sheet");
                }
            }
            0x000A => {
                depth = depth.saturating_sub(1);
            }
            0x01AE if in_globals => println!("SUPBOOK len={} {:02x?}", l, body),
            0x0017 if in_globals => println!("EXTERNSHEET len={} {:02x?}", l, body),
            0x0018 if in_globals => println!("NAME len={} {:02x?}", l, body),
            0x0223 if in_globals => println!("EXTERNNAME len={} {:02x?}", l, body),
            0x0221 => println!("ARRAY len={} {:02x?}", l, body),
            0x0236 => println!("SHRFMLA len={} {:02x?}", l, body),
            0x0222 => println!("TABLE len={} {:02x?}", l, body),
            0x0006 => {
                let row = u16(&s, i + 4);
                let col = u16(&s, i + 6);
                let cce = u16(&s, i + 24) as usize;
                let start = i + 4 + 22;
                let end = (start + cce).min(s.len());
                println!("FORMULA r{} c{} cce={} {:02x?}", row + 1, col + 1, cce, &s[start..end]);
                if (i + 4 + l) > end {
                    println!("  rgcb r{} {:02x?}", row + 1, &s[end..(i + 4 + l)]);
                }
            }
            _ => {
                if std::env::var("SC_ALL").is_ok() && l > 0 && !matches!(t, 0x0204 | 0x0203 | 0x00FD) {
                    println!("REC {:04x} len={} {:02x?}", t, l, &body[..body.len().min(24)]);
                }
            }
        }
        let n = (i + 4 + l).min(s.len());
        if n <= i {
            break;
        }
        i = n;
    }
    Ok(())
}
