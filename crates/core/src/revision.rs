//! 单元格级修订日志：谁、什么时候、改了哪格、改之前是什么。
//!
//! 不做 Excel 那套「共享工作簿 + revisions 部件」（365 已弃用，LibreOffice 也不认），
//! 改成随文件走的一份 JSONL：
//!
//! ```text
//! <工作簿目录>/.~sheetflow-history/<文件名>/revisions.jsonl
//! ```
//!
//! 每行一条动作（一次编辑/粘贴/格式刷/结构改动），只存受影响的格子，不存整表快照。
//! 放同目录的理由和版本备份一样：共享盘上换个人打开也能看到历史。

use crate::model::{Cell, Value};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 一条动作里最多记多少格（大块粘贴只记前 N 格 + 总数）
pub const MAX_CELLS: usize = 200;
/// 日志超过这个大小就轮转
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// 某一格的快照（值 + 公式 + 类型）
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Snap {
    #[serde(default)]
    pub v: String,
    #[serde(default)]
    pub f: Option<String>,
    /// n 数字 / s 文本 / b 布尔 / e 空
    #[serde(default)]
    pub t: String,
}

impl Snap {
    pub fn from_cell(c: &Cell) -> Self {
        let t = match &c.value {
            Value::Number(_) => "n",
            Value::Bool(_) => "b",
            Value::Empty => "e",
            Value::Text(_) => "s",
            // "x" 是错误值：旧版本没有这个分支，读到时按文本兜底
            Value::Error(_) => "x",
        };
        Self {
            v: c.value.display(),
            f: c.formula.as_deref().map(|s| s.to_string()),
            t: t.to_string(),
        }
    }

    pub fn to_cell(&self) -> Cell {
        let value = match self.t.as_str() {
            "n" => self.v.parse::<f64>().map(Value::Number).unwrap_or(Value::Empty),
            "b" => Value::Bool(self.v.eq_ignore_ascii_case("true") || self.v == "1"),
            "e" => Value::Empty,
            "x" => crate::formula::ErrKind::from_text(&self.v)
                .map(Value::Error)
                .unwrap_or_else(|| Value::Text(self.v.as_str().into())),
            _ => {
                if self.v.is_empty() {
                    Value::Empty
                } else {
                    Value::Text(self.v.as_str().into())
                }
            }
        };
        Cell {
            value,
            formula: self.f.as_deref().map(|s| s.into()),
        }
    }

    pub fn same(&self, c: Option<&Cell>) -> bool {
        match c {
            Some(c) => Snap::from_cell(c) == *self,
            None => *self == Snap::default(),
        }
    }

    /// 一行摘要：`10` / `=A1*2` / `(空)`
    pub fn summary(&self) -> String {
        match (&self.f, self.v.as_str()) {
            (Some(f), _) => f.clone(),
            (None, "") => "(空)".into(),
            (None, v) => v.to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub r: u32,
    pub c: u32,
    pub before: Snap,
    pub after: Snap,
    /// 格式也变了才带上（整份 CellStyle，撤回时能还原字体/底色/边框）
    #[serde(default)]
    pub before_style: Option<crate::style::CellStyle>,
    #[serde(default)]
    pub after_style: Option<crate::style::CellStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// unix 毫秒
    pub ts: u64,
    pub who: String,
    #[serde(default)]
    pub site: u32,
    /// 动作名：编辑单元格 / 从 Excel 粘贴 / 设置格式…
    pub label: String,
    pub sheet: String,
    #[serde(default)]
    pub cells: Vec<Change>,
    /// 大块动作或结构改动的说明（"共 1200 格" / "插入 2 行 @3"）
    #[serde(default)]
    pub note: String,
    /// 远端来的改动：只展示，不提供"撤回"（本地没有它的 undo 上下文）
    #[serde(default)]
    pub remote: bool,
}

impl Entry {
    pub fn new(who: &str, site: u32, label: &str, sheet: &str) -> Self {
        Self {
            ts: now_ms(),
            who: who.to_string(),
            site,
            label: label.to_string(),
            sheet: sheet.to_string(),
            cells: Vec::new(),
            note: String::new(),
            remote: false,
        }
    }

    /// 位置摘要：`B10` 或 `B10:D20`
    pub fn range(&self) -> String {
        let Some(first) = self.cells.first() else {
            return String::new();
        };
        let (mut r1, mut c1, mut r2, mut c2) = (first.r, first.c, first.r, first.c);
        for ch in &self.cells {
            r1 = r1.min(ch.r);
            c1 = c1.min(ch.c);
            r2 = r2.max(ch.r);
            c2 = c2.max(ch.c);
        }
        let a = format!("{}{}", crate::model::col_label(c1), r1 + 1);
        if (r1, c1) == (r2, c2) {
            a
        } else {
            format!("{}:{}{}", a, crate::model::col_label(c2), r2 + 1)
        }
    }

    /// 前后变化的简短描述
    pub fn delta_text(&self) -> String {
        let Some(ch) = self.cells.first() else {
            return self.note.clone();
        };
        if self.cells.len() == 1 {
            let same_value = ch.before.summary() == ch.after.summary();
            if same_value {
                // 值没变，那就是格式变了：把字体变化说出来，否则一行"x → x"看不出改了什么
                if let (Some(b), Some(a)) = (&ch.before_style, &ch.after_style) {
                    let name = |f: &str| if f.is_empty() { "默认".to_string() } else { f.to_string() };
                    let mut bits = Vec::new();
                    if b.font != a.font {
                        bits.push(format!("字体 {} → {}", name(&b.font), name(&a.font)));
                    }
                    if b.size != a.size {
                        bits.push(format!("字号 {} → {}", b.size, a.size));
                    }
                    if (b.bold, b.italic) != (a.bold, a.italic) {
                        bits.push("字形变化".into());
                    }
                    if b.fill != a.fill {
                        bits.push("底色变化".into());
                    }
                    if b.format != a.format {
                        bits.push(format!("格式 {} → {}", b.format, a.format));
                    }
                    if !bits.is_empty() {
                        return format!("格式：{}", bits.join("，"));
                    }
                }
                format!("{}（无变化）", ch.after.summary())
            } else {
                format!("{} → {}", ch.before.summary(), ch.after.summary())
            }
        } else {
            format!("{} 格（{}）", self.cells.len(), self.note)
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `<工作簿目录>/.~sheetflow-history/<文件名>/`
pub fn dir_for(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    dir.join(".~sheetflow-history").join(name)
}

pub fn log_path(file: &Path) -> PathBuf {
    dir_for(file).join("revisions.jsonl")
}

/// 追加一条；日志太大先轮转
pub fn append(file: &Path, e: &Entry) -> std::io::Result<()> {
    let dir = dir_for(file);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("revisions.jsonl");
    if let Ok(m) = std::fs::metadata(&path) {
        if m.len() > ROTATE_BYTES {
            let _ = std::fs::rename(&path, dir.join("revisions.1.jsonl"));
        }
    }
    let mut line = serde_json::to_string(e).unwrap_or_default();
    line.push('\n');
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(line.as_bytes())
}

/// 读最后 `limit` 条（新的在后）
pub fn load(file: &Path, limit: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(log_path(file)) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = text
        .lines()
        .rev()
        .take(limit.max(1) * 4)
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect();
    out.reverse();
    if out.len() > limit {
        out.drain(0..out.len() - limit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("sheet_revision").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("账.xlsx")
    }

    #[test]
    fn snap_roundtrip() {
        let c = Cell {
            value: Value::Number(12.5),
            formula: Some("=A1*2".into()),
        };
        let s = Snap::from_cell(&c);
        assert_eq!(s.t, "n");
        assert_eq!(s.v, "12.5");
        assert_eq!(s.f.as_deref(), Some("=A1*2"));
        let back = s.to_cell();
        assert_eq!(back.value.display(), "12.5");
        assert_eq!(back.formula.as_deref(), Some("=A1*2"));
        assert!(s.same(Some(&c)));

        let t = Snap::from_cell(&Cell { value: Value::Text("你好".into()), formula: None });
        assert_eq!(t.t, "s");
        assert_eq!(t.to_cell().value.display(), "你好");
    }

    #[test]
    fn append_and_load_last_entries() {
        let f = tmp("load");
        for i in 0..5u32 {
            let mut e = Entry::new("张三", 0, "编辑单元格", "Sheet1");
            e.cells.push(Change {
                r: i,
                c: 0,
                before: Snap::default(),
                after: Snap {
                    v: i.to_string(),
                    f: None,
                    t: "n".into(),
                },
                before_style: None,
                after_style: None,
            });
            append(&f, &e).unwrap();
        }
        let all = load(&f, 100);
        assert_eq!(all.len(), 5);
        assert_eq!(all[4].cells[0].r, 4);
        let last2 = load(&f, 2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[1].cells[0].r, 4);
    }

    #[test]
    fn log_lives_next_to_workbook() {
        let f = tmp("dir");
        let p = log_path(&f);
        assert!(p.ends_with(".~sheetflow-history/账.xlsx/revisions.jsonl"), "{p:?}");
    }

    #[test]
    fn entry_range_and_text() {
        let mut e = Entry::new("我", 0, "粘贴", "Sheet1");
        for (r, c) in [(0u32, 1u32), (1, 2)] {
            e.cells.push(Change {
                r,
                c,
                before: Snap::default(),
                after: Snap { v: "1".into(), f: None, t: "n".into() },
                before_style: None,
                after_style: None,
            });
        }
        assert_eq!(e.range(), "B1:C2");
        assert!(e.delta_text().contains("2 格"));
    }
}
