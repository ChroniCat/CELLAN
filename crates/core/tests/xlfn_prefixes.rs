//! 保存时把 `_xlfn.` / `_xlws.` / `_xlpm.` 前缀加回去。
//!
//! 加载时 `normalize_formula` 会剥掉这些前缀，求值表里不带前缀的名字才能命中。
//! 但 OOXML 要求 2010 之后新增的函数在文件里带 `_xlfn.`：少了它，Excel 把
//! `XLOOKUP(...)` 当成未知名称，打开就是 `#NAME?`。umya 写盘不会自动加，
//! 所以任何被整表重写的工作表都会把新函数写坏——这里锁住它。

use sheetcore::{add_fn_prefixes, normalize_formula, Cell, Value, Workbook};
use std::io::Read;

#[test]
fn new_functions_get_their_file_prefixes_back() {
    let cases = [
        ("XLOOKUP(A1,B1:B9,C1:C9)", "_xlfn.XLOOKUP(A1,B1:B9,C1:C9)"),
        ("FILTER(A1:A9,B1:B9>0)", "_xlfn._xlws.FILTER(A1:A9,B1:B9>0)"),
        ("SORT(A1:A9)", "_xlfn._xlws.SORT(A1:A9)"),
        ("IFERROR(TEXTJOIN(\",\",TRUE,A1:A3),\"\")", "IFERROR(_xlfn.TEXTJOIN(\",\",TRUE,A1:A3),\"\")"),
        ("SUM(A1:A3)+IFS(A1>0,1,TRUE,2)", "SUM(A1:A3)+_xlfn.IFS(A1>0,1,TRUE,2)"),
        ("STDEV.S(A1:A9)", "_xlfn.STDEV.S(A1:A9)"),
        ("LET(x,A1*2,y,x+1,x*y)", "_xlfn.LET(_xlpm.x,A1*2,_xlpm.y,_xlpm.x+1,_xlpm.x*_xlpm.y)"),
        (
            "LET(f,LAMBDA(v,v*2),f(3))",
            "_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.v,_xlpm.v*2),_xlpm.f(3))",
        ),
        // 老函数、引号里的文字、表名、结构化引用列名都不动
        ("VLOOKUP(A1,B1:C9,2,0)", "VLOOKUP(A1,B1:C9,2,0)"),
        ("\"XLOOKUP(1)\"&A1", "\"XLOOKUP(1)\"&A1"),
        ("'XLOOKUP'!A1+SUM(Sales[UNIQUE])", "'XLOOKUP'!A1+SUM(Sales[UNIQUE])"),
    ];
    for (model, file) in cases {
        assert_eq!(add_fn_prefixes(model), file, "保存形式：{model}");
        // 往返：文件形式读回来要还原成模型形式
        assert_eq!(normalize_formula(file), model, "读回形式：{file}");
    }
    // 只看保存形式的情况（整列引用在加载时会被展开成带行号的区域，不做逐字往返）
    for (model, file) in [
        // 已带前缀的不重复加
        ("_xlfn.XLOOKUP(1,A1:A2,B1:B2)", "_xlfn.XLOOKUP(1,A1:A2,B1:B2)"),
        ("VLOOKUP(A1,B:C,2,0)", "VLOOKUP(A1,B:C,2,0)"),
        // 名为 x 的参数不能误伤 X 列整列引用
        ("LET(x,1,SUM(x:x)+x)", "_xlfn.LET(_xlpm.x,1,SUM(x:x)+_xlpm.x)"),
    ] {
        assert_eq!(add_fn_prefixes(model), file, "保存形式：{model}");
    }
}

#[test]
fn prefix_table_is_sorted_for_binary_search() {
    // add_fn_prefixes 用二分查找；名单乱序会让部分函数漏加前缀
    let probe = ["ACOT", "BINOM.DIST.RANGE", "CONCAT", "FORECAST.ETS.STAT", "T.DIST.2T", "Z.TEST"];
    for name in probe {
        assert!(
            add_fn_prefixes(&format!("{name}(1)")).starts_with("_xlfn."),
            "{name} 应带 _xlfn. 前缀"
        );
    }
}

#[test]
fn saved_sheet_keeps_new_functions_readable_by_excel() {
    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    for r in 0..3u32 {
        sh.set(r, 0, Cell { value: Value::Number(r as f64 + 1.0), formula: None });
        sh.set(r, 1, Cell { value: Value::Text(format!("k{r}").into()), formula: None });
    }
    let formulas = [
        "=XLOOKUP(2,A1:A3,B1:B3)",
        "=TEXTJOIN(\",\",TRUE,B1:B3)",
        "=LET(x,A1*10,x+1)",
        "=IFS(A1>5,\"big\",TRUE,\"small\")",
    ];
    for (i, f) in formulas.iter().enumerate() {
        sh.set(i as u32, 4, Cell { value: Value::Empty, formula: Some((*f).into()) });
    }
    wb.dirty = true;

    let dir = std::env::temp_dir().join(format!("sheetcore-xlfn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("xlfn.xlsx");
    sheetcore::xlsx::save(&path, &wb).expect("save");

    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    zip.by_name("xl/worksheets/sheet1.xml").unwrap().read_to_string(&mut xml).unwrap();
    for needle in ["_xlfn.XLOOKUP(", "_xlfn.TEXTJOIN(", "_xlfn.LET(_xlpm.x,", "_xlfn.IFS("] {
        assert!(xml.contains(needle), "工作表 XML 缺少 {needle}：{xml}");
    }

    let back = sheetcore::xlsx::load(&path).expect("reload");
    let got: Vec<String> = (0..formulas.len() as u32)
        .map(|r| back.sheets[0].get(r, 4).and_then(|c| c.formula.clone()).unwrap_or_default().to_string())
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    for (want, got) in formulas.iter().zip(&got) {
        assert_eq!(got, want, "读回的公式应还原成不带前缀的模型形式");
    }
}
