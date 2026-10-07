pub mod config;
pub mod engine;
pub mod error;
pub mod metrics;
pub mod monte_carlo;
pub mod optimization;
pub mod paper;
pub mod walk_forward;

pub use config::BacktestConfig;
pub use engine::BacktestEngine;
pub use error::BacktestError;
pub use metrics::{BacktestMetrics, ClosedTrade};
pub use monte_carlo::{
    FanChartTrajectory, MonteCarloConfig, MonteCarloMetrics, MonteCarloReport, MonteCarloSimulator,
    ResampleMethod,
};
pub use optimization::{
    calculate_dsr, calculate_parameter_stability, run_backtest_slice, CandidateEvaluation,
    ParameterSpace, WalkForwardOptimizer,
};
pub use paper::{
    PaperTradingConfig, PaperTradingEvent, PaperTradingSession, TelemetryConfig, TelemetryEnvelope,
};
pub use walk_forward::{WalkForwardConfig, WalkForwardFold, WalkForwardSplitter};
