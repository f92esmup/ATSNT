use std::collections::BTreeMap;

use domain::{DollarBar, Position, PositionSide, RiskPolicy, Side};
use rust_decimal::Decimal;
use strategies::{Decision, MarketEvent, MarketType, Strategy};

use crate::config::BacktestConfig;
use crate::execution_core::{
    market_fill_price, ExecutionCore, PendingEntry, PendingEntryOutcome, PendingTarget,
};
use crate::metrics::{
    BacktestMetrics, ClosedTrade, TradeExcursion, DAILY_HALT_REJECTION, POSITION_CAP_CLIP,
    REARM_IGNORED_REJECTION, SPOT_SHORTING_DISALLOWED, UNSUPPORTED_DECISION_REJECTION,
};
use crate::price_event::PriceEvent;
use domain::accounting::utc_month_key;
use domain::accounting::DailyWatermark;

/// Deterministic, discrete-event backtesting simulator.
///
/// Models exchange fees, a fixed per-fill slippage and Triple Barrier exits evaluated per bar.
/// A signal becomes a market order that fills at the next bar's open (the first price after
/// the signal); take-profit and stop exits are market orders too, so they pay taker fees and
/// slippage and fill at the open when a bar gaps through the trigger. With
/// `BacktestConfig::latency_ms` set, an order is due that many milliseconds after the deciding
/// event's end and fills at the first [`PriceEvent`] starting at or after then. Tick-level
/// fills are tracked in `odd/tasks/sim-live-fidelity.md`.
///
/// A position pays perpetual funding at every eight-hour settlement a bar crosses (marked at
/// that bar's open) and runs on isolated margin: when a bar reaches the liquidation price
/// before the stop does, or gaps through it, the exchange closes the position at its
/// bankruptcy price and the whole margin is lost.
///
/// A [`Decision::Target`] is a signed exposure (strategy spec §11): it is sized when decided,
/// replaces any pending target, and fills as the delta to the current position (scale-in,
/// scale-out, or a flip in two fills) with one aggregate stop and no take profit or time limit.
/// Each flat-to-flat (or flip) campaign is one [`ClosedTrade`]. An `Entry` is ignored while a
/// target is open or pending, and a target is ignored (counted as
/// [`UNSUPPORTED_DECISION_REJECTION`]) while a triple-barrier entry is open or pending.
///
/// Position accounting (fills, exits, margin, funding, closed trades) lives in
/// [`ExecutionCore`]; the engine owns the loop, the cash balance, the breakers, and the
/// mark to market.
#[derive(Debug, Clone)]
pub struct BacktestEngine {
    config: BacktestConfig,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown_amount: Decimal,
    max_drawdown_pct: Decimal,
    core: ExecutionCore,
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
    bracket_priority: bool,
    /// Signals that did not become orders, per reason (see `BacktestMetrics::rejected_orders`).
    rejected_orders: BTreeMap<String, usize>,
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
            core: ExecutionCore::new(),
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
            bracket_priority: false,
            rejected_orders: BTreeMap::new(),
        }
    }

    /// Sets whether open target positions with active take-profit exits prioritize running to
    /// their bracket limits rather than market-closing on level 0 decisions.
    pub fn set_bracket_priority(&mut self, enabled: bool) {
        self.bracket_priority = enabled;
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
        match self.core.position() {
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
        self.core.position()
    }

    /// Returns the intent of the market order waiting to fill at the next bar's open, if any.
    #[inline]
    pub fn pending_intent(&self) -> Option<&domain::OrderIntent> {
        self.core.pending().map(|pending| &pending.intent)
    }

    /// Returns the active order intent for the current position, if any.
    #[inline]
    pub fn active_intent(&self) -> Option<&domain::OrderIntent> {
        self.core.intent()
    }

    /// Returns the protective exits of the open position (the one aggregate stop for a target
    /// position), if any.
    #[inline]
    pub fn active_exits(&self) -> Option<&domain::ProtectiveExits> {
        self.core.exits()
    }

    /// Returns the target waiting to fill, if any.
    #[inline]
    pub fn pending_target(&self) -> Option<&crate::execution_core::PendingTarget> {
        self.core.pending_target()
    }

    /// Returns the slice of all closed trades recorded during the simulation.
    #[inline]
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        self.core.ledger().trades()
    }

    /// Returns the number of bars the active position has been held.
    #[inline]
    pub fn bars_held(&self) -> usize {
        self.core.bars_held()
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
    ///
    /// A thin wrapper over [`BacktestEngine::process_event`]: the bar is the price event and the
    /// strategy's decision on it is the decision step.
    pub fn process_bar<S: Strategy>(&mut self, strategy: &mut S, bar: &DollarBar) {
        self.bracket_priority = strategy.bracket_priority();
        self.process_event(&PriceEvent::from(bar), &mut || {
            strategy.on_event(&MarketEvent::DollarBar(bar))
        });
    }

    /// Processes one price event in the order of strategy spec §11.2: day and month rollover,
    /// funding, the pending order's fill when it is due, liquidation and protective exits, the
    /// monthly kill switch, the decision step (`decide`), mark to market and the daily breaker,
    /// and the open trade's excursion.
    ///
    /// `decide` is called once per event unless the engine is halted. A decision is scheduled,
    /// never filled on the event that produced it: its order is due at `event.end_ms +
    /// latency_ms` and fills at the first later event whose `start_ms` is at or after that time.
    pub fn process_event(
        &mut self,
        event: &PriceEvent,
        decide: &mut dyn FnMut() -> Option<Decision>,
    ) {
        let bar = event;
        let previous_bar_end = self.last_bar_end_time;
        self.last_bar_end_time = Some(bar.end_ms);
        if self.halted {
            return;
        }
        let day_key = bar.end_ms / 86_400_000;
        let mark_before = self.total_equity(bar.open);

        // Daily session rollover: reset watermark and circuit breaker at 00:00:00 UTC day boundary
        if self.current_day_utc != Some(day_key) {
            self.current_day_utc = Some(day_key);
            self.daily.observe(bar.end_ms, mark_before);
            self.daily_circuit_breaker_tripped = false;
        }

        // Monthly session rollover: the reference is the equity at the start of the UTC month.
        let month_key = utc_month_key(bar.end_ms);
        if self.current_month_key != Some(month_key) {
            self.current_month_key = Some(month_key);
            self.monthly_open_equity = mark_before;
        }

        // Funding settles on the position held since the previous event, marked at this open.
        if let Some(previous_end) = previous_bar_end {
            self.core
                .settle_funding(previous_end, *bar, &self.config, &mut self.equity);
        }

        // A target fill may close a campaign on this event (a flip or a flat target), so the
        // trade count is taken before the fills. Entry fills never close a trade.
        let trades_before = self.closed_trades().len();

        // 0. A pending order (market or limit) submitted on an earlier event evaluates once due.
        if !self.halted {
            if let PendingEntryOutcome::ExpiredLimit =
                self.core
                    .step_pending_entry(*bar, &self.config, &mut self.equity)
            {
                self.count_rejection("LimitTimeoutExpired");
            }
        }
        if let Some(pending) = self.core.take_due_target(bar.start_ms) {
            let counted = self.core.fill_target(
                pending,
                *bar,
                &self.config,
                &mut self.equity,
                &mut self.open_excursion,
            );
            for reason in counted {
                self.count_rejection(reason);
            }
        }

        let had_position = self.core.position().is_some();

        // 1. If an active position exists, evaluate liquidation and the protective exits.
        if had_position {
            self.core.evaluate_exits(
                *bar,
                &self.config,
                &mut self.equity,
                &mut self.open_excursion,
            );
        }

        // 1b. Monthly kill-switch: flatten at market and halt for good, before any new entry.
        let month_dd_pct = self.monthly_drawdown_pct_at(bar.close);
        if month_dd_pct >= self.config.max_monthly_drawdown_pct {
            self.halted = true;
            self.monthly_trip_drawdown_pct = Some(month_dd_pct);
            if let Some(side) = self.core.position().map(|pos| pos.side) {
                let close_side = match side {
                    PositionSide::Long => Side::Sell,
                    PositionSide::Short => Side::Buy,
                };
                let (exit_price, slip) =
                    market_fill_price(close_side, bar.close, self.config.slippage_pct);
                self.execute_close(bar.end_ms, exit_price, slip, true, "MonthlyCircuitBreaker");
            }
        }

        // 2. The strategy sees every event so its rolling state matches the live session; a
        //    signal only becomes an order when flat. A time-barrier close is sent at the end of
        //    the event and is still settling live, so that event cannot open a new trade.
        if !self.halted {
            let signal = match decide() {
                Some(Decision::Entry(intent)) => Some(intent),
                Some(Decision::Target(target)) => {
                    let due_ms = bar.end_ms.saturating_add(self.config.latency_ms);
                    self.submit_target(target, bar.close, due_ms);
                    None
                }
                None => None,
            };
            let closed_by_time_barrier = self.closed_trades().len() > trades_before
                && self
                    .closed_trades()
                    .last()
                    .is_some_and(|trade| trade.exit_reason == "TimeBarrier");
            if let Some(intent) = signal {
                if self.core.position().is_none()
                    && self.core.pending().is_none()
                    && self.core.pending_target().is_none()
                    && !closed_by_time_barrier
                {
                    let due_ms = bar.end_ms.saturating_add(self.config.latency_ms);
                    self.submit_entry(intent, bar.close, due_ms);
                }
            }
        }

        // 3. Mark-to-market and update peak equity / drawdown
        let total_val = self.total_equity(bar.close);
        if total_val > self.peak_equity {
            self.peak_equity = total_val;
        }
        self.daily.observe(bar.end_ms, total_val);

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
        if self.core.position().is_some() {
            self.open_excursion
                .get_or_insert_with(|| TradeExcursion::start(bar.end_ms, total_val))
                .observe_equity(bar.end_ms, total_val);
            if let Some(excursion) = self.open_excursion.as_mut() {
                excursion.observe_drawdown(dd_amount, dd_pct);
            }
        } else if self.closed_trades().len() > trades_before {
            if let Some(trade) = self.core.ledger_mut().last_mut() {
                trade.excursion.observe_drawdown(dd_amount, dd_pct);
            }
        }
    }

    /// Gates and sizes a signal exactly like the live gateway (at the signal price) and queues
    /// the market order; it fills at the open of the first event starting at or after `due_ms`.
    fn submit_entry(&mut self, intent: domain::OrderIntent, mark_price: Decimal, due_ms: i64) {
        // If daily circuit breaker is tripped, halt trading for the remainder of today
        if self.daily_circuit_breaker_tripped {
            self.count_rejection(DAILY_HALT_REJECTION);
            return;
        }

        if self.config.market_type == MarketType::Spot && intent.side == Side::Sell {
            self.count_rejection(SPOT_SHORTING_DISALLOWED);
            return;
        }

        let sized = self
            .current_daily_drawdown_pct_at(mark_price)
            .and_then(|drawdown| {
                let risk_policy = self.config.risk_policy()?;
                risk_policy.size_order(
                    &intent,
                    intent.price,
                    self.total_equity(mark_price),
                    self.config.risk_per_trade_pct,
                    Decimal::ZERO,
                    drawdown,
                )
            });
        match sized {
            Ok(quantity) => self
                .core
                .submit_due(PendingEntry::new(intent, quantity), due_ms),
            Err(error) => self.count_rejection(error.kind()),
        }
    }

    /// Gates and sizes a target at the decision price (strategy spec §11.3, §11.7, §11.8) and
    /// queues it in place of any pending target; it fills at the first event starting at or
    /// after `due_ms`.
    ///
    /// The re-arm rule drops a target on the side of the last forced exit until a flat or
    /// opposite target arrives. While the daily breaker is tripped a target may only reduce the
    /// position: an increase or a flip is counted as [`DAILY_HALT_REJECTION`], as an entry is.
    fn submit_target(&mut self, target: domain::TargetPosition, mark_price: Decimal, due_ms: i64) {
        if self.config.market_type == MarketType::Spot && target.level() < 0 {
            self.count_rejection(SPOT_SHORTING_DISALLOWED);
            return;
        }
        if self.core.intent().is_some() || self.core.pending().is_some() {
            self.count_rejection(UNSUPPORTED_DECISION_REJECTION);
            return;
        }
        if let Some(blocked) = self.core.rearm_blocked_side() {
            let same_side = matches!(
                (blocked, target.side()),
                (PositionSide::Long, Some(Side::Buy)) | (PositionSide::Short, Some(Side::Sell))
            );
            if same_side {
                self.count_rejection(REARM_IGNORED_REJECTION);
                return;
            }
            self.core.clear_rearm();
        }
        if target.level() == 0
            && self.bracket_priority
            && self.core.position().is_some()
            && self.active_exits().is_some_and(|e| e.take_profit.is_some())
        {
            return;
        }
        let tripped = self.daily_circuit_breaker_tripped;
        let sized = self
            .current_daily_drawdown_pct_at(mark_price)
            .and_then(|drawdown| {
                let risk_policy = self.config.risk_policy()?;
                // A tripped breaker is applied through the policy, which lets reductions pass.
                let drawdown = if tripped {
                    drawdown.max(risk_policy.max_daily_drawdown_pct)
                } else {
                    drawdown
                };
                risk_policy.size_target(
                    &target,
                    mark_price,
                    self.total_equity(mark_price),
                    self.core.signed_quantity(),
                    drawdown,
                )
            });
        match sized {
            Ok(sizing) => {
                if sizing.clipped {
                    self.count_rejection(POSITION_CAP_CLIP);
                }
                self.core.submit_target_due(
                    PendingTarget {
                        target,
                        quantity: sizing.quantity,
                        reference_price: mark_price,
                    },
                    due_ms,
                );
            }
            Err(domain::RiskError::CircuitBreakerTriggered { .. }) if tripped => {
                self.count_rejection(DAILY_HALT_REJECTION)
            }
            Err(error) => self.count_rejection(error.kind()),
        }
    }

    fn count_rejection(&mut self, reason: &str) {
        *self.rejected_orders.entry(reason.to_string()).or_insert(0) += 1;
    }

    /// Closes the open position through the core, consuming the open trade's excursion.
    fn execute_close(
        &mut self,
        exit_time: i64,
        exit_price: Decimal,
        slippage_per_unit: Decimal,
        is_taker: bool,
        exit_reason: &str,
    ) {
        self.core.execute_close(
            exit_time,
            exit_price,
            slippage_per_unit,
            is_taker,
            exit_reason,
            &self.config,
            &mut self.equity,
            &mut self.open_excursion,
        );
    }

    /// Concludes backtest simulation and compiles statistical performance metrics along with closed trades.
    pub fn finish_with_trades(
        mut self,
        last_mark_price: Option<Decimal>,
    ) -> (BacktestMetrics, Vec<ClosedTrade>) {
        if self.core.position().is_some() {
            if let Some(mark) = last_mark_price {
                let exit_time = self.last_bar_end_time.unwrap_or(0);
                self.execute_close(exit_time, mark, Decimal::ZERO, true, "EndOfSimulation");
            }
        }

        let mut metrics = BacktestMetrics::calculate(
            self.config.initial_capital,
            self.core.ledger().trades(),
            self.max_drawdown_amount,
            self.max_drawdown_pct,
        );
        metrics.halted = self.halted;
        metrics.rejected_orders = self.rejected_orders;
        (metrics, self.core.into_trades())
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
    fn a_signal_the_risk_policy_rejects_is_counted_by_reason() {
        // Sized at 10_000 * 1% / (100 - 95) = 20 units = 2_000 notional, over the 1_000 cap.
        let mut engine = BacktestEngine::new(BacktestConfig {
            max_order_notional: Some(dec!(1000)),
            ..realistic_engine().config
        });
        let mut strategy = Scripted::new(vec![long_intent(50), long_intent(50), None]);

        for t in [1000, 2000, 3000] {
            engine.process_bar(
                &mut strategy,
                &ohlc_bar(t, dec!(100), dec!(100), dec!(100), dec!(100)),
            );
        }

        assert!(engine.active_position().is_none());
        let metrics = engine.finish(Some(dec!(100)));
        assert_eq!(metrics.total_trades, 0);
        assert_eq!(
            metrics.rejected_orders,
            std::collections::BTreeMap::from([("OrderSizeExceeded".to_string(), 2)])
        );
    }

    /// Asks for a long target (1% risk, stop 5 away) on every bar.
    struct TargetOnly;

    impl Strategy for TargetOnly {
        fn name(&self) -> &str {
            "target_only"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<Decision> {
            let target =
                domain::TargetPosition::new(event.timestamp(), 1, dec!(0.01), dec!(5)).unwrap();
            Some(Decision::Target(target))
        }

        fn reset(&mut self) {}
    }

    /// Supersedes `a_target_decision_is_counted_as_unsupported_and_never_trades` (odd
    /// swarm-consensus T5.3): the bar engine now executes targets.
    #[test]
    fn a_target_decision_from_a_strategy_now_trades_through_process_bar() {
        let mut engine = realistic_engine();
        let mut strategy = TargetOnly;

        for t in [1000, 2000, 3000] {
            engine.process_bar(
                &mut strategy,
                &ohlc_bar(t, dec!(100), dec!(100), dec!(100), dec!(100)),
            );
        }

        // Sized at 10_000 * 1% / 5 = 20 and filled at the second bar's open, 100 + 1% slippage.
        let position = engine
            .active_position()
            .expect("the target opened a position");
        assert_eq!(position.quantity, dec!(20));
        assert_eq!(position.entry_price, dec!(101));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(96)));
        assert_eq!(
            engine.active_exits().map(|e| (e.take_profit, e.max_bars)),
            Some((None, None))
        );
        // The second bar re-sizes on 9_978.99 of marked equity: 19.957, a 0.043 delta worth 4.3,
        // below the 5 minimum notional, so no order is sent.
        let metrics = engine.finish(Some(dec!(100)));
        assert_eq!(metrics.total_trades, 1);
        assert_eq!(
            metrics.rejected_orders,
            std::collections::BTreeMap::from([("OrderBelowMinNotional".to_string(), 1)])
        );
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
        engine.core.set_position_for_test(
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
            engine.active_position().is_none(),
            "the order fills on the next bar"
        );

        // Next bar: the market order fills at its open (flat bar, no barrier is touched).
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position().is_some());

        // Rebound back above mean (100) -> Take profit hit!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(95), dec!(102), dec!(94), dec!(100)),
        );
        assert!(engine.active_position().is_none());

        let metrics = engine.finish(None);
        assert_eq!(metrics.total_trades, 1);
        assert_eq!(metrics.winning_trades, 1);
        assert!(metrics.net_profit > Decimal::ZERO);
        assert!(metrics.total_fees > Decimal::ZERO);
        // A lone winner has no gross loss: the profit factor is undefined, not capped.
        assert_eq!(metrics.profit_factor, None);
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
            engine.active_position().is_none(),
            "the order fills on the next bar"
        );

        // Next bar: the market order fills at its open (flat bar, no barrier is touched).
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position().is_some());

        // Further crash to 70 -> hits Stop Loss!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(80), dec!(81), dec!(69), dec!(70)),
        );
        assert!(engine.active_position().is_none());

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
        assert!(engine.active_position().is_none());

        // The market order fills at this bar's open: bar 1 of the hold (within barrier)
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(91), dec!(92), dec!(89), dec!(91)),
        );
        assert!(engine.active_position().is_some());
        assert_eq!(engine.bars_held(), 1);

        // Bar 2 of hold -> reaches time_barrier_bars (2) -> Forced market close!
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(91), dec!(92), dec!(89), dec!(91)),
        );
        assert!(engine.active_position().is_none());

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
        assert!(engine.active_position().is_none());

        // The order fills at the next open: fill_price = 90 + (90 * 0.01) = 90.90
        engine.process_bar(
            &mut strategy,
            &create_test_bar(6000, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position().is_some());

        // Rebound back above mean (100) -> Take profit hit: a taker exit that pays slippage too.
        engine.process_bar(
            &mut strategy,
            &create_test_bar(7000, dec!(95), dec!(102), dec!(94), dec!(100)),
        );
        assert!(engine.active_position().is_none());

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
        assert!(engine.active_position().is_none());
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 4500, dec!(90), dec!(90), dec!(90), dec!(90)),
        );
        assert!(engine.active_position().is_some());

        // Day 1: Crash through the stop without a gap (it opens at the entry price) -> the stop
        // fills near its trigger and the loss exceeds 5% of the account, tripping the breaker.
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 5000, dec!(90), dec!(90), dec!(69), dec!(70)),
        );
        assert!(engine.active_position().is_none());
        assert!(engine.daily_circuit_breaker_tripped);

        // Day 1: Another plunge to 60 -> signal generated by strategy but REJECTED by daily circuit breaker
        engine.process_bar(
            &mut strategy,
            &create_test_bar(day_1_ms + 6000, dec!(60), dec!(61), dec!(59), dec!(60)),
        );
        assert!(
            engine.active_position().is_none(),
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
            engine.active_position().is_some(),
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

    fn trade_event(timestamp: i64, price: Decimal) -> PriceEvent {
        PriceEvent::from(&domain::Trade {
            timestamp,
            price,
            quantity: dec!(1),
            side: Side::Buy,
        })
    }

    /// Feeds trade events at `times`; the event at `times[0]` decides one long entry.
    fn run_scripted_events(latency_ms: i64, times: &[(i64, Decimal)]) -> Vec<Option<Position>> {
        let mut engine = BacktestEngine::new(BacktestConfig {
            latency_ms,
            slippage_pct: dec!(0),
            max_daily_drawdown_pct: dec!(0.50),
            ..BacktestConfig::default()
        });
        let mut entry = Some(Decision::Entry(
            domain::OrderIntent::new(times[0].0, Side::Buy, times[0].1, dec!(90), dec!(200), 50)
                .unwrap(),
        ));
        times
            .iter()
            .map(|&(t, price)| {
                engine.process_event(&trade_event(t, price), &mut || entry.take());
                engine.active_position().cloned()
            })
            .collect()
    }

    #[test]
    fn a_decision_is_never_filled_on_the_event_that_made_it() {
        let positions = run_scripted_events(0, &[(1_000, dec!(100)), (1_000, dec!(101))]);
        assert!(positions[0].is_none());
        assert_eq!(
            positions[1].as_ref().map(|p| p.entry_price),
            Some(dec!(101))
        );
    }

    #[test]
    fn a_latency_delays_the_fill_to_the_first_event_at_or_after_the_due_time() {
        let positions = run_scripted_events(
            100,
            &[
                (1_000, dec!(100)),
                (1_050, dec!(101)),
                (1_099, dec!(102)),
                (1_100, dec!(103)),
                (1_200, dec!(104)),
            ],
        );
        assert!(positions[..3].iter().all(Option::is_none));
        assert_eq!(
            positions[3].as_ref().map(|p| p.entry_price),
            Some(dec!(103))
        );
        assert_eq!(
            positions[4].as_ref().map(|p| p.entry_price),
            Some(dec!(103))
        );
    }

    // ---- Decision::Target execution (odd swarm-consensus T5.3, strategy spec §11) ----

    /// Frictionless-slippage config for target tests: 0.05% taker fee, no slippage, loose
    /// breakers, and notional caps far above every test order.
    fn target_config() -> BacktestConfig {
        BacktestConfig {
            initial_capital: dec!(10_000),
            slippage_pct: Decimal::ZERO,
            taker_fee_pct: dec!(0.0005),
            max_daily_drawdown_pct: dec!(0.50),
            max_monthly_drawdown_pct: dec!(0.95),
            max_order_notional: Some(dec!(1_000_000)),
            max_position_notional: Some(dec!(1_000_000)),
            ..BacktestConfig::default()
        }
    }

    /// Event `i` of a scripted tape: it starts at `i` seconds and ends 500 ms later.
    fn ev(i: i64, open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> PriceEvent {
        PriceEvent {
            start_ms: i * 1_000,
            end_ms: i * 1_000 + 500,
            open,
            high,
            low,
            close,
            ..Default::default()
        }
    }

    fn flat_ev(i: i64, price: Decimal) -> PriceEvent {
        ev(i, price, price, price, price)
    }

    fn target(level: i8, risk: Decimal, stop_distance: Decimal) -> Option<Decision> {
        Some(Decision::Target(
            domain::TargetPosition::new(0, level, risk, stop_distance).unwrap(),
        ))
    }

    fn flat_target() -> Option<Decision> {
        target(0, Decimal::ZERO, Decimal::ZERO)
    }

    /// Feeds each event with the decision scripted for it.
    fn run(engine: &mut BacktestEngine, script: Vec<(PriceEvent, Option<Decision>)>) {
        for (event, decision) in script {
            let mut decision = decision;
            engine.process_event(&event, &mut || decision.take());
        }
    }

    fn counters(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn a_target_is_sized_as_equity_times_risk_over_stop_floored_to_the_step() {
        let mut engine = BacktestEngine::new(target_config());
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(3))),
                (flat_ev(2, dec!(100)), None),
            ],
        );
        // 10_000 * 1% / 3 = 33.333..., floored to 33.333; filled at the next open, 100.
        let position = engine.active_position().unwrap();
        assert_eq!(position.side, PositionSide::Long);
        assert_eq!(position.quantity, dec!(33.333));
        assert_eq!(position.entry_price, dec!(100));
        // Fee 100 * 33.333 * 0.0005 = 1.66665.
        assert_eq!(engine.cash_equity(), dec!(9998.33335));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(97)));
        assert!(engine.closed_trades().is_empty());
        assert!(engine.rejected_orders.is_empty());
    }

    #[test]
    fn the_position_cap_clips_a_target_and_the_clipped_target_still_trades() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            max_position_notional: Some(dec!(1_000)),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(3))),
                (flat_ev(2, dec!(100)), None),
            ],
        );
        // 33.333 * 100 = 3_333.3 > 1_000: clipped to 1_000 / 100 = 10.
        assert_eq!(engine.active_position().unwrap().quantity, dec!(10));
        assert_eq!(engine.cash_equity(), dec!(9999.5));
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::POSITION_CAP_CLIP, 1)])
        );
    }

    #[test]
    fn a_dust_delta_is_skipped_but_a_flat_target_always_closes() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            min_order_notional: dec!(50),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                // 10_000 * 1% / 90 = 1.111 (notional 111.1).
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(90))),
                // Equity 9_999.94445 * 1.01% / 90 = 1.122: a delta of 0.011, worth 1.1 < 50.
                (flat_ev(2, dec!(100)), target(1, dec!(0.0101), dec!(90))),
                // The price falls to 40 (stop at 10): the position is worth 44.44 < 50.
                (
                    ev(3, dec!(100), dec!(100), dec!(40), dec!(40)),
                    flat_target(),
                ),
                (flat_ev(4, dec!(40)), None),
            ],
        );
        assert!(engine.active_position().is_none());
        // 10_000 - 0.05555 (fee at 100) - 66.66 (1.111 * -60) - 0.02222 (fee at 40).
        assert_eq!(engine.cash_equity(), dec!(9933.26223));
        let trade = &engine.closed_trades()[0];
        assert_eq!(engine.closed_trades().len(), 1);
        assert_eq!(trade.exit_reason, "TargetExit");
        assert_eq!(trade.side, PositionSide::Long);
        assert_eq!(trade.entry_price, dec!(100));
        assert_eq!(trade.exit_price, dec!(40));
        assert_eq!(trade.quantity, dec!(1.111));
        assert_eq!(trade.pnl_gross, dec!(-66.66));
        assert_eq!(trade.fees_paid, dec!(0.07777));
        assert_eq!(trade.pnl_net, dec!(-66.73777));
        assert_eq!(trade.entry_time, 2_000);
        assert_eq!(trade.exit_time, 4_000);
        assert_eq!(
            engine.rejected_orders,
            counters(&[("OrderBelowMinNotional", 1)])
        );
    }

    #[test]
    fn a_scale_in_averages_the_entry_and_moves_the_stop_to_the_last_fill() {
        let mut engine = BacktestEngine::new(target_config());
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), None),
                // Equity 9_998.75 + 4 * 25 = 10_098.75; * 2% / 4 = 50.493.
                (flat_ev(3, dec!(104)), target(2, dec!(0.02), dec!(4))),
                (flat_ev(4, dec!(104)), None),
            ],
        );
        let position = engine.active_position().unwrap();
        assert_eq!(position.quantity, dec!(50.493));
        // (25 * 100 + 25.493 * 104) / 50.493.
        assert_eq!(position.entry_price, dec!(5151.272) / dec!(50.493));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(100)));
        // 10_000 - 1.25 - 104 * 25.493 * 0.0005 (1.325636).
        assert_eq!(engine.cash_equity(), dec!(9997.424364));
        assert!(engine.closed_trades().is_empty());
    }

    #[test]
    fn a_scale_out_realises_pnl_and_the_campaign_closes_once() {
        let mut engine = BacktestEngine::new(target_config());
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), None),
                // Equity 9_998.75 + 250 = 10_248.75; * 0.5% / 4 = 12.810: sell 12.19.
                (flat_ev(3, dec!(110)), target(1, dec!(0.005), dec!(4))),
                (flat_ev(4, dec!(110)), None),
            ],
        );
        let position = engine.active_position().unwrap();
        assert_eq!(position.quantity, dec!(12.810));
        assert_eq!(position.entry_price, dec!(100));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(106)));
        // 9_998.75 + 12.19 * 10 (121.9) - 110 * 12.19 * 0.0005 (0.67045).
        assert_eq!(engine.cash_equity(), dec!(10119.97955));
        assert!(engine.closed_trades().is_empty());

        let (metrics, trades) = engine.finish_with_trades(Some(dec!(110)));
        assert_eq!(metrics.total_trades, 1);
        let trade = &trades[0];
        assert_eq!(trade.exit_reason, "EndOfSimulation");
        assert_eq!(trade.quantity, dec!(25));
        assert_eq!(trade.entry_price, dec!(100));
        assert_eq!(trade.exit_price, dec!(110));
        assert_eq!(trade.pnl_gross, dec!(250));
        // 1.25 + 0.67045 + 110 * 12.81 * 0.0005 (0.70455).
        assert_eq!(trade.fees_paid, dec!(2.625));
        assert_eq!(trade.pnl_net, dec!(247.375));
        assert_eq!(trade.return_pct, dec!(247.375) / dec!(2500));
    }

    #[test]
    fn a_flip_is_two_fills_and_two_campaigns() {
        let mut engine = BacktestEngine::new(target_config());
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                // Equity 9_998.75 * 1% / 4 = 24.996: short 24.996.
                (flat_ev(2, dec!(100)), target(-1, dec!(0.01), dec!(4))),
                (flat_ev(3, dec!(100)), flat_target()),
                (flat_ev(4, dec!(100)), None),
            ],
        );
        let trades = engine.closed_trades();
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].exit_reason, "TargetFlip");
        assert_eq!(trades[0].side, PositionSide::Long);
        assert_eq!(trades[0].quantity, dec!(25));
        assert_eq!(trades[0].fees_paid, dec!(2.5));
        assert_eq!(trades[0].pnl_net, dec!(-2.5));
        assert_eq!(trades[0].exit_time, 3_000);
        assert_eq!(trades[1].exit_reason, "TargetExit");
        assert_eq!(trades[1].side, PositionSide::Short);
        assert_eq!(trades[1].quantity, dec!(24.996));
        assert_eq!(trades[1].entry_time, 3_000);
        // Opening and closing 24.996 at 100: 1.2498 each.
        assert_eq!(trades[1].fees_paid, dec!(2.4996));
        assert!(engine.active_position().is_none());
        assert_eq!(
            engine.cash_equity(),
            dec!(10_000) - dec!(2.5) - dec!(2.4996)
        );
    }

    #[test]
    fn the_flip_opens_the_new_side_with_its_own_stop_on_the_same_event() {
        let mut engine = BacktestEngine::new(target_config());
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), target(-1, dec!(0.01), dec!(4))),
                (flat_ev(3, dec!(100)), None),
            ],
        );
        let position = engine.active_position().unwrap();
        assert_eq!(position.side, PositionSide::Short);
        assert_eq!(position.quantity, dec!(24.996));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(104)));
        assert_eq!(engine.closed_trades().len(), 1);
    }

    #[test]
    fn a_stop_gapped_through_fills_a_target_at_the_open() {
        let mut engine = BacktestEngine::new(target_config());
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), None),
                // Stop 96; the event opens at 95, through it.
                (ev(3, dec!(95), dec!(95), dec!(94), dec!(95)), None),
            ],
        );
        assert!(engine.active_position().is_none());
        let trade = &engine.closed_trades()[0];
        assert_eq!(trade.exit_reason, "StopLoss");
        assert_eq!(trade.exit_price, dec!(95));
        assert_eq!(trade.pnl_gross, dec!(-125));
        // 1.25 + 95 * 25 * 0.0005 (1.1875).
        assert_eq!(trade.fees_paid, dec!(2.4375));
        assert_eq!(engine.cash_equity(), dec!(9872.5625));
    }

    #[test]
    fn a_target_gapping_through_its_liquidation_price_is_liquidated_before_the_stop() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            initial_capital: dec!(1_000),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                // 1_000 * 10% / 1 = 100 units (10_000 notional): 11x, margin 909.09, liq 91.31.
                (flat_ev(1, dec!(100)), target(1, dec!(0.1), dec!(1))),
                (flat_ev(2, dec!(100)), None),
                (ev(3, dec!(80), dec!(81), dec!(79), dec!(80)), None),
            ],
        );
        let trade = &engine.closed_trades()[0];
        assert_eq!(trade.exit_reason, "Liquidation");
        assert_eq!(trade.exit_price.round_dp(4), dec!(90.9091));
        assert_eq!(trade.fees_paid, dec!(5));
        let margin = dec!(10_000) / dec!(11);
        assert_eq!(trade.pnl_gross.round_dp(6), (-margin).round_dp(6));
        assert_eq!(
            engine.cash_equity().round_dp(6),
            (dec!(995) - margin).round_dp(6)
        );
    }

    #[test]
    fn a_target_whose_liquidation_lies_before_its_stop_is_reduced_until_it_does_not() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            initial_capital: dec!(1_000),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                // 1_000 * 100% / 10 = 100 units: 11x puts the liquidation at 91.31, above the
                // stop at 90. The largest size at 9x (liquidation 89.29) is 89.596.
                (flat_ev(1, dec!(100)), target(1, dec!(1), dec!(10))),
                (flat_ev(2, dec!(100)), None),
            ],
        );
        let position = engine.active_position().unwrap();
        assert_eq!(position.quantity, dec!(89.596));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(90)));
        // Fee 100 * 89.596 * 0.0005 = 4.4798.
        assert_eq!(engine.cash_equity(), dec!(995.5202));
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::STOP_BEYOND_LIQUIDATION_CLIP, 1)])
        );
    }

    /// Opens 25 long at 100 (stop 96) and stops it out at 95 on event 3.
    fn stopped_out_script() -> Vec<(PriceEvent, Option<Decision>)> {
        vec![
            (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
            (flat_ev(2, dec!(100)), None),
            (ev(3, dec!(95), dec!(95), dec!(94), dec!(95)), None),
        ]
    }

    #[test]
    fn after_a_stop_same_side_targets_are_ignored_until_an_opposite_target() {
        let mut engine = BacktestEngine::new(target_config());
        let mut script = stopped_out_script();
        script.extend([
            (flat_ev(4, dec!(95)), target(1, dec!(0.01), dec!(4))),
            (flat_ev(5, dec!(95)), target(2, dec!(0.02), dec!(4))),
            (flat_ev(6, dec!(95)), target(-1, dec!(0.01), dec!(4))),
            (flat_ev(7, dec!(95)), None),
        ]);
        run(&mut engine, script);
        let position = engine.active_position().unwrap();
        assert_eq!(position.side, PositionSide::Short);
        assert_eq!(engine.closed_trades().len(), 1);
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::REARM_IGNORED_REJECTION, 2)])
        );
        // Re-armed: a long target is accepted again (it flips the short).
        run(
            &mut engine,
            vec![
                (flat_ev(8, dec!(95)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(9, dec!(95)), None),
            ],
        );
        assert_eq!(
            engine.active_position().map(|p| p.side),
            Some(PositionSide::Long)
        );
    }

    #[test]
    fn after_a_stop_a_flat_target_re_arms_the_same_side() {
        let mut engine = BacktestEngine::new(target_config());
        let mut script = stopped_out_script();
        script.extend([
            (flat_ev(4, dec!(95)), flat_target()),
            (flat_ev(5, dec!(95)), target(1, dec!(0.01), dec!(4))),
            (flat_ev(6, dec!(95)), None),
        ]);
        run(&mut engine, script);
        let position = engine.active_position().unwrap();
        assert_eq!(position.side, PositionSide::Long);
        assert_eq!(position.entry_price, dec!(95));
        assert!(engine.rejected_orders.is_empty());
    }

    #[test]
    fn a_daily_halt_blocks_target_increases_and_flips_but_allows_reductions() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            max_daily_drawdown_pct: dec!(0.02),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                // 10_000 * 5% / 4 = 125 units at 100 (fee 6.25).
                (flat_ev(1, dec!(100)), target(1, dec!(0.05), dec!(4))),
                (flat_ev(2, dec!(100)), None),
                // Marked at 98: 9_743.75, 2.56% below the day's peak: the breaker trips.
                (flat_ev(3, dec!(98)), None),
                (flat_ev(4, dec!(98)), target(2, dec!(0.08), dec!(4))),
                (flat_ev(5, dec!(98)), target(-1, dec!(0.01), dec!(4))),
                // 9_743.75 * 2% / 4 = 48.718: a reduction of 76.282.
                (flat_ev(6, dec!(98)), target(1, dec!(0.02), dec!(4))),
                (flat_ev(7, dec!(98)), None),
            ],
        );
        let position = engine.active_position().unwrap();
        assert_eq!(position.side, PositionSide::Long);
        assert_eq!(position.quantity, dec!(48.718));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(94)));
        // 9_993.75 - 76.282 * 2 (152.564) - 98 * 76.282 * 0.0005 (3.737818).
        assert_eq!(engine.cash_equity(), dec!(9837.448182));
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::DAILY_HALT_REJECTION, 2)])
        );
    }

    /// Four fills, one campaign, 0.1% slippage and 0.05% taker fee. Hand arithmetic:
    ///
    /// - e2 buys 25 at 100.1 (slip 0.1): fee 2_502.5 * 0.0005 = 1.25125; cash 9_998.74875.
    ///   Marked at 100: 9_996.24875; * 2% / 4 = 49.981.
    /// - e3 buys 24.981 at 102.102 (slip 0.102): 2_550.610062, fee 1.275305031; cash
    ///   9_997.473444969. Cost of the 49.981: 5_053.110062. Marked at 102 the equity sizes
    ///   0.5% / 4 to 12.553.
    /// - e4 sells 37.428 at 103.896 (slip 0.104): 3_888.619488, fee 1.944309744.
    /// - e5 sells 12.553 at 105.894 (slip 0.106): 1_329.287382, fee 0.664643691.
    /// - PnL = 3_888.619488 + 1_329.287382 - 5_053.110062 = 164.796808; fees 5.135508466;
    ///   cash = 10_000 + 164.796808 - 5.135508466 = 10_159.661299534.
    /// - Slippage: 2.5 + 52.510062 (the e3 buy also reports the 2-point adverse gap from its
    ///   decision close of 100 to the open of 102: 2.102 * 24.981) + 3.892512 + 1.330618 =
    ///   60.233192.
    #[test]
    fn a_multi_fill_campaign_is_one_trade_and_cash_reconciles_to_the_cent() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            slippage_pct: dec!(0.001),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), target(2, dec!(0.02), dec!(4))),
                (flat_ev(3, dec!(102)), target(1, dec!(0.005), dec!(4))),
                (flat_ev(4, dec!(104)), flat_target()),
                (flat_ev(5, dec!(106)), None),
            ],
        );
        assert!(engine.active_position().is_none());
        let trades = engine.closed_trades();
        assert_eq!(trades.len(), 1);
        let trade = &trades[0];
        assert_eq!(trade.exit_reason, "TargetExit");
        assert_eq!(trade.side, PositionSide::Long);
        assert_eq!(trade.entry_time, 2_000);
        assert_eq!(trade.exit_time, 5_000);
        assert_eq!(trade.holding_duration_seconds, 3);
        assert_eq!(trade.quantity, dec!(49.981));
        assert_eq!(trade.entry_price, dec!(5053.110062) / dec!(49.981));
        assert_eq!(trade.exit_price, dec!(5217.906870) / dec!(49.981));
        assert_eq!(trade.pnl_gross.round_dp(9), dec!(164.796808));
        assert_eq!(trade.fees_paid, dec!(5.135508466));
        assert_eq!(trade.slippage_paid, dec!(60.233192));
        assert_eq!(trade.funding_paid, Decimal::ZERO);
        assert_eq!(trade.pnl_net.round_dp(9), dec!(159.661299534));
        assert_eq!(engine.cash_equity().round_dp(2), dec!(10159.66));
        assert_eq!(engine.cash_equity().round_dp(9), dec!(10159.661299534));
        assert_eq!(engine.cash_equity(), dec!(10_000) + trade.pnl_net);
    }

    #[test]
    fn an_entry_is_ignored_while_a_target_is_pending_or_open() {
        let mut engine = BacktestEngine::new(target_config());
        let entry = || {
            Some(Decision::Entry(
                domain::OrderIntent::new(0, Side::Buy, dec!(100), dec!(95), dec!(110), 5).unwrap(),
            ))
        };
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), entry()),
                (flat_ev(3, dec!(100)), None),
            ],
        );
        assert!(engine.pending_intent().is_none());
        assert!(engine.active_intent().is_none());
        assert_eq!(engine.active_position().unwrap().quantity, dec!(25));
        assert_eq!(engine.active_exits().map(|e| e.take_profit), Some(None));

        // An entry decided while a target is still pending (100 ms latency) is ignored too.
        let mut engine = BacktestEngine::new(BacktestConfig {
            latency_ms: 10_000,
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), entry()),
            ],
        );
        assert!(engine.pending_intent().is_none());
        assert!(engine.pending_target().is_some());
    }

    #[test]
    fn a_newer_target_replaces_the_pending_one() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            latency_ms: 2_000,
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(100)), target(1, dec!(0.01), dec!(2))),
                (flat_ev(3, dec!(100)), None),
                (flat_ev(4, dec!(100)), None),
                (flat_ev(5, dec!(100)), None),
            ],
        );
        // Only the second target (10_000 * 1% / 2 = 50) fills, at its own due time.
        assert_eq!(engine.active_position().unwrap().quantity, dec!(50));
        assert_eq!(engine.active_exits().map(|e| e.stop), Some(dec!(98)));
    }

    #[test]
    fn the_monthly_kill_switch_flattens_a_target_campaign() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            max_monthly_drawdown_pct: dec!(0.01),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.05), dec!(10))),
                (flat_ev(2, dec!(100)), None),
                // 50 units marked at 97: -150, 1.5% of the month's open equity.
                (ev(3, dec!(100), dec!(100), dec!(97), dec!(97)), None),
            ],
        );
        assert!(engine.is_halted());
        let trade = &engine.closed_trades()[0];
        assert_eq!(trade.exit_reason, "MonthlyCircuitBreaker");
        assert_eq!(trade.exit_price, dec!(97));
        assert_eq!(trade.quantity, dec!(50));
    }

    // ---- T5.4 hardening (odd swarm-consensus): regression guards for Phase 5 advisories ----

    #[test]
    fn margin_and_liquidation_price_are_recomputed_after_a_scale_in_and_a_scale_out() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            initial_capital: dec!(1_000),
            ..target_config()
        });
        // 1_000 * 10% / 10 = 10 units (1_000 notional): 2x, margin 500,
        // liquidation 100 - (500 - 4) / 10 = 50.4.
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.1), dec!(10))),
                (flat_ev(2, dec!(100)), None),
            ],
        );
        assert_eq!(engine.core.margin(), dec!(500));
        assert_eq!(
            engine.core.liquidation_price(&engine.config),
            Some(dec!(50.4))
        );

        // Scale-in: 999.5 * 30% / 10 = 29.985 units (2_998.5 notional): 4x, margin 749.625,
        // liquidation 100 - (749.625 - 11.994) / 29.985 = 75.4. The liquidation moves up.
        run(
            &mut engine,
            vec![
                (flat_ev(2, dec!(100)), target(2, dec!(0.3), dec!(10))),
                (flat_ev(3, dec!(100)), None),
            ],
        );
        assert_eq!(engine.active_position().unwrap().quantity, dec!(29.985));
        assert_eq!(engine.core.margin(), dec!(749.625));
        assert_eq!(
            engine.core.liquidation_price(&engine.config),
            Some(dec!(75.4))
        );

        // Scale-out: 998.50075 * 10% / 10 = 9.985 units (998.5 notional): 2x, margin 499.25,
        // liquidation 100 - (499.25 - 3.994) / 9.985 = 50.4. The liquidation moves back down.
        run(
            &mut engine,
            vec![
                (flat_ev(3, dec!(100)), target(1, dec!(0.1), dec!(10))),
                (flat_ev(4, dec!(100)), None),
            ],
        );
        assert_eq!(engine.active_position().unwrap().quantity, dec!(9.985));
        assert_eq!(engine.core.margin(), dec!(499.25));
        assert_eq!(
            engine.core.liquidation_price(&engine.config),
            Some(dec!(50.4))
        );
    }

    #[test]
    fn a_target_fills_at_the_first_event_at_or_after_its_latency_due_time() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            latency_ms: 100,
            ..target_config()
        });
        let script = vec![
            (
                trade_event(1_000, dec!(100)),
                target(1, dec!(0.01), dec!(4)),
            ),
            (trade_event(1_050, dec!(101)), None),
            (trade_event(1_099, dec!(102)), None),
            (trade_event(1_100, dec!(103)), None),
        ];
        let mut positions = Vec::new();
        for (event, decision) in script {
            run(&mut engine, vec![(event, decision)]);
            positions.push(engine.active_position().cloned());
        }
        assert!(positions[..3].iter().all(Option::is_none));
        let opened = positions[3].as_ref().unwrap();
        assert_eq!(opened.quantity, dec!(25));
        assert_eq!(opened.entry_price, dec!(103));

        // A flat target decided at 1_200 closes at the first event at or after 1_300.
        for (t, price) in [(1_250, dec!(105)), (1_299, dec!(106))] {
            if t == 1_250 {
                run(
                    &mut engine,
                    vec![(trade_event(1_200, dec!(104)), flat_target())],
                );
            }
            run(&mut engine, vec![(trade_event(t, price), None)]);
            assert!(engine.active_position().is_some());
            assert!(engine.closed_trades().is_empty());
        }
        run(&mut engine, vec![(trade_event(1_300, dec!(107)), None)]);
        assert!(engine.active_position().is_none());
        let trade = &engine.closed_trades()[0];
        assert_eq!(trade.exit_reason, "TargetExit");
        assert_eq!(trade.entry_time, 1_100);
        assert_eq!(trade.exit_time, 1_300);
        assert_eq!(trade.exit_price, dec!(107));
    }

    /// Opens 100 long at 100 on 1_000 of capital (11x, liquidation 91.31) and liquidates it on
    /// event 3, which gaps to 80. The breakers are loose enough that the 91% loss trips none.
    fn liquidated_script() -> (BacktestEngine, Vec<(PriceEvent, Option<Decision>)>) {
        let engine = BacktestEngine::new(BacktestConfig {
            initial_capital: dec!(1_000),
            max_daily_drawdown_pct: dec!(0.99),
            max_monthly_drawdown_pct: dec!(0.99),
            ..target_config()
        });
        let script = vec![
            (flat_ev(1, dec!(100)), target(1, dec!(0.1), dec!(1))),
            (flat_ev(2, dec!(100)), None),
            (ev(3, dec!(80), dec!(81), dec!(79), dec!(80)), None),
        ];
        (engine, script)
    }

    #[test]
    fn after_a_liquidation_same_side_targets_are_ignored_until_a_flat_target() {
        let (mut engine, mut script) = liquidated_script();
        script.extend([
            (flat_ev(4, dec!(80)), target(1, dec!(0.01), dec!(4))),
            (flat_ev(5, dec!(80)), flat_target()),
            (flat_ev(6, dec!(80)), target(1, dec!(0.01), dec!(4))),
            (flat_ev(7, dec!(80)), None),
        ]);
        run(&mut engine, script);
        assert_eq!(engine.closed_trades()[0].exit_reason, "Liquidation");
        assert_eq!(
            engine.active_position().map(|p| p.side),
            Some(PositionSide::Long)
        );
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::REARM_IGNORED_REJECTION, 1)])
        );
    }

    #[test]
    fn after_a_liquidation_an_opposite_target_re_arms() {
        let (mut engine, mut script) = liquidated_script();
        script.extend([
            (flat_ev(4, dec!(80)), target(1, dec!(0.01), dec!(4))),
            (flat_ev(5, dec!(80)), target(-1, dec!(0.01), dec!(4))),
            (flat_ev(6, dec!(80)), None),
        ]);
        run(&mut engine, script);
        assert_eq!(
            engine.active_position().map(|p| p.side),
            Some(PositionSide::Short)
        );
        assert_eq!(engine.core.rearm_blocked_side(), None);
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::REARM_IGNORED_REJECTION, 1)])
        );
    }

    #[test]
    fn a_monthly_kill_switch_flatten_blocks_the_side_until_a_flat_or_opposite_target() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            max_monthly_drawdown_pct: dec!(0.01),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.05), dec!(10))),
                (flat_ev(2, dec!(100)), None),
                (ev(3, dec!(100), dec!(100), dec!(97), dec!(97)), None),
            ],
        );
        assert_eq!(
            engine.closed_trades()[0].exit_reason,
            "MonthlyCircuitBreaker"
        );
        assert_eq!(engine.core.rearm_blocked_side(), Some(PositionSide::Long));
        // The kill switch halts the run for good, so the submission gate is driven directly.
        let long = domain::TargetPosition::new(0, 1, dec!(0.01), dec!(4)).unwrap();
        let short = domain::TargetPosition::new(0, -1, dec!(0.01), dec!(4)).unwrap();
        let flat = domain::TargetPosition::new(0, 0, Decimal::ZERO, Decimal::ZERO).unwrap();
        engine.submit_target(long, dec!(97), 10_000);
        assert!(engine.pending_target().is_none());
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::REARM_IGNORED_REJECTION, 1)])
        );
        engine.submit_target(short, dec!(97), 10_000);
        assert_eq!(engine.core.rearm_blocked_side(), None);
        assert!(engine.pending_target().is_some());

        // A flat target re-arms the same way.
        let mut engine = BacktestEngine::new(BacktestConfig {
            max_monthly_drawdown_pct: dec!(0.01),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.05), dec!(10))),
                (flat_ev(2, dec!(100)), None),
                (ev(3, dec!(100), dec!(100), dec!(97), dec!(97)), None),
            ],
        );
        engine.submit_target(long, dec!(97), 10_000);
        engine.submit_target(flat, dec!(97), 10_000);
        assert_eq!(engine.core.rearm_blocked_side(), None);
        engine.submit_target(long, dec!(97), 10_000);
        assert!(engine
            .pending_target()
            .is_some_and(|p| p.quantity > Decimal::ZERO));
        assert_eq!(
            engine.rejected_orders,
            counters(&[(crate::metrics::REARM_IGNORED_REJECTION, 1)])
        );
    }

    #[test]
    fn a_target_fill_reports_the_adverse_gap_from_the_decision_price_plus_model_slippage() {
        let config = BacktestConfig {
            slippage_pct: dec!(0.001),
            ..target_config()
        };
        // Long decided at a close of 100, filled at an open of 102: 25 at 102.102. Slippage per
        // unit is the 2-point adverse gap plus 0.102 (2.102 * 25 = 52.55); the close at an open
        // of 102 pays 0.102 * 25 = 2.55 (market-exit convention, no gap term).
        let mut engine = BacktestEngine::new(config);
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(102)), flat_target()),
                (flat_ev(3, dec!(102)), None),
            ],
        );
        let trade = &engine.closed_trades()[0];
        assert_eq!(trade.slippage_paid, dec!(55.1));
        // Cash is unchanged by the report: (101.898 - 102.102) * 25 minus both fees.
        assert_eq!(trade.pnl_gross, dec!(-5.1));
        assert_eq!(trade.fees_paid, dec!(2.55));
        assert_eq!(engine.cash_equity(), dec!(9992.35));
        assert_eq!(engine.cash_equity(), dec!(10_000) + trade.pnl_net);

        // Short decided at 100, filled at 98: 25 at 97.902, slippage (2 + 0.098) * 25 = 52.45,
        // then bought back at 98.098: 0.098 * 25 = 2.45.
        let mut engine = BacktestEngine::new(config);
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(-1, dec!(0.01), dec!(4))),
                (flat_ev(2, dec!(98)), flat_target()),
                (flat_ev(3, dec!(98)), None),
            ],
        );
        let trade = &engine.closed_trades()[0];
        assert_eq!(trade.slippage_paid, dec!(54.9));
        assert_eq!(engine.cash_equity(), dec!(10_000) + trade.pnl_net);
    }

    #[test]
    fn a_flip_whose_opening_leg_is_dust_closes_as_a_target_exit() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            min_order_notional: dec!(50),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                // 9_998.75 * 0.01% / 4 = 0.249 short, worth 24.9 < 50: the opening leg is dust.
                (flat_ev(2, dec!(100)), target(-1, dec!(0.0001), dec!(4))),
                (flat_ev(3, dec!(100)), None),
            ],
        );
        assert!(engine.active_position().is_none());
        let trades = engine.closed_trades();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "TargetExit");
        assert_eq!(trades[0].side, PositionSide::Long);
        assert_eq!(engine.cash_equity(), dec!(9997.5));
        assert_eq!(
            engine.rejected_orders,
            counters(&[("OrderBelowMinNotional", 1)])
        );
    }

    #[test]
    fn a_flip_whose_opening_leg_exceeds_the_order_cap_closes_as_a_target_exit() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            max_order_notional: Some(dec!(3_000)),
            ..target_config()
        });
        run(
            &mut engine,
            vec![
                (flat_ev(1, dec!(100)), target(1, dec!(0.01), dec!(4))),
                // 9_998.75 * 2% / 4 = 49.993 short, worth 4_999.3 > 3_000.
                (flat_ev(2, dec!(100)), target(-1, dec!(0.02), dec!(4))),
                (flat_ev(3, dec!(100)), None),
            ],
        );
        assert!(engine.active_position().is_none());
        let trades = engine.closed_trades();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "TargetExit");
        assert_eq!(
            engine.rejected_orders,
            counters(&[("OrderSizeExceeded", 1)])
        );
    }

    #[test]
    fn spot_engine_rejects_short_entry_and_records_rejection_counter() {
        let mut engine = BacktestEngine::new(BacktestConfig::spot());
        let sell_intent =
            domain::OrderIntent::new(1, Side::Sell, dec!(100), dec!(105), dec!(95), 5).unwrap();

        engine.submit_entry(sell_intent, dec!(100), 2_000);
        assert_eq!(
            engine.rejected_orders.get(SPOT_SHORTING_DISALLOWED),
            Some(&1)
        );
        assert!(engine.pending_intent().is_none());

        // Now submit a valid Buy intent in Spot
        let buy_intent =
            domain::OrderIntent::new(2, Side::Buy, dec!(100), dec!(95), dec!(105), 5).unwrap();
        engine.submit_entry(buy_intent, dec!(100), 2_000);
        assert!(engine.pending_intent().is_some());
    }
}
