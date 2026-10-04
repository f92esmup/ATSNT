use rand::prelude::*;
use rand::rngs::StdRng;
use rayon::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::metrics::ClosedTrade;

/// Bootstrapping methodology for trade sequence resampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResampleMethod {
    /// Independent and identically distributed resampling with replacement.
    IidWithReplacement,
    /// Circular Block Bootstrap preserving local autocorrelation and streaks.
    CircularBlockBootstrap { block_size: usize },
}

/// Configuration parameters for Monte Carlo stress simulation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonteCarloConfig {
    /// Total number of synthetic equity paths to simulate (e.g. 10,000).
    pub iterations: usize,
    /// Resampling method.
    pub method: ResampleMethod,
    /// Drawdown percentage threshold that triggers an account ruin event (e.g. 0.30 = 30% loss).
    pub ruin_threshold_pct: Decimal,
    /// Seed for deterministic reproducibility (None for non-deterministic OS entropy).
    pub seed: Option<u64>,
}

impl Default for MonteCarloConfig {
    fn default() -> Self {
        Self {
            iterations: 10_000,
            method: ResampleMethod::CircularBlockBootstrap { block_size: 5 },
            ruin_threshold_pct: dec!(0.30),
            seed: Some(42),
        }
    }
}

/// Representative trajectory curve for fan-chart web visualization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanChartTrajectory {
    pub label: String,
    pub final_equity: Decimal,
    pub max_drawdown_pct: Decimal,
    pub equity_curve: Vec<Decimal>,
}

/// Aggregate statistical percentiles derived from N Monte Carlo paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonteCarloMetrics {
    pub total_simulations: usize,
    pub initial_capital: Decimal,
    pub historical_max_drawdown_pct: Decimal,
    pub p01_max_drawdown_pct: Decimal,
    pub p05_max_drawdown_pct: Decimal,
    pub p25_max_drawdown_pct: Decimal,
    pub p50_max_drawdown_pct: Decimal,
    pub p75_max_drawdown_pct: Decimal,
    pub p95_max_drawdown_pct: Decimal,
    pub p99_max_drawdown_pct: Decimal,
    pub worst_max_drawdown_pct: Decimal,
    pub probability_of_ruin_pct: Decimal,
    pub median_underwater_trades: usize,
    pub p95_underwater_trades: usize,
}

/// Complete report containing risk percentiles and fan-chart curves for web visualization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonteCarloReport {
    pub report_id: String,
    pub timestamp: i64,
    pub config: MonteCarloConfig,
    pub metrics: MonteCarloMetrics,
    pub fan_chart: Vec<FanChartTrajectory>,
}

/// Simulated path outcome
struct SimulatedPath {
    final_equity: Decimal,
    max_drawdown_pct: Decimal,
    max_underwater_trades: usize,
    hit_ruin: bool,
    equity_curve: Vec<Decimal>,
}

/// Discrete Event Monte Carlo Stress-Testing Engine.
pub struct MonteCarloSimulator {
    config: MonteCarloConfig,
}

impl MonteCarloSimulator {
    /// Creates a new simulator with the specified configuration.
    pub fn new(config: MonteCarloConfig) -> Self {
        Self { config }
    }

    /// Executes the Monte Carlo stress simulation across closed trades using `Rayon`.
    pub fn run(
        &self,
        initial_capital: Decimal,
        trades: &[ClosedTrade],
    ) -> Result<MonteCarloReport, String> {
        if trades.is_empty() {
            return Err("Cannot run Monte Carlo on empty trade history".to_string());
        }

        let num_trades = trades.len();
        let ruin_equity = initial_capital * (Decimal::ONE - self.config.ruin_threshold_pct);

        // 1. Calculate historical realized curve
        let historical_path = self.simulate_single_path(initial_capital, trades, ruin_equity);

        // 2. Simulate N paths in parallel with Rayon
        let seed_base = self.config.seed.unwrap_or(1337);
        let iterations = self.config.iterations;

        let paths: Vec<SimulatedPath> = (0..iterations)
            .into_par_iter()
            .map(|i| {
                let mut rng = StdRng::seed_from_u64(seed_base.wrapping_add(i as u64));
                let resampled_trades = match self.config.method {
                    ResampleMethod::IidWithReplacement => {
                        let mut res = Vec::with_capacity(num_trades);
                        for _ in 0..num_trades {
                            let idx = rng.gen_range(0..num_trades);
                            res.push(&trades[idx]);
                        }
                        res
                    }
                    ResampleMethod::CircularBlockBootstrap { block_size } => {
                        let mut res = Vec::with_capacity(num_trades);
                        let b_size = block_size.max(1);
                        while res.len() < num_trades {
                            let start_idx = rng.gen_range(0..num_trades);
                            for step in 0..b_size {
                                if res.len() >= num_trades {
                                    break;
                                }
                                let idx = (start_idx + step) % num_trades;
                                res.push(&trades[idx]);
                            }
                        }
                        res
                    }
                };

                let mut current_equity = initial_capital;
                let mut peak_equity = initial_capital;
                let mut max_dd_pct = Decimal::ZERO;
                let mut current_underwater = 0usize;
                let mut max_underwater = 0usize;
                let mut hit_ruin = false;
                let mut curve = Vec::with_capacity(num_trades + 1);
                curve.push(initial_capital);

                for t in resampled_trades {
                    current_equity += t.pnl_net;
                    curve.push(current_equity);

                    if current_equity <= ruin_equity {
                        hit_ruin = true;
                    }

                    if current_equity > peak_equity {
                        peak_equity = current_equity;
                        current_underwater = 0;
                    } else {
                        current_underwater += 1;
                        if current_underwater > max_underwater {
                            max_underwater = current_underwater;
                        }
                    }

                    if peak_equity > Decimal::ZERO {
                        let dd = (peak_equity - current_equity) / peak_equity;
                        if dd > max_dd_pct {
                            max_dd_pct = dd;
                        }
                    }
                }

                SimulatedPath {
                    final_equity: current_equity,
                    max_drawdown_pct: max_dd_pct,
                    max_underwater_trades: max_underwater,
                    hit_ruin,
                    equity_curve: curve,
                }
            })
            .collect();

        // 3. Compile Percentiles
        let mut dds: Vec<Decimal> = paths.iter().map(|p| p.max_drawdown_pct).collect();
        dds.sort();

        let mut underwaters: Vec<usize> = paths.iter().map(|p| p.max_underwater_trades).collect();
        underwaters.sort();

        let ruin_count = paths.iter().filter(|p| p.hit_ruin).count();
        let ruin_pct = (Decimal::from(ruin_count) / Decimal::from(iterations)) * dec!(100.0);

        let percentile = |sorted: &[Decimal], pct: f64| -> Decimal {
            let idx = ((sorted.len() as f64 - 1.0) * pct).round() as usize;
            sorted[idx.min(sorted.len() - 1)]
        };

        let metrics = MonteCarloMetrics {
            total_simulations: iterations,
            initial_capital,
            historical_max_drawdown_pct: historical_path.max_drawdown_pct,
            p01_max_drawdown_pct: percentile(&dds, 0.01),
            p05_max_drawdown_pct: percentile(&dds, 0.05),
            p25_max_drawdown_pct: percentile(&dds, 0.25),
            p50_max_drawdown_pct: percentile(&dds, 0.50),
            p75_max_drawdown_pct: percentile(&dds, 0.75),
            p95_max_drawdown_pct: percentile(&dds, 0.95),
            p99_max_drawdown_pct: percentile(&dds, 0.99),
            worst_max_drawdown_pct: *dds.last().unwrap_or(&Decimal::ZERO),
            probability_of_ruin_pct: ruin_pct,
            median_underwater_trades: underwaters[underwaters.len() / 2],
            p95_underwater_trades: underwaters[((underwaters.len() as f64 - 1.0) * 0.95) as usize],
        };

        // 4. Select Exactly 50 Fan-Chart Trajectories (Anti-Bloat Contract)
        let mut fan_chart = Vec::with_capacity(52);

        // 4.1 Realized historical path
        fan_chart.push(FanChartTrajectory {
            label: "Realized".to_string(),
            final_equity: historical_path.final_equity,
            max_drawdown_pct: historical_path.max_drawdown_pct,
            equity_curve: historical_path.equity_curve,
        });

        // 4.2 Sort paths by Max Drawdown descending for worst bounds
        let mut paths_by_dd = paths;
        paths_by_dd.sort_by_key(|a| std::cmp::Reverse(a.max_drawdown_pct));

        // 5 worst stress curves
        for (i, p) in paths_by_dd.iter().take(5).enumerate() {
            fan_chart.push(FanChartTrajectory {
                label: format!("Worst_Stress_{}", i + 1),
                final_equity: p.final_equity,
                max_drawdown_pct: p.max_drawdown_pct,
                equity_curve: p.equity_curve.clone(),
            });
        }

        // Sort by final equity descending for best ceilings
        paths_by_dd.sort_by_key(|a| std::cmp::Reverse(a.final_equity));

        // 5 best optimistic curves
        for (i, p) in paths_by_dd.iter().take(5).enumerate() {
            fan_chart.push(FanChartTrajectory {
                label: format!("Best_Ceiling_{}", i + 1),
                final_equity: p.final_equity,
                max_drawdown_pct: p.max_drawdown_pct,
                equity_curve: p.equity_curve.clone(),
            });
        }

        // 40 quantile-spaced representative curves across remaining distribution
        let step = (paths_by_dd.len() - 1) as f64 / 41.0;
        for i in 1..=40 {
            let idx = (i as f64 * step).round() as usize;
            if idx < paths_by_dd.len() {
                let p = &paths_by_dd[idx];
                fan_chart.push(FanChartTrajectory {
                    label: format!("Quantile_{:.2}", i as f64 / 40.0),
                    final_equity: p.final_equity,
                    max_drawdown_pct: p.max_drawdown_pct,
                    equity_curve: p.equity_curve.clone(),
                });
            }
        }

        let report_id = format!(
            "mc_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            iterations
        );

        Ok(MonteCarloReport {
            report_id,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            config: self.config.clone(),
            metrics,
            fan_chart,
        })
    }

    fn simulate_single_path(
        &self,
        initial_capital: Decimal,
        trades: &[ClosedTrade],
        ruin_equity: Decimal,
    ) -> SimulatedPath {
        let mut current_equity = initial_capital;
        let mut peak_equity = initial_capital;
        let mut max_dd_pct = Decimal::ZERO;
        let mut current_underwater = 0usize;
        let mut max_underwater = 0usize;
        let mut hit_ruin = false;
        let mut curve = Vec::with_capacity(trades.len() + 1);
        curve.push(initial_capital);

        for t in trades {
            current_equity += t.pnl_net;
            curve.push(current_equity);

            if current_equity <= ruin_equity {
                hit_ruin = true;
            }

            if current_equity > peak_equity {
                peak_equity = current_equity;
                current_underwater = 0;
            } else {
                current_underwater += 1;
                if current_underwater > max_underwater {
                    max_underwater = current_underwater;
                }
            }

            if peak_equity > Decimal::ZERO {
                let dd = (peak_equity - current_equity) / peak_equity;
                if dd > max_dd_pct {
                    max_dd_pct = dd;
                }
            }
        }

        SimulatedPath {
            final_equity: current_equity,
            max_drawdown_pct: max_dd_pct,
            max_underwater_trades: max_underwater,
            hit_ruin,
            equity_curve: curve,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_dummy_trades() -> Vec<ClosedTrade> {
        vec![
            ClosedTrade {
                exit_time: 1000,
                pnl_gross: dec!(150.0),
                fees_paid: dec!(2.0),
                slippage_paid: dec!(1.0),
                pnl_net: dec!(147.0),
                return_pct: dec!(0.0147),
            },
            ClosedTrade {
                exit_time: 2000,
                pnl_gross: dec!(-100.0),
                fees_paid: dec!(2.0),
                slippage_paid: dec!(1.0),
                pnl_net: dec!(-103.0),
                return_pct: dec!(-0.0103),
            },
            ClosedTrade {
                exit_time: 3000,
                pnl_gross: dec!(220.0),
                fees_paid: dec!(3.0),
                slippage_paid: dec!(1.0),
                pnl_net: dec!(216.0),
                return_pct: dec!(0.0216),
            },
            ClosedTrade {
                exit_time: 4000,
                pnl_gross: dec!(-80.0),
                fees_paid: dec!(2.0),
                slippage_paid: dec!(1.0),
                pnl_net: dec!(-83.0),
                return_pct: dec!(-0.0083),
            },
            ClosedTrade {
                exit_time: 5000,
                pnl_gross: dec!(300.0),
                fees_paid: dec!(4.0),
                slippage_paid: dec!(1.0),
                pnl_net: dec!(295.0),
                return_pct: dec!(0.0295),
            },
        ]
    }

    #[test]
    fn run_monte_carlo_simulation_iid() {
        let trades = create_dummy_trades();
        let config = MonteCarloConfig {
            iterations: 500,
            method: ResampleMethod::IidWithReplacement,
            ruin_threshold_pct: dec!(0.30),
            seed: Some(12345),
        };

        let sim = MonteCarloSimulator::new(config);
        let report = sim.run(dec!(10000.0), &trades).unwrap();

        assert_eq!(report.metrics.total_simulations, 500);
        assert!(report.metrics.p95_max_drawdown_pct >= report.metrics.p50_max_drawdown_pct);
        assert!(!report.fan_chart.is_empty());
        assert_eq!(report.fan_chart[0].label, "Realized");
    }

    #[test]
    fn run_monte_carlo_simulation_block_bootstrap() {
        let trades = create_dummy_trades();
        let config = MonteCarloConfig {
            iterations: 500,
            method: ResampleMethod::CircularBlockBootstrap { block_size: 2 },
            ruin_threshold_pct: dec!(0.30),
            seed: Some(54321),
        };

        let sim = MonteCarloSimulator::new(config);
        let report = sim.run(dec!(10000.0), &trades).unwrap();

        assert_eq!(report.metrics.total_simulations, 500);
        assert!(report.metrics.worst_max_drawdown_pct >= report.metrics.p99_max_drawdown_pct);
        assert!(report.fan_chart.len() <= 52);
    }
}
