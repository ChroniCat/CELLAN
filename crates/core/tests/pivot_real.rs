//! 真 pivotTable 部件：生成后文件仍能被读、被 Excel 识别（结构校验）
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

#[test]
fn writes_real_pivot_table() {
    let src = fixture("table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let mut wb = sheetcore::xlsx::load(&src).expect("读入");
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
    let out = out_path("real_pivot", "xlsx");
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let vlabel = wb.sheet(0).get(0, 2).map(|c| c.value.display()).unwrap_or_default();
    let sheet_name = wb.sheet(0).name.clone();
    let cache_id = sheetcore::pivot_xml::attach(&out, &sheet_name, &spec, &res, rect, &vlabel).expect("挂透视表");
    assert!(cache_id >= 1);
    // Excel 只认带 refreshedVersion 的缓存：缺这三个属性时 PivotTables.Count 直接是 0
    {
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut cd = String::new();
        use std::io::Read;
        z.by_name(&format!("xl/pivotCache/pivotCacheDefinition{cache_id}.xml"))
            .unwrap()
            .read_to_string(&mut cd)
            .unwrap();
        for attr in ["createdVersion=", "refreshedVersion=", "minRefreshableVersion="] {
            assert!(cd.contains(attr), "缓存定义缺 {attr}：{cd}");
        }
    }

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
    // 用仓库内的合成夹具（scripts/qa02_pivot_pdf.py 生成）；输出写到测试可执行
    // 文件目录，文件名带用例名和进程号，不依赖其它用例留下的中间产物。
    let src = fixture("table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());

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
    let out = out_path("parts", "xlsx");
    let _ = std::fs::remove_file(&out);
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

    // 3) pivotTable 的关系必须指向 cacheDefinition（解析 rels，不靠子串猜测）
    let table_rels = relationship_map(&out, "xl/pivotTables/pivotTable1.xml");
    let (_, _kind, target) = table_rels
        .iter()
        .find(|(_, kind, _)| kind.ends_with("/pivotCacheDefinition"))
        .expect("pivotTable 没挂到 cacheDefinition 上");
    assert_eq!(
        resolve_target("xl/pivotTables/pivotTable1.xml", target),
        cache_part,
        "pivotTable 的关系目标不对：{target}"
    );

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
    let (registered, cache_rid) = registered_cache(&w).expect("workbook 没登记透视缓存");
    assert_eq!(registered, cache_id, "workbook 里的 cacheId 不是 {cache_id}");
    let wb_rels = relationship_map(&out, "xl/workbook.xml");
    let (_, kind, target) = wb_rels
        .iter()
        .find(|(id, _, _)| *id == cache_rid)
        .unwrap_or_else(|| panic!("workbook 缺少关系 {cache_rid}"));
    assert!(kind.ends_with("/pivotCacheDefinition"), "缓存关系类型不对：{kind}");
    assert_eq!(resolve_target("xl/workbook.xml", target), cache_part, "缓存关系目标不对");

    // 工作表关系必须指向 pivotTable 部件（OPC 里透视表是隐式关系）
    let sheet_part = sheet_part(&out, &sheet_name);
    let sheet_rels = relationship_map(&out, &sheet_part);
    let (_, kind, target) = sheet_rels
        .iter()
        .find(|(_, kind, _)| kind.ends_with("/pivotTable"))
        .expect("工作表没挂透视表");
    assert!(kind.ends_with("/pivotTable"), "透视表关系类型不对：{kind}");
    assert_eq!(resolve_target(&sheet_part, target), table_part, "透视表关系目标不对");

    // 缓存自己的关系必须指向 records 部件，且该部件真的存在
    let cache_rels = relationship_map(&out, &cache_part);
    let (_, kind, target) = cache_rels
        .iter()
        .find(|(_, kind, _)| kind.ends_with("/pivotCacheRecords"))
        .expect("缓存没挂 records");
    assert!(kind.ends_with("/pivotCacheRecords"), "记录关系类型不对：{kind}");
    let records_part = resolve_target(&cache_part, target);
    assert!(names.iter().any(|n| *n == records_part), "缺少记录部件 {records_part}");

    let _ = std::fs::remove_file(&out);
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

/// 解析 OPC 关系部件：owner 部件 -> [(Id, Type, Target)]
fn relationship_map(path: &std::path::Path, owner: &str) -> Vec<(String, String, String)> {
    let (dir, file) = owner.rsplit_once('/').expect("部件路径");
    let part = format!("{dir}/_rels/{file}.rels");
    let xml = read_part(path, &part);
    let mut out = Vec::new();
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut root = String::new();
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                if root.is_empty() {
                    root = String::from_utf8_lossy(e.local_name().as_ref().as_bytes()).to_string();
                }
                if e.local_name().as_ref() != "Relationship" {
                    continue;
                }
                let mut id = None;
                let mut kind = None;
                let mut target = None;
                for a in e.attributes() {
                    let a = a.unwrap_or_else(|e| panic!("{part} 属性不合法：{e}"));
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .unwrap_or_else(|e| panic!("{part} 属性值不合法：{e}"))
                        .into_owned();
                    match a.key.as_ref() {
                        "Id" => id = Some(v),
                        "Type" => kind = Some(v),
                        "Target" => target = Some(v),
                        _ => {}
                    }
                }
                out.push((
                    id.unwrap_or_else(|| panic!("{part} 的关系缺少 Id")),
                    kind.unwrap_or_else(|| panic!("{part} 的关系缺少 Type")),
                    target.unwrap_or_else(|| panic!("{part} 的关系缺少 Target")),
                ));
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => panic!("{part} 不是良构 XML：{e}"),
        }
    }
    assert_eq!(root, "Relationships", "{part} 根元素不对");
    out
}

/// 关系里的相对目标 -> 包内部件名（不含前导 /）
fn resolve_target(owner: &str, target: &str) -> String {
    let mut pieces: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        owner
            .rsplit_once('/')
            .map(|(dir, _)| dir.split('/').filter(|p| !p.is_empty()).collect())
            .unwrap_or_default()
    };
    for piece in target.split('/') {
        match piece {
            "" | "." => {}
            ".." => {
                assert!(pieces.pop().is_some(), "关系目标越出包根目录：{target}");
            }
            p => pieces.push(p),
        }
    }
    pieces.join("/")
}

/// 从 workbook.xml 的 pivotCaches 里取 (cacheId, r:id)
fn registered_cache(xml: &str) -> Option<(u32, String)> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut in_caches = false;
    let mut found = None;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = e.local_name();
                if name.as_ref() == "pivotCaches" {
                    in_caches = true;
                } else if in_caches && name.as_ref() == "pivotCache" {
                    found = cache_attrs(&e);
                }
            }
            Ok(quick_xml::events::Event::Empty(e)) => {
                if in_caches && e.local_name().as_ref() == "pivotCache" {
                    found = cache_attrs(&e);
                }
            }
            Ok(quick_xml::events::Event::End(e)) if e.local_name().as_ref() == "pivotCaches" => {
                in_caches = false;
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => panic!("workbook.xml 不是良构 XML：{e}"),
        }
    }
    found
}

fn cache_attrs(e: &quick_xml::events::BytesStart<'_>) -> Option<(u32, String)> {
    let mut id = None;
    let mut rid = None;
    for a in e.attributes() {
        let a = a.ok()?;
        let v = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()?.into_owned();
        match a.key.as_ref() {
            "cacheId" => id = v.parse::<u32>().ok(),
            "r:id" => rid = Some(v),
            _ => {}
        }
    }
    Some((id?, rid?))
}

/// 工作表名 -> 包内部件名（经 workbook.xml 的关系解析）
fn sheet_part(path: &std::path::Path, sheet_name: &str) -> String {
    let xml = read_part(path, "xl/workbook.xml");
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut rid = None;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e))
                if e.local_name().as_ref() == "sheet" =>
            {
                let mut name = None;
                let mut id = None;
                for a in e.attributes() {
                    let a = a.unwrap_or_else(|e| panic!("workbook.xml 属性不合法：{e}"));
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .unwrap_or_else(|e| panic!("workbook.xml 属性值不合法：{e}"))
                        .into_owned();
                    match a.key.as_ref() {
                        "name" => name = Some(v),
                        k if k.ends_with(":id") => id = Some(v),
                        _ => {}
                    }
                }
                if name.as_deref() == Some(sheet_name) {
                    rid = id;
                    break;
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => panic!("workbook.xml 不是良构 XML：{e}"),
        }
    }
    let rid = rid.unwrap_or_else(|| panic!("workbook 里找不到工作表 {sheet_name}"));
    let rels = relationship_map(path, "xl/workbook.xml");
    let (_, kind, target) = rels
        .iter()
        .find(|(id, _, _)| *id == rid)
        .unwrap_or_else(|| panic!("找不到工作表关系 {rid}"));
    assert!(kind.ends_with("/worksheet"), "工作表关系类型不对：{kind}");
    resolve_target("xl/workbook.xml", target)
}

#[test]
fn save_attaches_pending_pivot() {
    let src = fixture("table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let mut wb = sheetcore::xlsx::load(&src).expect("读入");
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
    let out = out_path("pending_pivot", "xlsx");
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
