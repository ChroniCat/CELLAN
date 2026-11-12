//! 解析剪贴板上的 **XML Spreadsheet**（SpreadsheetML 2003）。
//!
//! Excel 复制区域时除了 HTML，还会放一份这个格式：里面有公式（R1C1）、缓存值、
//! 数据类型，以及字体名/字号/边框/数字格式——比 HTML 那条路拿到的还全。
//!
//! 实测样本（复制 A1:B2，B 列两个公式）：
//!
//! ```xml
//! <Cell ss:StyleID="s63" ss:Formula="=RC[-1]+R[1]C[-1]"><Data ss:Type="Number">3</Data></Cell>
//! ```
//!
//! 所以"带公式的粘贴"不需要解 OLE/BIFF：把 R1C1 折成 A1 就够了。

use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Number,
    Text,
    Bool,
    DateTime,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub font: String,
    pub size: f32,
    pub color: Option<[u8; 3]>,
    pub fill: Option<[u8; 3]>,
    pub border: Option<[u8; 3]>,
    pub num_format: String,
    /// 0 常规 1 左 2 中 3 右
    pub halign: u8,
}

/// XMLSS 的 `<Data ss:Type="DateTime">1904-01-02T00:00:00</Data>` → 目标簿的序列号。
///
/// 跨工作簿复制日期**不能**照搬数字：1900 簿的 1 是 1900-01-01，1904 簿的 1 是 1904-01-02，
/// 差 1462 天。所以按 ISO 日期 + **目标簿的日期体系**重新算序列号；普通数字原样保留。
pub fn iso_to_serial(sys: crate::model::DateSystem, iso: &str) -> Option<(f64, (i32, u32, u32))> {
    let s = iso.trim();
    let (date_part, time_part) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };
    let mut it = date_part.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    let serial = crate::formula::date_to_serial_in(sys, y, m, d)?;
    // 时间部分必须保留：XMLSS 的 DateTime 是完整时间戳，
    // "2024-03-01T12:00:00" 是中午，丢掉就成了午夜（差 0.5 天）。
    Some((serial + time_fraction(time_part)?, (y, m, d)))
}

/// "12:34:56.789" → 0.5242683…（一天的分数）；空串 → 0；格式不对 → None
fn time_fraction(t: &str) -> Option<f64> {
    let t = t.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    let mut parts = t.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let mi: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next().unwrap_or("0").parse().ok()?;
    if !(0.0..24.0).contains(&h) || !(0.0..60.0).contains(&mi) || !(0.0..61.0).contains(&sec) {
        return None;
    }
    Some((h * 3600.0 + mi * 60.0 + sec) / 86_400.0)
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    /// 显示值（数值会保留原始文本，便于回退）
    pub text: String,
    pub kind: Kind,
    /// A1 形式、带前导 `=`；没有公式就是 None
    pub formula: Option<String>,
    pub style: Style,
    pub colspan: u32,
    pub rowspan: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub rows: Vec<Vec<Cell>>,
}

impl Block {
    pub fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() || self.cols() == 0
    }
}

/// 解析整份 XML Spreadsheet；不是这个格式（没有 Workbook/Worksheet）就返回空
pub fn parse(xml: &str) -> Block {
    let mut styles: std::collections::HashMap<String, Style> = std::collections::HashMap::new();
    let mut block = Block::default();
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);

    let mut in_workbook = false;
    let mut style_id = String::new();
    let mut cur_style = Style::default();
    let mut in_style = false;

    let mut row: Vec<Cell> = Vec::new();
    let mut cur: Option<Cell> = None;
    let mut data_type = Kind::Number;
    let mut in_data = false;
    let mut text = String::new();
    let mut skip_sheet = false;
    let mut row_index: usize = 0;

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = local(e.name().as_ref());
                match n.as_str() {
                    "Workbook" => in_workbook = true,
                    "Style" => {
                        in_style = true;
                        style_id = attr(&e, "ID").unwrap_or_default();
                        cur_style = Style::default();
                    }
                    "Font" if in_style => {
                        cur_style.bold = matches!(attr(&e, "Bold").as_deref(), Some("1") | Some("true"));
                        cur_style.italic =
                            matches!(attr(&e, "Italic").as_deref(), Some("1") | Some("true"));
                        cur_style.font = attr(&e, "FontName").unwrap_or_default();
                        if let Some(sz) = attr(&e, "Size").and_then(|s| s.parse::<f32>().ok()) {
                            if sz > 0.0 {
                                cur_style.size = sz;
                            }
                        }
                        cur_style.color = attr(&e, "Color").and_then(|c| hex_rgb(&c));
                    }
                    "Interior" if in_style => {
                        let pattern = attr(&e, "Pattern").unwrap_or_default();
                        if !pattern.is_empty() && pattern != "None" {
                            cur_style.fill = attr(&e, "Color").and_then(|c| hex_rgb(&c));
                        }
                    }
                    "Border" if in_style => {
                        if let Some(c) = attr(&e, "Color").and_then(|c| hex_rgb(&c)) {
                            cur_style.border.get_or_insert(c);
                        }
                    }
                    "NumberFormat" if in_style => {
                        if let Some(f) = attr(&e, "Format") {
                            cur_style.num_format = normalize_num_format(&f);
                        }
                    }
                    "Alignment" if in_style => {
                        cur_style.halign = match attr(&e, "Horizontal").as_deref() {
                            Some("Left") => 1,
                            Some("Center") => 2,
                            Some("Right") => 3,
                            _ => 0,
                        };
                    }
                    "Worksheet" => {
                        // 只取第一张表（Excel 复制区域只给一张）
                        skip_sheet = !block.is_empty();
                    }
                    "Row" => {
                        if let Some(i) = attr(&e, "Index").and_then(|s| s.parse::<usize>().ok()) {
                            // Index 是 1 起；补空行
                            while row_index + 1 < i {
                                block.rows.push(Vec::new());
                                row_index += 1;
                            }
                        }
                        row = Vec::new();
                    }
                    "Cell" => {
                        skip_sheet = false;
                        // ss:Index 是 1 起的列号，中间缺的格要补空
                        if let Some(idx) = attr(&e, "Index").and_then(|s| s.parse::<usize>().ok()) {
                            while row.len() + 1 < idx {
                                row.push(Cell::default());
                            }
                        }
                        let mut c = Cell::default();
                        if let Some(id) = attr(&e, "StyleID") {
                            if let Some(s) = styles.get(&id) {
                                c.style = s.clone();
                            }
                        }
                        c.colspan = attr(&e, "MergeAcross")
                            .and_then(|s| s.parse::<u32>().ok())
                            .map(|n| n + 1)
                            .unwrap_or(1);
                        c.rowspan = attr(&e, "MergeDown")
                            .and_then(|s| s.parse::<u32>().ok())
                            .map(|n| n + 1)
                            .unwrap_or(1);
                        if let Some(f) = attr(&e, "Formula") {
                            let (r0, c0) = (row_index as u32, row.len() as u32);
                            let a1 = r1c1_to_a1(&f, r0, c0);
                            c.formula = Some(a1);
                        }
                        cur = Some(c);
                    }
                    "Data" => {
                        data_type = match attr(&e, "Type").as_deref() {
                            Some("String") => Kind::Text,
                            Some("Boolean") => Kind::Bool,
                            Some("DateTime") => Kind::DateTime,
                            Some("Error") => Kind::Error,
                            _ => Kind::Number,
                        };
                        in_data = true;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let n = local(e.name().as_ref());
                // 自闭合的样式元素
                if in_style {
                    match n.as_str() {
                        "Font" => {
                            cur_style.bold =
                                matches!(attr(&e, "Bold").as_deref(), Some("1") | Some("true"));
                            cur_style.italic =
                                matches!(attr(&e, "Italic").as_deref(), Some("1") | Some("true"));
                            cur_style.font = attr(&e, "FontName").unwrap_or_default();
                            if let Some(sz) = attr(&e, "Size").and_then(|s| s.parse::<f32>().ok()) {
                                if sz > 0.0 {
                                    cur_style.size = sz;
                                }
                            }
                            cur_style.color = attr(&e, "Color").and_then(|c| hex_rgb(&c));
                        }
                        "Interior" => {
                            let pattern = attr(&e, "Pattern").unwrap_or_default();
                            if !pattern.is_empty() && pattern != "None" {
                                cur_style.fill = attr(&e, "Color").and_then(|c| hex_rgb(&c));
                            }
                        }
                        "NumberFormat" => {
                            if let Some(f) = attr(&e, "Format") {
                                cur_style.num_format = normalize_num_format(&f);
                            }
                        }
                        "Alignment" => {
                            cur_style.halign = match attr(&e, "Horizontal").as_deref() {
                                Some("Left") => 1,
                                Some("Center") => 2,
                                Some("Right") => 3,
                                _ => 0,
                            };
                        }
                        _ => {}
                    }
                } else if n == "Cell" {
                    // 空格子（有样式、无 Data）
                    if let Some(idx) = attr(&e, "Index").and_then(|s| s.parse::<usize>().ok()) {
                        while row.len() + 1 < idx {
                            row.push(Cell::default());
                        }
                    }
                    let mut c = Cell::default();
                    if let Some(id) = attr(&e, "StyleID") {
                        if let Some(s) = styles.get(&id) {
                            c.style = s.clone();
                        }
                    }
                    c.colspan = attr(&e, "MergeAcross")
                        .and_then(|s| s.parse::<u32>().ok())
                        .map(|n| n + 1)
                        .unwrap_or(1);
                    c.rowspan = attr(&e, "MergeDown")
                        .and_then(|s| s.parse::<u32>().ok())
                        .map(|n| n + 1)
                        .unwrap_or(1);
                    row.push(c);
                }
            }
            Ok(Event::Text(t)) => {
                if in_data {
                    let raw = t.as_ref().to_string();
                    text.push_str(&quick_xml::escape::unescape(&raw).map(|c| c.into_owned()).unwrap_or_else(|_| raw.clone()));
                }
            }
            Ok(Event::End(e)) => {
                let n = local(e.name().as_ref());
                match n.as_str() {
                    "Style" => {
                        in_style = false;
                        styles.insert(style_id.clone(), cur_style.clone());
                    }
                    "Data" => {
                        in_data = false;
                        if let Some(c) = cur.as_mut() {
                            c.text = text.clone();
                            c.kind = data_type;
                        }
                    }
                    "Cell" => {
                        if let Some(c) = cur.take() {
                            row.push(c);
                        }
                    }
                    "Row" => {
                        if !skip_sheet {
                            block.rows.push(std::mem::take(&mut row));
                            row_index += 1;
                        }
                    }
                    "Worksheet" => skip_sheet = false,
                    "Workbook" => in_workbook = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    let _ = in_workbook;
    block
}

fn local(q: &str) -> String {
    q.rsplit(':').next().unwrap_or(q).to_string()
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().ends_with(key))
        .map(|a| {
            let raw = a.value.to_string();
            quick_xml::escape::unescape(&raw)
                .map(|c| c.into_owned())
                .unwrap_or(raw)
        })
}

fn hex_rgb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

/// XML Spreadsheet 里既有内置格式名（Fixed/Standard/Percent…）也有自定义格式串，
/// 前者要翻成 Excel 的格式代码，后者原样保留
fn normalize_num_format(f: &str) -> String {
    let t = f.trim();
    match t.to_ascii_lowercase().as_str() {
        "" | "general" => String::new(),
        "fixed" => "0.00".into(),
        "standard" => "#,##0.00".into(),
        "percent" => "0.00%".into(),
        "scientific" => "0.00E+00".into(),
        "currency" => "¥#,##0.00".into(),
        "short date" | "medium date" => "yyyy-mm-dd".into(),
        "long date" => "yyyy年m月d日".into(),
        "time" => "h:mm:ss".into(),
        "short time" => "h:mm".into(),
        _ => t.to_string(),
    }
}

// ---------------------------------------------------------------- R1C1 → A1

/// `=RC[-1]+R[1]C[-1]` 以 (row0,col0) 为基准折成 `=A1+A2`
pub fn r1c1_to_a1(f: &str, row0: u32, col0: u32) -> String {
    let chars: Vec<char> = f.chars().collect();
    let mut out = String::with_capacity(f.len() + 8);
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        // 字符串字面量整段搬走（里面的 R/C 不是引用）
        if c == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '"' {
                    if chars.get(i + 1) == Some(&'"') {
                        out.push('"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == 'R' || c == 'r' {
            let (rend, rpart) = parse_offset(&chars, i + 1);
            let is_c = matches!(chars.get(rend), Some('C') | Some('c'));
            if is_c {
                let (cend, cpart) = parse_offset(&chars, rend + 1);
                out.push_str(&build_a1(&rpart, &cpart, row0, col0));
                i = cend;
                continue;
            }
            // 只有 R 段（整行引用）本引擎不支持，原样留着
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// R/C 后面的偏移：`[±n]` 相对、`n` 绝对、什么都没有就是相对 0
#[derive(Debug, Clone, Copy)]
struct Part {
    abs: bool,
    n: i64,
}

fn parse_offset(chars: &[char], i: usize) -> (usize, Part) {
    let mut j = i;
    if j < chars.len() && chars[j] == '[' {
        let mut k = j + 1;
        let mut sign = 1i64;
        if k < chars.len() && (chars[k] == '-' || chars[k] == '+') {
            if chars[k] == '-' {
                sign = -1;
            }
            k += 1;
        }
        let start = k;
        while k < chars.len() && chars[k].is_ascii_digit() {
            k += 1;
        }
        if k > start && k < chars.len() && chars[k] == ']' {
            let n = chars[start..k]
                .iter()
                .collect::<String>()
                .parse::<i64>()
                .unwrap_or(0);
            return (k + 1, Part { abs: false, n: sign * n });
        }
        return (j, Part { abs: false, n: 0 });
    }
    let start = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if j > start {
        let n = chars[start..j]
            .iter()
            .collect::<String>()
            .parse::<i64>()
            .unwrap_or(1);
        return (j, Part { abs: true, n });
    }
    (i, Part { abs: false, n: 0 })
}

/// 绝对行写成 `B$3`（列绝对才是 `$B3`）
fn build_a1(r: &Part, c: &Part, row0: u32, col0: u32) -> String {
    let row = if r.abs { r.n } else { row0 as i64 + r.n + 1 };
    let col = if c.abs { c.n } else { col0 as i64 + c.n + 1 };
    let row = row.max(1);
    let col = col.max(1);
    let col_s = crate::model::col_label((col - 1) as u32);
    format!(
        "{}{}{}{}",
        if c.abs { "$" } else { "" },
        col_s,
        if r.abs { "$" } else { "" },
        row
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_relative_and_absolute() {
        // B1（row0=0,col0=1）
        assert_eq!(r1c1_to_a1("=RC[-1]+R[1]C[-1]", 0, 1), "=A1+A2");
        // B2
        assert_eq!(r1c1_to_a1("=R[-1]C[-1]*10", 1, 1), "=A1*10");
        // 绝对
        assert_eq!(r1c1_to_a1("=R1C1+R2C3", 5, 5), "=$A$1+$C$2");
        // 混合
        assert_eq!(r1c1_to_a1("=R3C[-1]", 0, 2), "=B$3");
    }

    #[test]
    fn keeps_strings_and_functions() {
        assert_eq!(r1c1_to_a1("=\"R2C3\"&RC[-1]", 0, 1), "=\"R2C3\"&A1");
        assert_eq!(r1c1_to_a1("=ROUND(RC[-1],2)", 0, 3), "=ROUND(C1,2)");
        assert_eq!(r1c1_to_a1("=SUM(R[-1]C:R[1]C)", 2, 2), "=SUM(C2:C4)");
    }

    #[test]
    fn parses_real_clipboard_sample() {
        let xml = r##"<?xml version="1.0"?>
<?mso-application progid="Excel.Sheet"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"
 xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <Styles>
  <Style ss:ID="Default" ss:Name="Normal"><Font ss:FontName="等线" ss:Size="11" ss:Color="#000000"/></Style>
  <Style ss:ID="s63"><Font ss:FontName="等线" ss:Size="11" ss:Color="#000000" ss:Bold="1"/><Interior ss:Color="#FFFF00" ss:Pattern="Solid"/></Style>
 </Styles>
 <Worksheet ss:Name="Sheet1">
  <Table ss:ExpandedColumnCount="2" ss:ExpandedRowCount="2">
   <Row>
    <Cell ss:StyleID="s63"><Data ss:Type="Number">1</Data></Cell>
    <Cell ss:StyleID="s63" ss:Formula="=RC[-1]+R[1]C[-1]"><Data ss:Type="Number">3</Data></Cell>
   </Row>
   <Row>
    <Cell ss:StyleID="s63"><Data ss:Type="Number">2</Data></Cell>
    <Cell ss:StyleID="s63" ss:Formula="=R[-1]C[-1]*10"><Data ss:Type="Number">10</Data></Cell>
   </Row>
  </Table>
 </Worksheet>
</Workbook>"##;
        let b = parse(xml);
        assert_eq!(b.rows.len(), 2);
        assert_eq!(b.rows[0].len(), 2);
        assert_eq!(b.rows[0][1].formula.as_deref(), Some("=A1+A2"));
        assert_eq!(b.rows[0][1].text, "3");
        assert_eq!(b.rows[1][1].formula.as_deref(), Some("=A1*10"));
        assert_eq!(b.rows[0][0].style.bold, true);
        assert_eq!(b.rows[0][0].style.fill, Some([255, 255, 0]));
        assert_eq!(b.rows[0][0].style.font, "等线");
        assert_eq!(b.rows[0][0].style.size, 11.0);
        // 内置格式名要翻成格式代码
        assert_eq!(normalize_num_format("Fixed"), "0.00");
        assert_eq!(normalize_num_format("General"), "");
        assert_eq!(normalize_num_format("#,##0"), "#,##0");
    }

    #[test]
    fn parses_merges_and_text() {
        let xml = r##"<Workbook xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet><Table>
<Row><Cell ss:MergeAcross="1" ss:MergeDown="1"><Data ss:Type="String">合计</Data></Cell><Cell ss:Index="4"><Data ss:Type="Boolean">1</Data></Cell></Row>
</Table></Worksheet></Workbook>"##;
        let b = parse(xml);
        assert_eq!(b.rows[0][0].colspan, 2);
        assert_eq!(b.rows[0][0].rowspan, 2);
        assert_eq!(b.rows[0][0].kind, Kind::Text);
        assert_eq!(b.rows[0][0].text, "合计");
        assert_eq!(b.rows[0].len(), 4, "ss:Index=4 要补两格空");
        assert_eq!(b.rows[0][3].kind, Kind::Bool);
    }
}
