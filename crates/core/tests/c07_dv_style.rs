//! C07.2：数据验证的错误样式（停止/警告/信息）、提示开关与保存重开一致性。
use sheetcore::dv::{self, DataValidation, DvKind, DvOp, DvStyle, DvVerdict};
use sheetcore::model::Workbook;
use std::path::PathBuf;

fn out_path(tag: &str) -> PathBuf {
    std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("{}_{}.xlsx", tag, std::process::id()))
}

fn wb_with_rules() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "输入".into();
    let rules = [
        DataValidation {
            ranges: vec![(1, 0, 1, 0)],
            kind: DvKind::Whole,
            op: DvOp::Between,
            f1: "1".into(),
            f2: "10".into(),
            error_style: DvStyle::Stop,
            error_title: "停止标题".into(),
            error_msg: "只允许 1-10".into(),
            prompt_title: "整数".into(),
            prompt_msg: "请输入 1-10".into(),
            ..Default::default()
        },
        DataValidation {
            ranges: vec![(2, 0, 2, 0)],
            kind: DvKind::Whole,
            op: DvOp::Between,
            f1: "1".into(),
            f2: "10".into(),
            error_style: DvStyle::Warning,
            show_error: true,
            show_prompt: false,
            error_title: "警告标题".into(),
            error_msg: "超出范围，仍可继续".into(),
            prompt_title: "不该显示".into(),
            prompt_msg: "开关关了".into(),
            ..Default::default()
        },
        DataValidation {
            ranges: vec![(3, 0, 3, 0)],
            kind: DvKind::Whole,
            op: DvOp::Between,
            f1: "1".into(),
            f2: "10".into(),
            error_style: DvStyle::Information,
            show_error: true,
            error_title: "信息标题".into(),
            error_msg: "仅供参考".into(),
            prompt_title: "信息提示".into(),
            prompt_msg: "提示内容".into(),
            ..Default::default()
        },
    ];
    for r in rules {
        wb.sheets[0].dv.push(r);
    }
    wb.sheets[0].dv_dirty = true;
    wb
}

#[test]
fn style_and_prompt_flags_survive_save_and_reload() {
    let wb = wb_with_rules();
    let out = out_path("c07_dv_style");
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    // 写出的属性必须显式，缺省会让 Excel 把警告关掉
    let xml = sheetcore::patch::dv_xml(&wb.sheets[0].dv);
    assert!(xml.contains("errorStyle=\"warning\""), "{xml}");
    assert!(xml.contains("errorStyle=\"information\""), "{xml}");
    assert!(xml.contains("showErrorMessage=\"1\""), "{xml}");
    assert!(xml.contains("showInputMessage=\"0\""), "{xml}");
    assert!(!xml.contains("errorStyle=\"stop\""), "stop 是默认值，不写：{xml}");

    let back = sheetcore::xlsx::load(&out).expect("重开");
    let sh = back.sheet(0);
    let stop = dv::find(&sh.dv, 1, 0).expect("停止规则");
    assert_eq!(stop.error_style, DvStyle::Stop);
    assert!(stop.show_error && stop.show_prompt);
    assert_eq!(stop.prompt_text().as_deref(), Some("整数\n请输入 1-10"));

    let warn = dv::find(&sh.dv, 2, 0).expect("警告规则");
    assert_eq!(warn.error_style, DvStyle::Warning);
    assert!(warn.show_error);
    assert!(!warn.show_prompt, "showInputMessage=0 必须保住");
    assert_eq!(warn.prompt_text(), None);
    assert_eq!(warn.error_title, "警告标题");

    let info = dv::find(&sh.dv, 3, 0).expect("信息规则");
    assert_eq!(info.error_style, DvStyle::Information);
    assert_eq!(info.prompt_text().as_deref(), Some("信息提示\n提示内容"));
    let _ = std::fs::remove_file(&out);
}

#[test]
fn verdict_follows_error_style_and_show_error_switch() {
    let mut d = DataValidation::default();
    assert_eq!(d.verdict(true), DvVerdict::Accept);
    assert_eq!(d.error_style, DvStyle::Stop);
    assert_eq!(d.verdict(false), DvVerdict::Reject, "默认是停止样式");

    d.error_style = DvStyle::Warning;
    assert_eq!(d.verdict(false), DvVerdict::AskWarning);
    d.error_style = DvStyle::Information;
    assert_eq!(d.verdict(false), DvVerdict::InformThenAccept);

    // showErrorMessage=0：Excel 不拦输入，静默放行，不再分样式
    d.show_error = false;
    d.error_style = DvStyle::Stop;
    assert_eq!(d.verdict(false), DvVerdict::AcceptSilently);
    d.error_style = DvStyle::Warning;
    assert_eq!(d.verdict(false), DvVerdict::AcceptSilently);
}

#[test]
fn prompt_text_needs_switch_and_content() {
    let mut d = DataValidation::default();
    assert_eq!(d.prompt_text(), None, "默认没有提示内容");
    d.prompt_msg = "只能填数字".into();
    assert_eq!(d.prompt_text().as_deref(), Some("只能填数字"));
    d.prompt_title = "注意".into();
    assert_eq!(d.prompt_text().as_deref(), Some("注意\n只能填数字"));
    d.show_prompt = false;
    assert_eq!(d.prompt_text(), None);
}
