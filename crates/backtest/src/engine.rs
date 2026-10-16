use domain::{DollarBar, Position, PositionSide, RiskPolicy, Side};
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::config::BacktestConfig;
use crate::metrics::{BacktestMetrics, ClosedTrade, TradeExcursion};
use crate::monthly_guard::utc_month_key;

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
    open_excursion: Option<TradeExcursion>,
    current_day_utc: Option<i64>,
    daily_peak_equity: Decimal,
    daily_circuit_breaker_tripped: bool,
    current_month_key: Option<i64>,
    /// Marked equity at the first bar of the current UTC month (monthly drawdown reference).
    monthly_open_equity: Decimal,
    monthly_trip_drawdown_pct: Option<Decimal>,
    halted: bool,
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
            open_excursion: None,
            current_day_utc: None,
            daily_peak_equity: initial,
            daily_circuit_breaker_tripped: false,
            current_month_key: None,
            monthly_open_equity: initial,
            monthly_trip_drawdown_pct: None,
            halted: false,
        }
    }

    /// Current loss versus the equity at the start of the UTC month (never negative).
    pub fn monthly_drawdown_pct_at(&self, current_price: Decimal) -> Decimal {
        if self.monthly_open_equity > Decimal::ZERO {
            (self.monthly_open_equity - self.total_equity(current_price)).max(Decimal::ZERO)
                / self.monthly_open_equity
        } else {
            Decimal::ZERO
        }
    }

    /// Monthly drawdown measured at the moment the kill-switch tripped, if it did.
    #[inline]
    pub fn monthly_trip_drawdown_pct(&self) -> Option<Decimal> {
        self.monthly_trip_drawdown_pct
    }

    /// Whether the monthly kill-switch has permanently halted this simulation.
    #[inline]
    pub fn is_halted(&self) -> bool {
        self.halted
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

    /// Current session drawdown percentage from all-time peak marked equity.
    pub fn current_drawdown_pct_at(
        &self,
        current_price: Decimal,
    ) -> Result<Decimal, domain::RiskError> {
        RiskPolicy::calculate_session_drawdown_pct(
            self.peak_equity,
            self.total_equity(current_price),
        )
    }

    /// Current daily session drawdown percentage from daily peak marked equity.
    pub fn current_daily_drawdown_pct_at(
        &self,
        current_price: Decimal,
    ) -> Result<Decimal, domain::RiskError> {
        RiskPolicy::calculate_session_drawdown_pct(
            self.daily_peak_equity,
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
        if self.halted {
            return;
        }
        let day_key = bar.end_time / 86_400_000;
        let mark_before = self.total_equity(bar.open);

        // Daily session rollover: reset watermark and circuit breaker at 00:00:00 UTC day boundary
        if self.current_day_utc != Some(day_key) {
            self.current_day_utc = Some(day_key);
            self.daily_peak_equity = mark_before;
            self.daily_circuit_breaker_tripped = false;
        }

        // Monthly session rollover: the reference is the equity at the start of the UTC month.
        let month_key = utc_month_key(bar.end_time);
        if self.current_month_key != Some(month_key) {
            self.current_month_key = Some(month_key);
            self.monthly_open_equity = mark_before;
        }

        let had_position = self.active_position.is_some();
        let trades_before = self.closed_trades.len();

        // 1. If an active position exists, evaluate Triple Barrier Exits
        if had_position {
            self.bars_held += 1;
            self.check_barriers(bar);
        }

        // 1b. Monthly kill-switch: flatten at market and halt for good, before any new entry.
        let month_dd_pct = self.monthly_drawdown_pct_at(bar.close);
        if month_dd_pct >= self.config.max_monthly_drawdown_pct {
            self.halted = true;
            self.monthly_trip_drawdown_pct = Some(month_dd_pct);
            if let Some(side) = self.active_position.as_ref().map(|pos| pos.side) {
                let slip = bar.close * self.config.slippage_pct;
                let exit_price = match side {
                    PositionSide::Long => bar.close - slip,
                    PositionSide::Short => bar.close + slip,
                };
                self.execute_close(
                    bar.end_time,
                    exit_price,
                    slip,
                    true,
                    "MonthlyCircuitBreaker",
                );
            }
        }

        // 2. If no position was active at the start of this bar, query the strategy for signals
        if !had_position && !self.halted {
            if let Some(intent) = strategy.on_bar(bar) {
                self.evaluate_entry(intent, bar.close, bar.end_time);
            }
        }

        // 3. Mark-to-market and update peak equity / drawdown
        let total_val = self.total_equity(bar.close);
        if total_val > self.peak_equity {
            self.peak_equity = total_val;
        }
        if total_val > self.daily_peak_equity {
            self.daily_peak_equity = total_val;
        }

        let dd_amount = self.peak_equity - total_val;
        if dd_amount > self.max_drawdown_amount {
            self.max_drawdown_amount = dd_amount;
        }

        let dd_pct = if self.peak_equity > Decimal::ZERO {
            dd_amount / self.peak_equity
        } else {
            Decimal::ZERO
        };
        if dd_pct > self.max_drawdown_pct {
            self.max_drawdown_pct = dd_pct;
        }

        let day_dd_pct = if self.daily_peak_equity > Decimal::ZERO {
            (self.daily_peak_equity - total_val).max(Decimal::ZERO) / self.daily_peak_equity
        } else {
            Decimal::ZERO
        };
        if day_dd_pct >= self.config.max_daily_drawdown_pct {
            self.daily_circuit_breaker_tripped = true;
        }

        // 4. Track the open trade's equity path, or attribute this bar's post-exit
        //    drawdown sample to the trade that just closed.
        if self.active_position.is_some() {
            self.open_excursion
                .get_or_insert_with(|| TradeExcursion::start(bar.end_time, total_val))
                .observe_equity(bar.end_time, total_val);
            if let Some(excursion) = self.open_excursion.as_mut() {
                excursion.observe_drawdown(dd_amount, dd_pct);
            }
        } else if self.closed_trades.len() > trades_before {
            if let Some(trade) = self.closed_trades.last_mut() {
                trade.excursion.observe_drawdown(dd_amount, dd_pct);
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
        // If daily circuit breaker is tripped, halt trading for the remainder of today
        if self.daily_circuit_breaker_tripped {
            return;
        }

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

        let Ok(current_daily_drawdown_pct) = self.current_daily_drawdown_pct_at(mark_price) else {
            return;
        };
        let Ok(risk_policy) = self.config.risk_policy() else {
            return;
        };
        let mark_to_market_equity = self.total_equity(mark_price);
        let Ok(quantity) = risk_policy.size_order(
            &intent,
            fill_price,
            mark_to_market_equity,
            self.config.risk_per_trade_pct,
            Decimal::ZERO,
            current_daily_drawdown_pct,
        ) else {
            return;
        };

        // Entry Fee (Taker)
        let entry_notional = fill_price * quantity;
        let entry_fee = entry_notional * self.config.taker_fee_pct;
        let entry_slippage = slippage_per_unit * quantity;
        // Slippage is already embedded in fill_price; do not deduct from cash equity here
        self.equity -= entry_fee;

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

        // In real execution, fill_price and exit_price already embed slippage into pnl_gross.
        // We report total_slippage as an informative metric, but do not double-deduct it.
        let pnl_net = pnl_gross - total_fees;

        // self.equity already had entry_fee deducted upon entry. Adjust with pnl_gross and exit_fee.
        self.equity += pnl_gross - exit_fee;

        let return_pct = if pos.entry_price > Decimal::ZERO && pos.quantity > Decimal::ZERO {
            pnl_net / (pos.entry_price * pos.quantity)
        } else {
            Decimal::ZERO
        };

        let holding_duration_seconds = ((exit_time - entry_time) / 1000).max(0);

        let mut excursion = self
            .open_excursion
            .take()
            .unwrap_or_else(|| TradeExcursion::start(entry_time, self.equity));
        excursion.observe_equity(exit_time, self.equity);
        excursion.exit_equity = self.equity;

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
            excursion,
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

        let mut metrics = BacktestMetrics::calculate(
            self.config.initial_capital,
            &self.closed_trades,
            self.max_drawdown_amount,
            self.max_drawdown_pct,
        );
        metrics.halted = self.halted;
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
            buy_volume: dec!(0.5),
            sell_volume: dec!(0.5),
            buy_dollar_volume: close / dec!(2),
            sell_dollar_volume: close / dec!(2),
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
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(btc_cfg);

        let strat_cfg = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 10,
            min_take_profit_pct: dec!(0.0025),
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
            min_take_profit_pct: dec!(0.0025),
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
            min_take_profit_pct: dec!(0.0025),
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

    #[test]
    fn slippage_accounting_does_not_double_count_in_cash_equity_and_pnl_net() {
        let btc_cfg = BacktestConfig {
            initial_capital: dec!(1000),
            maker_fee_pct: dec!(0.0002),
            taker_fee_pct: dec!(0.0005),
            slippage_pct: dec!(0.01), // 1% slippage for prominent measurement
            risk_per_trade_pct: dec!(0.01),
            max_daily_drawdown_pct: dec!(0.20),
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(btc_cfg);

        let strat_cfg = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 10,
            min_take_profit_pct: dec!(0.0025),
        };
        let mut strategy = DollarBarsCusumStrategy::new(strat_cfg).unwrap();

        // Baseline bars to warm up rolling stats
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
        // fill_price = 90 + (90 * 0.01) = 90.90
        engine.process_bar(
            &mut strategy,
            &create_test_bar(5000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Rebound back above mean (100) -> Take profit hit at 100!
        // exit_price = 100.00 (maker exit at TP, zero exit slippage)
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(95), dec!(102), dec!(94), dec!(100)),
        );
        assert!(engine.active_position.is_none());

        let (metrics, trades) = engine.finish_with_trades(None);
        assert_eq!(trades.len(), 1);
        let trade = &trades[0];

        // pnl_gross = (exit_price - fill_price) * quantity
        // fill_price already embeds entry slippage!
        assert_eq!(trade.pnl_net, trade.pnl_gross - trade.fees_paid);
        assert!(trade.slippage_paid > Decimal::ZERO);

        // Cash equity must exactly equal initial_capital + pnl_net
        assert_eq!(metrics.net_profit, trade.pnl_net);
        assert_eq!(dec!(1000) + trade.pnl_net, dec!(1000) + metrics.net_profit);
    }

    #[test]
    fn daily_circuit_breaker_resets_at_utc_day_boundary() {
        let btc_cfg = BacktestConfig {
            initial_capital: dec!(1000),
            maker_fee_pct: dec!(0.0002),
            taker_fee_pct: dec!(0.0005),
            slippage_pct: dec!(0.0005),
            risk_per_trade_pct: dec!(0.05), // large position to trigger breaker
            max_daily_drawdown_pct: dec!(0.05), // 5% daily breaker
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(btc_cfg);

        let strat_cfg = DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1.0),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(2.0),
            time_barrier_bars: 10,
            min_take_profit_pct: dec!(0.0025),
        };
        let mut strategy = DollarBarsCusumStrategy::new(strat_cfg).unwrap();

        let day_1_ms: i64 = 1_000_000;
        let day_2_ms: i64 = day_1_ms + 86_400_000; // Next UTC day (+24h)

        // Day 1 Baseline
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 1000, dec!(101), dec!(101), dec!(101), dec!(101)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 2000, dec!(99), dec!(99), dec!(99), dec!(99)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 3000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        // Day 1: Plunge to 90 -> triggers Long entry
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 4000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Day 1: Crash to 70 -> hits Stop Loss and loses >5% of account!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 5000, dec!(80), dec!(81), dec!(69), dec!(70)),
        );
        assert!(engine.active_position.is_none());
        assert!(engine.daily_circuit_breaker_tripped);

        // Day 1: Another plunge to 60 -> signal generated by strategy but REJECTED by daily circuit breaker
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 6000, dec!(60), dec!(61), dec!(59), dec!(60)),
        );
        assert!(
            engine.active_position.is_none(),
            "Should be blocked on Day 1"
        );

        // Day 2 (midnight UTC rollover): new day starts!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_2_ms, dec!(60), dec!(60), dec!(60), dec!(60)),
        );
        assert!(
            !engine.daily_circuit_breaker_tripped,
            "Breaker must reset on Day 2"
        );

        // Day 2: Baseline
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_2_ms + 1000, dec!(61), dec!(61), dec!(61), dec!(61)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_2_ms + 2000, dec!(59), dec!(59), dec!(59), dec!(59)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_2_ms + 3000, dec!(60), dec!(60), dec!(60), dec!(60)),
        );

        // Day 2: Plunge to 50 -> triggers entry on Day 2 successfully!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_2_ms + 4000, dec!(50), dec!(51), dec!(49), dec!(50)),
        );
        assert!(
            engine.active_position.is_some(),
            "Trading should resume on Day 2!"
        );
    }

    /// Emits one long intent per query with a wide stop so losses come from mark-to-market.
    struct ScriptedLong {
        stop: Decimal,
    }

    impl Strategy for ScriptedLong {
        fn name(&self) -> &str {
            "ScriptedLong"
        }

        fn compatible_market_types(&self) -> &'static [strategies::MarketType] {
            &[strategies::MarketType::UsdMPerpetual]
        }

        fn on_bar(&mut self, bar: &DollarBar) -> Option<domain::OrderIntent> {
            domain::OrderIntent::new(
                bar.end_time,
                Side::Buy,
                bar.close,
                self.stop,
                bar.close * dec!(2),
                1_000,
            )
            .ok()
        }

        fn reset(&mut self) {}
    }

    const JAN_1: i64 = 1_704_067_200_000;
    const DAY: i64 = 86_400_000;

    fn flat_bar(time: i64, close: Decimal) -> DollarBar {
        create_test_bar(time, close, close, close, close)
    }

    fn breaker_config(daily: Decimal, monthly: Decimal, risk: Decimal) -> BacktestConfig {
        BacktestConfig {
            max_daily_drawdown_pct: daily,
            max_monthly_drawdown_pct: monthly,
            risk_per_trade_pct: risk,
            ..BacktestConfig::default()
        }
    }

    #[test]
    fn month_key_follows_utc_calendar() {
        assert_eq!(utc_month_key(JAN_1), 202_401);
        assert_eq!(utc_month_key(JAN_1 + 31 * DAY - 1), 202_401);
        assert_eq!(utc_month_key(JAN_1 + 31 * DAY), 202_402);
        assert_eq!(utc_month_key(JAN_1 + 60 * DAY), 202_403); // 2024 is a leap year
        assert_eq!(utc_month_key(0), 197_001);
    }

    #[test]
    fn monthly_breaker_closes_open_position_and_halts_for_good() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.5), dec!(0.10), dec!(0.9)));
        let mut strategy = ScriptedLong { stop: dec!(1) };

        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        assert!(engine.active_position().is_some());

        // Mark-to-market loss of ~14% of equity while far from the stop.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 1_000, dec!(85)));
        assert!(engine.is_halted());
        assert!(engine.active_position().is_none());
        assert_eq!(engine.closed_trades().len(), 1);
        assert_eq!(
            engine.closed_trades()[0].exit_reason,
            "MonthlyCircuitBreaker"
        );

        // Neither later bars of the month nor the next month reopen anything.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 2_000, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 40 * DAY, dec!(100)));
        assert!(engine.active_position().is_none());
        assert_eq!(engine.closed_trades().len(), 1);
    }

    #[test]
    fn monthly_breaker_stays_idle_below_the_limit() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.5), dec!(0.30), dec!(0.9)));
        let mut strategy = ScriptedLong { stop: dec!(1) };

        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 1_000, dec!(85)));

        assert!(!engine.is_halted());
        assert!(engine.active_position().is_some());
    }

    #[test]
    fn daily_breaker_blocks_entries_until_next_utc_day_but_monthly_stays_open() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.05), dec!(0.90), dec!(0.2)));
        let mut strategy = ScriptedLong { stop: dec!(50) };

        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        assert!(engine.active_position().is_some());

        // Stop-loss breach: ~20% loss trips the daily breaker.
        engine.process_bar(
            &mut strategy,
            &create_test_bar(JAN_1 + 1_000, dec!(100), dec!(100), dec!(40), dec!(45)),
        );
        assert_eq!(engine.closed_trades().len(), 1);
        assert!(engine.active_position().is_none());

        // Same UTC day: strategy keeps signalling but entries are blocked.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 2_000, dec!(100)));
        assert!(engine.active_position().is_none());

        // Next UTC day at 00:00: breaker resets and trading resumes.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + DAY, dec!(100)));
        assert!(engine.active_position().is_some());
        assert!(!engine.is_halted());
    }

    #[test]
    fn monthly_drawdown_is_measured_from_month_open_not_from_the_peak() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.9), dec!(0.10), dec!(0.9)));
        let mut strategy = ScriptedLong { stop: dec!(1) };

        // Open, rally (gain ~+4.5% of equity), then fall back to a level still above month open.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 1_000, dec!(105)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 2_000, dec!(92)));

        // ~-11% from the peak would trip a peak-based breaker, but equity is only ~-7% vs open.
        assert!(!engine.is_halted());
        assert!(engine.active_position().is_some());
    }

    #[test]
    fn monthly_reference_resets_at_the_new_month_open() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.9), dec!(0.10), dec!(0.5)));
        let mut strategy = ScriptedLong { stop: dec!(1) };

        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        // A slow bleed that never breaches 10% inside one month keeps the run alive...
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 1_000, dec!(95)));
        assert!(!engine.is_halted());
        // ...and the next month starts from the lower equity, not from January's.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 31 * DAY, dec!(95)));
        assert!(!engine.is_halted());
    }

    #[test]
    fn tripping_bar_closes_without_opening_a_new_position_and_flags_metrics() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.9), dec!(0.10), dec!(0.9)));
        let mut strategy = ScriptedLong { stop: dec!(1) };

        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 1_000, dec!(85)));

        assert!(engine.is_halted());
        assert!(engine.monthly_trip_drawdown_pct().unwrap() >= dec!(0.10));
        let (metrics, trades) = engine.finish_with_trades(Some(dec!(85)));
        assert!(metrics.halted);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "MonthlyCircuitBreaker");
    }
}
