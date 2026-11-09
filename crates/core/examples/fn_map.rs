//! 从探针 .xls 里按行配对「B 列的文本标签」和「A 列的公式记号流」。
//! 输出 name<TAB>tokens，用来生成函数号表（不靠外部行号映射，避免错行）。

fn u16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

/// LABEL / RSTRING 里的 XLUnicodeString（cch 2 字节 + flags 1 字节）
fn read_string(b: &[u8], at: usize) -> Option<String> {
    if at + 3 > b.len() {
        return None;
    }
    let cch = u16(b, at) as usize;
    let flags = b[at + 2];
    let wide = flags & 1 != 0;
    let start = at + 3;
    let n = if wide { cch * 2 } else { cch };
    if start + n > b.len() {
        return None;
    }
    if wide {
        let units: Vec<u16> = (0..cch).map(|i| u16(b, start + i * 2)).collect();
        String::from_utf16(&units).ok()
    } else {
        Some(b[start..start + n].iter().map(|&c| c as char).collect())
    }
}

fn main() -> anyhow::Result<()> {
    let p = std::env::args().nth(1).unwrap();
    let c = sheetcore::cfb::Compound::open(std::fs::read(&p)?)?;
    let s = c.stream_by_path("Workbook").unwrap();

    let mut labels: std::collections::HashMap<u16, String> = std::collections::HashMap::new();
    let mut forms: std::collections::HashMap<u16, Vec<u8>> = std::collections::HashMap::new();
    let mut i = 0usize;
    let mut in_sheet = false;
    let mut bofs: i32 = 0;
    while i + 4 <= s.len() {
        let t = u16(&s, i);
        let l = u16(&s, i + 2) as usize;
        let b = &s[(i + 4).min(s.len())..(i + 4 + l).min(s.len())];
        match t {
            0x0809 => {
                bofs += 1;
                in_sheet = bofs > 1;
            }
            _ if in_sheet => {
                if t == 0x0006 && b.len() > 22 {
                    let row = u16(b, 0);
                    let col = u16(b, 2);
                    let cce = u16(b, 20) as usize;
                    if col == 0 {
                        let toks: Vec<u8> = b[22..(22 + cce).min(b.len())].to_vec();
                        forms.insert(row, toks);
                        if std::env::var("SC_LEN").is_ok() {
                            println!("LEN r{} body={} cce={}", row + 1, l, cce);
                        }
                    }
                }
            }
            _ => {}
        }
        let n = (i + 4 + l).min(s.len());
        if n <= i {
            break;
        }
        i = n;
    }

    eprintln!("[fn_map] labels={} formulas={}", labels.len(), forms.len());
    // 名字从模型里读（BIFF8 用 LABELSST 引用 SST，直接解析记录拿不到文本）
    let wb = sheetcore::xlsx::load(std::path::Path::new(&p))?;
    let sh = &wb.sheets[0];
    for r in forms.keys().copied().collect::<Vec<_>>() {
        if let Some(cell) = sh.get(r as u32, 1) {
            labels.insert(r, cell.value.display());
        }
    }
    let mut rows: Vec<&u16> = forms.keys().collect();
    rows.sort();
    for r in rows {
        let name = labels.get(r).cloned().unwrap_or_default();
        let toks = &forms[r];
        println!(
            "{}\t{}\t{}",
            name,
            r + 1,
            toks.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
        );
    }
    Ok(())
}
