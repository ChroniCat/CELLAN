//! 探针：读一个带外部引用的文件，看我们的模型/解析器怎么处理 [1]Sheet!A1。
//!
//! 用法：cargo run -p sheetcore --example ext_probe -- in.xlsx [out.xlsx]

fn main() {
    let mut args = std::env::args().skip(1);
    let inp = args.next().expect("用法: ext_probe <in.xlsx> [out.xlsx]");
    let outp = args.next();
    let mut wb = sheetcore::xlsx::load(std::path::Path::new(&inp)).expect("读入失败");
    println!("外部链接 {} 个", wb.links.len());
    for l in &wb.links {
        println!(
            "  [{}] {} → 表 {:?} 缓存 {} 格{}",
            l.index,
            l.target,
            l.sheets,
            l.cache.len(),
            if l.abs.is_some() { "（有绝对路径）" } else { "" }
        );
    }
    if std::env::var("SC_XLINK_REFRESH").is_ok() {
        let rep = sheetcore::xlinks::refresh(&mut wb);
        println!("刷新：成功 {} 个，失败 {:?}", rep.ok, rep.failed);
    }
    println!("表数={} 表名={:?}", wb.sheets.len(), wb.sheets.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
    for r in 0..6u32 {
        if let Some(c) = wb.sheets[0].get(r, 0) {
            println!("A{} 公式={:?} 值={:?}", r + 1, c.formula, c.value.display());
        }
    }
    sheetcore::formula::recalc_all(&mut wb);
    println!("--- 重算后");
    for r in 0..6u32 {
        if let Some(c) = wb.sheets[0].get(r, 0) {
            println!("A{} = {:?}", r + 1, c.value.display());
        }
    }
    // 单独试几种写法
    for f in [
        "=[1]预算!A1",
        "=SUM([1]预算!A1:A3)",
        "=[ext_plan_src2.xlsx]预算!A1",
        "='[ext_plan_src2.xlsx]预算'!A1",
    ] {
        let v = sheetcore::formula::eval_formula(&wb, 0, f);
        println!("{f} → {:?}", v.display());
    }
    if let Some(out) = outp {
        wb.path = Some(std::path::PathBuf::from(&inp));
        // 模拟"用户改了一格"：让这张表进入重写路径，看外部引用的缓存值会变成什么
        wb.sheets[0].set(
            20,
            20,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Text("probe".into()),
                formula: None,
            },
        );
        sheetcore::xlsx::save(std::path::Path::new(&out), &wb).expect("保存失败");
        println!("已另存 {out}");
    }
}
