//! In-process paper session boundary; market-data sources are supplied by callers.

use backtest::{
    PaperTradingConfig, PaperTradingEvent, PaperTradingSession, TelemetryConfig, TelemetryEnvelope,
};
use domain::Trade;
use tokio::sync::{broadcast, mpsc};

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
    pub async fn run(mut self) {
        while let Some(trade) = self.trades.recv().await {
            for event in self.session.process_trade(&trade) {
                let _ = self
                    .events
                    .send(self.telemetry.envelope(trade.timestamp, event));
            }
        }
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
