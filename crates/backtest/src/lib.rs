// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub use application::{
    alert_throttle, config, driver, execution_core, live_exits, operator_alerts, parity,
    price_event, session, tick_simulation,
};

pub mod artifact;
pub mod engine;
pub mod error;
pub mod monte_carlo;
pub mod optimization;
pub mod recording;
pub mod registry;
pub mod run;
pub mod walk_forward;
pub mod wiring;

pub use wiring::{
    load_artifact_shared_config, load_strategy, load_strategy_config, load_strategy_for_market,
    load_strategy_with_shared, now_unix_ms, resolve_strategy_id,
};

pub use alert_throttle::AlertThrottle;
pub use artifact::{
    parse_artifact_shared_config, parse_strategy_config, HpoDeploymentArtifact, HpoScorecard,
    LoadedStrategyConfig, SamplingConfig, StrategyDeploymentArtifact,
};
pub use config::BacktestConfig;
pub use domain::accounting::DailyWatermark;
pub use domain::accounting::ExcursionTracker;
pub use domain::accounting::{marked_equity, utc_month_key, MonthlyGuard};
pub use domain::metrics;
pub use driver::{DriverStep, SessionDriver};
pub use engine::BacktestEngine;
pub use error::{BacktestError, ConfigText, LoadError};
pub use execution_core::{
    barrier_exit, market_fill_price, BarrierExit, ExecutionCore, PendingEntry, PendingEntryOutcome,
    TradeLedger,
};
pub use live_exits::{ExitEvent, ExitGateway, ExitManager, HaltState};
pub use metrics::{
    format_ratio, format_rejections, sortino_ratio, trade_returns, BacktestMetrics, ClosedTrade,
    TradeExcursion, DAILY_HALT_REJECTION,
};
pub use monte_carlo::{
    FanChartTrajectory, MonteCarloConfig, MonteCarloMetrics, MonteCarloReport, MonteCarloSimulator,
    ResampleMethod,
};
pub use optimization::{
    bar_tape, calculate_parameter_stability, calibration_end_ms, check_calibration,
    deflated_sharpe_p_value, embargo_buckets, expected_max_sharpe, plateau_stability,
    run_tape_slice, trade_tape, CandidateEvaluation, FeaturesOf, FoldMetrics, ReturnMoments, Tape,
    TapeRun, WalkForwardOptimizer, DEFAULT_MIN_TRADES_PER_FOLD, TRADES_WARM_UP_MS,
};
pub use parity::FillRecordError;
pub use price_event::{bucket_trades, PriceEvent};
pub use recording::{
    record_backtest, record_backtest_with_audit, record_closed_trade, record_hpo,
    record_monte_carlo, record_run_status, DataWindow, HpoSearch, RunContext, RunStatus, GIT_SHA,
};
pub use registry::{
    build_strategy, strategy_id_of, ConfigSource, DeploymentPolicy, LoadedStrategy,
    ALLOW_UNPROVEN_STRATEGY_HELP, DEFAULT_STRATEGY_ID,
};
pub use run::{FillModel, InvalidRunId, RunId, RunSpec, RunType};
pub use tick_simulation::{TickSimulation, TickSimulationConfig, TickSimulationError, TickStep};
pub use walk_forward::{WalkForwardConfig, WalkForwardFold, WalkForwardSplitter};
