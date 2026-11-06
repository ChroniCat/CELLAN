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
    Text(String),
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
            Value::Text(s) => s.clone(),
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
            _ => Value::Text(s.to_string()),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub value: Value,
    /// 公式原文（以 = 开头），v0.1 只保存不计算
    pub formula: Option<String>,
}

impl Cell {
    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub name: String,
    pub cells: HashMap<CellRef, Cell>,
    /// 被清空过的单元格：保存时要真的从文件里删掉，否则旧值会残留
    pub cleared: std::collections::HashSet<CellRef>,
    /// 被动态数组公式溢出占用的单元格（重算前要清掉）
    pub spilled: std::collections::HashSet<CellRef>,
}

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            cells: HashMap::new(),
            cleared: std::collections::HashSet::new(),
            spilled: std::collections::HashSet::new(),
        }
    }

    pub fn get(&self, r: u32, c: u32) -> Option<&Cell> {
        self.cells.get(&CellRef::new(r, c))
    }

    pub fn value(&self, r: u32, c: u32) -> Value {
        self.cells.get(&CellRef::new(r, c)).map(|x| x.value.clone()).unwrap_or_default()
    }

    pub fn set(&mut self, r: u32, c: u32, cell: Cell) {
        let key = CellRef::new(r, c);
        if cell.is_blank() {
            if self.cells.remove(&key).is_some() {
                self.cleared.insert(key);
            }
        } else {
            self.cells.insert(key, cell);
            self.cleared.remove(&key);
        }
    }

    /// 已用区域的最大行列（0-based，含）
    pub fn used_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        let mut it = self.cells.keys().filter(|k| !self.cells[*k].is_blank());
        let first = it.next()?;
        let (mut r1, mut c1, mut r2, mut c2) = (first.row, first.col, first.row, first.col);
        for k in it {
            r1 = r1.min(k.row);
            c1 = c1.min(k.col);
            r2 = r2.max(k.row);
            c2 = c2.max(k.col);
        }
        Some((r1, c1, r2, c2))
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
