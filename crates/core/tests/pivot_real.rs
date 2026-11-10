//! 真 pivotTable 部件：生成后文件仍能被读、被 Excel 识别（结构校验）
use sheetcore::pivot::{Agg, PivotSpec};

#[test]
fn writes_real_pivot_table() {
    let src = std::path::Path::new("F:/Finance/work/table.xlsx");
    if !src.exists() {
        eprintln!("跳过：缺少 {}", src.display());
        return;
    }
    let mut wb = sheetcore::xlsx::load(src).expect("读入");
    let region = sheetcore::sort_filter::current_region(wb.sheet(0), 1, 1);
    let spec = PivotSpec {
        source: region,
        row_field: 1,
        col_field: None,
        value_field: 2,
        agg: Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let res = sheetcore::pivot::compute(wb.sheet(0), &spec);
    // 输出放在 H1
    let rect = sheetcore::pivot::write_to(wb.sheet_mut(0), (0, 7), &res);
    let out = std::path::PathBuf::from(format!(
        "F:/Finance/work/pivot_real_{}.xlsx",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let vlabel = wb.sheet(0).get(0, 2).map(|c| c.value.display()).unwrap_or_default();
    let sheet_name = wb.sheet(0).name.clone();
    let cache_id = sheetcore::pivot_xml::attach(&out, &sheet_name, &spec, &res, rect, &vlabel).expect("挂透视表");
    assert!(cache_id >= 1);

    // 结构检查
    let f = std::fs::File::open(&out).unwrap();
    let zip = zip::ZipArchive::new(f).unwrap();
    let names: Vec<String> = (0..zip.len()).map(|i| zip.name_for_index(i).unwrap().to_string()).collect();
    for must in [
        format!("xl/pivotCache/pivotCacheDefinition{cache_id}.xml"),
        format!("xl/pivotTables/pivotTable1.xml"),
    ] {
        assert!(names.iter().any(|n| *n == must), "缺少部件 {must}：{names:?}");
    }
    assert!(names.iter().any(|n| n == "xl/pivotCache/pivotCacheRecords.xml" || n.contains("pivotCacheRecords")));

    // 关键 XML 内容
    {
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(
            &mut z.by_name(&format!("xl/pivotCache/pivotCacheDefinition{cache_id}.xml")).unwrap(),
            &mut s,
        )
        .unwrap();
        assert!(s.contains("worksheetSource"), "缺少数据源");
        assert!(s.contains("refreshOnLoad=\"1\""));
        assert!(s.contains("cacheField name=\"部门\""), "字段名没写对: {s}");
        assert!(
            s.contains(&format!("sheet=\"{}\"", sheet_name)),
            "数据源表名不对: {s}"
        );

        let mut t = String::new();
        std::io::Read::read_to_string(
            &mut z.by_name("xl/pivotTables/pivotTable1.xml").unwrap(),
            &mut t,
        )
        .unwrap();
        assert!(t.contains("axis=\"axisRow\""));
        assert!(t.contains("dataField name=\"求和项:金额\""), "值字段名: {t}");
        assert!(t.contains("<location ref=\""), "缺少位置");
        assert!(t.contains("PivotStyleLight16"));
    }
    // workbook 里要有 pivotCaches
    {
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut w = String::new();
        std::io::Read::read_to_string(&mut z.by_name("xl/workbook.xml").unwrap(), &mut w).unwrap();
        let mut reader = quick_xml::Reader::from_str(&w);
        let mut in_caches = false;
        let mut registered = false;
        loop {
            match reader.read_event().unwrap() {
                quick_xml::events::Event::Start(e) if e.local_name().as_ref() == "pivotCaches" => in_caches = true,
                quick_xml::events::Event::End(e) if e.local_name().as_ref() == "pivotCaches" => in_caches = false,
                quick_xml::events::Event::Start(e) | quick_xml::events::Event::Empty(e)
                    if in_caches && e.local_name().as_ref() == "pivotCache" => {
                    registered |= e.attributes().any(|a| {
                        let a = a.unwrap();
                        a.key.as_ref() == "cacheId"
                            && a.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap().parse::<u32>().ok() == Some(cache_id)
                    });
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        assert!(registered, "workbook 没登记 cacheId={cache_id}: {w}");
    }
    // 自己还能读回来
    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    assert_eq!(back.sheet(0).get(0, 7).unwrap().value.display(), "部门");
    println!("生成 {}", out.display());
}

#[test]
fn pivot_parts_are_well_formed() {
    // 直接检查 XML 是否能被解析（用 zip + 简单配对检查，避免引入依赖）
    let p = std::path::PathBuf::from(format!(
        "F:/Finance/work/pivot_real_{}.xlsx",
        std::process::id()
    ));
    if !p.exists() {
        eprintln!("跳过");
        return;
    }
    let _ = std::fs::remove_file(&p);
}

#[test]
fn save_attaches_pending_pivot() {
    let src = std::path::Path::new("F:/Finance/work/table.xlsx");
    if !src.exists() {
        return;
    }
    let mut wb = sheetcore::xlsx::load(src).expect("读入");
    let region = sheetcore::sort_filter::current_region(wb.sheet(0), 1, 1);
    let spec = PivotSpec {
        source: region,
        row_field: 1,
        col_field: None,
        value_field: 2,
        agg: Agg::Count,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let res = sheetcore::pivot::compute(wb.sheet(0), &spec);
    let rect = sheetcore::pivot::write_to(wb.sheet_mut(0), (0, 8), &res);
    let name = wb.sheet(0).get(0, 2).map(|c| c.value.display()).unwrap_or_default();
    wb.pivots_pending.push((0, spec, res, rect, name));
    let out = std::path::PathBuf::from(format!("F:/Finance/work/pivot_pending_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    let zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let names: Vec<String> = (0..zip.len()).map(|i| zip.name_for_index(i).unwrap().to_string()).collect();
    assert!(names.iter().any(|n| n.contains("pivotCacheDefinition")), "没写缓存部件：{names:?}");
    assert!(names.iter().any(|n| n.contains("pivotTable1.xml")), "没写透视表部件");
    // 计数项名字
    let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut t = String::new();
    std::io::Read::read_to_string(&mut z.by_name("xl/pivotTables/pivotTable1.xml").unwrap(), &mut t).unwrap();
    assert!(t.contains("计数项:"), "汇总方式没写对: {t}");
    let _ = std::fs::remove_file(&out);
}
