//! 动态数组函数：返回二维数组，由 recalc 溢出到相邻单元格。

use super::functions::{int_arg, num, opt_num, r};
use super::value::Array;
use super::{Arg, ErrKind, FValue};
use std::rc::Rc;

fn arr(a: Array) -> FValue {
    FValue::Array(Rc::new(a))
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
            let mut data = Vec::with_capacity(rows * cols);
            for _ in 0..rows * cols {
                let t = pseudo_random();
                let x = min + (max - min) * t;
                data.push(FValue::number(if whole { x.floor() } else { x }));
            }
            Ok(arr(Array::new(rows, cols, data)))
        }),
        "TRANSPOSE" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            let mut out = vec![FValue::Empty; rows * cols];
            for r0 in 0..rows {
                for c0 in 0..cols {
                    out[c0 * rows + r0] = data[r0 * cols + c0].clone();
                }
            }
            Ok(arr(Array::new(cols, rows, out)))
        }),
        "UNIQUE" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            let by_col = args.get(1).map(|a| a.first().to_bool().unwrap_or(false)).unwrap_or(false);
            let once = args.get(2).map(|a| a.first().to_bool().unwrap_or(false)).unwrap_or(false);
            let get = |i: usize, j: usize| data[i * cols + j].clone();

            if by_col {
                let mut keep: Vec<usize> = Vec::new();
                for c in 0..cols {
                    let mut dup = false;
                    for &k in &keep {
                        let same = (0..rows).all(|i| get(i, c).display() == get(i, k).display());
                        if same {
                            dup = true;
                            break;
                        }
                    }
                    if !dup {
                        let count = (0..cols)
                            .filter(|&k2| (0..rows).all(|i| get(i, k2).display() == get(i, c).display()))
                            .count();
                        if !once || count == 1 {
                            keep.push(c);
                        }
                    }
                }
                let mut out = Vec::new();
                for i in 0..rows {
                    for &c in &keep {
                        out.push(get(i, c));
                    }
                }
                Ok(arr(Array::new(rows, keep.len(), out)))
            } else {
                let mut keep: Vec<usize> = Vec::new();
                for row in 0..rows {
                    let mut dup = false;
                    for &k in &keep {
                        let same = (0..cols).all(|j| get(row, j).display() == get(k, j).display());
                        if same {
                            dup = true;
                            break;
                        }
                    }
                    if !dup {
                        let count = (0..rows)
                            .filter(|&r2| (0..cols).all(|j| get(r2, j).display() == get(row, j).display()))
                            .count();
                        if !once || count == 1 {
                            keep.push(row);
                        }
                    }
                }
                let mut out = Vec::new();
                for &row in &keep {
                    for j in 0..cols {
                        out.push(get(row, j));
                    }
                }
                Ok(arr(Array::new(keep.len(), cols, out)))
            }
        }),
        "SORT" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            let idx = match args.get(1) {
                Some(_) => (int_arg(args, 1)?.max(1) as usize) - 1,
                None => 0,
            };
            let desc = opt_num(args, 2, 1.0)? < 0.0;
            let by_col = args.get(3).map(|a| a.first().to_bool().unwrap_or(false)).unwrap_or(false);
            if by_col {
                let mut order: Vec<usize> = (0..cols).collect();
                order.sort_by(|&a, &b| {
                    let cmp = sort_key_cmp(&data[idx.min(rows - 1) * cols + a], &data[idx.min(rows - 1) * cols + b]);
                    if desc { cmp.reverse() } else { cmp }
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
                    let cmp = sort_key_cmp(&data[a * cols + idx.min(cols - 1)], &data[b * cols + idx.min(cols - 1)]);
                    if desc { cmp.reverse() } else { cmp }
                });
                let mut out = Vec::with_capacity(rows * cols);
                for &row in &order {
                    for j in 0..cols {
                        out.push(data[row * cols + j].clone());
                    }
                }
                Ok(arr(Array::new(rows, cols, out)))
            }
        }),
        "SORTBY" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            let mut order: Vec<usize> = (0..rows).collect();
            // 依次按每个 key 排序（稳定排序，逆序从最后一组往前做）
            let mut keys: Vec<(Vec<FValue>, bool)> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                let by = args[i].values();
                let desc = args.get(i + 1).map(|a| a.first().to_number().unwrap_or(1.0) < 0.0).unwrap_or(false);
                keys.push((by, desc));
                i += 2;
            }
            for (by, desc) in keys.iter().rev() {
                order.sort_by(|&a, &b| {
                    let va = by.get(a).cloned().unwrap_or(FValue::Empty);
                    let vb = by.get(b).cloned().unwrap_or(FValue::Empty);
                    let cmp = sort_key_cmp(&va, &vb);
                    if *desc { cmp.reverse() } else { cmp }
                });
            }
            let mut out = Vec::with_capacity(rows * cols);
            for &row in &order {
                for j in 0..cols {
                    out.push(data[row * cols + j].clone());
                }
            }
            Ok(arr(Array::new(rows, cols, out)))
        }),
        "FILTER" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            // 条件按**形状**判方向，不能按元素个数：2×2 数据配 1×2 条件，
            // 个数也等于 2，若按长度猜就会错当成「按行」。
            let (ir, ic, include) = args.get(1).ok_or(ErrKind::Value)?.grid();
            if ir == 1 && ic == 1 {
                // 标量条件：TRUE 返回整块，FALSE 走 if_empty
                if include.first().map(truthy).unwrap_or(false) {
                    return Ok(arr(Array::new(rows, cols, data)));
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
            let mut out = Vec::new();
            let mut n = 0usize;
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
                for i in 0..rows {
                    for &c in &keep {
                        out.push(data[i * cols + c].clone());
                    }
                }
                n = keep.len();
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
                for &i in &keep {
                    for j in 0..cols {
                        out.push(data[i * cols + j].clone());
                    }
                }
                n = keep.len();
                Ok(arr(Array::new(n, cols, out)))
            }
        }),
        "TAKE" | "DROP" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
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
            for i in r0..r1 {
                for j in c0..c1 {
                    out.push(data[i * cols + j].clone());
                }
            }
            Ok(arr(Array::new(rs, cs, out)))
        }),
        "HSTACK" => r(|| {
            let grids: Vec<(usize, usize, Vec<FValue>)> = args.iter().map(|a| a.grid()).collect();
            let rows = grids.iter().map(|g| g.0).max().unwrap_or(0);
            let cols: usize = grids.iter().map(|g| g.1).sum();
            let mut out = vec![FValue::Error(ErrKind::Na); rows * cols];
            let mut c_off = 0;
            for (gr, gc, gd) in &grids {
                for i in 0..*gr {
                    for j in 0..*gc {
                        out[i * cols + c_off + j] = gd[i * gc + j].clone();
                    }
                }
                c_off += gc;
            }
            Ok(arr(Array::new(rows, cols, out)))
        }),
        "VSTACK" => r(|| {
            let grids: Vec<(usize, usize, Vec<FValue>)> = args.iter().map(|a| a.grid()).collect();
            let cols = grids.iter().map(|g| g.1).max().unwrap_or(0);
            let rows: usize = grids.iter().map(|g| g.0).sum();
            let mut out = vec![FValue::Error(ErrKind::Na); rows * cols];
            let mut r_off = 0;
            for (gr, gc, gd) in &grids {
                for i in 0..*gr {
                    for j in 0..*gc {
                        out[(r_off + i) * cols + j] = gd[i * gc + j].clone();
                    }
                }
                r_off += gr;
            }
            Ok(arr(Array::new(rows, cols, out)))
        }),
        "CHOOSECOLS" | "CHOOSEROWS" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            let mut idx: Vec<i64> = Vec::new();
            for a in args.iter().skip(1) {
                for v in a.values() {
                    if let Ok(n) = v.to_number() {
                        idx.push(n as i64);
                    }
                }
            }
            if name == "CHOOSECOLS" {
                let mut out = Vec::new();
                for i in 0..rows {
                    for &k in &idx {
                        let c = if k < 0 { cols as i64 + k } else { k - 1 };
                        if c < 0 || c >= cols as i64 {
                            return Err(ErrKind::Value);
                        }
                        out.push(data[i * cols + c as usize].clone());
                    }
                }
                Ok(arr(Array::new(rows, idx.len(), out)))
            } else {
                let mut out = Vec::new();
                for &k in &idx {
                    let rr = if k < 0 { rows as i64 + k } else { k - 1 };
                    if rr < 0 || rr >= rows as i64 {
                        return Err(ErrKind::Value);
                    }
                    for j in 0..cols {
                        out.push(data[rr as usize * cols + j].clone());
                    }
                }
                Ok(arr(Array::new(idx.len(), cols, out)))
            }
        }),
        "TOROW" | "TOCOL" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            let vals: Vec<FValue> = if name == "TOCOL" {
                let mut v = Vec::with_capacity(rows * cols);
                for c in 0..cols {
                    for r0 in 0..rows {
                        v.push(data[r0 * cols + c].clone());
                    }
                }
                v
            } else {
                data.clone()
            };
            if name == "TOCOL" {
                Ok(arr(Array::new(rows * cols, 1, vals)))
            } else {
                Ok(arr(Array::new(1, rows * cols, vals)))
            }
        }),
        "EXPAND" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            let new_rows = int_arg(args, 1)?.max(rows as i64) as usize;
            let new_cols = match args.get(2) {
                Some(_) => int_arg(args, 2)?.max(cols as i64) as usize,
                None => cols,
            };
            let pad = args.get(3).map(|a| a.first()).unwrap_or(FValue::Error(ErrKind::Na));
            let mut out = vec![pad; new_rows * new_cols];
            for i in 0..rows {
                for j in 0..cols {
                    out[i * new_cols + j] = data[i * cols + j].clone();
                }
            }
            Ok(arr(Array::new(new_rows, new_cols, out)))
        }),
        "MMULT" => r(|| {
            let (r1, c1, d1) = args.first().ok_or(ErrKind::Value)?.grid();
            let (r2, c2, d2) = args.get(1).ok_or(ErrKind::Value)?.grid();
            if c1 != r2 {
                return Err(ErrKind::Value);
            }
            let mut out = Vec::with_capacity(r1 * c2);
            for i in 0..r1 {
                for j in 0..c2 {
                    let mut s = 0.0;
                    for k in 0..c1 {
                        s += d1[i * c1 + k].to_number().unwrap_or(0.0) * d2[k * c2 + j].to_number().unwrap_or(0.0);
                    }
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
