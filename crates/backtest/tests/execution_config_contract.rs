use backtest::BacktestConfig;
use rust_decimal_macros::dec;

fn load(name: &str) -> BacktestConfig {
    let path = format!("{}/../../configs/{name}", env!("CARGO_MANIFEST_DIR"));
    let content = std::fs::read_to_string(path).expect("execution config file");
    serde_json::from_str(&content).expect("valid execution config JSON")
}

#[test]
fn the_500_eur_execution_config_is_valid_and_sets_explicit_caps_around_three_times_equity() {
    let config = load("execution_real_500eur.json");

    config.validate().expect("the shipped config must validate");
    assert!(config.has_explicit_notional_caps());
    assert_eq!(config.effective_notional_caps(), (dec!(1700), dec!(1700)));
    assert_eq!(config.initial_capital, dec!(562));
    assert_eq!(config.risk_per_trade_pct, dec!(0.005));
    assert_eq!(config.max_daily_drawdown_pct, dec!(0.03));
    assert_eq!(config.max_leverage, dec!(5));
    // Roughly 3x equity, as docs/capital-scaling.md explains: measured on the file's own caps,
    // so a cap edited without the capital (or the reverse) fails here.
    let (order_cap, position_cap) = config.effective_notional_caps();
    for cap in [order_cap, position_cap] {
        let multiple = cap / config.initial_capital;
        assert!(
            (dec!(2.9)..=dec!(3.1)).contains(&multiple),
            "cap {cap} is {multiple}x the equity"
        );
    }
}

#[test]
fn the_200_usdt_execution_config_keeps_the_policy_default_caps() {
    let config = load("execution_daytrading_200.json");

    config
        .validate()
        .expect("the legacy config must still validate");
    assert!(!config.has_explicit_notional_caps());
    let defaults = domain::RiskPolicy::default();
    assert_eq!(
        config.effective_notional_caps(),
        (defaults.max_order_notional, defaults.max_position_notional)
    );
}
