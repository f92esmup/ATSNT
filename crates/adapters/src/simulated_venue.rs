//! Tick-level simulated execution venue.
//!
//! Replays the exchange mechanics a live session meets, from a stream of aggregated trades:
//!
//! * every order reaches the market after a fixed latency;
//! * a market order walks the tape: it only consumes prints whose aggressor is on its own side
//!   (a buy lifts offers, which print as buyer-initiated trades), at each print's price, so the
//!   slippage is whatever the tape paid after the order arrived, not a fixed percentage;
//! * exchange-held stop-loss and take-profit are conditional market orders that arm when the
//!   tape crosses their trigger and then travel and fill like any other market order;
//! * every fill is a taker fill with its commission, and the venue keeps the wallet balance.
//!
//! `aggTrades` carries no mark price, so triggers use the last trade price as the mark proxy.

use std::cell::RefCell;

use domain::{OrderIntent, RiskError, RiskPolicy, Side, Trade};
use ports::{EntryAck, ExecutionReport, ExecutionVenue, ExitGateway};
use rust_decimal::Decimal;
use thiserror::Error;

/// Quote asset commissions are charged in.
const COMMISSION_ASSET: &str = "USDT";

/// Static parameters of a simulated venue.
#[derive(Debug, Clone)]
pub struct SimulatedVenueConfig {
    /// Wallet balance at the start of the run.
    pub initial_equity: Decimal,
    /// Taker fee as a fraction of notional (e.g. 0.0005 = 0.05%).
    pub taker_fee_pct: Decimal,
    /// Milliseconds between sending an order and it reaching the market.
    pub latency_ms: i64,
    /// Fraction of equity risked per entry.
    pub risk_per_trade_pct: Decimal,
    /// Sizing and pre-trade limits shared with the live gateway.
    pub risk_policy: RiskPolicy,
}

/// Why the simulated venue refused an operation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SimulatedVenueError {
    /// The venue has not seen a trade yet, so it has no market to trade in.
    #[error("no market data yet")]
    NoMarketData,
    /// Sizing or the pre-trade risk gate refused the entry.
    #[error("risk gate rejected the entry: {0}")]
    Risk(String),
    /// A position or an entry is already working.
    #[error("a position or an entry order already exists")]
    PositionExists,
    /// There is no position to protect.
    #[error("no open position to protect")]
    NoPosition,
    /// A trigger price sits on the wrong side of the market and would fire immediately.
    #[error("order would immediately trigger")]
    WouldTriggerImmediately,
}

impl From<RiskError> for SimulatedVenueError {
    fn from(error: RiskError) -> Self {
        Self::Risk(error.to_string())
    }
}

#[derive(Debug, Clone)]
struct WorkingOrder {
    side: Side,
    remaining: Decimal,
    /// Time the order reaches the market.
    due_ms: i64,
    /// Closing orders never increase the position and survive `cancel_orders`.
    reduce_only: bool,
}

#[derive(Debug, Clone, Copy)]
struct SimPosition {
    side: Side,
    quantity: Decimal,
    entry_price: Decimal,
}

#[derive(Debug, Clone, Copy)]
struct ExitLegs {
    stop_loss: Decimal,
    take_profit: Decimal,
}

#[derive(Debug)]
struct State {
    clock_ms: i64,
    last_price: Option<Decimal>,
    wallet: Decimal,
    position: Option<SimPosition>,
    exits: Option<ExitLegs>,
    orders: Vec<WorkingOrder>,
    next_order_id: u64,
}

/// Simulated exchange account for one symbol, driven tick by tick.
pub struct SimulatedVenue {
    config: SimulatedVenueConfig,
    state: RefCell<State>,
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}

impl SimulatedVenue {
    /// Creates a flat venue holding the configured balance.
    pub fn new(config: SimulatedVenueConfig) -> Self {
        let wallet = config.initial_equity;
        Self {
            config,
            state: RefCell::new(State {
                clock_ms: 0,
                last_price: None,
                wallet,
                position: None,
                exits: None,
                orders: Vec::new(),
                next_order_id: 1,
            }),
        }
    }

    /// Wallet balance: initial equity plus realized PnL minus commissions.
    pub fn wallet_balance(&self) -> Decimal {
        self.state.borrow().wallet
    }

    /// Side, quantity and average entry price of the open position, if any.
    pub fn position(&self) -> Option<(Side, Decimal, Decimal)> {
        self.state
            .borrow()
            .position
            .map(|p| (p.side, p.quantity, p.entry_price))
    }

    /// Advances the market by one print and returns the executions it caused, oldest first.
    pub fn on_trade(&self, trade: &Trade) -> Vec<ExecutionReport> {
        let mut state = self.state.borrow_mut();
        state.clock_ms = trade.timestamp;
        state.last_price = Some(trade.price);
        // Orders already at the market trade first; an exit armed by this print reaches the
        // market after it, so the print that triggers a stop never fills it.
        let reports = self.match_orders(&mut state, trade);
        self.arm_triggered_exits(&mut state, trade.price);
        reports
    }

    /// Arms the closing market order of a leg the tape has crossed; the other leg is dropped.
    fn arm_triggered_exits(&self, state: &mut State, price: Decimal) {
        let (Some(position), Some(legs)) = (state.position, state.exits) else {
            return;
        };
        let (stop_hit, take_hit) = match position.side {
            Side::Buy => (price <= legs.stop_loss, price >= legs.take_profit),
            Side::Sell => (price >= legs.stop_loss, price <= legs.take_profit),
        };
        if stop_hit || take_hit {
            state.exits = None;
            let due_ms = state.clock_ms + self.config.latency_ms;
            state.orders.push(WorkingOrder {
                side: opposite(position.side),
                remaining: position.quantity,
                due_ms,
                reduce_only: true,
            });
        }
    }

    /// Fills the working orders that have reached the market against one print.
    fn match_orders(&self, state: &mut State, trade: &Trade) -> Vec<ExecutionReport> {
        let mut reports = Vec::new();
        let mut available = trade.quantity;
        let mut index = 0;
        while index < state.orders.len() && available > Decimal::ZERO {
            let order = &state.orders[index];
            if order.due_ms > trade.timestamp || order.side != trade.side {
                index += 1;
                continue;
            }
            let mut take = order.remaining.min(available);
            if order.reduce_only {
                let open = state.position.map_or(Decimal::ZERO, |p| p.quantity);
                take = take.min(open);
            }
            if take <= Decimal::ZERO {
                // A closing order with nothing left to close is void.
                state.orders.remove(index);
                continue;
            }
            available -= take;
            let side = order.side;
            let reduce_only = order.reduce_only;
            state.orders[index].remaining -= take;
            let filled = state.orders[index].remaining <= Decimal::ZERO;
            let commission = trade.price * take * self.config.taker_fee_pct;
            self.apply_fill(state, side, take, trade.price, commission, reduce_only);
            reports.push(ExecutionReport {
                time_ms: trade.timestamp,
                side,
                last_quantity: take,
                last_price: trade.price,
                order_filled: filled,
                order_closed: filled,
                commission,
                commission_asset: Some(COMMISSION_ASSET.to_string()),
            });
            if filled {
                state.orders.remove(index);
            } else {
                index += 1;
            }
        }
        reports
    }

    fn apply_fill(
        &self,
        state: &mut State,
        side: Side,
        quantity: Decimal,
        price: Decimal,
        commission: Decimal,
        reduce_only: bool,
    ) {
        state.wallet -= commission;
        match state.position {
            Some(mut position) if reduce_only || position.side != side => {
                let direction = match position.side {
                    Side::Buy => Decimal::ONE,
                    Side::Sell => -Decimal::ONE,
                };
                state.wallet += (price - position.entry_price) * quantity * direction;
                position.quantity -= quantity;
                state.position = (position.quantity > Decimal::ZERO).then_some(position);
                if state.position.is_none() {
                    state.exits = None;
                }
            }
            Some(mut position) => {
                let total = position.quantity + quantity;
                position.entry_price =
                    (position.entry_price * position.quantity + price * quantity) / total;
                position.quantity = total;
                state.position = Some(position);
            }
            None => {
                state.position = Some(SimPosition {
                    side,
                    quantity,
                    entry_price: price,
                });
            }
        }
    }
}

impl ExitGateway for SimulatedVenue {
    type Error = SimulatedVenueError;

    async fn place_exits(
        &self,
        _symbol: &str,
        entry_side: Side,
        stop_loss: Decimal,
        take_profit: Decimal,
    ) -> Result<(), Self::Error> {
        let mut state = self.state.borrow_mut();
        let last = state.last_price.ok_or(SimulatedVenueError::NoMarketData)?;
        if state.position.is_none() {
            return Err(SimulatedVenueError::NoPosition);
        }
        let immediate = match entry_side {
            Side::Buy => stop_loss >= last || take_profit <= last,
            Side::Sell => stop_loss <= last || take_profit >= last,
        };
        if immediate {
            return Err(SimulatedVenueError::WouldTriggerImmediately);
        }
        state.exits = Some(ExitLegs {
            stop_loss,
            take_profit,
        });
        Ok(())
    }

    async fn cancel_orders(&self, _symbol: &str) -> Result<(), Self::Error> {
        let mut state = self.state.borrow_mut();
        state.exits = None;
        state.orders.retain(|order| order.reduce_only);
        Ok(())
    }

    async fn close_position(&self, _symbol: &str) -> Result<bool, Self::Error> {
        let mut state = self.state.borrow_mut();
        let Some(position) = state.position else {
            return Ok(false);
        };
        let pending: Decimal = state
            .orders
            .iter()
            .filter(|order| order.reduce_only)
            .map(|order| order.remaining)
            .sum();
        if pending < position.quantity {
            let due_ms = state.clock_ms + self.config.latency_ms;
            state.orders.push(WorkingOrder {
                side: opposite(position.side),
                remaining: position.quantity - pending,
                due_ms,
                reduce_only: true,
            });
        }
        Ok(true)
    }
}

impl ExecutionVenue for SimulatedVenue {
    async fn submit_entry(
        &self,
        _symbol: &str,
        intent: &OrderIntent,
        equity: Decimal,
        session_peak: Decimal,
    ) -> Result<EntryAck, Self::Error> {
        let mut state = self.state.borrow_mut();
        state.last_price.ok_or(SimulatedVenueError::NoMarketData)?;
        let working_entry = state.orders.iter().any(|order| !order.reduce_only);
        if state.position.is_some() || working_entry {
            return Err(SimulatedVenueError::PositionExists);
        }
        let drawdown = RiskPolicy::calculate_session_drawdown_pct(session_peak, equity)?;
        let quantity = self.config.risk_policy.size_order(
            intent,
            intent.price,
            equity,
            self.config.risk_per_trade_pct,
            Decimal::ZERO,
            drawdown,
        )?;
        let order_id = format!("sim-{}", state.next_order_id);
        state.next_order_id += 1;
        let due_ms = state.clock_ms + self.config.latency_ms;
        state.orders.push(WorkingOrder {
            side: intent.side,
            remaining: quantity,
            due_ms,
            reduce_only: false,
        });
        Ok(EntryAck {
            order_id,
            status: "NEW".to_string(),
            quantity,
            price: intent.price,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn venue(latency_ms: i64) -> SimulatedVenue {
        SimulatedVenue::new(SimulatedVenueConfig {
            initial_equity: dec!(1000),
            taker_fee_pct: dec!(0.001),
            latency_ms,
            risk_per_trade_pct: dec!(0.01),
            risk_policy: RiskPolicy::default(),
        })
    }

    fn print(timestamp: i64, price: Decimal, quantity: Decimal, side: Side) -> Trade {
        Trade::new(timestamp, price, quantity, side).unwrap()
    }

    fn long_intent() -> OrderIntent {
        OrderIntent::new(0, Side::Buy, dec!(100), dec!(98), dec!(104), 10).unwrap()
    }

    /// Venue that has seen a print at 100 and holds a long of 5 units entered at 100.
    async fn long_venue(latency_ms: i64) -> SimulatedVenue {
        let venue = venue(latency_ms);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));
        venue
            .submit_entry("BTCUSDT", &long_intent(), dec!(1000), dec!(1000))
            .await
            .unwrap();
        venue.on_trade(&print(1_000 + latency_ms, dec!(100), dec!(10), Side::Buy));
        venue
    }

    #[tokio::test]
    async fn an_entry_needs_a_market_before_it_can_be_sized() {
        let venue = venue(50);

        let result = venue
            .submit_entry("BTCUSDT", &long_intent(), dec!(1000), dec!(1000))
            .await;

        assert_eq!(result, Err(SimulatedVenueError::NoMarketData));
    }

    #[tokio::test]
    async fn the_entry_is_sized_by_the_shared_risk_policy() {
        let venue = venue(50);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));

        let ack = venue
            .submit_entry("BTCUSDT", &long_intent(), dec!(1000), dec!(1000))
            .await
            .unwrap();

        // 1% of 1000 is 10 at risk over a 2 stop distance.
        assert_eq!(ack.quantity, dec!(5));
        assert_eq!(ack.price, dec!(100));
        assert_eq!(ack.status, "NEW");
    }

    #[tokio::test]
    async fn a_second_entry_is_refused_while_one_is_working() {
        let venue = venue(50);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));
        let intent = long_intent();
        venue
            .submit_entry("BTCUSDT", &intent, dec!(1000), dec!(1000))
            .await
            .unwrap();

        let second = venue
            .submit_entry("BTCUSDT", &intent, dec!(1000), dec!(1000))
            .await;

        assert_eq!(second, Err(SimulatedVenueError::PositionExists));
    }

    #[tokio::test]
    async fn an_order_waits_for_its_latency_before_it_trades() {
        let venue = venue(100);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));
        venue
            .submit_entry("BTCUSDT", &long_intent(), dec!(1000), dec!(1000))
            .await
            .unwrap();

        let early = venue.on_trade(&print(1_050, dec!(100), dec!(10), Side::Buy));
        let on_time = venue.on_trade(&print(1_100, dec!(100.5), dec!(10), Side::Buy));

        assert!(early.is_empty());
        assert_eq!(on_time.len(), 1);
        assert_eq!(on_time[0].last_price, dec!(100.5));
        assert_eq!(on_time[0].last_quantity, dec!(5));
        assert!(on_time[0].order_filled && on_time[0].order_closed);
    }

    #[tokio::test]
    async fn a_market_buy_walks_the_tape_and_ignores_opposite_aggressor_prints() {
        let venue = venue(0);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));
        venue
            .submit_entry("BTCUSDT", &long_intent(), dec!(1000), dec!(1000))
            .await
            .unwrap();

        let first = venue.on_trade(&print(1_001, dec!(100.1), dec!(2), Side::Buy));
        let ignored = venue.on_trade(&print(1_002, dec!(99), dec!(50), Side::Sell));
        let last = venue.on_trade(&print(1_003, dec!(100.4), dec!(9), Side::Buy));

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].last_quantity, dec!(2));
        assert!(!first[0].order_filled);
        assert!(ignored.is_empty());
        assert_eq!(last[0].last_quantity, dec!(3));
        assert_eq!(last[0].last_price, dec!(100.4));
        assert!(last[0].order_filled);
        // VWAP of the two fills: (2 * 100.1 + 3 * 100.4) / 5.
        let (_, quantity, entry) = venue.position().unwrap();
        assert_eq!(quantity, dec!(5));
        assert_eq!(entry, dec!(100.28));
    }

    #[tokio::test]
    async fn fills_pay_the_taker_commission() {
        let venue = venue(0);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));
        venue
            .submit_entry("BTCUSDT", &long_intent(), dec!(1000), dec!(1000))
            .await
            .unwrap();

        let reports = venue.on_trade(&print(1_001, dec!(100), dec!(10), Side::Buy));

        // 5 units at 100 with a 0.1% taker fee.
        assert_eq!(reports[0].commission, dec!(0.5));
        assert_eq!(reports[0].commission_asset.as_deref(), Some("USDT"));
        assert_eq!(venue.wallet_balance(), dec!(999.5));
    }

    #[tokio::test]
    async fn exits_that_would_trigger_immediately_are_refused() {
        let venue = long_venue(0).await;

        let stop_above_market = venue
            .place_exits("BTCUSDT", Side::Buy, dec!(100.5), dec!(104))
            .await;
        let target_below_market = venue
            .place_exits("BTCUSDT", Side::Buy, dec!(98), dec!(99.5))
            .await;

        assert_eq!(
            stop_above_market,
            Err(SimulatedVenueError::WouldTriggerImmediately)
        );
        assert_eq!(
            target_below_market,
            Err(SimulatedVenueError::WouldTriggerImmediately)
        );
    }

    #[tokio::test]
    async fn exits_need_a_position() {
        let venue = venue(0);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));

        let result = venue
            .place_exits("BTCUSDT", Side::Buy, dec!(98), dec!(104))
            .await;

        assert_eq!(result, Err(SimulatedVenueError::NoPosition));
    }

    #[tokio::test]
    async fn a_crossed_stop_fills_on_the_tape_after_the_latency() {
        let venue = long_venue(100).await;
        venue
            .place_exits("BTCUSDT", Side::Buy, dec!(98), dec!(104))
            .await
            .unwrap();

        // The stop arms at 97.9; the sell order only reaches the market 100 ms later and
        // fills at the print it finds there, below the trigger.
        let armed = venue.on_trade(&print(2_000, dec!(97.9), dec!(1), Side::Sell));
        let too_early = venue.on_trade(&print(2_050, dec!(97.5), dec!(10), Side::Sell));
        let filled = venue.on_trade(&print(2_100, dec!(97.2), dec!(10), Side::Sell));

        assert!(armed.is_empty() && too_early.is_empty());
        assert_eq!(filled.len(), 1);
        assert_eq!(filled[0].side, Side::Sell);
        assert_eq!(filled[0].last_price, dec!(97.2));
        assert_eq!(filled[0].last_quantity, dec!(5));
        assert!(venue.position().is_none());
        // Entry 5 @ 100 and exit 5 @ 97.2: -14 gross, 0.5 + 0.486 in commissions.
        assert_eq!(venue.wallet_balance(), dec!(985.014));
    }

    #[tokio::test]
    async fn the_print_that_arms_a_stop_does_not_fill_it() {
        let venue = long_venue(0).await;
        venue
            .place_exits("BTCUSDT", Side::Buy, dec!(98), dec!(104))
            .await
            .unwrap();

        let trigger = venue.on_trade(&print(2_000, dec!(97.9), dec!(10), Side::Sell));
        let next = venue.on_trade(&print(2_001, dec!(97.6), dec!(10), Side::Sell));

        assert!(trigger.is_empty());
        assert_eq!(next[0].last_price, dec!(97.6));
    }

    #[tokio::test]
    async fn a_crossed_take_profit_fills_as_a_market_order_and_drops_the_stop() {
        let venue = long_venue(0).await;
        venue
            .place_exits("BTCUSDT", Side::Buy, dec!(98), dec!(104))
            .await
            .unwrap();

        venue.on_trade(&print(2_000, dec!(104), dec!(1), Side::Buy));
        let filled = venue.on_trade(&print(2_001, dec!(103.8), dec!(10), Side::Sell));
        let afterwards = venue.on_trade(&print(2_002, dec!(90), dec!(10), Side::Sell));

        assert_eq!(filled[0].last_price, dec!(103.8));
        assert!(afterwards.is_empty());
        assert!(venue.position().is_none());
    }

    #[tokio::test]
    async fn a_short_position_triggers_on_the_mirrored_sides() {
        let venue = venue(0);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Buy));
        let intent = OrderIntent::new(0, Side::Sell, dec!(100), dec!(102), dec!(96), 10).unwrap();
        venue
            .submit_entry("BTCUSDT", &intent, dec!(1000), dec!(1000))
            .await
            .unwrap();
        venue.on_trade(&print(1_001, dec!(100), dec!(10), Side::Sell));
        venue
            .place_exits("BTCUSDT", Side::Sell, dec!(102), dec!(96))
            .await
            .unwrap();

        venue.on_trade(&print(2_000, dec!(96), dec!(1), Side::Sell));
        let filled = venue.on_trade(&print(2_001, dec!(96.3), dec!(10), Side::Buy));

        assert_eq!(filled[0].side, Side::Buy);
        assert_eq!(filled[0].last_price, dec!(96.3));
        assert!(venue.position().is_none());
    }

    #[tokio::test]
    async fn cancelling_orders_drops_working_entries_and_exits_but_not_closing_orders() {
        let venue = long_venue(0).await;
        venue
            .place_exits("BTCUSDT", Side::Buy, dec!(98), dec!(104))
            .await
            .unwrap();
        assert!(venue.close_position("BTCUSDT").await.unwrap());

        venue.cancel_orders("BTCUSDT").await.unwrap();
        let filled = venue.on_trade(&print(3_000, dec!(100), dec!(10), Side::Sell));

        assert_eq!(filled.len(), 1);
        assert!(venue.position().is_none());
    }

    #[tokio::test]
    async fn an_in_flight_entry_is_cancelled_with_the_other_orders() {
        let venue = venue(100);
        venue.on_trade(&print(1_000, dec!(100), dec!(1), Side::Sell));
        venue
            .submit_entry("BTCUSDT", &long_intent(), dec!(1000), dec!(1000))
            .await
            .unwrap();

        venue.cancel_orders("BTCUSDT").await.unwrap();
        let reports = venue.on_trade(&print(1_200, dec!(100), dec!(10), Side::Buy));

        assert!(reports.is_empty());
        assert!(venue.position().is_none());
    }

    #[tokio::test]
    async fn closing_a_flat_account_reports_nothing_to_close() {
        let venue = venue(0);

        assert!(!venue.close_position("BTCUSDT").await.unwrap());
    }

    #[tokio::test]
    async fn closing_twice_does_not_queue_a_second_order() {
        let venue = long_venue(0).await;

        assert!(venue.close_position("BTCUSDT").await.unwrap());
        assert!(venue.close_position("BTCUSDT").await.unwrap());
        let fills = venue.on_trade(&print(3_000, dec!(100), dec!(100), Side::Sell));

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].last_quantity, dec!(5));
        assert!(venue.position().is_none());
    }

    #[tokio::test]
    async fn a_closing_order_never_flips_the_position() {
        let venue = long_venue(0).await;
        venue.close_position("BTCUSDT").await.unwrap();

        venue.on_trade(&print(3_000, dec!(100), dec!(100), Side::Sell));
        let later = venue.on_trade(&print(3_001, dec!(100), dec!(100), Side::Sell));

        assert!(later.is_empty());
        assert!(venue.position().is_none());
    }
}
