//! 现代图表分析、统计与排版计算内核（与 UI/PDF/SVG 解耦，三端共享一套数学与视觉基准）。
//!
//! 包含：
//! - 8 套金融/出版级配色主题 (`ChartThemeSpec`)
//! - Heckbert 智能刻度算法（线性与对数轴）+ 智能数字格式化
//! - 6 类回归与趋势线拟合（线性 OLS、2-4 阶多项式高斯消元、指数、对数、幂函数、N 期移动平均）含方程与 R²
//! - 误差线计算（固定值/百分比/标准差/标准误）
//! - 参考线与目标警示带求值（常量/均值/中位数/极值/分位数/区间带/±1σ带）
//! - 财务瀑布图桥接计算、漏斗图转化率计算、Squarified 矩形树图布局、直方图自动分箱（Freedman-Diaconis）+ 帕累托累积线、箱线图五数摘要+离群点
//! - 饼/环外部标签垂直防撞布局算法
//! - 智能选区剖析与 Top-N 图表推荐引擎 + 一键行列互换 (`switch_row_col`)

use crate::charts::{
    AxisConfig, Chart, ChartKind, ChartThemeId, ErrorBar, ErrorBarKind, RefLineMode, ReferenceLine,
    Series, Trendline, TrendlineKind,
};
use crate::model::{col_label, DateSystem, Sheet, Value};
use crate::style::{format_value_in, StyleTable};

// =============================================================================
// 1. 出版级配色主题库（8 套金融/研报/BI 主题）
// =============================================================================

#[derive(Clone, Copy, Debug)]
pub struct ChartThemeSpec {
    pub id: ChartThemeId,
    pub bg: [u8; 3],
    pub plot_bg: [u8; 3],
    pub border: [u8; 3],
    pub grid: [u8; 3],
    pub minor_grid: [u8; 3],
    pub axis_line: [u8; 3],
    pub title_fg: [u8; 3],
    pub subtitle_fg: [u8; 3],
    pub text_fg: [u8; 3],
    pub muted_fg: [u8; 3],
    pub palette: [[u8; 3]; 10],
    pub pos_color: [u8; 3],   // 瀑布图正增 / 上涨
    pub neg_color: [u8; 3],   // 瀑布图负减 / 下跌
    pub total_color: [u8; 3], // 瀑布图期初/总计
    pub is_dark: bool,
}

pub fn theme_spec(id: ChartThemeId) -> ChartThemeSpec {
    match id {
        ChartThemeId::CellanModern => ChartThemeSpec {
            id,
            bg: [255, 255, 255],
            plot_bg: [252, 253, 255],
            border: [222, 228, 236],
            grid: [234, 239, 245],
            minor_grid: [244, 247, 250],
            axis_line: [188, 198, 212],
            title_fg: [23, 33, 47],
            subtitle_fg: [104, 118, 138],
            text_fg: [64, 76, 94],
            muted_fg: [125, 138, 156],
            palette: [
                [37, 99, 235],   // Cobalt Blue
                [14, 165, 233],  // Sky Cyan
                [245, 158, 11],  // Amber Gold
                [16, 185, 129],  // Emerald
                [139, 92, 246],  // Violet
                [239, 68, 68],   // Coral Red
                [236, 72, 153],  // Rose
                [20, 184, 166],  // Teal
                [99, 102, 241],  // Indigo
                [100, 116, 139], // Slate
            ],
            pos_color: [16, 163, 111],
            neg_color: [224, 68, 68],
            total_color: [37, 99, 235],
            is_dark: false,
        },
        ChartThemeId::Financial => ChartThemeSpec {
            id,
            bg: [253, 252, 249],
            plot_bg: [253, 252, 249],
            border: [216, 210, 199],
            grid: [231, 227, 218],
            minor_grid: [242, 239, 233],
            axis_line: [120, 125, 132],
            title_fg: [26, 29, 34],
            subtitle_fg: [96, 102, 110],
            text_fg: [56, 62, 70],
            muted_fg: [118, 124, 132],
            palette: [
                [0, 107, 162],   // Economist Blue
                [219, 68, 75],   // Financial Crimson
                [62, 188, 210],  // Cyan Slate
                [55, 154, 139],  // Deep Teal
                [235, 180, 52],  // Brass Gold
                [180, 186, 195], // Neutral Gray
                [154, 96, 127],  // Plum
                [212, 117, 45],  // Ochre
                [42, 75, 114],   // Navy
                [117, 141, 153], // Steel
            ],
            pos_color: [55, 154, 139],
            neg_color: [219, 68, 75],
            total_color: [0, 107, 162],
            is_dark: false,
        },
        ChartThemeId::Executive => ChartThemeSpec {
            id,
            bg: [255, 255, 255],
            plot_bg: [255, 255, 255],
            border: [208, 218, 230],
            grid: [232, 238, 246],
            minor_grid: [243, 246, 251],
            axis_line: [130, 148, 172],
            title_fg: [12, 35, 64],
            subtitle_fg: [86, 106, 132],
            text_fg: [36, 56, 82],
            muted_fg: [112, 128, 148],
            palette: [
                [5, 28, 44],     // Deep Navy
                [0, 115, 207],   // Strategy Blue
                [0, 169, 244],   // Bright Cyan
                [122, 146, 168], // Cool Gray
                [0, 168, 135],   // Consulting Teal
                [229, 107, 31],  // Accent Orange
                [108, 92, 231],  // Deep Indigo
                [192, 57, 43],   // Alert Red
                [44, 62, 80],    // Charcoal
                [162, 181, 198], // Mist
            ],
            pos_color: [0, 168, 135],
            neg_color: [192, 57, 43],
            total_color: [5, 28, 44],
            is_dark: false,
        },
        ChartThemeId::Morandi => ChartThemeSpec {
            id,
            bg: [252, 251, 248],
            plot_bg: [252, 251, 248],
            border: [228, 223, 216],
            grid: [238, 234, 228],
            minor_grid: [246, 243, 239],
            axis_line: [182, 175, 166],
            title_fg: [62, 58, 54],
            subtitle_fg: [128, 121, 114],
            text_fg: [92, 86, 80],
            muted_fg: [146, 139, 131],
            palette: [
                [114, 137, 158], // Dusty Blue
                [198, 142, 126], // Terracotta
                [137, 161, 143], // Sage Green
                [209, 178, 128], // Warm Sand
                [158, 137, 161], // Muted Mauve
                [125, 161, 165], // Fog Teal
                [186, 130, 138], // Dusty Rose
                [168, 157, 142], // Taupe
                [101, 118, 134], // Slate Blue
                [189, 166, 148], // Clay
            ],
            pos_color: [121, 155, 131],
            neg_color: [194, 112, 104],
            total_color: [104, 126, 148],
            is_dark: false,
        },
        ChartThemeId::ColorblindSafe => ChartThemeSpec {
            id,
            bg: [255, 255, 255],
            plot_bg: [255, 255, 255],
            border: [215, 220, 228],
            grid: [232, 236, 242],
            minor_grid: [244, 246, 250],
            axis_line: [140, 146, 156],
            title_fg: [20, 24, 30],
            subtitle_fg: [94, 102, 114],
            text_fg: [50, 56, 66],
            muted_fg: [118, 126, 138],
            // Okabe-Ito colorblind-safe publication palette
            palette: [
                [0, 114, 178],   // Blue
                [230, 159, 0],   // Orange
                [0, 158, 115],   // Bluish Green
                [213, 94, 0],    // Vermillion
                [204, 121, 167], // Reddish Purple
                [86, 180, 233],  // Sky Blue
                [240, 228, 66],  // Yellow
                [51, 51, 51],    // Dark Charcoal
                [119, 170, 221], // Light Blue
                [153, 153, 153], // Gray
            ],
            pos_color: [0, 158, 115],
            neg_color: [213, 94, 0],
            total_color: [0, 114, 178],
            is_dark: false,
        },
        ChartThemeId::DarkBi => ChartThemeSpec {
            id,
            bg: [18, 24, 38],
            plot_bg: [24, 32, 50],
            border: [48, 61, 88],
            grid: [42, 54, 78],
            minor_grid: [33, 43, 64],
            axis_line: [94, 112, 146],
            title_fg: [241, 245, 252],
            subtitle_fg: [154, 170, 198],
            text_fg: [206, 217, 236],
            muted_fg: [132, 149, 178],
            palette: [
                [56, 189, 248],  // Neon Cyan
                [251, 191, 36],  // Bright Amber
                [52, 211, 153],  // Neon Emerald
                [167, 139, 250], // Soft Purple
                [251, 113, 133], // Neon Rose
                [96, 165, 250],  // Electric Blue
                [244, 114, 182], // Pink
                [45, 212, 191],  // Aqua
                [251, 146, 60],  // Vivid Orange
                [148, 163, 184], // Cool Silver
            ],
            pos_color: [52, 211, 153],
            neg_color: [251, 113, 133],
            total_color: [56, 189, 248],
            is_dark: true,
        },
        ChartThemeId::Monochrome => ChartThemeSpec {
            id,
            bg: [255, 255, 255],
            plot_bg: [255, 255, 255],
            border: [190, 190, 190],
            grid: [228, 228, 228],
            minor_grid: [242, 242, 242],
            axis_line: [80, 80, 80],
            title_fg: [18, 18, 18],
            subtitle_fg: [96, 96, 96],
            text_fg: [48, 48, 48],
            muted_fg: [120, 120, 120],
            palette: [
                [30, 30, 30],
                [82, 82, 82],
                [128, 128, 128],
                [168, 168, 168],
                [56, 56, 56],
                [105, 105, 105],
                [148, 148, 148],
                [192, 192, 192],
                [40, 40, 40],
                [115, 115, 115],
            ],
            pos_color: [55, 55, 55],
            neg_color: [155, 155, 155],
            total_color: [20, 20, 20],
            is_dark: false,
        },
        ChartThemeId::OfficeClassic => ChartThemeSpec {
            id,
            bg: [255, 255, 255],
            plot_bg: [255, 255, 255],
            border: [217, 217, 217],
            grid: [232, 232, 232],
            minor_grid: [244, 244, 244],
            axis_line: [176, 176, 176],
            title_fg: [51, 51, 51],
            subtitle_fg: [115, 115, 115],
            text_fg: [89, 89, 89],
            muted_fg: [130, 130, 130],
            palette: [
                [68, 114, 196],
                [237, 125, 49],
                [165, 165, 165],
                [255, 192, 0],
                [91, 155, 213],
                [112, 173, 71],
                [38, 68, 120],
                [158, 72, 14],
                [99, 99, 99],
                [153, 115, 0],
            ],
            pos_color: [112, 173, 71],
            neg_color: [237, 125, 49],
            total_color: [68, 114, 196],
            is_dark: false,
        },
    }
}

pub fn series_rgb(chart: &Chart, series_idx: usize) -> [u8; 3] {
    if let Some(c) = chart.series.get(series_idx).and_then(|s| s.color) {
        return c;
    }
    let th = theme_spec(chart.style.theme);
    th.palette[series_idx % th.palette.len()]
}

// =============================================================================
// 2. Heckbert 智能刻度算法（线性 + 对数）与数字格式化
// =============================================================================

#[derive(Clone, Debug)]
pub struct AxisScale {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub ticks: Vec<f64>,
    pub log_base: Option<f64>,
    pub reverse: bool,
}

impl AxisScale {
    /// 将数值映射为归一化坐标 `[0.0, 1.0]`（0 = 起点/底部，1 = 终点/顶部；考虑对数轴与反转轴）
    pub fn norm(&self, v: f64) -> f32 {
        let raw = if let Some(base) = self.log_base {
            let lb = base.ln().max(1e-9);
            let lo = self.min.max(1e-12).ln() / lb;
            let hi = self.max.max(self.min * base).ln() / lb;
            let span = (hi - lo).max(1e-12);
            let lv = v.max(1e-12).ln() / lb;
            ((lv - lo) / span).clamp(0.0, 1.0)
        } else {
            let span = (self.max - self.min).max(1e-12);
            ((v - self.min) / span).clamp(0.0, 1.0)
        };
        let t = if self.reverse { 1.0 - raw } else { raw };
        t as f32
    }

    pub fn zero_norm(&self) -> f32 {
        if self.log_base.is_some() {
            if self.reverse { 1.0 } else { 0.0 }
        } else {
            self.norm(0.0)
        }
    }
}

fn nice_num(range: f64, round: bool) -> f64 {
    if !range.is_finite() || range <= 0.0 {
        return 1.0;
    }
    let exp = range.log10().floor();
    let frac = range / 10f64.powf(exp);
    let nice_frac = if round {
        if frac < 1.5 {
            1.0
        } else if frac < 3.0 {
            2.0
        } else if frac < 7.0 {
            5.0
        } else {
            10.0
        }
    } else if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice_frac * 10f64.powf(exp)
}

/// 根据数据极值与轴配置计算人类友好的整齐刻度（对齐 Excel/科研标准）
pub fn compute_axis_scale(
    data_min: f64,
    data_max: f64,
    cfg: &AxisConfig,
    include_zero: bool,
    target_ticks: usize,
) -> AxisScale {
    if let Some(base) = cfg.log_base.filter(|b| *b > 1.0) {
        let pos_min = cfg.min.unwrap_or(data_min).max(1e-6);
        let pos_max = cfg.max.unwrap_or(data_max).max(pos_min * base);
        let lb = base.ln();
        let e0 = (pos_min.ln() / lb).floor() as i32;
        let e1 = (pos_max.ln() / lb).ceil().max((e0 + 1) as f64) as i32;
        let mut ticks = Vec::new();
        for e in e0..=e1.min(e0 + 12) {
            ticks.push(base.powi(e));
        }
        return AxisScale {
            min: base.powi(e0),
            max: base.powi(e1.min(e0 + 12)),
            step: base,
            ticks,
            log_base: Some(base),
            reverse: cfg.reverse,
        };
    }

    let mut dmin = if data_min.is_finite() { data_min } else { 0.0 };
    let mut dmax = if data_max.is_finite() { data_max } else { 1.0 };
    if include_zero {
        dmin = dmin.min(0.0);
        dmax = dmax.max(0.0);
    }
    if (dmax - dmin).abs() < 1e-12 {
        if dmax >= 0.0 {
            dmax = (dmax + 1.0).max(1.0);
            if include_zero {
                dmin = 0.0;
            }
        } else {
            dmin -= 1.0;
        }
    }

    let lo_in = cfg.min.unwrap_or(dmin);
    let mut hi_in = cfg.max.unwrap_or(dmax);
    if hi_in <= lo_in {
        hi_in = lo_in + 1.0;
    }

    let tick_count = target_ticks.clamp(3, 10);
    let step = if let Some(u) = cfg.major_unit.filter(|u| u.is_finite() && *u > 0.0) {
        u
    } else {
        let nice_range = nice_num(hi_in - lo_in, false);
        nice_num(nice_range / (tick_count.saturating_sub(1).max(1) as f64), true)
    };
    let step = step.max(1e-12);

    let min = match cfg.min {
        Some(m) => m,
        None => (lo_in / step).floor() * step,
    };
    let max = match cfg.max {
        Some(m) => m,
        None => {
            let ceil_hi = (hi_in / step).ceil() * step;
            // 若紧贴最大值且有余量空间，保持整洁上限
            if ceil_hi <= min {
                min + step
            } else {
                ceil_hi
            }
        }
    };

    let mut ticks = Vec::new();
    let max_steps = (((max - min) / step).ceil() as usize).clamp(1, 20);
    for i in 0..=max_steps {
        let mut v = min + step * i as f64;
        if v.abs() < step * 1e-9 {
            v = 0.0;
        }
        if v > max + step * 1e-6 {
            break;
        }
        ticks.push(v);
    }
    if ticks.is_empty() {
        ticks.push(min);
        ticks.push(max);
    }

    AxisScale {
        min,
        max,
        step,
        ticks,
        log_base: None,
        reverse: cfg.reverse,
    }
}

/// 统一数字格式化：有自定义格式串时直调 `sheetcore::style::format_value_in`，否则用智能紧凑财务格式
pub fn format_chart_number(v: f64, fmt: &str, sys: DateSystem) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let clean = if v.abs() < 1e-12 { 0.0 } else { v };
    if !fmt.is_empty() && !fmt.eq_ignore_ascii_case("General") {
        let out = format_value_in(sys, &Value::Number(clean), fmt);
        if !out.is_empty() {
            return out;
        }
    }
    let abs = clean.abs();
    if abs >= 1e8 {
        format!("{:.2}亿", clean / 1e8)
    } else if abs >= 1e6 {
        format!("{:.1}M", clean / 1e6)
    } else if abs >= 10_000.0 {
        let k = clean / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{:.0}k", k)
        } else {
            format!("{:.1}k", k)
        }
    } else if (clean - clean.round()).abs() < 1e-6 {
        format!("{:.0}", clean)
    } else if abs >= 100.0 {
        format!("{:.1}", clean)
    } else if abs >= 1.0 {
        let s = format!("{:.2}", clean);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        let s = format!("{:.3}", clean);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn format_percent(frac: f64) -> String {
    if !frac.is_finite() {
        return String::new();
    }
    let pct = frac * 100.0;
    if (pct - pct.round()).abs() < 0.05 {
        format!("{:.0}%", pct)
    } else {
        format!("{:.1}%", pct)
    }
}

// =============================================================================
// 3. 回归与趋势线引擎（线性 / 2-4阶多项式 / 指数 / 对数 / 幂 / 移动平均）
// =============================================================================

#[derive(Clone, Debug)]
pub struct ComputedTrendline {
    /// 拟合与预测采样点 `(x, y)`，x 采用绘图域坐标（分类图用 0..N-1，散点图用真实 X）
    pub points: Vec<(f64, f64)>,
    pub equation: String,
    pub r_squared: Option<f64>,
}

/// 高斯消元解线性方程组 `A x = b`（带列主元选主，稳定求解 2-4 阶多项式最小二乘）
fn solve_linear_system(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let mut pivot = col;
        let mut best = a[col][col].abs();
        for row in (col + 1)..n {
            if a[row][col].abs() > best {
                best = a[row][col].abs();
                pivot = row;
            }
        }
        if best < 1e-14 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let div = a[col][col];
        for j in col..n {
            a[col][j] /= div;
        }
        b[col] /= div;
        for row in 0..n {
            if row == col {
                continue;
            }
            let f = a[row][col];
            if f.abs() > 1e-18 {
                for j in col..n {
                    a[row][j] -= f * a[col][j];
                }
                b[row] -= f * b[col];
            }
        }
    }
    Some(b)
}

fn r2_score(ys: &[f64], preds: &[f64]) -> Option<f64> {
    if ys.len() < 2 || ys.len() != preds.len() {
        return None;
    }
    let mean = ys.iter().sum::<f64>() / ys.len() as f64;
    let ss_tot: f64 = ys.iter().map(|y| (y - mean).powi(2)).sum();
    let ss_res: f64 = ys
        .iter()
        .zip(preds.iter())
        .map(|(y, p)| (y - p).powi(2))
        .sum();
    if ss_tot < 1e-14 {
        return Some(1.0);
    }
    Some((1.0 - ss_res / ss_tot).clamp(0.0, 1.0))
}

fn fmt_coeff(v: f64) -> String {
    if v.abs() >= 1000.0 || (v.abs() > 0.0 && v.abs() < 0.001) {
        format!("{v:.2e}")
    } else {
        format!("{v:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// 计算单条趋势线：输入原始 `(x, y)` 点对，返回曲线采样点、回归方程与 $R^2$
pub fn compute_trendline(raw_pts: &[(f64, f64)], tl: &Trendline) -> Option<ComputedTrendline> {
    let pts: Vec<(f64, f64)> = raw_pts
        .iter()
        .copied()
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let xmin = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let xmax = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let draw_x0 = xmin - tl.backward.max(0.0);
    let draw_x1 = xmax + tl.forward.max(0.0);

    match tl.kind {
        TrendlineKind::MovingAverage(period) => {
            let p = (period as usize).clamp(2, pts.len());
            let mut out = Vec::new();
            for i in (p - 1)..pts.len() {
                let sum: f64 = pts[(i + 1 - p)..=i].iter().map(|q| q.1).sum();
                out.push((pts[i].0, sum / p as f64));
            }
            Some(ComputedTrendline {
                points: out,
                equation: format!("{p} 期移动平均"),
                r_squared: None,
            })
        }
        TrendlineKind::Linear => {
            let n = pts.len() as f64;
            let sx: f64 = pts.iter().map(|p| p.0).sum();
            let sy: f64 = pts.iter().map(|p| p.1).sum();
            let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
            let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
            let denom = n * sxx - sx * sx;
            if denom.abs() < 1e-14 {
                return None;
            }
            let a = (n * sxy - sx * sy) / denom;
            let b = (sy - a * sx) / n;
            let ys: Vec<f64> = pts.iter().map(|p| p.1).collect();
            let preds: Vec<f64> = pts.iter().map(|p| a * p.0 + b).collect();
            let r2 = r2_score(&ys, &preds);
            let sign = if b >= 0.0 { "+" } else { "-" };
            let eq = format!("y = {}x {} {}", fmt_coeff(a), sign, fmt_coeff(b.abs()));
            Some(ComputedTrendline {
                points: vec![(draw_x0, a * draw_x0 + b), (draw_x1, a * draw_x1 + b)],
                equation: eq,
                r_squared: r2,
            })
        }
        TrendlineKind::Polynomial(order) => {
            let deg = (order as usize).clamp(2, 4).min(pts.len() - 1);
            let m = deg + 1;
            let mut mat = vec![vec![0.0f64; m]; m];
            let mut rhs = vec![0.0f64; m];
            // 归一化 x 防止高次幂溢出
            let x_mid = 0.5 * (xmin + xmax);
            let x_scale = ((xmax - xmin) * 0.5).max(1e-9);
            for &(x, y) in &pts {
                let u = (x - x_mid) / x_scale;
                let mut pows = vec![1.0f64; 2 * m];
                for k in 1..pows.len() {
                    pows[k] = pows[k - 1] * u;
                }
                for r in 0..m {
                    rhs[r] += pows[r] * y;
                    for c in 0..m {
                        mat[r][c] += pows[r + c];
                    }
                }
            }
            let coeff = solve_linear_system(mat, rhs)?;
            let eval = |x: f64| -> f64 {
                let u = (x - x_mid) / x_scale;
                let mut acc = 0.0;
                let mut p = 1.0;
                for &c in &coeff {
                    acc += c * p;
                    p *= u;
                }
                acc
            };
            let ys: Vec<f64> = pts.iter().map(|p| p.1).collect();
            let preds: Vec<f64> = pts.iter().map(|p| eval(p.0)).collect();
            let r2 = r2_score(&ys, &preds);
            let samples = 48usize;
            let mut out = Vec::with_capacity(samples + 1);
            for i in 0..=samples {
                let x = draw_x0 + (draw_x1 - draw_x0) * (i as f64 / samples as f64);
                out.push((x, eval(x)));
            }
            Some(ComputedTrendline {
                points: out,
                equation: format!("{deg} 阶多项式回归"),
                r_squared: r2,
            })
        }
        TrendlineKind::Exponential => {
            let pos: Vec<(f64, f64)> = pts.iter().copied().filter(|(_, y)| *y > 0.0).collect();
            if pos.len() < 2 {
                return None;
            }
            let n = pos.len() as f64;
            let sx: f64 = pos.iter().map(|p| p.0).sum();
            let sly: f64 = pos.iter().map(|p| p.1.ln()).sum();
            let sxx: f64 = pos.iter().map(|p| p.0 * p.0).sum();
            let sxly: f64 = pos.iter().map(|p| p.0 * p.1.ln()).sum();
            let denom = n * sxx - sx * sx;
            if denom.abs() < 1e-14 {
                return None;
            }
            let b = (n * sxly - sx * sly) / denom;
            let a = ((sly - b * sx) / n).exp();
            let eval = |x: f64| a * (b * x).exp();
            let ys: Vec<f64> = pos.iter().map(|p| p.1).collect();
            let preds: Vec<f64> = pos.iter().map(|p| eval(p.0)).collect();
            let r2 = r2_score(&ys, &preds);
            let samples = 40usize;
            let mut out = Vec::with_capacity(samples + 1);
            for i in 0..=samples {
                let x = draw_x0 + (draw_x1 - draw_x0) * (i as f64 / samples as f64);
                let y = eval(x);
                if y.is_finite() {
                    out.push((x, y));
                }
            }
            Some(ComputedTrendline {
                points: out,
                equation: format!("y = {}e^({}x)", fmt_coeff(a), fmt_coeff(b)),
                r_squared: r2,
            })
        }
        TrendlineKind::Logarithmic => {
            let pos: Vec<(f64, f64)> = pts.iter().copied().filter(|(x, _)| *x > 0.0).collect();
            if pos.len() < 2 {
                return None;
            }
            let n = pos.len() as f64;
            let slx: f64 = pos.iter().map(|p| p.0.ln()).sum();
            let sy: f64 = pos.iter().map(|p| p.1).sum();
            let slx2: f64 = pos.iter().map(|p| p.0.ln().powi(2)).sum();
            let slxy: f64 = pos.iter().map(|p| p.0.ln() * p.1).sum();
            let denom = n * slx2 - slx * slx;
            if denom.abs() < 1e-14 {
                return None;
            }
            let a = (n * slxy - slx * sy) / denom;
            let b = (sy - a * slx) / n;
            let eval = |x: f64| a * x.ln() + b;
            let ys: Vec<f64> = pos.iter().map(|p| p.1).collect();
            let preds: Vec<f64> = pos.iter().map(|p| eval(p.0)).collect();
            let r2 = r2_score(&ys, &preds);
            let x0 = draw_x0.max(1e-6);
            let x1 = draw_x1.max(x0 + 1e-6);
            let samples = 40usize;
            let mut out = Vec::with_capacity(samples + 1);
            for i in 0..=samples {
                let x = x0 + (x1 - x0) * (i as f64 / samples as f64);
                out.push((x, eval(x)));
            }
            let sign = if b >= 0.0 { "+" } else { "-" };
            Some(ComputedTrendline {
                points: out,
                equation: format!("y = {}ln(x) {} {}", fmt_coeff(a), sign, fmt_coeff(b.abs())),
                r_squared: r2,
            })
        }
        TrendlineKind::Power => {
            let pos: Vec<(f64, f64)> = pts
                .iter()
                .copied()
                .filter(|(x, y)| *x > 0.0 && *y > 0.0)
                .collect();
            if pos.len() < 2 {
                return None;
            }
            let n = pos.len() as f64;
            let slx: f64 = pos.iter().map(|p| p.0.ln()).sum();
            let sly: f64 = pos.iter().map(|p| p.1.ln()).sum();
            let slx2: f64 = pos.iter().map(|p| p.0.ln().powi(2)).sum();
            let slxly: f64 = pos.iter().map(|p| p.0.ln() * p.1.ln()).sum();
            let denom = n * slx2 - slx * slx;
            if denom.abs() < 1e-14 {
                return None;
            }
            let b = (n * slxly - slx * sly) / denom;
            let a = ((sly - b * slx) / n).exp();
            let eval = |x: f64| a * x.powf(b);
            let ys: Vec<f64> = pos.iter().map(|p| p.1).collect();
            let preds: Vec<f64> = pos.iter().map(|p| eval(p.0)).collect();
            let r2 = r2_score(&ys, &preds);
            let x0 = draw_x0.max(1e-6);
            let x1 = draw_x1.max(x0 + 1e-6);
            let samples = 40usize;
            let mut out = Vec::with_capacity(samples + 1);
            for i in 0..=samples {
                let x = x0 + (x1 - x0) * (i as f64 / samples as f64);
                let y = eval(x);
                if y.is_finite() {
                    out.push((x, y));
                }
            }
            Some(ComputedTrendline {
                points: out,
                equation: format!("y = {}x^{}", fmt_coeff(a), fmt_coeff(b)),
                r_squared: r2,
            })
        }
    }
}

/// 计算误差线半宽 `delta`（对应每个点）
pub fn compute_error_bar_deltas(vals: &[f64], eb: &ErrorBar) -> Vec<f64> {
    let finite: Vec<f64> = vals.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return vec![0.0; vals.len()];
    }
    let std_dev = || -> f64 {
        if finite.len() < 2 {
            return 0.0;
        }
        let mean = finite.iter().sum::<f64>() / finite.len() as f64;
        let var = finite.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (finite.len() - 1) as f64;
        var.sqrt()
    };
    match eb.kind {
        ErrorBarKind::Fixed(d) => vec![d.abs(); vals.len()],
        ErrorBarKind::Percent(pct) => vals
            .iter()
            .map(|v| if v.is_finite() { v.abs() * pct.abs() / 100.0 } else { 0.0 })
            .collect(),
        ErrorBarKind::StdDev(k) => {
            let sd = std_dev() * k.abs();
            vec![sd; vals.len()]
        }
        ErrorBarKind::StdErr => {
            let se = std_dev() / (finite.len() as f64).sqrt();
            vec![se; vals.len()]
        }
    }
}

// =============================================================================
// 4. 参考线 / 警示带求值 & 统计摘要
// =============================================================================

#[derive(Clone, Debug)]
pub enum ResolvedRefLine {
    Line {
        value: f64,
        label: String,
    },
    Band {
        low: f64,
        high: f64,
        label: String,
    },
}

pub fn resolve_reference_line(rl: &ReferenceLine, values: &[f64]) -> Option<ResolvedRefLine> {
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    match rl.mode {
        RefLineMode::Fixed(v) => Some(ResolvedRefLine::Line {
            value: v,
            label: if rl.label.is_empty() {
                format!("目标 {}", format_chart_number(v, "", DateSystem::D1900))
            } else {
                rl.label.clone()
            },
        }),
        RefLineMode::Band { low, high } => Some(ResolvedRefLine::Band {
            low: low.min(high),
            high: low.max(high),
            label: if rl.label.is_empty() {
                "目标区间".into()
            } else {
                rl.label.clone()
            },
        }),
        RefLineMode::Mean => {
            if finite.is_empty() {
                return None;
            }
            let m = finite.iter().sum::<f64>() / finite.len() as f64;
            Some(ResolvedRefLine::Line {
                value: m,
                label: if rl.label.is_empty() {
                    format!("均值 {}", format_chart_number(m, "", DateSystem::D1900))
                } else {
                    format!("{} {}", rl.label, format_chart_number(m, "", DateSystem::D1900))
                },
            })
        }
        RefLineMode::Median => {
            if finite.is_empty() {
                return None;
            }
            finite.sort_by(|a, b| a.total_cmp(b));
            let mid = finite.len() / 2;
            let med = if finite.len() % 2 == 1 {
                finite[mid]
            } else {
                0.5 * (finite[mid - 1] + finite[mid])
            };
            Some(ResolvedRefLine::Line {
                value: med,
                label: if rl.label.is_empty() {
                    format!("中位数 {}", format_chart_number(med, "", DateSystem::D1900))
                } else {
                    format!("{} {}", rl.label, format_chart_number(med, "", DateSystem::D1900))
                },
            })
        }
        RefLineMode::Min => {
            let v = finite.iter().copied().fold(f64::INFINITY, f64::min);
            v.is_finite().then(|| ResolvedRefLine::Line {
                value: v,
                label: if rl.label.is_empty() {
                    format!("最小 {}", format_chart_number(v, "", DateSystem::D1900))
                } else {
                    rl.label.clone()
                },
            })
        }
        RefLineMode::Max => {
            let v = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            v.is_finite().then(|| ResolvedRefLine::Line {
                value: v,
                label: if rl.label.is_empty() {
                    format!("最大 {}", format_chart_number(v, "", DateSystem::D1900))
                } else {
                    rl.label.clone()
                },
            })
        }
        RefLineMode::Percentile(p) => {
            if finite.is_empty() {
                return None;
            }
            finite.sort_by(|a, b| a.total_cmp(b));
            let v = quantile_sorted(&finite, p.clamp(0.0, 1.0));
            Some(ResolvedRefLine::Line {
                value: v,
                label: if rl.label.is_empty() {
                    format!("P{:.0} {}", p * 100.0, format_chart_number(v, "", DateSystem::D1900))
                } else {
                    rl.label.clone()
                },
            })
        }
        RefLineMode::StdDevBand => {
            if finite.len() < 2 {
                return None;
            }
            let mean = finite.iter().sum::<f64>() / finite.len() as f64;
            let var = finite.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
                / (finite.len() - 1) as f64;
            let sd = var.sqrt();
            Some(ResolvedRefLine::Band {
                low: mean - sd,
                high: mean + sd,
                label: if rl.label.is_empty() {
                    "均值 ±1σ".into()
                } else {
                    rl.label.clone()
                },
            })
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChartInsightSummary {
    pub count: usize,
    pub total: f64,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub growth_pct: Option<f64>,
    pub text: String,
}

pub fn compute_insight_summary(chart: &Chart, sys: DateSystem) -> Option<ChartInsightSummary> {
    let first_vis = chart.series.iter().find(|s| !s.hidden && !s.vals.is_empty())?;
    let finite: Vec<f64> = first_vis
        .vals
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if finite.is_empty() {
        return None;
    }
    let total: f64 = finite.iter().sum();
    let mean = total / finite.len() as f64;
    let min = finite.iter().copied().fold(f64::INFINITY, f64::min);
    let max = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let growth_pct = if finite.len() >= 2 && finite[0].abs() > 1e-12 {
        Some((finite[finite.len() - 1] - finite[0]) / finite[0].abs())
    } else {
        None
    };
    let fmt = chart.effective_y_axis().num_fmt;
    let mut text = format!(
        "总计 {} · 均值 {} · 最小 {} · 最大 {}",
        format_chart_number(total, &fmt, sys),
        format_chart_number(mean, &fmt, sys),
        format_chart_number(min, &fmt, sys),
        format_chart_number(max, &fmt, sys),
    );
    if let Some(g) = growth_pct {
        let sign = if g >= 0.0 { "+" } else { "" };
        text.push_str(&format!(" · 首末 {}{:.1}%", sign, g * 100.0));
    }
    Some(ChartInsightSummary {
        count: finite.len(),
        total,
        mean,
        min,
        max,
        growth_pct,
        text,
    })
}

// =============================================================================
// 5. 财务瀑布图 / 漏斗图 / 矩形树图 / 直方图+帕累托 / 箱线图 算法引擎
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterfallBarKind {
    Total,
    Increase,
    Decrease,
}

#[derive(Clone, Debug)]
pub struct WaterfallBar {
    pub index: usize,
    pub label: String,
    pub delta: f64,
    pub y_start: f64,
    pub y_end: f64,
    pub kind: WaterfallBarKind,
}

/// 财务瀑布图计算：自动识别包含“总计/小计/合计/期初/期末/Total”的分类柱或显式 `total_indices`
pub fn compute_waterfall(series: &Series, explicit_totals: &[usize]) -> Vec<WaterfallBar> {
    let mut out = Vec::with_capacity(series.vals.len());
    let mut running = 0.0f64;
    for (i, &v) in series.vals.iter().enumerate() {
        if !v.is_finite() {
            continue;
        }
        let label = series
            .cats
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("项{}", i + 1));
        let lower = label.to_ascii_lowercase();
        let auto_total = explicit_totals.contains(&i)
            || (i == 0
                && (label.contains("期初")
                    || label.contains("起始")
                    || lower.contains("start")
                    || lower.contains("open")))
            || (i + 1 == series.vals.len()
                && (label.contains("总计")
                    || label.contains("合计")
                    || label.contains("期末")
                    || label.contains("净")
                    || lower.contains("total")
                    || lower.contains("net")))
            || label.contains("小计")
            || lower.contains("subtotal");
        if auto_total {
            // 若用户给了显式总计值则用该值，否则按累计值
            let total_val = if i == 0 || v.abs() > 1e-12 { v } else { running };
            running = total_val;
            out.push(WaterfallBar {
                index: i,
                label,
                delta: total_val,
                y_start: 0.0,
                y_end: total_val,
                kind: WaterfallBarKind::Total,
            });
        } else {
            let start = running;
            running += v;
            out.push(WaterfallBar {
                index: i,
                label,
                delta: v,
                y_start: start,
                y_end: running,
                kind: if v >= 0.0 {
                    WaterfallBarKind::Increase
                } else {
                    WaterfallBarKind::Decrease
                },
            });
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct FunnelStage {
    pub index: usize,
    pub label: String,
    pub value: f64,
    pub width_frac: f64,
    pub step_conv: Option<f64>,  // 较上一阶段转化率
    pub total_conv: Option<f64>, // 较首阶段总转化率
}

pub fn compute_funnel(series: &Series) -> Vec<FunnelStage> {
    let items: Vec<(usize, String, f64)> = series
        .vals
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite() && **v > 0.0)
        .map(|(i, &v)| {
            (
                i,
                series
                    .cats
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("阶段{}", i + 1)),
                v,
            )
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    let max_v = items
        .iter()
        .map(|x| x.2)
        .fold(0.0f64, f64::max)
        .max(1e-12);
    let first_v = items[0].2.max(1e-12);
    let mut out = Vec::with_capacity(items.len());
    for (pos, (idx, label, v)) in items.iter().enumerate() {
        let step_conv = if pos == 0 {
            None
        } else {
            let prev = items[pos - 1].2;
            (prev > 1e-12).then_some(*v / prev)
        };
        let total_conv = Some(*v / first_v);
        out.push(FunnelStage {
            index: *idx,
            label: label.clone(),
            value: *v,
            width_frac: (*v / max_v).clamp(0.08, 1.0),
            step_conv,
            total_conv,
        });
    }
    out
}

#[derive(Clone, Debug)]
pub struct TreemapTile {
    pub index: usize,
    pub label: String,
    pub value: f64,
    pub share: f64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Squarified Treemap 布局算法：将各分类按数值占比切分为长宽比最接近正方形的矩形
pub fn compute_treemap(series: &Series, x0: f32, y0: f32, w0: f32, h0: f32) -> Vec<TreemapTile> {
    if w0 <= 1.0 || h0 <= 1.0 {
        return Vec::new();
    }
    let mut items: Vec<(usize, String, f64)> = series
        .vals
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite() && **v > 0.0)
        .map(|(i, &v)| {
            (
                i,
                series
                    .cats
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("项{}", i + 1)),
                v,
            )
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    items.sort_by(|a, b| b.2.total_cmp(&a.2));
    let total: f64 = items.iter().map(|x| x.2).sum();
    if total <= 0.0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(items.len());
    let mut rx = x0 as f64;
    let mut ry = y0 as f64;
    let mut rw = w0 as f64;
    let mut rh = h0 as f64;
    let mut rem_total = total;
    let mut i = 0usize;

    while i < items.len() && rw > 0.5 && rh > 0.5 && rem_total > 0.0 {
        let short_side = rw.min(rh);
        let area_scale = (rw * rh) / rem_total;
        let mut row: Vec<usize> = vec![i];
        let mut row_area = items[i].2 * area_scale;
        let worst = |row: &[usize], row_a: f64, side: f64| -> f64 {
            if side <= 1e-9 || row_a <= 1e-9 {
                return f64::INFINITY;
            }
            let s2 = side * side;
            let a2 = row_a * row_a;
            row.iter()
                .map(|&idx| {
                    let a = (items[idx].2 * area_scale).max(1e-9);
                    (s2 * a / a2).max(a2 / (s2 * a))
                })
                .fold(0.0f64, f64::max)
        };
        let mut cur_worst = worst(&row, row_area, short_side);
        let mut j = i + 1;
        while j < items.len() {
            let next_area = row_area + items[j].2 * area_scale;
            row.push(j);
            let next_worst = worst(&row, next_area, short_side);
            if next_worst <= cur_worst {
                row_area = next_area;
                cur_worst = next_worst;
                j += 1;
            } else {
                row.pop();
                break;
            }
        }
        let horizontal_strip = rw >= rh;
        let thickness = (row_area / short_side).min(if horizontal_strip { rw } else { rh });
        let mut offset = 0.0f64;
        let row_val_sum: f64 = row.iter().map(|&idx| items[idx].2).sum();
        for &idx in &row {
            let (orig_i, ref lbl, val) = items[idx];
            let frac = if row_val_sum > 0.0 {
                val / row_val_sum
            } else {
                1.0 / row.len() as f64
            };
            let len = short_side * frac;
            let (tx, ty, tw, th) = if horizontal_strip {
                (rx, ry + offset, thickness, len)
            } else {
                (rx + offset, ry, len, thickness)
            };
            offset += len;
            out.push(TreemapTile {
                index: orig_i,
                label: lbl.clone(),
                value: val,
                share: val / total,
                x: tx as f32,
                y: ty as f32,
                w: tw as f32,
                h: th as f32,
            });
        }
        if horizontal_strip {
            rx += thickness;
            rw = (rw - thickness).max(0.0);
        } else {
            ry += thickness;
            rh = (rh - thickness).max(0.0);
        }
        rem_total -= row_val_sum;
        i = j;
    }
    out
}

#[derive(Clone, Debug)]
pub struct HistogramBin {
    pub lo: f64,
    pub hi: f64,
    pub count: usize,
    pub cum_pct: f64,
    pub label: String,
}

/// 直方图自动分箱（Freedman-Diaconis 规则，小样本回退 Sturges）+ 帕累托累积百分比
pub fn compute_histogram(series: &Series, explicit_bins: Option<u32>, pareto: bool) -> Vec<HistogramBin> {
    let mut vals: Vec<f64> = series
        .vals
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if vals.is_empty() {
        return Vec::new();
    }
    vals.sort_by(|a, b| a.total_cmp(b));
    let mn = vals[0];
    let mx = vals[vals.len() - 1];
    let n = vals.len();
    let bins = if let Some(b) = explicit_bins {
        (b as usize).clamp(2, 60)
    } else if (mx - mn).abs() < 1e-12 {
        1
    } else {
        let q1 = quantile_sorted(&vals, 0.25);
        let q3 = quantile_sorted(&vals, 0.75);
        let iqr = (q3 - q1).max(0.0);
        let fd = if iqr > 1e-12 {
            let h = 2.0 * iqr / (n as f64).cbrt();
            ((mx - mn) / h).ceil() as usize
        } else {
            ((n as f64).log2().ceil() as usize + 1).max(3)
        };
        fd.clamp(4, 24)
    };
    let span = (mx - mn).max(1.0);
    let width = span / bins as f64;
    let mut counts = vec![0usize; bins];
    for &v in &vals {
        let mut bi = ((v - mn) / width).floor() as usize;
        if bi >= bins {
            bi = bins - 1;
        }
        counts[bi] += 1;
    }
    let mut out: Vec<HistogramBin> = counts
        .into_iter()
        .enumerate()
        .map(|(i, count)| {
            let lo = mn + i as f64 * width;
            let hi = mn + (i + 1) as f64 * width;
            HistogramBin {
                lo,
                hi,
                count,
                cum_pct: 0.0,
                label: format!(
                    "[{},{}]",
                    format_chart_number(lo, "", DateSystem::D1900),
                    format_chart_number(hi, "", DateSystem::D1900)
                ),
            }
        })
        .collect();
    if pareto {
        out.sort_by(|a, b| b.count.cmp(&a.count));
    }
    let total = n.max(1) as f64;
    let mut acc = 0usize;
    for b in &mut out {
        acc += b.count;
        b.cum_pct = acc as f64 / total;
    }
    out
}

#[derive(Clone, Debug)]
pub struct BoxStats {
    pub label: String,
    pub whisker_lo: f64,
    pub q1: f64,
    pub median: f64,
    pub mean: f64,
    pub q3: f64,
    pub whisker_hi: f64,
    pub outliers: Vec<f64>,
}

pub fn quantile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// 箱线图五数摘要（每个系列绘制一个箱体；单系列且多重复分类时按分类分组）
pub fn compute_box_whisker(chart: &Chart) -> Vec<BoxStats> {
    let vis: Vec<&Series> = chart.series.iter().filter(|s| !s.hidden).collect();
    if vis.is_empty() {
        return Vec::new();
    }
    let build = |label: String, mut vals: Vec<f64>| -> Option<BoxStats> {
        vals.retain(|v| v.is_finite());
        if vals.is_empty() {
            return None;
        }
        vals.sort_by(|a, b| a.total_cmp(b));
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        let q1 = quantile_sorted(&vals, 0.25);
        let median = quantile_sorted(&vals, 0.5);
        let q3 = quantile_sorted(&vals, 0.75);
        let iqr = (q3 - q1).max(0.0);
        let lo_fence = q1 - 1.5 * iqr;
        let hi_fence = q3 + 1.5 * iqr;
        let mut outliers = Vec::new();
        let mut w_lo = q1;
        let mut w_hi = q3;
        let mut first_in = true;
        for &v in &vals {
            if v < lo_fence || v > hi_fence {
                outliers.push(v);
            } else if first_in {
                w_lo = v;
                w_hi = v;
                first_in = false;
            } else {
                w_lo = w_lo.min(v);
                w_hi = w_hi.max(v);
            }
        }
        Some(BoxStats {
            label,
            whisker_lo: w_lo,
            q1,
            median,
            mean,
            q3,
            whisker_hi: w_hi,
            outliers,
        })
    };

    // 若只有 1 个系列且分类名存在重复分组，自动按分类分组生成多箱对比！
    if vis.len() == 1 && !vis[0].cats.is_empty() {
        let mut order: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<f64>> =
            std::collections::HashMap::new();
        for (i, &v) in vis[0].vals.iter().enumerate() {
            let key = vis[0]
                .cats
                .get(i)
                .cloned()
                .unwrap_or_else(|| "全部".into());
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(v);
        }
        if order.len() < vis[0].vals.len() && order.len() >= 2 {
            return order
                .into_iter()
                .filter_map(|k| {
                    let vs = groups.remove(&k).unwrap_or_default();
                    build(k, vs)
                })
                .collect();
        }
    }

    vis.iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let lbl = if s.name.is_empty() {
                format!("系列{}", i + 1)
            } else {
                s.name.clone()
            };
            build(lbl, s.vals.clone())
        })
        .collect()
}

// =============================================================================
// 6. 饼图/圆环外部牵引线标签垂直防撞算法
// =============================================================================

#[derive(Clone, Debug)]
pub struct PieCallout {
    pub slice_idx: usize,
    pub mid_angle: f32,
    pub on_right: bool,
    pub anchor_x: f32,
    pub anchor_y: f32,
    pub elbow_x: f32,
    pub elbow_y: f32,
    pub label_x: f32,
    pub label_y: f32,
    pub text: String,
}

pub fn layout_pie_callouts(
    items: &[(usize, f32, String)], // (slice_idx, mid_angle, label_text)
    cx: f32,
    cy: f32,
    radius: f32,
    min_y: f32,
    max_y: f32,
    line_gap: f32,
) -> Vec<PieCallout> {
    let mut left: Vec<PieCallout> = Vec::new();
    let mut right: Vec<PieCallout> = Vec::new();
    for (idx, ang, text) in items {
        let ca = ang.cos();
        let sa = ang.sin();
        let on_right = ca >= 0.0;
        let ax = cx + radius * ca;
        let ay = cy + radius * sa;
        let ex = cx + (radius + 10.0) * ca;
        let ey = cy + (radius + 10.0) * sa;
        let lx = if on_right {
            cx + radius + 22.0
        } else {
            cx - radius - 22.0
        };
        let c = PieCallout {
            slice_idx: *idx,
            mid_angle: *ang,
            on_right,
            anchor_x: ax,
            anchor_y: ay,
            elbow_x: ex,
            elbow_y: ey,
            label_x: lx,
            label_y: ey.clamp(min_y, max_y),
            text: text.clone(),
        };
        if on_right {
            right.push(c);
        } else {
            left.push(c);
        }
    }
    let resolve_side = |side: &mut Vec<PieCallout>| {
        side.sort_by(|a, b| a.label_y.total_cmp(&b.label_y));
        for i in 1..side.len() {
            let need = side[i - 1].label_y + line_gap;
            if side[i].label_y < need {
                side[i].label_y = need;
            }
        }
        // 若底部溢出则向上反推压缩
        if let Some(last) = side.last()
            && last.label_y > max_y
        {
            let overflow = last.label_y - max_y;
            for item in side.iter_mut() {
                item.label_y -= overflow;
            }
            for i in (0..side.len().saturating_sub(1)).rev() {
                let cap = side[i + 1].label_y - line_gap;
                if side[i].label_y > cap {
                    side[i].label_y = cap;
                }
            }
            for item in side.iter_mut() {
                item.label_y = item.label_y.clamp(min_y, max_y);
            }
        }
        for item in side.iter_mut() {
            item.elbow_y = item.label_y;
        }
    };
    resolve_side(&mut left);
    resolve_side(&mut right);
    right.extend(left);
    right
}

// =============================================================================
// 7. 智能选区剖析、Top-N 图表推荐 & 一键行列互换
// =============================================================================

#[derive(Clone, Debug)]
pub struct ChartRecommendation {
    pub kind: ChartKind,
    pub title: String,
    pub reason: String,
    pub badge: &'static str,
    pub score: u8,
}

#[derive(Clone, Debug)]
pub struct SelectionProfile {
    pub rect: (u32, u32, u32, u32),
    pub rows: u32,
    pub cols: u32,
    pub has_header_row: bool,
    pub has_cat_col: bool,
    pub cat_looks_temporal: bool,
    pub cat_is_numeric: bool,
    pub numeric_series_count: usize,
    pub has_negative: bool,
    pub scale_ratio: f64, // 系列间量级比，> 8 强烈建议双轴组合图
    pub all_positive: bool,
    pub monotonic_decreasing: bool,
    pub has_total_keyword: bool,
}

fn cell_is_num(sh: &Sheet, r: u32, c: u32) -> Option<f64> {
    let cell = sh.get(r, c)?;
    match &cell.value {
        Value::Number(n) if n.is_finite() => Some(*n),
        Value::Text(t) => t.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
        _ => None,
    }
}

fn looks_temporal_label(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    t.contains("月")
        || t.contains("季")
        || t.contains("年")
        || t.starts_with('Q')
        || t.starts_with('q')
        || t.starts_with("202")
        || t.starts_with("201")
        || t.contains('-')
        || t.contains('/')
}

/// 智能分析当前选区数据结构，返回数据画像 + 按适配度排序的 Top-N 图表推荐
pub fn analyze_selection(
    sheet: &Sheet,
    rect: (u32, u32, u32, u32),
    styles: Option<&StyleTable>,
    sys: DateSystem,
) -> (SelectionProfile, Vec<ChartRecommendation>) {
    let (r1, c1, mut r2, mut c2) = rect;
    if (r2.saturating_sub(r1) as u64 + 1) * (c2.saturating_sub(c1) as u64 + 1) > 100_000 {
        if let Some((_, _, ur2, uc2)) = sheet.used_bounds() {
            r2 = r2.min(ur2.max(r1));
            c2 = c2.min(uc2.max(c1));
        }
    }
    let rows = r2.saturating_sub(r1) + 1;
    let cols = c2.saturating_sub(c1) + 1;

    // 判定首行是否表头
    let first_row_nums = (c1..=c2).filter(|&c| cell_is_num(sheet, r1, c).is_some()).count();
    let data_r1 = if rows >= 2 && first_row_nums <= (cols as usize / 2) {
        r1 + 1
    } else {
        r1
    };
    let has_header_row = data_r1 > r1;

    // 判定首列是否分类（文本/日期）或数值 X
    let sample_r2 = r2.min(data_r1 + 200);
    let mut cat_num_cnt = 0usize;
    let mut cat_txt_cnt = 0usize;
    let mut temporal_hits = 0usize;
    let mut has_total_keyword = false;
    for r in data_r1..=sample_r2 {
        if let Some(cell) = sheet.get(r, c1) {
            let txt = match styles {
                Some(st) if !matches!(cell.value, Value::Empty) => {
                    format_value_in(sys, &cell.value, &st.get(sheet.style_of(r, c1)).format)
                }
                _ => cell.value.display(),
            };
            if looks_temporal_label(&txt) {
                temporal_hits += 1;
            }
            if txt.contains("总计")
                || txt.contains("合计")
                || txt.contains("小计")
                || txt.contains("期初")
                || txt.contains("期末")
                || txt.to_ascii_lowercase().contains("total")
            {
                has_total_keyword = true;
            }
            match &cell.value {
                Value::Number(_) => {
                    // 带日期格式的数字视作时间分类而非普通散点 X
                    let is_date_fmt = styles
                        .map(|st| {
                            let f = st.get(sheet.style_of(r, c1)).format.to_ascii_lowercase();
                            f.contains('y') || f.contains('m') || f.contains('d')
                        })
                        .unwrap_or(false);
                    if is_date_fmt {
                        cat_txt_cnt += 1;
                        temporal_hits += 1;
                    } else {
                        cat_num_cnt += 1;
                    }
                }
                Value::Text(s) if !s.trim().is_empty() => cat_txt_cnt += 1,
                _ => {}
            }
        }
    }
    let has_cat_col = cols >= 2 && (cat_txt_cnt > 0 || cat_num_cnt > 0);
    let cat_is_numeric = cols >= 2 && cat_num_cnt >= 3 && cat_txt_cnt == 0;
    let cat_looks_temporal = temporal_hits >= ((sample_r2 - data_r1 + 1) as usize / 2).max(2);

    let val_c1 = if has_cat_col { c1 + 1 } else { c1 };
    let mut series_maxes = Vec::new();
    let mut has_negative = false;
    let mut first_vals = Vec::new();
    for c in val_c1..=c2 {
        let mut col_max = 0.0f64;
        let mut cnt = 0usize;
        for r in data_r1..=sample_r2 {
            if let Some(v) = cell_is_num(sheet, r, c) {
                cnt += 1;
                if v < 0.0 {
                    has_negative = true;
                }
                col_max = col_max.max(v.abs());
                if c == val_c1 {
                    first_vals.push(v);
                }
            }
        }
        if cnt > 0 {
            series_maxes.push(col_max);
        }
    }
    let numeric_series_count = series_maxes.len();
    let scale_ratio = if series_maxes.len() >= 2 {
        let hi = series_maxes.iter().copied().fold(0.0f64, f64::max);
        let lo = series_maxes
            .iter()
            .copied()
            .filter(|v| *v > 1e-9)
            .fold(f64::INFINITY, f64::min);
        if lo.is_finite() && lo > 0.0 {
            hi / lo
        } else {
            1.0
        }
    } else {
        1.0
    };
    let all_positive = !has_negative && !first_vals.is_empty();
    let monotonic_decreasing = all_positive
        && first_vals.len() >= 3
        && first_vals.len() <= 10
        && first_vals.windows(2).all(|w| w[0] >= w[1] && w[1] > 0.0)
        && first_vals[0] > first_vals[first_vals.len() - 1] * 1.3;

    let profile = SelectionProfile {
        rect: (r1, c1, r2, c2),
        rows,
        cols,
        has_header_row,
        has_cat_col,
        cat_looks_temporal,
        cat_is_numeric,
        numeric_series_count,
        has_negative,
        scale_ratio,
        all_positive,
        monotonic_decreasing,
        has_total_keyword,
    };

    let mut recs: Vec<ChartRecommendation> = Vec::new();
    let data_rows = sample_r2.saturating_sub(data_r1) + 1;

    // 单列纯数值 → 直方图 / 箱线图
    if cols == 1 && numeric_series_count == 1 && data_rows >= 6 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Histogram,
            title: "数据分布直方图".into(),
            reason: format!("单列 {data_rows} 条数值样本，自动分箱最适合查看频数分布与帕累托累积"),
            badge: "分布推荐",
            score: 96,
        });
        recs.push(ChartRecommendation {
            kind: ChartKind::BoxWhisker,
            title: "箱线图分析".into(),
            reason: "直观呈现四分位数、中位数、均值及异常离群点".into(),
            badge: "统计异常",
            score: 90,
        });
    }

    // 财务正负增减 / 含总计关键词 → 瀑布图
    if numeric_series_count == 1 && (has_negative || has_total_keyword) && data_rows >= 3 && data_rows <= 24 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Waterfall,
            title: "财务损益瀑布图".into(),
            reason: "检测到正负变动或总计项，瀑布桥图可清晰拆解增减贡献".into(),
            badge: "财务利器",
            score: 95,
        });
    }

    // 双列及以上量级悬殊 → 双轴组合图
    if numeric_series_count >= 2 && scale_ratio >= 6.0 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Combo,
            title: "双轴组合对比图".into(),
            reason: format!(
                "各系列量级相差 {:.1} 倍，自动将小量级系列设为次轴折线避免压扁",
                scale_ratio
            ),
            badge: "智能双轴",
            score: 97,
        });
    }

    // 时间序列 → 折线图 & 面积图
    if cat_looks_temporal && numeric_series_count >= 1 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Line,
            title: "趋势折线图".into(),
            reason: "首列为时间/周期维度，折线图最直观展示走势与拐点".into(),
            badge: "时间趋势",
            score: 94,
        });
        if numeric_series_count >= 2 && all_positive {
            recs.push(ChartRecommendation {
                kind: ChartKind::AreaStacked,
                title: "堆积面积趋势图".into(),
                reason: "同时呈现总体规模演进与各分项结构贡献".into(),
                badge: "结构演进",
                score: 86,
            });
        }
    }

    // 数值 X + 数值 Y → 散点/气泡图
    if cat_is_numeric && numeric_series_count == 1 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Scatter,
            title: "XY 相关性散点图".into(),
            reason: "两列均为连续数值，适合分析相关性与叠加回归趋势线".into(),
            badge: "相关分析",
            score: 95,
        });
    } else if cat_is_numeric && numeric_series_count >= 2 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Bubble,
            title: "三维气泡图".into(),
            reason: "三列数值可同时表达 X/Y 坐标与气泡体量三维关系".into(),
            badge: "三维洞察",
            score: 91,
        });
    }

    // 单调递减全正数 → 漏斗图
    if monotonic_decreasing && numeric_series_count == 1 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Funnel,
            title: "阶段转化漏斗图".into(),
            reason: "数据逐级递减，漏斗图自动计算每步转化率与总留存".into(),
            badge: "转化漏斗",
            score: 93,
        });
    }

    // 单系列正数、少量分类 → 圆环 KPI / 饼图 / 树图
    if numeric_series_count == 1 && all_positive && data_rows >= 2 && data_rows <= 10 {
        recs.push(ChartRecommendation {
            kind: ChartKind::Doughnut,
            title: "占比圆环 KPI 图".into(),
            reason: "全正数占比结构，圆环中心可直出总额 KPI 与牵引线百分比".into(),
            badge: "结构占比",
            score: 89,
        });
        if data_rows >= 4 {
            recs.push(ChartRecommendation {
                kind: ChartKind::Treemap,
                title: "矩形树图".into(),
                reason: "按面积精确对比各分类体量，比饼图更易区分接近占比".into(),
                badge: "体量对比",
                score: 84,
            });
        }
    }

    // 通用对比：簇状柱图 / 堆积柱图 / 条形图
    recs.push(ChartRecommendation {
        kind: if data_rows > 8 {
            ChartKind::Bar
        } else {
            ChartKind::Column
        },
        title: if data_rows > 8 {
            "横向对比条形图".into()
        } else {
            "簇状对比柱状图".into()
        },
        reason: if data_rows > 8 {
            "分类较多，横向条形图标签不拥挤且便于排名阅读".into()
        } else {
            "多维度直观对比各项数值高低，最通用的商业图表".into()
        },
        badge: "经典对比",
        score: 88,
    });
    if numeric_series_count >= 2 && all_positive {
        recs.push(ChartRecommendation {
            kind: ChartKind::ColumnStacked,
            title: "堆积柱状图".into(),
            reason: "清晰展示各期总额与内部构成比例".into(),
            badge: "构成堆积",
            score: 85,
        });
    }

    recs.sort_by(|a, b| b.score.cmp(&a.score));
    recs.dedup_by(|a, b| a.kind == b.kind);
    recs.truncate(6);
    (profile, recs)
}

/// 智能构建图表：在 `chart_from_selection` 基础上自动识别气泡图第三维、单列直方图、
/// 双轴组合图小量级次轴、圆环中心 KPI 与默认智能副标题，同时保留 `source_range` 支持一键行列互换。
pub fn build_smart_chart(
    sheet: &Sheet,
    sheet_name: &str,
    rect: (u32, u32, u32, u32),
    title: &str,
    kind: ChartKind,
    switch_row_col: bool,
    styles: Option<&StyleTable>,
    sys: DateSystem,
) -> Chart {
    let (r1, c1, mut r2, mut c2) = rect;
    if (r2.saturating_sub(r1) as u64 + 1) * (c2.saturating_sub(c1) as u64 + 1) > 100_000 {
        if let Some((_, _, ur2, uc2)) = sheet.used_bounds() {
            r2 = r2.min(ur2.max(r1));
            c2 = c2.min(uc2.max(c1));
        } else {
            r2 = r1;
            c2 = c1;
        }
    }
    c2 = c2.min(c1.saturating_add(255));
    r2 = r2.min(r1.saturating_add(50_000));

    let src_ref = format!(
        "'{}'!${}${}:${}${}",
        sheet_name.replace('\'', "''"),
        col_label(c1),
        r1 + 1,
        col_label(c2),
        r2 + 1
    );
    let fmt_text = |rr: u32, cc: u32| -> String {
        match sheet.get(rr, cc) {
            Some(cell) => match styles {
                Some(st) if !matches!(cell.value, Value::Empty) => {
                    format_value_in(sys, &cell.value, &st.get(sheet.style_of(rr, cc)).format)
                }
                _ => cell.value.display(),
            },
            None => String::new(),
        }
    };
    let q_sheet = sheet_name.replace('\'', "''");
    let mut series = Vec::new();

    if kind == ChartKind::Histogram && c1 == c2 {
        // 单列直方图
        let has_hdr = cell_is_num(sheet, r1, c1).is_none() && r2 > r1;
        let dr1 = if has_hdr { r1 + 1 } else { r1 };
        let name = if has_hdr {
            fmt_text(r1, c1)
        } else {
            "样本分布".into()
        };
        let vals: Vec<f64> = (dr1..=r2)
            .map(|r| cell_is_num(sheet, r, c1).unwrap_or(f64::NAN))
            .collect();
        let col = col_label(c1);
        series.push(Series {
            name,
            vals,
            name_ref: if has_hdr {
                format!("'{q_sheet}'!${col}${}", r1 + 1)
            } else {
                String::new()
            },
            val_ref: format!("'{q_sheet}'!${col}${}:${col}${}", dr1 + 1, r2 + 1),
            ..Series::default()
        });
    } else if kind == ChartKind::Bubble && c2 >= c1 + 2 {
        // 气泡图：第 1 列 X，第 2 列 Y，第 3 列 Size（可重复多组或单组）
        let has_hdr = cell_is_num(sheet, r1, c1 + 1).is_none() && r2 > r1;
        let dr1 = if has_hdr { r1 + 1 } else { r1 };
        let name = if has_hdr {
            fmt_text(r1, c1 + 1)
        } else {
            "气泡系列".into()
        };
        let mut xv = Vec::new();
        let mut vals = Vec::new();
        let mut sizes = Vec::new();
        let mut cats = Vec::new();
        for r in dr1..=r2 {
            if let (Some(x), Some(y)) = (cell_is_num(sheet, r, c1), cell_is_num(sheet, r, c1 + 1)) {
                let sz = cell_is_num(sheet, r, c1 + 2).unwrap_or(10.0).abs().max(0.1);
                xv.push(x);
                vals.push(y);
                sizes.push(sz);
                cats.push(fmt_text(r, c1));
            }
        }
        let cx = col_label(c1);
        let cy = col_label(c1 + 1);
        let cz = col_label(c1 + 2);
        series.push(Series {
            name,
            cats,
            vals,
            xv,
            sizes,
            name_ref: if has_hdr {
                format!("'{q_sheet}'!${cy}${}", r1 + 1)
            } else {
                String::new()
            },
            cat_ref: format!("'{q_sheet}'!${cx}${}:${cx}${}", dr1 + 1, r2 + 1),
            val_ref: format!("'{q_sheet}'!${cy}${}:${cy}${}", dr1 + 1, r2 + 1),
            size_ref: format!("'{q_sheet}'!${cz}${}:${cz}${}", dr1 + 1, r2 + 1),
            ..Series::default()
        });
    } else if switch_row_col && r2 > r1 && c2 > c1 {
        // 按行作为系列：首列为系列名，首行为分类轴
        let cat_ref = format!(
            "'{q_sheet}'!${}${}:${}${}",
            col_label(c1 + 1),
            r1 + 1,
            col_label(c2),
            r1 + 1
        );
        let cats: Vec<String> = ((c1 + 1)..=c2).map(|c| fmt_text(r1, c)).collect();
        for r in (r1 + 1)..=r2 {
            let name = {
                let t = fmt_text(r, c1);
                if t.is_empty() {
                    format!("系列{}", r - r1)
                } else {
                    t
                }
            };
            let mut vals = Vec::new();
            let mut xv = Vec::new();
            for c in (c1 + 1)..=c2 {
                let y = cell_is_num(sheet, r, c);
                if kind == ChartKind::Scatter {
                    if let (Some(x), Some(yv)) = (cell_is_num(sheet, r1, c), y) {
                        xv.push(x);
                        vals.push(yv);
                    }
                } else {
                    vals.push(y.unwrap_or(f64::NAN));
                }
            }
            series.push(Series {
                name,
                cats: cats.clone(),
                vals,
                xv,
                name_ref: format!("'{q_sheet}'!${}${}", col_label(c1), r + 1),
                cat_ref: cat_ref.clone(),
                val_ref: format!(
                    "'{q_sheet}'!${}${}:${}${}",
                    col_label(c1 + 1),
                    r + 1,
                    col_label(c2),
                    r + 1
                ),
                ..Series::default()
            });
        }
    } else {
        // 标准按列作为系列：首行为系列名，首列为分类轴（支持单列仅数值）
        let has_cat = c2 > c1;
        let has_hdr = if has_cat {
            cell_is_num(sheet, r1, c1 + 1).is_none() && r2 > r1
        } else {
            cell_is_num(sheet, r1, c1).is_none() && r2 > r1
        };
        let dr1 = if has_hdr { r1 + 1 } else { r1 };
        let val_c1 = if has_cat { c1 + 1 } else { c1 };
        let cat_col = col_label(c1);
        for c in val_c1..=c2 {
            let name = if has_hdr {
                let t = fmt_text(r1, c);
                if t.is_empty() {
                    format!("系列{}", c - val_c1 + 1)
                } else {
                    t
                }
            } else {
                format!("系列{}", c - val_c1 + 1)
            };
            let mut cats = Vec::new();
            let mut vals = Vec::new();
            let mut xv = Vec::new();
            for r in dr1..=r2 {
                let cat_str = if has_cat {
                    fmt_text(r, c1)
                } else {
                    format!("{}", r - dr1 + 1)
                };
                let y = cell_is_num(sheet, r, c);
                if kind == ChartKind::Scatter {
                    let x = if has_cat {
                        cell_is_num(sheet, r, c1)
                    } else {
                        Some((r - dr1 + 1) as f64)
                    };
                    if let (Some(xv_i), Some(yv_i)) = (x, y) {
                        cats.push(cat_str);
                        xv.push(xv_i);
                        vals.push(yv_i);
                    }
                } else {
                    cats.push(cat_str);
                    vals.push(y.unwrap_or(f64::NAN));
                }
            }
            let vc = col_label(c);
            series.push(Series {
                name,
                cats,
                vals,
                xv,
                name_ref: if has_hdr {
                    format!("'{q_sheet}'!${vc}${}", r1 + 1)
                } else {
                    String::new()
                },
                cat_ref: if has_cat {
                    format!("'{q_sheet}'!${cat_col}${}:${cat_col}${}", dr1 + 1, r2 + 1)
                } else {
                    String::new()
                },
                val_ref: format!("'{q_sheet}'!${vc}${}:${vc}${}", dr1 + 1, r2 + 1),
                ..Series::default()
            });
        }
    }

    // 组合图自动智能拆分：最后一个系列（或量级明显偏小的系列）设为次轴折线
    if kind == ChartKind::Combo && series.len() >= 2 {
        let maxes: Vec<f64> = series
            .iter()
            .map(|s| {
                s.vals
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .map(f64::abs)
                    .fold(0.0f64, f64::max)
            })
            .collect();
        let top = maxes.iter().copied().fold(0.0f64, f64::max);
        let mut any_sec = false;
        for (i, s) in series.iter_mut().enumerate() {
            if top > 0.0 && maxes[i] > 0.0 && top / maxes[i] >= 5.0 {
                s.kind = Some(ChartKind::Line);
                s.secondary = true;
                any_sec = true;
            } else {
                s.kind = Some(ChartKind::Column);
                s.secondary = false;
            }
        }
        if !any_sec {
            if let Some(last) = series.last_mut() {
                last.kind = Some(ChartKind::Line);
                last.secondary = true;
            }
        }
    }

    let mut ch = Chart {
        kind,
        title: title.to_string(),
        series,
        legend: !matches!(
            kind,
            ChartKind::Waterfall | ChartKind::Funnel | ChartKind::Histogram | ChartKind::Treemap
        ),
        anchor: (r1, c2 + 2, r1 + 16, c2 + 10),
        legend_pos: "b".into(),
        source_range: src_ref,
        content_dirty: true,
        ..Chart::default()
    };
    ch.style.switch_row_col = switch_row_col;
    if kind == ChartKind::Doughnut {
        ch.analytics.show_center_kpi = true;
        ch.analytics.center_kpi_title = "总计".into();
        ch.label_opts.show_pct = true;
    } else if kind == ChartKind::Pie {
        ch.label_opts.show_pct = true;
        ch.label_opts.leader_lines = true;
    } else if kind == ChartKind::Waterfall || kind == ChartKind::Funnel {
        ch.show_labels = true;
    }
    ch
}

/// 一键行列互换（Switch Row / Column）：基于 `source_range` 或首个系列推算的矩形重新切片，保留全部标题/主题/分析配置
pub fn switch_row_column(
    chart: &mut Chart,
    sheets: &[Sheet],
    default_sheet_idx: usize,
    styles: Option<&StyleTable>,
    sys: DateSystem,
) -> bool {
    let resolved = if !chart.source_range.is_empty() {
        crate::charts::parse_ref_rect(&chart.source_range)
    } else {
        infer_source_rect(chart)
    };
    let Some((sheet_opt, rect)) = resolved else {
        return false;
    };
    let sh = match sheet_opt.as_deref() {
        Some(n) => sheets
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(n))
            .or_else(|| sheets.get(default_sheet_idx)),
        None => sheets.get(default_sheet_idx),
    };
    let Some(sheet) = sh else {
        return false;
    };
    let next_sw = !chart.style.switch_row_col;
    let rebuilt = build_smart_chart(
        sheet,
        &sheet.name,
        rect,
        &chart.title,
        chart.kind,
        next_sw,
        styles,
        sys,
    );
    if rebuilt.series.is_empty() {
        return false;
    }
    chart.series = rebuilt.series;
    chart.source_range = rebuilt.source_range;
    chart.style.switch_row_col = next_sw;
    chart.content_dirty = true;
    true
}

/// 若旧图表没有 `source_range`，从所有系列的 `name_ref`/`cat_ref`/`val_ref` 并集反推出源矩形
pub fn infer_source_rect(chart: &Chart) -> Option<(Option<String>, (u32, u32, u32, u32))> {
    let mut sheet_name: Option<String> = None;
    let mut r1 = u32::MAX;
    let mut c1 = u32::MAX;
    let mut r2 = 0u32;
    let mut c2 = 0u32;
    for s in &chart.series {
        for r in [&s.name_ref, &s.cat_ref, &s.val_ref, &s.size_ref] {
            if r.is_empty() {
                continue;
            }
            if let Some((sn, (a, b, c, d))) = crate::charts::parse_ref_rect(r) {
                if sheet_name.is_none() {
                    sheet_name = sn;
                }
                r1 = r1.min(a);
                c1 = c1.min(b);
                r2 = r2.max(c);
                c2 = c2.max(d);
            }
        }
    }
    if r1 <= r2 && c1 <= c2 {
        Some((sheet_name, (r1, c1, r2, c2)))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ticks_produces_clean_steps_and_log_scale() {
        let sc = compute_axis_scale(0.0, 97.0, &AxisConfig::default(), true, 6);
        assert_eq!(sc.min, 0.0);
        assert_eq!(sc.max, 100.0);
        assert_eq!(sc.step, 20.0);
        assert_eq!(sc.ticks, vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);

        let log_cfg = AxisConfig {
            log_base: Some(10.0),
            ..Default::default()
        };
        let lsc = compute_axis_scale(1.0, 850.0, &log_cfg, false, 5);
        assert_eq!(lsc.min, 1.0);
        assert_eq!(lsc.max, 1000.0);
        assert!((lsc.norm(10.0) - 1.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn trendlines_fit_linear_poly_and_exponential_accurately() {
        let pts = vec![(1.0, 3.0), (2.0, 5.0), (3.0, 7.0), (4.0, 9.0)];
        let lin = compute_trendline(&pts, &Trendline::default()).unwrap();
        assert!((lin.r_squared.unwrap() - 1.0).abs() < 1e-9);
        assert!(lin.equation.contains("2x + 1"), "{}", lin.equation);

        let quad: Vec<(f64, f64)> = (0..=5).map(|x| (x as f64, (x * x) as f64 + 1.0)).collect();
        let poly = compute_trendline(
            &quad,
            &Trendline {
                kind: TrendlineKind::Polynomial(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert!((poly.r_squared.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn waterfall_funnel_treemap_histogram_box_compute_cleanly() {
        let wf = compute_waterfall(
            &Series {
                cats: vec!["期初".into(), "营收".into(), "成本".into(), "净利总计".into()],
                vals: vec![100.0, 60.0, -40.0, 120.0],
                ..Default::default()
            },
            &[],
        );
        assert_eq!(wf.len(), 4);
        assert_eq!(wf[0].kind, WaterfallBarKind::Total);
        assert_eq!((wf[1].y_start, wf[1].y_end), (100.0, 160.0));
        assert_eq!((wf[2].y_start, wf[2].y_end), (160.0, 120.0));
        assert_eq!(wf[3].kind, WaterfallBarKind::Total);

        let fnl = compute_funnel(&Series {
            cats: vec!["曝光".into(), "点击".into(), "成交".into()],
            vals: vec![1000.0, 250.0, 50.0],
            ..Default::default()
        });
        assert!((fnl[1].step_conv.unwrap() - 0.25).abs() < 1e-6);
        assert!((fnl[2].total_conv.unwrap() - 0.05).abs() < 1e-6);

        let tm = compute_treemap(
            &Series {
                cats: vec!["A".into(), "B".into(), "C".into()],
                vals: vec![50.0, 30.0, 20.0],
                ..Default::default()
            },
            0.0,
            0.0,
            200.0,
            100.0,
        );
        assert_eq!(tm.len(), 3);
        let area: f32 = tm.iter().map(|t| t.w * t.h).sum();
        assert!((area - 20_000.0).abs() < 5.0, "area={area}");

        let hb = compute_histogram(
            &Series {
                vals: vec![1.0, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 10.0],
                ..Default::default()
            },
            Some(5),
            true,
        );
        assert_eq!(hb.len(), 5);
        assert!((hb.last().unwrap().cum_pct - 1.0).abs() < 1e-6);

        let ch = Chart {
            kind: ChartKind::BoxWhisker,
            series: vec![Series {
                name: "组1".into(),
                vals: vec![10.0, 12.0, 13.0, 15.0, 16.0, 18.0, 100.0],
                ..Default::default()
            }],
            ..Default::default()
        };
        let bs = compute_box_whisker(&ch);
        assert_eq!(bs.len(), 1);
        assert_eq!(bs[0].outliers, vec![100.0]);
    }
}
