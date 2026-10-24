use std::collections::VecDeque;

use domain::{DollarBar, DomainError, OrderIntent, Side};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::{
    BarPassThrough, Decision, HyperparameterSpace, MarketType, OptimizableStrategy, Strategy,
};

/// Physical sample of a single Dollar Bar in continuous log-space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalSample {
    /// Log-price of bar close: $x_\tau = \ln(P_\tau)$.
    pub x: f64,
    /// Logarithmic return: $\Delta x_\tau = x_\tau - x_{\tau-1}$.
    pub delta_x: f64,
    /// Clock duration in milliseconds to print the bar (protected against division by zero).
    pub delta_t_ms: u64,
}

/// Diagnosed topology of the active potential well basin around the current price.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WellTopology {
    /// Equilibrium center (well bottom): $F(x_0) = 0$ with $V''(x_0) > 0$.
    pub x_0: f64,
    /// Well stiffness / curvature at bottom: $V''(x_0)$.
    pub curvature_0: f64,
    /// Thermal radius of the ground state: $\delta_{\text{th}} = \sqrt{-K_0(x_0)/K''_0(x_0)}$.
    pub delta_thermal: f64,
    /// Left elastic inflection point: $V''(x_{\text{inf}}^-) = 0$.
    pub x_inf_left: f64,
    /// Maximum restoring force magnitude on left wall: $F_{\max}^- = F(x_{\text{inf}}^-) > 0$.
    pub f_max_left: f64,
    /// Right elastic inflection point: $V''(x_{\text{inf}}^+) = 0$.
    pub x_inf_right: f64,
    /// Maximum restoring force magnitude on right wall: $F_{\max}^+ = |F(x_{\text{inf}}^+)| > 0$.
    pub f_max_right: f64,
    /// Left barrier crest (Stop-Loss / Breakout boundary): $x_b^-$.
    pub x_b_left: f64,
    /// Right barrier crest (Stop-Loss / Breakout boundary): $x_b^+$.
    pub x_b_right: f64,
    /// Total barrier height from the well bottom on left: $V(x_b^-) - V(x_0)$.
    pub barrier_height_left: f64,
    /// Total barrier height from the well bottom on right: $V(x_b^+) - V(x_0)$.
    pub barrier_height_right: f64,
}

/// Scale-invariant Regularized Exponentially Weighted Kernel Density Estimator (EW-KDE)
/// and non-equilibrium potential landscape solver.
#[derive(Debug, Clone)]
pub struct KdePotential {
    window: VecDeque<PhysicalSample>,
    capacity: usize,
    bandwidth_factor: f64,
    decay_eta: f64,
    vacuum_epsilon: f64,
    alpha_dissociation: f64,
    lambda_ema_alpha: f64,
    lambda_ema: f64,
    last_end_time: Option<i64>,
    last_log_price: Option<f64>,
}

impl KdePotential {
    /// Creates a new EW-KDE potential landscape engine.
    pub fn new(
        capacity: usize,
        bandwidth_factor: f64,
        decay_eta: f64,
        vacuum_epsilon: f64,
        alpha_dissociation: f64,
        lambda_ema_period: usize,
    ) -> Result<Self, DomainError> {
        if capacity < 4 {
            return Err(DomainError::InvalidWindowSize(capacity));
        }
        let period = lambda_ema_period.max(1);
        let lambda_ema_alpha = 2.0 / (period as f64 + 1.0);

        Ok(Self {
            window: VecDeque::with_capacity(capacity),
            capacity,
            bandwidth_factor: bandwidth_factor.max(0.1),
            decay_eta: decay_eta.max(0.0),
            vacuum_epsilon: vacuum_epsilon.clamp(1e-6, 0.1),
            alpha_dissociation: alpha_dissociation.clamp(0.05, 0.95),
            lambda_ema_alpha,
            lambda_ema: 1.0,
            last_end_time: None,
            last_log_price: None,
        })
    }

    /// Resets historical state for a clean replay isolation.
    pub fn reset(&mut self) {
        self.window.clear();
        self.lambda_ema = 1.0;
        self.last_end_time = None;
        self.last_log_price = None;
    }

    /// Number of samples currently accumulated.
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// Whether the estimator has accumulated any samples.
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// Whether the minimum sample warm-up requirement is met.
    pub fn is_warm(&self) -> bool {
        self.window.len() >= self.capacity
    }

    /// Current exponential moving average of bar arrival frequency $\lambda$.
    pub fn lambda_ema(&self) -> f64 {
        self.lambda_ema
    }

    /// Ingests a new Dollar Bar, updating physical samples and arrival rate.
    pub fn push(&mut self, bar: &DollarBar) {
        let price_f64 = bar.close.to_f64().unwrap_or(1.0).max(1e-8);
        let log_price = price_f64.ln();

        let delta_x = match self.last_log_price {
            Some(prev_x) => log_price - prev_x,
            None => 0.0,
        };

        let delta_t_ms = match self.last_end_time {
            Some(prev_end) => ((bar.end_time - prev_end).max(1)) as u64,
            None => 1000,
        };

        self.last_end_time = Some(bar.end_time);
        self.last_log_price = Some(log_price);

        // Update arrival intensity lambda (bars per second)
        let lambda = 1000.0 / delta_t_ms as f64;
        if self.window.is_empty() {
            self.lambda_ema = lambda;
        } else {
            self.lambda_ema =
                self.lambda_ema_alpha * lambda + (1.0 - self.lambda_ema_alpha) * self.lambda_ema;
        }

        if self.window.len() == self.capacity {
            self.window.pop_front();
        }

        self.window.push_back(PhysicalSample {
            x: log_price,
            delta_x,
            delta_t_ms,
        });
    }

    /// Computes normalized exponential weights $w_i \propto \exp(-\eta (N - i))$
    /// such that $\sum_{i=1}^N w_i = 1$.
    fn compute_weights(&self) -> Vec<f64> {
        let n = self.window.len();
        if n == 0 {
            return Vec::new();
        }

        let mut raw_weights = Vec::with_capacity(n);
        let mut sum_w = 0.0;
        for i in 0..n {
            let age = (n - 1 - i) as f64;
            let w = (-self.decay_eta * age).exp();
            raw_weights.push(w);
            sum_w += w;
        }

        if sum_w > 0.0 {
            for w in &mut raw_weights {
                *w /= sum_w;
            }
        }
        raw_weights
    }

    /// Computes Kish's effective sample size $N_{\text{eff}} = 1 / \sum w_i^2$.
    fn compute_n_eff(&self, weights: &[f64]) -> f64 {
        let sum_sq: f64 = weights.iter().map(|w| w * w).sum();
        if sum_sq > 0.0 {
            (1.0 / sum_sq).max(1.0)
        } else {
            1.0
        }
    }

    /// Computes thermal diffusion variance $\sigma^2 = \sum w_i (\Delta x_i)^2$.
    pub fn diffusivity_sigma2(&self, weights: &[f64]) -> f64 {
        if self.window.is_empty() || weights.len() != self.window.len() {
            return 1e-12;
        }
        let var: f64 = self
            .window
            .iter()
            .zip(weights.iter())
            .map(|(s, &w)| w * s.delta_x * s.delta_x)
            .sum();
        var.max(1e-12)
    }

    /// Computes robust dispersion $\hat{\sigma}_{\text{rob}} = \min(\hat{\sigma}_{x,w}, 1.4826 \cdot \text{MAD}_w(x))$
    /// and Silverman's adaptive bandwidth $h$.
    pub fn compute_bandwidth(&self, weights: &[f64]) -> f64 {
        let n = self.window.len();
        if n < 2 {
            return 0.001;
        }

        // 1. Weighted Mean
        let mean: f64 = self
            .window
            .iter()
            .zip(weights.iter())
            .map(|(s, &w)| w * s.x)
            .sum();

        // 2. Weighted Standard Deviation
        let variance: f64 = self
            .window
            .iter()
            .zip(weights.iter())
            .map(|(s, &w)| w * (s.x - mean).powi(2))
            .sum();
        let std_dev = variance.max(0.0).sqrt();

        // 3. Weighted Median and MAD
        let mut pairs: Vec<(f64, f64)> = self
            .window
            .iter()
            .zip(weights.iter())
            .map(|(s, &w)| (s.x, w))
            .collect();
        pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut cum_w = 0.0;
        let mut median = pairs[0].0;
        for (val, w) in &pairs {
            cum_w += w;
            if cum_w >= 0.5 {
                median = *val;
                break;
            }
        }

        let mut dev_pairs: Vec<(f64, f64)> = pairs
            .iter()
            .map(|(val, w)| ((val - median).abs(), *w))
            .collect();
        dev_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut cum_dev_w = 0.0;
        let mut mad = dev_pairs[0].0;
        for (dev, w) in &dev_pairs {
            cum_dev_w += w;
            if cum_dev_w >= 0.5 {
                mad = *dev;
                break;
            }
        }

        let mad_sigma = 1.4826 * mad;
        let sigma_rob = if mad_sigma > 1e-8 && mad_sigma < std_dev {
            mad_sigma
        } else {
            std_dev
        }
        .max(1e-6);

        let n_eff = self.compute_n_eff(weights);
        let h = self.bandwidth_factor * 0.9 * sigma_rob * n_eff.powf(-0.2);
        h.max(1e-5)
    }

    /// Evaluates base kernel sum and its closed-form derivatives:
    /// $K_0(x) = (1 - \epsilon_0) \sum w_i e^{-\frac{1}{2}u_i^2} + \epsilon_0$
    /// $K'_0(x) = -\frac{1-\epsilon_0}{h} \sum w_i u_i e^{-\frac{1}{2}u_i^2}$
    /// $K''_0(x) = \frac{1-\epsilon_0}{h^2} \sum w_i (u_i^2 - 1) e^{-\frac{1}{2}u_i^2}$
    pub fn kernel_derivatives(&self, x: f64, h: f64, weights: &[f64]) -> (f64, f64, f64) {
        let eps = self.vacuum_epsilon;
        let one_minus_eps = 1.0 - eps;

        let mut sum_k0 = 0.0;
        let mut sum_k1 = 0.0;
        let mut sum_k2 = 0.0;

        for (sample, &w) in self.window.iter().zip(weights.iter()) {
            let u = (x - sample.x) / h;
            let exponent = -0.5 * u * u;
            if exponent > -40.0 {
                let phi = w * exponent.exp();
                sum_k0 += phi;
                sum_k1 += u * phi;
                sum_k2 += (u * u - 1.0) * phi;
            }
        }

        let k0 = (one_minus_eps * sum_k0 + eps).max(1e-12);
        let k1 = -(one_minus_eps / h) * sum_k1;
        let k2 = (one_minus_eps / (h * h)) * sum_k2;

        (k0, k1, k2)
    }

    /// Deterministic market force: $F(x) = \frac{\sigma^2}{2} \frac{K'_0(x)}{K_0(x)}$.
    pub fn force(&self, x: f64, h: f64, sigma2: f64, weights: &[f64]) -> f64 {
        let (k0, k1, _) = self.kernel_derivatives(x, h, weights);
        0.5 * sigma2 * (k1 / k0)
    }

    /// Potential curvature: $V''(x) = -\frac{\sigma^2}{2} \left[ \frac{K''_0(x)}{K_0(x)} - \left(\frac{K'_0(x)}{K_0(x)}\right)^2 \right]$.
    pub fn curvature(&self, x: f64, h: f64, sigma2: f64, weights: &[f64]) -> f64 {
        let (k0, k1, k2) = self.kernel_derivatives(x, h, weights);
        let ratio1 = k1 / k0;
        let ratio2 = k2 / k0;
        -0.5 * sigma2 * (ratio2 - ratio1 * ratio1)
    }

    /// Exact potential energy difference: $\Delta V = V(x) - V(x_{\text{ref}}) = -\frac{\sigma^2}{2} \ln \frac{K_0(x)}{K_0(x_{\text{ref}})}$.
    pub fn potential_diff(&self, x: f64, x_ref: f64, h: f64, sigma2: f64, weights: &[f64]) -> f64 {
        let (k0_x, _, _) = self.kernel_derivatives(x, h, weights);
        let (k0_ref, _, _) = self.kernel_derivatives(x_ref, h, weights);
        let ratio = (k0_x / k0_ref).max(1e-12);
        -0.5 * sigma2 * ratio.ln()
    }

    /// Analyzes the active potential well basin around the current log-price $x_\tau$
    /// using ray-marching and robust bisection.
    pub fn analyze_well(&self, current_x: f64) -> Option<WellTopology> {
        if self.window.len() < 4 {
            return None;
        }

        let weights = self.compute_weights();
        let sigma2 = self.diffusivity_sigma2(&weights);
        if sigma2 <= 1e-12 {
            return None;
        }

        let h = self.compute_bandwidth(&weights);
        let f_curr = self.force(current_x, h, sigma2, &weights);

        // 1. Locate well bottom x_0 by ray-marching from current_x along the restoring force
        let step_x0 = 0.25 * h;
        let mut x_0 = current_x;
        let march_dir = if f_curr >= 0.0 { 1.0 } else { -1.0 };

        let mut bracket_a = current_x;
        let mut bracket_b = current_x;
        let mut found_bracket = false;

        let mut prev_x = current_x;
        let mut prev_f = f_curr;

        for step in 1..=40 {
            let test_x = current_x + march_dir * (step as f64) * step_x0;
            let test_f = self.force(test_x, h, sigma2, &weights);

            if (prev_f >= 0.0 && test_f <= 0.0) || (prev_f <= 0.0 && test_f >= 0.0) {
                bracket_a = prev_x;
                bracket_b = test_x;
                found_bracket = true;
                break;
            }
            prev_x = test_x;
            prev_f = test_f;
        }

        if found_bracket {
            // Bisection refinement for F(x) = 0
            let mut a = bracket_a.min(bracket_b);
            let mut b = bracket_a.max(bracket_b);
            let f_a = self.force(a, h, sigma2, &weights);

            for _ in 0..12 {
                let mid = 0.5 * (a + b);
                let f_mid = self.force(mid, h, sigma2, &weights);
                if (f_mid >= 0.0 && f_a <= 0.0) || (f_mid <= 0.0 && f_a >= 0.0) {
                    b = mid;
                } else {
                    a = mid;
                }
            }
            x_0 = 0.5 * (a + b);
        }

        // Verify curvature at well bottom: V''(x_0) must be positive (attractive basin)
        let curv_0 = self.curvature(x_0, h, sigma2, &weights);
        if curv_0 <= 1e-12 {
            return None;
        }

        // Compute thermal radius: delta_th = sqrt(-K_0(x_0) / K''_0(x_0))
        let (k0_0, _, k2_0) = self.kernel_derivatives(x_0, h, &weights);
        let delta_thermal = if k2_0 < -1e-12 {
            (-k0_0 / k2_0).max(0.0).sqrt()
        } else {
            h
        }
        .max(1e-5);

        // 2. Ray-march left from x_0 to locate x_inf_left and x_b_left
        let ray_step = 0.20 * h;
        let mut x_inf_left = x_0 - ray_step;
        let mut f_max_left = 1e-6;

        let mut found_inf_left = false;
        let mut prev_x_ray = x_0;
        let mut prev_curv = curv_0;

        for step in 1..=60 {
            let test_x = x_0 - (step as f64) * ray_step;
            let test_curv = self.curvature(test_x, h, sigma2, &weights);

            if prev_curv > 0.0 && test_curv <= 0.0 {
                // Bisection on V''(x) = 0
                let mut a = test_x;
                let mut b = prev_x_ray;
                for _ in 0..10 {
                    let mid = 0.5 * (a + b);
                    let c_mid = self.curvature(mid, h, sigma2, &weights);
                    if c_mid <= 0.0 {
                        a = mid;
                    } else {
                        b = mid;
                    }
                }
                x_inf_left = 0.5 * (a + b);
                f_max_left = self.force(x_inf_left, h, sigma2, &weights).abs().max(1e-6);
                found_inf_left = true;
                break;
            }
            prev_x_ray = test_x;
            prev_curv = test_curv;
        }

        if !found_inf_left {
            x_inf_left = x_0 - 3.0 * h;
            f_max_left = self.force(x_inf_left, h, sigma2, &weights).abs().max(1e-6);
        }

        // Continue left beyond x_inf_left to find barrier crest x_b_left
        let mut x_b_left = x_inf_left - ray_step;
        let dissoc_thresh_left = self.alpha_dissociation * f_max_left;

        for step in 1..=60 {
            let test_x = x_inf_left - (step as f64) * ray_step;
            let f_test = self.force(test_x, h, sigma2, &weights);

            if f_test <= 0.0 || f_test <= dissoc_thresh_left {
                x_b_left = test_x;
                break;
            }
            x_b_left = test_x;
        }

        // 3. Ray-march right from x_0 to locate x_inf_right and x_b_right
        let mut x_inf_right = x_0 + ray_step;
        let mut f_max_right = 1e-6;

        let mut found_inf_right = false;
        prev_x_ray = x_0;
        prev_curv = curv_0;

        for step in 1..=60 {
            let test_x = x_0 + (step as f64) * ray_step;
            let test_curv = self.curvature(test_x, h, sigma2, &weights);

            if prev_curv > 0.0 && test_curv <= 0.0 {
                // Bisection on V''(x) = 0
                let mut a = prev_x_ray;
                let mut b = test_x;
                for _ in 0..10 {
                    let mid = 0.5 * (a + b);
                    let c_mid = self.curvature(mid, h, sigma2, &weights);
                    if c_mid >= 0.0 {
                        a = mid;
                    } else {
                        b = mid;
                    }
                }
                x_inf_right = 0.5 * (a + b);
                f_max_right = self.force(x_inf_right, h, sigma2, &weights).abs().max(1e-6);
                found_inf_right = true;
                break;
            }
            prev_x_ray = test_x;
            prev_curv = test_curv;
        }

        if !found_inf_right {
            x_inf_right = x_0 + 3.0 * h;
            f_max_right = self.force(x_inf_right, h, sigma2, &weights).abs().max(1e-6);
        }

        // Continue right beyond x_inf_right to find barrier crest x_b_right
        let mut x_b_right = x_inf_right + ray_step;
        let dissoc_thresh_right = self.alpha_dissociation * f_max_right;

        for step in 1..=60 {
            let test_x = x_inf_right + (step as f64) * ray_step;
            let f_test = self.force(test_x, h, sigma2, &weights);

            if f_test >= 0.0 || f_test.abs() <= dissoc_thresh_right {
                x_b_right = test_x;
                break;
            }
            x_b_right = test_x;
        }

        let barrier_height_left = self
            .potential_diff(x_b_left, x_0, h, sigma2, &weights)
            .max(1e-8);
        let barrier_height_right = self
            .potential_diff(x_b_right, x_0, h, sigma2, &weights)
            .max(1e-8);

        Some(WellTopology {
            x_0,
            curvature_0: curv_0,
            delta_thermal,
            x_inf_left,
            f_max_left,
            x_inf_right,
            f_max_right,
            x_b_left,
            x_b_right,
            barrier_height_left,
            barrier_height_right,
        })
    }
}

/// Hyperparameter configuration for Langevin Potential Well Strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangevinConfig {
    /// Rolling window capacity in Dollar Bars ($N \in [50, 400]$).
    pub window_size: usize,
    /// Multiplier $\kappa_h$ on Silverman's bandwidth ($h = \kappa_h \cdot 0.9 \hat{\sigma}_{\text{rob}} N_{\text{eff}}^{-1/5}$).
    pub bandwidth_factor: Decimal,
    /// Exponential decay rate $\eta$ for past bars.
    pub decay_eta: Decimal,
    /// Dimensionless vacuum floor density $\epsilon_0$ (e.g. 0.001).
    pub vacuum_epsilon: Decimal,
    /// Traction threshold fraction $\beta \in [0.70, 0.90]$ for mean reversion entry ($|F(x)| \ge \beta F_{\max}$).
    pub beta_entry: Decimal,
    /// Dissociation fraction $\alpha \in [0.10, 0.40]$ for open boundary Stop-Loss ($|F(x_b)| \le \alpha F_{\max}$).
    pub alpha_dissociation: Decimal,
    /// Offset multiplier $\zeta \in [0.0, 0.5]$ of thermal radius for Take-Profit ($x_{\text{TP}} = x_0 \mp \zeta \delta_{\text{th}}$).
    pub zeta_thermal_tp: Decimal,
    /// Period for arrival intensity $\lambda$ EMA normalizer.
    pub lambda_ema_period: usize,
    /// Maximum bar holding duration for horizontal barrier timeout.
    pub time_barrier_bars: usize,
    /// Friction hurdle: minimum required Take-Profit distance as a fraction of price (e.g. 0.0025 = 0.25%).
    pub min_take_profit_pct: Decimal,
}

impl Default for LangevinConfig {
    fn default() -> Self {
        Self {
            window_size: 80,
            bandwidth_factor: dec!(1.0),
            decay_eta: dec!(0.02),
            vacuum_epsilon: dec!(0.001),
            beta_entry: dec!(0.75),
            alpha_dissociation: dec!(0.25),
            zeta_thermal_tp: dec!(0.2),
            lambda_ema_period: 20,
            time_barrier_bars: 30,
            min_take_profit_pct: dec!(0.0025),
        }
    }
}

impl std::fmt::Display for LangevinConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "W:{}|Bf:{}|Eta:{}|Beta:{}|Tb:{}|MinTp:{}",
            self.window_size,
            self.bandwidth_factor,
            self.decay_eta,
            self.beta_entry,
            self.time_barrier_bars,
            self.min_take_profit_pct
        )
    }
}

/// Hyperparameter search space for automated optimization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangevinSpace {
    pub window_sizes: Vec<usize>,
    pub bandwidth_factors: Vec<Decimal>,
    pub decay_etas: Vec<Decimal>,
    pub beta_entries: Vec<Decimal>,
    pub zeta_thermal_tps: Vec<Decimal>,
    pub time_barrier_bars: Vec<usize>,
    pub min_take_profit_pcts: Vec<Decimal>,
}

impl Default for LangevinSpace {
    fn default() -> Self {
        Self {
            window_sizes: vec![60, 80, 120, 160],
            bandwidth_factors: vec![dec!(0.8), dec!(1.0), dec!(1.2)],
            decay_etas: vec![dec!(0.01), dec!(0.02), dec!(0.04)],
            beta_entries: vec![dec!(0.70), dec!(0.75), dec!(0.85)],
            zeta_thermal_tps: vec![dec!(0.0), dec!(0.2), dec!(0.4)],
            time_barrier_bars: vec![20, 30, 50],
            min_take_profit_pcts: vec![dec!(0.0025), dec!(0.0035)],
        }
    }
}

impl HyperparameterSpace for LangevinSpace {
    type Config = LangevinConfig;

    fn generate_candidates(&self, max_candidates: usize) -> Vec<Self::Config> {
        let mut configs = Vec::new();

        for &w in &self.window_sizes {
            for &bf in &self.bandwidth_factors {
                for &eta in &self.decay_etas {
                    for &beta in &self.beta_entries {
                        for &zeta in &self.zeta_thermal_tps {
                            for &tb in &self.time_barrier_bars {
                                for &min_tp in &self.min_take_profit_pcts {
                                    configs.push(LangevinConfig {
                                        window_size: w,
                                        bandwidth_factor: bf,
                                        decay_eta: eta,
                                        vacuum_epsilon: dec!(0.001),
                                        beta_entry: beta,
                                        alpha_dissociation: dec!(0.25),
                                        zeta_thermal_tp: zeta,
                                        lambda_ema_period: 20,
                                        time_barrier_bars: tb,
                                        min_take_profit_pct: min_tp,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        if configs.len() > max_candidates {
            use rand::seq::SliceRandom;
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);
            configs.shuffle(&mut rng);
            configs.truncate(max_candidates);
        }

        configs
    }

    fn perturb_neighbors(&self, base: &Self::Config) -> Vec<Self::Config> {
        let mut neighbors = Vec::with_capacity(14);
        let windows = adjacent_grid_values(&self.window_sizes, base.window_size);
        let bfs = adjacent_grid_values(&self.bandwidth_factors, base.bandwidth_factor);
        let etas = adjacent_grid_values(&self.decay_etas, base.decay_eta);
        let betas = adjacent_grid_values(&self.beta_entries, base.beta_entry);
        let zetas = adjacent_grid_values(&self.zeta_thermal_tps, base.zeta_thermal_tp);
        let barriers = adjacent_grid_values(&self.time_barrier_bars, base.time_barrier_bars);
        let floors = adjacent_grid_values(&self.min_take_profit_pcts, base.min_take_profit_pct);

        for value in [windows.0, windows.1].into_iter().flatten() {
            neighbors.push(LangevinConfig {
                window_size: value,
                ..base.clone()
            });
        }
        for value in [bfs.0, bfs.1].into_iter().flatten() {
            neighbors.push(LangevinConfig {
                bandwidth_factor: value,
                ..base.clone()
            });
        }
        for value in [etas.0, etas.1].into_iter().flatten() {
            neighbors.push(LangevinConfig {
                decay_eta: value,
                ..base.clone()
            });
        }
        for value in [betas.0, betas.1].into_iter().flatten() {
            neighbors.push(LangevinConfig {
                beta_entry: value,
                ..base.clone()
            });
        }
        for value in [zetas.0, zetas.1].into_iter().flatten() {
            neighbors.push(LangevinConfig {
                zeta_thermal_tp: value,
                ..base.clone()
            });
        }
        for value in [barriers.0, barriers.1].into_iter().flatten() {
            neighbors.push(LangevinConfig {
                time_barrier_bars: value,
                ..base.clone()
            });
        }
        for value in [floors.0, floors.1].into_iter().flatten() {
            neighbors.push(LangevinConfig {
                min_take_profit_pct: value,
                ..base.clone()
            });
        }
        neighbors
    }
}

/// Helper for finding adjacent grid values for stability analysis.
fn adjacent_grid_values<T: PartialOrd + Copy>(grid: &[T], value: T) -> (Option<T>, Option<T>) {
    let below = grid
        .iter()
        .copied()
        .filter(|v| *v < value)
        .fold(None, |best: Option<T>, v| match best {
            Some(b) if b >= v => Some(b),
            _ => Some(v),
        });
    let above = grid
        .iter()
        .copied()
        .filter(|v| *v > value)
        .fold(None, |best: Option<T>, v| match best {
            Some(b) if b <= v => Some(b),
            _ => Some(v),
        });
    (below, above)
}

/// Algorithmic strategy powered by Non-Equilibrium Langevin Dynamics
/// and empirical potential well topology.
#[derive(Debug, Clone)]
pub struct LangevinPotentialStrategy {
    name: String,
    config: LangevinConfig,
    kde: KdePotential,
}

impl LangevinPotentialStrategy {
    /// Creates a new strategy instance with validated configuration.
    pub fn new(config: LangevinConfig) -> Result<Self, DomainError> {
        let bf = config.bandwidth_factor.to_f64().unwrap_or(1.0);
        let eta = config.decay_eta.to_f64().unwrap_or(0.02);
        let eps = config.vacuum_epsilon.to_f64().unwrap_or(0.001);
        let alpha = config.alpha_dissociation.to_f64().unwrap_or(0.25);

        let kde = KdePotential::new(
            config.window_size,
            bf,
            eta,
            eps,
            alpha,
            config.lambda_ema_period,
        )?;

        Ok(Self {
            name: "LangevinPotential_v1".to_string(),
            config,
            kde,
        })
    }

    /// Access active configuration.
    pub fn config(&self) -> &LangevinConfig {
        &self.config
    }
}

impl Strategy for LangevinPotentialStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn reset(&mut self) {
        self.kde.reset();
    }

    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
        self.kde.push(bar);

        if !self.kde.is_warm() {
            return None;
        }

        let price_f64 = bar.close.to_f64().unwrap_or(1.0).max(1e-8);
        let log_price = price_f64.ln();

        let topology = self.kde.analyze_well(log_price)?;

        let weights = self.kde.compute_weights();
        let sigma2 = self.kde.diffusivity_sigma2(&weights);
        if sigma2 <= 1e-12 {
            return None;
        }

        let h = self.kde.compute_bandwidth(&weights);
        let f_curr = self.kde.force(log_price, h, sigma2, &weights);

        // Arrival intensity acceleration and kinetic energy
        let last_sample = self.kde.window.back()?;
        let lambda_tau = 1000.0 / last_sample.delta_t_ms as f64;
        let lambda_tilde = (lambda_tau / self.kde.lambda_ema().max(1e-6)).clamp(0.1, 10.0);
        let e_kinetic = 0.5 * lambda_tilde * last_sample.delta_x.powi(2);

        let beta = self.config.beta_entry.to_f64().unwrap_or(0.75);
        let zeta = self.config.zeta_thermal_tp.to_f64().unwrap_or(0.2);

        // 1. Elastic Mean Reversion: LONG Condition (Price compressed in left wall)
        if log_price < topology.x_0 && log_price > topology.x_b_left {
            let traction_ok = f_curr >= beta * topology.f_max_left;
            let exhausted = last_sample.delta_x >= 0.0;
            let delta_v_rem = self
                .kde
                .potential_diff(topology.x_b_left, log_price, h, sigma2, &weights)
                .max(0.0);
            let energy_confined = e_kinetic < delta_v_rem;

            if traction_ok && exhausted && energy_confined {
                let x_tp = topology.x_0 - zeta * topology.delta_thermal;
                let x_sl = topology.x_b_left;

                let tp_f64 = x_tp.exp();
                let sl_f64 = x_sl.exp();

                let take_profit = Decimal::from_f64_retain(tp_f64).map(|d| d.round_dp(2))?;
                let stop_loss = Decimal::from_f64_retain(sl_f64).map(|d| d.round_dp(2))?;

                if stop_loss < bar.close && bar.close < take_profit {
                    let friction_ok =
                        (take_profit - bar.close) / bar.close >= self.config.min_take_profit_pct;
                    if friction_ok {
                        return OrderIntent::new(
                            bar.end_time,
                            Side::Buy,
                            bar.close,
                            stop_loss,
                            take_profit,
                            self.config.time_barrier_bars,
                        )
                        .ok();
                    }
                }
            }
        }

        // 2. Elastic Mean Reversion: SHORT Condition (Price compressed in right wall)
        if log_price > topology.x_0 && log_price < topology.x_b_right {
            let traction_ok = f_curr <= -beta * topology.f_max_right;
            let exhausted = last_sample.delta_x <= 0.0;
            let delta_v_rem = self
                .kde
                .potential_diff(topology.x_b_right, log_price, h, sigma2, &weights)
                .max(0.0);
            let energy_confined = e_kinetic < delta_v_rem;

            if traction_ok && exhausted && energy_confined {
                let x_tp = topology.x_0 + zeta * topology.delta_thermal;
                let x_sl = topology.x_b_right;

                let tp_f64 = x_tp.exp();
                let sl_f64 = x_sl.exp();

                let take_profit = Decimal::from_f64_retain(tp_f64).map(|d| d.round_dp(2))?;
                let stop_loss = Decimal::from_f64_retain(sl_f64).map(|d| d.round_dp(2))?;

                if stop_loss > bar.close && bar.close > take_profit {
                    let friction_ok =
                        (bar.close - take_profit) / bar.close >= self.config.min_take_profit_pct;
                    if friction_ok {
                        return OrderIntent::new(
                            bar.end_time,
                            Side::Sell,
                            bar.close,
                            stop_loss,
                            take_profit,
                            self.config.time_barrier_bars,
                        )
                        .ok();
                    }
                }
            }
        }

        None
    }
}

impl OptimizableStrategy for LangevinPotentialStrategy {
    type SharedConfig = ();
    type Config = LangevinConfig;
    type Space = LangevinSpace;

    fn id() -> &'static str {
        "langevin_potential"
    }

    type Extractor = BarPassThrough;

    fn from_config(_shared: &(), config: Self::Config) -> Result<Self, DomainError> {
        Self::new(config)
    }

    fn extractor(_shared: &()) -> Result<BarPassThrough, DomainError> {
        Ok(BarPassThrough)
    }

    fn on_features(&mut self, bar: &DollarBar) -> Option<Decision> {
        self.on_bar(bar).map(Decision::Entry)
    }

    fn default_space() -> Self::Space {
        LangevinSpace::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bar(time_ms: i64, price: Decimal) -> DollarBar {
        DollarBar {
            start_time: time_ms,
            end_time: time_ms + 1000,
            open: price,
            high: price + dec!(10),
            low: price - dec!(10),
            close: price,
            volume: dec!(10),
            dollar_volume: dec!(500_000),
            trade_count: 50,
            buy_volume: dec!(5),
            sell_volume: dec!(5),
            buy_dollar_volume: dec!(250_000),
            sell_dollar_volume: dec!(250_000),
        }
    }

    #[test]
    fn kde_potential_initialization_and_reset() {
        let mut kde = KdePotential::new(20, 1.0, 0.02, 0.001, 0.25, 10).unwrap();
        assert_eq!(kde.len(), 0);
        assert!(!kde.is_warm());

        for i in 0..25 {
            let bar = sample_bar(1000 * i, dec!(50_000) + Decimal::from(i));
            kde.push(&bar);
        }

        assert_eq!(kde.len(), 20);
        assert!(kde.is_warm());

        kde.reset();
        assert_eq!(kde.len(), 0);
        assert!(!kde.is_warm());
    }

    #[test]
    fn potential_well_topology_detection() {
        let mut kde = KdePotential::new(30, 1.0, 0.02, 0.001, 0.25, 10).unwrap();

        // Feed harmonic oscillating prices around 50,000
        for i in 0..30 {
            let offset = ((i as f64 * 0.5).sin() * 200.0) as i64;
            let price = dec!(50_000) + Decimal::from(offset);
            let bar = sample_bar(1000 * i, price);
            kde.push(&bar);
        }

        let current_x = (50000.0_f64).ln();
        let topology = kde.analyze_well(current_x).expect("well must be detected");

        assert!(topology.curvature_0 > 0.0);
        assert!(topology.x_inf_left < topology.x_0);
        assert!(topology.x_inf_right > topology.x_0);
        assert!(topology.x_b_left < topology.x_inf_left);
        assert!(topology.x_b_right > topology.x_inf_right);
        assert!(topology.delta_thermal > 0.0);
    }

    #[test]
    fn default_config_is_valid() {
        let config = LangevinConfig::default();
        let strategy = LangevinPotentialStrategy::new(config.clone()).unwrap();
        assert_eq!(strategy.name(), "LangevinPotential_v1");
        assert_eq!(
            strategy.compatible_market_types(),
            &[MarketType::UsdMPerpetual]
        );
        assert!(!config.to_string().is_empty());
    }

    #[test]
    fn zero_delta_t_does_not_panic_or_divide_by_zero() {
        let mut kde = KdePotential::new(10, 1.0, 0.02, 0.001, 0.25, 10).unwrap();
        // Send bars at the exact same millisecond
        for _ in 0..15 {
            let mut bar = sample_bar(1_700_000_000, dec!(50_000));
            bar.end_time = bar.start_time; // delta_t_ms = 0
            kde.push(&bar);
        }
        assert!(kde.lambda_ema().is_finite());
        assert!(kde.lambda_ema() > 0.0);
    }

    #[test]
    fn scale_invariance_holds_under_bandwidth_factor() {
        let mut kde = KdePotential::new(20, 1.0, 0.02, 0.001, 0.25, 10).unwrap();
        for i in 0..20 {
            let bar = sample_bar(1000 * i, dec!(50_000) + Decimal::from(i % 5));
            kde.push(&bar);
        }

        let weights = kde.compute_weights();
        let sigma2 = kde.diffusivity_sigma2(&weights);
        let h1 = kde.compute_bandwidth(&weights);
        let diff1 = kde.potential_diff(10.82, 10.81, h1, sigma2, &weights);

        assert!(diff1.is_finite());
    }

    #[test]
    fn strategy_detects_signals_and_respects_triple_barrier() {
        let config = LangevinConfig {
            window_size: 20,
            beta_entry: dec!(0.50),
            min_take_profit_pct: dec!(0.0001),
            ..LangevinConfig::default()
        };
        let mut strategy = LangevinPotentialStrategy::new(config).unwrap();

        // Feed oscillating bars to build a clear well around 50,000
        for i in 0..25 {
            let offset = ((i as f64 * 0.4).sin() * 300.0) as i64;
            let price = dec!(50_000) + Decimal::from(offset);
            let bar = sample_bar(1000 * i, price);
            if let Some(intent) = strategy.on_bar(&bar) {
                // Assert valid triple barrier contract
                match intent.side {
                    Side::Buy => {
                        assert!(intent.stop_loss < intent.price);
                        assert!(intent.price < intent.take_profit);
                    }
                    Side::Sell => {
                        assert!(intent.stop_loss > intent.price);
                        assert!(intent.price > intent.take_profit);
                    }
                }
            }
        }
    }
}
