//! 插入函数向导的类别筛选：分类表要覆盖绝大多数已实现函数，且常见函数不能归错。



use sheetcore::formula::fncat;



#[test]

fn fncat_spot_checks() {

    assert_eq!(fncat::category("SUM"), "数学与三角函数");

    assert_eq!(fncat::category("ABS"), "数学与三角函数");

    assert_eq!(fncat::category("AVERAGE"), "统计");

    assert_eq!(fncat::category("VLOOKUP"), "查找与引用");

    assert_eq!(fncat::category("TEXT"), "文本");

    assert_eq!(fncat::category("IF"), "逻辑");

    assert_eq!(fncat::category("TODAY"), "日期与时间");

    assert_eq!(fncat::category("PMT"), "财务");

    assert_eq!(fncat::category("ISBLANK"), "信息");

    assert_eq!(fncat::category("IMABS"), "工程");

    assert_eq!(fncat::category("DSUM"), "数据库");

    assert_eq!(fncat::category("SUMIF"), "数学与三角函数");

    // 大小写不敏感

    assert_eq!(fncat::category("vlookup"), "查找与引用");

    // 未知函数进「其他」，不 panic

    assert_eq!(fncat::category("NO_SUCH_FN"), "其他");

}



#[test]

fn fncat_covers_most_implemented_functions() {

    let all = sheetcore::formula::functions::implemented();

    let other = all

        .iter()

        .filter(|n| fncat::category(n) == "其他")

        .count();

    let names: Vec<&&str> = all.iter().filter(|n| fncat::category(n) == "其他").collect();

    eprintln!("[fncat] total={} other={} :: {:?}", all.len(), other, names);

    assert!(!all.is_empty());

    // 阈值按实测留一点余量：分类靠源码段落注释 + 前缀规则，不追求 100%

    assert!(

        other * 100 <= all.len() * 25,

        "「其他」占比过高：{other}/{}",

        all.len()

    );

}

