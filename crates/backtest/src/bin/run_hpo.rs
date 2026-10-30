use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use adapters::{BinanceCsvReader, MarketDataStream, TapeReader};
use anyhow::{Context, Result};
use backtest::{
    bar_tape, calibration_end_ms, check_calibration, embargo_buckets, now_unix_ms, record_hpo,
    trade_tape, BacktestConfig, CandidateEvaluation, DataWindow, FeaturesOf, HpoDeploymentArtifact,
    HpoScorecard, RunContext, RunSpec, RunType, SamplingConfig, StrategyDeploymentArtifact, Tape,
    WalkForwardConfig, WalkForwardFold, WalkForwardOptimizer, WalkForwardSplitter,
    TRADES_WARM_UP_MS,
};
use clap::Parser;
use domain::DollarBarAggregator;
use rust_decimal::Decimal;
use serde_json::json;
use strategies::{InputKind, OptimizableStrategy};

/// Price bucket width of a trade-consuming strategy when `--price-bucket-ms` is not given.
const DEFAULT_PRICE_BUCKET_MS: i64 = 1_000;
/// Embargo of a trade-consuming strategy when `--embargo-ms` is not given: one day.
const DEFAULT_EMBARGO_MS: i64 = 86_400_000;

#[derive(Parser, Debug)]
#[command(
    name = "run_hpo",
    about = "ATSNT - Parallel Walk-Forward Hyperparameter Optimizer (Rayon + Anti-Overfitting)"
)]
struct Args {
    /// Target trading symbol in uppercase (e.g. 'BTCUSDT')
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// Path to historical market data file (.parquet or .csv)
    #[arg(
        short,
        long,
        default_value = "data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet"
    )]
    data: PathBuf,

    /// Dollar bar threshold (e.g. 1000000 for $1M per bar); strategies that consume dollar
    /// bars only (for backwards compatibility)
    #[arg(long)]
    dollar_bar: Option<Decimal>,

    /// Comma-separated list of dollar bar thresholds to evaluate in a multi-resolution sweep
    /// (e.g. "2000000,4000000,6000000")
    #[arg(long, value_delimiter = ',')]
    dollar_bars: Option<Vec<Decimal>>,

    /// Number of Walk-Forward validation folds
    #[arg(short, long, visible_alias = "windows", default_value_t = 3)]
    folds: usize,

    /// Train ratio per window (e.g. 0.70 for 70% In-Sample)
    #[arg(long, default_value_t = 0.70)]
    train_ratio: f64,

    /// Number of embargo bars quarantined between train and test; strategies that consume
    /// dollar bars only (see --embargo-ms)
    #[arg(long, default_value_t = 20)]
    embargo_bars: usize,

    /// Path to the JSON shared configuration (structural, frozen for the whole run) of the
    /// strategy, e.g. the swarm's calibrated feeds written by calibrate_feeds. Required for
    /// strategies that consume trades; stored in the winning artifact
    #[arg(long)]
    shared_config: Option<PathBuf>,

    /// Width in milliseconds of the price buckets the trades are grouped into; strategies that
    /// consume trades only [default: 1000]
    #[arg(long)]
    price_bucket_ms: Option<i64>,

    /// Embargo in milliseconds quarantined between train and test, converted to price buckets
    /// (embargo / bucket); strategies that consume trades only [default: 86400000, one day]
    #[arg(long)]
    embargo_ms: Option<i64>,

    /// Maximum parameter combinations to evaluate
    #[arg(short, long, default_value_t = 60)]
    candidates: usize,

    /// Closed trades a candidate needs in every in-sample and out-of-sample fold to be
    /// ranked; candidates below it are recorded but cannot win
    #[arg(long, default_value_t = backtest::DEFAULT_MIN_TRADES_PER_FOLD)]
    min_trades_per_fold: usize,

    /// Output path for winning configuration JSON
    #[arg(
        short,
        long,
        visible_alias = "output",
        default_value = "configs/hpo_results.json"
    )]
    output_config: PathBuf,

    /// Target strategy identifier to optimize (e.g. 'swarm_consensus')
    #[arg(long, default_value = "swarm_consensus")]
    strategy: String,

    /// Optional path to JSON file overriding the strategy's default parameter search space
    #[arg(long)]
    param_space: Option<PathBuf>,

    /// Optional path to a JSON execution context (initial capital, maker/taker fees,
    /// slippage, risk) used for every simulated trial; defaults to the engine defaults
    #[arg(long)]
    execution_context: Option<PathBuf>,

    /// Initial starting capital in quote currency (overrides execution context, e.g. 200 for 200 USDT)
    #[arg(long)]
    capital: Option<Decimal>,

    /// Target Binance Spot instead of USD-M Perpetual Futures
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Apply Binance BNB fee discount (0.075% maker/taker) when targeting Spot
    #[arg(long, default_value_t = false)]
    bnb_discount: bool,
}

impl Args {
    /// Resolves the list of dollar bar thresholds to evaluate.
    fn resolved_dollar_bars(&self) -> Vec<Decimal> {
        if let Some(bars) = &self.dollar_bars {
            if !bars.is_empty() {
                return bars.clone();
            }
        }
        if let Some(bar) = self.dollar_bar {
            return vec![bar];
        }
        vec![Decimal::new(1_000_000, 0)]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let started_at_ms = now_unix_ms();

    println!("============================================================");
    println!(" ATSNT - Walk-Forward Hyperparameter Optimization (Rayon)  ");
    println!("============================================================");
    let symbol_upper = args.symbol.to_ascii_uppercase();
    println!("Symbol:      {}", symbol_upper);
    println!("Data:        {}", args.data.display());

    let start_total = Instant::now();
    let run = HpoRun {
        args: &args,
        started_at_ms,
        start_total,
        symbol_upper: &symbol_upper,
    };
    match strategies::dispatch(&args.strategy, run) {
        Some(report) => report.await?,
        None => anyhow::bail!(strategies::unknown_strategy_message(&args.strategy)),
    }

    print_phase_timing("total", start_total);
    Ok(())
}

/// Prints a machine-greppable elapsed-time line for one pipeline phase.
fn print_phase_timing(phase: &str, since: Instant) {
    println!(
        "[TIMING] phase={phase} seconds={:.2}",
        since.elapsed().as_secs_f64()
    );
}

/// One HPO run waiting for the registry to say which strategy it optimizes.
struct HpoRun<'a> {
    args: &'a Args,
    started_at_ms: i64,
    start_total: Instant,
    symbol_upper: &'a str,
}

impl<'a> strategies::StrategyVisitor for HpoRun<'a> {
    type Output = std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>>;

    fn visit<S>(self) -> Self::Output
    where
        S: OptimizableStrategy + 'static,
        S::Config: Default,
        S::Space: serde::Serialize + serde::de::DeserializeOwned,
    {
        Box::pin(run_and_report_hpo::<S>(self))
    }
}

/// Opens the market data file as a trade stream (Parquet file or directory, else CSV).
fn open_stream(
    data: &std::path::Path,
) -> Result<Box<dyn MarketDataStream<Error = adapters::AdapterError>>> {
    if data.is_dir() || data.extension().and_then(|s| s.to_str()) == Some("parquet") {
        Ok(Box::new(TapeReader::open(data)?))
    } else {
        let file =
            File::open(data).with_context(|| format!("Failed to open {}", data.display()))?;
        Ok(Box::new(BinanceCsvReader::new(BufReader::new(file))))
    }
}

/// One resolution's prepared input: threshold, tape, temporal splits, sampling config, and data window.
struct ResolutionInput<F> {
    threshold: Decimal,
    tape: Tape<F>,
    folds: Vec<WalkForwardFold>,
    sampling: SamplingConfig,
    data_window: DataWindow,
    unit: &'static str,
    extra_settings: Option<serde_json::Value>,
}

/// The run's input once prepared across all target resolutions (strategy spec §15.1).
struct PreparedInput<F> {
    resolutions: Vec<ResolutionInput<F>>,
    raw_trades: usize,
    embargo: usize,
}

/// Dollar-bar input: ingests trades once, aggregating into multiple resolutions in parallel.
fn prepare_dollar_bars<S: OptimizableStrategy>(
    args: &Args,
    shared: &S::SharedConfig,
    start_total: Instant,
) -> Result<PreparedInput<FeaturesOf<S>>> {
    let thresholds = args.resolved_dollar_bars();
    let thresh_str = thresholds
        .iter()
        .map(|t| format!("${t}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("Dollar Bars: {}", thresh_str);
    println!(
        "Folds:       {} (Train Ratio: {:.0}%, Embargo: {} bars)",
        args.folds,
        args.train_ratio * 100.0,
        args.embargo_bars
    );
    println!(
        "Candidates:  Up to {} per resolution (Total: {})",
        args.candidates,
        args.candidates * thresholds.len()
    );
    println!("------------------------------------------------------------");

    // 1. Ingest Market Data into In-Memory Dollar Bars (Single Pass)
    println!(
        "[1/4] Aggregating market trade stream across {} resolution(s)...",
        thresholds.len()
    );
    let mut stream = open_stream(&args.data)?;

    let mut aggregators = Vec::with_capacity(thresholds.len());
    for &t in &thresholds {
        aggregators.push(
            DollarBarAggregator::new(t)
                .map_err(|e| anyhow::anyhow!("Failed to initialize aggregator for ${t}: {e}"))?,
        );
    }

    let mut bars_list = vec![Vec::with_capacity(16_384); thresholds.len()];
    let mut raw_trades = 0usize;

    while let Some(trade) = stream.next_trade()? {
        raw_trades += 1;
        for (i, agg) in aggregators.iter_mut().enumerate() {
            if let Some(bar) = agg.process_trade(&trade) {
                bars_list[i].push(bar);
            }
        }
    }

    println!(
        "      Ingested {} ticks across {} resolution(s) in {:.2}s",
        raw_trades,
        thresholds.len(),
        start_total.elapsed().as_secs_f64()
    );
    print_phase_timing("ingest", start_total);

    let wf_config = WalkForwardConfig {
        num_folds: args.folds,
        train_ratio: args.train_ratio,
        embargo_bars: args.embargo_bars,
    };

    let mut resolutions = Vec::with_capacity(thresholds.len());
    for (i, &threshold) in thresholds.iter().enumerate() {
        let bars = &bars_list[i];
        if bars.is_empty() {
            anyhow::bail!(
                "No Dollar Bars were emitted for threshold ${threshold}; reduce threshold."
            );
        }
        println!(
            "      • Resolution ${threshold}: Formed {} Dollar Bars",
            bars.len()
        );
        let data_window = DataWindow {
            source: Some(args.data.display().to_string()),
            start_ms: bars.first().map(|b| b.start_time),
            end_ms: bars.last().map(|b| b.end_time),
        };
        let tape = bar_tape::<S>(shared, bars)?;
        let folds = WalkForwardSplitter::generate_rolling_folds(tape.len(), &wf_config)?;
        resolutions.push(ResolutionInput {
            threshold,
            tape,
            folds,
            sampling: SamplingConfig::DollarBar { threshold },
            data_window,
            unit: "bars",
            extra_settings: None,
        });
    }

    Ok(PreparedInput {
        resolutions,
        raw_trades,
        embargo: args.embargo_bars,
    })
}

/// Trade input (strategy spec §15.1–15.2): the trades stream once through the extractor and
/// into price buckets; the calibration check and the warm-up cut apply, and `--embargo-ms` is
/// converted to buckets.
fn prepare_trades<S: OptimizableStrategy>(
    args: &Args,
    shared: &S::SharedConfig,
    start_total: Instant,
) -> Result<PreparedInput<FeaturesOf<S>>> {
    let bucket_ms = args.price_bucket_ms.unwrap_or(DEFAULT_PRICE_BUCKET_MS);
    let embargo_ms = args.embargo_ms.unwrap_or(DEFAULT_EMBARGO_MS);
    let embargo = embargo_buckets(embargo_ms, bucket_ms)?;
    println!("Price Bucket: {} ms", bucket_ms);
    println!(
        "Folds:       {} (Train Ratio: {:.0}%, Embargo: {} ms = {} buckets)",
        args.folds,
        args.train_ratio * 100.0,
        embargo_ms,
        embargo
    );
    println!("Candidates:  Up to {}", args.candidates);
    println!("------------------------------------------------------------");

    println!(
        "[1/4] Streaming trades through the {} extractor into {} ms price buckets...",
        S::id(),
        bucket_ms
    );
    let mut stream = open_stream(&args.data)?;
    let mut read_error = None;
    let mut raw_trades = 0usize;
    let trades = std::iter::from_fn(|| match stream.next_trade() {
        Ok(trade) => trade,
        Err(error) => {
            read_error = Some(error);
            None
        }
    })
    .inspect(|_| raw_trades += 1);
    let tape = trade_tape::<S>(shared, trades, bucket_ms)?;
    if let Some(error) = read_error {
        return Err(error.into());
    }
    let (Some(first), Some(last)) = (tape.prices().first(), tape.prices().last()) else {
        anyhow::bail!("No trades were read from {}", args.data.display());
    };
    let data_window = DataWindow {
        source: Some(args.data.display().to_string()),
        start_ms: Some(first.start_ms),
        end_ms: Some(last.end_ms),
    };
    check_calibration(calibration_end_ms(shared), first.start_ms)?;
    let built = tape.len();
    let tape = tape.after_warm_up(TRADES_WARM_UP_MS)?;
    println!(
        "      Ingested {} ticks -> {} price buckets, {} evaluated after the {}-day warm-up, \
         {} features, in {:.2}s",
        raw_trades,
        built,
        tape.len(),
        TRADES_WARM_UP_MS / 86_400_000,
        tape.features().len(),
        start_total.elapsed().as_secs_f64()
    );
    print_phase_timing("ingest", start_total);

    let wf_config = WalkForwardConfig {
        num_folds: args.folds,
        train_ratio: args.train_ratio,
        embargo_bars: embargo,
    };
    let folds = WalkForwardSplitter::generate_rolling_folds(tape.len(), &wf_config)?;

    Ok(PreparedInput {
        resolutions: vec![ResolutionInput {
            threshold: Decimal::ZERO,
            tape,
            folds,
            sampling: SamplingConfig::Trades {
                price_bucket_ms: Some(bucket_ms),
            },
            data_window,
            unit: "buckets",
            extra_settings: Some(json!({
                "price_bucket_ms": bucket_ms,
                "embargo_ms": embargo_ms,
            })),
        }],
        raw_trades,
        embargo,
    })
}

/// The shared configuration of the run: `--shared-config`, else the strategy's default. A
/// strategy that consumes trades must be given one (its thresholds are calibrated); the trade
/// flags are refused for a strategy that consumes dollar bars.
fn shared_config<S>(args: &Args) -> Result<(S::SharedConfig, InputKind)>
where
    S: OptimizableStrategy,
    S::Config: Default,
{
    let shared = match &args.shared_config {
        Some(path) => {
            let content = fs::read_to_string(path).with_context(|| {
                format!(
                    "Failed to read shared configuration file: {}",
                    path.display()
                )
            })?;
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(shared_val) = value.get("shared_config").filter(|v| !v.is_null()) {
                    serde_json::from_value::<S::SharedConfig>(shared_val.clone()).with_context(
                        || "Failed to parse shared_config from deployment artifact JSON",
                    )?
                } else {
                    serde_json::from_str::<S::SharedConfig>(&content)
                        .with_context(|| "Failed to parse shared configuration JSON")?
                }
            } else {
                serde_json::from_str::<S::SharedConfig>(&content)
                    .with_context(|| "Failed to parse shared configuration JSON")?
            }
        }
        None => S::SharedConfig::default(),
    };
    let strategy_instance = S::from_config(&shared, S::Config::default())
        .map_err(|error| anyhow::anyhow!("Failed to initialize strategy: {error}"))?;
    let target_market = if args.spot {
        strategies::MarketType::Spot
    } else {
        strategies::MarketType::UsdMPerpetual
    };
    if !strategy_instance.supports_market_type(target_market) {
        anyhow::bail!(
            "Strategy '{}' does not support {} (supported: {:?})",
            S::id(),
            if args.spot { "Spot" } else { "USD-M Perpetual" },
            strategy_instance.compatible_market_types()
        );
    }
    let input_kind = strategy_instance.input_kind();
    match input_kind {
        InputKind::Trades if args.shared_config.is_none() => anyhow::bail!(
            "strategy '{}' consumes trades and needs the shared configuration its feeds were \
             calibrated with: pass --shared-config <file> (e.g. the output of calibrate_feeds)",
            S::id()
        ),
        InputKind::DollarBars if args.embargo_ms.is_some() || args.price_bucket_ms.is_some() => {
            anyhow::bail!(
                "--embargo-ms and --price-bucket-ms apply only to strategies that consume \
                 trades; strategy '{}' consumes dollar bars: use --embargo-bars",
                S::id()
            )
        }
        _ => Ok((shared, input_kind)),
    }
}

async fn run_and_report_hpo<S>(run: HpoRun<'_>) -> Result<()>
where
    S: OptimizableStrategy,
    S::Config: Default,
    S::Space: serde::Serialize + serde::de::DeserializeOwned,
{
    let HpoRun {
        args,
        started_at_ms,
        start_total,
        symbol_upper,
    } = run;
    let (shared, input_kind) = shared_config::<S>(args)?;
    let prepared = match input_kind {
        InputKind::DollarBars => prepare_dollar_bars::<S>(args, &shared, start_total)?,
        InputKind::Trades => prepare_trades::<S>(args, &shared, start_total)?,
    };

    // 2. Generate Walk-Forward Folds with Embargo across resolutions
    println!("[2/4] Generating Purged & Embargoed temporal splits across resolutions...");
    for res in &prepared.resolutions {
        let label = if res.threshold > Decimal::ZERO {
            format!("DollarBar(${:.0})", res.threshold)
        } else {
            "Trades".to_string()
        };
        println!(
            "      Resolution [{label}] (Total: {} {}):",
            res.tape.len(),
            res.unit
        );
        for fold in &res.folds {
            println!(
                "        Fold #{}: Train [{}..{}] ({} {}) | Embargo [{}..{}] ({} {}) | Test [{}..{}] ({} {})",
                fold.fold_idx,
                fold.train_range.start,
                fold.train_range.end,
                fold.train_len(),
                res.unit,
                fold.embargo_range.start,
                fold.embargo_range.end,
                fold.embargo_len(),
                res.unit,
                fold.test_range.start,
                fold.test_range.end,
                fold.test_len(),
                res.unit
            );
        }
    }

    // 3. Multi-Core Rayon Optimization
    println!(
        "\n[3/4] Launching parallel Rayon search across {} candidate(s) per resolution...",
        args.candidates
    );
    let opt_start = Instant::now();

    let mut engine_config = match &args.execution_context {
        Some(path) => {
            let content = fs::read_to_string(path).with_context(|| {
                format!("Failed to read execution context file: {}", path.display())
            })?;
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(ctx_val) = value.get("execution_context").filter(|v| !v.is_null()) {
                    serde_json::from_value::<BacktestConfig>(ctx_val.clone())
                        .with_context(|| "Failed to parse execution_context from JSON")?
                } else {
                    serde_json::from_value::<BacktestConfig>(value)
                        .with_context(|| "Failed to parse execution context JSON")?
                }
            } else {
                serde_json::from_str::<BacktestConfig>(&content)
                    .with_context(|| "Failed to parse execution context JSON")?
            }
        }
        None => {
            if let Some(shared_path) = &args.shared_config {
                if let Ok(content) = fs::read_to_string(shared_path) {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(ctx_val) =
                            value.get("execution_context").filter(|v| !v.is_null())
                        {
                            serde_json::from_value::<BacktestConfig>(ctx_val.clone())
                                .unwrap_or_default()
                        } else {
                            BacktestConfig::default()
                        }
                    } else {
                        BacktestConfig::default()
                    }
                } else {
                    BacktestConfig::default()
                }
            } else if args.spot {
                if args.bnb_discount {
                    BacktestConfig::spot_with_bnb_discount()
                } else {
                    BacktestConfig::spot()
                }
            } else {
                BacktestConfig::default()
            }
        }
    };
    if args.spot {
        engine_config.market_type = strategies::MarketType::Spot;
        engine_config.max_leverage = Decimal::ONE;
        engine_config.funding_rate_pct = Decimal::ZERO;
        engine_config.maintenance_margin_rate = Decimal::ZERO;
        if args.bnb_discount {
            engine_config.maker_fee_pct = rust_decimal_macros::dec!(0.00075);
            engine_config.taker_fee_pct = rust_decimal_macros::dec!(0.00075);
        }
    }
    if let Some(cap) = args.capital {
        engine_config.initial_capital = cap;
    }
    println!(
        "      Execution context: capital ${}, maker {} / taker {} fees, slippage {}",
        engine_config.initial_capital,
        engine_config.maker_fee_pct,
        engine_config.taker_fee_pct,
        engine_config.slippage_pct
    );
    println!(
        "      {}",
        engine_config.describe_notional_caps(engine_config.initial_capital)
    );

    let param_space = if let Some(path) = &args.param_space {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read parameter space file: {}", path.display()))?;
        serde_json::from_str::<S::Space>(&content)
            .with_context(|| "Failed to parse parameter space JSON")?
    } else {
        S::default_space()
    };

    let space_json = serde_json::to_value(&param_space)?;
    let shared_json = Some(serde_json::to_value(&shared)?).filter(|value| !value.is_null());
    let optimizer = WalkForwardOptimizer::<S>::new(engine_config, param_space)
        .with_shared_config(shared)
        .with_min_trades_per_fold(args.min_trades_per_fold);

    let mut all_evaluations: Vec<(
        Decimal,
        CandidateEvaluation<S::Config>,
        SamplingConfig,
        DataWindow,
    )> = Vec::new();

    for res in &prepared.resolutions {
        let res_start = Instant::now();
        let evals = optimizer.run_optimization_on_tape(&res.tape, &res.folds, args.candidates)?;
        let label = if res.threshold > Decimal::ZERO {
            format!("DollarBar(${:.0})", res.threshold)
        } else {
            "Trades".to_string()
        };
        println!(
            "      [{label}] Evaluated {} configurations in {:.2}s",
            evals.len(),
            res_start.elapsed().as_secs_f64()
        );
        for eval in evals {
            all_evaluations.push((res.threshold, eval, res.sampling, res.data_window.clone()));
        }
    }

    all_evaluations.sort_by(|a, b| {
        b.1.eligible
            .cmp(&a.1.eligible)
            .then_with(|| b.1.fitness.cmp(&a.1.fitness))
            .then_with(|| b.1.mean_oos_sortino.cmp(&a.1.mean_oos_sortino))
    });

    let winner = all_evaluations.first().filter(|e| e.1.eligible).cloned();
    let eligible_count = all_evaluations.iter().filter(|e| e.1.eligible).count();

    let opt_duration = opt_start.elapsed().as_secs_f64();
    println!(
        "\n      Evaluated {} total configurations across {} resolution(s) in {:.2}s",
        all_evaluations.len(),
        prepared.resolutions.len(),
        opt_duration
    );
    print_phase_timing("search", opt_start);

    // 4. Output Results and Reporting
    println!("\n=========================================================================================");
    println!(
        " TOP 10 CANDIDATES (eligible first, by OOS fitness across all resolutions)               "
    );
    println!(
        "========================================================================================="
    );
    println!(
        "      {} of {} candidates reached {} closed trades in every fold",
        eligible_count,
        all_evaluations.len(),
        args.min_trades_per_fold
    );
    println!(
        "{:<4} | {:<14} | {:<42} | {:>9} | {:>9} | {:>6} | {:>8} | {:>9} | {:>6} | {:>7}",
        "Rank",
        "Resolution",
        "Parameters",
        "IS Sort",
        "OOS Sort",
        "Stab.",
        "Fitness",
        "OOS trds",
        "MinFold",
        "DSR p"
    );
    println!(
        "-----------------------------------------------------------------------------------------"
    );

    for (rank, (thresh, eval, _, _)) in all_evaluations.iter().take(10).enumerate() {
        let res_label = if *thresh > Decimal::ZERO {
            format!("${thresh}")
        } else {
            "Trades".to_string()
        };
        println!(
            "#{:<3} | {:<14} | {:<42} | {:>9.4} | {:>9.4} | {:>6.2} | {:>8.4} | {:>9} | {:>6} | {:>7.4}{}",
            rank + 1,
            res_label,
            eval.config.to_string(),
            eval.mean_is_sortino,
            eval.mean_oos_sortino,
            eval.stability_score,
            eval.fitness,
            eval.total_oos_trades,
            eval.min_fold_trades,
            eval.dsr_p_value,
            if eval.eligible { "" } else { "  (ineligible)" }
        );
    }
    println!(
        "========================================================================================="
    );

    let Some((winner_thresh, winner_eval, winner_sampling, winner_window)) = winner else {
        anyhow::bail!(
            "no candidate reached {} closed trades in every in-sample and out-of-sample fold \
             ({} evaluated across {} resolution(s)): there is no measurable winner; widen the space, \
             add data or lower --min-trades-per-fold",
            args.min_trades_per_fold,
            all_evaluations.len(),
            prepared.resolutions.len()
        );
    };

    println!("\n[WINNER] Selected Robust Parameter Configuration & Resolution:");
    println!("  • Strategy:                  {}", S::id());
    if winner_thresh > Decimal::ZERO {
        println!("  • Winning Dollar Bar:        ${winner_thresh}");
    }
    println!("  • Configuration:             {}", winner_eval.config);
    println!(
        "  • Mean In-Sample Sortino:    {:.2}",
        winner_eval.mean_is_sortino
    );
    println!(
        "  • Mean Out-of-Sample Sortino: {:.2}",
        winner_eval.mean_oos_sortino
    );
    println!(
        "  • Parameter Stability Score: {:.2}",
        winner_eval.stability_score
    );
    println!(
        "  • OOS trades / fewest per fold: {} / {}",
        winner_eval.total_oos_trades, winner_eval.min_fold_trades
    );
    println!(
        "  • OOS Sharpe per trade:      {}",
        backtest::format_ratio(winner_eval.oos_sharpe)
    );
    println!(
        "  • Deflated Sharpe Ratio p:   {:.4} ({})",
        winner_eval.dsr_p_value,
        if winner_eval.is_statistically_significant {
            "significant at 5%"
        } else {
            "not significant: the edge may be selection luck"
        }
    );

    let sweep_spec = RunSpec {
        strategy_id: S::id().to_string(),
        symbol: symbol_upper.to_string(),
        market_type: engine_config.market_type,
        sampling: winner_sampling,
        strategy_config: space_json,
        execution_context: engine_config,
    };

    if !all_evaluations.is_empty() {
        let recorder = backtest::wiring::disabled_recorder();
        let sweep_ctx = RunContext::new(RunType::Hpo, &sweep_spec, started_at_ms);
        let eval_only: Vec<CandidateEvaluation<S::Config>> =
            all_evaluations.iter().map(|e| e.1.clone()).collect();
        let total_bars: usize = prepared.resolutions.iter().map(|r| r.tape.len()).sum();
        let mut settings = json!({
            "folds": args.folds,
            "train_ratio": args.train_ratio,
            "embargo_bars": prepared.embargo,
            "candidates_requested": args.candidates,
            "total_bars": total_bars,
            "raw_trades": prepared.raw_trades,
            "optimization_seconds": opt_duration,
            "resolutions_evaluated": prepared.resolutions.iter().map(|r| r.threshold.to_string()).collect::<Vec<_>>(),
        });
        if let (Some(settings), Some(serde_json::Value::Object(extra))) = (
            settings.as_object_mut(),
            prepared
                .resolutions
                .first()
                .and_then(|r| r.extra_settings.clone()),
        ) {
            settings.extend(extra);
        }
        record_hpo(
            &recorder,
            &sweep_ctx,
            &sweep_spec,
            &winner_window,
            &eval_only,
            settings,
            now_unix_ms(),
        )
        .await?;
    }

    // Serialize Winner to output_config as authoritative StrategyDeploymentArtifact
    let scorecard = HpoScorecard {
        mean_is_sortino: winner_eval.mean_is_sortino,
        mean_oos_sortino: winner_eval.mean_oos_sortino,
        oos_profit_factor: winner_eval.oos_profit_factor,
        oos_sharpe: winner_eval.oos_sharpe,
        total_oos_trades: winner_eval.total_oos_trades,
        min_fold_trades: winner_eval.min_fold_trades,
        stability_score: winner_eval.stability_score,
        fitness: winner_eval.fitness,
        dsr_p_value: winner_eval.dsr_p_value,
        is_statistically_significant: winner_eval.is_statistically_significant,
        trials_evaluated: all_evaluations.len(),
    };

    let deployment_artifact = HpoDeploymentArtifact {
        artifact: StrategyDeploymentArtifact {
            schema_version: "1.0".to_string(),
            spec: sweep_spec.with_config(winner_eval.config.clone()),
            hpo_scorecard: Some(scorecard),
        },
        shared_config: shared_json,
    };

    if let Some(parent) = args.output_config.parent() {
        fs::create_dir_all(parent)?;
    }
    let artifact_json = serde_json::to_string_pretty(&deployment_artifact)?;
    fs::write(&args.output_config, artifact_json)?;
    println!(
        "\n[SAVED] Strategy Deployment Artifact saved to: {}",
        args.output_config.display()
    );
    println!(
        "        • Strategy:     {}",
        deployment_artifact.artifact.spec.strategy_id
    );
    println!(
        "        • Symbol:       {}",
        deployment_artifact.artifact.spec.symbol
    );
    match winner_sampling {
        SamplingConfig::DollarBar { threshold } => {
            println!("        • Sampling:     DollarBar(${})", threshold)
        }
        SamplingConfig::Trades { price_bucket_ms } => println!(
            "        • Sampling:     Trades({} ms price buckets)",
            price_bucket_ms.unwrap_or(DEFAULT_PRICE_BUCKET_MS)
        ),
    }

    Ok(())
}
