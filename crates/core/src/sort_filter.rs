//! 排序与筛选。
//!
//! 排序按 Excel 的规矩：整行搬运（值、公式、样式一起走），空格永远排在最后；
//! 数字按大小、文本按不区分大小写的字典序、布尔按真假。
//! 筛选不删数据，只把不匹配的行隐藏起来（隐藏 = 行高 0，和 Excel 一致）。

use crate::model::{Cell, CellRef, Sheet, Value};
use std::collections::{HashMap, HashSet};

/// 排序键：数字 < 文本 < 布尔，空格单独处理
fn key_of(v: &Value) -> Option<(u8, f64, String)> {
    match v {
        Value::Number(n) => Some((0, *n, String::new())),
        Value::Text(t) => Some((1, 0.0, t.to_lowercase())),
        Value::Bool(b) => Some((2, if *b { 1.0 } else { 0.0 }, String::new())),
        Value::Empty => None,
    }
}

/// 在一个矩形区域里按 `col` 列排序（col 必须是区域内的一列）
pub fn sort_range(sheet: &mut Sheet, r1: u32, c1: u32, r2: u32, c2: u32, col: u32, asc: bool) -> Result<(), String> {
    if r2 <= r1 {
        return Ok(());
    }
    if col < c1 || col > c2 {
        return Err("排序列不在选区里".into());
    }
    // 区域里有合并单元格时 Excel 会拒绝排序，这里也一样
    if sheet
        .merges
        .iter()
        .any(|(a, b, c, d)| *a <= r2 && *c >= r1 && *b <= c2 && *d >= c1)
    {
        return Err("选区里有合并单元格，先取消合并再排序".into());
    }

    // 取每行的键与整行内容
    let mut rows: Vec<(Option<(u8, f64, String)>, u32)> = Vec::new();
    for r in r1..=r2 {
        let k = sheet.get(r, col).and_then(|c| key_of(&c.value));
        rows.push((k, r));
    }
    rows.sort_by(|a, b| match (&a.0, &b.0) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater, // 空格永远在最后
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => {
            let ord = x
                .0
                .cmp(&y.0)
                .then_with(|| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| x.2.cmp(&y.2));
            if asc {
                ord
            } else {
                ord.reverse()
            }
        }
    });

    // 先把原区域整块拍下来（按原始行序），再按排序结果落到新位置
    let width = (c2 - c1 + 1) as usize;
    let mut base: Vec<(Vec<Option<Cell>>, Vec<u16>)> = Vec::with_capacity(rows.len());
    for r in r1..=r2 {
        let mut line = Vec::with_capacity(width);
        let mut st = Vec::with_capacity(width);
        for c in c1..=c2 {
            line.push(sheet.get(r, c).cloned());
            st.push(sheet.style_of(r, c));
        }
        base.push((line, st));
    }

    for (i, (_, src)) in rows.iter().enumerate() {
        let dst = r1 + i as u32;
        let (line, st) = &base[(*src - r1) as usize];
        for (j, c) in (c1..=c2).enumerate() {
            match &line[j] {
                Some(cell) => sheet.set(dst, c, cell.clone()),
                None => {
                    sheet.remove(dst, c);
                }
            }
            let key = CellRef::new(dst, c);
            if st[j] != 0 {
                sheet.set_style_loaded(dst, c, st[j]);
            } else {
                sheet.styles.remove(&key);
            }
        }
    }
    Ok(())
}

/// 区域第一行是不是表头：整行都是非空文本，且下面出现过数字
pub fn has_header(sheet: &Sheet, r1: u32, c1: u32, r2: u32, c2: u32) -> bool {
    if r2 <= r1 {
        return false;
    }
    let all_text = (c1..=c2).all(|c| {
        matches!(
            sheet.get(r1, c).map(|x| &x.value),
            Some(Value::Text(t)) if !t.is_empty()
        )
    });
    if !all_text {
        return false;
    }
    (r1 + 1..=r2).any(|r| (c1..=c2).any(|c| matches!(sheet.get(r, c).map(|x| &x.value), Some(Value::Number(_)))))
}

/// 带表头判断的排序：有表头就只排序下面的数据行
pub fn sort_region(sheet: &mut Sheet, region: (u32, u32, u32, u32), col: u32, asc: bool) -> Result<(), String> {
    let (r1, c1, r2, c2) = region;
    if has_header(sheet, r1, c1, r2, c2) {
        sort_range(sheet, r1 + 1, c1, r2, c2, col, asc)
    } else {
        sort_range(sheet, r1, c1, r2, c2, col, asc)
    }
}

/// 以 (r, c) 为中心找出连续的数据区域（Excel 的「当前区域」）
pub fn current_region(sheet: &Sheet, r: u32, c: u32) -> (u32, u32, u32, u32) {
    let (rr2, cc2) = sheet
        .used_bounds()
        .map(|(_, _, r2, c2)| (r2 + 1, c2 + 1))
        .unwrap_or((r + 1, c + 1));
    let rows = rr2.max(r + 1);
    let cols = cc2.max(c + 1);
    let is_blank_row = |rr: u32| (0..cols).all(|cc| sheet.get(rr, cc).is_none());
    let is_blank_col = |cc: u32| (0..rows).all(|rr| sheet.get(rr, cc).is_none());

    let mut r1 = r;
    while r1 > 0 && !is_blank_row(r1 - 1) {
        r1 -= 1;
    }
    let mut r2 = r;
    while r2 + 1 < rows && !is_blank_row(r2 + 1) {
        r2 += 1;
    }
    let mut c1 = c;
    while c1 > 0 && !is_blank_col(c1 - 1) {
        c1 -= 1;
    }
    let mut c2 = c;
    while c2 + 1 < cols && !is_blank_col(c2 + 1) {
        c2 += 1;
    }
    (r1, c1, r2, c2)
}

/// 一个筛选器：区域 + 每列允许的值
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub range: (u32, u32, u32, u32),
    /// 列号 → 允许出现文本值；没有条目的列不过滤
    pub allows: HashMap<u32, HashSet<String>>,
}

impl Filter {
    pub fn new(r1: u32, c1: u32, r2: u32, c2: u32) -> Self {
        Self { range: (r1, c1, r2, c2), allows: HashMap::new() }
    }

    /// 按当前规则算出要隐藏的行（不含表头行）
    pub fn hidden_rows(&self, sheet: &Sheet) -> HashSet<u32> {
        let (r1, c1, r2, c2) = self.range;
        let mut out = HashSet::new();
        if self.allows.is_empty() {
            return out;
        }
        for r in (r1 + 1)..=r2 {
            for (col, allowed) in self.allows.iter() {
                if *col < c1 || *col > c2 {
                    continue;
                }
                let v = sheet.get(r, *col).map(|c| c.value.display()).unwrap_or_default();
                if !allowed.contains(&v) {
                    out.insert(r);
                    break;
                }
            }
        }
        out
    }
}

/// 某列在筛选区域里的去重值（按显示文本，排序后返回）
pub fn unique_values(sheet: &Sheet, range: (u32, u32, u32, u32), col: u32) -> Vec<String> {
    let (r1, _, r2, _) = range;
    let mut set: HashSet<String> = HashSet::new();
    for r in (r1 + 1)..=r2 {
        let v = sheet.get(r, col).map(|c| c.value.display()).unwrap_or_default();
        set.insert(v);
    }
    let mut out: Vec<String> = set.into_iter().collect();
    out.sort_by(|a, b| match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.cmp(b),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Sheet {
        let mut s = Sheet::new("T");
        let rows = [
            ("张三", 30.0),
            ("李四", 10.0),
            ("王五", 20.0),
            ("赵六", 0.0),
        ];
        for (i, (n, v)) in rows.iter().enumerate() {
            let r = (i + 1) as u32;
            s.set(r, 0, Cell { value: Value::Text((*n).into()), formula: None });
            s.set(r, 1, Cell { value: Value::Number(*v), formula: None });
        }
        s.set(0, 0, Cell { value: Value::Text("姓名".into()), formula: None });
        s.set(0, 1, Cell { value: Value::Text("分数".into()), formula: None });
        s
    }

    #[test]
    fn sorts_ascending_and_descending() {
        let mut s = sheet();
        sort_range(&mut s, 1, 0, 4, 1, 1, true).unwrap();
        let names: Vec<String> = (1..=4)
            .map(|r| s.get(r, 0).map(|c| c.value.display()).unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["赵六", "李四", "王五", "张三"], "按分数升序");
        sort_range(&mut s, 1, 0, 4, 1, 1, false).unwrap();
        let names: Vec<String> = (1..=4)
            .map(|r| s.get(r, 0).map(|c| c.value.display()).unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["张三", "王五", "李四", "赵六"], "降序");
    }

    #[test]
    fn blanks_go_last() {
        let mut s = sheet();
        s.remove(3, 1); // 王五那行分数留空（第 3 行是王五）
        sort_range(&mut s, 1, 0, 4, 1, 1, true).unwrap();
        let last = s.get(4, 0).map(|c| c.value.display()).unwrap_or_default();
        assert_eq!(last, "王五", "空格应排到最后，不受升降序影响");
    }

    #[test]
    fn styles_move_with_rows() {
        let mut s = sheet();
        let mut st = crate::style::CellStyle::default();
        st.bold = true;
        let id = {
            let mut t = crate::style::StyleTable::new();
            t.intern(st)
        };
        // 给王五（第 3 行，20 分）加样式
        s.set_style_loaded(3, 0, id);
        sort_range(&mut s, 1, 0, 4, 1, 1, true).unwrap();
        // 排序后找到王五所在行，样式应跟着它
        let mut found = None;
        for r in 1..=4 {
            if s.get(r, 0).map(|c| c.value.display()) == Some("王五".to_string()) {
                found = Some(r);
            }
        }
        let r = found.expect("王五应在表内");
        assert_eq!(s.style_of(r, 0), id, "样式应跟着王五走");
    }

    #[test]
    fn rejects_merged_range() {
        let mut s = sheet();
        s.merge_range(1, 0, 2, 0);
        assert!(sort_range(&mut s, 1, 0, 4, 1, 1, true).is_err());
    }

    #[test]
    fn filter_hides_non_matching_rows() {
        let s = sheet();
        let mut f = Filter::new(0, 0, 4, 1);
        let mut allow = HashSet::new();
        allow.insert("张三".to_string());
        allow.insert("王五".to_string());
        f.allows.insert(0, allow);
        let hidden = f.hidden_rows(&s);
        assert!(hidden.contains(&2), "李四应被隐藏");
        assert!(hidden.contains(&4), "赵六应被隐藏");
        assert!(!hidden.contains(&1), "张三应保留");
        assert!(!hidden.contains(&0), "表头不参与");
    }

    #[test]
    fn region_expands_to_contiguous_block() {
        let s = sheet();
        // 从中间任意一格出发应拿到 A1:B5（0 基：(0,0)-(4,1)）
        assert_eq!(current_region(&s, 2, 1), (0, 0, 4, 1));
        assert_eq!(current_region(&s, 0, 0), (0, 0, 4, 1));
        // 远处空格自成一块
        let (r1, c1, r2, c2) = current_region(&s, 20, 5);
        assert_eq!((r1, c1, r2, c2), (20, 5, 20, 5));
    }

    #[test]
    fn unique_values_sorted() {
        let s = sheet();
        let u = unique_values(&s, (0, 0, 4, 1), 0);
        assert_eq!(u, vec!["张三", "李四", "王五", "赵六"]);
    }
}
