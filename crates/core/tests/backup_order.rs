use sheetcore::backup;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("native_backup_order_{}_{tag}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn book(&self) -> PathBuf { self.0.join("12-sales-book.xlsx") }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

#[test]
fn list_and_prune_order_same_day_times_and_numeric_same_second_sequences() {
    let f = Fixture::new("order");
    let book = f.book();
    let dir = backup::history_dir(&book);
    std::fs::create_dir_all(&dir).unwrap();
    for (prefix, value) in [
        ("20261110-020000-9", "seq9"), ("20261110-010000", "old"),
        ("20261110-020000-10", "seq10"), ("20261110-020000", "base"),
        ("20261110-030000", "latest"),
    ] {
        std::fs::write(dir.join(format!("{prefix}-12-sales-book.xlsx")), value).unwrap();
    }
    let contents = || backup::list(&book).iter().map(|v| std::fs::read_to_string(&v.path).unwrap()).collect::<Vec<_>>();
    assert_eq!(contents(), ["latest", "seq10", "seq9", "base", "old"]);
    assert_eq!(backup::list(&book)[1].stamp, "20261110-020000-10");
    assert_eq!(backup::prune(&book, 2).unwrap(), 3);
    assert_eq!(contents(), ["latest", "seq10"]);
}

#[test]
fn repeated_backups_keep_latest_content_after_pruning_sequence_holes() {
    let f = Fixture::new("collision");
    let book = f.book();
    for i in 0..25 {
        std::fs::write(&book, format!("version{i}")).unwrap();
        backup::keep_previous(&book).unwrap();
        let all = backup::list(&book);
        assert_eq!(std::fs::read_to_string(&all[0].path).unwrap(), format!("version{i}"));
        assert_eq!(all.len(), (i + 1).min(backup::KEEP));
    }
    let all = backup::list(&book);
    for (index, version) in all.iter().enumerate() {
        assert_eq!(std::fs::read_to_string(&version.path).unwrap(), format!("version{}", 24 - index));
    }
}
