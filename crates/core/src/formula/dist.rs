//! 特殊函数与分布数值实现（不依赖第三方库）

/// 互补误差函数（Numerical Recipes erfcc，相对误差 < 1.2e-7）
pub fn erfc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    // 小 |x| 用级数，精度比 NR 近似高得多
    if x.abs() < 2.0 {
        let mut term = x;
        let mut sum = x;
        let x2 = x * x;
        let mut n = 0u32;
        while term.abs() > 1e-18 && n < 200 {
            n += 1;
            term *= -x2 / n as f64;
            sum += term / (2 * n + 1) as f64;
        }
        return 1.0 - 2.0 / std::f64::consts::PI.sqrt() * sum;
    }
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let ans = t
        * (-z * z - 1.26551223
            + t * (1.00002368
                + t * (0.37409196
                    + t * (0.09678418
                        + t * (-0.18628806
                            + t * (0.27886807
                                + t * (-1.13520398
                                    + t * (1.48851587
                                        + t * (-0.82215223 + t * 0.17087277)))))))))
        .exp();
    if x >= 0.0 {
        ans
    } else {
        2.0 - ans
    }
}

pub fn erf(x: f64) -> f64 {
    1.0 - erfc(x)
}

/// 标准正态密度
pub fn norm_pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / (2.0 * std::f64::consts::PI).sqrt()
}

/// 标准正态分布函数
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

/// 标准正态分位数（Acklam 算法，精度 1.15e-9）
pub fn norm_inv(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    let plow = 0.02425;
    if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - plow {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

/// ln Γ(x)（Lanczos）
pub fn gamma_ln(x: f64) -> f64 {
    const COF: [f64; 6] = [
        76.18009172947146,
        -86.50532032941677,
        24.01409824083091,
        -1.231739572450155,
        0.1208650973866179e-2,
        -0.5395239384953e-5,
    ];
    let mut y = x;
    let tmp = x + 5.5;
    let tmp = tmp - (x + 0.5) * tmp.ln();
    let mut ser = 1.000000000190015;
    for c in COF {
        y += 1.0;
        ser += c / y;
    }
    -tmp + (2.5066282746310005 * ser / x).ln()
}

/// 不完全伽马 P(a,x)（级数展开）
fn gamma_p_series(a: f64, x: f64) -> f64 {
    let mut ap = a;
    let mut sum = 1.0 / a;
    let mut del = sum;
    for _ in 0..500 {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * 1e-15 {
            break;
        }
    }
    sum * (-x + a * x.ln() - gamma_ln(a)).exp()
}

/// 不完全伽马 Q(a,x)（连分式）
fn gamma_q_cf(a: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / TINY;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..500 {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + an / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    (-x + a * x.ln() - gamma_ln(a)).exp() * h
}

pub fn gamma_p(a: f64, x: f64) -> f64 {
    if x < 0.0 || a <= 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        gamma_p_series(a, x)
    } else {
        1.0 - gamma_q_cf(a, x)
    }
}

pub fn gamma_q(a: f64, x: f64) -> f64 {
    1.0 - gamma_p(a, x)
}

/// 不完全贝塔 I_x(a,b)
fn betacf(a: f64, b: f64, x: f64) -> f64 {
    const MAXIT: usize = 300;
    const EPS: f64 = 1e-15;
    const FPMIN: f64 = 1e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAXIT {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

pub fn ibeta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bt = (gamma_ln(a + b) - gamma_ln(a) - gamma_ln(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * betacf(a, b, x) / a
    } else {
        1.0 - bt * betacf(b, a, 1.0 - x) / b
    }
}

/// 单调函数求逆（二分 + 迭代，足够稳）
pub fn invert<F: Fn(f64) -> f64>(f: F, target: f64, lo: f64, hi: f64) -> f64 {
    let (mut a, mut b) = (lo, hi);
    let mut fa = f(a) - target;
    let fb = f(b) - target;
    if fa == 0.0 {
        return a;
    }
    if fb == 0.0 {
        return b;
    }
    if fa * fb > 0.0 {
        // 端点外扩
        for _ in 0..60 {
            if fa * fb < 0.0 {
                break;
            }
            a -= (b - a);
            fa = f(a) - target;
        }
    }
    for _ in 0..300 {
        let m = 0.5 * (a + b);
        let fm = f(m) - target;
        if fm.abs() < 1e-13 || (b - a).abs() < 1e-12 {
            return m;
        }
        if fa * fm <= 0.0 {
            b = m;
        } else {
            a = m;
            fa = fm;
        }
    }
    0.5 * (a + b)
}

/// 学生 t 分布 CDF（x 可以是任意实数，df>0）
pub fn t_cdf(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    let p = 0.5 * ibeta(df / 2.0, 0.5, x);
    if t > 0.0 {
        1.0 - p
    } else {
        p
    }
}

pub fn chisq_cdf(x: f64, df: f64) -> f64 {
    if x <= 0.0 {
        0.0
    } else {
        gamma_p(df / 2.0, x / 2.0)
    }
}

pub fn f_cdf(f: f64, d1: f64, d2: f64) -> f64 {
    if f <= 0.0 {
        0.0
    } else {
        let x = d1 * f / (d1 * f + d2);
        ibeta(d1 / 2.0, d2 / 2.0, x)
    }
}

pub fn beta_pdf(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 || x >= 1.0 {
        return 0.0;
    }
    ((gamma_ln(a + b) - gamma_ln(a) - gamma_ln(b)) + (a - 1.0) * x.ln() + (b - 1.0) * (1.0 - x).ln())
        .exp()
}

pub fn gamma_pdf(x: f64, a: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    ((-gamma_ln(a)) + (a - 1.0) * x.ln() - x).exp()
}

pub fn chisq_pdf(x: f64, df: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let k = df / 2.0;
    ((-x / 2.0) + (k - 1.0) * (x / 2.0).ln() - gamma_ln(k) - (2.0f64).ln() * k + (2.0f64).ln() * (k - 1.0)).exp()
        .max(0.0)
}

pub fn t_pdf(t: f64, df: f64) -> f64 {
    let c = (gamma_ln((df + 1.0) / 2.0) - gamma_ln(df / 2.0) - 0.5 * (df * std::f64::consts::PI).ln()).exp();
    c * (1.0 + t * t / df).powf(-(df + 1.0) / 2.0)
}

pub fn f_pdf(f: f64, d1: f64, d2: f64) -> f64 {
    if f <= 0.0 {
        return 0.0;
    }
    let a = d1 / 2.0;
    let b = d2 / 2.0;
    let c = (gamma_ln(a + b) - gamma_ln(a) - gamma_ln(b) + a * (d1 / d2).ln()).exp();
    c * f.powf(a - 1.0) * (1.0 + d1 * f / d2).powf(-(a + b))
}
