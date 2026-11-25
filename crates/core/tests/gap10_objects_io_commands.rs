use sheetcore::crypto::{decrypt_bytes, encrypt_agile_bytes_with, encrypt_standard_bytes};
use sheetcore::csv::{export_sheet_csv_bytes, Encoding};
use sheetcore::hyperlinks::Hyperlink;
use sheetcore::model::{Cell, CellRef, DateSystem, Value, Workbook};
use sheetcore::notes::{Note, ThreadEntry, ThreadedComment};
use sheetcore::pic::Picture;
use sheetcore::textbox::{ShapeKind, TextBox};

#[test]
fn crypto_decrypts_excel_2007_standard_and_agile_variants_with_hmac_verification() {
    let payload = b"CELLAN-ENCRYPTED-PAYLOAD-TEST-DATA-0123456789-ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let pw = "Finance#2026!";

    // 1) Excel 2007 Standard Encryption (AES-128-ECB + SHA-1)
    let std_cfb = encrypt_standard_bytes(payload, pw).expect("standard encrypt");
    assert_eq!(decrypt_bytes(&std_cfb, pw).expect("standard decrypt"), payload);
    assert!(decrypt_bytes(&std_cfb, "wrong-pw").is_err(), "错误密码必须拒绝");

    // 2) Excel 2010/2013 Agile (AES-128-CBC + SHA-1, AES-128-CBC + SHA-256, AES-256-CBC + SHA-512)
    for (bits, alg) in [(128u32, "SHA1"), (128u32, "SHA256"), (192u32, "SHA256"), (256u32, "SHA512")] {
        let mut agile_cfb = encrypt_agile_bytes_with(payload, pw, bits, alg, 1000)
            .unwrap_or_else(|e| panic!("agile encrypt {bits}/{alg}: {e}"));
        let plain = decrypt_bytes(&agile_cfb, pw)
            .unwrap_or_else(|e| panic!("agile decrypt {bits}/{alg}: {e}"));
        assert_eq!(plain.as_slice(), payload, "roundtrip {bits}/{alg}");
        assert!(decrypt_bytes(&agile_cfb, "wrong").is_err(), "wrong pw {bits}/{alg}");

        // 篡改密文 1 字节 -> HMAC dataIntegrity 必须报校验失败！
        let mut cf = cfb::CompoundFile::open(std::io::Cursor::new(&mut agile_cfb)).unwrap();
        let mut enc_pkg = Vec::new();
        {
            use std::io::Read;
            cf.open_stream("EncryptedPackage").unwrap().read_to_end(&mut enc_pkg).unwrap();
        }
        let last = enc_pkg.len() - 1;
        enc_pkg[last] ^= 0xFF;
        {
            use std::io::Write;
            let mut s = cf.open_stream("EncryptedPackage").unwrap();
            s.write_all(&enc_pkg).unwrap();
        }
        drop(cf);
        let err = decrypt_bytes(&agile_cfb, pw).expect_err("篡改密文必须触发 HMAC 完整性拦截");
        assert!(
            err.to_string().contains("HMAC"),
            "期望 HMAC 错误提示，实际：{err}"
        );
    }
}

#[test]
fn native_threaded_comments_shapes_and_picture_transforms_roundtrip_ooxml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gap10_objects.xlsx");
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        // 原生线程批注（含多轮回复与已解决状态）
        let tc = ThreadedComment {
            r: 0,
            c: 0,
            resolved: true,
            entries: vec![
                ThreadEntry {
                    id: "{TC-ROOT-1}".into(),
                    author: "王财务".into(),
                    timestamp: "2026-11-25T09:00:00.00".into(),
                    text: "请核对本期折旧金额".into(),
                },
                ThreadEntry {
                    id: "{TC-REPLY-1}".into(),
                    author: "李审计".into(),
                    timestamp: "2026-11-25T10:30:00.00".into(),
                    text: "已核对无误，同意过账".into(),
                },
            ],
        };
        sh.notes.insert(
            CellRef::new(0, 0),
            Note {
                r: 0,
                c: 0,
                author: tc.root_author().to_string(),
                text: tc.summary_text(),
            },
        );
        sh.notes_dirty = true;
        sh.threads.insert(CellRef::new(0, 0), tc);
        sh.threads_dirty = true;

        // 矢量形状（圆角矩形、右箭头、椭圆、标注框）
        for (idx, kind) in [
            ShapeKind::RoundedRect,
            ShapeKind::RightArrow,
            ShapeKind::Ellipse,
            ShapeKind::Callout,
        ]
        .into_iter()
        .enumerate()
        {
            sh.textboxes.push(TextBox {
                id: 10 + idx as u32,
                name: format!("Shape {}", idx + 1),
                kind,
                from: (idx as u32, 0, 2, 0),
                to: (idx as u32 + 2, 0, 5, 0),
                text: format!("{}", kind.label()),
                fill: Some([0xDE, 0xEB, 0xF7]),
                line: Some([0x1F, 0x4E, 0x79]),
                ..Default::default()
            });
        }
        sh.textboxes_dirty = true;

        // 带旋转与裁剪的图片
        let png = std::sync::Arc::new(vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 4, 0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31,
            0, 3, 3, 2, 0, 238, 86, 219, 145, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ]);
        sh.pictures.push(Picture {
            rect: (6, 1, 10, 4),
            off: [0; 4],
            part: "xl/media/logo.png".into(),
            data: Some(png),
            ext: "png".into(),
            name: "印章".into(),
            rot_deg: 90.0,
            crop: [0.1, 0.2, 0.15, 0.05],
        });
        sh.pictures_dirty = true;
    }

    sheetcore::xlsx::save_atomic(&path, &wb).expect("save xlsx");
    let loaded = sheetcore::xlsx::load(&path).expect("reload xlsx");
    let sh = &loaded.sheets[0];

    // 验证线程批注完整往返
    let tc = sh.threads.get(&CellRef::new(0, 0)).expect("应读回原生线程批注");
    assert!(tc.resolved, "done=1 已解决状态应保留");
    assert_eq!(tc.entries.len(), 2);
    assert_eq!(tc.entries[0].author, "王财务");
    assert_eq!(tc.entries[1].author, "李审计");
    assert_eq!(tc.entries[1].text, "已核对无误，同意过账");

    // 验证 4 种矢量形状几何往返
    let kinds: Vec<ShapeKind> = sh.textboxes.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        vec![
            ShapeKind::RoundedRect,
            ShapeKind::RightArrow,
            ShapeKind::Ellipse,
            ShapeKind::Callout
        ]
    );

    // 验证图片旋转与裁剪往返
    assert_eq!(sh.pictures.len(), 1);
    assert!((sh.pictures[0].rot_deg - 90.0).abs() < 0.1);
    assert!((sh.pictures[0].crop[0] - 0.1).abs() < 0.01);
    assert!((sh.pictures[0].crop[1] - 0.2).abs() < 0.01);
    assert!((sh.pictures[0].crop[2] - 0.15).abs() < 0.01);
    assert!((sh.pictures[0].crop[3] - 0.05).abs() < 0.01);
}

#[test]
fn structural_shift_updates_internal_hyperlink_targets_and_csv_xls_io() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        sh.name = "报表".into();
        sh.set(0, 0, Cell { value: Value::Text("跳至合计".into()), formula: None });
        sh.hyperlinks.insert(
            (0, 0),
            Hyperlink {
                external: false,
                target: String::new(),
                location: "#报表!$B$5".into(),
                tooltip: "查看总计".into(),
                ..Default::default()
            },
        );
        sh.insert_rows(2, 3);
        let loc = &sh.hyperlinks.get(&(0, 0)).unwrap().location;
        assert_eq!(loc, "#报表!$B$8", "在第 3 行插入 3 行后内部超链接目标 $B$5 应同步下移至 $B$8");

        sh.insert_cols(0, 2);
        let loc2 = &sh.hyperlinks.get(&(0, 2)).unwrap().location;
        assert_eq!(loc2, "#报表!$D$8", "在第 1 列插入 2 列后超链接锚点与目标 $B$8 应同步右移至 $D$8");
    }

    // CSV 编码（UTF-8 BOM / UTF-8 / GB18030）与自定义分隔符导出
    let mut wb2 = Workbook::new_blank();
    wb2.sheets[0].set(0, 0, Cell { value: Value::Text("科目名称".into()), formula: None });
    wb2.sheets[0].set(0, 1, Cell { value: Value::Number(1234.5), formula: None });
    let bom_bytes = export_sheet_csv_bytes(&wb2.sheets[0], &wb2.styles, DateSystem::D1900, ';', Encoding::Utf8, true).unwrap();
    assert!(bom_bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert_eq!(std::str::from_utf8(&bom_bytes[3..]).unwrap(), "科目名称;1234.5\n");
    let gbk_bytes = export_sheet_csv_bytes(&wb2.sheets[0], &wb2.styles, DateSystem::D1900, '|', Encoding::Gb18030, false).unwrap();
    let (decoded, _, _) = encoding_rs::GB18030.decode(&gbk_bytes);
    assert_eq!(decoded, "科目名称|1234.5\n");

    // .xls 兼容性预警
    wb2.sheets[0].notes.insert(CellRef::new(0, 0), Note { r: 0, c: 0, text: "n".into(), author: "a".into() });
    let warns = sheetcore::xls_write::compatibility_warnings(&wb2);
    assert!(warns.iter().any(|w| w.contains("批注")));
}
