use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use strategies::MarketType;

use crate::config::BacktestConfig;
use crate::run::RunSpec;

/// Data sampling specification required by the strategy (AFML Ch. 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SamplingConfig {
    /// Information-driven volume in quote currency (USDT) to form a Dollar Bar.
    DollarBar { threshold: Decimal },
}

impl SamplingConfig {
    /// Returns the dollar bar threshold if this is a DollarBar sampling configuration.
    pub fn dollar_bar_threshold(&self) -> Option<Decimal> {
        match self {
            Self::DollarBar { threshold } => Some(*threshold),
        }
    }
}

/// Evaluation scorecard and anti-overfitting metrics from the HPO run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HpoScorecard {
    /// Mean In-Sample Sortino Ratio across all validation folds.
    pub mean_is_sortino: Decimal,
    /// Mean Out-of-Sample Sortino Ratio across all validation folds.
    pub mean_oos_sortino: Decimal,
    /// Out-of-Sample profit factor pooled over all folds; `None` without out-of-sample losses.
    pub mean_oos_profit_factor: Option<Decimal>,
    /// Per-trade Sharpe ratio of all out-of-sample trades.
    #[serde(default)]
    pub oos_sharpe: Option<Decimal>,
    /// Total out-of-sample trades across all folds.
    #[serde(default)]
    pub total_oos_trades: usize,
    /// Fewest closed trades in any in-sample or out-of-sample fold.
    #[serde(default)]
    pub min_fold_trades: usize,
    /// Share of the performance the grid neighbours keep [0.0 .. 1.0].
    pub stability_score: Decimal,
    /// Composite fitness score: `max(0, mean_oos_sortino) * stability_score`.
    pub fitness: Decimal,
    /// Deflated Sharpe Ratio p-value (Bailey and Lopez de Prado, 2014).
    pub dsr_p_value: Decimal,
    /// Whether the configuration passed the statistical significance threshold (p < 0.05).
    pub is_statistically_significant: bool,
    /// Total trials evaluated during the optimization sweep.
    pub trials_evaluated: usize,
}

/// Canonical, authoritative deployment manifest for an optimized trading strategy.
///
/// Encapsulates the complete quantitative environment required to faithfully
/// reproduce the strategy's statistical edge in backtesting, paper trading, and live execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyDeploymentArtifact<C> {
    /// Schema format version for backwards/forwards compatibility (e.g. "1.0").
    pub schema_version: String,
    /// Strategy, market, sampling, and friction specification, serialized
    /// flat for compatibility with existing artifact files.
    #[serde(flatten)]
    pub spec: RunSpec<C>,
    /// Statistical audit metrics from the HPO run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hpo_scorecard: Option<HpoScorecard>,
}

impl<C> StrategyDeploymentArtifact<C> {
    /// Convenience helper returning the dollar bar threshold if configured.
    pub fn dollar_bar_threshold(&self) -> Option<Decimal> {
        self.spec.sampling.dollar_bar_threshold()
    }
}

/// Result of loading a strategy configuration file, supporting both the
/// comprehensive [`StrategyDeploymentArtifact`] and legacy bare config JSONs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedStrategyConfig<C> {
    /// Authoritative deployment artifact with sampling, market, and context specifications.
    Artifact(Box<StrategyDeploymentArtifact<C>>),
    /// Legacy raw hyperparameter configuration without deployment metadata.
    Bare(C),
}

impl<C> LoadedStrategyConfig<C> {
    /// Returns a reference to the strategy hyperparameters.
    pub fn strategy_config(&self) -> &C {
        match self {
            Self::Artifact(a) => &a.spec.strategy_config,
            Self::Bare(c) => c,
        }
    }

    /// Consumes the wrapper and returns the strategy hyperparameters.
    pub fn into_strategy_config(self) -> C {
        match self {
            Self::Artifact(a) => a.spec.strategy_config,
            Self::Bare(c) => c,
        }
    }

    /// Returns the sampling dollar bar threshold if specified in the artifact.
    pub fn dollar_bar_threshold(&self) -> Option<Decimal> {
        match self {
            Self::Artifact(a) => a.dollar_bar_threshold(),
            Self::Bare(_) => None,
        }
    }

    /// Returns the canonical strategy ID if specified in the artifact.
    pub fn strategy_id(&self) -> Option<&str> {
        match self {
            Self::Artifact(a) => Some(&a.spec.strategy_id),
            Self::Bare(_) => None,
        }
    }

    /// Returns the target symbol if specified in the artifact.
    pub fn symbol(&self) -> Option<&str> {
        match self {
            Self::Artifact(a) => Some(&a.spec.symbol),
            Self::Bare(_) => None,
        }
    }

    /// Returns the market type if specified in the artifact.
    pub fn market_type(&self) -> Option<MarketType> {
        match self {
            Self::Artifact(a) => Some(a.spec.market_type),
            Self::Bare(_) => None,
        }
    }

    /// Returns the execution context if specified in the artifact.
    pub fn execution_context(&self) -> Option<&BacktestConfig> {
        match self {
            Self::Artifact(a) => Some(&a.spec.execution_context),
            Self::Bare(_) => None,
        }
    }

    /// Returns the inner artifact reference if this was loaded from a deployment manifest.
    pub fn artifact(&self) -> Option<&StrategyDeploymentArtifact<C>> {
        match self {
            Self::Artifact(a) => Some(a.as_ref()),
            Self::Bare(_) => None,
        }
    }
}

/// Loads a strategy configuration from a file path, transparently handling both
/// authoritative [`StrategyDeploymentArtifact`] files and legacy bare JSON files.
pub fn load_strategy_config<C: DeserializeOwned>(path: &Path) -> Result<LoadedStrategyConfig<C>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read configuration file: {}", path.display()))?;

    // Try parsing as comprehensive StrategyDeploymentArtifact first
    if let Ok(artifact) = serde_json::from_str::<StrategyDeploymentArtifact<C>>(&content) {
        return Ok(LoadedStrategyConfig::Artifact(Box::new(artifact)));
    }

    // Fall back to legacy bare configuration
    let bare = serde_json::from_str::<C>(&content).with_context(|| {
        format!(
            "Failed to parse configuration file as artifact or bare config: {}",
            path.display()
        )
    })?;
    Ok(LoadedStrategyConfig::Bare(bare))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use strategies::DollarBarsCusumConfig;

    #[test]
    fn artifact_round_trip_serialization() {
        let config = DollarBarsCusumConfig {
            rolling_window_len: 20,
            cusum_vol_multiplier: dec!(2.0),
            z_entry_threshold: dec!(1.8),
            z_stop_threshold: dec!(3.5),
            time_barrier_bars: 15,
            min_take_profit_pct: dec!(0.0025),
        };

        let artifact = StrategyDeploymentArtifact {
            schema_version: "1.0".to_string(),
            spec: RunSpec {
                strategy_id: "dollar_bars_cusum".to_string(),
                symbol: "BTCUSDT".to_string(),
                market_type: MarketType::UsdMPerpetual,
                sampling: SamplingConfig::DollarBar {
                    threshold: dec!(10_000_000),
                },
                strategy_config: config.clone(),
                execution_context: BacktestConfig::default(),
            },
            hpo_scorecard: Some(HpoScorecard {
                mean_is_sortino: dec!(2.45),
                mean_oos_sortino: dec!(2.10),
                mean_oos_profit_factor: Some(dec!(1.80)),
                oos_sharpe: None,
                total_oos_trades: 0,
                min_fold_trades: 0,
                stability_score: dec!(0.92),
                fitness: dec!(2.25),
                dsr_p_value: dec!(0.012),
                is_statistically_significant: true,
                trials_evaluated: 60,
            }),
        };

        let json = serde_json::to_string_pretty(&artifact).unwrap();
        let parsed: StrategyDeploymentArtifact<DollarBarsCusumConfig> =
            serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.spec.strategy_id, "dollar_bars_cusum");
        assert_eq!(parsed.dollar_bar_threshold(), Some(dec!(10_000_000)));
        assert_eq!(parsed.spec.strategy_config, config);
        assert_eq!(parsed.spec.market_type, MarketType::UsdMPerpetual);
    }

    #[test]
    fn load_strategy_config_parses_artifact_and_bare() {
        let temp_dir = std::env::temp_dir();
        let artifact_path = temp_dir.join("test_artifact.json");
        let bare_path = temp_dir.join("test_bare.json");

        let config = DollarBarsCusumConfig {
            rolling_window_len: 20,
            cusum_vol_multiplier: dec!(2.0),
            z_entry_threshold: dec!(1.8),
            z_stop_threshold: dec!(3.5),
            time_barrier_bars: 15,
            min_take_profit_pct: dec!(0.0025),
        };

        let artifact = StrategyDeploymentArtifact {
            schema_version: "1.0".to_string(),
            spec: RunSpec {
                strategy_id: "dollar_bars_cusum".to_string(),
                symbol: "BTCUSDT".to_string(),
                market_type: MarketType::UsdMPerpetual,
                sampling: SamplingConfig::DollarBar {
                    threshold: dec!(1_000_000),
                },
                strategy_config: config.clone(),
                execution_context: BacktestConfig::default(),
            },
            hpo_scorecard: None,
        };

        fs::write(&artifact_path, serde_json::to_string(&artifact).unwrap()).unwrap();
        fs::write(&bare_path, serde_json::to_string(&config).unwrap()).unwrap();

        let loaded_artifact =
            load_strategy_config::<DollarBarsCusumConfig>(&artifact_path).unwrap();
        assert!(matches!(loaded_artifact, LoadedStrategyConfig::Artifact(_)));
        assert_eq!(
            loaded_artifact.dollar_bar_threshold(),
            Some(dec!(1_000_000))
        );
        assert_eq!(loaded_artifact.strategy_id(), Some("dollar_bars_cusum"));
        assert_eq!(
            loaded_artifact.market_type(),
            Some(MarketType::UsdMPerpetual)
        );

        let loaded_bare = load_strategy_config::<DollarBarsCusumConfig>(&bare_path).unwrap();
        assert!(matches!(loaded_bare, LoadedStrategyConfig::Bare(_)));
        assert_eq!(loaded_bare.dollar_bar_threshold(), None);
        assert_eq!(loaded_bare.strategy_config(), &config);

        let _ = fs::remove_file(artifact_path);
        let _ = fs::remove_file(bare_path);
    }
}
