use backtest::{
    load_strategy_config, BacktestConfig, LoadedStrategyConfig, RunId, RunSpec, RunType,
    SamplingConfig, StrategyDeploymentArtifact,
};
use rust_decimal_macros::dec;
use strategies::{DollarBarsCusumConfig, MarketType};

fn sample_spec() -> RunSpec<DollarBarsCusumConfig> {
    RunSpec {
        strategy_id: "dollar_bars_cusum".to_string(),
        symbol: "BTCUSDT".to_string(),
        market_type: MarketType::UsdMPerpetual,
        sampling: SamplingConfig::DollarBar {
            threshold: dec!(1_000_000),
        },
        strategy_config: DollarBarsCusumConfig::default(),
        execution_context: BacktestConfig::default(),
    }
}

#[test]
fn deployed_artifact_json_still_loads_through_run_spec() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../configs/hpo_btcusdt_2026_1k.json");
    let loaded = load_strategy_config::<DollarBarsCusumConfig>(&path).unwrap();
    let LoadedStrategyConfig::Artifact(artifact) = loaded else {
        panic!("deployed artifact must parse as an artifact, not a bare config");
    };
    assert_eq!(artifact.spec.strategy_id, "dollar_bars_cusum");
    assert_eq!(artifact.spec.symbol, "BTCUSDT");
    assert_eq!(artifact.spec.market_type, MarketType::UsdMPerpetual);
    assert_eq!(artifact.dollar_bar_threshold(), Some(dec!(1_000_000)));
    assert_eq!(artifact.spec.strategy_config.rolling_window_len, 25);
    assert_eq!(artifact.spec.execution_context.taker_fee_pct, dec!(0.0005));
    assert!(artifact.hpo_scorecard.is_some());
}

#[test]
fn artifact_serializes_with_flat_legacy_layout() {
    let artifact = StrategyDeploymentArtifact {
        schema_version: "1.0".to_string(),
        spec: sample_spec(),
        hpo_scorecard: None,
    };
    let value = serde_json::to_value(&artifact).unwrap();
    assert_eq!(value["strategy_id"], "dollar_bars_cusum");
    assert_eq!(value["sampling"]["type"], "dollar_bar");
    assert_eq!(value["execution_context"]["taker_fee_pct"], "0.0005");
    assert!(value.get("spec").is_none());
}

#[test]
fn run_spec_exposes_strategy_params_as_json() {
    let params = sample_spec().params_json().unwrap();
    assert_eq!(params["rolling_window_len"], 20);
    assert!(params.get("z_entry_threshold").is_some());
}

#[test]
fn run_type_uses_stable_snake_case_labels() {
    let labels: Vec<&str> = [
        RunType::Backtest,
        RunType::Hpo,
        RunType::HpoTrial,
        RunType::MonteCarlo,
        RunType::Paper,
        RunType::Live,
    ]
    .iter()
    .map(|t| t.as_str())
    .collect();
    assert_eq!(
        labels,
        [
            "backtest",
            "hpo",
            "hpo_trial",
            "monte_carlo",
            "paper",
            "live"
        ]
    );
    assert_eq!(
        serde_json::to_value(RunType::HpoTrial).unwrap(),
        "hpo_trial"
    );
}

#[test]
fn run_id_is_uuid_v7_formatted_and_time_ordered() {
    let early = RunId::from_parts(1_791_544_244_000, u128::MAX);
    let late = RunId::from_parts(1_791_544_244_001, 0);
    let text = early.as_str();
    assert_eq!(text.len(), 36);
    assert_eq!(&text[14..15], "7", "version nibble must be 7");
    assert!(
        matches!(&text[19..20], "8" | "9" | "a" | "b"),
        "RFC 4122 variant"
    );
    assert!(early.as_str() < late.as_str());
    assert_ne!(RunId::generate(), RunId::generate());
}
