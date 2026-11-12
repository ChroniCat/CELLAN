//! VBA 源码提取：OLE 复合文档（含 mini stream）+ MS-OVBA 解压 → 模块与代码

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn reads_modules_from_fixture() {
    let p = std::path::Path::new("tests/test-work/vba_fixture2.bin");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    let data = std::fs::read(p).unwrap();
    let mods = sheetcore::vba_src::read_modules(&data).expect("解析宏工程");
    assert_eq!(mods.len(), 2, "应有两个模块");
    assert_eq!(mods[0].name, "Module1");
    assert!(mods[0].source.contains("Sub Hello()"), "源码没解出来: {}", mods[0].source);
    assert!(mods[0].source.contains("MsgBox \"hi\""));
    assert_eq!(mods[1].name, "Module2");
    assert!(mods[1].source.contains("Function Add(a, b)"));
    assert_eq!(mods[1].lines, 4);
}

#[test]
fn non_vba_bytes_are_rejected() {
    assert!(sheetcore::vba_src::read_modules(b"hello world").is_err());
    // 空工程：只有签名没有内容
    let mut junk = vec![0u8; 512];
    junk[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    assert!(sheetcore::vba_src::read_modules(&junk).is_err(), "空容器应报错而不是假成功");
}
