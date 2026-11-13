//! 辅助功能检查器：扫出影响读屏/键盘操作的常见问题。
//!
//! 对应 Excel「辅助功能检查器」里我们能真判定的子集（不做假装全量）：
//! - 合并单元格（读屏会跳过被合并的格子）
//! - 表头行有空单元格（表格对象/筛选区域的第一行）
//! - 图片没有可替换文字（Excel 的 alt text；我们只存部件名，没有描述就算缺失）
//! - 文本框没有文字
//! - 工作表还是默认名（Sheet1/Sheet2…）
//! - 迷你图所在单元格没有表头说明（列的表头行是空的）

use crate::model::Workbook;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    MergedCells,
    EmptyHeader,
    PictureAltText,
    EmptyTextBox,
    DefaultSheetName,
    SparklineHeader,
}

impl IssueKind {
    pub fn label(self) -> &'static str {
        match self {
            IssueKind::MergedCells => "合并单元格",
            IssueKind::EmptyHeader => "表头有空单元格",
            IssueKind::PictureAltText => "图片缺少可替换文字",
            IssueKind::EmptyTextBox => "文本框没有文字",
            IssueKind::DefaultSheetName => "工作表使用默认名称",
            IssueKind::SparklineHeader => "迷你图缺少表头说明",
        }
    }
    pub fn advice(self) -> &'static str {
        match self {
            IssueKind::MergedCells => "读屏会跳过合并区域里被覆盖的单元格；尽量用「跨列居中」代替合并",
            IssueKind::EmptyHeader => "表头行留空会让读屏念不出列名，补一个列名",
            IssueKind::PictureAltText => "给图片填「可替换文字」，读屏才知道它画的是什么",
            IssueKind::EmptyTextBox => "空文本框会打断阅读顺序，填上文字或删掉",
            IssueKind::DefaultSheetName => "用有意义的工作表名（如「一月销售」）代替 Sheet1",
            IssueKind::SparklineHeader => "迷你图本身没有文字，靠表头说明它画的是哪一列",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub sheet: usize,
    pub sheet_name: String,
    pub kind: IssueKind,
    /// 位置（合并区域给左上角）
    pub at: Option<(u32, u32)>,
    /// 具体说明
    pub detail: String,
}

/// 全簿扫描
pub fn scan(wb: &Workbook) -> Vec<Issue> {
    let mut out = Vec::new();
    for (si, sh) in wb.sheets.iter().enumerate() {
        for m in &sh.merges {
            out.push(Issue {
                sheet: si,
                sheet_name: sh.name.clone(),
                kind: IssueKind::MergedCells,
                at: Some((m.0, m.1)),
                detail: format!(
                    "{}-{} 合并了 {} 行 × {} 列",
                    crate::model::cell_name(m.0, m.1),
                    crate::model::cell_name(m.2, m.3),
                    m.2 - m.0 + 1,
                    m.3 - m.1 + 1
                ),
            });
        }
        // 表头：有筛选/表格就用它们的表头行与列范围，否则用已用区域的左上角那一行
        let header_span: Option<(u32, u32, u32)> = sh
            .filter
            .as_ref()
            .map(|f| (f.range.0, f.range.1, f.range.3))
            .or_else(|| sh.tables.first().map(|t| (t.range.0, t.range.1, t.range.3)))
            .or_else(|| sh.used_bounds().map(|(r1, c1, _, c2)| (r1, c1, c2)));
        let header_row = header_span.map(|(r, _, _)| r);
        if let Some((hr, hc1, hc2)) = header_span {
            for c in hc1..=hc2.min(hc1 + 200) {
                if sh.value(hr, c).is_empty() {
                    out.push(Issue {
                        sheet: si,
                        sheet_name: sh.name.clone(),
                        kind: IssueKind::EmptyHeader,
                        at: Some((hr, c)),
                        detail: format!("{} 是空的", crate::model::cell_name(hr, c)),
                    });
                }
            }
        }
        for p in &sh.pictures {
            out.push(Issue {
                sheet: si,
                sheet_name: sh.name.clone(),
                kind: IssueKind::PictureAltText,
                at: Some((p.rect.0, p.rect.1)),
                detail: format!("图片部件 {}（位置 {}）", p.part, crate::model::cell_name(p.rect.0, p.rect.1)),
            });
        }
        for tb in &sh.textboxes {
            if tb.text.trim().is_empty() {
                out.push(Issue {
                    sheet: si,
                    sheet_name: sh.name.clone(),
                    kind: IssueKind::EmptyTextBox,
                    at: None,
                    detail: format!("{} 没有文字", tb.name),
                });
            }
        }
        if is_default_sheet_name(&sh.name) {
            out.push(Issue {
                sheet: si,
                sheet_name: sh.name.clone(),
                kind: IssueKind::DefaultSheetName,
                at: None,
                detail: format!("名字是「{}」", sh.name),
            });
        }
        // 迷你图：表头行同一列要有文字
        for sp in &sh.sparklines {
            let hr = header_row.unwrap_or(0);
            if hr != sp.r && sh.value(hr, sp.c).is_empty() {
                out.push(Issue {
                    sheet: si,
                    sheet_name: sh.name.clone(),
                    kind: IssueKind::SparklineHeader,
                    at: Some((sp.r, sp.c)),
                    detail: format!(
                        "{} 的迷你图，表头 {} 是空的",
                        crate::model::cell_name(sp.r, sp.c),
                        crate::model::cell_name(hr, sp.c)
                    ),
                });
            }
        }
    }
    out
}

fn is_default_sheet_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "sheet" {
        return true;
    }
    let Some(rest) = lower.strip_prefix("sheet") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    #[test]
    fn flags_merges_and_default_names() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].merge_range(0, 0, 1, 1);
        let issues = scan(&wb);
        assert!(
            issues.iter().any(|i| i.kind == IssueKind::MergedCells),
            "合并要报"
        );
        assert!(
            issues.iter().any(|i| i.kind == IssueKind::DefaultSheetName),
            "Sheet1 默认名要报"
        );
    }

    #[test]
    fn flags_empty_header_when_table_present() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "销售".into();
        wb.sheets[0].tables.push(crate::table::Table {
                column_ids: Vec::new(),
            name: "T1".into(),
            display: "T1".into(),
            range: (0, 0, 3, 1),
            columns: vec!["姓名".into(), "金额".into()],
            header_row: true,
            totals_row: false,
        });
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("姓名".into()), formula: None });
        wb.sheets[0].set(1, 0, Cell { value: Value::Text("甲".into()), formula: None });
        let issues = scan(&wb);
        let empty: Vec<_> = issues
            .iter()
            .filter(|i| i.kind == IssueKind::EmptyHeader)
            .collect();
        assert_eq!(empty.len(), 1, "B 列表头空：{issues:?}");
        assert!(empty[0].detail.contains("B1"), "{}", empty[0].detail);
    }

    #[test]
    fn flags_empty_textbox_and_sparkline_header() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "数据".into();
        wb.sheets[0].textboxes.push(crate::textbox::TextBox::default());
        let mut sp = crate::sparkline::Sparkline::default();
        sp.r = 2;
        sp.c = 3;
        wb.sheets[0].sparklines.push(sp);
        let issues = scan(&wb);
        assert!(issues.iter().any(|i| i.kind == IssueKind::EmptyTextBox));
        assert!(issues.iter().any(|i| i.kind == IssueKind::SparklineHeader));
    }

    #[test]
    fn clean_sheet_reports_nothing() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "一月销售".into();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("金额".into()), formula: None });
        assert!(scan(&wb).is_empty(), "{:?}", scan(&wb));
    }

    #[test]
    fn default_name_detection() {
        assert!(is_default_sheet_name("Sheet1"));
        assert!(is_default_sheet_name("sheet12"));
        assert!(!is_default_sheet_name("Sheet1 副本"));
        assert!(!is_default_sheet_name("一月"));
    }
}
