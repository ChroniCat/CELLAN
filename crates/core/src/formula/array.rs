//! 动态数组函数：返回二维数组，由 recalc 溢出到相邻单元格。

use super::functions::{int_arg, opt_num, r};
use super::value::Array;
use super::{Arg, ErrKind, FValue};
use std::sync::Arc;

fn arr(a: Array) -> FValue {
    FValue::Array(Arc::new(a))
}

fn truthy(v: &FValue) -> bool {
    v.to_bool().unwrap_or(false)
}

fn sort_key_cmp(a: &FValue, b: &FValue) -> std::cmp::Ordering {
    a.compare(b).unwrap_or(std::cmp::Ordering::Equal)
}

pub fn call_array(name: &str, args: &[Arg]) -> Option<FValue> {
    let v = match name {
        "SEQUENCE" => r(|| {
            let rows = int_arg(args, 0)?.max(1) as usize;
            let cols = match args.get(1) {
                Some(_) => int_arg(args, 1)?.max(1) as usize,
                None => 1,
            };
            let start = opt_num(args, 2, 1.0)?;
            let step = opt_num(args, 3, 1.0)?;
            let mut data = Vec::with_capacity(rows * cols);
            for i in 0..rows * cols {
                data.push(FValue::number(start + step * i as f64));
            }
            Ok(arr(Array::new(rows, cols, data)))
        }),
        "RANDARRAY" => r(|| {
            let rows = match args.first() {
                Some(_) => int_arg(args, 0)?.max(1) as usize,
                None => 1,
            };
            let cols = match args.get(1) {
                Some(_) => int_arg(args, 1)?.max(1) as usize,
                None => 1,
            };
            let min = opt_num(args, 2, 0.0)?;
            let max = opt_num(args, 3, 1.0)?;
            let whole = args.get(4).map(|a| a.first().to_bool().unwrap_or(false)).unwrap_or(false);
            if min > max {
                return Err(ErrKind::Value);
            }
            let mut data = Vec::with_capacity(rows * cols);
            for _ in 0..rows * cols {
                let t = pseudo_random();
                let val = if whole {
                    let lo = min.ceil();
                    let hi = max.floor();
                    if lo > hi {
                        return Err(ErrKind::Value);
                    }
                    (lo + ((hi - lo + 1.0) * t).floor()).min(hi)
                } else {
                    min + (max - min) * t
                };
                data.push(FValue::number(val));
            }
            Ok(arr(Array::new(rows, cols, data)))
        }),
        "TRANSPOSE" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            let mut out = vec![FValue::Empty; rows * cols];
            for r0 in 0..rows {
                for c0 in 0..cols {
                    out[c0 * rows + r0] = data[r0 * cols + c0].clone();
                }
            }
            Ok(arr(Array::new(cols, rows, out)))
        }),
        "UNIQUE" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            let by_col = args.get(1).map(|a| a.first().to_bool().unwrap_or(false)).unwrap_or(false);
            let once = args.get(2).map(|a| a.first().to_bool().unwrap_or(false)).unwrap_or(false);
            let (lines, width) = if by_col { (cols, rows) } else { (rows, cols) };
            let index = |line: usize, offset: usize| {
                if by_col { offset * cols + line } else { line * cols + offset }
            };

            #[derive(Clone, Copy, PartialEq, Eq, Hash)]
            enum UniqueAtom<'a> {
                Empty,
                Num(u64),
                Text(&'a str),
                Bool(bool),
                Err(ErrKind),
                Func,
            }

            #[inline]
            fn atom_of(mut v: &FValue) -> UniqueAtom<'_> {
                while let FValue::Array(a) = v {
                    v = a.data.first().unwrap_or(&FValue::Empty);
                }
                match v {
                    FValue::Empty => UniqueAtom::Empty,
                    FValue::Number(n) => {
                        let c = if *n == 0.0 { 0.0 } else { *n };
                        UniqueAtom::Num(c.to_bits())
                    }
                    FValue::Text(s) => UniqueAtom::Text(s.as_ref()),
                    FValue::Bool(b) => UniqueAtom::Bool(*b),
                    FValue::Error(e) => UniqueAtom::Err(*e),
                    FValue::Lambda(_) => UniqueAtom::Func,
                    FValue::Array(_) => unreachable!(),
                }
            }

            let mut keep = Vec::with_capacity(lines);
            let mut repeated = vec![false; if once { lines } else { 0 }];
            if width == 1 {
                let mut first: super::fxhash::FxHashMap<UniqueAtom<'_>, usize> =
                    super::fxhash::FxHashMap::default();
                for line in 0..lines {
                    let k = atom_of(&data[index(line, 0)]);
                    let first_line = *first.entry(k).or_insert(line);
                    if first_line == line {
                        keep.push(line);
                    } else if once {
                        repeated[first_line] = true;
                    }
                }
            } else if width <= 4 {
                let mut first: super::fxhash::FxHashMap<[UniqueAtom<'_>; 4], usize> =
                    super::fxhash::FxHashMap::default();
                for line in 0..lines {
                    let mut k = [UniqueAtom::Empty; 4];
                    for offset in 0..width {
                        k[offset] = atom_of(&data[index(line, offset)]);
                    }
                    let first_line = *first.entry(k).or_insert(line);
                    if first_line == line {
                        keep.push(line);
                    } else if once {
                        repeated[first_line] = true;
                    }
                }
            } else {
                let mut first: super::fxhash::FxHashMap<Vec<UniqueAtom<'_>>, usize> =
                    super::fxhash::FxHashMap::default();
                let mut scratch = Vec::with_capacity(width);
                for line in 0..lines {
                    scratch.clear();
                    for offset in 0..width {
                        scratch.push(atom_of(&data[index(line, offset)]));
                    }
                    if let Some(&first_line) = first.get(&scratch) {
                        if once {
                            repeated[first_line] = true;
                        }
                    } else {
                        first.insert(scratch.clone(), line);
                        keep.push(line);
                    }
                }
            }
            if once {
                keep.retain(|&line| !repeated[line]);
            }
            let mut out = Vec::with_capacity(keep.len() * width);
            if by_col {
                for row in 0..rows {
                    out.extend(keep.iter().map(|&col| data[row * cols + col].clone()));
                }
                Ok(arr(Array::new(rows, keep.len(), out)))
            } else {
                for &row in &keep {
                    out.extend_from_slice(&data[row * cols..(row + 1) * cols]);
                }
                Ok(arr(Array::new(keep.len(), cols, out)))
            }
        }),
        "SORT" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            let by_col = args.get(3).map(|a| a.first().to_bool().unwrap_or(false)).unwrap_or(false);
            let limit = if by_col { rows } else { cols };
            let indices: Vec<usize> = match args.get(1) {
                Some(a) if !matches!(a, Arg::Val(FValue::Empty)) => {
                    let mut out = Vec::new();
                    for v in a.slice() {
                        let k = v.to_number()?.trunc() as i64;
                        if k < 1 || k as usize > limit {
                            return Err(ErrKind::Value);
                        }
                        out.push((k - 1) as usize);
                    }
                    if out.is_empty() { vec![0] } else { out }
                }
                _ => vec![0],
            };
            let orders: Vec<bool> = match args.get(2) {
                Some(a) if !matches!(a, Arg::Val(FValue::Empty)) => {
                    let mut out = Vec::new();
                    for v in a.slice() {
                        let o = v.to_number()?.trunc() as i64;
                        if o != 1 && o != -1 {
                            return Err(ErrKind::Value);
                        }
                        out.push(o < 0);
                    }
                    if out.is_empty() { vec![false] } else { out }
                }
                _ => vec![false],
            };
            if by_col {
                let mut order: Vec<usize> = (0..cols).collect();
                order.sort_by(|&a, &b| {
                    for (ki, &idx) in indices.iter().enumerate() {
                        let desc = orders.get(ki).copied().unwrap_or(orders[0]);
                        let cmp = sort_key_cmp(&data[idx * cols + a], &data[idx * cols + b]);
                        let ord = if desc { cmp.reverse() } else { cmp };
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                let mut out = Vec::with_capacity(rows * cols);
                for i in 0..rows {
                    for &c in &order {
                        out.push(data[i * cols + c].clone());
                    }
                }
                Ok(arr(Array::new(rows, cols, out)))
            } else {
                let mut order: Vec<usize> = (0..rows).collect();
                order.sort_by(|&a, &b| {
                    for (ki, &idx) in indices.iter().enumerate() {
                        let desc = orders.get(ki).copied().unwrap_or(orders[0]);
                        let cmp = sort_key_cmp(&data[a * cols + idx], &data[b * cols + idx]);
                        let ord = if desc { cmp.reverse() } else { cmp };
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                let mut out = Vec::with_capacity(rows * cols);
                for &row in &order {
                    out.extend_from_slice(&data[row * cols..(row + 1) * cols]);
                }
                Ok(arr(Array::new(rows, cols, out)))
            }
        }),
        "SORTBY" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            if args.len() < 2 {
                return Err(ErrKind::Value);
            }
            let (br0, bc0, _) = args[1].grid_slice();
            let by_col = br0 == 1 && bc0 == cols && !(bc0 == 1 && br0 == rows);
            let expected_len = if by_col { cols } else { rows };
            let mut keys: Vec<(&[FValue], bool)> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                let (kr, kc, by) = args[i].grid_slice();
                let valid = if by_col {
                    kr == 1 && kc == expected_len
                } else {
                    kc == 1 && kr == expected_len
                };
                if !valid && by.len() != expected_len {
                    return Err(ErrKind::Value);
                }
                let desc = match args.get(i + 1) {
                    Some(a) if !matches!(a, Arg::Val(FValue::Empty)) => {
                        let o = a.first().to_number()?.trunc() as i64;
                        if o != 1 && o != -1 {
                            return Err(ErrKind::Value);
                        }
                        o < 0
                    }
                    _ => false,
                };
                keys.push((args[i].slice(), desc));
                i += 2;
            }
            let mut order: Vec<usize> = (0..expected_len).collect();
            for (by, desc) in keys.iter().rev() {
                order.sort_by(|&a, &b| {
                    let va = by.get(a).unwrap_or(&FValue::Empty);
                    let vb = by.get(b).unwrap_or(&FValue::Empty);
                    let cmp = sort_key_cmp(va, vb);
                    if *desc { cmp.reverse() } else { cmp }
                });
            }
            if by_col {
                let mut out = Vec::with_capacity(rows * cols);
                for r0 in 0..rows {
                    for &c in &order {
                        out.push(data[r0 * cols + c].clone());
                    }
                }
                Ok(arr(Array::new(rows, cols, out)))
            } else {
                let mut out = Vec::with_capacity(rows * cols);
                for &row in &order {
                    out.extend_from_slice(&data[row * cols..(row + 1) * cols]);
                }
                Ok(arr(Array::new(rows, cols, out)))
            }
        }),
        "FILTER" => r(|| {
            let first_arg = args.first().ok_or(ErrKind::Value)?;
            let (rows, cols, data) = first_arg.grid_slice();
            // 条件按**形状**判方向，不能按元素个数：2×2 数据配 1×2 条件，
            // 个数也等于 2，若按长度猜就会错当成「按行」。
            let (ir, ic, include) = args.get(1).ok_or(ErrKind::Value)?.grid_slice();
            if ir == 1 && ic == 1 {
                // 标量条件：TRUE 返回整块，FALSE 走 if_empty
                if include.first().map(truthy).unwrap_or(false) {
                    return match first_arg {
                        Arg::Range { values, .. } => Ok(arr(Array::from_shared(rows, cols, values.clone()))),
                        Arg::Val(FValue::Array(a)) => Ok(FValue::Array(a.clone())),
                        _ => Ok(arr(Array::new(rows, cols, data.into_owned()))),
                    };
                }
                if let Some(f) = args.get(2) {
                    return Ok(f.first());
                }
                return Err(ErrKind::Value);
            }
            // 列向量（n×1）= 按行筛；行向量（1×n）= 按列筛；其余（含 2×2 条件）= #VALUE!
            let by_row = ic == 1 && ir == rows;
            let by_col = ir == 1 && ic == cols && !by_row;
            if !by_row && !by_col {
                return Err(ErrKind::Value);
            }
            if by_col {
                let mut keep = Vec::new();
                for c in 0..cols {
                    if include.get(c).map(truthy).unwrap_or(false) {
                        keep.push(c);
                    }
                }
                if keep.is_empty() {
                    if let Some(f) = args.get(2) {
                        return Ok(f.first());
                    }
                    return Err(ErrKind::Value);
                }
                let n = keep.len();
                let mut out = Vec::with_capacity(rows * n);
                for i in 0..rows {
                    for &c in &keep {
                        out.push(data[i * cols + c].clone());
                    }
                }
                Ok(arr(Array::new(rows, n, out)))
            } else {
                let mut keep = Vec::new();
                for i in 0..rows {
                    if include.get(i).map(truthy).unwrap_or(false) {
                        keep.push(i);
                    }
                }
                if keep.is_empty() {
                    if let Some(f) = args.get(2) {
                        return Ok(f.first());
                    }
                    return Err(ErrKind::Value);
                }
                let n = keep.len();
                let mut out = Vec::with_capacity(n * cols);
                for &i in &keep {
                    out.extend_from_slice(&data[i * cols..(i + 1) * cols]);
                }
                Ok(arr(Array::new(n, cols, out)))
            }
        }),
        "TAKE" | "DROP" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            let n_rows = if args.len() > 1 { int_arg(args, 1)? } else { 0 };
            let n_cols = if args.len() > 2 { int_arg(args, 2)? } else { 0 };
            let pick = |n: i64, total: usize| -> (usize, usize) {
                if n == 0 {
                    return (0, total);
                }
                if n > 0 {
                    let k = (n as usize).min(total);
                    if name == "TAKE" { (0, k) } else { (k, total) }
                } else {
                    let k = ((-n) as usize).min(total);
                    if name == "TAKE" { (total - k, total) } else { (0, total - k) }
                }
            };
            let (r0, r1) = pick(n_rows, rows);
            let (c0, c1) = pick(n_cols, cols);
            let rs = r1.saturating_sub(r0);
            let cs = c1.saturating_sub(c0);
            if rs == 0 || cs == 0 {
                return Err(ErrKind::Value);
            }
            let mut out = Vec::with_capacity(rs * cs);
            if c0 == 0 && c1 == cols {
                out.extend_from_slice(&data[r0 * cols..r1 * cols]);
            } else {
                for i in r0..r1 {
                    out.extend_from_slice(&data[i * cols + c0..i * cols + c1]);
                }
            }
            Ok(arr(Array::new(rs, cs, out)))
        }),
        "HSTACK" => r(|| {
            let grids: Vec<_> = args.iter().map(|a| a.grid_slice()).collect();
            let rows = grids.iter().map(|g| g.0).max().unwrap_or(0);
            let cols: usize = grids.iter().map(|g| g.1).sum();
            let mut out = vec![FValue::Error(ErrKind::Na); rows * cols];
            let mut c_off = 0;
            for (gr, gc, gd) in &grids {
                for i in 0..*gr {
                    out[i * cols + c_off..i * cols + c_off + *gc]
                        .clone_from_slice(&gd[i * gc..(i + 1) * gc]);
                }
                c_off += gc;
            }
            Ok(arr(Array::new(rows, cols, out)))
        }),
        "VSTACK" => r(|| {
            let grids: Vec<_> = args.iter().map(|a| a.grid_slice()).collect();
            let cols = grids.iter().map(|g| g.1).max().unwrap_or(0);
            let rows: usize = grids.iter().map(|g| g.0).sum();
            let mut out = vec![FValue::Error(ErrKind::Na); rows * cols];
            let mut r_off = 0;
            for (gr, gc, gd) in &grids {
                for i in 0..*gr {
                    out[(r_off + i) * cols..(r_off + i) * cols + *gc]
                        .clone_from_slice(&gd[i * gc..(i + 1) * gc]);
                }
                r_off += gr;
            }
            Ok(arr(Array::new(rows, cols, out)))
        }),
        "CHOOSECOLS" | "CHOOSEROWS" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            let mut idx: Vec<i64> = Vec::new();
            for a in args.iter().skip(1) {
                for v in a.slice() {
                    if let Ok(n) = v.to_number() {
                        idx.push(n as i64);
                    }
                }
            }
            if name == "CHOOSECOLS" {
                let mut cols_checked = Vec::with_capacity(idx.len());
                for &k in &idx {
                    let c = if k < 0 { cols as i64 + k } else { k - 1 };
                    if c < 0 || c >= cols as i64 {
                        return Err(ErrKind::Value);
                    }
                    cols_checked.push(c as usize);
                }
                let mut out = Vec::with_capacity(rows * cols_checked.len());
                for i in 0..rows {
                    let base = i * cols;
                    for &c in &cols_checked {
                        out.push(data[base + c].clone());
                    }
                }
                Ok(arr(Array::new(rows, cols_checked.len(), out)))
            } else {
                let mut out = Vec::with_capacity(idx.len() * cols);
                for &k in &idx {
                    let rr = if k < 0 { rows as i64 + k } else { k - 1 };
                    if rr < 0 || rr >= rows as i64 {
                        return Err(ErrKind::Value);
                    }
                    let start = rr as usize * cols;
                    out.extend_from_slice(&data[start..start + cols]);
                }
                Ok(arr(Array::new(idx.len(), cols, out)))
            }
        }),
        "TOROW" | "TOCOL" => r(|| {
            let first_arg = args.first().ok_or(ErrKind::Value)?;
            let (rows, cols, data) = first_arg.grid_slice();
            let ignore = if args.len() > 1 { int_arg(args, 1)? } else { 0 };
            let scan_by_col = args
                .get(2)
                .map(|a| a.first().to_bool().unwrap_or(false))
                .unwrap_or(false);
            let keep = |v: &FValue| -> bool {
                match ignore {
                    1 => !matches!(v, FValue::Empty),
                    2 => !v.is_error(),
                    3 => !matches!(v, FValue::Empty) && !v.is_error(),
                    _ => true,
                }
            };
            let mut v = Vec::with_capacity(rows * cols);
            if scan_by_col {
                for c in 0..cols {
                    for r0 in 0..rows {
                        let item = &data[r0 * cols + c];
                        if keep(item) {
                            v.push(item.clone());
                        }
                    }
                }
            } else {
                for item in data.iter() {
                    if keep(item) {
                        v.push(item.clone());
                    }
                }
            }
            if v.is_empty() {
                return Err(ErrKind::Value);
            }
            let len = v.len();
            if name == "TOCOL" {
                Ok(arr(Array::new(len, 1, v)))
            } else {
                Ok(arr(Array::new(1, len, v)))
            }
        }),
        "EXPAND" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            let new_rows = int_arg(args, 1)?.max(rows as i64) as usize;
            let new_cols = match args.get(2) {
                Some(_) => int_arg(args, 2)?.max(cols as i64) as usize,
                None => cols,
            };
            let pad = args.get(3).map(|a| a.first()).unwrap_or(FValue::Error(ErrKind::Na));
            let mut out = vec![pad; new_rows * new_cols];
            for i in 0..rows {
                out[i * new_cols..i * new_cols + cols]
                    .clone_from_slice(&data[i * cols..(i + 1) * cols]);
            }
            Ok(arr(Array::new(new_rows, new_cols, out)))
        }),
        "MMULT" => r(|| {
            let (r1, c1, d1) = args.first().ok_or(ErrKind::Value)?.grid_slice();
            let (r2, c2, d2) = args.get(1).ok_or(ErrKind::Value)?.grid_slice();
            if c1 != r2 {
                return Err(ErrKind::Value);
            }
            let n1: Vec<f64> = d1.iter().map(|v| v.to_number().unwrap_or(0.0)).collect();
            let mut n2_t = vec![0.0; c2 * r2];
            for r in 0..r2 {
                for c in 0..c2 {
                    n2_t[c * r2 + r] = d2[r * c2 + c].to_number().unwrap_or(0.0);
                }
            }
            let mut out = Vec::with_capacity(r1 * c2);
            for i in 0..r1 {
                let row = &n1[i * c1..(i + 1) * c1];
                for j in 0..c2 {
                    let col = &n2_t[j * c1..(j + 1) * c1];
                    let s = super::dot_f64_8way(row, col);
                    out.push(FValue::number(s));
                }
            }
            Ok(arr(Array::new(r1, c2, out)))
        }),
        _ => return None,
    };
    Some(v)
}

fn pseudo_random() -> f64 {
    use std::cell::Cell;
    use std::time::{SystemTime, UNIX_EPOCH};
    thread_local! {
        static SEED: Cell<u64> = Cell::new(
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7)
        );
    }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        ((x.wrapping_mul(0x2545F4914F6CDD1D)) >> 11) as f64 / (1u64 << 53) as f64
    })
}
