//! 行列尺寸：绝大多数列/行用默认值，只有被显式设置过的才记一条。
//!
//! 位置计算不能按 `i * 默认宽` 算——一旦某列被拉宽，后面所有列都要顺移。
//! 这里维护「自定义项 + 累计偏移」，位置查询是 O(log n)，改一项是 O(n)（改尺寸很少发生）。

use std::collections::BTreeMap;

pub const DEFAULT_COL_W: f32 = 104.0;
pub const DEFAULT_ROW_H: f32 = 22.0;

#[derive(Clone, Debug)]
pub struct Axis {
    default: f32,
    sizes: BTreeMap<u32, f32>,
    /// 与 sizes 同序的累计偏移：(索引, 该索引之前的额外偏移)
    prefix: Vec<(u32, f32)>,
}

impl Default for Axis {
    fn default() -> Self {
        Self::new(DEFAULT_COL_W)
    }
}

impl Axis {
    pub fn new(default: f32) -> Self {
        Self { default, sizes: BTreeMap::new(), prefix: Vec::new() }
    }

    pub fn set(&mut self, i: u32, size: f32) {
        if (size - self.default).abs() < 0.01 {
            self.sizes.remove(&i);
        } else {
            self.sizes.insert(i, size);
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.prefix.clear();
        let mut extra = 0.0;
        for (i, w) in self.sizes.iter() {
            self.prefix.push((*i, extra));
            extra += *w - self.default;
        }
    }

    /// 第 i 项的起点（相对内容原点）
    pub fn pos(&self, i: u32) -> f32 {
        i as f32 * self.default + self.extra_before(i)
    }

    fn extra_before(&self, i: u32) -> f32 {
        // 找最后一个 < i 的自定义项
        let idx = self.prefix.partition_point(|(k, _)| *k < i);
        if idx == 0 {
            return 0.0;
        }
        let (k, extra) = self.prefix[idx - 1];
        extra + (self.sizes[&k] - self.default)
    }

    pub fn size(&self, i: u32) -> f32 {
        self.sizes.get(&i).copied().unwrap_or(self.default)
    }

    /// 像素位置落在哪一项（用于点击命中、滚动定位）
    pub fn index_at(&self, px: f32) -> u32 {
        if px <= 0.0 {
            return 0;
        }
        // pos() 单调递增，二分即可；上界按「全默认」估计再加自定义项数
        let mut hi = (px / self.default).ceil() as u32 + self.sizes.len() as u32 + 2;
        let mut lo = 0u32;
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
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

    pub fn entries(&self) -> impl Iterator<Item = (&u32, &f32)> {
        self.sizes.iter()
    }

    pub fn default_size(&self) -> f32 {
        self.default
    }
}

/// Excel 列宽单位（字符数）→ 像素。默认字体 11pt 时 1 字符 ≈ 7px，再加 5px 内边距。
pub fn excel_width_to_px(w: f32) -> f32 {
    (w * 7.0 + 5.0).round().max(8.0)
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
    fn excel_units() {
        assert_eq!(excel_width_to_px(8.43), 64.0);
        assert_eq!(excel_width_to_px(16.0), 117.0);
        assert_eq!(pt_to_px(15.0), 20.0);
        assert_eq!(pt_to_px(30.0), 40.0);
    }
}
