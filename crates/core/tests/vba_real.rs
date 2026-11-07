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
    // 要么解对（有过程名），要么明确标成无法解析；绝不能把乱码当源码
    if m1.source.is_empty() {
        assert!(m1.kind.starts_with("无法解析"), "空源码但 kind={:?}", m1.kind);
    } else {
        assert!(m1.source.contains("AddTwo"), "源码不像样: {:?}", m1.source);
    }
}

#[test]
fn real_module_code_is_not_garbage() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/real_macro.xlsm"
    ));
    let bytes = sheetcore::vba::read_vba(path).expect("读不到宏");
    let mods = sheetcore::vba_src::read_modules(&bytes).expect("解析失败");
    let m1 = mods.iter().find(|m| m.name == "Module1").unwrap();
    // 这三个过程名在源码里必须出现（压缩 token 解错就会变成乱码或重复串）
    // 核心不变量：给出去的东西要么能看懂，要么干脆不给
    let garbled = m1.source.contains("me = \"Module1\"") && !m1.source.contains("AddTwo");
    assert!(!garbled, "把解错的乱码当源码返回了: {:?}", &m1.source.chars().take(120).collect::<String>());
    assert!(
        m1.source.is_empty() || m1.source.contains("AddTwo"),
        "源码不完整"
    );
}
