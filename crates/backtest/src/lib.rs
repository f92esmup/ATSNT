pub mod alert_throttle;
pub mod artifact;
pub mod config;
pub mod driver;
pub mod engine;
pub mod error;
pub mod execution_core;
pub mod live_exits;
pub mod metrics;
pub mod monte_carlo;
pub mod operator_alerts;
pub mod optimization;
pub mod paper;
pub mod parity;
pub mod portfolio_engine;
pub mod price_event;
pub mod recording;
pub mod registry;
pub mod run;
pub mod session;
pub mod sinks;
pub mod tick_simulation;
pub mod universe_selector;
pub mod walk_forward;
pub mod wiring;

pub use wiring::now_unix_ms;

pub use alert_throttle::AlertThrottle;
pub use artifact::{
    load_artifact_shared_config, load_portfolio_artifact, load_strategy_config,
    HpoDeploymentArtifact, HpoScorecard, LoadedStrategyConfig, PortfolioAssetSpec,
    PortfolioDeploymentArtifact, PortfolioExecutionSettings, PortfolioScorecard, SamplingConfig,
    StrategyDeploymentArtifact,
};
pub use config::BacktestConfig;
pub use domain::accounting::DailyWatermark;
pub use domain::accounting::ExcursionTracker;
pub use domain::accounting::{marked_equity, utc_month_key, MonthlyGuard};
pub use driver::{DriverStep, SessionDriver};
pub use engine::BacktestEngine;
pub use error::BacktestError;
pub use execution_core::{
    barrier_exit, market_fill_price, BarrierExit, ExecutionCore, PendingEntry, PendingEntryOutcome,
    PendingTarget, TradeLedger,
};
pub use live_exits::{ExitEvent, ExitGateway, ExitManager, HaltState};
pub use metrics::{
    format_ratio, format_rejections, sortino_ratio, trade_returns, BacktestMetrics, ClosedTrade,
    TradeExcursion, DAILY_HALT_REJECTION, POSITION_CAP_CLIP, REARM_IGNORED_REJECTION,
    STOP_BEYOND_LIQUIDATION_CLIP, UNSUPPORTED_DECISION_REJECTION,
};
pub use monte_carlo::{
    FanChartTrajectory, MonteCarloConfig, MonteCarloMetrics, MonteCarloReport, MonteCarloSimulator,
    ResampleMethod,
};
pub use optimization::{
    bar_tape, calculate_parameter_stability, calibration_end_ms, check_calibration,
    deflated_sharpe_p_value, embargo_buckets, expected_max_sharpe, plateau_stability,
    run_backtest_slice, run_backtest_slice_with_trades, run_tape_slice, trade_tape,
    CandidateEvaluation, FeaturesOf, FoldMetrics, ReturnMoments, Tape, TapeRun,
    WalkForwardOptimizer, DEFAULT_MIN_TRADES_PER_FOLD, TRADES_WARM_UP_MS,
};
pub use paper::{
    PaperSessionOwner, PaperSessionRuntime, PaperTradingConfig, PaperTradingEvent,
    PaperTradingSession, TelemetryConfig, TelemetryEnvelope,
};
pub use portfolio_engine::{
    EventMerger, PortfolioEngine, PortfolioEngineConfig, RollingCorrelationTracker,
    TaggedPriceEvent,
};
pub use price_event::{bucket_trades, PriceEvent};
pub use recording::{
    record_backtest, record_backtest_with_audit, record_closed_trade, record_hpo,
    record_monte_carlo, record_run_status, DataWindow, RunContext, RunStatus, GIT_SHA,
};
pub use registry::{
    load_strategy, load_strategy_with_shared, resolve_strategy_id, ConfigSource, DeploymentPolicy,
    LoadedStrategy, ALLOW_UNPROVEN_STRATEGY_HELP, DEFAULT_STRATEGY_ID,
};
pub use run::{FillModel, InvalidRunId, RunId, RunSpec, RunType};
pub use sinks::{artifact_object_name, artifact_run_suffix, SinkFailures, StrictSinkError};
pub use tick_simulation::{TickSimulation, TickSimulationConfig, TickSimulationError, TickStep};
pub use universe_selector::CrossSectionalUniverseSelector;
pub use walk_forward::{WalkForwardConfig, WalkForwardFold, WalkForwardSplitter};
