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
use domain::{OrderIntent, Position, PositionSide, ProtectiveExits, Side};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::config::BacktestConfig;
use crate::metrics::{ClosedTrade, TradeExcursion};
use crate::price_event::PriceEvent;

/// A submitted entry: the strategy intent and the quantity sized when it was signalled.
#[derive(Debug, Clone)]
pub struct PendingEntry {
    /// The strategy's triple-barrier intent.
    pub intent: OrderIntent,
    /// Quantity sized at the signal price.
    pub quantity: Decimal,
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

    fn push(&mut self, trade: ClosedTrade) {
        self.trades.push(trade);
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
    entry_time: Option<i64>,
    entry_fee: Decimal,
    entry_slippage: Decimal,
    /// Isolated margin posted for the open position.
    margin: Decimal,
    /// Funding paid by the open position so far.
    funding_accrued: Decimal,
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
        let entry_fee = fill_price * quantity * config.taker_fee_pct;

        let notional = fill_price * quantity;
        let Some(leverage) = futures::minimum_leverage(
            notional,
            notional * config.maintenance_margin_rate,
            entry_fee,
            *equity,
            config.max_leverage.trunc().to_u32().unwrap_or(1),
        ) else {
            return;
        };
        // Slippage is already embedded in fill_price; do not deduct from cash equity here
        *equity -= entry_fee;
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
            self.position = Some(pos);
            self.exits = Some(ProtectiveExits::from_intent(&intent));
            self.intent = Some(intent);
            self.bars_held = 0;
            self.entry_time = Some(bar.start_ms);
            self.entry_fee = entry_fee;
            self.entry_slippage = adverse_per_unit * quantity;
            self.margin = notional / Decimal::from(leverage);
            self.funding_accrued = Decimal::ZERO;
        }
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
        let (Some(exits), Some(position)) = (self.exits, &self.position) else {
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
        }
    }

    /// Liquidation price of the open position, if one is open.
    pub fn liquidation_price(&self, config: &BacktestConfig) -> Option<Decimal> {
        let position = self.position.as_ref()?;
        let side = opening_side(position.side);
        let maintenance = position.quantity * position.entry_price * config.maintenance_margin_rate;
        Some(futures::liquidation_price(
            side,
            position.entry_price,
            position.quantity,
            self.margin,
            maintenance,
        ))
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
    /// records the [`ClosedTrade`]. The open trade's excursion is consumed (or started at the
    /// entry when there is none) and finished at the post-close cash balance.
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
        let entry_time = self.entry_time.take().unwrap_or(exit_time);
        let entry_fee = std::mem::take(&mut self.entry_fee);
        let entry_slippage = std::mem::take(&mut self.entry_slippage);

        let pnl_gross = pos.unrealized_pnl(exit_price);
        let exit_notional = exit_price * pos.quantity;
        let funding_paid = std::mem::take(&mut self.funding_accrued);
        self.margin = Decimal::ZERO;

        let exit_fee = exit_notional * fee_rate;
        let exit_slippage = slippage_per_unit * pos.quantity;
        let total_fees = entry_fee + exit_fee;
        let total_slippage = entry_slippage + exit_slippage;

        // In real execution, fill_price and exit_price already embed slippage into pnl_gross.
        // We report total_slippage as an informative metric, but do not double-deduct it.
        let pnl_net = pnl_gross - total_fees - funding_paid;

        // equity already had entry_fee deducted upon entry. Adjust with pnl_gross and exit_fee.
        *equity += pnl_gross - exit_fee;

        let return_pct = if pos.entry_price > Decimal::ZERO && pos.quantity > Decimal::ZERO {
            pnl_net / (pos.entry_price * pos.quantity)
        } else {
            Decimal::ZERO
        };

        let holding_duration_seconds = ((exit_time - entry_time) / 1000).max(0);

        let mut excursion = excursion
            .take()
            .unwrap_or_else(|| TradeExcursion::start(entry_time, *equity));
        excursion.observe_equity(exit_time, *equity);
        excursion.exit_equity = *equity;

        self.ledger.push(ClosedTrade {
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
        }
    }

    fn stop_only(stop: Decimal) -> ProtectiveExits {
        ProtectiveExits {
            stop,
            take_profit: None,
            max_bars: None,
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
}
