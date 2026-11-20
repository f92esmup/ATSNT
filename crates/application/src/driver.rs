//! The loop that feeds a [`TradingSession`] with what happens in the market.
//!
//! The tick-level simulation drives its session through this type, in the order a live run
//! would: a market trade first reaches the monthly breaker, the exits and the emergency-close
//! retry, and then the strategy. Venues report executions and balances through the other methods.

use domain::Trade;
use ports::{ExecutionReport, ExecutionVenue};
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::session::{SessionEvent, TradingSession};

/// What one market trade produced.
#[derive(Debug, Clone, PartialEq)]
pub struct DriverStep {
    /// What the session did because of the trade itself (monthly breaker, exits, emergency close).
    pub trade_events: Vec<SessionEvent>,
    /// What the session did because the strategy saw the trade (signal, entry).
    pub strategy_events: Vec<SessionEvent>,
}

impl DriverStep {
    /// Every event of the step in the order it happened.
    pub fn into_events(self) -> Vec<SessionEvent> {
        let mut events = self.trade_events;
        events.extend(self.strategy_events);
        events
    }
}

/// A session and the count of the market trades it was fed.
pub struct SessionDriver<S: Strategy> {
    session: TradingSession<S>,
    ticks: u64,
}

impl<S: Strategy> SessionDriver<S> {
    /// Wraps `session`.
    pub fn new(session: TradingSession<S>) -> Self {
        Self { session, ticks: 0 }
    }

    /// Feeds one market trade: the session's trade-time duties run first, then the strategy.
    pub async fn on_market_trade<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        trade: &Trade,
    ) -> DriverStep {
        self.ticks += 1;
        let trade_events = self
            .session
            .on_trade(venue, trade.timestamp, trade.price)
            .await;
        let strategy_events = self.session.on_trade_event(venue, trade).await;
        DriverStep {
            trade_events,
            strategy_events,
        }
    }

    /// Feeds one execution report from the venue.
    pub async fn on_execution<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        report: &ExecutionReport,
    ) -> Vec<SessionEvent> {
        self.session.on_execution(venue, report).await
    }

    /// Records the latest wallet balance reported by the venue.
    pub fn on_wallet_balance(&mut self, equity: Decimal) {
        self.session.on_wallet_balance(equity);
    }

    /// Records a funding payment settled by the venue.
    pub fn on_funding(&mut self, amount: Decimal) {
        self.session.on_funding(amount);
    }

    /// Whether the account still matches the strategy profile.
    pub fn set_account_ok(&mut self, account_ok: bool) {
        self.session.set_account_ok(account_ok);
    }

    /// Warms the strategy up on historical trades. Returns how many trades it saw.
    pub fn warm_up_trades(&mut self, trades: impl IntoIterator<Item = Trade>) -> usize {
        self.session.warm_up_trades(trades)
    }

    /// The session being driven.
    pub fn session(&self) -> &TradingSession<S> {
        &self.session
    }

    /// The session being driven, to configure it between trades.
    pub fn session_mut(&mut self) -> &mut TradingSession<S> {
        &mut self.session
    }

    /// The strategy inside the session.
    pub fn strategy(&self) -> &S {
        self.session.strategy()
    }

    /// Market trades fed so far.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::session::SessionConfig;
    use domain::{Decision, OrderIntent, Side, StrategyView};
    use ports::{EntryAck, ExitGateway, OrderRequest};
    use rust_decimal_macros::dec;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use strategies::MarketEvent;
    use strategies::MarketType;

    #[derive(Default)]
    struct Venue {
        calls: RefCell<Vec<String>>,
    }

    impl ExitGateway for Venue {
        type Error = String;

        async fn place_exits(
            &self,
            _: &str,
            _: Side,
            _: Decimal,
            _: Option<Decimal>,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push("place".to_string());
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

    impl ExecutionVenue for Venue {
        async fn submit_order(&self, _: &str, request: &OrderRequest) -> Result<EntryAck, String> {
            self.calls.borrow_mut().push("entry".to_string());
            Ok(EntryAck {
                order_id: "e1".to_string(),
                status: ports::AckStatus::New,
                quantity: request.quantity,
                price: dec!(100),
                sent_at_ms: 0,
            })
        }
    }

    struct Scripted {
        intents: VecDeque<OrderIntent>,
        trades_seen: usize,
    }

    impl Scripted {
        fn decide(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent> {
            let MarketEvent::Trade { .. } = event else {
                return None;
            };
            self.trades_seen += 1;
            self.intents.pop_front()
        }
    }

    impl Strategy for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_event(
            &mut self,
            event: &MarketEvent<'_>,
            _view: &StrategyView,
            out: &mut Vec<Decision>,
        ) {
            out.extend(self.decide(event).map(Decision::Enter));
        }

        fn reset(&mut self) {}
    }

    fn driver(intents: Vec<OrderIntent>) -> SessionDriver<Scripted> {
        let session = TradingSession::new(
            SessionConfig {
                symbol: "BTCUSDT".to_string(),
                initial_equity: dec!(1000),
                fallback_fee_pct: dec!(0.0005),
                max_monthly_drawdown_pct: dec!(0.15),
                halt_retry_ms: 5_000,
                entry_timeout_ms: 30_000,
                gate: domain::EntryGate::new(
                    domain::RiskPolicy::default(),
                    dec!(0.01),
                    domain::MarketType::UsdMPerpetual,
                ),
            },
            Scripted {
                intents: intents.into(),
                trades_seen: 0,
            },
        );
        SessionDriver::new(session)
    }

    fn long() -> OrderIntent {
        OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap()
    }

    fn print(timestamp: i64) -> Trade {
        Trade::new(timestamp, dec!(100), dec!(1), Side::Buy).unwrap()
    }

    #[tokio::test]
    async fn a_trade_reaches_the_strategy_and_the_signal_reaches_the_venue() {
        let venue = Venue::default();
        let mut driver = driver(vec![long()]);

        let step = driver.on_market_trade(&venue, &print(1_000)).await;

        assert!(step.trade_events.is_empty());
        assert!(matches!(step.strategy_events[0], SessionEvent::Signal(_)));
        assert!(step
            .strategy_events
            .iter()
            .any(|e| matches!(e, SessionEvent::EntrySubmitted(_))));
        assert_eq!(venue.calls.borrow().as_slice(), ["entry"]);
        assert_eq!(driver.ticks(), 1);
        assert_eq!(driver.strategy().trades_seen, 1);
    }

    #[tokio::test]
    async fn trade_time_duties_run_before_the_strategy_sees_the_trade() {
        // The monthly kill-switch trips on the very trade the strategy then signals on; the
        // signal must be refused, which only holds if the breaker ran first.
        let venue = Venue::default();
        let mut driver = driver(vec![long(), long()]);
        driver.on_wallet_balance(dec!(1000));
        // The first trade marks the month's open equity and sends the first entry.
        driver.on_market_trade(&venue, &print(1_000)).await;
        // A 20% wallet drop breaches the 15% limit.
        driver.on_wallet_balance(dec!(800));

        let step = driver.on_market_trade(&venue, &print(1_001)).await;

        assert!(matches!(
            step.trade_events[0],
            SessionEvent::MonthlyBreakerTripped { .. }
        ));
        assert!(step
            .strategy_events
            .iter()
            .any(|e| matches!(e, SessionEvent::SignalSkipped(_))));
        let entries = venue
            .calls
            .borrow()
            .iter()
            .filter(|c| *c == "entry")
            .count();
        assert_eq!(entries, 1, "only the first signal reached the venue");
    }

    #[tokio::test]
    async fn warming_up_shows_the_strategy_history_without_counting_live_trades() {
        let venue = Venue::default();
        let mut driver = driver(vec![long()]);

        let warm = driver.warm_up_trades((0..2).map(print));
        let step = driver.on_market_trade(&venue, &print(5_000)).await;

        // The scripted signal was spent on history.
        assert_eq!(warm, 2);
        assert!(step.strategy_events.is_empty());
        assert_eq!(driver.ticks(), 1);
        assert!(venue.calls.borrow().is_empty());
    }
}
