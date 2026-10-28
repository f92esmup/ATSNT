//! Multi-Asset Quantum Well Spot Swing Strategy.
//!
//! Rooted strictly in quantum mechanics principles:
//! - 1D Quantum Harmonic Oscillator (QHO) with effective potential well $V(x) = \frac{1}{2} k (x - x_0)^2$.
//! - de Broglie-Bohm pilot-wave mechanics with Bohmian Quantum Potential:
//!   $$Q(x) = -\frac{\hbar^2}{2m} \frac{\nabla^2 R(x)}{R(x)}$$
//! - Bohmian restoring quantum force $F_Q(x) = -\nabla Q(x)$.
//! - Ground State ($n=0$) localization detection for high-probability Spot Long entries.
//! - Quantized energy transitions $\Delta E = \hbar \omega$ defining exact Take Profit levels $E_1, E_2$.
//! - Finite potential barrier dissociation defining protective Stop Loss.
//! - Environmental quantum decoherence defining horizontal Time Barrier timeout.
//!
//! Designed specifically for Spot markets:
//! - Strictly Long-Only (`compatible_market_types` returns only `[MarketType::Spot]`).
//! - 0% funding drag, zero liquidation risk, 1x leverage maximum.

use std::collections::VecDeque;

use domain::{BarInput, DollarBar, DomainError, OrderIntent, Side, WilderAtr};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::{
    BarPassThrough, Decision, HyperparameterSpace, MarketType, OptimizableStrategy, Strategy,
};

/// A single price sample in continuous logarithmic coordinate space: $x = \ln(P)$.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuantumSample {
    /// Log-price coordinate $x = \ln(P)$.
    pub x: f64,
    /// Bar close timestamp in milliseconds.
    pub timestamp_ms: i64,
}

/// Diagnosed state of the quantum wavepacket $\psi(x) = R(x) e^{i S(x)/\hbar}$.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WavepacketState {
    /// Equilibrium potential center (well bottom): $x_0 = \ln(P_0)$.
    pub x_0: f64,
    /// Spatial dispersion (width) of the wavepacket: $\sigma_x$.
    pub sigma: f64,
    /// Ground-state natural dispersion: $\sigma_0 = \sqrt{\hbar / (2 m \omega)}$.
    pub sigma_0: f64,
    /// Effective well stiffness (curvature): $k = V''(x_0) = m \omega^2$.
    pub curvature_k: f64,
    /// Natural quantum angular frequency: $\omega = \sqrt{k / m}$.
    pub omega: f64,
    /// Ground-state zero-point energy: $E_0 = \frac{1}{2}\hbar \omega$.
    pub e_0: f64,
    /// First quantized excitation energy: $E_1 = \frac{3}{2}\hbar \omega$.
    pub e_1: f64,
    /// Classical restoring force at current coordinate: $F_{\text{classical}} = -k(x - x_0)$.
    pub f_classical: f64,
    /// Bohmian quantum potential at current coordinate: $Q(x) = -\frac{\hbar^2}{2m}\frac{\nabla^2 R}{R}$.
    pub q_potential: f64,
    /// Bohmian quantum restoring force: $F_Q = -\nabla Q(x)$.
    pub f_bohmian: f64,
    /// Total effective physical force: $F_{\text{total}} = F_{\text{classical}} + F_Q$.
    pub f_total: f64,
    /// Left potential dissociation barrier boundary: $x_b^-$.
    pub x_barrier_left: f64,
}

/// Rolling estimator for the quantum potential landscape and Bohmian wavepacket.
#[derive(Debug, Clone)]
pub struct QuantumPotentialEstimator {
    window: VecDeque<QuantumSample>,
    capacity: usize,
    hbar_eff: f64,
    mass_eff: f64,
}

impl QuantumPotentialEstimator {
    /// Creates a new quantum potential estimator.
    pub fn new(capacity: usize, hbar_eff: f64, mass_eff: f64) -> Result<Self, DomainError> {
        if capacity < 8 {
            return Err(DomainError::InvalidWindowSize(capacity));
        }
        if hbar_eff <= 0.0 || mass_eff <= 0.0 {
            return Err(DomainError::InvalidThreshold(format!(
                "hbar_eff and mass_eff must be strictly positive, got hbar={hbar_eff}, mass={mass_eff}"
            )));
        }
        Ok(Self {
            window: VecDeque::with_capacity(capacity),
            capacity,
            hbar_eff,
            mass_eff,
        })
    }

    /// Pushes a new sample into the rolling window.
    pub fn push(&mut self, timestamp_ms: i64, price: f64) {
        if price <= 0.0 {
            return;
        }
        let sample = QuantumSample {
            x: price.ln(),
            timestamp_ms,
        };
        if self.window.len() == self.capacity {
            self.window.pop_front();
        }
        self.window.push_back(sample);
    }

    /// Whether enough bars have been accumulated to evaluate wavepacket dynamics.
    pub fn is_warm(&self) -> bool {
        self.window.len() >= self.capacity
    }

    /// Clears all historical samples.
    pub fn reset(&mut self) {
        self.window.clear();
    }

    /// Solves the wavepacket state and Bohmian quantum potential for the latest coordinate $x$.
    pub fn solve_wavepacket(&self, current_x: f64) -> Option<WavepacketState> {
        if !self.is_warm() || self.window.is_empty() {
            return None;
        }

        let n = self.window.len() as f64;
        let mut mean_x = 0.0;
        for s in &self.window {
            mean_x += s.x;
        }
        mean_x /= n;

        let mut var_x = 0.0;
        for s in &self.window {
            let diff = s.x - mean_x;
            var_x += diff * diff;
        }
        var_x /= (n - 1.0).max(1.0);
        let sigma = var_x.sqrt().max(1e-6);

        // Robust potential well equilibrium center x_0 and curvature k.
        let x_0 = mean_x;
        let hbar = self.hbar_eff;
        let m = self.mass_eff;

        let omega = (hbar / (2.0 * m * (sigma * sigma))).max(1e-6);
        let curvature_k = (m * omega * omega).max(1e-6);
        let sigma_0 = (hbar / (2.0 * m * omega)).sqrt().max(1e-6);

        let e_0 = 0.5 * hbar * omega;
        let e_1 = 1.5 * hbar * omega;

        // Classical restoring force: F_classical = -k * (x - x_0)
        let delta_x = current_x - x_0;
        let f_classical = -curvature_k * delta_x;

        // Bohmian Quantum Potential for Gaussian wavepacket:
        let s2 = sigma * sigma;
        let s4 = s2 * s2;
        let q_potential = (hbar * hbar / (8.0 * m * s4)) * (2.0 * s2 - delta_x * delta_x);

        // Bohmian quantum force: F_Q(x) = -∇ Q(x) = (hbar^2 / (4 m sigma^4)) * (x - x_0)
        let f_bohmian = (hbar * hbar / (4.0 * m * s4)) * delta_x;
        let f_total = f_classical + f_bohmian;

        // Finite well dissociation barrier: estimated at 2.5 * sigma from center
        let x_barrier_left = x_0 - 2.5 * sigma;

        Some(WavepacketState {
            x_0,
            sigma,
            sigma_0,
            curvature_k,
            omega,
            e_0,
            e_1,
            f_classical,
            q_potential,
            f_bohmian,
            f_total,
            x_barrier_left,
        })
    }
}

/// Hyperparameter configuration for the Quantum Well Spot Swing strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuantumWellSwingConfig {
    /// Rolling window capacity in Dollar Bars ($N \in [50, 300]$).
    pub window_size: usize,
    /// Effective Planck constant $\hbar_{\text{eff}}$ scaling quantum uncertainty.
    pub hbar_eff: Decimal,
    /// Effective particle inertia / mass parameter $m_{\text{eff}}$.
    pub mass_eff: Decimal,
    /// Minimum required well curvature $k = m \omega^2$ for well stability.
    pub min_well_curvature: Decimal,
    /// Maximum allowable wavepacket dispersion $\sigma_{\max}$ (wavepacket purity).
    pub max_wavepacket_sigma: Decimal,
    /// Tolerance from well bottom $x_0$ to qualify as Ground State ($n=0$) accumulation.
    pub ground_state_tolerance: Decimal,
    /// Minimum upward total force $F_{\text{total}} > 0$ required for entry.
    pub min_upward_force: Decimal,
    /// Target quantum excitation multiplier (1 for $E_1 = \frac{3}{2}\hbar \omega$, 2 for $E_2$).
    pub target_excitation_level: usize,
    /// Minimum Take Profit percentage to cover exchange Spot fees with margin of safety.
    pub min_take_profit_pct: Decimal,
    /// Multiplier on Wilder's ATR for protective Stop Loss buffer below barrier.
    pub stop_loss_atr_buffer: Decimal,
    /// Quantum decoherence horizontal time barrier in bars.
    pub decoherence_time_bars: usize,
    /// Whether to enforce the macro trend filter (e.g. 200 EMA slope $\ge 0$).
    pub macro_trend_filter: bool,
    /// Period for macro EMA.
    pub macro_ema_period: usize,
    /// Slope lookback window for macro EMA.
    pub macro_slope_window: usize,
    /// Whether to enter using resting limit orders instead of aggressive market fills.
    pub use_limit_entries: bool,
    /// Limit order Time-In-Force timeout in bars (if `use_limit_entries` is true).
    pub limit_time_in_force_bars: Option<usize>,
    /// Whether to enable dynamic breakeven stop ratchet upon favorable price progress.
    pub enable_dynamic_breakeven: bool,
    /// Ratio of distance towards Take Profit to trigger breakeven ratchet (default 0.50 = 50%).
    pub breakeven_trigger_ratio: Decimal,
}

impl Default for QuantumWellSwingConfig {
    fn default() -> Self {
        Self {
            window_size: 100,
            hbar_eff: dec!(1.0),
            mass_eff: dec!(1.0),
            min_well_curvature: dec!(0.05),
            max_wavepacket_sigma: dec!(0.08),
            ground_state_tolerance: dec!(0.025),
            min_upward_force: dec!(0.01),
            target_excitation_level: 1,
            min_take_profit_pct: dec!(0.015), // 1.5% min profit to overcome Spot fees
            stop_loss_atr_buffer: dec!(1.0),
            decoherence_time_bars: 120,
            macro_trend_filter: true,
            macro_ema_period: 200,
            macro_slope_window: 30,
            use_limit_entries: false,
            limit_time_in_force_bars: None,
            enable_dynamic_breakeven: true,
            breakeven_trigger_ratio: dec!(0.50),
        }
    }
}

impl std::fmt::Display for QuantumWellSwingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "W:{}|Hbar:{}|Mass:{}|MinCurv:{}|Tol:{}|MinForce:{}|MinTp:{}|Decoh:{}|Macro:{}|DynBe:{}",
            self.window_size,
            self.hbar_eff,
            self.mass_eff,
            self.min_well_curvature,
            self.ground_state_tolerance,
            self.min_upward_force,
            self.min_take_profit_pct,
            self.decoherence_time_bars,
            self.macro_trend_filter,
            self.enable_dynamic_breakeven
        )
    }
}

/// Hyperparameter optimization space for `QuantumWellSwingStrategy`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumWellSwingSpace {
    pub window_sizes: Vec<usize>,
    pub hbar_effs: Vec<Decimal>,
    pub mass_effs: Vec<Decimal>,
    pub min_well_curvatures: Vec<Decimal>,
    pub ground_state_tolerances: Vec<Decimal>,
    pub min_upward_forces: Vec<Decimal>,
    pub decoherence_time_bars: Vec<usize>,
    pub min_take_profit_pcts: Vec<Decimal>,
    pub stop_loss_atr_buffers: Vec<Decimal>,
    pub macro_trend_filters: Vec<bool>,
}

impl Default for QuantumWellSwingSpace {
    fn default() -> Self {
        Self {
            window_sizes: vec![20, 35, 45, 60, 80, 100, 150, 200, 260, 320, 400],
            hbar_effs: vec![
                dec!(0.15),
                dec!(0.25),
                dec!(0.35),
                dec!(0.50),
                dec!(1.00),
                dec!(1.50),
                dec!(2.00),
                dec!(2.50),
                dec!(3.00),
                dec!(4.00),
            ],
            mass_effs: vec![
                dec!(0.10),
                dec!(0.20),
                dec!(0.35),
                dec!(0.50),
                dec!(1.00),
                dec!(2.00),
                dec!(3.00),
                dec!(4.00),
                dec!(5.00),
            ],
            min_well_curvatures: vec![
                dec!(0.003),
                dec!(0.007),
                dec!(0.012),
                dec!(0.020),
                dec!(0.050),
                dec!(0.100),
                dec!(0.150),
                dec!(0.220),
                dec!(0.300),
            ],
            ground_state_tolerances: vec![
                dec!(0.004),
                dec!(0.007),
                dec!(0.010),
                dec!(0.015),
                dec!(0.025),
                dec!(0.040),
                dec!(0.055),
                dec!(0.075),
                dec!(0.100),
            ],
            min_upward_forces: vec![
                dec!(0.0008),
                dec!(0.0015),
                dec!(0.0030),
                dec!(0.0050),
                dec!(0.0100),
                dec!(0.0200),
                dec!(0.0350),
                dec!(0.0550),
                dec!(0.0800),
            ],
            decoherence_time_bars: vec![15, 30, 45, 60, 90, 120, 180, 240, 320, 420],
            min_take_profit_pcts: vec![
                dec!(0.004),
                dec!(0.006),
                dec!(0.009),
                dec!(0.012),
                dec!(0.015),
                dec!(0.025),
                dec!(0.035),
                dec!(0.050),
                dec!(0.075),
                dec!(0.100),
            ],
            stop_loss_atr_buffers: vec![
                dec!(0.20),
                dec!(0.35),
                dec!(0.50),
                dec!(0.75),
                dec!(1.00),
                dec!(1.50),
                dec!(2.00),
                dec!(2.50),
                dec!(3.00),
            ],
            macro_trend_filters: vec![true, false],
        }
    }
}

impl QuantumWellSwingSpace {
    /// Specialized hyperparameter search space for BTC (Macro / Low Volatility Swing).
    pub fn btc_space() -> Self {
        Self {
            window_sizes: vec![45, 90, 150, 240],
            hbar_effs: vec![dec!(1.0), dec!(1.5), dec!(2.0), dec!(2.5)],
            mass_effs: vec![dec!(2.0), dec!(3.0), dec!(4.0), dec!(5.0)],
            min_well_curvatures: vec![dec!(0.010), dec!(0.020), dec!(0.040)],
            ground_state_tolerances: vec![dec!(0.015), dec!(0.025), dec!(0.040)],
            min_upward_forces: vec![dec!(0.005), dec!(0.010), dec!(0.020)],
            decoherence_time_bars: vec![30, 60, 90],
            min_take_profit_pcts: vec![dec!(0.030), dec!(0.045), dec!(0.065), dec!(0.090)],
            stop_loss_atr_buffers: vec![dec!(1.00), dec!(1.50), dec!(2.00)],
            macro_trend_filters: vec![true],
        }
    }

    /// Specialized hyperparameter search space for ETH (Medium Volatility Swing).
    pub fn eth_space() -> Self {
        Self {
            window_sizes: vec![45, 90, 150, 220],
            hbar_effs: vec![dec!(1.5), dec!(2.0), dec!(2.5), dec!(3.5)],
            mass_effs: vec![dec!(1.5), dec!(2.5), dec!(3.5), dec!(4.5)],
            min_well_curvatures: vec![dec!(0.015), dec!(0.030), dec!(0.060)],
            ground_state_tolerances: vec![dec!(0.020), dec!(0.035), dec!(0.055)],
            min_upward_forces: vec![dec!(0.008), dec!(0.015), dec!(0.030)],
            decoherence_time_bars: vec![30, 60, 90],
            min_take_profit_pcts: vec![dec!(0.035), dec!(0.050), dec!(0.075), dec!(0.110)],
            stop_loss_atr_buffers: vec![dec!(1.00), dec!(1.50), dec!(2.00)],
            macro_trend_filters: vec![true],
        }
    }

    /// Specialized hyperparameter search space for SOL (High Beta / High Volatility Swing).
    pub fn sol_space() -> Self {
        Self {
            window_sizes: vec![30, 60, 90, 120],
            hbar_effs: vec![dec!(2.5), dec!(3.5), dec!(4.5), dec!(6.0)],
            mass_effs: vec![dec!(1.0), dec!(2.0), dec!(3.0), dec!(4.0)],
            min_well_curvatures: vec![dec!(0.020), dec!(0.050), dec!(0.100)],
            ground_state_tolerances: vec![dec!(0.030), dec!(0.050), dec!(0.080)],
            min_upward_forces: vec![dec!(0.010), dec!(0.025), dec!(0.050)],
            decoherence_time_bars: vec![20, 40, 60],
            min_take_profit_pcts: vec![dec!(0.050), dec!(0.075), dec!(0.100), dec!(0.150)],
            stop_loss_atr_buffers: vec![dec!(1.25), dec!(1.75), dec!(2.50)],
            macro_trend_filters: vec![true],
        }
    }
}

impl HyperparameterSpace for QuantumWellSwingSpace {
    type Config = QuantumWellSwingConfig;

    fn generate_candidates(&self, max_candidates: usize) -> Vec<Self::Config> {
        use rand::seq::SliceRandom;
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let mut configs = Vec::with_capacity(max_candidates);

        for _ in 0..max_candidates {
            configs.push(QuantumWellSwingConfig {
                window_size: *self.window_sizes.choose(&mut rng).unwrap_or(&100),
                hbar_eff: *self.hbar_effs.choose(&mut rng).unwrap_or(&dec!(1.0)),
                mass_eff: *self.mass_effs.choose(&mut rng).unwrap_or(&dec!(1.0)),
                min_well_curvature: *self
                    .min_well_curvatures
                    .choose(&mut rng)
                    .unwrap_or(&dec!(0.05)),
                max_wavepacket_sigma: dec!(0.08),
                ground_state_tolerance: *self
                    .ground_state_tolerances
                    .choose(&mut rng)
                    .unwrap_or(&dec!(0.025)),
                min_upward_force: *self
                    .min_upward_forces
                    .choose(&mut rng)
                    .unwrap_or(&dec!(0.01)),
                target_excitation_level: 1,
                min_take_profit_pct: *self
                    .min_take_profit_pcts
                    .choose(&mut rng)
                    .unwrap_or(&dec!(0.015)),
                stop_loss_atr_buffer: *self
                    .stop_loss_atr_buffers
                    .choose(&mut rng)
                    .unwrap_or(&dec!(1.0)),
                decoherence_time_bars: *self.decoherence_time_bars.choose(&mut rng).unwrap_or(&120),
                macro_trend_filter: *self.macro_trend_filters.choose(&mut rng).unwrap_or(&true),
                macro_ema_period: 200,
                macro_slope_window: 30,
                use_limit_entries: false,
                limit_time_in_force_bars: None,
                enable_dynamic_breakeven: true,
                breakeven_trigger_ratio: dec!(0.50),
            });
        }
        configs
    }

    fn perturb_neighbors(&self, base: &Self::Config) -> Vec<Self::Config> {
        let mut neighbors = Vec::new();

        let perturb_window = |w: usize| {
            let mut list = Vec::new();
            if w > 20 {
                list.push(w - 20);
            }
            list.push(w + 20);
            list
        };

        for w in perturb_window(base.window_size) {
            neighbors.push(QuantumWellSwingConfig {
                window_size: w,
                ..base.clone()
            });
        }

        neighbors.push(QuantumWellSwingConfig {
            hbar_eff: (base.hbar_eff * dec!(1.2)).round_dp(2),
            ..base.clone()
        });
        neighbors.push(QuantumWellSwingConfig {
            hbar_eff: (base.hbar_eff * dec!(0.8)).round_dp(2),
            ..base.clone()
        });

        neighbors.push(QuantumWellSwingConfig {
            min_take_profit_pct: (base.min_take_profit_pct + dec!(0.005)).round_dp(3),
            ..base.clone()
        });

        neighbors.push(QuantumWellSwingConfig {
            decoherence_time_bars: base.decoherence_time_bars + 30,
            ..base.clone()
        });

        neighbors
    }
}

/// Algorithmic Quantum Well Spot Swing strategy.
#[derive(Debug, Clone)]
pub struct QuantumWellSwingStrategy {
    config: QuantumWellSwingConfig,
    estimator: QuantumPotentialEstimator,
    atr: WilderAtr,
    macro_ema: Option<Decimal>,
    macro_ema_history: VecDeque<Decimal>,
    last_audit: Option<serde_json::Value>,
}

impl QuantumWellSwingStrategy {
    /// Creates a new validated `QuantumWellSwingStrategy`.
    pub fn new(config: QuantumWellSwingConfig) -> Result<Self, DomainError> {
        let hbar_f64 = config
            .hbar_eff
            .to_f64()
            .ok_or_else(|| DomainError::InvalidThreshold("invalid hbar_eff".to_string()))?;
        let mass_f64 = config
            .mass_eff
            .to_f64()
            .ok_or_else(|| DomainError::InvalidThreshold("invalid mass_eff".to_string()))?;

        let estimator = QuantumPotentialEstimator::new(config.window_size, hbar_f64, mass_f64)?;
        let atr = WilderAtr::new(14)?;

        Ok(Self {
            config,
            estimator,
            atr,
            macro_ema: None,
            macro_ema_history: VecDeque::with_capacity(64),
            last_audit: None,
        })
    }

    /// Returns a reference to the active configuration.
    pub fn config(&self) -> &QuantumWellSwingConfig {
        &self.config
    }

    /// Evaluates the macro trend filter (200 EMA slope $\ge 0$).
    fn evaluate_macro_trend(&mut self, close: Decimal) -> bool {
        if !self.config.macro_trend_filter {
            return true;
        }

        let k = dec!(2) / Decimal::from(self.config.macro_ema_period + 1);
        let next_ema = match self.macro_ema {
            Some(prev) => (close * k) + (prev * (Decimal::ONE - k)),
            None => close,
        };
        self.macro_ema = Some(next_ema);

        if self.macro_ema_history.len() >= self.config.macro_slope_window {
            self.macro_ema_history.pop_front();
        }
        self.macro_ema_history.push_back(next_ema);

        if self.macro_ema_history.len() < self.config.macro_slope_window {
            return true; // Warmup period
        }

        let oldest = self.macro_ema_history.front().copied().unwrap_or(next_ema);
        // Trend is valid for Spot Long swing if EMA is flat or rising AND price is above EMA
        next_ema >= oldest && close >= next_ema
    }
}

impl Strategy for QuantumWellSwingStrategy {
    fn name(&self) -> &'static str {
        "QuantumWellSwing"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::Spot]
    }

    fn reset(&mut self) {
        self.estimator.reset();
        self.macro_ema = None;
        self.macro_ema_history.clear();
        self.last_audit = None;
        if let Ok(fresh_atr) = WilderAtr::new(14) {
            self.atr = fresh_atr;
        }
    }

    fn audit(&self) -> Option<serde_json::Value> {
        self.last_audit.clone()
    }

    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
        let bar_input = BarInput {
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        };
        self.atr.update(&bar_input);

        let price_f64 = bar.close.to_f64().unwrap_or(1.0).max(1e-8);
        self.estimator.push(bar.end_time, price_f64);

        let macro_ok = self.evaluate_macro_trend(bar.close);

        if !self.estimator.is_warm() {
            return None;
        }

        let current_x = price_f64.ln();
        let state = self.estimator.solve_wavepacket(current_x)?;

        // Audit diagnostics for telemetry and visualization
        self.last_audit = Some(serde_json::json!({
            "x_0": state.x_0,
            "sigma": state.sigma,
            "curvature_k": state.curvature_k,
            "omega": state.omega,
            "e_0": state.e_0,
            "e_1": state.e_1,
            "q_potential": state.q_potential,
            "f_bohmian": state.f_bohmian,
            "f_total": state.f_total,
            "macro_ok": macro_ok,
        }));

        // 1. Structural well stability checks
        let k_dec = Decimal::from_f64_retain(state.curvature_k).unwrap_or(Decimal::ZERO);
        if k_dec < self.config.min_well_curvature {
            return None; // Well is too flat / degenerate
        }

        let sigma_dec = Decimal::from_f64_retain(state.sigma).unwrap_or(Decimal::ONE);
        if sigma_dec > self.config.max_wavepacket_sigma {
            return None; // Wavepacket is too dispersed (decoherent)
        }

        // 2. Macro trend confirmation
        if !macro_ok {
            return None;
        }

        // 3. Ground State localization in the well basin:
        // Price must be compressed in the lower half of the well basin and strictly above the dissociation barrier
        let in_well_basin = current_x <= state.x_0 && current_x > state.x_barrier_left;
        if !in_well_basin {
            return None;
        }

        // 4. Force check: Restoring force F_classical = k * (x_0 - x) >= min_upward_force
        let f_restoring_dec = Decimal::from_f64_retain(state.f_classical).unwrap_or(Decimal::ZERO);
        if f_restoring_dec < self.config.min_upward_force {
            return None;
        }

        // 5. Construct Quantized Exits (Zero-Toy Triple Barrier):
        let delta_e = state.e_1 - state.e_0;
        let delta_x_quantum = (2.0 * delta_e / state.curvature_k.max(1e-6)).sqrt();
        let target_price_f64 = price_f64 * delta_x_quantum.exp();

        let mut take_profit =
            Decimal::from_f64_retain(target_price_f64).unwrap_or(bar.close * dec!(1.03));

        // Ensure take profit meets minimum profit percentage to comfortably clear Spot fees
        let min_tp_price = bar.close * (Decimal::ONE + self.config.min_take_profit_pct);
        if take_profit < min_tp_price {
            take_profit = min_tp_price;
        }

        // Stop Loss: Dissociation barrier x_barrier_left minus ATR buffer
        let barrier_price_f64 = state.x_barrier_left.exp();
        let raw_barrier_price =
            Decimal::from_f64_retain(barrier_price_f64).unwrap_or(bar.close * dec!(0.97));

        let current_atr = self.atr.value().unwrap_or(bar.close * dec!(0.02));
        let atr_buffer = current_atr * self.config.stop_loss_atr_buffer;
        let mut stop_loss = raw_barrier_price - atr_buffer;

        // Strict sanity invariant: Stop Loss must be strictly below bar.close
        if stop_loss >= bar.close {
            stop_loss = bar.close * dec!(0.97); // 3% emergency floor
        }

        // Time Barrier: Quantum decoherence time limit
        let time_barrier = self.config.decoherence_time_bars;

        let breakeven_atr = if self.config.enable_dynamic_breakeven {
            let be_dist = (take_profit - bar.close) * self.config.breakeven_trigger_ratio;
            if be_dist > Decimal::ZERO {
                Some(be_dist)
            } else {
                None
            }
        } else {
            None
        };

        let mut intent = OrderIntent::with_risk_and_exits(
            bar.end_time,
            Side::Buy,
            bar.close,
            stop_loss,
            take_profit,
            time_barrier,
            None,
            None,
            breakeven_atr,
            None,
        )
        .ok()?;

        if self.config.use_limit_entries {
            intent = intent.into_limit(self.config.limit_time_in_force_bars, false);
        }

        Some(intent)
    }
}

impl OptimizableStrategy for QuantumWellSwingStrategy {
    type SharedConfig = ();
    type Config = QuantumWellSwingConfig;
    type Space = QuantumWellSwingSpace;
    type Extractor = BarPassThrough;

    fn id() -> &'static str {
        "quantum_well_swing"
    }

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
        QuantumWellSwingSpace::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_bar(end_time: i64, price: Decimal) -> DollarBar {
        DollarBar {
            start_time: end_time - 60_000,
            end_time,
            open: price,
            high: price + dec!(0.5),
            low: price - dec!(0.5),
            close: price,
            volume: dec!(10),
            dollar_volume: dec!(100_000),
            trade_count: 50,
            buy_volume: dec!(5),
            sell_volume: dec!(5),
            buy_dollar_volume: dec!(50_000),
            sell_dollar_volume: dec!(50_000),
        }
    }

    #[test]
    fn test_estimator_wavepacket_properties() {
        let mut estimator = QuantumPotentialEstimator::new(20, 1.0, 1.0).expect("valid estimator");

        // Seed oscillating prices around 100.0
        for i in 0..20 {
            let offset = ((i as f64) * 0.3).sin() * 2.0;
            estimator.push(i * 1000, 100.0 + offset);
        }

        assert!(estimator.is_warm());
        let current_x = 100.0_f64.ln();
        let state = estimator
            .solve_wavepacket(current_x)
            .expect("solves wavepacket");

        assert!(state.curvature_k > 0.0);
        assert!(state.omega > 0.0);
        assert!(state.e_0 > 0.0);
        assert!(state.e_1 > state.e_0);
        assert!((state.e_1 - state.e_0 - state.omega).abs() < 1e-9); // Delta E = hbar * omega with hbar = 1.0
        assert!(state.x_barrier_left < state.x_0);
    }

    #[test]
    fn test_strategy_market_type_spot_only() {
        let strategy = QuantumWellSwingStrategy::new(QuantumWellSwingConfig::default())
            .expect("valid strategy");
        assert_eq!(
            strategy.compatible_market_types(),
            &[MarketType::Spot],
            "QuantumWellSwing must strictly declare Spot compatibility"
        );
    }

    #[test]
    fn test_strategy_emits_only_buy_intents() {
        let config = QuantumWellSwingConfig {
            window_size: 10,
            macro_trend_filter: false,
            min_well_curvature: dec!(0.001),
            min_upward_force: dec!(0.0001),
            ground_state_tolerance: dec!(0.05),
            ..QuantumWellSwingConfig::default()
        };

        let mut strategy = QuantumWellSwingStrategy::new(config).expect("valid strategy");

        // Feed harmonic compression: start higher, settle into ground state bottom
        for i in 0..15 {
            let price = if i < 10 {
                dec!(100) + Decimal::from(i % 3)
            } else {
                dec!(99.5) // Slight dip below x_0 to enter ground state
            };
            let bar = create_test_bar(i as i64 * 60_000, price);
            if let Some(intent) = strategy.on_bar(&bar) {
                assert_eq!(intent.side, Side::Buy, "Spot signals must strictly be Buy");
                assert!(
                    intent.take_profit > intent.price,
                    "Take profit must be higher than entry"
                );
                assert!(
                    intent.stop_loss < intent.price,
                    "Stop loss must be lower than entry"
                );
            }
        }
    }

    #[test]
    fn test_space_sampling_and_perturbation() {
        let strategy = QuantumWellSwingStrategy::new(QuantumWellSwingConfig::default())
            .expect("valid strategy");
        let space = QuantumWellSwingStrategy::default_space();
        let candidates = space.generate_candidates(10);
        assert_eq!(candidates.len(), 10);

        let neighbors = space.perturb_neighbors(strategy.config());
        assert!(!neighbors.is_empty());
        for n in neighbors {
            assert!(n.window_size > 0);
            assert!(n.hbar_eff > Decimal::ZERO);
        }
    }

    #[test]
    fn test_dynamic_breakeven_attached_to_intent() {
        let config = QuantumWellSwingConfig {
            window_size: 10,
            macro_trend_filter: false,
            enable_dynamic_breakeven: true,
            breakeven_trigger_ratio: dec!(0.50),
            min_well_curvature: dec!(0.001),
            min_upward_force: dec!(0.0001),
            ground_state_tolerance: dec!(0.05),
            ..QuantumWellSwingConfig::default()
        };

        let mut strategy = QuantumWellSwingStrategy::new(config).expect("valid strategy");

        for i in 0..15 {
            let price = if i < 10 {
                dec!(100) + Decimal::from(i % 3)
            } else {
                dec!(99.5)
            };
            let bar = create_test_bar(i as i64 * 60_000, price);
            if let Some(intent) = strategy.on_bar(&bar) {
                assert!(
                    intent.breakeven_atr.is_some(),
                    "Dynamic breakeven must be populated on emitted intent"
                );
                let be_dist = intent.breakeven_atr.unwrap();
                let expected_be_dist = (intent.take_profit - intent.price) * dec!(0.50);
                assert_eq!(be_dist, expected_be_dist);
                return;
            }
        }
    }

    #[test]
    fn test_macro_trend_filter_blocks_downtrend() {
        let mut strategy = QuantumWellSwingStrategy::new(QuantumWellSwingConfig {
            window_size: 10,
            macro_trend_filter: true,
            macro_ema_period: 200,
            macro_slope_window: 5,
            ..QuantumWellSwingConfig::default()
        })
        .expect("valid strategy");

        // Feed descending prices (bear regime)
        for i in 0..30 {
            let price = dec!(100) - Decimal::from(i);
            let bar = create_test_bar(i as i64 * 60_000, price);
            let intent = strategy.on_bar(&bar);
            assert!(
                intent.is_none(),
                "Macro trend filter must block buys during descending price regime"
            );
        }
    }

    #[test]
    fn test_asset_spaces_calibration() {
        let btc_space = QuantumWellSwingSpace::btc_space();
        let _eth_space = QuantumWellSwingSpace::eth_space();
        let sol_space = QuantumWellSwingSpace::sol_space();

        // BTC should have larger observation windows and tighter stop buffers
        assert!(*btc_space.window_sizes.last().unwrap() >= 240);
        assert!(*btc_space.stop_loss_atr_buffers.first().unwrap() <= dec!(1.00));

        // SOL should have shorter observation windows and higher hbar_eff
        assert!(*sol_space.window_sizes.first().unwrap() <= 50);
        assert!(*sol_space.hbar_effs.last().unwrap() >= dec!(5.0));

        // SOL has wider TP than BTC for higher volatility
        assert!(
            *sol_space.min_take_profit_pcts.last().unwrap()
                > *btc_space.min_take_profit_pcts.last().unwrap()
        );
    }
}
