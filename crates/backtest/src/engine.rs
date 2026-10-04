use domain::{DollarBar, Position, PositionSide, Side};
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::config::BacktestConfig;
use crate::metrics::{BacktestMetrics, ClosedTrade};

/// Deterministic, discrete-event backtesting simulator.
///
/// Implements 1:1 behavioral parity without toy assumptions, modeling
/// exchange fees, latency slippage, and Triple Barrier exit mechanics.
#[derive(Debug, Clone)]
pub struct BacktestEngine {
    config: BacktestConfig,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown_amount: Decimal,
    max_drawdown_pct: Decimal,
    active_position: Option<Position>,
    active_intent: Option<domain::OrderIntent>,
    bars_held: usize,
    closed_trades: Vec<ClosedTrade>,
}

impl BacktestEngine {
    /// Creates a new backtest engine initialized with the given configuration.
    pub fn new(config: BacktestConfig) -> Self {
        let initial = config.initial_capital;
        Self {
            config,
            equity: initial,
            peak_equity: initial,
            max_drawdown_amount: Decimal::ZERO,
            max_drawdown_pct: Decimal::ZERO,
            active_position: None,
            active_intent: None,
            bars_held: 0,
            closed_trades: Vec::new(),
        }
    }

    /// Current cash equity balance (excluding unrealized PnL).
    #[inline]
    pub fn cash_equity(&self) -> Decimal {
        self.equity
    }

    /// Total portfolio equity including mark-to-market unrealized PnL.
    pub fn total_equity(&self, current_price: Decimal) -> Decimal {
        match &self.active_position {
            Some(pos) => self.equity + pos.unrealized_pnl(current_price),
            None => self.equity,
        }
    }

    /// Current peak-to-trough drawdown percentage.
    pub fn current_drawdown_pct(&self) -> Decimal {
        if self.peak_equity <= Decimal::ZERO {
            Decimal::ZERO
        } else {
            (self.peak_equity - self.equity) / self.peak_equity
        }
    }

    /// Ingests a closed DollarBar, updates position barriers, and steps the strategy.
    pub fn process_bar<S: Strategy>(&mut self, strategy: &mut S, bar: &DollarBar) {
        let had_position = self.active_position.is_some();

        // 1. If an active position exists, evaluate Triple Barrier Exits
        if had_position {
            self.bars_held += 1;
            self.check_barriers(bar);
        }

        // 2. If no position was active at the start of this bar, query the strategy for signals
        if !had_position {
            if let Some(intent) = strategy.on_bar(bar) {
                self.evaluate_entry(intent);
            }
        }

        // 3. Mark-to-market and update peak equity / drawdown
        let total_val = self.total_equity(bar.close);
        if total_val > self.peak_equity {
            self.peak_equity = total_val;
        }

        let dd_amount = self.peak_equity - total_val;
        if dd_amount > self.max_drawdown_amount {
            self.max_drawdown_amount = dd_amount;
        }

        if self.peak_equity > Decimal::ZERO {
            let dd_pct = dd_amount / self.peak_equity;
            if dd_pct > self.max_drawdown_pct {
                self.max_drawdown_pct = dd_pct;
            }
        }
    }

    fn check_barriers(&mut self, bar: &DollarBar) {
        let (intent, pos) = match (&self.active_intent, &self.active_position) {
            (Some(i), Some(p)) => (i.clone(), p.clone()),
            _ => return,
        };

        let mut exit_fill: Option<(Decimal, Decimal, bool)> = None; // (exit_price, slippage, is_taker)

        match pos.side {
            PositionSide::Long => {
                // Stop Loss breach (price traded through barrier)
                if bar.low <= intent.stop_loss {
                    let slip = intent.stop_loss * self.config.slippage_pct;
                    let fill_price = intent.stop_loss - slip;
                    exit_fill = Some((fill_price, slip, true));
                }
                // Take Profit hit
                else if bar.high >= intent.take_profit {
                    exit_fill = Some((intent.take_profit, Decimal::ZERO, false));
                }
                // Time barrier reached
                else if self.bars_held >= intent.max_bars_hold {
                    let slip = bar.close * self.config.slippage_pct;
                    let fill_price = bar.close - slip;
                    exit_fill = Some((fill_price, slip, true));
                }
            }
            PositionSide::Short => {
                // Stop Loss breach
                if bar.high >= intent.stop_loss {
                    let slip = intent.stop_loss * self.config.slippage_pct;
                    let fill_price = intent.stop_loss + slip;
                    exit_fill = Some((fill_price, slip, true));
                }
                // Take Profit hit
                else if bar.low <= intent.take_profit {
                    exit_fill = Some((intent.take_profit, Decimal::ZERO, false));
                }
                // Time barrier reached
                else if self.bars_held >= intent.max_bars_hold {
                    let slip = bar.close * self.config.slippage_pct;
                    let fill_price = bar.close + slip;
                    exit_fill = Some((fill_price, slip, true));
                }
            }
        }

        if let Some((exit_price, slippage_per_unit, is_taker)) = exit_fill {
            self.execute_close(bar.end_time, exit_price, slippage_per_unit, is_taker);
        }
    }

    fn evaluate_entry(&mut self, intent: domain::OrderIntent) {
        // Circuit breaker check
        if self.current_drawdown_pct() >= self.config.max_daily_drawdown_pct {
            return;
        }

        let per_unit_risk = (intent.price - intent.stop_loss).abs();
        if per_unit_risk <= Decimal::ZERO {
            return;
        }

        // Fixed Fractional Sizing: Risk Amount = Equity * RiskPct
        let capital_at_risk = self.equity * self.config.risk_per_trade_pct;
        let quantity = capital_at_risk / per_unit_risk;
        if quantity <= Decimal::ZERO {
            return;
        }

        // Apply entry slippage
        let (fill_price, slippage_per_unit) = match intent.side {
            Side::Buy => {
                let slip = intent.price * self.config.slippage_pct;
                (intent.price + slip, slip)
            }
            Side::Sell => {
                let slip = intent.price * self.config.slippage_pct;
                (intent.price - slip, slip)
            }
        };

        // Entry Fee (Taker)
        let entry_notional = fill_price * quantity;
        let entry_fee = entry_notional * self.config.taker_fee_pct;
        self.equity -= entry_fee;

        let pos_side = match intent.side {
            Side::Buy => PositionSide::Long,
            Side::Sell => PositionSide::Short,
        };

        if let Ok(pos) = Position::new(pos_side, quantity, fill_price) {
            self.active_position = Some(pos);
            self.active_intent = Some(intent);
            self.bars_held = 0;
            // Record initial entry friction
            self.equity -= slippage_per_unit * quantity;
        }
    }

    fn execute_close(
        &mut self,
        exit_time: i64,
        exit_price: Decimal,
        slippage_per_unit: Decimal,
        is_taker: bool,
    ) {
        let pos = match self.active_position.take() {
            Some(p) => p,
            None => return,
        };
        self.active_intent = None;

        let pnl_gross = pos.unrealized_pnl(exit_price);
        let exit_notional = exit_price * pos.quantity;
        let fee_rate = if is_taker {
            self.config.taker_fee_pct
        } else {
            self.config.maker_fee_pct
        };

        let exit_fee = exit_notional * fee_rate;
        let exit_slippage = slippage_per_unit * pos.quantity;
        let pnl_net = pnl_gross - exit_fee - exit_slippage;

        self.equity += pnl_net;

        let return_pct = if pos.entry_price > Decimal::ZERO {
            pnl_net / (pos.entry_price * pos.quantity)
        } else {
            Decimal::ZERO
        };

        self.closed_trades.push(ClosedTrade {
            exit_time,
            pnl_gross,
            fees_paid: exit_fee,
            slippage_paid: exit_slippage,
            pnl_net,
            return_pct,
        });
    }

    /// Concludes backtest simulation and compiles statistical performance metrics along with closed trades.
    pub fn finish_with_trades(
        mut self,
        last_mark_price: Option<Decimal>,
    ) -> (BacktestMetrics, Vec<ClosedTrade>) {
        if self.active_position.is_some() {
            if let Some(mark) = last_mark_price {
                self.execute_close(0, mark, Decimal::ZERO, true);
            }
        }

        let metrics = BacktestMetrics::calculate(
            self.config.initial_capital,
            &self.closed_trades,
            self.max_drawdown_amount,
            self.max_drawdown_pct,
        );
        (metrics, self.closed_trades)
    }

    /// Concludes backtest simulation and compiles statistical performance metrics.
    pub fn finish(self, last_mark_price: Option<Decimal>) -> BacktestMetrics {
        self.finish_with_trades(last_mark_price).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy};

    fn create_test_bar(
        time: i64,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
    ) -> DollarBar {
        DollarBar {
            start_time: time - 50,
            end_time: time,
            open,
            high,
            low,
            close,
            volume: dec!(1.0),
            dollar_volume: close,
            trade_count: 5,
        }
    }

    #[test]
    fn full_trade_cycle_hits_take_profit() {
        let btc_cfg = BacktestConfig {
            initial_capital: dec!(10000),
            maker_fee_pct: dec!(0.0002),
            taker_fee_pct: dec!(0.0005),
            slippage_pct: dec!(0.0005),
            risk_per_trade_pct: dec!(0.01),
            max_daily_drawdown_pct: dec!(0.05),
        };
        let mut engine = BacktestEngine::new(btc_cfg);

        let strat_cfg = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 10,
        };
        let mut strategy = DollarBarsCusumStrategy::new(strat_cfg).unwrap();

        // Feed baseline bars to establish volatility
        engine.process_bar(
            &mut strategy,
            &create_test_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(2000, dec!(101), dec!(101), dec!(101), dec!(101)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(3000, dec!(99), dec!(99), dec!(99), dec!(99)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(4000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        // Plunge to 90 -> triggers Long entry signal
        engine.process_bar(
            &mut strategy,
            &create_test_bar(5000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Rebound back above mean (100) -> Take profit hit!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(95), dec!(102), dec!(94), dec!(100)),
        );
        assert!(engine.active_position.is_none());

        let metrics = engine.finish(None);
        assert_eq!(metrics.total_trades, 1);
        assert_eq!(metrics.winning_trades, 1);
        assert!(metrics.net_profit > Decimal::ZERO);
        assert!(metrics.total_fees > Decimal::ZERO);
        assert!(metrics.profit_factor > dec!(1.0));
    }

    #[test]
    fn trade_cycle_hits_stop_loss() {
        let btc_cfg = BacktestConfig::default();
        let mut engine = BacktestEngine::new(btc_cfg);

        let strat_cfg = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(2.0),
            time_barrier_bars: 10,
        };
        let mut strategy = DollarBarsCusumStrategy::new(strat_cfg).unwrap();

        // Baseline
        engine.process_bar(
            &mut strategy,
            &create_test_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(2000, dec!(101), dec!(101), dec!(101), dec!(101)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(3000, dec!(99), dec!(99), dec!(99), dec!(99)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(4000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        // Plunge to 90 -> triggers Long entry
        engine.process_bar(
            &mut strategy,
            &create_test_bar(5000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Further crash to 70 -> hits Stop Loss!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(80), dec!(81), dec!(69), dec!(70)),
        );
        assert!(engine.active_position.is_none());

        let metrics = engine.finish(None);
        assert_eq!(metrics.total_trades, 1);
        assert_eq!(metrics.losing_trades, 1);
        assert!(metrics.net_profit < Decimal::ZERO);
        assert!(metrics.max_drawdown_amount > Decimal::ZERO);
    }

    #[test]
    fn trade_cycle_forced_exit_by_time_barrier() {
        let btc_cfg = BacktestConfig::default();
        let mut engine = BacktestEngine::new(btc_cfg);

        let strat_cfg = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(5.0), // wide stop
            time_barrier_bars: 2,        // short time barrier
        };
        let mut strategy = DollarBarsCusumStrategy::new(strat_cfg).unwrap();

        // Baseline
        engine.process_bar(
            &mut strategy,
            &create_test_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(2000, dec!(101), dec!(101), dec!(101), dec!(101)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(3000, dec!(99), dec!(99), dec!(99), dec!(99)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(4000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        // Entry
        engine.process_bar(
            &mut strategy,
            &create_test_bar(5000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Bar 1 of hold (within barrier)
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(91), dec!(92), dec!(89), dec!(91)),
        );
        assert!(engine.active_position.is_some());

        // Bar 2 of hold -> reaches time_barrier_bars (2) -> Forced market close!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(91), dec!(92), dec!(89), dec!(91)),
        );
        assert!(engine.active_position.is_none());

        let metrics = engine.finish(None);
        assert_eq!(metrics.total_trades, 1);
    }
}
