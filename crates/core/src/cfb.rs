//! 读 OLE 复合文档（CFB）：vbaProject.bin 用的就是这种容器。
//!
//! 只实现读取所需的最小集：头、FAT、MiniFAT、目录项、流。不写、不改、不建。

use anyhow::{bail, Result};
use std::collections::HashMap;

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub struct Entry {
    pub name: String,
    pub kind: u8, // 1 storage, 2 stream, 5 root
    pub start: u32,
    pub size: u64,
}

pub struct Compound {
    data: Vec<u8>,
    sector_size: usize,
    mini_size: usize,
    mini_cutoff: u32,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    mini_stream: Vec<u8>,
    pub entries: Vec<Entry>,
}

const FREE: u32 = 0xFFFF_FFFF;
const END: u32 = 0xFFFF_FFFE;

impl Compound {
    pub fn open(data: Vec<u8>) -> Result<Compound> {
        if data.len() < 512 || data[0..8] != SIGNATURE {
            bail!("不是 OLE 复合文档");
        }
        let sector_shift = u16le(&data, 30) as usize;
        let mini_shift = u16le(&data, 32) as usize;
        let sector_size = 1usize << sector_shift;
        let mini_size = 1usize << mini_shift;
        let n_fat = u32le(&data, 44) as usize;
        let dir_start = u32le(&data, 48);
        let mini_cutoff = u32le(&data, 56);
        let minifat_start = u32le(&data, 60);
        let n_minifat = u32le(&data, 64) as usize;
        let difat_start = u32le(&data, 68);
        let n_difat = u32le(&data, 72) as usize;

        let sector = |i: u32| -> usize { (i as usize + 1) * sector_size };

        // DIFAT：头里 109 条 + 扩展扇区链
        let mut difat: Vec<u32> = Vec::new();
        for i in 0..109 {
            let v = u32le(&data, 76 + i * 4);
            if v == FREE {
                break;
            }
            difat.push(v);
        }
        let mut next = difat_start;
        for _ in 0..n_difat {
            if next == FREE || next == END {
                break;
            }
            let base = sector(next);
            let n = (sector_size / 4) - 1;
            for i in 0..n {
                let v = u32le(&data, base + i * 4);
                if v != FREE {
                    difat.push(v);
                }
            }
            next = u32le(&data, base + n * 4);
        }

        // FAT
        let mut fat: Vec<u32> = Vec::new();
        for s in difat.iter().take(n_fat.max(difat.len())) {
            let base = sector(*s);
            if base + sector_size > data.len() {
                continue;
            }
            for i in 0..(sector_size / 4) {
                fat.push(u32le(&data, base + i * 4));
            }
        }

        // 目录
        let dir_bytes = read_chain(&data, sector_size, &fat, dir_start);
        let mut entries = Vec::new();
        for i in 0..(dir_bytes.len() / 128) {
            let b = &dir_bytes[i * 128..(i + 1) * 128];
            let name_len = u16le(b, 64) as usize;
            if name_len < 2 || name_len > 64 {
                continue;
            }
            let raw: Vec<u16> = (0..(name_len / 2 - 1))
                .map(|k| u16le(b, k * 2))
                .collect();
            let name = String::from_utf16_lossy(&raw);
            let kind = b[66];
            let start = u32le(b, 116);
            let size = if sector_shift == 12 {
                u32le(b, 120) as u64 | ((u32le(b, 124) as u64) << 32)
            } else {
                u32le(b, 120) as u64
            };
            entries.push(Entry { name, kind, start, size });
        }

        // MiniFAT
        let mut minifat = Vec::new();
        if n_minifat > 0 && minifat_start != FREE {
            let mf = read_chain(&data, sector_size, &fat, minifat_start);
            for i in 0..(mf.len() / 4) {
                minifat.push(u32le(&mf, i * 4));
            }
        }

        // 根目录项指向 mini stream
        let mini_stream = entries
            .iter()
            .find(|e| e.kind == 5)
            .map(|r| read_chain(&data, sector_size, &fat, r.start))
            .unwrap_or_default();

        Ok(Compound {
            data,
            sector_size,
            mini_size,
            mini_cutoff,
            fat,
            minifat,
            mini_stream,
            entries,
        })
    }

    /// 按名字取流（大小写不敏感，VBA 里 "VBA/dir" 这种路径是逐级存储）
    pub fn stream_by_path(&self, path: &str) -> Option<Vec<u8>> {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        self.entries
            .iter()
            .find(|e| e.kind == 2 && e.name.eq_ignore_ascii_case(leaf))
            .map(|e| self.read_stream(e))
    }

    pub fn read_stream(&self, e: &Entry) -> Vec<u8> {
        if e.size < self.mini_cutoff as u64 {
            // 小流走 mini stream + miniFAT
            let mut out = Vec::new();
            let mut cur = e.start;
            let mut guard = 0;
            while cur != END && cur != FREE && (out.len() as u64) < e.size && guard < 100_000 {
                let base = cur as usize * self.mini_size;
                if base + self.mini_size > self.mini_stream.len() {
                    break;
                }
                out.extend_from_slice(&self.mini_stream[base..base + self.mini_size]);
                cur = *self.minifat.get(cur as usize).unwrap_or(&END);
                guard += 1;
            }
            out.truncate(e.size as usize);
            out
        } else {
            let mut out = read_chain(&self.data, self.sector_size, &self.fat, e.start);
            out.truncate(e.size as usize);
            out
        }
    }

    pub fn debug_mini_len(&self) -> usize {
        self.mini_stream.len()
    }

    pub fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| !e.name.is_empty())
            .map(|e| format!("{}[{}]", e.name, e.kind))
            .collect()
    }

    /// 目录项按名字建索引（调试用）
    pub fn index(&self) -> HashMap<String, usize> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i))
            .collect()
    }
}

fn read_chain(data: &[u8], sector_size: usize, fat: &[u32], start: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cur = start;
    let mut guard = 0;
    while cur != END && cur != FREE && guard < 200_000 {
        let base = (cur as usize + 1) * sector_size;
        if base + sector_size > data.len() {
            break;
        }
        out.extend_from_slice(&data[base..base + sector_size]);
        cur = *fat.get(cur as usize).unwrap_or(&END);
        guard += 1;
    }
    out
}

fn read_sector_at(
    f: &mut std::fs::File,
    file_len: u64,
    sector_size: usize,
    sid: u32,
    buf: &mut [u8],
) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Some(off) = (sid as u64).checked_add(1).and_then(|n| n.checked_mul(sector_size as u64)) else {
        return false;
    };
    if off + sector_size as u64 > file_len {
        return false;
    }
    f.seek(SeekFrom::Start(off)).is_ok() && f.read_exact(&mut buf[..sector_size]).is_ok()
}

fn read_difat_sectors(
    f: &mut std::fs::File,
    file_len: u64,
    sector_size: usize,
    head: &[u8; 512],
) -> Result<Vec<u32>, ()> {
    let difat_start = u32le(head, 68);
    let n_difat = u32le(head, 72) as usize;
    let mut difat = Vec::with_capacity(109 + n_difat * (sector_size / 4));
    for i in 0..109 {
        let v = u32le(head, 76 + i * 4);
        if v == FREE {
            break;
        }
        difat.push(v);
    }
    let mut next = difat_start;
    let mut sec_buf = vec![0u8; sector_size];
    let n = (sector_size / 4) - 1;
    for _ in 0..n_difat.min(16_384) {
        if next == FREE || next == END {
            break;
        }
        if !read_sector_at(f, file_len, sector_size, next, &mut sec_buf) {
            return Err(());
        }
        for i in 0..n {
            let v = u32le(&sec_buf, i * 4);
            if v != FREE {
                difat.push(v);
            }
        }
        next = u32le(&sec_buf, n * 4);
    }
    Ok(difat)
}

/// 按需跳转只读 CFB 头与目录扇区（~1.5KB），判断是否存在 `EncryptedPackage` 流，
/// 避免打开 200MB `.xls` 时全盘逐兆字节扫描。仅在小型损坏/合成桩文件上回退字节扫描。
pub fn has_encrypted_package_file(path: &std::path::Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(meta) = f.metadata() else {
        return false;
    };
    let file_len = meta.len();
    if file_len < 8 {
        return false;
    }
    let check_dir = |f: &mut std::fs::File| -> Result<bool, ()> {
        if file_len < 512 {
            return Err(());
        }
        let mut head = [0u8; 512];
        f.read_exact(&mut head).map_err(|_| ())?;
        if head[0..8] != SIGNATURE {
            return Ok(false);
        }
        let sector_shift = u16le(&head, 30) as usize;
        if sector_shift != 9 && sector_shift != 12 {
            return Err(());
        }
        let sector_size = 1usize << sector_shift;
        let dir_start = u32le(&head, 48);
        if dir_start == FREE || dir_start == END {
            return Err(());
        }
        let difat = read_difat_sectors(f, file_len, sector_size, &head)?;
        let per_fat = sector_size / 4;
        let mut fat_cache: HashMap<usize, Vec<u32>> = HashMap::new();
        let mut sec_buf = vec![0u8; sector_size];
        let mut dir_buf = vec![0u8; sector_size];
        let mut cur = dir_start;
        let mut guard = 0usize;
        while cur != END && cur != FREE {
            if guard >= 65_536 || !read_sector_at(f, file_len, sector_size, cur, &mut dir_buf) {
                return Err(());
            }
            for i in 0..(sector_size / 128) {
                let b = &dir_buf[i * 128..(i + 1) * 128];
                let name_len = u16le(b, 64) as usize;
                let kind = b[66];
                if kind != 0 && (2..=64).contains(&name_len) {
                    let chars = name_len / 2 - 1;
                    if chars == "EncryptedPackage".len() {
                        let matched = "EncryptedPackage"
                            .bytes()
                            .enumerate()
                            .all(|(k, ch)| {
                                let u = u16le(b, k * 2);
                                u <= 0x7F && (u as u8).eq_ignore_ascii_case(&ch)
                            });
                        if matched {
                            return Ok(true);
                        }
                    }
                }
            }
            let fat_idx = (cur as usize) / per_fat;
            let fat_off = (cur as usize) % per_fat;
            let fat_sec = if let Some(v) = fat_cache.get(&fat_idx) {
                v
            } else {
                let &fat_sid = difat.get(fat_idx).ok_or(())?;
                if !read_sector_at(f, file_len, sector_size, fat_sid, &mut sec_buf) {
                    return Err(());
                }
                let words: Vec<u32> = (0..per_fat).map(|k| u32le(&sec_buf, k * 4)).collect();
                fat_cache.entry(fat_idx).or_insert(words)
            };
            cur = fat_sec[fat_off];
            guard += 1;
        }
        Ok(false)
    };

    match check_dir(&mut f) {
        Ok(ans) => ans,
        Err(()) => {
            if file_len > 64 * 1024 || f.seek(SeekFrom::Start(0)).is_err() {
                return false;
            }
            let mut raw = Vec::with_capacity(file_len as usize);
            if f.read_to_end(&mut raw).is_err() || raw.len() < 8 || raw[0..8] != SIGNATURE {
                return false;
            }
            const NEEDLE: [u8; 32] = [
                b'E', 0, b'n', 0, b'c', 0, b'r', 0, b'y', 0, b'p', 0, b't', 0, b'e', 0,
                b'd', 0, b'P', 0, b'a', 0, b'c', 0, b'k', 0, b'a', 0, b'g', 0, b'e', 0,
            ];
            raw.windows(NEEDLE.len()).any(|w| w == NEEDLE)
        }
    }
}

/// 按扇区链直接从文件流式读取 `Workbook`（或 `Book`）流，避免先把整个 `.xls` 读入内存再拷贝一份。
pub fn read_workbook_stream_file(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let file_len = f.metadata().ok()?.len();
    if file_len < 512 {
        return None;
    }
    let mut head = [0u8; 512];
    f.read_exact(&mut head).ok()?;
    if head[0..8] != SIGNATURE {
        return None;
    }
    let sector_shift = u16le(&head, 30) as usize;
    let mini_shift = u16le(&head, 32) as usize;
    if (sector_shift != 9 && sector_shift != 12) || mini_shift > sector_shift {
        return None;
    }
    let sector_size = 1usize << sector_shift;
    let mini_size = 1usize << mini_shift;
    let n_fat = u32le(&head, 44) as usize;
    let dir_start = u32le(&head, 48);
    let mini_cutoff = u32le(&head, 56);
    let minifat_start = u32le(&head, 60);
    let n_minifat = u32le(&head, 64) as usize;

    let difat = read_difat_sectors(&mut f, file_len, sector_size, &head).ok()?;
    let per_fat = sector_size / 4;
    let fat_count = difat.len().min(n_fat.max(difat.len()));
    let fat_sectors = &difat[..fat_count];
    let mut fat = Vec::with_capacity(fat_sectors.len() * per_fat);
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut run_buf = Vec::new();
        let mut idx = 0usize;
        while idx < fat_sectors.len() {
            let start_sid = fat_sectors[idx];
            let Some(run_off) = (start_sid as u64)
                .checked_add(1)
                .and_then(|n| n.checked_mul(sector_size as u64))
            else {
                idx += 1;
                continue;
            };
            if run_off
                .checked_add(sector_size as u64)
                .is_none_or(|end| end > file_len)
            {
                idx += 1;
                continue;
            }
            let mut run_len = 1usize;
            while idx + run_len < fat_sectors.len()
                && fat_sectors[idx + run_len - 1].checked_add(1) == Some(fat_sectors[idx + run_len])
            {
                let next_bytes = ((run_len + 1) as u64).checked_mul(sector_size as u64);
                let next_end = next_bytes.and_then(|b| run_off.checked_add(b));
                if next_end.is_none_or(|end| end > file_len) {
                    break;
                }
                run_len += 1;
            }
            let byte_len = run_len * sector_size;
            run_buf.resize(byte_len, 0);
            if f.seek(SeekFrom::Start(run_off)).is_ok() && f.read_exact(&mut run_buf).is_ok() {
                for chunk in run_buf.chunks_exact(4) {
                    fat.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            idx += run_len;
        }
    }

    let read_file_chain = |f: &mut std::fs::File, start: u32, expected: Option<usize>| -> Vec<u8> {
        use std::io::{Read, Seek, SeekFrom};
        let max_sectors_needed = expected.map(|lim| lim.div_ceil(sector_size));
        let init_cap = match expected {
            Some(lim) => lim.min(file_len as usize),
            None => sector_size * 4,
        };
        let mut out = Vec::with_capacity(init_cap);
        let mut cur = start;
        let mut guard = 0usize;
        while cur != END && cur != FREE && guard < 200_000 {
            if let Some(lim) = expected {
                if out.len() >= lim {
                    break;
                }
            }
            let Some(run_off) = (cur as u64)
                .checked_add(1)
                .and_then(|n| n.checked_mul(sector_size as u64))
            else {
                break;
            };
            if run_off
                .checked_add(sector_size as u64)
                .is_none_or(|end| end > file_len)
            {
                break;
            }
            let mut run_sectors = 1usize;
            let mut last_sid = cur;
            guard += 1;
            cur = *fat.get(cur as usize).unwrap_or(&END);

            while cur != END
                && cur != FREE
                && guard < 200_000
                && last_sid.checked_add(1) == Some(cur)
            {
                if let Some(max_sec) = max_sectors_needed {
                    if guard >= max_sec {
                        break;
                    }
                }
                let next_bytes = ((run_sectors + 1) as u64).checked_mul(sector_size as u64);
                let next_end = next_bytes.and_then(|b| run_off.checked_add(b));
                if next_end.is_none_or(|end| end > file_len) {
                    break;
                }
                last_sid = cur;
                run_sectors += 1;
                guard += 1;
                cur = *fat.get(cur as usize).unwrap_or(&END);
            }

            let run_bytes = run_sectors * sector_size;
            let to_read = match expected {
                Some(lim) => run_bytes.min(lim.saturating_sub(out.len())),
                None => run_bytes,
            };
            if to_read == 0 {
                break;
            }
            let old_len = out.len();
            out.resize(old_len + to_read, 0);
            if f.seek(SeekFrom::Start(run_off)).is_err()
                || f.read_exact(&mut out[old_len..old_len + to_read]).is_err()
            {
                out.truncate(old_len);
                break;
            }
        }
        if let Some(lim) = expected {
            out.truncate(lim);
        }
        out
    };

    let dir_bytes = read_file_chain(&mut f, dir_start, None);
    let mut root_entry: Option<Entry> = None;
    let mut wb_entry: Option<Entry> = None;
    let mut book_entry: Option<Entry> = None;
    for i in 0..(dir_bytes.len() / 128) {
        let b = &dir_bytes[i * 128..(i + 1) * 128];
        let name_len = u16le(b, 64) as usize;
        if !(2..=64).contains(&name_len) {
            continue;
        }
        let raw: Vec<u16> = (0..(name_len / 2 - 1)).map(|k| u16le(b, k * 2)).collect();
        let name = String::from_utf16_lossy(&raw);
        let kind = b[66];
        let start = u32le(b, 116);
        let size = if sector_shift == 12 {
            u32le(b, 120) as u64 | ((u32le(b, 124) as u64) << 32)
        } else {
            u32le(b, 120) as u64
        };
        if kind == 5 && root_entry.is_none() {
            root_entry = Some(Entry { name, kind, start, size });
        } else if kind == 2 && name.eq_ignore_ascii_case("Workbook") && wb_entry.is_none() {
            wb_entry = Some(Entry { name, kind, start, size });
        } else if kind == 2 && name.eq_ignore_ascii_case("Book") && book_entry.is_none() {
            book_entry = Some(Entry { name, kind, start, size });
        }
    }

    let target = wb_entry.or(book_entry)?;
    if target.size >= mini_cutoff as u64 {
        Some(read_file_chain(&mut f, target.start, usize::try_from(target.size).ok()))
    } else {
        let root = root_entry?;
        let mini_stream = read_file_chain(&mut f, root.start, None);
        let mut minifat = Vec::new();
        if n_minifat > 0 && minifat_start != FREE {
            let mf = read_file_chain(&mut f, minifat_start, None);
            for i in 0..(mf.len() / 4) {
                minifat.push(u32le(&mf, i * 4));
            }
        }
        let target_len = usize::try_from(target.size).ok()?;
        let mut out = Vec::with_capacity(target_len);
        let mut cur = target.start;
        let mut guard = 0usize;
        while cur != END && cur != FREE && out.len() < target_len && guard < 100_000 {
            let base = cur as usize * mini_size;
            if base + mini_size > mini_stream.len() {
                break;
            }
            out.extend_from_slice(&mini_stream[base..base + mini_size]);
            cur = *minifat.get(cur as usize).unwrap_or(&END);
            guard += 1;
        }
        out.truncate(target_len);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid 512-byte-sector CFB file with custom FAT layout and Workbook stream.
    fn build_synthetic_cfb(sector_order: &[u32], payload: &[u8]) -> Vec<u8> {
        let sector_size = 512usize;
        let max_data_sid = sector_order.iter().copied().max().unwrap_or(0);
        let dir_sid = max_data_sid + 1;
        let fat_sid = dir_sid + 1;
        let total_sectors = (fat_sid + 1) as usize;
        let mut img = vec![0u8; (total_sectors + 1) * sector_size];

        // Header (512 bytes)
        img[0..8].copy_from_slice(&SIGNATURE);
        img[30..32].copy_from_slice(&9u16.to_le_bytes()); // sector_shift = 9 (512B)
        img[32..34].copy_from_slice(&6u16.to_le_bytes()); // mini_shift = 6 (64B)
        img[44..48].copy_from_slice(&1u32.to_le_bytes()); // n_fat = 1
        img[48..52].copy_from_slice(&dir_sid.to_le_bytes()); // dir_start
        img[56..60].copy_from_slice(&4096u32.to_le_bytes()); // mini_cutoff = 4096
        img[60..64].copy_from_slice(&END.to_le_bytes()); // minifat_start
        img[64..68].copy_from_slice(&0u32.to_le_bytes()); // n_minifat = 0
        img[68..72].copy_from_slice(&END.to_le_bytes()); // difat_start
        img[72..76].copy_from_slice(&0u32.to_le_bytes()); // n_difat = 0
        for i in 0..109 {
            let v = if i == 0 { fat_sid } else { FREE };
            img[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
        }

        // Write payload slices according to `sector_order`
        for (k, &sid) in sector_order.iter().enumerate() {
            let src_start = k * sector_size;
            let src_end = (src_start + sector_size).min(payload.len());
            if src_start < src_end {
                let dst = (sid as usize + 1) * sector_size;
                img[dst..dst + (src_end - src_start)].copy_from_slice(&payload[src_start..src_end]);
            }
        }

        // Directory sector: entry 0 = Root Entry, entry 1 = Workbook
        let dir_off = (dir_sid as usize + 1) * sector_size;
        let write_entry = |buf: &mut [u8], name: &str, kind: u8, start: u32, size: u32| {
            let utf16: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            for (i, u) in utf16.iter().enumerate() {
                buf[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
            }
            buf[64..66].copy_from_slice(&((utf16.len() * 2) as u16).to_le_bytes());
            buf[66] = kind;
            buf[116..120].copy_from_slice(&start.to_le_bytes());
            buf[120..124].copy_from_slice(&size.to_le_bytes());
        };
        write_entry(&mut img[dir_off..dir_off + 128], "Root Entry", 5, END, 0);
        write_entry(
            &mut img[dir_off + 128..dir_off + 256],
            "Workbook",
            2,
            sector_order[0],
            payload.len() as u32,
        );

        // FAT sector
        let fat_off = (fat_sid as usize + 1) * sector_size;
        for i in 0..(sector_size / 4) {
            img[fat_off + i * 4..fat_off + i * 4 + 4].copy_from_slice(&FREE.to_le_bytes());
        }
        for w in sector_order.windows(2) {
            let p = fat_off + w[0] as usize * 4;
            img[p..p + 4].copy_from_slice(&w[1].to_le_bytes());
        }
        let last_p = fat_off + *sector_order.last().unwrap() as usize * 4;
        img[last_p..last_p + 4].copy_from_slice(&END.to_le_bytes());
        let dir_p = fat_off + dir_sid as usize * 4;
        img[dir_p..dir_p + 4].copy_from_slice(&END.to_le_bytes());
        let fat_p = fat_off + fat_sid as usize * 4;
        img[fat_p..fat_p + 4].copy_from_slice(&0xFFFF_FFFDu32.to_le_bytes());

        img
    }

    #[test]
    fn coalesced_fat_reads_contiguous_fragmented_and_unaligned_streams() {
        let dir = tempfile::tempdir().unwrap();
        // 5000 bytes (> 4096 mini_cutoff, unaligned to 512B -> 10 sectors, last sector partial)
        let payload: Vec<u8> = (0..5000usize).map(|i| ((i * 31 + 7) & 0xFF) as u8).collect();

        // 1) Pure contiguous run 0..10
        let p1 = dir.path().join("contiguous.xls");
        std::fs::write(&p1, build_synthetic_cfb(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9], &payload)).unwrap();
        assert_eq!(read_workbook_stream_file(&p1).unwrap(), payload);

        // 2) Mixed contiguous runs & jumps: [0,1,2] -> [6,7] -> [3,4,5] -> [9] -> [8]
        let p2 = dir.path().join("fragmented.xls");
        std::fs::write(&p2, build_synthetic_cfb(&[0, 1, 2, 6, 7, 3, 4, 5, 9, 8], &payload)).unwrap();
        assert_eq!(read_workbook_stream_file(&p2).unwrap(), payload);

        // 3) Truncated stream whose last FAT sector lies past EOF stops cleanly without panic
        let mut trunc_img = build_synthetic_cfb(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9], &payload);
        // Point sector 9 to an out-of-bounds sector 99 and claim size 6000
        let dir_off = (10usize + 1) * 512;
        trunc_img[dir_off + 128 + 120..dir_off + 128 + 124].copy_from_slice(&6000u32.to_le_bytes());
        let fat_off = (11usize + 1) * 512;
        trunc_img[fat_off + 9 * 4..fat_off + 9 * 4 + 4].copy_from_slice(&99u32.to_le_bytes());
        let p3 = dir.path().join("truncated.xls");
        std::fs::write(&p3, &trunc_img).unwrap();
        let got = read_workbook_stream_file(&p3).unwrap();
        assert_eq!(got.len(), 10 * 512);
        assert_eq!(&got[..5000], payload.as_slice());
    }
}
