//! Function completion, argument hints and wizard helpers for formula editing.

pub(super) mod references;

use crate::{EditState, debug_flags, theme};
use eframe::egui;

/// 参数名来自函数签名：`SUM(number1,[number2],…)` → ["number1","number2（可选）"]。
pub(super) fn fn_arg_names(name: &str) -> Vec<String> {
    let sig = formula_signature(name);
    let (Some(open), Some(close)) = (sig.find('('), sig.rfind(')')) else {
        return Vec::new();
    };
    let inner = &sig[open + 1..close];
    if inner.trim().is_empty() || inner.trim() == "参数…" {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty() && p != "…")
        .map(|p| {
            let opt = p.starts_with('[');
            let p = p.trim_matches(|c| c == '[' || c == ']');
            if opt {
                format!("{p}（可选）")
            } else {
                p.to_string()
            }
        })
        .collect()
}

/// 参数名去掉「（可选）」标记，用作输入框的值占位。
pub(super) fn fn_arg_plain(label: &str) -> &str {
    label.trim_end_matches("（可选）")
}

/// 用向导里的参数拼出公式；一个参数都没填时给出 `=SUM()`（光标停在括号里）。
pub(super) fn fn_formula_from_args(name: &str, args: &[String]) -> String {
    let used = args
        .iter()
        .rposition(|a| !a.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    let inner = args[..used]
        .iter()
        .map(|a| a.trim().to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("={name}({inner})")
}

/// 公式自动补全/参数提示。`start..end` 是光标所在标识符在缓冲里的字节区间。
#[derive(Clone, Default)]
pub(super) struct FormulaHelp {
    start: usize,
    end: usize,
    token: String,
    pub(super) suggestions: Vec<(String, String)>,
    pub(super) signature: Option<String>,
    /// 当前光标位于最内层未闭合函数的第几个参数（0 起）；用于参数提示加粗高亮。
    pub(super) active_arg: Option<usize>,
}

impl FormulaHelp {
    fn selected(&self, idx: usize) -> Option<&str> {
        self.suggestions.get(idx.min(self.suggestions.len().saturating_sub(1))).map(|x| x.0.as_str())
    }
}

/// 将字符下标安全转换为 UTF-8 字节偏移。
pub(super) fn byte_index_of_char(s: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// 判断字节位置 `byte_pos` 是否落在公式的双引号字符串字面量内部（正确处理 `""` 转义）。
fn is_inside_string_literal(buf: &str, byte_pos: usize) -> bool {
    let limit = byte_pos.min(buf.len());
    let bytes = buf.as_bytes();
    let mut i = 0usize;
    let mut in_str = false;
    while i < limit {
        if bytes[i] == b'"' {
            if in_str && i + 1 < limit && bytes[i + 1] == b'"' {
                i += 2;
                continue;
            }
            in_str = !in_str;
        }
        i += 1;
    }
    in_str
}

pub(super) fn formula_signature(name: &str) -> String {
    sheetcore::formula::fncat::signature(name)
}

pub(super) fn function_desc(name: &str) -> &'static str {
    sheetcore::formula::fncat::description(name)
}

/// 取 `=` 后位于 `caret_chars` 处正在输入的函数名前缀字节区间 `(start, end)`；
/// 跳过双引号字符串字面量，数字开头或前导非运算符/分隔符返回 None。
fn formula_token_span_at(buf: &str, caret_chars: usize) -> Option<(usize, usize)> {
    if !buf.starts_with('=') {
        return None;
    }
    let caret = byte_index_of_char(buf, caret_chars);
    if is_inside_string_literal(buf, caret) {
        return None;
    }
    let prefix = &buf[..caret];
    let mut start = caret;
    for (i, ch) in prefix.char_indices().rev() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            start = i;
        } else {
            break;
        }
    }
    if start == caret {
        return None;
    }
    // 若光标后紧跟字母/数字/下划线且没有括号，说明正在编辑中间标识符，扩展 end 到标识符尾部
    let mut end = caret;
    for (i, ch) in buf[caret..].char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            end = caret + i + ch.len_utf8();
        } else {
            break;
        }
    }
    let token = &buf[start..caret];
    if token.chars().next()?.is_ascii_digit() {
        return None;
    }
    let prev = buf[..start].chars().last();
    let ok = matches!(
        prev,
        None | Some('=') | Some('(') | Some(',') | Some('+') | Some('-') | Some('*')
            | Some('/') | Some('^') | Some('&') | Some('<') | Some('>') | Some(':')
            | Some(';') | Some('{') | Some(' ')
    );
    if !ok {
        return None;
    }
    Some((start, end))
}

/// 当前光标位置最内层未闭合函数的签名与当前参数索引（跳过 `"..."` 字符串字面量）。
fn formula_open_signature_at(buf: &str, caret_chars: usize) -> Option<(String, usize)> {
    if !buf.starts_with('=') {
        return None;
    }
    let caret = byte_index_of_char(buf, caret_chars);
    let bytes = buf.as_bytes();
    let mut i = 0usize;
    let mut in_str = false;
    // 栈帧：(左括号字节偏移, 该层已见逗号数)
    let mut stack: Vec<(usize, usize)> = Vec::new();
    while i < caret {
        let b = bytes[i];
        if in_str {
            if b == b'"' {
                if i + 1 < caret && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'(' => stack.push((i, 0)),
            b')' => {
                let _ = stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    // 从最内层向上找最近一个带合法函数名的左括号（跳过纯分组括号 `(a+b)`）
    for &(open, arg_idx) in stack.iter().rev() {
        let mut start = open;
        while start > 0 {
            let Some(ch) = buf[..start].chars().next_back() else { break };
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
                start -= ch.len_utf8();
            } else {
                break;
            }
        }
        if start == open {
            continue;
        }
        let name = buf[start..open].to_ascii_uppercase();
        if sheetcore::formula::functions::implemented().iter().any(|f| *f == name) {
            return Some((formula_signature(&name), arg_idx));
        }
    }
    None
}

/// 函数名匹配分级：前缀 0、词内连续 1、子序列 2（如 VLO → VLOOKUP），不匹配返回 None
fn func_match_tier(name: &str, token_upper: &str) -> Option<u8> {
    if name.starts_with(token_upper) {
        return Some(0);
    }
    if name.contains(token_upper) {
        return Some(1);
    }
    let mut it = name.chars();
    if token_upper.chars().all(|c| it.any(|n| n == c)) {
        return Some(2);
    }
    None
}

pub(super) fn formula_help_at(buf: &str, caret_chars: usize) -> Option<FormulaHelp> {
    let mut out = FormulaHelp::default();
    if let Some((start, end)) = formula_token_span_at(buf, caret_chars) {
        let caret = byte_index_of_char(buf, caret_chars);
        let token = &buf[start..caret];
        if token.chars().count() >= 2 {
            let up = token.to_ascii_uppercase();
            let mut ranked: Vec<(u8, &'static str)> = Vec::new();
            for f in sheetcore::formula::functions::implemented() {
                if let Some(tier) = func_match_tier(f, &up) {
                    ranked.push((tier, f));
                }
            }
            // 0=前缀，1=词内连续，2=子序列；同级按长度、再按字典序
            ranked.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then(a.1.len().cmp(&b.1.len()))
                    .then(a.1.cmp(b.1))
            });
            for (_, f) in ranked {
                out.suggestions.push((f.to_string(), function_desc(f).to_string()));
            }
            out.suggestions.truncate(8);
            out.start = start;
            out.end = end;
            out.token = token.to_string();
        }
    }
    if let Some((sig, arg_idx)) = formula_open_signature_at(buf, caret_chars) {
        out.signature = Some(sig);
        out.active_arg = Some(arg_idx);
    }
    if out.suggestions.is_empty() && out.signature.is_none() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
pub(super) fn formula_help(buf: &str) -> Option<FormulaHelp> {
    formula_help_at(buf, buf.chars().count())
}

/// 候选列表的键盘选择：↑↓ 改变高亮，Enter/Tab 接受；返回要接受的索引。
pub(super) fn formula_help_keys(
    ctx: &egui::Context,
    help: &FormulaHelp,
    selected: &mut usize,
) -> Option<usize> {
    if help.suggestions.is_empty() {
        return None;
    }
    *selected = (*selected).min(help.suggestions.len().saturating_sub(1));
    let up = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
    let down = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
    if up {
        *selected = if *selected == 0 {
            help.suggestions.len() - 1
        } else {
            *selected - 1
        };
    }
    if down {
        *selected = (*selected + 1) % help.suggestions.len();
    }
    let accept = ctx.input_mut(|i| {
        i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
            || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
    });
    accept.then_some(*selected)
}

/// 向导里的「帮助」链接：Excel 自己也是跳官方文档
pub(super) fn help_url(name: &str) -> String {
    format!(
        "https://support.microsoft.com/zh-cn/search/results?query={}%20%E5%87%BD%E6%95%B0",
        name
    )
}

pub(super) fn apply_formula_pick(ed: &mut EditState, help: &FormulaHelp, idx: usize) {
    let Some(name) = help.selected(idx).map(str::to_string) else {
        return;
    };
    let end = help.end.max(help.start).min(ed.buf.len());
    if ed.buf.len() < help.start {
        return;
    }
    if debug_flags::formula_log() {
        eprintln!(
            "[formula] pick idx={} start={} end={} token={:?} name={:?} before={:?}",
            idx, help.start, end, help.token, name, ed.buf
        );
    }
    let already_has_paren = ed.buf[end..].starts_with('(');
    let insert = if already_has_paren {
        name.clone()
    } else {
        format!("{name}(")
    };
    ed.buf.replace_range(help.start..end, &insert);
    let new_caret = ed.buf[..help.start].chars().count()
        + insert.chars().count()
        + usize::from(already_has_paren);
    ed.caret = new_caret.min(ed.buf.chars().count());
    ed.caret_override = Some(ed.caret);
    ed.complete_idx = 0;
}

/// 在编辑框下方（放不下则上方）画候选/签名提示。
pub(super) fn draw_formula_help(
    ctx: &egui::Context,
    anchor: egui::Pos2,
    above: bool,
    help: &FormulaHelp,
    selected: usize,
) {
    let screen = ctx.screen_rect();
    let h = 22.0 * help.suggestions.len().min(8) as f32
        + if help.suggestions.is_empty() { 0.0 } else { 6.0 }
        + if help.signature.is_some() { 22.0 } else { 0.0 }
        + 8.0;
    let y = if above {
        (anchor.y - h - 2.0).max(screen.top() + 4.0)
    } else {
        anchor.y.min(screen.bottom() - h - 4.0)
    };
    egui::Area::new(egui::Id::new("formula_complete"))
        .fixed_pos(egui::pos2(anchor.x, y))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(250.0);
                ui.spacing_mut().item_spacing.y = 1.0;
                for (i, (name, desc)) in help.suggestions.iter().enumerate() {
                    let mut rich = egui::RichText::new(format!("{name}    {desc}")).size(11.5);
                    if i == selected {
                        rich = rich.background_color(egui::Color32::from_rgb(212, 228, 246));
                    }
                    ui.label(rich);
                }
                if let Some(sig) = &help.signature {
                    if !help.suggestions.is_empty() {
                        ui.separator();
                    }
                    let pal = theme::palette();
                    let normal_fmt = egui::TextFormat {
                        font_id: egui::FontId::proportional(11.0),
                        color: pal.text_muted,
                        ..Default::default()
                    };
                    let active_fmt = egui::TextFormat {
                        font_id: egui::FontId::proportional(11.5),
                        color: pal.accent,
                        ..Default::default()
                    };
                    let mut job = egui::text::LayoutJob::default();
                    if let (Some(arg_idx), Some(open), Some(close)) =
                        (help.active_arg, sig.find('('), sig.rfind(')'))
                    {
                        let inner = &sig[open + 1..close];
                        let parts: Vec<&str> = if inner.is_empty() {
                            Vec::new()
                        } else {
                            inner.split(',').collect()
                        };
                        let has_ellipsis = parts.iter().any(|p| p.trim() == "…");
                        let target_idx = if parts.is_empty() {
                            None
                        } else if arg_idx < parts.len() && parts[arg_idx].trim() != "…" {
                            Some(arg_idx)
                        } else if has_ellipsis {
                            // 可变长参数（如 SUM(number1,[number2],…)）：高亮最后一个实际参数位
                            parts.iter().rposition(|p| p.trim() != "…")
                        } else {
                            None
                        };
                        job.append(&sig[..=open], 0.0, normal_fmt.clone());
                        for (idx, p) in parts.iter().enumerate() {
                            if idx > 0 {
                                job.append(",", 0.0, normal_fmt.clone());
                            }
                            if target_idx == Some(idx) {
                                job.append(p, 0.0, active_fmt.clone());
                            } else {
                                job.append(p, 0.0, normal_fmt.clone());
                            }
                        }
                        job.append(&sig[close..], 0.0, normal_fmt);
                    } else {
                        job.append(sig, 0.0, normal_fmt);
                    }
                    ui.label(job);
                }
            });
        });
}

/// 判断公式字符串在 `caret`（字符下标）处是否刚位于 `=` 或运算符/分隔符之后，按方向键可进入「点选引用」模式。
pub(super) fn formula_expects_ref_at_caret(buf: &str, caret: usize) -> bool {
    if !buf.starts_with('=') {
        return false;
    }
    let bpos = byte_index_of_char(buf, caret);
    if is_inside_string_literal(buf, bpos) {
        return false;
    }
    let trimmed = buf[..bpos].trim_end();
    matches!(
        trimmed.chars().last(),
        Some('=' | '+' | '-' | '*' | '/' | '^' | '&' | '(' | ',' | ';' | ':' | '<' | '>')
    )
}

/// 解析外层公式调用 `=FUNC(arg1, arg2, ...)` 用于函数向导回填（正确跳过引号与嵌套括号）。
pub(super) fn parse_outer_formula_call(raw: &str) -> Option<(String, Vec<String>)> {
    let t = raw.trim();
    let body = t.strip_prefix('=')?.trim();
    let open = body.find('(')?;
    let name = body[..open].trim().to_ascii_uppercase();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
    {
        return None;
    }
    if !sheetcore::formula::functions::implemented()
        .iter()
        .any(|f| *f == name)
    {
        return None;
    }
    let bytes = body.as_bytes();
    let mut i = open + 1;
    let mut depth = 1i32;
    let mut brace = 0i32;
    let mut in_str = false;
    let mut arg_start = i;
    let mut args = Vec::new();
    let mut close_pos = None;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    args.push(body[arg_start..i].trim().to_string());
                    close_pos = Some(i);
                    break;
                }
            }
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b',' if depth == 1 && brace == 0 => {
                args.push(body[arg_start..i].trim().to_string());
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if let Some(cp) = close_pos {
        // 确保外层是单一函数调用而非 `=SUM(A1)+SUM(B1)`
        if !body[cp + 1..].trim().is_empty() {
            return None;
        }
    } else {
        // 未闭合括号也尽量回填已输入参数
        let tail = body[arg_start..].trim();
        if !tail.is_empty() || !args.is_empty() {
            args.push(tail.to_string());
        }
    }
    if args.len() == 1 && args[0].is_empty() {
        args.clear();
    }
    Some((name, args))
}
