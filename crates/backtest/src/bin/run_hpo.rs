use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use adapters::{
    BigQuerySink, BinanceCsvReader, BinanceParquetReader, GcsArtifactSink, MarketDataStream,
};
use anyhow::{Context, Result};
use backtest::{
    artifact_object_name, artifact_run_suffix, bar_tape, calibration_end_ms, check_calibration,
    embargo_buckets, now_unix_ms, record_hpo, trade_tape, BacktestConfig, DataWindow, FeaturesOf,
    HpoDeploymentArtifact, HpoScorecard, RunContext, RunSpec, RunType, SamplingConfig,
    SinkFailures, StrategyDeploymentArtifact, Tape, WalkForwardConfig, WalkForwardOptimizer,
    WalkForwardSplitter, TRADES_WARM_UP_MS,
};
use chrono::Utc;
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
    /// bars only
    #[arg(long, default_value = "1000000")]
    dollar_bar: Decimal,

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

    /// Target strategy identifier to optimize (e.g. 'dollar_bars_cusum')
    #[arg(long, default_value = "dollar_bars_cusum")]
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

    /// Record the sweep (hpo parent run plus one hpo_trial run per candidate with
    /// per-fold metrics) to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Upload the winning Strategy Deployment Artifact to Google Cloud Storage as
    /// `artifacts/<output-config stem>-<artifact suffix>.json`
    #[arg(long, default_value_t = false)]
    gcs_upload: bool,

    /// Unique suffix for the uploaded artifact name (e.g. the Cloud Batch job name);
    /// defaults to the UTC timestamp `YYYYMMDDTHHMMSSZ`
    #[arg(long)]
    artifact_suffix: Option<String>,

    /// Exit non-zero if any BigQuery recording or GCS upload fails (or no winning
    /// artifact can be uploaded); failures are reported after all local work finishes
    #[arg(long, default_value_t = false)]
    strict_sinks: bool,
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
        Ok(Box::new(BinanceParquetReader::open(data)?))
    } else {
        let file =
            File::open(data).with_context(|| format!("Failed to open {}", data.display()))?;
        Ok(Box::new(BinanceCsvReader::new(BufReader::new(file))))
    }
}

/// The run's input once prepared for the strategy's input kind (strategy spec §15.1).
struct PreparedInput<F> {
    tape: Tape<F>,
    raw_trades: usize,
    embargo: usize,
    sampling: SamplingConfig,
    data_window: DataWindow,
    /// Fold unit in the console output.
    unit: &'static str,
    /// Settings recorded only for strategies that consume trades.
    extra_settings: Option<serde_json::Value>,
}

/// Dollar-bar input: today's path. The bars are the tape, and `--embargo-bars` counts bars.
fn prepare_dollar_bars<S: OptimizableStrategy>(
    args: &Args,
    shared: &S::SharedConfig,
    start_total: Instant,
) -> Result<PreparedInput<FeaturesOf<S>>> {
    println!("Dollar Bar:  ${}", args.dollar_bar);
    println!(
        "Folds:       {} (Train Ratio: {:.0}%, Embargo: {} bars)",
        args.folds,
        args.train_ratio * 100.0,
        args.embargo_bars
    );
    println!("Candidates:  Up to {}", args.candidates);
    println!("------------------------------------------------------------");

    // 1. Ingest Market Data into In-Memory Dollar Bars
    println!("[1/4] Aggregating market trade stream into Dollar Bars...");
    let mut stream = open_stream(&args.data)?;

    let mut aggregator = DollarBarAggregator::new(args.dollar_bar)
        .map_err(|e| anyhow::anyhow!("Failed to initialize aggregator: {e}"))?;

    let mut bars = Vec::with_capacity(16_384);
    let mut raw_trades = 0usize;

    while let Some(trade) = stream.next_trade()? {
        raw_trades += 1;
        if let Some(bar) = aggregator.process_trade(&trade) {
            bars.push(bar);
        }
    }

    println!(
        "      Ingested {} ticks -> Formed {} Dollar Bars in {:.2}s",
        raw_trades,
        bars.len(),
        start_total.elapsed().as_secs_f64()
    );
    print_phase_timing("ingest", start_total);

    if bars.is_empty() {
        anyhow::bail!("No Dollar Bars were emitted; reduce --dollar-bar threshold.");
    }

    let data_window = DataWindow {
        source: Some(args.data.display().to_string()),
        start_ms: bars.first().map(|b| b.start_time),
        end_ms: bars.last().map(|b| b.end_time),
    };
    Ok(PreparedInput {
        tape: bar_tape::<S>(shared, &bars)?,
        raw_trades,
        embargo: args.embargo_bars,
        sampling: SamplingConfig::DollarBar {
            threshold: args.dollar_bar,
        },
        data_window,
        unit: "bars",
        extra_settings: None,
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

    Ok(PreparedInput {
        tape,
        raw_trades,
        embargo,
        sampling: SamplingConfig::Trades {
            price_bucket_ms: Some(bucket_ms),
        },
        data_window,
        unit: "buckets",
        extra_settings: Some(json!({
            "price_bucket_ms": bucket_ms,
            "embargo_ms": embargo_ms,
        })),
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
            serde_json::from_str::<S::SharedConfig>(&content)
                .with_context(|| "Failed to parse shared configuration JSON")?
        }
        None => S::SharedConfig::default(),
    };
    let input_kind = S::from_config(&shared, S::Config::default())
        .map_err(|error| anyhow::anyhow!("Failed to initialize strategy: {error}"))?
        .input_kind();
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
    let PreparedInput {
        tape,
        raw_trades,
        embargo,
        sampling,
        data_window,
        unit,
        extra_settings,
    } = match input_kind {
        InputKind::DollarBars => prepare_dollar_bars::<S>(args, &shared, start_total)?,
        InputKind::Trades => prepare_trades::<S>(args, &shared, start_total)?,
    };

    // 2. Generate Walk-Forward Folds with Embargo
    println!("[2/4] Generating Purged & Embargoed temporal splits...");
    let wf_config = WalkForwardConfig {
        num_folds: args.folds,
        train_ratio: args.train_ratio,
        embargo_bars: embargo,
    };
    let folds = WalkForwardSplitter::generate_rolling_folds(tape.len(), &wf_config)?;
    let folds = folds.as_slice();

    for fold in folds {
        println!(
            "      Fold #{}: Train [{}..{}] ({} {unit}) | Embargo [{}..{}] ({} {unit}) | Test [{}..{}] ({} {unit})",
            fold.fold_idx,
            fold.train_range.start,
            fold.train_range.end,
            fold.train_len(),
            fold.embargo_range.start,
            fold.embargo_range.end,
            fold.embargo_len(),
            fold.test_range.start,
            fold.test_range.end,
            fold.test_len()
        );
    }

    // 3. Multi-Core Rayon Optimization
    println!(
        "[3/4] Launching parallel Rayon search across {} candidate configurations...",
        args.candidates
    );
    let opt_start = Instant::now();

    let mut engine_config = match &args.execution_context {
        Some(path) => {
            let content = fs::read_to_string(path).with_context(|| {
                format!("Failed to read execution context file: {}", path.display())
            })?;
            serde_json::from_str::<BacktestConfig>(&content)
                .with_context(|| "Failed to parse execution context JSON")?
        }
        None => BacktestConfig::default(),
    };
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
    // `()` (a dollar-bar strategy) serializes to `null`: nothing to store.
    let shared_json = Some(serde_json::to_value(&shared)?).filter(|value| !value.is_null());
    let optimizer = WalkForwardOptimizer::<S>::new(engine_config, param_space)
        .with_shared_config(shared)
        .with_min_trades_per_fold(args.min_trades_per_fold);
    let evaluations = optimizer.run_optimization_on_tape(&tape, folds, args.candidates)?;
    // Ranked eligible first: the first one is the winner only when it reached the minimum.
    let winner = evaluations.first().filter(|e| e.eligible).cloned();
    let eligible_count = evaluations.iter().filter(|e| e.eligible).count();

    let opt_duration = opt_start.elapsed().as_secs_f64();
    println!(
        "      Evaluated {} configurations across {} folds in {:.2}s ({:.1} backtests/sec)",
        evaluations.len(),
        folds.len(),
        opt_duration,
        (evaluations.len() * folds.len() * 2) as f64 / opt_duration
    );
    print_phase_timing("search", opt_start);

    // 4. Output Results and Reporting
    println!("\n============================================================");
    println!(" TOP 5 CANDIDATES (eligible first, by OOS fitness)          ");
    println!("============================================================");
    println!(
        "      {} of {} candidates reached {} closed trades in every fold",
        eligible_count,
        evaluations.len(),
        args.min_trades_per_fold
    );
    println!(
        "{:<4} | {:<45} | {:>9} | {:>9} | {:>6} | {:>8} | {:>9} | {:>6} | {:>7}",
        "Rank",
        "Parameters",
        "IS Sort",
        "OOS Sort",
        "Stab.",
        "Fitness",
        "OOS trds",
        "MinFold",
        "DSR p"
    );
    println!("------------------------------------------------------------");

    for (rank, eval) in evaluations.iter().take(5).enumerate() {
        println!(
            "#{:<3} | {:<45} | {:>9.4} | {:>9.4} | {:>6.2} | {:>8.4} | {:>9} | {:>6} | {:>7.4}{}",
            rank + 1,
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
    println!("============================================================");

    let sweep_spec = RunSpec {
        strategy_id: S::id().to_string(),
        symbol: symbol_upper.to_string(),
        market_type: strategies::MarketType::UsdMPerpetual,
        sampling,
        strategy_config: space_json,
        execution_context: engine_config,
    };

    let mut sinks = SinkFailures::new(args.strict_sinks);

    if args.gcp_bigquery && !evaluations.is_empty() {
        let bq = BigQuerySink::from_env().with_strict(args.strict_sinks);
        let sweep_ctx = RunContext::new(RunType::Hpo, &sweep_spec, started_at_ms);
        let mut settings = json!({
            "folds": folds.len(),
            "train_ratio": args.train_ratio,
            "embargo_bars": embargo,
            "candidates_requested": args.candidates,
            "total_bars": tape.len(),
            "raw_trades": raw_trades,
            "optimization_seconds": opt_duration,
        });
        if let (Some(settings), Some(serde_json::Value::Object(extra))) =
            (settings.as_object_mut(), extra_settings)
        {
            settings.extend(extra);
        }
        let record_start = Instant::now();
        println!(
            "[*] Recording HPO run {} with {} trial runs to BigQuery...",
            sweep_ctx.run_id,
            evaluations.len()
        );
        let recorded = record_hpo(
            &bq,
            &sweep_ctx,
            &sweep_spec,
            &data_window,
            &evaluations,
            settings,
        )
        .await
        .map(|_| ());
        sinks.record("record HPO run", recorded);
        print_phase_timing("record_bigquery", record_start);
    }

    let Some(winner) = &winner else {
        // The trials are recorded above; without an eligible one there is nothing to deploy,
        // and an artifact here would carry a ratio measured on a handful of trades.
        sinks.finish()?;
        anyhow::bail!(
            "no candidate reached {} closed trades in every in-sample and out-of-sample fold \
             ({} evaluated): there is no measurable winner; widen the space, add data or lower \
             --min-trades-per-fold",
            args.min_trades_per_fold,
            evaluations.len()
        );
    };
    println!("\n[WINNER] Selected Robust Parameter Configuration:");
    println!("  • Strategy:                  {}", S::id());
    println!("  • Configuration:             {}", winner.config);
    println!(
        "  • Mean In-Sample Sortino:    {:.2}",
        winner.mean_is_sortino
    );
    println!(
        "  • Mean Out-of-Sample Sortino: {:.2}",
        winner.mean_oos_sortino
    );
    println!(
        "  • Parameter Stability Score: {:.2}",
        winner.stability_score
    );
    println!(
        "  • OOS trades / fewest per fold: {} / {}",
        winner.total_oos_trades, winner.min_fold_trades
    );
    println!(
        "  • OOS Sharpe per trade:      {}",
        backtest::format_ratio(winner.oos_sharpe)
    );
    println!(
        "  • Deflated Sharpe Ratio p:   {:.4} ({})",
        winner.dsr_p_value,
        if winner.is_statistically_significant {
            "significant at 5%"
        } else {
            "not significant: the edge may be selection luck"
        }
    );

    // Serialize Winner to output_config as authoritative StrategyDeploymentArtifact
    let scorecard = HpoScorecard {
        mean_is_sortino: winner.mean_is_sortino,
        mean_oos_sortino: winner.mean_oos_sortino,
        oos_profit_factor: winner.oos_profit_factor,
        oos_sharpe: winner.oos_sharpe,
        total_oos_trades: winner.total_oos_trades,
        min_fold_trades: winner.min_fold_trades,
        stability_score: winner.stability_score,
        fitness: winner.fitness,
        dsr_p_value: winner.dsr_p_value,
        is_statistically_significant: winner.is_statistically_significant,
        trials_evaluated: evaluations.len(),
    };

    let deployment_artifact = HpoDeploymentArtifact {
        artifact: StrategyDeploymentArtifact {
            schema_version: "1.0".to_string(),
            spec: sweep_spec.with_config(winner.config.clone()),
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
    match sampling {
        SamplingConfig::DollarBar { threshold } => {
            println!("        • Sampling:     DollarBar(${})", threshold)
        }
        SamplingConfig::Trades { price_bucket_ms } => println!(
            "        • Sampling:     Trades({} ms price buckets)",
            price_bucket_ms.unwrap_or(DEFAULT_PRICE_BUCKET_MS)
        ),
    }

    if args.gcs_upload {
        let upload_start = Instant::now();
        // Create-only: a retried Batch task re-uploading the same name keeps the
        // first attempt's artifact instead of failing on overwrite.
        let gcs = GcsArtifactSink::from_env()
            .with_strict(args.strict_sinks)
            .with_create_only(true);
        let suffix = artifact_run_suffix(args.artifact_suffix.as_deref(), Utc::now());
        let object_name = artifact_object_name(&args.output_config, &suffix);
        println!("[*] Uploading Strategy Deployment Artifact to GCS ({object_name})...");
        let uploaded = gcs
            .upload_file(&args.output_config, &object_name, "application/json")
            .await;
        sinks.record("upload Strategy Deployment Artifact", uploaded);
        print_phase_timing("upload_artifact", upload_start);
    }

    sinks.finish()?;
    Ok(())
}
