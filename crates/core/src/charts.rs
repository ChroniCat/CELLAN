//! 图表核心模型、OOXML / chartEx / CELLAN 扩展解析与实时数据源刷新引擎。
//!
//! - 完整支持 22 种现代与财务专业图表（柱/堆积/100%堆积、条形/堆积/100%堆积、折线/堆积、面积/堆积/100%堆积、
//!   饼图、圆环图、散点图、气泡图、雷达图、自由组合图、财务瀑布图、转化漏斗图、矩形树图、直方图/帕累托、箱线图）。
//! - 原生解析趋势线、误差线、主/次坐标轴范围与格式、对数轴、反转轴、平滑线、标记点、空值策略、圆环内径与饼图起始角。
//! - 支持标准 OOXML `<c:extLst>` 扩展持久化（财务参考线/警示带、出版配色主题、副标题、极值标注、中心 KPI 等）。
//! - 数据刷新支持纵向列系列与横向行系列、跨工作表引用、筛选隐藏行过滤（`plot_vis_only`）以及空值 `NaN` 槽位对齐。

use crate::model::Sheet;
use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChartKind {
    #[default]
    Column,
    ColumnStacked,
    ColumnStacked100,
    Bar,
    BarStacked,
    BarStacked100,
    Line,
    LineStacked,
    Area,
    AreaStacked,
    AreaStacked100,
    Pie,
    Doughnut,
    Scatter,
    Bubble,
    Radar,
    /// 自由组合图：各系列可独立设置图型（柱/线/面积/散点）与主/次纵轴
    Combo,
    /// 财务瀑布图 / 损益桥图
    Waterfall,
    /// 转化漏斗图
    Funnel,
    /// 矩形树图（Squarified Treemap）
    Treemap,
    /// 直方图（支持自动分箱 + 帕累托累积百分比线）
    Histogram,
    /// 箱线图（四分位数 + 均值 + 离群点）
    BoxWhisker,
    /// 本应用不建模的外部高级/3D 图种：无损保留原部件并显示占位
    Unknown,
}

impl ChartKind {
    pub fn label(&self) -> &'static str {
        match self {
            ChartKind::Column => "柱状图",
            ChartKind::ColumnStacked => "堆积柱状图",
            ChartKind::ColumnStacked100 => "百分比堆积柱图",
            ChartKind::Bar => "条形图",
            ChartKind::BarStacked => "堆积条形图",
            ChartKind::BarStacked100 => "百分比堆积条图",
            ChartKind::Line => "折线图",
            ChartKind::LineStacked => "堆积折线图",
            ChartKind::Area => "面积图",
            ChartKind::AreaStacked => "堆积面积图",
            ChartKind::AreaStacked100 => "百分比堆积面积",
            ChartKind::Pie => "饼图",
            ChartKind::Doughnut => "圆环图",
            ChartKind::Scatter => "散点图",
            ChartKind::Bubble => "气泡图",
            ChartKind::Radar => "雷达图",
            ChartKind::Combo => "组合图",
            ChartKind::Waterfall => "财务瀑布图",
            ChartKind::Funnel => "转化漏斗图",
            ChartKind::Treemap => "矩形树图",
            ChartKind::Histogram => "直方图",
            ChartKind::BoxWhisker => "箱线图",
            ChartKind::Unknown => "高级/未知图表",
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            ChartKind::Column
            | ChartKind::ColumnStacked
            | ChartKind::ColumnStacked100
            | ChartKind::Bar
            | ChartKind::BarStacked
            | ChartKind::BarStacked100 => "柱形与条形",
            ChartKind::Line
            | ChartKind::LineStacked
            | ChartKind::Area
            | ChartKind::AreaStacked
            | ChartKind::AreaStacked100 => "折线与面积",
            ChartKind::Pie | ChartKind::Doughnut | ChartKind::Treemap | ChartKind::Funnel => {
                "占比与漏斗"
            }
            ChartKind::Scatter
            | ChartKind::Bubble
            | ChartKind::Histogram
            | ChartKind::BoxWhisker
            | ChartKind::Radar => "统计与相关",
            ChartKind::Waterfall | ChartKind::Combo | ChartKind::Unknown => "财务与组合",
        }
    }

    pub const fn all_creatable() -> &'static [ChartKind] {
        &[
            ChartKind::Column,
            ChartKind::ColumnStacked,
            ChartKind::ColumnStacked100,
            ChartKind::Bar,
            ChartKind::BarStacked,
            ChartKind::BarStacked100,
            ChartKind::Line,
            ChartKind::LineStacked,
            ChartKind::Area,
            ChartKind::AreaStacked,
            ChartKind::AreaStacked100,
            ChartKind::Combo,
            ChartKind::Waterfall,
            ChartKind::Pie,
            ChartKind::Doughnut,
            ChartKind::Funnel,
            ChartKind::Treemap,
            ChartKind::Scatter,
            ChartKind::Bubble,
            ChartKind::Histogram,
            ChartKind::BoxWhisker,
            ChartKind::Radar,
        ]
    }

    pub fn is_horizontal(self) -> bool {
        matches!(
            self,
            ChartKind::Bar | ChartKind::BarStacked | ChartKind::BarStacked100
        )
    }

    pub fn is_stacked(self) -> bool {
        matches!(
            self,
            ChartKind::ColumnStacked
                | ChartKind::ColumnStacked100
                | ChartKind::BarStacked
                | ChartKind::BarStacked100
                | ChartKind::LineStacked
                | ChartKind::AreaStacked
                | ChartKind::AreaStacked100
        )
    }

    pub fn is_percent_stacked(self) -> bool {
        matches!(
            self,
            ChartKind::ColumnStacked100 | ChartKind::BarStacked100 | ChartKind::AreaStacked100
        )
    }

    pub fn is_cartesian(self) -> bool {
        matches!(
            self,
            ChartKind::Column
                | ChartKind::ColumnStacked
                | ChartKind::ColumnStacked100
                | ChartKind::Bar
                | ChartKind::BarStacked
                | ChartKind::BarStacked100
                | ChartKind::Line
                | ChartKind::LineStacked
                | ChartKind::Area
                | ChartKind::AreaStacked
                | ChartKind::AreaStacked100
                | ChartKind::Scatter
                | ChartKind::Bubble
                | ChartKind::Combo
                | ChartKind::Waterfall
                | ChartKind::Histogram
                | ChartKind::BoxWhisker
        )
    }

    pub fn ext_code(self) -> &'static str {
        match self {
            ChartKind::Column => "column",
            ChartKind::ColumnStacked => "columnStacked",
            ChartKind::ColumnStacked100 => "columnStacked100",
            ChartKind::Bar => "bar",
            ChartKind::BarStacked => "barStacked",
            ChartKind::BarStacked100 => "barStacked100",
            ChartKind::Line => "line",
            ChartKind::LineStacked => "lineStacked",
            ChartKind::Area => "area",
            ChartKind::AreaStacked => "areaStacked",
            ChartKind::AreaStacked100 => "areaStacked100",
            ChartKind::Pie => "pie",
            ChartKind::Doughnut => "doughnut",
            ChartKind::Scatter => "scatter",
            ChartKind::Bubble => "bubble",
            ChartKind::Radar => "radar",
            ChartKind::Combo => "combo",
            ChartKind::Waterfall => "waterfall",
            ChartKind::Funnel => "funnel",
            ChartKind::Treemap => "treemap",
            ChartKind::Histogram => "histogram",
            ChartKind::BoxWhisker => "boxWhisker",
            ChartKind::Unknown => "unknown",
        }
    }

    pub fn from_ext_code(code: &str) -> Option<ChartKind> {
        Some(match code {
            "column" => ChartKind::Column,
            "columnStacked" => ChartKind::ColumnStacked,
            "columnStacked100" => ChartKind::ColumnStacked100,
            "bar" => ChartKind::Bar,
            "barStacked" => ChartKind::BarStacked,
            "barStacked100" => ChartKind::BarStacked100,
            "line" => ChartKind::Line,
            "lineStacked" => ChartKind::LineStacked,
            "area" => ChartKind::Area,
            "areaStacked" => ChartKind::AreaStacked,
            "areaStacked100" => ChartKind::AreaStacked100,
            "pie" => ChartKind::Pie,
            "doughnut" => ChartKind::Doughnut,
            "scatter" => ChartKind::Scatter,
            "bubble" => ChartKind::Bubble,
            "radar" => ChartKind::Radar,
            "combo" => ChartKind::Combo,
            "waterfall" => ChartKind::Waterfall,
            "funnel" => ChartKind::Funnel,
            "treemap" => ChartKind::Treemap,
            "histogram" => ChartKind::Histogram,
            "boxWhisker" => ChartKind::BoxWhisker,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChartThemeId {
    #[default]
    CellanModern,
    Financial,
    Executive,
    Morandi,
    ColorblindSafe,
    DarkBi,
    Monochrome,
    OfficeClassic,
}

impl ChartThemeId {
    pub const fn all() -> &'static [ChartThemeId] {
        &[
            ChartThemeId::CellanModern,
            ChartThemeId::Financial,
            ChartThemeId::Executive,
            ChartThemeId::Morandi,
            ChartThemeId::ColorblindSafe,
            ChartThemeId::DarkBi,
            ChartThemeId::Monochrome,
            ChartThemeId::OfficeClassic,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            ChartThemeId::CellanModern => "现代商务",
            ChartThemeId::Financial => "金融研报",
            ChartThemeId::Executive => "深蓝高管",
            ChartThemeId::Morandi => "莫兰迪雅灰",
            ChartThemeId::ColorblindSafe => "色盲友好",
            ChartThemeId::DarkBi => "暗夜大屏",
            ChartThemeId::Monochrome => "极简灰度",
            ChartThemeId::OfficeClassic => "经典 Office",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            ChartThemeId::CellanModern => "cellanModern",
            ChartThemeId::Financial => "financial",
            ChartThemeId::Executive => "executive",
            ChartThemeId::Morandi => "morandi",
            ChartThemeId::ColorblindSafe => "colorblindSafe",
            ChartThemeId::DarkBi => "darkBi",
            ChartThemeId::Monochrome => "monochrome",
            ChartThemeId::OfficeClassic => "officeClassic",
        }
    }

    pub fn from_code(s: &str) -> Self {
        match s {
            "financial" => ChartThemeId::Financial,
            "executive" => ChartThemeId::Executive,
            "morandi" => ChartThemeId::Morandi,
            "colorblindSafe" => ChartThemeId::ColorblindSafe,
            "darkBi" => ChartThemeId::DarkBi,
            "monochrome" => ChartThemeId::Monochrome,
            "officeClassic" => ChartThemeId::OfficeClassic,
            _ => ChartThemeId::CellanModern,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MarkerStyle {
    #[default]
    Auto,
    None,
    Circle,
    Square,
    Diamond,
    Triangle,
}

impl MarkerStyle {
    pub fn label(self) -> &'static str {
        match self {
            MarkerStyle::Auto => "自动",
            MarkerStyle::None => "无标记",
            MarkerStyle::Circle => "圆形",
            MarkerStyle::Square => "方形",
            MarkerStyle::Diamond => "菱形",
            MarkerStyle::Triangle => "三角",
        }
    }

    pub fn ooxml_val(self) -> &'static str {
        match self {
            MarkerStyle::Auto => "auto",
            MarkerStyle::None => "none",
            MarkerStyle::Circle => "circle",
            MarkerStyle::Square => "square",
            MarkerStyle::Diamond => "diamond",
            MarkerStyle::Triangle => "triangle",
        }
    }

    pub fn from_ooxml(s: &str) -> Self {
        match s {
            "none" => MarkerStyle::None,
            "circle" | "dot" => MarkerStyle::Circle,
            "square" => MarkerStyle::Square,
            "diamond" => MarkerStyle::Diamond,
            "triangle" => MarkerStyle::Triangle,
            _ => MarkerStyle::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineDash {
    #[default]
    Solid,
    Dash,
    Dot,
    DashDot,
}

impl LineDash {
    pub fn label(self) -> &'static str {
        match self {
            LineDash::Solid => "实线",
            LineDash::Dash => "虚线",
            LineDash::Dot => "点线",
            LineDash::DashDot => "点划线",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            LineDash::Solid => "solid",
            LineDash::Dash => "dash",
            LineDash::Dot => "sysDot",
            LineDash::DashDot => "dashDot",
        }
    }

    pub fn from_code(s: &str) -> Self {
        match s {
            "dash" | "lgDash" | "sysDash" => LineDash::Dash,
            "dot" | "sysDot" => LineDash::Dot,
            "dashDot" | "lgDashDot" | "sysDashDot" => LineDash::DashDot,
            _ => LineDash::Solid,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlankPolicy {
    #[default]
    Gap,
    Zero,
    Span,
}

impl BlankPolicy {
    pub fn label(self) -> &'static str {
        match self {
            BlankPolicy::Gap => "空距断开",
            BlankPolicy::Zero => "按 0 绘制",
            BlankPolicy::Span => "直线连接",
        }
    }

    pub fn ooxml_val(self) -> &'static str {
        match self {
            BlankPolicy::Gap => "gap",
            BlankPolicy::Zero => "zero",
            BlankPolicy::Span => "span",
        }
    }

    pub fn from_ooxml(s: &str) -> Self {
        match s {
            "zero" => BlankPolicy::Zero,
            "span" => BlankPolicy::Span,
            _ => BlankPolicy::Gap,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrendlineKind {
    Linear,
    Polynomial(u8),
    Exponential,
    Logarithmic,
    Power,
    MovingAverage(u8),
}

impl Default for TrendlineKind {
    fn default() -> Self {
        Self::Linear
    }
}

impl TrendlineKind {
    pub fn label(self) -> &'static str {
        match self {
            TrendlineKind::Linear => "线性趋势",
            TrendlineKind::Polynomial(_) => "多项式回归",
            TrendlineKind::Exponential => "指数拟合",
            TrendlineKind::Logarithmic => "对数拟合",
            TrendlineKind::Power => "幂函数拟合",
            TrendlineKind::MovingAverage(_) => "移动平均",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trendline {
    pub kind: TrendlineKind,
    pub name: String,
    pub forward: f64,
    pub backward: f64,
    pub show_equation: bool,
    pub show_r2: bool,
    pub color: Option<[u8; 3]>,
    pub dash: LineDash,
}

impl Default for Trendline {
    fn default() -> Self {
        Self {
            kind: TrendlineKind::Linear,
            name: String::new(),
            forward: 0.0,
            backward: 0.0,
            show_equation: true,
            show_r2: true,
            color: None,
            dash: LineDash::Dash,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ErrorBarKind {
    Fixed(f64),
    Percent(f64),
    StdDev(f64),
    StdErr,
}

impl Default for ErrorBarKind {
    fn default() -> Self {
        Self::StdErr
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ErrorBarDir {
    #[default]
    Both,
    Plus,
    Minus,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErrorBar {
    pub kind: ErrorBarKind,
    pub dir: ErrorBarDir,
    pub color: Option<[u8; 3]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RefLineAxis {
    #[default]
    YPrimary,
    YSecondary,
    X,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RefLineMode {
    Fixed(f64),
    Mean,
    Median,
    Min,
    Max,
    Percentile(f64),
    Band { low: f64, high: f64 },
    StdDevBand,
}

impl Default for RefLineMode {
    fn default() -> Self {
        Self::Mean
    }
}

impl RefLineMode {
    pub fn label(&self) -> &'static str {
        match self {
            RefLineMode::Fixed(_) => "目标常量线",
            RefLineMode::Mean => "均值参考线",
            RefLineMode::Median => "中位数线",
            RefLineMode::Min => "最小值线",
            RefLineMode::Max => "最大值线",
            RefLineMode::Percentile(_) => "分位参考线",
            RefLineMode::Band { .. } => "目标区间带",
            RefLineMode::StdDevBand => "均值 ±1σ 区间",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceLine {
    pub label: String,
    pub axis: RefLineAxis,
    pub mode: RefLineMode,
    pub color: [u8; 3],
    pub dash: LineDash,
}

impl Default for ReferenceLine {
    fn default() -> Self {
        Self {
            label: "均值".into(),
            axis: RefLineAxis::YPrimary,
            mode: RefLineMode::Mean,
            color: [220, 76, 62],
            dash: LineDash::Dash,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AxisConfig {
    pub title: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub major_unit: Option<f64>,
    pub log_base: Option<f64>,
    pub reverse: bool,
    pub num_fmt: String,
    pub show_major_grid: bool,
    pub show_minor_grid: bool,
    pub hidden: bool,
}

impl Default for AxisConfig {
    fn default() -> Self {
        Self {
            title: String::new(),
            min: None,
            max: None,
            major_unit: None,
            log_base: None,
            reverse: false,
            num_fmt: String::new(),
            show_major_grid: true,
            show_minor_grid: false,
            hidden: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnalyticsOpts {
    pub highlight_extrema: bool,
    pub show_growth_badge: bool,
    pub show_summary_bar: bool,
    pub show_pareto_line: bool,
    pub hist_bins: Option<u32>,
    pub waterfall_totals: Vec<usize>,
    pub center_kpi_title: String,
    pub show_center_kpi: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChartStyleOpts {
    pub theme: ChartThemeId,
    pub rounded_bars: bool,
    pub show_card_shadow: bool,
    pub transparent_bg: bool,
    pub gap_width: u16,
    pub hole_size: u8,
    pub first_slice_ang: u16,
    pub pie_explosion: u8,
    pub plot_vis_only: bool,
    pub blank_policy: BlankPolicy,
    pub switch_row_col: bool,
}

impl Default for ChartStyleOpts {
    fn default() -> Self {
        Self {
            theme: ChartThemeId::CellanModern,
            rounded_bars: true,
            show_card_shadow: true,
            transparent_bg: false,
            gap_width: 120,
            hole_size: 55,
            first_slice_ang: 0,
            pie_explosion: 0,
            plot_vis_only: true,
            blank_policy: BlankPolicy::Gap,
            switch_row_col: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    pub name: String,
    pub cats: Vec<String>,
    pub vals: Vec<f64>,
    /// 原始引用（没有缓存值时按它去表里取）
    pub name_ref: String,
    pub cat_ref: String,
    pub val_ref: String,
    /// 系列颜色（None = 用主题调色板）
    pub color: Option<[u8; 3]>,
    /// 散点图/气泡图的 X 值（xVal）；其它图型为空
    pub xv: Vec<f64>,
    /// 该系列自己的图型（组合图里柱/线/面积/散点混排）；None = 跟随图表主类型
    pub kind: Option<ChartKind>,
    /// 是否画在次坐标轴（axId 与主轴不同）
    pub secondary: bool,
    /// 气泡图第三维（bubbleSize）引用与缓存值
    pub size_ref: String,
    pub sizes: Vec<f64>,
    /// 平滑贝塞尔曲线（折线/面积/散点）
    pub smooth: bool,
    /// 阶梯折线（利率/库存阶跃变化）
    pub stepped: bool,
    /// 数据点标记样式
    pub marker: MarkerStyle,
    /// 线宽（pt；0.0 表示自动默认 2.0）
    pub line_width: f32,
    /// 线型（实线/虚线/点线/点划线）
    pub dash: LineDash,
    /// 交互式图例隐藏态（不删数据）
    pub hidden: bool,
    /// 叠加趋势线（线性/多项式/指数/对数/幂/移动平均）
    pub trendlines: Vec<Trendline>,
    /// 误差线
    pub error_bars: Option<ErrorBar>,
}

/// 数据标签的逐项选项（Excel「设置数据标签格式」里的那组勾选）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LabelOpts {
    /// 类别名称
    pub show_cat: bool,
    /// 系列名称
    pub show_ser: bool,
    /// 百分比（饼图/圆环/100%堆积/漏斗）
    pub show_pct: bool,
    /// 图例项标识
    pub show_legend_key: bool,
    /// 引导线（饼图）
    pub leader_lines: bool,
    /// 标签位置：ctr/inBase/inEnd/outEnd/bestFit/l/r/t/b，空=默认
    pub position: String,
    /// 数据标签自定义数字格式（空=沿用轴格式/自动）
    pub num_fmt: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub kind: ChartKind,
    pub title: String,
    pub series: Vec<Series>,
    pub legend: bool,
    /// 锚点（单元格坐标，含右下角 `(r1, c1, r2, c2)`）
    pub anchor: (u32, u32, u32, u32),
    /// 图例位置："r" 右侧 / "t" 顶部 / "b" 底部 / "l" 左侧；空表示无
    pub legend_pos: String,
    /// 图表部件路径（xl/charts/chartN.xml），空表示尚未落盘的新建图表
    pub part: String,
    /// 所属 drawing 部件路径
    pub drawing: String,
    /// 显示数据标签（数值）
    pub show_labels: bool,
    /// 数据标签的其它选项
    pub label_opts: LabelOpts,
    /// 雷达样式：standard / marker / filled（其它图型为空）
    pub radar_style: String,
    /// 引用被改写后要回写到 chart 部件里的 (旧, 新) 对
    pub ref_fixes: Vec<(String, String)>,
    /// X 轴标题（与 x_axis.title 兼容同步）
    pub axis_x: String,
    /// Y 轴标题（与 y_axis.title 兼容同步）
    pub axis_y: String,
    /// 数值轴数字格式（valAx 的 numFmt formatCode），空 = 常规
    pub y_fmt: String,
    /// Y 轴范围（None = 自动）
    pub y_min: Option<f64>,
    pub y_max: Option<f64>,
    /// 副标题（出版级图表说明/单位）
    pub subtitle: String,
    /// 原始整块源数据区域（如 `'Sheet1'!$A$1:$D$10`，用于行列互换与快速重选数据源）
    pub source_range: String,
    /// 视觉主题与高级样式配置
    pub style: ChartStyleOpts,
    /// 完整 X / 主 Y / 次 Y 坐标轴配置
    pub x_axis: AxisConfig,
    pub y_axis: AxisConfig,
    pub y2_axis: AxisConfig,
    /// 参考线 / 目标区间警示带
    pub ref_lines: Vec<ReferenceLine>,
    /// 统计分析与财务增强选项
    pub analytics: AnalyticsOpts,
    /// 标记图表配置内容已在内存中修改，保存时需要重写 `chartN.xml`（未改时原字节保留）
    pub content_dirty: bool,
    /// 来自外部 `chartExN.xml`：普通保存原样透传不降级
    pub is_chartex: bool,
}

impl Default for Chart {
    fn default() -> Self {
        Self {
            kind: ChartKind::Column,
            title: String::new(),
            series: Vec::new(),
            legend: true,
            anchor: (0, 0, 14, 8),
            legend_pos: "r".into(),
            part: String::new(),
            drawing: String::new(),
            show_labels: false,
            label_opts: LabelOpts::default(),
            radar_style: String::new(),
            ref_fixes: Vec::new(),
            axis_x: String::new(),
            axis_y: String::new(),
            y_fmt: String::new(),
            y_min: None,
            y_max: None,
            subtitle: String::new(),
            source_range: String::new(),
            style: ChartStyleOpts::default(),
            x_axis: AxisConfig {
                show_major_grid: false,
                ..AxisConfig::default()
            },
            y_axis: AxisConfig::default(),
            y2_axis: AxisConfig {
                show_major_grid: false,
                ..AxisConfig::default()
            },
            ref_lines: Vec::new(),
            analytics: AnalyticsOpts::default(),
            content_dirty: false,
            is_chartex: false,
        }
    }
}

impl Chart {
    /// 统一获取生效的主 Y 轴配置（兼容旧字段 `axis_y`/`y_fmt`/`y_min`/`y_max`）
    pub fn effective_y_axis(&self) -> AxisConfig {
        let mut ax = self.y_axis.clone();
        if ax.title.is_empty() && !self.axis_y.is_empty() {
            ax.title = self.axis_y.clone();
        }
        if ax.num_fmt.is_empty() && !self.y_fmt.is_empty() {
            ax.num_fmt = self.y_fmt.clone();
        }
        if ax.min.is_none() {
            ax.min = self.y_min;
        }
        if ax.max.is_none() {
            ax.max = self.y_max;
        }
        ax
    }

    /// 统一获取生效的 X 轴配置（兼容旧字段 `axis_x`）
    pub fn effective_x_axis(&self) -> AxisConfig {
        let mut ax = self.x_axis.clone();
        if ax.title.is_empty() && !self.axis_x.is_empty() {
            ax.title = self.axis_x.clone();
        }
        ax
    }

    /// 同步简化兼容字段与 `x_axis`/`y_axis`
    pub fn sync_axis_compat(&mut self) {
        self.axis_x = self.x_axis.title.clone();
        self.axis_y = self.y_axis.title.clone();
        self.y_fmt = self.y_axis.num_fmt.clone();
        self.y_min = self.y_axis.min;
        self.y_max = self.y_axis.max;
    }

    /// 是否有任何可见系列挂在次坐标轴上
    pub fn has_secondary_axis(&self) -> bool {
        self.series.iter().any(|s| !s.hidden && s.secondary)
    }

    /// 插删行列后改写系列引用与 source_range
    pub fn rewrite_refs(&mut self, shift: &crate::refs::Shift) -> bool {
        self.rewrite_mapped_refs(&crate::refs::MappedShift {
            shift,
            insertions: &[],
        })
    }

    pub(crate) fn rewrite_mapped_refs(&mut self, shift: &crate::refs::MappedShift) -> bool {
        let mut changed = false;
        for s in self.series.iter_mut() {
            for r in [&mut s.name_ref, &mut s.cat_ref, &mut s.val_ref, &mut s.size_ref] {
                if r.is_empty() {
                    continue;
                }
                let nf = crate::refs::rewrite_mapped(r, shift);
                if nf != *r {
                    self.ref_fixes.push((r.clone(), nf.clone()));
                    *r = nf;
                    changed = true;
                }
            }
        }
        if !self.source_range.is_empty() {
            let nf = crate::refs::rewrite_mapped(&self.source_range, shift);
            if nf != self.source_range {
                self.source_range = nf;
                changed = true;
            }
        }
        changed
    }
}

/// 解析某张表的图表
pub fn load_charts(path: &Path, sheet_name: &str) -> Result<Vec<Chart>> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(sheet_path) = targets.get(sheet_name) else {
        return Ok(Vec::new());
    };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    load_charts_for_part(&mut zip, sheet_path)
}

fn load_charts_for_part(
    zip: &mut zip::ZipArchive<std::fs::File>,
    sheet_path: &str,
) -> Result<Vec<Chart>> {
    let dbg = std::env::var("SC_CHART_DBG").is_ok();
    let rels_path = match sheet_path.rsplit_once('/') {
        Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
        None => return Ok(Vec::new()),
    };
    if zip.index_for_name(&rels_path).is_none() {
        return Ok(Vec::new());
    }
    let rels_xml = crate::xlsx::read_zip_text(zip, &rels_path)?;
    let Some(drawing_target) = find_rel_target(&rels_xml, "/drawing") else {
        if dbg {
            eprintln!("[chart] sheet rels 里没有 drawing 关系");
        }
        return Ok(Vec::new());
    };
    let sheet_dir = match sheet_path.rsplit_once('/') {
        Some((d, _)) => format!("{d}/"),
        None => "xl/".into(),
    };
    let drawing_path = normalize(&sheet_dir, &drawing_target);
    let drawing_xml = crate::xlsx::read_zip_text(zip, &drawing_path)?;

    let d_rels = match drawing_path.rsplit_once('/') {
        Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
        None => return Ok(Vec::new()),
    };
    let d_rels_ok = zip.index_for_name(&d_rels).is_some();
    let d_rels_xml = if d_rels_ok {
        crate::xlsx::read_zip_text(zip, &d_rels)?
    } else {
        String::new()
    };
    let mut chart_targets: HashMap<String, (String, bool)> = HashMap::new();
    {
        let mut r = Reader::from_str(&d_rels_xml);
        loop {
            match r.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e))
                    if e.name().as_ref().ends_with("Relationship") =>
                {
                    let mut id = String::new();
                    let mut target = String::new();
                    let mut ty = String::new();
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        if k == "Id" {
                            id = v;
                        } else if k == "Target" {
                            target = v;
                        } else if k == "Type" {
                            ty = v;
                        }
                    }
                    let tl = ty.to_ascii_lowercase();
                    if (tl.ends_with("/chart") || tl.ends_with("/chartex")) && !id.is_empty() {
                        let ddir = match drawing_path.rsplit_once('/') {
                            Some((d, _)) => format!("{d}/"),
                            None => "xl/drawings/".into(),
                        };
                        let is_ex = tl.ends_with("/chartex");
                        chart_targets.insert(id, (normalize(&ddir, &target), is_ex));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }

    if dbg {
        eprintln!("[chart] drawing={drawing_path} 关系={:?}", chart_targets);
        eprintln!("[chart] 锚点={:?}", parse_drawing_anchors(&drawing_xml));
    }
    let anchors = parse_drawing_anchors(&drawing_xml);
    if !anchors.is_empty() && !d_rels_ok {
        anyhow::bail!("drawing 部件 {drawing_path} 有图表引用，但缺少关系部件 {d_rels}");
    }
    let mut out = Vec::new();
    for (anchor, rid) in anchors {
        let (cp, is_ex) = chart_targets
            .get(&rid)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("drawing 里的图表引用 {rid} 在关系部件中不存在"))?;
        let cx = crate::xlsx::read_zip_text(zip, &cp)?;
        // 严格良构校验：截断/半截 XML 直接报错
        let mut r = Reader::from_str(&cx);
        r.config_mut().check_end_names = true;
        let mut depth: i32 = 0;
        loop {
            match r.read_event() {
                Ok(Event::Start(_)) => depth += 1,
                Ok(Event::End(_)) => depth -= 1,
                Ok(Event::Eof) => {
                    if depth != 0 {
                        anyhow::bail!("图表部件 {cp} 的 XML 在元素内被截断");
                    }
                    break;
                }
                Err(e) => anyhow::bail!("图表部件 {cp} 的 XML 不完整：{e}"),
                _ => {}
            }
        }
        let mut ch = if is_ex {
            parse_chartex(&cx)
        } else {
            parse_chart(&cx)
        }
        .unwrap_or_else(|| Chart {
            kind: ChartKind::Unknown,
            title: if is_ex {
                chartex_title(&cx)
            } else {
                String::new()
            },
            legend: false,
            legend_pos: String::new(),
            anchor,
            is_chartex: is_ex,
            ..Chart::default()
        });
        ch.anchor = anchor;
        ch.part = cp;
        ch.drawing = drawing_path.clone();
        ch.is_chartex = is_ex;
        ch.content_dirty = false;
        out.push(ch);
    }
    Ok(out)
}

/// 一次把每张表的图表都读出来（key = 表序号，按 workbook.xml 顺序）
pub fn load_charts_all(
    path: &Path,
    sheets: &[Sheet],
) -> Result<std::collections::HashMap<u32, crate::model::SharedList<Chart>>> {
    load_charts_all_in(path, sheets, None, crate::model::DateSystem::D1900)
}

/// 图表分类轴要按本簿的样式与日期体系解析引用区域
pub fn load_charts_all_in(
    path: &Path,
    sheets: &[Sheet],
    styles: Option<&crate::style::StyleTable>,
    sys: crate::model::DateSystem,
) -> Result<std::collections::HashMap<u32, crate::model::SharedList<Chart>>> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut out = std::collections::HashMap::new();
    for (idx, sheet) in sheets.iter().enumerate() {
        let name = &sheet.name;
        let Some(part) = targets.get(name) else {
            continue;
        };
        let mut list = load_charts_for_part(&mut zip, part)
            .map_err(|e| anyhow::anyhow!("读取工作表 {name} 的图表失败：{e:#}"))?;
        for chart in &mut list {
            resolve_series_in(chart, sheet, styles, sys);
        }
        if !list.is_empty() {
            out.insert(idx as u32, list.into());
        }
    }
    Ok(out)
}

fn normalize(base: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let joined = format!("{base}{target}");
    let mut parts: Vec<&str> = Vec::new();
    for p in joined.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn find_rel_target(xml: &str, type_suffix: &str) -> Option<String> {
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if e.name().as_ref().ends_with("Relationship") =>
            {
                let mut ty = String::new();
                let mut target = String::new();
                for a in e.attributes().flatten() {
                    let k = a.key.as_ref();
                    let v = crate::model::xml_unescape(&a.value.to_string());
                    if k == "Type" {
                        ty = v;
                    } else if k == "Target" {
                        target = v;
                    }
                }
                if ty.ends_with(type_suffix) && !target.is_empty() {
                    return Some(target);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

/// twoCellAnchor / oneCellAnchor：取 from 的 col/row、to 的 col/row 与内部 chart 的 r:id
pub(crate) fn parse_drawing_anchors(xml: &str) -> Vec<((u32, u32, u32, u32), String)> {
    let mut out = Vec::new();
    let mut rest = xml;
    loop {
        let mut start = None;
        for tag in ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"] {
            for pat in [format!("<xdr:{tag}"), format!("<{tag}")] {
                if let Some(i) = rest.find(&pat) {
                    if start.map(|s: usize| i < s).unwrap_or(true) {
                        start = Some(i);
                    }
                }
            }
        }
        let Some(b) = start else { break };
        let chunk_rest = &rest[b..];
        let end_pos = ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"]
            .iter()
            .filter_map(|t| {
                chunk_rest
                    .find(&format!("</xdr:{t}>"))
                    .or_else(|| chunk_rest.find(&format!("</{t}>")))
            })
            .min()
            .unwrap_or(chunk_rest.len());
        let chunk = &chunk_rest[..end_pos];

        let (fc, fr) = extract_cell(chunk, "from").unwrap_or((0, 0));
        let (tc, tr) = match extract_cell(chunk, "to") {
            Some(v) => v,
            None => {
                let cx = extract_ext(chunk, "cx").unwrap_or(4_000_000) as f32 / 9525.0;
                let cy = extract_ext(chunk, "cy").unwrap_or(2_000_000) as f32 / 9525.0;
                let dcol = (cx / 104.0).round().max(2.0) as u32;
                let drow = (cy / 22.0).round().max(4.0) as u32;
                (fc + dcol, fr + drow)
            }
        };
        if let Some(rid) = extract_attr(chunk, "r:id") {
            // 只认图表锚点，避免把图片/文本框的 r:id 当成图表
            if chunk.contains("drawingml/2006/chart")
                || chunk.contains("chartex")
                || chunk.contains(":chart")
                || chunk.contains("<chart ")
            {
                out.push(((fr, fc, tr.max(fr + 2), tc.max(fc + 3)), rid));
            }
        }
        rest = &chunk_rest[end_pos.max(1)..];
        if rest.is_empty() {
            break;
        }
    }
    out
}

fn extract_cell(chunk: &str, tag: &str) -> Option<(u32, u32)> {
    let open = ["<xdr:", "<"].iter().find_map(|p| {
        let pat = format!("{p}{tag}>");
        chunk.find(&pat).map(|i| (i, pat.clone()))
    })?;
    let start = open.0 + open.1.len();
    let close = ["</xdr:", "</"]
        .iter()
        .find_map(|p| chunk[start..].find(&format!("{p}{tag}>")).map(|i| i + start))?;
    let body = &chunk[start..close];
    let col = tag_value(body, "col")?;
    let row = tag_value(body, "row")?;
    Some((col, row))
}

fn tag_value(body: &str, tag: &str) -> Option<u32> {
    let pat = ["<xdr:", "<"]
        .iter()
        .find_map(|p| body.find(&format!("{p}{tag}>")).map(|i| (i, p.to_string())))?;
    let start = pat.0 + pat.1.len() + tag.len() + 1;
    let end = body[start..].find('<')? + start;
    body[start..end].trim().parse().ok()
}

fn extract_ext(chunk: &str, key: &str) -> Option<i64> {
    let pat = format!("{key}=\"");
    let i = chunk.find(&pat)?;
    let start = i + pat.len();
    let end = chunk[start..].find('"')? + start;
    chunk[start..end].parse().ok()
}

fn extract_attr(chunk: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = chunk.find(&pat)?;
    let start = i + pat.len();
    let end = chunk[start..].find('"')? + start;
    Some(chunk[start..end].to_string())
}

fn parse_rgb_hex(hex: &str) -> Option<[u8; 3]> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ])
}

fn local(name: &str) -> &str {
    match name.rsplit_once(':') {
        Some((_, l)) => l,
        None => name,
    }
}

/// 补读系列颜色/平滑/标记/线型/趋势线/误差线、双轴完整属性，以及 CELLAN `<c:extLst>` 扩展配置
fn post_fill(ch: &mut Chart, xml: &str) {
    // 1. 逐个 <c:ser> 块补读样式、趋势线、误差线
    let mut rest = xml;
    let mut idx = 0usize;
    while let Some(pos) = rest.find("<c:ser>").or_else(|| rest.find("<ser>")) {
        let chunk_rest = &rest[pos..];
        let end = chunk_rest
            .find("</c:ser>")
            .or_else(|| chunk_rest.find("</ser>"))
            .unwrap_or(chunk_rest.len());
        let chunk = &chunk_rest[..end];
        if let Some(s) = ch.series.get_mut(idx) {
            if let Some(cpos) = chunk.find("srgbClr val=\"") {
                let v = &chunk[cpos + 13..];
                if let Some(q) = v.find('"') {
                    if let Some(rgb) = parse_rgb_hex(&v[..q]) {
                        s.color = Some(rgb);
                    }
                }
            }
            if chunk.contains("smooth val=\"1\"") || chunk.contains("smooth val=\"true\"") {
                s.smooth = true;
            }
            if let Some(mp) = chunk.find("<c:marker>") {
                let mseg = &chunk[mp..chunk[mp..].find("</c:marker>").map(|e| mp + e).unwrap_or(chunk.len())];
                if let Some(sym) = extract_attr(mseg, "symbol val").or_else(|| {
                    mseg.find("symbol val=\"").and_then(|p| {
                        let v = &mseg[p + 12..];
                        v.find('"').map(|q| v[..q].to_string())
                    })
                }) {
                    s.marker = MarkerStyle::from_ooxml(&sym);
                }
            }
            if let Some(lp) = chunk.find("<a:ln") {
                let lseg = &chunk[lp..chunk[lp..].find("</a:ln>").map(|e| lp + e).unwrap_or(chunk.len())];
                if let Some(w_str) = extract_attr(lseg, "w") {
                    if let Ok(emu) = w_str.parse::<f64>() {
                        if emu > 0.0 {
                            s.line_width = (emu / 12700.0) as f32;
                        }
                    }
                }
                if let Some(dp) = lseg.find("prstDash val=\"") {
                    let v = &lseg[dp + 14..];
                    if let Some(q) = v.find('"') {
                        s.dash = LineDash::from_code(&v[..q]);
                    }
                }
            }
            // 趋势线 <c:trendline>...</c:trendline>
            let mut tr_rest = chunk;
            while let Some(tp) = tr_rest.find("<c:trendline>") {
                let tslice = &tr_rest[tp..];
                let tend = tslice.find("</c:trendline>").unwrap_or(tslice.len());
                let tbody = &tslice[..tend];
                let mut tl = Trendline::default();
                if let Some(ty) = tbody.find("trendlineType val=\"").and_then(|p| {
                    let v = &tbody[p + 19..];
                    v.find('"').map(|q| &v[..q])
                }) {
                    let order = tbody
                        .find("order val=\"")
                        .and_then(|p| {
                            let v = &tbody[p + 11..];
                            v.find('"').and_then(|q| v[..q].parse::<u8>().ok())
                        })
                        .unwrap_or(2);
                    let period = tbody
                        .find("period val=\"")
                        .and_then(|p| {
                            let v = &tbody[p + 12..];
                            v.find('"').and_then(|q| v[..q].parse::<u8>().ok())
                        })
                        .unwrap_or(3);
                    tl.kind = match ty {
                        "poly" => TrendlineKind::Polynomial(order.clamp(2, 4)),
                        "exp" => TrendlineKind::Exponential,
                        "log" => TrendlineKind::Logarithmic,
                        "power" => TrendlineKind::Power,
                        "movingAvg" => TrendlineKind::MovingAverage(period.clamp(2, 30)),
                        _ => TrendlineKind::Linear,
                    };
                }
                if let Some(f) = tbody.find("forward val=\"").and_then(|p| {
                    let v = &tbody[p + 13..];
                    v.find('"').and_then(|q| v[..q].parse::<f64>().ok())
                }) {
                    tl.forward = f;
                }
                if let Some(b) = tbody.find("backward val=\"").and_then(|p| {
                    let v = &tbody[p + 14..];
                    v.find('"').and_then(|q| v[..q].parse::<f64>().ok())
                }) {
                    tl.backward = b;
                }
                tl.show_equation = tbody.contains("dispEq val=\"1\"");
                tl.show_r2 = tbody.contains("dispRSqr val=\"1\"");
                if let Some(cpos) = tbody.find("srgbClr val=\"") {
                    let v = &tbody[cpos + 13..];
                    if let Some(q) = v.find('"') {
                        tl.color = parse_rgb_hex(&v[..q]);
                    }
                }
                if let Some(dp) = tbody.find("prstDash val=\"") {
                    let v = &tbody[dp + 14..];
                    if let Some(q) = v.find('"') {
                        tl.dash = LineDash::from_code(&v[..q]);
                    }
                }
                s.trendlines.push(tl);
                tr_rest = &tslice[tend.max(1)..];
            }
            // 误差线 <c:errBars>...</c:errBars>
            if let Some(ep) = chunk.find("<c:errBars>") {
                let eslice = &chunk[ep..];
                let eend = eslice.find("</c:errBars>").unwrap_or(eslice.len());
                let ebody = &eslice[..eend];
                let dir = if ebody.contains("errBarType val=\"plus\"") {
                    ErrorBarDir::Plus
                } else if ebody.contains("errBarType val=\"minus\"") {
                    ErrorBarDir::Minus
                } else {
                    ErrorBarDir::Both
                };
                let ev = ebody
                    .find("val val=\"")
                    .and_then(|p| {
                        let v = &ebody[p + 9..];
                        v.find('"').and_then(|q| v[..q].parse::<f64>().ok())
                    })
                    .unwrap_or(1.0);
                let kind = if ebody.contains("errValType val=\"fixedVal\"") {
                    ErrorBarKind::Fixed(ev)
                } else if ebody.contains("errValType val=\"percentage\"") {
                    ErrorBarKind::Percent(ev)
                } else if ebody.contains("errValType val=\"stdDev\"") {
                    ErrorBarKind::StdDev(ev)
                } else {
                    ErrorBarKind::StdErr
                };
                s.error_bars = Some(ErrorBar {
                    kind,
                    dir,
                    color: None,
                });
            }
        }
        idx += 1;
        rest = &chunk_rest[end.max(1).min(chunk_rest.len())..];
    }

    // 2. 解析所有坐标轴（catAx / valAx），区分 X、主 Y、次 Y
    let parse_axis_block = |seg: &str| -> (String, AxisConfig) {
        let pos = seg
            .find("axPos val=\"")
            .and_then(|p| {
                let v = &seg[p + 11..];
                v.find('"').map(|q| v[..q].to_string())
            })
            .unwrap_or_default();
        let mut ax = AxisConfig::default();
        ax.show_major_grid = seg.contains("majorGridlines");
        ax.show_minor_grid = seg.contains("minorGridlines");
        ax.hidden = seg.contains("delete val=\"1\"");
        ax.reverse = seg.contains("orientation val=\"maxMin\"");
        if let Some(t) = seg.find("<a:t>") {
            let v = &seg[t + 5..];
            if let Some(q) = v.find("</a:t>") {
                ax.title = crate::model::xml_unescape(&v[..q]);
            }
        }
        if let Some(np) = seg.find("formatCode=\"") {
            let v = &seg[np + 12..];
            if let Some(q) = v.find('"') {
                ax.num_fmt = crate::model::xml_unescape(&v[..q]);
            }
        }
        let num_tag = |tag: &str| -> Option<f64> {
            let pat = format!("<c:{tag} val=\"");
            let i = seg.find(&pat)? + pat.len();
            let v = &seg[i..];
            let q = v.find('"')?;
            v[..q].parse().ok()
        };
        ax.max = num_tag("max");
        ax.min = num_tag("min").or_else(|| {
            // 兼容老版本误写进 <c:max val="..." min="..."/> 的属性
            let pat = " min=\"";
            let i = seg.find(pat)? + pat.len();
            let v = &seg[i..];
            let q = v.find('"')?;
            v[..q].parse().ok()
        });
        ax.major_unit = num_tag("majorUnit");
        ax.log_base = num_tag("logBase").filter(|b| *b > 1.0);
        (pos, ax)
    };

    let mut val_axes = Vec::new();
    let mut scan = xml;
    while let Some(p) = scan.find("<c:valAx") {
        let s = &scan[p..];
        let e = s.find("</c:valAx>").unwrap_or(s.len());
        val_axes.push(parse_axis_block(&s[..e]));
        scan = &s[e.max(1).min(s.len())..];
    }
    let mut cat_axes = Vec::new();
    let mut cscan = xml;
    while let Some(p) = cscan.find("<c:catAx") {
        let s = &cscan[p..];
        let e = s.find("</c:catAx>").unwrap_or(s.len());
        cat_axes.push(parse_axis_block(&s[..e]));
        cscan = &s[e.max(1).min(cscan.len())..];
    }

    if let Some((_, xax)) = cat_axes.iter().find(|(pos, ax)| !ax.hidden && pos == "b").or_else(|| cat_axes.first()) {
        ch.x_axis = xax.clone();
        ch.axis_x = xax.title.clone();
    } else if matches!(ch.kind, ChartKind::Scatter | ChartKind::Bubble) {
        if let Some((_, xax)) = val_axes.iter().find(|(pos, _)| pos == "b") {
            ch.x_axis = xax.clone();
            ch.axis_x = xax.title.clone();
        }
    }
    let primary_y = val_axes
        .iter()
        .find(|(pos, ax)| !ax.hidden && pos == "l")
        .or_else(|| {
            if matches!(ch.kind, ChartKind::Scatter | ChartKind::Bubble) {
                val_axes.get(1)
            } else {
                val_axes.first()
            }
        });
    if let Some((_, yax)) = primary_y {
        ch.y_axis = yax.clone();
        ch.axis_y = yax.title.clone();
        if !yax.num_fmt.is_empty() {
            ch.y_fmt = yax.num_fmt.clone();
        }
        ch.y_min = yax.min;
        ch.y_max = yax.max;
    }
    if let Some((_, y2)) = val_axes.iter().find(|(pos, ax)| !ax.hidden && pos == "r") {
        ch.y2_axis = y2.clone();
    }

    // 3. 解析 CELLAN 专属 `<cellanChart .../>` 扩展块（标准 OOXML `<c:extLst>` 内）
    parse_cellan_ext(ch, xml);
}

fn parse_cellan_ext(ch: &mut Chart, xml: &str) {
    let Some(pos) = xml.find("<cellan:meta") else {
        return;
    };
    let rest = &xml[pos..];
    let end = rest.find("</cellan:meta>").unwrap_or(rest.len());
    let body = &rest[..end];
    if let Some(k) = extract_attr(body, "kind").and_then(|s| ChartKind::from_ext_code(&s)) {
        ch.kind = k;
    }
    if let Some(t) = extract_attr(body, "theme") {
        ch.style.theme = ChartThemeId::from_code(&t);
    }
    if let Some(sub) = extract_attr(body, "subtitle") {
        ch.subtitle = crate::model::xml_unescape(&sub);
    }
    if let Some(sr) = extract_attr(body, "sourceRange") {
        ch.source_range = crate::model::xml_unescape(&sr);
    }
    if let Some(v) = extract_attr(body, "roundedBars") {
        ch.style.rounded_bars = v == "1";
    }
    if let Some(v) = extract_attr(body, "shadow") {
        ch.style.show_card_shadow = v == "1";
    }
    if let Some(v) = extract_attr(body, "transparentBg") {
        ch.style.transparent_bg = v == "1";
    }
    if let Some(v) = extract_attr(body, "switchRowCol") {
        ch.style.switch_row_col = v == "1";
    }
    if let Some(v) = extract_attr(body, "extrema") {
        ch.analytics.highlight_extrema = v == "1";
    }
    if let Some(v) = extract_attr(body, "growthBadge") {
        ch.analytics.show_growth_badge = v == "1";
    }
    if let Some(v) = extract_attr(body, "summaryBar") {
        ch.analytics.show_summary_bar = v == "1";
    }
    if let Some(v) = extract_attr(body, "pareto") {
        ch.analytics.show_pareto_line = v == "1";
    }
    if let Some(v) = extract_attr(body, "histBins").and_then(|s| s.parse::<u32>().ok()) {
        ch.analytics.hist_bins = Some(v);
    }
    if let Some(v) = extract_attr(body, "centerKpi") {
        ch.analytics.show_center_kpi = v == "1";
    }
    if let Some(v) = extract_attr(body, "centerKpiTitle") {
        ch.analytics.center_kpi_title = crate::model::xml_unescape(&v);
    }
    if let Some(v) = extract_attr(body, "wfTotals") {
        ch.analytics.waterfall_totals = v
            .split(',')
            .filter_map(|p| p.trim().parse::<usize>().ok())
            .collect();
    }
    if let Some(v) = extract_attr(body, "steppedMask") {
        for (i, bit) in v.split(',').enumerate() {
            if let Some(s) = ch.series.get_mut(i) {
                s.stepped = bit.trim() == "1";
            }
        }
    }
    if let Some(v) = extract_attr(body, "hiddenMask") {
        for (i, bit) in v.split(',').enumerate() {
            if let Some(s) = ch.series.get_mut(i) {
                s.hidden = bit.trim() == "1";
            }
        }
    }
    // 参考线 <cellan:refLine .../>
    let mut rscan = body;
    while let Some(rp) = rscan.find("<cellan:refLine") {
        let seg = &rscan[rp..];
        let rend = seg.find("/>").map(|e| e + 2).unwrap_or(seg.len());
        let tag = &seg[..rend];
        let label = extract_attr(tag, "label")
            .map(|s| crate::model::xml_unescape(&s))
            .unwrap_or_default();
        let axis = match extract_attr(tag, "axis").as_deref() {
            Some("y2") => RefLineAxis::YSecondary,
            Some("x") => RefLineAxis::X,
            _ => RefLineAxis::YPrimary,
        };
        let color = extract_attr(tag, "color")
            .and_then(|c| parse_rgb_hex(&c))
            .unwrap_or([220, 76, 62]);
        let dash = extract_attr(tag, "dash")
            .map(|d| LineDash::from_code(&d))
            .unwrap_or(LineDash::Dash);
        let v1 = extract_attr(tag, "v1")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let v2 = extract_attr(tag, "v2")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let mode = match extract_attr(tag, "mode").as_deref() {
            Some("fixed") => RefLineMode::Fixed(v1),
            Some("median") => RefLineMode::Median,
            Some("min") => RefLineMode::Min,
            Some("max") => RefLineMode::Max,
            Some("pct") => RefLineMode::Percentile(v1),
            Some("band") => RefLineMode::Band { low: v1, high: v2 },
            Some("stdBand") => RefLineMode::StdDevBand,
            _ => RefLineMode::Mean,
        };
        ch.ref_lines.push(ReferenceLine {
            label,
            axis,
            mode,
            color,
            dash,
        });
        rscan = &seg[rend.max(1)..];
    }
}

pub fn parse_chart(xml: &str) -> Option<Chart> {
    let mut kind: Option<ChartKind> = None;
    let mut grouping = String::new();
    let mut show_labels = false;
    let mut label_opts = LabelOpts::default();
    let mut radar_style = String::new();
    let mut bar_dir = String::new();
    let mut title = String::new();
    let mut series: Vec<Series> = Vec::new();
    let mut legend = false;
    let mut legend_pos = String::new();
    let mut hole_size: u8 = 55;
    let mut first_slice_ang: u16 = 0;
    let mut pie_explosion: u8 = 0;
    let mut gap_width: u16 = 120;
    let mut plot_vis_only = true;
    let mut blank_policy = BlankPolicy::Gap;

    let mut r = Reader::from_str(xml);
    let mut buf = String::new();
    let mut cur: Option<Series> = None;
    let mut in_tx = false;
    let mut in_cat = false;
    let mut in_val = false;
    let mut in_x = false;
    let mut in_bubble_size = false;
    let mut in_valax = false;
    let mut in_dlbls = false;
    let mut in_title = false;
    let mut in_axis = false;
    let mut saw_bar = false;
    let mut saw_line = false;
    let mut saw_area = false;
    let mut plot: Option<(ChartKind, Vec<String>, usize)> = None;
    let mut plot_dir = String::new();
    let mut plot_grouping = String::new();
    let mut y_fmt = String::new();
    let mut pt_idx: Option<usize> = None;
    let mut containers: Vec<(usize, usize, Vec<String>, ChartKind)> = Vec::new();

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref()).to_string();
                match name.as_str() {
                    "barChart" | "bar3DChart" => {
                        saw_bar = true;
                        kind.get_or_insert(ChartKind::Column);
                        plot = Some((ChartKind::Column, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "lineChart" | "line3DChart" => {
                        saw_line = true;
                        kind.get_or_insert(ChartKind::Line);
                        plot = Some((ChartKind::Line, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "areaChart" | "area3DChart" => {
                        saw_area = true;
                        kind.get_or_insert(ChartKind::Area);
                        plot = Some((ChartKind::Area, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "pieChart" | "pie3DChart" => {
                        kind.get_or_insert(ChartKind::Pie);
                        plot = Some((ChartKind::Pie, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "doughnutChart" => {
                        kind.get_or_insert(ChartKind::Doughnut);
                        plot = Some((ChartKind::Doughnut, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "radarChart" => {
                        kind.get_or_insert(ChartKind::Radar);
                        plot = Some((ChartKind::Radar, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "scatterChart" => {
                        kind.get_or_insert(ChartKind::Scatter);
                        plot = Some((ChartKind::Scatter, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "bubbleChart" => {
                        kind.get_or_insert(ChartKind::Bubble);
                        plot = Some((ChartKind::Bubble, Vec::new(), series.len()));
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "ser" => {
                        if cur.is_none() {
                            cur = Some(Series::default());
                        }
                    }
                    "pt" => {
                        pt_idx = None;
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("idx") {
                                pt_idx = crate::model::xml_unescape(&a.value.to_string())
                                    .parse::<usize>()
                                    .ok();
                            }
                        }
                    }
                    "tx" => in_tx = true,
                    "cat" => in_cat = true,
                    "val" | "yVal" => in_val = true,
                    "xVal" => in_x = true,
                    "bubbleSize" => in_bubble_size = true,
                    "valAx" => {
                        in_valax = true;
                        in_axis = true;
                    }
                    "catAx" | "dateAx" => in_axis = true,
                    "dLbls" => in_dlbls = true,
                    "title" if !in_axis => in_title = true,
                    "legend" => legend = true,
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref()).to_string();
                let val_attr = || -> Option<String> {
                    e.attributes().flatten().find_map(|a| {
                        a.key
                            .as_ref()
                            .ends_with("val")
                            .then(|| crate::model::xml_unescape(&a.value.to_string()))
                    })
                };
                match name.as_str() {
                    "showVal" => {
                        if val_attr().as_deref() == Some("1") {
                            show_labels = true;
                        }
                    }
                    "showCatName" => {
                        if val_attr().as_deref() == Some("1") {
                            label_opts.show_cat = true;
                        }
                    }
                    "showSerName" => {
                        if val_attr().as_deref() == Some("1") {
                            label_opts.show_ser = true;
                        }
                    }
                    "showPercent" => {
                        if val_attr().as_deref() == Some("1") {
                            label_opts.show_pct = true;
                        }
                    }
                    "showLegendKey" => {
                        if val_attr().as_deref() == Some("1") {
                            label_opts.show_legend_key = true;
                        }
                    }
                    "showLeaderLines" => {
                        if val_attr().as_deref() == Some("1") {
                            label_opts.leader_lines = true;
                        }
                    }
                    "dLblPos" => {
                        if let Some(v) = val_attr() {
                            label_opts.position = v;
                        }
                    }
                    "numFmt" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("formatCode") {
                                let fc = crate::model::xml_unescape(&a.value.to_string());
                                if in_dlbls && label_opts.num_fmt.is_empty() {
                                    label_opts.num_fmt = fc;
                                } else if in_valax && y_fmt.is_empty() {
                                    y_fmt = fc;
                                }
                            }
                        }
                    }
                    "radarStyle" => {
                        if let Some(v) = val_attr() {
                            radar_style = v;
                        }
                    }
                    "grouping" => {
                        if let Some(v) = val_attr() {
                            if grouping.is_empty() {
                                grouping = v.clone();
                            }
                            plot_grouping = v;
                        }
                    }
                    "barDir" => {
                        if let Some(v) = val_attr() {
                            bar_dir = v.clone();
                            plot_dir = v;
                        }
                    }
                    "axId" => {
                        if let Some(v) = val_attr() {
                            if let Some(p) = plot.as_mut() {
                                p.1.push(v);
                            }
                        }
                    }
                    "legendPos" => {
                        if let Some(v) = val_attr() {
                            legend_pos = v;
                        }
                    }
                    "holeSize" => {
                        if let Some(v) = val_attr().and_then(|s| s.parse::<u8>().ok()) {
                            hole_size = v.clamp(10, 90);
                        }
                    }
                    "firstSliceAng" => {
                        if let Some(v) = val_attr().and_then(|s| s.parse::<u16>().ok()) {
                            first_slice_ang = v % 360;
                        }
                    }
                    "explosion" => {
                        if let Some(v) = val_attr().and_then(|s| s.parse::<u8>().ok()) {
                            pie_explosion = v.min(50);
                        }
                    }
                    "gapWidth" => {
                        if let Some(v) = val_attr().and_then(|s| s.parse::<u16>().ok()) {
                            gap_width = v.clamp(0, 500);
                        }
                    }
                    "plotVisOnly" => {
                        if let Some(v) = val_attr() {
                            plot_vis_only = v != "0" && !v.eq_ignore_ascii_case("false");
                        }
                    }
                    "dispBlanksAs" => {
                        if let Some(v) = val_attr() {
                            blank_policy = BlankPolicy::from_ooxml(&v);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                buf.push_str(&crate::model::xml_unescape(&t));
            }
            Ok(Event::End(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref());
                let text = buf.trim().to_string();
                match name {
                    "ser" => {
                        if let Some(mut s) = cur.take() {
                            s.kind = plot.as_ref().map(|p| p.0);
                            series.push(s);
                        }
                    }
                    "tx" => in_tx = false,
                    "cat" => in_cat = false,
                    "val" | "yVal" => in_val = false,
                    "xVal" => in_x = false,
                    "bubbleSize" => in_bubble_size = false,
                    "valAx" => {
                        in_valax = false;
                        in_axis = false;
                    }
                    "catAx" | "dateAx" => in_axis = false,
                    "dLbls" => in_dlbls = false,
                    "title" if !in_axis => in_title = false,
                    "f" => {
                        if let Some(s) = cur.as_mut() {
                            if in_tx && s.name_ref.is_empty() {
                                s.name_ref = text.clone();
                            } else if (in_cat || in_x) && s.cat_ref.is_empty() {
                                s.cat_ref = text.clone();
                            } else if in_bubble_size && s.size_ref.is_empty() {
                                s.size_ref = text.clone();
                            } else if in_val && s.val_ref.is_empty() {
                                s.val_ref = text.clone();
                            }
                        }
                    }
                    "barChart" | "bar3DChart" | "lineChart" | "line3DChart" | "areaChart"
                    | "area3DChart" | "pieChart" | "pie3DChart" | "doughnutChart"
                    | "scatterChart" | "bubbleChart" | "radarChart" => {
                        if let Some((mut k, ids, start)) = plot.take() {
                            k = resolve_plot_kind(k, &plot_dir, &plot_grouping);
                            for se in series.iter_mut().skip(start) {
                                se.kind = Some(k);
                            }
                            containers.push((start, series.len(), ids, k));
                        }
                        plot_dir.clear();
                        plot_grouping.clear();
                    }
                    "v" => {
                        if let Some(s) = cur.as_mut() {
                            if in_tx && s.name.is_empty() {
                                s.name = text.clone();
                            } else if in_cat {
                                match pt_idx {
                                    Some(i) => {
                                        if s.cats.len() <= i {
                                            s.cats.resize(i + 1, String::new());
                                        }
                                        s.cats[i] = text.clone();
                                    }
                                    None => s.cats.push(text.clone()),
                                }
                            } else if in_x {
                                let v = text.parse::<f64>().unwrap_or(f64::NAN);
                                match pt_idx {
                                    Some(i) => {
                                        if s.xv.len() <= i {
                                            s.xv.resize(i + 1, f64::NAN);
                                        }
                                        s.xv[i] = v;
                                    }
                                    None => s.xv.push(v),
                                }
                            } else if in_bubble_size {
                                let v = text.parse::<f64>().unwrap_or(f64::NAN);
                                match pt_idx {
                                    Some(i) => {
                                        if s.sizes.len() <= i {
                                            s.sizes.resize(i + 1, f64::NAN);
                                        }
                                        s.sizes[i] = v;
                                    }
                                    None => s.sizes.push(v),
                                }
                            } else if in_val {
                                let v = text.parse::<f64>().unwrap_or(f64::NAN);
                                match pt_idx {
                                    Some(i) => {
                                        if s.vals.len() <= i {
                                            s.vals.resize(i + 1, f64::NAN);
                                        }
                                        s.vals[i] = v;
                                    }
                                    None => s.vals.push(v),
                                }
                            }
                        } else if in_title && title.is_empty() {
                            title = text.clone();
                        }
                    }
                    _ => {}
                }
                if name == "t" && in_title && title.is_empty() {
                    title = text.clone();
                }
                buf.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    let base_k = kind?;
    if let Some((_, _, primary, _)) = containers.first() {
        for (start, end, ids, _) in containers.iter().skip(1) {
            if !primary.is_empty() && ids != primary {
                for se in series.iter_mut().take(*end).skip(*start) {
                    se.secondary = true;
                }
            }
        }
    }
    let resolved_k = if let Some((_, _, _, ck)) = containers.first() {
        *ck
    } else {
        resolve_plot_kind(base_k, &bar_dir, &grouping)
    };
    let multi_kind = (saw_bar as u8 + saw_line as u8 + saw_area as u8) >= 2;
    let final_k = if multi_kind {
        ChartKind::Combo
    } else {
        resolved_k
    };

    let mut chart = Chart {
        kind: final_k,
        title,
        series,
        legend,
        anchor: (0, 0, 14, 8),
        legend_pos,
        show_labels,
        label_opts,
        radar_style,
        y_fmt,
        style: ChartStyleOpts {
            hole_size,
            first_slice_ang,
            pie_explosion,
            gap_width,
            plot_vis_only,
            blank_policy,
            ..ChartStyleOpts::default()
        },
        ..Chart::default()
    };
    post_fill(&mut chart, xml);
    Some(chart)
}

fn resolve_plot_kind(base: ChartKind, bar_dir: &str, grouping: &str) -> ChartKind {
    match base {
        ChartKind::Column | ChartKind::Bar => {
            let is_bar = bar_dir == "bar";
            match grouping {
                "percentStacked" => {
                    if is_bar {
                        ChartKind::BarStacked100
                    } else {
                        ChartKind::ColumnStacked100
                    }
                }
                "stacked" => {
                    if is_bar {
                        ChartKind::BarStacked
                    } else {
                        // 兼容旧约定：parse_chart 以前把 barDir="col"+stacked 映射为 BarStacked 的测试仍可正常识别
                        ChartKind::ColumnStacked
                    }
                }
                _ => {
                    if is_bar {
                        ChartKind::Bar
                    } else {
                        ChartKind::Column
                    }
                }
            }
        }
        ChartKind::Line => match grouping {
            "stacked" | "percentStacked" => ChartKind::LineStacked,
            _ => ChartKind::Line,
        },
        ChartKind::Area => match grouping {
            "percentStacked" => ChartKind::AreaStacked100,
            "stacked" => ChartKind::AreaStacked,
            _ => ChartKind::Area,
        },
        other => other,
    }
}

/// 尝试原生解析 Excel 2016+ `chartEx`（瀑布图/漏斗图/树图/直方图/箱线图）。
/// 若该 `chartEx` 仅含 Excel 内部私有公式 `_xlchart.v1.*` 且无内联缓存数据点，返回 `None`，
/// 交由上层保持 `ChartKind::Unknown` 并逐字节无损保留原部件。
fn parse_chartex(xml: &str) -> Option<Chart> {
    let mut r = Reader::from_str(xml);
    let mut layout_id = String::new();
    let mut has_binning = false;
    let mut title = chartex_title(xml);
    let mut series_name = String::new();
    let mut cat_ref = String::new();
    let mut val_ref = String::new();
    let mut cats = Vec::new();
    let mut vals = Vec::new();
    let mut subtotals = Vec::new();
    let mut in_str_dim = false;
    let mut in_num_dim = false;
    let mut in_tx = false;
    let mut in_f = false;
    let mut in_pt = false;
    let mut pt_idx: Option<usize> = None;
    let mut buf = String::new();

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref());
                match name {
                    "series" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("layoutId") {
                                layout_id = crate::model::xml_unescape(&a.value.to_string());
                            }
                        }
                    }
                    "binning" => has_binning = true,
                    "strDim" => in_str_dim = true,
                    "numDim" => in_num_dim = true,
                    "txData" => in_tx = true,
                    "f" => in_f = true,
                    "pt" => {
                        in_pt = true;
                        pt_idx = e.attributes().flatten().find_map(|a| {
                            a.key
                                .as_ref()
                                .ends_with("idx")
                                .then(|| a.value.to_string().parse::<usize>().ok())
                                .flatten()
                        });
                    }
                    "idx" => {
                        if let Some(v) = e.attributes().flatten().find_map(|a| {
                            a.key
                                .as_ref()
                                .ends_with("val")
                                .then(|| a.value.to_string().parse::<usize>().ok())
                                .flatten()
                        }) {
                            subtotals.push(v);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => buf.push_str(&crate::model::xml_unescape(&t)),
            Ok(Event::End(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref());
                let text = buf.trim().to_string();
                match name {
                    "strDim" => in_str_dim = false,
                    "numDim" => in_num_dim = false,
                    "txData" => in_tx = false,
                    "f" => {
                        in_f = false;
                        if !text.starts_with("_xlchart.") {
                            if in_str_dim && cat_ref.is_empty() {
                                cat_ref = text.clone();
                            } else if in_num_dim && val_ref.is_empty() {
                                val_ref = text.clone();
                            }
                        }
                    }
                    "v" => {
                        if in_tx && series_name.is_empty() {
                            series_name = text.clone();
                        }
                    }
                    "pt" => {
                        in_pt = false;
                        if in_str_dim {
                            let idx = pt_idx.unwrap_or(cats.len());
                            if cats.len() <= idx {
                                cats.resize(idx + 1, String::new());
                            }
                            cats[idx] = text.clone();
                        } else if in_num_dim {
                            let v = text.parse::<f64>().unwrap_or(f64::NAN);
                            let idx = pt_idx.unwrap_or(vals.len());
                            if vals.len() <= idx {
                                vals.resize(idx + 1, f64::NAN);
                            }
                            vals[idx] = v;
                        }
                    }
                    _ => {}
                }
                if in_f || in_pt {
                    // handled above
                }
                buf.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    let kind = match layout_id.as_str() {
        "waterfall" => ChartKind::Waterfall,
        "funnel" => ChartKind::Funnel,
        "treemap" => ChartKind::Treemap,
        "boxWhisker" => ChartKind::BoxWhisker,
        "clusteredColumn" | "paretoLine" if has_binning => ChartKind::Histogram,
        _ => return None,
    };
    if vals.is_empty() && val_ref.is_empty() {
        return None;
    }
    if title.is_empty() {
        title = series_name.clone();
    }
    let mut ch = Chart {
        kind,
        title,
        series: vec![Series {
            name: if series_name.is_empty() {
                kind.label().into()
            } else {
                series_name
            },
            cats,
            vals,
            cat_ref,
            val_ref,
            ..Series::default()
        }],
        is_chartex: true,
        ..Chart::default()
    };
    ch.analytics.waterfall_totals = subtotals;
    if layout_id == "paretoLine" {
        ch.analytics.show_pareto_line = true;
    }
    Some(ch)
}

/// 按当前工作簿最新单元格数据刷新图表系列值/分类/名称/气泡大小。
///
/// - 同时支持纵向列区域（`A2:A10`）与横向行区域（`B1:F1`）。
/// - 尊重 `chart.style.plot_vis_only`（启用时跳过筛选/手动隐藏的行与列）。
/// - 保留空值/错误槽位为 `NaN`，避免后续点左移错位。
pub fn refresh_chart_from_sheets(
    chart: &mut Chart,
    sheets: &[Sheet],
    default_sheet_idx: usize,
    styles: Option<&crate::style::StyleTable>,
    sys: crate::model::DateSystem,
) {
    let sheet_for = |r: &str| -> Option<&Sheet> {
        if let Some((prefix, _)) = r.rsplit_once('!') {
            let name = prefix.trim().trim_matches('\'').replace("''", "'");
            sheets
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(&name))
                .or_else(|| sheets.get(default_sheet_idx))
        } else {
            sheets.get(default_sheet_idx)
        }
    };
    let vis_only = chart.style.plot_vis_only;
    for s in chart.series.iter_mut() {
        let skind = s.kind.unwrap_or(chart.kind);
        if !s.name_ref.is_empty()
            && let Some(sh) = sheet_for(&s.name_ref)
            && let Some(cells) = read_ref_in_vis(sh, &s.name_ref, styles, sys, false)
            && let Some((_, v)) = cells.first()
        {
            s.name = v.clone();
        }
        if !s.cat_ref.is_empty()
            && let Some(sh) = sheet_for(&s.cat_ref)
        {
            if let Some(cells) = read_ref_in_vis(sh, &s.cat_ref, styles, sys, vis_only) {
                s.cats = cells.iter().map(|(_, v)| v.clone()).collect();
            }
            if matches!(skind, ChartKind::Scatter | ChartKind::Bubble)
                && let Some(nums) = read_ref_numbers_aligned(sh, &s.cat_ref, vis_only)
            {
                s.xv = nums;
            }
        }
        if !s.val_ref.is_empty()
            && let Some(sh) = sheet_for(&s.val_ref)
            && let Some(nums) = read_ref_numbers_aligned(sh, &s.val_ref, vis_only)
        {
            s.vals = nums;
        }
        if !s.size_ref.is_empty()
            && let Some(sh) = sheet_for(&s.size_ref)
            && let Some(nums) = read_ref_numbers_aligned(sh, &s.size_ref, vis_only)
        {
            s.sizes = nums;
        }
    }
}

/// 按当前工作簿最新单元格数据刷新图表系列值/分类/名称（仅对带有引用公式的系列生效，保留无引用的纯缓存图表）。
pub fn refresh_chart_from_sheets(
    chart: &mut Chart,
    sheets: &[Sheet],
    default_sheet_idx: usize,
    styles: Option<&crate::style::StyleTable>,
    sys: crate::model::DateSystem,
) {
    let sheet_for = |r: &str| -> Option<&Sheet> {
        if let Some((prefix, _)) = r.rsplit_once('!') {
            let name = prefix.trim().trim_matches('\'').replace("''", "'");
            sheets
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(&name))
                .or_else(|| sheets.get(default_sheet_idx))
        } else {
            sheets.get(default_sheet_idx)
        }
    };
    for s in chart.series.iter_mut() {
        if !s.name_ref.is_empty()
            && let Some(sh) = sheet_for(&s.name_ref)
            && let Some(cells) = read_ref_in(sh, &s.name_ref, styles, sys)
            && let Some((_, v)) = cells.first()
        {
            s.name = v.clone();
        }
        if !s.cat_ref.is_empty()
            && let Some(sh) = sheet_for(&s.cat_ref)
        {
            if let Some(cells) = read_ref_in(sh, &s.cat_ref, styles, sys) {
                s.cats = cells.iter().map(|(_, v)| v.clone()).collect();
            }
            if matches!(s.kind.unwrap_or(chart.kind), ChartKind::Scatter)
                && let Some(nums) = read_ref_numbers(sh, &s.cat_ref)
            {
                s.xv = nums.into_iter().map(|(_, v)| v).collect();
            }
        }
        if !s.val_ref.is_empty()
            && let Some(sh) = sheet_for(&s.val_ref)
            && let Some(cells) = read_ref_numbers(sh, &s.val_ref)
        {
            s.vals = cells.into_iter().map(|(_, v)| v).collect();
        }
    }
}

/// 用表里的实际值回填（图表只写了区间引用、没有缓存值的时候）
pub fn resolve_series(chart: &mut Chart, sheet: &Sheet) {
    resolve_series_in(chart, sheet, None, crate::model::DateSystem::D1900)
}

pub fn resolve_series_in(
    chart: &mut Chart,
    sheet: &Sheet,
    styles: Option<&crate::style::StyleTable>,
    sys: crate::model::DateSystem,
) {
    for s in chart.series.iter_mut() {
        if !s.name_ref.is_empty() && s.name.is_empty() {
            if let Some(cells) = read_ref_in_vis(sheet, &s.name_ref, styles, sys, false) {
                if let Some((_, v)) = cells.first() {
                    s.name = v.clone();
                }
            }
        }
        if !s.cat_ref.is_empty() && s.cats.is_empty() {
            if let Some(cells) = read_ref_in_vis(sheet, &s.cat_ref, styles, sys, false) {
                s.cats = cells.iter().map(|(_, v)| v.clone()).collect();
            }
        }
        if !s.val_ref.is_empty() && s.vals.is_empty() {
            if let Some(cells) = read_ref_numbers(sheet, &s.val_ref) {
                s.vals = cells.into_iter().map(|(_, v)| v).collect();
            }
        }
        if !s.size_ref.is_empty() && s.sizes.is_empty() {
            if let Some(cells) = read_ref_numbers(sheet, &s.size_ref) {
                s.sizes = cells.into_iter().map(|(_, v)| v).collect();
            }
        }
    }
}

/// 解析单个引用字符串得到 `(表名Option, (r1, c1, r2, c2))`（供画布数据源高亮与引擎共用）
pub fn parse_ref_rect(r: &str) -> Option<(Option<String>, (u32, u32, u32, u32))> {
    let (sheet, tail) = match r.rsplit_once('!') {
        Some((s, t)) => {
            let s = s.trim().trim_matches('\'').replace("''", "'");
            (Some(s), t)
        }
        None => (None, r),
    };
    let t = tail.replace('$', "").replace('\'', "");
    let (a, b) = match t.split_once(':') {
        Some((x, y)) => (x.trim(), y.trim()),
        None => (t.trim(), t.trim()),
    };
    let (r1, c1) = parse_a1(a)?;
    let (r2, c2) = parse_a1(b)?;
    Some((sheet, (r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2))))
}

/// "Sheet1!$B$2:$B$5" → 逐格坐标（按行优先）
fn ref_cells(r: &str) -> Option<Vec<(u32, u32)>> {
    let (_, (r1, c1, r2, c2)) = parse_ref_rect(r)?;
    let count = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
    if count > 200_000 {
        return None;
    }
    let mut out = Vec::with_capacity(count as usize);
    for rr in r1..=r2 {
        for cc in c1..=c2 {
            out.push((rr, cc));
        }
    }
    Some(out)
}

#[inline]
fn cell_is_hidden(sheet: &Sheet, rr: u32, cc: u32) -> bool {
    sheet.hidden_rows.contains(&rr)
        || sheet.hidden_rows_user.contains(&rr)
        || sheet.hidden_cols.contains(&cc)
}

fn read_ref_numbers(sheet: &Sheet, r: &str) -> Option<Vec<((u32, u32), f64)>> {
    let mut out = Vec::new();
    for (rr, cc) in ref_cells(r)? {
        let Some(cell) = sheet.get(rr, cc) else {
            continue;
        };
        let v = match &cell.value {
            crate::model::Value::Number(n) => Some(*n),
            crate::model::Value::Text(t) => t.trim().parse::<f64>().ok(),
            _ => None,
        };
        if let Some(v) = v {
            out.push(((rr, cc), v));
        }
    }
    Some(out)
}

/// 与分类下标一一对齐地读取数值：空格/文本/错误写入 `f64::NAN` 保持槽位对齐
fn read_ref_numbers_aligned(sheet: &Sheet, r: &str, vis_only: bool) -> Option<Vec<f64>> {
    let mut out = Vec::new();
    for (rr, cc) in ref_cells(r)? {
        if vis_only && cell_is_hidden(sheet, rr, cc) {
            continue;
        }
        let v = sheet
            .get(rr, cc)
            .and_then(|cell| match &cell.value {
                crate::model::Value::Number(n) => Some(*n),
                crate::model::Value::Text(t) => t.trim().parse::<f64>().ok(),
                _ => None,
            })
            .unwrap_or(f64::NAN);
        out.push(v);
    }
    Some(out)
}

fn read_ref_in_vis(
    sheet: &Sheet,
    r: &str,
    styles: Option<&crate::style::StyleTable>,
    sys: crate::model::DateSystem,
    vis_only: bool,
) -> Option<Vec<((u32, u32), String)>> {
    let mut out = Vec::new();
    for (rr, cc) in ref_cells(r)? {
        if vis_only && cell_is_hidden(sheet, rr, cc) {
            continue;
        }
        let v = sheet
            .get(rr, cc)
            .map(|c| match styles {
                Some(t) if !matches!(c.value, crate::model::Value::Empty) => {
                    let fmt = &t.get(sheet.style_of(rr, cc)).format;
                    crate::style::format_value_in(sys, &c.value, fmt)
                }
                _ => c.value.display(),
            })
            .unwrap_or_default();
        out.push(((rr, cc), v));
    }
    Some(out)
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut row = 0u32;
    let mut digits = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphabetic() && !digits {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if ch.is_ascii_digit() {
            digits = true;
            row = row * 10 + (ch as u32 - '0' as u32);
        } else if ch == ' ' {
            continue;
        } else {
            return None;
        }
    }
    if col == 0 || row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

/// chartex（Excel 2016+ 高级图表）里取标题文本
fn chartex_title(xml: &str) -> String {
    let mut r = Reader::from_str(xml);
    let mut in_title = false;
    let mut buf = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                if local(e.name().as_ref()) == "title" {
                    in_title = true;
                }
            }
            Ok(Event::End(e)) => {
                if local(e.name().as_ref()) == "title" {
                    let t = buf.trim();
                    if !t.is_empty() {
                        return t.to_string();
                    }
                    in_title = false;
                    buf.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if in_title {
                    buf.push_str(&crate::model::xml_unescape(t.as_ref()));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bar_chart() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart>
            <c:title><c:tx><c:rich><a:p><a:r><a:t>销售额</a:t></a:r></a:p></c:rich></c:tx></c:title>
            <c:plotArea><c:barChart><c:barDir val="col"/><c:grouping val="clustered"/>
              <c:ser><c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:pt><c:v>一月</c:v></c:pt></c:strCache></c:strRef></c:tx>
                <c:cat><c:strRef><c:f>Sheet1!$A$2:$A$4</c:f><c:strCache><c:pt><c:v>甲</c:v></c:pt><c:pt><c:v>乙</c:v></c:pt><c:pt><c:v>丙</c:v></c:pt></c:strCache></c:strRef></c:cat>
                <c:val><c:numRef><c:f>Sheet1!$B$2:$B$4</c:f><c:numCache><c:pt><c:v>10</c:v></c:pt><c:pt><c:v>20</c:v></c:pt><c:pt><c:v>30</c:v></c:pt></c:numCache></c:numRef></c:val>
              </c:ser>
            </c:barChart></c:plotArea><c:legend><c:legendPos val="r"/></c:legend>
        </c:chart></c:chartSpace>"#;
        let c = parse_chart(xml).expect("应解析出图表");
        assert_eq!(c.kind, ChartKind::Column);
        assert_eq!(c.title, "销售额");
        assert!(c.legend);
        assert_eq!(c.series.len(), 1);
        assert_eq!(c.series[0].name, "一月");
        assert_eq!(c.series[0].cats, vec!["甲", "乙", "丙"]);
        assert_eq!(c.series[0].vals, vec![10.0, 20.0, 30.0]);
    }

    #[test]
    fn parses_pie_and_line() {
        let pie = r#"<c:chartSpace><c:chart><c:plotArea><c:pieChart><c:ser><c:val><c:numRef><c:numCache><c:pt><c:v>1</c:v></c:pt><c:pt><c:v>3</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:pieChart></c:plotArea></c:chart></c:chartSpace>"#;
        let c = parse_chart(pie).unwrap();
        assert_eq!(c.kind, ChartKind::Pie);
        assert_eq!(c.series[0].vals, vec![1.0, 3.0]);

        let line = r#"<c:chartSpace><c:chart><c:plotArea><c:lineChart><c:ser><c:val><c:numRef><c:numCache><c:pt><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#;
        assert_eq!(parse_chart(line).unwrap().kind, ChartKind::Line);
    }

    #[test]
    fn bar_direction() {
        let xml = r#"<c:chartSpace><c:chart><c:plotArea><c:barChart><c:barDir val="bar"/><c:ser><c:val><c:numRef><c:numCache><c:pt><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        assert_eq!(parse_chart(xml).unwrap().kind, ChartKind::Bar);
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

    #[test]
    fn one_cell_anchor() {
        let xml = r#"<wsDr xmlns="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"><oneCellAnchor><from><col>4</col><colOff>0</colOff><row>1</row><rowOff>0</rowOff></from><ext cx="5400000" cy="2700000" /><graphicFrame><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="c" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1" /></a:graphicData></a:graphic></graphicFrame></oneCellAnchor></wsDr>"#;
        let a = parse_drawing_anchors(xml);
        assert_eq!(a.len(), 1, "应解析出一个锚点");
        assert_eq!(a[0].1, "rId1");
        assert_eq!(a[0].0 .0, 1, "起始行");
        assert_eq!(a[0].0 .1, 4, "起始列");
    }
}
