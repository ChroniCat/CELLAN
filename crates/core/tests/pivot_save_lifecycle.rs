use sheetcore::{Cell, Value, Workbook, xlsx, pivot::{Agg, PivotSpec}, structural::{apply_structural_wb, Structural}};
use quick_xml::{Reader, events::Event};
use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("native_pivot_lifecycle_{}_{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self, name: &str) -> PathBuf { self.0.join(name) }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn number(n: f64) -> Cell { Cell { value: Value::Number(n), formula: None } }
fn save(path: &Path, wb: &mut Workbook) {
    xlsx::save_atomic_with_receipt(path, wb).unwrap().apply_to(wb);
    wb.path = Some(path.to_owned());
}
fn bytes(path: &Path, part: &str) -> Vec<u8> { sheetcore::fixups::read_part_file(path, part).unwrap() }
fn tags(xml: &[u8], tag: &str) -> Vec<HashMap<String, String>> {
    let mut reader = Reader::from_reader(xml);
    let mut found = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == tag => {
                found.push(e.attributes().map(|a| {
                    let a = a.unwrap();
                    (a.key.as_ref().to_string(), sheetcore::model::xml_unescape(&a.value))
                }).collect());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    found
}
fn resolve(owner: &str, target: &str) -> String {
    let combined = if target.starts_with('/') { target.to_owned() } else {
        format!("{}/{}", owner.rsplit_once('/').map(|(d, _)| d).unwrap_or(""), target)
    };
    let mut parts = Vec::new();
    for part in combined.split('/') { match part { "" | "." => {}, ".." => { parts.pop(); }, _ => parts.push(part) } }
    parts.join("/")
}
fn rels_of(part: &str) -> String {
    let (dir, file) = part.rsplit_once('/').unwrap();
    format!("{dir}/_rels/{file}.rels")
}
fn relations(path: &Path, owner: &str) -> HashMap<String, String> {
    let Some(body) = sheetcore::fixups::read_part_file(path, &rels_of(owner)) else { return HashMap::new() };
    let mut result = HashMap::new();
    for rel in tags(&body, "Relationship") {
        assert!(!result.contains_key(&rel["Id"]), "duplicate relationship ID in {owner}");
        if rel.get("TargetMode").map(String::as_str) != Some("External") {
            result.insert(rel["Id"].clone(), resolve(owner, &rel["Target"]));
        }
    }
    result
}

#[derive(Debug)]
struct Cache { id: String, part: String, sheet: String, range: String, marker: String }
fn inventory(path: &Path, expected: usize) -> Vec<Cache> {
    let parts: HashSet<_> = sheetcore::fixups::parts(path).into_iter().collect();
    let cache_parts: HashSet<_> = parts.iter().filter(|p| p.contains("pivotCacheDefinition") && p.ends_with(".xml")).cloned().collect();
    assert_eq!(cache_parts.len(), expected, "all cache definitions: {cache_parts:?}");
    let workbook = bytes(path, "xl/workbook.xml");
    let wb_rels = relations(path, "xl/workbook.xml");
    let caches = tags(&workbook, "pivotCache");
    assert_eq!(caches.len(), expected, "workbook registration must retain every pivot");
    let mut result = Vec::new();
    let mut ids = HashSet::new();
    let mut referenced = HashSet::new();
    for cache in caches {
        let id = &cache["cacheId"];
        assert!(ids.insert(id.clone()), "duplicate cache ID");
        let part = &wb_rels[&cache["r:id"]];
        assert!(cache_parts.contains(part));
        referenced.insert(part.clone());
        let def = bytes(path, part);
        let source = tags(&def, "worksheetSource").pop().unwrap();
        let root = tags(&def, "pivotCacheDefinition").pop().unwrap();
        if let Some(rid) = root.get("r:id") {
            let records = &relations(path, part)[rid];
            assert!(parts.contains(records), "cache records relationship must resolve");
        }
        result.push(Cache { id: id.clone(), part: part.clone(), sheet: source["sheet"].clone(), range: source["ref"].clone(), marker: root.get("refreshedBy").cloned().unwrap_or_default() });
    }
    assert_eq!(referenced, cache_parts, "no orphan cache definitions");
    let table_parts: HashSet<_> = parts.iter().filter(|p| p.starts_with("xl/pivotTables/") && p.ends_with(".xml")).cloned().collect();
    assert_eq!(table_parts.len(), expected, "no orphan or duplicate pivot tables");
    let mut attached = HashSet::new();
    for sheet in sheetcore::patch::sheet_targets(path).unwrap().values() {
        for part in relations(path, sheet).values().filter(|p| table_parts.contains(*p)) { attached.insert(part.clone()); }
    }
    assert_eq!(attached, table_parts, "every table belongs to a worksheet");
    for table in table_parts {
        let root = tags(&bytes(path, &table), "pivotTableDefinition").pop().unwrap();
        assert!(ids.contains(&root["cacheId"]), "pivot table cacheId resolves");
        let cache = result.iter().find(|c| c.id == root["cacheId"]).unwrap();
        let table_rels = relations(path, &table);
        assert!(table_rels.values().any(|part| part == &cache.part), "table {table} must point to the definition registered for cacheId {}", cache.id);
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    result
}

fn seeded() -> Workbook {
    let mut wb = Workbook::new_blank();
    for r in 0..4 { wb.sheets[0].set(r, 0, number(r as f64)); wb.sheets[0].set(r, 1, number(10.)); }
    wb
}
fn spec() -> PivotSpec { PivotSpec { source: (0, 0, 3, 1), row_field: 0, col_field: None, value_field: 1, agg: Agg::Sum } }
fn add(wb: &mut Workbook, sheet: usize, out_row: u32) {
    let spec = spec();
    let result = sheetcore::pivot::compute(&wb.sheets[sheet], &spec);
    wb.pivots_pending.push((sheet, spec, result, (out_row, 0, out_row + 4, 1), "Sum".into()));
    wb.dirty = true;
}
fn imported(path: &Path) -> Workbook {
    let mut wb = seeded();
    save(path, &mut wb);
    let result = sheetcore::pivot::compute(&wb.sheets[0], &spec());
    sheetcore::pivot_xml::attach(path, "Sheet1", &spec(), &result, (6, 0, 10, 1), "Imported").unwrap();
    xlsx::load(path).unwrap()
}

#[test]
fn first_and_later_pivots_remain_registered_after_repeated_save_and_save_as() {
    for save_empty_first in [false, true] {
        let f = Fixture::new();
        let path = f.file("first.xlsx");
        let mut wb = seeded();
        if save_empty_first { save(&path, &mut wb); wb = xlsx::load(&path).unwrap(); }
        add(&mut wb, 0, 6);
        let token = wb.pivot_session.token().to_owned();
        for _ in 0..3 {
            save(&path, &mut wb);
            let caches = inventory(&path, 1);
            assert_eq!(caches[0].marker, format!("sheetcore:{token}"));
            assert_eq!(caches[0].range, "A1:B4");
            assert_eq!(wb.pivots_pending.len(), 1, "logical model is not consumed by save");
        }
        let copy = f.file("copy.xlsx");
        save(&copy, &mut wb);
        save(&copy, &mut wb);
        inventory(&copy, 1);
        let mut reopened = xlsx::load(&copy).unwrap();
        assert_ne!(reopened.pivot_session.token(), token);
        assert!(reopened.pivots_pending.is_empty());
        save(&copy, &mut reopened);
        inventory(&copy, 1);
        let fresh = Workbook::default();
        assert_eq!(fresh.clone().pivot_session.token(), fresh.pivot_session.token());
        assert_ne!(Workbook::default().pivot_session.token(), Workbook::default().pivot_session.token());
    }
}

#[test]
fn direct_save_and_atomic_save_are_idempotent_without_acknowledgement() {
    let f = Fixture::new();
    let path = f.file("direct.xlsx");
    let mut wb = seeded();
    save(&path, &mut wb);
    add(&mut wb, 0, 6);
    for _ in 0..3 { xlsx::save(&path, &wb).unwrap(); inventory(&path, 1); }
    for _ in 0..3 { xlsx::save_atomic(&path, &wb).unwrap(); inventory(&path, 1); }
}

#[test]
fn imported_and_multiple_new_pivots_keep_independent_caches_and_relationships() {
    let f = Fixture::new();
    let path = f.file("multiple.xlsx");
    let mut wb = imported(&path);
    let imported = inventory(&path, 1).pop().unwrap();
    let original = bytes(&path, &imported.part);
    add(&mut wb, 0, 12);
    add(&mut wb, 0, 18);
    let mut other = seeded().sheets.remove(0);
    other.name = "Second".into();
    sheetcore::sheet_ops::insert_sheet(&mut wb, 1, other);
    add(&mut wb, 1, 6);
    for _ in 0..3 {
        save(&path, &mut wb);
        let caches = inventory(&path, 4);
        assert_eq!(caches.iter().filter(|c| c.sheet == "Sheet1").count(), 3);
        assert_eq!(caches.iter().filter(|c| c.sheet == "Second").count(), 1);
        assert_eq!(bytes(&path, &imported.part), original, "session rebuild must not overwrite imported definition");
    }
}

#[test]
fn unmarked_cache_added_outside_pending_model_is_preserved() {
    let f = Fixture::new();
    let path = f.file("unmarked.xlsx");
    let mut wb = seeded();
    add(&mut wb, 0, 6);
    save(&path, &mut wb);
    let result = sheetcore::pivot::compute(&wb.sheets[0], &spec());
    sheetcore::pivot_xml::attach(&path, "Sheet1", &spec(), &result, (12, 0, 16, 1), "External").unwrap();
    let caches = inventory(&path, 2);
    let unmarked = caches.iter().find(|c| c.marker == "sheetcore").unwrap();
    let original = bytes(&path, &unmarked.part);
    for atomic in [false, true] {
        if atomic { save(&path, &mut wb); } else { xlsx::save(&path, &wb).unwrap(); }
        inventory(&path, 2);
        assert_eq!(bytes(&path, &unmarked.part), original);
    }
}

#[test]
fn rename_reorder_and_delete_remove_only_owned_pivot_attachments() {
    let f = Fixture::new();
    let path = f.file("sheets.xlsx");
    let mut wb = imported(&path);
    let mut other = seeded().sheets.remove(0);
    other.name = "Pending".into();
    sheetcore::sheet_ops::insert_sheet(&mut wb, 1, other);
    add(&mut wb, 1, 6);
    save(&path, &mut wb);
    sheetcore::sheet_ops::rename_sheet(&mut wb, 1, "Moved & Renamed");
    sheetcore::sheet_ops::move_sheet(&mut wb, 1, 0);
    for _ in 0..2 {
        save(&path, &mut wb);
        let caches = inventory(&path, 2);
        assert!(caches.iter().any(|c| c.sheet == "Moved & Renamed"));
        assert!(caches.iter().any(|c| c.sheet == "Sheet1"));
    }
    sheetcore::sheet_ops::delete_sheet(&mut wb, 0);
    assert!(wb.pivots_pending.is_empty());
    for _ in 0..2 { save(&path, &mut wb); assert_eq!(inventory(&path, 1)[0].sheet, "Sheet1"); }
}

#[test]
fn pending_created_after_old_structural_journal_uses_current_absolute_source() {
    let f = Fixture::new();
    let path = f.file("late-pivot.xlsx");
    let mut wb = imported(&path);
    apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 2 });
    add(&mut wb, 0, 8); // created after the edit, intentionally still A1:B4
    for _ in 0..3 {
        save(&path, &mut wb);
        let caches = inventory(&path, 2);
        assert_eq!(caches.iter().find(|c| c.marker == "sheetcore").unwrap().range, "A3:B6");
        assert_eq!(caches.iter().find(|c| c.marker.starts_with("sheetcore:")).unwrap().range, "A1:B4");
    }
}

#[test]
fn older_save_snapshot_cannot_consume_newer_pending_specs() {
    let f = Fixture::new();
    let path = f.file("snapshot.xlsx");
    let mut live = seeded();
    save(&path, &mut live);
    add(&mut live, 0, 6);
    let snapshot = live.clone();
    apply_structural_wb(&mut live, 0, &Structural::InsertRows { at: 0, n: 1 });
    add(&mut live, 0, 15);
    xlsx::save_atomic_with_receipt(&path, &snapshot).unwrap().apply_to(&mut live);
    assert_eq!(live.pivots_pending.len(), 2);
    save(&path, &mut live);
    let caches = inventory(&path, 2);
    assert!(caches.iter().any(|c| c.range == "A2:B5"));
    assert!(caches.iter().any(|c| c.range == "A1:B4"));
    save(&path, &mut live);
    inventory(&path, 2);
}

#[test]
fn failing_second_attachment_leaves_atomic_target_and_live_model_unchanged() {
    let f = Fixture::new();
    let path = f.file("failure.xlsx");
    let mut wb = imported(&path);
    let original = std::fs::read(&path).unwrap();
    add(&mut wb, 0, 12);
    add(&mut wb, 0, 18);
    wb.pivots_pending[1].0 = 99;
    assert!(xlsx::save_atomic_with_receipt(&path, &wb).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(wb.pivots_pending.len(), 2);
    inventory(&path, 1);
    wb.pivots_pending[1].0 = 0;
    wb.pivots_pending[1].1.value_field = 99;
    assert!(xlsx::save_atomic_with_receipt(&path, &wb).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn cleanup_preserves_record_parts_shared_with_imported_caches() {
    let f = Fixture::new();
    let path = f.file("shared-records.xlsx");
    let mut wb = imported(&path);
    add(&mut wb, 0, 12);
    save(&path, &mut wb);
    let caches = inventory(&path, 2);
    let old = caches.iter().find(|c| c.marker == "sheetcore").unwrap();
    let new = caches.iter().find(|c| c.marker.starts_with("sheetcore:")).unwrap();
    let shared = relations(&path, &new.part).into_values().next().unwrap();
    let old_rels = rels_of(&old.part);
    let old_target = relations(&path, &old.part).into_values().next().unwrap();
    let original_body = bytes(&path, &shared);
    sheetcore::patch::rewrite_zip(&path, |name, body| {
        if name != old_rels { return body; }
        String::from_utf8(body).unwrap().replace(old_target.rsplit('/').next().unwrap(), shared.rsplit('/').next().unwrap()).into_bytes()
    }).unwrap();
    wb.pivots_pending.clear();
    save(&path, &mut wb);
    assert_eq!(inventory(&path, 1)[0].id, old.id);
    assert_eq!(bytes(&path, &shared), original_body);
}
