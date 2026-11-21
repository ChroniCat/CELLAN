//! 端到端：数组常量 / 定义名称 / 跨簿引用 / 2007+ 函数写进 .xls，交给 Excel 读回公式文本。
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::xls_formula::BookSheets;

fn c(v: Value, f: Option<&str>) -> Cell {
    Cell {
        value: v,
        formula: f.map(|s| s.into()),
    }
}

fn main() -> anyhow::Result<()> {
    let mut wb = Workbook::new_blank();
    let mut s = Sheet::new("S");
    let mut t = Sheet::new("T");
    t.set(0, 0, c(Value::Number(7.0), None));
    s.set(0, 1, c(Value::Number(1.0), None));
    s.set(1, 1, c(Value::Number(2.0), None));

    let only = std::env::var("SC_ONLY").unwrap_or_else(|_| "all".into());
    let want = |k: &str| only == "all" || only == k;
    let mut row = 0u32;
    let mut put = |s: &mut Sheet, f: &str, v: Value| {
        s.set(row, 0, c(v, Some(f)));
        row += 1;
    };
    if want("array") {
        put(&mut s, "=SUM({1,2,3})", Value::Number(6.0));
        put(&mut s, "=SUM({1,2;3,4})", Value::Number(10.0));
    }
    if want("name") {
        put(&mut s, "=Total*2", Value::Number(6.0));
    }
    if want("book") {
        put(&mut s, "=[1]Sheet1!A1", Value::Number(42.0));
    }
    if want("future") {
        put(&mut s, "=IFERROR(1/0,\"err\")", Value::Text("err".into()));
        put(&mut s, "=XLOOKUP(2,B1:B2,B1:B2)", Value::Number(2.0));
    }
    if want("classic") {
        put(&mut s, "=ADDRESS(1,1)", Value::Text("$A$1".into()));
        put(&mut s, "=CHOOSE(1,10,20)", Value::Number(10.0));
        put(&mut s, "=RANK(1,B1:B2)", Value::Number(2.0));
        put(&mut s, "=T!A1-5", Value::Number(2.0));
    }

    wb.sheets = vec![s, t];
    if want("name") {
        wb.names = vec![("Total".into(), None, "S!$B$1:$B$2".into())];
    }
    if want("book") {
        wb.links = vec![sheetcore::xlinks::ExternalBook {
        index: 1,
        part: "xl/externalLinks/externalLink1.xml".into(),
        target: "book_b.xls".into(),
        abs: None,
        sheets: vec!["Sheet1".into()],
        defined_names: Default::default(),
        cache: Default::default(),
        refreshed: false,
    }];
    }

    // 只为编译期知道外部簿表名；写盘时从 wb.links 重建
    let books = vec![BookSheets {
        file: "book_b.xls".into(),
        sheets: vec!["Sheet1".into()],
    }];
    let _ = books;

    let out = format!("../work/adv_{only}.xls");
    sheetcore::xls_write::write(std::path::Path::new(&out), &wb)?;
    println!("ok {out}");
    Ok(())
}
