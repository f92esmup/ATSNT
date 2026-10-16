pub mod artifact;
pub mod config;
pub mod engine;
pub mod error;
pub mod metrics;
pub mod monte_carlo;
pub mod monthly_guard;
pub mod optimization;
pub mod paper;
pub mod recording;
pub mod run;
pub mod sinks;
pub mod walk_forward;

pub use artifact::{
    load_strategy_config, HpoScorecard, LoadedStrategyConfig, SamplingConfig,
    StrategyDeploymentArtifact,
};
pub use config::BacktestConfig;
pub use engine::BacktestEngine;
pub use error::BacktestError;
pub use metrics::{BacktestMetrics, ClosedTrade, TradeExcursion};
pub use monte_carlo::{
    FanChartTrajectory, MonteCarloConfig, MonteCarloMetrics, MonteCarloReport, MonteCarloSimulator,
    ResampleMethod,
};
pub use monthly_guard::{marked_equity, utc_month_key, MonthlyGuard};
pub use optimization::{
    calculate_dsr, calculate_parameter_stability, run_backtest_slice, CandidateEvaluation,
    FoldMetrics, ParameterSpace, WalkForwardOptimizer,
};
pub use paper::{
    PaperSessionOwner, PaperSessionRuntime, PaperTradingConfig, PaperTradingEvent,
    PaperTradingSession, TelemetryConfig, TelemetryEnvelope,
};
pub use recording::{
    now_unix_ms, record_backtest, record_closed_trade, record_hpo, record_monte_carlo,
    record_run_status, DataWindow, RunContext, RunStatus, GIT_SHA,
};
pub use run::{RunId, RunSpec, RunType};
pub use sinks::{artifact_object_name, artifact_run_suffix, SinkFailures, StrictSinkError};
pub use walk_forward::{WalkForwardConfig, WalkForwardFold, WalkForwardSplitter};
