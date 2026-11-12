//! C14：部分加载失败不得伪装成"完整打开"。
//!
//! 每个用例都造一个**合成损坏包**，断言 `load_diag` 报出具体问题、标出是否致命；
//! 同时 `load()`（兼容入口）仍然能返回工作簿，但不再是"什么都好"的假象。

use sheetcore::model::{Cell, Value, Workbook};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sc_c14_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("金额".into()), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(100.0), formula: Some("=A2*20".into()) });
    wb
}

/// 重写/删除 zip 里的某个部件
fn rewrite_part(path: &Path, name: &str, body: Option<&[u8]>) {
    let file = std::fs::File::open(path).unwrap();
    let mut zin = zip::ZipArchive::new(file).unwrap();
    let out_path = path.with_extension("tmp.xlsx");
    let out = std::fs::File::create(&out_path).unwrap();
    let mut zout = zip::ZipWriter::new(out);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).unwrap();
        let part = f.name().to_string();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        if part == name {
            if let Some(b) = body {
                zout.start_file(&part, opts).unwrap();
                zout.write_all(b).unwrap();
            }
            continue;
        }
        zout.start_file(&part, opts).unwrap();
        zout.write_all(&buf).unwrap();
    }
    zout.finish().unwrap();
    std::fs::rename(&out_path, path).unwrap();
}

fn sheet_part(path: &Path) -> String {
    sheetcore::patch::sheet_targets(path)
        .ok()
        .and_then(|m| m.get("数据").cloned())
        .unwrap_or_else(|| "xl/worksheets/sheet1.xml".into())
}

#[test]
fn healthy_workbook_reports_nothing() {
    let d = dir("ok");
    let p = d.join("ok.xlsx");
    sheetcore::xlsx::save(&p, &book()).unwrap();
    let (wb, diag) = sheetcore::xlsx::load_diag(&p).unwrap();
    assert!(diag.is_complete(), "正常簿不应报问题：{:?}", diag.problems);
    assert!(!diag.has_fatal());
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(100.0));
}

#[test]
fn broken_sheet_xml_is_fatal_not_silently_empty() {
    let d = dir("sheet");
    let p = d.join("broken.xlsx");
    sheetcore::xlsx::save(&p, &book()).unwrap();
    rewrite_part(&p, &sheet_part(&p), Some(b"<worksheet><sheetData><row>"));
    let hash_before = std::fs::read(&p).unwrap().len();

    let (wb, diag) = sheetcore::xlsx::load_diag(&p).unwrap();
    let p0 = diag
        .problems
        .iter()
        .find(|x| x.sheet.as_deref() == Some("数据"))
        .unwrap_or_else(|| panic!("必须点名「数据」表：{:?}", diag.problems));
    assert!(p0.fatal, "单元格数据读不出来必须致命：{p0:?}");
    assert!(p0.what.contains("单元格") || p0.what.contains("数据"), "{p0:?}");
    // 兼容入口仍然给工作簿，但表是空的——这正是"看起来打开了"的陷阱
    assert!(wb.sheets[0].iter().count() <= 1);
    assert_eq!(std::fs::read(&p).unwrap().len(), hash_before, "读失败不得改原文件");
}

#[test]
fn missing_sheet_part_is_reported() {
    let d = dir("missing");
    let p = d.join("missing.xlsx");
    sheetcore::xlsx::save(&p, &book()).unwrap();
    let part = sheet_part(&p);
    let stamp = std::time::SystemTime::now();
    rewrite_part(&p, &part, None);
    let _ = stamp;

    // 整个部件没了：要么打开就失败，要么 diag 明确报缺表——不能静默成功
    match sheetcore::xlsx::load_diag(&p) {
        Err(_) => {}
        Ok((wb, diag)) => {
            assert!(!diag.is_complete(), "缺工作表部件却报告完整：{:?}", diag.problems);
            assert!(
                diag.problems.iter().any(|x| x.fatal),
                "缺整张表必须至少有一条致命问题：{:?}",
                diag.problems
            );
            assert!(wb.sheets.iter().all(|s| s.name != "数据") || wb.sheets[0].iter().count() <= 1);
        }
    }
}

#[test]
fn broken_styles_are_reported_as_fatal() {
    let d = dir("styles");
    let p = d.join("styles.xlsx");
    let mut wb = book();
    let sid = wb.styles.intern(sheetcore::style::CellStyle { bold: true, ..Default::default() });
    wb.sheets[0].set_style(1, 0, sid);
    sheetcore::xlsx::save(&p, &wb).unwrap();
    rewrite_part(&p, "xl/styles.xml", Some(b"<styleSheet><numFmts></styleSheet>"));

    // 样式表整份损坏时解析器直接打不开整包：这本身就是"没有伪装成功"，
    // 但错误必须指明是哪个文件，用户才知道该修什么。
    match sheetcore::xlsx::load_diag(&p) {
        Err(e) => {
            let msg = format!("{e:#}");
            assert!(msg.contains("无法打开") || msg.contains("styles"), "错误不够具体：{msg}");
            assert!(
                sheetcore::xlsx::load(&p).is_err(),
                "兼容入口也不能把损坏的包装成打开成功"
            );
        }
        Ok((_wb, diag)) => {
            let st = diag
                .problems
                .iter()
                .find(|x| x.what.contains("样式"))
                .unwrap_or_else(|| panic!("样式损坏必须被发现：{:?}", diag.problems));
            assert!(st.fatal, "样式丢了覆盖保存就是数据丢失：{st:?}");
        }
    }
}

#[test]
fn missing_shared_strings_is_reported() {
    let d = dir("shared");
    let p = d.join("shared.xlsx");
    sheetcore::xlsx::save(&p, &book()).unwrap();
    // 文本在 sharedStrings 里；删掉它，单元格文本必然读不出来
    if sheetcore::fixups::read_part(&p, "xl/sharedStrings.xml").is_none() {
        return; // 这个簿子没有共享字符串部件，用例不适用
    }
    rewrite_part(&p, "xl/sharedStrings.xml", None);
    match sheetcore::xlsx::load_diag(&p) {
        Err(_) => {}
        Ok((wb, diag)) => {
            assert!(
                !diag.is_complete(),
                "共享字符串缺失却报告完整；A1={:?}",
                wb.sheets[0].value(0, 0)
            );
        }
    }
}

#[test]
fn load_wrapper_still_returns_workbook_for_healthy_files() {
    // 兼容性：load() 的签名与行为不变（老调用点不用改）
    let d = dir("compat");
    let p = d.join("ok.xlsx");
    sheetcore::xlsx::save(&p, &book()).unwrap();
    let wb = sheetcore::xlsx::load(&p).unwrap();
    assert_eq!(wb.sheets.len(), 1);
}
