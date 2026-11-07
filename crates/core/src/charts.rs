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
        }
    }
}

#[derive(Clone, Debug, Default)]
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
}

#[derive(Clone, Debug)]
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
    /// 显示数据标签
    pub show_labels: bool,
    /// 引用被改写后要回写到 chart 部件里的 (旧, 新) 对
    pub ref_fixes: Vec<(String, String)>,
    /// X 轴标题
    pub axis_x: String,
    /// Y 轴标题
    pub axis_y: String,
    /// Y 轴范围（None = 自动）
    pub y_min: Option<f64>,
    pub y_max: Option<f64>,
}

/// 解析某张表的图表
pub fn load_charts(path: &Path, sheet_name: &str) -> Result<Vec<Chart>> {
    let dbg = std::env::var("SC_CHART_DBG").is_ok();
    let targets = crate::patch::sheet_targets(path)?;
    let Some(sheet_path) = targets.get(sheet_name) else { if dbg { eprintln!("[chart] 没有工作表路径") } return Ok(Vec::new()) };
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;

    // sheet 的 rels：drawing 关系
    let rels_path = match sheet_path.rsplit_once('/') {
        Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
        None => return Ok(Vec::new()),
    };
    let mut rels_xml = String::new();
    {
        use std::io::Read;
        let Ok(mut f) = zip.by_name(&rels_path) else { if dbg { eprintln!("[chart] 没有 sheet rels {rels_path}") } return Ok(Vec::new()) };
        let _ = f.read_to_string(&mut rels_xml);
    }
    let Some(drawing_target) = find_rel_target(&rels_xml, "/drawing") else { if dbg { eprintln!("[chart] sheet rels 里没有 drawing 关系") } return Ok(Vec::new()) };
    // rels 里的路径相对于 sheet 所在目录
    let sheet_dir = match sheet_path.rsplit_once('/') {
        Some((d, _)) => format!("{d}/"),
        None => "xl/".into(),
    };
    let drawing_path = normalize(&sheet_dir, &drawing_target);

    let mut drawing_xml = String::new();
    {
        use std::io::Read;
        let Ok(mut f) = zip.by_name(&drawing_path) else { if dbg { eprintln!("[chart] 打不开 drawing {drawing_path}") } return Ok(Vec::new()) };
        let _ = f.read_to_string(&mut drawing_xml);
    }

    // drawing 的 rels：chart 关系
    let d_rels = match drawing_path.rsplit_once('/') {
        Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
        None => return Ok(Vec::new()),
    };
    let mut d_rels_xml = String::new();
    {
        use std::io::Read;
        if let Ok(mut f) = zip.by_name(&d_rels) {
            let _ = f.read_to_string(&mut d_rels_xml);
        }
    }
    let mut chart_targets: HashMap<String, String> = HashMap::new();
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
                    if ty.ends_with("/chart") && !id.is_empty() {
                        let ddir = match drawing_path.rsplit_once('/') {
                            Some((d, _)) => format!("{d}/"),
                            None => "xl/drawings/".into(),
                        };
                        chart_targets.insert(id, normalize(&ddir, &target));
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
    let mut out = Vec::new();
    for (anchor, rid) in parse_drawing_anchors(&drawing_xml) {
        if let Some(cp) = chart_targets.get(&rid) {
            use std::io::Read;
            let mut cx = String::new();
            if let Ok(mut f) = zip.by_name(cp) {
                let _ = f.read_to_string(&mut cx);
            } else {
                continue;
            }
            if let Some(mut ch) = parse_chart(&cx) {
                ch.anchor = anchor;
                ch.part = cp.clone();
                ch.drawing = drawing_path.clone();
                out.push(ch);
            }
        }
    }
    Ok(out)
}

/// 一次把每张表的图表都读出来（key = 表序号，按 workbook.xml 顺序）
pub fn load_charts_all(
    path: &Path,
    sheets: &[Sheet],
) -> Result<std::collections::HashMap<u32, Vec<Chart>>> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut out = std::collections::HashMap::new();
    for (idx, (name, _)) in sorted_targets(&targets).into_iter().enumerate() {
        if let Ok(mut list) = load_charts(path, &name) {
            if let Some(sh) = sheets.get(idx) {
                for ch in list.iter_mut() {
                    resolve_series(ch, sh);
                }
            }
            if !list.is_empty() {
                out.insert(idx as u32, list);
            }
        }
    }
    Ok(out)
}

fn sorted_targets(m: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
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
fn parse_drawing_anchors(xml: &str) -> Vec<((u32, u32, u32, u32), String)> {
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
    let end_pat = format!("</{tag}>");
    let close = chunk[start..].find(&end_pat)? + start;
    let body = &chunk[start..close];
    let col = tag_value(body, "col")?;
    let row = tag_value(body, "row")?;
    Some((col, row))
}

fn tag_value(body: &str, tag: &str) -> Option<u32> {
    let pat = format!("<{tag}>");
    let i = body.find(&pat)?;
    let start = i + pat.len();
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
pub fn parse_chart(xml: &str) -> Option<Chart> {
    let mut kind: Option<ChartKind> = None;
    let mut stacked = false;
    let mut in_dlbls = false;
    let mut show_labels = false;
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

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref()).to_string();
                match name.as_str() {
                    "barChart" => {
                        kind.get_or_insert(ChartKind::Column);
                    }
                    "bar3DChart" => {
                        kind.get_or_insert(ChartKind::Column);
                    }
                    "lineChart" => {
                        kind.get_or_insert(ChartKind::Line);
                    }
                    "line3DChart" => {
                        kind.get_or_insert(ChartKind::Line);
                    }
                    "areaChart" => {
                        kind.get_or_insert(ChartKind::Area);
                    }
                    "pieChart" => {
                        kind.get_or_insert(ChartKind::Pie);
                    }
                    "pie3DChart" => {
                        kind.get_or_insert(ChartKind::Pie);
                    }
                    "doughnutChart" => {
                        kind.get_or_insert(ChartKind::Doughnut);
                    }
                    "scatterChart" => {
                        kind.get_or_insert(ChartKind::Scatter);
                    }
                    "ser" => {
                        if cur.is_none() {
                            cur = Some(Series::default());
                        }
                    }
                    "tx" => in_tx = true,
                    "f" => in_f = true,
                    "cat" => in_cat = true,
                    "val" | "yVal" => in_val = true,
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
                        if let Some(s) = cur.take() {
                            series.push(s);
                        }
                    }
                    "tx" => in_tx = false,
                    "cat" => in_cat = false,
                    "val" | "yVal" => in_val = false,
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
                    "v" => {
                        if let Some(s) = cur.as_mut() {
                            if in_tx && s.name.is_empty() {
                                s.name = text.clone();
                            } else if in_cat {
                                s.cats.push(text.clone());
                            } else if in_val {
                                if let Ok(v) = text.parse::<f64>() {
                                    s.vals.push(v);
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
    if matches!(k, ChartKind::Column) && bar_dir == "bar" {
        k = ChartKind::Bar;
    }
    if stacked {
        if matches!(k, ChartKind::Column | ChartKind::Bar) {
            k = ChartKind::BarStacked;
        }
    }
    Some(Chart {
        kind: k,
        title,
        series,
        legend,
        anchor: (0, 0, 8, 12),
        legend_pos,
        part: String::new(),
        drawing: String::new(),
        show_labels,
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_min: None,
        y_max: None,
    })
}

impl Chart {
    /// 插删行列后改写系列引用
    pub fn rewrite_refs(&mut self, shift: &crate::refs::Shift) -> bool {
        let mut changed = false;
        for s in self.series.iter_mut() {
            for r in [&mut s.name_ref, &mut s.cat_ref, &mut s.val_ref] {
                let nf = crate::refs::rewrite(r, shift);
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
    for s in chart.series.iter_mut() {
        if !s.name_ref.is_empty() && s.name.is_empty() {
            if let Some(cells) = read_ref(sheet, &s.name_ref) {
                if let Some((_, v)) = cells.first() {
                    s.name = v.clone();
                }
            }
        }
        if !s.cat_ref.is_empty() && s.cats.is_empty() {
            if let Some(cells) = read_ref(sheet, &s.cat_ref) {
                s.cats = cells.iter().map(|(_, v)| v.clone()).collect();
            }
        }
        if !s.val_ref.is_empty() && s.vals.is_empty() {
            if let Some(cells) = read_ref(sheet, &s.val_ref) {
                s.vals = cells
                    .iter()
                    .filter_map(|(_, v)| v.trim().parse::<f64>().ok())
                    .collect();
            }
        }
    }
}

/// "'表名'!$A$2:$A$5" → [(行列, 显示值)]
fn read_ref(sheet: &Sheet, r: &str) -> Option<Vec<((u32, u32), String)>> {
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
            let v = sheet
                .get(rr, cc)
                .map(|c| c.value.display())
                .unwrap_or_default();
            out.push(((rr, cc), v));
        }
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
