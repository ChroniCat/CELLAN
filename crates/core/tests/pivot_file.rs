//! 数据透视表：从真实文件算、写回、存盘
use sheetcore::pivot::{Agg, PivotSpec};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/qa02_pivot_pdf")
        .join(name)
}

fn out_path(name: &str, ext: &str) -> PathBuf {
    std::env::current_exe()
        .expect("test executable path")
        .parent()
        .expect("test executable directory")
        .join(format!("qa02_pivot_{}_{}.{}", name, std::process::id(), ext))
}

fn load() -> Option<sheetcore::model::Workbook> {
    let p = fixture("table.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    Some(sheetcore::xlsx::load(&p).expect("读入"))
}

#[test]
fn pivot_by_department() {
    let Some(mut wb) = load() else { return };
    // 表格：A 姓名 / B 部门 / C 金额，5 行数据（合成样本见 scripts/qa02_pivot_pdf.py）
    let region = sheetcore::sort_filter::current_region(wb.sheet(0), 1, 1);
    assert_eq!(region, (0, 0, 5, 2));
    let spec = PivotSpec {
        source: region,
        row_field: 1,  // 部门
        col_field: None,
        value_field: 2, // 金额
        agg: Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let res = sheetcore::pivot::compute(wb.sheet(0), &spec);
    assert_eq!(res.headers, vec!["部门", "金额"]);
    // 研发 300+200=500，销售 100+400=500，行政 50
    let get = |name: &str| -> Option<f64> {
        res.rows.iter().find(|r| r[0].display() == name).and_then(|r| {
            if let sheetcore::model::Value::Number(n) = r[1] {
                Some(n)
            } else {
                None
            }
        })
    };
    assert_eq!(get("研发"), Some(500.0));
    assert_eq!(get("销售"), Some(500.0));
    assert_eq!(get("行政"), Some(50.0));
    assert_eq!(res.rows.len(), 4, "三个部门 + 总计");
    // 总计
    let total = res.rows.last().unwrap();
    assert_eq!(total[0].display(), "总计");
    assert_eq!(total[1].display(), "1050");

    // 写到 G1 并存盘
    let rect = sheetcore::pivot::write_to(wb.sheet_mut(0), (0, 6), &res);
    assert_eq!(rect, (0, 6, 4, 7), "G1:H5");
    wb.dirty = true;
    let out = out_path("by_department", "xlsx");
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    // 读回核对
    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    assert_eq!(back.sheet(0).get(0, 6).unwrap().value.display(), "部门");
    assert_eq!(back.sheet(0).get(1, 6).unwrap().value.display(), "研发");
    assert_eq!(back.sheet(0).get(1, 7).unwrap().value.display(), "500");
    assert_eq!(back.sheet(0).get(4, 6).unwrap().value.display(), "总计");
    assert_eq!(back.sheet(0).get(4, 7).unwrap().value.display(), "1050");
    assert_eq!(back.sheet(0).name, "表格", "工作表名应保持");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn pivot_count_and_average() {
    let Some(wb) = load() else { return };
    let region = (0u32, 0u32, 5u32, 2u32);
    let mk = |agg| PivotSpec {
        source: region,
        row_field: 1,
        col_field: None,
        value_field: 2,
        agg,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let count = sheetcore::pivot::compute(wb.sheet(0), &mk(Agg::Count));
    let rd = count
        .rows
        .iter()
        .find(|r| r[0].display() == "研发")
        .unwrap();
    assert_eq!(rd[1].display(), "2", "研发两笔");
    assert_eq!(count.rows.len(), 4, "三个部门 + 总计");
    assert_eq!(count.rows.last().unwrap()[1].display(), "5", "明细共五行");

    let avg = sheetcore::pivot::compute(wb.sheet(0), &mk(Agg::Avg));
    let rd = avg.rows.iter().find(|r| r[0].display() == "研发").unwrap();
    assert_eq!(rd[1].display(), "250", "(300+200)/2");
    assert_eq!(avg.rows.last().unwrap()[1].display(), "210", "1050/5");
}
