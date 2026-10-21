//! Position accounting shared by every runner that simulates fills.
//!
//! [`ExecutionCore`] was extracted from [`crate::BacktestEngine`] (odd swarm-consensus T5.1,
//! strategy spec `docs/strategy/03-swarm-consensus.md` §3.5). It owns the open position, the
//! pending market order, the [`ProtectiveExits`], isolated margin and liquidation, funding, the
//! fill price, slippage and fee math, and the [`TradeLedger`] that turns a closed position into a
//! [`ClosedTrade`]. The engine keeps its loop (rollovers, breakers, kill switch, mark to market,
//! excursion tracking) and owns the cash balance, which it lends to the core on every call.
//!
//! Every `Decimal` operation keeps the shape and order it had in the engine, so results are
//! byte-identical to the pre-extraction engine (proved by `tests/golden_execution.rs`).

use domain::futures;
#[cfg(test)]
use domain::DollarBar;
use domain::{
    OrderIntent, Position, PositionSide, ProtectiveExits, RiskError, Side, TargetPosition,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::config::BacktestConfig;
use crate::metrics::{
    ClosedTrade, TradeExcursion, REARM_IGNORED_REJECTION, STOP_BEYOND_LIQUIDATION_CLIP,
    UNSUPPORTED_DECISION_REJECTION,
};
use crate::price_event::PriceEvent;

/// A submitted entry: the strategy intent and the quantity sized when it was signalled.
#[derive(Debug, Clone)]
pub struct PendingEntry {
    /// The strategy's triple-barrier intent.
    pub intent: OrderIntent,
    /// Quantity sized at the signal price.
    pub quantity: Decimal,
}

/// A submitted target: the strategy's target, the signed quantity sized when it was decided, and
/// the decision price it was sized at.
///
/// The order that moves the position (`quantity - current`) is only computed when it fills, so
/// fills that happened in between are taken into account (strategy spec §11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingTarget {
    /// The strategy's target position.
    pub target: TargetPosition,
    /// Target quantity sized at the decision price: positive long, negative short, 0 flat.
    pub quantity: Decimal,
    /// The decision price (the deciding event's close) the target was sized at. An increasing
    /// fill reports the adverse gap from it as slippage, as an entry does from its signal price.
    pub reference_price: Decimal,
}

/// Exit reason of a target campaign that ended because the target reached 0.
pub const TARGET_EXIT_REASON: &str = "TargetExit";

/// Exit reason of a target campaign that ended because the target flipped to the other side.
pub const TARGET_FLIP_REASON: &str = "TargetFlip";

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

/// The state an increasing target fill would leave, before it is committed.
#[derive(Debug, Clone)]
struct IncreasePlan {
    quantity: Decimal,
    position: Position,
    margin: Decimal,
    exits: ProtectiveExits,
    fee: Decimal,
}

/// Why an increasing target fill cannot be placed as planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanFailure {
    /// No supported leverage margins the new aggregate notional.
    Unmarginable,
    /// The stop price would not be a positive price.
    InvalidStop,
    /// The liquidation price would lie between the entry and the stop.
    LiquidationBeforeStop,
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

/// Fill price of a market order on `side` against `reference`, and the slippage per unit.
///
/// A buy pays `reference + reference * pct`; a sell receives `reference - reference * pct`.
pub fn market_fill_price(
    side: Side,
    reference: Decimal,
    slippage_pct: Decimal,
) -> (Decimal, Decimal) {
    let slip = reference * slippage_pct;
    let fill_price = match side {
        Side::Buy => reference + slip,
        Side::Sell => reference - slip,
    };
    (fill_price, slip)
}

/// The order side that closes a position on `side`.
fn closing_side(side: PositionSide) -> Side {
    match side {
        PositionSide::Long => Side::Sell,
        PositionSide::Short => Side::Buy,
    }
}

/// The order side that opened a position on `side`.
fn opening_side(side: PositionSide) -> Side {
    match side {
        PositionSide::Long => Side::Buy,
        PositionSide::Short => Side::Sell,
    }
}

/// Fee of a fill of `quantity` at `price` charged at `rate`: `(price · quantity) · rate`.
fn fill_fee(price: Decimal, quantity: Decimal, rate: Decimal) -> Decimal {
    price * quantity * rate
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

/// Slippage per unit an opening market order on `side` reports: the model slippage `slip` on
/// the `open` plus the adverse part of the gap from the `reference` price the order was sized
/// at. A gap in favour does not cancel the slippage.
fn opening_slippage_per_unit(
    side: Side,
    slip: Decimal,
    open: Decimal,
    reference: Decimal,
) -> Decimal {
    slip + match side {
        Side::Buy => (open - reference).max(Decimal::ZERO),
        Side::Sell => (reference - open).max(Decimal::ZERO),
    }
}

/// Evaluates the protective exits of a position on `side` against one price event (a
/// [`domain::DollarBar`] converts into one with the same OHLC).
///
/// Barrier exits are market orders: they fill at the trigger or, when the event gapped through
/// it, at the open, and pay slippage against the position. The stop is checked first, then
/// the take profit (when there is one), then the time limit (when there is one) at the close.
/// The reported stop slippage is measured against the trigger so it includes the gap.
pub fn barrier_exit(
    side: PositionSide,
    exits: &ProtectiveExits,
    event: impl Into<PriceEvent>,
    bars_held: usize,
    slippage_pct: Decimal,
) -> Option<BarrierExit> {
    let bar = event.into();
    let close_side = closing_side(side);
    let time_due = exits.max_bars.is_some_and(|max_bars| bars_held >= max_bars);
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
    bars_held: usize,
    /// Isolated margin posted for the open position.
    margin: Decimal,
    /// Funding paid by the open position so far.
    funding_accrued: Decimal,
    /// Target waiting for its due time to fill.
    pending_target: Option<PendingTarget>,
    /// Earliest event start, in Unix milliseconds, at which `pending_target` may fill.
    pending_target_due_ms: i64,
    /// Sums of the open campaign; `Some` exactly while a position opened by a fill is open.
    campaign: Option<Campaign>,
    /// Side of the last target campaign closed by a forced exit, until re-armed (§11.7).
    rearm_blocked: Option<PositionSide>,
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

    /// The target waiting to fill, if any.
    pub fn pending_target(&self) -> Option<&PendingTarget> {
        self.pending_target.as_ref()
    }

    /// Queues a target, replacing any pending target; it may fill on the first event starting
    /// at or after `due_ms`.
    pub fn submit_target_due(&mut self, pending: PendingTarget, due_ms: i64) {
        self.pending_target = Some(pending);
        self.pending_target_due_ms = due_ms;
    }

    /// Removes and returns the pending target when it is due on an event starting at
    /// `event_start_ms`; otherwise leaves it pending.
    pub fn take_due_target(&mut self, event_start_ms: i64) -> Option<PendingTarget> {
        if self.pending_target_due_ms <= event_start_ms {
            self.pending_target.take()
        } else {
            None
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

    /// Whether the open position is a target campaign (as opposed to a triple-barrier entry).
    pub fn has_target_position(&self) -> bool {
        self.campaign.is_some() && self.intent.is_none()
    }

    /// Side on which targets are ignored after a forced exit, until a flat or opposite target
    /// re-arms it (strategy spec §11.7).
    pub fn rearm_blocked_side(&self) -> Option<PositionSide> {
        self.rearm_blocked
    }

    /// Re-arms targets after a forced exit.
    pub fn clear_rearm(&mut self) {
        self.rearm_blocked = None;
    }

    /// Isolated margin posted for the open position (0 when flat).
    pub fn margin(&self) -> Decimal {
        self.margin
    }

    /// Bars the open position has been held.
    pub fn bars_held(&self) -> usize {
        self.bars_held
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
    #[cfg(test)]
    pub(crate) fn set_position_for_test(&mut self, position: Position) {
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
    /// The exchange runs the position at the lowest leverage the balance can margin; an order
    /// it cannot margin at any supported leverage is dropped.
    pub fn fill_entry(
        &mut self,
        pending: PendingEntry,
        event: impl Into<PriceEvent>,
        config: &BacktestConfig,
        equity: &mut Decimal,
    ) {
        let bar = event.into();
        let PendingEntry { intent, quantity } = pending;
        let (fill_price, slip) = market_fill_price(intent.side, bar.open, config.slippage_pct);

        // Entry Fee (Taker)
        let entry_fee = fill_fee(fill_price, quantity, config.taker_fee_pct);

        let notional = fill_price * quantity;
        let Some(leverage) = margin_leverage(notional, entry_fee, *equity, config) else {
            return;
        };
        // Slippage is already embedded in fill_price; do not deduct from cash equity here
        *equity -= entry_fee;
        // Reported slippage is what the order paid: the slippage on the open plus the adverse
        // part of the gap from the signal price.
        let adverse_per_unit = opening_slippage_per_unit(intent.side, slip, bar.open, intent.price);

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
            self.bars_held = 0;
            self.margin = notional / Decimal::from(leverage);
            self.funding_accrued = Decimal::ZERO;
        }
    }

    /// Fills a due target at the event's open (strategy spec §11.4–11.7) and returns the
    /// counters to add to `rejected_orders` (rejections and adjustment counters).
    ///
    /// The order is `delta = target - current`, a taker market order at the open plus adverse
    /// slippage. A target of 0 closes the whole position whatever its size ([`TARGET_EXIT_REASON`]).
    /// A flip is two fills on this event: the whole position is closed ([`TARGET_FLIP_REASON`]),
    /// then the new side opens as a new campaign; when that opening leg is not placed (any of the
    /// rejections below), the position ended flat and the closed campaign is relabelled
    /// [`TARGET_EXIT_REASON`]. Any other delta below the minimum lot or
    /// notional is skipped and counted under the [`RiskError`] kind, and one above
    /// `max_order_notional` is rejected as `OrderSizeExceeded`; closes of the whole position are
    /// exempt, as barrier exits are. An increase the balance cannot margin is dropped, as an
    /// unmarginable entry is. An increase whose liquidation price would lie between the entry and
    /// the stop is reduced, in whole lot steps, to the largest quantity whose stop trades first,
    /// and [`STOP_BEYOND_LIQUIDATION_CLIP`] is counted. After every fill that leaves a position
    /// open, margin is recomputed on the aggregate notional and the one stop is placed
    /// `stop_distance` from that fill price. A target on the side blocked by the re-arm rule is
    /// dropped and counted as [`REARM_IGNORED_REJECTION`]. An increasing fill reports slippage
    /// as an entry does (model slippage plus the adverse gap from
    /// [`PendingTarget::reference_price`]); a reducing fill reports the model slippage only.
    pub fn fill_target(
        &mut self,
        pending: PendingTarget,
        event: impl Into<PriceEvent>,
        config: &BacktestConfig,
        equity: &mut Decimal,
        excursion: &mut Option<TradeExcursion>,
    ) -> Vec<&'static str> {
        let bar = event.into();
        let mut counters = Vec::new();
        if self.intent.is_some() {
            // A triple-barrier entry is open: targets never manage it.
            counters.push(UNSUPPORTED_DECISION_REJECTION);
            return counters;
        }
        let target = pending.quantity;
        if let (Some(blocked), Some(side)) = (self.rearm_blocked, signed_side(target)) {
            if blocked == side {
                counters.push(REARM_IGNORED_REJECTION);
                return counters;
            }
        }
        let current = self.signed_quantity();
        if target == current {
            return counters;
        }
        if target.is_zero() {
            self.close_target_at_open(&bar, TARGET_EXIT_REASON, config, equity, excursion);
            return counters;
        }
        let flipped = !current.is_zero() && current.is_sign_negative() != target.is_sign_negative();
        if flipped {
            self.close_target_at_open(&bar, TARGET_FLIP_REASON, config, equity, excursion);
        }
        let filled = self.fill_target_delta(&pending, &bar, config, equity, &mut counters);
        if flipped && !filled {
            // The opening leg was not placed, so the flip ended flat: the campaign it closed
            // ended because the position returned to 0.
            if let Some(trade) = self.ledger.last_mut() {
                trade.exit_reason = TARGET_EXIT_REASON.to_string();
            }
        }
        counters
    }

    /// Places the order `pending.quantity - current` at the event's open, after any flip close,
    /// and returns whether it filled. Rejections and adjustments are pushed onto `counters`.
    fn fill_target_delta(
        &mut self,
        pending: &PendingTarget,
        bar: &PriceEvent,
        config: &BacktestConfig,
        equity: &mut Decimal,
        counters: &mut Vec<&'static str>,
    ) -> bool {
        let target = pending.quantity;
        let current = self.signed_quantity();
        let delta = target - current;
        let side = if delta.is_sign_positive() {
            Side::Buy
        } else {
            Side::Sell
        };
        let quantity = delta.abs();
        if let Some(kind) = order_limit_rejection(quantity, bar.open, config) {
            counters.push(kind);
            return false;
        }
        let (fill_price, slip) = market_fill_price(side, bar.open, config.slippage_pct);
        let target_pos = &pending.target;
        // After a flip `current` is 0, so it is flat or on the target's side here.
        if target.abs() > current.abs() {
            let plan =
                match self.plan_increase(side, quantity, fill_price, target_pos, config, *equity) {
                    Ok(plan) => plan,
                    Err(PlanFailure::Unmarginable) => return false,
                    Err(PlanFailure::InvalidStop) => {
                        counters.push(invalid_stop_kind());
                        return false;
                    }
                    Err(PlanFailure::LiquidationBeforeStop) => {
                        counters.push(STOP_BEYOND_LIQUIDATION_CLIP);
                        let Some(plan) = self.largest_safe_increase(
                            side, quantity, fill_price, target_pos, config, *equity,
                        ) else {
                            return false;
                        };
                        if let Some(kind) = order_limit_rejection(plan.quantity, bar.open, config) {
                            counters.push(kind);
                            return false;
                        }
                        plan
                    }
                };
            *equity -= plan.fee;
            if self.position.is_none() {
                self.bars_held = 0;
                self.funding_accrued = Decimal::ZERO;
            }
            // An increase reports slippage as an entry does: the model slippage plus the adverse
            // gap from the decision price.
            let slippage_per_unit =
                opening_slippage_per_unit(side, slip, bar.open, pending.reference_price);
            self.campaign
                .get_or_insert_with(|| Campaign::starting(bar.start_ms))
                .add_increase(
                    fill_price,
                    plan.quantity,
                    plan.fee,
                    slippage_per_unit * plan.quantity,
                    plan.position.quantity,
                );
            self.position = Some(plan.position);
            self.margin = plan.margin;
            self.exits = Some(plan.exits);
        } else {
            let Some(mut position) = self.position.clone() else {
                return false;
            };
            let Ok(exits) = ProtectiveExits::for_target(
                opening_side(position.side),
                fill_price,
                target_pos.stop_distance(),
                target_pos.take_profit_distance(),
            ) else {
                counters.push(invalid_stop_kind());
                return false;
            };
            let realized_before = position.realized_pnl;
            if position.apply_fill(side, quantity, fill_price).is_err() {
                return false;
            }
            let pnl = position.realized_pnl - realized_before;
            let fee = fill_fee(fill_price, quantity, config.taker_fee_pct);
            let notional = position.entry_price * position.quantity;
            // A reduction never needs more margin; at worst it keeps the highest leverage.
            let leverage =
                margin_leverage(notional, fee, *equity, config).unwrap_or(max_leverage(config));
            *equity += pnl - fee;
            // A reduction is a market exit: it reports the model slippage only.
            if let Some(campaign) = self.campaign.as_mut() {
                campaign.add_reduction(fill_price, quantity, pnl, fee, slip * quantity);
            }
            self.position = Some(position);
            self.margin = notional / Decimal::from(leverage.max(1));
            self.exits = Some(exits);
        }
        true
    }

    /// The position, margin, stop and fee an increase of `quantity` on `side` at `fill_price`
    /// would leave, with the engine's margin rule on the new aggregate notional.
    fn plan_increase(
        &self,
        side: Side,
        quantity: Decimal,
        fill_price: Decimal,
        target: &TargetPosition,
        config: &BacktestConfig,
        equity: Decimal,
    ) -> Result<IncreasePlan, PlanFailure> {
        let position = match &self.position {
            Some(position) => {
                let mut position = position.clone();
                position
                    .apply_fill(side, quantity, fill_price)
                    .map_err(|_| PlanFailure::Unmarginable)?;
                position
            }
            None => Position::new(position_side(side), quantity, fill_price)
                .map_err(|_| PlanFailure::Unmarginable)?,
        };
        let exits = ProtectiveExits::for_target(
            side,
            fill_price,
            target.stop_distance(),
            target.take_profit_distance(),
        )
        .map_err(|_| PlanFailure::InvalidStop)?;
        let fee = fill_fee(fill_price, quantity, config.taker_fee_pct);
        let notional = position.entry_price * position.quantity;
        let leverage =
            margin_leverage(notional, fee, equity, config).ok_or(PlanFailure::Unmarginable)?;
        let margin = notional / Decimal::from(leverage);
        let liquidation = liquidation_of(&position, margin, config);
        let stop_first = match position.side {
            PositionSide::Long => exits.stop > liquidation,
            PositionSide::Short => exits.stop < liquidation,
        };
        if !stop_first {
            return Err(PlanFailure::LiquidationBeforeStop);
        }
        Ok(IncreasePlan {
            quantity,
            position,
            margin,
            exits,
            fee,
        })
    }

    /// The largest increase below `quantity`, in whole lot steps, whose stop trades before its
    /// liquidation price; `None` when not even one step does.
    ///
    /// A larger order needs a higher leverage, which moves the liquidation price closer, so
    /// the condition is monotone in the quantity and a bisection over lot steps finds it.
    fn largest_safe_increase(
        &self,
        side: Side,
        quantity: Decimal,
        fill_price: Decimal,
        target: &TargetPosition,
        config: &BacktestConfig,
        equity: Decimal,
    ) -> Option<IncreasePlan> {
        let step = config.quantity_step_size;
        if step <= Decimal::ZERO {
            return None;
        }
        // Invariant: `low` steps are safe (0 places nothing), `high` steps are not.
        let mut low: u64 = 0;
        let mut high: u64 = (quantity / step).floor().to_u64()?;
        let mut best = None;
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            match self.plan_increase(
                side,
                Decimal::from(middle) * step,
                fill_price,
                target,
                config,
                equity,
            ) {
                Ok(plan) => {
                    low = middle;
                    best = Some(plan);
                }
                Err(_) => high = middle,
            }
        }
        best
    }

    /// Closes the whole target position with a taker market order at the event's open.
    fn close_target_at_open(
        &mut self,
        bar: &PriceEvent,
        reason: &str,
        config: &BacktestConfig,
        equity: &mut Decimal,
        excursion: &mut Option<TradeExcursion>,
    ) {
        let Some(side) = self.position.as_ref().map(|position| position.side) else {
            return;
        };
        let (price, slip) = market_fill_price(closing_side(side), bar.open, config.slippage_pct);
        self.close_with_fee_rate(
            bar.start_ms,
            price,
            slip,
            config.taker_fee_pct,
            reason,
            equity,
            excursion,
        );
    }

    /// Counts one more event held and closes the position when it is liquidated or a protective
    /// exit triggers on this event, at the event's end time. Liquidation is evaluated first.
    pub fn evaluate_exits(
        &mut self,
        event: impl Into<PriceEvent>,
        config: &BacktestConfig,
        equity: &mut Decimal,
        excursion: &mut Option<TradeExcursion>,
    ) {
        let bar = event.into();
        self.bars_held += 1;
        if self.liquidate_if_due(&bar, config, equity, excursion) {
            return;
        }
        let (Some(mut exits), Some(position)) = (self.exits, &self.position) else {
            return;
        };
        if let Some(exit) = barrier_exit(
            position.side,
            &exits,
            bar,
            self.bars_held,
            config.slippage_pct,
        ) {
            self.execute_close(
                bar.end_ms,
                exit.price,
                exit.slippage_per_unit,
                true,
                exit.reason,
                config,
                equity,
                excursion,
            );
            return;
        }

        // Trailing / Breakeven stop ratchet for subsequent events (AFML Ch. 10):
        if let Some(atr) = exits.step_trailing_atr {
            if atr > Decimal::ZERO {
                match position.side {
                    PositionSide::Long => {
                        let fav_excursion = bar.high - position.entry_price;
                        let new_stop = if fav_excursion >= dec!(2.0) * atr {
                            position.entry_price + atr
                        } else if fav_excursion >= dec!(1.5) * atr {
                            position.entry_price + dec!(0.5) * atr
                        } else if fav_excursion >= dec!(1.0) * atr {
                            position.entry_price
                        } else {
                            exits.stop
                        };
                        exits.stop = exits.stop.max(new_stop);
                    }
                    PositionSide::Short => {
                        let fav_excursion = position.entry_price - bar.low;
                        let new_stop = if fav_excursion >= dec!(2.0) * atr {
                            position.entry_price - atr
                        } else if fav_excursion >= dec!(1.5) * atr {
                            position.entry_price - dec!(0.5) * atr
                        } else if fav_excursion >= dec!(1.0) * atr {
                            position.entry_price
                        } else {
                            exits.stop
                        };
                        exits.stop = exits.stop.min(new_stop);
                    }
                }
                self.exits = Some(exits);
            }
        }
    }

    /// Liquidation price of the open position, if one is open.
    pub fn liquidation_price(&self, config: &BacktestConfig) -> Option<Decimal> {
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
    /// the post-close cash balance. A forced exit of a target campaign blocks new targets on its
    /// side until re-armed (§11.7).
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
        let is_target = self.intent.take().is_none();
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

        if is_target
            && matches!(
                exit_reason,
                "StopLoss" | "TakeProfit" | "TimeBarrier" | "Liquidation" | "MonthlyCircuitBreaker"
            )
        {
            self.rearm_blocked = Some(pos.side);
        }

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

/// Volume-weighted average price of fills summing to `notional` over `quantity`.
fn vwap(notional: Decimal, quantity: Decimal) -> Decimal {
    if quantity > Decimal::ZERO {
        notional / quantity
    } else {
        Decimal::ZERO
    }
}

/// The side of a signed quantity; `None` for 0.
fn signed_side(quantity: Decimal) -> Option<PositionSide> {
    if quantity > Decimal::ZERO {
        Some(PositionSide::Long)
    } else if quantity < Decimal::ZERO {
        Some(PositionSide::Short)
    } else {
        None
    }
}

/// The position side an opening order on `side` creates.
fn position_side(side: Side) -> PositionSide {
    match side {
        Side::Buy => PositionSide::Long,
        Side::Sell => PositionSide::Short,
    }
}

/// The order-level limit a target delta order of `quantity` at `price` breaks, as the
/// [`RiskError::kind`] counted for it: the minimum lot, the minimum notional, then the maximum
/// order notional (the same limits and names an entry is rejected with).
fn order_limit_rejection(
    quantity: Decimal,
    price: Decimal,
    config: &BacktestConfig,
) -> Option<&'static str> {
    let (max_order_notional, _) = config.effective_notional_caps();
    order_limit_kind(
        quantity,
        price,
        config.min_order_quantity,
        config.min_order_notional,
        max_order_notional,
    )
}

/// The order-level limit an order of `quantity` at `price` breaks, as the [`RiskError::kind`]
/// counted for it: the minimum lot, the minimum notional, then the maximum order notional.
/// Shared by the bar engine and the trading session, so a target delta is judged the same way
/// on both paths.
pub(crate) fn order_limit_kind(
    quantity: Decimal,
    price: Decimal,
    min_order_quantity: Decimal,
    min_order_notional: Decimal,
    max_order_notional: Decimal,
) -> Option<&'static str> {
    let notional = quantity * price;
    let error = if quantity < min_order_quantity {
        RiskError::OrderBelowMinLotSize {
            quantity: quantity.to_string(),
            min_quantity: min_order_quantity.to_string(),
        }
    } else if notional < min_order_notional {
        RiskError::OrderBelowMinNotional {
            notional: notional.to_string(),
            min_notional: min_order_notional.to_string(),
        }
    } else if notional > max_order_notional {
        RiskError::OrderSizeExceeded {
            order_notional: notional.to_string(),
            limit: max_order_notional.to_string(),
        }
    } else {
        return None;
    };
    Some(error.kind())
}

/// The kind counted for a target whose stop would not be a positive price.
fn invalid_stop_kind() -> &'static str {
    RiskError::InvalidParameters(String::new()).kind()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn bar(open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> DollarBar {
        DollarBar {
            start_time: 1_000,
            end_time: 2_000,
            open,
            high,
            low,
            close,
            volume: dec!(1),
            dollar_volume: close,
            trade_count: 2,
            buy_volume: dec!(0.5),
            sell_volume: dec!(0.5),
            buy_dollar_volume: close / dec!(2),
            sell_dollar_volume: close / dec!(2),
        }
    }

    fn triple(stop: Decimal, take_profit: Decimal, max_bars: usize) -> ProtectiveExits {
        ProtectiveExits {
            stop,
            take_profit: Some(take_profit),
            max_bars: Some(max_bars),
            step_trailing_atr: None,
        }
    }

    fn stop_only(stop: Decimal) -> ProtectiveExits {
        ProtectiveExits {
            stop,
            take_profit: None,
            max_bars: None,
            step_trailing_atr: None,
        }
    }

    #[test]
    fn market_fill_price_adds_slippage_on_buys_and_subtracts_on_sells() {
        assert_eq!(
            market_fill_price(Side::Buy, dec!(100), dec!(0.001)),
            (dec!(100.1), dec!(0.1))
        );
        assert_eq!(
            market_fill_price(Side::Sell, dec!(100), dec!(0.001)),
            (dec!(99.9), dec!(0.1))
        );
    }

    #[test]
    fn a_stop_only_exit_never_takes_profit_or_times_out() {
        let exits = stop_only(dec!(95));
        let rally = bar(dec!(100), dec!(150), dec!(99), dec!(140));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, &rally, usize::MAX, dec!(0.001)),
            None
        );
        let short_exits = stop_only(dec!(105));
        let drop = bar(dec!(100), dec!(101), dec!(50), dec!(60));
        assert_eq!(
            barrier_exit(
                PositionSide::Short,
                &short_exits,
                &drop,
                usize::MAX,
                dec!(0.001)
            ),
            None
        );
    }

    #[test]
    fn a_stop_gapped_through_fills_at_the_open_and_reports_slippage_from_the_trigger() {
        let exits = stop_only(dec!(95));
        let gap = bar(dec!(90), dec!(91), dec!(89), dec!(90));
        let exit = barrier_exit(PositionSide::Long, &exits, &gap, 1, dec!(0.001));
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
        let exit = barrier_exit(PositionSide::Short, &short_exits, &gap_up, 1, dec!(0.001));
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
        let exits = triple(dec!(95), dec!(110), 3);
        // Both the stop and the target trade on one bar: the stop wins.
        let both = bar(dec!(100), dec!(111), dec!(94), dec!(100));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, &both, 1, dec!(0)).map(|e| e.reason),
            Some("StopLoss")
        );
        let target = bar(dec!(100), dec!(111), dec!(99), dec!(105));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, &target, 3, dec!(0.001)),
            Some(BarrierExit {
                price: dec!(109.890),
                slippage_per_unit: dec!(0.110),
                reason: "TakeProfit",
            })
        );
        let quiet = bar(dec!(100), dec!(101), dec!(99), dec!(100));
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, &quiet, 2, dec!(0.001)),
            None
        );
        assert_eq!(
            barrier_exit(PositionSide::Long, &exits, &quiet, 3, dec!(0.001)),
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
        core.submit(PendingEntry {
            intent,
            quantity: dec!(2),
        });
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            &bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        );
        assert_eq!(equity, dec!(9_999.8));
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(95)));

        core.evaluate_exits(
            &bar(dec!(100), dec!(111), dec!(100), dec!(110)),
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
        let intent = OrderIntent::with_risk_and_trailing(
            1,
            Side::Buy,
            dec!(100),
            dec!(95),
            dec!(115),
            5,
            Some(dec!(0.01)),
            Some(dec!(5)), // ATR = 5
        )
        .expect("valid intent");
        core.submit(PendingEntry {
            intent,
            quantity: dec!(1),
        });
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            &bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        );
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(95)));

        // Bar 1: Favorable excursion reaches 106 (+1.2 ATR >= 1.0 ATR)
        core.evaluate_exits(
            &bar(dec!(100), dec!(106), dec!(99), dec!(105)),
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
        let intent = OrderIntent::with_risk_and_trailing(
            1,
            Side::Buy,
            dec!(100),
            dec!(95),
            dec!(115),
            5,
            Some(dec!(0.01)),
            Some(dec!(5)),
        )
        .expect("valid intent");
        core.submit(PendingEntry {
            intent,
            quantity: dec!(1),
        });
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            &bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        );

        // Bar 1: High reaches 105.5 (+1.1 ATR) -> stop ratchets to breakeven (100)
        core.evaluate_exits(
            &bar(dec!(100), dec!(105.5), dec!(99), dec!(104)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(100)));

        // Bar 2: Sharp reversal drops below breakeven (low 98)
        core.evaluate_exits(
            &bar(dec!(104), dec!(104), dec!(98), dec!(98.5)),
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
        let intent = OrderIntent::with_risk_and_trailing(
            1,
            Side::Buy,
            dec!(100),
            dec!(95),
            dec!(120),
            5,
            Some(dec!(0.01)),
            Some(dec!(5)),
        )
        .expect("valid intent");
        core.submit(PendingEntry {
            intent,
            quantity: dec!(1),
        });
        let pending = core.take_pending().expect("queued");
        core.fill_entry(
            pending,
            &bar(dec!(100), dec!(100), dec!(100), dec!(100)),
            &config,
            &mut equity,
        );

        // Bar 1: High reaches 108 (+1.6 ATR >= 1.5 ATR) -> Stop ratchets to entry + 0.5 ATR = 102.5
        core.evaluate_exits(
            &bar(dec!(100), dec!(108), dec!(99), dec!(107)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(102.5)));

        // Bar 2: High reaches 111 (+2.2 ATR >= 2.0 ATR) -> Stop ratchets to entry + 1.0 ATR = 105.0
        core.evaluate_exits(
            &bar(dec!(107), dec!(111), dec!(106), dec!(110)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert_eq!(core.exits().map(|e| e.stop), Some(dec!(105.0)));

        // Bar 3: Reversal drops to 104, triggering the trailed stop at 105.0
        core.evaluate_exits(
            &bar(dec!(110), dec!(110), dec!(104), dec!(104.5)),
            &config,
            &mut equity,
            &mut excursion,
        );
        assert!(core.position().is_none());
        let trade = &core.ledger().trades()[0];
        assert_eq!(trade.exit_reason, "StopLoss");
        assert_eq!(trade.exit_price, dec!(105.0));
        assert_eq!(trade.pnl_gross, dec!(5.0)); // Locked in profit!
    }
}
