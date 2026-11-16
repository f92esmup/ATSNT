//! Strategy selection and loading for the runners.
//!
//! Every runner (backtest, HPO, paper, live) turns a strategy name and an optional configuration
//! file into a ready strategy through [`build_strategy`], which resolves the name in the
//! [`strategies`] registry. No runner matches on strategy names itself, so registering a new
//! strategy there makes it available everywhere.

use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use serde::Serialize;
use strategies::{
    canonical_id, dispatch, unknown_strategy_message, MarketType, OptimizableStrategy, Strategy,
    StrategyVisitor,
};

use crate::artifact::{
    parse_artifact_shared_config, parse_strategy_config, HpoScorecard, LoadedStrategyConfig,
};
use crate::config::BacktestConfig;
use crate::error::{ConfigText, LoadError};

/// Strategy a runner uses when neither the artifact nor the command line names one.
pub const DEFAULT_STRATEGY_ID: &str = "swarm_consensus";

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

/// Whether start-up refuses a configuration whose HPO scorecard shows no deployable edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentPolicy {
    /// Refuse it (the default).
    Enforce,
    /// Run it anyway, with a warning (`--allow-unproven-strategy`).
    AllowUnproven,
}

impl DeploymentPolicy {
    /// The policy selected by the `--allow-unproven-strategy` flag.
    pub fn from_allow_unproven(allow_unproven: bool) -> Self {
        if allow_unproven {
            Self::AllowUnproven
        } else {
            Self::Enforce
        }
    }
}

/// Help of `--allow-unproven-strategy`, shared by the paper and live binaries.
pub const ALLOW_UNPROVEN_STRATEGY_HELP: &str = "Run an artifact whose HPO scorecard shows no \
deployable edge (fitness <= 0, a Deflated Sharpe p-value >= 0.05, or no fold with enough \
trades). Without it such an artifact is refused at start-up";

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
    /// The shared configuration the strategy was built with, as JSON; `None` for a strategy
    /// without one (its shared configuration is `()`).
    pub shared_config_json: Option<serde_json::Value>,
    /// The market the strategy narrowed its shared configuration for, when that changed it
    /// (the swarm drops short selling on spot).
    pub narrowed_for_market: Option<MarketType>,
}

impl LoadedStrategy {
    /// Why paper and live trading would refuse this configuration (see
    /// [`HpoScorecard::deployment_blocker`]), or `None`. Files without a scorecard
    /// (hand-written or bare configurations) carry no evidence either way and are not blocked.
    pub fn deployment_blocker(&self) -> Option<String> {
        match &self.source {
            ConfigSource::Artifact {
                scorecard: Some(card),
                ..
            } => card.deployment_blocker(),
            _ => None,
        }
    }

    /// Applies the deployment gate at start-up. A refused configuration is an error under
    /// [`DeploymentPolicy::Enforce`]. The returned notice, when there is one, is for the
    /// operator: the warning of an overridden refusal, or that the override changed nothing.
    pub fn ensure_deployable(&self, policy: DeploymentPolicy) -> Result<Option<String>, LoadError> {
        let blocker = self.deployment_blocker();
        match (policy, blocker) {
            (DeploymentPolicy::Enforce, None) => Ok(None),
            (DeploymentPolicy::Enforce, Some(reason)) => Err(LoadError::NotDeployable(reason)),
            (DeploymentPolicy::AllowUnproven, Some(reason)) => Ok(Some(format!(
                "[WARNING] Running an unproven strategy (--allow-unproven-strategy): {reason}"
            ))),
            (DeploymentPolicy::AllowUnproven, None) => {
                let why = match &self.source {
                    ConfigSource::Artifact {
                        scorecard: Some(_), ..
                    } => "the artifact passes the deployment gate",
                    _ => "the configuration has no HPO scorecard to gate",
                };
                Ok(Some(format!(
                    "[*] --allow-unproven-strategy has no effect: {why}"
                )))
            }
        }
    }

    /// Human-readable lines describing the configuration source, for runner start-up output.
    pub fn report_lines(&self) -> Vec<String> {
        let mut lines = self.source_lines();
        if let Some(market) = self.narrowed_for_market {
            lines.push(format!(
                "[*] Shared configuration narrowed for the {market:?} market"
            ));
        }
        lines
    }

    /// Lines describing where the configuration came from.
    fn source_lines(&self) -> Vec<String> {
        match &self.source {
            ConfigSource::Default => vec![format!(
                "[*] Using default {} hyperparameters: {}",
                self.strategy_id, self.config_display
            )],
            ConfigSource::Bare => vec![
                "[*] Legacy bare hyperparameter configuration loaded (no deployment manifest)"
                    .to_string(),
            ],
            ConfigSource::Artifact {
                schema_version,
                scorecard,
            } => {
                let mut lines = vec![format!(
                    "[*] Authoritative Deployment Artifact loaded (schema v{schema_version})"
                )];
                if let Some(card) = scorecard {
                    lines.push(format!(
                        "    • HPO Scorecard: IS Sortino={:.2}, OOS Sortino={:.2}, Stability={:.2}, DSR p={:.4} ({})",
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

/// Strategy id to use: the artifact's id, else the command line, else the default. Aliases
/// resolve to the canonical id; an unregistered name is an error listing the registered ones.
/// A command-line strategy that disagrees with the artifact's is an error too, never silently
/// ignored.
pub fn strategy_id_of(
    requested: Option<&str>,
    config: Option<ConfigText<'_>>,
) -> Result<String, LoadError> {
    let from_artifact = match config {
        Some(text) => {
            let value: serde_json::Value =
                serde_json::from_str(text.content).map_err(|error| LoadError::Parse {
                    what: "the configuration",
                    origin: text.origin.to_string(),
                    message: error.to_string(),
                })?;
            value
                .get("strategy_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }
        None => None,
    };
    let resolve = |name: &str| {
        canonical_id(name)
            .map(str::to_string)
            .ok_or_else(|| LoadError::UnknownStrategy(unknown_strategy_message(name)))
    };
    match (from_artifact, requested) {
        (Some(declared), Some(asked)) => {
            let (declared, asked) = (resolve(&declared)?, resolve(asked)?);
            if declared != asked {
                return Err(LoadError::Conflict(format!(
                    "--strategy {asked} conflicts with the {declared} strategy declared by the \
                     configuration file; drop the option or use a matching file"
                )));
            }
            Ok(declared)
        }
        (Some(declared), None) => resolve(&declared),
        (None, Some(asked)) => resolve(asked),
        (None, None) => resolve(DEFAULT_STRATEGY_ID),
    }
}

struct Loader<'a> {
    config: Option<ConfigText<'a>>,
    shared: Option<ConfigText<'a>>,
    market: Option<MarketType>,
}

/// The shared configuration of a load: the artifact's, else the `--shared-config` text's, else
/// the strategy's default. A text that contradicts the artifact's is refused.
fn resolve_shared_config<T: DeserializeOwned + Default>(
    artifact_shared: Option<serde_json::Value>,
    shared: Option<ConfigText<'_>>,
) -> Result<T, LoadError> {
    let (given, origin) = match shared {
        Some(text) => {
            let value: serde_json::Value =
                serde_json::from_str(text.content).map_err(|error| LoadError::Parse {
                    what: "the shared configuration",
                    origin: text.origin.to_string(),
                    message: error.to_string(),
                })?;
            (Some(value), text.origin)
        }
        None => (None, "the deployment artifact"),
    };
    let chosen = match (artifact_shared, given) {
        (Some(declared), Some(given)) if declared != given => {
            return Err(LoadError::Conflict(
                "--shared-config conflicts with the shared configuration stored in the \
                 deployment artifact; drop the option or use a matching file"
                    .to_string(),
            ))
        }
        (Some(declared), _) => Some(declared),
        (None, given) => given,
    };
    match chosen {
        Some(value) => serde_json::from_value(value).map_err(|error| LoadError::Parse {
            what: "the strategy's shared configuration",
            origin: origin.to_string(),
            message: error.to_string(),
        }),
        None => Ok(T::default()),
    }
}

impl StrategyVisitor for Loader<'_> {
    type Output = Result<LoadedStrategy, LoadError>;

    fn visit<S>(self) -> Self::Output
    where
        S: OptimizableStrategy + 'static,
        S::Config: Default,
        S::Space: Serialize + DeserializeOwned,
    {
        let mut artifact_shared = None;
        let (config, source, threshold, symbol, market_type, context) = match self.config {
            Some(text) => {
                let loaded = parse_strategy_config::<S::Config>(text)?;
                if matches!(loaded, LoadedStrategyConfig::Artifact(_)) {
                    artifact_shared = parse_artifact_shared_config(text)?;
                }
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
        let serialize = |what: &'static str| {
            move |error: serde_json::Error| LoadError::Serialize {
                what,
                message: error.to_string(),
            }
        };
        let config_json =
            serde_json::to_value(&config).map_err(serialize("the strategy configuration"))?;
        let config_display = config.to_string();
        let given: S::SharedConfig = resolve_shared_config(artifact_shared, self.shared)?;
        // The requested market wins over the artifact's; with neither, nothing is narrowed.
        let market = self.market.or(market_type);
        let given_json =
            serde_json::to_value(&given).map_err(serialize("the shared configuration"))?;
        let shared = match market {
            Some(market) => S::shared_for_market(given, market),
            None => given,
        };
        // `()` (a dollar-bar strategy) serializes to `null`: there is nothing to report.
        let shared_json =
            serde_json::to_value(&shared).map_err(serialize("the shared configuration"))?;
        let narrowed_for_market = market.filter(|_| shared_json != given_json);
        let shared_config_json = Some(shared_json).filter(|v| !v.is_null());
        let strategy = S::from_config(&shared, config)
            .map_err(|error| LoadError::Initialize(error.to_string()))?;
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
            shared_config_json,
            narrowed_for_market,
        })
    }
}

/// Builds the strategy registered under `name` (id or alias) from the text of its configuration,
/// or from its default hyperparameters when none is given.
///
/// The shared configuration is the one the deployment artifact stores, else `shared`, else the
/// strategy's default; a `shared` that contradicts the artifact's is an error. The strategy then
/// narrows it for `market`, or for the artifact's market when `market` is `None`.
pub fn build_strategy(
    name: &str,
    config: Option<ConfigText<'_>>,
    shared: Option<ConfigText<'_>>,
    market: Option<MarketType>,
) -> Result<LoadedStrategy, LoadError> {
    dispatch(
        name,
        Loader {
            config,
            shared,
            market,
        },
    )
    .unwrap_or_else(|| Err(LoadError::UnknownStrategy(unknown_strategy_message(name))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::{load_strategy, load_strategy_with_shared, resolve_strategy_id};

    fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("atsnt-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    /// A swarm artifact with its trade sampling, optionally carrying an HPO scorecard.
    fn artifact_with_scorecard(
        name: &str,
        scorecard: Option<serde_json::Value>,
    ) -> std::path::PathBuf {
        let mut artifact = serde_json::json!({
            "schema_version": "1.0",
            "strategy_id": "swarm_consensus",
            "symbol": "BTCUSDT",
            "market_type": "usd_m_perpetual",
            "sampling": {"type": "trades", "price_bucket_ms": 1000},
            "strategy_config": serde_json::to_value(strategies::PolicyConfig::default()).unwrap(),
            "execution_context": serde_json::to_value(crate::BacktestConfig::default()).unwrap(),
        });
        if let Some(card) = scorecard {
            artifact["hpo_scorecard"] = card;
        }
        temp_file(name, &artifact.to_string())
    }

    /// Shared with the binary-level gate tests.
    fn losing_scorecard() -> serde_json::Value {
        let artifact: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/losing_artifact.json")).unwrap();
        artifact["hpo_scorecard"].clone()
    }

    fn swarm_shared(calibration_end_ms: i64) -> strategies::SwarmConfig {
        strategies::SwarmConfig {
            calibration_end_ms,
            ..strategies::SwarmConfig::default()
        }
    }

    #[test]
    fn a_losing_artifact_is_refused_unless_explicitly_allowed() {
        let path = artifact_with_scorecard("losing.json", Some(losing_scorecard()));
        let loaded = load_strategy("swarm", Some(&path)).unwrap();

        let refused = loaded
            .ensure_deployable(DeploymentPolicy::Enforce)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("not deployable"), "{refused}");
        let notice = loaded
            .ensure_deployable(DeploymentPolicy::AllowUnproven)
            .unwrap()
            .expect("a warning");
        assert!(
            notice.starts_with("[WARNING] Running an unproven strategy"),
            "{notice}"
        );
    }

    #[test]
    fn a_file_without_a_scorecard_carries_no_verdict_and_is_not_blocked() {
        let artifact = artifact_with_scorecard("no-card.json", None);
        let bare = temp_file(
            "bare-gate.json",
            &serde_json::to_string(&strategies::PolicyConfig::default()).unwrap(),
        );

        for path in [artifact, bare] {
            let loaded = load_strategy("swarm", Some(&path)).unwrap();
            assert_eq!(
                loaded.ensure_deployable(DeploymentPolicy::Enforce).unwrap(),
                None
            );
        }
        let default = load_strategy("swarm", None).unwrap();
        assert_eq!(
            default
                .ensure_deployable(DeploymentPolicy::Enforce)
                .unwrap(),
            None
        );
    }

    #[test]
    fn an_override_that_changes_nothing_says_so() {
        let loaded = load_strategy("swarm", None).unwrap();

        let notice = loaded
            .ensure_deployable(DeploymentPolicy::AllowUnproven)
            .unwrap()
            .expect("a notice");
        assert!(
            notice.contains("--allow-unproven-strategy has no effect"),
            "{notice}"
        );
        assert!(notice.contains("no HPO scorecard"), "{notice}");
    }

    #[test]
    fn without_a_file_a_strategy_loads_its_own_default_hyperparameters() {
        let loaded = load_strategy("swarm", None).unwrap();

        assert_eq!(loaded.strategy_id, "swarm_consensus");
        assert_eq!(loaded.source, ConfigSource::Default);
        assert_eq!(
            loaded.config_json,
            serde_json::to_value(strategies::PolicyConfig::default()).unwrap()
        );
        assert!(loaded.dollar_bar_threshold.is_none());
        assert!(loaded.report_lines()[0].starts_with("[*] Using default swarm_consensus"));
    }

    #[test]
    fn an_artifact_supplies_its_symbol_market_and_execution_context() {
        let artifact = format!(
            r#"{{"schema_version":"1.0","strategy_id":"swarm_consensus","symbol":"ETHUSDT","market_type":"usd_m_perpetual","sampling":{{"type":"trades","price_bucket_ms":1000}},"strategy_config":{},"execution_context":{}}}"#,
            serde_json::to_string(&strategies::PolicyConfig::default()).unwrap(),
            serde_json::to_string(&BacktestConfig::default()).unwrap(),
        );
        let path = temp_file("artifact.json", &artifact);

        let loaded = load_strategy("swarm", Some(&path)).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(loaded.strategy_id, "swarm_consensus");
        assert!(loaded.dollar_bar_threshold.is_none());
        assert_eq!(loaded.symbol.as_deref(), Some("ETHUSDT"));
        assert_eq!(loaded.market_type, Some(MarketType::UsdMPerpetual));
        assert_eq!(loaded.execution_context, Some(BacktestConfig::default()));
        assert!(matches!(loaded.source, ConfigSource::Artifact { .. }));
        assert!(loaded.report_lines()[0].starts_with("[*] Authoritative Deployment Artifact"));
        assert!(loaded.report_lines()[0].contains("schema v1.0"));
    }

    #[test]
    fn a_bare_hyperparameter_file_still_loads() {
        let path = temp_file(
            "bare.json",
            &serde_json::to_string(&strategies::PolicyConfig::default()).unwrap(),
        );

        let loaded = load_strategy("swarm", Some(&path)).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(loaded.source, ConfigSource::Bare);
        assert!(loaded.report_lines()[0].starts_with("[*] Legacy bare"));
    }

    #[test]
    fn the_artifact_id_is_honoured_and_aliases_resolve() {
        let path = temp_file(
            "swarm-artifact.json",
            r#"{"strategy_id":"swarm","schema_version":"1.0"}"#,
        );

        // Nothing on the command line, or the same strategy under another spelling: fine.
        assert_eq!(
            resolve_strategy_id(None, Some(&path)).unwrap(),
            "swarm_consensus"
        );
        assert_eq!(
            resolve_strategy_id(Some("swarm_consensus"), Some(&path)).unwrap(),
            "swarm_consensus"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            resolve_strategy_id(Some("swarm"), None).unwrap(),
            "swarm_consensus"
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
            assert!(message.contains("swarm_consensus"));
        }
    }

    #[test]
    fn the_shared_config_flag_reaches_the_strategy() {
        let shared = temp_file(
            "shared-flag.json",
            &serde_json::to_string(&swarm_shared(7)).unwrap(),
        );

        let loaded = load_strategy_with_shared("swarm", None, Some(&shared)).unwrap();
        std::fs::remove_file(&shared).unwrap();

        assert_eq!(loaded.strategy_id, "swarm_consensus");
        assert_eq!(
            loaded.shared_config_json,
            Some(serde_json::to_value(swarm_shared(7)).unwrap())
        );
        assert_eq!(loaded.strategy.input_kind(), strategies::InputKind::Trades);
    }

    #[test]
    fn without_a_shared_config_the_strategy_default_is_used() {
        let loaded = load_strategy("swarm", None).unwrap();
        assert_eq!(
            loaded.shared_config_json,
            Some(serde_json::to_value(strategies::SwarmConfig::default()).unwrap())
        );
    }

    #[test]
    fn the_artifact_shared_config_comes_first_and_a_contradicting_flag_is_refused() {
        let artifact = serde_json::json!({
            "schema_version": "1.0",
            "strategy_id": "swarm_consensus",
            "symbol": "BTCUSDT",
            "market_type": "usd_m_perpetual",
            "sampling": {"type": "trades", "price_bucket_ms": 1000},
            "strategy_config": serde_json::to_value(strategies::PolicyConfig::default()).unwrap(),
            "execution_context": serde_json::to_value(BacktestConfig::default()).unwrap(),
            "shared_config": serde_json::to_value(swarm_shared(5)).unwrap(),
        });
        let artifact = temp_file("swarm-artifact.json", &artifact.to_string());
        let same = temp_file(
            "shared-same.json",
            &serde_json::to_string(&swarm_shared(5)).unwrap(),
        );
        let other = temp_file(
            "shared-other.json",
            &serde_json::to_string(&swarm_shared(9)).unwrap(),
        );

        let from_artifact = load_strategy("swarm", Some(&artifact)).unwrap();
        let agreeing = load_strategy_with_shared("swarm", Some(&artifact), Some(&same)).unwrap();
        let refused = load_strategy_with_shared("swarm", Some(&artifact), Some(&other))
            .err()
            .unwrap()
            .to_string();
        for path in [artifact, same, other] {
            std::fs::remove_file(path).unwrap();
        }

        let expected = Some(serde_json::to_value(swarm_shared(5)).unwrap());
        assert_eq!(from_artifact.shared_config_json, expected);
        assert_eq!(agreeing.shared_config_json, expected);
        assert!(refused.contains("--shared-config"), "{refused}");
        assert!(refused.contains("conflicts"), "{refused}");
    }

    #[test]
    fn a_file_that_is_no_configuration_is_rejected_instead_of_misread() {
        let path = temp_file("wrong.json", r#"{"unrelated": true}"#);

        let result = load_strategy("swarm", Some(&path));
        std::fs::remove_file(&path).unwrap();

        assert!(result.is_err());
    }

    #[test]
    fn a_spot_run_narrows_the_shared_configuration_and_says_so() {
        let shared = temp_file(
            "spot-shared.json",
            &serde_json::to_string(&swarm_shared(7)).unwrap(),
        );
        assert!(swarm_shared(7).allow_short);

        let perpetual = crate::wiring::load_strategy_for_market(
            "swarm",
            None,
            Some(&shared),
            Some(MarketType::UsdMPerpetual),
        )
        .unwrap();
        assert_eq!(perpetual.narrowed_for_market, None);

        let spot = crate::wiring::load_strategy_for_market(
            "swarm",
            None,
            Some(&shared),
            Some(MarketType::Spot),
        )
        .unwrap();
        std::fs::remove_file(&shared).unwrap();
        assert_eq!(spot.narrowed_for_market, Some(MarketType::Spot));
        assert_eq!(
            spot.shared_config_json.as_ref().unwrap()["allow_short"],
            serde_json::json!(false)
        );
        assert!(spot
            .report_lines()
            .iter()
            .any(|line| line.contains("narrowed for the Spot market")));
    }
}
