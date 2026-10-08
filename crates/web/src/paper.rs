//! In-process paper session boundary; market-data sources are supplied by callers.

use backtest::{
    BacktestMetrics, ClosedTrade, PaperTradingConfig, PaperTradingEvent, PaperTradingSession,
    TelemetryConfig, TelemetryEnvelope,
};
use domain::Trade;
use tokio::sync::{broadcast, mpsc};

/// Owns the source-waiting input and task until explicit shutdown.
pub struct PaperSessionRuntime {
    input: mpsc::Sender<Trade>,
    task: tokio::task::JoinHandle<(BacktestMetrics, Vec<ClosedTrade>)>,
}

impl PaperSessionRuntime {
    /// Starts a session without creating a trade producer.
    pub fn spawn(owner: PaperSessionOwner, input: mpsc::Sender<Trade>) -> Self {
        Self {
            input,
            task: tokio::spawn(owner.run()),
        }
    }

    /// Closes the owned input, drains queued trades, and joins finalization.
    /// The input has no external clones in this source-waiting runtime.
    pub async fn shutdown(
        self,
    ) -> Result<(BacktestMetrics, Vec<ClosedTrade>), tokio::task::JoinError> {
        drop(self.input);
        self.task.await
    }
}

/// Owns one paper session and its bounded input receiver.
/// No market-data producer is created by this boundary.
pub struct PaperSessionOwner {
    session: PaperTradingSession,
    trades: mpsc::Receiver<Trade>,
    telemetry: TelemetryConfig,
    events: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
}

impl PaperSessionOwner {
    /// Creates a session and bounded source handle. Unknown identity stays empty.
    /// The caller must configure identity from its source, never infer it from Trade.
    pub fn new(
        config: PaperTradingConfig,
        telemetry: Option<TelemetryConfig>,
        events: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
    ) -> Result<(Self, mpsc::Sender<Trade>), backtest::BacktestError> {
        let capacity = config.channel_capacity.max(1);
        let session = PaperTradingSession::new(config)?;
        let (input, trades) = mpsc::channel(capacity);
        Ok((
            Self {
                session,
                trades,
                telemetry: telemetry.unwrap_or(TelemetryConfig {
                    symbol: String::new(),
                    strategy_id: String::new(),
                }),
                events,
            },
            input,
        ))
    }

    /// Waits for supplied trades, forwarding session events to the web projection.
    pub async fn run(mut self) -> (BacktestMetrics, Vec<ClosedTrade>) {
        while let Some(trade) = self.trades.recv().await {
            for event in self.session.process_trade(&trade) {
                let _ = self
                    .events
                    .send(self.telemetry.envelope(trade.timestamp, event));
            }
        }
        // Consuming finish guarantees one finalization. No mark means no forced
        // liquidation; finalization itself does not publish a telemetry event.
        self.session.finish(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use domain::Side;
    use rust_decimal_macros::dec;
    use std::path::PathBuf;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn shutdown_drains_input_joins_and_returns_finalized_metrics() {
        let (events, _) = broadcast::channel(16);
        let mut rx = events.subscribe();
        let (owner, input) = PaperSessionOwner::new(
            PaperTradingConfig {
                dollar_bar_threshold: dec!(1000),
                channel_capacity: 2,
                ..Default::default()
            },
            None,
            events,
        )
        .unwrap();
        // Queue before spawning: shutdown must drain, not abort the task.
        input
            .try_send(Trade::new(42, dec!(100), dec!(11), Side::Buy).unwrap())
            .unwrap();
        let runtime = PaperSessionRuntime::spawn(owner, input);
        let (metrics, trades) = timeout(Duration::from_secs(1), runtime.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metrics.total_trades, 0);
        assert!(trades.is_empty());
        let event = rx.try_recv().unwrap();
        assert_eq!(event.timestamp, Some(42));
        assert!(matches!(event.payload, PaperTradingEvent::BarFormed(_)));
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn shutdown_does_not_liquidate_an_open_position_or_emit_a_final_event() {
        let (events, _) = broadcast::channel(16);
        let mut rx = events.subscribe();
        let mut config = PaperTradingConfig {
            dollar_bar_threshold: dec!(100),
            ..Default::default()
        };
        config.strategy.rolling_window_len = 4;
        config.strategy.cusum_vol_multiplier = dec!(1);
        config.strategy.z_entry_threshold = dec!(1.5);
        config.strategy.z_stop_threshold = dec!(3);
        config.strategy.time_barrier_bars = 10;
        let (mut owner, input) = PaperSessionOwner::new(config, None, events).unwrap();
        for (i, price) in [dec!(100), dec!(101), dec!(99), dec!(100), dec!(90)]
            .into_iter()
            .enumerate()
        {
            owner.session.process_trade(
                &Trade::new((i as i64 + 1) * 1000, price, dec!(1.5), Side::Buy).unwrap(),
            );
        }
        assert!(owner.session.active_position().is_some());
        let runtime = PaperSessionRuntime::spawn(owner, input);
        let (metrics, trades) = timeout(Duration::from_secs(1), runtime.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metrics.total_trades, 0);
        assert!(trades.is_empty());
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn shutdown_joins_a_session_waiting_without_a_source() {
        let (events, _) = broadcast::channel(16);
        let mut rx = events.subscribe();
        let (owner, input) =
            PaperSessionOwner::new(PaperTradingConfig::default(), None, events).unwrap();
        let runtime = PaperSessionRuntime::spawn(owner, input);
        let (metrics, trades) = timeout(Duration::from_secs(1), runtime.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metrics.total_trades, 0);
        assert!(trades.is_empty());
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn input_is_bounded_and_unknown_identity_is_not_invented() {
        let (events, _) = broadcast::channel(16);
        let mut rx = events.subscribe();
        let (owner, input) = PaperSessionOwner::new(
            PaperTradingConfig {
                dollar_bar_threshold: dec!(1000),
                channel_capacity: 1,
                ..Default::default()
            },
            None,
            events,
        )
        .unwrap();
        let trade = Trade::new(42, dec!(100), dec!(5), Side::Buy).unwrap();
        input.try_send(trade.clone()).unwrap();
        assert!(matches!(
            input.try_send(trade.clone()),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        let task = tokio::spawn(owner.run());
        input.send(trade).await.unwrap();
        drop(input);
        task.await.unwrap();
        let event = rx.try_recv().unwrap();
        assert_eq!(event.timestamp, Some(42));
        assert!(event.symbol.is_empty());
        assert!(event.strategy_id.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn supplied_trades_reach_paper_events_and_web_projection() {
        let (events, _) = broadcast::channel(16);
        let mut rx = events.subscribe();
        let state = AppState::new(events.clone(), PathBuf::new());
        let identity = TelemetryConfig {
            symbol: "FIXTURE-USD".into(),
            strategy_id: "fixture-cusum-session".into(),
        };
        let (owner, input) = PaperSessionOwner::new(
            PaperTradingConfig {
                dollar_bar_threshold: dec!(1000),
                channel_capacity: 2,
                ..Default::default()
            },
            Some(identity.clone()),
            events,
        )
        .unwrap();
        let task = tokio::spawn(owner.run());
        assert!(rx.try_recv().is_err());
        assert_eq!(state.state.read().await.timestamp, None);
        input
            .send(Trade::new(1_700_000_000_000, dec!(100), dec!(5), Side::Buy).unwrap())
            .await
            .unwrap();
        input
            .send(Trade::new(1_700_000_001_000, dec!(100), dec!(6), Side::Sell).unwrap())
            .await
            .unwrap();
        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.timestamp, Some(1_700_000_001_000));
        assert_eq!(event.symbol, identity.symbol);
        assert_eq!(event.strategy_id, identity.strategy_id);
        match event.payload {
            PaperTradingEvent::BarFormed(bar) => {
                assert_eq!(bar.trade_count, 2);
                assert_eq!(bar.volume, dec!(11));
                assert_eq!(bar.dollar_volume, dec!(1100));
            }
            other => panic!("expected session-produced bar, got {other:?}"),
        }
        timeout(Duration::from_secs(1), async {
            loop {
                if state.state.read().await.timestamp == event.timestamp {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let snapshot = state.state.read().await;
        assert_eq!(snapshot.active_symbol, identity.symbol);
        assert_eq!(snapshot.active_strategy, identity.strategy_id);
        assert_eq!(snapshot.last_price, Some(dec!(100)));
        drop(snapshot);
        drop(input);
        task.await.unwrap();
    }
}
