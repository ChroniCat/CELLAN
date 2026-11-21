//! Per-thread caches: AST values may contain Rc arrays, so workers own their caches.
use super::{ErrKind, Node, parse, parse_a1};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

type Parsed = Result<Rc<Node>, ErrKind>;
const MAX_SOURCE: usize = 1024;
const ENTRIES: usize = 2048;

#[derive(Default)]
struct Cache {
    exact: HashMap<String, Parsed>,
    relative: HashMap<Vec<u8>, (u32, u32, Rc<Node>)>,
}

thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache::default()); }

pub(super) fn compile_at(src: &str, row: u32, col: u32) -> Parsed {
    let parse_body = || parse(&crate::normalize_formula(src)).map(Rc::new);
    if src.len() > MAX_SOURCE {
        return parse_body();
    }
    CACHE.with_borrow_mut(|cache| {
        if let Some(parsed) = cache.exact.get(src) {
            return parsed.clone();
        }
        let key = relative_key(src, row, col);
        let parsed = match key.as_ref().and_then(|k| cache.relative.get(k)) {
            Some((base_row, base_col, node)) => {
                let mut result = node.as_ref().clone();
                relocate(
                    &mut result,
                    i64::from(row) - i64::from(*base_row),
                    i64::from(col) - i64::from(*base_col),
                );
                Ok(Rc::new(result))
            }
            None => {
                let parsed = parse_body();
                if let (Some(key), Ok(node)) = (key, &parsed) {
                    if cache.relative.len() == ENTRIES {
                        cache.relative.clear();
                    }
                    cache.relative.insert(key, (row, col, node.clone()));
                }
                parsed
            }
        };
        if cache.exact.len() == ENTRIES {
            cache.exact.clear();
        }
        cache.exact.insert(src.into(), parsed.clone());
        parsed
    })
}

/// Encode ordinary same-sheet fill formulas with relative R1C1 offsets. Richer syntax
/// goes through the exact-source cache to keep workbook names and implicit bounds intact.
fn relative_key(src: &str, row: u32, col: u32) -> Option<Vec<u8>> {
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let (mut i, mut saw_ref, mut need_ref, mut was_ref) = (0, false, false, false);
    while i < bytes.len() {
        let start = i;
        let byte = bytes[i];
        if byte.is_ascii_alphanumeric() || matches!(byte, b'$' | b'.' | b'_') {
            i += 1;
            while bytes
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'$' | b'.' | b'_'))
            {
                i += 1;
            }
            let word = &src[start..i];
            if bytes.get(i) != Some(&b'(')
                && let Some(r) = parse_a1(word)
            {
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
                was_ref = true;
                need_ref = false;
                continue;
            }
            if need_ref {
                return None;
            }
            // Avoid ambiguities involving names, scientific notation and whole columns.
            if bytes.get(i) != Some(&b'(') && !word.bytes().all(|c| c.is_ascii_digit() || c == b'.')
            {
                return None;
            }
            out.extend_from_slice(word.as_bytes());
        } else {
            if need_ref
                || !matches!(
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
                )
            {
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
        }
        was_ref = false;
    }
    (saw_ref && !need_ref).then_some(out)
}

fn relocate(node: &mut Node, dr: i64, dc: i64) {
    let shift = |r: &mut super::RefExpr| {
        if !r.row_abs {
            r.row += dr;
        }
        if !r.col_abs {
            r.col += dc;
        }
    };
    match node {
        Node::Ref(r) | Node::Spill(r) => shift(r),
        Node::Range(a, b) => {
            shift(a);
            shift(b);
        }
        Node::Func(_, args) => args.iter_mut().for_each(|a| relocate(a, dr, dc)),
        Node::Implicit(n) | Node::Percent(n) | Node::Unary(_, n) => relocate(n, dr, dc),
        Node::Binary(_, a, b) => {
            relocate(a, dr, dc);
            relocate(b, dr, dc);
        }
        _ => {}
    }
}
