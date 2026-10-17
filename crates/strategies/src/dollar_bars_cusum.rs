use domain::{CusumEvent, CusumFilter, DollarBar, DomainError, OrderIntent, RollingZScore, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::{HyperparameterSpace, MarketType, OptimizableStrategy, Strategy};

/// Hyperparameters governing the Dollar Bars + CUSUM + Z-Score Strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DollarBarsCusumConfig {
    /// Window lookback length for rolling mean and standard deviation.
    pub rolling_window_len: usize,
    /// Multiplier on rolling standard deviation to calibrate dynamic CUSUM threshold h.
    pub cusum_vol_multiplier: Decimal,
    /// Z-Score threshold for exhaustion entry trigger (e.g. 2.0).
    pub z_entry_threshold: Decimal,
    /// Z-Score threshold for statistical stop-loss invalidation barrier (e.g. 3.5).
    pub z_stop_threshold: Decimal,
    /// Maximum bar holding duration for horizontal barrier exit.
    pub time_barrier_bars: usize,
    /// Minimum required Take Profit distance as a percentage of entry price (e.g. 0.0025 = 0.25%).
    /// Filters out trades where target is too compressed to clear exchange friction.
    #[serde(default = "default_min_take_profit_pct")]
    pub min_take_profit_pct: Decimal,
}

pub fn default_min_take_profit_pct() -> Decimal {
    dec!(0.0025)
}

impl Default for DollarBarsCusumConfig {
    fn default() -> Self {
        Self {
            rolling_window_len: 20,
            cusum_vol_multiplier: dec!(2.0),
            z_entry_threshold: dec!(2.0),
            z_stop_threshold: dec!(3.5),
            time_barrier_bars: 15,
            min_take_profit_pct: default_min_take_profit_pct(),
        }
    }
}

impl std::fmt::Display for DollarBarsCusumConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "W:{}|V:{}|Ze:{}|Zs:{}|Tb:{}|MinTp:{}",
            self.rolling_window_len,
            self.cusum_vol_multiplier,
            self.z_entry_threshold,
            self.z_stop_threshold,
            self.time_barrier_bars,
            self.min_take_profit_pct
        )
    }
}

/// Parameter search space definition for `DollarBarsCusumConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DollarBarsCusumSpace {
    pub rolling_window_lens: Vec<usize>,
    pub cusum_vol_multipliers: Vec<Decimal>,
    pub z_entry_thresholds: Vec<Decimal>,
    pub z_stop_thresholds: Vec<Decimal>,
    pub time_barrier_bars: Vec<usize>,
    /// Minimum take-profit distance as a fraction of price. It is the lever against fees: the
    /// round trip costs about 0.1% at taker rates, so a floor close to that leaves no edge.
    /// Files written before this field existed keep the single legacy value.
    #[serde(default = "default_min_take_profit_pcts")]
    pub min_take_profit_pcts: Vec<Decimal>,
}

fn default_min_take_profit_pcts() -> Vec<Decimal> {
    vec![default_min_take_profit_pct()]
}

impl Default for DollarBarsCusumSpace {
    fn default() -> Self {
        Self {
            rolling_window_lens: vec![14, 18, 22, 26, 30, 35, 40],
            cusum_vol_multipliers: vec![
                dec!(0.8),
                dec!(1.0),
                dec!(1.2),
                dec!(1.5),
                dec!(1.8),
                dec!(2.0),
                dec!(2.4),
            ],
            z_entry_thresholds: vec![
                dec!(1.5),
                dec!(1.7),
                dec!(1.9),
                dec!(2.1),
                dec!(2.3),
                dec!(2.6),
                dec!(3.0),
            ],
            z_stop_thresholds: vec![
                dec!(2.5),
                dec!(3.0),
                dec!(3.5),
                dec!(4.0),
                dec!(4.5),
                dec!(5.0),
            ],
            // Intraday holding horizons (about 12 min to 4 h at an average 7 s per $1M bar; a
            // quiet market stretches them): the wider take-profit floors need hours to be reached.
            time_barrier_bars: vec![100, 300, 1000, 2000],
            min_take_profit_pcts: vec![
                dec!(0.0025),
                dec!(0.0035),
                dec!(0.005),
                dec!(0.007),
                dec!(0.01),
            ],
        }
    }
}

impl HyperparameterSpace for DollarBarsCusumSpace {
    type Config = DollarBarsCusumConfig;

    fn generate_candidates(&self, max_candidates: usize) -> Vec<DollarBarsCusumConfig> {
        let mut configs = Vec::new();

        for &win in &self.rolling_window_lens {
            for &vol in &self.cusum_vol_multipliers {
                for &z_entry in &self.z_entry_thresholds {
                    for &z_stop in &self.z_stop_thresholds {
                        for &tb in &self.time_barrier_bars {
                            for &min_tp in &self.min_take_profit_pcts {
                                configs.push(DollarBarsCusumConfig {
                                    rolling_window_len: win,
                                    cusum_vol_multiplier: vol,
                                    z_entry_threshold: z_entry,
                                    z_stop_threshold: z_stop,
                                    time_barrier_bars: tb,
                                    min_take_profit_pct: min_tp,
                                });
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

    /// One step down and one step up the grid for each of the six parameters, the others
    /// held: up to 12 neighbours. A value that is not on the grid steps to the nearest grid
    /// values around it; at the edge of the grid only the inward step exists.
    fn perturb_neighbors(&self, base: &DollarBarsCusumConfig) -> Vec<DollarBarsCusumConfig> {
        let mut neighbors = Vec::with_capacity(12);
        let windows = adjacent_grid_values(&self.rolling_window_lens, base.rolling_window_len);
        let vols = adjacent_grid_values(&self.cusum_vol_multipliers, base.cusum_vol_multiplier);
        let entries = adjacent_grid_values(&self.z_entry_thresholds, base.z_entry_threshold);
        let stops = adjacent_grid_values(&self.z_stop_thresholds, base.z_stop_threshold);
        let barriers = adjacent_grid_values(&self.time_barrier_bars, base.time_barrier_bars);
        let floors = adjacent_grid_values(&self.min_take_profit_pcts, base.min_take_profit_pct);

        for value in [windows.0, windows.1].into_iter().flatten() {
            neighbors.push(DollarBarsCusumConfig {
                rolling_window_len: value,
                ..base.clone()
            });
        }
        for value in [vols.0, vols.1].into_iter().flatten() {
            neighbors.push(DollarBarsCusumConfig {
                cusum_vol_multiplier: value,
                ..base.clone()
            });
        }
        for value in [entries.0, entries.1].into_iter().flatten() {
            neighbors.push(DollarBarsCusumConfig {
                z_entry_threshold: value,
                ..base.clone()
            });
        }
        for value in [stops.0, stops.1].into_iter().flatten() {
            neighbors.push(DollarBarsCusumConfig {
                z_stop_threshold: value,
                ..base.clone()
            });
        }
        for value in [barriers.0, barriers.1].into_iter().flatten() {
            neighbors.push(DollarBarsCusumConfig {
                time_barrier_bars: value,
                ..base.clone()
            });
        }
        for value in [floors.0, floors.1].into_iter().flatten() {
            neighbors.push(DollarBarsCusumConfig {
                min_take_profit_pct: value,
                ..base.clone()
            });
        }
        neighbors
    }
}

/// The largest grid value below `value` and the smallest above it.
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

/// Concrete implementation of the AFML information-driven mean-reversion strategy.
///
/// Samples events using a dynamic CUSUM filter and evaluates statistical deviation
/// against a rolling Z-Score. Emits `OrderIntent` with Triple Barrier exit parameters.
#[derive(Debug, Clone)]
pub struct DollarBarsCusumStrategy {
    name: String,
    config: DollarBarsCusumConfig,
    rolling_stats: RollingZScore,
    cusum_filter: CusumFilter,
}

impl DollarBarsCusumStrategy {
    /// Initializes strategy with given configuration.
    pub fn new(config: DollarBarsCusumConfig) -> Result<Self, DomainError> {
        let rolling_stats = RollingZScore::new(config.rolling_window_len)?;
        let cusum_filter = CusumFilter::new();

        Ok(Self {
            name: "DollarBarsCusum_v1".to_string(),
            config,
            rolling_stats,
            cusum_filter,
        })
    }

    /// Access configured hyperparameters.
    pub fn config(&self) -> &DollarBarsCusumConfig {
        &self.config
    }
}

impl Strategy for DollarBarsCusumStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn reset(&mut self) {
        if let Ok(fresh_stats) = RollingZScore::new(self.config.rolling_window_len) {
            self.rolling_stats = fresh_stats;
        }
        self.cusum_filter = CusumFilter::new();
    }

    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
        // 1. Update rolling statistics
        let z_res = match self.rolling_stats.update(bar.close) {
            Some(res) => res,
            None => {
                // Maintain baseline price in CUSUM filter during warmup
                let _ = self.cusum_filter.update(bar.close, dec!(1.0));
                return None;
            }
        };

        // 2. Volatility check: if standard deviation is zero, market is static
        if z_res.std_dev <= Decimal::ZERO || z_res.mean <= Decimal::ZERO {
            let _ = self.cusum_filter.update(bar.close, dec!(1.0));
            return None;
        }

        // 3. Dynamic CUSUM threshold in return space: h = mult * (sigma / mu)
        let vol_return = z_res.std_dev / z_res.mean;
        let h = self.config.cusum_vol_multiplier * vol_return;
        if h <= Decimal::ZERO {
            let _ = self.cusum_filter.update(bar.close, dec!(1.0));
            return None;
        }

        // 4. Update CUSUM filter
        let cusum_event = match self.cusum_filter.update(bar.close, h) {
            Ok(Some(event)) => event,
            _ => return None,
        };

        // 5. Evaluate Mean Reversion hypothesis
        match cusum_event {
            // Negative shift + Oversold Z-Score -> GO LONG (Expect bounce to mean)
            CusumEvent::NegativeShift { .. } if z_res.z_score <= -self.config.z_entry_threshold => {
                let take_profit = z_res.mean;
                let tp_dist = (take_profit - bar.close).abs();
                let min_tp_dist = bar.close * self.config.min_take_profit_pct;
                if tp_dist < min_tp_dist {
                    return None; // Target run distance too small to overcome exchange friction
                }

                let stop_loss = z_res.mean - self.config.z_stop_threshold * z_res.std_dev;
                if stop_loss <= Decimal::ZERO || stop_loss >= bar.close {
                    // The move already crossed the stop barrier (|z| >= z_stop), or the
                    // barrier is below zero: there is no stop that bounds this trade.
                    return None;
                }

                OrderIntent::new(
                    bar.end_time,
                    Side::Buy,
                    bar.close,
                    stop_loss,
                    take_profit,
                    self.config.time_barrier_bars,
                )
                .ok()
            }

            // Positive shift + Overbought Z-Score -> GO SHORT (Expect pullback to mean)
            CusumEvent::PositiveShift { .. } if z_res.z_score >= self.config.z_entry_threshold => {
                let take_profit = z_res.mean;
                let tp_dist = (take_profit - bar.close).abs();
                let min_tp_dist = bar.close * self.config.min_take_profit_pct;
                if tp_dist < min_tp_dist {
                    return None; // Target run distance too small to overcome exchange friction
                }

                let stop_loss = z_res.mean + self.config.z_stop_threshold * z_res.std_dev;
                if stop_loss <= bar.close {
                    // The move already crossed the stop barrier (|z| >= z_stop).
                    return None;
                }

                OrderIntent::new(
                    bar.end_time,
                    Side::Sell,
                    bar.close,
                    stop_loss,
                    take_profit,
                    self.config.time_barrier_bars,
                )
                .ok()
            }

            _ => None,
        }
    }
}

impl OptimizableStrategy for DollarBarsCusumStrategy {
    type Config = DollarBarsCusumConfig;
    type Space = DollarBarsCusumSpace;

    fn id() -> &'static str {
        "dollar_bars_cusum"
    }

    fn from_config(config: Self::Config) -> Result<Self, domain::DomainError> {
        Self::new(config)
    }

    fn default_space() -> Self::Space {
        DollarBarsCusumSpace::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_bar(timestamp: i64, price: Decimal) -> DollarBar {
        DollarBar {
            start_time: timestamp - 100,
            end_time: timestamp,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: dec!(1.0),
            dollar_volume: price,
            trade_count: 10,
            buy_volume: dec!(0.5),
            sell_volume: dec!(0.5),
            buy_dollar_volume: price * dec!(0.5),
            sell_dollar_volume: price * dec!(0.5),
        }
    }

    #[test]
    fn default_space_searches_intraday_holding_horizons() {
        let space = DollarBarsCusumSpace::default();

        // At about 7 s per $1M bar (BTCUSDT 2026-01) these are 12 min, 35 min, 2 h and 4 h.
        assert_eq!(space.time_barrier_bars, vec![100, 300, 1000, 2000]);
        let candidates = space.generate_candidates(usize::MAX);
        assert_eq!(candidates.len(), 7 * 7 * 7 * 6 * 4 * 5);
        for horizon in &space.time_barrier_bars {
            assert!(candidates.iter().any(|c| c.time_barrier_bars == *horizon));
        }
    }

    #[test]
    fn default_space_searches_the_take_profit_floor_that_must_clear_the_round_trip_fees() {
        let space = DollarBarsCusumSpace::default();

        assert_eq!(
            space.min_take_profit_pcts,
            vec![
                dec!(0.0025),
                dec!(0.0035),
                dec!(0.005),
                dec!(0.007),
                dec!(0.01)
            ]
        );
        let candidates = space.generate_candidates(usize::MAX);
        for floor in &space.min_take_profit_pcts {
            assert!(candidates.iter().any(|c| c.min_take_profit_pct == *floor));
        }
        assert_eq!(space.z_stop_thresholds.last(), Some(&dec!(5.0)));
    }

    #[test]
    fn a_legacy_space_file_without_the_take_profit_floor_keeps_the_old_single_value() {
        let legacy = r#"{
            "rolling_window_lens": [14],
            "cusum_vol_multipliers": ["1.0"],
            "z_entry_thresholds": ["2.0"],
            "z_stop_thresholds": ["3.0"],
            "time_barrier_bars": [100]
        }"#;

        let space: DollarBarsCusumSpace = serde_json::from_str(legacy).expect("legacy space");

        assert_eq!(
            space.min_take_profit_pcts,
            vec![default_min_take_profit_pct()]
        );
        let candidates = space.generate_candidates(usize::MAX);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].min_take_profit_pct,
            default_min_take_profit_pct()
        );
    }

    fn grid_config(
        win: usize,
        vol: Decimal,
        ze: Decimal,
        zs: Decimal,
        tb: usize,
        tp: Decimal,
    ) -> DollarBarsCusumConfig {
        DollarBarsCusumConfig {
            rolling_window_len: win,
            cusum_vol_multiplier: vol,
            z_entry_threshold: ze,
            z_stop_threshold: zs,
            time_barrier_bars: tb,
            min_take_profit_pct: tp,
        }
    }

    #[test]
    fn neighbors_step_every_parameter_to_its_adjacent_grid_values() {
        let space = DollarBarsCusumSpace::default();
        let base = grid_config(22, dec!(1.2), dec!(1.9), dec!(3.5), 300, dec!(0.005));

        let neighbors = space.perturb_neighbors(&base);

        let expected = vec![
            grid_config(18, dec!(1.2), dec!(1.9), dec!(3.5), 300, dec!(0.005)),
            grid_config(26, dec!(1.2), dec!(1.9), dec!(3.5), 300, dec!(0.005)),
            grid_config(22, dec!(1.0), dec!(1.9), dec!(3.5), 300, dec!(0.005)),
            grid_config(22, dec!(1.5), dec!(1.9), dec!(3.5), 300, dec!(0.005)),
            grid_config(22, dec!(1.2), dec!(1.7), dec!(3.5), 300, dec!(0.005)),
            grid_config(22, dec!(1.2), dec!(2.1), dec!(3.5), 300, dec!(0.005)),
            grid_config(22, dec!(1.2), dec!(1.9), dec!(3.0), 300, dec!(0.005)),
            grid_config(22, dec!(1.2), dec!(1.9), dec!(4.0), 300, dec!(0.005)),
            grid_config(22, dec!(1.2), dec!(1.9), dec!(3.5), 100, dec!(0.005)),
            grid_config(22, dec!(1.2), dec!(1.9), dec!(3.5), 1000, dec!(0.005)),
            grid_config(22, dec!(1.2), dec!(1.9), dec!(3.5), 300, dec!(0.0035)),
            grid_config(22, dec!(1.2), dec!(1.9), dec!(3.5), 300, dec!(0.007)),
        ];
        assert_eq!(neighbors, expected);
    }

    #[test]
    fn neighbors_at_the_grid_edge_only_step_inwards() {
        let space = DollarBarsCusumSpace::default();
        let base = grid_config(14, dec!(2.4), dec!(1.5), dec!(5.0), 100, dec!(0.01));

        let neighbors = space.perturb_neighbors(&base);

        assert_eq!(neighbors.len(), 6);
        assert!(neighbors.iter().all(|n| n.rolling_window_len >= 14
            && n.cusum_vol_multiplier <= dec!(2.4)
            && n.z_entry_threshold >= dec!(1.5)
            && n.z_stop_threshold <= dec!(5.0)
            && n.time_barrier_bars >= 100
            && n.min_take_profit_pct <= dec!(0.01)));
    }

    #[test]
    fn an_off_grid_value_steps_to_the_nearest_grid_values_around_it() {
        let space = DollarBarsCusumSpace::default();
        let base = DollarBarsCusumConfig::default(); // window 20 is between 18 and 22

        let windows: Vec<usize> = space
            .perturb_neighbors(&base)
            .iter()
            .filter(|n| n.rolling_window_len != base.rolling_window_len)
            .map(|n| n.rolling_window_len)
            .collect();

        assert_eq!(windows, vec![18, 22]);
    }

    /// Bars that end in a drop to 90 with z = -1.71 against a window-4 mean of 97.5.
    fn drop_to_ninety(strategy: &mut DollarBarsCusumStrategy) -> Option<OrderIntent> {
        for (i, price) in [100, 100, 100, 101, 99, 100].into_iter().enumerate() {
            strategy.on_bar(&create_test_bar(
                (i as i64 + 1) * 1000,
                Decimal::from(price),
            ));
        }
        strategy.on_bar(&create_test_bar(7000, dec!(90)))
    }

    #[test]
    fn a_signal_whose_z_score_already_crossed_the_stop_barrier_is_skipped() {
        let base = grid_config(4, dec!(1.0), dec!(1.5), dec!(3.0), 10, dec!(0.0025));
        let mut accepted = DollarBarsCusumStrategy::new(base.clone()).unwrap();
        assert!(
            drop_to_ninety(&mut accepted).is_some(),
            "|z| 1.71 < z_stop 3.0"
        );

        // With z_stop 1.6 the stop (mean - 1.6 sigma = 90.5) would sit above the long entry.
        let mut crossed = DollarBarsCusumStrategy::new(DollarBarsCusumConfig {
            z_stop_threshold: dec!(1.6),
            ..base
        })
        .unwrap();
        assert!(drop_to_ninety(&mut crossed).is_none());
    }

    #[test]
    fn warmup_period_emits_no_signals() {
        let config = DollarBarsCusumConfig {
            rolling_window_len: 5,
            ..Default::default()
        };
        let mut strategy = DollarBarsCusumStrategy::new(config).unwrap();

        for i in 1..=4 {
            let bar = create_test_bar(i * 1000, dec!(50000));
            assert!(strategy.on_bar(&bar).is_none());
        }
    }

    #[test]
    fn dollar_bars_cusum_declares_usd_m_futures_only_compatibility() {
        let strategy = DollarBarsCusumStrategy::new(DollarBarsCusumConfig::default()).unwrap();

        assert!(strategy
            .compatible_market_types()
            .contains(&crate::strategy::MarketType::UsdMPerpetual));
        assert!(!strategy
            .compatible_market_types()
            .contains(&crate::strategy::MarketType::Spot));
    }

    #[test]
    fn negative_climax_triggers_long_order_intent() {
        let config = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 10,
            min_take_profit_pct: dec!(0.0025),
        };
        let mut strategy = DollarBarsCusumStrategy::new(config).unwrap();

        // Feed baseline bars: 100, 100, 100
        strategy.on_bar(&create_test_bar(1000, dec!(100)));
        strategy.on_bar(&create_test_bar(2000, dec!(100)));
        strategy.on_bar(&create_test_bar(3000, dec!(100)));

        // Feed minor fluctuations to create non-zero sigma
        strategy.on_bar(&create_test_bar(4000, dec!(101)));
        strategy.on_bar(&create_test_bar(5000, dec!(99)));
        strategy.on_bar(&create_test_bar(6000, dec!(100)));

        // Sharp drop to 90 -> Triggers negative CUSUM event and deep negative Z-Score
        let signal = strategy.on_bar(&create_test_bar(7000, dec!(90)));
        assert!(signal.is_some());

        let intent = signal.unwrap();
        assert_eq!(intent.side, Side::Buy);
        assert_eq!(intent.price, dec!(90));
        assert!(intent.take_profit > dec!(90)); // TP is mean (above 90)
        assert!(intent.stop_loss < dec!(90)); // SL is below 90
        assert_eq!(intent.max_bars_hold, 10);
    }

    #[test]
    fn positive_climax_triggers_short_order_intent() {
        let config = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 10,
            min_take_profit_pct: dec!(0.0025),
        };
        let mut strategy = DollarBarsCusumStrategy::new(config).unwrap();

        // Establish non-zero baseline
        strategy.on_bar(&create_test_bar(1000, dec!(100)));
        strategy.on_bar(&create_test_bar(2000, dec!(101)));
        strategy.on_bar(&create_test_bar(3000, dec!(99)));
        strategy.on_bar(&create_test_bar(4000, dec!(100)));

        // Sharp pump to 112 -> Triggers positive CUSUM event and high Z-Score
        let signal = strategy.on_bar(&create_test_bar(5000, dec!(112)));
        assert!(signal.is_some());

        let intent = signal.unwrap();
        assert_eq!(intent.side, Side::Sell);
        assert_eq!(intent.price, dec!(112));
        assert!(intent.take_profit < dec!(112)); // TP is mean (below 112)
        assert!(intent.stop_loss > dec!(112)); // SL is above 112
        assert_eq!(intent.max_bars_hold, 10);
    }

    #[test]
    fn friction_filter_blocks_compressed_reversion_signals() {
        let config = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 10,
            min_take_profit_pct: dec!(0.20), // Require 20% minimum price move to mean
        };
        let mut strategy = DollarBarsCusumStrategy::new(config).unwrap();

        // Baseline: mean is around 100
        strategy.on_bar(&create_test_bar(1000, dec!(100)));
        strategy.on_bar(&create_test_bar(2000, dec!(101)));
        strategy.on_bar(&create_test_bar(3000, dec!(99)));
        strategy.on_bar(&create_test_bar(4000, dec!(100)));

        // Drop to 90 -> Mean is ~97.5, distance is ~7.5 (< 20% of 90 = 18.0)
        let signal = strategy.on_bar(&create_test_bar(5000, dec!(90)));
        assert!(
            signal.is_none(),
            "Should be filtered out by min_take_profit_pct"
        );
    }
}
