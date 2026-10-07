use backtest::{PaperTradingConfig, PaperTradingEvent, PaperTradingSession, TelemetryConfig};
use domain::{Side, Trade};
use rust_decimal_macros::dec;
use strategies::DollarBarsCusumConfig;

#[test]
fn configured_session_preserves_raw_events_and_market_metadata() {
    let config = PaperTradingConfig {
        dollar_bar_threshold: dec!(100),
        strategy: DollarBarsCusumConfig {
            rolling_window_len: 4,
            cusum_vol_multiplier: dec!(1),
            z_entry_threshold: dec!(1.5),
            z_stop_threshold: dec!(3),
            time_barrier_bars: 10,
        },
        ..Default::default()
    };
    let metadata = TelemetryConfig {
        symbol: "ETHUSDT".into(),
        strategy_id: "cusum-session-42".into(),
    };
    let mut session = PaperTradingSession::new_with_telemetry(config, metadata).unwrap();
    let mut raw = session.subscribe();
    let mut telemetry = session.subscribe_telemetry().unwrap();
    let mut kinds = std::collections::BTreeSet::new();
    for (timestamp, price, quantity) in [
        (1000, dec!(100), dec!(1)),
        (2000, dec!(101), dec!(1)),
        (3000, dec!(99), dec!(1.1)),
        (4000, dec!(100), dec!(1)),
        (5000, dec!(90), dec!(1.5)),
        (5500, dec!(91), dec!(0.1)),
        (6000, dec!(101), dec!(1)),
    ] {
        let trade = Trade::new(timestamp, price, quantity, Side::Buy).unwrap();
        for event in session.process_trade(&trade) {
            assert_eq!(raw.try_recv().unwrap(), event);
            let envelope = telemetry.try_recv().unwrap();
            assert_eq!(envelope.timestamp, Some(timestamp));
            assert_eq!(envelope.symbol, "ETHUSDT");
            assert_eq!(envelope.strategy_id, "cusum-session-42");
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
    let session = PaperTradingSession::new(PaperTradingConfig::default()).unwrap();
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
