//! 第二批函数：统计分布、工程、高级财务、数据库、杂项。
//! 由 `functions::call` 在未命中时转入，命中返回 Some。

use super::dist;
use super::functions::{
    arg, date_to_serial, first_numbers, flatten_values, int_arg, match_criteria, num, numbers,
    opt_num, r, range_dims, range_get, serial_to_date, text,
};
use super::{Arg, ErrKind, FValue};

/// 双参数分布的统一入口：Excel 的 cumulative 是第三个参数
fn cum(args: &[Arg], i: usize) -> Result<bool, ErrKind> {
    match args.get(i) {
        None => Ok(true),
        Some(a) => match a.first() {
            FValue::Empty => Ok(true),
            v => v.to_bool(),
        },
    }
}

fn check_p(p: f64) -> Result<f64, ErrKind> {
    if !(0.0..=1.0).contains(&p) {
        Err(ErrKind::Num)
    } else {
        Ok(p)
    }
}

pub fn call_extra(name: &str, args: &[Arg]) -> Option<FValue> {
    let v = match name {
        // ============================ 统计分布
        "NORM.DIST" | "NORMDIST" => r(|| {
            let x = num(args, 0)?;
            let mean = num(args, 1)?;
            let sd = num(args, 2)?;
            if sd <= 0.0 {
                return Err(ErrKind::Num);
            }
            let z = (x - mean) / sd;
            Ok(FValue::number(if cum(args, 3)? {
                dist::norm_cdf(z)
            } else {
                dist::norm_pdf(z) / sd
            }))
        }),
        "NORM.S.DIST" | "NORMSDIST" => r(|| {
            let z = num(args, 0)?;
            Ok(FValue::number(if cum(args, 1)? {
                dist::norm_cdf(z)
            } else {
                dist::norm_pdf(z)
            }))
        }),
        "NORM.INV" | "NORMINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let mean = num(args, 1)?;
            let sd = num(args, 2)?;
            if sd <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(mean + sd * dist::norm_inv(p)))
        }),
        "NORM.S.INV" | "NORMSINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            Ok(FValue::number(dist::norm_inv(p)))
        }),
        "LOGNORM.DIST" | "LOGNORMDIST" => r(|| {
            let x = num(args, 0)?;
            let mean = num(args, 1)?;
            let sd = num(args, 2)?;
            if x <= 0.0 || sd <= 0.0 {
                return Err(ErrKind::Num);
            }
            let z = (x.ln() - mean) / sd;
            Ok(FValue::number(if cum(args, 3)? {
                dist::norm_cdf(z)
            } else {
                dist::norm_pdf(z) / (x * sd)
            }))
        }),
        "LOGNORM.INV" | "LOGINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let mean = num(args, 1)?;
            let sd = num(args, 2)?;
            if sd <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((mean + sd * dist::norm_inv(p)).exp()))
        }),
        "EXPON.DIST" | "EXPONDIST" => r(|| {
            let x = num(args, 0)?;
            let lambda = num(args, 1)?;
            if x < 0.0 || lambda <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if cum(args, 2)? {
                1.0 - (-lambda * x).exp()
            } else {
                lambda * (-lambda * x).exp()
            }))
        }),
        "POISSON.DIST" | "POISSON" => r(|| {
            let x = num(args, 0)?.floor();
            let mean = num(args, 1)?;
            if x < 0.0 || mean < 0.0 {
                return Err(ErrKind::Num);
            }
            let pmf = (-mean + x * mean.ln() - dist::gamma_ln(x + 1.0)).exp();
            Ok(FValue::number(if cum(args, 2)? {
                1.0 - dist::gamma_p(x + 1.0, mean)
            } else {
                pmf
            }))
        }),
        "BINOM.DIST" | "BINOMDIST" => r(|| {
            let k = num(args, 0)?.floor();
            let n = num(args, 1)?.floor();
            let p = check_p(num(args, 2)?)?;
            if k < 0.0 || n < 0.0 || k > n {
                return Err(ErrKind::Num);
            }
            let ln_c = dist::gamma_ln(n + 1.0) - dist::gamma_ln(k + 1.0) - dist::gamma_ln(n - k + 1.0);
            let pmf = (ln_c + k * p.ln() + (n - k) * (1.0 - p).ln()).exp();
            Ok(FValue::number(if cum(args, 3)? {
                dist::ibeta(n - k, k + 1.0, 1.0 - p)
            } else {
                pmf
            }))
        }),
        "BINOM.INV" | "CRITBINOM" => r(|| {
            let n = num(args, 0)?.floor();
            let p = check_p(num(args, 1)?)?;
            let alpha = check_p(num(args, 2)?)?;
            let mut k = 0.0;
            while k <= n {
                let cdf = dist::ibeta(n - k, k + 1.0, 1.0 - p);
                if cdf >= alpha {
                    return Ok(FValue::number(k));
                }
                k += 1.0;
            }
            Ok(FValue::number(n))
        }),
        "NEGBINOM.DIST" | "NEGBINOMDIST" => r(|| {
            let f = num(args, 0)?.floor();
            let s = num(args, 1)?.floor();
            let p = check_p(num(args, 2)?)?;
            if f < 0.0 || s < 1.0 {
                return Err(ErrKind::Num);
            }
            let ln_c = dist::gamma_ln(f + s) - dist::gamma_ln(f + 1.0) - dist::gamma_ln(s);
            let pmf = (ln_c + s * p.ln() + f * (1.0 - p).ln()).exp();
            Ok(FValue::number(if cum(args, 3)? {
                dist::ibeta(s, f + 1.0, p)
            } else {
                pmf
            }))
        }),
        "HYPGEOM.DIST" | "HYPGEOMDIST" => r(|| {
            let x = num(args, 0)?.floor();
            let n = num(args, 1)?.floor();
            let k = num(args, 2)?.floor();
            let big = num(args, 3)?.floor();
            if x < 0.0 || x > n || k > big {
                return Err(ErrKind::Num);
            }
            let ln_c = |a: f64, b: f64| dist::gamma_ln(a + 1.0) - dist::gamma_ln(b + 1.0) - dist::gamma_ln(a - b + 1.0);
            let pmf = (ln_c(k, x) + ln_c(big - k, n - x) - ln_c(big, n)).exp();
            if !cum(args, 4)? {
                return Ok(FValue::number(pmf));
            }
            let mut s = 0.0;
            let mut i = x;
            while i >= 0.0 && i >= n - (big - k) {
                s += (ln_c(k, i) + ln_c(big - k, n - i) - ln_c(big, n)).exp();
                i -= 1.0;
            }
            Ok(FValue::number(s.min(1.0)))
        }),
        "GAMMA.DIST" | "GAMMADIST" => r(|| {
            let x = num(args, 0)?;
            let a = num(args, 1)?;
            let b = num(args, 2)?;
            if x < 0.0 || a <= 0.0 || b <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if cum(args, 3)? {
                dist::gamma_p(a, x / b)
            } else {
                dist::gamma_pdf(x / b, a) / b
            }))
        }),
        "GAMMA.INV" | "GAMMAINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let a = num(args, 1)?;
            let b = num(args, 2)?;
            if a <= 0.0 || b <= 0.0 {
                return Err(ErrKind::Num);
            }
            let hi = (a + 10.0 * a.sqrt() + 10.0) * 4.0;
            let x = dist::invert(|t| dist::gamma_p(a, t), p, 0.0, hi.max(10.0));
            Ok(FValue::number(x * b))
        }),
        "GAMMALN" | "GAMMALN.PRECISE" => r(|| {
            let x = num(args, 0)?;
            if x <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(dist::gamma_ln(x)))
        }),
        "BETA.DIST" | "BETADIST" => r(|| {
            let x = num(args, 0)?;
            let a = num(args, 1)?;
            let b = num(args, 2)?;
            if a <= 0.0 || b <= 0.0 || x < 0.0 || x > 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if cum(args, 3)? {
                dist::ibeta(a, b, x)
            } else {
                dist::beta_pdf(x, a, b)
            }))
        }),
        "BETA.INV" | "BETAINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let a = num(args, 1)?;
            let b = num(args, 2)?;
            if a <= 0.0 || b <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(dist::invert(|t| dist::ibeta(a, b, t), p, 0.0, 1.0)))
        }),
        "CHISQ.DIST" => r(|| {
            let x = num(args, 0)?;
            let df = num(args, 1)?;
            if x < 0.0 || df < 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if cum(args, 2)? {
                dist::chisq_cdf(x, df)
            } else {
                dist::chisq_pdf(x, df)
            }))
        }),
        "CHISQ.DIST.RT" | "CHIDIST" => r(|| {
            let x = num(args, 0)?;
            let df = num(args, 1)?;
            if x < 0.0 || df < 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(1.0 - dist::chisq_cdf(x, df)))
        }),
        "CHISQ.INV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let df = num(args, 1)?;
            Ok(FValue::number(dist::invert(|t| dist::chisq_cdf(t, df), p, 0.0, df * 20.0 + 100.0)))
        }),
        "CHISQ.INV.RT" | "CHIINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let df = num(args, 1)?;
            Ok(FValue::number(dist::invert(
                |t| 1.0 - dist::chisq_cdf(t, df),
                p,
                0.0,
                df * 20.0 + 100.0,
            )))
        }),
        "T.DIST" => r(|| {
            let t = num(args, 0)?;
            let df = num(args, 1)?;
            if df < 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if cum(args, 2)? {
                dist::t_cdf(t, df)
            } else {
                dist::t_pdf(t, df)
            }))
        }),
        "T.DIST.RT" => r(|| {
            let t = num(args, 0)?;
            let df = num(args, 1)?;
            Ok(FValue::number(1.0 - dist::t_cdf(t, df)))
        }),
        "T.DIST.2T" => r(|| {
            let t = num(args, 0)?.abs();
            let df = num(args, 1)?;
            Ok(FValue::number(2.0 * (1.0 - dist::t_cdf(t, df))))
        }),
        "T.INV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let df = num(args, 1)?;
            Ok(FValue::number(dist::invert(|t| dist::t_cdf(t, df), p, -1e4, 1e4)))
        }),
        "T.INV.2T" | "TINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let df = num(args, 1)?;
            let target = 1.0 - p / 2.0;
            Ok(FValue::number(dist::invert(|t| dist::t_cdf(t, df), target, -1e4, 1e4)))
        }),
        "F.DIST" => r(|| {
            let x = num(args, 0)?;
            let d1 = num(args, 1)?;
            let d2 = num(args, 2)?;
            if x < 0.0 || d1 < 1.0 || d2 < 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if cum(args, 3)? {
                dist::f_cdf(x, d1, d2)
            } else {
                dist::f_pdf(x, d1, d2)
            }))
        }),
        "F.DIST.RT" | "FDIST" => r(|| {
            let x = num(args, 0)?;
            let d1 = num(args, 1)?;
            let d2 = num(args, 2)?;
            Ok(FValue::number(1.0 - dist::f_cdf(x, d1, d2)))
        }),
        "F.INV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let d1 = num(args, 1)?;
            let d2 = num(args, 2)?;
            Ok(FValue::number(dist::invert(|t| dist::f_cdf(t, d1, d2), p, 0.0, 1e5)))
        }),
        "F.INV.RT" | "FINV" => r(|| {
            let p = check_p(num(args, 0)?)?;
            let d1 = num(args, 1)?;
            let d2 = num(args, 2)?;
            Ok(FValue::number(dist::invert(
                |t| 1.0 - dist::f_cdf(t, d1, d2),
                p,
                0.0,
                1e5,
            )))
        }),
        "WEIBULL.DIST" | "WEIBULL" => r(|| {
            let x = num(args, 0)?;
            let alpha = num(args, 1)?;
            let beta = num(args, 2)?;
            if x < 0.0 || alpha <= 0.0 || beta <= 0.0 {
                return Err(ErrKind::Num);
            }
            let z = (x / beta).powf(alpha);
            Ok(FValue::number(if cum(args, 3)? {
                1.0 - (-z).exp()
            } else {
                alpha / beta * (x / beta).powf(alpha - 1.0) * (-z).exp()
            }))
        }),
        "CONFIDENCE.NORM" | "CONFIDENCE" => r(|| {
            let alpha = check_p(num(args, 0)?)?;
            let sd = num(args, 1)?;
            let n = num(args, 2)?;
            if sd <= 0.0 || n < 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(dist::norm_inv(1.0 - alpha / 2.0) * sd / n.sqrt()))
        }),
        "CONFIDENCE.T" => r(|| {
            let alpha = check_p(num(args, 0)?)?;
            let sd = num(args, 1)?;
            let n = num(args, 2)?;
            if sd <= 0.0 || n < 2.0 {
                return Err(ErrKind::Num);
            }
            let t = dist::invert(|t| dist::t_cdf(t, n - 1.0), 1.0 - alpha / 2.0, -1e4, 1e4);
            Ok(FValue::number(t * sd / n.sqrt()))
        }),
        "STANDARDIZE" => r(|| {
            let x = num(args, 0)?;
            let mean = num(args, 1)?;
            let sd = num(args, 2)?;
            if sd <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((x - mean) / sd))
        }),
        "FISHER" => r(|| {
            let x = num(args, 0)?;
            if x <= -1.0 || x >= 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(0.5 * ((1.0 + x) / (1.0 - x)).ln()))
        }),
        "FISHERINV" => r(|| {
            let y = num(args, 0)?;
            let e = (2.0 * y).exp();
            Ok(FValue::number((e - 1.0) / (e + 1.0)))
        }),
        "SKEW" | "SKEW.P" => r(|| {
            let v = numbers(args, 0)?;
            let n = v.len() as f64;
            if n < 3.0 && name == "SKEW" || n < 1.0 {
                return Err(ErrKind::Div0);
            }
            let m = v.iter().sum::<f64>() / n;
            let s = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n).sqrt();
            if s == 0.0 {
                return Err(ErrKind::Div0);
            }
            let sum3 = v.iter().map(|x| ((x - m) / s).powi(3)).sum::<f64>();
            Ok(FValue::number(if name == "SKEW" {
                n / ((n - 1.0) * (n - 2.0)) * sum3
            } else {
                sum3 / n
            }))
        }),
        "KURT" => r(|| {
            let v = numbers(args, 0)?;
            let n = v.len() as f64;
            if n < 4.0 {
                return Err(ErrKind::Div0);
            }
            let m = v.iter().sum::<f64>() / n;
            let s = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n).sqrt();
            if s == 0.0 {
                return Err(ErrKind::Div0);
            }
            let sum4 = v.iter().map(|x| ((x - m) / s).powi(4)).sum::<f64>();
            Ok(FValue::number(
                n * (n + 1.0) / ((n - 1.0) * (n - 2.0) * (n - 3.0)) * sum4
                    - 3.0 * (n - 1.0).powi(2) / ((n - 2.0) * (n - 3.0)),
            ))
        }),
        "MINIFS" | "MAXIFS" => r(|| {
            let target = args.first().ok_or(ErrKind::Value)?.values();
            let pairs: Vec<(Vec<FValue>, String)> = (1..args.len())
                .step_by(2)
                .filter_map(|i| {
                    let rg = args.get(i)?.values();
                    let c = args.get(i + 1)?.first().to_text().ok()?;
                    Some((rg, c))
                })
                .collect();
            let mut best: Option<f64> = None;
            for (i, v) in target.iter().enumerate() {
                if !pairs.iter().all(|(rg, c)| rg.get(i).map(|x| match_criteria(x, c)).unwrap_or(false)) {
                    continue;
                }
                if let Ok(n) = v.to_number() {
                    best = Some(match best {
                        None => n,
                        Some(b) => {
                            if name == "MINIFS" {
                                b.min(n)
                            } else {
                                b.max(n)
                            }
                        }
                    });
                }
            }
            match best {
                Some(b) => Ok(FValue::number(b)),
                None => Ok(FValue::number(0.0)),
            }
        }),
        "TRIMMEAN" => r(|| {
            let mut v = first_numbers(args)?;
            let p = check_p(num(args, 1)?)?;
            if v.is_empty() {
                return Err(ErrKind::Num);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let drop_n = ((v.len() as f64 * p) / 2.0).floor() as usize;
            let slice = &v[drop_n..v.len() - drop_n];
            Ok(FValue::number(slice.iter().sum::<f64>() / slice.len() as f64))
        }),
        "Z.TEST" | "ZTEST" => r(|| {
            let v = first_numbers(args)?;
            let x = num(args, 1)?;
            let sigma = opt_num(args, 2, 0.0)?;
            let n = v.len() as f64;
            if n < 1.0 {
                return Err(ErrKind::Div0);
            }
            let mean = v.iter().sum::<f64>() / n;
            let s = if sigma > 0.0 {
                sigma
            } else {
                super::functions::std_dev(&v, true)?
            };
            Ok(FValue::number(1.0 - dist::norm_cdf((mean - x) / (s / n.sqrt()))))
        }),

        // ============================ 工程
        "BIN2DEC" | "BIN2HEX" | "BIN2OCT" | "OCT2DEC" | "OCT2HEX" | "OCT2BIN" | "HEX2DEC"
        | "HEX2BIN" | "HEX2OCT" | "DEC2BIN" | "DEC2HEX" | "DEC2OCT" => {
            r(|| base_convert(name, args))
        }
        "BITAND" => r(|| {
            let a = num(args, 0)? as i64;
            let b = num(args, 1)? as i64;
            if a < 0 || b < 0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((a & b) as f64))
        }),
        "BITOR" => r(|| {
            let a = num(args, 0)? as i64;
            let b = num(args, 1)? as i64;
            if a < 0 || b < 0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((a | b) as f64))
        }),
        "BITXOR" => r(|| {
            let a = num(args, 0)? as i64;
            let b = num(args, 1)? as i64;
            if a < 0 || b < 0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((a ^ b) as f64))
        }),
        "BITLSHIFT" => r(|| {
            let a = num(args, 0)? as i64;
            let s = int_arg(args, 1)?;
            if a < 0 || s.abs() > 53 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if s >= 0 {
                (a << s) as f64
            } else {
                (a >> (-s)) as f64
            }))
        }),
        "BITRSHIFT" => r(|| {
            let a = num(args, 0)? as i64;
            let s = int_arg(args, 1)?;
            if a < 0 || s.abs() > 53 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(if s >= 0 {
                (a >> s) as f64
            } else {
                (a << (-s)) as f64
            }))
        }),
        "DELTA" => r(|| {
            let a = num(args, 0)?;
            let b = opt_num(args, 1, 0.0)?;
            Ok(FValue::number(if (a - b).abs() < f64::EPSILON { 1.0 } else { 0.0 }))
        }),
        "GESTEP" => r(|| {
            let a = num(args, 0)?;
            let step = opt_num(args, 1, 0.0)?;
            Ok(FValue::number(if a >= step { 1.0 } else { 0.0 }))
        }),
        "ERF" | "ERF.PRECISE" => r(|| {
            let lo = num(args, 0)?;
            Ok(FValue::number(match args.len() {
                0 | 1 => dist::erf(lo),
                _ => {
                    let hi = num(args, 1)?;
                    dist::erf(hi) - dist::erf(lo)
                }
            }))
        }),
        "ERFC" | "ERFC.PRECISE" => r(|| Ok(FValue::number(dist::erfc(num(args, 0)?)))),
        "COMPLEX" | "IMREAL" | "IMAGINARY" | "IMABS" | "IMARGUMENT" | "IMCONJUGATE" | "IMADD"
        | "IMSUB" | "IMMUL" | "IMDIV" | "IMPOWER" | "IMSQRT" | "IMEXP" | "IMLN" | "IMLOG10"
        | "IMLOG2" | "IMSIN" | "IMCOS" | "IMTAN" | "IMSUM" | "IMPRODUCT" => {
            r(|| complex_fn(name, args))
        }
        "BASE" => r(|| {
            let n = num(args, 0)?.floor() as u64;
            let radix = int_arg(args, 1)?;
            if !(2..=36).contains(&radix) {
                return Err(ErrKind::Num);
            }
            Ok(FValue::Text(to_radix(n, radix as u32)))
        }),
        "DECIMAL" => r(|| {
            let s = text(args, 0)?;
            let radix = int_arg(args, 1)?;
            if !(2..=36).contains(&radix) {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(from_radix(&s, radix as u32)? as f64))
        }),

        // ============================ 高级财务
        "IRR" => r(|| {
            let flows = args.first().ok_or(ErrKind::Value)?.numbers()?;
            let guess = opt_num(args, 1, 0.1)?;
            let f = |rate: f64| -> f64 {
                flows
                    .iter()
                    .enumerate()
                    .map(|(i, v)| v / (1.0 + rate).powi(i as i32))
                    .sum()
            };
            Ok(FValue::number(dist::invert(f, 0.0, -0.9999, 10.0).max(-0.9999)))
        }),
        "MIRR" => r(|| {
            let flows = args.first().ok_or(ErrKind::Value)?.numbers()?;
            let fin = num(args, 1)?;
            let reinv = num(args, 2)?;
            let n = flows.len() as f64;
            if n < 2.0 {
                return Err(ErrKind::Div0);
            }
            let mut pv_neg = 0.0;
            let mut fv_pos = 0.0;
            for (i, v) in flows.iter().enumerate() {
                if *v < 0.0 {
                    pv_neg += v / (1.0 + fin).powi(i as i32);
                } else {
                    fv_pos += v * (1.0 + reinv).powf(n - 1.0 - i as f64);
                }
            }
            if pv_neg == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number((-fv_pos / pv_neg).powf(1.0 / (n - 1.0)) - 1.0))
        }),
        "XNPV" => r(|| {
            let rate = num(args, 0)?;
            let flows = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let dates = args.get(2).ok_or(ErrKind::Value)?.numbers()?;
            if flows.len() != dates.len() || flows.is_empty() {
                return Err(ErrKind::Num);
            }
            let d0 = dates[0];
            let mut s = 0.0;
            for i in 0..flows.len() {
                s += flows[i] / (1.0 + rate).powf((dates[i] - d0) / 365.0);
            }
            Ok(FValue::number(s))
        }),
        "XIRR" => r(|| {
            let flows = args.get(0).ok_or(ErrKind::Value)?.numbers()?;
            let dates = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            if flows.len() != dates.len() || flows.is_empty() {
                return Err(ErrKind::Num);
            }
            let d0 = dates[0];
            let f = |rate: f64| -> f64 {
                flows
                    .iter()
                    .enumerate()
                    .map(|(i, v)| v / (1.0 + rate).powf((dates[i] - d0) / 365.0))
                    .sum()
            };
            Ok(FValue::number(dist::invert(f, 0.0, -0.9999, 10.0)))
        }),
        "RATE" => r(|| {
            let nper = num(args, 0)?;
            let pmt_v = num(args, 1)?;
            let pv = num(args, 2)?;
            let fv_ = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            let guess = opt_num(args, 5, 0.1)?;
            let f = |rate: f64| -> f64 {
                if rate == 0.0 {
                    return pv + pmt_v * nper + fv_;
                }
                let p = (1.0 + rate).powf(nper);
                pv * p + pmt_v * (1.0 + rate * t) * (p - 1.0) / rate + fv_
            };
            let lo = (-0.9999f64).max(guess - 1.0);
            Ok(FValue::number(dist::invert(f, 0.0, lo, guess.max(1.0) + 1.0)))
        }),
        "EFFECT" => r(|| {
            let nominal = num(args, 0)?;
            let n = int_arg(args, 1)?;
            if nominal <= 0.0 || n < 1 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((1.0 + nominal / n as f64).powi(n as i32) - 1.0))
        }),
        "NOMINAL" => r(|| {
            let effect = num(args, 0)?;
            let n = int_arg(args, 1)?;
            if effect <= 0.0 || n < 1 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n as f64 * ((1.0 + effect).powf(1.0 / n as f64) - 1.0)))
        }),
        "DDB" => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 1)?;
            let life = num(args, 2)?;
            let period = num(args, 3)?;
            let factor = opt_num(args, 4, 2.0)?;
            if life <= 0.0 || period <= 0.0 {
                return Err(ErrKind::Num);
            }
            let rate = (factor / life).min(1.0);
            let mut book = cost;
            let mut dep = 0.0;
            for p in 1..=(period.floor() as i64) {
                dep = (book * rate).min(book - salvage).max(0.0);
                if p as f64 == period.floor() {
                    break;
                }
                book -= dep;
            }
            Ok(FValue::number(dep))
        }),
        "DB" => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 1)?;
            let life = num(args, 2)?;
            let period = num(args, 3)?;
            let month = opt_num(args, 4, 12.0)?;
            if cost <= 0.0 || life <= 0.0 || period <= 0.0 {
                return Err(ErrKind::Num);
            }
            let rate = (1.0 - (salvage / cost).powf(1.0 / life)).round_to(3);
            let mut book = cost;
            let mut dep = 0.0;
            for p in 1..=(period.ceil() as i64) {
                dep = if p == 1 {
                    book * rate * month / 12.0
                } else if (p as f64) > life {
                    book * rate * (12.0 - month) / 12.0
                } else {
                    book * rate
                };
                if p as f64 >= period.ceil() {
                    break;
                }
                book -= dep;
            }
            Ok(FValue::number(dep))
        }),
        "CUMIPMT" | "CUMPRINC" => r(|| {
            let rate = num(args, 0)?;
            let nper = num(args, 1)?;
            let pv = num(args, 2)?;
            let start = int_arg(args, 3)?;
            let end = int_arg(args, 4)?;
            let t = opt_num(args, 5, 0.0)?;
            // Excel：rate<=0、nper<=0、pv<=0、start<1、start>end 都是 #NUM!
            if rate <= 0.0 || nper <= 0.0 || pv <= 0.0 || start < 1 || end < start {
                return Err(ErrKind::Num);
            }
            let pay = pmt_of(rate, nper, pv, 0.0, t);
            let mut interest_sum = 0.0;
            let mut principal_sum = 0.0;
            for p in start..=end {
                let bal = balance(rate, nper, pv, pay, (p - 1) as f64, t);
                let interest = bal * rate;
                interest_sum += interest;
                principal_sum += pay - interest;
            }
            Ok(FValue::number(if name == "CUMIPMT" {
                -interest_sum
            } else {
                -principal_sum
            }))
        }),
        "PDURATION" => r(|| {
            let rate = num(args, 0)?;
            let pv = num(args, 1)?;
            let fv_ = num(args, 2)?;
            if rate <= 0.0 || pv <= 0.0 || fv_ <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(((fv_ / pv).ln()) / ((1.0 + rate).ln())))
        }),
        "RRI" => r(|| {
            let nper = num(args, 0)?;
            let pv = num(args, 1)?;
            let fv_ = num(args, 2)?;
            if nper <= 0.0 || pv <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((fv_ / pv).powf(1.0 / nper) - 1.0))
        }),
        "FVSCHEDULE" => r(|| {
            let principal = num(args, 0)?;
            let rates = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let mut v = principal;
            for r_ in rates {
                v *= 1.0 + r_;
            }
            Ok(FValue::number(v))
        }),
        "DOLLARDE" => r(|| {
            let frac = num(args, 0)?;
            let denom = int_arg(args, 1)?;
            if denom <= 0 {
                return Err(ErrKind::Div0);
            }
            let int_part = frac.trunc();
            let frac_part = frac - int_part;
            Ok(FValue::number(int_part + frac_part * 100.0 / denom as f64))
        }),
        "DOLLARFR" => r(|| {
            let dec = num(args, 0)?;
            let denom = int_arg(args, 1)?;
            if denom <= 0 {
                return Err(ErrKind::Div0);
            }
            let int_part = dec.trunc();
            let frac_part = dec - int_part;
            Ok(FValue::number(int_part + frac_part * denom as f64 / 100.0))
        }),

        // ============================ 数据库函数
        "DSUM" | "DAVERAGE" | "DCOUNT" | "DCOUNTA" | "DGET" | "DMAX" | "DMIN" | "DPRODUCT"
        | "DSTDEV" | "DSTDEVP" | "DVAR" | "DVARP" => r(|| database_fn(name, args)),

        // ============================ 杂项数学 / 文本
        "CEILING.PRECISE" | "ISO.CEILING" => r(|| {
            let n = num(args, 0)?;
            let s = opt_num(args, 1, 1.0)?.abs();
            if s == 0.0 {
                return Ok(FValue::number(0.0));
            }
            Ok(FValue::number((n / s).ceil() * s))
        }),
        "FLOOR.PRECISE" => r(|| {
            let n = num(args, 0)?;
            let s = opt_num(args, 1, 1.0)?.abs();
            if s == 0.0 {
                return Ok(FValue::number(0.0));
            }
            Ok(FValue::number((n / s).floor() * s))
        }),
        "SUMX2MY2" | "SUMX2PY2" | "SUMXMY2" => r(|| {
            let x = args.first().ok_or(ErrKind::Value)?.numbers()?;
            let y = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let n = x.len().min(y.len());
            let mut s = 0.0;
            for i in 0..n {
                s += match name {
                    "SUMX2MY2" => x[i] * x[i] - y[i] * y[i],
                    "SUMX2PY2" => x[i] * x[i] + y[i] * y[i],
                    _ => (x[i] - y[i]).powi(2),
                };
            }
            Ok(FValue::number(s))
        }),
        "SERIESSUM" => r(|| {
            let x = num(args, 0)?;
            let n = num(args, 1)?;
            let m = num(args, 2)?;
            let coeffs = args.get(3).ok_or(ErrKind::Value)?.numbers()?;
            let mut s = 0.0;
            for (i, a) in coeffs.iter().enumerate() {
                s += a * x.powf(n + i as f64 * m);
            }
            Ok(FValue::number(s))
        }),
        "MULTINOMIAL" => r(|| {
            let v = numbers(args, 0)?;
            let total: f64 = v.iter().sum();
            let mut ln = dist::gamma_ln(total + 1.0);
            for x in v {
                ln -= dist::gamma_ln(x + 1.0);
            }
            Ok(FValue::number(ln.exp().round()))
        }),
        "FACTDOUBLE" => r(|| {
            let n = int_arg(args, 0)?;
            if n < -1 {
                return Err(ErrKind::Num);
            }
            let mut v = 1.0;
            let mut i = n;
            while i > 1 {
                v *= i as f64;
                i -= 2;
            }
            Ok(FValue::number(v))
        }),
        "COMBINA" => r(|| {
            let n = int_arg(args, 0)?;
            let k = int_arg(args, 1)?;
            if n < 0 || k < 0 {
                return Err(ErrKind::Num);
            }
            let nn = n + k - 1;
            let mut v = 1.0f64;
            for i in 0..k {
                v = v * (nn - i) as f64 / (i + 1) as f64;
            }
            Ok(FValue::number(v.round()))
        }),
        "PERMUT" => r(|| {
            let n = int_arg(args, 0)?;
            let k = int_arg(args, 1)?;
            if n < 0 || k < 0 || k > n {
                return Err(ErrKind::Num);
            }
            let mut v = 1.0;
            for i in 0..k {
                v *= (n - i) as f64;
            }
            Ok(FValue::number(v))
        }),
        "SEC" => r(|| Ok(FValue::number(1.0 / num(args, 0)?.cos()))),
        "CSC" => r(|| Ok(FValue::number(1.0 / num(args, 0)?.sin()))),
        "COT" => r(|| Ok(FValue::number(1.0 / num(args, 0)?.tan()))),
        "SECH" => r(|| Ok(FValue::number(1.0 / num(args, 0)?.cosh()))),
        "CSCH" => r(|| Ok(FValue::number(1.0 / num(args, 0)?.sinh()))),
        "COTH" => r(|| Ok(FValue::number(1.0 / num(args, 0)?.tanh()))),
        "ACOT" => r(|| {
            let n = num(args, 0)?;
            Ok(FValue::number(std::f64::consts::FRAC_PI_2 - n.atan()))
        }),
        "ASINH" => r(|| Ok(FValue::number(num(args, 0)?.asinh()))),
        "ACOSH" => r(|| {
            let n = num(args, 0)?;
            if n < 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.acosh()))
        }),
        "ATANH" => r(|| {
            let n = num(args, 0)?;
            if n.abs() >= 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.atanh()))
        }),
        "TEXTBEFORE" => r(|| {
            let s = text(args, 0)?;
            let d = text(args, 1)?;
            match s.find(&d) {
                Some(i) => Ok(FValue::Text(s[..i].to_string())),
                None => Err(ErrKind::Na),
            }
        }),
        "TEXTAFTER" => r(|| {
            let s = text(args, 0)?;
            let d = text(args, 1)?;
            match s.find(&d) {
                Some(i) => Ok(FValue::Text(s[i + d.len()..].to_string())),
                None => Err(ErrKind::Na),
            }
        }),
        "UNICHAR" => r(|| {
            let n = int_arg(args, 0)?;
            let c = char::from_u32(n as u32).ok_or(ErrKind::Value)?;
            Ok(FValue::Text(c.to_string()))
        }),
        "UNICODE" => r(|| {
            let s = text(args, 0)?;
            let c = s.chars().next().ok_or(ErrKind::Value)?;
            Ok(FValue::number(c as u32 as f64))
        }),
        "ARRAYTOTEXT" | "VALUETOTEXT" => r(|| {
            let vals = flatten_values(args, 0);
            let parts: Vec<String> = vals.iter().map(|v| v.display()).collect();
            Ok(FValue::Text(parts.join(",")))
        }),
        "ROMAN" => r(|| {
            let n = int_arg(args, 0)?;
            if !(0..4000).contains(&n) {
                return Err(ErrKind::Value);
            }
            Ok(FValue::Text(to_roman(n as usize)))
        }),
        "ARABIC" => r(|| {
            let s = text(args, 0)?;
            Ok(FValue::number(from_roman(&s).ok_or(ErrKind::Value)? as f64))
        }),


        // ============================ 假设检验
        "T.TEST" | "TTEST" => r(|| {
            let a = first_numbers(args)?;
            let b = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let tails = int_arg(args, 2)?.max(1);
            let kind = int_arg(args, 3)?.max(1);
            if tails != 1 && tails != 2 {
                return Err(ErrKind::Num);
            }
            let (t, df) = match kind {
                1 => {
                    if a.len() != b.len() || a.len() < 2 {
                        return Err(ErrKind::Num);
                    }
                    let d: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x - y).collect();
                    let n = d.len() as f64;
                    let m = d.iter().sum::<f64>() / n;
                    let var = d.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
                    if var == 0.0 {
                        return Err(ErrKind::Div0);
                    }
                    (m / (var / n).sqrt(), n - 1.0)
                }
                2 | 3 => {
                    let n1 = a.len() as f64;
                    let n2 = b.len() as f64;
                    if n1 < 2.0 || n2 < 2.0 {
                        return Err(ErrKind::Num);
                    }
                    let m1 = a.iter().sum::<f64>() / n1;
                    let m2 = b.iter().sum::<f64>() / n2;
                    let v1 = a.iter().map(|x| (x - m1).powi(2)).sum::<f64>() / (n1 - 1.0);
                    let v2 = b.iter().map(|x| (x - m2).powi(2)).sum::<f64>() / (n2 - 1.0);
                    let se = (v1 / n1 + v2 / n2).sqrt();
                    if se == 0.0 {
                        return Err(ErrKind::Div0);
                    }
                    let t = (m1 - m2) / se;
                    let df = if kind == 2 {
                        n1 + n2 - 2.0
                    } else {
                        let num = (v1 / n1 + v2 / n2).powi(2);
                        let den = (v1 / n1).powi(2) / (n1 - 1.0) + (v2 / n2).powi(2) / (n2 - 1.0);
                        num / den
                    };
                    (t, df)
                }
                _ => return Err(ErrKind::Num),
            };
            let p = 1.0 - super::dist::t_cdf(t.abs(), df);
            Ok(FValue::number(if tails == 2 { 2.0 * p } else { p }))
        }),
        "F.TEST" | "FTEST" => r(|| {
            let a = first_numbers(args)?;
            let b = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let v1 = super::functions::std_dev(&a, true)?.powi(2);
            let v2 = super::functions::std_dev(&b, true)?.powi(2);
            if v1 == 0.0 || v2 == 0.0 {
                return Err(ErrKind::Div0);
            }
            let (f, d1, d2) = if v1 >= v2 {
                (v1 / v2, (a.len() - 1) as f64, (b.len() - 1) as f64)
            } else {
                (v2 / v1, (b.len() - 1) as f64, (a.len() - 1) as f64)
            };
            let p = 1.0 - super::dist::f_cdf(f, d1, d2);
            Ok(FValue::number((2.0 * p).min(1.0)))
        }),
        "CHISQ.TEST" | "CHITEST" => r(|| {
            let (r1, c1, obs) = args.first().ok_or(ErrKind::Value)?.grid();
            let (r2, c2, exp) = args.get(1).ok_or(ErrKind::Value)?.grid();
            if r1 != r2 || c1 != c2 || r1 < 2 || c1 < 2 {
                return Err(ErrKind::Num);
            }
            let mut chi = 0.0;
            for i in 0..r1 * c1 {
                let o = obs[i].to_number()?;
                let e = exp[i].to_number()?;
                if e == 0.0 {
                    return Err(ErrKind::Div0);
                }
                chi += (o - e).powi(2) / e;
            }
            let df = ((r1 - 1) * (c1 - 1)) as f64;
            Ok(FValue::number(1.0 - super::dist::chisq_cdf(chi, df)))
        }),

        // ============================ 回归
        "RSQ" => r(|| {
            let y = first_numbers(args)?;
            let x = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let r = pearson(&x, &y)?;
            Ok(FValue::number(r * r))
        }),
        "FORECAST" | "FORECAST.LINEAR" => r(|| {
            let x0 = num(args, 0)?;
            let y = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let x = args.get(2).ok_or(ErrKind::Value)?.numbers()?;
            let (m, b) = linfit(&x, &y)?;
            Ok(FValue::number(m * x0 + b))
        }),
        "STEYX" => r(|| {
            let y = first_numbers(args)?;
            let x = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let n = x.len().min(y.len());
            if n < 3 {
                return Err(ErrKind::Div0);
            }
            let (m, b) = linfit(&x[..n], &y[..n])?;
            let ss: f64 = (0..n).map(|i| (y[i] - (m * x[i] + b)).powi(2)).sum();
            Ok(FValue::number((ss / (n as f64 - 2.0)).sqrt()))
        }),
        "TREND" | "GROWTH" => r(|| {
            let ys = first_numbers(args)?;
            let xs = args.get(1).map(|a| a.numbers()).transpose()?.unwrap_or_else(|| {
                (1..=ys.len()).map(|i| i as f64).collect()
            });
            let log_mode = name == "GROWTH";
            let ys_t: Vec<f64> = if log_mode { ys.iter().map(|v| v.ln()).collect() } else { ys.clone() };
            let (m, b) = linfit(&xs, &ys_t)?;
            let targets = match args.get(2) {
                Some(a) => a.values().iter().filter_map(|v| v.to_number().ok()).collect::<Vec<f64>>(),
                None => xs.clone(),
            };
            let out: Vec<FValue> = targets
                .iter()
                .map(|x| {
                    let v = m * x + b;
                    FValue::number(if log_mode { v.exp() } else { v })
                })
                .collect();
            Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(1, out.len(), out))))
        }),
        "LINEST" => r(|| {
            let y = first_numbers(args)?;
            let x = args.get(1).map(|a| a.numbers()).transpose()?.unwrap_or_else(|| {
                (1..=y.len()).map(|i| i as f64).collect()
            });
            let (m, b) = linfit(&x, &y)?;
            let out = vec![FValue::number(m), FValue::number(b)];
            Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(1, 2, out))))
        }),

        // ============================ 百分位 / 排名补充
        "PERCENTRANK" | "PERCENTRANK.INC" => r(|| {
            let mut v = first_numbers(args)?;
            let x = num(args, 1)?;
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if v.is_empty() || x < v[0] || x > *v.last().unwrap() {
                return Err(ErrKind::Na);
            }
            let pos = v.iter().position(|y| (*y - x).abs() < 1e-12);
            let frac = match pos {
                Some(i) => i as f64 / (v.len() - 1) as f64,
                None => {
                    let mut k = 0;
                    while k + 1 < v.len() && v[k + 1] < x {
                        k += 1;
                    }
                    let lo = v[k];
                    let hi = v[k + 1];
                    (k as f64 + (x - lo) / (hi - lo)) / (v.len() - 1) as f64
                }
            };
            Ok(FValue::number((frac * 1000.0).floor() / 1000.0))
        }),
        "PERCENTILE.EXC" => r(|| {
            let mut v = first_numbers(args)?;
            let p = check_p(num(args, 1)?)?;
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if p <= 0.0 || p >= 1.0 || v.len() < 2 {
                return Err(ErrKind::Num);
            }
            let pos = p * (v.len() + 1) as f64 - 1.0;
            if pos < 0.0 || pos > (v.len() - 1) as f64 {
                return Err(ErrKind::Num);
            }
            let lo = pos.floor() as usize;
            let hi = pos.ceil() as usize;
            let frac = pos - lo as f64;
            Ok(FValue::number(v[lo] + (v[hi.min(v.len() - 1)] - v[lo]) * frac))
        }),
        "QUARTILE.EXC" => r(|| {
            let v = first_numbers(args)?;
            let q = int_arg(args, 1)?;
            if !(1..=3).contains(&q) {
                return Err(ErrKind::Num);
            }
            let mut sorted = v;
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let p = q as f64 / 4.0;
            let pos = p * (sorted.len() + 1) as f64 - 1.0;
            if pos < 0.0 || pos > (sorted.len() - 1) as f64 {
                return Err(ErrKind::Num);
            }
            let lo = pos.floor() as usize;
            let hi = pos.ceil() as usize;
            let frac = pos - lo as f64;
            Ok(FValue::number(sorted[lo] + (sorted[hi.min(sorted.len() - 1)] - sorted[lo]) * frac))
        }),
        "RANK.AVG" => r(|| {
            let x = num(args, 0)?;
            let v = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let asc = opt_num(args, 2, 0.0)? != 0.0;
            let mut sorted = v.clone();
            sorted.sort_by(|a, b| if asc { a.partial_cmp(b).unwrap() } else { b.partial_cmp(a).unwrap() });
            let hits: Vec<usize> = sorted.iter().enumerate()
                .filter(|(_, y)| (**y - x).abs() < 1e-12)
                .map(|(i, _)| i + 1)
                .collect();
            if hits.is_empty() {
                return Err(ErrKind::Na);
            }
            let avg = hits.iter().sum::<usize>() as f64 / hits.len() as f64;
            Ok(FValue::number(avg))
        }),

        // ============================ 单位换算
        "CONVERT" => r(|| {
            let v = num(args, 0)?;
            let from = text(args, 1)?;
            let to = text(args, 2)?;
            convert_unit(v, &from, &to)
        }),

        // ============================ 财务补充
        "VDB" => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 1)?;
            let life = num(args, 2)?;
            let start = num(args, 3)?;
            let end = num(args, 4)?;
            let factor = opt_num(args, 5, 2.0)?;
            if cost <= 0.0 || life <= 0.0 || start < 0.0 || end < start {
                return Err(ErrKind::Num);
            }
            let rate = (factor / life).min(1.0);
            let mut book = cost;
            let mut total = 0.0;
            let mut period = 0.0f64;
            while period < end {
                let remaining_life = life - period;
                let straight = if remaining_life > 0.0 { (book - salvage) / remaining_life } else { 0.0 };
                let mut dep = (book * rate).min(book - salvage).max(0.0);
                if straight > dep {
                    dep = straight.max(0.0);
                }
                let frac_start = (start - period).max(0.0).min(1.0);
                let frac_end = (end - period).max(0.0).min(1.0);
                total += dep * (frac_end - frac_start);
                book -= dep;
                period += 1.0;
                if book <= salvage {
                    break;
                }
            }
            Ok(FValue::number(total))
        }),
        "DURATION" | "MDURATION" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let coupon = num(args, 2)?;
            let yld = num(args, 3)?;
            let freq = int_arg(args, 4)?.max(1) as f64;
            let days = maturity - settlement;
            if days <= 0.0 {
                return Err(ErrKind::Num);
            }
            let n = (days / (365.0 / freq)).ceil().max(1.0);
            let c = coupon * 100.0 / freq;
            let y = yld / freq;
            let mut price = 0.0;
            let mut weighted = 0.0;
            for t in 1..=(n as i64) {
                let cf = c + if t as f64 == n { 100.0 } else { 0.0 };
                let pv = cf / (1.0 + y).powi(t as i32);
                price += pv;
                weighted += pv * (t as f64 / freq);
            }
            if price == 0.0 {
                return Err(ErrKind::Div0);
            }
            let dur = weighted / price;
            Ok(FValue::number(if name == "MDURATION" { dur / (1.0 + y) } else { dur }))
        }),
        "TBILLPRICE" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let discount = num(args, 2)?;
            let days = maturity - settlement;
            if days <= 0.0 || days > 365.0 || discount <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(100.0 * (1.0 - discount * days / 360.0)))
        }),
        "TBILLYIELD" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let price = num(args, 2)?;
            let days = maturity - settlement;
            if days <= 0.0 || price <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((100.0 - price) / price * 360.0 / days))
        }),
        "TBILLEQ" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let discount = num(args, 2)?;
            let days = maturity - settlement;
            if days <= 0.0 || days > 365.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(365.0 * discount / (360.0 - discount * days)))
        }),
        "AMORLINC" => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 3)?;
            let period = num(args, 4)?;
            let rate = num(args, 5)?;
            let first = cost * rate;
            if period == 0.0 {
                return Ok(FValue::number(first));
            }
            let depreciable = cost - salvage - first;
            let per_period = cost * rate;
            let n = (depreciable / per_period).ceil();
            Ok(FValue::number(if period <= n { per_period.min(cost - salvage) } else { 0.0 }))
        }),
        "AMORDEGRC" => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 3)?;
            let period = num(args, 4)?;
            let rate = num(args, 5)?;
            let life = 1.0 / rate;
            let coeff = if life < 3.0 { 1.0 } else if life < 5.0 { 1.5 } else if life <= 6.0 { 2.0 } else { 2.5 };
            let mut book = cost;
            let mut dep = 0.0;
            for p in 0..=(period as i64) {
                dep = (book * rate * coeff).min(book - salvage).max(0.0);
                if p as f64 == period {
                    break;
                }
                book -= dep;
            }
            Ok(FValue::number(dep))
        }),


        // ============================ 条件聚合补充
        "AVERAGEIFS" => r(|| {
            let avg = args.first().ok_or(ErrKind::Value)?.values();
            let pairs: Vec<(Vec<FValue>, String)> = (1..args.len())
                .step_by(2)
                .filter_map(|i| {
                    let rg = args.get(i)?.values();
                    let c = args.get(i + 1)?.first().to_text().ok()?;
                    Some((rg, c))
                })
                .collect();
            let len = pairs.iter().map(|(r, _)| r.len()).min().unwrap_or(0).min(avg.len());
            let mut sum = 0.0;
            let mut n = 0;
            for i in 0..len {
                if pairs.iter().all(|(rg, c)| rg.get(i).map(|x| match_criteria(x, c)).unwrap_or(false)) {
                    if let Ok(v) = avg[i].to_number() {
                        sum += v;
                        n += 1;
                    }
                }
            }
            if n == 0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(sum / n as f64))
        }),
        "MODE.SNGL" => r(|| {
            let v = numbers(args, 0)?;
            let mut best: Option<(f64, usize)> = None;
            for x in &v {
                let c = v.iter().filter(|y| (**y - *x).abs() < 1e-12).count();
                if c > 1 && best.map(|(_, bc)| c > bc).unwrap_or(true) {
                    best = Some((*x, c));
                }
            }
            best.map(|(x, _)| FValue::number(x)).ok_or(ErrKind::Na)
        }),
        "MODE.MULT" => r(|| {
            let v = numbers(args, 0)?;
            let mut out: Vec<(f64, usize)> = Vec::new();
            for x in &v {
                let c = v.iter().filter(|y| (**y - *x).abs() < 1e-12).count();
                if c > 1 && !out.iter().any(|(y, _)| (*y - *x).abs() < 1e-12) {
                    out.push((*x, c));
                }
            }
            if out.is_empty() {
                return Err(ErrKind::Na);
            }
            let top = out.iter().map(|(_, c)| *c).max().unwrap();
            let vals: Vec<FValue> = out
                .into_iter()
                .filter(|(_, c)| *c == top)
                .map(|(x, _)| FValue::number(x))
                .collect();
            Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(1, vals.len(), vals))))
        }),
        "AGGREGATE" => r(|| {
            let func = int_arg(args, 0)?;
            let opts = int_arg(args, 1)?;
            let ignore_errors = matches!(opts, 2 | 3 | 6 | 7);
            let mut vals: Vec<FValue> = args.iter().skip(2).flat_map(|a| a.values()).collect();
            if ignore_errors {
                vals.retain(|v| !v.is_error());
            }
            let nums: Vec<f64> = vals.iter().filter_map(|v| v.to_number().ok()).collect();
            let r = match func {
                1 => {
                    if nums.is_empty() { return Err(ErrKind::Div0); }
                    nums.iter().sum::<f64>() / nums.len() as f64
                }
                2 | 3 => {
                    let n = if func == 2 { nums.len() } else { vals.iter().filter(|v| **v != FValue::Empty).count() };
                    n as f64
                }
                4 => nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                5 => nums.iter().cloned().fold(f64::INFINITY, f64::min),
                6 => nums.iter().product::<f64>(),
                7 | 8 => super::functions::std_dev(&nums, func == 7)?,
                9 => nums.iter().sum::<f64>(),
                10 | 11 => super::functions::std_dev(&nums, func == 10)?.powi(2),
                12 => {
                    let mut v = nums.clone();
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    if v.is_empty() { return Err(ErrKind::Num); }
                    let m = v.len() / 2;
                    if v.len() % 2 == 0 { (v[m - 1] + v[m]) / 2.0 } else { v[m] }
                }
                _ => return Err(ErrKind::Value),
            };
            Ok(FValue::number(r))
        }),

        // ============================ 统计补充
        "FREQUENCY" => r(|| {
            let data = args.first().ok_or(ErrKind::Value)?.numbers()?;
            let mut bins = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            bins.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut counts = vec![0f64; bins.len() + 1];
            for x in &data {
                let mut placed = false;
                for (i, b) in bins.iter().enumerate() {
                    if *x <= *b {
                        counts[i] += 1.0;
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    counts[bins.len()] += 1.0;
                }
            }
            let vals: Vec<FValue> = counts.into_iter().map(FValue::number).collect();
            Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(vals.len(), 1, vals))))
        }),
        "PROB" => r(|| {
            let xs = args.first().ok_or(ErrKind::Value)?.numbers()?;
            let ps = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let lo = num(args, 2)?;
            let hi = opt_num(args, 3, lo)?;
            if xs.len() != ps.len() {
                return Err(ErrKind::Na);
            }
            let total: f64 = ps.iter().sum();
            if (total - 1.0).abs() > 1e-9 {
                return Err(ErrKind::Num);
            }
            let s: f64 = xs
                .iter()
                .zip(ps.iter())
                .filter(|(x, _)| **x >= lo && **x <= hi)
                .map(|(_, p)| *p)
                .sum();
            Ok(FValue::number(s))
        }),
        "STDEVA" | "VARA" => r(|| {
            let mut nums: Vec<f64> = Vec::new();
            for v in args.iter().flat_map(|a| a.values()) {
                match v {
                    FValue::Number(n) => nums.push(n),
                    FValue::Text(_) => nums.push(0.0),
                    FValue::Bool(b) => nums.push(if b { 1.0 } else { 0.0 }),
                    _ => {}
                }
            }
            let sd = super::functions::std_dev(&nums, true)?;
            Ok(FValue::number(if name == "VARA" { sd * sd } else { sd }))
        }),
        "COVARIANCE.P" | "COVARIANCE.S" | "COVARIANCE" => r(|| {
            let a = first_numbers(args)?;
            let b = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let n = a.len().min(b.len());
            let denom = if name == "COVARIANCE.P" { n as f64 } else { (n as f64) - 1.0 };
            if n < 2 {
                return Err(ErrKind::Div0);
            }
            let ma = a[..n].iter().sum::<f64>() / n as f64;
            let mb = b[..n].iter().sum::<f64>() / n as f64;
            let s: f64 = (0..n).map(|i| (a[i] - ma) * (b[i] - mb)).sum();
            Ok(FValue::number(s / denom))
        }),
        "PEARSON" => r(|| {
            let a = first_numbers(args)?;
            let b = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            Ok(FValue::number(pearson(&a, &b)?))
        }),

        // ============================ 文本 / 链接
        "HYPERLINK" => r(|| {
            let _ = text(args, 0)?;
            match args.get(1) {
                Some(a) => Ok(FValue::Text(a.first().to_text().unwrap_or_default())),
                None => Ok(FValue::Text(text(args, 0)?)),
            }
        }),
        "ENCODEURL" => r(|| {
            let s = text(args, 0)?;
            let mut out = String::new();
            for b in s.bytes() {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        out.push(b as char)
                    }
                    _ => out.push_str(&format!("%{b:02X}")),
                }
            }
            Ok(FValue::Text(out.as_str().into()))
        }),
        "TEXTSPLIT" => r(|| {
            let s = text(args, 0)?;
            let coldelim = text(args, 1)?;
            let rowdelim = args.get(2).map(|a| a.first().to_text()).transpose()?.unwrap_or_default();
            let rows_src: Vec<String> = if rowdelim.is_empty() {
                vec![s]
            } else {
                s.split(rowdelim.as_str()).map(|x| x.to_string()).collect()
            };
            let grid: Vec<Vec<FValue>> = rows_src
                .iter()
                .map(|line| {
                    line.split(coldelim.as_str())
                        .map(|cell| FValue::Text(cell.to_string()))
                        .collect()
                })
                .collect();
            let cols = grid.iter().map(|r| r.len()).max().unwrap_or(0);
            let mut data = Vec::new();
            for row in &grid {
                for c in 0..cols {
                    data.push(row.get(c).cloned().unwrap_or(FValue::Empty));
                }
            }
            Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(grid.len(), cols, data))))
        }),
        "WRAPROWS" | "WRAPCOLS" => r(|| {
            let vals = args.first().ok_or(ErrKind::Value)?.values();
            let n = int_arg(args, 1)?;
            if n <= 0 {
                return Err(ErrKind::Num);
            }
            let pad = args.get(2).cloned().map(|a| a.first()).unwrap_or(FValue::Error(ErrKind::Na));
            let n = n as usize;
            let rows = vals.len().div_ceil(n);
            let mut data = Vec::with_capacity(rows * n);
            for i in 0..rows * n {
                data.push(vals.get(i).cloned().unwrap_or_else(|| pad.clone()));
            }
            if name == "WRAPROWS" {
                Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(rows, n, data))))
            } else {
                // 按列填充：先转置
                let mut t = Vec::with_capacity(rows * n);
                for r in 0..rows {
                    for c in 0..n {
                        t.push(data[c * rows + r].clone());
                    }
                }
                Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(n, rows, t))))
            }
        }),

        // ============================ 数学 / 统计补充
        "ACOTH" => r(|| {
            let x = num(args, 0)?;
            if x.abs() <= 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(0.5 * ((x + 1.0) / (x - 1.0)).ln()))
        }),
        "GAMMA" => r(|| {
            let x = num(args, 0)?;
            if x <= 0.0 && x.fract() == 0.0 {
                return Err(ErrKind::Num);
            }
            let v = if x < 0.5 {
                std::f64::consts::PI / ((std::f64::consts::PI * x).sin() * super::dist::gamma_ln(1.0 - x).exp())
            } else {
                super::dist::gamma_ln(x).exp()
            };
            Ok(FValue::number(v))
        }),
        "GAUSS" => r(|| Ok(FValue::number(super::dist::norm_cdf(num(args, 0)?) - 0.5))),
        "PHI" => r(|| {
            let z = num(args, 0)?;
            Ok(FValue::number((-z * z / 2.0).exp() / (2.0 * std::f64::consts::PI).sqrt()))
        }),
        "PERMUTATIONA" => r(|| {
            let n = int_arg(args, 0)? as f64;
            let k = int_arg(args, 1)? as f64;
            if n < 0.0 || k < 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.powf(k)))
        }),
        "BINOM.DIST.RANGE" => r(|| {
            let n = int_arg(args, 0)?;
            let p = check_p(num(args, 1)?)?;
            let s1 = int_arg(args, 2)?;
            let s2 = match args.get(3) {
                Some(_) => int_arg(args, 3)?,
                None => s1,
            };
            if s1 > s2 || s2 > n {
                return Err(ErrKind::Num);
            }
            let mut acc = 0.0;
            for k in s1..=s2 {
                let c = binom_coef(n as usize, k as usize);
                acc += c * p.powi(k as i32) * (1.0 - p).powi((n - k) as i32);
            }
            Ok(FValue::number(acc))
        }),
        "TDIST" => r(|| {
            // 旧版：只给右尾/双尾，没有 cumulative 参数
            let x = num(args, 0)?.abs();
            let df = num(args, 1)?;
            let tails = int_arg(args, 2)?;
            let p = 1.0 - super::dist::t_cdf(x, df);
            Ok(FValue::number(if tails == 2 { 2.0 * p } else { p }))
        }),
        "COVAR" => r(|| {
            let a = first_numbers(args)?;
            let b = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
            let n = a.len().min(b.len());
            if n == 0 {
                return Err(ErrKind::Div0);
            }
            let ma = a[..n].iter().sum::<f64>() / n as f64;
            let mb = b[..n].iter().sum::<f64>() / n as f64;
            let s: f64 = (0..n).map(|i| (a[i] - ma) * (b[i] - mb)).sum();
            Ok(FValue::number(s / n as f64))
        }),
        "PERCENTOF" => r(|| {
            let sub: f64 = first_numbers(args)?.iter().sum();
            let all: f64 = args.get(1).ok_or(ErrKind::Value)?.numbers()?.iter().sum();
            if all == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(sub / all))
        }),
        "PERCENTRANK.EXC" => r(|| {
            let mut v = first_numbers(args)?;
            let x = num(args, 1)?;
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if v.is_empty() || x < v[0] || x > *v.last().unwrap() {
                return Err(ErrKind::Na);
            }
            let n = v.len() as f64;
            let frac = match v.iter().position(|y| (*y - x).abs() < 1e-12) {
                Some(i) => (i as f64 + 1.0) / (n + 1.0),
                None => {
                    let mut k = 0;
                    while k + 1 < v.len() && v[k + 1] < x {
                        k += 1;
                    }
                    let lo = v[k];
                    let hi = v[k + 1];
                    (k as f64 + 1.0 + (x - lo) / (hi - lo)) / (n + 1.0)
                }
            };
            Ok(FValue::number((frac * 1000.0).floor() / 1000.0))
        }),
        "LOGEST" => r(|| {
            let ys = first_numbers(args)?;
            let xs = args.get(1).map(|a| a.numbers()).transpose()?.unwrap_or_else(|| {
                (1..=ys.len()).map(|i| i as f64).collect()
            });
            let ln_y: Vec<f64> = ys.iter().map(|v| v.ln()).collect();
            let (m, b) = linfit(&xs, &ln_y)?;
            let out = vec![FValue::number(m.exp()), FValue::number(b.exp())];
            Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(1, 2, out))))
        }),

        // ============================ 矩阵
        "MDETERM" | "MINVERSE" => r(|| {
            let (rows, cols, data) = args.first().ok_or(ErrKind::Value)?.grid();
            if rows != cols || rows == 0 {
                return Err(ErrKind::Value);
            }
            let n = rows;
            let mut m: Vec<Vec<f64>> = (0..n)
                .map(|r| (0..n).map(|c| data[r * n + c].to_number().unwrap_or(0.0)).collect())
                .collect();
            if name == "MDETERM" {
                let mut det = 1.0;
                for i in 0..n {
                    let mut piv = i;
                    for r in i + 1..n {
                        if m[r][i].abs() > m[piv][i].abs() {
                            piv = r;
                        }
                    }
                    if m[piv][i].abs() < 1e-14 {
                        return Ok(FValue::number(0.0));
                    }
                    if piv != i {
                        m.swap(piv, i);
                        det = -det;
                    }
                    det *= m[i][i];
                    let d = m[i][i];
                    for r in i + 1..n {
                        let f = m[r][i] / d;
                        for c in i..n {
                            m[r][c] -= f * m[i][c];
                        }
                    }
                }
                return Ok(FValue::number(det));
            }
            // MINVERSE：高斯-约当
            let mut inv: Vec<Vec<f64>> = (0..n)
                .map(|r| (0..n).map(|c| if r == c { 1.0 } else { 0.0 }).collect())
                .collect();
            for i in 0..n {
                let mut piv = i;
                for r in i + 1..n {
                    if m[r][i].abs() > m[piv][i].abs() {
                        piv = r;
                    }
                }
                if m[piv][i].abs() < 1e-14 {
                    return Err(ErrKind::Num);
                }
                m.swap(i, piv);
                inv.swap(i, piv);
                let d = m[i][i];
                for c in 0..n {
                    m[i][c] /= d;
                    inv[i][c] /= d;
                }
                for r in 0..n {
                    if r == i {
                        continue;
                    }
                    let f = m[r][i];
                    if f != 0.0 {
                        for c in 0..n {
                            m[r][c] -= f * m[i][c];
                            inv[r][c] -= f * inv[i][c];
                        }
                    }
                }
            }
            let out: Vec<FValue> = inv.iter().flatten().map(|v| FValue::number(*v)).collect();
            Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(n, n, out))))
        }),

        // ============================ 文本补充
        "CLEAN" => r(|| {
            let s = text(args, 0)?;
            Ok(FValue::Text(s.chars().filter(|c| (*c as u32) > 31).collect()))
        }),
        "ASC" => r(|| {
            let s = text(args, 0)?;
            let out: String = s
                .chars()
                .map(|c| {
                    let u = c as u32;
                    if (0xFF01..=0xFF5E).contains(&u) {
                        char::from_u32(u - 0xFEE0).unwrap_or(c)
                    } else if u == 0x3000 {
                        ' '
                    } else {
                        c
                    }
                })
                .collect();
            Ok(FValue::Text(out.as_str().into()))
        }),
        "DOLLAR" | "FIXED" => r(|| {
            let v = num(args, 0)?;
            let dec = match args.get(1) {
                Some(_) => int_arg(args, 1)? as i32,
                None => 2,
            };
            let no_commas = match args.get(2) {
                Some(a) => a.first().to_bool().unwrap_or(false),
                None => false,
            };
            let body = format!("{:.*}", dec.max(0) as usize, v.abs());
            let (int_part, frac) = match body.split_once('.') {
                Some((i, f)) => (i.to_string(), Some(f.to_string())),
                None => (body.clone(), None),
            };
            let mut grouped = String::new();
            let chars: Vec<char> = int_part.chars().collect();
            for (i, ch) in chars.iter().enumerate() {
                if i > 0 && !no_commas && (chars.len() - i) % 3 == 0 {
                    grouped.push(',');
                }
                grouped.push(*ch);
            }
            let mut out = String::new();
            if v < 0.0 {
                out.push('-');
            }
            if name == "DOLLAR" {
                out.push('¥');
            }
            out.push_str(&grouped);
            if let Some(f) = frac {
                out.push('.');
                out.push_str(&f);
            }
            Ok(FValue::Text(out.as_str().into()))
        }),
        "NUMBERVALUE" => r(|| {
            let s = text(args, 0)?;
            let dec = args.get(1).map(|a| a.first().to_text()).transpose()?.unwrap_or(".".into());
            let grp = args.get(2).map(|a| a.first().to_text()).transpose()?.unwrap_or(",".into());
            let mut t = s.trim().to_string();
            if !grp.is_empty() {
                t = t.replace(grp.as_str(), "");
            }
            let t = if dec != "." { t.replace(dec.as_str(), ".") } else { t };
            let pct = t.ends_with('%');
            let t = t.trim_end_matches('%');
            match t.parse::<f64>() {
                Ok(v) => Ok(FValue::number(if pct { v / 100.0 } else { v })),
                Err(_) => Err(ErrKind::Value),
            }
        }),

        // ============================ 日期补充
        "DAYS360" => r(|| {
            let (y1, m1, d1) = serial_to_date(num(args, 0)?).ok_or(ErrKind::Num)?;
            let (y2, m2, d2) = serial_to_date(num(args, 1)?).ok_or(ErrKind::Num)?;
            let european = match args.get(2) {
                Some(a) => a.first().to_bool().unwrap_or(false),
                None => false,
            };
            let mut dd1 = d1 as i64;
            let mut dd2 = d2 as i64;
            if european {
                dd1 = dd1.min(30);
                dd2 = dd2.min(30);
            } else {
                if dd1 == 31 {
                    dd1 = 30;
                }
                if dd2 == 31 && dd1 == 30 {
                    dd2 = 30;
                }
            }
            let diff = (y2 as i64 - y1 as i64) * 360
                + (m2 as i64 - m1 as i64) * 30
                + (dd2 - dd1);
            Ok(FValue::number(diff as f64))
        }),
        "DATEVALUE" => r(|| {
            let s = text(args, 0)?.trim().to_string();
            let cleaned = s
                .replace('年', "-")
                .replace('月', "-")
                .replace('日', "")
                .replace('/', "-");
            let parts: Vec<&str> = cleaned.split('-').filter(|p| !p.is_empty()).collect();
            if parts.len() != 3 {
                return Err(ErrKind::Value);
            }
            let a = parts[0].parse::<i32>().map_err(|_| ErrKind::Value)?;
            let b = parts[1].parse::<u32>().map_err(|_| ErrKind::Value)?;
            let c = parts[2].parse::<u32>().map_err(|_| ErrKind::Value)?;
            // 年份在最前 → ISO；否则按 月/日/年
            let (y, m, d): (i32, u32, u32) = if a > 31 { (a, b, c) } else { (c as i32, b, a as u32) };
            date_to_serial(y, m, d).map(FValue::number).ok_or(ErrKind::Value)
        }),
        "TIMEVALUE" => r(|| {
            let s = text(args, 0)?.trim().to_ascii_uppercase();
            let pm = s.ends_with("PM");
            let am = s.ends_with("AM");
            let body = s.trim_end_matches("PM").trim_end_matches("AM").trim();
            let parts: Vec<&str> = body.split(':').collect();
            if parts.len() < 2 {
                return Err(ErrKind::Value);
            }
            let mut h: f64 = parts[0].parse().map_err(|_| ErrKind::Value)?;
            let m: f64 = parts[1].parse().map_err(|_| ErrKind::Value)?;
            let sec: f64 = if parts.len() > 2 { parts[2].parse().unwrap_or(0.0) } else { 0.0 };
            if pm && h < 12.0 {
                h += 12.0;
            }
            if am && h == 12.0 {
                h = 0.0;
            }
            Ok(FValue::number((h * 3600.0 + m * 60.0 + sec) / 86400.0))
        }),
        "ISOWEEKNUM" => r(|| {
            let serial = num(args, 0)?;
            let (y, m, d) = serial_to_date(serial).ok_or(ErrKind::Num)?;
            Ok(FValue::number(iso_week(y, m, d) as f64))
        }),

        // ============================ 债券：票息日推算
        "COUPPCD" | "COUPNCD" | "COUPNUM" | "COUPDAYS" | "COUPDAYBS" | "COUPDAYSNC" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let freq = int_arg(args, 2)?.max(1);
            if settlement >= maturity {
                return Err(ErrKind::Num);
            }
            let (d_prev, _, d_next, _) = coupon_frame(settlement, maturity, freq)?;
            let period = d_next - d_prev;
            match name {
                "COUPPCD" => Ok(FValue::number(d_prev)),
                "COUPNCD" => Ok(FValue::number(d_next)),
                "COUPNUM" => {
                    let (_, _, _, n) = coupon_frame(settlement, maturity, freq)?;
                    Ok(FValue::number(n as f64))
                }
                "COUPDAYS" => Ok(FValue::number(period)),
                "COUPDAYBS" => Ok(FValue::number(settlement - d_prev)),
                _ => Ok(FValue::number(d_next - settlement)),
            }
        }),
        "PRICE" | "YIELD" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let rate = num(args, 2)?;
            // PRICE/YIELD: (settlement, maturity, rate, yld|pr, redemption, frequency, [basis])
            let freq = int_arg(args, 5)?.max(1);
            if settlement >= maturity {
                return Err(ErrKind::Num);
            }
            let (s, m, e, n) = coupon_frame(settlement, maturity, freq)?;
            let dsc = e - s;
            let a = s - m;
            let e_len = e - m;
            let frac = if e_len > 0.0 { dsc / e_len } else { 0.0 };
            let accrued_frac = if e_len > 0.0 { a / e_len } else { 0.0 };
            let c = rate * 100.0 / freq as f64;

            if name == "PRICE" {
                let yld = num(args, 3)?;
                let redemption = if args.len() > 4 { num(args, 4)? } else { 100.0 };
                let y = yld / freq as f64;
                if y <= -1.0 {
                    return Err(ErrKind::Num);
                }
                let mut pv = 0.0;
                for k in 1..=n {
                    let t = (k as f64 - 1.0) + frac;
                    pv += c / (1.0 + y).powf(t);
                }
                let red = redemption / (1.0 + y).powf((n as f64 - 1.0) + frac);
                return Ok(FValue::number(pv + red - c * accrued_frac));
            }

            // YIELD：用二分法反解
            let price = num(args, 3)?;
            let redemption = if args.len() > 4 { num(args, 4)? } else { 100.0 };
            let f = |yld: f64| -> f64 {
                let y = yld / freq as f64;
                let mut pv = 0.0;
                for k in 1..=n {
                    let t = (k as f64 - 1.0) + frac;
                    pv += c / (1.0 + y).powf(t);
                }
                pv + redemption / (1.0 + y).powf((n as f64 - 1.0) + frac) - c * accrued_frac
            };
            let mut lo = -0.99;
            let mut hi = 10.0;
            if (f(lo) - price) * (f(hi) - price) > 0.0 {
                return Err(ErrKind::Num);
            }
            for _ in 0..200 {
                let mid = (lo + hi) / 2.0;
                if f(mid) > price {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            Ok(FValue::number((lo + hi) / 2.0))
        }),
        "DISC" | "INTRATE" | "RECEIVED" | "PRICEDISC" | "YIELDDISC" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let days = maturity - settlement;
            if days <= 0.0 {
                return Err(ErrKind::Num);
            }
            match name {
                "DISC" => {
                    let pr = num(args, 2)?;
                    let red = num(args, 3)?;
                    if red == 0.0 {
                        return Err(ErrKind::Div0);
                    }
                    Ok(FValue::number((red - pr) / red * 360.0 / days))
                }
                "INTRATE" => {
                    let inv = num(args, 2)?;
                    let red = num(args, 3)?;
                    if inv == 0.0 {
                        return Err(ErrKind::Div0);
                    }
                    Ok(FValue::number((red - inv) / inv * 360.0 / days))
                }
                "RECEIVED" => {
                    let inv = num(args, 2)?;
                    let disc = num(args, 3)?;
                    let denom = 1.0 - disc * days / 360.0;
                    if denom <= 0.0 {
                        return Err(ErrKind::Num);
                    }
                    Ok(FValue::number(inv / denom))
                }
                "PRICEDISC" => {
                    let red = num(args, 3)?;
                    let disc = num(args, 2)?;
                    Ok(FValue::number(red - disc * red * days / 360.0))
                }
                _ => {
                    // YIELDDISC(settlement, maturity, pr, redemption)
                    let pr = num(args, 2)?;
                    let red = num(args, 3)?;
                    if pr == 0.0 {
                        return Err(ErrKind::Div0);
                    }
                    Ok(FValue::number((red - pr) / pr * 360.0 / days))
                }
            }
        }),
        "PRICEMAT" | "YIELDMAT" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            let issue = num(args, 2)?.floor();
            let rate = num(args, 3)?;
            let dsm = maturity - settlement;
            let dim = maturity - issue;
            if dsm <= 0.0 || dim <= 0.0 {
                return Err(ErrKind::Num);
            }
            if name == "PRICEMAT" {
                let yld = num(args, 4)?;
                let price = (100.0 + dim * rate * 100.0 / 360.0) / (1.0 + dsm * yld / 360.0);
                Ok(FValue::number(price - dsm * rate * 100.0 / 360.0))
            } else {
                let price = num(args, 4)?;
                let num_ = (100.0 + dim * rate * 100.0 / 360.0) - price - dsm * rate * 100.0 / 360.0;
                Ok(FValue::number(num_ / price * 360.0 / dsm))
            }
        }),

        // ============================ 正则（Excel 365）
        "REGEXTEST" => r(|| {
            let text_ = text(args, 0)?;
            let pat = text(args, 1)?;
            match regex::Regex::new(&pat) {
                Ok(re) => Ok(FValue::Bool(re.is_match(&text_))),
                Err(_) => Err(ErrKind::Value),
            }
        }),
        "REGEXEXTRACT" => r(|| {
            let text_ = text(args, 0)?;
            let pat = text(args, 1)?;
            let re = regex::Regex::new(&pat).map_err(|_| ErrKind::Value)?;
            match re.captures(&text_) {
                Some(caps) => {
                    let groups: Vec<FValue> = (0..caps.len())
                        .map(|i| FValue::Text(caps.get(i).map(|m| m.as_str().to_string()).unwrap_or_default()))
                        .collect();
                    if groups.len() == 1 {
                        Ok(groups.into_iter().next().unwrap())
                    } else {
                        Ok(FValue::Array(std::rc::Rc::new(super::value::Array::new(
                            1,
                            groups.len(),
                            groups,
                        ))))
                    }
                }
                None => Err(ErrKind::Na),
            }
        }),
        "REGEXREPLACE" => r(|| {
            let text_ = text(args, 0)?;
            let pat = text(args, 1)?;
            let rep = text(args, 2)?;
            let re = regex::Regex::new(&pat).map_err(|_| ErrKind::Value)?;
            // Excel 用 $1 还是 \1：regex crate 用 $1，与 Excel 一致
            Ok(FValue::Text(re.replace_all(&text_, rep.as_str()).to_string()))
        }),

        // ============================ 国际化工作日
        "NETWORKDAYS.INTL" | "WORKDAY.INTL" => r(|| {
            let weekend_mask = |a: Option<&Arg>| -> Result<[bool; 7], ErrKind> {
                let mut m = [false; 7]; // 0=周一 .. 6=周日
                match a.map(|x| x.first()) {
                    None => {
                        m[5] = true;
                        m[6] = true;
                    }
                    Some(FValue::Number(n)) => {
                        let code = n as i32;
                        let idx: Vec<usize> = match code {
                            1 => vec![5, 6],
                            2 => vec![6, 0],
                            3 => vec![0, 1],
                            4 => vec![1, 2],
                            5 => vec![2, 3],
                            6 => vec![3, 4],
                            7 => vec![4, 5],
                            11 => vec![6],
                            12 => vec![0],
                            13 => vec![1],
                            14 => vec![2],
                            15 => vec![3],
                            16 => vec![4],
                            17 => vec![5],
                            _ => return Err(ErrKind::Num),
                        };
                        for i in idx {
                            m[i] = true;
                        }
                    }
                    Some(v) => {
                        let s = v.to_text().map_err(|_| ErrKind::Value)?;
                        let chars: Vec<char> = s.chars().collect();
                        if chars.len() != 7 {
                            return Err(ErrKind::Value);
                        }
                        for (i, c) in chars.iter().enumerate() {
                            if *c == '1' {
                                m[i] = true;
                            } else if *c != '0' {
                                return Err(ErrKind::Value);
                            }
                        }
                    }
                }
                Ok(m)
            };
            let idx_for = |serial: f64, m: &[bool; 7]| -> usize {
                let wd = ((serial.floor() as i64) - 1).rem_euclid(7); // 0=周日
                // 转成 0=周一
                (wd + 6) as usize % 7
            };
            let holidays: Vec<i64> = match args.get(args.len() - 1) {
                Some(a) if args.len() >= 4 => a
                    .values()
                    .iter()
                    .filter_map(|v| v.to_number().ok())
                    .map(|v| v.floor() as i64)
                    .collect(),
                _ => Vec::new(),
            };
            let is_workday = |d: i64, m: &[bool; 7], hol: &[i64]| -> bool {
                let wd = ((d - 1).rem_euclid(7)) as usize;
                let mon_based = (wd + 6) % 7;
                !m[mon_based] && !hol.contains(&d)
            };
            if name == "NETWORKDAYS.INTL" {
                let start = num(args, 0)?.floor() as i64;
                let end = num(args, 1)?.floor() as i64;
                let m = weekend_mask(args.get(2))?;
                let (lo, hi, sign) = if start <= end { (start, end, 1.0) } else { (end, start, -1.0) };
                let mut count = 0i64;
                let mut d = lo;
                while d <= hi {
                    if is_workday(d, &m, &holidays) {
                        count += 1;
                    }
                    d += 1;
                }
                Ok(FValue::number(sign * count as f64))
            } else {
                let start = num(args, 0)?.floor() as i64;
                let days = num(args, 1)? as i64;
                let m = weekend_mask(args.get(2))?;
                let mut d = start;
                let step = if days >= 0 { 1 } else { -1 };
                let mut left = days.abs();
                while left > 0 {
                    d += step;
                    if is_workday(d, &m, &holidays) {
                        left -= 1;
                    }
                }
                Ok(FValue::number(d as f64))
            }
        }),

        // ============================ 不规则首/末期的债券
        "ODDFPRICE" | "ODDFYIELD" | "ODDLPRICE" | "ODDLYIELD" => r(|| {
            let settlement = num(args, 0)?.floor();
            let maturity = num(args, 1)?.floor();
            if settlement >= maturity {
                return Err(ErrKind::Num);
            }
            let odd_first = name.starts_with("ODDF");
            // 参数位置：ODDF 是 (s,m,issue,first_coupon,rate,y|pr,red,freq)，
            //           ODDL 是 (s,m,last_interest,rate,y|pr,red,freq)
            let rate = if odd_first { num(args, 4)? } else { num(args, 3)? };
            let y_or_price = if odd_first { num(args, 5)? } else { num(args, 4)? };
            let redemption = if odd_first { num(args, 6)? } else { num(args, 5)? };
            let freq = if odd_first { int_arg(args, 7)? } else { int_arg(args, 6)? }.max(1);
            let anchor = num(args, 2)?.floor(); // issue 或 last_interest

            let flows = odd_cashflows(settlement, maturity, anchor, rate, freq, odd_first)?;
            if flows.items.is_empty() {
                return Err(ErrKind::Num);
            }
            let accrued = flows.accrued;
            let price_at = |yld: f64| -> f64 {
                let y = yld / freq as f64;
                let mut pv = 0.0;
                for (t, cf) in &flows.items {
                    pv += cf / (1.0 + y).powf(*t);
                }
                pv + redemption / (1.0 + y).powf(flows.last_t) - accrued
            };

            if name.ends_with("PRICE") {
                Ok(FValue::number(price_at(y_or_price)))
            } else {
                let target = y_or_price;
                let mut lo = -0.5;
                let mut hi = 5.0;
                let mut flo = price_at(lo) - target;
                let fhi = price_at(hi) - target;
                if flo * fhi > 0.0 {
                    return Err(ErrKind::Num);
                }
                for _ in 0..200 {
                    let mid = (lo + hi) / 2.0;
                    let fm = price_at(mid) - target;
                    if fm == 0.0 {
                        break;
                    }
                    if flo * fm < 0.0 {
                        hi = mid;
                    } else {
                        lo = mid;
                        flo = fm;
                    }
                }
                Ok(FValue::number((lo + hi) / 2.0))
            }
        }),

        // ============================ 贝塞尔函数
        "BESSELJ" | "BESSELI" => r(|| {
            let n = int_arg(args, 0)?;
            let x = num(args, 1)?;
            if n < 0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(bessel_ji(n as u32, x, name == "BESSELI")))
        }),
        "BESSELK" | "BESSELY" => r(|| {
            let n = int_arg(args, 0)?;
            let x = num(args, 1)?;
            if n < 0 || x <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(bessel_ky(n as u32, x, name == "BESSELK")?))
        }),

        // ============================ FORECAST.ETS 家族
        "FORECAST.ETS" | "FORECAST.ETS.CONFINT" | "FORECAST.ETS.SEASONALITY" | "FORECAST.ETS.STAT" => r(|| {
            // FORECAST.ETS / CONFINT 是 (target, values, timeline)，
            // SEASONALITY / STAT 是 (values, timeline, ...)
            let (vals, times) = if matches!(name, "FORECAST.ETS" | "FORECAST.ETS.CONFINT") {
                let v = args.get(1).ok_or(ErrKind::Value)?.numbers()?;
                let t = args.get(2).ok_or(ErrKind::Value)?.numbers()?;
                (v, t)
            } else {
                let v = first_numbers(args)?;
                let t = args.get(1).map(|a| a.numbers()).transpose()?.unwrap_or_else(|| {
                    (1..=v.len()).map(|i| i as f64).collect()
                });
                (v, t)
            };
            if vals.len() < 4 || times.len() != vals.len() {
                return Err(ErrKind::Num);
            }
            let step = if times.len() > 1 {
                let mut diffs: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
                diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
                diffs[diffs.len() / 2].max(1e-9)
            } else {
                1.0
            };
            let season_arg = match name {
                "FORECAST.ETS.SEASONALITY" => 0.0,
                "FORECAST.ETS.STAT" => match args.get(3) {
                    Some(a) => a.first().to_number().unwrap_or(0.0),
                    None => 0.0,
                },
                _ => match args.get(3) {
                    Some(a) => a.first().to_number().unwrap_or(0.0),
                    None => 0.0,
                },
            };
            let period = if season_arg >= 2.0 {
                season_arg as usize
            } else if season_arg == 1.0 {
                1
            } else {
                detect_seasonality(&vals)
            };

            if name == "FORECAST.ETS.SEASONALITY" {
                return Ok(FValue::number(period as f64));
            }

            let fit = ets_fit(&vals, period)?;

            if name == "FORECAST.ETS" {
                let target = num(args, 0)?;
                let h = ((target - times[times.len() - 1]) / step).round().max(0.0) as usize;
                return Ok(FValue::number(ets_predict(&fit, period, h)));
            }

            if name == "FORECAST.ETS.CONFINT" {
                let target = num(args, 0)?;
                let conf = match args.get(3) {
                    Some(a) => a.first().to_number().unwrap_or(0.95),
                    None => 0.95,
                };
                if !(0.0..1.0).contains(&conf) {
                    return Err(ErrKind::Num);
                }
                let z = super::dist::norm_inv(0.5 + conf / 2.0);
                let rmse = fit.rmse.max(0.0);
                let h = ((target - times[times.len() - 1]) / step).round().max(1.0);
                return Ok(FValue::number(z * rmse * h.sqrt()));
            }

            // FORECAST.ETS.STAT
            let which = int_arg(args, 2)?;
            Ok(FValue::number(match which {
                1 => fit.alpha,
                2 => fit.beta,
                3 => fit.gamma,
                4 => fit.mase,
                5 => fit.smape,
                6 => fit.mae,
                7 => fit.rmse,
                _ => return Err(ErrKind::Num),
            }))
        }),
        _ => return None,
    };
    Some(v)
}

// ---------------------------------------------------------------- 工程：进制

fn to_radix(mut n: u64, radix: u32) -> String {
    if n == 0 {
        return "0".into();
    }
    let digits = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut out = Vec::new();
    while n > 0 {
        out.push(digits[(n % radix as u64) as usize]);
        n /= radix as u64;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

fn from_radix(s: &str, radix: u32) -> Result<u64, ErrKind> {
    let mut v: u64 = 0;
    for c in s.trim().chars() {
        let d = c.to_digit(radix).ok_or(ErrKind::Num)?;
        v = v.checked_mul(radix as u64).ok_or(ErrKind::Num)? + d as u64;
    }
    Ok(v)
}

fn base_convert(name: &str, args: &[Arg]) -> Result<FValue, ErrKind> {
    // Excel 限制：BIN 10 位、OCT 10 位、HEX 10 位，负数用补码
    let (from_radix_n, bits_of, to_radix_n): (u32, u32, u32) = match name {
        "BIN2DEC" => (2, 10, 10),
        "BIN2HEX" => (2, 10, 16),
        "BIN2OCT" => (2, 10, 8),
        "OCT2DEC" => (8, 30, 10),
        "OCT2HEX" => (8, 30, 16),
        "OCT2BIN" => (8, 30, 2),
        "HEX2DEC" => (16, 40, 10),
        "HEX2BIN" => (16, 40, 2),
        "HEX2OCT" => (16, 40, 8),
        "DEC2BIN" => (10, 0, 2),
        "DEC2HEX" => (10, 0, 16),
        "DEC2OCT" => (10, 0, 8),
        _ => return Err(ErrKind::Name),
    };

    if name.starts_with("DEC2") {
        let n = num(args, 0)?.trunc() as i64;
        let target_bits = match to_radix_n {
            2 => 10,
            8 => 30,
            _ => 40,
        };
        let max_pos = (1i64 << (target_bits - 1)) - 1;
        let min_neg = -(1i64 << (target_bits - 1));
        if n > max_pos || n < min_neg {
            return Err(ErrKind::Num);
        }
        let v = if n < 0 {
            ((1i64 << target_bits) + n) as u64
        } else {
            n as u64
        };
        let s = to_radix(v, to_radix_n);
        return Ok(if n == 0 { FValue::Text("0".into()) } else { FValue::Text(s) });
    }

    let s = text(args, 0)?;
    let raw = from_radix(&s, from_radix_n)?;
    let negative = (raw >> (bits_of - 1)) & 1 == 1 && s.trim().len() as u32 >= bits_of / 4;
    let signed = if negative {
        raw as i64 - (1i64 << bits_of)
    } else {
        raw as i64
    };

    if to_radix_n == 10 {
        return Ok(FValue::number(signed as f64));
    }
    let bits_out = if to_radix_n == 2 { 10 } else { 30 };
    if signed < 0 && (-signed) > (1i64 << (bits_out - 1)) {
        return Err(ErrKind::Num);
    }
    let v = if signed < 0 {
        ((1i64 << bits_out) + signed) as u64
    } else {
        signed as u64
    };
    Ok(FValue::Text(to_radix(v, to_radix_n)))
}

// ---------------------------------------------------------------- 复数

#[derive(Clone, Copy, Debug)]
struct Complex {
    re: f64,
    im: f64,
}

fn parse_complex(s: &str) -> Result<Complex, ErrKind> {
    let t: String = s.trim().replace(' ', "");
    if t.is_empty() {
        return Ok(Complex { re: 0.0, im: 0.0 });
    }
    if !t.contains('i') && !t.contains('j') {
        return Ok(Complex { re: t.parse().map_err(|_| ErrKind::Num)?, im: 0.0 });
    }
    let body = t.trim_end_matches(['i', 'j']);
    // 找中间的分隔符（跳过开头的正负号）
    let chars: Vec<char> = body.chars().collect();
    let mut split = None;
    for i in (1..chars.len()).rev() {
        if chars[i] == '+' || chars[i] == '-' {
            split = Some(i);
            break;
        }
    }
    let (re_s, im_s) = match split {
        Some(i) => (chars[..i].iter().collect::<String>(), chars[i..].iter().collect::<String>()),
        None => ("0".to_string(), body.to_string()),
    };
    let re = if re_s.is_empty() || re_s == "+" { 0.0 } else { re_s.parse().map_err(|_| ErrKind::Num)? };
    let im = match im_s.as_str() {
        "" | "+" => 1.0,
        "-" => -1.0,
        _ => im_s.parse().map_err(|_| ErrKind::Num)?,
    };
    Ok(Complex { re, im })
}

fn fmt_complex(c: Complex) -> String {
    let n = |x: f64| crate::model::format_number(x);
    if c.im == 0.0 {
        return n(c.re);
    }
    if c.re == 0.0 {
        return format!("{}i", n(c.im));
    }
    if c.im > 0.0 {
        format!("{}+{}i", n(c.re), n(c.im))
    } else {
        format!("{}{}i", n(c.re), n(c.im))
    }
}

fn complex_fn(name: &str, args: &[Arg]) -> Result<FValue, ErrKind> {
    let get = |i: usize| -> Result<Complex, ErrKind> { parse_complex(&text(args, i)?) };
    let out = match name {
        "COMPLEX" => {
            let re = num(args, 0)?;
            let im = num(args, 1)?;
            let suffix = args.get(2).map(|a| a.first().to_text().unwrap_or_default()).unwrap_or_default();
            let mut s = fmt_complex(Complex { re, im });
            if suffix == "j" {
                s = s.replace('i', "j");
            }
            return Ok(FValue::Text(s));
        }
        "IMREAL" => FValue::number(get(0)?.re),
        "IMAGINARY" => FValue::number(get(0)?.im),
        "IMABS" => FValue::number(get(0)?.re.hypot(get(0)?.im)),
        "IMARGUMENT" => {
            let c = get(0)?;
            if c.re == 0.0 && c.im == 0.0 {
                return Err(ErrKind::Div0);
            }
            FValue::number(c.im.atan2(c.re))
        }
        "IMCONJUGATE" => {
            let c = get(0)?;
            FValue::Text(fmt_complex(Complex { re: c.re, im: -c.im }))
        }
        "IMADD" | "IMSUB" | "IMMUL" | "IMDIV" => {
            let a = get(0)?;
            let b = get(1)?;
            let r = match name {
                "IMADD" => Complex { re: a.re + b.re, im: a.im + b.im },
                "IMSUB" => Complex { re: a.re - b.re, im: a.im - b.im },
                "IMMUL" => Complex {
                    re: a.re * b.re - a.im * b.im,
                    im: a.re * b.im + a.im * b.re,
                },
                _ => {
                    let d = b.re * b.re + b.im * b.im;
                    if d == 0.0 {
                        return Err(ErrKind::Num);
                    }
                    Complex {
                        re: (a.re * b.re + a.im * b.im) / d,
                        im: (a.im * b.re - a.re * b.im) / d,
                    }
                }
            };
            FValue::Text(fmt_complex(r))
        }
        "IMPOWER" => {
            let a = get(0)?;
            let n = num(args, 1)?;
            let r = a.re.hypot(a.im);
            let theta = a.im.atan2(a.re);
            let rn = r.powf(n);
            FValue::Text(fmt_complex(Complex {
                re: rn * (n * theta).cos(),
                im: rn * (n * theta).sin(),
            }))
        }
        "IMSQRT" => {
            let a = get(0)?;
            let r = a.re.hypot(a.im).sqrt();
            let theta = a.im.atan2(a.re) / 2.0;
            FValue::Text(fmt_complex(Complex { re: r * theta.cos(), im: r * theta.sin() }))
        }
        "IMEXP" => {
            let a = get(0)?;
            let e = a.re.exp();
            FValue::Text(fmt_complex(Complex { re: e * a.im.cos(), im: e * a.im.sin() }))
        }
        "IMLN" | "IMLOG10" | "IMLOG2" => {
            let a = get(0)?;
            let r = a.re.hypot(a.im);
            if r == 0.0 {
                return Err(ErrKind::Num);
            }
            let base = match name {
                "IMLOG10" => 10f64.ln(),
                "IMLOG2" => 2f64.ln(),
                _ => 1.0,
            };
            FValue::Text(fmt_complex(Complex {
                re: r.ln() / base,
                im: a.im.atan2(a.re) / base,
            }))
        }
        "IMSIN" | "IMCOS" | "IMTAN" => {
            let a = get(0)?;
            let (s_re, s_im) = (a.re.sin() * a.im.cosh(), a.re.cos() * a.im.sinh());
            let (c_re, c_im) = (a.re.cos() * a.im.cosh(), -a.re.sin() * a.im.sinh());
            let r = if name == "IMSIN" {
                Complex { re: s_re, im: s_im }
            } else if name == "IMCOS" {
                Complex { re: c_re, im: c_im }
            } else {
                let d = c_re * c_re + c_im * c_im;
                if d == 0.0 {
                    return Err(ErrKind::Num);
                }
                Complex {
                    re: (s_re * c_re + s_im * c_im) / d,
                    im: (s_im * c_re - s_re * c_im) / d,
                }
            };
            FValue::Text(fmt_complex(r))
        }
        "IMSUM" | "IMPRODUCT" => {
            let mut acc = if name == "IMSUM" {
                Complex { re: 0.0, im: 0.0 }
            } else {
                Complex { re: 1.0, im: 0.0 }
            };
            for a in args.iter() {
                for v in a.values() {
                    let c = parse_complex(&v.display())?;
                    acc = if name == "IMSUM" {
                        Complex { re: acc.re + c.re, im: acc.im + c.im }
                    } else {
                        Complex {
                            re: acc.re * c.re - acc.im * c.im,
                            im: acc.re * c.im + acc.im * c.re,
                        }
                    };
                }
            }
            FValue::Text(fmt_complex(acc))
        }
        _ => return Err(ErrKind::Name),
    };
    Ok(out)
}

// ---------------------------------------------------------------- 财务辅助

fn pmt_of(rate: f64, nper: f64, pv: f64, fv: f64, t: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + fv) / nper;
    }
    let p = (1.0 + rate).powf(nper);
    -(rate * (pv * p + fv)) / ((1.0 + rate * t) * (p - 1.0))
}

fn balance(rate: f64, nper: f64, pv: f64, pay: f64, periods: f64, t: f64) -> f64 {
    if rate == 0.0 {
        return pv + pay * periods;
    }
    let p = (1.0 + rate).powf(periods);
    pv * p + pay * (1.0 + rate * t) * (p - 1.0) / rate
}

trait RoundTo {
    fn round_to(self, digits: i32) -> f64;
}

impl RoundTo for f64 {
    fn round_to(self, digits: i32) -> f64 {
        let f = 10f64.powi(digits);
        (self * f).round() / f
    }
}

// ---------------------------------------------------------------- 数据库函数

fn database_fn(name: &str, args: &[Arg]) -> Result<FValue, ErrKind> {
    let db = args.first().ok_or(ErrKind::Value)?;
    let (rows, cols) = range_dims(db);
    let field_arg = arg(args, 1);
    let field_idx = match &field_arg {
        FValue::Number(n) => (*n as usize).saturating_sub(1),
        FValue::Text(s) => {
            let mut found = None;
            for c in 0..cols {
                if range_get(db, 0, c).display().eq_ignore_ascii_case(s) {
                    found = Some(c);
                    break;
                }
            }
            found.ok_or(ErrKind::Value)?
        }
        _ => return Err(ErrKind::Value),
    };
    if field_idx >= cols {
        return Err(ErrKind::Value);
    }
    let crit = args.get(2).ok_or(ErrKind::Value)?;
    let (crows, ccols) = range_dims(crit);

    // 条件表头对应的数据库列
    let mut crit_cols: Vec<(usize, usize)> = Vec::new(); // (crit_col, db_col)
    for cc in 0..ccols {
        let header = range_get(crit, 0, cc).display();
        for dc in 0..cols {
            if range_get(db, 0, dc).display().eq_ignore_ascii_case(&header) {
                crit_cols.push((cc, dc));
                break;
            }
        }
    }
    if crit_cols.is_empty() {
        return Err(ErrKind::Value);
    }

    let row_matches = |r: usize| -> bool {
        // 条件行之间是 OR，同一行内是 AND
        for cr in 1..crows {
            let mut all = true;
            for (cc, dc) in &crit_cols {
                let cond = range_get(crit, cr, *cc);
                if matches!(cond, FValue::Empty) {
                    continue;
                }
                let target = range_get(db, r, *dc);
                if !match_criteria(&target, &cond.display()) {
                    all = false;
                    break;
                }
            }
            if all {
                return true;
            }
        }
        false
    };

    let mut vals: Vec<FValue> = Vec::new();
    for r in 1..rows {
        if row_matches(r) {
            vals.push(range_get(db, r, field_idx));
        }
    }

    match name {
        "DCOUNT" => Ok(FValue::number(
            vals.iter().filter(|v| matches!(v, FValue::Number(_))).count() as f64,
        )),
        "DCOUNTA" => Ok(FValue::number(
            vals.iter().filter(|v| !matches!(v, FValue::Empty)).count() as f64,
        )),
        "DGET" => {
            if vals.len() != 1 {
                return Err(ErrKind::Num);
            }
            Ok(vals[0].clone())
        }
        _ => {
            let nums: Vec<f64> = vals
                .iter()
                .filter_map(|v| match v {
                    FValue::Number(n) => Some(*n),
                    _ => None,
                })
                .collect();
            let out = match name {
                "DSUM" => nums.iter().sum(),
                "DPRODUCT" => {
                    if nums.is_empty() {
                        0.0
                    } else {
                        nums.iter().product()
                    }
                }
                "DAVERAGE" => {
                    if nums.is_empty() {
                        return Err(ErrKind::Div0);
                    }
                    nums.iter().sum::<f64>() / nums.len() as f64
                }
                "DMAX" => nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                "DMIN" => nums.iter().cloned().fold(f64::INFINITY, f64::min),
                "DSTDEV" | "DSTDEVP" | "DVAR" | "DVARP" => {
                    let sample = name == "DSTDEV" || name == "DVAR";
                    let sd = super::functions::std_dev(&nums, sample)?;
                    if name == "DVAR" || name == "DVARP" {
                        sd * sd
                    } else {
                        sd
                    }
                }
                _ => return Err(ErrKind::Name),
            };
            Ok(FValue::number(if out.is_infinite() { 0.0 } else { out }))
        }
    }
}



fn binom_coef(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    let k = k.min(n - k);
    let mut acc = 1.0f64;
    for i in 0..k {
        acc = acc * (n - i) as f64 / (i + 1) as f64;
    }
    acc
}

/// ISO 8601 周数
fn iso_week(y: i32, m: u32, d: u32) -> u32 {
    let serial = match date_to_serial(y, m, d) {
        Some(s) => s as i64,
        None => return 0,
    };
    // Excel 序列号 1 = 1900-01-01 = 周日，所以 (serial-1)%7 就是 0=周日
    let weekday = (serial - 1).rem_euclid(7);
    let iso_wd = if weekday == 0 { 7 } else { weekday };
    let thursday = serial + (4 - iso_wd);
    let (ty, _, _) = match serial_to_date(thursday as f64) {
        Some(v) => v,
        None => return 0,
    };
    let jan1 = date_to_serial(ty, 1, 1).unwrap_or(0.0) as i64;
    ((thursday - jan1) / 7 + 1) as u32
}


/// 票息框架：返回 (上一票息日, 结算日, 下一票息日, 剩余票息次数)
/// 从到期日按 12/freq 个月往前推，票息日序列 d_j = maturity - j*step。
fn coupon_frame(settlement: f64, maturity: f64, freq: i64) -> Result<(f64, f64, f64, usize), ErrKind> {
    let step = (12 / freq.max(1)) as u32;
    if step == 0 {
        return Err(ErrKind::Num);
    }
    let (my, mm, md) = serial_to_date(maturity).ok_or(ErrKind::Num)?;
    let at = |j: u32| -> Option<f64> {
        let (y, m, d) = months_back(my, mm, md, j * step);
        date_to_serial(y, m, d)
    };
    // 票息日随时间递减，找到第一个不晚于结算日的下标 jp
    let mut jp: u32 = 0;
    loop {
        let d = at(jp).ok_or(ErrKind::Num)?;
        if d <= settlement {
            break;
        }
        jp += 1;
        if jp > 2000 {
            return Err(ErrKind::Num);
        }
    }
    if jp == 0 {
        return Err(ErrKind::Num);
    }
    let pcd = at(jp).ok_or(ErrKind::Num)?;
    let ncd = at(jp - 1).ok_or(ErrKind::Num)?;
    if ncd <= pcd {
        return Err(ErrKind::Num);
    }
    Ok((pcd, settlement, ncd, jp as usize))
}

/// 往前推 n 个月，日取不超过原日的最小值
fn months_back(y: i32, m: u32, d: u32, n: u32) -> (i32, u32, u32) {
    let total = (y as i64) * 12 + (m as i64 - 1) - n as i64;
    let ny = total.div_euclid(12) as i32;
    let nm = (total.rem_euclid(12) + 1) as u32;
    let maxd = days_in_month(ny, nm);
    (ny, nm, d.min(maxd))
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}


// ---------------------------------------------------------------- 不规则票息

struct Cashflows {
    /// (距今期数, 现金流)
    items: Vec<(f64, f64)>,
    /// 最后一笔本金的折现期数
    last_t: f64,
    /// 应计利息
    accrued: f64,
}

/// 生成现金流。odd_first=true 表示首个付息期不规则（issue → first_coupon），
/// 否则表示最后一个付息期不规则（last_interest → maturity）。
fn odd_cashflows(
    settlement: f64,
    maturity: f64,
    anchor: f64,
    rate: f64,
    freq: i64,
    odd_first: bool,
) -> Result<Cashflows, ErrKind> {
    let step = (12 / freq.max(1)) as u32;
    if step == 0 {
        return Err(ErrKind::Num);
    }
    let (ay, am, ad) = serial_to_date(anchor).ok_or(ErrKind::Num)?;
    let date_at = |j: i64| -> Option<f64> {
        let (y, m, d) = if j >= 0 {
            months_fwd(ay, am, ad, (j as u32) * step)
        } else {
            months_back(ay, am, ad, ((-j) as u32) * step)
        };
        date_to_serial(y, m, d)
    };

    // 常规期长度（按锚点后一个周期算）
    let normal = date_at(1).ok_or(ErrKind::Num)? - anchor;
    if normal <= 0.0 {
        return Err(ErrKind::Num);
    }

    let mut dates: Vec<f64> = Vec::new();
    if odd_first {
        // 付息日从 first_coupon 起算，一直排到到期
        let mut j = 0i64;
        loop {
            let d = date_at(j).ok_or(ErrKind::Num)?;
            if d > maturity + 0.5 {
                break;
            }
            if d >= maturity - 0.5 {
                dates.push(maturity);
                break;
            }
            dates.push(d);
            j += 1;
            if j > 2000 {
                return Err(ErrKind::Num);
            }
        }
    } else {
        // 常规票息排到 last_interest，再补一个到期日（末期不规则）
        let mut back = 0i64;
        while date_at(-back).map(|d| d > settlement).unwrap_or(false) {
            back += 1;
            if back > 2000 {
                return Err(ErrKind::Num);
            }
        }
        // 结算日之后的票息日：从 -back+1 到 0
        let mut j = -(back - 1);
        while j <= 0 {
            if let Some(d) = date_at(j) {
                if d > settlement + 0.5 {
                    dates.push(d);
                }
            }
            j += 1;
        }
        dates.push(maturity);
    }

    let mut items = Vec::new();
    let mut accrued = 0.0;
    let mut t_acc = 0.0;
    let mut prev = if odd_first { anchor } else { settlement };
    for (i, d) in dates.iter().enumerate() {
        let period_days = d - prev;
        // 与标准期等长（±3 天）时按整期计，避免天数取整造成的漂移
        let frac = if (period_days - normal).abs() <= 3.0 {
            1.0
        } else {
            period_days / normal
        };
        t_acc += frac;
        let coupon = 100.0 * rate / freq as f64 * frac;
        if odd_first && i == 0 && settlement < *d {
            accrued = coupon * ((settlement - anchor).max(0.0) / period_days.max(1.0));
        }
        items.push((t_acc, coupon));
        prev = *d;
    }

    let last_t = t_acc;
    Ok(Cashflows { items, last_t, accrued })
}

fn months_fwd(y: i32, m: u32, d: u32, n: u32) -> (i32, u32, u32) {
    let total = (y as i64) * 12 + (m as i64 - 1) + n as i64;
    let ny = total.div_euclid(12) as i32;
    let nm = (total.rem_euclid(12) + 1) as u32;
    let maxd = days_in_month(ny, nm);
    (ny, nm, d.min(maxd))
}

// ---------------------------------------------------------------- 贝塞尔

/// I_n / J_n：级数展开
fn bessel_ji(n: u32, x: f64, is_i: bool) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0f64;
    for k in 1..=n {
        term *= half / k as f64;
    }
    let mut sum = term;
    let mut k = 0u32;
    loop {
        k += 1;
        term *= half * half / ((k as f64) * (k + n) as f64);
        let add = if is_i || k % 2 == 0 { term } else { -term };
        sum += add;
        if term.abs() < 1e-17 * sum.abs().max(1e-300) || k > 500 {
            break;
        }
    }
    sum
}

/// K_n / Y_n：用积分表示，Simpson 求积
/// K_n(x) = ∫₀^∞ e^{-x·cosh t}·cosh(n t) dt
/// Y_n(x) = (1/π)∫₀^π sin(x·sinθ − nθ)dθ − (1/π)∫₀^∞ [e^{nt} + (−1)^n e^{-nt}]·e^{-x·sinh t} dt
fn bessel_ky(n: u32, x: f64, is_k: bool) -> Result<f64, ErrKind> {
    let nn = n as f64;
    if is_k {
        let f = |t: f64| -> f64 {
            let ch = t.cosh();
            (-x * ch).exp() * (nn * t).cosh()
        };
        // 找到积分上限：被积函数足够小
        let mut tmax = 2.0;
        while f(tmax) > 1e-15 && tmax < 40.0 {
            tmax += 0.5;
        }
        Ok(simpson(&f, 0.0, tmax, 4000))
    } else {
        let f1 = |theta: f64| -> f64 { (x * theta.sin() - nn * theta).sin() };
        let i1 = simpson(&f1, 0.0, std::f64::consts::PI, 4000);
        let sign = if n % 2 == 0 { 1.0 } else { -1.0 };
        let f2 = |t: f64| -> f64 {
            let e = (nn * t).exp() + sign * (-nn * t).exp();
            e * (-x * t.sinh()).exp()
        };
        let mut tmax = 2.0;
        while (f2(tmax) > 1e-15 || !f2(tmax).is_finite()) && tmax < 30.0 {
            tmax += 0.5;
        }
        let i2 = simpson(&f2, 0.0, tmax, 4000);
        Ok((i1 - i2) / std::f64::consts::PI)
    }
}

fn simpson<F: Fn(f64) -> f64>(f: &F, a: f64, b: f64, n: usize) -> f64 {
    let n = if n % 2 == 0 { n } else { n + 1 };
    let h = (b - a) / n as f64;
    let mut s = f(a) + f(b);
    for i in 1..n {
        let x = a + h * i as f64;
        s += if i % 2 == 0 { 2.0 * f(x) } else { 4.0 * f(x) };
    }
    s * h / 3.0
}


// ---------------------------------------------------------------- ETS

struct EtsFit {
    alpha: f64,
    beta: f64,
    gamma: f64,
    level: f64,
    trend: f64,
    season: Vec<f64>,
    mae: f64,
    rmse: f64,
    mase: f64,
    smape: f64,
    n: usize,
}

/// 用自相关找季节长度：先去掉线性趋势，再取第一个显著局部峰
fn detect_seasonality(vals: &[f64]) -> usize {
    let n = vals.len();
    if n < 8 {
        return 1;
    }
    // 最小二乘去趋势
    let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let mx = xs.iter().sum::<f64>() / n as f64;
    let my = vals.iter().sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..n {
        num += (xs[i] - mx) * (vals[i] - my);
        den += (xs[i] - mx).powi(2);
    }
    let slope = if den > 0.0 { num / den } else { 0.0 };
    let dev: Vec<f64> = (0..n).map(|i| vals[i] - (my + slope * (xs[i] - mx))).collect();
    let total: f64 = dev.iter().map(|d| d * d).sum();
    if total <= 1e-12 {
        return 1;
    }
    let max_lag = (n / 2).max(3);
    let mut r = vec![0.0; max_lag + 2];
    for lag in 2..=max_lag {
        let mut acc = 0.0;
        for i in 0..(n - lag) {
            acc += dev[i] * dev[i + lag];
        }
        r[lag] = acc / total;
    }
    for lag in 3..=max_lag {
        let left = r[lag - 1];
        let right = if lag + 1 <= max_lag { r[lag + 1] } else { -1.0 };
        if r[lag] >= left && r[lag] > right && r[lag] > 0.3 {
            return lag;
        }
    }
    1
}

fn ets_run(vals: &[f64], period: usize, alpha: f64, beta: f64, gamma: f64) -> (Vec<f64>, f64, f64, Vec<f64>) {
    let m = period.max(1);
    let n = vals.len();
    let init_level = if n >= m {
        vals[..m].iter().sum::<f64>() / m as f64
    } else {
        vals[0]
    };
    let init_trend = if n >= 2 * m {
        (vals[m..2 * m].iter().sum::<f64>() - vals[..m].iter().sum::<f64>()) / (m * m) as f64
    } else {
        0.0
    };
    let mut season = vec![0.0; m];
    if m > 1 {
        // 按整周期去均值初始化，避免把趋势混进季节项
        let mut acc = vec![0.0; m];
        let mut cnt = vec![0.0; m];
        let cycles = n / m;
        for c in 0..cycles.max(1) {
            let lo = c * m;
            let hi = ((c + 1) * m).min(n);
            if hi <= lo {
                break;
            }
            let cm = vals[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
            for i in lo..hi {
                acc[i % m] += vals[i] - cm;
                cnt[i % m] += 1.0;
            }
        }
        for i in 0..m {
            season[i] = if cnt[i] > 0.0 { acc[i] / cnt[i] } else { 0.0 };
        }
        // 季节项均值归零
        let sm = season.iter().sum::<f64>() / m as f64;
        for x in season.iter_mut() {
            *x -= sm;
        }
    }
    let mut level = init_level;
    let mut trend = init_trend;
    let mut fitted = Vec::with_capacity(n);
    for t in 0..n {
        let s = if m > 1 { season[t % m] } else { 0.0 };
        let pred = level + trend + s;
        fitted.push(pred);
        let err = vals[t] - pred;
        let new_level = alpha * (vals[t] - s) + (1.0 - alpha) * (level + trend);
        let new_trend = beta * (new_level - level) + (1.0 - beta) * trend;
        if m > 1 {
            season[t % m] = gamma * (vals[t] - new_level) + (1.0 - gamma) * s;
        }
        level = new_level;
        trend = new_trend;
    }
    (fitted, level, trend, season)
}

fn ets_fit(vals: &[f64], period: usize) -> Result<EtsFit, ErrKind> {
    let m = period.max(1);
    if vals.len() < m + 2 {
        return Err(ErrKind::Num);
    }
    // 粗网格搜索平滑系数，目标是 SSE 最小
    let mut best = (f64::INFINITY, 0.3, 0.1, 0.3);
    let grid: Vec<f64> = (1..=19).map(|i| i as f64 * 0.05).collect();
    for a in grid.iter().copied() {
        for b in grid.iter().copied() {
            for g in if m > 1 { grid.clone() } else { vec![0.0] } {
                let (fitted, _, _, _) = ets_run(vals, m, a, b, g);
                let sse: f64 = fitted.iter().zip(vals).map(|(f, v)| (v - f).powi(2)).sum();
                if sse < best.0 {
                    best = (sse, a, b, g);
                }
            }
        }
    }
    let (_, alpha, beta, gamma) = best;
    let (fitted, level, trend, season) = ets_run(vals, m, alpha, beta, gamma);

    // 误差指标：跳过前 m 期（季节初始化不准）
    let start = m.min(vals.len() - 1);
    let errs: Vec<f64> = (start..vals.len()).map(|i| vals[i] - fitted[i]).collect();
    let n = errs.len().max(1) as f64;
    let mae = errs.iter().map(|e| e.abs()).sum::<f64>() / n;
    let rmse = (errs.iter().map(|e| e * e).sum::<f64>() / n).sqrt();
    let smape = (start..vals.len())
        .map(|i| {
            let denom = (vals[i].abs() + fitted[i].abs()).max(1e-9);
            2.0 * (vals[i] - fitted[i]).abs() / denom
        })
        .sum::<f64>()
        / n;
    // MASE：相对于「上一步预测」的朴素误差
    let naive: f64 = (1..vals.len())
        .map(|i| (vals[i] - vals[i - 1]).abs())
        .sum::<f64>()
        / (vals.len() - 1).max(1) as f64;
    let mase = if naive > 0.0 { mae / naive } else { 0.0 };
    Ok(EtsFit { alpha, beta, gamma, level, trend, season, mae, rmse, mase, smape, n: vals.len() })
}

fn ets_predict(fit: &EtsFit, period: usize, h: usize) -> f64 {
    let m = period.max(1);
    let s = if m > 1 {
        // 观测索引是 0..n-1，第 h 步对应索引 n-1+h
        fit.season[(fit.n + h - 1) % m]
    } else {
        0.0
    };
    fit.level + (h as f64) * fit.trend + s
}

// ---------------------------------------------------------------- 回归辅助

fn pearson(x: &[f64], y: &[f64]) -> Result<f64, ErrKind> {
    let n = x.len().min(y.len());
    if n < 2 {
        return Err(ErrKind::Div0);
    }
    let mx = x[..n].iter().sum::<f64>() / n as f64;
    let my = y[..n].iter().sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut dx = 0.0;
    let mut dy = 0.0;
    for i in 0..n {
        num += (x[i] - mx) * (y[i] - my);
        dx += (x[i] - mx).powi(2);
        dy += (y[i] - my).powi(2);
    }
    if dx == 0.0 || dy == 0.0 {
        return Err(ErrKind::Div0);
    }
    Ok(num / (dx.sqrt() * dy.sqrt()))
}

fn linfit(x: &[f64], y: &[f64]) -> Result<(f64, f64), ErrKind> {
    let n = x.len().min(y.len());
    if n < 2 {
        return Err(ErrKind::Div0);
    }
    let mx = x[..n].iter().sum::<f64>() / n as f64;
    let my = y[..n].iter().sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..n {
        num += (x[i] - mx) * (y[i] - my);
        den += (x[i] - mx).powi(2);
    }
    if den == 0.0 {
        return Err(ErrKind::Div0);
    }
    let m = num / den;
    Ok((m, my - m * mx))
}

// ---------------------------------------------------------------- 单位换算

fn convert_unit(v: f64, from: &str, to: &str) -> Result<FValue, ErrKind> {
    let f = from.trim();
    let t = to.trim();
    if f == t {
        return Ok(FValue::number(v));
    }
    let temp = |u: &str| -> Option<char> {
        match u {
            "C" | "cel" => Some('C'),
            "F" | "fah" => Some('F'),
            "K" | "kel" => Some('K'),
            _ => None,
        }
    };
    if let (Some(a), Some(b)) = (temp(f), temp(t)) {
        let celsius = match a {
            'C' => v,
            'F' => (v - 32.0) * 5.0 / 9.0,
            _ => v - 273.15,
        };
        return Ok(FValue::number(match b {
            'C' => celsius,
            'F' => celsius * 9.0 / 5.0 + 32.0,
            _ => celsius + 273.15,
        }));
    }
    let ratio = |u: &str| -> Option<f64> {
        Some(match u {
            "m" => 1.0,
            "km" => 1000.0,
            "cm" => 0.01,
            "mm" => 0.001,
            "mi" => 1609.344,
            "yd" => 0.9144,
            "ft" => 0.3048,
            "in" => 0.0254,
            "Nmi" => 1852.0,
            "g" => 0.001,
            "kg" => 1.0,
            "lbm" => 0.45359237,
            "ozm" => 0.028349523125,
            "ton" => 907.18474,
            "sec" | "s" => 1.0,
            "min" => 60.0,
            "hr" => 3600.0,
            "day" => 86400.0,
            "yr" => 31_557_600.0,
            "l" | "L" => 1.0,
            "ml" => 0.001,
            "gal" => 3.785411784,
            "qt" => 0.946352946,
            "pt" => 0.473176473,
            "cup" => 0.2365882365,
            "tsp" => 0.00492892159,
            "tbs" => 0.0147867648,
            "m2" => 1.0,
            "km2" => 1_000_000.0,
            "ha" => 10_000.0,
            "ft2" => 0.09290304,
            "in2" => 0.00064516,
            _ => return None,
        })
    };
    match (ratio(f), ratio(t)) {
        (Some(a), Some(b)) => Ok(FValue::number(v * a / b)),
        _ => Err(ErrKind::Na),
    }
}

// ---------------------------------------------------------------- 罗马数字

fn to_roman(mut n: usize) -> String {
    const TABLE: &[(usize, &str)] = &[
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
        (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
    ];
    let mut out = String::new();
    for (v, s) in TABLE {
        while n >= *v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

fn from_roman(s: &str) -> Option<i64> {
    let val = |c: char| -> Option<i64> {
        Some(match c.to_ascii_uppercase() {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => return None,
        })
    };
    let chars: Vec<char> = s.chars().collect();
    let mut total = 0;
    let mut prev = 0;
    for c in chars.iter().rev() {
        let v = val(*c)?;
        if v < prev {
            total -= v;
        } else {
            total += v;
            prev = v;
        }
    }
    Some(total)
}
