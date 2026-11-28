//! 流式安全轻量级 Markdown 解析与渲染器（专供 AI 表格助手开放式阅读流）。
//!
//! 支持：
//! - 标题（`#` / `##` / `###`，严格要求 `#` 后带空白，避免把 `#REF!` / `#DIV/0!` 误判为标题）
//! - 行内富文本（`**粗体**`、`*斜体*`、`~~删除线~~`、`` `公式/代码徽章` ``、`[链接](url)`）
//! - 无序列表（`- ` / `* ` / `+ `）与有序列表（`1. `）悬挂缩进
//! - 围栏代码块（`` ``` `` / `~~~`），带语言标签与一键「复制」反馈，支持流式未闭合代码块
//! - Markdown 表格（`| a | b |` + 对齐分隔行），全宽自适应无横向撑宽
//! - 引用块（`> `）与水平分隔线（`---`）
//! - 全量走 `crate::theme::palette()`（浅色/护眼/深色/高对比），全整数逻辑字号（11/12/13/14/16）

use crate::theme::ThemePalette;
use crate::ui_kit;
use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Rect, Sense, Stroke, TextFormat, Vec2,
    text::LayoutJob,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MdBlock {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(String),
    CodeBlock {
        lang: String,
        code: String,
        closed: bool,
    },
    BulletList(Vec<String>),
    OrderedList(Vec<(String, String)>),
    Blockquote(Vec<String>),
    Table {
        headers: Vec<String>,
        alignments: Vec<TableAlign>,
        rows: Vec<Vec<String>>,
    },
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InlineSpan {
    Text(String),
    Bold(String),
    Italic(String),
    Strike(String),
    Code(String),
    Link { label: String, url: String },
}

/// 将任意（含流式未闭合）Markdown 文本解析为块级 AST；永不 panic。
pub(crate) fn parse_blocks(input: &str) -> Vec<MdBlock> {
    let lines: Vec<&str> = input.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // 1. 围栏代码块（``` 或 ~~~）
        if let Some((fence_ch, fence_len, lang)) = parse_fence_open(trimmed) {
            i += 1;
            let mut code_lines = Vec::new();
            let mut closed = false;
            while i < lines.len() {
                if is_fence_close(lines[i].trim(), fence_ch, fence_len) {
                    closed = true;
                    i += 1;
                    break;
                }
                code_lines.push(lines[i]);
                i += 1;
            }
            blocks.push(MdBlock::CodeBlock {
                lang: lang.to_owned(),
                code: code_lines.join("\n"),
                closed,
            });
            continue;
        }

        // 2. 水平分隔线（--- / *** / ___）
        if is_horizontal_rule(trimmed) {
            blocks.push(MdBlock::Rule);
            i += 1;
            continue;
        }

        // 3. ATX 标题（# / ## / ###；必须后跟空格，避免误伤 #REF! / #N/A）
        if let Some((level, text)) = parse_heading(trimmed) {
            blocks.push(MdBlock::Heading {
                level,
                text: text.to_owned(),
            });
            i += 1;
            continue;
        }

        // 4. Markdown 表格（当前行含 `|` 且下一行为合法分隔行）
        if i + 1 < lines.len()
            && raw.contains('|')
            && let Some(alignments) = parse_table_separator(lines[i + 1])
        {
            let mut headers = split_table_row(raw);
            let col_count = headers.len().max(alignments.len()).max(1);
            headers.resize(col_count, String::new());
            let mut aligns = alignments;
            aligns.resize(col_count, TableAlign::Left);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() {
                let row_trim = lines[i].trim();
                if row_trim.is_empty()
                    || !lines[i].contains('|')
                    || parse_fence_open(row_trim).is_some()
                    || parse_heading(row_trim).is_some()
                {
                    break;
                }
                let mut cells = split_table_row(lines[i]);
                cells.resize(col_count, String::new());
                cells.truncate(col_count);
                rows.push(cells);
                i += 1;
            }
            blocks.push(MdBlock::Table {
                headers,
                alignments: aligns,
                rows,
            });
            continue;
        }

        // 5. 引用块（> ...）
        if trimmed.starts_with('>') {
            let mut quote_lines = Vec::new();
            while i < lines.len() {
                let qt = lines[i].trim();
                if let Some(rest) = qt.strip_prefix('>') {
                    quote_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::Blockquote(quote_lines));
            continue;
        }

        // 6. 无序列表（- / * / + / •）
        if parse_bullet_item(raw).is_some() {
            let mut items: Vec<String> = Vec::new();
            while i < lines.len() {
                if let Some(item_text) = parse_bullet_item(lines[i]) {
                    items.push(item_text.to_owned());
                    i += 1;
                } else if (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !lines[i].trim().is_empty()
                    && !items.is_empty()
                    && parse_ordered_item(lines[i]).is_none()
                    && parse_fence_open(lines[i].trim()).is_none()
                {
                    if let Some(last) = items.last_mut() {
                        last.push('\n');
                        last.push_str(lines[i].trim());
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::BulletList(items));
            continue;
        }

        // 7. 有序列表（1. / 2.）
        if parse_ordered_item(raw).is_some() {
            let mut items: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                if let Some((num, item_text)) = parse_ordered_item(lines[i]) {
                    items.push((num, item_text.to_owned()));
                    i += 1;
                } else if (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !lines[i].trim().is_empty()
                    && !items.is_empty()
                    && parse_bullet_item(lines[i]).is_none()
                    && parse_fence_open(lines[i].trim()).is_none()
                {
                    if let Some((_, last)) = items.last_mut() {
                        last.push('\n');
                        last.push_str(lines[i].trim());
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::OrderedList(items));
            continue;
        }

        // 8. 普通段落（保留单换行，贴合中文大模型分段习惯）
        let mut para_lines = vec![raw.trim_end()];
        i += 1;
        while i < lines.len() {
            let next = lines[i];
            let nt = next.trim();
            if nt.is_empty()
                || parse_fence_open(nt).is_some()
                || is_horizontal_rule(nt)
                || parse_heading(nt).is_some()
                || nt.starts_with('>')
                || parse_bullet_item(next).is_some()
                || parse_ordered_item(next).is_some()
                || (i + 1 < lines.len()
                    && next.contains('|')
                    && parse_table_separator(lines[i + 1]).is_some())
            {
                break;
            }
            para_lines.push(next.trim_end());
            i += 1;
        }
        blocks.push(MdBlock::Paragraph(para_lines.join("\n")));
    }

    blocks
}

fn parse_fence_open(trimmed: &str) -> Option<(char, usize, &str)> {
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == first).count();
    if count < 3 {
        return None;
    }
    let rest = trimmed[count..].trim();
    // 行内反引号如 ```foo``` 不应被当成围栏块
    if first == '`' && rest.contains('`') {
        return None;
    }
    Some((first, count, rest))
}

fn is_fence_close(trimmed: &str, ch: char, min_len: usize) -> bool {
    let count = trimmed.chars().take_while(|&c| c == ch).count();
    count >= min_len && trimmed[count..].trim().is_empty()
}

fn is_horizontal_rule(trimmed: &str) -> bool {
    let mut marker: Option<char> = None;
    let mut count = 0usize;
    for ch in trimmed.chars() {
        if ch == ' ' || ch == '\t' {
            continue;
        }
        if !matches!(ch, '-' | '*' | '_') {
            return false;
        }
        if let Some(m) = marker {
            if m != ch {
                return false;
            }
        } else {
            marker = Some(ch);
        }
        count += 1;
    }
    count >= 3
}

fn parse_heading(trimmed: &str) -> Option<(u8, &str)> {
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &trimmed[hashes..];
    if !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    let body = after.trim().trim_end_matches('#').trim_end();
    if body.is_empty() {
        return None;
    }
    Some((hashes.min(3) as u8, body))
}

fn parse_bullet_item(raw: &str) -> Option<&str> {
    let trimmed = raw.trim_start();
    if raw.len() - trimmed.len() > 4 {
        return None;
    }
    for prefix in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest.trim_end());
        }
    }
    None
}

fn parse_ordered_item(raw: &str) -> Option<(String, &str)> {
    let trimmed = raw.trim_start();
    if raw.len() - trimmed.len() > 4 {
        return None;
    }
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 4 {
        return None;
    }
    let rest = &trimmed[digits..];
    let after = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))?;
    Some((format!("{}.", &trimmed[..digits]), after.trim_end()))
}

fn split_table_row(line: &str) -> Vec<String> {
    let mut s = line.trim();
    if let Some(stripped) = s.strip_prefix('|') {
        s = stripped;
    }
    if let Some(stripped) = s.strip_suffix('|') {
        s = stripped;
    }
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(ch);
            }
            '|' if !in_code => {
                cells.push(cur.trim().to_owned());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    cells.push(cur.trim().to_owned());
    cells
}

fn parse_table_separator(line: &str) -> Option<Vec<TableAlign>> {
    if !line.contains('-') || !line.contains('|') {
        return None;
    }
    let cells = split_table_row(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in cells {
        let c = cell.trim();
        if c.is_empty() {
            return None;
        }
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let core = c.trim_start_matches(':').trim_end_matches(':');
        if core.is_empty() || !core.bytes().all(|b| b == b'-') {
            return None;
        }
        aligns.push(match (left, right) {
            (true, true) => TableAlign::Center,
            (false, true) => TableAlign::Right,
            _ => TableAlign::Left,
        });
    }
    Some(aligns)
}

/// 解析单行/段落内的行内 Markdown 语法；未闭合标记安全降级为普通文本。
pub(crate) fn parse_inlines(text: &str) -> Vec<InlineSpan> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    let flush_plain = |plain: &mut String, out: &mut Vec<InlineSpan>| {
        if !plain.is_empty() {
            out.push(InlineSpan::Text(std::mem::take(plain)));
        }
    };

    while i < bytes.len() {
        // 1. 行内代码 `...`
        if bytes[i] == b'`' {
            let tick_count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            let content_start = i + tick_count;
            if let Some(rel_end) = find_closing_ticks(&text[content_start..], tick_count) {
                let code = &text[content_start..content_start + rel_end];
                if !code.is_empty() {
                    flush_plain(&mut plain, &mut out);
                    out.push(InlineSpan::Code(code.trim().to_owned()));
                    i = content_start + rel_end + tick_count;
                    continue;
                }
            }
            // 未闭合（如流式输出中）：原样输出，避免文字闪烁消失
            plain.push_str(&text[i..content_start]);
            i = content_start;
            continue;
        }

        // 2. 粗体 **...**
        if bytes[i..].starts_with(b"**") {
            let content_start = i + 2;
            if content_start < bytes.len()
                && !text[content_start..].starts_with(char::is_whitespace)
                && let Some(rel_end) = find_closing_double(&text[content_start..], "**")
            {
                let inner = &text[content_start..content_start + rel_end];
                if !inner.is_empty() && !inner.ends_with(char::is_whitespace) {
                    flush_plain(&mut plain, &mut out);
                    out.push(InlineSpan::Bold(inner.to_owned()));
                    i = content_start + rel_end + 2;
                    continue;
                }
            }
            plain.push_str("**");
            i += 2;
            continue;
        }

        // 3. 删除线 ~~...~~
        if bytes[i..].starts_with(b"~~") {
            let content_start = i + 2;
            if content_start < bytes.len()
                && !text[content_start..].starts_with(char::is_whitespace)
                && let Some(rel_end) = find_closing_double(&text[content_start..], "~~")
            {
                let inner = &text[content_start..content_start + rel_end];
                if !inner.is_empty() && !inner.ends_with(char::is_whitespace) {
                    flush_plain(&mut plain, &mut out);
                    out.push(InlineSpan::Strike(inner.to_owned()));
                    i = content_start + rel_end + 2;
                    continue;
                }
            }
            plain.push_str("~~");
            i += 2;
            continue;
        }

        // 4. 斜体 *...*（避免误伤数学/公式乘法如 B2*C2*1.13）
        if bytes[i] == b'*' && !bytes[i..].starts_with(b"**") {
            let prev_alnum = text[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric());
            let content_start = i + 1;
            let next_alnum = text[content_start..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric());
            if !(prev_alnum && next_alnum)
                && content_start < bytes.len()
                && !text[content_start..].starts_with(char::is_whitespace)
                && let Some(rel_end) = find_closing_single_star(&text[content_start..])
            {
                let inner = &text[content_start..content_start + rel_end];
                if !inner.is_empty() && !inner.ends_with(char::is_whitespace) {
                    flush_plain(&mut plain, &mut out);
                    out.push(InlineSpan::Italic(inner.to_owned()));
                    i = content_start + rel_end + 1;
                    continue;
                }
            }
            plain.push('*');
            i += 1;
            continue;
        }

        // 5. 链接 [label](url)
        if bytes[i] == b'[' {
            if let Some((label, url, total_len)) = parse_inline_link(&text[i..]) {
                flush_plain(&mut plain, &mut out);
                out.push(InlineSpan::Link {
                    label: label.to_owned(),
                    url: url.to_owned(),
                });
                i += total_len;
                continue;
            }
            plain.push('[');
            i += 1;
            continue;
        }

        // 6. 反斜杠转义常见 Markdown 标点
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let next_b = bytes[i + 1];
            if matches!(next_b, b'*' | b'`' | b'_' | b'[' | b']' | b'\\' | b'~') {
                plain.push(next_b as char);
                i += 2;
                continue;
            }
        }

        let ch = text[i..].chars().next().unwrap();
        plain.push(ch);
        i += ch.len_utf8();
    }

    flush_plain(&mut plain, &mut out);
    out
}

fn find_closing_ticks(s: &str, count: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if run == count {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

fn find_closing_double(s: &str, delim: &str) -> Option<usize> {
    let db = delim.as_bytes();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i + 1 < bytes.len() {
        if bytes[i] == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if !in_code && &bytes[i..i + 2] == db {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_closing_single_star(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if !in_code && bytes[i] == b'*' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                i += 2;
                continue;
            }
            if i > 0 && bytes[i - 1] == b'\\' {
                i += 1;
                continue;
            }
            return Some(i);
        }
        if bytes[i] == b'\n' {
            return None;
        }
        i += 1;
    }
    None
}

fn parse_inline_link(s: &str) -> Option<(&str, &str, usize)> {
    let rest = s.strip_prefix('[')?;
    let close_bracket = rest.find(']')?;
    let label = &rest[..close_bracket];
    if label.is_empty() || label.contains('\n') {
        return None;
    }
    let after = &rest[close_bracket + 1..];
    let url_part = after.strip_prefix('(')?;
    let close_paren = url_part.find(')')?;
    let url = url_part[..close_paren].trim();
    if url.is_empty() || url.contains('\n') || url.contains(' ') {
        return None;
    }
    let total = 1 + close_bracket + 1 + 1 + close_paren + 1;
    Some((label, url, total))
}

fn line_height_for(size: f32) -> f32 {
    if size >= 16.0 {
        24.0
    } else if size >= 14.0 {
        21.0
    } else if size >= 13.0 {
        20.0
    } else {
        18.0
    }
}

fn build_inline_job(
    ctx: &egui::Context,
    text: &str,
    size: f32,
    base_color: Color32,
    pal: ThemePalette,
    max_width: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_width.max(40.0);

    let lh = Some(line_height_for(size));
    let normal_fmt = TextFormat {
        font_id: FontId::proportional(size),
        color: base_color,
        line_height: lh,
        ..Default::default()
    };
    let bold_fmt = TextFormat {
        font_id: ui_kit::medium_in(ctx, size),
        color: pal.text,
        line_height: lh,
        ..Default::default()
    };
    let italic_fmt = match ui_kit::ui_italic_in(ctx, size) {
        Some(fid) => TextFormat {
            font_id: fid,
            color: base_color,
            line_height: lh,
            ..Default::default()
        },
        None => TextFormat {
            font_id: FontId::proportional(size),
            color: base_color,
            italics: true,
            line_height: lh,
            ..Default::default()
        },
    };
    let strike_fmt = TextFormat {
        font_id: FontId::proportional(size),
        color: pal.text_weak,
        strikethrough: Stroke::new(1.0_f32, pal.text_weak),
        line_height: lh,
        ..Default::default()
    };
    let code_size = if size <= 12.0 { 11.0 } else { 12.0 };
    let code_bg = if pal.theme == crate::theme::AppTheme::HighContrast {
        pal.surface_subtle()
    } else {
        pal.hover
    };
    let code_fg = if pal.theme == crate::theme::AppTheme::HighContrast {
        pal.accent
    } else {
        pal.brand_ink()
    };
    let code_fmt = TextFormat {
        font_id: FontId::new(code_size, FontFamily::Monospace),
        color: code_fg,
        background: code_bg,
        line_height: lh,
        ..Default::default()
    };
    let link_fmt = TextFormat {
        font_id: FontId::proportional(size),
        color: pal.info_fg,
        underline: Stroke::new(1.0_f32, pal.info_fg.gamma_multiply(0.65)),
        line_height: lh,
        ..Default::default()
    };

    for span in parse_inlines(text) {
        match span {
            InlineSpan::Text(t) => job.append(&t, 0.0, normal_fmt.clone()),
            InlineSpan::Bold(t) => job.append(&t, 0.0, bold_fmt.clone()),
            InlineSpan::Italic(t) => job.append(&t, 0.0, italic_fmt.clone()),
            InlineSpan::Strike(t) => job.append(&t, 0.0, strike_fmt.clone()),
            InlineSpan::Code(code) => {
                // 用窄空格做微内边距，让行内公式/代码徽章不紧贴文字边缘
                job.append(&format!("\u{2009}{code}\u{2009}"), 0.0, code_fmt.clone());
            }
            InlineSpan::Link { label, .. } => job.append(&label, 0.0, link_fmt.clone()),
        }
    }

    job
}

fn render_inline_paragraph(
    ui: &mut egui::Ui,
    text: &str,
    size: f32,
    color: Color32,
    pal: ThemePalette,
) {
    let max_w = ui.available_width().max(40.0);
    let job = build_inline_job(ui.ctx(), text, size, color, pal, max_w);
    ui.add(egui::Label::new(job).selectable(true).wrap());
}

/// 在开放式 AI 对话画布上渲染完整 Markdown 内容（支持流式增量更新，零宽度漂移）。
pub(crate) fn render_markdown(
    ui: &mut egui::Ui,
    text: &str,
    base_size: f32,
    color: Color32,
    id_salt: impl std::hash::Hash,
    pal: ThemePalette,
) {
    let blocks = parse_blocks(text);
    if blocks.is_empty() {
        return;
    }
    let root_id = ui.make_persistent_id(id_salt);
    let avail_w = ui.available_width().max(80.0);

    ui.scope(|ui| {
        ui.set_max_width(avail_w);
        ui.spacing_mut().item_spacing.y = 6.0;

        for (bi, block) in blocks.iter().enumerate() {
            match block {
                MdBlock::Heading { level, text } => {
                    if bi > 0 {
                        ui.add_space(if *level == 1 { 6.0 } else { 4.0 });
                    }
                    let (h_size, font) = match level {
                        1 => (16.0, ui_kit::ui_bold_in(ui.ctx(), 16.0)),
                        2 => (14.0, ui_kit::medium_in(ui.ctx(), 14.0)),
                        _ => (13.0, ui_kit::medium_in(ui.ctx(), 13.0)),
                    };
                    let mut job = LayoutJob::default();
                    job.wrap.max_width = ui.available_width().max(40.0);
                    let fmt = TextFormat {
                        font_id: font,
                        color: pal.text,
                        line_height: Some(line_height_for(h_size)),
                        ..Default::default()
                    };
                    // 标题内同样兼容 ** / ` 剥离与代码徽章
                    for span in parse_inlines(text) {
                        match span {
                            InlineSpan::Code(code) => {
                                let code_fmt = TextFormat {
                                    font_id: FontId::new(
                                        (h_size - 1.0).max(12.0),
                                        FontFamily::Monospace,
                                    ),
                                    color: pal.brand_ink(),
                                    background: pal.hover,
                                    line_height: Some(line_height_for(h_size)),
                                    ..Default::default()
                                };
                                job.append(&format!("\u{2009}{code}\u{2009}"), 0.0, code_fmt);
                            }
                            InlineSpan::Text(t)
                            | InlineSpan::Bold(t)
                            | InlineSpan::Italic(t)
                            | InlineSpan::Strike(t)
                            | InlineSpan::Link { label: t, .. } => {
                                job.append(&t, 0.0, fmt.clone());
                            }
                        }
                    }
                    ui.add(egui::Label::new(job).selectable(true).wrap());
                }

                MdBlock::Paragraph(p) => {
                    render_inline_paragraph(ui, p, base_size, color, pal);
                }

                MdBlock::BulletList(items) => {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for item in items {
                            let row_w = ui.available_width().max(60.0);
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let (mr, _) = ui.allocate_exact_size(
                                    Vec2::new(14.0, line_height_for(base_size)),
                                    Sense::hover(),
                                );
                                ui.painter().circle_filled(
                                    egui::pos2(mr.left() + 5.0, mr.center().y),
                                    2.0,
                                    pal.text_weak,
                                );
                                ui.vertical(|ui| {
                                    ui.set_max_width((row_w - 14.0).max(40.0));
                                    render_inline_paragraph(ui, item, base_size, color, pal);
                                });
                            });
                        }
                    });
                }

                MdBlock::OrderedList(items) => {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for (marker, item) in items {
                            let row_w = ui.available_width().max(60.0);
                            let marker_w = 20.0;
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let (mr, _) = ui.allocate_exact_size(
                                    Vec2::new(marker_w, line_height_for(base_size)),
                                    Sense::hover(),
                                );
                                ui.painter().text(
                                    egui::pos2(mr.left() + 1.0, mr.center().y),
                                    Align2::LEFT_CENTER,
                                    marker,
                                    ui_kit::medium_in(ui.ctx(), 12.0),
                                    pal.text_weak,
                                );
                                ui.vertical(|ui| {
                                    ui.set_max_width((row_w - marker_w).max(40.0));
                                    render_inline_paragraph(ui, item, base_size, color, pal);
                                });
                            });
                        }
                    });
                }

                MdBlock::CodeBlock { lang, code, .. } => {
                    render_code_block(ui, root_id.with(("code", bi)), lang, code, pal, avail_w);
                }

                MdBlock::Table {
                    headers,
                    alignments,
                    rows,
                } => {
                    render_table(ui, headers, alignments, rows, pal, avail_w);
                }

                MdBlock::Blockquote(lines) => {
                    let quote_text = lines.join("\n");
                    let q_w = ui.available_width().max(60.0);
                    let resp = ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.set_max_width((q_w - 12.0).max(40.0));
                            render_inline_paragraph(ui, &quote_text, 12.0, pal.text_weak, pal);
                        });
                    });
                    let bar = Rect::from_min_max(
                        egui::pos2(resp.response.rect.left() + 1.0, resp.response.rect.top() + 2.0),
                        egui::pos2(
                            resp.response.rect.left() + 3.5,
                            resp.response.rect.bottom() - 2.0,
                        ),
                    );
                    ui.painter().rect_filled(bar, 1.5, pal.accent_border());
                }

                MdBlock::Rule => {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(avail_w, 8.0), Sense::hover());
                    let y = r.center().y;
                    ui.painter().line_segment(
                        [egui::pos2(r.left(), y), egui::pos2(r.right(), y)],
                        Stroke::new(1.0_f32, pal.chrome_separator()),
                    );
                }
            }
        }
    });
}

fn render_code_block(
    ui: &mut egui::Ui,
    block_id: egui::Id,
    lang: &str,
    code: &str,
    pal: ThemePalette,
    avail_w: f32,
) {
    let copied_id = block_id.with("copied_until");
    let now = ui.ctx().input(|i| i.time);
    let copied_until = ui.ctx().data(|d| d.get_temp::<f64>(copied_id).unwrap_or(0.0));
    let show_copied = copied_until > now;
    if show_copied && !crate::debug_flags::shot() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(180));
    }

    egui::Frame::NONE
        .fill(pal.surface_subtle())
        .stroke(Stroke::new(1.0_f32, pal.card_border))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::ZERO)
        .show(ui, |ui| {
            ui.set_width((avail_w - 2.0).max(60.0));
            ui.spacing_mut().item_spacing = Vec2::ZERO;

            // 顶部细栏：语言标签 + 一键复制按钮
            let hdr_w = ui.available_width();
            let (hdr_r, _) = ui.allocate_exact_size(Vec2::new(hdr_w, 24.0), Sense::hover());
            let lang_lbl = if lang.is_empty() { "代码 / 公式" } else { lang };
            ui.painter().text(
                egui::pos2(hdr_r.left() + 10.0, hdr_r.center().y),
                Align2::LEFT_CENTER,
                lang_lbl,
                FontId::proportional(11.0),
                pal.text_muted,
            );
            let copy_lbl = if show_copied { "✓ 已复制" } else { "复制" };
            let copy_w = if show_copied { 52.0 } else { 36.0 };
            let copy_rect = Rect::from_min_size(
                egui::pos2(hdr_r.right() - copy_w - 6.0, hdr_r.top() + 3.0),
                Vec2::new(copy_w, 18.0),
            );
            let copy_resp = ui.interact(copy_rect, block_id.with("copy_btn"), Sense::click());
            let hover_t = if crate::debug_flags::shot() {
                if copy_resp.hovered() { 1.0 } else { 0.0 }
            } else {
                ui.ctx().animate_bool_with_time_and_easing(
                    block_id.with("copy_hover"),
                    copy_resp.hovered(),
                    0.11,
                    egui::emath::easing::cubic_out,
                )
            };
            if hover_t > 0.01 {
                let bg = Color32::from_rgba_unmultiplied(
                    pal.hover.r(),
                    pal.hover.g(),
                    pal.hover.b(),
                    (pal.hover.a() as f32 * hover_t).round() as u8,
                );
                ui.painter().rect_filled(copy_rect, 4.0, bg);
            }
            ui.painter().text(
                copy_rect.center(),
                Align2::CENTER_CENTER,
                copy_lbl,
                FontId::proportional(11.0),
                if show_copied {
                    pal.accent
                } else if copy_resp.hovered() {
                    pal.text
                } else {
                    pal.text_weak
                },
            );
            if copy_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if copy_resp.clicked() {
                ui.ctx().copy_text(code.to_owned());
                ui.ctx().data_mut(|d| d.insert_temp(copied_id, now + 1.5));
            }
            ui.painter().line_segment(
                [
                    egui::pos2(hdr_r.left(), hdr_r.bottom()),
                    egui::pos2(hdr_r.right(), hdr_r.bottom()),
                ],
                Stroke::new(1.0_f32, pal.chrome_separator()),
            );

            // 等宽代码区（支持横向滚动防撑宽）
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt(block_id.with("hscroll"))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let mut job = LayoutJob::default();
                            job.wrap.max_width = f32::INFINITY;
                            job.append(
                                code,
                                0.0,
                                TextFormat {
                                    font_id: FontId::new(12.0, FontFamily::Monospace),
                                    color: pal.text,
                                    line_height: Some(18.0),
                                    ..Default::default()
                                },
                            );
                            ui.add(egui::Label::new(job).selectable(true));
                        });
                });
        });
}

fn render_table(
    ui: &mut egui::Ui,
    headers: &[String],
    alignments: &[TableAlign],
    rows: &[Vec<String>],
    pal: ThemePalette,
    avail_w: f32,
) {
    let ncols = headers.len().max(1);
    let inner_w = (avail_w - 2.0).max(80.0);

    // 按内容显示宽度比例分配列宽，保底最小列宽且严格不超过 inner_w
    let mut weights = vec![4usize; ncols];
    let display_len = |s: &str| -> usize {
        s.chars()
            .map(|c| if c.is_ascii() { 1 } else { 2 })
            .sum::<usize>()
            .clamp(4, 24)
    };
    for (c, h) in headers.iter().enumerate() {
        weights[c] = weights[c].max(display_len(h));
    }
    for row in rows {
        for (c, cell) in row.iter().take(ncols).enumerate() {
            weights[c] = weights[c].max(display_len(cell));
        }
    }
    let total_weight: f32 = weights.iter().map(|&w| w as f32).sum::<f32>().max(1.0);
    let min_col = (inner_w / ncols as f32).min(46.0);
    let mut col_ws: Vec<f32> = weights
        .iter()
        .map(|&w| (inner_w * (w as f32 / total_weight)).max(min_col))
        .collect();
    let sum_w: f32 = col_ws.iter().sum();
    if sum_w > inner_w {
        let scale = inner_w / sum_w;
        for w in &mut col_ws {
            *w *= scale;
        }
    }

    egui::Frame::NONE
        .fill(pal.dialog_bg)
        .stroke(Stroke::new(1.0_f32, pal.card_border))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::ZERO)
        .show(ui, |ui| {
            ui.set_width(inner_w);
            ui.spacing_mut().item_spacing = Vec2::ZERO;

            // 表头行
            egui::Frame::NONE
                .fill(pal.surface_subtle())
                .corner_radius(egui::CornerRadius {
                    nw: 6,
                    ne: 6,
                    sw: 0,
                    se: 0,
                })
                .inner_margin(egui::Margin::symmetric(0, 4))
                .show(ui, |ui| {
                    ui.set_width(inner_w);
                    paint_table_row(ui, headers, alignments, &col_ws, true, pal);
                });

            // 数据行
            for row in rows {
                let (sep_r, _) = ui.allocate_exact_size(Vec2::new(inner_w, 1.0), Sense::hover());
                ui.painter().rect_filled(sep_r, 0.0, pal.chrome_separator());
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(0, 4))
                    .show(ui, |ui| {
                        ui.set_width(inner_w);
                        paint_table_row(ui, row, alignments, &col_ws, false, pal);
                    });
            }
        });
}

fn paint_table_row(
    ui: &mut egui::Ui,
    cells: &[String],
    alignments: &[TableAlign],
    col_ws: &[f32],
    is_header: bool,
    pal: ThemePalette,
) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (ci, &cw) in col_ws.iter().enumerate() {
            let cell_txt = cells.get(ci).map(String::as_str).unwrap_or("");
            let align = alignments.get(ci).copied().unwrap_or(TableAlign::Left);
            let layout_align = match align {
                TableAlign::Left => egui::Align::Min,
                TableAlign::Center => egui::Align::Center,
                TableAlign::Right => egui::Align::Max,
            };
            ui.allocate_ui_with_layout(
                Vec2::new(cw, 0.0),
                egui::Layout::top_down(layout_align),
                |ui| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(6, 0))
                        .show(ui, |ui| {
                            let content_w = (cw - 12.0).max(20.0);
                            if is_header {
                                let mut job = LayoutJob::default();
                                job.wrap.max_width = content_w;
                                job.halign = layout_align;
                                job.append(
                                    cell_txt,
                                    0.0,
                                    TextFormat {
                                        font_id: ui_kit::medium_in(ui.ctx(), 12.0),
                                        color: pal.text,
                                        line_height: Some(18.0),
                                        ..Default::default()
                                    },
                                );
                                ui.add(egui::Label::new(job).selectable(true).wrap());
                            } else {
                                let mut job = build_inline_job(
                                    ui.ctx(),
                                    cell_txt,
                                    12.0,
                                    pal.text,
                                    pal,
                                    content_w,
                                );
                                job.halign = layout_align;
                                ui.add(egui::Label::new(job).selectable(true).wrap());
                            }
                        });
                },
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{AppTheme, palette_for};

    #[test]
    fn parses_headings_lists_code_tables_quotes_rules_and_excel_errors() {
        let md = "\
# 主标题
## 部门摘要
### 详细说明

普通段落含 `#REF!` 与 #DIV/0! 误差值，以及 **加粗**、*斜体*、`=SUM(B2:B20)` 和 [帮助](https://example.com)。

- 第一项：检查 `Sheet1!A1`
- 第二项：确认公式 B2*C2*1.13 不误斜体

1. 先读取概览
2. 再写入结果

```excel
=IF(C2=0, 0, (C2-D2)/C2)
```

| 部门 | 营收 | 状态 |
| :--- | ---: | :---: |
| 华东区 | 482.0 | 达标 |
| 西北区 | 154.0 | **预警** |

> 提示：所有写入均支持一键撤销。

---
";
        let blocks = parse_blocks(md);
        assert_eq!(
            blocks[0],
            MdBlock::Heading {
                level: 1,
                text: "主标题".into()
            }
        );
        assert_eq!(
            blocks[1],
            MdBlock::Heading {
                level: 2,
                text: "部门摘要".into()
            }
        );
        assert_eq!(
            blocks[2],
            MdBlock::Heading {
                level: 3,
                text: "详细说明".into()
            }
        );
        assert!(matches!(&blocks[3], MdBlock::Paragraph(p) if p.contains("#DIV/0!")));
        assert!(matches!(&blocks[4], MdBlock::BulletList(items) if items.len() == 2));
        assert!(matches!(&blocks[5], MdBlock::OrderedList(items) if items.len() == 2 && items[0].0 == "1."));
        assert_eq!(
            blocks[6],
            MdBlock::CodeBlock {
                lang: "excel".into(),
                code: "=IF(C2=0, 0, (C2-D2)/C2)".into(),
                closed: true,
            }
        );
        match &blocks[7] {
            MdBlock::Table {
                headers,
                alignments,
                rows,
            } => {
                assert_eq!(headers, &["部门", "营收", "状态"]);
                assert_eq!(
                    alignments,
                    &[TableAlign::Left, TableAlign::Right, TableAlign::Center]
                );
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[1][2], "**预警**");
            }
            other => panic!("expected Table, got {other:?}"),
        }
        assert_eq!(
            blocks[8],
            MdBlock::Blockquote(vec!["提示：所有写入均支持一键撤销。".into()])
        );
        assert_eq!(blocks[9], MdBlock::Rule);
    }

    #[test]
    fn parses_inline_spans_and_handles_unclosed_streaming_tokens_safely() {
        let spans = parse_inlines(
            "合计 **¥1,280**，*华东* 公式 `=SUM(B2:B20)`，链接 [文档](https://cellan.local)，算式 B2*C2*2",
        );
        assert!(spans.contains(&InlineSpan::Bold("¥1,280".into())));
        assert!(spans.contains(&InlineSpan::Italic("华东".into())));
        assert!(spans.contains(&InlineSpan::Code("=SUM(B2:B20)".into())));
        assert!(spans.contains(&InlineSpan::Link {
            label: "文档".into(),
            url: "https://cellan.local".into()
        }));
        assert!(
            spans
                .iter()
                .any(|s| matches!(s, InlineSpan::Text(t) if t.contains("B2*C2*2"))),
            "单元格乘法不应被误解析为斜体: {spans:?}"
        );

        // 流式未闭合语法不能 panic 且必须保留已收到的文本
        let partial_inline = parse_inlines("正在输出 **未闭合加粗 与 `=SUM(A1:A");
        let joined: String = partial_inline
            .iter()
            .map(|s| match s {
                InlineSpan::Text(t)
                | InlineSpan::Bold(t)
                | InlineSpan::Italic(t)
                | InlineSpan::Strike(t)
                | InlineSpan::Code(t)
                | InlineSpan::Link { label: t, .. } => t.as_str(),
            })
            .collect();
        assert!(joined.contains("未闭合加粗"), "{joined}");
        assert!(joined.contains("=SUM(A1:A"), "{joined}");

        let partial_fence = parse_blocks("```excel\n=SUM(B2:B20)");
        assert_eq!(
            partial_fence,
            vec![MdBlock::CodeBlock {
                lang: "excel".into(),
                code: "=SUM(B2:B20)".into(),
                closed: false,
            }]
        );
    }

    #[test]
    fn markdown_renderer_renders_cleanly_across_all_themes_without_overflow() {
        let md = "\
## Q1 汇总

- 营收使用 `=SUM(C2:C5)`，共 **1,280 万**
- 毛利率 *31.4%*

```excel
=IF(C2=0, 0, (C2-D2)/C2)
```

| 部门 | 营收 | 状态 |
| :--- | ---: | :---: |
| 华东区 | 482.0 | 达标 |

> 支持一键撤销

---
";
        let ctx = egui::Context::default();
        for theme in AppTheme::ALL {
            let pal = palette_for(theme);
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(360.0, 600.0))),
                ..Default::default()
            };
            let out = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(340.0);
                    render_markdown(ui, md, 13.0, pal.text, ("md_test", theme as u8), pal);
                    assert!(
                        ui.min_rect().width() <= 346.0,
                        "{theme:?} markdown overflowed width: {}",
                        ui.min_rect().width()
                    );
                });
            });
            assert!(!out.shapes.is_empty(), "{theme:?} should emit shapes");
        }
    }
}
