use crate::store::Store;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellRef {
    pub row: u32, // 0-based
    pub col: u32, // 0-based
}

impl CellRef {
    pub fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(Box<str>),
    Bool(bool),
}

/// 数字显示：整数不带小数点，小数最多 10 位并去掉尾零
pub fn format_number(n: f64) -> String {
    if n.is_nan() || n.is_infinite() {
        return "#NUM!".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{:.10}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

impl Value {
    /// 显示用文本（数字去掉多余的 .0）
    pub fn display(&self) -> String {
        match self {
            Value::Empty => String::new(),
            Value::Number(n) => format_number(*n),
            Value::Text(s) => s.to_string(),
            Value::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        }
    }

    /// 用户输入解析：能当数字就当数字，TRUE/FALSE 当布尔，其余当文本
    pub fn parse_input(s: &str) -> Value {
        let t = s.trim();
        if t.is_empty() {
            return Value::Empty;
        }
        if let Ok(n) = t.parse::<f64>() {
            return Value::Number(n);
        }
        match t.to_ascii_uppercase().as_str() {
            "TRUE" => Value::Bool(true),
            "FALSE" => Value::Bool(false),
            _ => Value::Text(s.into()),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub value: Value,
    /// 公式原文（以 = 开头）。用 Box<str> 而不是 String：省 8 字节/格，
    /// 而且公式一旦写入就不再变长，不需要容量字段
    pub formula: Option<Box<str>>,
}

impl Cell {
    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct Sheet {
    pub name: String,
    /// Runtime identity survives snapshots, renames and reordering. Save
    /// acknowledgements must never identify a sheet by its current index/name.
    pub identity: u64,
    /// Stable worksheet part in the loaded OOXML package. Renaming/reordering a
    /// sheet must retain its original relationships and other unmodeled content.
    /// A duplicated/new sheet has no source part.
    pub source_part: Option<String>,
    /// 分页列式存储（见 store.rs）：页内按列连续，扫描快、每格只占 40 字节
    cells: Store,
    /// 被清空过的单元格：保存时要真的从文件里删掉，否则旧值会残留
    pub cleared: std::collections::HashSet<CellRef>,
    /// 被动态数组公式溢出占用的单元格（重算前要清掉）
    pub spilled: std::collections::HashSet<CellRef>,
    /// 老式 CSE 数组公式：锚点 → 覆盖区域（保存时要写回 t="array" ref）
    pub arrays: Vec<(CellRef, (u32, u32, u32, u32))>,
    /// Excel 表格对象（ListObject）
    pub tables: Vec<crate::table::Table>,
    /// 图片（只记位置和部件路径；保存时原样不动）
    pub pictures: Vec<crate::pic::Picture>,
    /// 正在从文件加载（加载期间的 set 不算编辑）
    pub loading: bool,
    /// 这张表被改过没有：没改过就整段沿用原文件的 XML，别让 umya 重写
    pub edited: bool,
    /// 格式动过没有（动过就不能整段沿用基准的样式表）
    pub styles_dirty: bool,
    /// 合并单元格区域 (r1, c1, r2, c2)
    pub merges: Vec<(u32, u32, u32, u32)>,
    /// 列宽（只有被设置过的列才有条目）
    pub cols: crate::layout::Axis,
    /// 行高
    pub rows: crate::layout::Axis,
    /// 有样式的单元格 → 样式表索引（只存有样式的格子，稀疏）
    pub styles: HashMap<CellRef, u16>,
    /// 用户改过样式的格子（保存时只需要回写这些，不必重写整表样式）
    pub style_edits: std::collections::HashSet<CellRef>,
    /// 合并区域被改过，保存时要同步
    pub merges_dirty: bool,
    /// 条件格式
    pub cf: crate::cf::CfSheet,
    /// 条件格式被改过，保存时要同步
    pub cf_dirty: bool,
    /// 数据验证
    pub dv: Vec<crate::dv::DataValidation>,
    /// 数据验证被改过
    pub dv_dirty: bool,
    /// 批注
    pub notes: HashMap<CellRef, crate::notes::Note>,
    /// 批注被改过
    pub notes_dirty: bool,
    /// 冻结窗格：(冻结行数, 冻结列数)
    pub freeze: Option<(u32, u32)>,
    /// 冻结设置被改过
    pub freeze_dirty: bool,
    /// 自动筛选
    pub filter: Option<crate::sort_filter::Filter>,
    /// 被筛选隐藏的行（渲染时按高度 0 处理）
    pub hidden_rows: std::collections::HashSet<u32>,
    /// 用户手动隐藏的行（和筛选隐藏分开，筛选重算不会清掉）
    pub hidden_rows_user: std::collections::HashSet<u32>,
    /// 用户手动隐藏的列
    pub hidden_cols: std::collections::HashSet<u32>,
    /// 列宽/行高/隐藏状态被改过：保存时要重写 <cols> 和行属性
    pub layout_dirty: bool,
    /// 筛选状态被改过
    pub filter_dirty: bool,
    /// 页面设置
    pub page: crate::page::PageSetup,
    /// 页面设置被改过
    pub page_dirty: bool,
    /// 图表（只读展示，原样保留）
    pub charts: Vec<crate::charts::Chart>,
    /// 图表引用被改过：保存时要重写 chart 部件
    pub charts_dirty: bool,
    /// 工作表标签颜色（RGB）
    pub tab_color: Option<[u8; 3]>,
    /// 标签颜色被改过：保存时要写 <sheetPr><tabColor>
    pub tab_color_dirty: bool,
    /// 表格对象被改过（新建/删除/转换）：保存时要同步 table 部件
    pub tables_dirty: bool,
    /// 工作表保护（None = 未保护）
    pub protection: Option<crate::protection::SheetProtection>,
    /// 保护状态被改过
    pub protect_dirty: bool,
    /// 单元格富文本（有记录时优先于纯文本绘制）
    pub rich: std::collections::HashMap<(u32, u32), Vec<crate::richtext::RichRun>>,
    /// 富文本被改过
    pub rich_dirty: bool,
    /// 单元格超链接
    pub hyperlinks: std::collections::HashMap<(u32, u32), crate::hyperlinks::Hyperlink>,
    pub hyperlinks_dirty: bool,
}

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            identity: next_sheet_identity(),
            source_part: None,
            cells: Store::default(),
            cleared: std::collections::HashSet::new(),
            spilled: std::collections::HashSet::new(),
            arrays: Vec::new(),
            tables: Vec::new(),
            pictures: Vec::new(),
            loading: false,
            edited: false,
            styles_dirty: false,
            merges: Vec::new(),
            cols: crate::layout::Axis::new(crate::layout::DEFAULT_COL_W),
            rows: crate::layout::Axis::new(crate::layout::DEFAULT_ROW_H),
            styles: HashMap::new(),
            style_edits: std::collections::HashSet::new(),
            merges_dirty: false,
            cf: crate::cf::CfSheet::default(),
            cf_dirty: false,
            dv: Vec::new(),
            dv_dirty: false,
            notes: HashMap::new(),
            notes_dirty: false,
            freeze: None,
            freeze_dirty: false,
            filter: None,
            hidden_rows: std::collections::HashSet::new(),
            hidden_rows_user: std::collections::HashSet::new(),
            hidden_cols: std::collections::HashSet::new(),
            layout_dirty: false,
            filter_dirty: false,
            page: crate::page::PageSetup::default(),
            page_dirty: false,
            charts: Vec::new(),
            charts_dirty: false,
            tab_color: None,
            tab_color_dirty: false,
            tables_dirty: false,
            protection: None,
            protect_dirty: false,
            rich: std::collections::HashMap::new(),
            rich_dirty: false,
            hyperlinks: std::collections::HashMap::new(),
            hyperlinks_dirty: false,
        }
    }

    /// A copy inserted as a new sheet must not inherit the original identity.
    pub fn renew_identity(&mut self) {
        self.identity = next_sheet_identity();
        self.source_part = None;
    }

    pub fn get(&self, r: u32, c: u32) -> Option<&Cell> {
        self.cells.get(r, c)
    }

    pub fn get_mut(&mut self, r: u32, c: u32) -> Option<&mut Cell> {
        self.cells.get_mut(r, c)
    }

    pub fn value(&self, r: u32, c: u32) -> Value {
        self.cells.get(r, c).map(|x| x.value.clone()).unwrap_or_default()
    }

    pub fn contains(&self, r: u32, c: u32) -> bool {
        self.cells.contains(r, c)
    }

    pub fn set(&mut self, r: u32, c: u32, cell: Cell) {
        if !self.loading {
            self.edited = true;
        }
        let key = CellRef::new(r, c);
        if cell.is_blank() {
            if self.cells.remove(r, c).is_some() {
                self.cleared.insert(key);
            }
            return;
        }
        self.cleared.remove(&key);
        self.cells.set(r, c, cell);
    }

    pub fn remove(&mut self, r: u32, c: u32) -> Option<Cell> {
        if !self.loading {
            self.edited = true;
        }
        self.cells.remove(r, c)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    /// 按行、列升序遍历所有单元格
    pub(crate) fn cells_mut(&mut self) -> &mut Store {
        &mut self.cells
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.cells.iter()
    }

    pub fn values(&self) -> impl Iterator<Item = &Cell> {
        self.cells.iter_values()
    }

    pub fn style_of(&self, r: u32, c: u32) -> u16 {
        self.styles.get(&CellRef::new(r, c)).copied().unwrap_or(0)
    }

    /// 用户改样式（标记脏，保存时回写）
    pub fn set_style(&mut self, r: u32, c: u32, id: u16) {
        if !self.loading {
            self.edited = true;
            self.styles_dirty = true;
        }
        let key = CellRef::new(r, c);
        if id == 0 {
            self.styles.remove(&key);
        } else {
            self.styles.insert(key, id);
        }
        self.style_edits.insert(key);
    }

    /// 从文件载入样式（不算用户修改）
    pub fn set_style_loaded(&mut self, r: u32, c: u32, id: u16) {
        let key = CellRef::new(r, c);
        if id == 0 {
            self.styles.remove(&key);
        } else {
            self.styles.insert(key, id);
        }
    }

    /// 合并一个矩形区域（与区域内已有合并冲突的先拆掉）
    pub fn merge_range(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) {
        let (r1, r2) = (r1.min(r2), r1.max(r2));
        let (c1, c2) = (c1.min(c2), c1.max(c2));
        if r1 == r2 && c1 == c2 {
            return;
        }
        // 拆掉所有与新区域相交的旧合并
        self.merges
            .retain(|(a, b, cc, d)| *a > r2 || *cc < r1 || *b > c2 || *d < c1);
        self.merges.push((r1, c1, r2, c2));
        self.merges.sort();
        self.merges_dirty = true;
    }

    /// 拆掉与给定区域相交的所有合并
    pub fn unmerge_range(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) {
        let (r1, r2) = (r1.min(r2), r1.max(r2));
        let (c1, c2) = (c1.min(c2), c1.max(c2));
        let before = self.merges.len();
        self.merges
            .retain(|(a, b, cc, d)| *a > r2 || *cc < r1 || *b > c2 || *d < c1);
        if self.merges.len() != before {
            self.merges_dirty = true;
        }
    }

    /// 命中合并区域时返回该区域（含锚点）
    pub fn merge_at(&self, r: u32, c: u32) -> Option<(u32, u32, u32, u32)> {
        self.merges
            .iter()
            .find(|(r1, c1, r2, c2)| r >= *r1 && r <= *r2 && c >= *c1 && c <= *c2)
            .copied()
    }

    /// 已用区域 (r1, c1, r2, c2)，0-based
    pub fn used_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        self.cells.used_bounds()
    }

    /// 单列连续访问（范围求值的快路径）
    pub fn column_span(&self, r1: u32, r2: u32, c: u32) -> Vec<&Cell> {
        self.cells.column_span(r1, r2, c)
    }

    /// 存储层实际占用字节（调试用）
    pub fn allocated_bytes(&self) -> usize {
        self.cells.allocated_bytes()
    }
}

fn next_sheet_identity() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for Sheet {
    fn default() -> Self { Self::new("") }
}

/// Imported pivot definitions are replayed from this immutable baseline.
/// Keeping it independent of the last saved file makes the structural journal
/// repeatable, including undo after a successful save.
#[derive(Clone, Debug, Default)]
pub struct PivotCacheBase {
    pub definitions: Vec<(String, Vec<u8>)>,
    pub sheets: Vec<(u64, String)>,
}

/// Marks pivot attachments produced by this open document. Cloning a save or
/// undo snapshot preserves ownership; opening the file creates a new session,
/// so previously persisted attachments become imported parts.
#[derive(Clone, Debug)]
pub struct PivotSession(String);

impl PivotSession {
    pub fn token(&self) -> &str { &self.0 }
}

impl Default for PivotSession {
    fn default() -> Self {
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_nanos();
        Self(format!("{time:x}-{:x}-{:x}", std::process::id(), next_sheet_identity()))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Workbook {
    /// 文件里是否带 VBA 宏（只保留，不执行）
    pub has_vba: bool,
    /// 自定义名称（名称管理器里的东西）：(名字, 作用表, 公式文本)
    pub names: Vec<(String, Option<usize>, String)>,
    /// 外部数据源（导入的 CSV、跨表取数），刷新用
    pub sources: Vec<crate::external::Source>,
    /// 跨工作簿引用（xl/externalLinks）；`[N]` 里的 N 就是这里的 index - 1
    pub links: Vec<crate::xlinks::ExternalBook>,
    /// 名称被改过
    pub names_dirty: bool,
    /// 结构改动记录：保存时用来搬透视表缓存里的源区域
    /// (表名, 0=行 1=列, at, n, 是否删除)
    pub pivot_shifts: Vec<(String, u8, u32, u32, bool)>,
    pub pivot_cache_base: std::sync::Arc<PivotCacheBase>,
    pub pivot_session: PivotSession,
    /// 保存时要挂上的透视表部件：(表序号, 规格, 结果, 输出矩形, 值字段名)
    pub pivots_pending: Vec<(
        usize,
        crate::pivot::PivotSpec,
        crate::pivot::PivotResult,
        (u32, u32, u32, u32),
        String,
    )>,
    /// 样式表（单元格只存索引，见 Sheet.styles）
    pub styles: crate::style::StyleTable,
    pub sheets: Vec<Sheet>,
    pub path: Option<PathBuf>,
    pub dirty: bool,
}

impl Workbook {
    pub fn new_blank() -> Self {
        Self {
            has_vba: false,
            names: Vec::new(),
            sources: Vec::new(),
            links: Vec::new(),
            pivot_shifts: Vec::new(),
            pivot_cache_base: Default::default(),
            pivot_session: Default::default(),
            names_dirty: false,
            pivots_pending: Vec::new(),
            sheets: vec![Sheet::new("Sheet1")],
            path: None,
            dirty: false,
            styles: crate::style::StyleTable::new(),
        }
    }

    pub fn sheet(&self, idx: usize) -> &Sheet {
        &self.sheets[idx.min(self.sheets.len() - 1)]
    }

    pub fn sheet_mut(&mut self, idx: usize) -> &mut Sheet {
        let i = idx.min(self.sheets.len() - 1);
        &mut self.sheets[i]
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名.xlsx".to_string())
    }
}

/// 0 -> A, 25 -> Z, 26 -> AA
pub fn col_label(mut col: u32) -> String {
    let mut out = String::new();
    loop {
        out.insert(0, (b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out
}

/// "A1" 风格地址
pub fn cell_name(row: u32, col: u32) -> String {
    format!("{}{}", col_label(col), row + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(col_label(0), "A");
        assert_eq!(col_label(25), "Z");
        assert_eq!(col_label(26), "AA");
        assert_eq!(col_label(701), "ZZ");
        assert_eq!(col_label(702), "AAA");
        assert_eq!(cell_name(0, 0), "A1");
        assert_eq!(cell_name(9, 27), "AB10");
    }

    #[test]
    fn parse() {
        assert_eq!(Value::parse_input("42"), Value::Number(42.0));
        assert_eq!(Value::parse_input(" 3.5 "), Value::Number(3.5));
        assert_eq!(Value::parse_input("abc"), Value::Text("abc".into()));
        assert_eq!(Value::parse_input(""), Value::Empty);
        assert_eq!(Value::parse_input("TRUE"), Value::Bool(true));
    }

    #[test]
    fn display_numbers() {
        assert_eq!(Value::Number(42.0).display(), "42");
        assert_eq!(Value::Number(3.5).display(), "3.5");
        assert_eq!(Value::Number(0.1 + 0.2).display(), "0.3");
    }
}

/// XML 属性/文本里的实体还原（够用即可）
pub fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "
")
        .replace("&amp;", "&")
}

/// 解析 XML 实体引用（quick-xml 把 &amp; 单独发成 GeneralRef 事件）
pub fn resolve_entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        _ => {
            if let Some(num) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                if let Ok(v) = u32::from_str_radix(num, 16) {
                    return char::from_u32(v).map(|c| c.to_string()).unwrap_or_default();
                }
            }
            if let Some(num) = name.strip_prefix('#') {
                if let Ok(v) = num.parse::<u32>() {
                    return char::from_u32(v).map(|c| c.to_string()).unwrap_or_default();
                }
            }
            String::new()
        }
    }
}
