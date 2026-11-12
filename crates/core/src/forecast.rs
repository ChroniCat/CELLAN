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
