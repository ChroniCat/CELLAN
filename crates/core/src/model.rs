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

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub name: String,
    /// 行号 → 该行的单元格，按列号升序存放。
    /// 每格只有 4（列号）+ 40（Cell）字节，没有哈希槽位开销。
    rows: HashMap<u32, Vec<(u32, Cell)>>,
    /// 被清空过的单元格：保存时要真的从文件里删掉，否则旧值会残留
    pub cleared: std::collections::HashSet<CellRef>,
    /// 被动态数组公式溢出占用的单元格（重算前要清掉）
    pub spilled: std::collections::HashSet<CellRef>,
}

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            rows: HashMap::new(),
            cleared: std::collections::HashSet::new(),
            spilled: std::collections::HashSet::new(),
        }
    }

    #[inline]
    fn row_get(row: &[(u32, Cell)], c: u32) -> Option<usize> {
        row.binary_search_by_key(&c, |(cc, _)| *cc).ok()
    }

    pub fn get(&self, r: u32, c: u32) -> Option<&Cell> {
        let row = self.rows.get(&r)?;
        Self::row_get(row, c).map(|i| &row[i].1)
    }

    pub fn get_mut(&mut self, r: u32, c: u32) -> Option<&mut Cell> {
        let row = self.rows.get_mut(&r)?;
        let i = Self::row_get(row, c)?;
        Some(&mut row[i].1)
    }

    pub fn value(&self, r: u32, c: u32) -> Value {
        self.get(r, c).map(|x| x.value.clone()).unwrap_or_default()
    }

    pub fn contains(&self, r: u32, c: u32) -> bool {
        self.get(r, c).is_some()
    }

    pub fn set(&mut self, r: u32, c: u32, cell: Cell) {
        let key = CellRef::new(r, c);
        if cell.is_blank() {
            if self.remove(r, c).is_some() {
                self.cleared.insert(key);
            }
            return;
        }
        self.cleared.remove(&key);
        let row = self.rows.entry(r).or_default();
        match Self::row_get(row, c) {
            Some(i) => row[i].1 = cell,
            None => {
                let pos = row.partition_point(|(cc, _)| *cc < c);
                row.insert(pos, (c, cell));
            }
        }
    }

    pub fn remove(&mut self, r: u32, c: u32) -> Option<Cell> {
        let row = self.rows.get_mut(&r)?;
        let i = Self::row_get(row, c)?;
        let cell = row.remove(i).1;
        if row.is_empty() {
            self.rows.remove(&r);
        }
        Some(cell)
    }

    pub fn len(&self) -> usize {
        self.rows.values().map(|r| r.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        self.rows.clear();
    }

    /// 按行、列升序遍历所有单元格
    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        let mut rows: Vec<(&u32, &Vec<(u32, Cell)>)> = self.rows.iter().collect();
        rows.sort_by_key(|(r, _)| **r);
        rows.into_iter().flat_map(|(r, row)| {
            row.iter().map(move |(c, cell)| (CellRef::new(*r, *c), cell))
        })
    }

    /// 只需要单元格引用时用这个（避免每行一次 Vec 分配）
    pub fn values(&self) -> impl Iterator<Item = &Cell> {
        self.rows.values().flat_map(|row| row.iter().map(|(_, c)| c))
    }

    /// 已用区域的最大行列（0-based，含）
    pub fn used_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        let mut r1 = u32::MAX;
        let mut c1 = u32::MAX;
        let mut r2 = 0u32;
        let mut c2 = 0u32;
        let mut any = false;
        for (r, row) in self.rows.iter() {
            for (c, cell) in row.iter() {
                if cell.is_blank() {
                    continue;
                }
                any = true;
                r1 = r1.min(*r);
                r2 = r2.max(*r);
                c1 = c1.min(*c);
                c2 = c2.max(*c);
            }
        }
        if any {
            Some((r1, c1, r2, c2))
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub path: Option<PathBuf>,
    pub dirty: bool,
}

impl Workbook {
    pub fn new_blank() -> Self {
        Self { sheets: vec![Sheet::new("Sheet1")], path: None, dirty: false }
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
