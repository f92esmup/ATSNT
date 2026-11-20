// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use adapters::{BinanceCsvReader, TapeReader};
use anyhow::{Context, Result};
use backtest::{
    check_calibration, embargo_buckets, now_unix_ms, record_hpo, trade_tape, BacktestConfig,
    DataWindow, FeaturesOf, HpoDeploymentArtifact, HpoScorecard, RunContext, RunSpec, RunType,
    SamplingConfig, StrategyDeploymentArtifact, Tape, WalkForwardConfig, WalkForwardFold,
    WalkForwardOptimizer, WalkForwardSplitter,
};
use clap::Parser;
use ports::MarketDataStream;
use rust_decimal::Decimal;
use serde_json::json;
use strategies::OptimizableStrategy;

/// Price bucket width when `--price-bucket-ms` is not given.
const DEFAULT_PRICE_BUCKET_MS: i64 = 1_000;
/// Embargo when `--embargo-ms` is not given: one day.
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

    /// One instrument's trades as SYMBOL=PATH; repeat it to optimise one per-instrument strategy
    /// on several instruments with one account. When given, --data and --symbol are not used
    #[arg(long)]
    instrument_data: Vec<String>,

    /// Most positions open at once across instruments when optimising on several [default: 1]
    #[arg(long)]
    max_open_positions: Option<usize>,

    /// Number of Walk-Forward validation folds
    #[arg(short, long, visible_alias = "windows", default_value_t = 3)]
    folds: usize,

    /// Train ratio per window (e.g. 0.70 for 70% In-Sample)
    #[arg(long, default_value_t = 0.70)]
    train_ratio: f64,

    /// Path to the JSON shared configuration (structural, frozen for the whole run) of the
    /// strategy, e.g. the swarm's calibrated feeds written by calibrate_feeds. Required by a
    /// strategy that calibrates it (the swarm); stored in the winning artifact
    #[arg(long)]
    shared_config: Option<PathBuf>,

    /// Width in milliseconds of the price buckets the trades are grouped into [default: 1000]
    #[arg(long)]
    price_bucket_ms: Option<i64>,

    /// Embargo in milliseconds quarantined between train and test, converted to price buckets
    /// (embargo / bucket) [default: 86400000, one day]
    #[arg(long)]
    embargo_ms: Option<i64>,

    /// Maximum parameter combinations to evaluate
    #[arg(short, long, default_value_t = 60)]
    candidates: usize,

    /// Closed trades a candidate needs in every in-sample and out-of-sample fold to be
    /// ranked; candidates below it are recorded but cannot win
    #[arg(long, default_value_t = backtest::DEFAULT_MIN_TRADES_PER_FOLD)]
    min_trades_per_fold: usize,

    /// Output path for the winning configuration JSON [default: configs/hpo_<strategy>_results.json]
    #[arg(short, long, visible_alias = "output")]
    output_config: Option<PathBuf>,

    /// Strategy to optimize: a registered id or alias (e.g. 'swarm_consensus')
    #[arg(long)]
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

    /// Target Spot instead of USD-M Perpetual Futures
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Write the sweep's run analytics to this SQLite file, created if missing
    #[arg(long)]
    record: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let started_at_ms = now_unix_ms();

    println!("============================================================");
    println!(" ATSNT - Walk-Forward Hyperparameter Optimization (Rayon)  ");
    println!("============================================================");
    let symbol_upper = if args.instrument_data.is_empty() {
        args.symbol.to_ascii_uppercase()
    } else {
        let mut symbols = Vec::new();
        for spec in &args.instrument_data {
            symbols.push(parse_instrument_data(spec)?.0.to_string());
        }
        symbols.sort();
        symbols.join(",")
    };
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

/// `SYMBOL=PATH` of `--instrument-data`.
fn parse_instrument_data(spec: &str) -> Result<(domain::InstrumentId, PathBuf)> {
    let Some((symbol, path)) = spec.split_once('=') else {
        anyhow::bail!("--instrument-data {spec}: expected SYMBOL=PATH");
    };
    let instrument = domain::InstrumentId::new(symbol.to_uppercase())
        .with_context(|| format!("--instrument-data {spec}: invalid symbol"))?;
    Ok((instrument, PathBuf::from(path)))
}

/// The run's input once prepared (strategy spec §15.1): the tape, its temporal splits, the
/// sampling it declares and the window of data it covers.
struct PreparedInput<F> {
    tape: Tape<F>,
    folds: Vec<WalkForwardFold>,
    sampling: SamplingConfig,
    data_window: DataWindow,
    extra_settings: serde_json::Value,
    raw_trades: usize,
    embargo: usize,
}

/// The input (strategy spec §15.1–15.2): the trades stream once through the extractor and
/// into price buckets; the calibration check and the warm-up cut apply, and `--embargo-ms` is
/// converted to buckets.
fn prepare<S: OptimizableStrategy>(
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
    let mut raw_trades = 0usize;
    let mut tape_of = |path: &std::path::Path| -> Result<Tape<FeaturesOf<S>>> {
        let mut stream = open_stream(path)?;
        let mut read_error = None;
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
        Ok(tape)
    };
    let (tape, source) = if args.instrument_data.is_empty() {
        (tape_of(&args.data)?, args.data.display().to_string())
    } else {
        let mut tapes = Vec::new();
        for spec in &args.instrument_data {
            let (instrument, path) = parse_instrument_data(spec)?;
            println!("      {instrument}: {}", path.display());
            tapes.push((instrument, tape_of(&path)?));
        }
        (
            Tape::merge_instruments(tapes),
            args.instrument_data.join(","),
        )
    };
    let (Some(first), Some(last)) = (tape.prices().first(), tape.prices().last()) else {
        anyhow::bail!("No trades were read from {source}");
    };
    let data_window = DataWindow {
        source: Some(source),
        start_ms: Some(first.start_ms),
        end_ms: Some(last.end_ms),
    };
    check_calibration(S::calibration_end_ms(shared), first.start_ms)?;
    let built = tape.len();
    let warm_up_ms = S::warm_up_ms(shared);
    let tape = tape.after_warm_up(warm_up_ms)?;
    println!(
        "      Ingested {} ticks -> {} price buckets, {} evaluated after a warm-up of {} ms, \
         {} features, in {:.2}s",
        raw_trades,
        built,
        tape.len(),
        warm_up_ms,
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
        tape,
        folds,
        sampling: SamplingConfig::Trades {
            price_bucket_ms: Some(bucket_ms),
        },
        data_window,
        extra_settings: json!({
            "price_bucket_ms": bucket_ms,
            "embargo_ms": embargo_ms,
        }),
        raw_trades,
        embargo,
    })
}

/// The shared configuration of the run: `--shared-config`, or the strategy's default. A
/// strategy whose default would mislead ([`OptimizableStrategy::requires_shared_config`], the
/// swarm's uncalibrated feed thresholds) must be given one.
fn shared_config<S>(args: &Args) -> Result<S::SharedConfig>
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
    let target_market = if args.spot {
        strategies::MarketType::Spot
    } else {
        strategies::MarketType::UsdMPerpetual
    };
    let shared = S::shared_for_market(shared, target_market);
    let strategy_instance = S::from_config(&shared, S::Config::default())
        .map_err(|error| anyhow::anyhow!("Failed to initialize strategy: {error}"))?;
    if !strategy_instance.supports_market_type(target_market) {
        anyhow::bail!(
            "Strategy '{}' does not support {} (supported: {:?})",
            S::id(),
            if args.spot { "Spot" } else { "USD-M Perpetual" },
            strategy_instance.compatible_market_types()
        );
    }
    if args.shared_config.is_none() && S::requires_shared_config() {
        anyhow::bail!(
            "strategy '{}' needs the shared configuration it was calibrated with: pass \
             --shared-config <file> (for the swarm, the output of calibrate_feeds)",
            S::id()
        );
    }
    Ok(shared)
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
    let shared = shared_config::<S>(args)?;
    let prepared = prepare::<S>(args, &shared, start_total)?;

    // 2. Generate Walk-Forward Folds with Embargo
    println!("[2/4] Generating Purged & Embargoed temporal splits...");
    println!("      Trades (Total: {} buckets):", prepared.tape.len());
    for fold in &prepared.folds {
        println!(
            "        Fold #{}: Train [{}..{}] ({} buckets) | Embargo [{}..{}] ({} buckets) | Test [{}..{}] ({} buckets)",
            fold.fold_idx,
            fold.train_range.start,
            fold.train_range.end,
            fold.train_len(),
            fold.embargo_range.start,
            fold.embargo_range.end,
            fold.embargo_len(),
            fold.test_range.start,
            fold.test_range.end,
            fold.test_len(),
        );
    }

    // 3. Multi-Core Rayon Optimization
    println!(
        "\n[3/4] Launching parallel Rayon search across {} candidate(s)...",
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
                BacktestConfig::spot()
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
    }
    if let Some(cap) = args.capital {
        engine_config.initial_capital = cap;
    }
    engine_config.max_open_positions = args.max_open_positions.or(engine_config.max_open_positions);
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
        .with_min_trades_per_fold(args.min_trades_per_fold)
        .with_progress(std::sync::Arc::new(
            backtest::wiring::TerminalProgress::default(),
        ));

    let res_start = Instant::now();
    let mut all_evaluations =
        optimizer.run_optimization_on_tape(&prepared.tape, &prepared.folds, args.candidates)?;
    println!(
        "      [Trades] Evaluated {} configurations in {:.2}s",
        all_evaluations.len(),
        res_start.elapsed().as_secs_f64()
    );

    all_evaluations.sort_by(|a, b| {
        b.eligible
            .cmp(&a.eligible)
            .then_with(|| b.fitness.cmp(&a.fitness))
            .then_with(|| b.mean_oos_sortino.cmp(&a.mean_oos_sortino))
    });

    let winner = all_evaluations.first().filter(|e| e.eligible).cloned();
    let eligible_count = all_evaluations.iter().filter(|e| e.eligible).count();

    let opt_duration = opt_start.elapsed().as_secs_f64();
    println!(
        "\n      Evaluated {} total configurations in {:.2}s",
        all_evaluations.len(),
        opt_duration
    );
    print_phase_timing("search", opt_start);

    // 4. Output Results and Reporting
    println!("\n=========================================================================================");
    println!(
        " TOP 10 CANDIDATES (eligible first, by OOS fitness)                                         "
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
        "{:<4} | {:<42} | {:>9} | {:>9} | {:>6} | {:>8} | {:>9} | {:>6} | {:>7}",
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
    println!(
        "-----------------------------------------------------------------------------------------"
    );

    for (rank, eval) in all_evaluations.iter().take(10).enumerate() {
        println!(
            "#{:<3} | {:<42} | {:>9.4} | {:>9.4} | {:>6.2} | {:>8.4} | {:>9} | {:>6} | {:>7.4}{}",
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
    println!(
        "========================================================================================="
    );

    let Some(winner_eval) = winner else {
        anyhow::bail!(
            "no candidate reached {} closed trades in every in-sample and out-of-sample fold \
             ({} evaluated): there is no measurable winner; widen the space, \
             add data or lower --min-trades-per-fold",
            args.min_trades_per_fold,
            all_evaluations.len()
        );
    };

    println!("\n[WINNER] Selected Robust Parameter Configuration:");
    println!("  • Strategy:                  {}", S::id());
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
        sampling: prepared.sampling,
        strategy_config: space_json,
        execution_context: engine_config,
    };

    if !all_evaluations.is_empty() {
        let recorder = backtest::wiring::recorder(args.record.as_deref())?;
        let sweep_ctx = RunContext::new(
            RunType::Hpo,
            &sweep_spec,
            started_at_ms,
            &backtest::wiring::entropy(),
        );
        let mut settings = json!({
            "folds": args.folds,
            "train_ratio": args.train_ratio,
            "embargo_bars": prepared.embargo,
            "candidates_requested": args.candidates,
            "total_bars": prepared.tape.len(),
            "raw_trades": prepared.raw_trades,
            "optimization_seconds": opt_duration,
        });
        if let (Some(settings), serde_json::Value::Object(extra)) =
            (settings.as_object_mut(), prepared.extra_settings.clone())
        {
            settings.extend(extra);
        }
        record_hpo(
            &recorder,
            &sweep_ctx,
            &sweep_spec,
            &prepared.data_window,
            backtest::HpoSearch {
                evaluations: &all_evaluations,
                settings,
            },
            now_unix_ms(),
            &backtest::wiring::entropy(),
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
            // 2.0 marks an artifact optimised on several instruments: its symbol lists them.
            schema_version: if args.instrument_data.is_empty() {
                "1.0"
            } else {
                "2.0"
            }
            .to_string(),
            spec: sweep_spec.with_config(winner_eval.config.clone()),
            hpo_scorecard: Some(scorecard),
        },
        shared_config: shared_json,
    };

    let output_config = args
        .output_config
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("configs/hpo_{}_results.json", S::id())));
    if let Some(parent) = output_config.parent() {
        fs::create_dir_all(parent)?;
    }
    let artifact_json = serde_json::to_string_pretty(&deployment_artifact)?;
    fs::write(&output_config, artifact_json)?;
    println!(
        "\n[SAVED] Strategy Deployment Artifact saved to: {}",
        output_config.display()
    );
    println!(
        "        • Strategy:     {}",
        deployment_artifact.artifact.spec.strategy_id
    );
    println!(
        "        • Symbol:       {}",
        deployment_artifact.artifact.spec.symbol
    );
    match prepared.sampling {
        SamplingConfig::Trades { price_bucket_ms } => println!(
            "        • Sampling:     Trades({} ms price buckets)",
            price_bucket_ms.unwrap_or(DEFAULT_PRICE_BUCKET_MS)
        ),
    }

    Ok(())
}
