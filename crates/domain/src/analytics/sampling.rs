use rust_decimal::Decimal;

use crate::bar::DollarBar;
use crate::error::DomainError;
use crate::trade::Trade;

/// In-flight state of a DollarBar currently being accumulated.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InProgressBar {
    start_time: i64,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Decimal,
    dollar_volume: Decimal,
    trade_count: u64,
}

impl InProgressBar {
    fn new(first_trade: &Trade, initial_dollar_volume: Decimal) -> Self {
        Self {
            start_time: first_trade.timestamp,
            open: first_trade.price,
            high: first_trade.price,
            low: first_trade.price,
            close: first_trade.price,
            volume: first_trade.quantity,
            dollar_volume: initial_dollar_volume,
            trade_count: 1,
        }
    }

    fn update(&mut self, trade: &Trade, dollar_val: Decimal) {
        if trade.price > self.high {
            self.high = trade.price;
        }
        if trade.price < self.low {
            self.low = trade.price;
        }
        self.close = trade.price;
        self.volume += trade.quantity;
        self.dollar_volume += dollar_val;
        self.trade_count += 1;
    }

    fn into_dollar_bar(self, end_time: i64) -> DollarBar {
        DollarBar {
            start_time: self.start_time,
            end_time,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            dollar_volume: self.dollar_volume,
            trade_count: self.trade_count,
        }
    }
}

/// Deterministic, zero-I/O streaming aggregator for Dollar Bars.
///
/// Implements information-driven sampling as defined in Marcos López de Prado's
/// *Advances in Financial Machine Learning* (Chapter 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DollarBarAggregator {
    threshold: Decimal,
    current_bar: Option<InProgressBar>,
    carryover_dollar_volume: Decimal,
}

impl DollarBarAggregator {
    /// Initializes an aggregator with a strictly positive dollar threshold.
    pub fn new(threshold: Decimal) -> Result<Self, DomainError> {
        if threshold <= Decimal::ZERO {
            return Err(DomainError::InvalidThreshold(threshold.to_string()));
        }
        Ok(Self {
            threshold,
            current_bar: None,
            carryover_dollar_volume: Decimal::ZERO,
        })
    }

    /// Threshold value configured for each bar.
    #[inline]
    pub fn threshold(&self) -> Decimal {
        self.threshold
    }

    /// Currently accumulated dollar volume toward the next bar.
    #[inline]
    pub fn current_accumulated_dollar_volume(&self) -> Decimal {
        match &self.current_bar {
            Some(bar) => bar.dollar_volume,
            None => self.carryover_dollar_volume,
        }
    }

    /// Ingests a single trade event into the stream.
    ///
    /// If the accumulated dollar volume reaches or exceeds `threshold`,
    /// emits a completed `DollarBar`. Excess dollar volume is carried over
    /// to preserve sampling continuity without statistical drift.
    pub fn process_trade(&mut self, trade: &Trade) -> Option<DollarBar> {
        let trade_dollar_val = trade.dollar_value();

        let in_progress = match self.current_bar.take() {
            Some(mut bar) => {
                bar.update(trade, trade_dollar_val);
                bar
            }
            None => {
                let initial_val = self.carryover_dollar_volume + trade_dollar_val;
                self.carryover_dollar_volume = Decimal::ZERO;
                InProgressBar::new(trade, initial_val)
            }
        };

        if in_progress.dollar_volume >= self.threshold {
            let rollover = in_progress.dollar_volume - self.threshold;
            self.carryover_dollar_volume = rollover;
            self.current_bar = None;
            Some(in_progress.into_dollar_bar(trade.timestamp))
        } else {
            self.current_bar = Some(in_progress);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::Side;
    use rust_decimal_macros::dec;

    #[test]
    fn invalid_threshold_rejected() {
        assert!(DollarBarAggregator::new(dec!(0)).is_err());
        assert!(DollarBarAggregator::new(dec!(-500)).is_err());
    }

    #[test]
    fn single_trade_exceeding_threshold_emits_immediately() {
        let mut agg = DollarBarAggregator::new(dec!(100_000)).unwrap();
        let trade = Trade::new(1000, dec!(50_000), dec!(2.5), Side::Buy).unwrap(); // $125,000

        let bar = agg.process_trade(&trade);
        assert!(bar.is_some());
        let bar = bar.unwrap();

        assert_eq!(bar.start_time, 1000);
        assert_eq!(bar.end_time, 1000);
        assert_eq!(bar.open, dec!(50_000));
        assert_eq!(bar.high, dec!(50_000));
        assert_eq!(bar.low, dec!(50_000));
        assert_eq!(bar.close, dec!(50_000));
        assert_eq!(bar.volume, dec!(2.5));
        assert_eq!(bar.dollar_volume, dec!(125_000));
        assert_eq!(bar.trade_count, 1);

        // Rollover of $25,000 carried to next bar
        assert_eq!(agg.current_accumulated_dollar_volume(), dec!(25_000));
    }

    #[test]
    fn multiple_trades_aggregation_with_high_low_tracking() {
        let mut agg = DollarBarAggregator::new(dec!(100_000)).unwrap();

        // Trade 1: $40,000 (accumulated: $40,000) -> None
        let t1 = Trade::new(1000, dec!(40_000), dec!(1.0), Side::Buy).unwrap();
        assert!(agg.process_trade(&t1).is_none());

        // Trade 2: $42,000 (accumulated: $82,000) -> None
        let t2 = Trade::new(2000, dec!(42_000), dec!(1.0), Side::Buy).unwrap();
        assert!(agg.process_trade(&t2).is_none());

        // Trade 3: $39,000 (accumulated: $121,000) -> Some(DollarBar)
        let t3 = Trade::new(3000, dec!(39_000), dec!(1.0), Side::Sell).unwrap();
        let bar = agg.process_trade(&t3);
        assert!(bar.is_some());

        let bar = bar.unwrap();
        assert_eq!(bar.start_time, 1000);
        assert_eq!(bar.end_time, 3000);
        assert_eq!(bar.open, dec!(40_000));
        assert_eq!(bar.high, dec!(42_000));
        assert_eq!(bar.low, dec!(39_000));
        assert_eq!(bar.close, dec!(39_000));
        assert_eq!(bar.volume, dec!(3.0));
        assert_eq!(bar.dollar_volume, dec!(121_000));
        assert_eq!(bar.trade_count, 3);

        // Rollover check: 121,000 - 100,000 = 21,000
        assert_eq!(agg.current_accumulated_dollar_volume(), dec!(21_000));
    }

    #[test]
    fn rollover_impacts_subsequent_bar() {
        let mut agg = DollarBarAggregator::new(dec!(100_000)).unwrap();

        // Trade 1: $110,000 -> Emits bar, carries $10,000
        let t1 = Trade::new(1000, dec!(55_000), dec!(2.0), Side::Buy).unwrap();
        let bar1 = agg.process_trade(&t1);
        assert!(bar1.is_some());
        assert_eq!(agg.current_accumulated_dollar_volume(), dec!(10_000));

        // Trade 2: $90,000 -> Combined with rollover ($10k + $90k = $100k) -> Emits bar immediately!
        let t2 = Trade::new(2000, dec!(45_000), dec!(2.0), Side::Sell).unwrap();
        let bar2 = agg.process_trade(&t2);
        assert!(bar2.is_some());
        let bar2 = bar2.unwrap();

        assert_eq!(bar2.dollar_volume, dec!(100_000));
        assert_eq!(agg.current_accumulated_dollar_volume(), dec!(0));
    }
}
