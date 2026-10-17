//! Strategy selection and loading for the runners.
//!
//! Every runner (backtest, HPO, paper, live) turns a strategy name and an optional configuration
//! file into a ready strategy through [`load_strategy`], which resolves the name in the
//! [`strategies`] registry. No runner matches on strategy names itself, so registering a new
//! strategy there makes it available everywhere.

use std::path::Path;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use serde::Serialize;
use strategies::{
    canonical_id, dispatch, unknown_strategy_message, MarketType, OptimizableStrategy, Strategy,
    StrategyVisitor,
};

use crate::artifact::{load_strategy_config, HpoScorecard, LoadedStrategyConfig};
use crate::config::BacktestConfig;

/// Strategy a runner uses when neither the artifact nor the command line names one.
pub const DEFAULT_STRATEGY_ID: &str = "dollar_bars_cusum";

/// Where a loaded strategy's configuration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// The strategy's own default hyperparameters; no file was given.
    Default,
    /// A legacy file holding only hyperparameters.
    Bare,
    /// An authoritative deployment artifact.
    Artifact {
        /// Artifact schema version.
        schema_version: String,
        /// HPO audit metrics recorded with the artifact, if any.
        scorecard: Option<HpoScorecard>,
    },
}

/// A strategy resolved from the registry together with what its configuration file declared.
pub struct LoadedStrategy {
    /// The strategy, ready for `on_bar`.
    pub strategy: Box<dyn Strategy>,
    /// Canonical id of the strategy.
    pub strategy_id: String,
    /// Hyperparameters as JSON (the form run specs and artifacts store).
    pub config_json: serde_json::Value,
    /// Hyperparameters as the strategy prints them.
    pub config_display: String,
    /// Where the configuration came from.
    pub source: ConfigSource,
    /// Dollar-bar threshold the artifact declared.
    pub dollar_bar_threshold: Option<Decimal>,
    /// Symbol the artifact declared.
    pub symbol: Option<String>,
    /// Market the artifact declared.
    pub market_type: Option<MarketType>,
    /// Execution context (fees, risk, margin) the artifact declared.
    pub execution_context: Option<BacktestConfig>,
}

impl LoadedStrategy {
    /// Human-readable lines describing the configuration source, for runner start-up output.
    pub fn report_lines(&self) -> Vec<String> {
        match &self.source {
            ConfigSource::Default => vec![format!(
                "Using default {} hyperparameters: {}",
                self.strategy_id, self.config_display
            )],
            ConfigSource::Bare => vec![
                "Legacy bare hyperparameter configuration loaded (no deployment manifest)"
                    .to_string(),
            ],
            ConfigSource::Artifact {
                schema_version,
                scorecard,
            } => {
                let mut lines = vec![format!(
                    "Authoritative Deployment Artifact loaded (schema v{schema_version})"
                )];
                if let Some(card) = scorecard {
                    lines.push(format!(
                        "HPO Scorecard: IS Sortino={:.2}, OOS Sortino={:.2}, Stability={:.2}, DSR p={:.4} ({})",
                        card.mean_is_sortino,
                        card.mean_oos_sortino,
                        card.stability_score,
                        card.dsr_p_value,
                        if card.is_statistically_significant {
                            "PASS"
                        } else {
                            "FAIL"
                        }
                    ));
                }
                lines
            }
        }
    }
}

/// Strategy id to use: the artifact's own id wins, then the command line, then the default.
/// Aliases resolve to the canonical id; an unregistered name is an error listing the registered
/// ones.
pub fn resolve_strategy_id(requested: Option<&str>, config_path: Option<&Path>) -> Result<String> {
    let from_artifact = match config_path {
        Some(path) => {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let value: serde_json::Value = serde_json::from_str(&content)
                .with_context(|| "Failed to parse configuration JSON")?;
            value
                .get("strategy_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }
        None => None,
    };
    let name = from_artifact
        .or_else(|| requested.map(str::to_string))
        .unwrap_or_else(|| DEFAULT_STRATEGY_ID.to_string());
    canonical_id(&name)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!(unknown_strategy_message(&name)))
}

struct Loader<'a> {
    config_path: Option<&'a Path>,
}

impl StrategyVisitor for Loader<'_> {
    type Output = Result<LoadedStrategy>;

    fn visit<S>(self) -> Self::Output
    where
        S: OptimizableStrategy + 'static,
        S::Config: Default,
        S::Space: Serialize + DeserializeOwned,
    {
        let (config, source, threshold, symbol, market_type, context) = match self.config_path {
            Some(path) => {
                let loaded = load_strategy_config::<S::Config>(path)?;
                let source = match &loaded {
                    LoadedStrategyConfig::Artifact(artifact) => ConfigSource::Artifact {
                        schema_version: artifact.schema_version.clone(),
                        scorecard: artifact.hpo_scorecard.clone(),
                    },
                    LoadedStrategyConfig::Bare(_) => ConfigSource::Bare,
                };
                let threshold = loaded.dollar_bar_threshold();
                let symbol = loaded.symbol().map(str::to_string);
                let market_type = loaded.market_type();
                let context = loaded.execution_context().copied();
                (
                    loaded.into_strategy_config(),
                    source,
                    threshold,
                    symbol,
                    market_type,
                    context,
                )
            }
            None => (
                S::Config::default(),
                ConfigSource::Default,
                None,
                None,
                None,
                None,
            ),
        };
        let config_json = serde_json::to_value(&config)?;
        let config_display = config.to_string();
        let strategy = S::from_config(config)
            .map_err(|error| anyhow::anyhow!("Failed to initialize strategy: {error}"))?;
        Ok(LoadedStrategy {
            strategy: Box::new(strategy),
            strategy_id: S::id().to_string(),
            config_json,
            config_display,
            source,
            dollar_bar_threshold: threshold,
            symbol,
            market_type,
            execution_context: context,
        })
    }
}

/// Builds the strategy registered under `name` (id or alias) from `config_path`, or from its
/// default hyperparameters when no file is given.
pub fn load_strategy(name: &str, config_path: Option<&Path>) -> Result<LoadedStrategy> {
    dispatch(name, Loader { config_path })
        .unwrap_or_else(|| Err(anyhow::anyhow!(unknown_strategy_message(name))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use strategies::{DollarBarsCusumConfig, OrderFlowImbalanceConfig};

    fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("atsnt-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn without_a_file_a_strategy_loads_its_own_default_hyperparameters() {
        let loaded = load_strategy("ofi", None).unwrap();

        assert_eq!(loaded.strategy_id, "order_flow_imbalance");
        assert_eq!(loaded.source, ConfigSource::Default);
        assert_eq!(
            loaded.config_json,
            serde_json::to_value(OrderFlowImbalanceConfig::default()).unwrap()
        );
        assert!(loaded.dollar_bar_threshold.is_none());
        assert!(loaded.report_lines()[0].contains("default order_flow_imbalance"));
    }

    #[test]
    fn an_artifact_supplies_the_declared_market_threshold_and_context() {
        let artifact = format!(
            r#"{{"schema_version":"1.0","strategy_id":"dollar_bars_cusum","symbol":"ETHUSDT","market_type":"usd_m_perpetual","sampling":{{"type":"dollar_bar","threshold":"750000"}},"strategy_config":{},"execution_context":{}}}"#,
            serde_json::to_string(&DollarBarsCusumConfig::default()).unwrap(),
            serde_json::to_string(&BacktestConfig::default()).unwrap(),
        );
        let path = temp_file("artifact.json", &artifact);

        let loaded = load_strategy("cusum", Some(&path)).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(loaded.strategy_id, "dollar_bars_cusum");
        assert_eq!(loaded.dollar_bar_threshold, Some(dec!(750_000)));
        assert_eq!(loaded.symbol.as_deref(), Some("ETHUSDT"));
        assert_eq!(loaded.market_type, Some(MarketType::UsdMPerpetual));
        assert_eq!(loaded.execution_context, Some(BacktestConfig::default()));
        assert!(matches!(loaded.source, ConfigSource::Artifact { .. }));
        assert!(loaded.report_lines()[0].contains("schema v1.0"));
    }

    #[test]
    fn a_bare_hyperparameter_file_still_loads() {
        let path = temp_file(
            "bare.json",
            &serde_json::to_string(&OrderFlowImbalanceConfig::default()).unwrap(),
        );

        let loaded = load_strategy("order_flow_imbalance", Some(&path)).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(loaded.source, ConfigSource::Bare);
        assert!(loaded.report_lines()[0].contains("Legacy bare"));
    }

    #[test]
    fn the_artifact_id_beats_the_command_line_and_aliases_resolve() {
        let path = temp_file(
            "ofi-artifact.json",
            r#"{"strategy_id":"ofi","schema_version":"1.0"}"#,
        );

        assert_eq!(
            resolve_strategy_id(Some("cusum"), Some(&path)).unwrap(),
            "order_flow_imbalance"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            resolve_strategy_id(Some("ofi"), None).unwrap(),
            "order_flow_imbalance"
        );
        assert_eq!(
            resolve_strategy_id(None, None).unwrap(),
            DEFAULT_STRATEGY_ID
        );
    }

    #[test]
    fn an_unknown_strategy_is_reported_with_the_registered_ones() {
        let by_name = load_strategy("nope", None).err().unwrap().to_string();
        let by_id = resolve_strategy_id(Some("nope"), None)
            .unwrap_err()
            .to_string();

        for message in [by_name, by_id] {
            assert!(message.contains("Unknown strategy 'nope'"));
            assert!(
                message.contains("dollar_bars_cusum") && message.contains("order_flow_imbalance")
            );
        }
    }

    #[test]
    fn a_file_for_another_strategy_is_rejected_instead_of_misread() {
        // Hyperparameters of the CUSUM strategy are not an order-flow configuration.
        let path = temp_file("wrong.json", r#"{"unrelated": true}"#);

        let result = load_strategy("order_flow_imbalance", Some(&path));
        std::fs::remove_file(&path).unwrap();

        assert!(result.is_err());
    }
}
