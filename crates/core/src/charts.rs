//! 图表：读 drawing 锚点 + chart 定义，并在网格上画出来。
//!
//! 支持柱状/条形/折线/面积/饼图/散点。数据点优先用图表里缓存的值（numCache/strCache），
//! 没有缓存再按 `Sheet1!$B$2:$B$5` 去表里取——这样打开别人的文件不会因为公式没算而空白。

use crate::model::Sheet;
use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Bar,
    BarStacked,
    Column,
    Line,
    Area,
    Pie,
    Doughnut,
    Scatter,
    /// 组合图：前面的系列柱状（主轴），最后一个系列折线（次轴）
    Combo,
    /// 雷达图
    Radar,
    /// 本应用不建模的图种（瀑布/直方/树图/箱线等）：只保留原部件并显示占位
    Unknown,
}

impl ChartKind {
    pub fn label(&self) -> &'static str {
        match self {
            ChartKind::Bar => "条形图",
            ChartKind::BarStacked => "堆积条形图",
            ChartKind::Column => "柱状图",
            ChartKind::Line => "折线图",
            ChartKind::Area => "面积图",
            ChartKind::Pie => "饼图",
            ChartKind::Doughnut => "圆环图",
            ChartKind::Scatter => "散点图",
            ChartKind::Combo => "组合图",
            ChartKind::Radar => "雷达图",
            ChartKind::Unknown => "高级/未知图表",
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
    /// 系列颜色（None = 用默认调色板）
    pub color: Option<[u8; 3]>,
    /// 散点图的 X 值（xVal）；其它图型为空
    pub xv: Vec<f64>,
    /// 该系列自己的图型（组合图里柱/线混排）；None = 跟随图表主类型
    pub kind: Option<ChartKind>,
    /// 是否画在次坐标轴（axId 与主轴不同）
    pub secondary: bool,
}

/// 数据标签的逐项选项（Excel「设置数据标签格式」里的那组勾选）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LabelOpts {
    /// 类别名称
    pub show_cat: bool,
    /// 系列名称
    pub show_ser: bool,
    /// 百分比（饼图/圆环）
    pub show_pct: bool,
    /// 图例项标识
    pub show_legend_key: bool,
    /// 引导线（饼图）
    pub leader_lines: bool,
    /// 标签位置：ctr/inBase/outEnd/bestFit/l/r/t/b，空=默认
    pub position: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub kind: ChartKind,
    pub title: String,
    pub series: Vec<Series>,
    pub legend: bool,
    /// 锚点（单元格坐标，含右下角）
    pub anchor: (u32, u32, u32, u32),
    /// 图例位置："r" 右侧等；空表示无
    pub legend_pos: String,
    /// 图表部件路径（xl/charts/chartN.xml），删除/改类型要用
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
    /// X 轴标题
    pub axis_x: String,
    /// Y 轴标题
    pub axis_y: String,
    /// 数值轴数字格式（valAx 的 numFmt formatCode），空 = 常规
    pub y_fmt: String,
    /// Y 轴范围（None = 自动）
    pub y_min: Option<f64>,
    pub y_max: Option<f64>,
}

/// 解析某张表的图表
pub fn load_charts(path: &Path, sheet_name: &str) -> Result<Vec<Chart>> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(sheet_path) = targets.get(sheet_name) else { return Ok(Vec::new()); };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    load_charts_for_part(&mut zip, sheet_path)
}

fn load_charts_for_part(zip: &mut zip::ZipArchive<std::fs::File>, sheet_path: &str) -> Result<Vec<Chart>> {
    let dbg = std::env::var("SC_CHART_DBG").is_ok();
    // sheet 的 rels：drawing 关系
    let rels_path = match sheet_path.rsplit_once('/') {
        Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
        None => return Ok(Vec::new()),
    };
    if zip.index_for_name(&rels_path).is_none() { return Ok(Vec::new()); }
    let rels_xml = crate::xlsx::read_zip_text(zip, &rels_path)?;
    let Some(drawing_target) = find_rel_target(&rels_xml, "/drawing") else { if dbg { eprintln!("[chart] sheet rels 里没有 drawing 关系") } return Ok(Vec::new()) };
    // rels 里的路径相对于 sheet 所在目录
    let sheet_dir = match sheet_path.rsplit_once('/') {
        Some((d, _)) => format!("{d}/"),
        None => "xl/".into(),
    };
    let drawing_path = normalize(&sheet_dir, &drawing_target);

    let drawing_xml = crate::xlsx::read_zip_text(zip, &drawing_path)?;

    // drawing 的 rels：chart 关系
    let d_rels = match drawing_path.rsplit_once('/') {
        Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
        None => return Ok(Vec::new()),
    };
    let d_rels_ok = zip.index_for_name(&d_rels).is_some();
    let d_rels_xml = if d_rels_ok { crate::xlsx::read_zip_text(zip, &d_rels)? } else { String::new() };
    let mut chart_targets: HashMap<String, (String, bool)> = HashMap::new();
    {
        let mut r = Reader::from_str(&d_rels_xml);
        loop {
            match r.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("Relationship") => {
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
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }
    }

    if dbg {
        eprintln!("[chart] drawing={drawing_path} 关系={:?}", chart_targets);
        eprintln!("[chart] 锚点={:?}", parse_drawing_anchors(&drawing_xml));
    }
    // 每个 anchor 里的 chart r:id
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
        {
            let cx = crate::xlsx::read_zip_text(zip, &cp)?;
            // 截断/半截 XML 里可能已经出现 <c:barChart>，parse_chart 会返回部分结果；
            // 先做严格良构校验，坏 XML 一律报错而不是"读到一半当成功"。
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
                None
            } else {
                parse_chart(&cx)
            }
            .unwrap_or_else(|| Chart {
                kind: ChartKind::Unknown,
                title: if is_ex { chartex_title(&cx) } else { String::new() },
                series: Vec::new(),
                legend: false,
                anchor,
                legend_pos: String::new(),
                part: String::new(),
                drawing: String::new(),
                show_labels: false,
            label_opts: Default::default(),
            radar_style: String::new(),
                ref_fixes: Vec::new(),
                axis_x: String::new(),
                axis_y: String::new(),
                y_fmt: String::new(),
                y_min: None,
                y_max: None,
            });
            ch.anchor = anchor;
            ch.part = cp.clone();
            ch.drawing = drawing_path.clone();
            out.push(ch);
        }
    }
    Ok(out)
}

/// 一次把每张表的图表都读出来（key = 表序号，按 workbook.xml 顺序）
pub fn load_charts_all(
    path: &Path,
    sheets: &[Sheet],
) -> Result<std::collections::HashMap<u32, Vec<Chart>>> {
    load_charts_all_in(path, sheets, None, crate::model::DateSystem::D1900)
}

/// 图表分类轴要按本簿的样式与日期体系解析引用区域
pub fn load_charts_all_in(
    path: &Path,
    sheets: &[Sheet],
    styles: Option<&crate::style::StyleTable>,
    sys: crate::model::DateSystem,
) -> Result<std::collections::HashMap<u32, Vec<Chart>>> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut out = std::collections::HashMap::new();
    for (idx, sheet) in sheets.iter().enumerate() {
        let name = &sheet.name;
        let Some(part) = targets.get(name) else { continue };
        let mut list = load_charts_for_part(&mut zip, part)
            .map_err(|e| anyhow::anyhow!("读取工作表 {name} 的图表失败：{e:#}"))?;
        for chart in &mut list {
            resolve_series_in(chart, sheet, styles, sys);
        }
        if !list.is_empty() {
            out.insert(idx as u32, list);
        }
    }
    Ok(out)
}

fn normalize(base: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let joined = format!("{base}{target}");
    // 处理 ../ 与重复斜杠
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
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("Relationship") => {
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
            Ok(Event::Eof) => break,
            Err(_) => break,
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
        // 找下一个 anchor 起点
        let mut start = None;
        for tag in ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"] {
            // 标签可能带 xdr: 前缀，也可能用默认命名空间
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
        // 对应的结束标签
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
                // oneCellAnchor：用 ext 的 EMU 尺寸估算跨度（1 px = 9525 EMU）
                let cx = extract_ext(chunk, "cx").unwrap_or(4_000_000) as f32 / 9525.0;
                let cy = extract_ext(chunk, "cy").unwrap_or(2_000_000) as f32 / 9525.0;
                let dcol = (cx / 104.0).round().max(2.0) as u32;
                let drow = (cy / 22.0).round().max(4.0) as u32;
                (fc + dcol, fr + drow)
            }
        };
        if let Some(rid) = extract_attr(chunk, "r:id") {
            out.push(((fr, fc, tr.max(fr + 2), tc.max(fc + 3)), rid));
        }
        rest = &chunk_rest[end_pos..];
        if rest.is_empty() {
            break;
        }
    }
    out
}

/// 取 from/to 里的 col/row
fn extract_cell(chunk: &str, tag: &str) -> Option<(u32, u32)> {
    let open = ["<xdr:", "<"].iter().find_map(|p| {
        let pat = format!("{p}{tag}>");
        chunk.find(&pat).map(|i| (i, pat.clone()))
    })?;
    let start = open.0 + open.1.len();
    // 取到配对的结束标签，而不是下一个 '<'（那会是第一个子元素）
    // 结束标签同样可能带 xdr: 前缀
    let close = ["</xdr:", "</"]
        .iter()
        .find_map(|p| chunk[start..].find(&format!("{p}{tag}>")).map(|i| i + start))?;
    let body = &chunk[start..close];
    let col = tag_value(body, "col")?;
    let row = tag_value(body, "row")?;
    Some((col, row))
}

fn tag_value(body: &str, tag: &str) -> Option<u32> {
    // 子元素可能写成 <col> 或 <xdr:col>
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

fn local(name: &str) -> &str {
    match name.rsplit_once(':') {
        Some((_, l)) => l,
        None => name,
    }
}

/// 解析 chart XML

/// 从已生成的 chart XML 里补读：系列颜色、坐标轴标题、Y 轴 min/max
fn post_fill(ch: &mut Chart, xml: &str) {
    // 每个 <c:ser> 块里的第一个 srgbClr
    let mut rest = xml;
    let mut idx = 0usize;
    while let Some(pos) = rest.find("<c:ser>") {
        let chunk_rest = &rest[pos..];
        let end = chunk_rest.find("</c:ser>").unwrap_or(chunk_rest.len());
        let chunk = &chunk_rest[..end];
        if let Some(cpos) = chunk.find("srgbClr val=\"") {
            let v = &chunk[cpos + 13..];
            if let Some(q) = v.find('"') {
                let hex = &v[..q];
                if hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        if let Some(s) = ch.series.get_mut(idx) {
                            s.color = Some([r, g, b]);
                        }
                    }
                }
            }
        }
        idx += 1;
        rest = &chunk_rest[end.min(chunk_rest.len())..];
    }

    let axis_title = |xml: &str, tag: &str| -> String {
        let Some(pos) = xml.find(&format!("<c:{tag}")) else {
            return String::new();
        };
        let seg = &xml[pos..];
        let end = seg.find(&format!("</c:{tag}>")).unwrap_or(seg.len());
        let body = &seg[..end];
        match body.find("<a:t>") {
            Some(t) => {
                let v = &body[t + 5..];
                match v.find("</a:t>") {
                    Some(q) => crate::model::xml_unescape(&v[..q]),
                    None => String::new(),
                }
            }
            None => String::new(),
        }
    };
    ch.axis_x = axis_title(xml, "catAx");
    ch.axis_y = axis_title(xml, "valAx");

    // Y 轴 max/min：在 valAx 段里找 <c:max val=".." min=".."/>
    if let Some(pos) = xml.find("<c:valAx") {
        let body = &xml[pos..];
        let end = body.find("</c:valAx>").unwrap_or(body.len());
        let seg = &body[..end];
        let num = |key: &str| -> Option<f64> {
            let pat = format!("<c:{key} val=\"");
            let i = seg.find(&pat)? + pat.len();
            let rest = &seg[i..];
            let q = rest.find('"')?;
            rest[..q].parse().ok()
        };
        ch.y_max = num("max");
        ch.y_min = num("min");
    }
}

pub fn parse_chart(xml: &str) -> Option<Chart> {
    let mut kind: Option<ChartKind> = None;
    let mut stacked = false;
    let mut in_dlbls = false;
    let mut show_labels = false;
    let mut label_opts = LabelOpts::default();
    let mut radar_style = String::new();
    let mut bar_dir = String::new();
    let mut title = String::new();
    let mut series: Vec<Series> = Vec::new();
    let mut legend = false;
    let mut legend_pos = String::new();

    let mut r = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut buf = String::new();
    // 当前系列状态
    let mut cur: Option<Series> = None;
    let mut in_tx = false;
    let mut in_cat = false;
    let mut in_val = false;
    let mut in_title = false;
    let mut in_f = false;
    let mut saw_bar = false;
    let mut saw_line = false;
    // 当前 chart 容器（barChart/lineChart…）：类型、axId、起始系列号
    let mut plot: Option<(ChartKind, Vec<String>, usize)> = None;
    let mut plot_dir = String::new();
    let mut in_x = false;
    let mut in_valax = false;
    let mut y_fmt = String::new();
    let mut pt_idx: Option<usize> = None;
    let mut containers: Vec<(usize, usize, Vec<String>)> = Vec::new();

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
                    }
                    "lineChart" | "line3DChart" => {
                        saw_line = true;
                        kind.get_or_insert(ChartKind::Line);
                        plot = Some((ChartKind::Line, Vec::new(), series.len()));
                        plot_dir.clear();
                    }
                    "areaChart" => {
                        kind.get_or_insert(ChartKind::Area);
                        plot = Some((ChartKind::Area, Vec::new(), series.len()));
                        plot_dir.clear();
                    }
                    "pieChart" | "pie3DChart" => {
                        kind.get_or_insert(ChartKind::Pie);
                        plot = Some((ChartKind::Pie, Vec::new(), series.len()));
                        plot_dir.clear();
                    }
                    "doughnutChart" => {
                        kind.get_or_insert(ChartKind::Doughnut);
                        plot = Some((ChartKind::Doughnut, Vec::new(), series.len()));
                        plot_dir.clear();
                    }
                    "radarChart" => {
                        kind.get_or_insert(ChartKind::Radar);
                        plot = Some((ChartKind::Radar, Vec::new(), series.len()));
                        plot_dir.clear();
                    }
                    "scatterChart" => {
                        kind.get_or_insert(ChartKind::Scatter);
                        plot = Some((ChartKind::Scatter, Vec::new(), series.len()));
                        plot_dir.clear();
                    }
                    "ser" => {
                        if cur.is_none() {
                            cur = Some(Series::default());
                        }
                    }
                    "pt" => {
                        pt_idx = None;
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == "idx"
                                || a.key.as_ref().ends_with(":idx")
                                || a.key.as_ref().ends_with("idx")
                            {
                                pt_idx = crate::model::xml_unescape(&a.value.to_string())
                                    .parse::<usize>()
                                    .ok();
                            }
                        }
                    }
                    "tx" => in_tx = true,
                    "f" => in_f = true,
                    "cat" => in_cat = true,
                    "val" | "yVal" => in_val = true,
                    "xVal" => in_x = true,
                    "valAx" => in_valax = true,
                    "dLbls" => in_dlbls = true,
                    "title" => in_title = true,
                    "legend" => legend = true,
                    "grouping" => {}
                    "stacked" => stacked = true,
                    _ => {}
                }
                stack.push(name.clone());
            }
            Ok(Event::Empty(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref()).to_string();
                if name == "showVal" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("val")
                            && crate::model::xml_unescape(&a.value.to_string()) == "1"
                        {
                            show_labels = true;
                        }
                    }
                } else if matches!(
                    name.as_str(),
                    "showCatName"
                        | "showSerName"
                        | "showPercent"
                        | "showLegendKey"
                        | "showLeaderLines"
                ) {
                    let on = e.attributes().flatten().any(|a| {
                        a.key.as_ref().ends_with("val")
                            && crate::model::xml_unescape(&a.value.to_string()) == "1"
                    });
                    if on {
                        match name.as_str() {
                            "showCatName" => label_opts.show_cat = true,
                            "showSerName" => label_opts.show_ser = true,
                            "showPercent" => label_opts.show_pct = true,
                            "showLegendKey" => label_opts.show_legend_key = true,
                            "showLeaderLines" => label_opts.leader_lines = true,
                            _ => {}
                        }
                    }
                } else if name == "radarStyle" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("val") {
                            radar_style = crate::model::xml_unescape(&a.value.to_string());
                        }
                    }
                } else if name == "dLblPos" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("val") {
                            label_opts.position = crate::model::xml_unescape(&a.value.to_string());
                        }
                    }
                } else if name == "grouping" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("val")
                            && crate::model::xml_unescape(&a.value.to_string()) == "stacked"
                        {
                            stacked = true;
                        }
                    }
                } else if name == "barDir" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("val") {
                            bar_dir = crate::model::xml_unescape(&a.value.to_string());
                            plot_dir = bar_dir.clone();
                        }
                    }
                } else if name == "numFmt" && in_valax && y_fmt.is_empty() {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("formatCode") {
                            y_fmt = crate::model::xml_unescape(&a.value.to_string());
                        }
                    }
                } else if name == "axId" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("val") {
                            if let Some(p) = plot.as_mut() {
                                p.1.push(crate::model::xml_unescape(&a.value.to_string()));
                            }
                        }
                    }
                } else if name == "legendPos" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("val") {
                            legend_pos = crate::model::xml_unescape(&a.value.to_string());
                        }
                    }
                } else if name == "pt" {
                    // 点值在 Start/Text/End 里
                }
            }
            Ok(Event::Text(t)) => {
                let raw: &str = &t;
                buf.push_str(&crate::model::xml_unescape(raw));
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
                    "valAx" => in_valax = false,
                    "title" => in_title = false,
                    "f" => {
                        in_f = false;
                        if let Some(s) = cur.as_mut() {
                            if in_tx && s.name_ref.is_empty() {
                                s.name_ref = text.clone();
                            } else if in_cat && s.cat_ref.is_empty() {
                                s.cat_ref = text.clone();
                            } else if in_val && s.val_ref.is_empty() {
                                s.val_ref = text.clone();
                            }
                        }
                    }
                    "barChart" | "bar3DChart" | "lineChart" | "line3DChart" | "areaChart"
                    | "pieChart" | "pie3DChart" | "doughnutChart" | "scatterChart" | "radarChart" => {
                        if let Some((mut k, ids, start)) = plot.take() {
                            if matches!(k, ChartKind::Column) && plot_dir == "bar" {
                                k = ChartKind::Bar;
                            }
                            containers.push((start, series.len(), ids));
                        }
                        plot_dir.clear();
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
                                // 空值被 Excel 整个省掉，按 pt/@idx 放回原位，后面的值不会左移
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
                // 标题里的 <a:t> 也算
                if name == "t" && in_title && title.is_empty() {
                    title = text.clone();
                }
                buf.clear();
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    let mut k = kind?;
    // 次坐标轴：不同 chart 容器用了不同的 axId 对。第一个容器当主轴。
    if let Some((_, _, primary)) = containers.first() {
        for (start, end, ids) in containers.iter().skip(1) {
            if !primary.is_empty() && ids != primary {
                for se in series.iter_mut().take(*end).skip(*start) {
                    se.secondary = true;
                }
            }
        }
    }
    if matches!(k, ChartKind::Column) && bar_dir == "bar" {
        k = ChartKind::Bar;
    }
    if stacked {
        if matches!(k, ChartKind::Column | ChartKind::Bar) {
            k = ChartKind::BarStacked;
        }
    }
    let k = if saw_bar && saw_line { ChartKind::Combo } else { k };
    let mut chart = Chart {
        kind: k,
        title,
        series,
        legend,
        anchor: (0, 0, 8, 12),
        legend_pos,
        part: String::new(),
        drawing: String::new(),
        show_labels,
        label_opts,
        radar_style,
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_fmt,
        y_min: None,
        y_max: None,
    };
    post_fill(&mut chart, xml);
    Some(chart)
}

impl Chart {
    /// 插删行列后改写系列引用
    pub fn rewrite_refs(&mut self, shift: &crate::refs::Shift) -> bool {
        self.rewrite_mapped_refs(&crate::refs::MappedShift { shift, insertions: &[] })
    }

    pub(crate) fn rewrite_mapped_refs(&mut self, shift: &crate::refs::MappedShift) -> bool {
        let mut changed = false;
        for s in self.series.iter_mut() {
            for r in [&mut s.name_ref, &mut s.cat_ref, &mut s.val_ref] {
                let nf = crate::refs::rewrite_mapped(r, shift);
                if nf != *r {
                    self.ref_fixes.push((r.clone(), nf.clone()));
                    *r = nf;
                    changed = true;
                }
            }
        }
        changed
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
            if let Some(cells) = read_ref_in(sheet, &s.name_ref, styles, sys) {
                if let Some((_, v)) = cells.first() {
                    s.name = v.clone();
                }
            }
        }
        if !s.cat_ref.is_empty() && s.cats.is_empty() {
            if let Some(cells) = read_ref_in(sheet, &s.cat_ref, styles, sys) {
                s.cats = cells.iter().map(|(_, v)| v.clone()).collect();
            }
        }
        if !s.val_ref.is_empty() && s.vals.is_empty() {
            // 值轴拿原始数值；分类/名称才用显示文本
            if let Some(cells) = read_ref_numbers(sheet, &s.val_ref) {
                s.vals = cells.into_iter().map(|(_, v)| v).collect();
            }
        }
    }
}

/// "'表名'!$A$2:$A$5" → [(行列, 显示值)]
fn read_ref(sheet: &Sheet, r: &str) -> Option<Vec<((u32, u32), String)>> {
    read_ref_in(sheet, r, None, crate::model::DateSystem::D1900)
}

/// "Sheet1!$B$2:$B$5" → 逐格坐标（按行优先）
fn ref_cells(r: &str) -> Option<Vec<(u32, u32)>> {
    let tail = match r.rsplit_once('!') {
        Some((_, t)) => t,
        None => r,
    };
    let t = tail.replace('$', "").replace('\'', "");
    let (a, b) = match t.split_once(':') {
        Some((x, y)) => (x.to_string(), y.to_string()),
        None => (t.clone(), t),
    };
    let (r1, c1) = parse_a1(&a)?;
    let (r2, c2) = parse_a1(&b)?;
    let mut out = Vec::new();
    for rr in r1.min(r2)..=r1.max(r2) {
        for cc in c1.min(c2)..=c1.max(c2) {
            out.push((rr, cc));
        }
    }
    Some(out)
}

/// 值轴专用：取**原始数值**，不做数字格式化。
///
/// 显示文本会把 `1234.5` 变成 "1,234.50"、`0.125` 变成 "12.50%"、日期变成 "1904-01-02"，
/// 这些再 `parse::<f64>()` 全都失败——旧实现就是这样把整段系列点丢掉的。
fn read_ref_numbers(sheet: &Sheet, r: &str) -> Option<Vec<((u32, u32), f64)>> {
    let mut out = Vec::new();
    for (rr, cc) in ref_cells(r)? {
        let Some(cell) = sheet.get(rr, cc) else { continue };
        let v = match &cell.value {
            crate::model::Value::Number(n) => Some(*n),
            // 文本里存的数字仍然可画，按字面解析；带 % 或货币符号的文本不算数
            crate::model::Value::Text(t) => t.trim().parse::<f64>().ok(),
            _ => None,
        };
        if let Some(v) = v {
            out.push(((rr, cc), v));
        }
    }
    Some(out)
}

/// 从工作表读引用区域的**显示文本**。日期轴/分类轴必须按单元格自己的数字格式
/// 和本簿的日期体系渲染，否则 1904 簿的日期分类会显示成裸序列（或差 1462 天）。
fn read_ref_in(
    sheet: &Sheet,
    r: &str,
    styles: Option<&crate::style::StyleTable>,
    sys: crate::model::DateSystem,
) -> Option<Vec<((u32, u32), String)>> {
    let mut out = Vec::new();
    for (rr, cc) in ref_cells(r)? {
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

/// chartex（Excel 2016+ 高级图表）里取标题文本，只为占位显示
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
