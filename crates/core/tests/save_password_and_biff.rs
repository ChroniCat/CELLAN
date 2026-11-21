use sheetcore::crypto::{self, EncryptionSettings};
use sheetcore::{backup, xls_write, xlsx, Cell, Value, Workbook};

fn put(wb: &mut Workbook, row: u32, col: u32, value: f64) {
    wb.sheets[0].set(row, col, Cell { value: Value::Number(value), formula: None });
}

fn test_password(password: &str) -> EncryptionSettings {
    EncryptionSettings { password: password.into(), spin: 5 }
}

#[test]
fn password_transitions_preserve_the_package_and_acknowledge_the_written_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("budget.xlsx");
    let mut wb = Workbook::new_blank();
    put(&mut wb, 0, 0, 11.0);
    xlsx::save_atomic(&path, &wb).unwrap();
    let marker = b"<record version=\"1\"/>";
    let custom_part = "customXml/record.xml";
    sheetcore::fixups::write_parts(&path, &[(custom_part.into(), marker.to_vec())]).unwrap();
    let _original = std::fs::read(&path).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    wb.encryption = Some(test_password("first"));
    put(&mut wb, 0, 0, 22.0);
    xlsx::save_atomic_with_receipt(&path, &wb).unwrap().apply_to(&mut wb);
    assert!(wb.sheets[0].source_part.is_some());
    assert_eq!(wb.source_encryption.as_ref().unwrap().password, "first");
    assert!(backup::list(&path).is_empty(), "正常保存不再自动留存完整历史备份");
    assert!(!dir.path().join(".~sheetflow-history").exists());
    let protected = xlsx::load_with_password(&path, "first").unwrap();
    assert_eq!(protected.sheets[0].value(0, 0), Value::Number(22.0));

    wb.encryption = Some(test_password("second"));
    let renamed = dir.path().join("renamed.xlsx");
    xlsx::save_atomic_with_receipt(&renamed, &wb).unwrap().apply_to(&mut wb);
    wb.path = Some(renamed.clone());
    assert!(crypto::decrypt(&renamed, "first").is_err());
    let reopened = xlsx::load_with_password(&renamed, "second").unwrap();
    assert_eq!(reopened.sheets[0].value(0, 0), Value::Number(22.0));
    assert_eq!(reopened.sheets[0].source_part, wb.sheets[0].source_part);

    wb.encryption = None;
    xlsx::save_atomic_with_receipt(&renamed, &wb).unwrap().apply_to(&mut wb);
    assert!(wb.source_encryption.is_none());
    assert!(!crypto::needs_password(&renamed));
    assert_eq!(xlsx::load(&renamed).unwrap().sheets[0].value(0, 0), Value::Number(22.0));
    let restored = sheetcore::fixups::read_part_file(&renamed, custom_part).unwrap();
    assert_eq!(restored.as_slice(), marker.as_slice());
}

#[test]
fn encrypted_saves_can_overlap_in_the_same_destination_directory() {
    let dir = tempfile::tempdir().unwrap();
    let barrier = std::sync::Barrier::new(2);
    let mut books = Vec::new();
    for (name, value) in [("forecast", 17.0), ("ledger", 36.0)] {
        let target = dir.path().join(format!("{name}.xlsx"));
        let mut wb = Workbook::new_blank();
        wb.encryption = Some(test_password(name));
        put(&mut wb, 0, 0, value);
        xlsx::save_atomic_with_receipt(&target, &wb).unwrap().apply_to(&mut wb);
        wb.path = Some(target.clone());
        put(&mut wb, 1, 0, value + 1.0);
        books.push((target, wb, name, value));
    }
    std::thread::scope(|scope| {
        for (target, wb, name, value) in books {
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                xlsx::save_atomic_with_receipt(&target, &wb).unwrap();
                let back = xlsx::load_with_password(&target, name).unwrap();
                assert_eq!(back.sheets[0].value(0, 0), Value::Number(value));
                assert_eq!(back.sheets[0].value(1, 0), Value::Number(value + 1.0));
            });
        }
    });
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn biff_rejects_passwords_and_out_of_grid_coordinates_without_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("budget.xls");
    std::fs::write(&path, b"original").unwrap();
    let mut wb = Workbook::new_blank();
    wb.encryption = Some(test_password("secret"));
    let err = xlsx::save_atomic_with_receipt(&path, &wb).unwrap_err();
    assert!(err.to_string().contains("打开密码"));
    assert_eq!(std::fs::read(&path).unwrap(), b"original");

    for (r, c) in [(65_536, 0), (0, 256)] {
        let mut wb = Workbook::new_blank();
        put(&mut wb, 0, 0, 1.0);
        put(&mut wb, r, c, 2.0);
        let err = xls_write::write(&path, &wb).unwrap_err();
        assert!(err.to_string().contains("超出"));
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }
    let mut wb = Workbook::new_blank();
    put(&mut wb, 65_535, 255, 7.0);
    xls_write::write(&path, &wb).unwrap();
    let back = sheetcore::xls_read::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(65_535, 255), Value::Number(7.0));
}
