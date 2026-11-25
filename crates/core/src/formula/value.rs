//! 值类型与错误值（Excel 语义）

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrKind {
    Null,      // #NULL!
    Div0,      // #DIV/0!
    Value,     // #VALUE!
    Ref,       // #REF!
    Name,      // #NAME?
    Num,       // #NUM!
    Na,        // #N/A
    Circular,  // 循环引用
    Spill,     // #SPILL!（溢出区域被占用）
}

impl ErrKind {
    pub fn text(&self) -> &'static str {
        match self {
            ErrKind::Null => "#NULL!",
            ErrKind::Div0 => "#DIV/0!",
            ErrKind::Value => "#VALUE!",
            ErrKind::Ref => "#REF!",
            ErrKind::Name => "#NAME?",
            ErrKind::Num => "#NUM!",
            ErrKind::Na => "#N/A",
            ErrKind::Circular => "#REF!",
            ErrKind::Spill => "#SPILL!",
        }
    }
}

impl ErrKind {
    /// 从 `#DIV/0!` 之类的文本还原错误种类（读文件、读修订日志都要用）。
    pub fn from_text(s: &str) -> Option<ErrKind> {
        Some(match s.trim().to_ascii_uppercase().as_str() {
            "#NULL!" => ErrKind::Null,
            "#DIV/0!" => ErrKind::Div0,
            "#VALUE!" => ErrKind::Value,
            "#REF!" => ErrKind::Ref,
            "#NAME?" => ErrKind::Name,
            "#NUM!" => ErrKind::Num,
            "#N/A" => ErrKind::Na,
            "#SPILL!" => ErrKind::Spill,
            _ => return None,
        })
    }
}

impl fmt::Display for ErrKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// 二维数组（动态数组函数的返回值，Arc 共享支持多线程零拷贝传递）
#[derive(Clone, Debug, PartialEq)]
pub struct Array {
    pub rows: usize,
    pub cols: usize,
    pub data: std::sync::Arc<Vec<FValue>>,
}

impl Array {
    pub fn new(rows: usize, cols: usize, data: Vec<FValue>) -> Self {
        Self::from_shared(rows, cols, std::sync::Arc::new(data))
    }
    pub(super) fn from_shared(rows: usize, cols: usize, data: std::sync::Arc<Vec<FValue>>) -> Self {
        Self { rows, cols, data }
    }
    pub fn get(&self, r: usize, c: usize) -> FValue {
        self.data.get(r * self.cols + c).cloned().unwrap_or(FValue::Empty)
    }
    pub fn from_rows(rows: Vec<Vec<FValue>>) -> Self {
        let r = rows.len();
        let c = rows.first().map(|x| x.len()).unwrap_or(0);
        let mut data = Vec::with_capacity(r * c);
        for row in rows {
            for i in 0..c {
                data.push(row.get(i).cloned().unwrap_or(FValue::Empty));
            }
        }
        Self::new(r, c, data)
    }
}

/// 词法环境帧：<=4 个局部变量（99% 的 LAMBDA/LET 调用）直接在栈上内联，零 HashMap 分配
#[derive(Clone, Debug, PartialEq)]
pub enum EnvFrame {
    Small {
        len: u8,
        slots: [(std::sync::Arc<str>, FValue); 4],
    },
    Map(std::collections::HashMap<String, FValue>),
}

impl Default for EnvFrame {
    fn default() -> Self {
        Self::new_small()
    }
}

impl EnvFrame {
    #[inline]
    pub fn new_small() -> Self {
        let empty: std::sync::Arc<str> = "".into();
        Self::Small {
            len: 0,
            slots: [
                (empty.clone(), FValue::Empty),
                (empty.clone(), FValue::Empty),
                (empty.clone(), FValue::Empty),
                (empty, FValue::Empty),
            ],
        }
    }

    #[inline]
    pub fn with_capacity(cap: usize) -> Self {
        if cap <= 4 {
            Self::new_small()
        } else {
            Self::Map(std::collections::HashMap::with_capacity(cap))
        }
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<&FValue> {
        match self {
            Self::Small { len, slots } => {
                for i in (0..*len as usize).rev() {
                    if slots[i].0.as_ref() == name || slots[i].0.eq_ignore_ascii_case(name) {
                        return Some(&slots[i].1);
                    }
                }
                None
            }
            Self::Map(m) => m.get(name).or_else(|| {
                m.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v)
            }),
        }
    }

    #[inline]
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    #[inline]
    pub fn insert_arc(&mut self, name: std::sync::Arc<str>, val: FValue) {
        self.insert(name, val);
    }

    #[inline]
    pub fn insert(&mut self, name: impl Into<std::sync::Arc<str>>, val: FValue) {
        let name = name.into();
        match self {
            Self::Small { len, slots } => {
                for i in 0..*len as usize {
                    if slots[i].0.as_ref() == name.as_ref()
                        || slots[i].0.eq_ignore_ascii_case(name.as_ref())
                    {
                        slots[i].1 = val;
                        return;
                    }
                }
                if (*len as usize) < 4 {
                    slots[*len as usize] = (name, val);
                    *len += 1;
                } else {
                    let mut map = std::collections::HashMap::with_capacity(8);
                    for i in 0..4 {
                        let (k, v) = std::mem::replace(
                            &mut slots[i],
                            ("".into(), FValue::Empty),
                        );
                        map.insert(k.to_string(), v);
                    }
                    map.insert(name.to_string(), val);
                    *self = Self::Map(map);
                }
            }
            Self::Map(m) => {
                m.insert(name.to_string(), val);
            }
        }
    }
}

/// 一等公民的 LAMBDA：LET(f, LAMBDA(x, x+1), f(1)) 需要把函数当值传
#[derive(Clone, Debug, PartialEq)]
pub struct LambdaVal {
    pub params: Vec<std::sync::Arc<str>>,
    pub body: crate::formula::Node,
    /// 定义时捕获的环境（词法闭包：内层 LAMBDA 要能看见外层的参数）
    pub env: Vec<EnvFrame>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FValue {
    Empty,
    Number(f64),
    Text(std::sync::Arc<str>),
    Bool(bool),
    Error(ErrKind),
    Array(std::sync::Arc<Array>),
    Lambda(std::sync::Arc<LambdaVal>),
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FValue>();
};

impl FValue {
    #[inline]
    pub fn text(s: impl Into<std::sync::Arc<str>>) -> Self {
        FValue::Text(s.into())
    }

    pub fn number(n: f64) -> Self {
        if n.is_nan() || n.is_infinite() {
            FValue::Error(ErrKind::Num)
        } else {
            FValue::Number(n)
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, FValue::Error(_))
    }

    /// 数组 → 左上角元素（隐式交集）
    pub fn scalar(&self) -> FValue {
        match self {
            FValue::Array(a) => a.get(0, 0),
            other => other.clone(),
        }
    }

    pub fn as_array(&self) -> Option<std::sync::Arc<Array>> {
        match self {
            FValue::Array(a) => Some(a.clone()),
            _ => None,
        }
    }

    pub fn err(e: ErrKind) -> Self {
        FValue::Error(e)
    }

    /// 显示文本（与界面一致）
    pub fn display(&self) -> String {
        match self {
            FValue::Array(a) => a.get(0, 0).display(),
            FValue::Empty => String::new(),
            FValue::Number(n) => crate::model::format_number(*n),
            FValue::Text(s) => s.to_string(),
            FValue::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            FValue::Error(e) => e.text().to_string(),
            FValue::Lambda(_) => "#FUNC!".into(),
        }
    }

    /// 标量 → 数字（Excel 的强制转换：文本可转就转，布尔 TRUE=1）
    pub fn to_number(&self) -> Result<f64, ErrKind> {
        match self {
            FValue::Array(a) => a.get(0, 0).to_number(),
            FValue::Empty => Ok(0.0),
            FValue::Number(n) => Ok(*n),
            FValue::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            FValue::Text(s) => {
                let t = s.trim();
                if t.is_empty() {
                    return Err(ErrKind::Value);
                }
                t.parse::<f64>().map_err(|_| ErrKind::Value)
            }
            FValue::Error(e) => Err(*e),
            FValue::Lambda(_) => Err(ErrKind::Value),
        }
    }

    /// 标量 → 文本
    pub fn to_text(&self) -> Result<String, ErrKind> {
        match self {
            FValue::Array(a) => a.get(0, 0).to_text(),
            FValue::Error(e) => Err(*e),
            other => Ok(other.display()),
        }
    }

    /// 逻辑判断（数字 0=false，非 0=true；文本 TRUE/FALSE）
    pub fn to_bool(&self) -> Result<bool, ErrKind> {
        match self {
            FValue::Array(a) => a.get(0, 0).to_bool(),
            FValue::Empty => Ok(false),
            FValue::Bool(b) => Ok(*b),
            FValue::Number(n) => Ok(*n != 0.0),
            FValue::Text(s) => {
                let t = s.trim();
                if t.eq_ignore_ascii_case("TRUE") {
                    Ok(true)
                } else if t.eq_ignore_ascii_case("FALSE") {
                    Ok(false)
                } else {
                    Err(ErrKind::Value)
                }
            }
            FValue::Error(e) => Err(*e),
            FValue::Lambda(_) => Err(ErrKind::Value),
        }
    }

    /// 比较：返回 -1/0/1；类型不同按 Excel 规则（数字 < 文本 < 布尔）
    pub fn compare(&self, other: &FValue) -> Result<std::cmp::Ordering, ErrKind> {
        use std::cmp::Ordering;
        // Excel 的跨类型排序：数字 < 文本 < 布尔。空引用不参与排序，
        // 先按对方的类型强制成 0 / "" / FALSE（=A1=0、=A1=""、=A1=FALSE 都为 TRUE）。
        fn rank(v: &FValue) -> u8 {
            match v {
                FValue::Empty => 0,
                FValue::Number(_) => 1,
                FValue::Text(_) => 2,
                FValue::Bool(_) => 3,
                FValue::Error(_) => 4,
                FValue::Array(_) => 5,
                FValue::Lambda(_) => 6,
            }
        }
        Ok(match (self, other) {
            (FValue::Error(e), _) | (_, FValue::Error(e)) => return Err(*e),
            (FValue::Number(x), FValue::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (FValue::Text(x), FValue::Text(y)) => compare_text(x, y),
            (FValue::Bool(x), FValue::Bool(y)) => x.cmp(y),
            (FValue::Empty, FValue::Number(n)) => 0.0_f64.partial_cmp(n).unwrap_or(Ordering::Equal),
            (FValue::Empty, FValue::Text(s)) => "".cmp(s.as_ref()),
            (FValue::Empty, FValue::Bool(b)) => false.cmp(b),
            (_, FValue::Empty) if !matches!(self, FValue::Empty) => return other.compare(self).map(Ordering::reverse),
            (FValue::Empty, _) => Ordering::Equal,
            _ => rank(self).cmp(&rank(other)),
        })
    }
}

/// Preserve Unicode lowercase ordering (including context-sensitive final sigma).
/// The common ASCII path compares borrowed bytes with no temporary strings.
pub(super) fn compare_text(a: &str, b: &str) -> std::cmp::Ordering {
    if a.is_ascii() && b.is_ascii() {
        a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
    } else if a.contains('Σ') || b.contains('Σ') {
        // Rust's string-level casing handles contextual Greek final sigma.
        a.to_lowercase().cmp(&b.to_lowercase())
    } else {
        a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
    }
}

/// 字符数→显示宽度无关的截断（供 UI 用）
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
