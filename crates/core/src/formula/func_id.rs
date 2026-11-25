//! Compact interned `FuncId` enum and zero-allocation `FuncName` wrapper for built-in functions.
//!
//! Replaces ~32 sequential string checks in `Calc::eval_node` and ~174 string match arms in
//! `functions::call_or_else_in` with single-instruction jump tables while keeping external
//! `Node::Func(name, args)` call sites (`xlinks.rs`, `xls_formula.rs`) 100% source-compatible.

use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;

macro_rules! define_func_ids {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u16)]
        pub(crate) enum FuncId {
            $($variant,)*
            Other,
        }

        impl FuncId {
            #[inline]
            pub(crate) fn from_upper(s: &str) -> Self {
                match s {
                    $($name => Self::$variant,)*
                    _ => Self::Other,
                }
            }

            #[inline]
            pub(crate) fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                    Self::Other => "",
                }
            }
        }
    };
}

define_func_ids! {
    // Special forms handled directly in Calc::eval_node
    If => "IF",
    IfError => "IFERROR",
    IfNa => "IFNA",
    Ifs => "IFS",
    Switch => "SWITCH",
    Let => "LET",
    Lambda => "LAMBDA",
    InternalCall => "__CALL__",
    IsOmitted => "ISOMITTED",
    IsFormula => "ISFORMULA",
    FormulaText => "FORMULATEXT",
    Sheet => "SHEET",
    Sheets => "SHEETS",
    Map => "MAP",
    Reduce => "REDUCE",
    Scan => "SCAN",
    ByRow => "BYROW",
    ByCol => "BYCOL",
    MakeArray => "MAKEARRAY",
    Cell => "CELL",
    Areas => "AREAS",
    Info => "INFO",
    Row => "ROW",
    Column => "COLUMN",
    Rows => "ROWS",
    Columns => "COLUMNS",
    Indirect => "INDIRECT",
    Offset => "OFFSET",
    Address => "ADDRESS",
    Index => "INDEX",
    VLookup => "VLOOKUP",
    HLookup => "HLOOKUP",
    Match => "MATCH",
    XMatch => "XMATCH",
    XLookup => "XLOOKUP",
    Lookup => "LOOKUP",
    Choose => "CHOOSE",

    // Aggregation / Criteria / Volatility / Visibility
    Sum => "SUM",
    SumIf => "SUMIF",
    SumIfs => "SUMIFS",
    Count => "COUNT",
    CountA => "COUNTA",
    CountBlank => "COUNTBLANK",
    CountIf => "COUNTIF",
    CountIfs => "COUNTIFS",
    Average => "AVERAGE",
    AverageA => "AVERAGEA",
    AverageIf => "AVERAGEIF",
    AverageIfs => "AVERAGEIFS",
    Max => "MAX",
    Min => "MIN",
    MaxA => "MAXA",
    MinA => "MINA",
    MaxIfs => "MAXIFS",
    MinIfs => "MINIFS",
    Subtotal => "SUBTOTAL",
    Aggregate => "AGGREGATE",
    Rand => "RAND",
    RandBetween => "RANDBETWEEN",
    RandArray => "RANDARRAY",
    Now => "NOW",
    Today => "TODAY",

    // Math / Trig
    Product => "PRODUCT",
    Abs => "ABS",
    Sign => "SIGN",
    Int => "INT",
    Trunc => "TRUNC",
    Round => "ROUND",
    RoundUp => "ROUNDUP",
    RoundDown => "ROUNDDOWN",
    Mod => "MOD",
    Quotient => "QUOTIENT",
    Power => "POWER",
    Sqrt => "SQRT",
    SqrtPi => "SQRTPI",
    Exp => "EXP",
    Ln => "LN",
    Log10 => "LOG10",
    Log => "LOG",
    Pi => "PI",
    Sin => "SIN",
    Cos => "COS",
    Tan => "TAN",
    Asin => "ASIN",
    Acos => "ACOS",
    Atan => "ATAN",
    Atan2 => "ATAN2",
    Sinh => "SINH",
    Cosh => "COSH",
    Tanh => "TANH",
    Degrees => "DEGREES",
    Radians => "RADIANS",
    Fact => "FACT",
    Gcd => "GCD",
    Lcm => "LCM",
    Combin => "COMBIN",
    Ceiling => "CEILING",
    CeilingMath => "CEILING.MATH",
    Floor => "FLOOR",
    FloorMath => "FLOOR.MATH",
    MRound => "MROUND",
    Even => "EVEN",
    Odd => "ODD",
    SumSq => "SUMSQ",
    SumProduct => "SUMPRODUCT",

    // Statistics
    Median => "MEDIAN",
    Mode => "MODE",
    Large => "LARGE",
    Small => "SMALL",
    Rank => "RANK",
    RankEq => "RANK.EQ",
    Stdev => "STDEV",
    StdevS => "STDEV.S",
    StdevP => "STDEVP",
    StdevDotP => "STDEV.P",
    Var => "VAR",
    VarS => "VAR.S",
    VarP => "VARP",
    VarDotP => "VAR.P",
    AveDev => "AVEDEV",
    DevSq => "DEVSQ",
    GeoMean => "GEOMEAN",
    HarMean => "HARMEAN",
    Percentile => "PERCENTILE",
    PercentileInc => "PERCENTILE.INC",
    Quartile => "QUARTILE",
    QuartileInc => "QUARTILE.INC",
    Correl => "CORREL",
    Slope => "SLOPE",
    Intercept => "INTERCEPT",

    // Text
    Len => "LEN",
    Left => "LEFT",
    Right => "RIGHT",
    Mid => "MID",
    Lower => "LOWER",
    Upper => "UPPER",
    Proper => "PROPER",
    Trim => "TRIM",
    Concat => "CONCAT",
    Concatenate => "CONCATENATE",
    TextJoin => "TEXTJOIN",
    Rept => "REPT",
    Substitute => "SUBSTITUTE",
    Replace => "REPLACE",
    Find => "FIND",
    Search => "SEARCH",
    Exact => "EXACT",
    Char => "CHAR",
    Code => "CODE",
    Value => "VALUE",
    Text => "TEXT",
    T => "T",

    // Logic
    True => "TRUE",
    False => "FALSE",
    Not => "NOT",
    And => "AND",
    Or => "OR",
    Xor => "XOR",

    // Date & Time
    Date => "DATE",
    Time => "TIME",
    Year => "YEAR",
    Month => "MONTH",
    Day => "DAY",
    Hour => "HOUR",
    Minute => "MINUTE",
    Second => "SECOND",
    Weekday => "WEEKDAY",
    EDate => "EDATE",
    EOMonth => "EOMONTH",
    Days => "DAYS",
    DateDif => "DATEDIF",
    WeekNum => "WEEKNUM",
    NetworkDays => "NETWORKDAYS",
    Workday => "WORKDAY",
    YearFrac => "YEARFRAC",

    // Financial
    Pmt => "PMT",
    Fv => "FV",
    Pv => "PV",
    Nper => "NPER",
    Ipmt => "IPMT",
    Ppmt => "PPMT",
    Npv => "NPV",
    Sln => "SLN",
    Syd => "SYD",

    // Information
    IsBlank => "ISBLANK",
    IsNumber => "ISNUMBER",
    IsText => "ISTEXT",
    IsNonText => "ISNONTEXT",
    IsLogical => "ISLOGICAL",
    IsEven => "ISEVEN",
    IsOdd => "ISODD",
    IsError => "ISERROR",
    IsErr => "ISERR",
    IsNa => "ISNA",
    IsRef => "ISREF",
    Na => "NA",
    N => "N",
    Type => "TYPE",
    ErrorType => "ERROR.TYPE",
}

impl FuncId {
    #[inline]
    pub(crate) fn is_volatile(self) -> bool {
        matches!(
            self,
            Self::Now | Self::Today | Self::Rand | Self::RandBetween | Self::RandArray
        )
    }

    /// True only for RNG volatile functions (`RAND`, `RANDBETWEEN`, `RANDARRAY`) that need
    /// per-pass memoization in `Calc::volatile_values` and cannot safely run concurrently.
    /// `TODAY()` and `NOW()` read an immutable `EvalCtx::now_snapshot` and are 100% parallel-safe.
    #[inline]
    pub(crate) fn is_nondeterministic_volatile(self) -> bool {
        matches!(self, Self::Rand | Self::RandBetween | Self::RandArray)
    }

    #[inline]
    pub(crate) fn is_volatile_ref(self) -> bool {
        matches!(self, Self::Indirect | Self::Offset)
    }

    #[inline]
    pub(crate) fn is_visibility(self) -> bool {
        matches!(self, Self::Subtotal | Self::Aggregate)
    }

    #[inline]
    pub(crate) fn is_position_or_layout(self) -> bool {
        matches!(
            self,
            Self::Row
                | Self::Column
                | Self::Rows
                | Self::Columns
                | Self::Areas
                | Self::Address
                | Self::Cell
                | Self::Info
                | Self::Offset
                | Self::Indirect
                | Self::FormulaText
                | Self::IsFormula
                | Self::Subtotal
                | Self::Aggregate
                | Self::Sheet
                | Self::Sheets
        )
    }

    #[inline]
    pub(crate) fn is_known_builtin(self) -> bool {
        !matches!(self, Self::Other | Self::InternalCall)
    }
}

/// Function name stored inside `Node::Func`.
///
/// Built-in functions occupy zero heap memory (`extra: None`) and dispatch via `FuncId`.
/// Unknown/extra/user-defined names store an uppercase `Box<str>`.
#[derive(Clone)]
pub struct FuncName {
    pub(crate) id: FuncId,
    extra: Option<Box<str>>,
}

impl FuncName {
    #[inline]
    pub(crate) fn from_upper(s: &str) -> Self {
        let id = FuncId::from_upper(s);
        if id != FuncId::Other {
            Self { id, extra: None }
        } else {
            Self {
                id: FuncId::Other,
                extra: Some(s.into()),
            }
        }
    }

    #[inline]
    pub(crate) fn from_raw(s: &str) -> Self {
        if !s.bytes().any(|b| b.is_ascii_lowercase()) {
            Self::from_upper(s)
        } else {
            let up = s.to_ascii_uppercase();
            Self::from_upper(&up)
        }
    }

    #[inline]
    pub(crate) const fn from_id(id: FuncId) -> Self {
        Self { id, extra: None }
    }

    #[inline]
    pub(crate) fn id(&self) -> FuncId {
        self.id
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.extra {
            Some(s) => s,
            None => self.id.as_str(),
        }
    }
}

impl Deref for FuncName {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for FuncName {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for FuncName {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for FuncName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for FuncName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for FuncName {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        if self.id != other.id {
            return false;
        }
        if self.id != FuncId::Other {
            true
        } else {
            self.extra == other.extra
        }
    }
}

impl Eq for FuncName {}

impl PartialEq<str> for FuncName {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for FuncName {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for FuncName {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Hash for FuncName {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl From<&str> for FuncName {
    #[inline]
    fn from(s: &str) -> Self {
        Self::from_raw(s)
    }
}

impl From<String> for FuncName {
    #[inline]
    fn from(s: String) -> Self {
        let id = FuncId::from_upper(&s);
        if id != FuncId::Other {
            Self { id, extra: None }
        } else if !s.bytes().any(|b| b.is_ascii_lowercase()) {
            Self {
                id: FuncId::Other,
                extra: Some(s.into_boxed_str()),
            }
        } else {
            let up = s.to_ascii_uppercase();
            Self::from_upper(&up)
        }
    }
}
