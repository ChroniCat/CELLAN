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
fn real_module_source_is_exact() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/real_macro.xlsm"
    ));
    let bytes = sheetcore::vba::read_vba(path).expect("读不到宏");
    let mods = sheetcore::vba_src::read_modules(&bytes).expect("解析失败");
    let m1 = mods.iter().find(|m| m.name == "Module1").unwrap();

    // 逐行比对：这就是当时写进宏里的原文
    let want = [
        "Attribute VB_Name = \"Module1\"",
        "Option Explicit",
        "",
        "Public Function AddTwo(a As Long, b As Long) As Long",
        "    AddTwo = a + b",
        "End Function",
        "",
        "Sub Hello()",
        "    Dim total As Long",
        "    total = AddTwo(12, 30)",
        "    MsgBox \"合计 \" & total, vbInformation, \"SheetCore 测试\"",
        "End Sub",
        "",
        "Sub 中文过程名测试()",
        "    Debug.Print \"中文标识符也能压缩\"",
        "End Sub",
    ];
    let got: Vec<&str> = m1.source.lines().map(|l| l.trim_end_matches(char::from(13))).collect();
    assert_eq!(got.len(), want.len(), "行数不对: {got:#?}");
    for (i, (a, b)) in want.iter().zip(got.iter()).enumerate() {
        assert_eq!(a, b, "第 {} 行不一致", i + 1);
    }
}

#[test]
fn all_modules_decode() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/real_macro.xlsm"
    ));
    let bytes = sheetcore::vba::read_vba(path).expect("读不到宏");
    let mods = sheetcore::vba_src::read_modules(&bytes).expect("解析失败");
    assert_eq!(mods.len(), 3, "应有 3 个模块: {mods:?}");
    for m in mods.iter() {
        assert!(
            !m.kind.starts_with("无法解析"),
            "{} 没解出来（{}）",
            m.name,
            m.kind
        );
        assert!(
            m.source.starts_with("Attribute VB_Name"),
            "{} 源码开头不对: {:?}",
            m.name,
            &m.source.chars().take(60).collect::<String>()
        );
    }
}
