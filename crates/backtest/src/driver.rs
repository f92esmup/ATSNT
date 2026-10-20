//! The loop that feeds a [`TradingSession`] with what happens in the market.
//!
//! Live trading and the tick-level simulation both drive their session through this type, so the
//! order of operations the simulation validates is exactly the order live runs: a market trade
//! first reaches the monthly breaker and the emergency-close retry, then the dollar-bar sampler,
//! and a finished bar reaches the strategy last. Venues report executions and balances through
//! the other methods.

use domain::{DollarBar, DollarBarAggregator, DomainError, Trade};
use ports::{ExecutionReport, ExecutionVenue};
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::session::{SessionEvent, TradingSession};

/// What one market trade produced.
#[derive(Debug, Clone, PartialEq)]
pub struct DriverStep {
    /// The dollar bar this trade completed, if any.
    pub bar: Option<DollarBar>,
    /// What the session did because of the trade itself (monthly breaker, emergency close).
    pub trade_events: Vec<SessionEvent>,
    /// What the session did because the trade completed a bar (time barrier, signal, entry).
    pub bar_events: Vec<SessionEvent>,
}

impl DriverStep {
    /// Every event of the step in the order it happened.
    pub fn into_events(self) -> Vec<SessionEvent> {
        let mut events = self.trade_events;
        events.extend(self.bar_events);
        events
    }
}

/// A session plus the dollar-bar sampler that feeds its strategy.
pub struct SessionDriver<S: Strategy> {
    session: TradingSession<S>,
    aggregator: DollarBarAggregator,
    threshold: Decimal,
    ticks: u64,
    bars: u64,
}

impl<S: Strategy> SessionDriver<S> {
    /// Wraps `session`, sampling bars of `threshold` dollars.
    pub fn new(session: TradingSession<S>, threshold: Decimal) -> Result<Self, DomainError> {
        Ok(Self {
            aggregator: DollarBarAggregator::new(threshold)?,
            session,
            threshold,
            ticks: 0,
            bars: 0,
        })
    }

    /// Feeds one market trade: the session's trade-time duties run first, then the sampler, and
    /// a completed bar is delivered to the session (or raw trade for Trades strategies).
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
        let (bar, bar_events) = match self.session.strategy().input_kind() {
            strategies::InputKind::Trades => {
                let events = self.session.on_trade_event(venue, trade).await;
                (None, events)
            }
            strategies::InputKind::DollarBars => {
                let bar = self.aggregator.process_trade(trade);
                let mut bar_events = Vec::new();
                if let Some(bar) = &bar {
                    self.bars += 1;
                    bar_events = self.session.on_bar(venue, bar).await;
                }
                (bar, bar_events)
            }
        };
        DriverStep {
            bar,
            trade_events,
            bar_events,
        }
    }

    /// Advances the session's clock when no market trade arrives; see
    /// [`TradingSession::on_idle`].
    pub async fn on_idle<V: ExecutionVenue>(
        &mut self,
        venue: &V,
        now_ms: i64,
    ) -> Vec<SessionEvent> {
        self.session.on_idle(venue, now_ms).await
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

    /// Warms the strategy up on historical trades; the live sampler starts on a fresh bar.
    pub fn warm_up_trades(
        &mut self,
        trades: impl IntoIterator<Item = Trade>,
    ) -> Result<usize, DomainError> {
        self.session.warm_up_trades(self.threshold, trades)
    }

    /// The session being driven.
    pub fn session(&self) -> &TradingSession<S> {
        &self.session
    }

    /// The strategy inside the session.
    pub fn strategy(&self) -> &S {
        self.session.strategy()
    }

    /// Market trades fed so far.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Dollar bars the strategy has seen so far.
    pub fn bars(&self) -> u64 {
        self.bars
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use domain::{OrderIntent, Side};
    use ports::{EntryAck, ExitGateway, OrderRequest};
    use rust_decimal_macros::dec;
    use std::cell::RefCell;
    use std::collections::VecDeque;
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
        async fn submit_entry(
            &self,
            _: &str,
            _: &OrderIntent,
            _: Decimal,
            _: Decimal,
        ) -> Result<EntryAck, String> {
            self.calls.borrow_mut().push("entry".to_string());
            Ok(EntryAck {
                order_id: "e1".to_string(),
                status: "NEW".to_string(),
                quantity: dec!(1),
                price: dec!(100),
                sent_at_ms: 0,
            })
        }

        async fn submit_order(&self, _: &str, _: &OrderRequest) -> Result<EntryAck, String> {
            self.calls.borrow_mut().push("order".to_string());
            Err("explicit orders are not scripted".to_string())
        }
    }

    struct Scripted {
        intents: VecDeque<OrderIntent>,
        bars_seen: usize,
    }

    impl Strategy for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_bar(&mut self, _: &DollarBar) -> Option<OrderIntent> {
            self.bars_seen += 1;
            self.intents.pop_front()
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
                risk_policy: None,
            },
            Scripted {
                intents: intents.into(),
                bars_seen: 0,
            },
        );
        SessionDriver::new(session, dec!(200)).unwrap()
    }

    fn long() -> OrderIntent {
        OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap()
    }

    fn print(timestamp: i64) -> Trade {
        Trade::new(timestamp, dec!(100), dec!(1), Side::Buy).unwrap()
    }

    #[tokio::test]
    async fn a_trade_that_does_not_complete_a_bar_does_not_reach_the_strategy() {
        let venue = Venue::default();
        let mut driver = driver(vec![long()]);

        let step = driver.on_market_trade(&venue, &print(1_000)).await;

        assert!(step.bar.is_none());
        assert!(step.trade_events.is_empty() && step.bar_events.is_empty());
        assert_eq!((driver.ticks(), driver.bars()), (1, 0));
    }

    #[tokio::test]
    async fn the_trade_that_completes_a_bar_delivers_it_and_the_signal_reaches_the_venue() {
        let venue = Venue::default();
        let mut driver = driver(vec![long()]);
        driver.on_market_trade(&venue, &print(1_000)).await;

        // $100 + $100 reaches the $200 threshold.
        let step = driver.on_market_trade(&venue, &print(1_001)).await;

        assert_eq!(step.bar.as_ref().map(|bar| bar.close), Some(dec!(100)));
        assert!(step.trade_events.is_empty());
        assert!(matches!(step.bar_events[0], SessionEvent::Signal(_)));
        assert!(step
            .bar_events
            .iter()
            .any(|e| matches!(e, SessionEvent::EntrySubmitted(_))));
        assert_eq!(venue.calls.borrow().as_slice(), ["entry"]);
        assert_eq!((driver.ticks(), driver.bars()), (2, 1));
    }

    #[tokio::test]
    async fn trade_time_duties_run_before_the_bar_reaches_the_strategy() {
        // The monthly kill-switch trips on the very trade that completes a bar; the signal that
        // bar produces must then be refused, which only holds if the breaker ran first.
        let venue = Venue::default();
        let mut driver = driver(vec![long()]);
        driver.on_wallet_balance(dec!(1000));
        driver.on_market_trade(&venue, &print(1_000)).await;
        // Month open equity was marked at 1000; a 20% wallet drop breaches the 15% limit.
        driver.on_wallet_balance(dec!(800));

        let step = driver.on_market_trade(&venue, &print(1_001)).await;

        // The breaker is a trade-time event; the refused signal belongs to the bar.
        assert!(matches!(
            step.trade_events[0],
            SessionEvent::MonthlyBreakerTripped { .. }
        ));
        assert!(step
            .bar_events
            .iter()
            .any(|e| matches!(e, SessionEvent::SignalSkipped(_))));
        assert!(!venue.calls.borrow().iter().any(|c| c == "entry"));
    }

    #[tokio::test]
    async fn warming_up_shows_the_strategy_history_without_counting_live_bars() {
        let venue = Venue::default();
        let mut driver = driver(vec![long()]);

        let warm_bars = driver.warm_up_trades((0..2).map(print)).unwrap();
        let step = driver.on_market_trade(&venue, &print(5_000)).await;

        // The scripted signal was spent on history; the live sampler starts on a fresh bar.
        assert_eq!(warm_bars, 1);
        assert!(step.bar.is_none());
        assert_eq!((driver.ticks(), driver.bars()), (1, 0));
        assert!(venue.calls.borrow().is_empty());
    }

    #[test]
    fn an_invalid_threshold_is_rejected() {
        let session = TradingSession::new(
            SessionConfig {
                symbol: "BTCUSDT".to_string(),
                initial_equity: dec!(1000),
                fallback_fee_pct: dec!(0.0005),
                max_monthly_drawdown_pct: dec!(0.15),
                halt_retry_ms: 5_000,
                entry_timeout_ms: 30_000,
                risk_policy: None,
            },
            Scripted {
                intents: VecDeque::new(),
                bars_seen: 0,
            },
        );

        assert!(SessionDriver::new(session, Decimal::ZERO).is_err());
    }

    #[tokio::test]
    async fn the_clock_can_advance_without_a_market_trade() {
        let venue = Venue::default();
        let mut driver = driver(vec![long()]);
        driver.on_market_trade(&venue, &print(1_000)).await;
        driver.on_market_trade(&venue, &print(1_001)).await;
        assert!(driver.session().is_exposed());

        // The entry never reaches a terminal status and no trade arrives.
        let events = driver.on_idle(&venue, 1_001 + 30_000).await;

        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::EntryTimedOut { .. })));
        assert!(!driver.session().is_exposed());
    }
}
