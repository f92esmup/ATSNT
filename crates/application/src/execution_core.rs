//! Position accounting shared by every runner that simulates fills.
//!
//! [`ExecutionCore`] was extracted from the bar engine (odd swarm-consensus T5.1,
//! strategy spec `docs/strategy/03-swarm-consensus.md` §3.5). It owns the open position, the
//! pending market order, the [`ProtectiveExits`], isolated margin and liquidation, funding, the
//! fill price, slippage and fee math, and the [`TradeLedger`] that turns a closed position into a
//! [`ClosedTrade`]. The engine keeps its loop (rollovers, breakers, kill switch, mark to market,
//! excursion tracking) and owns the cash balance, which it lends to the core on every call.
//!
//! Every `Decimal` operation keeps the shape and order it had in the engine, so results are
//! byte-identical to the pre-extraction engine (proved by `tests/golden_execution.rs`).

use domain::futures;
use domain::MarketType;
use domain::{OrderIntent, Position, PositionSide, ProtectiveExits, Side};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::config::BacktestConfig;
use crate::metrics::{ClosedTrade, TradeExcursion};
use crate::price_event::PriceEvent;

pub use domain::accounting::fills::market_fill_price;
use domain::accounting::fills::{
    closing_side, fill_fee, opening_side, opening_slippage_per_unit, position_side, vwap,
};

/// Result of evaluating a due pending entry against a price event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingEntryOutcome {
    /// No pending entry was due on this event.
    NoDueEntry,
    /// A market order filled at the event open.
    FilledMarket,
    /// A limit order filled at the limit price.
    FilledLimit,
    /// A limit order was not reached by the event and remains resting.
    RestingLimit { bars_pending: usize },
    /// A limit order exceeded its Time-In-Force timeout and was canceled.
    ExpiredLimit,
    /// The order was due but could not open: the rejection reason it is counted under.
    Refused(&'static str),
}

/// A submitted entry: the strategy intent and the quantity sized when it was signalled.
#[derive(Debug, Clone)]
pub struct PendingEntry {
    /// The strategy's triple-barrier intent.
    pub intent: OrderIntent,
    /// Quantity sized at the signal price.
    pub quantity: Decimal,
    /// Number of bars this entry has waited without filling (for Limit orders).
    pub bars_pending: usize,
}

impl PendingEntry {
    /// Constructs a pending entry initialized with zero bars pending.
    pub fn new(intent: OrderIntent, quantity: Decimal) -> Self {
        Self {
            intent,
            quantity,
            bars_pending: 0,
        }
    }
}

/// Running sums of one campaign: from the fill that leaves flat to the fill that returns to flat
/// or flips (strategy spec §11.9).
///
/// Every position is a campaign. A triple-barrier entry is the campaign with one increasing fill
/// and one reducing fill (the close), so both decision kinds share one [`ClosedTrade`] builder.
/// The bar engine and the trading session both feed their fills here and close campaigns
/// through [`TradeLedger::close_campaign`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Campaign {
    entry_time: i64,
    increase_notional: Decimal,
    increase_quantity: Decimal,
    increase_fills: u32,
    reduce_notional: Decimal,
    reduce_quantity: Decimal,
    reduce_fills: u32,
    peak_quantity: Decimal,
    realized_pnl: Decimal,
    fees: Decimal,
    slippage: Decimal,
}

impl Campaign {
    /// An empty campaign whose first fill happens at `entry_time`.
    pub(crate) fn starting(entry_time: i64) -> Self {
        Self {
            entry_time,
            ..Self::default()
        }
    }

    /// Adds a fill of `quantity` at `price` that increased the position to `position_quantity`.
    pub(crate) fn add_increase(
        &mut self,
        price: Decimal,
        quantity: Decimal,
        fee: Decimal,
        slippage: Decimal,
        position_quantity: Decimal,
    ) {
        self.increase_notional += price * quantity;
        self.increase_quantity += quantity;
        self.increase_fills += 1;
        self.fees += fee;
        self.slippage += slippage;
        self.peak_quantity = self.peak_quantity.max(position_quantity);
    }

    /// Adds a fill of `quantity` at `price` that reduced the position and realised `pnl`.
    pub(crate) fn add_reduction(
        &mut self,
        price: Decimal,
        quantity: Decimal,
        pnl: Decimal,
        fee: Decimal,
        slippage: Decimal,
    ) {
        self.reduce_notional += price * quantity;
        self.reduce_quantity += quantity;
        self.reduce_fills += 1;
        self.realized_pnl += pnl;
        self.fees += fee;
        self.slippage += slippage;
    }

    /// Realised PnL net of the campaign's fees and of `funding_paid`.
    pub(crate) fn net_pnl(&self, funding_paid: Decimal) -> Decimal {
        self.realized_pnl - self.fees - funding_paid
    }
}

/// How a campaign ended: its last (closing) fill and what the trade is labelled with.
#[derive(Debug, Clone)]
pub(crate) struct CampaignClose<'a> {
    /// Side of the position that closed.
    pub(crate) side: PositionSide,
    /// Time of the closing fill.
    pub(crate) exit_time: i64,
    /// Average entry price of the position when it closed; the trade's entry price when the
    /// campaign had a single increasing fill.
    pub(crate) last_entry_price: Decimal,
    /// Price of the closing fill; the trade's exit price when the campaign had a single
    /// reducing fill.
    pub(crate) last_exit_price: Decimal,
    /// Quantity the closing fill closed.
    pub(crate) last_quantity: Decimal,
    /// Funding paid by the position over the campaign.
    pub(crate) funding_paid: Decimal,
    /// Exit reason string of the trade.
    pub(crate) exit_reason: &'a str,
    /// The trade's finished excursion.
    pub(crate) excursion: TradeExcursion,
}

/// A barrier exit decided on one bar: a market order to close the whole position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarrierExit {
    /// Fill price, slippage included.
    pub price: Decimal,
    /// Slippage per unit reported for the fill, measured against the trigger.
    pub slippage_per_unit: Decimal,
    /// Exit reason string (`StopLoss`, `TakeProfit` or `TimeBarrier`).
    pub reason: &'static str,
}

/// The highest leverage the configuration allows, as a whole number (1 when unreadable).
fn max_leverage(config: &BacktestConfig) -> u32 {
    config.max_leverage.trunc().to_u32().unwrap_or(1)
}

/// The exchange's margin rule: the lowest supported leverage at which `equity` margins
/// `notional` (initial margin or maintenance margin, whichever is larger) plus `fee`.
fn margin_leverage(
    notional: Decimal,
    fee: Decimal,
    equity: Decimal,
    config: &BacktestConfig,
) -> Option<u32> {
    futures::minimum_leverage(
        notional,
        notional * config.maintenance_margin_rate,
        fee,
        equity,
        max_leverage(config),
    )
}

/// Liquidation price of `position` posted with `margin` on isolated margin.
fn liquidation_of(position: &Position, margin: Decimal, config: &BacktestConfig) -> Decimal {
    let maintenance = position.quantity * position.entry_price * config.maintenance_margin_rate;
    futures::liquidation_price(
        opening_side(position.side),
        position.entry_price,
        position.quantity,
        margin,
        maintenance,
    )
}

/// Evaluates the protective exits of a position on `side` against one price event.
///
/// Barrier exits are market orders: they fill at the trigger or, when the event gapped through
/// it, at the open, and pay slippage against the position. The stop is checked first, then
/// the take profit (when there is one), then the time limit (when there is one) at the close.
/// The reported stop slippage is measured against the trigger so it includes the gap.
pub fn barrier_exit(
    side: PositionSide,
    exits: &ProtectiveExits,
    event: impl Into<PriceEvent>,
    slippage_pct: Decimal,
) -> Option<BarrierExit> {
    let bar = event.into();
    let close_side = closing_side(side);
    let time_due = exits
        .deadline_ms
        .is_some_and(|deadline| bar.end_ms >= deadline);
    match side {
        PositionSide::Long => {
            if bar.low <= exits.stop {
                let reference = exits.stop.min(bar.open);
                let (price, _) = market_fill_price(close_side, reference, slippage_pct);
                let slippage_per_unit = (exits.stop - price).max(Decimal::ZERO);
                Some(BarrierExit {
                    price,
                    slippage_per_unit,
                    reason: "StopLoss",
                })
            } else if let Some(take_profit) = exits.take_profit.filter(|tp| bar.high >= *tp) {
                let reference = take_profit.max(bar.open);
                // A target never gaps against the order: the slippage is what the market sell
                // pays on the reference, even when the fill still beats the trigger.
                let (price, slippage_per_unit) =
                    market_fill_price(close_side, reference, slippage_pct);
                Some(BarrierExit {
                    price,
                    slippage_per_unit,
                    reason: "TakeProfit",
                })
            } else if time_due {
                let (price, slippage_per_unit) =
                    market_fill_price(close_side, bar.close, slippage_pct);
                Some(BarrierExit {
                    price,
                    slippage_per_unit,
                    reason: "TimeBarrier",
                })
            } else {
                None
            }
        }
        PositionSide::Short => {
            if bar.high >= exits.stop {
                let reference = exits.stop.max(bar.open);
                let (price, _) = market_fill_price(close_side, reference, slippage_pct);
                let slippage_per_unit = (price - exits.stop).max(Decimal::ZERO);
                Some(BarrierExit {
                    price,
                    slippage_per_unit,
                    reason: "StopLoss",
                })
            } else if let Some(take_profit) = exits.take_profit.filter(|tp| bar.low <= *tp) {
                let reference = take_profit.min(bar.open);
                let (price, slippage_per_unit) =
                    market_fill_price(close_side, reference, slippage_pct);
                Some(BarrierExit {
                    price,
                    slippage_per_unit,
                    reason: "TakeProfit",
                })
            } else if time_due {
                let (price, slippage_per_unit) =
                    market_fill_price(close_side, bar.close, slippage_pct);
                Some(BarrierExit {
                    price,
                    slippage_per_unit,
                    reason: "TimeBarrier",
                })
            } else {
                None
            }
        }
    }
}

/// The closed trades of a simulation, in close order.
#[derive(Debug, Clone, Default)]
pub struct TradeLedger {
    trades: Vec<ClosedTrade>,
}

impl TradeLedger {
    /// Every closed trade so far.
    pub fn trades(&self) -> &[ClosedTrade] {
        &self.trades
    }

    /// The most recently closed trade, mutable (post-exit drawdown attribution).
    pub fn last_mut(&mut self) -> Option<&mut ClosedTrade> {
        self.trades.last_mut()
    }

    /// Consumes the ledger and returns its trades.
    pub fn into_trades(self) -> Vec<ClosedTrade> {
        self.trades
    }

    /// Records a finished campaign as one [`ClosedTrade`] and returns it. This is the only
    /// builder of closed trades in the crate, for both decision kinds and every runner.
    ///
    /// The entry price is the VWAP of the increasing fills, the exit price that of the reducing
    /// fills, the quantity the peak absolute quantity, and the PnL, fees, slippage and funding
    /// are the campaign sums. A VWAP over a single fill is that fill's price, so a triple-barrier
    /// entry (one increasing fill, one reducing fill) keeps the prices it always had.
    pub(crate) fn close_campaign(
        &mut self,
        campaign: Campaign,
        close: CampaignClose<'_>,
    ) -> &ClosedTrade {
        let CampaignClose {
            side,
            exit_time,
            last_entry_price,
            last_exit_price,
            last_quantity,
            funding_paid,
            exit_reason,
            excursion,
        } = close;
        let pnl_net = campaign.net_pnl(funding_paid);
        let entry_price = if campaign.increase_fills > 1 {
            vwap(campaign.increase_notional, campaign.increase_quantity)
        } else {
            last_entry_price
        };
        let exit_vwap = if campaign.reduce_fills > 1 {
            vwap(campaign.reduce_notional, campaign.reduce_quantity)
        } else {
            last_exit_price
        };
        let quantity = campaign.peak_quantity.max(last_quantity);
        let return_pct = if entry_price > Decimal::ZERO && quantity > Decimal::ZERO {
            pnl_net / (entry_price * quantity)
        } else {
            Decimal::ZERO
        };
        let entry_time = campaign.entry_time;
        let holding_duration_seconds = ((exit_time - entry_time) / 1000).max(0);
        self.trades.push(ClosedTrade {
            entry_time,
            exit_time,
            entry_price,
            exit_price: exit_vwap,
            quantity,
            side,
            pnl_gross: campaign.realized_pnl,
            fees_paid: campaign.fees,
            funding_paid,
            slippage_paid: campaign.slippage,
            pnl_net,
            return_pct,
            exit_reason: exit_reason.to_string(),
            holding_duration_seconds,
            excursion,
        });
        let index = self.trades.len() - 1;
        &self.trades[index]
    }
}

/// Position state and accounting of one simulated account on isolated margin.
///
/// The cash balance is not stored here: every method that moves cash receives it as
/// `equity`, so the owner keeps one balance for its breakers and mark to market.
#[derive(Debug, Clone, Default)]
pub struct ExecutionCore {
    position: Option<Position>,
    intent: Option<OrderIntent>,
    exits: Option<ProtectiveExits>,
    /// Market order sized at the signal event and waiting for its due time to fill.
    pending: Option<PendingEntry>,
    /// Earliest event start, in Unix milliseconds, at which `pending` may fill.
    pending_due_ms: i64,
    /// Isolated margin posted for the open position.
    margin: Decimal,
    /// Funding paid by the open position so far.
    funding_accrued: Decimal,
    /// Sums of the open campaign; `Some` exactly while a position opened by a fill is open.
    campaign: Option<Campaign>,
    ledger: TradeLedger,
}

impl ExecutionCore {
    /// An empty core: flat, nothing pending, no trades.
    pub fn new() -> Self {
        Self::default()
    }

    /// The open position, if any.
    pub fn position(&self) -> Option<&Position> {
        self.position.as_ref()
    }

    /// The intent of the open position, if any.
    pub fn intent(&self) -> Option<&OrderIntent> {
        self.intent.as_ref()
    }

    /// The protective exits of the open position, if any.
    pub fn exits(&self) -> Option<&ProtectiveExits> {
        self.exits.as_ref()
    }

    /// Returns `(free_quote, free_base)` balance amounts for Spot accounts.
    ///
    /// For a flat account, `free_quote == equity` and `free_base == 0`.
    /// When holding a position, `free_base == position.quantity`, and
    /// `free_quote == equity - (position.quantity * position.entry_price)`.
    pub fn spot_balances(&self, equity: Decimal) -> (Decimal, Decimal) {
        match &self.position {
            Some(pos) => {
                let notional = pos.quantity * pos.entry_price;
                let free_quote = (equity - notional).max(Decimal::ZERO);
                (free_quote, pos.quantity)
            }
            None => (equity.max(Decimal::ZERO), Decimal::ZERO),
        }
    }

    /// The market order waiting to fill, if any.
    pub fn pending(&self) -> Option<&PendingEntry> {
        self.pending.as_ref()
    }

    /// Queues a market order that is due immediately: it fills on the next event.
    pub fn submit(&mut self, pending: PendingEntry) {
        self.submit_due(pending, i64::MIN);
    }

    /// Queues a market order that may fill on the first event starting at or after `due_ms`.
    pub fn submit_due(&mut self, pending: PendingEntry, due_ms: i64) {
        self.pending = Some(pending);
        self.pending_due_ms = due_ms;
    }

    /// Earliest event start at which the pending order may fill, if one is pending.
    pub fn pending_due_ms(&self) -> Option<i64> {
        self.pending.as_ref().map(|_| self.pending_due_ms)
    }

    /// Removes and returns the pending market order, whether or not it is due.
    pub fn take_pending(&mut self) -> Option<PendingEntry> {
        self.pending.take()
    }

    /// Removes and returns the pending market order when it is due on an event starting at
    /// `event_start_ms` (`due_ms <= event_start_ms`); otherwise leaves it pending.
    pub fn take_due_pending(&mut self, event_start_ms: i64) -> Option<PendingEntry> {
        if self.pending_due_ms <= event_start_ms {
            self.pending.take()
        } else {
            None
        }
    }

    /// Checks and executes any due pending entry (market or limit) on this event.
    /// Returns the outcome of the evaluation.
    pub fn step_pending_entry(
        &mut self,
        event: impl Into<PriceEvent>,
        config: &BacktestConfig,
        equity: &mut Decimal,
    ) -> PendingEntryOutcome {
        let bar = event.into();
        let Some(pending) = self.pending.as_ref() else {
            return PendingEntryOutcome::NoDueEntry;
        };
        if self.pending_due_ms > bar.start_ms {
            return PendingEntryOutcome::NoDueEntry;
        }

        match pending.intent.order_type {
            domain::OrderType::Market => {
                if let Some(pending) = self.pending.take() {
                    match self.fill_entry(pending, bar, config, equity) {
                        Ok(()) => PendingEntryOutcome::FilledMarket,
                        Err(reason) => PendingEntryOutcome::Refused(reason.kind()),
                    }
                } else {
                    PendingEntryOutcome::NoDueEntry
                }
            }
            domain::OrderType::Limit => {
                let limit_price = pending.intent.price;
                let touched = match pending.intent.side {
                    Side::Buy => bar.low <= limit_price,
                    Side::Sell => bar.high >= limit_price,
                };

                if touched {
                    if let Some(pending) = self.pending.take() {
                        match self.fill_entry(pending, bar, config, equity) {
                            Ok(()) => PendingEntryOutcome::FilledLimit,
                            Err(reason) => PendingEntryOutcome::Refused(reason.kind()),
                        }
                    } else {
                        PendingEntryOutcome::NoDueEntry
                    }
                } else {
                    let timeout = pending.intent.time_in_force_bars.unwrap_or(3);
                    if let Some(entry) = self.pending.as_mut() {
                        entry.bars_pending += 1;
                        if entry.bars_pending >= timeout {
                            self.pending = None;
                            return PendingEntryOutcome::ExpiredLimit;
                        }
                        PendingEntryOutcome::RestingLimit {
                            bars_pending: entry.bars_pending,
                        }
                    } else {
                        PendingEntryOutcome::NoDueEntry
                    }
                }
            }
        }
    }

    /// Signed quantity of the open position: positive long, negative short, 0 flat.
    pub fn signed_quantity(&self) -> Decimal {
        match &self.position {
            Some(position) => match position.side {
                PositionSide::Long => position.quantity,
                PositionSide::Short => -position.quantity,
            },
            None => Decimal::ZERO,
        }
    }

    /// Isolated margin posted for the open position (0 when flat).
    pub fn margin(&self) -> Decimal {
        self.margin
    }

    /// The closed trades so far.
    pub fn ledger(&self) -> &TradeLedger {
        &self.ledger
    }

    /// The closed trades so far, mutable.
    pub fn ledger_mut(&mut self) -> &mut TradeLedger {
        &mut self.ledger
    }

    /// Consumes the core and returns its closed trades.
    pub fn into_trades(self) -> Vec<ClosedTrade> {
        self.ledger.into_trades()
    }

    /// Puts `position` on the books without a fill, for tests of the owner's equity math.
    #[doc(hidden)]
    pub fn set_position_for_test(&mut self, position: Position) {
        self.position = Some(position);
    }

    /// Charges every funding settlement in `(previous_end, event.end_ms]` to the open position,
    /// marked at the event's open.
    pub fn settle_funding(
        &mut self,
        previous_end: i64,
        event: impl Into<PriceEvent>,
        config: &BacktestConfig,
        equity: &mut Decimal,
    ) {
        if config.market_type == MarketType::Spot || config.funding_rate_pct == Decimal::ZERO {
            return;
        }
        let bar = event.into();
        let Some(position) = &self.position else {
            return;
        };
        let side = opening_side(position.side);
        let settlements =
            futures::funding_settlements(futures::FUNDING_INTERVAL_MS, previous_end, bar.end_ms);
        for _ in settlements {
            let payment = futures::funding_payment(
                side,
                position.quantity,
                bar.open,
                config.funding_rate_pct,
            );
            *equity -= payment;
            self.funding_accrued += payment;
        }
    }

    /// Fills a pending market order at the event's open plus adverse slippage; the entry time
    /// is the event's start.
    ///
    /// The exchange runs the position at the lowest leverage the balance can margin. An order
    /// it cannot margin at any supported leverage does not open: the gate sizes entries to fit,
    /// so this only happens when the fill gapped past the slippage it allowed for.
    pub fn fill_entry(
        &mut self,
        pending: PendingEntry,
        event: impl Into<PriceEvent>,
        config: &BacktestConfig,
        equity: &mut Decimal,
    ) -> Result<(), domain::EntryRefusal> {
        let bar = event.into();
        let PendingEntry {
            intent, quantity, ..
        } = pending;
        if config.market_type == MarketType::Spot && intent.side == Side::Sell {
            return Err(domain::EntryRefusal::SpotShortingDisallowed);
        }
        let is_limit = intent.order_type == domain::OrderType::Limit;

        let (fill_price, entry_fee, adverse_per_unit) = if is_limit {
            let price = intent.price;
            let fee = fill_fee(price, quantity, config.maker_fee_pct);
            (price, fee, Decimal::ZERO)
        } else {
            let (market_price, slip) =
                market_fill_price(intent.side, bar.open, config.slippage_pct);
            let fee = fill_fee(market_price, quantity, config.taker_fee_pct);
            let adverse = opening_slippage_per_unit(intent.side, slip, bar.open, intent.price);
            (market_price, fee, adverse)
        };

        let notional = fill_price * quantity;
        let Some(leverage) = margin_leverage(notional, entry_fee, *equity, config) else {
            return Err(domain::EntryRefusal::InsufficientMargin);
        };
        // Slippage is already embedded in fill_price; do not deduct from cash equity here
        *equity -= entry_fee;

        if let Ok(pos) = Position::new(position_side(intent.side), quantity, fill_price) {
            let mut campaign = Campaign::starting(bar.start_ms);
            campaign.add_increase(
                fill_price,
                quantity,
                entry_fee,
                adverse_per_unit * quantity,
                pos.quantity,
            );
            self.campaign = Some(campaign);
            self.position = Some(pos);
            self.exits = Some(ProtectiveExits::from_intent(&intent));
            self.intent = Some(intent);
            self.margin = notional / Decimal::from(leverage);
            self.funding_accrued = Decimal::ZERO;
        }
        Ok(())
    }

    /// Closes the position when it is liquidated or a protective exit triggers on this event, at
    /// the event's end time. Liquidation is evaluated first.
    pub fn evaluate_exits(
        &mut self,
        event: impl Into<PriceEvent>,
        config: &BacktestConfig,
        equity: &mut Decimal,
        excursion: &mut Option<TradeExcursion>,
    ) {
        let bar = event.into();
        if self.liquidate_if_due(&bar, config, equity, excursion) {
            return;
        }
        let (Some(mut exits), Some(position)) = (self.exits, &self.position) else {
            return;
        };
        if let Some(exit) = barrier_exit(position.side, &exits, bar, config.slippage_pct) {
            let is_taker = !(config.resting_maker_exits && exit.reason == "TakeProfit");
            let slippage = if !is_taker {
                Decimal::ZERO
            } else {
                exit.slippage_per_unit
            };
            self.execute_close(
                bar.end_ms,
                exit.price,
                slippage,
                is_taker,
                exit.reason,
                config,
                equity,
                excursion,
            );
            return;
        }

        // Trailing / Breakeven stop ratchet for subsequent events (AFML Ch. 10):
        if exits.breakeven_dist.is_some() || exits.step_trailing_atr.is_some() {
            let fav_excursion = match position.side {
                PositionSide::Long => bar.high - position.entry_price,
                PositionSide::Short => position.entry_price - bar.low,
            };

            if fav_excursion > Decimal::ZERO {
                let mut new_stop = exits.stop;

                // 1. Breakeven check: move stop to entry_price once favorable excursion >= breakeven_dist
                if let Some(be_dist) = exits.breakeven_dist {
                    if be_dist > Decimal::ZERO && fav_excursion >= be_dist {
                        match position.side {
                            PositionSide::Long => {
                                new_stop = new_stop.max(position.entry_price);
                            }
                            PositionSide::Short => {
                                new_stop = new_stop.min(position.entry_price);
                            }
                        }
                    }
                }

                // 2. Trailing step ratchet check (decoupled from breakeven):
                if let Some(step) = exits.step_trailing_atr {
                    if step > Decimal::ZERO && fav_excursion >= step {
                        let k = (fav_excursion / step).floor();
                        match position.side {
                            PositionSide::Long => {
                                let trailing_stop =
                                    position.entry_price + (k - Decimal::ONE) * step;
                                new_stop = new_stop.max(trailing_stop);
                            }
                            PositionSide::Short => {
                                let trailing_stop =
                                    position.entry_price - (k - Decimal::ONE) * step;
                                new_stop = new_stop.min(trailing_stop);
                            }
                        }
                    }
                }

                if new_stop != exits.stop {
                    exits.stop = new_stop;
                    self.exits = Some(exits);
                }
            }
        }
    }

    /// Liquidation price of the open position, if one is open.
    pub fn liquidation_price(&self, config: &BacktestConfig) -> Option<Decimal> {
        if config.market_type == MarketType::Spot || config.maintenance_margin_rate == Decimal::ZERO
        {
            return None;
        }
        let position = self.position.as_ref()?;
        Some(liquidation_of(position, self.margin, config))
    }

    /// Closes the position at its bankruptcy price when the exchange would liquidate it on
    /// this bar: the mark reaches the liquidation price before the stop trades, or gaps through.
    fn liquidate_if_due(
        &mut self,
        bar: &PriceEvent,
        config: &BacktestConfig,
        equity: &mut Decimal,
        excursion: &mut Option<TradeExcursion>,
    ) -> bool {
        if config.market_type == MarketType::Spot || config.maintenance_margin_rate == Decimal::ZERO
        {
            return false;
        }
        let (Some(liquidation), Some(exits), Some(position)) = (
            self.liquidation_price(config),
            self.exits,
            self.position.clone(),
        ) else {
            return false;
        };
        let (reached, gapped, stop_first) = match position.side {
            PositionSide::Long => (
                bar.low <= liquidation,
                bar.open <= liquidation,
                exits.stop > liquidation,
            ),
            PositionSide::Short => (
                bar.high >= liquidation,
                bar.open >= liquidation,
                exits.stop < liquidation,
            ),
        };
        if !reached || (stop_first && !gapped) {
            return false;
        }
        let bankruptcy = futures::bankruptcy_price(
            opening_side(position.side),
            position.entry_price,
            position.quantity,
            self.margin,
        );
        self.close_with_fee_rate(
            bar.end_ms,
            bankruptcy,
            Decimal::ZERO,
            Decimal::ZERO,
            "Liquidation",
            equity,
            excursion,
        );
        true
    }

    /// Closes the whole position at `exit_price`, paying the taker or maker fee.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_close(
        &mut self,
        exit_time: i64,
        exit_price: Decimal,
        slippage_per_unit: Decimal,
        is_taker: bool,
        exit_reason: &str,
        config: &BacktestConfig,
        equity: &mut Decimal,
        excursion: &mut Option<TradeExcursion>,
    ) {
        let fee_rate = if is_taker {
            config.taker_fee_pct
        } else {
            config.maker_fee_pct
        };
        self.close_with_fee_rate(
            exit_time,
            exit_price,
            slippage_per_unit,
            fee_rate,
            exit_reason,
            equity,
            excursion,
        );
    }

    /// Closes the whole position at `exit_price` with `fee_rate` on the exit notional and
    /// records its campaign as one [`ClosedTrade`] (strategy spec §11.9). This is the only
    /// builder of closed trades, for both decision kinds.
    ///
    /// The closing fill is the campaign's last reducing fill. The entry price is the VWAP of the
    /// increasing fills, the exit price that of the reducing fills, the quantity the peak absolute
    /// quantity, and the PnL, fees, slippage and funding are the campaign sums. A VWAP over a
    /// single fill is that fill's price, so a triple-barrier entry (one increasing fill, one
    /// reducing fill) keeps the entry and exit prices, quantity and sums it always had. The open
    /// trade's excursion is consumed (or started at the entry when there is none) and finished at
    /// the post-close cash balance.
    #[allow(clippy::too_many_arguments)]
    fn close_with_fee_rate(
        &mut self,
        exit_time: i64,
        exit_price: Decimal,
        slippage_per_unit: Decimal,
        fee_rate: Decimal,
        exit_reason: &str,
        equity: &mut Decimal,
        excursion: &mut Option<TradeExcursion>,
    ) {
        let pos = match self.position.take() {
            Some(p) => p,
            None => return,
        };
        self.intent = None;
        self.exits = None;
        let mut campaign = self
            .campaign
            .take()
            .unwrap_or_else(|| Campaign::starting(exit_time));

        let pnl_gross = pos.unrealized_pnl(exit_price);
        let exit_fee = fill_fee(exit_price, pos.quantity, fee_rate);
        let funding_paid = std::mem::take(&mut self.funding_accrued);
        self.margin = Decimal::ZERO;

        // In real execution, fill_price and exit_price already embed slippage into pnl_gross.
        // We report total_slippage as an informative metric, but do not double-deduct it.
        campaign.add_reduction(
            exit_price,
            pos.quantity,
            pnl_gross,
            exit_fee,
            slippage_per_unit * pos.quantity,
        );

        // equity already had the fees of earlier fills deducted. Adjust with this fill's PnL
        // and fee.
        *equity += pnl_gross - exit_fee;

        let entry_time = campaign.entry_time;
        let mut excursion = excursion
            .take()
            .unwrap_or_else(|| TradeExcursion::start(entry_time, *equity));
        excursion.observe_equity(exit_time, *equity);
        excursion.exit_equity = *equity;

        self.ledger.close_campaign(
            campaign,
            CampaignClose {
                side: pos.side,
                exit_time,
                last_entry_price: pos.entry_price,
                last_exit_price: exit_price,
                last_quantity: pos.quantity,
                funding_paid,
                exit_reason,
                excursion,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn bar(open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> PriceEvent {
        PriceEvent {
            start_ms: 1_000,
            end_ms: 2_000,
            open,
            high,
            low,
            close,
            volume: dec!(1),
            dollar_volume: close,
            buy_dollar_volume: close / dec!(2),
            sell_dollar_volume: close / dec!(2),
        }
    }

    fn triple(stop: Decimal, take_profit: Decimal, deadline_ms: i64) -> ProtectiveExits {
        ProtectiveExits {
            stop,
            take_profit: Some(take_profit),
            step_trailing_atr: None,
            breakeven_dist: None,
            deadline_ms: Some(deadline_ms),
        }
    }

    /// [`bar`] ending at `end_ms`.
    fn bar_ending(
        end_ms: i64,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
    ) -> PriceEvent {
        PriceEvent {
            start_ms: end_ms - 1_000,
            end_ms,
            ..bar(open, high, low, close)
        }
    }

    fn stop_only(stop: Decimal) -> ProtectiveExits {
        ProtectiveExits {
            stop,
            take_profit: None,
            step_trailing_atr: None,
            breakeven_dist: None,
            deadline_ms: None,
        }
    }

    #[test]
    fn a_stop_only_exit_never_takes_profit_or_times_out() {
        let exits = stop_only(dec!(95));
        let rally = bar(dec!(100), dec!(150), dec!(99), dec!(140));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, rally, dec!(0.001)),
            None
        );
        let short_exits = stop_only(dec!(105));
        let drop = bar(dec!(100), dec!(101), dec!(50), dec!(60));
        assert_eq!(
            barrier_exit(PositionSide::Short, &short_exits, drop, dec!(0.001)),
            None
        );
    }

    #[test]
    fn a_stop_gapped_through_fills_at_the_open_and_reports_slippage_from_the_trigger() {
        let exits = stop_only(dec!(95));
        let gap = bar(dec!(90), dec!(91), dec!(89), dec!(90));
        let exit = barrier_exit(PositionSide::Long, &exits, gap, dec!(0.001));
        assert_eq!(
            exit,
            Some(BarrierExit {
                price: dec!(89.910),
                slippage_per_unit: dec!(5.090),
                reason: "StopLoss",
            })
        );
        let short_exits = stop_only(dec!(105));
        let gap_up = bar(dec!(110), dec!(111), dec!(109), dec!(110));
        let exit = barrier_exit(PositionSide::Short, &short_exits, gap_up, dec!(0.001));
        assert_eq!(
            exit,
            Some(BarrierExit {
                price: dec!(110.110),
                slippage_per_unit: dec!(5.110),
                reason: "StopLoss",
            })
        );
    }

    #[test]
    fn the_triple_barrier_checks_stop_then_take_profit_then_time() {
        let exits = triple(dec!(95), dec!(110), 3_000);
        // Both the stop and the target trade on one bar: the stop wins.
        let both = bar(dec!(100), dec!(111), dec!(94), dec!(100));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, both, dec!(0)).map(|e| e.reason),
            Some("StopLoss")
        );
        let target = bar_ending(3_000, dec!(100), dec!(111), dec!(99), dec!(105));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, target, dec!(0.001)),
            Some(BarrierExit {
                price: dec!(109.890),
                slippage_per_unit: dec!(0.110),
                reason: "TakeProfit",
            })
        );
        let quiet = bar_ending(2_999, dec!(100), dec!(101), dec!(99), dec!(100));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, quiet, dec!(0.001)),
            None
        );
        let due = bar_ending(3_000, dec!(100), dec!(101), dec!(99), dec!(100));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, due, dec!(0.001)),
            Some(BarrierExit {
                price: dec!(99.900),
                slippage_per_unit: dec!(0.100),
                reason: "TimeBarrier",
            })
        );
    }

    #[test]
    fn an_entry_fill_and_close_charge_fees_once_and_record_one_trade() {
        let config = BacktestConfig {
            slippage_pct: dec!(0),
            taker_fee_pct: dec!(0.001),
            ..BacktestConfig::default()
        };
        let mut equity = dec!(10_000);
        let mut excursion = None;
        let mut core = ExecutionCore::new();
        let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(95), dec!(110), 5)
            .expect("valid intent");
        core.submit(PendingEntry::new(intent, dec!(2)));
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        )
        .unwrap();
        assert_eq!(equity, dec!(9_999.8));
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(95)));

        core.evaluate_exits(
            bar(dec!(100), dec!(111), dec!(100), dec!(110)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert!(core.position().is_none());
        assert!(core.exits().is_none());
        let trade = &core.ledger().trades()[0];
        assert_eq!(trade.exit_reason, "TakeProfit");
        assert_eq!(trade.pnl_gross, dec!(20));
        assert_eq!(trade.fees_paid, dec!(0.42));
        assert_eq!(trade.pnl_net, dec!(19.58));
        assert_eq!(equity, dec!(10_019.58));
    }

    #[test]
    fn position_moves_to_breakeven_on_favorable_excursion() {
        let config = BacktestConfig {
            slippage_pct: dec!(0),
            taker_fee_pct: dec!(0),
            ..BacktestConfig::default()
        };
        let mut equity = dec!(10_000);
        let mut excursion = None;
        let mut core = ExecutionCore::new();
        let intent = OrderIntent::with_risk_and_exits(
            1,
            Side::Buy,
            dec!(100),
            dec!(95),
            dec!(115),
            60_000,
            Some(dec!(0.01)),
            None,
            Some(dec!(3.75)), // 0.75 ATR (ATR = 5)
        )
        .expect("valid intent");
        core.submit(PendingEntry::new(intent, dec!(1)));
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        )
        .unwrap();
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(95)));

        // Bar 1: Favorable excursion reaches 106 (+1.2 ATR >= 0.75 ATR)
        core.evaluate_exits(
            bar(dec!(100), dec!(106), dec!(99), dec!(105)),
            &config,
            &mut equity,
            &mut excursion,
        );
        // Position still open, stop raised to breakeven (100)
        assert!(core.position().is_some());
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(100)));
    }

    #[test]
    fn breakeven_prevents_loss_on_subsequent_reversal() {
        let config = BacktestConfig {
            slippage_pct: dec!(0),
            taker_fee_pct: dec!(0),
            ..BacktestConfig::default()
        };
        let mut equity = dec!(10_000);
        let mut excursion = None;
        let mut core = ExecutionCore::new();
        let intent = OrderIntent::with_risk_and_exits(
            1,
            Side::Buy,
            dec!(100),
            dec!(95),
            dec!(115),
            60_000,
            Some(dec!(0.01)),
            None,
            Some(dec!(3.75)), // 0.75 ATR (ATR = 5)
        )
        .expect("valid intent");
        core.submit(PendingEntry::new(intent, dec!(1)));
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        )
        .unwrap();

        // Bar 1: High reaches 105.5 (+1.1 ATR >= 0.75 ATR) -> stop ratchets to breakeven (100)
        core.evaluate_exits(
            bar(dec!(100), dec!(105.5), dec!(99), dec!(104)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(100)));

        // Bar 2: Sharp reversal drops below breakeven (low 98)
        core.evaluate_exits(
            bar(dec!(104), dec!(104), dec!(98), dec!(98.5)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert!(core.position().is_none());
        let trade = &core.ledger().trades()[0];
        assert_eq!(trade.exit_reason, "StopLoss");
        assert_eq!(trade.exit_price, dec!(100));
        assert_eq!(trade.pnl_gross, dec!(0)); // Zero loss thanks to breakeven!
    }

    #[test]
    fn position_trails_to_lock_in_profits() {
        let config = BacktestConfig {
            slippage_pct: dec!(0),
            taker_fee_pct: dec!(0),
            ..BacktestConfig::default()
        };
        let mut equity = dec!(10_000);
        let mut excursion = None;
        let mut core = ExecutionCore::new();
        let intent = OrderIntent::with_risk_and_exits(
            1,
            Side::Buy,
            dec!(100),
            dec!(95),
            dec!(120),
            60_000,
            Some(dec!(0.01)),
            Some(dec!(2.5)),  // 0.50 ATR step (ATR = 5)
            Some(dec!(3.75)), // 0.75 ATR breakeven trigger
        )
        .expect("valid intent");
        core.submit(PendingEntry::new(intent, dec!(1)));
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        )
        .unwrap();

        // Bar 1: High reaches 108 (+1.6 ATR >= 1.5 ATR, k=3) -> Stop ratchets to entry + 1.0 ATR = 105.0
        core.evaluate_exits(
            bar(dec!(100), dec!(108), dec!(99), dec!(107)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(105.0)));

        // Bar 2: High reaches 111 (+2.2 ATR >= 2.0 ATR, k=4) -> Stop ratchets to entry + 1.5 ATR = 107.5
        core.evaluate_exits(
            bar(dec!(107), dec!(111), dec!(106), dec!(110)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(107.5)));

        // Bar 3: Reversal drops to 104, triggering the trailed stop at 107.5
        core.evaluate_exits(
            bar(dec!(110), dec!(110), dec!(104), dec!(104.5)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert!(core.position().is_none());
        let trade = &core.ledger().trades()[0];
        assert_eq!(trade.exit_reason, "StopLoss");
        assert_eq!(trade.exit_price, dec!(107.5));
        assert_eq!(trade.pnl_gross, dec!(7.5)); // Locked in profit!
    }

    #[test]
    fn limit_entry_fills_as_maker_with_zero_slippage() {
        let config = BacktestConfig {
            maker_fee_pct: dec!(0.0002),
            taker_fee_pct: dec!(0.0005),
            slippage_pct: dec!(0.001),
            ..BacktestConfig::default()
        };
        let mut equity = dec!(10_000);
        let mut core = ExecutionCore::new();
        let intent = OrderIntent::limit(
            1,
            Side::Buy,
            dec!(100),
            dec!(90),
            dec!(110),
            5,
            Some(2),
            true,
        )
        .expect("valid limit intent");

        core.submit(PendingEntry::new(intent, dec!(2)));
        assert!(core.pending().is_some());

        // Bar touches limit price (low 99 <= 100)
        let bar_touched = bar(dec!(102), dec!(105), dec!(99), dec!(101));
        let outcome = core.step_pending_entry(bar_touched, &config, &mut equity);

        assert_eq!(outcome, PendingEntryOutcome::FilledLimit);
        assert!(core.pending().is_none());
        assert!(core.position().is_some());

        let pos = core.position().unwrap();
        assert_eq!(pos.entry_price, dec!(100));
        assert_eq!(pos.quantity, dec!(2));

        // Maker fee = 200 notional * 0.0002 = 0.04 (vs 0.10 taker)
        assert_eq!(equity, dec!(9_999.96));
        assert_eq!(core.campaign.unwrap().slippage, dec!(0));
    }

    #[test]
    fn limit_entry_rests_and_expires_after_tif_timeout() {
        let config = BacktestConfig::default();
        let mut equity = dec!(10_000);
        let mut core = ExecutionCore::new();
        let intent = OrderIntent::limit(
            1,
            Side::Buy,
            dec!(100),
            dec!(90),
            dec!(110),
            5,
            Some(2),
            true,
        )
        .expect("valid limit intent");

        core.submit(PendingEntry::new(intent, dec!(1)));

        // Bar 1: Price stays above limit (low 102 > 100)
        let bar_1 = bar(dec!(105), dec!(106), dec!(102), dec!(103));
        let outcome_1 = core.step_pending_entry(bar_1, &config, &mut equity);
        assert_eq!(
            outcome_1,
            PendingEntryOutcome::RestingLimit { bars_pending: 1 }
        );
        assert!(core.position().is_none());
        assert!(core.pending().is_some());

        // Bar 2: Price still stays above limit (low 101 > 100) -> reaches TIF timeout of 2
        let bar_2 = bar(dec!(103), dec!(104), dec!(101), dec!(102));
        let outcome_2 = core.step_pending_entry(bar_2, &config, &mut equity);
        assert_eq!(outcome_2, PendingEntryOutcome::ExpiredLimit);
        assert!(core.position().is_none());
        assert!(core.pending().is_none()); // Slot freed!
    }

    #[test]
    fn spot_mode_rejects_sell_entry() {
        let config = BacktestConfig::spot();
        let mut equity = dec!(10_000);
        let mut core = ExecutionCore::new();
        let intent = OrderIntent::new(1, Side::Sell, dec!(100), dec!(110), dec!(90), 5).unwrap();

        core.submit(PendingEntry::new(intent, dec!(1)));
        assert!(core.pending().is_some());

        let bar_1 = bar(dec!(100), dec!(105), dec!(95), dec!(100));
        let pending = core.take_pending().unwrap();
        let refusal = core
            .fill_entry(pending, bar_1, &config, &mut equity)
            .unwrap_err();

        // Core must remain completely flat because short entries are forbidden in Spot
        assert_eq!(refusal, domain::EntryRefusal::SpotShortingDisallowed);
        assert!(core.position().is_none());
        assert_eq!(equity, dec!(10_000));
    }

    #[test]
    fn spot_mode_ignores_funding_settlements() {
        let config = BacktestConfig::spot();
        let mut equity = dec!(10_000);
        let mut core = ExecutionCore::new();
        let pos = Position::new(PositionSide::Long, dec!(2), dec!(100)).unwrap();
        core.set_position_for_test(pos);

        // Cross 8-hour funding boundary (e.g. from 0 to 8*3600*1000 + 1000)
        let eight_hours = 8 * 3600 * 1000;
        let mut test_bar = bar(dec!(100), dec!(105), dec!(95), dec!(100));
        test_bar.start_ms = eight_hours;
        test_bar.end_ms = eight_hours + 1000;

        core.settle_funding(0, test_bar, &config, &mut equity);

        assert_eq!(equity, dec!(10_000));
        assert_eq!(core.funding_accrued, Decimal::ZERO);
    }

    #[test]
    fn spot_mode_disables_margin_liquidation() {
        let config = BacktestConfig::spot();
        let mut equity = dec!(10_000);
        let mut core = ExecutionCore::new();
        let pos = Position::new(PositionSide::Long, dec!(1), dec!(100)).unwrap();
        core.set_position_for_test(pos);

        // Liquidation price must be None for Spot
        assert_eq!(core.liquidation_price(&config), None);

        // Even on a massive price collapse, liquidation does not trigger
        let crash_bar = bar(dec!(10), dec!(10), dec!(1), dec!(5));
        let mut excursion = None;
        assert!(!core.liquidate_if_due(&crash_bar, &config, &mut equity, &mut excursion));
    }

    #[test]
    fn spot_dual_balances_track_free_quote_and_base() {
        let mut core = ExecutionCore::new();
        let equity = dec!(10_000);

        // Flat: all equity is free quote, 0 base
        assert_eq!(core.spot_balances(equity), (dec!(10_000), dec!(0)));

        // Open position: 2 units at price 100 = 200 notional
        let pos = Position::new(PositionSide::Long, dec!(2), dec!(100)).unwrap();
        core.set_position_for_test(pos);

        assert_eq!(core.spot_balances(equity), (dec!(9_800), dec!(2)));
    }
}
