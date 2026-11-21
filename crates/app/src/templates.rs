//! 开始页的内置工作簿模板（仅保留空白工作簿，在内存里直接构建）。

use sheetcore::model::Workbook;
use std::sync::OnceLock;

pub struct Template {
    pub name: &'static str,
    pub build: fn() -> Workbook,
}

pub const ALL: [Template; 1] = [Template {
    name: "空白工作簿",
    build: blank,
}];

pub fn build(idx: usize) -> Workbook {
    (ALL.get(idx).map(|t| t.build).unwrap_or(blank))()
}

fn blank() -> Workbook {
    Workbook::new_blank()
}

// ---------------------------------------------------------------- 缩略图数据

pub struct Preview {
    pub cells: Vec<Vec<PreviewCell>>,
}

pub struct PreviewCell {
    pub text: String,
    pub fill: Option<[u8; 3]>,
    pub bold: bool,
}

pub fn previews() -> &'static [Preview] {
    static CACHE: OnceLock<Vec<Preview>> = OnceLock::new();
    CACHE.get_or_init(|| {
        (0..ALL.len())
            .map(|i| {
                let wb = build(i);
                let sh = &wb.sheets[0];
                let mut rows = Vec::new();
                for r in 0..3 {
                    let mut row = Vec::new();
                    for c in 0..4 {
                        let text = sh
                            .get(r, c)
                            .map(|cell| cell.value.display())
                            .unwrap_or_default();
                        let st = wb.styles.get(sh.style_of(r, c));
                        row.push(PreviewCell {
                            text,
                            fill: st.fill,
                            bold: st.bold,
                        });
                    }
                    rows.push(row);
                }
                Preview { cells: rows }
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::model::Value;

    #[test]
    fn every_template_builds_and_recalcs() {
        assert_eq!(ALL.len(), 1, "仅保留空白工作簿，不包含预置业务模板");
        for (i, t) in ALL.iter().enumerate() {
            let wb = build(i);
            assert!(!wb.sheets.is_empty(), "{} 没有工作表", t.name);
            assert!(!wb.sheets[0].name.is_empty());
            for (si, sheet) in wb.sheets.iter().enumerate() {
                for (cr, cell) in sheet.iter() {
                    assert!(
                        !matches!(cell.value, Value::Error(_)),
                        "{} 表{} ({},{}) 出现公式错误 {:?}",
                        t.name,
                        si,
                        cr.row,
                        cr.col,
                        cell.value
                    );
                }
            }
        }
        let fallback = build(99);
        assert_eq!(fallback.sheets.len(), 1);
        assert!(fallback.sheets[0].is_empty());
    }
}
