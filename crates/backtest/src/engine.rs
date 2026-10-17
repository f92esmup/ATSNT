use domain::futures;
use domain::{DollarBar, Position, PositionSide, RiskPolicy, Side};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::config::BacktestConfig;
use crate::daily_watermark::DailyWatermark;
use crate::metrics::{BacktestMetrics, ClosedTrade, TradeExcursion};
use crate::monthly_guard::utc_month_key;

/// A submitted entry: the strategy intent and the quantity sized when it was signalled.
#[derive(Debug, Clone)]
struct PendingEntry {
    intent: domain::OrderIntent,
    quantity: Decimal,
}

/// Deterministic, discrete-event backtesting simulator.
///
/// Models exchange fees, a fixed per-fill slippage and Triple Barrier exits evaluated per bar.
/// A signal becomes a market order that fills at the next bar's open (the first price after
/// the signal); take-profit and stop exits are market orders too, so they pay taker fees and
/// slippage and fill at the open when a bar gaps through the trigger. Tick-level fills,
/// latency are tracked in `odd/tasks/sim-live-fidelity.md`.
///
/// A position pays perpetual funding at every eight-hour settlement a bar crosses (marked at
/// that bar's open) and runs on isolated margin: when a bar reaches the liquidation price
/// before the stop does, or gaps through it, the exchange closes the position at its
/// bankruptcy price and the whole margin is lost.
#[derive(Debug, Clone)]
pub struct BacktestEngine {
    config: BacktestConfig,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown_amount: Decimal,
    max_drawdown_pct: Decimal,
    active_position: Option<Position>,
    active_intent: Option<domain::OrderIntent>,
    /// Market order sized at the signal bar and waiting for the next bar's open to fill.
    pending_entry: Option<PendingEntry>,
    bars_held: usize,
    closed_trades: Vec<ClosedTrade>,
    entry_time: Option<i64>,
    entry_fee: Decimal,
    entry_slippage: Decimal,
    /// Isolated margin posted for the open position.
    position_margin: Decimal,
    /// Funding paid by the open position so far.
    funding_accrued: Decimal,
    last_bar_end_time: Option<i64>,
    open_excursion: Option<TradeExcursion>,
    current_day_utc: Option<i64>,
    /// Per-UTC-day marked-equity peak, the same type the live session uses.
    daily: DailyWatermark,
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
            pending_entry: None,
            bars_held: 0,
            closed_trades: Vec::new(),
            entry_time: None,
            entry_fee: Decimal::ZERO,
            entry_slippage: Decimal::ZERO,
            position_margin: Decimal::ZERO,
            funding_accrued: Decimal::ZERO,
            last_bar_end_time: None,
            open_excursion: None,
            current_day_utc: None,
            daily: {
                // Before the first bar the day's peak is the starting capital.
                let mut watermark = DailyWatermark::new();
                watermark.observe(0, initial);
                watermark
            },
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
            self.daily.peak(),
            self.total_equity(current_price),
        )
    }

    /// Returns the currently active position, if any.
    #[inline]
    pub fn active_position(&self) -> Option<&Position> {
        self.active_position.as_ref()
    }

    /// Returns the intent of the market order waiting to fill at the next bar's open, if any.
    #[inline]
    pub fn pending_intent(&self) -> Option<&domain::OrderIntent> {
        self.pending_entry.as_ref().map(|pending| &pending.intent)
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
        let previous_bar_end = self.last_bar_end_time;
        self.last_bar_end_time = Some(bar.end_time);
        if self.halted {
            return;
        }
        let day_key = bar.end_time / 86_400_000;
        let mark_before = self.total_equity(bar.open);

        // Daily session rollover: reset watermark and circuit breaker at 00:00:00 UTC day boundary
        if self.current_day_utc != Some(day_key) {
            self.current_day_utc = Some(day_key);
            self.daily.observe(bar.end_time, mark_before);
            self.daily_circuit_breaker_tripped = false;
        }

        // Monthly session rollover: the reference is the equity at the start of the UTC month.
        let month_key = utc_month_key(bar.end_time);
        if self.current_month_key != Some(month_key) {
            self.current_month_key = Some(month_key);
            self.monthly_open_equity = mark_before;
        }

        // Funding settles on the position held since the previous bar, marked at this open.
        if let Some(previous_end) = previous_bar_end {
            self.settle_funding(previous_end, bar);
        }

        // 0. A market order submitted on the previous bar fills at this bar's open.
        if let Some(pending) = self.pending_entry.take() {
            self.fill_entry(pending, bar);
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

        // 2. The strategy sees every bar so its rolling state matches the live session; a
        //    signal only becomes an order when flat. A time-barrier close is sent at the end of
        //    the bar and is still settling live, so that bar cannot open a new trade.
        if !self.halted {
            let signal = strategy.on_bar(bar);
            let closed_by_time_barrier = self.closed_trades.len() > trades_before
                && self
                    .closed_trades
                    .last()
                    .is_some_and(|trade| trade.exit_reason == "TimeBarrier");
            if let Some(intent) = signal {
                if self.active_position.is_none()
                    && self.pending_entry.is_none()
                    && !closed_by_time_barrier
                {
                    self.submit_entry(intent, bar.close);
                }
            }
        }

        // 3. Mark-to-market and update peak equity / drawdown
        let total_val = self.total_equity(bar.close);
        if total_val > self.peak_equity {
            self.peak_equity = total_val;
        }
        self.daily.observe(bar.end_time, total_val);

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

        let day_dd_pct = self.daily.drawdown_pct(total_val);
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

    /// Charges every funding settlement in `(previous_end, bar.end_time]` to the open position.
    fn settle_funding(&mut self, previous_end: i64, bar: &DollarBar) {
        let Some(position) = &self.active_position else {
            return;
        };
        let side = match position.side {
            PositionSide::Long => Side::Buy,
            PositionSide::Short => Side::Sell,
        };
        let settlements =
            futures::funding_settlements(futures::FUNDING_INTERVAL_MS, previous_end, bar.end_time);
        for _ in settlements {
            let payment = futures::funding_payment(
                side,
                position.quantity,
                bar.open,
                self.config.funding_rate_pct,
            );
            self.equity -= payment;
            self.funding_accrued += payment;
        }
    }

    /// Liquidation price of the open position, if one is open.
    fn liquidation_price(&self) -> Option<Decimal> {
        let position = self.active_position.as_ref()?;
        let side = match position.side {
            PositionSide::Long => Side::Buy,
            PositionSide::Short => Side::Sell,
        };
        let maintenance =
            position.quantity * position.entry_price * self.config.maintenance_margin_rate;
        Some(futures::liquidation_price(
            side,
            position.entry_price,
            position.quantity,
            self.position_margin,
            maintenance,
        ))
    }

    /// Closes the position at its bankruptcy price when the exchange would liquidate it on
    /// this bar: the mark reaches the liquidation price before the stop trades, or gaps through.
    fn liquidate_if_due(&mut self, bar: &DollarBar) -> bool {
        let (Some(liquidation), Some(intent), Some(position)) = (
            self.liquidation_price(),
            self.active_intent.clone(),
            self.active_position.clone(),
        ) else {
            return false;
        };
        let (reached, gapped, stop_first) = match position.side {
            PositionSide::Long => (
                bar.low <= liquidation,
                bar.open <= liquidation,
                intent.stop_loss > liquidation,
            ),
            PositionSide::Short => (
                bar.high >= liquidation,
                bar.open >= liquidation,
                intent.stop_loss < liquidation,
            ),
        };
        if !reached || (stop_first && !gapped) {
            return false;
        }
        let side = match position.side {
            PositionSide::Long => Side::Buy,
            PositionSide::Short => Side::Sell,
        };
        let bankruptcy = futures::bankruptcy_price(
            side,
            position.entry_price,
            position.quantity,
            self.position_margin,
        );
        self.execute_close_with_fee_rate(
            bar.end_time,
            bankruptcy,
            Decimal::ZERO,
            Decimal::ZERO,
            "Liquidation",
        );
        true
    }

    fn check_barriers(&mut self, bar: &DollarBar) {
        if self.liquidate_if_due(bar) {
            return;
        }
        let (intent, pos) = match (&self.active_intent, &self.active_position) {
            (Some(i), Some(p)) => (i.clone(), p.clone()),
            _ => return,
        };

        let mut exit_fill: Option<(Decimal, Decimal, bool, &'static str)> = None; // (exit_price, slippage, is_taker, exit_reason)

        // Barrier exits are market orders: they fill at the trigger or, when the bar gapped
        // through it, at the open, and pay slippage against the position. The reported
        // slippage is measured against the trigger so it includes the gap.
        let pct = self.config.slippage_pct;
        match pos.side {
            PositionSide::Long => {
                // Stop Loss breach (price traded through barrier)
                if bar.low <= intent.stop_loss {
                    let reference = intent.stop_loss.min(bar.open);
                    let fill_price = reference - reference * pct;
                    let slippage = (intent.stop_loss - fill_price).max(Decimal::ZERO);
                    exit_fill = Some((fill_price, slippage, true, "StopLoss"));
                }
                // Take Profit hit
                else if bar.high >= intent.take_profit {
                    let reference = intent.take_profit.max(bar.open);
                    let fill_price = reference - reference * pct;
                    // A target never gaps against the order: the slippage is what the market
                    // sell pays on the reference, even when the fill still beats the trigger.
                    let slippage = reference * pct;
                    exit_fill = Some((fill_price, slippage, true, "TakeProfit"));
                }
                // Time barrier reached
                else if self.bars_held >= intent.max_bars_hold {
                    let slip = bar.close * pct;
                    let fill_price = bar.close - slip;
                    exit_fill = Some((fill_price, slip, true, "TimeBarrier"));
                }
            }
            PositionSide::Short => {
                // Stop Loss breach
                if bar.high >= intent.stop_loss {
                    let reference = intent.stop_loss.max(bar.open);
                    let fill_price = reference + reference * pct;
                    let slippage = (fill_price - intent.stop_loss).max(Decimal::ZERO);
                    exit_fill = Some((fill_price, slippage, true, "StopLoss"));
                }
                // Take Profit hit
                else if bar.low <= intent.take_profit {
                    let reference = intent.take_profit.min(bar.open);
                    let fill_price = reference + reference * pct;
                    let slippage = reference * pct;
                    exit_fill = Some((fill_price, slippage, true, "TakeProfit"));
                }
                // Time barrier reached
                else if self.bars_held >= intent.max_bars_hold {
                    let slip = bar.close * pct;
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

    /// Gates and sizes a signal exactly like the live gateway (at the signal price) and queues
    /// the market order; it fills at the next bar's open.
    fn submit_entry(&mut self, intent: domain::OrderIntent, mark_price: Decimal) {
        // If daily circuit breaker is tripped, halt trading for the remainder of today
        if self.daily_circuit_breaker_tripped {
            return;
        }

        let Ok(current_daily_drawdown_pct) = self.current_daily_drawdown_pct_at(mark_price) else {
            return;
        };
        let Ok(risk_policy) = self.config.risk_policy() else {
            return;
        };
        let mark_to_market_equity = self.total_equity(mark_price);
        let Ok(quantity) = risk_policy.size_order(
            &intent,
            intent.price,
            mark_to_market_equity,
            self.config.risk_per_trade_pct,
            Decimal::ZERO,
            current_daily_drawdown_pct,
        ) else {
            return;
        };
        self.pending_entry = Some(PendingEntry { intent, quantity });
    }

    /// Fills a pending market order at the bar's open plus adverse slippage.
    fn fill_entry(&mut self, pending: PendingEntry, bar: &DollarBar) {
        if self.halted {
            return;
        }
        let PendingEntry { intent, quantity } = pending;
        let slip = bar.open * self.config.slippage_pct;
        let fill_price = match intent.side {
            Side::Buy => bar.open + slip,
            Side::Sell => bar.open - slip,
        };

        // Entry Fee (Taker)
        let entry_fee = fill_price * quantity * self.config.taker_fee_pct;

        // The exchange runs the position at the lowest leverage the balance can margin; an
        // order it cannot margin at any supported leverage is rejected.
        let notional = fill_price * quantity;
        let Some(leverage) = futures::minimum_leverage(
            notional,
            notional * self.config.maintenance_margin_rate,
            entry_fee,
            self.equity,
            self.config.max_leverage.trunc().to_u32().unwrap_or(1),
        ) else {
            return;
        };
        // Slippage is already embedded in fill_price; do not deduct from cash equity here
        self.equity -= entry_fee;
        // Reported slippage is what the order paid: the slippage on the open plus the adverse
        // part of the gap from the signal price. A gap in favor does not cancel the slippage.
        let adverse_per_unit = slip
            + match intent.side {
                Side::Buy => (bar.open - intent.price).max(Decimal::ZERO),
                Side::Sell => (intent.price - bar.open).max(Decimal::ZERO),
            };

        let pos_side = match intent.side {
            Side::Buy => PositionSide::Long,
            Side::Sell => PositionSide::Short,
        };

        if let Ok(pos) = Position::new(pos_side, quantity, fill_price) {
            self.active_position = Some(pos);
            self.active_intent = Some(intent);
            self.bars_held = 0;
            self.entry_time = Some(bar.start_time);
            self.entry_fee = entry_fee;
            self.entry_slippage = adverse_per_unit * quantity;
            self.position_margin = notional / Decimal::from(leverage);
            self.funding_accrued = Decimal::ZERO;
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
        let fee_rate = if is_taker {
            self.config.taker_fee_pct
        } else {
            self.config.maker_fee_pct
        };
        self.execute_close_with_fee_rate(
            exit_time,
            exit_price,
            slippage_per_unit,
            fee_rate,
            exit_reason,
        );
    }

    fn execute_close_with_fee_rate(
        &mut self,
        exit_time: i64,
        exit_price: Decimal,
        slippage_per_unit: Decimal,
        fee_rate: Decimal,
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
        let funding_paid = std::mem::take(&mut self.funding_accrued);
        self.position_margin = Decimal::ZERO;

        let exit_fee = exit_notional * fee_rate;
        let exit_slippage = slippage_per_unit * pos.quantity;
        let total_fees = entry_fee + exit_fee;
        let total_slippage = entry_slippage + exit_slippage;

        // In real execution, fill_price and exit_price already embed slippage into pnl_gross.
        // We report total_slippage as an informative metric, but do not double-deduct it.
        let pnl_net = pnl_gross - total_fees - funding_paid;

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
            funding_paid,
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
    use std::collections::VecDeque;
    use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy, MarketType};

    /// Emits one scripted decision per bar and counts how many bars it was shown.
    struct Scripted {
        decisions: VecDeque<Option<domain::OrderIntent>>,
        bars_seen: usize,
    }

    impl Scripted {
        fn new(decisions: Vec<Option<domain::OrderIntent>>) -> Self {
            Self {
                decisions: decisions.into(),
                bars_seen: 0,
            }
        }
    }

    impl Strategy for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_bar(&mut self, _: &DollarBar) -> Option<domain::OrderIntent> {
            self.bars_seen += 1;
            self.decisions.pop_front().flatten()
        }

        fn reset(&mut self) {}
    }

    /// Long intent at 100 with SL 95, TP 110 and the given time barrier.
    fn long_intent(max_bars_hold: usize) -> Option<domain::OrderIntent> {
        Some(
            domain::OrderIntent::new(1, Side::Buy, dec!(100), dec!(95), dec!(110), max_bars_hold)
                .unwrap(),
        )
    }

    fn realistic_engine() -> BacktestEngine {
        BacktestEngine::new(BacktestConfig {
            initial_capital: dec!(10_000),
            maker_fee_pct: dec!(0.0002),
            taker_fee_pct: dec!(0.0005),
            slippage_pct: dec!(0.01),
            risk_per_trade_pct: dec!(0.01),
            max_daily_drawdown_pct: dec!(0.50),
            ..BacktestConfig::default()
        })
    }

    fn ohlc_bar(
        time: i64,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
    ) -> DollarBar {
        create_test_bar(time, open, high, low, close)
    }

    #[test]
    fn the_strategy_sees_every_bar_even_while_a_position_is_open() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None, None, None]);

        for (i, price) in [dec!(100), dec!(100), dec!(101), dec!(102)]
            .iter()
            .enumerate()
        {
            let t = 1000 * (i as i64 + 1);
            engine.process_bar(&mut strategy, &ohlc_bar(t, *price, *price, *price, *price));
        }

        assert!(engine.active_position().is_some());
        assert_eq!(strategy.bars_seen, 4);
    }

    #[test]
    fn a_market_entry_fills_at_the_next_bar_open_with_slippage() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None]);

        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        assert!(
            engine.active_position().is_none(),
            "the order only exists after the signal bar"
        );

        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2000, dec!(102), dec!(103), dec!(101), dec!(102)),
        );

        let position = engine.active_position().expect("filled at the next bar");
        // Open 102 plus 1% slippage; sized at the signal price: 10_000 * 1% / (100 - 95) = 20.
        assert_eq!(position.entry_price, dec!(103.02));
        assert_eq!(position.quantity, dec!(20));
        assert_eq!(engine.bars_held(), 1);
    }

    #[test]
    fn take_profit_is_a_taker_exit_with_slippage() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2000, dec!(100), dec!(101), dec!(99), dec!(100)),
        );

        engine.process_bar(
            &mut strategy,
            &ohlc_bar(3000, dec!(101), dec!(112), dec!(100), dec!(111)),
        );

        let (_, trades) = engine.finish_with_trades(None);
        let trade = &trades[0];
        assert_eq!(trade.exit_reason, "TakeProfit");
        // Trigger 110, market sell: one percent worse than the trigger.
        assert_eq!(trade.exit_price, dec!(108.9));
        let entry_fee = trade.entry_price * trade.quantity * dec!(0.0005);
        let exit_fee = trade.exit_price * trade.quantity * dec!(0.0005);
        assert_eq!(trade.fees_paid, entry_fee + exit_fee);
    }

    #[test]
    fn a_stop_through_a_gap_fills_at_the_open_not_at_the_trigger() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2000, dec!(100), dec!(101), dec!(99), dec!(100)),
        );

        engine.process_bar(
            &mut strategy,
            &ohlc_bar(3000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );

        let (_, trades) = engine.finish_with_trades(None);
        assert_eq!(trades[0].exit_reason, "StopLoss");
        // Gapped below the 95 stop: filled around the 90 open, minus slippage.
        assert_eq!(trades[0].exit_price, dec!(89.1));
    }

    #[test]
    fn a_take_profit_gap_in_favor_fills_at_the_open() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2000, dec!(100), dec!(101), dec!(99), dec!(100)),
        );

        engine.process_bar(
            &mut strategy,
            &ohlc_bar(3000, dec!(120), dec!(121), dec!(119), dec!(120)),
        );

        let (_, trades) = engine.finish_with_trades(None);
        assert_eq!(trades[0].exit_reason, "TakeProfit");
        assert_eq!(trades[0].exit_price, dec!(118.8));
    }

    #[test]
    fn no_entry_is_taken_on_the_bar_where_the_time_barrier_closed_the_position() {
        let mut engine = realistic_engine();
        // Signal, fill bar, time-barrier bar (which also signals again), one more bar.
        let mut strategy = Scripted::new(vec![long_intent(2), None, long_intent(50), None]);

        for (i, price) in [dec!(100), dec!(100), dec!(100), dec!(100)]
            .iter()
            .enumerate()
        {
            let t = 1000 * (i as i64 + 1);
            engine.process_bar(
                &mut strategy,
                &ohlc_bar(t, *price, *price + dec!(1), *price - dec!(1), *price),
            );
        }

        assert!(engine.active_position().is_none());
        let (_, trades) = engine.finish_with_trades(None);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "TimeBarrier");
    }

    #[test]
    fn an_order_still_pending_when_the_data_ends_never_trades() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50)]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        let (metrics, trades) = engine.finish_with_trades(Some(dec!(100)));

        assert!(trades.is_empty());
        assert_eq!(metrics.total_trades, 0);
    }

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
        assert!(
            engine.active_position.is_none(),
            "the order fills on the next bar"
        );

        // Next bar: the market order fills at its open (flat bar, no barrier is touched).
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Rebound back above mean (100) -> Take profit hit!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(95), dec!(102), dec!(94), dec!(100)),
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
        assert!(
            engine.active_position.is_none(),
            "the order fills on the next bar"
        );

        // Next bar: the market order fills at its open (flat bar, no barrier is touched).
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Further crash to 70 -> hits Stop Loss!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(80), dec!(81), dec!(69), dec!(70)),
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

        // Signal
        engine.process_bar(
            &mut strategy,
            &create_test_bar(5000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_none());

        // The market order fills at this bar's open: bar 1 of the hold (within barrier)
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(91), dec!(92), dec!(89), dec!(91)),
        );
        assert!(engine.active_position.is_some());
        assert_eq!(engine.bars_held(), 1);

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

        // Plunge to 90 -> triggers the Long signal
        engine.process_bar(
            &mut strategy,
            &create_test_bar(5000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_none());

        // The order fills at the next open: fill_price = 90 + (90 * 0.01) = 90.90
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Rebound back above mean (100) -> Take profit hit: a taker exit that pays slippage too.
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(95), dec!(102), dec!(94), dec!(100)),
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

        // Day 1: Plunge to 90 -> triggers the Long signal; it fills on the next bar's open
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 4000, dec!(90), dec!(91), dec!(89), dec!(90)),
        );
        assert!(engine.active_position.is_none());
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 4500, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position.is_some());

        // Day 1: Crash through the stop without a gap (it opens at the entry price) -> the stop
        // fills near its trigger and the loss exceeds 5% of the account, tripping the breaker.
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 5000, dec!(90), dec!(90), dec!(69), dec!(70)),
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

        // Day 2: Plunge to 50 -> triggers the signal; the order fills on the next bar
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_2_ms + 4000, dec!(50), dec!(51), dec!(49), dec!(50)),
        );
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_2_ms + 4500, dec!(50), dec!(50), dec!(50), dec!(50)),
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
        assert!(
            engine.active_position().is_none(),
            "the order fills on the next bar"
        );
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 500, dec!(100)));
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
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 500, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 1_000, dec!(85)));

        assert!(!engine.is_halted());
        assert!(engine.active_position().is_some());
    }

    #[test]
    fn daily_breaker_blocks_entries_until_next_utc_day_but_monthly_stays_open() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.05), dec!(0.90), dec!(0.2)));
        let mut strategy = ScriptedLong { stop: dec!(50) };

        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        assert!(
            engine.active_position().is_none(),
            "the order fills on the next bar"
        );

        // The order fills at this bar's open and the same bar breaches the stop:
        // a ~20% loss trips the daily breaker.
        engine.process_bar(
            &mut strategy,
            &create_test_bar(JAN_1 + 1_000, dec!(100), dec!(100), dec!(40), dec!(45)),
        );
        assert_eq!(engine.closed_trades().len(), 1);
        assert!(engine.active_position().is_none());

        // Same UTC day: strategy keeps signalling but entries are blocked.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 2_000, dec!(100)));
        assert!(engine.active_position().is_none());

        // Next UTC day at 00:00: the breaker resets, the strategy signals again and the
        // order fills on the following bar.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + DAY, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + DAY + 500, dec!(100)));
        assert!(engine.active_position().is_some());
        assert!(!engine.is_halted());
    }

    #[test]
    fn monthly_drawdown_is_measured_from_month_open_not_from_the_peak() {
        let mut engine = BacktestEngine::new(breaker_config(dec!(0.9), dec!(0.10), dec!(0.9)));
        let mut strategy = ScriptedLong { stop: dec!(1) };

        // Open, rally (gain ~+4.5% of equity), then fall back to a level still above month open.
        engine.process_bar(&mut strategy, &flat_bar(JAN_1, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 500, dec!(100)));
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
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 500, dec!(100)));
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
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 500, dec!(100)));
        engine.process_bar(&mut strategy, &flat_bar(JAN_1 + 1_000, dec!(85)));

        assert!(engine.is_halted());
        assert!(engine.monthly_trip_drawdown_pct().unwrap() >= dec!(0.10));
        let (metrics, trades) = engine.finish_with_trades(Some(dec!(85)));
        assert!(metrics.halted);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "MonthlyCircuitBreaker");
    }

    const SETTLEMENT: i64 = domain::futures::FUNDING_INTERVAL_MS * 10;

    fn leveraged_engine(capital: Decimal) -> BacktestEngine {
        BacktestEngine::new(BacktestConfig {
            initial_capital: capital,
            slippage_pct: Decimal::ZERO,
            max_daily_drawdown_pct: dec!(0.90),
            max_monthly_drawdown_pct: dec!(0.95),
            ..BacktestConfig::default()
        })
    }

    /// Long at 100 with a stop 0.1 away, so the risk sizing asks for a lot of notional.
    fn tight_long() -> Option<domain::OrderIntent> {
        Some(domain::OrderIntent::new(1, Side::Buy, dec!(100), dec!(99.9), dec!(110), 50).unwrap())
    }

    #[test]
    fn a_settlement_crossed_while_open_charges_funding_to_the_trade() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None, None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT - 3_000,
                dec!(100),
                dec!(100),
                dec!(100),
                dec!(100),
            ),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT - 2_000,
                dec!(100),
                dec!(101),
                dec!(99),
                dec!(100),
            ),
        );
        // This bar crosses the settlement while the long is open.
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT + 1_000,
                dec!(101),
                dec!(102),
                dec!(100),
                dec!(101),
            ),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT + 2_000,
                dec!(101),
                dec!(112),
                dec!(100),
                dec!(111),
            ),
        );

        let (_, trades) = engine.finish_with_trades(None);

        let trade = &trades[0];
        // 20 units marked at the crossing bar's open (101) with a 0.01% rate.
        assert_eq!(trade.funding_paid, dec!(0.202));
        assert_eq!(
            trade.pnl_net,
            trade.pnl_gross - trade.fees_paid - trade.funding_paid
        );
    }

    #[test]
    fn funding_is_not_charged_without_a_position_or_a_settlement() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![None, None, long_intent(50), None, None]);
        // The settlement passes while flat.
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT - 1_000,
                dec!(100),
                dec!(100),
                dec!(100),
                dec!(100),
            ),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT + 1_000,
                dec!(100),
                dec!(100),
                dec!(100),
                dec!(100),
            ),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT + 2_000,
                dec!(100),
                dec!(100),
                dec!(100),
                dec!(100),
            ),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT + 3_000,
                dec!(100),
                dec!(101),
                dec!(99),
                dec!(100),
            ),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(
                SETTLEMENT + 4_000,
                dec!(101),
                dec!(112),
                dec!(100),
                dec!(111),
            ),
        );

        let (_, trades) = engine.finish_with_trades(None);

        assert_eq!(trades[0].funding_paid, Decimal::ZERO);
    }

    #[test]
    fn a_gap_through_the_liquidation_price_loses_the_margin_and_no_more() {
        let mut engine = leveraged_engine(dec!(1000));
        let mut strategy = Scripted::new(vec![tight_long(), None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1_000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2_000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        let position = engine.active_position().unwrap().clone();
        assert_eq!(position.quantity, dec!(100));

        // 100 units on 1000 need 11x: 909.09 of margin, liquidated near 91.3. The bar opens at
        // 80, so the stop would have filled far below the bankruptcy price.
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(3_000, dec!(80), dec!(81), dec!(79), dec!(80)),
        );

        let (_, trades) = engine.finish_with_trades(None);
        let trade = &trades[0];
        assert_eq!(trade.exit_reason, "Liquidation");
        assert_eq!(trade.exit_price.round_dp(4), dec!(90.9091));
        let entry_fee = dec!(100) * dec!(100) * dec!(0.0005);
        assert_eq!(trade.fees_paid, entry_fee);
        assert_eq!((trade.pnl_gross).round_dp(2), dec!(-909.09));
    }

    #[test]
    fn a_stop_that_trades_before_the_liquidation_price_is_a_stop() {
        let mut engine = leveraged_engine(dec!(1000));
        let mut strategy = Scripted::new(vec![tight_long(), None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1_000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2_000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        // The bar trades through the stop (99.9) and on down to the liquidation price.
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(3_000, dec!(100), dec!(100), dec!(90), dec!(90)),
        );

        let (_, trades) = engine.finish_with_trades(None);
        assert_eq!(trades[0].exit_reason, "StopLoss");
    }

    #[test]
    fn an_entry_the_balance_cannot_margin_never_opens() {
        let mut engine = leveraged_engine(dec!(400));
        let intent = Some(
            domain::OrderIntent::new(1, Side::Buy, dec!(100), dec!(99.99), dec!(110), 50).unwrap(),
        );
        let mut strategy = Scripted::new(vec![intent, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1_000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2_000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        // The leverage cap limits the order to 8000 of notional: 400 of margin at 20x plus
        // the entry fee exceeds the 400 balance.
        assert!(engine.active_position().is_none());
        assert_eq!(engine.cash_equity(), dec!(400));
    }

    #[test]
    fn slippage_paid_on_a_favorable_target_gap_is_still_reported() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2000, dec!(100), dec!(101), dec!(99), dec!(100)),
        );

        // The target (110) gaps in favor to an open of 120; the market sell pays 1% of that open.
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(3000, dec!(120), dec!(121), dec!(119), dec!(120)),
        );

        let (_, trades) = engine.finish_with_trades(None);
        // Entry: filled at 101 for a signal at 100 (1 x 20); exit: 1.2 x 20.
        assert_eq!(trades[0].slippage_paid, dec!(44));
    }

    #[test]
    fn slippage_paid_on_an_entry_that_gaps_in_favor_is_still_reported() {
        let mut engine = realistic_engine();
        let mut strategy = Scripted::new(vec![long_intent(50), None, None]);
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(1000, dec!(100), dec!(100), dec!(100), dec!(100)),
        );

        // The next open is 98, better than the signal at 100, but the order still pays 1% of it.
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(2000, dec!(98), dec!(99), dec!(97), dec!(98)),
        );
        engine.process_bar(
            &mut strategy,
            &ohlc_bar(3000, dec!(99), dec!(112), dec!(98), dec!(111)),
        );

        let (_, trades) = engine.finish_with_trades(None);
        // Entry 0.98 x 20; exit: the target at 110 sold 1.1 below, x 20.
        assert_eq!(trades[0].slippage_paid, dec!(41.6));
    }
}
