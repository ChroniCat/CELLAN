pub mod formula;
pub mod cf;
pub mod chart_write;
pub mod charts;
pub mod cfb;
pub mod vba_src;
pub mod arr;
pub mod backup;
pub mod pic;
pub mod sparse;
pub mod table;
pub mod data_tools;
pub mod external;
pub mod csv;
pub mod fixups;
pub mod dv;
pub mod layout;
pub mod merges;
pub mod notes;
pub mod page;
pub mod patch;
pub mod pdf;
pub mod print;
pub mod pivot;
pub mod refs;
pub mod pivot_xml;
pub mod qtable;
pub mod model;
pub mod style;
pub mod sort_filter;
pub mod store;
pub mod structural;
pub mod undo;
pub mod vba;
pub mod wire;
pub mod sync;
pub mod xlsx;

pub use model::*;

/// OOXML 把新函数写成 _xlfn.XLOOKUP、_xlfn._xlws.FILTER，LAMBDA 参数写成 _xlpm.x。
/// 我们的函数表里没有这些前缀，统一剥掉。
pub fn normalize_formula(text: &str) -> String {
    if !text.contains("_xl") {
        return text.to_string();
    }
    text.replace("_xlfn._xlws.", "")
        .replace("_xlfn.", "")
        .replace("_xlws.", "")
        .replace("_xlpm.", "")
        .replace("_xludf.", "")
}
