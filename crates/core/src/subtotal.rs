//! 分类汇总：按某一列把**相邻的相同值**归成一组，每组下面插一行小计，末尾加总计。
//!
//! 和 Excel 一致的口径：
//! - 只对相邻同值分组（Excel 也要求先按该列排序，界面上会提示）
//! - 小计用 `SUBTOTAL(9/1/2/4/5, 区域)`，不是 SUM —— 这样以后再嵌套分类汇总
//!   或手动筛选时，小计不会把自己算进去
//! - 明细行打上分级显示级别 1，小计行是级别 2 的父级位置（汇总行在下方）
//! - 插入行复用 `Sheet::insert_rows`，所以条件格式/数据验证/批注会跟着一起移动

use crate::model::{Cell, Sheet, Value, Workbook};
#[cfg(test)]
use crate::structural::Structural;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SubFn {
    #[default]
    Sum,
    Average,
    Count,
    Max,
    Min,
}

impl SubFn {
    /// SUBTOTAL 的函数号
    pub fn code(self) -> u8 {
        match self {
            SubFn::Average => 1,
            SubFn::Count => 2,
            SubFn::Max => 4,
            SubFn::Min => 5,
            SubFn::Sum => 9,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SubFn::Sum => "求和",
            SubFn::Average => "平均值",
            SubFn::Count => "计数",
            SubFn::Max => "最大值",
            SubFn::Min => "最小值",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SubtotalReport {
    /// 插了几组
    pub groups: usize,
    /// 插了几行（含总计行）
    pub rows: usize,
}

/// `region` 含表头行；`key_col` 分组列；`value_cols` 要汇总的列。
/// 返回插入的组数与行数。
///
/// Find group ends in original coordinates and move details and references together.
pub fn subtotal(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    key_col: u32,
    value_cols: &[u32],
    f: SubFn,
) -> SubtotalReport {
    let mut book = Workbook::new_blank();
    std::mem::swap(sheet, &mut book.sheets[0]);
    let report = subtotal_workbook(&mut book, 0, region, key_col, value_cols, f);
    std::mem::swap(sheet, &mut book.sheets[0]);
    report
}

/// Insert summary rows and update all workbook references to the moved details.
pub fn subtotal_workbook(
    wb: &mut Workbook,
    sheet_idx: usize,
    region: (u32, u32, u32, u32),
    key_col: u32,
    value_cols: &[u32],
    f: SubFn,
) -> SubtotalReport {
    try_subtotal_workbook(wb, sheet_idx, region, key_col, value_cols, f).unwrap_or_default()
}

pub fn try_subtotal_workbook(
    wb: &mut Workbook, sheet_idx: usize, region: (u32, u32, u32, u32),
    key_col: u32, value_cols: &[u32], f: SubFn,
) -> Result<SubtotalReport, &'static str> {
    let Some(sheet) = wb.sheets.get(sheet_idx) else { return Ok(SubtotalReport::default()) };
    let (r1, c1, r2, c2) = region;
    if r2 <= r1 || key_col < c1 || key_col > c2 {
        return Ok(SubtotalReport::default());
    }
    const LAST_ROW: u32 = 1_048_575;
    const OVERFLOW: &str = "分类汇总会将数据或格式移出工作表的最后一行";
    if r2 >= LAST_ROW { return Err(OVERFLOW); }
    // 扫描分组（原始坐标）
    let mut groups: Vec<(u32, u32)> = Vec::new();
    let mut start = r1 + 1;
    let mut cur = sheet.value(r1 + 1, key_col).display();
    for r in (r1 + 2)..=r2 {
        let v = sheet.value(r, key_col).display();
        if v != cur {
            groups.push((start, r - 1));
            start = r;
            cur = v;
        }
    }
    groups.push((start, r2));
    let n = groups.len();
    let mut insertions: Vec<_> = groups.iter().map(|&(_, end)| end + 1).collect();
    insertions.push(r2 + 1);
    let total_at = r2 + n as u32 + 1;
    if total_at > LAST_ROW { return Err(OVERFLOW); }
    let last_safe = LAST_ROW - insertions.len() as u32;
    if tail_occupied(sheet, last_safe)
        || wb.sheets.iter().flat_map(|s| &s.sparklines)
            .any(|sp| sp.source_sheet.as_deref() == Some(&sheet.name) && sp.rect.2 > last_safe)
        || wb.pivots_pending.iter().any(|(index, spec, _, output, _)|
            *index == sheet_idx && (spec.source.2 > last_safe || output.2 > last_safe))
    {
        return Err(OVERFLOW);
    }
    crate::structural::insert_subtotal_rows(wb, sheet_idx, &insertions);
    let sheet = &mut wb.sheets[sheet_idx];
    for (i, (gs, ge)) in groups.iter().enumerate() {
        let shift = i as u32;
        let fgs = gs + shift;
        let fge = ge + shift;
        let sub_row = fge + 1;
        let name = sheet.value(fgs, key_col).display();
        sheet.set(
            sub_row,
            c1,
            Cell {
                value: Value::Text(format!("{name} 汇总").into()),
                formula: None,
            },
        );
        for c in value_cols.iter().copied() {
            if c < c1 || c > c2 {
                continue;
            }
            let range = format!(
                "{}{}:{}{}",
                crate::model::col_label(c),
                fgs + 1,
                crate::model::col_label(c),
                fge + 1
            );
            let formula = format!("=SUBTOTAL({},{range})", f.code());
            let val = match f {
                SubFn::Count => Value::Number((fge - fgs + 1) as f64),
                _ => Value::Empty,
            };
            sheet.set(
                sub_row,
                c,
                Cell {
                    value: val,
                    formula: Some(formula.into()),
                },
            );
        }
        // 明细打 1 级（Excel 的 summaryBelow 约定：汇总行在明细下方）
        for r in fgs..=fge {
            sheet.row_levels.insert(r, 1);
        }
    }
    // 总计行
    sheet.set(
        total_at,
        c1,
        Cell {
            value: Value::Text("总计".into()),
            formula: None,
        },
    );
    for c in value_cols.iter().copied() {
        if c < c1 || c > c2 {
            continue;
        }
        let col = crate::model::col_label(c);
        let formula = format!("=SUBTOTAL({},{col}{}:{col}{total_at})", f.code(), r1 + 2);
        sheet.set(
            total_at,
            c,
            Cell {
                value: Value::Empty,
                formula: Some(formula.into()),
            },
        );
    }
    sheet.edited = true;
    sheet.layout_dirty = true;
    Ok(SubtotalReport {
        groups: n,
        rows: insertions.len(),
    })
}

// An empty-looking tail may still contain formatting, a drawing anchor or a
// validation range. Check it before the first mutation, including style-only cells.
fn tail_occupied(s: &Sheet, last: u32) -> bool {
    let past = |r: u32| r > last;
    s.used_bounds().is_some_and(|b| past(b.2))
        || s.styles.iter_from(true, last + 1).next().is_some()
        || s.rows.entries().any(|(&r, _)| past(r))
        || s.row_styles.keys().chain(s.row_levels.keys()).copied().any(past)
        || s.hidden_rows.iter().chain(s.hidden_rows_user.iter()).copied().any(past)
        || s.notes.keys().chain(s.spilled.iter()).any(|k| past(k.row))
        || s.spill_shapes.iter().any(|(k, shape)| past(k.row.saturating_add(shape.0.saturating_sub(1))))
        || s.arrays.iter().any(|(k, r)| past(k.row) || past(r.2))
        || s.rich.keys().chain(s.ignored_errors.iter()).any(|&(r, _)| past(r))
        || s.hyperlinks.iter().any(|(&(r, _), h)| past(r.saturating_add(h.span.0)))
        || s.merges.iter().any(|r| past(r.2))
        || s.cf.blocks.iter().flat_map(|b| &b.ranges).chain(s.dv.iter().flat_map(|d| &d.ranges))
            .any(|r| past(r.2) && (r.0, r.2) != (0, 1_048_575))
        || s.tables.iter().any(|t| past(t.range.2))
        || s.filter.as_ref().is_some_and(|f| past(f.range.2))
        || s.freeze.is_some_and(|(r, _)| past(r))
        || s.pictures.iter().any(|p| past(p.rect.2))
        || s.charts.iter().any(|c| past(c.anchor.2))
        || s.textboxes.iter().any(|t| past(t.from.2) || past(t.to.2))
        || s.sparklines.iter().any(|sp| past(sp.r) || (sp.source_sheet.is_none() && past(sp.rect.2)))
        || s.page.print_area.is_some_and(|r| past(r.2))
        || s.page.title_rows.is_some_and(|r| past(r.1))
        || s.page.row_breaks.iter().copied().any(past)
        || s.scenarios.iter().flat_map(|s| &s.cells).any(|((r, _), _)| past(*r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with(rows: &[(&str, f64)]) -> Sheet {
        let mut sh = Sheet::new("S");
        sh.set(0, 0, Cell { value: Value::Text("部门".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        for (i, (k, v)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            sh.set(r, 0, Cell { value: Value::Text((*k).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Number(*v), formula: None });
        }
        sh
    }

    #[test]
    fn workbook_subtotals_keep_report_references_through_undo_and_redo() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0] = sheet_with(&[("甲", 100.0), ("乙", 200.0)]);
        wb.sheets.push(Sheet::new("Report"));
        wb.sheets[1].set(0, 0, Cell { value: Value::Number(200.0), formula: Some("=S!B3".into()) });
        wb.names.push(("SecondAmount".into(), None, "S!$B$3".into()));
        let before = crate::undo::snapshot_structure(&wb, 0, &Structural::InsertRows { at: 2, n: 1 });
        let report = subtotal_workbook(&mut wb, 0, (0, 0, 2, 1), 0, &[1], SubFn::Sum);
        assert_eq!(report.rows, 3);
        let after = before.capture_after(&wb);
        let mut undo = crate::undo::UndoStack::default();
        undo.push_structure("分类汇总", before, after, (0, 0, 0));
        for (step, row) in [4, 3, 4].into_iter().enumerate() {
            match step {
                1 => { undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap(); }
                2 => { undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap(); }
                _ => {}
            }
            crate::formula::recalc_all(&mut wb);
            assert_eq!(wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some(format!("=S!B{row}").as_str()));
            assert_eq!(wb.sheets[1].value(0, 0), Value::Number(200.0));
            assert_eq!(wb.names[0].2, format!("S!$B${row}"));
            assert_eq!(wb.sheets[0].row_levels.contains_key(&1), step != 1);
        }
    }

    #[test]
    fn groups_and_inserts_summary_rows() {
        let mut sh = sheet_with(&[("甲", 10.0), ("甲", 5.0), ("乙", 7.0)]);
        let rep = subtotal(&mut sh, (0, 0, 3, 1), 0, &[1], SubFn::Sum);
        assert_eq!(rep.groups, 2, "两组");
        assert_eq!(rep.rows, 3, "两条小计 + 总计");
        // 行布局：表头 / 甲 / 甲 / 甲 汇总 / 乙 / 乙 汇总 / 总计
        assert_eq!(sh.value(3, 0).display(), "甲 汇总");
        assert_eq!(sh.value(5, 0).display(), "乙 汇总");
        assert_eq!(sh.value(6, 0).display(), "总计");
        let f = sh.get(3, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert_eq!(&*f, "=SUBTOTAL(9,B2:B3)");
        let f2 = sh.get(5, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert_eq!(&*f2, "=SUBTOTAL(9,B5:B5)", "乙的明细被推到第 5 行");
        // 总计引用连续区域；求值器跳过其中的小计。
        let ft = sh.get(6, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert_eq!(&*ft, "=SUBTOTAL(9,B2:B6)");
        // 明细行变成 1 级
        assert_eq!(sh.row_levels.get(&1), Some(&1));
    }

    #[test]
    fn thousands_of_groups_use_one_grand_total_range() {
        let mut wb = Workbook::new_blank();
        for row in 1..=30_000 {
            let s = &mut wb.sheets[0];
            s.set(row, 0, Cell { value: Value::Number(((row - 1) / 10) as f64), formula: None });
            s.set(row, 1, Cell { value: Value::Number(1.0), formula: None });
            s.set_style_loaded(row, 1, 1);
        }
        let started = std::time::Instant::now();
        let report = try_subtotal_workbook(&mut wb, 0, (0, 0, 30_000, 1), 0, &[1], SubFn::Sum).unwrap();
        eprintln!("30k details / 3k groups insertion: {:?}", started.elapsed());
        assert_eq!((report.groups, report.rows), (3000, 3001));
        assert_eq!(wb.sheets[0].get(33_001, 1).unwrap().formula.as_deref(), Some("=SUBTOTAL(9,B2:B33001)"));
        crate::formula::recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].value(33_001, 1), Value::Number(30_000.0));
    }

    #[test]
    fn grand_total_respects_filtered_rows_and_nested_subtotal_calls() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0] = sheet_with(&[("A", 4.0), ("B", 9.0), ("B", 3.0)]);
        let sheet = &mut wb.sheets[0];
        sheet.hidden_rows.insert(2);
        sheet.dv.push(crate::dv::DataValidation { ranges: vec![(0, 1, 1_048_575, 1)], ..Default::default() });
        sheet.set(3, 1, Cell { value: Value::Empty, formula: Some("=1+SUBTOTAL(9,B2:B2)".into()) });
        try_subtotal_workbook(&mut wb, 0, (0, 0, 3, 1), 0, &[1], SubFn::Sum).unwrap();
        crate::formula::recalc_all(&mut wb);
        assert!(wb.sheets[0].hidden_rows.contains(&3));
        assert_eq!(wb.sheets[0].dv[0].ranges, [(0, 1, 1_048_575, 1)]);
        assert_eq!(wb.sheets[0].value(6, 1), Value::Number(4.0));
    }

    #[test]
    fn table_calculated_column_formulas_match_ordered_insertions() {
        let mut batch = Workbook::new_blank();
        let sh = &mut batch.sheets[0];
        sh.tables.push(crate::table::Table { range: (0, 0, 5, 2), header_row: true, ..Default::default() });
        for row in 1..=5 {
            sh.set(row, 2, Cell {
                value: Value::Empty,
                formula: Some(format!("=SUM(B{}:B{})+$B$6", row + 1, row + 3).into()),
            });
            sh.set_style_loaded(row, 2, 4);
        }
        let mut serial = batch.clone();
        let positions = [2, 4, 6, 6];
        for at in [6, 4, 2, 9] {
            crate::structural::apply_structural_wb(&mut serial, 0, &Structural::InsertRows { at, n: 1 });
        }
        crate::structural::insert_subtotal_rows(&mut batch, 0, &positions);
        assert_eq!(batch.sheets[0].tables, serial.sheets[0].tables);
        for r in 0..12 {
            assert_eq!(batch.sheets[0].get(r, 2), serial.sheets[0].get(r, 2), "row {r}");
            assert_eq!(batch.sheets[0].style_of(r, 2), serial.sheets[0].style_of(r, 2));
        }
    }

    #[test]
    fn rejects_subtotals_that_would_displace_formatting_off_the_sheet() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0] = sheet_with(&[("A", 1.0), ("B", 2.0)]);
        wb.sheets[0].rows.set(1_048_574, 40.0);
        let version = wb.sheets[0].cell_revision();
        assert!(try_subtotal_workbook(&mut wb, 0, (0, 0, 2, 1), 0, &[1], SubFn::Sum).is_err());
        assert_eq!(wb.sheets[0].cell_revision(), version);
        wb.sheets[0].rows.clear_custom();
        wb.sheets[0].set_style_loaded(1_048_574, 3, 2);
        assert!(try_subtotal_workbook(&mut wb, 0, (0, 0, 2, 1), 0, &[1], SubFn::Sum).is_err());
        assert!(wb.pivot_shifts.is_empty());
    }

    #[test]
    fn average_and_count_codes() {
        assert_eq!(SubFn::Average.code(), 1);
        assert_eq!(SubFn::Count.code(), 2);
        assert_eq!(SubFn::Sum.code(), 9);
        let mut sh = sheet_with(&[("甲", 10.0), ("甲", 20.0)]);
        subtotal(&mut sh, (0, 0, 2, 1), 0, &[1], SubFn::Average);
        let f = sh.get(3, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert!(f.starts_with("=SUBTOTAL(1,"), "{f}");
    }

    #[test]
    fn unsorted_groups_are_split_like_excel() {
        let mut sh = sheet_with(&[("甲", 1.0), ("乙", 2.0), ("甲", 3.0)]);
        let rep = subtotal(&mut sh, (0, 0, 3, 1), 0, &[1], SubFn::Sum);
        assert_eq!(rep.groups, 3, "不相邻的同值算不同组（和 Excel 一致）");
    }

    #[test]
    fn empty_or_bad_region_does_nothing() {
        let mut sh = sheet_with(&[("甲", 1.0)]);
        let rep = subtotal(&mut sh, (0, 0, 0, 1), 0, &[1], SubFn::Sum);
        assert_eq!(rep.groups, 0, "只有表头不分组");
        let rep2 = subtotal(&mut sh, (0, 0, 1, 1), 9, &[1], SubFn::Sum);
        assert_eq!(rep2.groups, 0, "key_col 不在区域内要拒绝");
    }
}
