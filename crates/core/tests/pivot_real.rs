//! 真 pivotTable 部件：生成后文件仍能被读、被 Excel 识别（结构校验）
use sheetcore::pivot::{Agg, PivotSpec};

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn writes_real_pivot_table() {
    let src = std::path::Path::new("tests/test-work/table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
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
        "tests/test-work/pivot_real_{}.xlsx",
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
    // 自建夹具：以前这个用例只断言「另一个用例的产物存在」然后删掉它，
    // 既没解析 XML 也没检查关系引用，还因为 #[ignore] 从来不跑——等于没有。
    let dir = std::env::temp_dir().join(format!("sheetcore-pivot-parts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("table.xlsx");

    let mut wb = sheetcore::model::Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        let rows = [("部门", "金额"), ("研发", "100"), ("研发", "50"), ("财务", "200")];
        for (r, (a, b)) in rows.iter().enumerate() {
            sh.set(r as u32, 1, sheetcore::model::Cell {
                value: sheetcore::model::Value::Text((*a).into()),
                formula: None,
            });
            sh.set(r as u32, 2, sheetcore::model::Cell {
                value: sheetcore::model::Value::parse_input(b),
                formula: None,
            });
        }
    }
    sheetcore::xlsx::save(&src, &wb).expect("保存夹具");

    let mut wb = sheetcore::xlsx::load(&src).expect("读回夹具");
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
    let rect = sheetcore::pivot::write_to(wb.sheet_mut(0), (0, 7), &res);
    let out = dir.join("pivot_parts.xlsx");
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let sheet_name = wb.sheet(0).name.clone();
    let vlabel = wb.sheet(0).get(0, 2).map(|c| c.value.display()).unwrap_or_default();
    let cache_id = sheetcore::pivot_xml::attach(&out, &sheet_name, &spec, &res, rect, &vlabel).expect("挂透视表");

    let cache_part = format!("xl/pivotCache/pivotCacheDefinition{cache_id}.xml");
    let table_part = "xl/pivotTables/pivotTable1.xml".to_string();

    // 1) 部件都在
    let names: Vec<String> = {
        let z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        (0..z.len()).map(|i| z.name_for_index(i).unwrap().to_string()).collect()
    };
    for must in [cache_part.clone(), table_part.clone()] {
        assert!(names.iter().any(|n| *n == must), "缺少部件 {must}");
    }

    // 2) [Content_Types].xml 必须把两个部件声明成**正确的**内容类型
    //    （只查 PartName 不够：类型写错 Excel 一样报文件损坏）
    let ct = read_part(&out, "[Content_Types].xml");
    let types = content_type_map(&ct);
    let want_cache = "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml";
    let want_table = "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml";
    assert_eq!(
        types.get(&format!("/{cache_part}")).map(String::as_str),
        Some(want_cache),
        "透视缓存的 ContentType 不对：{types:?}"
    );
    assert_eq!(
        types.get(&format!("/{table_part}")).map(String::as_str),
        Some(want_table),
        "透视表的 ContentType 不对：{types:?}"
    );

    // 3) pivotTable 的关系必须指向 cacheDefinition
    let rels = read_part(&out, "xl/pivotTables/_rels/pivotTable1.xml.rels");
    assert!(rels.contains("pivotCacheDefinition"), "pivotTable 没挂到 cacheDefinition 上：{rels}");

    // 4) 两个部件都是良构 XML（quick_xml 逐事件读到 EOF，不许报错）
    for part in [&cache_part, &table_part] {
        let body = read_part(&out, part);
        let mut reader = quick_xml::Reader::from_str(&body);
        let mut root = String::new();
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Start(e)) => {
                    if root.is_empty() {
                        root = String::from_utf8_lossy(e.local_name().as_ref().as_bytes()).to_string();
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("{part} 不是良构 XML：{e}"),
            }
        }
        let want = if part.contains("pivotCache") { "pivotCacheDefinition" } else { "pivotTableDefinition" };
        assert_eq!(root, want, "{part} 根元素不对");
    }

    // 5) cacheDefinition 的来源区域与表名必须与工作簿一致
    let cache = read_part(&out, &cache_part);
    assert!(cache.contains("worksheetSource"), "缺少数据源");
    assert!(cache.contains(&format!("sheet=\"{sheet_name}\"")), "表名不对");
    assert!(cache.contains("部门"), "字段名没写进去");

    // 6) 文件还能被自己读回来，且透视表登记在 workbook 里
    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    assert_eq!(back.sheet(0).get(0, 7).unwrap().value.display(), "部门");
    let w = read_part(&out, "xl/workbook.xml");
    assert!(w.contains("pivotCaches"), "workbook 没登记透视缓存");
    assert!(
        w.contains(&format!("cacheId=\"{cache_id}\"")),
        "workbook 里的 cacheId 不是 {cache_id}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 把 [Content_Types].xml 解析成 PartName -> ContentType（真解析，不做字符串猜测）
fn content_type_map(xml: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                if e.local_name().as_ref() != "Override" {
                    continue;
                }
                let mut part = None;
                let mut kind = None;
                for a in e.attributes() {
                    let a = a.unwrap();
                    let v = a.value.to_string();
                    match a.key.as_ref() {
                        "PartName" => part = Some(v),
                        "ContentType" => kind = Some(v),
                        _ => {}
                    }
                }
                if let (Some(p), Some(k)) = (part, kind) {
                    out.insert(p, k);
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => panic!("[Content_Types].xml 不是良构 XML：{e}"),
        }
    }
    out
}

/// 读 zip 里的一个部件；不存在直接失败（不要静默返回空串）
fn read_part(path: &std::path::Path, name: &str) -> String {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut s = String::new();
    let mut f = z.by_name(name).unwrap_or_else(|_| panic!("缺少部件 {name}"));
    std::io::Read::read_to_string(&mut f, &mut s).unwrap();
    s
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn save_attaches_pending_pivot() {
    let src = std::path::Path::new("tests/test-work/table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
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
    let out = std::path::PathBuf::from(format!("tests/test-work/pivot_pending_{}.xlsx", std::process::id()));
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
