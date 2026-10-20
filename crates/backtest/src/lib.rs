pub mod alert_throttle;
pub mod artifact;
pub mod config;
pub mod daily_watermark;
pub mod driver;
pub mod engine;
pub mod error;
pub mod excursion;
pub mod execution_core;
pub mod fee_ledger;
pub mod live_exits;
pub mod metrics;
pub mod monte_carlo;
pub mod monthly_guard;
pub mod operator_alerts;
pub mod optimization;
pub mod paper;
pub mod parity;
pub mod price_event;
pub mod recording;
pub mod registry;
pub mod run;
pub mod session;
pub mod sinks;
pub mod tick_simulation;
pub mod walk_forward;

pub use alert_throttle::AlertThrottle;
pub use artifact::{
    load_strategy_config, HpoScorecard, LoadedStrategyConfig, SamplingConfig,
    StrategyDeploymentArtifact,
};
pub use config::BacktestConfig;
pub use daily_watermark::DailyWatermark;
pub use driver::{DriverStep, SessionDriver};
pub use engine::BacktestEngine;
pub use error::BacktestError;
pub use excursion::ExcursionTracker;
pub use execution_core::{
    barrier_exit, market_fill_price, BarrierExit, ExecutionCore, PendingEntry, PendingTarget,
    TradeLedger,
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
pub use monthly_guard::{marked_equity, utc_month_key, MonthlyGuard};
pub use optimization::{
    calculate_parameter_stability, deflated_sharpe_p_value, expected_max_sharpe, plateau_stability,
    run_backtest_slice, run_backtest_slice_with_trades, CandidateEvaluation, FoldMetrics,
    ReturnMoments, WalkForwardOptimizer, DEFAULT_MIN_TRADES_PER_FOLD,
};
pub use paper::{
    PaperSessionOwner, PaperSessionRuntime, PaperTradingConfig, PaperTradingEvent,
    PaperTradingSession, TelemetryConfig, TelemetryEnvelope,
};
pub use price_event::{bucket_trades, PriceEvent};
pub use recording::{
    now_unix_ms, record_backtest, record_closed_trade, record_hpo, record_monte_carlo,
    record_run_status, DataWindow, RunContext, RunStatus, GIT_SHA,
};
pub use registry::{
    load_strategy, resolve_strategy_id, ConfigSource, DeploymentPolicy, LoadedStrategy,
    ALLOW_UNPROVEN_STRATEGY_HELP, DEFAULT_STRATEGY_ID,
};
pub use run::{FillModel, InvalidRunId, RunId, RunSpec, RunType};
pub use sinks::{artifact_object_name, artifact_run_suffix, SinkFailures, StrictSinkError};
pub use tick_simulation::{TickSimulation, TickSimulationConfig, TickSimulationError, TickStep};
pub use walk_forward::{WalkForwardConfig, WalkForwardFold, WalkForwardSplitter};
