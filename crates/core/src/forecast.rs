//! 预测工作表：对一列时间序列做最小二乘线性回归，往后外推 N 期。
//!
//! Excel 365 的「预测工作表」用的是 ETS（指数平滑 + 季节），本机没有那套模型，
//! 这里给的是**线性回归**：y = a + b·x（x 用期序号 1..n），这是产品口径里写明的做法。
//! 同时对残差算标准差，给出 ±1.96σ 的近似 95% 区间，方便用户判断可靠性。

#[derive(Clone, Debug, Default)]
pub struct Forecast {
    /// 每期的拟合值（和输入等长）
    pub fitted: Vec<f64>,
    /// 往后外推的值
    pub future: Vec<f64>,
    pub a: f64,
    pub b: f64,
    /// 残差标准差
    pub sigma: f64,
    /// 拟合优度 R²
    pub r2: f64,
}

/// `xs`：横轴（一般是 1..n），`ys`：观测值。`periods`：往后推几期。
pub fn linear(xs: &[f64], ys: &[f64], periods: usize) -> Option<Forecast> {
    let n = xs.len().min(ys.len());
    if n < 2 {
        return None;
    }
    let (xs, ys) = (&xs[..n], &ys[..n]);
    let mx = xs.iter().sum::<f64>() / n as f64;
    let my = ys.iter().sum::<f64>() / n as f64;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for i in 0..n {
        sxx += (xs[i] - mx) * (xs[i] - mx);
        sxy += (xs[i] - mx) * (ys[i] - my);
    }
    if sxx.abs() < f64::EPSILON {
        return None;
    }
    let b = sxy / sxx;
    let a = my - b * mx;
    let fitted: Vec<f64> = xs.iter().map(|x| a + b * x).collect();
    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    for i in 0..n {
        let e = ys[i] - fitted[i];
        ss_res += e * e;
        ss_tot += (ys[i] - my) * (ys[i] - my);
    }
    let sigma = if n > 2 {
        (ss_res / (n as f64 - 2.0)).sqrt()
    } else {
        0.0
    };
    let r2 = if ss_tot.abs() < f64::EPSILON {
        1.0
    } else {
        1.0 - ss_res / ss_tot
    };
    let last_x = xs[n - 1];
    let future: Vec<f64> = (1..=periods)
        .map(|k| a + b * (last_x + k as f64))
        .collect();
    Some(Forecast {
        fitted,
        future,
        a,
        b,
        sigma,
        r2,
    })
}

/// Holt-Winters 加法 ETS（指数平滑 + 趋势 + 自动/指定季节周期 + 置信区间上下界）
#[derive(Clone, Debug, Default)]
pub struct EtsForecast {
    pub fitted: Vec<f64>,
    pub future: Vec<f64>,
    pub lower: Vec<f64>,
    pub upper: Vec<f64>,
    pub alpha: f64,
    pub beta: f64,
    pub gamma: f64,
    /// 检测或指定的季节周期（1 = 无季节性，走 Holt 双指数平滑）
    pub season_len: usize,
    pub sigma: f64,
    pub rmse: f64,
    pub mae: f64,
    pub r2: f64,
}

/// 通过自相关自动探测季节周期（如 4 季度、7 天、12 个月）
pub fn detect_seasonality(ys: &[f64]) -> usize {
    let n = ys.len();
    if n < 8 {
        return 1;
    }
    let mean = ys.iter().sum::<f64>() / n as f64;
    // 去线性趋势后计算自相关
    let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let detrended: Vec<f64> = if let Some(lin) = linear(&xs, ys, 0) {
        ys.iter().zip(&lin.fitted).map(|(y, f)| y - f).collect()
    } else {
        ys.iter().map(|y| y - mean).collect()
    };
    let denom: f64 = detrended.iter().map(|v| v * v).sum();
    if denom <= 1e-12 {
        return 1;
    }
    let max_lag = (n / 2).min(24);
    let mut best_lag = 1usize;
    let mut best_acf = 0.35f64;
    for lag in 2..=max_lag {
        let mut num = 0.0;
        for i in 0..(n - lag) {
            num += detrended[i] * detrended[i + lag];
        }
        let acf = num / denom;
        if acf > best_acf {
            best_acf = acf;
            best_lag = lag;
        }
    }
    best_lag
}

fn eval_hw_additive(
    ys: &[f64],
    alpha: f64,
    beta: f64,
    gamma: f64,
    season: usize,
    periods: usize,
) -> (Vec<f64>, Vec<f64>, f64, f64) {
    let n = ys.len();
    let mut fitted = Vec::with_capacity(n);
    if season <= 1 || n < season * 2 {
        // Holt 双指数平滑 (level + trend)
        let mut level = ys[0];
        let mut trend = if n >= 2 { ys[1] - ys[0] } else { 0.0 };
        fitted.push(ys[0]);
        let mut sse = 0.0;
        let mut sae = 0.0;
        for &y in &ys[1..] {
            let pred = level + trend;
            fitted.push(pred);
            let err = y - pred;
            sse += err * err;
            sae += err.abs();
            let prev_level = level;
            level = alpha * y + (1.0 - alpha) * pred;
            trend = beta * (level - prev_level) + (1.0 - beta) * trend;
        }
        let future = (1..=periods).map(|h| level + trend * h as f64).collect();
        return (fitted, future, sse, sae);
    }
    // Holt-Winters 加法季节性初始化
    let m = season;
    let avg1 = ys[..m].iter().sum::<f64>() / m as f64;
    let avg2 = ys[m..2 * m].iter().sum::<f64>() / m as f64;
    let mut level = avg1;
    let mut trend = (avg2 - avg1) / m as f64;
    let mut seasonal: Vec<f64> = (0..m).map(|i| ys[i] - avg1).collect();
    let mut sse = 0.0;
    let mut sae = 0.0;
    for (t, &y) in ys.iter().enumerate() {
        let s_idx = t % m;
        let pred = level + trend + seasonal[s_idx];
        fitted.push(pred);
        let err = y - pred;
        if t >= m {
            sse += err * err;
            sae += err.abs();
        }
        let prev_level = level;
        level = alpha * (y - seasonal[s_idx]) + (1.0 - alpha) * (level + trend);
        trend = beta * (level - prev_level) + (1.0 - beta) * trend;
        seasonal[s_idx] = gamma * (y - level) + (1.0 - gamma) * seasonal[s_idx];
    }
    let future = (1..=periods)
        .map(|h| level + trend * h as f64 + seasonal[(n + h - 1) % m])
        .collect();
    (fitted, future, sse, sae)
}

/// 执行 Holt-Winters / ETS 时间序列预测（自动选择最优平滑参数与季节周期，并给出置信上下界）。
pub fn ets(ys: &[f64], periods: usize, season_len: Option<usize>, confidence: f64) -> Option<EtsForecast> {
    let n = ys.len();
    if n < 2 {
        return None;
    }
    let season = match season_len {
        Some(0) => detect_seasonality(ys),
        Some(s) if s <= 1 || n < s * 2 => 1,
        Some(s) => s,
        None => detect_seasonality(ys),
    };
    let mut best = (f64::INFINITY, 0.3, 0.1, 0.1);
    let alphas = [0.1, 0.25, 0.4, 0.6, 0.8];
    let betas = [0.0, 0.1, 0.25, 0.5];
    let gammas: &[f64] = if season > 1 { &[0.05, 0.2, 0.4, 0.6] } else { &[0.0] };
    for &a in &alphas {
        for &b in &betas {
            for &g in gammas {
                let (_, _, sse, _) = eval_hw_additive(ys, a, b, g, season, 0);
                if sse.is_finite() && sse < best.0 {
                    best = (sse, a, b, g);
                }
            }
        }
    }
    let (_, alpha, beta, gamma) = best;
    let (fitted, future, sse, sae) = eval_hw_additive(ys, alpha, beta, gamma, season, periods);
    let eval_n = if season > 1 && n > season { n - season } else { n.saturating_sub(1).max(1) };
    let rmse = (sse / eval_n as f64).sqrt();
    let mae = sae / eval_n as f64;
    let dof = eval_n.saturating_sub(2).max(1) as f64;
    let sigma = (sse / dof).sqrt();
    let mean = ys.iter().sum::<f64>() / n as f64;
    let ss_tot: f64 = ys.iter().map(|y| (y - mean) * (y - mean)).sum();
    let ss_all: f64 = ys.iter().zip(&fitted).map(|(y, f)| (y - f) * (y - f)).sum();
    let r2 = if ss_tot.abs() < f64::EPSILON { 1.0 } else { (1.0 - ss_all / ss_tot).clamp(-1.0, 1.0) };
    let z = if confidence >= 0.99 {
        2.576
    } else if confidence >= 0.95 {
        1.96
    } else if confidence >= 0.90 {
        1.645
    } else {
        1.282
    };
    let mut lower = Vec::with_capacity(periods);
    let mut upper = Vec::with_capacity(periods);
    for (i, &val) in future.iter().enumerate() {
        let h = (i + 1) as f64;
        // 多步预测误差随步长扩展
        let half = z * sigma * (1.0 + (h - 1.0) * (alpha * alpha + beta * beta)).sqrt();
        lower.push(val - half);
        upper.push(val + half);
    }
    Some(EtsForecast {
        fitted,
        future,
        lower,
        upper,
        alpha,
        beta,
        gamma,
        season_len: season,
        sigma,
        rmse,
        mae,
        r2,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_a_straight_line() {
        let ys = [10.0, 20.0, 30.0, 40.0];
        let xs = [1.0, 2.0, 3.0, 4.0];
        let f = linear(&xs, &ys, 2).unwrap();
        assert!((f.b - 10.0).abs() < 1e-9);
        assert!((f.a - 0.0).abs() < 1e-9);
        assert_eq!(f.future.len(), 2);
        assert!((f.future[0] - 50.0).abs() < 1e-9);
        assert!((f.future[1] - 60.0).abs() < 1e-9);
        assert!((f.r2 - 1.0).abs() < 1e-9, "直线 R²=1");
        assert!(f.sigma < 1e-9);
    }

    #[test]
    fn handles_noise_and_reports_r2() {
        let xs = [1.0, 2.0, 3.0, 4.0];
        let ys = [2.0, 1.0, 4.0, 3.0];
        let f = linear(&xs, &ys, 1).unwrap();
        assert!(f.r2 < 1.0 && f.r2 > 0.0, "r2={}", f.r2);
        assert!(f.sigma > 0.0);
        assert_eq!(f.fitted.len(), 4);
    }

    #[test]
    fn needs_two_points() {
        assert!(linear(&[1.0], &[2.0], 3).is_none());
        assert!(linear(&[1.0, 1.0], &[2.0, 3.0], 3).is_none(), "x 全等无解");
    }

    #[test]
    fn forecast_range_is_linear_in_periods() {
        let f = linear(&[1.0, 2.0, 3.0], &[5.0, 7.0, 9.0], 3).unwrap();
        // y = 3 + 2x → 第 4/5/6 期 = 11/13/15
        let want = [11.0, 13.0, 15.0];
        for (got, w) in f.future.iter().zip(want) {
            assert!((got - w).abs() < 1e-9, "{got} != {w}");
        }
    }
}
