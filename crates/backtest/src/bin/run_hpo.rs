use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use adapters::{
    BigQuerySink, BinanceCsvReader, BinanceParquetReader, GcsParquetSink, MarketDataStream,
};
use anyhow::{Context, Result};
use backtest::{
    now_unix_ms, record_hpo, BacktestConfig, DataWindow, HpoScorecard, RunContext, RunSpec,
    RunType, SamplingConfig, StrategyDeploymentArtifact, WalkForwardConfig, WalkForwardOptimizer,
    WalkForwardSplitter,
};
use clap::Parser;
use domain::DollarBarAggregator;
use rust_decimal::Decimal;
use serde_json::json;
use strategies::OptimizableStrategy;

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

    /// Dollar bar threshold (e.g. 1000000 for $1M per bar)
    #[arg(long, default_value = "1000000")]
    dollar_bar: Decimal,

    /// Number of Walk-Forward validation folds
    #[arg(short, long, visible_alias = "windows", default_value_t = 3)]
    folds: usize,

    /// Train ratio per window (e.g. 0.70 for 70% In-Sample)
    #[arg(long, default_value_t = 0.70)]
    train_ratio: f64,

    /// Number of embargo bars quarantined between train and test
    #[arg(long, default_value_t = 20)]
    embargo_bars: usize,

    /// Maximum parameter combinations to evaluate
    #[arg(short, long, default_value_t = 60)]
    candidates: usize,

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

    /// Record the sweep (hpo parent run plus one hpo_trial run per candidate with
    /// per-fold metrics) to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Upload the winning Strategy Deployment Artifact to Google Cloud Storage
    #[arg(long, default_value_t = false)]
    gcs_upload: bool,
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
    println!("Dollar Bar:  ${}", args.dollar_bar);
    println!(
        "Folds:       {} (Train Ratio: {:.0}%, Embargo: {} bars)",
        args.folds,
        args.train_ratio * 100.0,
        args.embargo_bars
    );
    println!("Candidates:  Up to {}", args.candidates);
    println!("------------------------------------------------------------");

    let start_total = Instant::now();

    // 1. Ingest Market Data into In-Memory Dollar Bars
    println!("[1/4] Aggregating market trade stream into Dollar Bars...");
    let mut stream: Box<dyn MarketDataStream<Error = adapters::AdapterError>> = if args
        .data
        .is_dir()
        || args.data.extension().and_then(|s| s.to_str()) == Some("parquet")
    {
        Box::new(BinanceParquetReader::open(&args.data)?)
    } else {
        let file = File::open(&args.data)
            .with_context(|| format!("Failed to open {}", args.data.display()))?;
        Box::new(BinanceCsvReader::new(BufReader::new(file)))
    };

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

    if bars.is_empty() {
        anyhow::bail!("No Dollar Bars were emitted; reduce --dollar-bar threshold.");
    }

    // 2. Generate Walk-Forward Folds with Embargo
    println!("[2/4] Generating Purged & Embargoed temporal splits...");
    let wf_config = WalkForwardConfig {
        num_folds: args.folds,
        train_ratio: args.train_ratio,
        embargo_bars: args.embargo_bars,
    };
    let folds = WalkForwardSplitter::generate_rolling_folds(bars.len(), &wf_config)?;

    for fold in &folds {
        println!(
            "      Fold #{}: Train [{}..{}] ({} bars) | Embargo [{}..{}] ({} bars) | Test [{}..{}] ({} bars)",
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

    let engine_config = match &args.execution_context {
        Some(path) => {
            let content = fs::read_to_string(path).with_context(|| {
                format!("Failed to read execution context file: {}", path.display())
            })?;
            serde_json::from_str::<BacktestConfig>(&content)
                .with_context(|| "Failed to parse execution context JSON")?
        }
        None => BacktestConfig::default(),
    };
    println!(
        "      Execution context: capital ${}, maker {} / taker {} fees, slippage {}",
        engine_config.initial_capital,
        engine_config.maker_fee_pct,
        engine_config.taker_fee_pct,
        engine_config.slippage_pct
    );

    let (evaluations, winner, strategy_id, search_space) = match args.strategy.as_str() {
        "dollar_bars_cusum" | "cusum" => {
            let param_space = if let Some(path) = &args.param_space {
                let content = fs::read_to_string(path).with_context(|| {
                    format!("Failed to read parameter space file: {}", path.display())
                })?;
                serde_json::from_str::<strategies::DollarBarsCusumSpace>(&content)
                    .with_context(|| "Failed to parse parameter space JSON")?
            } else {
                strategies::DollarBarsCusumSpace::default()
            };

            let space_json = serde_json::to_value(&param_space)?;
            let optimizer = WalkForwardOptimizer::<strategies::DollarBarsCusumStrategy>::new(
                engine_config,
                param_space,
            );
            let evals = optimizer.run_optimization(&bars, &folds, args.candidates)?;
            let win = evals.first().cloned();
            (
                evals,
                win,
                strategies::DollarBarsCusumStrategy::id(),
                space_json,
            )
        }
        unknown => {
            anyhow::bail!(
                "Unknown strategy '{}'. Available strategies: 'dollar_bars_cusum'",
                unknown
            );
        }
    };

    let opt_duration = opt_start.elapsed().as_secs_f64();
    println!(
        "      Evaluated {} configurations across {} folds in {:.2}s ({:.1} backtests/sec)",
        evaluations.len(),
        folds.len(),
        opt_duration,
        (evaluations.len() * folds.len() * 2) as f64 / opt_duration
    );

    // 4. Output Results and Reporting
    println!("\n============================================================");
    println!(" TOP 5 CANDIDATES (Ranked by Stability-Penalized Fitness)   ");
    println!("============================================================");
    println!(
        "{:<4} | {:<25} | {:<7} | {:<7} | {:<6} | {:<7} | {:<6}",
        "Rank", "Parameters", "IS Sort", "OOS Sort", "Stab.", "Fitness", "DSR Sig"
    );
    println!("------------------------------------------------------------");

    for (rank, eval) in evaluations.iter().take(5).enumerate() {
        let p_str = format!("{}", eval.config);
        println!(
            "#{:<3} | {:<25} | {:<7.2} | {:<7.2} | {:<6.2} | {:<7.2} | {}",
            rank + 1,
            p_str,
            eval.mean_is_sortino,
            eval.mean_oos_sortino,
            eval.stability_score,
            eval.fitness,
            if eval.is_statistically_significant {
                "PASS"
            } else {
                "FAIL"
            }
        );
    }
    println!("============================================================");

    let sweep_spec = RunSpec {
        strategy_id: strategy_id.to_string(),
        symbol: symbol_upper.clone(),
        market_type: strategies::MarketType::UsdMPerpetual,
        sampling: SamplingConfig::DollarBar {
            threshold: args.dollar_bar,
        },
        strategy_config: search_space,
        execution_context: engine_config,
    };

    if args.gcp_bigquery && !evaluations.is_empty() {
        let bq = BigQuerySink::from_env();
        let sweep_ctx = RunContext::new(RunType::Hpo, &sweep_spec, started_at_ms);
        let data_window = DataWindow {
            source: Some(args.data.display().to_string()),
            start_ms: bars.first().map(|b| b.start_time),
            end_ms: bars.last().map(|b| b.end_time),
        };
        let settings = json!({
            "folds": folds.len(),
            "train_ratio": args.train_ratio,
            "embargo_bars": args.embargo_bars,
            "candidates_requested": args.candidates,
            "total_bars": bars.len(),
            "raw_trades": raw_trades,
            "optimization_seconds": opt_duration,
        });
        println!(
            "[*] Recording HPO run {} with {} trial runs to BigQuery...",
            sweep_ctx.run_id,
            evaluations.len()
        );
        if let Err(e) = record_hpo(
            &bq,
            &sweep_ctx,
            &sweep_spec,
            &data_window,
            &evaluations,
            settings,
        )
        .await
        {
            println!("[WARN] Failed to record HPO run: {e}");
        }
    }

    if let Some(winner) = &winner {
        println!("\n[WINNER] Selected Robust Parameter Configuration:");
        println!("  • Strategy:                  {}", args.strategy);
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
        println!("  • Deflated Sharpe Ratio p:   {:.4}", winner.dsr_p_value);

        // Serialize Winner to output_config as authoritative StrategyDeploymentArtifact
        let scorecard = HpoScorecard {
            mean_is_sortino: winner.mean_is_sortino,
            mean_oos_sortino: winner.mean_oos_sortino,
            mean_oos_profit_factor: winner.mean_oos_profit_factor,
            stability_score: winner.stability_score,
            fitness: winner.fitness,
            dsr_p_value: winner.dsr_p_value,
            is_statistically_significant: winner.is_statistically_significant,
            trials_evaluated: evaluations.len(),
        };

        let deployment_artifact = StrategyDeploymentArtifact {
            schema_version: "1.0".to_string(),
            spec: sweep_spec.with_config(winner.config.clone()),
            hpo_scorecard: Some(scorecard),
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
            deployment_artifact.spec.strategy_id
        );
        println!(
            "        • Symbol:       {}",
            deployment_artifact.spec.symbol
        );
        println!("        • Sampling:     DollarBar(${})", args.dollar_bar);

        if args.gcs_upload {
            let gcs = GcsParquetSink::from_env("storage/reports");
            let artifact_dest_name = args
                .output_config
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("strategy_artifact.json");
            println!(
                "[*] Uploading Strategy Deployment Artifact to GCS (artifacts/{artifact_dest_name})..."
            );
            let _ = gcs
                .upload_file(
                    &args.output_config,
                    &format!("artifacts/{}", artifact_dest_name),
                    "application/json",
                )
                .await;
        }
    }

    Ok(())
}
