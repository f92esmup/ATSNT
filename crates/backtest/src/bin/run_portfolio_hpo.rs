use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use adapters::{BinanceCsvReader, BinanceParquetReader, GcsArtifactSink, MarketDataStream};
use anyhow::{Context, Result};
use backtest::metrics::trade_returns;
use backtest::{
    artifact_object_name, artifact_run_suffix, BacktestMetrics, ClosedTrade, EventMerger,
    PortfolioAssetSpec, PortfolioDeploymentArtifact, PortfolioEngine, PortfolioEngineConfig,
    PortfolioExecutionSettings, PortfolioScorecard, PriceEvent, SinkFailures, TaggedPriceEvent,
    WalkForwardConfig, WalkForwardFold, WalkForwardSplitter,
};
use chrono::Utc;
use clap::Parser;
use domain::{DollarBar, DollarBarAggregator, PortfolioRiskPolicy};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use strategies::{dispatch, HyperparameterSpace, MarketType, OptimizableStrategy, StrategyVisitor};

#[derive(Parser, Debug)]
#[command(
    name = "run_portfolio_hpo",
    about = "ATSNT - Joint Multi-Asset Portfolio Walk-Forward Hyperparameter Optimizer"
)]
struct Args {
    /// Target strategy identifier to optimize across the portfolio (default: 'quantum_well_swing')
    #[arg(long, default_value = "quantum_well_swing")]
    strategy: String,

    /// Optional path to JSON file overriding the strategy parameter search space
    #[arg(long)]
    param_space: Option<PathBuf>,

    /// Path to BTC historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/BTCUSDT")]
    btc_data: PathBuf,

    /// Path to ETH historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/ETHUSDT")]
    eth_data: PathBuf,

    /// Path to SOL historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/SOLUSDT")]
    sol_data: PathBuf,

    /// Optional path to BTC single-asset deployment artifact
    #[arg(long)]
    btc_config: Option<PathBuf>,

    /// Optional path to ETH single-asset deployment artifact
    #[arg(long)]
    eth_config: Option<PathBuf>,

    /// Optional path to SOL single-asset deployment artifact
    #[arg(long)]
    sol_config: Option<PathBuf>,

    /// Dollar bar threshold override for BTC (single value fallback)
    #[arg(long)]
    btc_dollar_bar: Option<Decimal>,

    /// Dollar bar threshold override for ETH (single value fallback)
    #[arg(long)]
    eth_dollar_bar: Option<Decimal>,

    /// Dollar bar threshold override for SOL (single value fallback)
    #[arg(long)]
    sol_dollar_bar: Option<Decimal>,

    /// Comma-separated dollar bar thresholds to evaluate for BTC (e.g. "150000000,300000000,600000000")
    #[arg(long, value_delimiter = ',')]
    btc_dollar_bars: Option<Vec<Decimal>>,

    /// Comma-separated dollar bar thresholds to evaluate for ETH (e.g. "100000000,200000000,400000000")
    #[arg(long, value_delimiter = ',')]
    eth_dollar_bars: Option<Vec<Decimal>>,

    /// Comma-separated dollar bar thresholds to evaluate for SOL (e.g. "30000000,60000000,120000000")
    #[arg(long, value_delimiter = ',')]
    sol_dollar_bars: Option<Vec<Decimal>>,

    /// Initial shared capital in quote currency (USDT)
    #[arg(long, default_value = "500")]
    capital: Decimal,

    /// Number of Walk-Forward validation folds
    #[arg(long, default_value_t = 3)]
    folds: usize,

    /// Ratio of each rolling window dedicated to training (e.g. 0.70 for 70%)
    #[arg(long, default_value_t = 0.70)]
    train_ratio: f64,

    /// Number of bars quarantined between train and test to prevent serial leakage
    #[arg(long, default_value_t = 20)]
    embargo_bars: usize,

    /// Number of portfolio parameter candidates to evaluate (or fallback budget)
    #[arg(long, default_value_t = 50)]
    candidates: usize,

    /// Number of candidates to evaluate per asset in Stage 1 (default: 60)
    #[arg(long, default_value_t = 60)]
    stage1_candidates: usize,

    /// Number of top candidates per asset selected in Stage 1 to combine in Stage 2 (default: 8, yielding 512 joint evaluations)
    #[arg(long, default_value_t = 8)]
    top_per_asset: usize,

    /// Minimum closed trades required in every fold to be eligible for ranking
    #[arg(long, default_value_t = 1)]
    min_trades_per_fold: usize,

    /// Maximum drawdown threshold fraction before fitness is penalized to zero (default: 0.20 = 20%)
    #[arg(long, default_value = "0.20")]
    max_dd_threshold: Decimal,

    /// Whether to run in Spot market mode (strict 1x leverage, 0% funding, 0.10% fees)
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Whether to apply Binance 25% BNB fee discount (0.075% in Spot)
    #[arg(long, default_value_t = false)]
    bnb_discount: bool,

    /// Path to write the winning Portfolio Deployment Artifact JSON
    #[arg(long, default_value = "configs/portfolio_deployment_artifact.json")]
    output_config: PathBuf,

    /// Whether to upload the winning artifact to Google Cloud Storage
    #[arg(long, default_value_t = false)]
    gcs_upload: bool,

    /// Optional suffix appended to the uploaded artifact filename (default: UTC timestamp)
    #[arg(long)]
    artifact_suffix: Option<String>,

    /// Fail the command when GCS or BigQuery uploads fail
    #[arg(long, default_value_t = false)]
    strict_sinks: bool,
}

/// Evaluated joint portfolio hyperparameter candidate with independent per-asset configurations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JointPortfolioCandidate<C> {
    pub btc_config: C,
    pub eth_config: C,
    pub sol_config: C,
    pub risk_per_trade_pct: Decimal,
    pub max_open_positions: usize,
    pub max_directional_positions: usize,
    pub max_slot_margin_pct: Decimal,
    pub use_limit_orders: bool,
    pub limit_timeout_bars: usize,
}

impl<C: std::fmt::Display> JointPortfolioCandidate<C> {
    pub fn summary_string(&self) -> String {
        format!(
            "Risk:{:.2}%|MaxPos:{}|Margin:{:.0}%|Mode:{}|BTC:[{}]|ETH:[{}]|SOL:[{}]",
            self.risk_per_trade_pct * dec!(100),
            self.max_open_positions,
            self.max_slot_margin_pct * dec!(100),
            if self.use_limit_orders {
                "Limit"
            } else {
                "Market"
            },
            self.btc_config,
            self.eth_config,
            self.sol_config
        )
    }
}

/// Evaluation result for a single asset configuration evaluated in Stage 1.
#[derive(Debug, Clone)]
pub struct SingleAssetEvaluation<C> {
    pub config: C,
    pub mean_oos_sortino: Decimal,
    pub oos_profit_factor: Option<Decimal>,
    pub max_drawdown_pct: Decimal,
    pub oos_win_rate_pct: Decimal,
    pub total_trades: usize,
    pub fitness: Decimal,
}

/// Specification for asymmetric parameter spaces per asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsymmetricParamSpace<Space> {
    pub btc: Option<Space>,
    pub eth: Option<Space>,
    pub sol: Option<Space>,
    #[serde(default)]
    pub btc_dollar_bars: Option<Vec<Decimal>>,
    #[serde(default)]
    pub eth_dollar_bars: Option<Vec<Decimal>>,
    #[serde(default)]
    pub sol_dollar_bars: Option<Vec<Decimal>>,
}

/// Evaluation outcome for a candidate across Walk-Forward folds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JointPortfolioEvaluation<C> {
    pub candidate: JointPortfolioCandidate<C>,
    pub mean_is_sortino: Decimal,
    pub mean_oos_sortino: Decimal,
    pub oos_sharpe: Option<Decimal>,
    pub oos_profit_factor: Option<Decimal>,
    pub oos_win_rate_pct: Decimal,
    pub total_oos_trades: usize,
    pub min_fold_trades: usize,
    pub max_drawdown_pct: Decimal,
    pub diversification_score: Decimal,
    pub fitness: Decimal,
    pub eligible: bool,
    pub trades_per_asset: BTreeMap<String, usize>,
}

fn open_stream(data: &Path) -> Result<Box<dyn MarketDataStream<Error = adapters::AdapterError>>> {
    if data.is_dir() || data.extension().and_then(|s| s.to_str()) == Some("parquet") {
        Ok(Box::new(BinanceParquetReader::open(data)?))
    } else {
        let file =
            File::open(data).with_context(|| format!("Failed to open {}", data.display()))?;
        Ok(Box::new(BinanceCsvReader::new(BufReader::new(file))))
    }
}

fn load_asset_multi_resolution_bars(
    symbol: &str,
    path: &Path,
    thresholds: &[Decimal],
) -> Result<Vec<(Decimal, Vec<DollarBar>)>> {
    println!(
        "      • Ingesting {symbol} across {} thresholds: [{}]...",
        thresholds.len(),
        thresholds
            .iter()
            .map(|t| format!("${:.0}M", t / dec!(1_000_000)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut stream = open_stream(path)?;
    let mut aggregators: Vec<(Decimal, DollarBarAggregator, Vec<DollarBar>)> = thresholds
        .iter()
        .map(|&t| {
            let agg = DollarBarAggregator::new(t)
                .map_err(|e| anyhow::anyhow!("Invalid dollar bar threshold {t}: {e}"))?;
            Ok((t, agg, Vec::with_capacity(8192)))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut trade_count = 0usize;
    let start = Instant::now();

    while let Some(trade) = stream.next_trade()? {
        trade_count += 1;
        for (_thresh, agg, bars) in &mut aggregators {
            if let Some(bar) = agg.process_trade(&trade) {
                bars.push(bar);
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "        [OK] {symbol}: Processed {trade_count} trades in {:.2}s ({:.1}M trades/s)",
        elapsed,
        (trade_count as f64 / elapsed.max(0.001)) / 1_000_000.0
    );
    for (t, _, bars) in &aggregators {
        println!(
            "          - Threshold ${:<6.0}M: {:>6} bars (~{:.1} bars/day)",
            t / dec!(1_000_000),
            bars.len(),
            bars.len() as f64 / 300.0
        );
    }

    Ok(aggregators
        .into_iter()
        .map(|(t, _, bars)| (t, bars))
        .collect())
}

fn evaluate_single_asset_candidate<S: OptimizableStrategy>(
    symbol: &str,
    config: &S::Config,
    ctx: &PortfolioEvaluationContext<'_>,
    shared: &S::SharedConfig,
) -> Result<SingleAssetEvaluation<S::Config>> {
    let mut oos_sortinos = Vec::with_capacity(ctx.folds.len());
    let mut oos_max_dds = Vec::with_capacity(ctx.folds.len());
    let mut all_oos_trades = Vec::new();

    let candidate = JointPortfolioCandidate {
        btc_config: config.clone(),
        eth_config: config.clone(),
        sol_config: config.clone(),
        risk_per_trade_pct: dec!(0.015),
        max_open_positions: 1,
        max_directional_positions: 1,
        max_slot_margin_pct: dec!(0.35),
        use_limit_orders: false,
        limit_timeout_bars: 2,
    };

    for fold in ctx.folds {
        let oos_slice = &ctx.events[fold.test_range.clone()];
        let filtered: Vec<TaggedPriceEvent> = oos_slice
            .iter()
            .filter(|e| e.symbol == symbol)
            .cloned()
            .collect();

        if filtered.is_empty() {
            continue;
        }

        let (oos_metrics, oos_trades, _) =
            run_portfolio_slice::<S>(&filtered, &candidate, ctx, shared)?;

        oos_sortinos.push(oos_metrics.sortino_ratio.unwrap_or(Decimal::ZERO));
        oos_max_dds.push(oos_metrics.max_drawdown_pct);
        all_oos_trades.extend(oos_trades);
    }

    let num_folds_dec = Decimal::from(ctx.folds.len().max(1));
    let mean_oos_sortino = oos_sortinos.iter().copied().sum::<Decimal>() / num_folds_dec;
    let max_drawdown_pct = oos_max_dds
        .iter()
        .copied()
        .fold(Decimal::ZERO, Decimal::max);

    let total_trades = all_oos_trades.len();
    let wins = all_oos_trades
        .iter()
        .filter(|t| t.pnl_net > Decimal::ZERO)
        .count();
    let oos_win_rate_pct = if total_trades > 0 {
        (Decimal::from(wins) / Decimal::from(total_trades)) * dec!(100)
    } else {
        Decimal::ZERO
    };

    let gross_profit: Decimal = all_oos_trades
        .iter()
        .filter(|t| t.pnl_net > Decimal::ZERO)
        .map(|t| t.pnl_net)
        .sum();
    let gross_loss: Decimal = all_oos_trades
        .iter()
        .filter(|t| t.pnl_net < Decimal::ZERO)
        .map(|t| t.pnl_net.abs())
        .sum();
    let oos_profit_factor = if gross_loss > Decimal::ZERO {
        Some(gross_profit / gross_loss)
    } else if gross_profit > Decimal::ZERO {
        Some(dec!(99.99))
    } else {
        None
    };

    let dd_factor = if ctx.max_dd_threshold > Decimal::ZERO {
        (Decimal::ONE - (max_drawdown_pct / ctx.max_dd_threshold)).max(Decimal::ZERO)
    } else {
        Decimal::ZERO
    };

    let fitness = if total_trades >= ctx.min_trades_per_fold * ctx.folds.len()
        && mean_oos_sortino > Decimal::ZERO
    {
        mean_oos_sortino * dd_factor
    } else {
        Decimal::ZERO
    };

    Ok(SingleAssetEvaluation {
        config: config.clone(),
        mean_oos_sortino,
        oos_profit_factor,
        max_drawdown_pct,
        oos_win_rate_pct,
        total_trades,
        fitness,
    })
}

#[derive(Debug, Clone)]
struct SingleAssetResolutionResult<C> {
    threshold: Decimal,
    bars_count: usize,
    best_eval: SingleAssetEvaluation<C>,
    top_configs: Vec<C>,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_asset_multi_resolution<S: OptimizableStrategy>(
    symbol: &str,
    candidates: &[S::Config],
    resolutions: &[(Decimal, Vec<DollarBar>)],
    wf_config: &WalkForwardConfig,
    args: &Args,
    shared: &S::SharedConfig,
    market_type: MarketType,
    top_k: usize,
) -> Result<(Decimal, Vec<DollarBar>, Vec<S::Config>)> {
    let mut resolution_results: Vec<SingleAssetResolutionResult<S::Config>> = Vec::new();

    for (thresh, bars) in resolutions {
        if bars.len() < 30 {
            println!(
                "        [SKIP] Threshold ${:.0}M has only {} bars (need >= 30 for folds)",
                thresh / dec!(1_000_000),
                bars.len()
            );
            continue;
        }

        let single_events: Vec<TaggedPriceEvent> = bars
            .iter()
            .map(|b| TaggedPriceEvent {
                symbol: symbol.to_string(),
                event: PriceEvent::from(b),
            })
            .collect();

        let folds =
            match WalkForwardSplitter::generate_rolling_folds(single_events.len(), wf_config) {
                Ok(f) => f,
                Err(e) => {
                    println!(
                        "        [SKIP] Threshold ${:.0}M fold generation failed: {e}",
                        thresh / dec!(1_000_000)
                    );
                    continue;
                }
            };

        let ctx = PortfolioEvaluationContext {
            folds: &folds,
            events: &single_events,
            capital: args.capital,
            min_trades_per_fold: args.min_trades_per_fold,
            max_dd_threshold: args.max_dd_threshold,
            market_type,
            bnb_discount: args.bnb_discount,
        };

        let mut evals: Vec<SingleAssetEvaluation<S::Config>> = candidates
            .par_iter()
            .filter_map(|cfg| evaluate_single_asset_candidate::<S>(symbol, cfg, &ctx, shared).ok())
            .collect();

        if evals.is_empty() {
            continue;
        }

        evals.sort_by(|a, b| {
            b.fitness
                .cmp(&a.fitness)
                .then_with(|| b.mean_oos_sortino.cmp(&a.mean_oos_sortino))
                .then_with(|| {
                    let a_pf = a.oos_profit_factor.unwrap_or(Decimal::ZERO);
                    let b_pf = b.oos_profit_factor.unwrap_or(Decimal::ZERO);
                    b_pf.cmp(&a_pf)
                })
                .then_with(|| b.oos_win_rate_pct.cmp(&a.oos_win_rate_pct))
        });

        let best = evals[0].clone();
        let top_configs: Vec<S::Config> = evals.into_iter().take(top_k).map(|e| e.config).collect();

        resolution_results.push(SingleAssetResolutionResult {
            threshold: *thresh,
            bars_count: bars.len(),
            best_eval: best,
            top_configs,
        });
    }

    if resolution_results.is_empty() {
        anyhow::bail!("No viable dollar bar resolution found for {symbol} (all had < 30 bars or failed evaluation)");
    }

    println!(
        "\n      ========================================================================================================="
    );
    println!("       [RESOLUTION SWEEP] {symbol} Dollar Bar Grid & Inverted-U Analysis:");
    println!(
        "      ---------------------------------------------------------------------------------------------------------"
    );
    println!(
        "       Threshold       Bars   ~Bars/Day  Trades   WinRate   ProfitFactor  OOS Sortino   MaxDD    Status"
    );
    println!(
        "      ---------------------------------------------------------------------------------------------------------"
    );

    for res in &resolution_results {
        let pf = res.best_eval.oos_profit_factor.unwrap_or(Decimal::ZERO);
        let status = if res.best_eval.total_trades < 10 {
            "Low trades (<10)"
        } else if pf >= dec!(1.2) {
            "Strong edge (PF >= 1.2)"
        } else if pf >= dec!(1.0) {
            "Profitable (PF >= 1.0)"
        } else {
            "Friction heavy (PF < 1.0)"
        };

        println!(
            "       ${:<11.0}M  {:>6}     {:>5.1}    {:>5}    {:>5.1}%     {:>8.2}     {:>+9.4}   {:>5.1}%    {}",
            res.threshold / dec!(1_000_000),
            res.bars_count,
            res.bars_count as f64 / 300.0,
            res.best_eval.total_trades,
            res.best_eval.oos_win_rate_pct,
            pf,
            res.best_eval.mean_oos_sortino,
            res.best_eval.max_drawdown_pct * dec!(100),
            status
        );
    }
    println!(
        "      ========================================================================================================="
    );

    // Pick winning resolution
    let mut sorted_resolutions = resolution_results.clone();
    sorted_resolutions.sort_by(|a, b| {
        let a_viable = a.best_eval.total_trades >= 10;
        let b_viable = b.best_eval.total_trades >= 10;
        b_viable
            .cmp(&a_viable)
            .then_with(|| b.best_eval.fitness.cmp(&a.best_eval.fitness))
            .then_with(|| {
                let a_pf = a.best_eval.oos_profit_factor.unwrap_or(Decimal::ZERO);
                let b_pf = b.best_eval.oos_profit_factor.unwrap_or(Decimal::ZERO);
                b_pf.cmp(&a_pf)
            })
            .then_with(|| {
                b.best_eval
                    .mean_oos_sortino
                    .cmp(&a.best_eval.mean_oos_sortino)
            })
            .then_with(|| {
                b.best_eval
                    .oos_win_rate_pct
                    .cmp(&a.best_eval.oos_win_rate_pct)
            })
    });

    let winner = sorted_resolutions.into_iter().next().unwrap();
    let winning_thresh = winner.threshold;
    let winning_top_configs = winner.top_configs;

    let winning_bars = resolutions
        .iter()
        .find(|(t, _)| *t == winning_thresh)
        .map(|(_, b)| b.clone())
        .ok_or_else(|| anyhow::anyhow!("Winning resolution bars not found"))?;

    println!(
        "      [WINNER] Selected optimal resolution for {symbol}: ${:.0}M ({} bars, PF: {:.2}, Trades: {})\n",
        winning_thresh / dec!(1_000_000),
        winning_bars.len(),
        winner.best_eval.oos_profit_factor.unwrap_or(Decimal::ZERO),
        winner.best_eval.total_trades
    );

    Ok((winning_thresh, winning_bars, winning_top_configs))
}

type PortfolioSliceResult = (
    BacktestMetrics,
    Vec<ClosedTrade>,
    HashMap<String, Vec<ClosedTrade>>,
);

struct PortfolioEvaluationContext<'a> {
    folds: &'a [WalkForwardFold],
    events: &'a [TaggedPriceEvent],
    capital: Decimal,
    min_trades_per_fold: usize,
    max_dd_threshold: Decimal,
    market_type: MarketType,
    bnb_discount: bool,
}

fn run_portfolio_slice<S: OptimizableStrategy>(
    slice: &[TaggedPriceEvent],
    candidate: &JointPortfolioCandidate<S::Config>,
    ctx: &PortfolioEvaluationContext<'_>,
    shared: &S::SharedConfig,
) -> Result<PortfolioSliceResult> {
    let (maker_fee_pct, taker_fee_pct, max_leverage) = match (ctx.market_type, ctx.bnb_discount) {
        (MarketType::Spot, true) => (dec!(0.00075), dec!(0.00075), dec!(1)),
        (MarketType::Spot, false) => (dec!(0.0010), dec!(0.0010), dec!(1)),
        (MarketType::UsdMPerpetual, true) => (dec!(0.00018), dec!(0.00045), dec!(20)),
        (MarketType::UsdMPerpetual, false) => (dec!(0.0002), dec!(0.0005), dec!(20)),
    };

    let risk_policy = PortfolioRiskPolicy {
        max_open_positions: candidate.max_open_positions,
        max_directional_positions: if ctx.market_type == MarketType::Spot {
            candidate.max_open_positions
        } else {
            candidate.max_directional_positions
        },
        risk_per_trade_pct: candidate.risk_per_trade_pct,
        max_slot_margin_pct: candidate.max_slot_margin_pct,
        max_leverage,
        min_order_quantity: dec!(0.001),
        quantity_step_size: dec!(0.001),
        min_order_notional: dec!(5),
        ..PortfolioRiskPolicy::default()
    };

    let engine_config = PortfolioEngineConfig {
        initial_capital: ctx.capital,
        maker_fee_pct,
        taker_fee_pct,
        slippage_pct: dec!(0.0005),
        risk_policy,
        latency_ms: 100,
        market_type: ctx.market_type,
        btc_breadth_filter: false,
    };

    let mut engine = PortfolioEngine::new(engine_config);

    let mut btc_strat = S::from_config(shared, candidate.btc_config.clone())
        .map_err(|e| anyhow::anyhow!("Failed to construct BTC strategy: {e}"))?;
    let mut eth_strat = S::from_config(shared, candidate.eth_config.clone())
        .map_err(|e| anyhow::anyhow!("Failed to construct ETH strategy: {e}"))?;
    let mut sol_strat = S::from_config(shared, candidate.sol_config.clone())
        .map_err(|e| anyhow::anyhow!("Failed to construct SOL strategy: {e}"))?;

    for tagged in slice {
        match tagged.symbol.as_str() {
            "BTCUSDT" => {
                let _ = engine.process_event("BTCUSDT", tagged.event, &mut btc_strat);
            }
            "ETHUSDT" => {
                let _ = engine.process_event("ETHUSDT", tagged.event, &mut eth_strat);
            }
            "SOLUSDT" => {
                let _ = engine.process_event("SOLUSDT", tagged.event, &mut sol_strat);
            }
            _ => {}
        }
    }

    let last_ts = slice.last().map(|e| e.event.end_ms).unwrap_or(0);
    let (metrics, closed_trades) = engine.finish(last_ts);
    let by_symbol = engine.closed_trades_by_symbol();

    Ok((metrics, closed_trades, by_symbol))
}

fn calculate_sharpe(returns: &[Decimal]) -> Option<Decimal> {
    if returns.len() < 2 {
        return None;
    }
    let n = returns.len() as f64;
    let sum: f64 = returns.iter().filter_map(|r| r.to_f64()).sum();
    let mean = sum / n;

    let variance_sum: f64 = returns
        .iter()
        .filter_map(|r| r.to_f64())
        .map(|r| (r - mean).powi(2))
        .sum();
    let variance = variance_sum / (n - 1.0);
    if variance <= 1e-12 {
        return None;
    }
    let std_dev = variance.sqrt();
    Decimal::from_f64_retain(mean / std_dev)
}

fn evaluate_candidate<S: OptimizableStrategy>(
    candidate: &JointPortfolioCandidate<S::Config>,
    ctx: &PortfolioEvaluationContext<'_>,
    shared: &S::SharedConfig,
) -> Result<JointPortfolioEvaluation<S::Config>> {
    let mut is_sortinos = Vec::with_capacity(ctx.folds.len());
    let mut oos_sortinos = Vec::with_capacity(ctx.folds.len());
    let mut oos_max_dds = Vec::with_capacity(ctx.folds.len());
    let mut all_oos_trades = Vec::new();
    let mut min_fold_trades = usize::MAX;
    let mut asset_trades_count: BTreeMap<String, usize> = BTreeMap::new();
    asset_trades_count.insert("BTCUSDT".to_string(), 0);
    asset_trades_count.insert("ETHUSDT".to_string(), 0);
    asset_trades_count.insert("SOLUSDT".to_string(), 0);

    for fold in ctx.folds {
        let is_slice = &ctx.events[fold.train_range.clone()];
        let oos_slice = &ctx.events[fold.test_range.clone()];

        let (is_metrics, _, _) = run_portfolio_slice::<S>(is_slice, candidate, ctx, shared)?;
        let (oos_metrics, oos_trades, by_symbol) =
            run_portfolio_slice::<S>(oos_slice, candidate, ctx, shared)?;

        is_sortinos.push(is_metrics.sortino_ratio.unwrap_or(Decimal::ZERO));
        oos_sortinos.push(oos_metrics.sortino_ratio.unwrap_or(Decimal::ZERO));
        oos_max_dds.push(oos_metrics.max_drawdown_pct);

        min_fold_trades = min_fold_trades.min(oos_trades.len());
        all_oos_trades.extend(oos_trades);

        for (sym, trades) in by_symbol {
            *asset_trades_count.entry(sym).or_insert(0) += trades.len();
        }
    }

    if min_fold_trades == usize::MAX {
        min_fold_trades = 0;
    }

    let num_folds_dec = Decimal::from(ctx.folds.len());
    let mean_is_sortino = is_sortinos.iter().copied().sum::<Decimal>() / num_folds_dec;
    let mean_oos_sortino = oos_sortinos.iter().copied().sum::<Decimal>() / num_folds_dec;
    let max_drawdown_pct = oos_max_dds
        .iter()
        .copied()
        .fold(Decimal::ZERO, Decimal::max);

    let total_oos_trades = all_oos_trades.len();
    let oos_wins = all_oos_trades
        .iter()
        .filter(|t| t.pnl_net > Decimal::ZERO)
        .count();
    let oos_win_rate_pct = if total_oos_trades > 0 {
        (Decimal::from(oos_wins) / Decimal::from(total_oos_trades)) * dec!(100)
    } else {
        Decimal::ZERO
    };

    let gross_profit: Decimal = all_oos_trades
        .iter()
        .filter(|t| t.pnl_net > Decimal::ZERO)
        .map(|t| t.pnl_net)
        .sum();
    let gross_loss: Decimal = all_oos_trades
        .iter()
        .filter(|t| t.pnl_net < Decimal::ZERO)
        .map(|t| t.pnl_net.abs())
        .sum();
    let oos_profit_factor = if gross_loss > Decimal::ZERO {
        Some(gross_profit / gross_loss)
    } else if gross_profit > Decimal::ZERO {
        Some(dec!(99.99))
    } else {
        None
    };

    let oos_returns = trade_returns(ctx.capital, &all_oos_trades);
    let oos_sharpe = calculate_sharpe(&oos_returns);

    // Diversification score using Herfindahl-Hirschman Index (HHI)
    let diversification_score = if total_oos_trades >= 3 {
        let total_f = total_oos_trades as f64;
        let mut hhi = 0.0;
        for &count in asset_trades_count.values() {
            let share = count as f64 / total_f;
            hhi += share * share;
        }
        let n = 3.0;
        let min_hhi = 1.0 / n;
        let score = (1.0 - hhi) / (1.0 - min_hhi);
        Decimal::from_f64_retain(score.clamp(0.0, 1.0)).unwrap_or(Decimal::ZERO)
    } else {
        Decimal::ZERO
    };

    let eligible = min_fold_trades >= ctx.min_trades_per_fold;

    let dd_factor = if ctx.max_dd_threshold > Decimal::ZERO {
        (Decimal::ONE - (max_drawdown_pct / ctx.max_dd_threshold)).max(Decimal::ZERO)
    } else {
        Decimal::ZERO
    };

    let fitness = if eligible && mean_is_sortino > Decimal::ZERO && mean_oos_sortino > Decimal::ZERO
    {
        mean_oos_sortino * dd_factor * diversification_score
    } else {
        Decimal::ZERO
    };

    Ok(JointPortfolioEvaluation {
        candidate: candidate.clone(),
        mean_is_sortino,
        mean_oos_sortino,
        oos_sharpe,
        oos_profit_factor,
        oos_win_rate_pct,
        total_oos_trades,
        min_fold_trades,
        max_drawdown_pct,
        diversification_score,
        fitness,
        eligible,
        trades_per_asset: asset_trades_count,
    })
}

fn run_joint_portfolio_hpo<S>(args: &Args) -> Result<()>
where
    S: OptimizableStrategy + 'static,
    S::Config:
        Default + Serialize + for<'de> Deserialize<'de> + std::fmt::Display + Clone + Send + Sync,
    S::Space: Serialize + for<'de> Deserialize<'de> + Send + Sync,
{
    let start_total = Instant::now();
    let market_type = if args.spot {
        MarketType::Spot
    } else {
        MarketType::UsdMPerpetual
    };

    let shared = S::SharedConfig::default();
    let probe = S::from_config(&shared, S::Config::default())
        .map_err(|e| anyhow::anyhow!("Strategy initialization error: {e}"))?;

    if !probe.supports_market_type(market_type) {
        anyhow::bail!(
            "Strategy '{}' does not support market type {:?}. Compatible market types: {:?}",
            probe.name(),
            market_type,
            probe.compatible_market_types()
        );
    }

    println!("============================================================");
    println!(" ATSNT - Joint Multi-Asset Portfolio Walk-Forward HPO       ");
    println!("============================================================");
    println!("Strategy:             {}", probe.name());
    println!("Market Type:          {:?}", market_type);
    println!("Shared Capital:       ${:.2} USDT", args.capital);
    println!(
        "Folds:                {} (Train: {:.0}%, Embargo: {} bars)",
        args.folds,
        args.train_ratio * 100.0,
        args.embargo_bars
    );
    println!("Candidates:           Up to {}", args.candidates);
    println!("Min Trades/Fold:      {}", args.min_trades_per_fold);
    println!(
        "Max DD Threshold:     {:.1}%",
        args.max_dd_threshold * dec!(100)
    );
    println!("------------------------------------------------------------");

    // 1. Resolve parameter spaces and multi-resolution threshold grids
    let (btc_space, eth_space, sol_space, btc_bars_space, eth_bars_space, sol_bars_space) =
        if let Some(path) = &args.param_space {
            println!(
                "[*] Loading parameter space override from: {}",
                path.display()
            );
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            if let Ok(asym) = serde_json::from_str::<AsymmetricParamSpace<S::Space>>(&content) {
                (
                    asym.btc.unwrap_or_else(S::default_space),
                    asym.eth.unwrap_or_else(S::default_space),
                    asym.sol.unwrap_or_else(S::default_space),
                    asym.btc_dollar_bars,
                    asym.eth_dollar_bars,
                    asym.sol_dollar_bars,
                )
            } else if let Ok(single) = serde_json::from_str::<S::Space>(&content) {
                (single.clone(), single.clone(), single, None, None, None)
            } else {
                anyhow::bail!("Failed to parse parameter space from {}", path.display());
            }
        } else if Path::new("configs/param_space_asymmetric_spot.json").exists() {
            println!("[*] Loading default asymmetric parameter space: configs/param_space_asymmetric_spot.json");
            let content = fs::read_to_string("configs/param_space_asymmetric_spot.json")?;
            if let Ok(asym) = serde_json::from_str::<AsymmetricParamSpace<S::Space>>(&content) {
                (
                    asym.btc.unwrap_or_else(S::default_space),
                    asym.eth.unwrap_or_else(S::default_space),
                    asym.sol.unwrap_or_else(S::default_space),
                    asym.btc_dollar_bars,
                    asym.eth_dollar_bars,
                    asym.sol_dollar_bars,
                )
            } else {
                (
                    S::default_space(),
                    S::default_space(),
                    S::default_space(),
                    None,
                    None,
                    None,
                )
            }
        } else {
            (
                S::default_space(),
                S::default_space(),
                S::default_space(),
                None,
                None,
                None,
            )
        };

    let btc_thresholds = if let Some(bars) = args.btc_dollar_bars.clone() {
        bars
    } else if let Some(single) = args.btc_dollar_bar {
        vec![single]
    } else if let Some(space_bars) = btc_bars_space {
        space_bars
    } else if args.spot {
        vec![
            dec!(150_000_000),
            dec!(300_000_000),
            dec!(600_000_000),
            dec!(1_200_000_000),
            dec!(2_500_000_000),
            dec!(5_000_000_000),
        ]
    } else {
        vec![
            dec!(10_000_000),
            dec!(25_000_000),
            dec!(50_000_000),
            dec!(100_000_000),
        ]
    };

    let eth_thresholds = if let Some(bars) = args.eth_dollar_bars.clone() {
        bars
    } else if let Some(single) = args.eth_dollar_bar {
        vec![single]
    } else if let Some(space_bars) = eth_bars_space {
        space_bars
    } else if args.spot {
        vec![
            dec!(100_000_000),
            dec!(200_000_000),
            dec!(400_000_000),
            dec!(800_000_000),
            dec!(1_500_000_000),
            dec!(3_000_000_000),
        ]
    } else {
        vec![
            dec!(8_000_000),
            dec!(20_000_000),
            dec!(40_000_000),
            dec!(80_000_000),
        ]
    };

    let sol_thresholds = if let Some(bars) = args.sol_dollar_bars.clone() {
        bars
    } else if let Some(single) = args.sol_dollar_bar {
        vec![single]
    } else if let Some(space_bars) = sol_bars_space {
        space_bars
    } else if args.spot {
        vec![
            dec!(30_000_000),
            dec!(60_000_000),
            dec!(120_000_000),
            dec!(250_000_000),
            dec!(500_000_000),
            dec!(1_000_000_000),
        ]
    } else {
        vec![
            dec!(4_000_000),
            dec!(10_000_000),
            dec!(25_000_000),
            dec!(50_000_000),
        ]
    };

    println!("[1/5] Ingesting multi-asset historical data across resolution grids...");
    let btc_resolutions =
        load_asset_multi_resolution_bars("BTCUSDT", &args.btc_data, &btc_thresholds)?;
    let eth_resolutions =
        load_asset_multi_resolution_bars("ETHUSDT", &args.eth_data, &eth_thresholds)?;
    let sol_resolutions = if args.sol_data.exists() {
        Some(load_asset_multi_resolution_bars(
            "SOLUSDT",
            &args.sol_data,
            &sol_thresholds,
        )?)
    } else {
        None
    };

    let wf_config = WalkForwardConfig {
        num_folds: args.folds,
        train_ratio: args.train_ratio,
        embargo_bars: args.embargo_bars,
    };

    let stage1_n = if args.candidates < 60 {
        args.candidates
    } else {
        args.stage1_candidates
    };
    let top_k = if args.candidates < 60 {
        (args.candidates / 4).clamp(2, 8)
    } else {
        args.top_per_asset
    };

    println!(
        "\n[2/5] Stage 1: Evaluating independent single-asset parameter spaces across resolution grids (Rayon)..."
    );
    println!(
        "      Generating {} candidates per asset in Stage 1...",
        stage1_n
    );
    let btc_candidates = btc_space.generate_candidates(stage1_n);
    let eth_candidates = eth_space.generate_candidates(stage1_n);
    let sol_candidates = sol_space.generate_candidates(stage1_n);

    let stage1_start = Instant::now();
    let (winning_btc_thresh, winning_btc_bars, top_btc) = evaluate_asset_multi_resolution::<S>(
        "BTCUSDT",
        &btc_candidates,
        &btc_resolutions,
        &wf_config,
        args,
        &shared,
        market_type,
        top_k,
    )?;
    let (winning_eth_thresh, winning_eth_bars, top_eth) = evaluate_asset_multi_resolution::<S>(
        "ETHUSDT",
        &eth_candidates,
        &eth_resolutions,
        &wf_config,
        args,
        &shared,
        market_type,
        top_k,
    )?;
    let (winning_sol_thresh, winning_sol_bars, top_sol) =
        if let Some(sol_res) = sol_resolutions.as_ref() {
            evaluate_asset_multi_resolution::<S>(
                "SOLUSDT",
                &sol_candidates,
                sol_res,
                &wf_config,
                args,
                &shared,
                market_type,
                top_k,
            )?
        } else {
            (dec!(0), Vec::new(), vec![S::Config::default()])
        };

    println!(
        "      [OK] Stage 1 finished in {:.2}s (Selected optimal resolutions & Top-{} priors per asset)",
        stage1_start.elapsed().as_secs_f64(),
        top_k
    );

    // 2. Merge winning streams into unified chronological priority queue
    println!("\n[3/5] Merging optimal multi-asset streams into chronological priority queue...");
    let mut merger = EventMerger::new();
    let total_bars = winning_btc_bars.len() + winning_eth_bars.len() + winning_sol_bars.len();

    for bar in &winning_btc_bars {
        merger.push("BTCUSDT", bar);
    }
    for bar in &winning_eth_bars {
        merger.push("ETHUSDT", bar);
    }
    if !winning_sol_bars.is_empty() {
        for bar in &winning_sol_bars {
            merger.push("SOLUSDT", bar);
        }
    }

    let mut all_events: Vec<TaggedPriceEvent> = Vec::with_capacity(total_bars);
    while let Some(e) = merger.pop() {
        all_events.push(e);
    }

    if all_events.is_empty() {
        anyhow::bail!("No price events were ingested across the portfolio assets.");
    }

    println!(
        "      Unified stream: {} total events ({:.1}k BTC, {:.1}k ETH, {:.1}k SOL)",
        all_events.len(),
        winning_btc_bars.len() as f64 / 1000.0,
        winning_eth_bars.len() as f64 / 1000.0,
        winning_sol_bars.len() as f64 / 1000.0
    );

    let folds = WalkForwardSplitter::generate_rolling_folds(all_events.len(), &wf_config)?;
    println!("      Purged & Embargoed temporal splits across portfolio events:");
    for fold in &folds {
        let train_start_dt = chrono::DateTime::from_timestamp_millis(
            all_events[fold.train_range.start].event.start_ms,
        )
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
        let train_end_dt = chrono::DateTime::from_timestamp_millis(
            all_events[fold.train_range.end - 1].event.end_ms,
        )
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
        let test_start_dt = chrono::DateTime::from_timestamp_millis(
            all_events[fold.test_range.start].event.start_ms,
        )
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
        let test_end_dt = chrono::DateTime::from_timestamp_millis(
            all_events[fold.test_range.end - 1].event.end_ms,
        )
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default();

        println!(
            "        Fold #{}: Train [{}..{}] ({} bars, {}..{}) | Embargo ({} bars) | Test [{}..{}] ({} bars, {}..{})",
            fold.fold_idx,
            fold.train_range.start,
            fold.train_range.end,
            fold.train_len(),
            train_start_dt,
            train_end_dt,
            fold.embargo_len(),
            fold.test_range.start,
            fold.test_range.end,
            fold.test_len(),
            test_start_dt,
            test_end_dt
        );
    }

    let ctx = PortfolioEvaluationContext {
        folds: &folds,
        events: &all_events,
        capital: args.capital,
        min_trades_per_fold: args.min_trades_per_fold,
        max_dd_threshold: args.max_dd_threshold,
        market_type,
        bnb_discount: args.bnb_discount,
    };

    // 4. Stage 2: Combinatorial Joint Portfolio Optimization
    let mut joint_candidates = Vec::with_capacity(top_btc.len() * top_eth.len() * top_sol.len());
    for btc_cfg in &top_btc {
        for eth_cfg in &top_eth {
            for sol_cfg in &top_sol {
                joint_candidates.push(JointPortfolioCandidate {
                    btc_config: btc_cfg.clone(),
                    eth_config: eth_cfg.clone(),
                    sol_config: sol_cfg.clone(),
                    risk_per_trade_pct: dec!(0.015),
                    max_open_positions: 3,
                    max_directional_positions: 3,
                    max_slot_margin_pct: dec!(0.35),
                    use_limit_orders: false,
                    limit_timeout_bars: 2,
                });
            }
        }
    }

    println!(
        "\n[4/5] Stage 2: Launching parallel Rayon search across {} joint portfolio configurations...",
        joint_candidates.len()
    );

    let progress = ProgressBar::new(joint_candidates.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) | ETA: {eta} | {msg}")
            .expect("valid template")
            .progress_chars("#>-"),
    );
    progress.set_message("Evaluating multi-asset portfolio space");

    let opt_start = Instant::now();

    let evaluations: Result<Vec<JointPortfolioEvaluation<S::Config>>> = joint_candidates
        .par_iter()
        .map(|candidate| {
            let eval = evaluate_candidate::<S>(candidate, &ctx, &shared)?;
            progress.inc(1);
            Ok(eval)
        })
        .collect();

    let mut evaluations = evaluations?;
    progress.finish_with_message("Portfolio evaluation complete");
    println!(
        "      Evaluated {} configurations in {:.2}s",
        evaluations.len(),
        opt_start.elapsed().as_secs_f64()
    );

    // Sort by eligibility first, then descending by fitness, then descending by OOS Sortino
    evaluations.sort_by(|a, b| {
        b.eligible
            .cmp(&a.eligible)
            .then_with(|| b.fitness.cmp(&a.fitness))
            .then_with(|| b.mean_oos_sortino.cmp(&a.mean_oos_sortino))
    });

    println!("\n=========================================================================================");
    println!(
        " TOP 10 PORTFOLIO CONFIGURATIONS (eligible first, by portfolio fitness)                  "
    );
    println!(
        "========================================================================================="
    );
    println!(
        "Rank | Parameters                           | IS Sort | OOS Sort | Max DD | Div. | Fitness | OOS trds (BTC/ETH/SOL)"
    );
    println!(
        "-----------------------------------------------------------------------------------------"
    );

    for (i, eval) in evaluations.iter().take(10).enumerate() {
        let btc_trds = eval.trades_per_asset.get("BTCUSDT").copied().unwrap_or(0);
        let eth_trds = eval.trades_per_asset.get("ETHUSDT").copied().unwrap_or(0);
        let sol_trds = eval.trades_per_asset.get("SOLUSDT").copied().unwrap_or(0);

        println!(
            "#{:<3} | {:<36} | {:>7.4} | {:>8.4} | {:>5.1}% | {:>4.2} | {:>7.4} | {:>3} ({}/{}/{})",
            i + 1,
            format!(
                "R:{:.1}%|MP:{}|M:{:.0}%|{}",
                eval.candidate.risk_per_trade_pct * dec!(100),
                eval.candidate.max_open_positions,
                eval.candidate.max_slot_margin_pct * dec!(100),
                if eval.candidate.use_limit_orders {
                    "Lim"
                } else {
                    "Mkt"
                },
            ),
            eval.mean_is_sortino,
            eval.mean_oos_sortino,
            eval.max_drawdown_pct * dec!(100),
            eval.diversification_score,
            eval.fitness,
            eval.total_oos_trades,
            btc_trds,
            eth_trds,
            sol_trds
        );
    }
    println!(
        "========================================================================================="
    );

    let winner = evaluations
        .first()
        .context("No portfolio candidates were evaluated")?;

    println!("\n[WINNER] Selected Joint Portfolio Configuration:");
    println!(
        "  • Risk Per Trade:            {:.2}%",
        winner.candidate.risk_per_trade_pct * dec!(100)
    );
    println!(
        "  • Max Open Positions:        {}",
        winner.candidate.max_open_positions
    );
    println!(
        "  • Max Directional Positions: {}",
        winner.candidate.max_directional_positions
    );
    println!(
        "  • Max Slot Margin:           {:.1}%",
        winner.candidate.max_slot_margin_pct * dec!(100)
    );
    println!(
        "  • Execution Mode:            {}",
        if winner.candidate.use_limit_orders {
            "Maker Limit (TIF)"
        } else {
            "Aggressive Market"
        }
    );
    println!(
        "  • BTC Config:                {}",
        winner.candidate.btc_config
    );
    println!(
        "  • ETH Config:                {}",
        winner.candidate.eth_config
    );
    println!(
        "  • SOL Config:                {}",
        winner.candidate.sol_config
    );
    println!(
        "  • Mean In-Sample Sortino:    {:.4}",
        winner.mean_is_sortino
    );
    println!(
        "  • Mean Out-of-Sample Sortino:{:.4}",
        winner.mean_oos_sortino
    );
    println!(
        "  • Out-of-Sample Max DD:      {:.2}%",
        winner.max_drawdown_pct * dec!(100)
    );
    println!(
        "  • Diversification Score:     {:.2}",
        winner.diversification_score
    );
    println!("  • Portfolio Fitness:         {:.4}", winner.fitness);
    println!(
        "  • Total OOS Trades:          {} (BTC: {}, ETH: {}, SOL: {})",
        winner.total_oos_trades,
        winner.trades_per_asset.get("BTCUSDT").copied().unwrap_or(0),
        winner.trades_per_asset.get("ETHUSDT").copied().unwrap_or(0),
        winner.trades_per_asset.get("SOLUSDT").copied().unwrap_or(0),
    );

    // 5. Build and save deployment artifact
    println!("\n[5/5] Building and saving deployment artifact...");
    let (maker_fee_pct, taker_fee_pct, max_leverage) = match (market_type, args.bnb_discount) {
        (MarketType::Spot, true) => (dec!(0.00075), dec!(0.00075), dec!(1)),
        (MarketType::Spot, false) => (dec!(0.0010), dec!(0.0010), dec!(1)),
        (MarketType::UsdMPerpetual, true) => (dec!(0.00018), dec!(0.00045), dec!(20)),
        (MarketType::UsdMPerpetual, false) => (dec!(0.0002), dec!(0.0005), dec!(20)),
    };

    let serialized_btc_config = serde_json::to_value(&winner.candidate.btc_config)?;
    let serialized_eth_config = serde_json::to_value(&winner.candidate.eth_config)?;
    let serialized_sol_config = serde_json::to_value(&winner.candidate.sol_config)?;

    let artifact = PortfolioDeploymentArtifact {
        schema_version: "1.0".to_string(),
        created_at: Utc::now().to_rfc3339(),
        capital: args.capital,
        risk_policy: PortfolioRiskPolicy {
            max_open_positions: winner.candidate.max_open_positions,
            max_directional_positions: winner.candidate.max_directional_positions,
            risk_per_trade_pct: winner.candidate.risk_per_trade_pct,
            max_slot_margin_pct: winner.candidate.max_slot_margin_pct,
            max_leverage,
            min_order_quantity: dec!(0.001),
            quantity_step_size: dec!(0.001),
            min_order_notional: dec!(5),
            ..PortfolioRiskPolicy::default()
        },
        execution: PortfolioExecutionSettings {
            use_limit_orders: winner.candidate.use_limit_orders,
            limit_timeout_bars: winner.candidate.limit_timeout_bars,
            maker_fee_pct,
            taker_fee_pct,
            slippage_pct: dec!(0.0005),
            btc_breadth_filter: true,
        },
        assets: vec![
            PortfolioAssetSpec {
                symbol: "BTCUSDT".to_string(),
                dollar_bar_threshold: winning_btc_thresh,
                strategy_id: S::id().to_string(),
                strategy_config: serialized_btc_config,
            },
            PortfolioAssetSpec {
                symbol: "ETHUSDT".to_string(),
                dollar_bar_threshold: winning_eth_thresh,
                strategy_id: S::id().to_string(),
                strategy_config: serialized_eth_config,
            },
            PortfolioAssetSpec {
                symbol: "SOLUSDT".to_string(),
                dollar_bar_threshold: winning_sol_thresh,
                strategy_id: S::id().to_string(),
                strategy_config: serialized_sol_config,
            },
        ],
        scorecard: PortfolioScorecard {
            mean_is_sortino: winner.mean_is_sortino,
            mean_oos_sortino: winner.mean_oos_sortino,
            oos_sharpe: winner.oos_sharpe,
            oos_profit_factor: winner.oos_profit_factor,
            oos_win_rate_pct: winner.oos_win_rate_pct,
            total_oos_trades: winner.total_oos_trades,
            min_fold_trades: winner.min_fold_trades,
            max_drawdown_pct: winner.max_drawdown_pct,
            diversification_score: winner.diversification_score,
            fitness: winner.fitness,
            trades_per_asset: winner.trades_per_asset.clone(),
        },
    };

    if let Some(parent) = args.output_config.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(&artifact)?;
    fs::write(&args.output_config, serialized).with_context(|| {
        format!(
            "Failed to write artifact to {}",
            args.output_config.display()
        )
    })?;

    println!(
        "\n[SAVED] Portfolio Deployment Artifact saved to: {}",
        args.output_config.display()
    );

    if args.gcs_upload {
        let upload_start = Instant::now();
        let mut sinks = SinkFailures::new(args.strict_sinks);
        let gcs = GcsArtifactSink::from_env()
            .with_strict(args.strict_sinks)
            .with_create_only(true);
        let suffix = artifact_run_suffix(args.artifact_suffix.as_deref(), Utc::now());
        let object_name = artifact_object_name(&args.output_config, &suffix);
        println!("[*] Uploading Portfolio Deployment Artifact to GCS ({object_name})...");
        let rt = tokio::runtime::Runtime::new()?;
        let uploaded =
            rt.block_on(gcs.upload_file(&args.output_config, &object_name, "application/json"));
        sinks.record("upload Portfolio Deployment Artifact", uploaded);
        sinks.finish()?;
        println!(
            "[TIMING] phase=upload_artifact seconds={:.2}",
            upload_start.elapsed().as_secs_f64()
        );
    }

    println!(
        "[TIMING] phase=total seconds={:.2}",
        start_total.elapsed().as_secs_f64()
    );

    Ok(())
}

struct PortfolioHpoRunner<'a> {
    args: &'a Args,
}

impl<'a> StrategyVisitor for PortfolioHpoRunner<'a> {
    type Output = Result<()>;

    fn visit<S>(self) -> Self::Output
    where
        S: OptimizableStrategy + 'static,
        S::Config: Default
            + Serialize
            + for<'de> Deserialize<'de>
            + std::fmt::Display
            + Clone
            + Send
            + Sync,
        S::Space: Serialize + for<'de> Deserialize<'de> + Send + Sync,
    {
        run_joint_portfolio_hpo::<S>(self.args)
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let runner = PortfolioHpoRunner { args: &args };
    match dispatch(&args.strategy, runner) {
        Some(res) => res?,
        None => anyhow::bail!(strategies::unknown_strategy_message(&args.strategy)),
    }
    Ok(())
}
