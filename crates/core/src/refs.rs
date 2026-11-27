//! 公式里的引用改写：插行删行之后，公式里的 A1、$B$2、A1:C5、A:A、1:3 要跟着动。
//!
//! 这不是简单的字符串替换——必须跳过字符串字面量（`"A1"` 不能动）、
//! 跳过函数名（`LOG10(` 不是单元格）、认出引号包着的表名（`'我的 表'!A1`）。
//!
//! 插删行列时绝对引用也平移（`$A$1` 插行后变 `$A$2`）；复制时则分别遵守
//! 行、列的 `$` 锁定。引用落进被删掉的区域或复制后越界时变成 `#REF!`。

use crate::model::col_label;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Col,
}

/// 一次平移
#[derive(Clone, Debug)]
pub struct Shift {
    pub axis: Axis,
    /// 起始下标（0 基）
    pub at: u32,
    pub n: u32,
    pub delete: bool,
    /// 被改的工作表名（用于判断带表名的引用要不要动）
    pub target_sheet: String,
    /// 公式所在的工作表名
    pub own_sheet: String,
    /// 平移模式（复制粘贴用）：(行偏移, 列偏移)，设了就忽略 at/n/delete
    pub translate: Option<(i64, i64)>,
    /// 只影响这些行（插入/删除单元格用）
    pub row_filter: Option<(u32, u32)>,
    /// 只影响这些列（插入/删除单元格用）
    pub col_filter: Option<(u32, u32)>,
}

impl Default for Shift {
    fn default() -> Self {
        Self {
            axis: Axis::Row,
            at: 0,
            n: 0,
            delete: false,
            target_sheet: String::new(),
            own_sheet: String::new(),
            translate: None,
            row_filter: None,
            col_filter: None,
        }
    }
}

impl Shift {
    #[inline]
    pub fn axis_limit(&self) -> u32 {
        match self.axis {
            Axis::Row => crate::sync::MAX_ROWS,
            Axis::Col => crate::sync::MAX_COLS,
        }
    }

    /// 区间平移：删除时区间会「收缩」而不是整体报废（A:C 删掉 A 列 → A:B）
    pub fn map_range(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if !self.delete {
            return Some((self.map(a)?, self.map(b)?));
        }
        let del_end = self.at.saturating_add(self.n); // 不含
        // 完全在被删区之前
        if b < self.at {
            return Some((a, b));
        }
        // 完全在被删区之后
        if a >= del_end {
            return Some((a - self.n, b - self.n));
        }
        // at=0 时不能靠 at-1 判断是否还有剩余：饱和减法会把空区间变成第 1 行。
        if a >= self.at && b < del_end {
            return None;
        }
        // 被删掉之后剩下的最小/最大下标
        let new_a = if a < self.at { a } else { self.at };
        let new_b = if b >= del_end { b - self.n } else { self.at.saturating_sub(1) };
        if new_a > new_b {
            return None;
        }
        Some((new_a, new_b))
    }

    /// Bounded coordinate map aligned with `CoordTransform::map_index` (`< MAX_ROWS` / `< MAX_COLS`).
    pub fn map_bounded(&self, v: u32) -> Option<u32> {
        let limit = self.axis_limit();
        if v >= limit {
            return None;
        }
        if self.delete {
            if v < self.at {
                Some(v)
            } else if v < self.at.saturating_add(self.n) {
                None
            } else {
                Some(v - self.n)
            }
        } else if v >= self.at {
            v.checked_add(self.n).filter(|&nv| nv < limit)
        } else {
            Some(v)
        }
    }

    /// Bounded inclusive range map aligned with `CoordTransform::map_range` (`< MAX_ROWS` / `< MAX_COLS`).
    pub fn map_range_bounded(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let limit = self.axis_limit();
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if b >= limit {
            return None;
        }
        if !self.delete {
            return Some((self.map_bounded(a)?, self.map_bounded(b)?));
        }
        self.map_range(a, b).filter(|&(_, hi)| hi < limit)
    }

    /// 一个下标平移到哪；None 表示落进被删的区间
    pub fn map(&self, v: u32) -> Option<u32> {
        if self.delete {
            if v < self.at {
                Some(v)
            } else if v < self.at.saturating_add(self.n) {
                None
            } else {
                Some(v - self.n)
            }
        } else if v >= self.at {
            Some(v + self.n)
        } else {
            Some(v)
        }
    }
}

/// A sorted list of insertions is expressed in the original row coordinates.
pub(crate) struct MappedShift<'a> {
    pub shift: &'a Shift,
    pub insertions: &'a [u32],
}

impl std::ops::Deref for MappedShift<'_> {
    type Target = Shift;
    fn deref(&self) -> &Shift { self.shift }
}

impl MappedShift<'_> {
    pub fn map(&self, pos: u32) -> Option<u32> {
        if self.insertions.is_empty() { return self.shift.map(pos); }
        pos.checked_add(self.insertions.partition_point(|&at| at <= pos) as u32)
    }

    pub fn map_range(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        if self.insertions.is_empty() { return self.shift.map_range(a, b); }
        Some((self.map(a.min(b))?, self.map(a.max(b))?))
    }

    /// Bounded coordinate map aligned with `CoordTransform::map_index` (`< MAX_ROWS` / `< MAX_COLS`).
    pub fn map_bounded(&self, pos: u32) -> Option<u32> {
        if self.insertions.is_empty() {
            return self.shift.map_bounded(pos);
        }
        let limit = self.shift.axis_limit();
        if pos >= limit {
            return None;
        }
        let delta = self.insertions.partition_point(|&at| at <= pos) as u32;
        pos.checked_add(delta).filter(|&v| v < limit)
    }

    /// Bounded inclusive range map aligned with `CoordTransform::map_range` (`< MAX_ROWS` / `< MAX_COLS`).
    pub fn map_range_bounded(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        if self.insertions.is_empty() {
            return self.shift.map_range_bounded(a, b);
        }
        Some((self.map_bounded(a.min(b))?, self.map_bounded(a.max(b))?))
    }
    fn last_index(&self) -> u32 {
        match self.axis { Axis::Row => 1_048_575, Axis::Col => 16_383 }
    }

    fn map_reference(&self, index: u32) -> Option<u32> {
        self.map(index).filter(|&r| r <= self.last_index())
    }

    fn map_reference_range(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let (first, last) = self.map_range(a, b)?;
        (first <= self.last_index()).then_some((first, last.min(self.last_index())))
    }
}

/// 内部轴映射器：统一支持普通结构位移、批量插入，以及整行/整列移动的区间置换。
enum AxisMapper<'a> {
    Mapped(&'a MappedShift<'a>),
    Permute {
        shift: &'a Shift,
        from: u32,
        n: u32,
        at: u32,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AxisMove {
    pub axis: Axis,
    pub from: u32,
    pub n: u32,
    pub at: u32,
}

impl AxisMove {
    /// Convert the UI insertion line (in the original sheet) to a final coordinate.
    pub fn from_drop(axis: Axis, from: u32, n: u32, drop: u32) -> Option<Self> {
        let limit = match axis { Axis::Row => crate::sync::MAX_ROWS, Axis::Col => crate::sync::MAX_COLS };
        let end = from.checked_add(n)?;
        if n == 0 || end > limit || drop > limit || (from..=end).contains(&drop) { return None; }
        Some(Self { axis, from, n, at: if drop > from { drop - n } else { drop } })
    }

    pub fn inverse(self) -> Self { Self { from: self.at, at: self.from, ..self } }

    pub fn rewrite(self, text: &str, target: &str, owner: &str) -> String {
        permute_axis(text, self.axis, self.from, self.n, self.at, target, owner)
    }

    /// Map the complete set, including interior breakpoints. Separated pieces cannot
    /// serve as one rectangular external-data binding.
    pub fn contiguous_range(self, first: u32, last: u32) -> Option<(u32, u32)> {
        let map = |v| permute_index(v, self.from, self.n, self.at);
        let mut lo = map(first).min(map(last));
        let mut hi = map(first).max(map(last));
        for edge in [self.from.min(self.at), self.from, self.from + self.n, self.from.max(self.at) + self.n] {
            if first < edge && edge <= last {
                for point in [map(edge - 1), map(edge)] {
                    lo = lo.min(point);
                    hi = hi.max(point);
                }
            }
        }
        (hi - lo == last - first).then_some((lo, hi))
    }
}

pub(crate) fn permute_index(pos: u32, from: u32, n: u32, at: u32) -> u32 {
    let lo = from.min(at);
    let hi = (from + n).max(at + n);
    if pos < lo || pos >= hi {
        pos
    } else if pos >= from && pos < from + n {
        at + (pos - from)
    } else if at > from {
        pos - n
    } else {
        pos + n
    }
}

impl std::ops::Deref for AxisMapper<'_> {
    type Target = Shift;
    fn deref(&self) -> &Shift {
        match self {
            AxisMapper::Mapped(ms) => ms.shift,
            AxisMapper::Permute { shift, .. } => shift,
        }
    }
}

impl AxisMapper<'_> {
    fn map_reference(&self, index: u32) -> Option<u32> {
        match self {
            AxisMapper::Mapped(ms) => ms.map_reference(index),
            AxisMapper::Permute { from, n, at, .. } => Some(permute_index(index, *from, *n, *at)),
        }
    }

    fn map_reference_range(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        match self {
            AxisMapper::Mapped(ms) => ms.map_reference_range(a, b),
            AxisMapper::Permute { from, n, at, .. } => {
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                let lo = (*from).min(*at);
                let hi = (*from + *n).max(*at + *n);
                if b < lo || a >= hi {
                    // 完全落在置换区间外：保持不动
                    return Some((a, b));
                }
                if a <= lo && b >= hi - 1 {
                    // 完整覆盖整个置换区间：包含的行列集合不变
                    return Some((a, b));
                }
                let na = permute_index(a, *from, *n, *at);
                let nb = permute_index(b, *from, *n, *at);
                Some((na.min(nb), na.max(nb)))
            }
        }
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b >= 0x80
}

fn col_index(letters: &[u8]) -> Option<u32> {
    let mut v: u32 = 0;
    for b in letters {
        let d = b.to_ascii_uppercase();
        if !d.is_ascii_uppercase() {
            return None;
        }
        v = v.checked_mul(26)?.checked_add((d - b'A') as u32 + 1)?;
        if v > 16_384 {
            return None;
        }
    }
    Some(v - 1)
}

fn row_index(digits: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(digits).ok()?;
    let v: u32 = s.parse().ok()?;
    if v == 0 || v > 1_048_576 {
        return None;
    }
    Some(v - 1)
}

struct Ref {
    /// 结束位置（不含）
    end: usize,
    /// 替换文本
    text: String,
}

fn unchanged_ref(b: &[u8], start: usize, end: usize) -> Ref {
    Ref { end, text: String::from_utf8_lossy(&b[start..end]).into_owned() }
}

fn translated_axis(value: u32, offset: i64, absolute: bool, limit: i64) -> Option<u32> {
    let value = (value as i64).checked_add(if absolute { 0 } else { offset })?;
    (0..limit).contains(&value).then_some(value as u32)
}

/// 整行引用也保留每一端自己的锁定标记；列方向偏移不影响它。
fn parse_row_ref(b: &[u8], start: usize, sh: &AxisMapper, qualified: Option<&str>) -> Option<Ref> {
    let mut i = start;
    let abs1 = b.get(i) == Some(&b'$');
    i += usize::from(abs1);
    let first = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let r1 = row_index(&b[first..i])?;
    if b.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    let abs2 = b.get(i) == Some(&b'$');
    i += usize::from(abs2);
    let second = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let r2 = row_index(&b[second..i])?;
    if b.get(i).is_some_and(|c| is_ident_byte(*c) || *c == b'$' || *c == b'!') {
        return None;
    }
    if !shift_applies(sh, qualified)
        || (sh.translate.is_none()
            && (sh.axis != Axis::Row
                || sh.col_filter.is_some_and(|(a, b)| a > 0 || b < crate::sync::MAX_COLS - 1)))
    {
        return Some(unchanged_ref(b, start, i));
    }
    let mapped = match sh.translate {
        Some((dr, _)) => translated_axis(r1, dr, abs1, 1_048_576)
            .zip(translated_axis(r2, dr, abs2, 1_048_576)),
        None => sh.map_reference_range(r1, r2),
    };
    let text = match mapped {
        Some((r1, r2)) => format!(
            "{}{}:{}{}", if abs1 { "$" } else { "" }, r1 + 1,
            if abs2 { "$" } else { "" }, r2 + 1,
        ),
        None => "#REF!".into(),
    };
    Some(Ref { end: i, text })
}

/// 解析一个单元格引用或区域；识别但不需改动的引用也返回原文，以消费表名限定符。
fn parse_ref(b: &[u8], start: usize, sh: &AxisMapper, qualified_sheet: Option<&str>) -> Option<Ref> {
    let mut i = start;
    let d1 = if b.get(i) == Some(&b'$') {
        i += 1;
        true
    } else {
        false
    };
    let ls = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    let letters = &b[ls..i];
    if letters.is_empty() {
        return parse_row_ref(b, start, sh, qualified_sheet);
    }
    if letters.len() > 3 {
        return None;
    }
    let d2 = if b.get(i) == Some(&b'$') {
        i += 1;
        true
    } else {
        false
    };
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let digits = &b[ds..i];

    // 整列引用 A:C
    if digits.is_empty() {
        if b.get(i) == Some(&b':') {
            let mut j = i + 1;
            let d3 = b.get(j) == Some(&b'$');
            j += usize::from(d3);
            let ls2 = j;
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > ls2 && j - ls2 <= 3 {
                if b.get(j).is_some_and(|c| is_ident_byte(*c) || *c == b'$' || *c == b'!') {
                    return None;
                }
                if let (Some(c1), Some(c2)) = (col_index(letters), col_index(&b[ls2..j])) {
                    if (sh.translate.is_none()
                        && (sh.axis != Axis::Col
                            || sh.row_filter.is_some_and(|(a, b)| a > 0 || b < crate::sync::MAX_ROWS - 1)))
                        || !shift_applies(sh, qualified_sheet)
                    {
                        return Some(unchanged_ref(b, start, j));
                    }
                    let mapped = match sh.translate {
                        Some((_, dc)) => translated_axis(c1, dc, d1, 16_384)
                            .zip(translated_axis(c2, dc, d3, 16_384)),
                        None => sh.map_reference_range(c1, c2),
                    };
                    let text = match mapped {
                        Some((x, y)) => format!(
                            "{}{}:{}{}", if d1 { "$" } else { "" }, col_label(x),
                            if d3 { "$" } else { "" }, col_label(y),
                        ),
                        None => "#REF!".to_string(),
                    };
                    return Some(Ref { end: j, text });
                }
            }
        }
        return None;
    }

    // 函数名（LOG10( 之类）不是引用
    if b.get(i).is_some_and(|c| *c == b'(' || is_ident_byte(*c) || *c == b'$') {
        return None;
    }
    // 前面紧挨着字母/数字说明是更长标识符的一部分
    if start > 0 && is_ident_byte(b[start - 1]) {
        return None;
    }

    let c1 = col_index(letters)?;
    let r1 = row_index(digits)?;
    // 区域：A1:B2
    if b.get(i) == Some(&b':') {
        let mut j = i + 1;
        let d3 = if b.get(j) == Some(&b'$') {
            j += 1;
            true
        } else {
            false
        };
        let ls2 = j;
        while j < b.len() && b[j].is_ascii_alphabetic() {
            j += 1;
        }
        let letters2 = &b[ls2..j];
        let d4 = if b.get(j) == Some(&b'$') {
            j += 1;
            true
        } else {
            false
        };
        let ds2 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if !letters2.is_empty() && !b[ds2..j].is_empty() {
            if let (Some(c2), Some(r2)) = (col_index(letters2), row_index(&b[ds2..j])) {
                if b.get(j).is_some_and(|c| *c == b'(' || is_ident_byte(*c) || *c == b'$') {
                    return None;
                }
                if !shift_applies(sh, qualified_sheet) {
                    return Some(unchanged_ref(b, start, j));
                }
                let none2 = (None::<u32>, None::<u32>);
                if sh.translate.is_none() && (!allowed(sh, r1, c1) || !allowed(sh, r2, c2)) {
                    return Some(unchanged_ref(b, start, j));
                }
                let (p1, p2) = if sh.translate.is_some() {
                    (apply(sh, r1, c1, d2, d1), apply(sh, r2, c2, d4, d3))
                } else if sh.axis == Axis::Row {
                    // 已经 map 过了，不能再走 apply（否则平移两次）
                    match sh.map_reference_range(r1, r2) {
                        Some((x, y)) => ((Some(x), Some(c1)), (Some(y), Some(c2))),
                        None => (none2, none2),
                    }
                } else {
                    match sh.map_reference_range(c1, c2) {
                        Some((x, y)) => ((Some(r1), Some(x)), (Some(r2), Some(y))),
                        None => (none2, none2),
                    }
                };
                let (nr1, nc1) = p1;
                let (nr2, nc2) = p2;
                let text = match (nr1, nc1, nr2, nc2) {
                    (Some(a), Some(b1), Some(c), Some(d)) => format!(
                        "{}{}{}:{}{}{}",
                        if d1 { "$" } else { "" },
                        col_label(b1),
                        if d2 { format!("${}", a + 1) } else { format!("{}", a + 1) },
                        if d3 { "$" } else { "" },
                        col_label(d),
                        if d4 { format!("${}", c + 1) } else { format!("{}", c + 1) },
                    ),
                    _ => "#REF!".to_string(),
                };
                return Some(Ref { end: j, text });
            }
        }
        return None;
    }

    if !shift_applies(sh, qualified_sheet) || !allowed(sh, r1, c1) {
        return Some(unchanged_ref(b, start, i));
    }
    let (nr, nc) = apply(sh, r1, c1, d2, d1);
    let text = match (nr, nc) {
        (Some(r), Some(c)) => format!(
            "{}{}{}{}",
            if d1 { "$" } else { "" },
            col_label(c),
            if d2 { "$" } else { "" },
            r + 1
        ),
        _ => "#REF!".to_string(),
    };
    Some(Ref { end: i, text })
}

/// 平移或结构位移的统一出口
fn allowed(sh: &Shift, r: u32, c: u32) -> bool {
    sh.row_filter.is_none_or(|(a, b)| r >= a && r <= b)
        && sh.col_filter.is_none_or(|(a, b)| c >= a && c <= b)
}

fn apply(sh: &AxisMapper, r: u32, c: u32, row_absolute: bool, col_absolute: bool) -> (Option<u32>, Option<u32>) {
    if let Some((dr, dc)) = sh.translate {
        return (
            translated_axis(r, dr, row_absolute, 1_048_576),
            translated_axis(c, dc, col_absolute, 16_384),
        );
    }
    match sh.axis {
        Axis::Row => (sh.map_reference(r), Some(c)),
        Axis::Col => (Some(r), sh.map_reference(c)),
    }
}

fn shift_applies(sh: &Shift, qualified: Option<&str>) -> bool {
    // 复制时带表名的相对引用也要移动；只有结构编辑才受目标表限制。
    if sh.translate.is_some() {
        return true;
    }
    match qualified {
        Some(s) => s.eq_ignore_ascii_case(&sh.target_sheet),
        None => sh.own_sheet.eq_ignore_ascii_case(&sh.target_sheet),
    }
}

/// 复制粘贴时按 (dr, dc) 平移相对引用，分别保留被 `$` 锁定的行和列。
pub fn translate(formula: &str, dr: i64, dc: i64, _sheet: &str) -> String {
    // Copy shifts apply equally to qualified references, regardless of sheet name.
    let sh = Shift { translate: Some((dr, dc)), ..Default::default() };
    rewrite(formula, &sh)
}

/// 整行/整列移动（把 `[from, from+n)` 移至最终起始下标 `at`）时，按真实坐标置换重写公式中
/// 指向 `target_sheet` 的单元格与区域引用：
/// - 落在置换带 `[min(from,at), max(from+n,at+n))` 之外及指向其他工作表的引用保持原目标；
/// - 指向被移动行/列或被挤动行/列的引用（含绝对/混合引用）随真实坐标置换更新。
pub fn permute_axis(
    formula: &str,
    axis: Axis,
    from: u32,
    n: u32,
    at: u32,
    target_sheet: &str,
    own_sheet: &str,
) -> String {
    if n == 0 || from == at {
        return formula.to_string();
    }
    let lo = from.min(at);
    let hi = (from + n).max(at + n);
    let sh = Shift {
        axis,
        at: lo,
        n: hi - lo,
        delete: false,
        target_sheet: target_sheet.to_string(),
        own_sheet: own_sheet.to_string(),
        ..Default::default()
    };
    rewrite_with(formula, &AxisMapper::Permute { shift: &sh, from, n, at })
}

struct CopyReference {
    span: std::ops::Range<usize>,
    row: u32,
    col: u32,
    row_abs: bool,
    col_abs: bool,
}

/// Pre-tokenize ordinary A1 expressions for a fill operation. Keep literal
/// slices verbatim, including function names, operators and number spelling.
fn copy_references(src: &str) -> Option<Vec<CopyReference>> {
    let b = src.as_bytes();
    let (mut i, mut last_ref, mut need_ref) = (0, false, false);
    let mut refs = Vec::new();
    while i < b.len() {
        let start = i;
        if b[i].is_ascii_alphanumeric() || matches!(b[i], b'$' | b'_' | b'.') {
            i += 1;
            while b.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'$' | b'_' | b'.')) {
                i += 1;
            }
            let word = &src[start..i];
            if b.get(i) != Some(&b'(') && let Some(r) = crate::formula::parse_a1(word) {
                if r.row >= 1_048_576 || r.col >= 16_384 { return None; }
                refs.push(CopyReference {
                    span: start..i, row: r.row as u32, col: r.col as u32,
                    row_abs: r.row_abs, col_abs: r.col_abs,
                });
                last_ref = true;
                need_ref = false;
                continue;
            }
            let function = b.get(i) == Some(&b'(') && !word.contains('$') && b[start].is_ascii_alphabetic();
            if need_ref || (!function && !word.bytes().all(|c| c.is_ascii_digit() || c == b'.')) {
                return None;
            }
        } else {
            if need_ref { return None; }
            match b[i] {
                b':' if last_ref => need_ref = true,
                b'+' | b'-' | b'*' | b'/' | b'^' | b'&' | b'%' | b'=' | b'>' | b'<' | b',' | b'(' | b')' => {}
                _ => return None,
            }
            i += 1;
        }
        last_ref = false;
    }
    (!need_ref).then_some(refs)
}

fn append_copy_column(out: &mut String, col: u32) {
    if col >= 26 { append_copy_column(out, col / 26 - 1); }
    out.push((b'A' + (col % 26) as u8) as char);
}

/// Reuse the source reference locations across destinations of one fill. The
/// cache is bounded even when the user fills a column from many distinct formulas.
#[derive(Default)]
pub struct FillFormulas {
    sources: std::collections::HashMap<Box<str>, Option<Vec<CopyReference>>>,
}

impl FillFormulas {
    pub fn translate(&mut self, src: &str, dr: i64, dc: i64) -> String {
        use std::fmt::Write;
        let refs = match self.sources.get(src) {
            Some(cached) => cached,
            None if self.sources.len() >= 1024 => return translate(src, dr, dc, ""),
            None => self.sources.entry(src.into()).or_insert_with(|| copy_references(src)),
        };
        let Some(refs) = refs else { return translate(src, dr, dc, ""); };
        let mut out = String::with_capacity(src.len() + refs.len() * 3);
        let mut end = 0;
        for r in refs {
            let Some(row) = translated_axis(r.row, dr, r.row_abs, 1_048_576) else { return translate(src, dr, dc, ""); };
            let Some(col) = translated_axis(r.col, dc, r.col_abs, 16_384) else { return translate(src, dr, dc, ""); };
            out.push_str(&src[end..r.span.start]);
            if r.col_abs { out.push('$'); }
            append_copy_column(&mut out, col);
            if r.row_abs { out.push('$'); }
            write!(out, "{}", row + 1).unwrap();
            end = r.span.end;
        }
        out.push_str(&src[end..]);
        out
    }
}

/// 跳过 `[Book]...` 外部限定符（含 `[1]!`、`[1]Sheet!`、`[1]First:Third!`、`[1]'First':'Third'!`），
/// 只有结尾确实跟 `!` 才返回 bang 后的位置；否则可能是结构化表列引用（`Table1[Col]`），返回 `None`。
fn consume_ext_qualifier(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i;
    let mut depth = 0usize;
    while j < b.len() {
        if b[j] == b'[' {
            depth += 1;
        } else if b[j] == b']' {
            depth -= 1;
        }
        j += 1;
        if depth == 0 {
            break;
        }
    }
    if depth != 0 {
        return None;
    }
    if b.get(j) == Some(&b'!') {
        return Some(j + 1);
    }
    let skip_sheet = |idx: usize| -> usize {
        if b.get(idx) == Some(&b'\'') {
            quoted_end(b, idx, b'\'')
        } else {
            let mut k = idx;
            while b.get(k).is_some_and(|c| is_ident_byte(*c)) {
                k += 1;
            }
            k
        }
    };
    j = skip_sheet(j);
    if b.get(j) == Some(&b':') {
        j = skip_sheet(j + 1);
    }
    (b.get(j) == Some(&b'!')).then_some(j + 1)
}

/// 跳过单引号包住书名/表名的外部限定符（如 `'[1]First:Third'!` 或 `'[1]First 1':'Third 3'!`）。
fn consume_quoted_ext_qualifier(b: &[u8], formula: &str, i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'\'') {
        return None;
    }
    let qe = quoted_end(b, i, b'\'');
    if qe <= i + 1 || b.get(qe - 1) != Some(&b'\'') {
        return None;
    }
    if !formula[i + 1..qe - 1].contains('[') {
        return None;
    }
    if b.get(qe) == Some(&b'!') {
        return Some(qe + 1);
    }
    if b.get(qe) == Some(&b':') {
        let mut j = qe + 1;
        if b.get(j) == Some(&b'\'') {
            j = quoted_end(b, j, b'\'');
        } else {
            while b.get(j).is_some_and(|c| is_ident_byte(*c)) {
                j += 1;
            }
        }
        if b.get(j) == Some(&b'!') {
            return Some(j + 1);
        }
    }
    None
}

/// 剪切/拖动源块 `src_rect = (r1, c1, r2, c2)` 从 `src_sheet` 平移 `(dr, dc)` 到 `dst_sheet` 后，
/// 重写任意公式中指向 `src_rect` 内部的单元格与区域引用（包括 `$A$1` 绝对引用，与 Excel 剪切/拖拽一致）。
pub fn relocate_moved_rect(
    formula: &str,
    own_sheet: &str,
    src_sheet: &str,
    src_rect: (u32, u32, u32, u32),
    dst_sheet: &str,
    dr: i64,
    dc: i64,
) -> String {
    relocate_moved_rows(formula, own_sheet, src_sheet, src_rect, dst_sheet, dr, dc, None, false)
}

/// A filtered cut compacts its sorted, visible source rows into contiguous destination rows.
/// Rewrite each token once so overlapping source and destination addresses cannot cascade.
pub fn relocate_moved_rows(
    formula: &str, own_sheet: &str, src_sheet: &str, src_rect: (u32,u32,u32,u32),
    dst_sheet: &str, dr: i64, dc: i64, rows: Option<&[u32]>, formula_moves: bool,
) -> String {
    let same_sheet = src_sheet.eq_ignore_ascii_case(dst_sheet);
    if rows.is_none() && same_sheet && dr == 0 && dc == 0 {
        return formula.to_string();
    }
    let (sr1, sc1, sr2, sc2) = (
        src_rect.0.min(src_rect.2),
        src_rect.1.min(src_rect.3),
        src_rect.0.max(src_rect.2),
        src_rect.1.max(src_rect.3),
    );
    let in_rect = |r: u32, c: u32| r >= sr1 && r <= sr2 && c >= sc1 && c <= sc2
        && rows.is_none_or(|visible| visible.binary_search(&r).is_ok());
    let whole_range_moved = |a: u32, b: u32| rows.is_none_or(|visible| {
        let lo = a.min(b);
        let hi = a.max(b);
        visible.partition_point(|&r| r <= hi) - visible.partition_point(|&r| r < lo)
            == (hi - lo + 1) as usize
    });
    let map_pt = |r: u32, c: u32| -> Option<(u32, u32)> {
        let offset_row = match rows {
            Some(visible) => sr1 + visible.binary_search(&r).ok()? as u32,
            None => r,
        };
        let nr = i64::from(offset_row).checked_add(dr)?;
        let nc = (c as i64).checked_add(dc)?;
        if (0..1_048_576).contains(&nr) && (0..16_384).contains(&nc) {
            Some((nr as u32, nc as u32))
        } else {
            None
        }
    };
    let fmt_qual = |name: &str| -> String {
        let needs_q = name.is_empty()
            || !name.bytes().all(|x| x.is_ascii_alphanumeric() || x == b'_' || x >= 0x80)
            || name.as_bytes()[0].is_ascii_digit();
        if needs_q {
            format!("'{}'!", name.replace('\'', "''"))
        } else {
            format!("{name}!")
        }
    };

    let b = formula.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(formula.len() + 16);
    let mut i = 0usize;
    let mut qual: Option<(String, usize)> = None; // (sheet_name, start_in_out)
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'"' {
                    if b.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.extend_from_slice(&b[start..i]);
            qual = None;
            continue;
        }
        if let Some(after_bang) = consume_quoted_ext_qualifier(b, formula, i) {
            out.extend_from_slice(&b[i..after_bang]);
            qual = Some(("\0EXT".into(), out.len()));
            i = after_bang;
            continue;
        }
        if c == b'\'' {
            let start = i;
            let out_start = out.len();
            i += 1;
            let ns = i;
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            let name = &formula[ns..i];
            if i < b.len() {
                i += 1;
            }
            out.extend_from_slice(&b[start..i]);
            if b.get(i) == Some(&b'!') {
                qual = Some((name.replace("''", "'"), out_start));
                out.push(b'!');
                i += 1;
            }
            continue;
        }
        if let Some(after_bang) = consume_ext_qualifier(b, i) {
            out.extend_from_slice(&b[i..after_bang]);
            qual = Some(("\0EXT".into(), out.len()));
            i = after_bang;
            continue;
        }
        if c == b'[' {
            let start = i;
            let mut depth = 0usize;
            while i < b.len() {
                match b[i] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            out.extend_from_slice(&b[start..i]);
            qual = None;
            continue;
        }
        if c.is_ascii_alphabetic() || c >= 0x80 {
            let start = i;
            let mut j = i;
            while j < b.len()
                && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.' || b[j] >= 0x80)
            {
                j += 1;
            }
            if b.get(j) == Some(&b'!') {
                let out_start = out.len();
                out.extend_from_slice(&b[start..=j]);
                qual = Some((formula[start..j].to_string(), out_start));
                i = j + 1;
                continue;
            }
        }
        if (c == b'$' || c.is_ascii_alphabetic())
            && (i == 0 || !is_ident_byte(b[i - 1]))
        {
            let mut p = i;
            let d1 = if b.get(p) == Some(&b'$') { p += 1; true } else { false };
            let ls = p;
            while p < b.len() && b[p].is_ascii_alphabetic() { p += 1; }
            let letters = &b[ls..p];
            if !letters.is_empty() && letters.len() <= 3 {
                let d2 = if b.get(p) == Some(&b'$') { p += 1; true } else { false };
                let ds = p;
                while p < b.len() && b[p].is_ascii_digit() { p += 1; }
                let digits = &b[ds..p];
                if !digits.is_empty()
                    && !b.get(p).is_some_and(|x| *x == b'(' || is_ident_byte(*x) || *x == b'$')
                    && let (Some(c1), Some(r1)) = (col_index(letters), row_index(digits))
                {
                    let targets_src = match &qual {
                        Some((q, _)) => q.eq_ignore_ascii_case(src_sheet),
                        None => own_sheet.eq_ignore_ascii_case(src_sheet),
                    };
                    // Check range A1:B2
                    if b.get(p) == Some(&b':') {
                        let mut q = p + 1;
                        let d3 = if b.get(q) == Some(&b'$') { q += 1; true } else { false };
                        let ls2 = q;
                        while q < b.len() && b[q].is_ascii_alphabetic() { q += 1; }
                        let letters2 = &b[ls2..q];
                        let d4 = if b.get(q) == Some(&b'$') { q += 1; true } else { false };
                        let ds2 = q;
                        while q < b.len() && b[q].is_ascii_digit() { q += 1; }
                        let digits2 = &b[ds2..q];
                        if !letters2.is_empty()
                            && !digits2.is_empty()
                            && !b.get(q).is_some_and(|x| *x == b'(' || is_ident_byte(*x) || *x == b'$')
                            && let (Some(c2), Some(r2)) = (col_index(letters2), row_index(digits2))
                        {
                            if targets_src && in_rect(r1, c1) && in_rect(r2, c2) && whole_range_moved(r1, r2) {
                                if !same_sheet {
                                    if let Some((_, qstart)) = qual.take() {
                                        out.truncate(qstart);
                                    }
                                    if !formula_moves && !own_sheet.eq_ignore_ascii_case(dst_sheet) {
                                        out.extend_from_slice(fmt_qual(dst_sheet).as_bytes());
                                    }
                                }
                                let txt = match (map_pt(r1, c1), map_pt(r2, c2)) {
                                    (Some((nr1, nc1)), Some((nr2, nc2))) => format!(
                                        "{}{}{}{}:{}{}{}{}",
                                        if d1 { "$" } else { "" }, col_label(nc1),
                                        if d2 { "$" } else { "" }, nr1 + 1,
                                        if d3 { "$" } else { "" }, col_label(nc2),
                                        if d4 { "$" } else { "" }, nr2 + 1,
                                    ),
                                    _ => "#REF!".to_string(),
                                };
                                out.extend_from_slice(txt.as_bytes());
                            } else {
                                if formula_moves && !same_sheet && qual.is_none() {
                                    out.extend_from_slice(fmt_qual(src_sheet).as_bytes());
                                }
                                out.extend_from_slice(&b[i..q]);
                            }
                            i = q;
                            qual = None;
                            continue;
                        }
                    }
                    if targets_src && in_rect(r1, c1) {
                        if !same_sheet {
                            if let Some((_, qstart)) = qual.take() {
                                out.truncate(qstart);
                            }
                            if !formula_moves && !own_sheet.eq_ignore_ascii_case(dst_sheet) {
                                out.extend_from_slice(fmt_qual(dst_sheet).as_bytes());
                            }
                        }
                        let txt = match map_pt(r1, c1) {
                            Some((nr, nc)) => format!(
                                "{}{}{}{}",
                                if d1 { "$" } else { "" }, col_label(nc),
                                if d2 { "$" } else { "" }, nr + 1,
                            ),
                            None => "#REF!".to_string(),
                        };
                        out.extend_from_slice(txt.as_bytes());
                    } else {
                        if formula_moves && !same_sheet && qual.is_none() {
                            out.extend_from_slice(fmt_qual(src_sheet).as_bytes());
                        }
                        out.extend_from_slice(&b[i..p]);
                    }
                    i = p;
                    qual = None;
                    continue;
                }
            }
        }
        if matches!(c, b'!' | b'+' | b'-' | b'*' | b'/' | b'^' | b'&' | b',' | b';' | b'(' | b')' | b'=' | b'<' | b'>') {
            qual = None;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 改写一条公式（不带前导 `=` 也可以）
pub fn rewrite(formula: &str, sh: &Shift) -> String {
    rewrite_mapped(formula, &MappedShift { shift: sh, insertions: &[] })
}

pub(crate) fn rewrite_mapped(formula: &str, sh: &MappedShift) -> String {
    rewrite_with(formula, &AxisMapper::Mapped(sh))
}

fn rewrite_with(formula: &str, sh: &AxisMapper) -> String {
    let b = formula.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(formula.len() + 8);
    let mut i = 0usize;
    let mut qual: Option<String> = None;
    while i < b.len() {
        let c = b[i];
        // 字符串字面量：原样搬，内部的 "" 是转义
        if c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'"' {
                    if b.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.extend_from_slice(&b[start..i]);
            continue;
        }
        // 带引号的外部限定符：'[1]Sheet1'! / '[1]First:Third'!
        if let Some(after_bang) = consume_quoted_ext_qualifier(b, formula, i) {
            out.extend_from_slice(&b[i..after_bang]);
            qual = Some("\0EXT".into());
            i = after_bang;
            continue;
        }
        // 带引号的表名：'Sheet 1'!
        if c == b'\'' {
            let start = i;
            i += 1;
            let ns = i;
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            let name = &formula[ns..i];
            if i < b.len() {
                i += 1; // 收尾引号
            }
            out.extend_from_slice(&b[start..i]);
            // 后跟 ! 才算表名限定（顺手把这个 ! 吃掉，否则下面会被当成
            // 新的限定符起点，把刚记下的表名清掉）
            if b.get(i) == Some(&b'!') {
                qual = Some(name.replace("''", "'"));
                out.push(b'!');
                i += 1;
            }
            continue;
        }
        // 外部限定符 `[1]First:Third!` / `[1]Sheet1!`：复制粘贴平移其后的 A1，结构 edits 不动外部引用。
        if let Some(after_bang) = consume_ext_qualifier(b, i) {
            out.extend_from_slice(&b[i..after_bang]);
            qual = Some("\0EXT".into());
            i = after_bang;
            continue;
        }
        // 表格列名和外部工作簿名中的 A1 等字样不是单元格引用。
        if c == b'[' {
            let start = i;
            let mut depth = 0usize;
            while i < b.len() {
                match b[i] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            out.extend_from_slice(&b[start..i]);
            continue;
        }
        // 裸表名：Sheet2!A1 / 数据!A1（表名可以是中文）
        if c.is_ascii_alphabetic() || c >= 0x80 {
            let start = i;
            let mut j = i;
            while j < b.len()
                && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.' || b[j] >= 0x80)
            {
                j += 1;
            }
            if b.get(j) == Some(&b'!') {
                out.extend_from_slice(&b[start..=j]);
                qual = Some(formula[start..j].to_string());
                i = j + 1;
                continue;
            }
        }
        // 引用
        if c == b'$' || c.is_ascii_alphabetic() || c.is_ascii_digit() {
            let boundary = i == 0 || !is_ident_byte(b[i - 1]);
            if boundary {
                if let Some(r) = parse_ref(b, i, sh, qual.as_deref()) {
                    out.extend_from_slice(r.text.as_bytes());
                    i = r.end;
                    qual = None;
                    continue;
                }
            }
        }
        if matches!(c, b'!' | b'+' | b'-' | b'*' | b'/' | b'^' | b'&' | b',' | b';' | b'(' | b')' | b'=' | b'<' | b'>') {
            qual = None;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Rename a local sheet qualifier, or replace a deleted sheet reference with
/// `#REF!`. Strings, structured column names and external workbook references
/// are copied intact. Coordinates keep their original spelling and `$` flags.
/// 从 `i` 读一个表名（引号表名或裸标识符），返回 (名字, 结尾)。
fn name_at(b: &[u8], formula: &str, i: usize) -> Option<(String, usize)> {
    if b.get(i) == Some(&b'\'') {
        let end = quoted_end(b, i, b'\'');
        if end <= i + 1 || b.get(end - 1) != Some(&b'\'') {
            return None;
        }
        return Some((formula[i + 1..end - 1].replace("''", "'"), end));
    }
    let mut end = i;
    while b.get(end).is_some_and(|c| is_ident_byte(*c)) {
        end += 1;
    }
    (end > i).then(|| (formula[i..end].to_owned(), end))
}

/// 识别 `Sheet1:Sheet3!` 的三维限定符。返回：
/// (bang 之后的位置, 起始表名, 结束表名, 起始名结束, 结束名结束)。
fn scan_3d(b: &[u8], formula: &str, i: usize) -> Option<(usize, String, String, usize, usize)> {
    let (a, ea) = name_at(b, formula, i)?;
    if b.get(ea) != Some(&b':') {
        return None;
    }
    let (z, ez) = name_at(b, formula, ea + 1)?;
    (b.get(ez) == Some(&b'!')).then_some((ez + 1, a, z, ea, ez))
}

/// 识别整段引号的三维区间：`'Start 1:End 3'!`。
/// 返回 (bang 后位置, 起始名, 结束名, 结束引号后位置)。
fn scan_quoted_3d(b: &[u8], formula: &str, i: usize) -> Option<(usize, String, String, usize)> {
    if b.get(i) != Some(&b'\'') {
        return None;
    }
    let qe = quoted_end(b, i, b'\'');
    if qe <= i + 2 || b.get(qe) != Some(&b'!') || b.get(qe - 1) != Some(&b'\'') {
        return None;
    }
    let content = &formula[i + 1..qe - 1];
    let (a, z) = content.split_once(':')?;
    if a.is_empty() || z.is_empty() || a.contains('[') || z.contains('[') {
        return None;
    }
    Some((qe + 1, a.replace("''", "'"), z.replace("''", "'"), qe))
}

fn whole_3d_quote(a: &str, z: &str) -> String {
    format!("'{}:{}'!", a.replace('\'', "''"), z.replace('\'', "''"))
}

/// 三维表名在公式里的显示形式；有空格、引号等字符时加单引号。
fn quote_3d_sheet(s: &str) -> String {
    if !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.'))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

/// 删除端点时沿用原公式的引号风格，避免中文/空格名被改写成另一种
/// 合法但用户没写过的形式。
fn render_endpoint(name: &str, was_quoted: bool) -> String {
    if was_quoted {
        format!("'{}'", name.replace('\'', "''"))
    } else if name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.'))
    {
        name.to_owned()
    } else {
        quote_3d_sheet(name)
    }
}

pub fn rewrite_sheet(formula: &str, old: &str, new: Option<&str>) -> String {
    let b = formula.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let identity = Shift {
        axis: Axis::Row, at: 0, n: 0, delete: false,
        target_sheet: String::new(), own_sheet: String::new(), translate: None,
        row_filter: None,
        col_filter: None,
    };
    while i < b.len() {
        let start = i;
        if b[i] == b'"' {
            i = quoted_end(b, i, b'"');
            out.extend_from_slice(&b[start..i]);
            continue;
        }
        if let Some(after_bang) = consume_quoted_ext_qualifier(b, formula, i) {
            out.extend_from_slice(&b[i..after_bang]);
            i = after_bang;
            continue;
        }
        if let Some(after_bang) = consume_ext_qualifier(b, i) {
            out.extend_from_slice(&b[i..after_bang]);
            i = after_bang;
            continue;
        }
        if b[i] == b'[' {
            // A nested table selector, or the workbook part of [Book]Sheet!A1.
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'[' { depth += 1; }
                if b[i] == b']' { depth -= 1; }
                i += 1;
                if depth == 0 { break; }
            }
            // Consume an external sheet qualifier too, so it cannot be renamed
            // merely because it shares the local sheet's name.
            let mut end = i;
            if b.get(end) == Some(&b'\'') {
                end = quoted_end(b, end, b'\'');
            } else {
                while b.get(end).is_some_and(|c| is_ident_byte(*c)) { end += 1; }
            }
            if b.get(end) == Some(&b'!') { i = end + 1; }
            out.extend_from_slice(&b[start..i]);
            continue;
        }
        if let Some((after_bang, a, z, _qe)) = scan_quoted_3d(b, formula, i) {
            let hit_a = a.eq_ignore_ascii_case(old);
            let hit_z = z.eq_ignore_ascii_case(old);
            if let Some(new) = new {
                let sa = if hit_a { new } else { a.as_str() };
                let sz = if hit_z { new } else { z.as_str() };
                out.extend_from_slice(whole_3d_quote(sa, sz).as_bytes());
                i = after_bang;
            } else if hit_a || hit_z {
                out.extend_from_slice(b"#REF!");
                i = consume_ref_after(b, after_bang, &identity);
            } else {
                out.extend_from_slice(&b[i..after_bang]);
                i = after_bang;
            }
            continue;
        }
        if let Some((after_bang, a, z, ea, ez)) = scan_3d(b, formula, i) {
            let hit_a = a.eq_ignore_ascii_case(old);
            let hit_z = z.eq_ignore_ascii_case(old);
            if let Some(new) = new {
                if hit_a {
                    out.extend_from_slice(quote_3d_sheet(new).as_bytes());
                } else {
                    out.extend_from_slice(&b[i..ea]);
                }
                out.push(b':');
                if hit_z {
                    out.extend_from_slice(quote_3d_sheet(new).as_bytes());
                } else {
                    out.extend_from_slice(&b[ea + 1..ez]);
                }
                out.push(b'!');
                i = after_bang;
            } else if hit_a || hit_z {
                // 删除三维区间端点：整个三维引用失去端点，按 Excel 的
                // 失败方式写成 #REF!，并吞掉后面的单元格/区域。
                let mut j = after_bang;
                if b[j..].starts_with(b"#REF!") {
                    j += 5;
                } else if let Some(reference) = parse_ref(b, j, &AxisMapper::Mapped(&MappedShift { shift: &identity, insertions: &[] }), None) {
                    j = reference.end;
                    // 三维引用也可能是 A1:B2/整行区间；吞掉第二个端点，
                    // 否则会留下 `#REF!:B2` 这种不可解析的残片。
                    if b.get(j) == Some(&b':') {
                        if let Some(second) = parse_ref(b, j + 1, &AxisMapper::Mapped(&MappedShift { shift: &identity, insertions: &[] }), None) {
                            j = second.end;
                        }
                    }
                } else {
                    while b.get(j).is_some_and(|c| is_ident_byte(*c) || *c == b'$') {
                        j += 1;
                    }
                }
                if b.get(j) == Some(&b'#') {
                    j += 1;
                }
                out.extend_from_slice(b"#REF!");
                i = j;
            } else {
                out.extend_from_slice(&b[i..after_bang]);
                i = after_bang;
            }
            continue;
        }
        let name = if b[i] == b'\'' {
            i = quoted_end(b, i, b'\'');
            if i > start + 1 && b[i - 1] == b'\'' {
                Some(formula[start + 1..i - 1].replace("''", "'"))
            } else { None }
        } else if is_ident_byte(b[i]) {
            while b.get(i).is_some_and(|c| is_ident_byte(*c)) { i += 1; }
            Some(formula[start..i].to_owned())
        } else {
            if b[i] >= 0x80 {
                let ch = formula[i..].chars().next().expect("valid UTF-8");
                let n = ch.len_utf8();
                out.extend_from_slice(&b[i..i + n]);
                i += n;
                continue;
            }
            i += 1;
            None
        };
        if b.get(i) == Some(&b'!') && name.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(old))
            && (start == 0 || !matches!(b[start - 1], b':' | b'#'))
        {
            if let Some(new) = new {
                out.extend_from_slice(format!("'{}'!", new.replace('\'', "''")).as_bytes());
                i += 1;
            } else {
                i += 1;
                // Consume a complete A1/range reference (including whole rows
                // and columns), or a sheet-scoped name after the qualifier.
                if b[i..].starts_with(b"#REF!") {
                    i += 5;
                } else if let Some(reference) = parse_ref(b, i, &AxisMapper::Mapped(&MappedShift { shift: &identity, insertions: &[] }), None) {
                    i = reference.end;
                } else {
                    while b.get(i).is_some_and(|c| is_ident_byte(*c) || *c == b'$') { i += 1; }
                }
                if b.get(i) == Some(&b'#') { i += 1; }
                out.extend_from_slice(b"#REF!");
            }
        } else {
            out.extend_from_slice(&b[start..i]);
        }
    }
    String::from_utf8(out).expect("reference rewriting preserves UTF-8")
}

/// 删除表格时，三维区间的端点要收缩到**删前区间内的相邻表**。
/// 不能拿全局 next/prev：`Third:First` 的 end 在低索引侧，
/// `Second:Second` 删掉唯一成员必须整段 `#REF!`，不能写半截。
pub fn retarget_3d_endpoint(formula: &str, old: &str, order: &[String]) -> String {
    let b = formula.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    let identity = Shift {
        axis: Axis::Row,
        at: 0,
        n: 0,
        delete: false,
        target_sheet: String::new(),
        own_sheet: String::new(),
        translate: None,
        row_filter: None,
        col_filter: None,
    };
    while i < b.len() {
        let start = i;
        if b[i] == b'"' {
            i = quoted_end(b, i, b'"');
            out.extend_from_slice(&b[start..i]);
            continue;
        }
        if let Some(after_bang) = consume_quoted_ext_qualifier(b, formula, i) {
            out.extend_from_slice(&b[i..after_bang]);
            i = after_bang;
            continue;
        }
        if let Some(after_bang) = consume_ext_qualifier(b, i) {
            out.extend_from_slice(&b[i..after_bang]);
            i = after_bang;
            continue;
        }
        if let Some((after_bang, a, z, _qe)) = scan_quoted_3d(b, formula, i) {
            let hit_a = a.eq_ignore_ascii_case(old);
            let hit_z = z.eq_ignore_ascii_case(old);
            if hit_a || hit_z {
                let sa = if hit_a {
                    neighbor_name(order, old, &a, &a, &z, true)
                } else {
                    Some(a.clone())
                };
                let sz = if hit_z {
                    neighbor_name(order, old, &z, &a, &z, false)
                } else {
                    Some(z.clone())
                };
                match (sa, sz) {
                    (Some(sa), Some(sz)) => {
                        out.extend_from_slice(whole_3d_quote(&sa, &sz).as_bytes());
                        i = after_bang;
                    }
                    _ => {
                        out.extend_from_slice(b"#REF!");
                        i = consume_ref_after(b, after_bang, &identity);
                    }
                }
            } else {
                out.extend_from_slice(&b[i..after_bang]);
                i = after_bang;
            }
            continue;
        }
        if let Some((after_bang, a, z, ea, ez)) = scan_3d(b, formula, i) {
            let hit_a = a.eq_ignore_ascii_case(old);
            let hit_z = z.eq_ignore_ascii_case(old);
            if hit_a || hit_z {
                let sa = if hit_a {
                    neighbor_name(order, old, &a, &a, &z, true)
                } else {
                    Some(a.clone())
                };
                let sz = if hit_z {
                    neighbor_name(order, old, &z, &a, &z, false)
                } else {
                    Some(z.clone())
                };
                let (Some(sa), Some(sz)) = (sa, sz) else {
                    out.extend_from_slice(b"#REF!");
                    i = consume_ref_after(b, after_bang, &identity);
                    continue;
                };
                if hit_a {
                    out.extend_from_slice(
                        render_endpoint(&sa, b.get(i) == Some(&b'\'')).as_bytes(),
                    );
                } else {
                    out.extend_from_slice(&b[i..ea]);
                }
                out.push(b':');
                if hit_z {
                    out.extend_from_slice(
                        render_endpoint(&sz, b.get(ea + 1) == Some(&b'\'')).as_bytes(),
                    );
                } else {
                    out.extend_from_slice(&b[ea + 1..ez]);
                }
                out.push(b'!');
                i = after_bang;
            } else {
                out.extend_from_slice(&b[i..after_bang]);
                i = after_bang;
            }
            continue;
        }
        if b[i] >= 0x80 {
            // UTF-8 字符必须整字前进；否则下一轮会从第二字节调用
            // name_at/scan_3d，formula[i..end] 直接 panic。
            let ch = formula[i..].chars().next().expect("valid UTF-8");
            let n = ch.len_utf8();
            out.extend_from_slice(&b[i..i + n]);
            i += n;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("reference rewriting preserves UTF-8")
}

/// 删除 `endpoint` 时的收缩目标。返回 None 表示区间已无剩余成员，
/// 整个三维引用必须变成 `#REF!`。
fn neighbor_name(
    order: &[String],
    old: &str,
    endpoint: &str,
    a: &str,
    z: &str,
    start: bool,
) -> Option<String> {
    if !endpoint.eq_ignore_ascii_case(old) {
        return Some(endpoint.to_owned());
    }
    let (Some(po), Some(pa), Some(pz)) = (
        order.iter().position(|s| s.eq_ignore_ascii_case(old)),
        order.iter().position(|s| s.eq_ignore_ascii_case(a)),
        order.iter().position(|s| s.eq_ignore_ascii_case(z)),
    ) else {
        // 外部表或缺失端点：没有本地顺序可调整，保持原文更安全。
        return Some(endpoint.to_owned());
    };
    let lo = pa.min(pz);
    let hi = pa.max(pz);
    if po < lo || po > hi {
        return Some(endpoint.to_owned());
    }
    let candidates: Vec<usize> = (lo..=hi).filter(|p| *p != po).collect();
    if candidates.is_empty() {
        return None;
    }
    let pick = if start {
        if pa < pz {
            candidates.iter().copied().filter(|p| *p > po).min()
        } else {
            candidates.iter().copied().filter(|p| *p < po).max()
        }
    } else if pz > pa {
        candidates.iter().copied().filter(|p| *p < pz).max()
    } else {
        candidates.iter().copied().filter(|p| *p > pz).min()
    };
    pick.map(|p| order[p].clone())
}

fn consume_ref_after(b: &[u8], mut j: usize, identity: &Shift) -> usize {
    if b[j..].starts_with(b"#REF!") {
        return j + 5;
    }
    if let Some(reference) = parse_ref(b, j, &AxisMapper::Mapped(&MappedShift { shift: identity, insertions: &[] }), None) {
        j = reference.end;
        if b.get(j) == Some(&b':') {
            if let Some(second) = parse_ref(b, j + 1, &AxisMapper::Mapped(&MappedShift { shift: identity, insertions: &[] }), None) {
                j = second.end;
            }
        }
    } else {
        while b.get(j).is_some_and(|c| is_ident_byte(*c) || *c == b'$') {
            j += 1;
        }
    }
    if b.get(j) == Some(&b'#') {
        j += 1;
    }
    j
}

fn quoted_end(b: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        if b[i] == quote {
            i += 1;
            if b.get(i) != Some(&quote) { return i; }
        }
        i += 1;
    }
    i
}

/// 整行引用 1:3
pub fn rewrite_row_range(text: &str, sh: &Shift) -> String {
    // 只处理形如 "3:7" 的裸行区间（放在 rewrite 之后兜底用）
    let t = text.trim();
    if let Some((a, b)) = t.split_once(':') {
        if let (Ok(x), Ok(y)) = (a.parse::<u32>(), b.parse::<u32>()) {
            if sh.axis == Axis::Row && shift_applies(sh, None) {
                return match (sh.map(x - 1), sh.map(y - 1)) {
                    (Some(p), Some(q)) => format!("{}:{}", p + 1, q + 1),
                    _ => "#REF!".into(),
                };
            }
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_row(at: u32, n: u32) -> Shift {
        Shift {
            axis: Axis::Row,
            at,
            n,
            delete: false,
            target_sheet: "S".into(),
            own_sheet: "S".into(),
            translate: None,
            row_filter: None,
            col_filter: None,
        }
    }
    fn delete_row(at: u32, n: u32) -> Shift {
        Shift { delete: true, ..insert_row(at, n) }
    }
    fn insert_col(at: u32, n: u32) -> Shift {
        Shift { axis: Axis::Col, ..insert_row(at, n) }
    }
    fn delete_col(at: u32, n: u32) -> Shift {
        Shift { axis: Axis::Col, delete: true, ..insert_row(at, n) }
    }

    #[test]
    fn insert_row_moves_refs() {
        assert_eq!(rewrite("=A1+1", &insert_row(0, 1)), "=A2+1");
        assert_eq!(rewrite("=A5*2", &insert_row(0, 2)), "=A7*2");
        assert_eq!(rewrite("=A1+B9", &insert_row(3, 1)), "=A1+B10");
    }

    #[test]
    fn absolute_refs_move_too() {
        assert_eq!(rewrite("=$A$1+$B2", &insert_row(0, 1)), "=$A$2+$B3");
    }

    #[test]
    fn delete_row_gives_ref_error() {
        assert_eq!(rewrite("=A2+1", &delete_row(1, 1)), "=#REF!+1");
        assert_eq!(rewrite("=A3+1", &delete_row(1, 1)), "=A2+1");
        assert_eq!(rewrite("=A1+1", &delete_row(1, 1)), "=A1+1");
    }

    #[test]
    fn ranges_shift_on_both_ends() {
        assert_eq!(rewrite("=SUM(A1:A5)", &insert_row(0, 1)), "=SUM(A2:A6)");
        assert_eq!(rewrite("=SUM($A$1:$A$5)", &insert_row(2, 1)), "=SUM($A$1:$A$6)");
        // 区间收缩：只删掉一部分时保留活下来的那一格
        assert_eq!(rewrite("=SUM(A1:A5)", &delete_row(1, 4)), "=SUM(A1:A1)");
        // 整段都被删掉才是 #REF!
        assert_eq!(rewrite("=SUM(A2:A5)", &delete_row(1, 4)), "=SUM(#REF!)");
    }

    #[test]
    fn columns_and_whole_columns() {
        assert_eq!(rewrite("=B1*2", &insert_col(1, 1)), "=C1*2");
        assert_eq!(rewrite("=SUM(B:B)", &insert_col(1, 1)), "=SUM(C:C)");
        assert_eq!(rewrite("=SUM(A:C)", &delete_col(0, 1)), "=SUM(A:B)");
        // 行方向的改动不该动列引用
        assert_eq!(rewrite("=B1*2", &insert_row(0, 3)), "=B4*2");
    }

    #[test]
    fn strings_and_functions_untouched() {
        assert_eq!(rewrite("=\"A1 不是引用\"&A1", &insert_row(0, 1)), "=\"A1 不是引用\"&A2");
        assert_eq!(rewrite("=LOG10(A1)", &insert_row(0, 1)), "=LOG10(A2)");
        assert_eq!(rewrite("=LOG10(100)", &insert_row(0, 1)), "=LOG10(100)");
        assert_eq!(rewrite("=IF(A1>0,\"A1\",\"\")", &insert_row(0, 1)), "=IF(A2>0,\"A1\",\"\")");
    }

    #[test]
    fn sheet_qualifiers_respected() {
        let sh = Shift {
            axis: Axis::Row,
            at: 0,
            n: 1,
            delete: false,
            target_sheet: "数据".into(),
            own_sheet: "汇总".into(),
            translate: None,
            row_filter: None,
            col_filter: None,
        };
        // 指着被改的表 → 跟着动
        assert_eq!(rewrite("=数据!A1", &sh), "=数据!A2");
        assert_eq!(rewrite("='数据'!A1", &sh), "='数据'!A2");
        // 指着别的表 → 不动
        assert_eq!(rewrite("=别处!A1", &sh), "=别处!A1");
        // 不带表名 = 本表（汇总），不是被改的表 → 不动
        assert_eq!(rewrite("=A1", &sh), "=A1");
    }

    #[test]
    fn leaves_plain_numbers_alone() {
        assert_eq!(rewrite("=1+2*3", &insert_row(0, 1)), "=1+2*3");
        assert_eq!(rewrite("=SUM(1,2)", &insert_row(0, 1)), "=SUM(1,2)");
    }
}

#[cfg(test)]
mod cell_shift_tests {
    use super::*;

    #[test]
    fn cell_insert_only_moves_refs_in_the_selected_columns() {
        let sh = Shift {
            axis: Axis::Row,
            at: 2,
            n: 1,
            delete: false,
            target_sheet: "Sheet1".into(),
            own_sheet: "Sheet1".into(),
            col_filter: Some((0, 0)),
            ..Default::default()
        };
        assert_eq!(rewrite("A5+A1+B5", &sh), "A6+A1+B5");
        let del = Shift { delete: true, ..sh };
        assert_eq!(rewrite("A1+A3+A9", &del), "A1+#REF!+A8");
    }

    #[test]
    fn cell_insert_horizontal_moves_refs_in_the_selected_rows() {
        let sh = Shift {
            axis: Axis::Col,
            at: 1,
            n: 1,
            delete: false,
            target_sheet: "Sheet1".into(),
            own_sheet: "Sheet1".into(),
            row_filter: Some((0, 0)),
            ..Default::default()
        };
        assert_eq!(rewrite("B1+C1+B2", &sh), "C1+D1+B2");
    }
}
