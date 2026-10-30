//! Venue-independent trading session: signal gating, entry, protection, exit and accounting.
//!
//! The session is a state machine driven by three inputs (trades, bars and execution reports)
//! and talks to the market only through [`ports::ExecutionVenue`]. Live runs it on Binance and
//! simulations run it on a simulated venue, so the lifecycle logic is written once. It performs
//! no I/O of its own: everything the caller may want to log, alert or persist comes back as
//! [`SessionEvent`]s.

use std::collections::VecDeque;

use domain::{
    DollarBar, DollarBarAggregator, DomainError, OrderIntent, PositionSide, RiskPolicy, Side,
    TargetPosition, Trade,
};
use ports::{EntryAck, ExecutionReport, ExecutionVenue, OrderRequest};
use rust_decimal::Decimal;
use strategies::{Decision, MarketEvent, Strategy};

use crate::execution_core::{
    order_limit_kind, Campaign, CampaignClose, TradeLedger, TARGET_EXIT_REASON, TARGET_FLIP_REASON,
};
use crate::live_exits::{ExitEvent, ExitManager, HaltState};
use crate::metrics::{ClosedTrade, TradeExcursion};
use domain::accounting::{
    marked_equity, DailyWatermark, ExcursionTracker, FeeLedger, MonthlyGuard,
};

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
    /// How long a submitted entry may wait for a terminal status before the session cancels it
    /// (market-trade time). The wait is also the retry throttle's partner: failed attempts repeat
    /// every `halt_retry_ms`.
    pub entry_timeout_ms: i64,
    /// Risk policy used to size `Target` decisions; `None` for sessions that do not trade targets.
    pub risk_policy: Option<RiskPolicy>,
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
    /// The entry never reached a terminal status: the session cancelled it and went flat.
    /// `closed_unreported_position` is `true` when the venue turned out to hold a position the
    /// session had not been told about (a lost fill report), which was closed at market.
    EntryTimedOut {
        /// Whether an unreported position had to be closed.
        closed_unreported_position: bool,
    },
    /// The entry timed out but cancelling it or checking the position failed; it stays pending
    /// and is retried.
    EntryTimeoutFailed(String),
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
        /// Market-trade time the entry was sent.
        submitted_ms: i64,
        /// Market-trade time the cancel was sent after a timeout with part of the entry already
        /// filled; the terminal report that follows carries the final filled quantity.
        cancel_sent_ms: Option<i64>,
    },
    Open {
        intent: Option<OrderIntent>,
        side: Side,
        entry_price: Decimal,
        quantity: Decimal,
        entry_time_ms: i64,
        exit_fills: Fills,
        stop_distance: Option<Decimal>,
        take_profit_distance: Option<Decimal>,
    },
    Adjusting {
        position: Option<(Side, Decimal, Decimal)>,
        target: TargetPosition,
        delta: Decimal,
        submitted_ms: i64,
        pending_flip: Option<OrderRequest>,
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
    ledger: TradeLedger,
    campaign: Option<Campaign>,
    rearm_blocked: Option<PositionSide>,
    last_halt_attempt_ms: Option<i64>,
    exit_hint: Option<&'static str>,
    funding: Decimal,
    last_entry_timeout_attempt_ms: Option<i64>,
    /// Trade ids of the latest fills, to ignore a report the venue repeats.
    seen_trade_ids: VecDeque<i64>,
    /// A position the entry timeout had to close because its fill report was lost.
    recovery: Option<Recovery>,
}

/// What is known about a position recovered by the entry timeout: the signal that opened it
/// and the fills of the order that closed it.
struct Recovery {
    intent: OrderIntent,
    submitted_ms: i64,
    fills: Fills,
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
            ledger: TradeLedger::default(),
            campaign: None,
            rearm_blocked: None,
            last_halt_attempt_ms: None,
            exit_hint: None,
            funding: Decimal::ZERO,
            last_entry_timeout_attempt_ms: None,
            seen_trade_ids: VecDeque::new(),
            recovery: None,
        }
    }

    /// Trades closed so far, oldest first.
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        self.ledger.trades()
    }

    /// Reference to the session's strategy.
    pub fn strategy(&self) -> &S {
        &self.strategy
    }

    /// Mutable reference to the session's strategy.
    pub fn strategy_mut(&mut self) -> &mut S {
        &mut self.strategy
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
        let _ = self.strategy.on_event(&MarketEvent::DollarBar(bar));
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

        events.extend(self.resolve_stale_entry(venue, time_ms).await);

        if let Some(event) = self.exits.on_price_update(venue, price).await {
            events.push(SessionEvent::Exit(event));
        }

        if let Some(event) = self.exits.on_time_update(venue, time_ms).await {
            if event == ExitEvent::TimeBarrierClosed {
                self.exit_hint = Some("TimeBarrier");
            }
            events.push(SessionEvent::Exit(event));
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

    /// Advances the session's clock without a market trade, so an entry that never reaches a
    /// terminal status is still resolved when the public stream goes quiet. `now_ms` is in the
    /// same market-trade time as [`Self::on_trade`].
    pub async fn on_idle<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        now_ms: i64,
    ) -> Vec<SessionEvent> {
        self.resolve_stale_entry(venue, now_ms).await
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

        if let Some((side, entry_price, quantity)) = self.open_position() {
            self.excursion.mark(
                bar.end_time,
                self.wallet,
                side,
                entry_price,
                quantity,
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

        let decision = self.strategy.on_event(&MarketEvent::DollarBar(bar));
        if let Some(Decision::Target(target)) = decision {
            events.extend(
                self.dispatch_target(venue, target, bar.close, bar.end_time)
                    .await,
            );
        } else if let Some(Decision::Entry(intent)) = decision {
            events.push(SessionEvent::Signal(intent.clone()));
            events.extend(self.dispatch(venue, intent, day_peak, bar.end_time).await);
        }
        events
    }

    /// Feeds one raw market trade for trade-consuming strategies.
    pub async fn on_trade_event<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        trade: &Trade,
    ) -> Vec<SessionEvent> {
        let mut events = Vec::new();

        if let Some((side, entry_price, quantity)) = self.open_position() {
            self.excursion.mark(
                trade.timestamp,
                self.wallet,
                side,
                entry_price,
                quantity,
                trade.price,
            );
        }

        let open_position = self.open_position();
        let day_peak = self.daily.observe(
            trade.timestamp,
            marked_equity(self.wallet, open_position, trade.price),
        );

        let decision = self.strategy.on_event(&MarketEvent::Trade(trade));
        if let Some(Decision::Target(target)) = decision {
            events.extend(
                self.dispatch_target(venue, target, trade.price, trade.timestamp)
                    .await,
            );
        } else if let Some(Decision::Entry(intent)) = decision {
            events.push(SessionEvent::Signal(intent.clone()));
            events.extend(
                self.dispatch(venue, intent, day_peak, trade.timestamp)
                    .await,
            );
        }
        events
    }

    async fn dispatch_target<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        target: TargetPosition,
        mark_price: Decimal,
        now_ms: i64,
    ) -> Vec<SessionEvent> {
        let level = target.level();
        if let Some(blocked) = self.rearm_blocked {
            if level == 0 {
                self.rearm_blocked = None;
            } else {
                let side = if level > 0 {
                    PositionSide::Long
                } else {
                    PositionSide::Short
                };
                if side != blocked {
                    self.rearm_blocked = None;
                } else {
                    return Vec::new();
                }
            }
        }

        if level == 0
            && self.strategy.bracket_priority()
            && matches!(
                self.phase,
                Phase::Open {
                    take_profit_distance: Some(_),
                    ..
                }
            )
        {
            return Vec::new();
        }

        if !self.account_ok || self.is_halted() {
            if self.is_halted() && level != 0 {
                return vec![SessionEvent::SignalSkipped(SkipReason::Halted)];
            }
            if !self.account_ok {
                return vec![SessionEvent::SignalSkipped(SkipReason::AccountMismatch)];
            }
        }

        if matches!(
            self.phase,
            Phase::EntryPending { .. } | Phase::Adjusting { .. }
        ) {
            return vec![SessionEvent::SignalSkipped(SkipReason::PositionOpen)];
        }

        let current = self.open_position();
        let current_signed_qty = match current {
            Some((Side::Buy, _, q)) => q,
            Some((Side::Sell, _, q)) => -q,
            None => Decimal::ZERO,
        };

        let Some(policy) = &self.config.risk_policy else {
            return Vec::new();
        };

        let drawdown = self
            .daily
            .drawdown_pct(marked_equity(self.wallet, current, mark_price));
        let sizing = match policy.size_target(
            &target,
            mark_price,
            self.wallet,
            current_signed_qty,
            drawdown,
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let target_qty = sizing.quantity;
        let delta = target_qty - current_signed_qty;
        if delta == Decimal::ZERO {
            return Vec::new();
        }

        if target.level() != 0
            && order_limit_kind(
                delta.abs(),
                mark_price,
                policy.min_order_quantity,
                policy.min_order_notional,
                policy.max_order_notional,
            )
            .is_some()
        {
            return Vec::new();
        }

        let is_flip = current_signed_qty != Decimal::ZERO
            && target_qty != Decimal::ZERO
            && (current_signed_qty > Decimal::ZERO) != (target_qty > Decimal::ZERO);

        if is_flip {
            let Some((current_side, _, _)) = current else {
                return Vec::new();
            };
            let close_side = match current_side {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            };
            let close_req = OrderRequest {
                side: close_side,
                quantity: current_signed_qty.abs(),
                reduce_only: true,
            };
            let open_side = if target_qty > Decimal::ZERO {
                Side::Buy
            } else {
                Side::Sell
            };
            let open_req = OrderRequest {
                side: open_side,
                quantity: target_qty.abs(),
                reduce_only: false,
            };
            match venue.submit_order(&self.config.symbol, &close_req).await {
                Ok(ack) => {
                    self.phase = Phase::Adjusting {
                        position: current,
                        target,
                        delta: -current_signed_qty,
                        submitted_ms: now_ms,
                        pending_flip: Some(open_req),
                    };
                    vec![SessionEvent::EntrySubmitted(ack)]
                }
                Err(err) => vec![SessionEvent::EntryFailed(err.to_string())],
            }
        } else {
            let is_reduction = current_signed_qty != Decimal::ZERO
                && target_qty.abs() < current_signed_qty.abs()
                && (target_qty == Decimal::ZERO
                    || (current_signed_qty > Decimal::ZERO) == (target_qty > Decimal::ZERO));
            let side = if delta > Decimal::ZERO {
                Side::Buy
            } else {
                Side::Sell
            };
            let req = OrderRequest {
                side,
                quantity: delta.abs(),
                reduce_only: is_reduction,
            };
            match venue.submit_order(&self.config.symbol, &req).await {
                Ok(ack) => {
                    self.phase = Phase::Adjusting {
                        position: current,
                        target,
                        delta,
                        submitted_ms: now_ms,
                        pending_flip: None,
                    };
                    vec![SessionEvent::EntrySubmitted(ack)]
                }
                Err(err) => vec![SessionEvent::EntryFailed(err.to_string())],
            }
        }
    }

    /// Feeds one execution report from the venue (entry fills, exit fills, acknowledgements).
    pub async fn on_execution<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        report: &ExecutionReport,
    ) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        if self.is_repeated_fill(report) {
            return events;
        }
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
                // The closing fills of a position the entry timeout recovered are booked as a
                // trade, with the signal price standing in for the entry that was never seen.
                let recovered = match &mut self.recovery {
                    Some(recovery) if report.last_quantity > Decimal::ZERO => {
                        recovery.fills.add(report);
                        report.order_filled.then(|| {
                            (
                                recovery.intent.clone(),
                                recovery.submitted_ms,
                                recovery.fills.clone(),
                            )
                        })
                    }
                    _ => None,
                };
                if let Some((intent, submitted_ms, fills)) = recovered {
                    self.recovery = None;
                    self.exit_hint = Some("RecoveredUnreportedEntry");
                    let closed = ClosedPosition {
                        side: intent.side,
                        entry_price: intent.price,
                        quantity: fills.quantity,
                        entry_time_ms: submitted_ms,
                        exit_price: fills.average_price(),
                        exit_time_ms: report.time_ms,
                        intent,
                    };
                    events.extend(self.close_position_from(venue, closed).await);
                }
                // A fill nobody asked for cannot be protected: flatten and stop.
                if report.last_quantity > Decimal::ZERO || report.order_filled {
                    let event = self.exits.halt(venue).await;
                    events.push(SessionEvent::Exit(event));
                }
            }
            Phase::EntryPending {
                intent,
                mut fills,
                submitted_ms,
                cancel_sent_ms,
            } => {
                fills.add(report);
                let ended = report.order_filled || report.order_closed;
                if ended && fills.quantity > Decimal::ZERO {
                    // A partially filled order that then ended still left a position: protect it.
                    events.extend(
                        self.open_position_from(venue, intent, &fills, report.side, report.time_ms)
                            .await,
                    );
                } else if ended {
                    events.push(SessionEvent::EntryCancelled);
                } else {
                    self.phase = Phase::EntryPending {
                        intent,
                        fills,
                        submitted_ms,
                        cancel_sent_ms,
                    };
                }
            }
            Phase::Open {
                intent,
                side,
                entry_price,
                quantity,
                entry_time_ms,
                mut exit_fills,
                stop_distance,
                take_profit_distance,
            } => {
                exit_fills.add(report);
                if report.liquidation {
                    self.exit_hint = Some("Liquidation");
                }
                if report.order_filled && exit_fills.quantity > Decimal::ZERO {
                    if let Some(intent) = intent {
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
                        events.extend(
                            self.close_target_position(
                                venue,
                                ClosedTarget {
                                    side,
                                    entry_price,
                                    quantity,
                                    exit_price: exit_fills.average_price(),
                                    exit_time_ms: report.time_ms,
                                    stop_distance,
                                    take_profit_distance,
                                },
                            )
                            .await,
                        );
                    }
                } else {
                    self.phase = Phase::Open {
                        intent,
                        side,
                        entry_price,
                        quantity,
                        entry_time_ms,
                        exit_fills,
                        stop_distance,
                        take_profit_distance,
                    };
                }
            }
            Phase::Adjusting {
                position,
                target,
                delta,
                submitted_ms,
                pending_flip,
            } => {
                if report.last_quantity <= Decimal::ZERO
                    && !report.order_filled
                    && !report.order_closed
                {
                    self.phase = Phase::Adjusting {
                        position,
                        target,
                        delta,
                        submitted_ms,
                        pending_flip,
                    };
                    return events;
                }

                let fee = if report.commission > Decimal::ZERO {
                    report.commission
                } else {
                    report.last_price * report.last_quantity * self.config.fallback_fee_pct
                };
                let slippage = Decimal::ZERO;

                match position {
                    None => {
                        // Opening from flat
                        let side = report.side;
                        let fill_price = report.last_price;
                        let fill_qty = report.last_quantity;
                        let mut campaign = Campaign::starting(report.time_ms);
                        self.excursion.open(report.time_ms, self.wallet);
                        campaign.add_increase(fill_price, fill_qty, fee, slippage, fill_qty);

                        let stop = match side {
                            Side::Buy => fill_price - target.stop_distance(),
                            Side::Sell => fill_price + target.stop_distance(),
                        };
                        let take_profit = target.take_profit_distance().map(|tp_dist| match side {
                            Side::Buy => fill_price + tp_dist,
                            Side::Sell => fill_price - tp_dist,
                        });
                        let _ = venue.cancel_orders(&self.config.symbol).await;
                        let _ = venue
                            .place_exits(&self.config.symbol, side, stop, take_profit)
                            .await;

                        self.campaign = Some(campaign);
                        if report.order_filled {
                            self.phase = Phase::Open {
                                intent: None,
                                side,
                                entry_price: fill_price,
                                quantity: fill_qty,
                                entry_time_ms: report.time_ms,
                                exit_fills: Fills::default(),
                                stop_distance: Some(target.stop_distance()),
                                take_profit_distance: target.take_profit_distance(),
                            };
                        } else {
                            self.phase = Phase::Adjusting {
                                position: Some((side, fill_price, fill_qty)),
                                target,
                                delta,
                                submitted_ms,
                                pending_flip,
                            };
                        }
                    }
                    Some((pos_side, entry_price, pos_qty)) => {
                        if report.side == pos_side {
                            // Scale-in (increase)
                            let new_qty = pos_qty + report.last_quantity;
                            let new_entry_price = (pos_qty * entry_price
                                + report.last_quantity * report.last_price)
                                / new_qty;
                            if let Some(camp) = self.campaign.as_mut() {
                                camp.add_increase(
                                    report.last_price,
                                    report.last_quantity,
                                    fee,
                                    slippage,
                                    new_qty,
                                );
                            }
                            let stop = match pos_side {
                                Side::Buy => report.last_price - target.stop_distance(),
                                Side::Sell => report.last_price + target.stop_distance(),
                            };
                            let take_profit =
                                target.take_profit_distance().map(|tp_dist| match pos_side {
                                    Side::Buy => report.last_price + tp_dist,
                                    Side::Sell => report.last_price - tp_dist,
                                });
                            let _ = venue.cancel_orders(&self.config.symbol).await;
                            let _ = venue
                                .place_exits(&self.config.symbol, pos_side, stop, take_profit)
                                .await;

                            if report.order_filled {
                                self.phase = Phase::Open {
                                    intent: None,
                                    side: pos_side,
                                    entry_price: new_entry_price,
                                    quantity: new_qty,
                                    entry_time_ms: report.time_ms,
                                    exit_fills: Fills::default(),
                                    stop_distance: Some(target.stop_distance()),
                                    take_profit_distance: target.take_profit_distance(),
                                };
                            } else {
                                self.phase = Phase::Adjusting {
                                    position: Some((pos_side, new_entry_price, new_qty)),
                                    target,
                                    delta,
                                    submitted_ms,
                                    pending_flip,
                                };
                            }
                        } else {
                            // Reduction or close
                            let closed_qty = report.last_quantity.min(pos_qty);
                            let remaining_qty = pos_qty - closed_qty;
                            let pnl = match pos_side {
                                Side::Buy => (report.last_price - entry_price) * closed_qty,
                                Side::Sell => (entry_price - report.last_price) * closed_qty,
                            };
                            if let Some(camp) = self.campaign.as_mut() {
                                camp.add_reduction(
                                    report.last_price,
                                    closed_qty,
                                    pnl,
                                    fee,
                                    slippage,
                                );
                            }

                            if remaining_qty > Decimal::ZERO {
                                let stop = match pos_side {
                                    Side::Buy => report.last_price - target.stop_distance(),
                                    Side::Sell => report.last_price + target.stop_distance(),
                                };
                                let take_profit =
                                    target.take_profit_distance().map(|tp_dist| match pos_side {
                                        Side::Buy => report.last_price + tp_dist,
                                        Side::Sell => report.last_price - tp_dist,
                                    });
                                let _ = venue.cancel_orders(&self.config.symbol).await;
                                let _ = venue
                                    .place_exits(&self.config.symbol, pos_side, stop, take_profit)
                                    .await;

                                if report.order_filled {
                                    self.phase = Phase::Open {
                                        intent: None,
                                        side: pos_side,
                                        entry_price,
                                        quantity: remaining_qty,
                                        entry_time_ms: report.time_ms,
                                        exit_fills: Fills::default(),
                                        stop_distance: Some(target.stop_distance()),
                                        take_profit_distance: target.take_profit_distance(),
                                    };
                                } else {
                                    self.phase = Phase::Adjusting {
                                        position: Some((pos_side, entry_price, remaining_qty)),
                                        target,
                                        delta,
                                        submitted_ms,
                                        pending_flip,
                                    };
                                }
                            } else {
                                // Full close of the campaign
                                let campaign = self
                                    .campaign
                                    .take()
                                    .unwrap_or_else(|| Campaign::starting(report.time_ms));
                                let exit_reason = if pending_flip.is_some() {
                                    TARGET_FLIP_REASON
                                } else {
                                    TARGET_EXIT_REASON
                                };
                                let funding_paid = std::mem::take(&mut self.funding);
                                let entry_equity =
                                    self.excursion.entry_equity().unwrap_or(self.wallet);
                                let net = campaign.net_pnl(funding_paid);
                                let exit_equity = entry_equity + net;
                                let excursion = self
                                    .excursion
                                    .close(report.time_ms, exit_equity)
                                    .unwrap_or_else(|| {
                                        TradeExcursion::start(report.time_ms, exit_equity)
                                    });

                                let trade = self
                                    .ledger
                                    .close_campaign(
                                        campaign,
                                        CampaignClose {
                                            side: match pos_side {
                                                Side::Buy => PositionSide::Long,
                                                Side::Sell => PositionSide::Short,
                                            },
                                            exit_time: report.time_ms,
                                            last_entry_price: entry_price,
                                            last_exit_price: report.last_price,
                                            last_quantity: pos_qty,
                                            funding_paid,
                                            exit_reason,
                                            excursion,
                                        },
                                    )
                                    .clone();

                                events.push(SessionEvent::PositionClosed(Box::new(trade)));
                                let _ = venue.cancel_orders(&self.config.symbol).await;

                                if let Some(flip_order) = pending_flip {
                                    match venue.submit_order(&self.config.symbol, &flip_order).await
                                    {
                                        Ok(ack) => {
                                            let flip_delta = if flip_order.side == Side::Buy {
                                                flip_order.quantity
                                            } else {
                                                -flip_order.quantity
                                            };
                                            self.phase = Phase::Adjusting {
                                                position: None,
                                                target,
                                                delta: flip_delta,
                                                submitted_ms: report.time_ms,
                                                pending_flip: None,
                                            };
                                            events.push(SessionEvent::EntrySubmitted(ack));
                                        }
                                        Err(err) => {
                                            if let Some(last) = self.ledger.last_mut() {
                                                last.exit_reason = TARGET_EXIT_REASON.to_string();
                                            }
                                            self.phase = Phase::Flat;
                                            events.push(SessionEvent::EntryFailed(err.to_string()));
                                        }
                                    }
                                } else {
                                    self.phase = Phase::Flat;
                                }
                            }
                        }
                    }
                }
            }
        }
        events
    }

    /// Whether `report` repeats a fill already counted. The venue numbers its trades, and a
    /// repeated report would otherwise double the quantity and the commission.
    fn is_repeated_fill(&mut self, report: &ExecutionReport) -> bool {
        const REMEMBERED: usize = 1024;
        let Some(id) = report.trade_id else {
            return false;
        };
        if report.last_quantity <= Decimal::ZERO {
            return false;
        }
        if self.seen_trade_ids.contains(&id) {
            return true;
        }
        if self.seen_trade_ids.len() == REMEMBERED {
            self.seen_trade_ids.pop_front();
        }
        self.seen_trade_ids.push_back(id);
        false
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
            Phase::Adjusting { position, .. } => *position,
            _ => None,
        }
    }

    async fn dispatch<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        intent: OrderIntent,
        day_peak: Decimal,
        now_ms: i64,
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
                    submitted_ms: now_ms,
                    cancel_sent_ms: None,
                };
                self.last_entry_timeout_attempt_ms = None;
                vec![SessionEvent::EntrySubmitted(ack)]
            }
            Err(error) => vec![SessionEvent::EntryFailed(error.to_string())],
        }
    }

    /// Resolves an entry that has waited past the timeout without a terminal status.
    ///
    /// With nothing filled, the working order is cancelled and the venue is checked for a
    /// position whose fill report was lost, which is closed. With part of the entry filled, only
    /// the cancel is sent: quantity matched before it is reported afterwards, so the position is
    /// protected from the terminal report, which carries the final filled quantity. If that
    /// report never comes, the session flattens and halts rather than guess a size. A failure
    /// leaves the entry pending for a throttled retry.
    async fn resolve_stale_entry<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        now_ms: i64,
    ) -> Vec<SessionEvent> {
        let Phase::EntryPending {
            submitted_ms,
            cancel_sent_ms,
            ..
        } = &self.phase
        else {
            return Vec::new();
        };
        let waiting_since = cancel_sent_ms.unwrap_or(*submitted_ms);
        if now_ms - waiting_since < self.config.entry_timeout_ms {
            return Vec::new();
        }
        let throttled = self
            .last_entry_timeout_attempt_ms
            .is_some_and(|at| now_ms - at < self.config.halt_retry_ms);
        if throttled {
            return Vec::new();
        }
        self.last_entry_timeout_attempt_ms = Some(now_ms);

        if cancel_sent_ms.is_some() {
            // The cancel went out and the terminal report still has not arrived.
            return match self.exits.halt(venue).await {
                ExitEvent::Halted => {
                    self.phase = Phase::Flat;
                    vec![
                        SessionEvent::Exit(ExitEvent::Halted),
                        SessionEvent::EntryTimedOut {
                            closed_unreported_position: true,
                        },
                    ]
                }
                other => vec![
                    SessionEvent::Exit(other),
                    SessionEvent::EntryTimeoutFailed(
                        "the terminal report never arrived and the venue could not be flattened"
                            .to_string(),
                    ),
                ],
            };
        }

        if let Err(error) = venue.cancel_orders(&self.config.symbol).await {
            return vec![SessionEvent::EntryTimeoutFailed(error.to_string())];
        }
        let Phase::EntryPending {
            intent,
            fills,
            submitted_ms,
            ..
        } = std::mem::replace(&mut self.phase, Phase::Flat)
        else {
            return Vec::new();
        };
        if fills.quantity > Decimal::ZERO {
            self.phase = Phase::EntryPending {
                intent,
                fills,
                submitted_ms,
                cancel_sent_ms: Some(now_ms),
            };
            return Vec::new();
        }
        match venue.close_position(&self.config.symbol).await {
            Ok(closed) => {
                if closed {
                    // Its closing fill is accounted when the report arrives.
                    self.recovery = Some(Recovery {
                        intent,
                        submitted_ms,
                        fills: Fills::default(),
                    });
                }
                vec![SessionEvent::EntryTimedOut {
                    closed_unreported_position: closed,
                }]
            }
            Err(error) => {
                self.phase = Phase::EntryPending {
                    intent,
                    fills,
                    submitted_ms,
                    cancel_sent_ms: None,
                };
                vec![SessionEvent::EntryTimeoutFailed(error.to_string())]
            }
        }
    }

    async fn open_position_from<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        intent: OrderIntent,
        fills: &Fills,
        side: Side,
        time_ms: i64,
    ) -> Vec<SessionEvent> {
        let price = fills.average_price();
        let (stop_loss, take_profit) = (intent.stop_loss, intent.take_profit);
        self.excursion.open(time_ms, self.wallet);
        // Exits live on the venue so they survive a crash of this process.
        let protection = self
            .exits
            .on_entry_filled_with_trailing(
                venue,
                side,
                price,
                intent.stop_loss,
                intent.take_profit,
                intent.max_bars_hold,
                intent.trailing_atr,
                intent.breakeven_atr,
                intent.deadline_ms,
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
            intent: Some(intent),
            side,
            entry_price: price,
            quantity: fills.quantity,
            entry_time_ms: time_ms,
            exit_fills: Fills::default(),
            stop_distance: None,
            take_profit_distance: None,
        };
        vec![SessionEvent::PositionOpened {
            side,
            price,
            quantity: fills.quantity,
            time_ms,
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

        let reason = self
            .exit_hint
            .take()
            .unwrap_or_else(|| infer_exit_reason(side, exit_price, &intent));
        let entry_slippage = adverse_move(side, entry_price, intent.price) * quantity;
        let exit_slippage = trigger_slippage(side, exit_price, &intent, reason) * quantity;

        let entry_equity = self.excursion.entry_equity().unwrap_or(self.wallet);
        let net = gross - fees - funding;
        let exit_equity = entry_equity + net;
        let excursion = self
            .excursion
            .close(exit_time_ms, exit_equity)
            .unwrap_or_else(|| TradeExcursion::start(exit_time_ms, exit_equity));

        let mut campaign = Campaign::starting(entry_time_ms);
        campaign.add_increase(
            entry_price,
            quantity,
            Decimal::ZERO,
            entry_slippage,
            quantity,
        );
        campaign.add_reduction(exit_price, quantity, gross, fees, exit_slippage);

        let pos_side = match side {
            Side::Buy => PositionSide::Long,
            Side::Sell => PositionSide::Short,
        };
        let trade = self
            .ledger
            .close_campaign(
                campaign,
                CampaignClose {
                    side: pos_side,
                    exit_time: exit_time_ms,
                    last_entry_price: entry_price,
                    last_exit_price: exit_price,
                    last_quantity: quantity,
                    funding_paid: funding,
                    exit_reason: reason,
                    excursion,
                },
            )
            .clone();

        vec![
            SessionEvent::Exit(cleanup),
            SessionEvent::PositionClosed(Box::new(trade)),
        ]
    }

    async fn close_target_position<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        closed: ClosedTarget,
    ) -> Vec<SessionEvent> {
        let cleanup = self.exits.on_position_closed_by_exchange(venue).await;
        let mut campaign = self
            .campaign
            .take()
            .unwrap_or_else(|| Campaign::starting(closed.exit_time_ms));

        let gross = match closed.side {
            Side::Buy => (closed.exit_price - closed.entry_price) * closed.quantity,
            Side::Sell => (closed.entry_price - closed.exit_price) * closed.quantity,
        };
        let fees = self.fees.take().unwrap_or(
            (closed.entry_price * closed.quantity + closed.exit_price * closed.quantity)
                * self.config.fallback_fee_pct,
        );
        let funding = std::mem::take(&mut self.funding);
        let reason = self.exit_hint.take().unwrap_or_else(|| {
            match (closed.stop_distance, closed.take_profit_distance) {
                (Some(_), Some(_)) => {
                    let is_profit = match closed.side {
                        Side::Buy => closed.exit_price >= closed.entry_price,
                        Side::Sell => closed.exit_price <= closed.entry_price,
                    };
                    if is_profit {
                        "TakeProfit"
                    } else {
                        "StopLoss"
                    }
                }
                _ => "StopLoss",
            }
        });
        let slippage = Decimal::ZERO;

        campaign.add_reduction(closed.exit_price, closed.quantity, gross, fees, slippage);

        let pos_side = match closed.side {
            Side::Buy => PositionSide::Long,
            Side::Sell => PositionSide::Short,
        };
        self.rearm_blocked = Some(pos_side);

        let entry_equity = self.excursion.entry_equity().unwrap_or(self.wallet);
        let net = campaign.net_pnl(funding);
        let exit_equity = entry_equity + net;
        let excursion = self
            .excursion
            .close(closed.exit_time_ms, exit_equity)
            .unwrap_or_else(|| TradeExcursion::start(closed.exit_time_ms, exit_equity));

        let trade = self
            .ledger
            .close_campaign(
                campaign,
                CampaignClose {
                    side: pos_side,
                    exit_time: closed.exit_time_ms,
                    last_entry_price: closed.entry_price,
                    last_exit_price: closed.exit_price,
                    last_quantity: closed.quantity,
                    funding_paid: funding,
                    exit_reason: reason,
                    excursion,
                },
            )
            .clone();

        vec![
            SessionEvent::Exit(cleanup),
            SessionEvent::PositionClosed(Box::new(trade)),
        ]
    }
}

struct ClosedTarget {
    side: Side,
    entry_price: Decimal,
    quantity: Decimal,
    exit_price: Decimal,
    exit_time_ms: i64,
    stop_distance: Option<Decimal>,
    take_profit_distance: Option<Decimal>,
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
    use ports::{EntryAck, ExecutionReport, ExecutionVenue, ExitGateway, OrderRequest};
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
        /// What `close_position` answers: whether something was left to close.
        position_left: RefCell<bool>,
        fail_cancel: RefCell<bool>,
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
            take_profit: Option<Decimal>,
        ) -> Result<(), String> {
            let take_profit = take_profit.map_or_else(|| "none".to_string(), |tp| tp.to_string());
            self.calls
                .borrow_mut()
                .push(format!("place {side:?} {stop_loss} {take_profit}"));
            Ok(())
        }

        async fn cancel_orders(&self, _: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("cancel".to_string());
            if *self.fail_cancel.borrow() {
                return Err("cancel rejected".to_string());
            }
            Ok(())
        }

        async fn close_position(&self, _: &str) -> Result<bool, String> {
            self.calls.borrow_mut().push("close".to_string());
            Ok(std::mem::take(&mut *self.position_left.borrow_mut()))
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
                sent_at_ms: 0,
            })
        }

        async fn submit_order(&self, _: &str, request: &OrderRequest) -> Result<EntryAck, String> {
            self.calls.borrow_mut().push(format!(
                "order {:?} {} {}",
                request.side, request.quantity, request.reduce_only
            ));
            Ok(EntryAck {
                order_id: "order-1".to_string(),
                status: "NEW".to_string(),
                quantity: request.quantity,
                price: dec!(100),
                sent_at_ms: 0,
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
                entry_timeout_ms: 30_000,
                risk_policy: None,
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
            trade_id: None,
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
            trade_id: None,
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

    /// Asks for a long target on every bar; used to test target execution in sessions.
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

    #[tokio::test]
    async fn a_target_decision_never_reaches_the_venue_without_risk_policy() {
        let venue = FakeVenue::default();
        let mut session = TradingSession::new(
            SessionConfig {
                symbol: "BTCUSDT".to_string(),
                initial_equity: dec!(1000),
                fallback_fee_pct: dec!(0.0005),
                max_monthly_drawdown_pct: dec!(0.15),
                halt_retry_ms: 5_000,
                entry_timeout_ms: 30_000,
                risk_policy: None,
            },
            TargetOnly,
        );

        let events = session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        assert!(events.is_empty());
        assert!(venue.entries.borrow().is_empty());
        assert!(venue.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_target_decision_with_risk_policy_submits_an_order() {
        let venue = FakeVenue::default();
        let policy = domain::RiskPolicy::new(dec!(100_000), dec!(50_000), dec!(0.05)).unwrap();
        let mut session = TradingSession::new(
            SessionConfig {
                symbol: "BTCUSDT".to_string(),
                initial_equity: dec!(1000),
                fallback_fee_pct: dec!(0.0005),
                max_monthly_drawdown_pct: dec!(0.15),
                halt_retry_ms: 5_000,
                entry_timeout_ms: 30_000,
                risk_policy: Some(policy),
            },
            TargetOnly,
        );

        let events = session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        assert_eq!(events.len(), 1); // EntrySubmitted
        assert!(matches!(events[0], SessionEvent::EntrySubmitted(_)));
        let calls = venue.calls.borrow().clone();
        assert_eq!(calls, vec!["order Buy 2.000 false".to_string()]); // 1000 * 0.01 / 5 = 2 BTC
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

    const TIMEOUT: i64 = 30_000;

    #[tokio::test]
    async fn an_entry_still_working_before_the_timeout_is_left_alone() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        let events = session
            .on_trade(&venue, JAN_1 + 10 + TIMEOUT - 1, dec!(100))
            .await;

        assert!(events.is_empty());
        assert!(session.is_exposed());
        assert!(!venue.calls().contains(&"cancel".to_string()));
    }

    #[tokio::test]
    async fn an_entry_that_never_terminates_is_cancelled_and_the_session_returns_to_flat() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent(), long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        let events = session
            .on_trade(&venue, JAN_1 + 10 + TIMEOUT, dec!(100))
            .await;

        assert!(events.contains(&SessionEvent::EntryTimedOut {
            closed_unreported_position: false
        }));
        assert_eq!(venue.calls(), vec!["cancel", "close"]);
        assert!(!session.is_exposed());
        // Ready for the next signal.
        let next = session
            .on_bar(&venue, &bar(JAN_1 + 20_000_000, dec!(100)))
            .await;
        assert!(next
            .iter()
            .any(|e| matches!(e, SessionEvent::EntrySubmitted(_))));
    }

    #[tokio::test]
    async fn a_position_the_session_never_heard_about_is_closed_at_the_timeout() {
        // The fill report was lost: the venue holds a position the session knows nothing of.
        let venue = FakeVenue::default();
        *venue.position_left.borrow_mut() = true;
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        let events = session
            .on_trade(&venue, JAN_1 + 10 + TIMEOUT, dec!(100))
            .await;

        assert!(events.contains(&SessionEvent::EntryTimedOut {
            closed_unreported_position: true
        }));
        assert!(!session.is_exposed());
    }

    /// A long entry with 0.4 filled, then the timeout: the cancel has been sent.
    async fn partially_filled_then_timed_out(
        venue: &FakeVenue,
        session: &mut TradingSession<Scripted>,
    ) -> Vec<SessionEvent> {
        session.on_bar(venue, &bar(JAN_1 + 10, dec!(100))).await;
        session
            .on_execution(
                venue,
                &fill(
                    Side::Buy,
                    dec!(0.4),
                    dec!(100),
                    false,
                    JAN_1 + 20,
                    dec!(0.02),
                ),
            )
            .await;
        session
            .on_trade(venue, JAN_1 + 10 + TIMEOUT, dec!(100))
            .await
    }

    #[tokio::test]
    async fn a_partial_fill_waits_for_the_terminal_report_before_it_is_protected() {
        // Quantity that matched before the cancel is reported afterwards, so the exits must be
        // sized from the terminal report, not from the fills seen at the timeout.
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);

        let events = partially_filled_then_timed_out(&venue, &mut session).await;

        assert!(events.is_empty());
        assert_eq!(venue.calls(), vec!["cancel"]);
        assert!(session.is_exposed());
        assert!(session.open_position().is_none());
    }

    #[tokio::test]
    async fn the_terminal_report_after_the_cancel_protects_everything_that_filled() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        partially_filled_then_timed_out(&venue, &mut session).await;

        // 0.1 more had matched before the cancel; its report arrives last.
        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Buy,
                    dec!(0.1),
                    dec!(100),
                    true,
                    JAN_1 + 40_000,
                    dec!(0.005),
                ),
            )
            .await;

        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::PositionOpened { quantity, .. } if *quantity == dec!(0.5)
        )));
        assert!(venue.calls().contains(&"place Buy 90 110".to_string()));
        assert_eq!(session.open_position().map(|p| p.2), Some(dec!(0.5)));
    }

    #[tokio::test]
    async fn a_cancellation_report_protects_the_quantity_that_had_filled() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        partially_filled_then_timed_out(&venue, &mut session).await;

        let events = session
            .on_execution(&venue, &terminated(Side::Buy, JAN_1 + 40_000))
            .await;

        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::PositionOpened { quantity, .. } if *quantity == dec!(0.4)
        )));
    }

    #[tokio::test]
    async fn a_terminal_report_that_never_arrives_flattens_and_halts_the_session() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        partially_filled_then_timed_out(&venue, &mut session).await;

        let events = session
            .on_trade(&venue, JAN_1 + 10 + 2 * TIMEOUT, dec!(100))
            .await;

        assert!(events.contains(&SessionEvent::EntryTimedOut {
            closed_unreported_position: true
        }));
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::Exit(ExitEvent::Halted))));
        assert!(venue.calls().contains(&"close".to_string()));
        assert!(session.is_halted());
        assert!(!venue.calls().contains(&"place Buy 90 110".to_string()));
    }

    #[tokio::test]
    async fn a_failed_cancel_keeps_the_entry_pending_and_is_retried_after_the_throttle() {
        let venue = FakeVenue::default();
        *venue.fail_cancel.borrow_mut() = true;
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        let due = JAN_1 + 10 + TIMEOUT;

        let first = session.on_trade(&venue, due, dec!(100)).await;
        let throttled = session.on_trade(&venue, due + 1_000, dec!(100)).await;
        *venue.fail_cancel.borrow_mut() = false;
        let retried = session.on_trade(&venue, due + 5_000, dec!(100)).await;

        assert!(first
            .iter()
            .any(|e| matches!(e, SessionEvent::EntryTimeoutFailed(_))));
        assert!(throttled.is_empty());
        assert!(retried
            .iter()
            .any(|e| matches!(e, SessionEvent::EntryTimedOut { .. })));
        assert!(!session.is_exposed());
    }

    #[tokio::test]
    async fn a_fill_that_arrives_after_the_timeout_flattens_and_stops_the_session() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        session
            .on_trade(&venue, JAN_1 + 10 + TIMEOUT, dec!(100))
            .await;

        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Buy,
                    dec!(1),
                    dec!(100),
                    true,
                    JAN_1 + 40_000,
                    dec!(0.05),
                ),
            )
            .await;

        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::Exit(ExitEvent::Halted))));
        assert!(session.is_halted());
    }

    fn with_trade_id(mut report: ExecutionReport, id: i64) -> ExecutionReport {
        report.trade_id = Some(id);
        report
    }

    #[tokio::test]
    async fn a_repeated_fill_is_counted_once() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        let first = with_trade_id(
            fill(
                Side::Buy,
                dec!(0.6),
                dec!(100),
                false,
                JAN_1 + 20,
                dec!(0.03),
            ),
            41,
        );

        session.on_execution(&venue, &first).await;
        // The stream delivers the same trade again, e.g. after a reconnection.
        let repeated = session.on_execution(&venue, &first).await;
        let last = with_trade_id(
            fill(
                Side::Buy,
                dec!(0.4),
                dec!(100),
                true,
                JAN_1 + 30,
                dec!(0.02),
            ),
            42,
        );
        let opened = session.on_execution(&venue, &last).await;

        assert!(repeated.is_empty());
        assert!(opened.iter().any(|e| matches!(
            e,
            SessionEvent::PositionOpened { quantity, .. } if *quantity == dec!(1.0)
        )));
        // The commission of the repeated fill is not charged twice.
        let closed = session
            .on_execution(
                &venue,
                &with_trade_id(
                    fill(
                        Side::Sell,
                        dec!(1),
                        dec!(110),
                        true,
                        JAN_1 + 90_000,
                        dec!(0.05),
                    ),
                    43,
                ),
            )
            .await;
        assert_eq!(closed_trade(&closed).fees_paid, dec!(0.10));
    }

    #[tokio::test]
    async fn reports_without_a_trade_id_are_never_treated_as_repeats() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        session
            .on_execution(
                &venue,
                &fill(
                    Side::Buy,
                    dec!(0.5),
                    dec!(100),
                    false,
                    JAN_1 + 20,
                    dec!(0.02),
                ),
            )
            .await;
        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Buy,
                    dec!(0.5),
                    dec!(100),
                    true,
                    JAN_1 + 30,
                    dec!(0.02),
                ),
            )
            .await;

        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::PositionOpened { quantity, .. } if *quantity == dec!(1.0)
        )));
    }

    #[tokio::test]
    async fn a_position_recovered_by_the_timeout_is_accounted_as_a_closed_trade() {
        // The entry fill report was lost; the timeout closed the position it found.
        let venue = FakeVenue::default();
        *venue.position_left.borrow_mut() = true;
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;
        session
            .on_trade(&venue, JAN_1 + 10 + TIMEOUT, dec!(100))
            .await;

        // The recovery close fills at 101 and its report arrives while the session is flat.
        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(101),
                    true,
                    JAN_1 + 40_000,
                    dec!(0.05),
                ),
            )
            .await;

        let trade = closed_trade(&events);
        assert_eq!(trade.exit_reason, "RecoveredUnreportedEntry");
        // The entry fill was never seen: the signal price stands in for it.
        assert_eq!(trade.entry_price, dec!(100));
        assert_eq!(trade.exit_price, dec!(101));
        assert_eq!(trade.quantity, dec!(1));
        assert_eq!(trade.fees_paid, dec!(0.05));
        assert_eq!(session.closed_trades().len(), 1);
        // The anomaly still stops the bot for inspection.
        assert!(session.is_halted());
    }

    #[tokio::test]
    async fn a_fill_with_nothing_to_recover_is_not_booked_as_a_trade() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);

        let events = session
            .on_execution(
                &venue,
                &fill(
                    Side::Sell,
                    dec!(1),
                    dec!(101),
                    true,
                    JAN_1 + 40_000,
                    dec!(0.05),
                ),
            )
            .await;

        assert!(!events
            .iter()
            .any(|e| matches!(e, SessionEvent::PositionClosed(_))));
        assert!(session.closed_trades().is_empty());
        assert!(session.is_halted());
    }

    #[tokio::test]
    async fn the_entry_timeout_also_runs_when_no_market_trade_arrives() {
        // The public stream is silent: only the clock moves.
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);
        session.on_bar(&venue, &bar(JAN_1 + 10, dec!(100))).await;

        let early = session.on_idle(&venue, JAN_1 + 10 + TIMEOUT - 1).await;
        let late = session.on_idle(&venue, JAN_1 + 10 + TIMEOUT).await;

        assert!(early.is_empty());
        assert!(late.contains(&SessionEvent::EntryTimedOut {
            closed_unreported_position: false
        }));
        assert!(!session.is_exposed());
    }

    #[tokio::test]
    async fn idling_does_nothing_without_a_pending_entry() {
        let venue = FakeVenue::default();
        let mut session = session(vec![long_intent()]);

        let events = session.on_idle(&venue, JAN_1 + 10 * TIMEOUT).await;

        assert!(events.is_empty());
        assert!(venue.calls().is_empty());
    }
}
