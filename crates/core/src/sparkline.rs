//! 迷你图（sparkline）：画在单元格里的折线 / 柱形 / 盈亏小图。
//!
//! 存储位置按 OOXML 的 x14 扩展：工作表 `<extLst>` 里 `uri="{05C60535-1F16-4fd2-B633-F4F36F0B64E0}"`
//! 的 `<x14:sparklineGroups>`，每个 `<x14:sparklineGroup>` 带一组格式 + 若干
//! `<x14:sparkline><xm:f>数据引用</xm:f><xm:sqref>锚点格</xm:sqref></x14:sparkline>`。
//! 本机 Excel 365 造样本实测出来的结构（见 docs/native-rewrite.md 那一轮记录）：
//!
//! - 折线：不写 `type`；柱形：`type="column"`；盈亏：`type="stacked" negative="1"`
//! - 颜色 8 项固定顺序 colorSeries/colorNegative/colorAxis/colorMarkers/
//!   colorFirst/colorLast/colorHigh/colorLow，ARGB 十六进制
//! - `xr2:uid` 是 Excel 自己用的 GUID，可省（省了 Excel 也照常打开）
//!
//! 模型按「一个锚点一条迷你图」拍平：Excel 的分组只是共享格式，读进来再按
//! 相同格式重新分组写回去，语义等价，改一条不会连带改别的。

use std::path::Path;

/// 迷你图类型
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SparkKind {
    #[default]
    Line,
    Column,
    /// Excel 的「盈亏」（win/loss）：正数向上、负数向下的等高分柱
    WinLoss,
}

impl SparkKind {
    pub fn label(self) -> &'static str {
        match self {
            SparkKind::Line => "折线",
            SparkKind::Column => "柱形",
            SparkKind::WinLoss => "盈亏",
        }
    }
}

/// 空单元格怎么处理（对应 `displayEmptyCellsAs`）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EmptyAs {
    /// 断开（gap）
    #[default]
    Gap,
    /// 当 0（zero）
    Zero,
    /// 用直线连起来（span）
    Connect,
}

impl EmptyAs {
    pub fn key(self) -> &'static str {
        match self {
            EmptyAs::Gap => "gap",
            EmptyAs::Zero => "zero",
            EmptyAs::Connect => "span",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "zero" => EmptyAs::Zero,
            "span" => EmptyAs::Connect,
            _ => EmptyAs::Gap,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            EmptyAs::Gap => "空单元格显示为空白",
            EmptyAs::Zero => "空单元格显示为零",
            EmptyAs::Connect => "用直线连接数据点",
        }
    }
}

/// 一条迷你图
#[derive(Clone, Debug, PartialEq)]
pub struct Sparkline {
    /// 锚点格（0-based）
    pub r: u32,
    pub c: u32,
    /// 数据所在工作表名；None = 和锚点同一张表
    pub source_sheet: Option<String>,
    /// 数据区（0-based，含端点）
    pub rect: (u32, u32, u32, u32),
    pub kind: SparkKind,
    /// 主色（colorSeries）
    pub color: [u8; 3],
    /// 标记所有数据点（折线才有意义）
    pub markers: bool,
    pub high: bool,
    pub low: bool,
    pub first: bool,
    pub last: bool,
    /// 负值用 colorNegative 画（折线/柱形）
    pub negative: bool,
    /// 显示水平坐标轴（零线）
    pub axis: bool,
    pub empty: EmptyAs,
}

impl Default for Sparkline {
    fn default() -> Self {
        Self {
            r: 0,
            c: 0,
            source_sheet: None,
            rect: (0, 0, 0, 0),
            kind: SparkKind::Line,
            // Excel 默认主色 #376092（Office 主题蓝）
            color: [0x37, 0x60, 0x92],
            markers: false,
            high: false,
            low: false,
            first: false,
            last: false,
            negative: false,
            axis: false,
            empty: EmptyAs::Gap,
        }
    }
}

impl Sparkline {
    /// `'表名'!$A$1:$E$1` 形式的数据引用（写文件用）
    pub fn data_ref(&self, own_sheet: &str) -> String {
        let name = self.source_sheet.as_deref().unwrap_or(own_sheet);
        format!(
            "'{}'!${}${}:${}${}",
            name.replace('\'', "''"),
            crate::model::col_label(self.rect.1),
            self.rect.0 + 1,
            crate::model::col_label(self.rect.3),
            self.rect.2 + 1
        )
    }

    /// 数据点个数（列数或行数里大的那个）
    pub fn point_count(&self) -> usize {
        let rows = (self.rect.2 - self.rect.0 + 1) as usize;
        let cols = (self.rect.3 - self.rect.1 + 1) as usize;
        rows.max(cols)
    }

    /// 显示用摘要：`折线  A1:E1`
    pub fn title(&self, own_sheet: &str) -> String {
        format!("{}  {}", self.kind.label(), self.data_ref(own_sheet))
    }

    fn same_style(&self, o: &Sparkline) -> bool {
        self.kind == o.kind
            && self.color == o.color
            && self.markers == o.markers
            && self.high == o.high
            && self.low == o.low
            && self.first == o.first
            && self.last == o.last
            && self.negative == o.negative
            && self.axis == o.axis
            && self.empty == o.empty
    }
}

/// 解析数据引用：`'S1'!$A$1:$E$1` / `Sheet1!A1:E1` / `A1:E1`
/// 返回 (表名, 区域)。表名带引号时按 Excel 规则还原 `''` → `'`。
pub fn parse_ref(s: &str) -> Option<(Option<String>, (u32, u32, u32, u32))> {
    let s = s.trim();
    let (sheet, range) = match s.rfind('!') {
        Some(i) => {
            let mut name = s[..i].trim().to_string();
            if name.starts_with('\'') && name.ends_with('\'') && name.len() >= 2 {
                name = name[1..name.len() - 1].replace("''", "'");
            }
            (Some(name), s[i + 1..].trim())
        }
        None => (None, s),
    };
    let range = crate::arr::parse_a1_range(range)?;
    Some((sheet, range))
}

/// 从工作表 XML 里的 `<x14:sparklineGroups>` 解析出拍平的迷你图列表
pub fn parse_sheet(xml: &str) -> Vec<Sparkline> {
    let mut out = Vec::new();
    let Some(start) = xml.find("<x14:sparklineGroups") else {
        return out;
    };
    let Some(end) = xml[start..]
        .find("</x14:sparklineGroups>")
        .map(|e| start + e)
    else {
        return out;
    };
    let body = &xml[start..end];
    let mut from = 0usize;
    while let Some(i) = body[from..].find("<x14:sparklineGroup") {
        let i = from + i;
        // 跳过 <x14:sparklineGroups 本身
        if body[i..].starts_with("<x14:sparklineGroups") {
            from = i + "<x14:sparklineGroups".len();
            continue;
        }
        let Some(gt) = body[i..].find('>').map(|e| i + e) else {
            break;
        };
        let tag = &body[i..=gt];
        let next = body[gt..]
            .find("<x14:sparklineGroup")
            .map(|e| gt + e)
            .unwrap_or(body.len());
        let block = &body[i..next];
        from = next;
        // 自闭合（没有内容）的组直接跳过
        if tag.ends_with("/>") {
            continue;
        }
        let kind = match attr(tag, "type").as_deref() {
            Some("column") => SparkKind::Column,
            Some("stacked") => SparkKind::WinLoss,
            _ => SparkKind::Line,
        };
        let color = find_tag_text(block, "x14:colorSeries")
            .as_deref()
            .and_then(|t| attr(t, "rgb"))
            .and_then(|v| parse_argb(&v))
            .unwrap_or([0x37, 0x60, 0x92]);
        let base = Sparkline {
            kind,
            color,
            markers: flag(tag, "markers"),
            high: flag(tag, "high"),
            low: flag(tag, "low"),
            first: flag(tag, "first"),
            last: flag(tag, "last"),
            negative: flag(tag, "negative") || kind == SparkKind::WinLoss,
            axis: flag(tag, "displayXAxis"),
            empty: EmptyAs::from_key(&attr(tag, "displayEmptyCellsAs").unwrap_or_default()),
            ..Default::default()
        };
        // 一条组里可能有多条 <x14:sparkline>
        let mut p = 0usize;
        while let Some(j) = block[p..].find("<x14:sparkline>") {
            let j = p + j;
            let Some(close) = block[j..].find("</x14:sparkline>") else {
                break;
            };
            let one = &block[j..j + close];
            p = j + close + 1;
            let Some(f) = tag_text(one, "xm:f") else { continue };
            let Some(sq) = tag_text(one, "xm:sqref") else {
                continue;
            };
            let Some((sheet, rect)) = parse_ref(&f) else {
                continue;
            };
            let Some((r, c)) = crate::arr::parse_a1_range(&sq).map(|x| (x.0, x.1)) else {
                continue;
            };
            out.push(Sparkline {
                r,
                c,
                source_sheet: sheet,
                rect,
                ..base.clone()
            });
        }
    }
    out
}

/// 把一组迷你图写成 `<ext …>` 块（不含 `<extLst>` 外壳）。
/// 相邻且格式相同的合成一个组，和 Excel 的分组语义一致。
pub fn groups_xml(list: &[Sparkline], own_sheet: &str) -> String {
    if list.is_empty() {
        return String::new();
    }
    let mut groups: Vec<Vec<&Sparkline>> = Vec::new();
    for sp in list {
        match groups.last_mut() {
            Some(g) if g[0].same_style(sp) => g.push(sp),
            _ => groups.push(vec![sp]),
        }
    }
    let mut out = String::new();
    out.push_str(
        "<ext uri=\"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}\" \
         xmlns:x14=\"http://schemas.microsoft.com/office/spreadsheetml/2009/9/main\">\
         <x14:sparklineGroups xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\">",
    );
    for g in groups {
        let s = g[0];
        out.push_str("<x14:sparklineGroup");
        match s.kind {
            SparkKind::Line => {}
            SparkKind::Column => out.push_str(" type=\"column\""),
            SparkKind::WinLoss => out.push_str(" type=\"stacked\""),
        }
        out.push_str(&format!(
            " displayEmptyCellsAs=\"{}\"",
            s.empty.key()
        ));
        for (name, on) in [
            ("markers", s.markers),
            ("high", s.high),
            ("low", s.low),
            ("first", s.first),
            ("last", s.last),
            ("negative", s.negative),
            ("displayXAxis", s.axis),
        ] {
            if on {
                out.push_str(&format!(" {name}=\"1\""));
            }
        }
        out.push('>');
        let argb = |c: [u8; 3]| format!("FF{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
        out.push_str(&format!("<x14:colorSeries rgb=\"{}\"/>", argb(s.color)));
        out.push_str("<x14:colorNegative rgb=\"FFD00000\"/>");
        out.push_str("<x14:colorAxis rgb=\"FF000000\"/>");
        out.push_str("<x14:colorMarkers rgb=\"FFD00000\"/>");
        out.push_str("<x14:colorFirst rgb=\"FFD00000\"/>");
        out.push_str("<x14:colorLast rgb=\"FFD00000\"/>");
        out.push_str("<x14:colorHigh rgb=\"FFD00000\"/>");
        out.push_str("<x14:colorLow rgb=\"FFD00000\"/>");
        out.push_str("<x14:sparklines>");
        for sp in &g {
            out.push_str(&format!(
                "<x14:sparkline><xm:f>{}</xm:f><xm:sqref>{}</xm:sqref></x14:sparkline>",
                esc(&sp.data_ref(own_sheet)),
                crate::model::cell_name(sp.r, sp.c)
            ));
        }
        out.push_str("</x14:sparklines></x14:sparklineGroup>");
    }
    out.push_str("</x14:sparklineGroups></ext>");
    out
}

/// 把迷你图写回工作表 XML：先摘掉旧的 sparkline 扩展，再按需插回去。
pub fn set_sparklines(xml: &str, list: &[Sparkline], own_sheet: &str) -> String {
    let mut x = strip_ext(xml);
    let block = groups_xml(list, own_sheet);
    if block.is_empty() {
        return x;
    }
    let ext = format!("<extLst>{block}</extLst>");
    match x.rfind("</extLst>") {
        // 已经有别的扩展（条件格式 x14、数据验证 x14…）：并进去
        Some(i) if has_real_extlst(&x) => {
            x.insert_str(i, &block);
            x
        }
        _ => {
            match x.rfind("</worksheet>") {
                Some(i) => x.insert_str(i, &ext),
                None => x.push_str(&ext),
            }
            x
        }
    }
}

/// 摘掉我们认的那条 `<ext uri="{05C60535…}">…</ext>`；extLst 空了就整块删掉。
fn strip_ext(xml: &str) -> String {
    let marker = "{05C60535-1F16-4fd2-B633-F4F36F0B64E0}";
    let mut x = xml.to_string();
    loop {
        let Some(mi) = x.find(marker) else { break };
        // 往回找 <ext
        let Some(start) = x[..mi].rfind("<ext ") else {
            break;
        };
        // 往前找该 <ext> 的结束：先看是不是自闭合，否则找 </ext>
        let Some(gt) = x[mi..].find('>').map(|e| mi + e) else {
            break;
        };
        // sparkline 扩展一定不是自闭合
        let Some(close) = x[gt..].find("</ext>").map(|e| gt + e + "</ext>".len()) else {
            break;
        };
        x.replace_range(start..close, "");
    }
    // extLst 空了就删掉（不留 <extLst></extLst>）
    for empty in ["<extLst></extLst>", "<extLst/>"] {
        while let Some(i) = x.find(empty) {
            x.replace_range(i..i + empty.len(), "");
        }
    }
    x
}

/// extLst 里除了空白还有别的扩展吗
fn has_real_extlst(xml: &str) -> bool {
    let Some(i) = xml.rfind("<extLst") else {
        return false;
    };
    // rfind 给的是最后一个 extLst；用它判断是否有 <ext 子元素
    let tail = &xml[i..];
    tail.contains("<ext ")
}

// ---------------------------------------------------------------- 小工具

fn attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let i = tag.find(&pat)? + pat.len();
    let rest = &tag[i..];
    let q = rest.find('"')?;
    Some(crate::model::xml_unescape(&rest[..q]))
}

fn flag(tag: &str, name: &str) -> bool {
    matches!(attr(tag, name).as_deref(), Some("1") | Some("true"))
}

fn parse_argb(s: &str) -> Option<[u8; 3]> {
    let t = s.trim();
    let hex = if t.len() == 8 { &t[2..] } else { t };
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// 找 `<tag …>` 这个起始标签（返回整段标签文本）
fn find_tag_text<'a>(xml: &'a str, tag: &str) -> Option<String> {
    let pat = format!("<{tag}");
    let i = xml.find(&pat)?;
    let gt = xml[i..].find('>').map(|e| i + e)?;
    Some(xml[i..=gt].to_string())
}

/// 找 `<tag>文字</tag>` 里的文字
fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let i = xml.find(&open)? + open.len();
    let rest = &xml[i..];
    let j = rest.find(&close)?;
    Some(crate::model::xml_unescape(&rest[..j]))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 保存收尾：把迷你图写回各表（只处理 dirty 的表）
pub fn apply(path: &Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut map: std::collections::HashMap<String, (Vec<Sparkline>, String)> =
        std::collections::HashMap::new();
    for s in &wb.sheets {
        if !s.sparklines_dirty {
            continue;
        }
        if let Some(part) = targets.get(&s.name) {
            map.insert(part.clone(), (s.sparklines.clone(), s.name.clone()));
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    crate::patch::rewrite_zip(path, |name, buf| {
        if let Some((list, sheet_name)) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return set_sparklines(&xml, list, sheet_name).into_bytes();
            }
        }
        buf
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机 Excel 365 造出来的真实片段（三种类型 + 一个双迷你图组）
    const EXCEL: &str = r#"<worksheet><sheetData/><extLst><ext uri="{05C60535-1F16-4fd2-B633-F4F36F0B64E0}" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><x14:sparklineGroups xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><x14:sparklineGroup displayEmptyCellsAs="gap" xr2:uid="{556C111C}"><x14:colorSeries rgb="FF376092"/><x14:colorNegative rgb="FFD00000"/><x14:sparklines><x14:sparkline><xm:f>'S1'!A1:E1</xm:f><xm:sqref>G1</xm:sqref></x14:sparkline><x14:sparkline><xm:f>'S1'!A2:E2</xm:f><xm:sqref>G2</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup><x14:sparklineGroup type="stacked" displayEmptyCellsAs="gap" negative="1"><x14:colorSeries rgb="FFFF0000"/><x14:sparklines><x14:sparkline><xm:f>'S1'!A3:E3</xm:f><xm:sqref>F3</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup><x14:sparklineGroup type="column" displayEmptyCellsAs="zero" high="1" low="1"><x14:colorSeries rgb="FF00B050"/><x14:sparklines><x14:sparkline><xm:f>'S1'!A2:E2</xm:f><xm:sqref>F2</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup></x14:sparklineGroups></ext></extLst></worksheet>"#;

    #[test]
    fn parses_excel_groups() {
        let list = parse_sheet(EXCEL);
        assert_eq!(list.len(), 4, "四条迷你图");
        assert_eq!((list[0].r, list[0].c), (0, 6));
        assert_eq!(list[0].kind, SparkKind::Line);
        assert_eq!(list[0].color, [0x37, 0x60, 0x92]);
        assert_eq!(list[0].rect, (0, 0, 0, 4));
        assert_eq!(list[0].source_sheet.as_deref(), Some("S1"));
        // 第二条同组，共享格式
        assert_eq!(list[1].color, list[0].color);
        // 盈亏
        assert_eq!(list[2].kind, SparkKind::WinLoss);
        assert!(list[2].negative);
        assert_eq!(list[2].color, [0xFF, 0x00, 0x00]);
        // 柱形 + 高低点 + 空单元当零
        assert_eq!(list[3].kind, SparkKind::Column);
        assert!(list[3].high && list[3].low);
        assert_eq!(list[3].empty, EmptyAs::Zero);
        assert_eq!(list[3].color, [0x00, 0xB0, 0x50]);
    }

    #[test]
    fn roundtrip_replaces_old_ext() {
        let mut list = parse_sheet(EXCEL);
        list[0].color = [1, 2, 3];
        let xml = set_sparklines(EXCEL, &list, "S1");
        let again = parse_sheet(&xml);
        assert_eq!(again.len(), 4);
        assert_eq!(again[0].color, [1, 2, 3]);
        // 模型是拍平的：改一条不会连带改同组的另一条（写回时按格式重新分组）
        assert_eq!(again[1].color, [0x37, 0x60, 0x92], "G2 保持原色");
        // 只剩一个 extLst、一个 ext
        assert_eq!(xml.matches("<extLst>").count(), 1);
        assert_eq!(xml.matches("{05C60535-1F16-4fd2-B633-F4F36F0B64E0}").count(), 1);
        // 依旧排在 worksheet 末尾
        assert!(xml.trim_end().ends_with("</worksheet>"));
    }

    #[test]
    fn clearing_removes_ext_lst() {
        let xml = set_sparklines(EXCEL, &[], "S1");
        assert!(!xml.contains("extLst"), "空列表要把 extLst 清干净：{xml}");
    }

    #[test]
    fn keeps_other_extensions() {
        let src = r#"<worksheet><extLst><ext uri="{CCE6A557-97BC-4b89-ADB6-D9C93CAAB3DF}" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><x14:dataValidations/></ext></extLst></worksheet>"#;
        let sp = Sparkline {
            r: 2,
            c: 3,
            rect: (0, 0, 0, 4),
            ..Default::default()
        };
        let xml = set_sparklines(src, &[sp], "S1");
        assert!(xml.contains("dataValidations"), "别的扩展不能丢");
        assert!(xml.contains("<x14:sparklineGroup"));
        assert_eq!(xml.matches("<extLst>").count(), 1);
    }

    #[test]
    fn parse_ref_handles_quotes() {
        assert_eq!(
            parse_ref("'My Sheet'!$A$1:$E$1"),
            Some((Some("My Sheet".into()), (0, 0, 0, 4)))
        );
        assert_eq!(
            parse_ref("'It''s'!A1:A3"),
            Some((Some("It's".into()), (0, 0, 2, 0)))
        );
        assert_eq!(parse_ref("B2:D4"), Some((None, (1, 1, 3, 3))));
        assert_eq!(parse_ref("坏的"), None);
    }

    #[test]
    fn data_ref_quotes_sheet() {
        let mut sp = Sparkline {
            c: 5,
            rect: (0, 0, 0, 4),
            source_sheet: Some("一季度 数据".into()),
            ..Default::default()
        };
        assert_eq!(sp.data_ref("S1"), "'一季度 数据'!$A$1:$E$1");
        sp.source_sheet = None;
        assert_eq!(sp.data_ref("S 1"), "'S 1'!$A$1:$E$1");
    }
}
