use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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
    /// Raw trades, the input of a trade-consuming strategy (strategy spec §11.1): grouped into
    /// `price_bucket_ms` price events (HPO), or one price event per trade when it is `None`
    /// (the bar-fill backtest at trade resolution).
    Trades {
        /// Width of the price buckets in milliseconds; `None` at trade resolution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        price_bucket_ms: Option<i64>,
    },
}

impl SamplingConfig {
    /// Returns the dollar bar threshold if this is a DollarBar sampling configuration.
    pub fn dollar_bar_threshold(&self) -> Option<Decimal> {
        match self {
            Self::DollarBar { threshold } => Some(*threshold),
            Self::Trades { .. } => None,
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
    #[serde(rename = "mean_oos_profit_factor")]
    pub oos_profit_factor: Option<Decimal>,
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

/// `min_fold_trades` of a scorecard whose search recorded no fold with enough trades. Scorecards
/// written before the minimum-trades rule lack the field, so it deserializes to this value;
/// a search run with `--min-trades-per-fold 0` can also produce it.
pub const NO_ELIGIBLE_FOLD_TRADES: usize = 0;

impl HpoScorecard {
    /// Why paper and live trading must not run this artifact, or `None` when its search
    /// measured a deployable edge: eligible folds, a positive out-of-sample fitness and a
    /// significant Deflated Sharpe Ratio.
    pub fn deployment_blocker(&self) -> Option<String> {
        if self.min_fold_trades == NO_ELIGIBLE_FOLD_TRADES {
            return Some(
                "the scorecard records no fold with enough trades: either it predates the \
                 minimum-trades rule (its ratios may be the old 100 cap) or the search ran with \
                 --min-trades-per-fold 0; re-run the HPO with the minimum"
                    .to_string(),
            );
        }
        if self.fitness <= Decimal::ZERO {
            return Some(format!(
                "no positive out-of-sample edge (fitness {:.4}, mean OOS Sortino {:.4})",
                self.fitness, self.mean_oos_sortino
            ));
        }
        // The p-value is the evidence; the stored flag alone could contradict it.
        if !self.is_statistically_significant || self.dsr_p_value >= dec!(0.05) {
            return Some(format!(
                "Deflated Sharpe p = {:.4} >= 0.05: the edge is indistinguishable from the luck \
                 of picking the best of {} trials",
                self.dsr_p_value, self.trials_evaluated
            ));
        }
        None
    }
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

/// The deployment artifact `run_hpo` writes: a [`StrategyDeploymentArtifact`] plus the shared
/// configuration the whole run was frozen with (strategy spec §15.1).
///
/// `shared_config` is omitted when the strategy has none (its shared configuration is `()`), so
/// such artifacts are byte-identical to a plain [`StrategyDeploymentArtifact`], and artifacts
/// written before the field existed load with `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HpoDeploymentArtifact<C> {
    /// The artifact, serialized flat.
    #[serde(flatten)]
    pub artifact: StrategyDeploymentArtifact<C>,
    /// The frozen shared configuration of the run, as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_config: Option<serde_json::Value>,
}

/// The `shared_config` an artifact file stores, or `None` when it stores none (or `null`).
pub fn load_artifact_shared_config(path: &Path) -> Result<Option<serde_json::Value>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read configuration file: {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse configuration file: {}", path.display()))?;
    Ok(value
        .get("shared_config")
        .filter(|shared| !shared.is_null())
        .cloned())
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

    fn scorecard() -> HpoScorecard {
        HpoScorecard {
            mean_is_sortino: dec!(0.21),
            mean_oos_sortino: dec!(0.14),
            oos_profit_factor: Some(dec!(1.3)),
            oos_sharpe: Some(dec!(0.08)),
            total_oos_trades: 412,
            min_fold_trades: 37,
            stability_score: dec!(0.78),
            fitness: dec!(0.109),
            dsr_p_value: dec!(0.012),
            is_statistically_significant: true,
            trials_evaluated: 120,
        }
    }

    #[test]
    fn the_pooled_profit_factor_keeps_its_serialized_name() {
        // Artifacts and the BI contract read it as `mean_oos_profit_factor`.
        let json = serde_json::to_value(scorecard()).unwrap();

        assert_eq!(json["mean_oos_profit_factor"], serde_json::json!("1.3"));
        assert!(json.get("oos_profit_factor").is_none());
    }

    #[test]
    fn a_profitable_and_significant_scorecard_is_deployable() {
        assert_eq!(scorecard().deployment_blocker(), None);
    }

    #[test]
    fn a_scorecard_without_an_out_of_sample_edge_is_not_deployable() {
        let losing = HpoScorecard {
            fitness: Decimal::ZERO,
            mean_oos_sortino: dec!(-0.32),
            ..scorecard()
        };

        let reason = losing.deployment_blocker().expect("blocked");
        assert!(
            reason.contains("no positive out-of-sample edge"),
            "{reason}"
        );
        assert!(reason.contains("-0.32"), "{reason}");
    }

    #[test]
    fn a_scorecard_whose_edge_may_be_selection_luck_is_not_deployable() {
        let lucky = HpoScorecard {
            dsr_p_value: dec!(0.31),
            is_statistically_significant: false,
            ..scorecard()
        };

        let reason = lucky.deployment_blocker().expect("blocked");
        assert!(reason.contains("Deflated Sharpe p = 0.3100"), "{reason}");
    }

    #[test]
    fn the_significance_boundary_is_strict_at_five_percent() {
        let at = |p| HpoScorecard {
            dsr_p_value: p,
            is_statistically_significant: p < dec!(0.05),
            ..scorecard()
        };

        assert!(at(dec!(0.05)).deployment_blocker().is_some());
        assert_eq!(at(dec!(0.0499)).deployment_blocker(), None);
    }

    #[test]
    fn a_significance_flag_that_contradicts_the_p_value_does_not_pass_the_gate() {
        let inconsistent = HpoScorecard {
            dsr_p_value: dec!(0.31),
            is_statistically_significant: true,
            ..scorecard()
        };

        let reason = inconsistent.deployment_blocker().expect("blocked");
        assert!(reason.contains("Deflated Sharpe p = 0.3100"), "{reason}");
    }

    #[test]
    fn a_scorecard_from_before_the_minimum_trades_rule_is_not_deployable() {
        // The legacy scoring capped Sortino at 100 and called it significant (fitness 80).
        let legacy: HpoScorecard = serde_json::from_value(serde_json::json!({
            "mean_is_sortino": "80", "mean_oos_sortino": "20", "mean_oos_profit_factor": "20",
            "stability_score": "1", "fitness": "80", "dsr_p_value": "0.0000",
            "is_statistically_significant": true, "trials_evaluated": 720
        }))
        .unwrap();

        let reason = legacy.deployment_blocker().expect("blocked");
        assert!(reason.contains("re-run the HPO"), "{reason}");
    }

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
                oos_profit_factor: Some(dec!(1.80)),
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

    fn cusum_artifact() -> StrategyDeploymentArtifact<DollarBarsCusumConfig> {
        StrategyDeploymentArtifact {
            schema_version: "1.0".to_string(),
            spec: RunSpec {
                strategy_id: "dollar_bars_cusum".to_string(),
                symbol: "BTCUSDT".to_string(),
                market_type: MarketType::UsdMPerpetual,
                sampling: SamplingConfig::DollarBar {
                    threshold: dec!(1_000_000),
                },
                strategy_config: DollarBarsCusumConfig::default(),
                execution_context: BacktestConfig::default(),
            },
            hpo_scorecard: Some(scorecard()),
        }
    }

    #[test]
    fn trade_sampling_round_trips_and_declares_no_dollar_bar() {
        let sampling = SamplingConfig::Trades {
            price_bucket_ms: Some(1_000),
        };
        let json = serde_json::to_value(sampling).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "trades", "price_bucket_ms": 1000})
        );
        assert_eq!(
            serde_json::from_value::<SamplingConfig>(json).unwrap(),
            sampling
        );
        assert_eq!(sampling.dollar_bar_threshold(), None);

        // Trade resolution (one price event per trade) has no bucket.
        let per_trade = SamplingConfig::Trades {
            price_bucket_ms: None,
        };
        let json = serde_json::to_value(per_trade).unwrap();
        assert_eq!(json, serde_json::json!({"type": "trades"}));
        assert_eq!(
            serde_json::from_value::<SamplingConfig>(json).unwrap(),
            per_trade
        );
    }

    #[test]
    fn an_hpo_artifact_without_a_shared_config_is_byte_identical_to_the_plain_artifact() {
        let plain = cusum_artifact();
        let hpo = HpoDeploymentArtifact {
            artifact: plain.clone(),
            shared_config: None,
        };

        assert_eq!(
            serde_json::to_string_pretty(&hpo).unwrap(),
            serde_json::to_string_pretty(&plain).unwrap()
        );
    }

    #[test]
    fn an_hpo_artifact_stores_its_shared_config_and_round_trips() {
        let shared = serde_json::json!({"genome_seed": 123456, "calibration_end_ms": 42});
        let hpo = HpoDeploymentArtifact {
            artifact: cusum_artifact(),
            shared_config: Some(shared.clone()),
        };
        let json = serde_json::to_string_pretty(&hpo).unwrap();

        let parsed: HpoDeploymentArtifact<DollarBarsCusumConfig> =
            serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, hpo);
        // The plain artifact type still reads it, ignoring the new key.
        let plain: StrategyDeploymentArtifact<DollarBarsCusumConfig> =
            serde_json::from_str(&json).unwrap();
        assert_eq!(plain, cusum_artifact());

        let path =
            std::env::temp_dir().join(format!("atsnt-{}-shared-artifact.json", std::process::id()));
        fs::write(&path, &json).unwrap();
        assert_eq!(load_artifact_shared_config(&path).unwrap(), Some(shared));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn an_old_artifact_without_the_new_fields_still_loads() {
        let old = serde_json::to_string(&cusum_artifact()).unwrap();
        assert!(!old.contains("shared_config"));

        let parsed: HpoDeploymentArtifact<DollarBarsCusumConfig> =
            serde_json::from_str(&old).unwrap();
        assert_eq!(parsed.shared_config, None);
        assert_eq!(parsed.artifact, cusum_artifact());

        let path =
            std::env::temp_dir().join(format!("atsnt-{}-old-artifact.json", std::process::id()));
        fs::write(&path, &old).unwrap();
        assert_eq!(load_artifact_shared_config(&path).unwrap(), None);
        let _ = fs::remove_file(path);
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
