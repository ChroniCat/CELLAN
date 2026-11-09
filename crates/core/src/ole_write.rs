//! 最小 OLE2（CFB v3）写容器：只写一条 `Workbook` 流。
//!
//! 为什么不直接用 `cfb` crate：它写出来的文件 Excel 能开，但 calamine 的 CFB
//! 解析器会报 "Empty Root directory"，导致我们自己写出的 .xls 我们自己也读不了。
//! 这里按规范手写：512 字节扇区、一条 FAT、一个目录扇区，流一律放常规扇区
//! （调用方负责把流补到 ≥4096 字节，避开 mini-stream）。

use anyhow::{bail, Result};
use std::path::Path;

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const SECTOR: usize = 512;
const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const FREESECT: u32 = 0xFFFF_FFFF;
const FATSECT: u32 = 0xFFFF_FFFD;

fn put_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// 目录项（128 字节）
fn dir_entry(name: &str, kind: u8, start: u32, size: u64, child: u32) -> [u8; 128] {
    let mut e = [0u8; 128];
    let units: Vec<u16> = name.encode_utf16().collect();
    let n = units.len().min(31);
    for (i, u) in units.iter().take(n).enumerate() {
        put_u16(&mut e, i * 2, *u);
    }
    put_u16(&mut e, 64, ((n + 1) * 2) as u16); // 名字字节数（含结尾 0）
    e[66] = kind; // 1=storage 2=stream 5=root
    e[67] = 1; // 黑色节点
    put_u32(&mut e, 68, FREESECT); // 左兄弟
    put_u32(&mut e, 72, FREESECT); // 右兄弟
    put_u32(&mut e, 76, child); // 子节点
    put_u32(&mut e, 116, start);
    put_u32(&mut e, 120, size as u32);
    put_u32(&mut e, 124, (size >> 32) as u32);
    e
}

/// 写一个只含 `Workbook` 流的 CFB。`stream` 必须 ≥ 4096 字节。
pub fn write(path: &Path, stream: &[u8]) -> Result<()> {
    if stream.len() < 4096 {
        bail!("Workbook 流太小（{} 字节），会落进 mini-stream", stream.len());
    }
    let n_stream = stream.len().div_ceil(SECTOR);
    // 扇区布局：0=FAT，1=目录，2..=流
    let total_sectors = 2 + n_stream;

    let mut out = vec![0u8; SECTOR]; // 头部
    out[0..8].copy_from_slice(&SIGNATURE);
    put_u16(&mut out, 24, 0x003E); // minor
    put_u16(&mut out, 26, 0x0003); // major：512 字节扇区
    put_u16(&mut out, 28, 0xFFFE); // 小端
    put_u16(&mut out, 30, 9); // sector shift
    put_u16(&mut out, 32, 6); // mini sector shift
    put_u32(&mut out, 40, 0); // 目录扇区数（v3 必须 0）
    put_u32(&mut out, 44, 1); // FAT 扇区数
    put_u32(&mut out, 48, 1); // 目录起始扇区
    put_u32(&mut out, 56, 4096); // mini stream 截断
    put_u32(&mut out, 60, ENDOFCHAIN); // mini FAT 起始
    put_u32(&mut out, 64, 0); // mini FAT 数
    put_u32(&mut out, 68, ENDOFCHAIN); // DIFAT 起始
    put_u32(&mut out, 72, 0); // DIFAT 数
    put_u32(&mut out, 76, 0); // DIFAT[0] = FAT 扇区 0
    for i in 1..109 {
        put_u32(&mut out, 76 + i * 4, FREESECT);
    }

    // FAT
    let mut fat = vec![0u8; SECTOR];
    put_u32(&mut fat, 0, FATSECT); // 扇区 0 是 FAT 自己
    put_u32(&mut fat, 4, ENDOFCHAIN); // 扇区 1 是目录
    for i in 0..n_stream {
        let id = 2 + i as u32;
        let next = if i + 1 == n_stream { ENDOFCHAIN } else { id + 1 };
        put_u32(&mut fat, id as usize * 4, next);
    }
    for i in (2 + n_stream)..(SECTOR / 4) {
        put_u32(&mut fat, i * 4, FREESECT);
    }
    out.extend_from_slice(&fat);

    // 目录：根 + Workbook
    let mut dir = Vec::with_capacity(SECTOR);
    dir.extend_from_slice(&dir_entry("Root Entry", 5, ENDOFCHAIN, 0, 1));
    dir.extend_from_slice(&dir_entry("Workbook", 2, 2, stream.len() as u64, FREESECT));
    dir.resize(SECTOR, 0);
    out.extend_from_slice(&dir);

    out.extend_from_slice(stream);
    out.resize(SECTOR * (2 + total_sectors), 0);

    std::fs::write(path, &out)?;
    Ok(())
}
