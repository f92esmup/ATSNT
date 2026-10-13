use domain::DollarBar;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rust_decimal::{Decimal, MathematicalOps};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use strategies::{HyperparameterSpace, OptimizableStrategy};

use crate::config::BacktestConfig;
use crate::engine::BacktestEngine;
use crate::error::BacktestError;
use crate::metrics::BacktestMetrics;
use crate::walk_forward::WalkForwardFold;

/// Backwards-compatibility alias for DollarBarsCusumSpace.
pub type ParameterSpace = strategies::DollarBarsCusumSpace;

/// Runs a deterministic backtest against a contiguous slice of `DollarBar`s.
pub fn run_backtest_slice<S: OptimizableStrategy>(
    config: BacktestConfig,
    strat_config: &S::Config,
    bars: &[DollarBar],
) -> Result<BacktestMetrics, BacktestError> {
    let mut strategy =
        S::from_config(strat_config.clone()).map_err(|e| BacktestError::Strategy(e.to_string()))?;
    let mut engine = BacktestEngine::new(config);

    for bar in bars {
        engine.process_bar(&mut strategy, bar);
    }

    let mark_price = bars.last().map(|b| b.close);
    Ok(engine.finish(mark_price))
}

/// Evaluation outcome for a single hyperparameter candidate across Walk-Forward folds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEvaluation<C> {
    /// Evaluated parameter configuration.
    pub config: C,
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
    /// Full in-sample and out-of-sample metrics for every Walk-Forward fold.
    #[serde(default)]
    pub fold_metrics: Vec<FoldMetrics>,
}

/// In-sample and out-of-sample performance of one candidate on one fold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldMetrics {
    /// Index of the Walk-Forward fold.
    pub fold_idx: usize,
    /// Metrics on the fold's training slice.
    pub in_sample: BacktestMetrics,
    /// Metrics on the fold's test slice.
    pub out_of_sample: BacktestMetrics,
}

/// Evaluates parameter stability by perturbing parameters by $\pm \delta$ on In-Sample bars.
pub fn calculate_parameter_stability<S: OptimizableStrategy>(
    space: &S::Space,
    base: &S::Config,
    bars: &[DollarBar],
    engine_config: BacktestConfig,
) -> Decimal {
    let neighbors = space.perturb_neighbors(base);
    let mut sortinos = Vec::with_capacity(neighbors.len() + 1);

    if let Ok(m) = run_backtest_slice::<S>(engine_config, base, bars) {
        sortinos.push(m.sortino_ratio);
    }

    for neighbor in &neighbors {
        if let Ok(m) = run_backtest_slice::<S>(engine_config, neighbor, bars) {
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
pub struct WalkForwardOptimizer<S: OptimizableStrategy> {
    engine_config: BacktestConfig,
    parameter_space: S::Space,
    _marker: std::marker::PhantomData<fn() -> S>,
}

impl<S: OptimizableStrategy> WalkForwardOptimizer<S> {
    /// Creates a new optimizer instance with the specified parameter space.
    pub fn new(engine_config: BacktestConfig, parameter_space: S::Space) -> Self {
        Self {
            engine_config,
            parameter_space,
            _marker: std::marker::PhantomData,
        }
    }

    /// Creates a new optimizer instance using the strategy's default parameter space.
    pub fn with_default_space(engine_config: BacktestConfig) -> Self {
        Self::new(engine_config, S::default_space())
    }

    /// Evaluates candidate configurations in parallel across all CPU cores with `Rayon`.
    pub fn run_optimization(
        &self,
        bars: &[DollarBar],
        folds: &[WalkForwardFold],
        max_candidates: usize,
    ) -> Result<Vec<CandidateEvaluation<S::Config>>, BacktestError> {
        if folds.is_empty() {
            return Err(BacktestError::InvalidFoldConfig(
                "at least one fold required".to_string(),
            ));
        }

        let candidates = self.parameter_space.generate_candidates(max_candidates);
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

        let mut evaluations: Vec<CandidateEvaluation<S::Config>> = candidates
            .par_iter()
            .filter_map(|candidate| {
                let mut is_sortinos = Vec::with_capacity(folds.len());
                let mut oos_sortinos = Vec::with_capacity(folds.len());
                let mut oos_profit_factors = Vec::with_capacity(folds.len());
                let mut total_oos_trades = 0usize;
                let mut fold_metrics = Vec::with_capacity(folds.len());

                for fold in folds.iter() {
                    let train_slice = fold.train_slice(bars);
                    let test_slice = fold.test_slice(bars);

                    let is_metrics =
                        run_backtest_slice::<S>(config, candidate, train_slice).ok()?;
                    let oos_metrics =
                        run_backtest_slice::<S>(config, candidate, test_slice).ok()?;

                    is_sortinos.push(is_metrics.sortino_ratio);
                    oos_sortinos.push(oos_metrics.sortino_ratio);
                    oos_profit_factors.push(oos_metrics.profit_factor);
                    total_oos_trades += oos_metrics.total_trades;
                    fold_metrics.push(FoldMetrics {
                        fold_idx: fold.fold_idx,
                        in_sample: is_metrics,
                        out_of_sample: oos_metrics,
                    });
                }

                let num_folds_dec = Decimal::from(folds.len());
                let mean_is_sortino = is_sortinos.iter().copied().sum::<Decimal>() / num_folds_dec;
                let mean_oos_sortino =
                    oos_sortinos.iter().copied().sum::<Decimal>() / num_folds_dec;
                let mean_oos_profit_factor =
                    oos_profit_factors.iter().copied().sum::<Decimal>() / num_folds_dec;

                // Parameter stability evaluated on the first fold's In-Sample slice
                let first_train = folds[0].train_slice(bars);
                let stability_score = calculate_parameter_stability::<S>(
                    &self.parameter_space,
                    candidate,
                    first_train,
                    config,
                );

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
                    fold_metrics,
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
    use strategies::DollarBarsCusumStrategy;

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

        let optimizer =
            WalkForwardOptimizer::<DollarBarsCusumStrategy>::new(engine_config, param_space);
        let results = optimizer.run_optimization(&bars, &folds, 8).unwrap();

        assert!(!results.is_empty());
        // Verify results are sorted descending by fitness
        for i in 1..results.len() {
            assert!(results[i - 1].fitness >= results[i].fitness);
        }
    }

    // Custom Mock Strategy to verify that WalkForwardOptimizer is 100% generic
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct MockConfig {
        window: usize,
        threshold: Decimal,
    }

    impl std::fmt::Display for MockConfig {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "window:{}|threshold:{}", self.window, self.threshold)
        }
    }

    #[derive(Debug, Clone)]
    struct MockSpace {
        windows: Vec<usize>,
        thresholds: Vec<Decimal>,
    }

    impl HyperparameterSpace for MockSpace {
        type Config = MockConfig;

        fn generate_candidates(&self, max_candidates: usize) -> Vec<Self::Config> {
            let mut res = Vec::new();
            for &w in &self.windows {
                for &t in &self.thresholds {
                    res.push(MockConfig {
                        window: w,
                        threshold: t,
                    });
                }
            }
            res.truncate(max_candidates);
            res
        }

        fn perturb_neighbors(&self, base: &Self::Config) -> Vec<Self::Config> {
            vec![
                MockConfig {
                    window: base.window.saturating_sub(1).max(1),
                    threshold: base.threshold,
                },
                MockConfig {
                    window: base.window + 1,
                    threshold: base.threshold,
                },
            ]
        }
    }

    struct MockStrategy {
        config: MockConfig,
        counter: usize,
    }

    impl strategies::Strategy for MockStrategy {
        fn name(&self) -> &str {
            "MockStrategy"
        }

        fn compatible_market_types(&self) -> &'static [strategies::MarketType] {
            &[strategies::MarketType::UsdMPerpetual]
        }

        fn on_bar(&mut self, bar: &DollarBar) -> Option<domain::OrderIntent> {
            self.counter += 1;
            if self.counter >= self.config.window {
                self.counter = 0;
                domain::OrderIntent::new(
                    bar.end_time,
                    domain::Side::Buy,
                    bar.close,
                    bar.close - dec!(100),
                    bar.close + dec!(100),
                    5,
                )
                .ok()
            } else {
                None
            }
        }

        fn reset(&mut self) {
            self.counter = 0;
        }
    }

    impl OptimizableStrategy for MockStrategy {
        type Config = MockConfig;
        type Space = MockSpace;

        fn id() -> &'static str {
            "mock_strategy"
        }

        fn from_config(config: Self::Config) -> Result<Self, domain::DomainError> {
            Ok(Self { config, counter: 0 })
        }

        fn default_space() -> Self::Space {
            MockSpace {
                windows: vec![3, 5, 8],
                thresholds: vec![dec!(1.0), dec!(2.0)],
            }
        }
    }

    #[test]
    fn run_optimization_for_custom_mock_strategy() {
        let bars = generate_synthetic_bars(200);
        let wf_config = WalkForwardConfig {
            num_folds: 2,
            train_ratio: 0.70,
            embargo_bars: 5,
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

        let optimizer = WalkForwardOptimizer::<MockStrategy>::with_default_space(engine_config);
        let results = optimizer.run_optimization(&bars, &folds, 6).unwrap();

        assert!(!results.is_empty());
        for i in 1..results.len() {
            assert!(results[i - 1].fitness >= results[i].fitness);
        }
    }
}
