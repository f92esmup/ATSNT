use domain::DollarBar;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use strategies::{HyperparameterSpace, OptimizableStrategy};

use crate::config::BacktestConfig;
use crate::engine::BacktestEngine;
use crate::error::BacktestError;
use crate::metrics::{trade_returns, BacktestMetrics, ClosedTrade};
use crate::walk_forward::WalkForwardFold;

/// Closed trades a candidate needs in every in-sample and out-of-sample fold to be ranked.
///
/// Below it a ratio says more about luck than about the rule: a fold with two winning trades
/// has no measurable downside. 30 is the usual floor for a sample mean to be meaningful.
pub const DEFAULT_MIN_TRADES_PER_FOLD: usize = 30;

/// p-value below which a candidate's Deflated Sharpe Ratio counts as significant.
const DSR_SIGNIFICANCE: Decimal = dec!(0.05);

/// Runs a deterministic backtest against a contiguous slice of `DollarBar`s.
pub fn run_backtest_slice<S: OptimizableStrategy>(
    config: BacktestConfig,
    strat_config: &S::Config,
    bars: &[DollarBar],
) -> Result<BacktestMetrics, BacktestError> {
    run_backtest_slice_with_trades::<S>(config, strat_config, bars).map(|(metrics, _)| metrics)
}

/// [`run_backtest_slice`] that also returns the closed trades.
pub fn run_backtest_slice_with_trades<S: OptimizableStrategy>(
    config: BacktestConfig,
    strat_config: &S::Config,
    bars: &[DollarBar],
) -> Result<(BacktestMetrics, Vec<ClosedTrade>), BacktestError> {
    config.validate()?;
    let mut strategy =
        S::from_config(strat_config.clone()).map_err(|e| BacktestError::Strategy(e.to_string()))?;
    let mut engine = BacktestEngine::new(config);

    for bar in bars {
        engine.process_bar(&mut strategy, bar);
        if engine.is_halted() {
            break;
        }
    }

    let mark_price = bars.last().map(|b| b.close);
    Ok(engine.finish_with_trades(mark_price))
}

/// Evaluation outcome for a single hyperparameter candidate across Walk-Forward folds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEvaluation<C> {
    /// Evaluated parameter configuration.
    pub config: C,
    /// Mean of the per-fold in-sample Sortino ratios; a fold where it is undefined counts 0.
    pub mean_is_sortino: Decimal,
    /// Mean of the per-fold out-of-sample Sortino ratios; a fold where it is undefined counts 0.
    pub mean_oos_sortino: Decimal,
    /// Out-of-sample gross profit over gross loss, pooled over all folds; `None` without
    /// out-of-sample losses.
    #[serde(rename = "mean_oos_profit_factor")]
    pub oos_profit_factor: Option<Decimal>,
    /// Total Out-of-Sample executed trades across all folds.
    pub total_oos_trades: usize,
    /// Fewest closed trades in any in-sample or out-of-sample fold.
    #[serde(default)]
    pub min_fold_trades: usize,
    /// Whether every fold reached the minimum trade count; only eligible candidates can win.
    #[serde(default)]
    pub eligible: bool,
    /// Per-trade Sharpe ratio of all out-of-sample trades (mean over sample standard
    /// deviation of the returns on entry equity); `None` below two trades or without variance.
    #[serde(default)]
    pub oos_sharpe: Option<Decimal>,
    /// Share of the base's performance its grid neighbours keep [0 .. 1] (see
    /// [`plateau_stability`]).
    pub stability_score: Decimal,
    /// `max(0, mean_oos_sortino) * stability_score` for an eligible candidate whose mean
    /// in-sample Sortino is positive; 0 otherwise.
    pub fitness: Decimal,
    /// Deflated Sharpe Ratio p-value (Bailey and Lopez de Prado, 2014) of `oos_sharpe` given
    /// every trial of the sweep; 1 when it cannot be computed.
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

/// Evaluates parameter stability on `bars` (a training slice) by re-running the grid
/// neighbours of `base` (see `HyperparameterSpace::perturb_neighbors`).
///
/// `base_metrics` are the base's metrics on the same bars. A neighbour that fails, trades
/// fewer than `min_trades` times or has no defined Sortino counts as 0 (see
/// [`plateau_stability`]).
pub fn calculate_parameter_stability<S: OptimizableStrategy>(
    space: &S::Space,
    base: &S::Config,
    base_metrics: &BacktestMetrics,
    bars: &[DollarBar],
    engine_config: BacktestConfig,
    min_trades: usize,
) -> Decimal {
    let measured = |m: &BacktestMetrics| {
        if m.total_trades >= min_trades {
            m.sortino_ratio
        } else {
            None
        }
    };
    let base_sortino = measured(base_metrics);
    if !base_sortino.is_some_and(|s| s > Decimal::ZERO) {
        return Decimal::ZERO;
    }
    let neighbours: Vec<Option<Decimal>> = space
        .perturb_neighbors(base)
        .iter()
        .map(|neighbour| {
            run_backtest_slice::<S>(engine_config, neighbour, bars)
                .ok()
                .and_then(|m| measured(&m))
        })
        .collect();
    plateau_stability(base_sortino, &neighbours)
}

/// Share of the base Sortino its neighbours keep: `mean(max(0, s_i)) / s_base`, clamped to
/// [0, 1], with an undefined neighbour counted as 0.
///
/// 1 means every one-step change of a parameter performs at least as well (a plateau); 0.5
/// means a step loses half the edge on average (a peak). 0 when the base itself is not
/// positive. With no neighbours (a single-value grid) there is nothing to compare and it is 1.
pub fn plateau_stability(base: Option<Decimal>, neighbours: &[Option<Decimal>]) -> Decimal {
    let Some(base) = base.filter(|b| *b > Decimal::ZERO) else {
        return Decimal::ZERO;
    };
    if neighbours.is_empty() {
        return Decimal::ONE;
    }
    let kept = neighbours
        .iter()
        .map(|s| s.unwrap_or(Decimal::ZERO).max(Decimal::ZERO))
        .sum::<Decimal>()
        / Decimal::from(neighbours.len());
    (kept / base).clamp(Decimal::ZERO, Decimal::ONE)
}

/// Mean, sample standard deviation, skewness and kurtosis (not excess) of a return series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReturnMoments {
    pub count: usize,
    pub mean: f64,
    pub std_dev: f64,
    pub skewness: f64,
    pub kurtosis: f64,
}

impl ReturnMoments {
    /// Moments of `returns`; `None` below two returns or without variance.
    pub fn of(returns: &[Decimal]) -> Option<Self> {
        let values: Vec<f64> = returns.iter().filter_map(|r| r.to_f64()).collect();
        let count = values.len();
        if count < 2 {
            return None;
        }
        let n = count as f64;
        let mean = values.iter().sum::<f64>() / n;
        let central = |power: i32| values.iter().map(|v| (v - mean).powi(power)).sum::<f64>() / n;
        let m2 = central(2);
        if m2 <= 0.0 {
            return None;
        }
        Some(Self {
            count,
            mean,
            std_dev: (m2 * n / (n - 1.0)).sqrt(),
            skewness: central(3) / m2.powf(1.5),
            kurtosis: central(4) / (m2 * m2),
        })
    }

    /// Per-trade Sharpe ratio: mean over sample standard deviation.
    pub fn sharpe(&self) -> f64 {
        self.mean / self.std_dev
    }
}

/// Expected maximum Sharpe ratio of `trials` unskilled strategies whose Sharpe ratios have
/// variance `sharpe_variance` (Bailey and Lopez de Prado, 2014, eq. for SR0).
pub fn expected_max_sharpe(trials: usize, sharpe_variance: f64) -> f64 {
    if trials <= 1 || sharpe_variance <= 0.0 {
        return 0.0;
    }
    const EULER_MASCHERONI: f64 = 0.577_215_664_901_532_9;
    let n = trials as f64;
    sharpe_variance.sqrt()
        * ((1.0 - EULER_MASCHERONI) * inverse_normal_cdf(1.0 - 1.0 / n)
            + EULER_MASCHERONI * inverse_normal_cdf(1.0 - 1.0 / (n * std::f64::consts::E)))
}

/// Deflated Sharpe Ratio p-value: the probability that a strategy with no skill, selected as
/// the best of `trials` (Sharpe variance `sharpe_variance` across them), shows the observed
/// Sharpe ratio or a higher one over `moments.count` returns.
///
/// `p = 1 - Phi((SR - SR0) sqrt(T - 1) / sqrt(1 - g3 SR + (g4 - 1) / 4 SR^2))`, with SR the
/// per-trade Sharpe, SR0 [`expected_max_sharpe`], g3 the skewness and g4 the kurtosis of the
/// returns. Returns 1 when the statistic is undefined.
pub fn deflated_sharpe_p_value(
    moments: &ReturnMoments,
    trials: usize,
    sharpe_variance: f64,
) -> Decimal {
    let sharpe = moments.sharpe();
    let sr0 = expected_max_sharpe(trials, sharpe_variance);
    let denominator_sq =
        1.0 - moments.skewness * sharpe + (moments.kurtosis - 1.0) / 4.0 * sharpe * sharpe;
    if !sharpe.is_finite() || denominator_sq <= 0.0 || moments.count < 2 {
        return Decimal::ONE;
    }
    let z = (sharpe - sr0) * ((moments.count - 1) as f64).sqrt() / denominator_sq.sqrt();
    let p = 1.0 - normal_cdf(z);
    Decimal::from_f64_retain(p.clamp(0.0, 1.0))
        .map(|p| p.round_dp(4))
        .unwrap_or(Decimal::ONE)
}

fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Abramowitz and Stegun 7.1.26 error function (absolute error below 1.5e-7).
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

/// Acklam's rational approximation of the standard normal quantile (relative error 1.2e-9).
fn inverse_normal_cdf(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    const LOW: f64 = 0.02425;
    if p < LOW {
        tail((-2.0 * p.ln()).sqrt())
    } else if p > 1.0 - LOW {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    }
}

/// High-throughput parallel Walk-Forward optimizer powered by `Rayon`.
pub struct WalkForwardOptimizer<S: OptimizableStrategy> {
    engine_config: BacktestConfig,
    parameter_space: S::Space,
    min_trades_per_fold: usize,
    _marker: std::marker::PhantomData<fn() -> S>,
}

impl<S: OptimizableStrategy> WalkForwardOptimizer<S> {
    /// Creates a new optimizer instance with the specified parameter space.
    pub fn new(engine_config: BacktestConfig, parameter_space: S::Space) -> Self {
        Self {
            engine_config,
            parameter_space,
            min_trades_per_fold: DEFAULT_MIN_TRADES_PER_FOLD,
            _marker: std::marker::PhantomData,
        }
    }

    /// Creates a new optimizer instance using the strategy's default parameter space.
    pub fn with_default_space(engine_config: BacktestConfig) -> Self {
        Self::new(engine_config, S::default_space())
    }

    /// Sets the closed trades a candidate needs in every fold to be eligible.
    pub fn with_min_trades_per_fold(mut self, min_trades: usize) -> Self {
        self.min_trades_per_fold = min_trades;
        self
    }

    /// Evaluates candidate configurations in parallel across all CPU cores with `Rayon`.
    ///
    /// The result is ranked: eligible candidates (see [`CandidateEvaluation::eligible`]) by
    /// descending fitness, then the ineligible ones by descending fitness.
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
        let config = self.engine_config;
        let min_trades = self.min_trades_per_fold;

        let pb = ProgressBar::new(total_candidates as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) | ETA: {eta_precise} | {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("#>-"),
        );
        pb.set_message("Evaluating HPO parameter space");

        let evaluated: Vec<(CandidateEvaluation<S::Config>, Option<ReturnMoments>)> = candidates
            .par_iter()
            .filter_map(|candidate| {
                let mut fold_metrics = Vec::with_capacity(folds.len());
                let mut oos_returns = Vec::new();

                for fold in folds.iter() {
                    let is_metrics =
                        run_backtest_slice::<S>(config, candidate, fold.train_slice(bars)).ok()?;
                    let (oos_metrics, oos_trades) = run_backtest_slice_with_trades::<S>(
                        config,
                        candidate,
                        fold.test_slice(bars),
                    )
                    .ok()?;
                    oos_returns.extend(trade_returns(config.initial_capital, &oos_trades));
                    fold_metrics.push(FoldMetrics {
                        fold_idx: fold.fold_idx,
                        in_sample: is_metrics,
                        out_of_sample: oos_metrics,
                    });
                }

                let num_folds = Decimal::from(folds.len());
                let mean_sortino = |pick: fn(&FoldMetrics) -> &BacktestMetrics| {
                    fold_metrics
                        .iter()
                        .map(|f| pick(f).sortino_ratio.unwrap_or(Decimal::ZERO))
                        .sum::<Decimal>()
                        / num_folds
                };
                let mean_is_sortino = mean_sortino(|f| &f.in_sample);
                let mean_oos_sortino = mean_sortino(|f| &f.out_of_sample);
                let (oos_gross_profit, oos_gross_loss) =
                    fold_metrics
                        .iter()
                        .fold((Decimal::ZERO, Decimal::ZERO), |(p, l), f| {
                            (
                                p + f.out_of_sample.gross_profit,
                                l + f.out_of_sample.gross_loss,
                            )
                        });
                let total_oos_trades = fold_metrics
                    .iter()
                    .map(|f| f.out_of_sample.total_trades)
                    .sum();
                let min_fold_trades = fold_metrics
                    .iter()
                    .flat_map(|f| [f.in_sample.total_trades, f.out_of_sample.total_trades])
                    .min()
                    .unwrap_or(0);
                let eligible = min_fold_trades >= min_trades;
                // Only these can win; the rest keep fitness and stability at 0.
                let rankable = eligible && mean_is_sortino > Decimal::ZERO;

                // Stability costs up to 12 extra backtests: only candidates that can win pay it.
                let stability_score = if rankable {
                    calculate_parameter_stability::<S>(
                        &self.parameter_space,
                        candidate,
                        &fold_metrics[0].in_sample,
                        folds[0].train_slice(bars),
                        config,
                        min_trades,
                    )
                } else {
                    Decimal::ZERO
                };
                let fitness = if rankable {
                    mean_oos_sortino.max(Decimal::ZERO) * stability_score
                } else {
                    Decimal::ZERO
                };
                let moments = ReturnMoments::of(&oos_returns);

                pb.inc(1);

                Some((
                    CandidateEvaluation {
                        config: candidate.clone(),
                        mean_is_sortino,
                        mean_oos_sortino,
                        oos_profit_factor: (oos_gross_loss > Decimal::ZERO)
                            .then(|| oos_gross_profit / oos_gross_loss),
                        total_oos_trades,
                        min_fold_trades,
                        eligible,
                        oos_sharpe: moments
                            .and_then(|m| Decimal::from_f64_retain(m.sharpe()))
                            .map(|s| s.round_dp(6)),
                        stability_score,
                        fitness,
                        dsr_p_value: Decimal::ONE,
                        is_statistically_significant: false,
                        fold_metrics,
                    },
                    moments,
                ))
            })
            .collect();

        pb.finish_with_message("HPO parameter evaluation complete");

        let mut evaluations = assign_deflated_p_values(evaluated, total_candidates);

        // Ties in fitness (e.g. every eligible candidate at 0 when none is profitable) fall back
        // to the out-of-sample Sortino, so the first one is still the least bad.
        evaluations.sort_by(|a, b| {
            b.eligible
                .cmp(&a.eligible)
                .then(b.fitness.cmp(&a.fitness))
                .then(b.mean_oos_sortino.cmp(&a.mean_oos_sortino))
        });

        Ok(evaluations)
    }
}

/// Sets each eligible trial's Deflated Sharpe p-value against the Sharpe variance of the
/// eligible trials, with `trials` (every candidate tried) as the number of trials.
///
/// An ineligible trial keeps p = 1 and stays out of the variance: its Sharpe ratio comes from
/// too few returns to be an estimate (two near-equal returns give a Sharpe of 20), and one such
/// outlier would raise the expected maximum enough to deflate every real trial to p = 1.
pub fn assign_deflated_p_values<C>(
    evaluated: Vec<(CandidateEvaluation<C>, Option<ReturnMoments>)>,
    trials: usize,
) -> Vec<CandidateEvaluation<C>> {
    let sharpes: Vec<f64> = evaluated
        .iter()
        .filter(|(eval, _)| eval.eligible)
        .filter_map(|(_, m)| m.map(|m| m.sharpe()))
        .collect();
    let sharpe_variance = population_variance(&sharpes);
    evaluated
        .into_iter()
        .map(|(mut eval, moments)| {
            eval.dsr_p_value = Decimal::ONE;
            eval.is_statistically_significant = false;
            if let Some(moments) = moments.filter(|_| eval.eligible) {
                eval.dsr_p_value = deflated_sharpe_p_value(&moments, trials, sharpe_variance);
                eval.is_statistically_significant = eval.dsr_p_value < DSR_SIGNIFICANCE;
            }
            eval
        })
        .collect()
}

fn population_variance(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk_forward::WalkForwardConfig;
    use crate::walk_forward::WalkForwardSplitter;
    use strategies::{DollarBarsCusumSpace, DollarBarsCusumStrategy};

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
                buy_volume: dec!(5.0),
                sell_volume: dec!(5.0),
                buy_dollar_volume: dec!(200000.0),
                sell_dollar_volume: dec!(200000.0),
                trade_count: 50,
            });
        }
        bars
    }

    fn moments(count: usize, mean: f64, std_dev: f64) -> ReturnMoments {
        ReturnMoments {
            count,
            mean,
            std_dev,
            skewness: 0.0,
            kurtosis: 3.0,
        }
    }

    #[test]
    fn a_single_trial_is_tested_against_zero_skill() {
        // SR 0.1 over 101 normal returns: z = 0.1 * 10 / sqrt(1 + 0.5 * 0.01) = 0.9975.
        let p = deflated_sharpe_p_value(&moments(101, 0.01, 0.1), 1, 0.0);

        assert!((p - dec!(0.1593)).abs() <= dec!(0.0002), "p = {p}");
    }

    #[test]
    fn the_best_of_many_trials_must_beat_the_expected_maximum_of_luck() {
        // sqrt(0.01) * (0.4228 * PhiInv(1 - 1/120) + 0.5772 * PhiInv(1 - 1/(120 e))).
        let sr0 = expected_max_sharpe(120, 0.01);
        assert!((sr0 - 0.2594).abs() < 0.001, "SR0 = {sr0}");

        let observed = moments(101, 0.02, 0.1); // SR 0.2, below SR0
        assert!(deflated_sharpe_p_value(&observed, 120, 0.01) > dec!(0.5));
        assert!(deflated_sharpe_p_value(&observed, 1, 0.01) < dec!(0.05));
    }

    #[test]
    fn more_returns_make_the_same_sharpe_more_significant() {
        let short = deflated_sharpe_p_value(&moments(31, 0.03, 0.1), 120, 0.001);
        let long = deflated_sharpe_p_value(&moments(301, 0.03, 0.1), 120, 0.001);

        assert!(long < short, "{long} !< {short}");
    }

    #[test]
    fn return_moments_need_two_varying_returns() {
        assert_eq!(ReturnMoments::of(&[dec!(0.01)]), None);
        assert_eq!(ReturnMoments::of(&[dec!(0.01), dec!(0.01)]), None);

        let m = ReturnMoments::of(&[dec!(0.01), dec!(-0.01), dec!(0.03)]).unwrap();
        assert!((m.mean - 0.01).abs() < 1e-12);
        assert!((m.std_dev - 0.02).abs() < 1e-12); // sample standard deviation
        assert!((m.sharpe() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn stability_is_the_share_of_the_edge_the_neighbours_keep() {
        let neighbours = [Some(dec!(0.2)), Some(dec!(0.1)), None, Some(dec!(-0.1))];

        // (0.2 + 0.1 + 0 + 0) / 4 = 0.075 of a 0.2 base.
        assert_eq!(plateau_stability(Some(dec!(0.2)), &neighbours), dec!(0.375));
        assert_eq!(
            plateau_stability(Some(dec!(0.1)), &[Some(dec!(0.3))]),
            Decimal::ONE
        );
    }

    #[test]
    fn stability_is_zero_without_a_positive_base_and_one_without_neighbours() {
        assert_eq!(plateau_stability(None, &[Some(dec!(0.3))]), Decimal::ZERO);
        assert_eq!(
            plateau_stability(Some(dec!(-0.1)), &[Some(dec!(0.3))]),
            Decimal::ZERO
        );
        assert_eq!(plateau_stability(Some(dec!(0.1)), &[]), Decimal::ONE);
    }

    fn mock_sweep(min_trades: usize) -> Vec<CandidateEvaluation<MockConfig>> {
        let bars = generate_synthetic_bars(400);
        let wf_config = WalkForwardConfig {
            num_folds: 2,
            train_ratio: 0.70,
            embargo_bars: 5,
        };
        let folds = WalkForwardSplitter::generate_rolling_folds(bars.len(), &wf_config).unwrap();
        let engine_config = BacktestConfig {
            initial_capital: dec!(10000.0),
            risk_per_trade_pct: dec!(0.01),
            max_daily_drawdown_pct: dec!(0.5),
            // One unit at 40_000 per signal: caps above it so no signal is rejected.
            max_order_notional: Some(dec!(100_000)),
            max_position_notional: Some(dec!(100_000)),
            ..BacktestConfig::default()
        };
        WalkForwardOptimizer::<MockStrategy>::new(
            engine_config,
            MockSpace {
                windows: vec![2, 6, 200],
                thresholds: vec![dec!(1.0)],
            },
        )
        .with_min_trades_per_fold(min_trades)
        .run_optimization(&bars, &folds, 10)
        .unwrap()
    }

    #[test]
    fn a_candidate_below_the_minimum_trades_in_any_fold_is_ineligible_and_ranked_last() {
        let results = mock_sweep(5);

        assert!(
            results.iter().any(|e| e.eligible),
            "the busy windows trade enough"
        );
        assert!(
            results.iter().any(|e| !e.eligible),
            "window 200 trades too little"
        );
        for eval in &results {
            let fewest = eval
                .fold_metrics
                .iter()
                .flat_map(|f| [f.in_sample.total_trades, f.out_of_sample.total_trades])
                .min()
                .unwrap();
            assert_eq!(eval.min_fold_trades, fewest);
            assert_eq!(eval.eligible, fewest >= 5);
            if !eval.eligible {
                assert_eq!(eval.fitness, Decimal::ZERO);
                assert_eq!(eval.stability_score, Decimal::ZERO);
            }
        }
        let first_ineligible = results.iter().position(|e| !e.eligible).unwrap();
        assert!(results[first_ineligible..].iter().all(|e| !e.eligible));
    }

    #[test]
    fn fitness_ranks_by_the_out_of_sample_sortino_of_candidates_that_also_win_in_sample() {
        for eval in mock_sweep(1) {
            let expected = if eval.eligible && eval.mean_is_sortino > Decimal::ZERO {
                eval.mean_oos_sortino.max(Decimal::ZERO) * eval.stability_score
            } else {
                Decimal::ZERO
            };
            assert_eq!(eval.fitness, expected, "{}", eval.config);
        }
    }

    #[test]
    fn every_trial_with_out_of_sample_returns_gets_a_deflated_p_value() {
        for eval in mock_sweep(1) {
            assert!(eval.dsr_p_value >= Decimal::ZERO && eval.dsr_p_value <= Decimal::ONE);
            assert_eq!(
                eval.is_statistically_significant,
                eval.dsr_p_value < dec!(0.05)
            );
            if eval.oos_sharpe.is_none() {
                assert_eq!(eval.dsr_p_value, Decimal::ONE);
            }
        }
    }

    fn evaluation(eligible: bool) -> CandidateEvaluation<MockConfig> {
        CandidateEvaluation {
            config: MockConfig {
                window: 1,
                threshold: Decimal::ONE,
            },
            mean_is_sortino: Decimal::ZERO,
            mean_oos_sortino: Decimal::ZERO,
            oos_profit_factor: None,
            total_oos_trades: 0,
            min_fold_trades: 0,
            eligible,
            oos_sharpe: None,
            stability_score: Decimal::ZERO,
            fitness: Decimal::ZERO,
            dsr_p_value: Decimal::ONE,
            is_statistically_significant: false,
            fold_metrics: Vec::new(),
        }
    }

    #[test]
    fn a_sharpe_over_a_handful_of_trades_neither_gets_a_p_value_nor_deflates_the_others() {
        // Seen in BigQuery: two near-equal OOS returns give a per-trade Sharpe of 20 and p = 0,
        // and that outlier inflated the cross-trial variance so every real trial got p = 1.
        let evaluated = vec![
            (evaluation(true), Some(moments(101, 0.03, 0.1))),
            (evaluation(false), Some(moments(2, 1.0, 0.05))),
        ];

        let deflated = assign_deflated_p_values(evaluated, 2);

        assert_eq!(deflated[1].dsr_p_value, Decimal::ONE);
        assert!(!deflated[1].is_statistically_significant);
        // Only the eligible Sharpe enters V (zero), so it is tested against SR0 = 0:
        // z = 0.3 * 10 / sqrt(1 + 0.5 * 0.09) = 2.93, p = 0.0017.
        assert!(
            deflated[0].dsr_p_value < dec!(0.01),
            "{}",
            deflated[0].dsr_p_value
        );
        assert!(deflated[0].is_statistically_significant);
    }

    #[test]
    fn eligible_trials_with_equal_fitness_rank_by_out_of_sample_sortino() {
        let results = mock_sweep(1);

        for pair in results.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if a.eligible == b.eligible && a.fitness == b.fitness {
                assert!(
                    a.mean_oos_sortino >= b.mean_oos_sortino,
                    "{} ({}) ranked above {} ({})",
                    a.config,
                    a.mean_oos_sortino,
                    b.config,
                    b.mean_oos_sortino
                );
            }
        }
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
            ..BacktestConfig::default()
        };

        let param_space = DollarBarsCusumSpace {
            rolling_window_lens: vec![5, 10],
            cusum_vol_multipliers: vec![dec!(1.0), dec!(1.5)],
            z_entry_thresholds: vec![dec!(1.0), dec!(1.5)],
            z_stop_thresholds: vec![dec!(2.5)],
            time_barrier_bars: vec![5],
            ..DollarBarsCusumSpace::default()
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
            ..BacktestConfig::default()
        };

        let optimizer = WalkForwardOptimizer::<MockStrategy>::with_default_space(engine_config);
        let results = optimizer.run_optimization(&bars, &folds, 6).unwrap();

        assert!(!results.is_empty());
        for i in 1..results.len() {
            assert!(results[i - 1].fitness >= results[i].fitness);
        }
    }
}
