use sheetcore::{Cell, Sheet, Value, Workbook, pivot::{Agg, PivotSpec}, pivot_xml, xlsx};
use quick_xml::{Reader, events::Event};
use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("native_pivot_attach_{}_{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self) -> PathBuf { self.0.join("book.xlsx") }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn xml(path: &Path, part: &str) -> Vec<u8> { sheetcore::fixups::read_part_file(path, part).unwrap_or_else(|| panic!("missing {part}")) }
fn tags(bytes: &[u8], name: &str) -> Vec<HashMap<String, String>> {
    let mut reader = Reader::from_reader(bytes);
    let mut result = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == name => {
                result.push(e.attributes().map(|a| { let a = a.unwrap(); (a.key.as_ref().to_owned(), a.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap().into_owned()) }).collect());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    result
}
fn rels_part(part: &str) -> String {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") }
}
fn resolve(part: &str, target: &str) -> String {
    let mut bits: Vec<_> = if target.starts_with('/') { Vec::new() } else { part.split('/').collect() };
    if !target.starts_with('/') { bits.pop(); }
    for bit in target.split('/') { match bit { "" | "." => {}, ".." => { bits.pop(); }, _ => bits.push(bit) } }
    bits.join("/")
}
fn relationships(path: &Path, owner: &str) -> Vec<HashMap<String, String>> { tags(&xml(path, &rels_part(owner)), "Relationship") }
fn book(path: &Path) -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Second"));
    for sheet in &mut wb.sheets {
        sheet.set(0, 0, Cell { value: Value::Text("Group".into()), formula: None });
        sheet.set(0, 1, Cell { value: Value::Text("Value".into()), formula: None });
        for row in 1..4 { for col in 0..2 { sheet.set(row, col, Cell { value: Value::Number((row + col) as f64), formula: None }); } }
    }
    xlsx::save_atomic_with_receipt(path, &wb).unwrap().apply_to(&mut wb);
    wb.path = Some(path.into());
    wb
}
fn attach(path: &Path, wb: &Workbook, sheet: usize, row: u32) -> u32 {
    let spec = PivotSpec { source: (0, 0, 3, 1), row_field: 0, col_field: None, value_field: 1, agg: Agg::Sum, group: sheetcore::pivot::GroupBy::None, exclude: Vec::new() };
    pivot_xml::attach(path, &wb.sheets[sheet].name, &spec, &sheetcore::pivot::compute(&wb.sheets[sheet], &spec), (row, 0, row + 4, 1), "Sum").unwrap()
}

// Follow the actual OPC graph; merely finding files in the ZIP does not prove
// that Excel can discover their pivot tables or resolve the correct cache.
fn assert_graph(path: &Path, expected: &[(&str, usize)]) {
    let targets: HashMap<_, _> = sheetcore::patch::sheet_targets(path).unwrap().into_iter()
        .map(|(name, part)| (name, resolve("", &format!("/{part}")))).collect();
    let workbook = xml(path, "xl/workbook.xml");
    let registrations = tags(&workbook, "pivotCache");
    let workbook_rels = relationships(path, "xl/workbook.xml");
    let types = tags(&xml(path, "[Content_Types].xml"), "Override");
    let mut ids = HashSet::new();
    for rel in &workbook_rels { assert!(ids.insert(&rel["Id"]), "duplicate workbook relationship id"); }
    let caches: HashMap<_, _> = registrations.iter().map(|entry| {
        let rel = workbook_rels.iter().find(|rel| rel["Id"] == entry["r:id"]).unwrap();
        assert!(rel["Type"].ends_with("/pivotCacheDefinition"));
        (entry["cacheId"].clone(), resolve("xl/workbook.xml", &rel["Target"]))
    }).collect();
    assert_eq!(caches.len(), registrations.len(), "duplicate cache IDs");
    let mut table_names = HashSet::new();
    for (name, count) in expected {
        let sheet = &targets[*name];
        let rels = relationships(path, sheet);
        let mut ids = HashSet::new();
        for rel in &rels { assert!(ids.insert(&rel["Id"]), "duplicate worksheet relationship id"); }
        let tables: Vec<_> = rels.iter().filter(|rel| rel["Type"].ends_with("/pivotTable")).collect();
        assert_eq!(tables.len(), *count, "pivot table count in {name}");
        for rel in tables {
            let part = resolve(sheet, &rel["Target"]);
            let root = &tags(&xml(path, &part), "pivotTableDefinition")[0];
            assert!(table_names.insert(root["name"].clone()), "duplicate pivot name");
            let cache = &caches[&root["cacheId"]];
            let cache_rel = relationships(path, &part).into_iter().find(|rel| rel["Type"].ends_with("/pivotCacheDefinition")).unwrap();
            assert_eq!(resolve(&part, &cache_rel["Target"]), *cache);
            let cache_xml = xml(path, cache);
            let cache_root = &tags(&cache_xml, "pivotCacheDefinition")[0];
            let source = &tags(&cache_xml, "worksheetSource")[0];
            assert_eq!(source["sheet"], *name);
            assert_eq!(source["ref"], "A1:B4");
            let records = relationships(path, cache).into_iter().find(|rel| rel["Id"] == cache_root["r:id"]).unwrap();
            assert!(records["Type"].ends_with("/pivotCacheRecords"));
            let records = resolve(cache, &records["Target"]);
            assert_eq!(tags(&xml(path, &records), "pivotCacheRecords").len(), 1);
            for (part, suffix) in [(&part, "pivotTable+xml"), (cache, "pivotCacheDefinition+xml"), (&records, "pivotCacheRecords+xml")] {
                assert!(types.iter().any(|t| t["PartName"] == format!("/{part}") && t["ContentType"].ends_with(suffix)), "missing type {part}");
            }
        }
    }
    let text = String::from_utf8(workbook).unwrap();
    if let (Some(calc), Some(caches)) = (text.find("<calcPr"), text.find("<pivotCaches")) { assert!(calc < caches, "pivotCaches follows calcPr in CT_Workbook"); }
}

#[test]
fn attaching_multiple_pivots_preserves_existing_sources_and_every_relationship() {
    let f = Fixture::new(); let path = f.file(); let wb = book(&path);
    let first = attach(&path, &wb, 0, 6);
    let original = xml(&path, &format!("xl/pivotCache/pivotCacheDefinition{first}.xml"));
    let second = attach(&path, &wb, 1, 6);
    let third = attach(&path, &wb, 0, 15);
    assert_ne!(first, second); assert_ne!(first, third); assert_ne!(second, third);
    assert_eq!(xml(&path, &format!("xl/pivotCache/pivotCacheDefinition{first}.xml")), original);
    assert_graph(&path, &[("Sheet1", 2), ("Second", 1)]);
}

#[test]
fn sparse_parts_and_unrelated_relationship_ids_are_never_overwritten() {
    let f = Fixture::new(); let path = f.file(); let wb = book(&path);
    let first = attach(&path, &wb, 0, 6);
    let reserved = [
        ("xl/pivotCache/pivotCacheRecords2.xml", "<pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"0\"/>"),
        ("xl/pivotTables/pivotTable3.xml", "<pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" name=\"数据透视表4\" cacheId=\"42\"/>"),
        ("xl/pivotTables/_rels/pivotTable2.xml.rels", "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"/>"),
    ];
    for (part, bytes) in reserved { sheetcore::patch::append_entry(&path, part, bytes.as_bytes()).unwrap(); }
    let sheet = sheetcore::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
    for (part, id) in [("xl/_rels/workbook.xml.rels".to_string(), "rIdPivotCache3"), (rels_part(&sheet), "rIdPivotTable4")] {
        let old = String::from_utf8(xml(&path, &part)).unwrap();
        let new = old.replace("</Relationships>", &format!("<Relationship Id=\"{id}\" Type=\"urn:test\" Target=\"https://example.invalid\" TargetMode=\"External\"/></Relationships>"));
        sheetcore::patch::append_entry(&path, &part, new.as_bytes()).unwrap();
    }
    // A relationship can already own a path even when its target is missing.
    let rels = "xl/_rels/workbook.xml.rels";
    let original = String::from_utf8(xml(&path, rels)).unwrap();
    let extra = original.replace("</Relationships>", "<Relationship Id=\"reserved-cache\" Type=\"urn:test\" Target=\"pivotCache/pivotCacheDefinition3.xml\"/></Relationships>");
    sheetcore::patch::append_entry(&path, rels, extra.as_bytes()).unwrap();
    let next = attach(&path, &wb, 0, 15);
    assert_ne!(next, first); assert_ne!(next, 2); assert_ne!(next, 3);
    for (part, bytes) in reserved { assert_eq!(xml(&path, part), bytes.as_bytes()); }
    assert_graph(&path, &[("Sheet1", 2)]);
}

#[test]
fn nonstandard_worksheet_paths_and_empty_prefixed_registries_are_supported() {
    let f = Fixture::new(); let path = f.file(); let wb = book(&path);
    let sheet = sheetcore::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
    let renamed = "custom/sheets/report.xml";
    let bytes = xml(&path, &sheet);
    sheetcore::patch::append_entry(&path, renamed, &bytes).unwrap();
    sheetcore::patch::append_entry(&path, &rels_part(renamed),
        b"<p:Relationships xmlns:p=\"http://schemas.openxmlformats.org/package/2006/relationships\"/>").unwrap();
    sheetcore::patch::rewrite_zip(&path, |part, bytes| {
        if part == "xl/_rels/workbook.xml.rels" {
            return String::from_utf8(bytes).unwrap().replace(sheet.trim_start_matches("xl/"), "../custom/sheets/report.xml").into_bytes();
        }
        if part == "xl/workbook.xml" {
            return String::from_utf8(bytes).unwrap().replace("</workbook>", "<x:pivotCaches xmlns:x=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"/><extLst><ext uri=\"marker\"/></extLst></workbook>").into_bytes();
        }
        bytes
    }).unwrap();
    attach(&path, &wb, 0, 6); attach(&path, &wb, 0, 15);
    let rels = relationships(&path, renamed);
    assert_eq!(rels.iter().filter(|r| r["Type"].ends_with("/pivotTable")).count(), 2);
    for rel in rels { assert!(!xml(&path, &resolve(renamed, &rel["Target"])).is_empty()); }
    let workbook = xml(&path, "xl/workbook.xml");
    assert_eq!(tags(&workbook, "pivotCaches").len(), 1);
    assert_eq!(tags(&workbook, "pivotCache").len(), 2);
    let workbook = String::from_utf8(workbook).unwrap();
    assert!(workbook.contains("uri=\"marker\""));
    assert!(workbook.find("pivotCaches").unwrap() < workbook.find("<extLst").unwrap());
    assert_graph(&path, &[("Sheet1", 2)]);
}

#[test]
fn attachments_remain_discoverable_after_reopen_save_as_and_another_attach() {
    let f = Fixture::new(); let path = f.file(); let wb = book(&path);
    attach(&path, &wb, 0, 6);
    let mut reopened = xlsx::load(&path).unwrap();
    let copy = f.0.join("copy.xlsx");
    xlsx::save_atomic_with_receipt(&copy, &reopened).unwrap().apply_to(&mut reopened);
    reopened.path = Some(copy.clone());
    attach(&copy, &reopened, 1, 6);
    attach(&copy, &reopened, 0, 15);
    assert_graph(&copy, &[("Sheet1", 2), ("Second", 1)]);
    let back = xlsx::load(&copy).unwrap();
    assert_eq!(back.sheets[0].value(2, 1), Value::Number(3.));
    assert_eq!(back.sheets[1].value(2, 1), Value::Number(3.));
}

#[test]
fn imported_cache_ids_part_numbers_and_table_names_are_independent() {
    let f = Fixture::new(); let path = f.file(); let wb = book(&path);
    attach(&path, &wb, 0, 6);
    let replacements = [
        ("pivotCacheDefinition1.xml", "pivotCacheDefinition7.xml"),
        ("pivotCacheRecords1.xml", "pivotCacheRecords11.xml"),
        ("pivotTable1.xml", "pivotTable9.xml"),
    ];
    let rename = |name: &str| replacements.iter().fold(name.to_owned(), |s, (a, b)| s.replace(a, b));
    let mut removed = Vec::new();
    let mut additions = Vec::new();
    for part in sheetcore::fixups::parts(&path) {
        let new = rename(&part);
        if part != new {
            additions.push((new, xml(&path, &part)));
            removed.push(part);
        }
    }
    sheetcore::patch::append_entries(&path, &additions).unwrap();
    sheetcore::patch::remove_entries(&path, &removed).unwrap();
    sheetcore::patch::rewrite_zip(&path, |part, bytes| {
        if !part.ends_with(".xml") && !part.ends_with(".rels") { return bytes; }
        let text = rename(&String::from_utf8(bytes).unwrap());
        text.replace("cacheId=\"1\"", "cacheId=\"42\"").replace("name=\"数据透视表1\"", "name=\"数据透视表2\"").into_bytes()
    }).unwrap();
    let original = xml(&path, "xl/pivotCache/pivotCacheDefinition7.xml");
    let first = attach(&path, &wb, 0, 15);
    let second = attach(&path, &wb, 1, 6);
    assert_ne!(first, 42); assert_ne!(second, 42); assert_ne!(first, second);
    assert_eq!(xml(&path, "xl/pivotCache/pivotCacheDefinition7.xml"), original);
    assert_graph(&path, &[("Sheet1", 2), ("Second", 1)]);
}

#[test]
fn invalid_attachment_fails_before_mutating_the_package() {
    let f = Fixture::new(); let path = f.file(); let wb = book(&path);
    let mut spec = PivotSpec { source: (0, 0, 3, 1), row_field: 0, col_field: None, value_field: 1, agg: Agg::Sum, group: sheetcore::pivot::GroupBy::None, exclude: Vec::new() };
    let res = sheetcore::pivot::compute(&wb.sheets[0], &spec);
    let before = std::fs::read(&path).unwrap();
    assert!(pivot_xml::attach(&path, "Missing", &spec, &res, (6, 0, 10, 1), "Sum").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    spec.source = (0, 0, 1_048_576, 1);
    assert!(pivot_xml::attach(&path, "Sheet1", &spec, &res, (6, 0, 10, 1), "Sum").is_err());
    spec.source = (0, 0, 3, 1);
    for rect in [(10, 0, 6, 1), (6, 0, 10, 16_384)] {
        assert!(pivot_xml::attach(&path, "Sheet1", &spec, &res, rect, "Sum").is_err());
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    spec.source = (0, 2, 3, 3);
    assert!(pivot_xml::attach(&path, "Sheet1", &spec, &res, (6, 0, 10, 1), "Sum").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    sheetcore::patch::append_entry(&path, "xl/_rels/workbook.xml.rels", b"<Relationships>").unwrap();
    let before = std::fs::read(&path).unwrap();
    spec.source = (0, 0, 3, 1);
    assert!(pivot_xml::attach(&path, "Sheet1", &spec, &res, (6, 0, 10, 1), "Sum").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
