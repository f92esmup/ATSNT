use domain::DollarBar;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rust_decimal::{Decimal, MathematicalOps};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy};

use crate::config::BacktestConfig;
use crate::engine::BacktestEngine;
use crate::error::BacktestError;
use crate::metrics::BacktestMetrics;
use crate::walk_forward::WalkForwardFold;

/// Runs a deterministic backtest against a contiguous slice of `DollarBar`s.
pub fn run_backtest_slice(
    config: BacktestConfig,
    strat_config: &DollarBarsCusumConfig,
    bars: &[DollarBar],
) -> Result<BacktestMetrics, BacktestError> {
    let mut strategy = DollarBarsCusumStrategy::new(strat_config.clone())
        .map_err(|e| BacktestError::Strategy(e.to_string()))?;
    let mut engine = BacktestEngine::new(config);

    for bar in bars {
        engine.process_bar(&mut strategy, bar);
    }

    let mark_price = bars.last().map(|b| b.close);
    Ok(engine.finish(mark_price))
}

/// Evaluation outcome for a single hyperparameter candidate across Walk-Forward folds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    /// Evaluated parameter configuration.
    pub config: DollarBarsCusumConfig,
    /// Mean In-Sample Sortino Ratio across all folds.
    pub mean_is_sortino: Decimal,
    /// Mean Out-of-Sample Sortino Ratio across all folds.
    pub mean_oos_sortino: Decimal,
    /// Mean Out-of-Sample Profit Factor.
    pub mean_oos_profit_factor: Decimal,
    /// Total Out-of-Sample executed trades across all folds.
    pub total_oos_trades: usize,
    /// Parameter stability score [0.0 .. 1.0] penalizing fragile peaks.
    pub stability_score: Decimal,
    /// Final composite fitness: `mean_is_sortino * stability_score`.
    pub fitness: Decimal,
    /// Deflated Sharpe Ratio p-value (AFML Ch. 14).
    pub dsr_p_value: Decimal,
    /// Whether this candidate passes statistical significance (p < 0.05).
    pub is_statistically_significant: bool,
}

/// Parameter search space definition for `DollarBarsCusumConfig`.
#[derive(Debug, Clone)]
pub struct ParameterSpace {
    pub rolling_window_lens: Vec<usize>,
    pub cusum_vol_multipliers: Vec<Decimal>,
    pub z_entry_thresholds: Vec<Decimal>,
    pub z_stop_thresholds: Vec<Decimal>,
    pub time_barrier_bars: Vec<usize>,
}

impl Default for ParameterSpace {
    fn default() -> Self {
        Self {
            rolling_window_lens: vec![10, 15, 20, 25, 30],
            cusum_vol_multipliers: vec![dec!(1.0), dec!(1.5), dec!(2.0), dec!(2.5)],
            z_entry_thresholds: vec![dec!(1.2), dec!(1.5), dec!(1.8), dec!(2.2)],
            z_stop_thresholds: vec![dec!(2.5), dec!(3.0), dec!(3.5)],
            time_barrier_bars: vec![10, 15, 20],
        }
    }
}

impl ParameterSpace {
    /// Generates grid combinations within the search space.
    pub fn generate_grid(&self, max_combinations: usize) -> Vec<DollarBarsCusumConfig> {
        let mut configs = Vec::new();

        for &win in &self.rolling_window_lens {
            for &vol in &self.cusum_vol_multipliers {
                for &z_entry in &self.z_entry_thresholds {
                    for &z_stop in &self.z_stop_thresholds {
                        for &tb in &self.time_barrier_bars {
                            configs.push(DollarBarsCusumConfig {
                                rolling_window_len: win,
                                cusum_vol_multiplier: vol,
                                z_entry_threshold: z_entry,
                                z_stop_threshold: z_stop,
                                time_barrier_bars: tb,
                            });
                            if configs.len() >= max_combinations {
                                return configs;
                            }
                        }
                    }
                }
            }
        }

        configs
    }
}

/// Evaluates parameter stability by perturbing parameters by $\pm \delta$ on In-Sample bars.
pub fn calculate_parameter_stability(
    base: &DollarBarsCusumConfig,
    bars: &[DollarBar],
    engine_config: BacktestConfig,
) -> Decimal {
    let mut neighbors = Vec::with_capacity(4);

    // Perturbation 1: cusum_vol_multiplier +/- 0.2
    if base.cusum_vol_multiplier > dec!(0.5) {
        let mut c = base.clone();
        c.cusum_vol_multiplier -= dec!(0.2);
        neighbors.push(c);
    }
    {
        let mut c = base.clone();
        c.cusum_vol_multiplier += dec!(0.2);
        neighbors.push(c);
    }

    // Perturbation 2: z_entry_threshold +/- 0.2
    if base.z_entry_threshold > dec!(0.5) {
        let mut c = base.clone();
        c.z_entry_threshold -= dec!(0.2);
        neighbors.push(c);
    }
    {
        let mut c = base.clone();
        c.z_entry_threshold += dec!(0.2);
        neighbors.push(c);
    }

    let mut sortinos = Vec::with_capacity(neighbors.len() + 1);
    if let Ok(m) = run_backtest_slice(engine_config, base, bars) {
        sortinos.push(m.sortino_ratio);
    }

    for neighbor in &neighbors {
        if let Ok(m) = run_backtest_slice(engine_config, neighbor, bars) {
            sortinos.push(m.sortino_ratio);
        }
    }

    if sortinos.len() <= 1 {
        return dec!(1.0);
    }

    let count = Decimal::from(sortinos.len());
    let mean = sortinos.iter().copied().sum::<Decimal>() / count;

    let variance = sortinos
        .iter()
        .map(|&s| (s - mean) * (s - mean))
        .sum::<Decimal>()
        / count;

    // Standard deviation of Sortinos across neighborhood
    // Using rust_decimal sqrt feature
    let std_dev = variance.sqrt().unwrap_or(Decimal::ZERO);

    // Stability Score = 1 / (1 + lambda * std_dev)
    // Range [0.0 .. 1.0], 1.0 = completely flat robust plateau
    let lambda = dec!(0.5);
    let denominator = Decimal::ONE + (lambda * std_dev);
    if denominator <= Decimal::ZERO {
        dec!(1.0)
    } else {
        (Decimal::ONE / denominator).min(Decimal::ONE)
    }
}

/// Computes the Deflated Sharpe Ratio (DSR) p-value following Marcos López de Prado (AFML Ch. 14).
pub fn calculate_dsr(observed_sr: Decimal, total_trials: usize, num_bars: usize) -> Decimal {
    if observed_sr <= Decimal::ZERO || total_trials <= 1 || num_bars <= 10 {
        return dec!(1.0); // Not significant
    }

    let n = total_trials as f64;
    // Expected maximum Sharpe Ratio under null hypothesis of zero skill:
    // E[max_N] ~ sqrt(2 * ln(N)) + gamma / sqrt(2 * ln(N))
    let e_max_sr = (2.0 * n.ln()).sqrt() + (0.5772156649 / (2.0 * n.ln()).sqrt());

    let obs_sr_f64 = observed_sr.to_string().parse::<f64>().unwrap_or(0.0);
    let t_f64 = num_bars as f64;

    // Test statistic z = (SR - E[max_SR]) * sqrt(T - 1)
    let z = (obs_sr_f64 - e_max_sr) * (t_f64 - 1.0).sqrt() / 1.0;

    // Normal CDF approximation for p-value: P(Z > z)
    let p_val = 0.5 * (1.0 - erf(z / std::f64::consts::SQRT_2));
    Decimal::from_str_exact(&format!("{:.4}", p_val.clamp(0.0, 1.0))).unwrap_or(dec!(0.5))
}

/// Abramowitz and Stegun error function approximation
fn erf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let abs_x = x.abs();

    let t = 1.0 / (1.0 + p * abs_x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-abs_x * abs_x).exp();

    sign * y
}

/// High-throughput parallel Walk-Forward optimizer powered by `Rayon`.
pub struct WalkForwardOptimizer {
    engine_config: BacktestConfig,
    parameter_space: ParameterSpace,
}

impl WalkForwardOptimizer {
    /// Creates a new optimizer instance.
    pub fn new(engine_config: BacktestConfig, parameter_space: ParameterSpace) -> Self {
        Self {
            engine_config,
            parameter_space,
        }
    }

    /// Evaluates candidate configurations in parallel across all CPU cores with `Rayon`.
    pub fn run_optimization(
        &self,
        bars: &[DollarBar],
        folds: &[WalkForwardFold],
        max_candidates: usize,
    ) -> Result<Vec<CandidateEvaluation>, BacktestError> {
        if folds.is_empty() {
            return Err(BacktestError::InvalidFoldConfig(
                "at least one fold required".to_string(),
            ));
        }

        let candidates = self.parameter_space.generate_grid(max_candidates);
        let total_candidates = candidates.len();
        let total_bars = bars.len();
        let config = self.engine_config;

        let pb = ProgressBar::new(total_candidates as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) | ETA: {eta_precise} | {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("#>-"),
        );
        pb.set_message("Evaluating HPO parameter space");

        let mut evaluations: Vec<CandidateEvaluation> = candidates
            .par_iter()
            .filter_map(|candidate| {
                let mut is_sortinos = Vec::with_capacity(folds.len());
                let mut oos_sortinos = Vec::with_capacity(folds.len());
                let mut oos_profit_factors = Vec::with_capacity(folds.len());
                let mut total_oos_trades = 0usize;

                for fold in folds.iter() {
                    let train_slice = fold.train_slice(bars);
                    let test_slice = fold.test_slice(bars);

                    let is_metrics = run_backtest_slice(config, candidate, train_slice).ok()?;
                    let oos_metrics = run_backtest_slice(config, candidate, test_slice).ok()?;

                    is_sortinos.push(is_metrics.sortino_ratio);
                    oos_sortinos.push(oos_metrics.sortino_ratio);
                    oos_profit_factors.push(oos_metrics.profit_factor);
                    total_oos_trades += oos_metrics.total_trades;
                }

                let num_folds_dec = Decimal::from(folds.len());
                let mean_is_sortino = is_sortinos.iter().copied().sum::<Decimal>() / num_folds_dec;
                let mean_oos_sortino =
                    oos_sortinos.iter().copied().sum::<Decimal>() / num_folds_dec;
                let mean_oos_profit_factor =
                    oos_profit_factors.iter().copied().sum::<Decimal>() / num_folds_dec;

                // Parameter stability evaluated on the first fold's In-Sample slice
                let first_train = folds[0].train_slice(bars);
                let stability_score = calculate_parameter_stability(candidate, first_train, config);

                // Fitness = max(0, mean_is_sortino) * stability_score
                let clamped_is_sortino = mean_is_sortino.max(Decimal::ZERO);
                let fitness = clamped_is_sortino * stability_score;

                // Statistical significance via Deflated Sharpe Ratio
                let dsr_p = calculate_dsr(mean_oos_sortino, total_candidates, total_bars);
                let is_significant = dsr_p < dec!(0.05);

                pb.inc(1);

                Some(CandidateEvaluation {
                    config: candidate.clone(),
                    mean_is_sortino,
                    mean_oos_sortino,
                    mean_oos_profit_factor,
                    total_oos_trades,
                    stability_score,
                    fitness,
                    dsr_p_value: dsr_p,
                    is_statistically_significant: is_significant,
                })
            })
            .collect();

        pb.finish_with_message("HPO parameter evaluation complete");

        // Sort descending by Fitness (highest quality robust configurations first)
        evaluations.sort_by_key(|a| std::cmp::Reverse(a.fitness));

        Ok(evaluations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk_forward::WalkForwardConfig;
    use crate::walk_forward::WalkForwardSplitter;

    fn generate_synthetic_bars(count: usize) -> Vec<DollarBar> {
        let mut bars = Vec::with_capacity(count);
        let mut current_price = dec!(40000.0);
        let mut current_time = 1700000000000i64;

        for i in 0..count {
            // Oscillating price movement
            let delta = if i % 2 == 0 { dec!(50.0) } else { dec!(-48.0) };
            current_price += delta;
            current_time += 60000; // 1 minute per bar

            bars.push(DollarBar {
                start_time: current_time - 60000,
                end_time: current_time,
                open: current_price - delta,
                high: current_price + dec!(20.0),
                low: current_price - dec!(20.0),
                close: current_price,
                volume: dec!(10.0),
                dollar_volume: dec!(400000.0),
                trade_count: 50,
            });
        }
        bars
    }

    #[test]
    fn run_parallel_walk_forward_optimization() {
        let bars = generate_synthetic_bars(500);
        let wf_config = WalkForwardConfig {
            num_folds: 2,
            train_ratio: 0.70,
            embargo_bars: 10,
        };
        let folds = WalkForwardSplitter::generate_rolling_folds(bars.len(), &wf_config).unwrap();

        let engine_config = BacktestConfig {
            initial_capital: dec!(10000.0),
            maker_fee_pct: dec!(0.0002),
            taker_fee_pct: dec!(0.0005),
            slippage_pct: dec!(0.0005),
            risk_per_trade_pct: dec!(0.01),
            max_daily_drawdown_pct: dec!(0.05),
        };

        let param_space = ParameterSpace {
            rolling_window_lens: vec![5, 10],
            cusum_vol_multipliers: vec![dec!(1.0), dec!(1.5)],
            z_entry_thresholds: vec![dec!(1.0), dec!(1.5)],
            z_stop_thresholds: vec![dec!(2.5)],
            time_barrier_bars: vec![5],
        };

        let optimizer = WalkForwardOptimizer::new(engine_config, param_space);
        let results = optimizer.run_optimization(&bars, &folds, 8).unwrap();

        assert!(!results.is_empty());
        // Verify results are sorted descending by fitness
        for i in 1..results.len() {
            assert!(results[i - 1].fitness >= results[i].fitness);
        }
    }
}
