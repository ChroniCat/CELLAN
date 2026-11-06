//! VBA 宏保留：我们不执行宏，但打开/保存 .xlsm 时不能把它弄丢。
//!
//! umya 写出的永远是普通 xlsx（没有 vbaProject.bin、workbook 的 content type 也变回
//! 非宏版本）。所以保存后用这里把宏部件塞回去，并把内容类型与关系补上。

use anyhow::Result;
use std::io::Read;
use std::path::Path;

const VBA_PART: &str = "xl/vbaProject.bin";
const VBA_TYPE: &str = "application/vnd.ms-office.vbaProject";
const MACRO_WORKBOOK: &str = "application/vnd.ms-excel.sheet.macroEnabled.main+xml";
const PLAIN_WORKBOOK: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
const VBA_REL: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";

/// 读出文件里的 vbaProject.bin（没有就返回 None）
pub fn read_vba(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut f = zip.by_name(VBA_PART).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// 保存后把宏塞回文件
pub fn restore_vba(path: &Path, vba: &[u8]) -> Result<()> {
    // 1) 补内容类型与关系
    crate::patch::rewrite_zip(path, |name, buf| {
        if name == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                if !x.contains("vbaProject.bin") {
                    x = x.replace(
                        "</Types>",
                        &format!("<Override PartName=\"/{VBA_PART}\" ContentType=\"{VBA_TYPE}\"/></Types>"),
                    );
                }
                // workbook 的类型要改成 macroEnabled，否则 Excel 认为宏没启用
                x = x.replace(PLAIN_WORKBOOK, MACRO_WORKBOOK);
                return x.into_bytes();
            }
        }
        if name == "xl/_rels/workbook.xml.rels" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                if !xml.contains("vbaProject") {
                    let add = format!(
                        "<Relationship Id=\"rIdVba1\" Type=\"{VBA_REL}\" Target=\"vbaProject.bin\"/>"
                    );
                    return xml.replace("</Relationships>", &format!("{add}</Relationships>")).into_bytes();
                }
            }
        }
        buf
    })?;
    // 2) 追加部件本体
    crate::patch::append_entry(path, VBA_PART, vba)?;
    Ok(())
}

/// 文件里是否含宏
pub fn has_macros(path: &Path) -> bool {
    read_vba(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_xlsm(path: &Path) {
        // 造一个带「宏部件」的最小 xlsm：xlsx 结构 + vbaProject.bin
        let wb = crate::model::Workbook::new_blank();
        let tmp = path.with_extension("xlsx");
        crate::xlsx::save(&tmp, &wb).unwrap();
        crate::patch::append_entry(&tmp, VBA_PART, b"\x00fake vba".as_slice()).unwrap();
        // 补内容类型
        crate::patch::rewrite_zip(&tmp, |name, buf| {
            if name == "[Content_Types].xml" {
                if let Ok(xml) = String::from_utf8(buf.clone()) {
                    let x = xml
                        .replace(
                            "</Types>",
                            &format!("<Override PartName=\"/{VBA_PART}\" ContentType=\"{VBA_TYPE}\"/></Types>"),
                        )
                        .replace(PLAIN_WORKBOOK, MACRO_WORKBOOK);
                    return x.into_bytes();
                }
            }
            if name == "xl/_rels/workbook.xml.rels" {
                if let Ok(xml) = String::from_utf8(buf.clone()) {
                    let add = format!(
                        "<Relationship Id=\"rIdVba1\" Type=\"{VBA_REL}\" Target=\"vbaProject.bin\"/>"
                    );
                    return xml.replace("</Relationships>", &format!("{add}</Relationships>")).into_bytes();
                }
            }
            buf
        })
        .unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    #[test]
    fn detects_and_restores_vba() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("vba_test_{}.xlsm", std::process::id()));
        let _ = std::fs::remove_file(&p);
        make_xlsm(&p);
        assert!(has_macros(&p), "应检测到宏");

        // 模拟一次保存（会把宏丢掉）
        let mut wb = crate::xlsx::load(&p).expect("读入");
        wb.sheets[0].set(
            0,
            0,
            crate::model::Cell { value: crate::model::Value::Number(42.0), formula: None },
        );
        let vba = read_vba(&p).expect("读到宏");
        crate::xlsx::save(&p, &wb).expect("保存");
        restore_vba(&p, &vba).expect("恢复宏");

        assert!(has_macros(&p), "保存后宏应还在");
        let back = crate::xlsx::load(&p).expect("再读");
        assert_eq!(back.sheet(0).get(0, 0).unwrap().value.display(), "42");
        let _ = std::fs::remove_file(&p);
    }
}
