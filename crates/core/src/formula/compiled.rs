//! Bounded syntax-only caches. Syntax never captures a workbook, evaluation state or a date system.
use super::{ErrKind, FValue, Node, RefExpr, bin_scalar, parse, parse_a1, parse_normalized};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{BuildHasher, RandomState},
    sync::{Arc, LazyLock, RwLock},
};

type Parsed = Result<Arc<Node>, ErrKind>;
const MAX_SOURCE: usize = 2048;
const SHARDS: usize = 32;
const PER_SHARD: usize = 1024;

#[derive(Default)]
struct Cache {
    exact: HashMap<String, Parsed>,
    relative: HashMap<Vec<u8>, Arc<Template>>,
}

static HASH: LazyLock<RandomState> = LazyLock::new(RandomState::new);
static CACHE: LazyLock<[RwLock<Cache>; SHARDS]> =
    LazyLock::new(|| std::array::from_fn(|_| RwLock::new(Cache::default())));

fn shard<T: std::hash::Hash + ?Sized>(key: &T) -> &'static RwLock<Cache> {
    &CACHE[HASH.hash_one(key) as usize % SHARDS]
}

struct Template {
    node: Arc<Node>,
    row: u32,
    col: u32,
    scalar: bool,
}

#[derive(Default)]
struct LastTemplate {
    key: Vec<u8>,
    scratch: Vec<u8>,
    template: Option<Arc<Template>>,
}

thread_local! { static LAST: RefCell<LastTemplate> = RefCell::new(LastTemplate::default()); }

fn parse_body(src: &str) -> Parsed {
    let norm = crate::normalize_formula_cow(src);
    let rows = crate::expand_whole_rows_cow(&norm);
    let cols = crate::expand_whole_cols_cow(&rows);
    parse_normalized(&cols).map(Arc::new)
}

fn template(src: &str, row: u32, col: u32) -> Option<Arc<Template>> {
    if src.len() > MAX_SOURCE {
        return None;
    }
    LAST.with_borrow_mut(|front| {
        relative_key_opts(src, row, col, true, &mut front.scratch)?;
        if front.key == front.scratch && front.template.is_some() {
            return front.template.clone();
        }
        let shard = shard(&front.scratch);
        let cached = shard.read().unwrap().relative.get(&front.scratch).cloned();
        let template = match cached {
            Some(value) => value,
            None => {
                let node = parse_body(src).ok()?;
                let value = Arc::new(Template {
                    row,
                    col,
                    scalar: projectable(&node),
                    node,
                });
                let mut cache = shard.write().unwrap();
                if let Some(existing) = cache.relative.get(&front.scratch) {
                    existing.clone()
                } else {
                    if cache.relative.len() >= PER_SHARD {
                        cache.relative.clear();
                    }
                    cache
                        .relative
                        .entry(front.scratch.clone())
                        .or_insert(value)
                        .clone()
                }
            }
        };
        std::mem::swap(&mut front.key, &mut front.scratch);
        front.template = Some(template.clone());
        Some(template)
    })
}

pub(super) fn compile_at(src: &str, row: u32, col: u32) -> Parsed {
    if src.len() > MAX_SOURCE {
        return parse_body(src);
    }
    let shard = shard(src);
    if let Some(value) = shard.read().unwrap().exact.get(src) {
        return value.clone();
    }
    let value = match template(src, row, col) {
        Some(base) if (row, col) == (base.row, base.col) => Ok(base.node.clone()),
        Some(base) => {
            let mut translated = (*base.node).clone();
            if relocate(
                &mut translated,
                i64::from(row) - i64::from(base.row),
                i64::from(col) - i64::from(base.col),
            ) {
                Ok(Arc::new(translated))
            } else {
                parse_body(src)
            }
        }
        None => parse_body(src),
    };
    let mut cache = shard.write().unwrap();
    if let Some(existing) = cache.exact.get(src) {
        return existing.clone();
    }
    if cache.exact.len() >= PER_SHARD {
        cache.exact.clear();
    }
    cache.exact.entry(src.to_owned()).or_insert(value).clone()
}

struct Shifted {
    template: Arc<Template>,
    row: u32,
    col: u32,
    tree: Node,
}

thread_local! { static SHIFTED: RefCell<Vec<Shifted>> = const { RefCell::new(Vec::new()) }; }

/// Most callers only borrow syntax for one evaluation. Rebind an already allocated
/// tree in place; take the slot out before visiting so nested formula evaluation
/// cannot borrow the same tree. Owned/long-lived callers still use compile_at.
pub(crate) fn with_node<T>(
    src: &str,
    row: u32,
    col: u32,
    visit: impl FnOnce(&Node) -> T,
) -> Result<T, ErrKind> {
    let Some(template) = template(src, row, col) else {
        return compile_at(src, row, col).map(|n| visit(&n));
    };
    if (row, col) == (template.row, template.col) {
        return Ok(visit(&template.node));
    }
    let mut slot = SHIFTED
        .with_borrow_mut(|slots| {
            let i = slots
                .iter()
                .rposition(|s| Arc::ptr_eq(&s.template, &template))?;
            Some(slots.swap_remove(i))
        })
        .unwrap_or_else(|| Shifted {
            tree: (*template.node).clone(),
            row: template.row,
            col: template.col,
            template,
        });
    if (row, col) != (slot.row, slot.col)
        && !relocate(
            &mut slot.tree,
            i64::from(row) - i64::from(slot.row),
            i64::from(col) - i64::from(slot.col),
        )
    {
        return parse_body(src).map(|node| visit(&node));
    }
    (slot.row, slot.col) = (row, col);
    let result = visit(&slot.tree);
    SHIFTED.with_borrow_mut(|slots| {
        if slots.len() >= 8 {
            slots.remove(0);
        }
        slots.push(slot);
    });
    Ok(result)
}

fn projectable(node: &Node) -> bool {
    match node {
        Node::Number(_) => true,
        Node::Ref(r) => r.sheet.is_none() && r.sheet2.is_none() && r.book.is_none(),
        Node::Unary(_, n) | Node::Percent(n) => projectable(n),
        Node::Binary(_, a, b) => projectable(a) && projectable(b),
        _ => false,
    }
}

pub(super) struct BoundScalar {
    template: Arc<Template>,
    dr: i64,
    dc: i64,
}

pub(super) fn scalar_at(src: &str, row: u32, col: u32) -> Option<BoundScalar> {
    let template = template(src, row, col)?;
    template.scalar.then(|| BoundScalar {
        dr: i64::from(row) - i64::from(template.row),
        dc: i64::from(col) - i64::from(template.col),
        template,
    })
}

impl BoundScalar {
    fn coord(&self, r: &RefExpr) -> (u32, u32) {
        let row = r.row + if r.row_abs { 0 } else { self.dr };
        let col = r.col + if r.col_abs { 0 } else { self.dc };
        // Both template and current source were accepted by the same A1 tokenizer.
        (row as u32, col as u32)
    }

    pub(super) fn refs(&self, mut visit: impl FnMut(u32, u32)) {
        fn walk(bound: &BoundScalar, node: &Node, visit: &mut impl FnMut(u32, u32)) {
            match node {
                Node::Ref(r) => {
                    let (row, col) = bound.coord(r);
                    visit(row, col);
                }
                Node::Unary(_, n) | Node::Percent(n) => walk(bound, n, visit),
                Node::Binary(_, a, b) => {
                    walk(bound, a, visit);
                    walk(bound, b, visit);
                }
                _ => {}
            }
        }
        walk(self, &self.template.node, &mut visit);
    }

    pub(super) fn evaluate(
        &self,
        mut read: impl FnMut(u32, u32) -> Option<FValue>,
    ) -> Option<FValue> {
        fn eval(
            bound: &BoundScalar,
            n: &Node,
            read: &mut impl FnMut(u32, u32) -> Option<FValue>,
        ) -> Option<FValue> {
            Some(match n {
                Node::Number(v) => FValue::Number(*v),
                Node::Ref(r) => {
                    let (row, col) = bound.coord(r);
                    read(row, col)?.scalar()
                }
                Node::Unary(neg, n) => eval(bound, n, read)?
                    .to_number()
                    .map(|v| FValue::number(if *neg { -v } else { v }))
                    .unwrap_or_else(FValue::Error),
                Node::Percent(n) => eval(bound, n, read)?
                    .to_number()
                    .map(|v| FValue::number(v / 100.0))
                    .unwrap_or_else(FValue::Error),
                Node::Binary(op, a, b) => {
                    bin_scalar(*op, &eval(bound, a, read)?, &eval(bound, b, read)?)
                }
                _ => return None,
            })
        }
        eval(self, &self.template.node, &mut read)
    }
}

#[inline]
fn col_letters_valid(s: &str) -> bool {
    let mut c = 0u32;
    for &b in s.as_bytes() {
        if !b.is_ascii_alphabetic() {
            return false;
        }
        c = c * 26 + u32::from(b.to_ascii_uppercase() - b'A' + 1);
    }
    (1..=16_384).contains(&c)
}

#[inline]
fn strip_xlfn_prefix(word: &str) -> Option<&str> {
    if word.len() > 12 && word.as_bytes()[..12].eq_ignore_ascii_case(b"_xlfn._xlws.") {
        return Some(&word[12..]);
    }
    if word.len() > 6 && word.as_bytes()[..6].eq_ignore_ascii_case(b"_xlfn.") {
        return Some(&word[6..]);
    }
    if word.len() >= 4 && word.as_bytes()[..4].eq_ignore_ascii_case(b"_xl") {
        return None;
    }
    Some(word)
}

#[inline]
fn scan_quoted_ident(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'\'') {
        return None;
    }
    let mut j = start + 1;
    loop {
        let &b = bytes.get(j)?;
        if b == b'[' || b == b']' {
            return None;
        }
        if b == b'\'' {
            if bytes.get(j + 1) == Some(&b'\'') {
                j += 2;
                continue;
            }
            return Some(j + 1);
        }
        j += 1;
    }
}

/// Encode fill formulas with relative R1C1 offsets, supporting whitespace, string literals,
/// TRUE/FALSE, scientific notation, cross-sheet qualifiers (`Sheet!`, `'My Sheet'!`, 3D),
/// whole-column (`A:C`) and whole-row (`1:3`) references.
fn relative_key(src: &str, row: u32, col: u32, out: &mut Vec<u8>) -> Option<()> {
    relative_key_opts(src, row, col, false, out)
}

fn relative_key_opts(
    src: &str,
    row: u32,
    col: u32,
    extended: bool,
    out: &mut Vec<u8>,
) -> Option<()> {
    let bytes = src.as_bytes();
    out.clear();
    let (mut i, mut saw_ref, mut need_ref, mut was_ref, mut after_bang) =
        (0usize, false, false, false, false);
    while i < bytes.len() {
        let byte = bytes[i];

        // 1. Collapse ASCII whitespace
        if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            if !extended || need_ref || after_bang {
                return None;
            }
            out.push(b' ');
            i += 1;
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
                i += 1;
            }
            continue;
        }

        // 2. Double-quoted string literal "..."
        if byte == b'"' {
            if !extended || need_ref || after_bang {
                return None;
            }
            let start = i;
            i += 1;
            loop {
                let &c = bytes.get(i)?;
                if c == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(4);
            out.extend_from_slice(&(i - start).to_le_bytes());
            out.extend_from_slice(&bytes[start..i]);
            was_ref = false;
            continue;
        }

        // 3. Single-quoted sheet qualifier 'Sheet Name'! or 'First':'Last'!
        if byte == b'\'' {
            if !extended || need_ref || after_bang {
                return None;
            }
            let start = i;
            let mut end = scan_quoted_ident(bytes, start)?;
            if bytes.get(end) == Some(&b':') {
                if bytes.get(end + 1) == Some(&b'\'') {
                    end = scan_quoted_ident(bytes, end + 1)?;
                } else {
                    let s2 = end + 1;
                    let mut j = s2;
                    while j < bytes.len()
                        && (bytes[j].is_ascii_alphanumeric()
                            || matches!(bytes[j], b'_' | b'.')
                            || bytes[j] >= 0x80)
                    {
                        j += 1;
                    }
                    if j == s2 {
                        return None;
                    }
                    end = j;
                }
            }
            if bytes.get(end) != Some(&b'!') {
                return None;
            }
            i = end + 1;
            out.push(5);
            out.extend_from_slice(&(i - start).to_le_bytes());
            out.extend_from_slice(&bytes[start..i]);
            after_bang = true;
            was_ref = false;
            continue;
        }

        // 4. Whole-row reference `1:3` / `$1:$3` (before numeric literal scan!)
        if extended
            && !need_ref
            && (byte == b'$' || byte.is_ascii_digit())
            && let Some((end, r1, r2, a_abs, b_abs)) = crate::match_rows_bytes(bytes, i)
        {
            if r1 > 1_048_576 || r2 > 1_048_576 {
                return None;
            }
            let r1_0 = i64::from(r1) - 1;
            let r2_0 = i64::from(r2) - 1;
            out.push(2);
            out.push(u8::from(a_abs) | (u8::from(b_abs) << 1));
            out.extend_from_slice(&(if a_abs { r1_0 } else { r1_0 - i64::from(row) }).to_le_bytes());
            out.extend_from_slice(&(if b_abs { r2_0 } else { r2_0 - i64::from(row) }).to_le_bytes());
            // Whole-row expansion uses relative columns A..XFD, so pin template per column (dc == 0).
            out.extend_from_slice(&col.to_le_bytes());
            i = end;
            saw_ref = true;
            was_ref = false;
            after_bang = false;
            continue;
        }

        // 5. Whole-column reference `A:C` / `$A:$C`
        if extended
            && !need_ref
            && (byte == b'$' || byte.is_ascii_alphabetic())
            && let Some((end, first, second)) = crate::match_cols_bytes(src, i)
        {
            if !col_letters_valid(first) || !col_letters_valid(second) {
                return None;
            }
            out.push(1);
            out.push(first.len() as u8);
            for &b in first.as_bytes() {
                out.push(b.to_ascii_uppercase());
            }
            out.push(second.len() as u8);
            for &b in second.as_bytes() {
                out.push(b.to_ascii_uppercase());
            }
            i = end;
            saw_ref = true;
            was_ref = false;
            after_bang = false;
            continue;
        }

        // 6. Numeric literal (including scientific notation 1e3 / 1.5E-4)
        if byte.is_ascii_digit() || (byte == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            if need_ref || after_bang {
                return None;
            }
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if extended && i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
                let save = i;
                i += 1;
                if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
                    i += 1;
                }
                if i < bytes.len() && bytes[i].is_ascii_digit() {
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            if bytes
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'$' | b'_' | b'.' | b'!' | b':'))
            {
                return None;
            }
            out.push(6);
            out.extend_from_slice(&(i - start).to_le_bytes());
            out.extend_from_slice(&bytes[start..i]);
            was_ref = false;
            continue;
        }

        // 7. Identifier / A1 ref / unquoted sheet qualifier / function call / TRUE / FALSE
        if byte.is_ascii_alphabetic() || matches!(byte, b'$' | b'_') || (extended && byte >= 0x80) {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric()
                    || matches!(bytes[i], b'$' | b'.' | b'_')
                    || (extended && bytes[i] >= 0x80))
            {
                i += 1;
            }
            let word = &src[start..i];

            // Unquoted sheet qualifier `Sheet1!A1` or 3D `Sheet1:Sheet3!A1`
            if extended && !need_ref && !after_bang && !word.contains('$') {
                if bytes.get(i) == Some(&b'!') {
                    i += 1;
                    out.push(5);
                    out.extend_from_slice(&(i - start).to_le_bytes());
                    out.extend_from_slice(&bytes[start..i]);
                    after_bang = true;
                    was_ref = false;
                    continue;
                }
                if bytes.get(i) == Some(&b':') {
                    let mut j = i + 1;
                    while j < bytes.len()
                        && (bytes[j].is_ascii_alphanumeric()
                            || matches!(bytes[j], b'_' | b'.')
                            || bytes[j] >= 0x80)
                    {
                        j += 1;
                    }
                    if j > i + 1 && bytes.get(j) == Some(&b'!') {
                        i = j + 1;
                        out.push(5);
                        out.extend_from_slice(&(i - start).to_le_bytes());
                        out.extend_from_slice(&bytes[start..i]);
                        after_bang = true;
                        was_ref = false;
                        continue;
                    }
                }
            }

            // Function call `SUM(` / `_xlfn.XLOOKUP(` (with optional spaces before `(` in extended mode)
            let mut open_pos = i;
            if extended {
                while open_pos < bytes.len() && matches!(bytes[open_pos], b' ' | b'\t') {
                    open_pos += 1;
                }
            }
            if bytes.get(open_pos) == Some(&b'(') {
                if need_ref || after_bang || word.contains('$') {
                    return None;
                }
                let fn_name = if extended {
                    strip_xlfn_prefix(word)?
                } else {
                    if word.as_bytes().windows(3).any(|w| w.eq_ignore_ascii_case(b"_xl")) {
                        return None;
                    }
                    word
                };
                out.push(7);
                out.extend_from_slice(&fn_name.len().to_le_bytes());
                for &b in fn_name.as_bytes() {
                    out.push(b.to_ascii_uppercase());
                }
                was_ref = false;
                continue;
            }

            // A1 Cell reference
            if word.is_ascii()
                && let Some(r) = parse_a1(word)
            {
                if r.row >= 1_048_576 || r.col >= 16_384 {
                    return None;
                }
                out.push(0);
                out.push(u8::from(r.row_abs) | (u8::from(r.col_abs) << 1));
                out.extend_from_slice(
                    &(if r.row_abs {
                        r.row
                    } else {
                        r.row - i64::from(row)
                    })
                    .to_le_bytes(),
                );
                out.extend_from_slice(
                    &(if r.col_abs {
                        r.col
                    } else {
                        r.col - i64::from(col)
                    })
                    .to_le_bytes(),
                );
                saw_ref = true;
                was_ref = !need_ref;
                need_ref = false;
                after_bang = false;
                continue;
            }

            // Boolean literals TRUE / FALSE
            if extended
                && !need_ref
                && !after_bang
                && (word.eq_ignore_ascii_case("TRUE") || word.eq_ignore_ascii_case("FALSE"))
            {
                out.push(3);
                out.push(u8::from(word.len() == 4));
                was_ref = false;
                continue;
            }

            return None;
        }

        // 8. Spill operator `#` right after a single-cell reference (`A2#`)
        if extended
            && byte == b'#'
            && was_ref
            && !bytes
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'!' | b'_'))
        {
            out.push(b'#');
            i += 1;
            was_ref = false;
            continue;
        }

        // 9. Operators and punctuation
        if need_ref || after_bang {
            return None;
        }
        let allowed_punct = matches!(
            byte,
            b'+' | b'-'
                | b'*'
                | b'/'
                | b'^'
                | b'&'
                | b'%'
                | b'='
                | b'>'
                | b'<'
                | b','
                | b'('
                | b')'
                | b':'
        ) || (extended && byte == b'@');
        if !allowed_punct {
            return None;
        }
        if byte == b':' {
            if !was_ref {
                return None;
            }
            need_ref = true;
        }
        out.push(byte);
        i += 1;
        was_ref = false;
    }
    (saw_ref && !need_ref && !after_bang).then_some(())
}

/// Reuse the parsed template for relative A1 rules. Extended references and strings
/// retain the text translator's handling of whole axes, sheet names and bounds.
#[derive(Default)]
pub struct RuleFormulas {
    templates: HashMap<String, Option<Node>>,
}

impl RuleFormulas {
    pub fn shifted(&mut self, src: &str, dr: u32, dc: u32) -> Result<Node, ErrKind> {
        let src = src.trim_start_matches('=');
        if !self.templates.contains_key(src) {
            let parsed = relative_key(src, 0, 0, &mut Vec::new()).and_then(|_| parse(src).ok());
            self.templates.insert(src.to_owned(), parsed);
        }
        if let Some(mut node) = self.templates.get(src).and_then(Option::as_ref).cloned() {
            if relocate(&mut node, i64::from(dr), i64::from(dc)) {
                return Ok(node);
            }
        }
        parse(&crate::cf::shift_formula(src, dr, dc))
    }
}

fn relocate(node: &mut Node, dr: i64, dc: i64) -> bool {
    let shift = |r: &mut super::RefExpr| {
        if !r.row_abs {
            r.row += dr;
        }
        if !r.col_abs {
            r.col += dc;
        }
        (0..1_048_576).contains(&r.row) && (0..16_384).contains(&r.col)
    };
    match node {
        Node::Ref(r) | Node::Spill(r) => shift(r),
        Node::Range(a, b) => shift(a) & shift(b),
        Node::Func(_, args) => args
            .iter_mut()
            .fold(true, |valid, a| relocate(a, dr, dc) && valid),
        Node::Implicit(n) | Node::Percent(n) | Node::Unary(_, n) => relocate(n, dr, dc),
        Node::Binary(_, a, b) => relocate(a, dr, dc) & relocate(b, dr, dc),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_syntax_is_shared_across_worker_lifetimes_including_literal_arrays() {
        let source = "IF(TRUE,SUM({1,2;3,4}),'Other tab'!A7)";
        let first = std::thread::spawn(move || compile_at(source, 0, 0).unwrap())
            .join()
            .unwrap();
        let second = std::thread::spawn(move || compile_at(source, 999, 4).unwrap())
            .join()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(*first, parse(source).unwrap());
    }

    #[test]
    fn relative_formulas_reuse_tree_storage_and_preserve_offsets_and_nesting() {
        let src = |r: u32| format!("SUM($A{}:B$9)+$C{}", r + 1, r + 1);
        let original = template(&src(0), 0, 1).unwrap();
        let other = std::thread::spawn(move || template("SUM($A2:B$9)+$C2", 1, 1).unwrap())
            .join()
            .unwrap();
        assert!(Arc::ptr_eq(&original, &other));
        let mut previous = None;
        for r in [3, 91, 5, 409, 10] {
            let source = src(r);
            let ptr = with_node(&source, r, 1, |node| {
                assert_eq!(*node, parse(&source).unwrap());
                with_node(&src(r + 2), r + 2, 1, |inner| {
                    assert_eq!(*inner, parse(&src(r + 2)).unwrap())
                })
                .unwrap();
                assert_eq!(*node, parse(&source).unwrap());
                let Node::Binary(_, lhs, _) = node else {
                    panic!()
                };
                lhs.as_ref() as *const Node as usize
            })
            .unwrap();
            if let Some(p) = previous {
                assert_eq!(ptr, p);
            }
            previous = Some(ptr);
        }
        for (r, source) in [(1, "-($A2^2)+B$1%"), (5, "-($A6^2)+B$1%")] {
            let bound = scalar_at(source, r, 0).unwrap();
            let mut refs = Vec::new();
            bound.refs(|r, c| refs.push((r, c)));
            assert_eq!(refs, [(r, 0), (0, 1)]);
            let value = bound
                .evaluate(|r, c| Some(FValue::Number(f64::from(r + c + 1))))
                .unwrap();
            assert_eq!(value, FValue::Number(-f64::from(r + 1).powi(2) + 0.02));
        }
        assert!(scalar_at("'Other'!A1+1", 0, 0).is_none());
    }
}
