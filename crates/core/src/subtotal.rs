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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubtotalOpts {
    /// 替换当前分类汇总（删掉已有 SUBTOTAL 汇总行后再重算）
    pub replace_existing: bool,
    /// 每组数据分页（在每条小计行后插入手动水平分页符）
    pub page_break_between_groups: bool,
    /// 汇总结果显示在数据下方（默认 true）
    pub summary_below: bool,
}

impl Default for SubtotalOpts {
    fn default() -> Self {
        Self {
            replace_existing: true,
            page_break_between_groups: false,
            summary_below: true,
        }
    }
}

fn is_subtotal_summary_row(sheet: &Sheet, r: u32, c1: u32, c2: u32) -> bool {
    (c1..=c2).any(|c| {
        sheet.formula_of(r, c)
            .is_some_and(|f| f.to_ascii_uppercase().contains("SUBTOTAL("))
    })
}

pub fn try_subtotal_workbook(
    wb: &mut Workbook, sheet_idx: usize, region: (u32, u32, u32, u32),
    key_col: u32, value_cols: &[u32], f: SubFn,
) -> Result<SubtotalReport, &'static str> {
    try_subtotal_workbook_opts(wb, sheet_idx, region, key_col, value_cols, f, SubtotalOpts {
        replace_existing: false,
        page_break_between_groups: false,
        summary_below: true,
    })
}

pub fn try_subtotal_workbook_opts(
    wb: &mut Workbook,
    sheet_idx: usize,
    region: (u32, u32, u32, u32),
    key_col: u32,
    value_cols: &[u32],
    f: SubFn,
    opts: SubtotalOpts,
) -> Result<SubtotalReport, &'static str> {
    if sheet_idx >= wb.sheets.len() {
        return Ok(SubtotalReport::default());
    }
    let (r1, c1, mut r2, c2) = region;
    if r2 <= r1 || key_col < c1 || key_col > c2 {
        return Ok(SubtotalReport::default());
    }
    const LAST_ROW: u32 = 1_048_575;
    const OVERFLOW: &str = "分类汇总会将数据或格式移出工作表的最后一行";
    if r2 > LAST_ROW || (!opts.replace_existing && r2 >= LAST_ROW) {
        return Err(OVERFLOW);
    }

    // 在 CoW 副本上暂存整套操作（移除旧汇总行 -> 重新分组 -> MultiInsert 越界预检 -> 写入公式），
    // 任一路径返回 Err 时 `wb` 保持 100% 未修改。
    let mut staged = wb.clone();

    // 替换当前分类汇总：先移除区域内已有的 SUBTOTAL 汇总行并重置行分级
    if opts.replace_existing {
        let mut old_rows: Vec<u32> = ((r1 + 1)..=r2)
            .filter(|&r| is_subtotal_summary_row(&staged.sheets[sheet_idx], r, c1, c2))
            .collect();
        if !old_rows.is_empty() {
            old_rows.sort_unstable();
            for &rr in old_rows.iter().rev() {
                crate::structural::apply_structural_wb(
                    &mut staged,
                    sheet_idx,
                    &crate::structural::Structural::DeleteRows { at: rr, n: 1 },
                );
                r2 = r2.saturating_sub(1);
            }
            if let Some(sh) = staged.sheets.get_mut(sheet_idx) {
                for r in (r1 + 1)..=r2 {
                    sh.row_levels.remove(&r);
                }
            }
            if r2 <= r1 {
                return Ok(SubtotalReport::default());
            }
        }
    }

    let has_existing_levels = !opts.replace_existing
        && ((r1 + 1)..=r2).any(|r| staged.sheets[sheet_idx].row_levels.contains_key(&r));

    // 扫描分组（原始坐标）；嵌套模式下跳过已有 SUBTOTAL 汇总行
    let mut groups: Vec<(u32, u32)> = Vec::new();
    {
        let sheet = &staged.sheets[sheet_idx];
        let mut cur_start: Option<u32> = None;
        let mut cur_val = String::new();
        let mut last_detail = r1;
        for r in (r1 + 1)..=r2 {
            if has_existing_levels && is_subtotal_summary_row(sheet, r, c1, c2) {
                if let Some(st) = cur_start.take() {
                    groups.push((st, last_detail));
                }
                continue;
            }
            let v = sheet.value(r, key_col).display();
            match cur_start {
                None => {
                    cur_start = Some(r);
                    cur_val = v;
                    last_detail = r;
                }
                Some(st) => {
                    if v != cur_val {
                        groups.push((st, last_detail));
                        cur_start = Some(r);
                        cur_val = v;
                    }
                    last_detail = r;
                }
            }
        }
        if let Some(st) = cur_start {
            groups.push((st, last_detail));
        }
    }
    if groups.is_empty() {
        return Ok(SubtotalReport::default());
    }
    let n = groups.len();
    let mut insertions: Vec<_> = groups.iter().map(|&(_, end)| end + 1).collect();
    if !has_existing_levels {
        insertions.push(r2 + 1);
    }
    let Some(total_at) = r2.checked_add(insertions.len() as u32) else {
        return Err(OVERFLOW);
    };
    if total_at > LAST_ROW {
        return Err(OVERFLOW);
    }
    if crate::transform::CoordTransform::from_multi_insert(
        sheet_idx,
        crate::refs::Axis::Row,
        insertions.clone(),
    )
    .preflight(&staged)
    .is_err()
    {
        return Err(OVERFLOW);
    }
    crate::structural::insert_subtotal_rows(&mut staged, sheet_idx, &insertions);
    let sheet = &mut staged.sheets[sheet_idx];
    let mut added_breaks: Vec<u32> = Vec::new();
    for (i, (gs, ge)) in groups.iter().enumerate() {
        let shift = i as u32;
        let fgs = gs + shift;
        let fge = ge + shift;
        let sub_row = fge + 1;
        let name = sheet.value(fgs, key_col).display();
        // 写入分组列 `key_col`，绝不覆写无关首列 `c1`！
        sheet.set(
            sub_row,
            key_col,
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
        // 明细分级：嵌套汇总时保留原有层级并递增 1（上限 7），避免抹掉外层分组
        for r in fgs..=fge {
            let cur_lv = sheet.row_levels.get(&r).copied().unwrap_or(0);
            let next_lv = if has_existing_levels { (cur_lv + 1).min(7) } else { 1 };
            sheet.row_levels.insert(r, next_lv);
        }
        if has_existing_levels {
            let cur_lv = sheet.row_levels.get(&sub_row).copied().unwrap_or(0);
            sheet.row_levels.insert(sub_row, cur_lv.max(1));
        }
        if opts.page_break_between_groups && i + 1 < n {
            added_breaks.push(sub_row + 1);
        }
    }
    if !added_breaks.is_empty() {
        for br in added_breaks {
            if !sheet.page.row_breaks.contains(&br) {
                sheet.page.row_breaks.push(br);
            }
        }
        sheet.page.row_breaks.sort_unstable();
        sheet.page_dirty = true;
    }
    // 非嵌套时添加总计行
    if !has_existing_levels {
        sheet.set(
            total_at,
            key_col,
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
    }
    sheet.edited = true;
    sheet.layout_dirty = true;
    *wb = staged;
    Ok(SubtotalReport {
        groups: n,
        rows: insertions.len(),
    })
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

    #[test]
    fn replace_subtotals_overflow_must_leave_workbook_unchanged() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "S".into();
        for (row, label) in [(0, "类型"), (1, "甲"), (2, "乙"), (3, "甲 汇总"), (4, "总计")] {
            wb.sheets[0].set(row, 0, Cell { value: Value::Text(label.into()), formula: None });
        }
        wb.sheets[0].set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        for (row, value) in [(1, 10.0), (2, 20.0)] {
            wb.sheets[0].set(row, 1, Cell { value: Value::Number(value), formula: None });
        }
        for (row, formula) in [(3, "=SUBTOTAL(9,B2:B3)"), (4, "=SUBTOTAL(9,B2:B4)")] {
            wb.sheets[0].set(row, 1, Cell { value: Value::Empty, formula: Some(formula.into()) });
        }
        wb.sheets[0].row_levels.insert(1, 1);
        wb.sheets[0].row_levels.insert(2, 1);
        wb.sheets[0].rows.set(3, 36.0);
        let last = crate::sync::MAX_ROWS - 1;
        wb.sheets[0].set(last, 0, Cell { value: Value::Number(7.0), formula: None });
        let mut watcher = Sheet::new("Watcher");
        watcher.set(0, 0, Cell { value: Value::Empty, formula: Some("=S!A1048576".into()) });
        wb.sheets.push(watcher);
        wb.dirty = false;
        let epoch_before = wb.coord_epoch();
        let rev_before = wb.sheets[0].cell_revision();

        let result = try_subtotal_workbook_opts(
            &mut wb,
            0,
            (0, 0, 4, 1),
            0,
            &[1],
            SubFn::Sum,
            SubtotalOpts {
                replace_existing: true,
                page_break_between_groups: false,
                summary_below: true,
            },
        );
        assert!(result.is_err(), "Tail overflow must reject the replacement");
        assert_eq!(wb.coord_epoch(), epoch_before, "A rejected replacement must not delete old summary rows");
        assert_eq!(wb.sheets[0].cell_revision(), rev_before);
        assert_eq!(wb.sheets[0].formula_of(3, 1), Some("=SUBTOTAL(9,B2:B3)"));
        assert_eq!(wb.sheets[0].formula_of(4, 1), Some("=SUBTOTAL(9,B2:B4)"));
        assert_eq!(wb.sheets[0].value(last, 0), Value::Number(7.0));
        assert_eq!(wb.sheets[0].row_levels.get(&1), Some(&1));
        assert_eq!(wb.sheets[0].row_levels.get(&2), Some(&1));
        assert!((wb.sheets[0].rows.size(3) - 36.0).abs() < 1e-3);
        assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A1048576"));
        assert!(!wb.dirty);
    }

    #[test]
    fn replace_existing_false_overflow_leaves_workbook_unchanged() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0] = sheet_with(&[("甲", 10.0), ("乙", 20.0)]);
        wb.sheets[0].rows.set(1, 44.0);
        let last = crate::sync::MAX_ROWS - 1;
        wb.sheets[0].set(last, 0, Cell { value: Value::Number(9.0), formula: None });
        let mut watcher = Sheet::new("Watcher");
        watcher.set(0, 0, Cell { value: Value::Empty, formula: Some("=S!A1048576".into()) });
        wb.sheets.push(watcher);
        wb.dirty = false;
        let epoch_before = wb.coord_epoch();
        let rev_before = wb.sheets[0].cell_revision();

        let result = try_subtotal_workbook_opts(
            &mut wb,
            0,
            (0, 0, 2, 1),
            0,
            &[1],
            SubFn::Sum,
            SubtotalOpts {
                replace_existing: false,
                page_break_between_groups: true,
                summary_below: true,
            },
        );
        assert!(result.is_err());
        assert_eq!(wb.coord_epoch(), epoch_before);
        assert_eq!(wb.sheets[0].cell_revision(), rev_before);
        assert_eq!(wb.sheets[0].value(1, 1), Value::Number(10.0));
        assert_eq!(wb.sheets[0].value(2, 1), Value::Number(20.0));
        assert!(wb.sheets[0].get(3, 0).is_none());
        assert_eq!(wb.sheets[0].value(last, 0), Value::Number(9.0));
        assert!((wb.sheets[0].rows.size(1) - 44.0).abs() < 1e-3);
        assert!(wb.sheets[0].page.row_breaks.is_empty());
        assert!(wb.sheets[0].row_levels.is_empty());
        assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A1048576"));
        assert!(!wb.dirty);
    }

    #[test]
    fn replace_existing_true_succeeds_and_roundtrips_undo_redo() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "S".into();
        for (row, label) in [(0, "类型"), (1, "甲"), (2, "乙"), (3, "甲 汇总"), (4, "总计")] {
            wb.sheets[0].set(row, 0, Cell { value: Value::Text(label.into()), formula: None });
        }
        wb.sheets[0].set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        wb.sheets[0].set(1, 1, Cell { value: Value::Number(10.0), formula: None });
        wb.sheets[0].set(2, 1, Cell { value: Value::Number(20.0), formula: None });
        wb.sheets[0].set(3, 1, Cell { value: Value::Empty, formula: Some("=SUBTOTAL(9,B2:B3)".into()) });
        wb.sheets[0].set(4, 1, Cell { value: Value::Empty, formula: Some("=SUBTOTAL(9,B2:B4)".into()) });
        wb.sheets[0].set(10, 0, Cell { value: Value::Number(77.0), formula: None });
        let mut watcher = Sheet::new("Watcher");
        watcher.set(0, 0, Cell { value: Value::Empty, formula: Some("=S!A11".into()) });
        wb.sheets.push(watcher);
        crate::formula::recalc_all(&mut wb);

        let before = crate::undo::snapshot_structure(&wb, 0, &Structural::InsertRows { at: 1, n: 1 });
        let report = try_subtotal_workbook_opts(
            &mut wb,
            0,
            (0, 0, 4, 1),
            0,
            &[1],
            SubFn::Sum,
            SubtotalOpts {
                replace_existing: true,
                page_break_between_groups: false,
                summary_below: true,
            },
        )
        .expect("replacement with room below should succeed");
        assert_eq!((report.groups, report.rows), (2, 3));
        let after = before.capture_after(&wb);
        let mut undo = crate::undo::UndoStack::default();
        undo.push_structure("分类汇总", before, after, (0, 0, 0));

        // After replace: row 0 header, row 1 甲(10), row 2 甲 汇总, row 3 乙(20), row 4 乙 汇总, row 5 总计.
        // Net +1 row shifted row 10 -> row 11 (A12).
        crate::formula::recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].value(2, 0).display(), "甲 汇总");
        assert_eq!(wb.sheets[0].formula_of(2, 1), Some("=SUBTOTAL(9,B2:B2)"));
        assert_eq!(wb.sheets[0].value(2, 1), Value::Number(10.0));
        assert_eq!(wb.sheets[0].value(4, 0).display(), "乙 汇总");
        assert_eq!(wb.sheets[0].formula_of(4, 1), Some("=SUBTOTAL(9,B4:B4)"));
        assert_eq!(wb.sheets[0].value(4, 1), Value::Number(20.0));
        assert_eq!(wb.sheets[0].value(5, 0).display(), "总计");
        assert_eq!(wb.sheets[0].formula_of(5, 1), Some("=SUBTOTAL(9,B2:B5)"));
        assert_eq!(wb.sheets[0].value(5, 1), Value::Number(30.0));
        assert_eq!(wb.sheets[0].value(11, 0), Value::Number(77.0));
        assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A12"));
        assert_eq!(wb.sheets[1].value(0, 0), Value::Number(77.0));

        // Undo restores original 2-summary-row layout and Watcher ref `=S!A11`.
        undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        crate::formula::recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].formula_of(3, 1), Some("=SUBTOTAL(9,B2:B3)"));
        assert_eq!(wb.sheets[0].formula_of(4, 1), Some("=SUBTOTAL(9,B2:B4)"));
        assert_eq!(wb.sheets[0].value(10, 0), Value::Number(77.0));
        assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A11"));
        assert_eq!(wb.sheets[1].value(0, 0), Value::Number(77.0));

        // Redo re-applies the replaced 2-group layout and `=S!A12`.
        undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        crate::formula::recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].formula_of(2, 1), Some("=SUBTOTAL(9,B2:B2)"));
        assert_eq!(wb.sheets[0].formula_of(4, 1), Some("=SUBTOTAL(9,B4:B4)"));
        assert_eq!(wb.sheets[0].formula_of(5, 1), Some("=SUBTOTAL(9,B2:B5)"));
        assert_eq!(wb.sheets[0].value(5, 1), Value::Number(30.0));
        assert_eq!(wb.sheets[0].value(11, 0), Value::Number(77.0));
        assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A12"));
        assert_eq!(wb.sheets[1].value(0, 0), Value::Number(77.0));
    }

    #[test]
    fn replace_existing_true_only_subtotal_rows_leaves_workbook_unchanged() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "S".into();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("类型".into()), formula: None });
        wb.sheets[0].set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        wb.sheets[0].set(1, 0, Cell { value: Value::Text("甲 汇总".into()), formula: None });
        wb.sheets[0].set(1, 1, Cell { value: Value::Empty, formula: Some("=SUBTOTAL(9,B2:B2)".into()) });
        wb.sheets[0].set(2, 0, Cell { value: Value::Text("总计".into()), formula: None });
        wb.sheets[0].set(2, 1, Cell { value: Value::Empty, formula: Some("=SUBTOTAL(9,B2:B3)".into()) });
        wb.sheets[0].set(5, 0, Cell { value: Value::Number(77.0), formula: None });
        wb.sheets[0].rows.set(1, 42.0);
        let mut watcher = Sheet::new("Watcher");
        watcher.set(0, 0, Cell { value: Value::Empty, formula: Some("=S!A6".into()) });
        wb.sheets.push(watcher);
        wb.dirty = false;
        let epoch_before = wb.coord_epoch();
        let rev_before = wb.sheets[0].cell_revision();

        let rep = try_subtotal_workbook_opts(
            &mut wb,
            0,
            (0, 0, 2, 1),
            0,
            &[1],
            SubFn::Sum,
            SubtotalOpts {
                replace_existing: true,
                page_break_between_groups: false,
                summary_below: true,
            },
        )
        .expect("no-detail region returns empty SubtotalReport");
        assert_eq!((rep.groups, rep.rows), (0, 0));
        assert_eq!(wb.coord_epoch(), epoch_before);
        assert_eq!(wb.sheets[0].cell_revision(), rev_before);
        assert!(!wb.dirty);
        assert_eq!(wb.sheets[0].formula_of(1, 1), Some("=SUBTOTAL(9,B2:B2)"));
        assert_eq!(wb.sheets[0].formula_of(2, 1), Some("=SUBTOTAL(9,B2:B3)"));
        assert_eq!(wb.sheets[0].value(5, 0), Value::Number(77.0));
        assert!((wb.sheets[0].rows.size(1) - 42.0).abs() < 1e-3);
        assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A6"));
    }
}
