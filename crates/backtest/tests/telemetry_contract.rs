use std::collections::VecDeque;

use backtest::{PaperTradingConfig, PaperTradingEvent, PaperTradingSession, TelemetryConfig};
use domain::{DollarBar, OrderIntent, Side, Trade};
use rust_decimal_macros::dec;
use strategies::{MarketType, Strategy};

/// Four warm-up bars pass; the fifth, the plunge to 90, signals a long with a stop at 85 and a
/// target at 95, so the tape below fills, crosses the target and exits.
struct PlungeLong {
    decisions: VecDeque<Option<OrderIntent>>,
}

impl Strategy for PlungeLong {
    fn name(&self) -> &str {
        "plunge_long"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn on_bar(&mut self, _bar: &DollarBar) -> Option<OrderIntent> {
        self.decisions.pop_front().flatten()
    }

    fn reset(&mut self) {}
}

#[tokio::test]
async fn configured_session_preserves_raw_events_and_market_metadata() {
    let config = PaperTradingConfig {
        dollar_bar_threshold: dec!(100),
        latency_ms: 0,
        ..Default::default()
    };
    let metadata = TelemetryConfig {
        symbol: "ETHUSDT".into(),
        strategy_id: "session-42".into(),
    };
    let signal = OrderIntent::new(1, Side::Buy, dec!(90), dec!(85), dec!(95), 10).unwrap();
    let mut decisions: VecDeque<Option<OrderIntent>> = vec![None; 4].into();
    decisions.push_back(Some(signal));
    let mut session = PaperTradingSession::new_with_telemetry(
        config,
        Box::new(PlungeLong { decisions }),
        metadata,
    )
    .unwrap();
    let mut raw = session.subscribe();
    let mut telemetry = session.subscribe_telemetry().unwrap();
    let mut kinds = std::collections::BTreeSet::new();
    for (timestamp, price, quantity, side) in [
        (1000, dec!(100), dec!(1), Side::Buy),
        (2000, dec!(101), dec!(1), Side::Buy),
        (3000, dec!(99), dec!(1.1), Side::Buy),
        (4000, dec!(100), dec!(1), Side::Buy),
        (5000, dec!(90), dec!(1.5), Side::Sell), // signal: the market order needs a buyer
        (5200, dec!(90.5), dec!(1000), Side::Buy), // a buyer print fills the entry
        (5500, dec!(91), dec!(0.1), Side::Buy),
        (6000, dec!(101), dec!(1), Side::Buy), // the tape crosses the target
        (6100, dec!(100.9), dec!(1000), Side::Sell), // a seller takes the market exit
    ] {
        let trade = Trade::new(timestamp, price, quantity, side).unwrap();
        for event in session.process_trade(&trade).await {
            assert_eq!(raw.try_recv().unwrap(), event);
            let envelope = telemetry.try_recv().unwrap();
            assert_eq!(envelope.timestamp, Some(timestamp));
            assert_eq!(envelope.symbol, "ETHUSDT");
            assert_eq!(envelope.strategy_id, "session-42");
            assert_eq!(envelope.payload, event);
            kinds.insert(envelope.event_type.clone());
            let json = serde_json::to_value(&envelope).unwrap();
            assert_eq!(json.as_object().unwrap().len(), 5);
            assert_eq!(json["timestamp"], timestamp);
            assert_eq!(json["event_type"], event.event_type());
            assert_eq!(json["payload"], serde_json::to_value(&event).unwrap());
            assert!(json["payload"].get(event.event_type()).is_some());
        }
        assert!(telemetry.try_recv().is_err());
    }
    assert_eq!(
        kinds,
        [
            "BarFormed",
            "SignalGenerated",
            "PositionOpened",
            "MarkToMarket",
            "PositionClosed"
        ]
        .map(String::from)
        .into_iter()
        .collect()
    );
}

#[test]
fn unconfigured_session_does_not_invent_telemetry_metadata() {
    let silent = PlungeLong {
        decisions: VecDeque::new(),
    };
    let session =
        PaperTradingSession::with_strategy(PaperTradingConfig::default(), Box::new(silent))
            .unwrap();
    assert!(session.subscribe_telemetry().is_none());
    assert_eq!(
        PaperTradingEvent::PositionClosed {
            exit_reason: "StopLoss".into(),
            exit_price: dec!(1),
            net_pnl: dec!(-1),
            total_equity: dec!(9)
        }
        .event_type(),
        "PositionClosed"
    );
}
