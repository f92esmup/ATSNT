use domain::{CusumEvent, CusumFilter, DollarBar, DomainError, OrderIntent, RollingZScore, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::Strategy;

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
}

impl Default for DollarBarsCusumConfig {
    fn default() -> Self {
        Self {
            rolling_window_len: 20,
            cusum_vol_multiplier: dec!(2.0),
            z_entry_threshold: dec!(2.0),
            z_stop_threshold: dec!(3.5),
            time_barrier_bars: 15,
        }
    }
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
                let stop_dist = self.config.z_stop_threshold * z_res.std_dev;
                let stop_loss = if z_res.mean > stop_dist {
                    z_res.mean - stop_dist
                } else {
                    bar.close * dec!(0.90) // safety fallback floor
                };

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
                let stop_dist = self.config.z_stop_threshold * z_res.std_dev;
                let stop_loss = z_res.mean + stop_dist;

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
        }
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
    fn negative_climax_triggers_long_order_intent() {
        let config = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 10,
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
}
