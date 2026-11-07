//! D4：真实 Excel 生成的 vbaProject.bin（tests/data/real_macro.xlsm）
//!
//! 这份文件是 Excel 2016 通过 COM 写入的，容器结构与自造 fixture 不同：
//! 模块流前面有 PerformanceCache、容器首字节是 0x01、dir 里的名字带流名。

#[test]
fn reads_real_vba_project() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/real_macro.xlsm"
    ));
    let bytes = sheetcore::vba::read_vba(path).expect("读不到 vbaProject.bin");
    let cfb = sheetcore::cfb::Compound::open(bytes.clone()).expect("OLE 解析失败");
    let names = cfb.names();
    for want in ["VBA", "dir", "PROJECT", "Module1"] {
        assert!(
            names.iter().any(|n| n.starts_with(want)),
            "缺流 {want}，实际: {names:?}"
        );
    }

    let mods = sheetcore::vba_src::read_modules(&bytes).expect("模块解析失败");
    let m1 = mods.iter().find(|m| m.name == "Module1").expect("没有 Module1");
    let first = m1.source.lines().next().unwrap_or("");
    assert_eq!(first, "Attribute VB_Name = \"Module1\"", "首行不对: {first:?}");
}

#[test]
#[ignore = "已知 bug：真实文件的 CopyToken 解码还不对，见 docs/native-rewrite.md"]
fn real_module_code_is_not_garbage() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/real_macro.xlsm"
    ));
    let bytes = sheetcore::vba::read_vba(path).expect("读不到宏");
    let mods = sheetcore::vba_src::read_modules(&bytes).expect("解析失败");
    let m1 = mods.iter().find(|m| m.name == "Module1").unwrap();
    // 这三个过程名在源码里必须出现（压缩 token 解错就会变成乱码或重复串）
    let want = ["Option Explicit", "AddTwo", "Hello"];
    let missing: Vec<&str> = want
        .iter()
        .copied()
        .filter(|w| !m1.source.contains(w))
        .collect();
    assert!(missing.is_empty(), "源码里缺 {:?}，实际前 200 字: {:?}", missing,
        &m1.source.chars().take(200).collect::<String>());
}
