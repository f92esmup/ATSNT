use domain::{DollarBar, Position, PositionSide, RiskPolicy, Side};
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
    entry_time: Option<i64>,
    entry_fee: Decimal,
    entry_slippage: Decimal,
    last_bar_end_time: Option<i64>,
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
            entry_time: None,
            entry_fee: Decimal::ZERO,
            entry_slippage: Decimal::ZERO,
            last_bar_end_time: None,
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

    /// Current cash-equity drawdown percentage, excluding unrealized PnL.
    pub fn current_drawdown_pct(&self) -> Decimal {
        if self.peak_equity <= Decimal::ZERO {
            Decimal::ZERO
        } else {
            (self.peak_equity - self.equity) / self.peak_equity
        }
    }

    fn current_drawdown_pct_at(
        &self,
        current_price: Decimal,
    ) -> Result<Decimal, domain::RiskError> {
        RiskPolicy::calculate_session_drawdown_pct(
            self.peak_equity,
            self.total_equity(current_price),
        )
    }

    /// Returns the currently active position, if any.
    #[inline]
    pub fn active_position(&self) -> Option<&Position> {
        self.active_position.as_ref()
    }

    /// Returns the active order intent for the current position, if any.
    #[inline]
    pub fn active_intent(&self) -> Option<&domain::OrderIntent> {
        self.active_intent.as_ref()
    }

    /// Returns the slice of all closed trades recorded during the simulation.
    #[inline]
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        &self.closed_trades
    }

    /// Returns the number of bars the active position has been held.
    #[inline]
    pub fn bars_held(&self) -> usize {
        self.bars_held
    }

    /// Returns the peak equity reached during the simulation.
    #[inline]
    pub fn peak_equity(&self) -> Decimal {
        self.peak_equity
    }

    /// Returns the maximum drawdown in currency amount.
    #[inline]
    pub fn max_drawdown_amount(&self) -> Decimal {
        self.max_drawdown_amount
    }

    /// Returns the maximum drawdown percentage observed.
    #[inline]
    pub fn max_drawdown_pct(&self) -> Decimal {
        self.max_drawdown_pct
    }

    /// Returns a reference to the backtest configuration.
    #[inline]
    pub fn config(&self) -> &BacktestConfig {
        &self.config
    }

    /// Ingests a closed DollarBar, updates position barriers, and steps the strategy.
    pub fn process_bar<S: Strategy>(&mut self, strategy: &mut S, bar: &DollarBar) {
        self.last_bar_end_time = Some(bar.end_time);
        let had_position = self.active_position.is_some();

        // 1. If an active position exists, evaluate Triple Barrier Exits
        if had_position {
            self.bars_held += 1;
            self.check_barriers(bar);
        }

        // 2. If no position was active at the start of this bar, query the strategy for signals
        if !had_position {
            if let Some(intent) = strategy.on_bar(bar) {
                self.evaluate_entry(intent, bar.close, bar.end_time);
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

        let mut exit_fill: Option<(Decimal, Decimal, bool, &'static str)> = None; // (exit_price, slippage, is_taker, exit_reason)

        match pos.side {
            PositionSide::Long => {
                // Stop Loss breach (price traded through barrier)
                if bar.low <= intent.stop_loss {
                    let slip = intent.stop_loss * self.config.slippage_pct;
                    let fill_price = intent.stop_loss - slip;
                    exit_fill = Some((fill_price, slip, true, "StopLoss"));
                }
                // Take Profit hit
                else if bar.high >= intent.take_profit {
                    exit_fill = Some((intent.take_profit, Decimal::ZERO, false, "TakeProfit"));
                }
                // Time barrier reached
                else if self.bars_held >= intent.max_bars_hold {
                    let slip = bar.close * self.config.slippage_pct;
                    let fill_price = bar.close - slip;
                    exit_fill = Some((fill_price, slip, true, "TimeBarrier"));
                }
            }
            PositionSide::Short => {
                // Stop Loss breach
                if bar.high >= intent.stop_loss {
                    let slip = intent.stop_loss * self.config.slippage_pct;
                    let fill_price = intent.stop_loss + slip;
                    exit_fill = Some((fill_price, slip, true, "StopLoss"));
                }
                // Take Profit hit
                else if bar.low <= intent.take_profit {
                    exit_fill = Some((intent.take_profit, Decimal::ZERO, false, "TakeProfit"));
                }
                // Time barrier reached
                else if self.bars_held >= intent.max_bars_hold {
                    let slip = bar.close * self.config.slippage_pct;
                    let fill_price = bar.close + slip;
                    exit_fill = Some((fill_price, slip, true, "TimeBarrier"));
                }
            }
        }

        if let Some((exit_price, slippage_per_unit, is_taker, exit_reason)) = exit_fill {
            self.execute_close(
                bar.end_time,
                exit_price,
                slippage_per_unit,
                is_taker,
                exit_reason,
            );
        }
    }

    fn evaluate_entry(
        &mut self,
        intent: domain::OrderIntent,
        mark_price: Decimal,
        entry_time: i64,
    ) {
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

        let Ok(current_session_drawdown_pct) = self.current_drawdown_pct_at(mark_price) else {
            return;
        };
        let defaults = RiskPolicy::default();
        let Ok(risk_policy) = RiskPolicy::new(
            defaults.max_position_notional,
            defaults.max_order_notional,
            self.config.max_daily_drawdown_pct,
        ) else {
            return;
        };
        let mark_to_market_equity = self.total_equity(mark_price);
        let Ok(quantity) = risk_policy.size_order(
            &intent,
            fill_price,
            mark_to_market_equity,
            self.config.risk_per_trade_pct,
            Decimal::ZERO,
            current_session_drawdown_pct,
        ) else {
            return;
        };

        // Entry Fee (Taker)
        let entry_notional = fill_price * quantity;
        let entry_fee = entry_notional * self.config.taker_fee_pct;
        let entry_slippage = slippage_per_unit * quantity;
        self.equity -= entry_fee + entry_slippage;

        let pos_side = match intent.side {
            Side::Buy => PositionSide::Long,
            Side::Sell => PositionSide::Short,
        };

        if let Ok(pos) = Position::new(pos_side, quantity, fill_price) {
            self.active_position = Some(pos);
            self.active_intent = Some(intent);
            self.bars_held = 0;
            self.entry_time = Some(entry_time);
            self.entry_fee = entry_fee;
            self.entry_slippage = entry_slippage;
        }
    }

    fn execute_close(
        &mut self,
        exit_time: i64,
        exit_price: Decimal,
        slippage_per_unit: Decimal,
        is_taker: bool,
        exit_reason: &str,
    ) {
        let pos = match self.active_position.take() {
            Some(p) => p,
            None => return,
        };
        self.active_intent = None;
        let entry_time = self.entry_time.take().unwrap_or(exit_time);
        let entry_fee = std::mem::take(&mut self.entry_fee);
        let entry_slippage = std::mem::take(&mut self.entry_slippage);

        let pnl_gross = pos.unrealized_pnl(exit_price);
        let exit_notional = exit_price * pos.quantity;
        let fee_rate = if is_taker {
            self.config.taker_fee_pct
        } else {
            self.config.maker_fee_pct
        };

        let exit_fee = exit_notional * fee_rate;
        let exit_slippage = slippage_per_unit * pos.quantity;
        let total_fees = entry_fee + exit_fee;
        let total_slippage = entry_slippage + exit_slippage;
        let pnl_net = pnl_gross - total_fees - total_slippage;

        // self.equity already had entry_fee and entry_slippage deducted upon entry
        self.equity += pnl_gross - exit_fee - exit_slippage;

        let return_pct = if pos.entry_price > Decimal::ZERO && pos.quantity > Decimal::ZERO {
            pnl_net / (pos.entry_price * pos.quantity)
        } else {
            Decimal::ZERO
        };

        let holding_duration_seconds = ((exit_time - entry_time) / 1000).max(0);

        self.closed_trades.push(ClosedTrade {
            entry_time,
            exit_time,
            entry_price: pos.entry_price,
            exit_price,
            quantity: pos.quantity,
            side: pos.side,
            pnl_gross,
            fees_paid: total_fees,
            slippage_paid: total_slippage,
            pnl_net,
            return_pct,
            exit_reason: exit_reason.to_string(),
            holding_duration_seconds,
        });
    }

    /// Concludes backtest simulation and compiles statistical performance metrics along with closed trades.
    pub fn finish_with_trades(
        mut self,
        last_mark_price: Option<Decimal>,
    ) -> (BacktestMetrics, Vec<ClosedTrade>) {
        if self.active_position.is_some() {
            if let Some(mark) = last_mark_price {
                let exit_time = self.last_bar_end_time.unwrap_or(0);
                self.execute_close(exit_time, mark, Decimal::ZERO, true, "EndOfSimulation");
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
    fn current_drawdown_at_price_includes_unrealized_pnl() {
        let mut engine = BacktestEngine::new(BacktestConfig::default());
        engine.equity = dec!(10_000);
        engine.peak_equity = dec!(10_000);
        engine.active_position = Some(
            Position::new(PositionSide::Long, dec!(100), dec!(100)).expect("valid test position"),
        );

        assert_eq!(
            engine.current_drawdown_pct_at(dec!(94)).unwrap(),
            dec!(0.06)
        );
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
