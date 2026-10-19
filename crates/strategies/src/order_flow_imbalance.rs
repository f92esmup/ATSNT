use std::collections::VecDeque;

use domain::{DollarBar, DomainError, OrderIntent, RollingZScore, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::{
    BarPassThrough, Decision, HyperparameterSpace, MarketType, OptimizableStrategy, Strategy,
};

/// Hyperparameters governing the Order Flow Imbalance (OFI) & Volume Toxicity Strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderFlowImbalanceConfig {
    /// Window lookback length for rolling mean and standard deviation of OFI and bar range.
    pub rolling_window_len: usize,
    /// Z-Score threshold on Order Flow Imbalance to detect an institutional flow anomaly (e.g. 1.8).
    pub z_ofi_threshold: Decimal,
    /// Price efficiency threshold: |Close - Open| / (High - Low).
    /// Below this threshold is classified as Liquidity Absorption (Reversal).
    /// Above or equal is classified as Toxic Flow Breakout (Momentum).
    pub absorption_ratio_threshold: Decimal,
    /// Multiplier on rolling volatility (mean bar range) for Take Profit placement.
    pub take_profit_vol_mult: Decimal,
    /// Multiplier on rolling volatility (mean bar range) for Stop Loss buffer.
    pub stop_loss_vol_mult: Decimal,
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

impl Default for OrderFlowImbalanceConfig {
    fn default() -> Self {
        Self {
            rolling_window_len: 20,
            z_ofi_threshold: dec!(1.8),
            absorption_ratio_threshold: dec!(0.35),
            take_profit_vol_mult: dec!(2.0),
            stop_loss_vol_mult: dec!(1.5),
            time_barrier_bars: 12,
            min_take_profit_pct: default_min_take_profit_pct(),
        }
    }
}

impl std::fmt::Display for OrderFlowImbalanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "W:{}|Zofi:{}|Abs:{}|TP:{}|SL:{}|Tb:{}|MinTp:{}",
            self.rolling_window_len,
            self.z_ofi_threshold,
            self.absorption_ratio_threshold,
            self.take_profit_vol_mult,
            self.stop_loss_vol_mult,
            self.time_barrier_bars,
            self.min_take_profit_pct
        )
    }
}

/// Parameter search space definition for Walk-Forward Optimization of `OrderFlowImbalanceConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderFlowImbalanceSpace {
    pub rolling_window_lens: Vec<usize>,
    pub z_ofi_thresholds: Vec<Decimal>,
    pub absorption_ratio_thresholds: Vec<Decimal>,
    pub take_profit_vol_mults: Vec<Decimal>,
    pub stop_loss_vol_mults: Vec<Decimal>,
    pub time_barrier_bars: Vec<usize>,
}

impl Default for OrderFlowImbalanceSpace {
    fn default() -> Self {
        Self {
            rolling_window_lens: vec![15, 20, 25],
            z_ofi_thresholds: vec![dec!(1.5), dec!(1.8), dec!(2.2)],
            absorption_ratio_thresholds: vec![dec!(0.30), dec!(0.40)],
            take_profit_vol_mults: vec![dec!(1.5), dec!(2.0), dec!(2.5)],
            stop_loss_vol_mults: vec![dec!(1.0), dec!(1.5)],
            time_barrier_bars: vec![8, 12, 16],
        }
    }
}

impl HyperparameterSpace for OrderFlowImbalanceSpace {
    type Config = OrderFlowImbalanceConfig;

    fn generate_candidates(&self, max_candidates: usize) -> Vec<OrderFlowImbalanceConfig> {
        let mut configs = Vec::new();

        for &win in &self.rolling_window_lens {
            for &z_ofi in &self.z_ofi_thresholds {
                for &abs in &self.absorption_ratio_thresholds {
                    for &tp in &self.take_profit_vol_mults {
                        for &sl in &self.stop_loss_vol_mults {
                            for &tb in &self.time_barrier_bars {
                                configs.push(OrderFlowImbalanceConfig {
                                    rolling_window_len: win,
                                    z_ofi_threshold: z_ofi,
                                    absorption_ratio_threshold: abs,
                                    take_profit_vol_mult: tp,
                                    stop_loss_vol_mult: sl,
                                    time_barrier_bars: tb,
                                    min_take_profit_pct: default_min_take_profit_pct(),
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

    fn perturb_neighbors(&self, base: &OrderFlowImbalanceConfig) -> Vec<OrderFlowImbalanceConfig> {
        let mut neighbors = Vec::new();

        // 1. Perturb window size +- 2
        if base.rolling_window_len > 4 {
            let mut c = base.clone();
            c.rolling_window_len -= 2;
            neighbors.push(c);
        }
        let mut c = base.clone();
        c.rolling_window_len += 2;
        neighbors.push(c);

        // 2. Perturb Z-Score threshold +- 0.2
        if base.z_ofi_threshold > dec!(0.4) {
            let mut c = base.clone();
            c.z_ofi_threshold -= dec!(0.2);
            neighbors.push(c);
        }
        let mut c = base.clone();
        c.z_ofi_threshold += dec!(0.2);
        neighbors.push(c);

        // 3. Perturb absorption threshold +- 0.05
        if base.absorption_ratio_threshold > dec!(0.10) {
            let mut c = base.clone();
            c.absorption_ratio_threshold -= dec!(0.05);
            neighbors.push(c);
        }
        let mut c = base.clone();
        c.absorption_ratio_threshold += dec!(0.05);
        neighbors.push(c);

        neighbors
    }
}

/// Concrete implementation of the Order Flow Imbalance & Volume Toxicity Strategy.
///
/// Ingests microstructural `DollarBar` events, derives normalized OFI in [-1.0, 1.0],
/// standardizes with rolling Z-Score, and discriminates between Informed Flow Breakouts
/// and Liquidity Absorption Reversals with dynamic Triple Barrier exits.
#[derive(Debug, Clone)]
pub struct OrderFlowImbalanceStrategy {
    name: String,
    config: OrderFlowImbalanceConfig,
    rolling_ofi_stats: RollingZScore,
    rolling_ranges: VecDeque<Decimal>,
    running_range_sum: Decimal,
}

impl OrderFlowImbalanceStrategy {
    /// Initializes a new strategy with the given configuration.
    pub fn new(config: OrderFlowImbalanceConfig) -> Result<Self, DomainError> {
        let rolling_ofi_stats = RollingZScore::new(config.rolling_window_len)?;
        let window_cap = config.rolling_window_len;

        Ok(Self {
            name: "OrderFlowImbalance_v1".to_string(),
            config,
            rolling_ofi_stats,
            rolling_ranges: VecDeque::with_capacity(window_cap),
            running_range_sum: Decimal::ZERO,
        })
    }

    /// Access configured hyperparameters.
    pub fn config(&self) -> &OrderFlowImbalanceConfig {
        &self.config
    }

    fn update_rolling_range(&mut self, bar_range: Decimal) -> Decimal {
        if self.rolling_ranges.len() == self.config.rolling_window_len {
            if let Some(evicted) = self.rolling_ranges.pop_front() {
                self.running_range_sum -= evicted;
            }
        }
        self.rolling_ranges.push_back(bar_range);
        self.running_range_sum += bar_range;
        self.running_range_sum / Decimal::from(self.rolling_ranges.len())
    }
}

impl Strategy for OrderFlowImbalanceStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn reset(&mut self) {
        if let Ok(fresh) = RollingZScore::new(self.config.rolling_window_len) {
            self.rolling_ofi_stats = fresh;
        }
        self.rolling_ranges.clear();
        self.running_range_sum = Decimal::ZERO;
    }

    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
        // 1. Calculate normalized Order Flow Imbalance (OFI) in [-1.0, 1.0]
        let total_vol = bar.buy_volume + bar.sell_volume;
        let ofi = if total_vol > Decimal::ZERO {
            (bar.buy_volume - bar.sell_volume) / total_vol
        } else {
            Decimal::ZERO
        };

        // 2. Track bar range High - Low for volatility normalization
        let bar_range = (bar.high - bar.low).max(Decimal::ZERO);
        let mean_range = self.update_rolling_range(bar_range);

        // 3. Update rolling OFI statistics and obtain standardized Z-Score
        let z_res = self.rolling_ofi_stats.update(ofi)?;

        // Safety: ensure variance is strictly positive and price range is non-zero
        if z_res.std_dev <= Decimal::ZERO || mean_range <= Decimal::ZERO {
            return None;
        }

        // 4. Calculate Bar Price Efficiency: |Close - Open| / max(High - Low, epsilon)
        let price_delta = bar.close - bar.open;
        let abs_delta = price_delta.abs();
        let bar_efficiency = if bar_range > Decimal::ZERO {
            (abs_delta / bar_range).min(dec!(1.0))
        } else {
            Decimal::ZERO
        };

        let is_absorption = bar_efficiency <= self.config.absorption_ratio_threshold;
        let is_breakout = bar_efficiency > self.config.absorption_ratio_threshold;

        // Volatility buffer for barriers
        let tp_distance = mean_range * self.config.take_profit_vol_mult;
        let sl_distance = (mean_range * self.config.stop_loss_vol_mult).max(dec!(0.01));

        // Friction guard: skip signal if target run distance cannot clear round-trip friction
        let min_tp_distance = bar.close * self.config.min_take_profit_pct;
        if tp_distance < min_tp_distance {
            return None;
        }

        // 5. Evaluate order flow imbalance triggers
        if z_res.z_score >= self.config.z_ofi_threshold {
            // Extreme aggressive BUY volume pressure detected
            if is_breakout && price_delta > Decimal::ZERO {
                // REGIME A: Toxic Flow Breakout (Buyers swept the book -> Go LONG)
                let stop_loss = bar.close - sl_distance;
                let take_profit = bar.close + tp_distance;
                if stop_loss > Decimal::ZERO && take_profit > Decimal::ZERO {
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
            } else if is_absorption {
                // REGIME B: Liquidity Absorption (Aggressive buyers trapped against passive iceberg sell wall -> Go SHORT)
                let stop_loss = bar.close + sl_distance;
                let take_profit = bar.close - tp_distance;
                if stop_loss > Decimal::ZERO && take_profit > Decimal::ZERO {
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
        } else if z_res.z_score <= -self.config.z_ofi_threshold {
            // Extreme aggressive SELL volume pressure detected
            if is_breakout && price_delta < Decimal::ZERO {
                // REGIME A: Toxic Flow Breakout (Sellers swept the book -> Go SHORT)
                let stop_loss = bar.close + sl_distance;
                let take_profit = bar.close - tp_distance;
                if stop_loss > Decimal::ZERO && take_profit > Decimal::ZERO {
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
            } else if is_absorption {
                // REGIME B: Liquidity Absorption (Aggressive sellers trapped against passive iceberg buy wall -> Go LONG)
                let stop_loss = bar.close - sl_distance;
                let take_profit = bar.close + tp_distance;
                if stop_loss > Decimal::ZERO && take_profit > Decimal::ZERO {
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

        None
    }
}

impl OptimizableStrategy for OrderFlowImbalanceStrategy {
    type SharedConfig = ();
    type Config = OrderFlowImbalanceConfig;
    type Space = OrderFlowImbalanceSpace;

    fn id() -> &'static str {
        "order_flow_imbalance"
    }

    type Extractor = BarPassThrough;

    fn from_config(_shared: &(), config: Self::Config) -> Result<Self, domain::DomainError> {
        Self::new(config)
    }

    fn extractor(_shared: &()) -> Result<BarPassThrough, domain::DomainError> {
        Ok(BarPassThrough)
    }

    fn on_features(&mut self, bar: &DollarBar) -> Option<Decision> {
        self.on_bar(bar).map(Decision::Entry)
    }

    fn default_space() -> Self::Space {
        OrderFlowImbalanceSpace::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_bar(
        timestamp: i64,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
        buy_vol: Decimal,
        sell_vol: Decimal,
    ) -> DollarBar {
        let vol = buy_vol + sell_vol;
        DollarBar {
            start_time: timestamp - 100,
            end_time: timestamp,
            open,
            high,
            low,
            close,
            volume: vol,
            dollar_volume: close * vol,
            trade_count: 10,
            buy_volume: buy_vol,
            sell_volume: sell_vol,
            buy_dollar_volume: close * buy_vol,
            sell_dollar_volume: close * sell_vol,
        }
    }

    #[test]
    fn order_flow_imbalance_declares_usd_m_futures_only() {
        let strategy =
            OrderFlowImbalanceStrategy::new(OrderFlowImbalanceConfig::default()).unwrap();
        assert!(strategy
            .compatible_market_types()
            .contains(&MarketType::UsdMPerpetual));
        assert!(!strategy
            .compatible_market_types()
            .contains(&MarketType::Spot));
    }

    #[test]
    fn warmup_period_emits_no_signals() {
        let config = OrderFlowImbalanceConfig {
            rolling_window_len: 5,
            ..Default::default()
        };
        let mut strategy = OrderFlowImbalanceStrategy::new(config).unwrap();

        for i in 1..=4 {
            let bar = create_test_bar(
                i * 1000,
                dec!(50000),
                dec!(50100),
                dec!(49900),
                dec!(50050),
                dec!(1.0),
                dec!(1.0),
            );
            assert!(strategy.on_bar(&bar).is_none());
        }
    }

    #[test]
    fn aggressive_buy_breakout_triggers_long() {
        let config = OrderFlowImbalanceConfig {
            rolling_window_len: 5,
            z_ofi_threshold: dec!(1.5),
            absorption_ratio_threshold: dec!(0.35),
            ..Default::default()
        };
        let mut strategy = OrderFlowImbalanceStrategy::new(config).unwrap();

        // Feed 5 neutral bars (OFI = 0.0)
        for i in 1..=5 {
            let bar = create_test_bar(
                i * 1000,
                dec!(50000),
                dec!(50100),
                dec!(49900),
                dec!(50000),
                dec!(5.0),
                dec!(5.0),
            );
            strategy.on_bar(&bar);
        }

        // 6th bar: Massive aggressive buying (OFI = +0.8) and strong close near high (Breakout)
        let breakout_bar = create_test_bar(
            6000,
            dec!(50000),
            dec!(50500),
            dec!(49950),
            dec!(50480), // Close near High -> efficiency = 480 / 550 = 0.87 > 0.35
            dec!(9.0),
            dec!(1.0),
        );

        let intent = strategy.on_bar(&breakout_bar);
        assert!(intent.is_some());
        let intent = intent.unwrap();
        assert_eq!(intent.side, Side::Buy);
        assert!(intent.take_profit > intent.price);
        assert!(intent.stop_loss < intent.price);
    }

    #[test]
    fn aggressive_buy_absorption_triggers_short() {
        let config = OrderFlowImbalanceConfig {
            rolling_window_len: 5,
            z_ofi_threshold: dec!(1.5),
            absorption_ratio_threshold: dec!(0.35),
            ..Default::default()
        };
        let mut strategy = OrderFlowImbalanceStrategy::new(config).unwrap();

        // Feed 5 neutral bars (OFI = 0.0)
        for i in 1..=5 {
            let bar = create_test_bar(
                i * 1000,
                dec!(50000),
                dec!(50100),
                dec!(49900),
                dec!(50000),
                dec!(5.0),
                dec!(5.0),
            );
            strategy.on_bar(&bar);
        }

        // 6th bar: Massive aggressive buying (OFI = +0.8) but price closed near open with big upper wick (Absorption)
        let absorption_bar = create_test_bar(
            6000,
            dec!(50000),
            dec!(50500), // Pushed to 50500
            dec!(49950),
            dec!(50050), // But closed back down at 50050 -> efficiency = 50 / 550 = 0.09 <= 0.35
            dec!(9.0),
            dec!(1.0),
        );

        let intent = strategy.on_bar(&absorption_bar);
        assert!(intent.is_some());
        let intent = intent.unwrap();
        assert_eq!(intent.side, Side::Sell);
        assert!(intent.take_profit < intent.price);
        assert!(intent.stop_loss > intent.price);
    }

    #[test]
    fn friction_filter_blocks_compressed_volatility_signals() {
        let config = OrderFlowImbalanceConfig {
            rolling_window_len: 5,
            z_ofi_threshold: dec!(1.5),
            absorption_ratio_threshold: dec!(0.35),
            min_take_profit_pct: dec!(0.05), // 5% minimum target required
            take_profit_vol_mult: dec!(1.0),
            ..Default::default()
        };
        let mut strategy = OrderFlowImbalanceStrategy::new(config).unwrap();

        // Feed neutral bars with tiny range ($10 on a $50k asset = 0.02%)
        for i in 1..=5 {
            let bar = create_test_bar(
                i * 1000,
                dec!(50000),
                dec!(50010),
                dec!(50000),
                dec!(50005),
                dec!(5.0),
                dec!(5.0),
            );
            strategy.on_bar(&bar);
        }

        // 6th bar: Strong breakout signal, but target is only ~$10 (<< 5% = $2500)
        let breakout_bar = create_test_bar(
            6000,
            dec!(50000),
            dec!(50010),
            dec!(50000),
            dec!(50010),
            dec!(9.0),
            dec!(1.0),
        );

        let intent = strategy.on_bar(&breakout_bar);
        assert!(intent.is_none(), "Should be blocked by friction filter");
    }
}
