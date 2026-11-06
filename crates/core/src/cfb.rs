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
