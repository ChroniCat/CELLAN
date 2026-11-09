//! 保存前的历史版本：每次覆盖前把旧文件留一份。
//!
//! 放在工作簿旁边的 `.~sheetflow-history/<文件名>/` 里，时间戳命名。
//! 保留最近 N 份（默认 10），超出就删最旧的——共享盘上不能无限长胖。
//!
//! 之所以放在同一目录：用户用的是网盘（Z: 之类），历史留本机就跟着机器走了，
//! 换个人打开就看不到；放同目录则跟文件在一起，谁都能翻。

use std::path::{Path, PathBuf};

/// 默认保留几份
pub const KEEP: usize = 10;

pub fn history_dir(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    dir.join(".~sheetflow-history").join(name)
}

fn stamp() -> String {
    // 20261108-193045
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_stamp(now)
}

fn format_stamp(now: u64) -> String {
    let secs = now % 86_400;
    let days = now / 86_400;
    // 简化：用天数偏移算日期（从 1970 起），只用于生成有序名字，不要求日历精确到闰秒
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

// Older builds applied the civil-date offset twice (e.g. 2026 became 3996).
// Normalize those names when ordering existing history; otherwise every new
// correctly dated backup would be pruned before the old versions.
fn normalized_date(date: &str) -> String {
    let year = date[..4].parse::<i64>().unwrap_or(0);
    let month = date[4..6].parse::<i64>().unwrap_or(0);
    let day = date[6..8].parse::<i64>().unwrap_or(0);
    if year < 3900 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return date.to_owned();
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let (y, m, d) = civil_from_days(days - 719_468);
    format!("{y:04}{m:02}{d:02}")
}

/// 从 1970-01-01 起的天数换算成年月日（Howard Hinnant 的算法）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[derive(Clone, Debug)]
pub struct Version {
    pub path: PathBuf,
    pub stamp: String,
    pub bytes: u64,
}

/// 保存前调用：把当前磁盘上的旧内容复制进历史目录。
///
/// 目标还不存在（第一次保存）时什么都不做。
pub fn keep_previous(file: &Path) -> std::io::Result<Option<PathBuf>> {
    if !file.exists() {
        return Ok(None);
    }
    let dir = history_dir(file);
    std::fs::create_dir_all(&dir)?;
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook.xlsx".into());
    let stamp = stamp();
    // Pruning may have removed the unsuffixed version already. Continue past
    // the highest sequence instead of reusing a hole that would sort oldest.
    let mut n = list(file).iter().filter_map(|v| {
        let suffix = v.stamp.strip_prefix(&stamp)?;
        if suffix.is_empty() { Some(0) } else { suffix.strip_prefix('-')?.parse::<u64>().ok() }
    }).max().map_or(0, |n| n + 1);
    let target_for = |n| dir.join(if n == 0 { format!("{stamp}-{name}") } else { format!("{stamp}-{n}-{name}") });
    let mut target = target_for(n);
    // 同一秒保存两次：加序号，别互相覆盖
    while target.exists() {
        n += 1;
        target = target_for(n);
    }
    std::fs::copy(file, &target)?;
    let _ = prune(file, KEEP);
    Ok(Some(target))
}

/// 列出历史版本，新的在前
pub fn list(file: &Path) -> Vec<Version> {
    let dir = history_dir(file);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let original_name = file.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out: Vec<(Version, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().to_string();
            let prefix = name.strip_suffix(&format!("-{original_name}"))?;
            let mut pieces = prefix.split('-');
            let date = pieces.next()?;
            let time = pieces.next()?;
            if date.len() != 8 || time.len() != 6 || !date.bytes().chain(time.bytes()).all(|b| b.is_ascii_digit()) {
                return None;
            }
            let sequence = pieces.next().map(str::parse::<u64>).transpose().ok()?.unwrap_or(0);
            if pieces.next().is_some() { return None; }
            let date = normalized_date(date);
            let stamp = if sequence == 0 { format!("{date}-{time}") } else { format!("{date}-{time}-{sequence}") };
            let bytes = e.metadata().ok().map(|m| m.len()).unwrap_or(0);
            Some((Version { path, stamp, bytes }, sequence))
        })
        .collect();
    out.sort_by(|(a, a_seq), (b, b_seq)| b.stamp[..15].cmp(&a.stamp[..15]).then_with(|| b_seq.cmp(a_seq)));
    out.into_iter().map(|(version, _)| version).collect()
}

/// 只保留最近 `keep` 份
pub fn prune(file: &Path, keep: usize) -> std::io::Result<usize> {
    let all = list(file);
    let mut removed = 0;
    for v in all.into_iter().skip(keep) {
        if std::fs::remove_file(&v.path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sheetflow_hist_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn keeps_previous_version_before_overwrite() {
        let d = tmp_dir("book");
        let f = d.join("book.xlsx");
        std::fs::write(&f, b"v1").unwrap();
        let saved = keep_previous(&f).unwrap().expect("应当留一份");
        assert!(saved.exists());
        assert_eq!(std::fs::read(&saved).unwrap(), b"v1");

        std::fs::write(&f, b"v2").unwrap();
        let _ = keep_previous(&f).unwrap().expect("第二份");
        assert_eq!(list(&f).len(), 2, "两次保存应有两份历史");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn first_save_has_no_history() {
        let d = tmp_dir("new");
        let f = d.join("new.xlsx");
        assert!(keep_previous(&f).unwrap().is_none(), "文件还不存在时不该留空历史");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn prune_keeps_newest() {
        let d = tmp_dir("prune");
        let f = d.join("p.xlsx");
        std::fs::write(&f, b"x").unwrap();
        for i in 0..6 {
            std::fs::write(&f, format!("v{i}")).unwrap();
            let _ = keep_previous(&f);
        }
        assert_eq!(list(&f).len(), 6);
        prune(&f, 3).unwrap();
        assert_eq!(list(&f).len(), 3, "只留最近 3 份");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn atomic_save_leaves_history() {
        let d = tmp_dir("atomic");
        let f = d.join("book.xlsx");
        let mut wb = crate::model::Workbook::new_blank();
        crate::xlsx::save_atomic(&f, &wb).expect("第一次保存");
        assert!(list(&f).is_empty(), "第一次保存没有旧版本可留");
        wb.sheets[0].set(
            0,
            0,
            crate::model::Cell { value: crate::model::Value::Text("改过".into()), formula: None },
        );
        crate::xlsx::save_atomic(&f, &wb).expect("第二次保存");
        assert_eq!(list(&f).len(), 1, "第二次保存应留下一份历史");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn civil_dates_are_sane() {
        let (y, m, d) = civil_from_days(0);
        assert_eq!((y, m, d), (1970, 1, 1));
        assert_eq!(format_stamp(0), "19700101-000000");
        assert_eq!(format_stamp(86_401), "19700102-000001");
        let (y, m, d) = civil_from_days(20_767 + 719_468);
        assert_eq!(normalized_date(&format!("{y:04}{m:02}{d:02}")), format_stamp(20_767 * 86_400)[..8]);
    }
}
