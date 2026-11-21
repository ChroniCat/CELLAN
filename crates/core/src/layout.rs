//! 行列尺寸：绝大多数列/行用默认值，只有被显式设置过的才记一条。
//!
//! 位置计算不能按 `i * 默认宽` 算——一旦某列被拉宽，后面所有列都要顺移。
//! 这里维护「自定义项 + 累计偏移」，位置查询是 O(log n)，改一项是 O(n)（改尺寸很少发生）。

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

fn layout_revision() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A snapshot can share a set of hidden indices until its first edit.
/// Revisions distinguish divergent snapshots even if they hide the same number of rows.
#[derive(Clone, Debug)]
pub struct HiddenIndices {
    indices: std::sync::Arc<HashSet<u32>>,
    revision: u64,
}

impl From<HashSet<u32>> for HiddenIndices {
    fn from(indices: HashSet<u32>) -> Self {
        Self { indices: std::sync::Arc::new(indices), revision: layout_revision() }
    }
}

impl Default for HiddenIndices {
    fn default() -> Self { HashSet::new().into() }
}

impl HiddenIndices {
    pub fn revision(&self) -> u64 { self.revision }
}

impl PartialEq for HiddenIndices {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision || self.indices == other.indices
    }
}

impl std::ops::Deref for HiddenIndices {
    type Target = HashSet<u32>;
    fn deref(&self) -> &Self::Target { &self.indices }
}

impl std::ops::DerefMut for HiddenIndices {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.revision = layout_revision();
        std::sync::Arc::make_mut(&mut self.indices)
    }
}


/// Excel 96 DPI 的默认列宽（54 pt = 72 px）
pub const DEFAULT_COL_W: f32 = 72.0;
/// Excel 96 DPI 的默认行高（14.25 pt = 19 px）
pub const DEFAULT_ROW_H: f32 = 18.0;

#[derive(Clone, Debug)]
pub struct Axis {
    revision: u64,
    default: f32,
    sizes: BTreeMap<u32, f32>,
    /// 自定义项的索引和结束位置。连续隐藏项的结束位置完全相同，可二分跳过。
    prefix: Vec<(u32, f64)>,
}

impl Default for Axis {
    fn default() -> Self {
        Self::new(DEFAULT_COL_W)
    }
}

impl Axis {
    pub fn new(default: f32) -> Self {
        Self { revision: layout_revision(), default, sizes: BTreeMap::new(), prefix: Vec::new() }
    }

    pub fn revision(&self) -> u64 { self.revision }

    pub fn set(&mut self, i: u32, size: f32) {
        if (size - self.default).abs() < 0.01 {
            self.sizes.remove(&i);
        } else {
            self.sizes.insert(i, size);
        }
        self.rebuild();
    }

    /// 批量将指定行/列尺寸设为 0（筛选/手动隐藏）：一次性写入后只 rebuild 一遍，
    /// 避免在数万行筛选表上逐行 `set(r, 0.0)` 退化成 O(N²)。
    pub fn hide_many(&mut self, indices: impl IntoIterator<Item = u32>) {
        let mut changed = false;
        for i in indices {
            if self.sizes.insert(i, 0.0) != Some(0.0) {
                changed = true;
            }
        }
        if changed {
            self.rebuild();
        }
    }

    pub(crate) fn remap_indices(&mut self, map: impl Fn(u32) -> Option<u32>) {
        if self.sizes.is_empty() { return; }
        self.sizes = std::mem::take(&mut self.sizes).into_iter()
            .filter_map(|(index, size)| map(index).map(|new| (new, size))).collect();
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.revision = layout_revision();
        self.prefix.clear();
        self.prefix.reserve(self.sizes.len());
        let mut next = 0u64;
        let mut end = 0.0;
        for (&i, &size) in &self.sizes {
            end += (u64::from(i) - next) as f64 * f64::from(self.default) + f64::from(size);
            self.prefix.push((i, end));
            next = u64::from(i) + 1;
        }
    }

    /// 第 i 项的起点（相对内容原点）
    #[inline]
    pub fn pos(&self, i: u32) -> f32 {
        self.start_position(i) as f32
    }

    #[inline]
    fn start_position(&self, i: u32) -> f64 {
        let preceding = self.prefix.partition_point(|&(idx, _)| idx < i);
        match self.prefix[..preceding].last() {
            Some(&(idx, end)) => end + f64::from(i - idx - 1) * f64::from(self.default),
            None => f64::from(i) * f64::from(self.default),
        }
    }

    /// 从 i（含）直达第一个有尺寸的行/列。跳过整段隐藏项只需一次二分。
    pub fn next_visible(&self, i: u32) -> Option<u32> {
        if self.size(i) > 0.0 {
            return Some(i);
        }
        let start = self.start_position(i);
        let after = self.prefix.partition_point(|&(_, end)| end <= start);
        if self.default > 0.0 {
            self.prefix[..after].last()?.0.checked_add(1)
        } else {
            self.prefix.get(after).map(|&(idx, _)| idx)
        }
    }

    pub fn visible_indices(&self, range: std::ops::Range<u32>) -> impl Iterator<Item = u32> + '_ {
        let mut next = range.start;
        std::iter::from_fn(move || {
            if next >= range.end {
                return None;
            }
            let i = self.next_visible(next)?;
            if i >= range.end {
                return None;
            }
            next = i + 1;
            Some(i)
        })
    }

    pub fn size(&self, i: u32) -> f32 {
        self.sizes.get(&i).copied().unwrap_or(self.default)
    }

    /// 像素位置落在哪一项（用于点击命中、滚动定位）
    pub fn index_at(&self, px: f32) -> u32 {
        if px <= 0.0 {
            return self.next_visible(0).unwrap_or(0);
        }
        // pos() 单调递增，二分即可；上界按「全默认」估计再加自定义项数
        let mut hi = if self.default > 0.0 {
            ((px / self.default).ceil() as u32).saturating_add(self.sizes.len() as u32).saturating_add(2)
        } else {
            self.prefix.last().map_or(0, |&(idx, _)| idx)
        };
        let mut lo = 0u32;
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if self.pos(mid) <= px {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    }

    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    /// 所有自定义尺寸（(索引, 大小)）
    pub fn custom_pairs(&self) -> Vec<(u32, f32)> {
        self.sizes.iter().map(|(k, v)| (*k, *v)).collect()
    }

    /// 清掉所有自定义尺寸
    pub fn clear_custom(&mut self) {
        self.sizes.clear();
        self.rebuild();
    }

    /// 按缩放倍数复制一份轴：默认尺寸与自定义尺寸一起缩放，隐藏(0)仍是 0。
    /// 只影响视图几何，不回写模型。
    pub fn scaled(&self, z: f32) -> Self {
        let mut out = Axis::new(self.default * z);
        for (i, w) in self.sizes.iter() {
            out.sizes.insert(*i, *w * z);
        }
        out.rebuild();
        out
    }

    pub fn entries(&self) -> impl Iterator<Item = (&u32, &f32)> {
        self.sizes.iter()
    }

    pub fn default_size(&self) -> f32 {
        self.default
    }

    /// 改默认尺寸（Excel 的 `<col min="1" max="16384">` 就是改整表默认列宽）。
    /// 已记下的绝对尺寸不受影响。
    pub fn set_default(&mut self, size: f32) {
        if (size - self.default).abs() > 0.01 {
            self.default = size;
            // 原来的自定义项相对旧默认的"额外偏移"要重算
            self.rebuild();
        }
    }
}

/// Excel 列宽单位（字符数）→ 像素。
///
/// 本机 Excel 16.0 实测（96dpi，Calibri/宋体 11pt）：显示宽 = 字符数 × 8 + 5px。
/// 8 来自 `Truncate(((256*w + Truncate(128/MDW))/256)*MDW)` 且 MDW=8 的像素量化，
/// +5 是单元格内边距；默认列宽 8.38 字符 → 72px。
pub fn excel_width_to_px(w: f32) -> f32 {
    (w * 8.0 + 5.0).round().max(8.0)
}

/// `<col width="21.25">` → 175px（21.25×8+5）。默认宽度 8.38 走同一公式。
pub fn col_width_to_px(w: f32) -> f32 {
    if w <= 0.0 {
        return 0.0;
    }
    (w * 8.0 + 5.0).round()
}

/// 行高单位是磅（pt）→ 像素（96 DPI）
pub fn pt_to_px(pt: f32) -> f32 {
    (pt * 96.0 / 72.0).round().max(4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_when_empty() {
        let a = Axis::new(100.0);
        assert_eq!(a.pos(0), 0.0);
        assert_eq!(a.pos(3), 300.0);
        assert_eq!(a.size(5), 100.0);
        assert_eq!(a.index_at(250.0), 2);
        assert_eq!(a.index_at(300.0), 3);
    }

    #[test]
    fn shifted_by_custom_width() {
        let mut a = Axis::new(100.0);
        a.set(0, 200.0);
        assert_eq!(a.pos(0), 0.0);
        assert_eq!(a.pos(1), 200.0);
        assert_eq!(a.pos(2), 300.0);
        assert_eq!(a.size(0), 200.0);
        assert_eq!(a.index_at(150.0), 0);
        assert_eq!(a.index_at(250.0), 1);
        assert_eq!(a.index_at(350.0), 2);
    }

    #[test]
    fn many_custom_widths() {
        let mut a = Axis::new(64.0);
        for i in 0..50 {
            a.set(i, 64.0 + (i as f32) * 3.0);
        }
        // 第 50 项起点 = 前面 50 个自定义宽度之和
        let expect: f32 = (0..50).map(|i| 64.0 + i as f32 * 3.0).sum();
        assert!((a.pos(50) - expect).abs() < 0.5, "{} vs {}", a.pos(50), expect);
        assert_eq!(a.index_at(expect + 5.0), 50);
        // 反查一致性
        for i in 0..50 {
            assert_eq!(a.index_at(a.pos(i) + 1.0), i);
        }
    }

    #[test]
    fn reset_to_default_removes_entry() {
        let mut a = Axis::new(100.0);
        a.set(2, 300.0);
        assert_eq!(a.len(), 1);
        a.set(2, 100.0);
        assert_eq!(a.len(), 0);
        assert_eq!(a.pos(3), 300.0);
    }

    #[test]
    fn viewport_skips_twenty_thousand_hidden_rows_at_fractional_zoom() {
        let mut axis = Axis::new(18.0);
        axis.hide_many(2..20_002);
        axis.set(20_003, 30.0);
        let axis = axis.scaled(0.73);
        let edge = axis.pos(2);
        assert_eq!(axis.pos(20_002), edge);
        assert_eq!(axis.index_at(edge), 20_002);
        assert_eq!(axis.index_at(edge - 0.05), 1);
        assert_eq!(axis.next_visible(2), Some(20_002));
        assert_eq!(axis.next_visible(19_999), Some(20_002));
        assert_eq!(axis.visible_indices(1..20_005).collect::<Vec<_>>(), [1, 20_002, 20_003, 20_004]);
        assert_eq!(axis.visible_indices(2..19_999).next(), None);
    }

    #[test]
    fn visible_indices_on_hidden_default_axis_find_only_sized_entries() {
        let mut axis = Axis::new(0.0);
        axis.set(20_000, 18.0);
        axis.set(20_004, 18.0);
        assert_eq!(axis.index_at(0.0), 20_000);
        assert_eq!(axis.visible_indices(0..30_000).collect::<Vec<_>>(), [20_000, 20_004]);
    }

    #[test]
    fn excel_units() {
        assert_eq!(excel_width_to_px(8.43), 72.0);
        assert_eq!(col_width_to_px(21.25), 175.0);
        assert_eq!(col_width_to_px(9.0), 77.0);
        assert_eq!(pt_to_px(15.0), 20.0);
        assert_eq!(pt_to_px(30.0), 40.0);
    }

    #[test]
    fn scaled_axis_scales_default_and_custom_but_not_hidden() {
        let mut a = Axis::new(100.0);
        a.set(2, 150.0);
        a.set(3, 0.0);
        let b = a.scaled(2.0);
        assert_eq!(b.size(0), 200.0);
        assert_eq!(b.size(2), 300.0);
        assert_eq!(b.size(3), 0.0, "隐藏行/列缩放后仍是 0");
        assert_eq!(b.pos(2), 400.0);
        assert_eq!(a.size(0), 100.0, "原轴不能被改");
    }
}
