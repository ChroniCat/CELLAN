//! Excel XML Spreadsheet representation of one or more complete worksheet columns.
use crate::model::{Cell, Value, Workbook};
use crate::style::StyleTable;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

const LAST_ROW: u32 = 1_048_575;

fn escaped(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // Excel's XML Spreadsheet writer emits decimal references for C0
            // characters. Its reader accepts these, including embedded newlines.
            '\0'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => write!(out, "&#{};", ch as u32).unwrap(),
            _ => out.push(ch),
        }
    }
    out
}

fn rgb([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn pattern_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "solid" => "Solid",
        "darkGray" => "Gray75",
        "mediumGray" => "Gray50",
        "lightGray" => "Gray25",
        "gray125" => "Gray125",
        "gray0625" => "Gray0625",
        "darkHorizontal" => "HorzStripe",
        "darkVertical" => "VertStripe",
        "darkUp" => "DiagStripe",
        "darkDown" => "ReverseDiagStripe",
        "darkGrid" => "DiagCross",
        "darkTrellis" => "ThickDiagCross",
        "lightHorizontal" => "ThinHorzStripe",
        "lightVertical" => "ThinVertStripe",
        "lightUp" => "ThinDiagStripe",
        "lightDown" => "ThinReverseDiagStripe",
        "lightGrid" => "ThinHorzCross",
        "lightTrellis" => "ThinDiagCross",
        _ => return None,
    })
}

fn xmlss_border(style: &str) -> (&'static str, u8) {
    match style {
        "double" => ("Double", 3),
        "dashed" => ("Dash", 1),
        "mediumDashed" => ("Dash", 2),
        "dotted" => ("Dot", 1),
        "dashDot" => ("DashDot", 1),
        "mediumDashDot" => ("DashDot", 2),
        "dashDotDot" => ("DashDotDot", 1),
        "mediumDashDotDot" => ("DashDotDot", 2),
        "slantDashDot" => ("SlantDashDot", 2),
        "hair" => ("Continuous", 0),
        "medium" => ("Continuous", 2),
        "thick" => ("Continuous", 3),
        _ => ("Continuous", 1),
    }
}

fn write_style(xml: &mut String, id: u16, styles: &StyleTable) {
    let s = styles.get(id);
    if id == 0 {
        xml.push_str("<Style ss:ID=\"Default\" ss:Name=\"Normal\">");
    } else {
        write!(xml, "<Style ss:ID=\"s{id}\">").unwrap();
    }
    let h = [
        "Automatic",
        "Left",
        "Center",
        "Right",
        "CenterAcrossSelection",
        "Distributed",
    ]
    .get(s.halign as usize)
    .unwrap_or(&"Automatic");
    let v = ["Bottom", "Top", "Center", "Bottom"]
        .get(s.valign as usize)
        .unwrap_or(&"Bottom");
    write!(xml, "<Alignment ss:Horizontal=\"{h}\" ss:Vertical=\"{v}\"").unwrap();
    if s.wrap {
        xml.push_str(" ss:WrapText=\"1\"");
    }
    if s.shrink_to_fit {
        xml.push_str(" ss:ShrinkToFit=\"1\"");
    }
    if s.rotation != 0 {
        write!(xml, " ss:Rotate=\"{}\"", s.rotation.clamp(-90, 90)).unwrap();
    }
    if s.vertical_text {
        xml.push_str(" ss:VerticalText=\"1\"");
    }
    if s.indent != 0 {
        write!(xml, " ss:Indent=\"{}\"", s.indent).unwrap();
    }
    xml.push_str("/><Borders>");
    for (position, b) in ["Left", "Right", "Top", "Bottom"]
        .into_iter()
        .zip(&s.border)
    {
        if b.is_none() { continue; }
        let (line, weight) = xmlss_border(crate::style::Border::ooxml_name(b.style));
        write!(xml, "<Border ss:Position=\"{position}\" ss:LineStyle=\"{line}\" ss:Weight=\"{weight}\" ss:Color=\"{}\"/>", rgb(b.color)).unwrap();
    }
    if let Some(d) = styles.diag_of(id) {
        let (line, weight) = xmlss_border(&d.style);
        for (position, enabled) in [("DiagonalLeft", d.down), ("DiagonalRight", d.up)] {
            if enabled {
                write!(xml, "<Border ss:Position=\"{position}\" ss:LineStyle=\"{line}\" ss:Weight=\"{weight}\" ss:Color=\"{}\"/>", rgb(d.color)).unwrap();
            }
        }
    }
    xml.push_str("</Borders><Font");
    if !s.font.is_empty() {
        write!(xml, " ss:FontName=\"{}\"", escaped(&s.font)).unwrap();
    }
    if s.size.is_finite() && s.size > 0.0 {
        write!(xml, " ss:Size=\"{}\"", s.size).unwrap();
    }
    write!(xml, " ss:Color=\"{}\"", rgb(s.color)).unwrap();
    if s.bold {
        xml.push_str(" ss:Bold=\"1\"");
    }
    if s.italic {
        xml.push_str(" ss:Italic=\"1\"");
    }
    if s.underline {
        write!(
            xml,
            " ss:Underline=\"{}\"",
            if s.double_underline {
                "Double"
            } else {
                "Single"
            }
        )
        .unwrap();
    }
    if s.strikethrough {
        xml.push_str(" ss:StrikeThrough=\"1\"");
    }
    xml.push_str("/>");
    let pattern = styles
        .pattern_of(id)
        .and_then(|p| Some((p, pattern_name(&p.kind)?)));
    if let Some((p, name)) = pattern.filter(|(_, name)| *name != "Solid") {
        write!(xml, "<Interior ss:Pattern=\"{name}\"").unwrap();
        // SpreadsheetML Color is the background; OOXML fgColor is the ink.
        if let Some(bg) = p.bg {
            write!(xml, " ss:Color=\"{}\"", rgb(bg)).unwrap();
        }
        if let Some(fg) = p.fg.or(s.fill) {
            write!(xml, " ss:PatternColor=\"{}\"", rgb(fg)).unwrap();
        }
        xml.push_str("/>");
    } else if let Some(color) = s.fill.or_else(|| pattern.and_then(|(p, _)| p.fg)) {
        write!(
            xml,
            "<Interior ss:Color=\"{}\" ss:Pattern=\"Solid\"/>",
            rgb(color)
        )
        .unwrap();
    }
    if !s.format.is_empty() {
        write!(xml, "<NumberFormat ss:Format=\"{}\"/>", escaped(&s.format)).unwrap();
    }
    write!(
        xml,
        "<Protection ss:Protected=\"{}\" x:HideFormula=\"{}\"/></Style>",
        u8::from(s.locked),
        u8::from(s.hidden)
    )
    .unwrap();
}

fn style_attr(xml: &mut String, ids: &mut BTreeSet<u16>, id: u16) {
    ids.insert(id);
    if id == 0 {
        xml.push_str(" ss:StyleID=\"Default\"");
    } else {
        write!(xml, " ss:StyleID=\"s{id}\"").unwrap();
    }
}

fn write_cell(xml: &mut String, cell: Option<&Cell>, r: u32, c: u32, quote: bool) {
    let Some(cell) = cell else {
        xml.push_str("/>");
        return;
    };
    if let Some(f) = &cell.formula {
        let formula = super::a1_to_r1c1(f, r, c);
        write!(xml, " ss:Formula=\"{}\"", escaped(&formula)).unwrap();
    }
    let (kind, data) = match &cell.value {
        Value::Empty => {
            xml.push_str("/>");
            return;
        }
        Value::Text(t) => ("String", escaped(t)),
        Value::Bool(b) => ("Boolean", u8::from(*b).to_string()),
        Value::Number(n) if n.is_finite() => ("Number", n.to_string()),
        Value::Number(_) => ("Error", "#NUM!".to_owned()),
        Value::Error(e) => ("Error", e.text().to_string()),
    };
    let ticked = if quote && matches!(cell.value, Value::Text(_)) {
        " x:Ticked=\"1\""
    } else {
        ""
    };
    write!(
        xml,
        "><Data ss:Type=\"{kind}\"{ticked}>{data}</Data></Cell>"
    )
    .unwrap();
}

/// Encode the entire logical selection with ss:Index on occupied rows/cells.
/// No matrix is allocated. A merge straddling the selection is not representable.
pub fn sparse_column_xmlss(wb: &Workbook, si: usize, rect: (u32, u32, u32, u32)) -> Option<String> {
    let (r1, c1, r2, c2) = rect;
    if (r1, r2) != (0, LAST_ROW) || c1 > c2 || c2 >= 16_384 {
        return None;
    }
    let sh = wb.sheets.get(si)?;
    let mut rows: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (key, _) in sh.iter_rect(rect) {
        rows.entry(key.row).or_default().push(key.col);
    }
    for (key, _) in sh.styles.iter_from(false, c1) {
        if key.col <= c2 {
            rows.entry(key.row).or_default().push(key.col);
        }
    }
    let mut merges = Vec::new();
    for &(a, b, z, d) in &sh.merges {
        if d < c1 || b > c2 {
            continue;
        }
        if b < c1 || d > c2 || a > z || z > LAST_ROW {
            return None;
        }
        rows.entry(a).or_default().push(b);
        merges.push((a, b, z, d));
    }
    for r in sh
        .row_styles
        .keys()
        .chain(sh.rows.entries().map(|(r, _)| r))
        .chain(sh.hidden_rows.iter())
        .chain(sh.hidden_rows_user.iter())
    {
        rows.entry(*r).or_default();
    }
    let mut index = crate::merges::MergeIndex::default();
    index.refresh(&merges);
    let mut ids = BTreeSet::from([0]);
    let mut body = String::new();
    for c in c1..=c2 {
        let width = sh.cols.size(c);
        let hidden = sh.hidden_cols.contains(&c) || width == 0.0;
        if width == sh.cols.default_size() && !hidden && !sh.col_styles.contains_key(&c) {
            continue;
        }
        write!(body, "<Column ss:Index=\"{}\"", c - c1 + 1).unwrap();
        if width > 0.0 {
            write!(body, " ss:AutoFitWidth=\"0\" ss:Width=\"{}\"", width * 0.75).unwrap();
        }
        if hidden {
            body.push_str(" ss:Hidden=\"1\"");
        }
        if let Some(&id) = sh.col_styles.get(&c) {
            style_attr(&mut body, &mut ids, id);
        }
        body.push_str("/>");
    }
    for (r, mut cols) in rows {
        if r > LAST_ROW {
            continue;
        }
        write!(body, "<Row ss:Index=\"{}\"", r + 1).unwrap();
        let height = sh.rows.size(r);
        if height != sh.rows.default_size() && height > 0.0 {
            write!(
                body,
                " ss:AutoFitHeight=\"0\" ss:Height=\"{}\"",
                height * 0.75
            )
            .unwrap();
        }
        if height == 0.0 || sh.hidden_rows.contains(&r) || sh.hidden_rows_user.contains(&r) {
            body.push_str(" ss:Hidden=\"1\"");
        }
        if let Some(&id) = sh.row_styles.get(&r) {
            style_attr(&mut body, &mut ids, id);
        }
        body.push('>');
        cols.sort_unstable();
        cols.dedup();
        for c in cols {
            let merge = index.at(r, c);
            if merge.is_some_and(|(a, b, _, _)| (a, b) != (r, c)) {
                continue;
            }
            write!(body, "<Cell ss:Index=\"{}\"", c - c1 + 1).unwrap();
            let id = sh.style_of(r, c);
            style_attr(&mut body, &mut ids, id);
            if let Some((a, b, z, d)) = merge {
                if d > b {
                    write!(body, " ss:MergeAcross=\"{}\"", d - b).unwrap();
                }
                if z > a {
                    write!(body, " ss:MergeDown=\"{}\"", z - a).unwrap();
                }
            }
            write_cell(&mut body, sh.get(r, c), r, c, wb.styles.quote_of(id));
        }
        body.push_str("</Row>");
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\"?><?mso-application progid=\"Excel.Sheet\"?><Workbook xmlns=\"urn:schemas-microsoft-com:office:spreadsheet\" xmlns:ss=\"urn:schemas-microsoft-com:office:spreadsheet\" xmlns:x=\"urn:schemas-microsoft-com:office:excel\">",
    );
    if wb.date_system.is_1904() {
        xml.push_str("<x:ExcelWorkbook><x:Date1904/></x:ExcelWorkbook>");
    }
    xml.push_str("<Styles>");
    for id in ids {
        write_style(&mut xml, id, &wb.styles);
    }
    write!(xml, "</Styles><Worksheet ss:Name=\"{}\"><Table ss:ExpandedColumnCount=\"{}\" ss:ExpandedRowCount=\"1048576\" x:FullColumns=\"1\" ss:DefaultColumnWidth=\"{}\" ss:DefaultRowHeight=\"{}\">",
        escaped(&sh.name), c2 - c1 + 1, sh.cols.default_size() * 0.75, sh.rows.default_size() * 0.75).unwrap();
    xml.push_str(&body);
    xml.push_str("</Table></Worksheet></Workbook>");
    Some(xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DateSystem;
    use crate::style::CellStyle;

    #[test]
    fn full_columns_keep_sparse_coordinates_layout_formulas_and_styles() {
        let mut wb = Workbook::new_blank();
        wb.date_system = DateSystem::D1904;
        let colstyle = wb.styles.intern(CellStyle {
            bold: true,
            ..CellStyle::default()
        });
        let mut cellstyle = CellStyle {
            fill: Some([1, 2, 3]),
            ..CellStyle::default()
        };
        cellstyle.border[0].style = 2;
        let sid = wb.styles.intern(cellstyle);
        wb.styles.set_diag(
            sid,
            crate::style_ext::DiagBorder {
                style: "mediumDashed".into(),
                up: true,
                down: true,
                color: [7, 8, 9],
                color_theme: None,
            },
        );
        wb.styles.set_pattern(
            sid,
            crate::style_ext::FillPattern {
                kind: "lightGrid".into(),
                fg: Some([1, 2, 3]),
                bg: Some([4, 5, 6]),
                fg_theme: None,
                bg_theme: None,
                fg_indexed: None,
                bg_indexed: None,
            },
        );
        let sh = &mut wb.sheets[0];
        sh.set_col_style_loaded(2, colstyle);
        sh.set_row_style_loaded(700_000, sid);
        sh.set_style_loaded(800_000, 2, 0);
        sh.set_style_loaded(800_001, 3, sid);
        sh.cols.set(2, 160.0);
        sh.rows.set(700_000, 40.0);
        sh.hidden_cols.insert(4);
        sh.hidden_rows_user.insert(700_001);
        sh.merge_range(30, 2, 31, 3);
        sh.set(
            900_000,
            3,
            Cell {
                value: Value::Number(6.0),
                formula: Some("=C$1*2".into()),
            },
        );
        sh.set(
            LAST_ROW,
            4,
            Cell {
                value: Value::Text("x&\u{1}y".into()),
                ..Cell::default()
            },
        );
        let xml = sparse_column_xmlss(&wb, 0, (0, 2, LAST_ROW, 4)).unwrap();
        for expected in [
            r#"ss:ExpandedColumnCount="3" ss:ExpandedRowCount="1048576" x:FullColumns="1""#,
            "<x:Date1904/>",
            r#"<Column ss:Index="1" ss:AutoFitWidth="0" ss:Width="120""#,
            r#"ss:MergeAcross="1" ss:MergeDown="1""#,
            r#"ss:Height="30""#,
            r#"<Row ss:Index="700002" ss:Hidden="1""#,
            r#"<Row ss:Index="800001"><Cell ss:Index="1" ss:StyleID="Default"/>"#,
            r#"<Row ss:Index="900001"><Cell ss:Index="2""#,
            r#"ss:Formula="=R1C[-1]*2""#,
            r#"<Row ss:Index="1048576"><Cell ss:Index="3""#,
            "x&amp;&#1;y",
            r#"ss:Position="DiagonalRight" ss:LineStyle="Dash""#,
            r##"ss:Pattern="ThinHorzCross" ss:Color="#040506" ss:PatternColor="#010203""##,
            r#"ss:Position="Left""#,
        ] {
            assert!(xml.contains(expected), "missing {expected} from {xml}");
        }
        assert_eq!(xml.matches("<Row ").count(), 7);
        assert!(super::super::parse(&xml).is_empty());
    }

    #[test]
    fn refuses_partial_merges_and_retains_the_full_extent_of_empty_columns() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].merge_range(0, 2, 0, 4);
        assert!(sparse_column_xmlss(&wb, 0, (0, 1, LAST_ROW, 3)).is_none());
        let xml = sparse_column_xmlss(&wb, 0, (0, 5, LAST_ROW, 5)).unwrap();
        assert!(xml.contains(r#"ss:ExpandedColumnCount="1" ss:ExpandedRowCount="1048576""#));
        assert!(!xml.contains("<Row "));
        assert!(sparse_column_xmlss(&wb, 0, (1, 0, LAST_ROW, 2)).is_none());
    }
}
