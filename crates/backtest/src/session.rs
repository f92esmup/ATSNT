//! Venue-independent trading session: signal gating, entry, protection, exit and accounting.
//!
//! The session is a state machine driven by three inputs (trades, bars and execution reports)
//! and talks to the market only through [`ports::ExecutionVenue`]. Live runs it on Binance and
//! simulations run it on a simulated venue, so the lifecycle logic is written once. It performs
//! no I/O of its own: everything the caller may want to log, alert or persist comes back as
//! [`SessionEvent`]s.

use domain::{DollarBar, DollarBarAggregator, DomainError, OrderIntent, PositionSide, Side, Trade};
use ports::{EntryAck, ExecutionReport, ExecutionVenue};
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::daily_watermark::DailyWatermark;
use crate::excursion::ExcursionTracker;
use crate::fee_ledger::FeeLedger;
use crate::live_exits::{ExitEvent, ExitManager, HaltState};
use crate::metrics::{ClosedTrade, TradeExcursion};
use crate::monthly_guard::{marked_equity, MonthlyGuard};

/// Static parameters of a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Instrument the session trades.
    pub symbol: String,
    /// Wallet equity at the start of the session.
    pub initial_equity: Decimal,
    /// Taker fee used when the venue reports no commission for a position.
    pub fallback_fee_pct: Decimal,
    /// Monthly kill-switch threshold (fraction of the month-open equity).
    pub max_monthly_drawdown_pct: Decimal,
    /// Minimum milliseconds between emergency-close attempts while halting.
    pub halt_retry_ms: i64,
}

/// Why a strategy signal was not turned into an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The account no longer matches the strategy profile.
    AccountMismatch,
    /// The monthly breaker or an emergency halt is active.
    Halted,
    /// An entry is pending or a position is open.
    PositionOpen,
    /// Exits or leftover orders of the previous trade are still being cleaned up.
    ExitsPending,
}

/// What happened during a session step, for the caller to log, alert or persist.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// The strategy emitted an order intent.
    Signal(OrderIntent),
    /// The intent was not dispatched.
    SignalSkipped(SkipReason),
    /// The venue accepted the entry order.
    EntrySubmitted(EntryAck),
    /// The venue rejected the entry order; the session stays flat.
    EntryFailed(String),
    /// The entry order ended (cancelled, expired or rejected) without any fill; back to flat.
    EntryCancelled,
    /// The entry completed and the exit protection was attempted.
    PositionOpened {
        side: Side,
        price: Decimal,
        quantity: Decimal,
        time_ms: i64,
        stop_loss: Decimal,
        take_profit: Decimal,
        protection: ExitEvent,
    },
    /// The exit lifecycle reported progress (time barrier, cleanup, halt, ...).
    Exit(ExitEvent),
    /// The position closed and the trade was accounted.
    PositionClosed(Box<ClosedTrade>),
    /// The monthly kill-switch tripped at this drawdown versus the month open.
    MonthlyBreakerTripped { drawdown: Decimal },
}

/// Executions accumulated for one order, so partial fills price the position at their VWAP.
#[derive(Debug, Clone, Default)]
struct Fills {
    quantity: Decimal,
    notional: Decimal,
}

impl Fills {
    fn add(&mut self, report: &ExecutionReport) {
        if report.last_quantity > Decimal::ZERO {
            self.quantity += report.last_quantity;
            self.notional += report.last_price * report.last_quantity;
        }
    }

    fn average_price(&self) -> Decimal {
        if self.quantity > Decimal::ZERO {
            self.notional / self.quantity
        } else {
            Decimal::ZERO
        }
    }
}

#[derive(Debug)]
enum Phase {
    Flat,
    EntryPending {
        intent: OrderIntent,
        fills: Fills,
    },
    Open {
        intent: OrderIntent,
        side: Side,
        entry_price: Decimal,
        quantity: Decimal,
        entry_time_ms: i64,
        exit_fills: Fills,
    },
}

/// One strategy trading one symbol on one venue.
pub struct TradingSession<S: Strategy> {
    config: SessionConfig,
    strategy: S,
    exits: ExitManager,
    phase: Phase,
    fees: FeeLedger,
    excursion: ExcursionTracker,
    daily: DailyWatermark,
    monthly: MonthlyGuard,
    wallet: Decimal,
    account_ok: bool,
    closed_trades: Vec<ClosedTrade>,
    last_halt_attempt_ms: Option<i64>,
    exit_hint: Option<&'static str>,
    funding: Decimal,
}

impl<S: Strategy> TradingSession<S> {
    /// Creates a flat session.
    pub fn new(config: SessionConfig, strategy: S) -> Self {
        Self {
            exits: ExitManager::new(config.symbol.clone()),
            monthly: MonthlyGuard::new(config.max_monthly_drawdown_pct),
            excursion: ExcursionTracker::new(config.initial_equity),
            wallet: config.initial_equity,
            config,
            strategy,
            phase: Phase::Flat,
            fees: FeeLedger::new(),
            daily: DailyWatermark::new(),
            account_ok: true,
            closed_trades: Vec::new(),
            last_halt_attempt_ms: None,
            exit_hint: None,
            funding: Decimal::ZERO,
        }
    }

    /// Trades closed so far, oldest first.
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        &self.closed_trades
    }

    /// Whether the monthly breaker or an emergency halt blocks new entries.
    pub fn is_halted(&self) -> bool {
        self.monthly.is_halted() || self.exits.halt_state() != HaltState::Running
    }

    /// Latest wallet equity known to the session.
    pub fn wallet_equity(&self) -> Decimal {
        self.wallet
    }

    /// Whether an entry is pending, a position is open or its exits are still being cleaned up.
    ///
    /// While exposed the account profile must not be modified.
    pub fn is_exposed(&self) -> bool {
        !matches!(self.phase, Phase::Flat) || !self.exits.can_open()
    }

    /// Feeds one historical dollar bar to the strategy so its rolling windows are as warm as in
    /// a backtest. Nothing trades: a signal raised on history is discarded.
    pub fn warm_up(&mut self, bar: &DollarBar) {
        let _ = self.strategy.on_bar(bar);
    }

    /// Warms the strategy up from historical trades, aggregated with a dedicated aggregator of
    /// the live `threshold` so the live stream starts on a fresh bar. Returns how many bars the
    /// strategy saw; the trailing partial bar is dropped.
    pub fn warm_up_trades(
        &mut self,
        threshold: Decimal,
        trades: impl IntoIterator<Item = Trade>,
    ) -> Result<usize, DomainError> {
        let mut aggregator = DollarBarAggregator::new(threshold)?;
        let mut bars = 0;
        for trade in trades {
            if let Some(bar) = aggregator.process_trade(&trade) {
                self.warm_up(&bar);
                bars += 1;
            }
        }
        Ok(bars)
    }

    /// Records the latest wallet balance reported by the venue.
    pub fn on_wallet_balance(&mut self, equity: Decimal) {
        self.wallet = equity;
        self.excursion.observe_cash(equity);
    }

    /// Records a funding payment settled by the venue (positive when paid, negative when
    /// received). It is charged to the open position's trade; funding that settles while flat
    /// belongs to no trade and is ignored.
    pub fn on_funding(&mut self, amount: Decimal) {
        if matches!(self.phase, Phase::Open { .. }) {
            self.funding += amount;
        }
    }

    /// Whether the account still matches the strategy profile; signals are skipped while not.
    pub fn set_account_ok(&mut self, account_ok: bool) {
        self.account_ok = account_ok;
    }

    /// Feeds one market trade: runs the monthly breaker and retries a pending emergency close.
    pub async fn on_trade<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        time_ms: i64,
        price: Decimal,
    ) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        let marked = marked_equity(self.wallet, self.open_position(), price);
        if let Some(drawdown) = self.monthly.observe(time_ms, marked) {
            events.push(SessionEvent::MonthlyBreakerTripped { drawdown });
        }

        let retry_due = self
            .last_halt_attempt_ms
            .map_or(true, |at| time_ms - at >= self.config.halt_retry_ms);
        if self.monthly.is_halted() && self.exits.halt_state() != HaltState::Halted && retry_due {
            self.last_halt_attempt_ms = Some(time_ms);
            let event = self.exits.halt(venue).await;
            self.exit_hint = Some("MonthlyCircuitBreaker");
            events.push(SessionEvent::Exit(event));
        }
        events
    }

    /// Feeds one closed bar: enforces the time barrier, then lets the strategy signal.
    pub async fn on_bar<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        bar: &DollarBar,
    ) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        if let Some(event) = self.exits.on_bar_closed(venue).await {
            if event == ExitEvent::TimeBarrierClosed {
                self.exit_hint = Some("TimeBarrier");
            }
            events.push(SessionEvent::Exit(event));
        }

        if let Phase::Open {
            side,
            entry_price,
            quantity,
            ..
        } = &self.phase
        {
            self.excursion.mark(
                bar.end_time,
                self.wallet,
                *side,
                *entry_price,
                *quantity,
                bar.close,
            );
        }

        // Daily breaker reference: the engine's per-UTC-day peak, marked at open then close.
        let open_position = self.open_position();
        self.daily.observe(
            bar.end_time,
            marked_equity(self.wallet, open_position, bar.open),
        );
        let day_peak = self.daily.observe(
            bar.end_time,
            marked_equity(self.wallet, open_position, bar.close),
        );

        if let Some(intent) = self.strategy.on_bar(bar) {
            events.push(SessionEvent::Signal(intent.clone()));
            events.extend(self.dispatch(venue, intent, day_peak).await);
        }
        events
    }

    /// Feeds one execution report from the venue (entry fills, exit fills, acknowledgements).
    pub async fn on_execution<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        report: &ExecutionReport,
    ) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        if report.last_quantity > Decimal::ZERO {
            self.fees.record_fill(
                report.commission,
                report.commission_asset.as_deref(),
                report.last_price * report.last_quantity,
                self.config.fallback_fee_pct,
            );
        }

        match std::mem::replace(&mut self.phase, Phase::Flat) {
            Phase::Flat => {
                // A fill nobody asked for cannot be protected: flatten and stop.
                if report.last_quantity > Decimal::ZERO || report.order_filled {
                    let event = self.exits.halt(venue).await;
                    events.push(SessionEvent::Exit(event));
                }
            }
            Phase::EntryPending { intent, mut fills } => {
                fills.add(report);
                let ended = report.order_filled || report.order_closed;
                if ended && fills.quantity > Decimal::ZERO {
                    // A partially filled order that then ended still left a position: protect it.
                    events.extend(self.open_position_from(venue, intent, &fills, report).await);
                } else if ended {
                    events.push(SessionEvent::EntryCancelled);
                } else {
                    self.phase = Phase::EntryPending { intent, fills };
                }
            }
            Phase::Open {
                intent,
                side,
                entry_price,
                quantity,
                entry_time_ms,
                mut exit_fills,
            } => {
                exit_fills.add(report);
                if report.liquidation {
                    self.exit_hint = Some("Liquidation");
                }
                if report.order_filled && exit_fills.quantity > Decimal::ZERO {
                    let closed = ClosedPosition {
                        intent,
                        side,
                        entry_price,
                        quantity,
                        entry_time_ms,
                        exit_price: exit_fills.average_price(),
                        exit_time_ms: report.time_ms,
                    };
                    events.extend(self.close_position_from(venue, closed).await);
                } else {
                    self.phase = Phase::Open {
                        intent,
                        side,
                        entry_price,
                        quantity,
                        entry_time_ms,
                        exit_fills,
                    };
                }
            }
        }
        events
    }

    /// Side, average entry price and quantity of the open position, if any.
    pub fn open_position(&self) -> Option<(Side, Decimal, Decimal)> {
        match &self.phase {
            Phase::Open {
                side,
                entry_price,
                quantity,
                ..
            } => Some((*side, *entry_price, *quantity)),
            _ => None,
        }
    }

    async fn dispatch<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        intent: OrderIntent,
        day_peak: Decimal,
    ) -> Vec<SessionEvent> {
        let skip = if !self.account_ok {
            Some(SkipReason::AccountMismatch)
        } else if self.is_halted() {
            Some(SkipReason::Halted)
        } else if !matches!(self.phase, Phase::Flat) {
            Some(SkipReason::PositionOpen)
        } else if !self.exits.can_open() {
            Some(SkipReason::ExitsPending)
        } else {
            None
        };
        if let Some(reason) = skip {
            return vec![SessionEvent::SignalSkipped(reason)];
        }

        match venue
            .submit_entry(&self.config.symbol, &intent, self.wallet, day_peak)
            .await
        {
            Ok(ack) => {
                self.phase = Phase::EntryPending {
                    intent,
                    fills: Fills::default(),
                };
                vec![SessionEvent::EntrySubmitted(ack)]
            }
            Err(error) => vec![SessionEvent::EntryFailed(error.to_string())],
        }
    }

    async fn open_position_from<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        intent: OrderIntent,
        fills: &Fills,
        report: &ExecutionReport,
    ) -> Vec<SessionEvent> {
        let side = report.side;
        let price = fills.average_price();
        let (stop_loss, take_profit) = (intent.stop_loss, intent.take_profit);
        self.excursion.open(report.time_ms, self.wallet);
        // Exits live on the venue so they survive a crash of this process.
        let protection = self
            .exits
            .on_entry_filled(
                venue,
                side,
                intent.stop_loss,
                intent.take_profit,
                intent.max_bars_hold,
            )
            .await;
        if matches!(
            protection,
            ExitEvent::UnprotectedEntryClosed { .. }
                | ExitEvent::UnprotectedEntryCloseFailed { .. }
        ) {
            self.exit_hint = Some("ProtectionFailed");
        }
        self.phase = Phase::Open {
            intent,
            side,
            entry_price: price,
            quantity: fills.quantity,
            entry_time_ms: report.time_ms,
            exit_fills: Fills::default(),
        };
        vec![SessionEvent::PositionOpened {
            side,
            price,
            quantity: fills.quantity,
            time_ms: report.time_ms,
            stop_loss,
            take_profit,
            protection,
        }]
    }

    async fn close_position_from<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        closed: ClosedPosition,
    ) -> Vec<SessionEvent> {
        // Drop leftover protective and entry orders of the finished trade.
        let cleanup = self.exits.on_position_closed_by_exchange(venue).await;
        let ClosedPosition {
            intent,
            side,
            entry_price,
            quantity,
            entry_time_ms,
            exit_price,
            exit_time_ms,
        } = closed;

        let gross = match side {
            Side::Buy => (exit_price - entry_price) * quantity,
            Side::Sell => (entry_price - exit_price) * quantity,
        };
        // Real commissions; the estimate only stands in when no fill was observed.
        let fees = self.fees.take().unwrap_or(
            (entry_price * quantity + exit_price * quantity) * self.config.fallback_fee_pct,
        );
        let funding = std::mem::take(&mut self.funding);
        let net = gross - fees - funding;
        let return_pct = if entry_price > Decimal::ZERO && quantity > Decimal::ZERO {
            net / (entry_price * quantity)
        } else {
            Decimal::ZERO
        };

        let reason = self
            .exit_hint
            .take()
            .unwrap_or_else(|| infer_exit_reason(side, exit_price, &intent));
        let slippage = adverse_move(side, entry_price, intent.price) * quantity
            + trigger_slippage(side, exit_price, &intent, reason) * quantity;

        let entry_equity = self.excursion.entry_equity().unwrap_or(self.wallet);
        let exit_equity = entry_equity + net;
        let excursion = self
            .excursion
            .close(exit_time_ms, exit_equity)
            .unwrap_or_else(|| TradeExcursion::start(exit_time_ms, exit_equity));

        let trade = ClosedTrade {
            entry_time: entry_time_ms,
            exit_time: exit_time_ms,
            entry_price,
            exit_price,
            quantity,
            side: match side {
                Side::Buy => PositionSide::Long,
                Side::Sell => PositionSide::Short,
            },
            pnl_gross: gross,
            fees_paid: fees,
            funding_paid: funding,
            slippage_paid: slippage,
            pnl_net: net,
            return_pct,
            exit_reason: reason.to_string(),
            holding_duration_seconds: ((exit_time_ms - entry_time_ms) / 1000).max(0),
            excursion,
        };
        self.closed_trades.push(trade.clone());
        vec![
            SessionEvent::Exit(cleanup),
            SessionEvent::PositionClosed(Box::new(trade)),
        ]
    }
}

struct ClosedPosition {
    intent: OrderIntent,
    side: Side,
    entry_price: Decimal,
    quantity: Decimal,
    entry_time_ms: i64,
    exit_price: Decimal,
    exit_time_ms: i64,
}

/// Labels an exit by where it filled relative to the intent's barriers.
fn infer_exit_reason(side: Side, exit_price: Decimal, intent: &OrderIntent) -> &'static str {
    let (stopped, targeted) = match side {
        Side::Buy => (
            exit_price <= intent.stop_loss,
            exit_price >= intent.take_profit,
        ),
        Side::Sell => (
            exit_price >= intent.stop_loss,
            exit_price <= intent.take_profit,
        ),
    };
    if stopped {
        "StopLoss"
    } else if targeted {
        "TakeProfit"
    } else {
        // Both exchange-held legs are market orders, so a fill can land on the inner side of
        // its own trigger (a target filled a tick below, a stop that bounced above): the exit
        // belongs to the nearer trigger.
        let to_stop = (exit_price - intent.stop_loss).abs();
        let to_target = (exit_price - intent.take_profit).abs();
        match to_stop.cmp(&to_target) {
            std::cmp::Ordering::Less => "StopLoss",
            std::cmp::Ordering::Greater => "TakeProfit",
            std::cmp::Ordering::Equal => "ExchangeClose",
        }
    }
}

/// How much worse than `reference` an `entry_side` fill at `price` was (never negative).
fn adverse_move(entry_side: Side, price: Decimal, reference: Decimal) -> Decimal {
    match entry_side {
        Side::Buy => (price - reference).max(Decimal::ZERO),
        Side::Sell => (reference - price).max(Decimal::ZERO),
    }
}

/// Slippage of a barrier exit versus its trigger price; market exits have no reference.
fn trigger_slippage(
    entry_side: Side,
    exit_price: Decimal,
    intent: &OrderIntent,
    reason: &str,
) -> Decimal {
    // The exit trades against the position, so its adverse direction is the opposite side's.
    let exit_side = match entry_side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    };
    match reason {
        "StopLoss" => adverse_move(exit_side, exit_price, intent.stop_loss),
        "TakeProfit" => adverse_move(exit_side, exit_price, intent.take_profit),
        _ => Decimal::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DollarBar, OrderIntent, Side};
    use ports::{EntryAck, ExecutionReport, ExecutionVenue, ExitGateway};
    use rust_decimal_macros::dec;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use strategies::{MarketType, Strategy};

    const JAN_1: i64 = 1_704_067_200_000;
    const DAY: i64 = 86_400_000;

    #[derive(Default)]
    struct FakeVenue {
        calls: RefCell<Vec<String>>,
        entries: RefCell<Vec<(Decimal, Decimal)>>,
        fail_entry: RefCell<bool>,
    }

    impl FakeVenue {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl ExitGateway for FakeVenue {
        type Error = String;

        async fn place_exits(
            &self,
            _: &str,
            side: Side,
            stop_loss: Decimal,
            take_profit: Decimal,
        ) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("place {side:?} {stop_loss} {take_profit}"));
            Ok(())
        }

        async fn cancel_orders(&self, _: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("cancel".to_string());
            Ok(())
        }

        async fn close_position(&self, _: &str) -> Result<bool, String> {
            self.calls.borrow_mut().push("close".to_string());
            Ok(false)
        }
    }

    impl ExecutionVenue for FakeVenue {
        async fn submit_entry(
            &self,
            _: &str,
            _: &OrderIntent,
            equity: Decimal,
            session_peak: Decimal,
        ) -> Result<EntryAck, String> {
            self.entries.borrow_mut().push((equity, session_peak));
            if *self.fail_entry.borrow() {
                return Err("rejected".to_string());
            }
            Ok(EntryAck {
                order_id: "entry-1".to_string(),
                status: "NEW".to_string(),
                quantity: dec!(1),
                price: dec!(100),
            })
        }
    }

    struct Scripted {
        intents: VecDeque<OrderIntent>,
    }

    impl Scripted {
        fn with(intents: Vec<OrderIntent>) -> Self {
            Self {
                intents: intents.into(),
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

        fn on_bar(&mut self, _: &DollarBar) -> Option<OrderIntent> {
            self.intents.pop_front()
        }

        fn reset(&mut self) {}
    }

    fn bar(end_time: i64, price: Decimal) -> DollarBar {
        DollarBar {
            start_time: end_time - 50,
            end_time,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: dec!(1),
            dollar_volume: price,
            buy_volume: dec!(0.5),
            sell_volume: dec!(0.5),
            buy_dollar_volume: price / dec!(2),
            sell_dollar_volume: price / dec!(2),
            trade_count: 5,
        }
    }

    fn long_intent() -> OrderIntent {
        OrderIntent::new(JAN_1, Side::Buy, dec!(100), dec!(90), dec!(110), 3).unwrap()
    }

    fn session(intents: Vec<OrderIntent>) -> TradingSession<Scripted> {
        TradingSession::new(
            SessionConfig {
                symbol: "BTCUSDT".to_string(),
                initial_equity: dec!(1000),
                fallback_fee_pct: dec!(0.0005),
                max_monthly_drawdown_pct: dec!(0.15),
                halt_retry_ms: 5_000,
            },
            Scripted::with(intents),
        )
    }

    fn fill(
        side: Side,
        quantity: Decimal,
        price: Decimal,
        order_filled: bool,
        time_ms: i64,
        commission: Decimal,
    ) -> ExecutionReport {
        ExecutionReport {
            time_ms,
            side,
            last_quantity: quantity,
            last_price: price,
            order_filled,
            order_closed: order_filled,
            commission,
            commission_asset: Some("USDT".to_string()),
            liquidation: false,
        }
    }

    /// A report that ends an order without completing it (cancelled, expired, rejected).
    fn terminated(side: Side, time_ms: i64) -> ExecutionReport {
        ExecutionReport {
            time_ms,
            side,
            last_quantity: Decimal::ZERO,
            last_price: Decimal::ZERO,
            order_filled: false,
            order_closed: true,
            commission: Decimal::ZERO,
            commission_asset: None,
            liquidation: false,
        }
    }

    /// Submits the long intent and fills the entry at 100 (quantity 1, 0.05 USDT commission).
    async fn open_long(session: &mut TradingSession<Scripted>, venue: &FakeVenue) {
        session.on_bar(venue, &bar(JAN_1 + 10, dec!(100))).await;
        let events = session
            .on_execution(
                venue,
                &fill(Side::Buy, dec!(1), dec!(100), true, JAN_1 + 20, dec!(0.05)),
            )
            .await;
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::PositionOpened { .. })));
    }

    #[tokio::test]
    async fn a_signal_is_submitted_with_the_wallet_equity_and_the_day_peak() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_wallet_balance(dec!(1000));

        let events = session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        assert!(events.contains(&SessionEvent::Signal(long_intent())));
        assert!(matches!(
            events.last(),
            Some(SessionEvent::EntrySubmitted(_))
        ));
        assert_eq!(*venue.entries.borrow(), vec![(dec!(1000), dec!(1000))]);
    }

    #[tokio::test]
    async fn a_signal_is_skipped_while_the_account_does_not_match_the_strategy_profile() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.set_account_ok(false);

        let events = session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        assert!(events.contains(&SessionEvent::SignalSkipped(SkipReason::AccountMismatch)));
        assert!(venue.entries.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_second_signal_is_skipped_while_the_first_entry_is_pending() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent(), long_intent()]);

        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        let events = session.on_bar(&venue, &bar(JAN_1 + 20, dec!(100))).await;

        assert!(events.contains(&SessionEvent::SignalSkipped(SkipReason::PositionOpen)));
        assert_eq!(venue.entries.borrow().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_entry_leaves_the_session_flat_and_ready_for_the_next_signal() {
        let venue = FakeVenue::default();
        *venue.fail_entry.borrow_mut() = true;
        let mut session = session(vec![long_intent(), long_intent()]);

        let first = session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        let second = session.on_bar(&venue, &bar(JAN_1 + 20, dec!(100))).await;

        assert!(first
            .iter()
            .any(|e| matches!(e, SessionEvent::EntryFailed(_))));
        assert!(second.contains(&SessionEvent::Signal(long_intent())));
        assert_eq!(venue.entries.borrow().len(), 2);
    }

    #[tokio::test]
    async fn an_entry_cancelled_without_fills_returns_the_session_to_flat() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent(), long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        let events = session
            .on_execution(&venue, &terminated(Side::Buy, JAN_1 + 20))
            .await;
        let exposed_after_cancel = session.is_exposed();
        let next = session.on_bar(&venue, &bar(JAN_1 + 30, dec!(100))).await;

        assert!(events.contains(&SessionEvent::EntryCancelled));
        assert!(!exposed_after_cancel);
        assert!(next.contains(&SessionEvent::Signal(long_intent())));
        assert_eq!(venue.entries.borrow().len(), 2);
    }

    #[tokio::test]
    async fn an_entry_that_expires_after_a_partial_fill_is_opened_and_protected() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        session
            .on_execution(
                &venue,
                &fill(Side::Buy, dec!(2), dec!(100), false, JAN_1 + 20, dec!(0.05)),
            )
            .await;

        let events = session
            .on_execution(&venue, &terminated(Side::Buy, JAN_1 + 30))
            .await;

        assert!(events.contains(&SessionEvent::PositionOpened {
            side: Side::Buy,
            price: dec!(100),
            quantity: dec!(2),
            time_ms: JAN_1 + 30,
            stop_loss: dec!(90),
            take_profit: dec!(110),
            protection: ExitEvent::Protected,
        }));
        assert!(venue.calls().contains(&"place Buy 90 110".to_string()));
    }

    #[tokio::test]
    async fn the_entry_fill_installs_the_exchange_held_exits_from_the_intent() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        let events = session
            .on_execution(
                &venue,
                &fill(Side::Buy, dec!(1), dec!(100), true, JAN_1 + 20, dec!(0.05)),
            )
            .await;

        assert!(events.contains(&SessionEvent::PositionOpened {
            side: Side::Buy,
            price: dec!(100),
            quantity: dec!(1),
            time_ms: JAN_1 + 20,
            stop_loss: dec!(90),
            take_profit: dec!(110),
            protection: ExitEvent::Protected,
        }));
        assert!(venue.calls().contains(&"place Buy 90 110".to_string()));
    }

    #[tokio::test]
    async fn partial_entry_fills_open_the_position_at_the_volume_weighted_price() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        let partial = session
            .on_execution(
                &venue,
                &fill(Side::Buy, dec!(1), dec!(100), false, JAN_1 + 20, dec!(0.05)),
            )
            .await;
        let last = session
            .on_execution(
                &venue,
                &fill(Side::Buy, dec!(3), dec!(104), true, JAN_1 + 30, dec!(0.15)),
            )
            .await;

        assert!(partial.is_empty());
        assert!(last.contains(&SessionEvent::PositionOpened {
            side: Side::Buy,
            price: dec!(103),
            quantity: dec!(4),
            time_ms: JAN_1 + 30,
            stop_loss: dec!(90),
            take_profit: dec!(110),
            protection: ExitEvent::Protected,
        }));
    }

    #[tokio::test]
    async fn a_stop_loss_fill_closes_the_trade_with_the_commissions_actually_charged() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        open_long(&mut session, &venue).await;

        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(90),
                    true,
                    JAN_1 + 90_000,
                    dec!(0.04),
                ),
            )
            .await;

        let trade = events
            .iter()
            .find_map(|e| match e {
                SessionEvent::PositionClosed(trade) => Some(trade.as_ref().clone()),
                _ => None,
            })
            .expect("the exit fill should close the trade");
        assert_eq!(trade.pnl_gross, dec!(-10));
        assert_eq!(trade.fees_paid, dec!(0.09));
        assert_eq!(trade.pnl_net, dec!(-10.09));
        assert_eq!(trade.exit_reason, "StopLoss");
        assert_eq!(trade.holding_duration_seconds, 89);
        assert!(events.contains(&SessionEvent::Exit(ExitEvent::CleanedUp)));
        assert_eq!(session.closed_trades().len(), 1);
    }

    #[tokio::test]
    async fn slippage_is_measured_against_the_signal_and_trigger_prices() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        // Bought 0.5 above the signal price (100), stopped out 0.2 below the trigger (90).
        session
            .on_execution(
                &venue,
                &fill(
                    Side::Buy,
                    dec!(2),
                    dec!(100.5),
                    true,
                    JAN_1 + 20,
                    dec!(0.05),
                ),
            )
            .await;

        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(2),
                    dec!(89.8),
                    true,
                    JAN_1 + 30,
                    dec!(0.05),
                ),
            )
            .await;

        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::PositionClosed(trade) if trade.slippage_paid == dec!(1.4)
        )));
    }

    #[tokio::test]
    async fn a_fill_at_the_target_is_labeled_take_profit() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        open_long(&mut session, &venue).await;

        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(110),
                    true,
                    JAN_1 + 60_000,
                    dec!(0.05),
                ),
            )
            .await;

        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::PositionClosed(trade) if trade.exit_reason == "TakeProfit"
        )));
    }

    #[tokio::test]
    async fn the_time_barrier_closes_the_position_and_labels_the_trade() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        open_long(&mut session, &venue).await;

        session.on_bar(&venue, &bar(JAN_1 + 100, dec!(101))).await;
        session.on_bar(&venue, &bar(JAN_1 + 200, dec!(102))).await;
        let third = session.on_bar(&venue, &bar(JAN_1 + 300, dec!(103))).await;
        let closed = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(103),
                    true,
                    JAN_1 + 310,
                    dec!(0.05),
                ),
            )
            .await;

        assert!(third.contains(&SessionEvent::Exit(ExitEvent::TimeBarrierClosed)));
        assert!(venue.calls().contains(&"close".to_string()));
        assert!(closed.iter().any(|e| matches!(
            e,
            SessionEvent::PositionClosed(trade) if trade.exit_reason == "TimeBarrier"
        )));
    }

    #[tokio::test]
    async fn trading_resumes_after_the_trade_is_closed_and_cleaned_up() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent(), long_intent()]);
        open_long(&mut session, &venue).await;
        session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(110),
                    true,
                    JAN_1 + 60_000,
                    dec!(0.05),
                ),
            )
            .await;

        let events = session
            .on_bar(&venue, &bar(JAN_1 + 70_000, dec!(100)))
            .await;

        assert!(events.contains(&SessionEvent::Signal(long_intent())));
        assert_eq!(venue.entries.borrow().len(), 2);
    }

    #[tokio::test]
    async fn the_monthly_breaker_flattens_the_position_and_blocks_new_entries() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_wallet_balance(dec!(1000));
        session.on_trade(&venue, JAN_1, dec!(100)).await;

        session.on_wallet_balance(dec!(800));
        let events = session.on_trade(&venue, JAN_1 + DAY, dec!(100)).await;
        let skipped = session
            .on_bar(&venue, &bar(JAN_1 + DAY + 10, dec!(100)))
            .await;

        assert!(events.contains(&SessionEvent::MonthlyBreakerTripped {
            drawdown: dec!(0.2)
        }));
        assert!(events.contains(&SessionEvent::Exit(ExitEvent::Halted)));
        assert!(venue.calls().contains(&"close".to_string()));
        assert!(skipped.contains(&SessionEvent::SignalSkipped(SkipReason::Halted)));
        assert!(venue.entries.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_fill_without_a_pending_entry_cannot_be_protected_so_the_session_halts() {
        let venue = FakeVenue::default();
        let mut session = session(vec![]);

        let events = session
            .on_execution(
                &venue,
                &fill(Side::Buy, dec!(1), dec!(100), true, JAN_1 + 20, dec!(0.05)),
            )
            .await;

        assert!(events.contains(&SessionEvent::Exit(ExitEvent::Halted)));
        assert!(venue.calls().contains(&"close".to_string()));
    }

    #[test]
    fn an_exit_between_the_triggers_belongs_to_the_nearer_one() {
        let long = OrderIntent::new(0, Side::Buy, dec!(100), dec!(98), dec!(104), 5).unwrap();
        let short = OrderIntent::new(0, Side::Sell, dec!(100), dec!(102), dec!(96), 5).unwrap();

        // A market target filled a tick below its trigger, and a stop that bounced above it.
        assert_eq!(
            infer_exit_reason(Side::Buy, dec!(103.9), &long),
            "TakeProfit"
        );
        assert_eq!(infer_exit_reason(Side::Buy, dec!(98.2), &long), "StopLoss");
        assert_eq!(
            infer_exit_reason(Side::Sell, dec!(96.1), &short),
            "TakeProfit"
        );
        assert_eq!(
            infer_exit_reason(Side::Sell, dec!(101.8), &short),
            "StopLoss"
        );
        // Beyond a trigger and exactly halfway keep their previous meaning.
        assert_eq!(infer_exit_reason(Side::Buy, dec!(97), &long), "StopLoss");
        assert_eq!(
            infer_exit_reason(Side::Buy, dec!(101), &long),
            "ExchangeClose"
        );
    }

    fn closed_trade(events: &[SessionEvent]) -> ClosedTrade {
        events
            .iter()
            .find_map(|e| match e {
                SessionEvent::PositionClosed(trade) => Some(trade.as_ref().clone()),
                _ => None,
            })
            .expect("the exit fill should close the trade")
    }

    #[tokio::test]
    async fn funding_paid_while_open_is_charged_to_the_trade() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        open_long(&mut session, &venue).await;

        session.on_funding(dec!(0.4));
        session.on_funding(dec!(-0.1));
        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(110),
                    true,
                    JAN_1 + 90_000,
                    dec!(0.05),
                ),
            )
            .await;

        let trade = closed_trade(&events);
        assert_eq!(trade.funding_paid, dec!(0.3));
        assert_eq!(trade.pnl_gross, dec!(10));
        assert_eq!(trade.pnl_net, dec!(10) - dec!(0.10) - dec!(0.3));
    }

    #[tokio::test]
    async fn funding_does_not_leak_into_the_next_trade() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent(), long_intent()]);
        open_long(&mut session, &venue).await;
        session.on_funding(dec!(0.4));
        session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(110),
                    true,
                    JAN_1 + 90_000,
                    dec!(0.05),
                ),
            )
            .await;

        // Funding that arrives while flat belongs to no trade.
        session.on_funding(dec!(5));
        open_long(&mut session, &venue).await;
        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(110),
                    true,
                    JAN_1 + 190_000,
                    dec!(0.05),
                ),
            )
            .await;

        assert_eq!(closed_trade(&events).funding_paid, Decimal::ZERO);
    }

    #[tokio::test]
    async fn a_liquidation_fill_closes_the_trade_as_liquidated() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        open_long(&mut session, &venue).await;
        let mut report = fill(Side::Sell, dec!(1), dec!(80), true, JAN_1 + 90_000, dec!(0));
        report.liquidation = true;

        let events = session.on_execution(&venue, &report).await;

        let trade = closed_trade(&events);
        assert_eq!(trade.exit_reason, "Liquidation");
        assert_eq!(trade.exit_price, dec!(80));
    }

    #[tokio::test]
    async fn warming_up_feeds_the_strategy_without_trading() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);

        // The scripted strategy would signal on the first bar it sees.
        session.warm_up(&bar(JAN_1 + 10, dec!(100)));
        let events = session.on_bar(&venue, &bar(JAN_1 + 20, dec!(100))).await;

        assert!(venue.entries.borrow().is_empty());
        assert!(!events.iter().any(|e| matches!(e, SessionEvent::Signal(_))));
        assert!(!session.is_exposed());
    }

    #[test]
    fn warming_up_from_trades_forms_bars_and_drops_the_partial_one() {
        let mut session = session(Vec::new());
        let trades: Vec<Trade> = (0..5)
            .map(|i| Trade::new(JAN_1 + i, dec!(100), dec!(1), Side::Buy).unwrap())
            .collect();

        // $100 trades with a $200 threshold: two bars, and one trade left over.
        let bars = session.warm_up_trades(dec!(200), trades).unwrap();

        assert_eq!(bars, 2);
    }

    #[test]
    fn warming_up_rejects_an_invalid_threshold() {
        let mut session = session(Vec::new());

        assert!(session.warm_up_trades(Decimal::ZERO, Vec::new()).is_err());
    }
}
