//! 校对：不依赖词典的确定性规则检查（重复空格、首尾空格、全角字母数字、
//! 重复标点、中文里的半角标点）。Excel 的「拼写检查」要词典，这里给出的是
//! 中文表格里最常见的几类排版问题。

use crate::model::{Sheet, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofKind {
    DoubleSpace,
    EdgeSpace,
    FullWidth,
    RepeatPunct,
    HalfPunct,
}

impl ProofKind {
    pub fn detail(&self) -> &'static str {
        match self {
            ProofKind::DoubleSpace => "连续多个空格",
            ProofKind::EdgeSpace => "文本首尾有多余空格",
            ProofKind::FullWidth => "全角字母/数字（应改成半角）",
            ProofKind::RepeatPunct => "重复的中文标点",
            ProofKind::HalfPunct => "中文里混用了半角标点",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProofIssue {
    pub row: u32,
    pub col: u32,
    pub kind: ProofKind,
    /// 命中的片段（方便在对话框里看）
    pub text: String,
}

#[derive(Clone, Copy, Debug)]
pub struct ProofRules {
    pub double_space: bool,
    pub edge_space: bool,
    pub full_width: bool,
    pub repeat_punct: bool,
    pub half_punct: bool,
}

impl Default for ProofRules {
    fn default() -> Self {
        Self {
            double_space: true,
            edge_space: true,
            full_width: true,
            repeat_punct: true,
            half_punct: true,
        }
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn is_cjk_punct(c: char) -> bool {
    matches!(c, '，' | '。' | '、' | '；' | '：' | '！' | '？' | '…' | '—')
}

/// 扫一张表；命中多个规则只报第一条（同一格不刷屏）
pub fn scan(sheet: &Sheet, rules: &ProofRules) -> Vec<ProofIssue> {
    let mut out = Vec::new();
    for (cref, cell) in sheet.iter() {
        let Value::Text(t) = &cell.value else {
            continue;
        };
        let text: &str = t;
        if text.trim().is_empty() {
            continue;
        }
        let mut hit: Option<(ProofKind, String)> = None;
        if rules.edge_space && text != text.trim() {
            hit = Some((ProofKind::EdgeSpace, text.trim().chars().take(24).collect()));
        }
        if hit.is_none() && rules.double_space {
            if let Some(pos) = text.find("  ") {
                let snippet: String = text[pos..].chars().take(24).collect();
                hit = Some((ProofKind::DoubleSpace, snippet));
            }
        }
        if hit.is_none() && rules.full_width {
            if let Some(c) = text
                .chars()
                .find(|c| matches!(c, '　' | '０'..='９' | 'Ａ'..='Ｚ' | 'ａ'..='ｚ'))
            {
                hit = Some((ProofKind::FullWidth, format!("含 {c}")));
            }
        }
        if hit.is_none() && rules.repeat_punct {
            let chars: Vec<char> = text.chars().collect();
            for w in chars.windows(2) {
                if is_cjk_punct(w[0]) && is_cjk_punct(w[1]) {
                    // 「……」是合法省略号，其余重复都算
                    if !(w[0] == '…' && w[1] == '…') {
                        hit = Some((
                            ProofKind::RepeatPunct,
                            format!("{}{}", w[0], w[1]),
                        ));
                        break;
                    }
                }
            }
        }
        if hit.is_none() && rules.half_punct {
            let chars: Vec<char> = text.chars().collect();
            for i in 1..chars.len().saturating_sub(1) {
                if matches!(chars[i], ',' | '.' | ';' | ':' | '!' | '?')
                    && is_cjk(chars[i - 1])
                    && is_cjk(chars[i + 1])
                {
                    hit = Some((
                        ProofKind::HalfPunct,
                        format!("{}{}{}", chars[i - 1], chars[i], chars[i + 1]),
                    ));
                    break;
                }
            }
        }
        if let Some((kind, text)) = hit {
            out.push(ProofIssue {
                row: cref.row,
                col: cref.col,
                kind,
                text,
            });
        }
    }
    out.sort_by_key(|i| (i.row, i.col));
    out
}

/// 把一条文本按启用的规则修好（幂等：再跑一次不会再变）
pub fn fix_text(text: &str, rules: &ProofRules) -> String {
    let mut t = text.to_string();
    if rules.full_width {
        let mut out = String::with_capacity(t.len());
        for c in t.chars() {
            let m = match c {
                '\u{3000}' => ' ',
                '０'..='９' => char::from_u32(c as u32 - 0xFF10 + '0' as u32).unwrap_or(c),
                'Ａ'..='Ｚ' => char::from_u32(c as u32 - 0xFF21 + 'A' as u32).unwrap_or(c),
                'ａ'..='ｚ' => char::from_u32(c as u32 - 0xFF41 + 'a' as u32).unwrap_or(c),
                _ => c,
            };
            out.push(m);
        }
        t = out;
    }
    if rules.double_space {
        // 连续的半角空格压成一个
        let mut out = String::with_capacity(t.len());
        let mut prev_space = false;
        for c in t.chars() {
            if c == ' ' {
                if prev_space {
                    continue;
                }
                prev_space = true;
            } else {
                prev_space = false;
            }
            out.push(c);
        }
        t = out;
    }
    if rules.half_punct {
        // 中文里的半角标点换成全角
        let chars: Vec<char> = t.chars().collect();
        let mut out = String::with_capacity(t.len());
        for i in 0..chars.len() {
            let c = chars[i];
            let between_cjk = i > 0
                && i + 1 < chars.len()
                && is_cjk(chars[i - 1])
                && is_cjk(chars[i + 1]);
            if between_cjk {
                let m = match c {
                    ',' => '，',
                    '.' => '。',
                    ';' => '；',
                    ':' => '：',
                    '!' => '！',
                    '?' => '？',
                    _ => c,
                };
                out.push(m);
            } else {
                out.push(c);
            }
        }
        t = out;
    }
    if rules.repeat_punct {
        // 相同的中文标点连写压成一个（「……」保留）
        let chars: Vec<char> = t.chars().collect();
        let mut out = String::with_capacity(t.len());
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if is_cjk_punct(c) && c != '…' {
                out.push(c);
                i += 1;
                while i < chars.len() && chars[i] == c {
                    i += 1;
                }
            } else {
                out.push(c);
                i += 1;
            }
        }
        t = out;
    }
    if rules.edge_space {
        t = t.trim().to_string();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Sheet};

    fn sheet_with(cells: &[(u32, u32, &str)]) -> Sheet {
        let mut s = Sheet::new("S");
        for (r, c, t) in cells {
            s.set(
                *r,
                *c,
                Cell {
                    value: Value::Text((*t).into()),
                    formula: None,
                },
            );
        }
        s
    }

    #[test]
    fn flags_edge_and_double_space() {
        let s = sheet_with(&[(0, 0, " 前导"), (1, 0, "中间  两个空格"), (2, 0, "正常")]);
        let got = scan(&s, &ProofRules::default());
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].kind, ProofKind::EdgeSpace);
        assert_eq!(got[1].kind, ProofKind::DoubleSpace);
    }

    #[test]
    fn flags_full_width_and_repeat_punct() {
        let s = sheet_with(&[(0, 0, "１２３"), (1, 0, "好的。。")]);
        let got = scan(&s, &ProofRules::default());
        assert_eq!(got[0].kind, ProofKind::FullWidth);
        assert_eq!(got[1].kind, ProofKind::RepeatPunct);
    }

    #[test]
    fn flags_half_punct_inside_cjk() {
        let s = sheet_with(&[(0, 0, "甲,乙"), (1, 0, "A, B"), (2, 0, "……")]);
        let got = scan(&s, &ProofRules::default());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].kind, ProofKind::HalfPunct);
    }

    #[test]
    fn fixes_are_idempotent_and_correct() {
        let r = ProofRules::default();
        assert_eq!(fix_text("  前导  ", &r), "前导");
        assert_eq!(fix_text("中间  两个空格", &r), "中间 两个空格");
        assert_eq!(fix_text("１２３ＡＢＣ", &r), "123ABC");
        assert_eq!(fix_text("好的。。", &r), "好的。");
        assert_eq!(fix_text("甲,乙", &r), "甲，乙");
        assert_eq!(fix_text("A, B", &r), "A, B", "中文之外的半角标点不动");
        assert_eq!(fix_text("……", &r), "……", "省略号不被当成重复标点");
        // 幂等
        let once = fix_text("  好的。。１２３  ", &r);
        assert_eq!(fix_text(&once, &r), once);
    }

    #[test]
    fn rules_can_be_disabled() {
        let s = sheet_with(&[(0, 0, " 前导")]);
        let mut r = ProofRules::default();
        r.edge_space = false;
        assert!(scan(&s, &r).is_empty());
    }
}
