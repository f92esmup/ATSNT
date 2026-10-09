use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use adapters::{
    format_unix_ms_rfc3339, BigQuerySink, BinanceCsvReader, BinanceParquetReader, GcsParquetSink,
    HpoEvaluationRow, MarketDataStream,
};
use anyhow::{Context, Result};
use backtest::{
    BacktestConfig, ParameterSpace, WalkForwardConfig, WalkForwardOptimizer, WalkForwardSplitter,
};
use clap::Parser;
use domain::DollarBarAggregator;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;

#[derive(Parser, Debug)]
#[command(
    name = "run_hpo",
    about = "ATSNT - Parallel Walk-Forward Hyperparameter Optimizer (Rayon + Anti-Overfitting)"
)]
struct Args {
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

    /// Stream evaluated candidate parameters to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Export HPO evaluation report to Google Cloud Storage
    #[arg(long, default_value_t = false)]
    gcs_upload: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    println!("============================================================");
    println!(" ATSNT - Walk-Forward Hyperparameter Optimization (Rayon)  ");
    println!("============================================================");
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

    let engine_config = BacktestConfig {
        initial_capital: dec!(10_000),
        maker_fee_pct: dec!(0.0002),
        taker_fee_pct: dec!(0.0005),
        slippage_pct: dec!(0.0005),
        risk_per_trade_pct: dec!(0.01),
        max_daily_drawdown_pct: dec!(0.05),
    };

    let param_space = ParameterSpace::default();
    let optimizer = WalkForwardOptimizer::new(engine_config, param_space);
    let evaluations = optimizer.run_optimization(&bars, &folds, args.candidates)?;

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
        "{:<4} | {:<20} | {:<7} | {:<7} | {:<6} | {:<7} | {:<6}",
        "Rank", "Params (W/V/Ze/Zs/Tb)", "IS Sort", "OOS Sort", "Stab.", "Fitness", "DSR Sig"
    );
    println!("------------------------------------------------------------");

    for (rank, eval) in evaluations.iter().take(5).enumerate() {
        let p_str = format!(
            "{}/{}/{}/{}/{}",
            eval.config.rolling_window_len,
            eval.config.cusum_vol_multiplier,
            eval.config.z_entry_threshold,
            eval.config.z_stop_threshold,
            eval.config.time_barrier_bars
        );
        println!(
            "#{:<3} | {:<20} | {:<7.2} | {:<7.2} | {:<6.2} | {:<7.2} | {}",
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

    if let Some(winner) = evaluations.first() {
        println!("\n[WINNER] Selected Robust Parameter Configuration:");
        println!(
            "  • Rolling Window Lookback:   {} bars",
            winner.config.rolling_window_len
        );
        println!(
            "  • Dynamic CUSUM Vol Mult:    {}",
            winner.config.cusum_vol_multiplier
        );
        println!(
            "  • Z-Score Entry Threshold:   {}",
            winner.config.z_entry_threshold
        );
        println!(
            "  • Z-Score Stop Loss Barrier: {}",
            winner.config.z_stop_threshold
        );
        println!(
            "  • Time Barrier:              {} bars",
            winner.config.time_barrier_bars
        );
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

        // Serialize Winner to output_config
        if let Some(parent) = args.output_config.parent() {
            fs::create_dir_all(parent)?;
        }
        let winner_json = serde_json::to_string_pretty(&winner.config)?;
        fs::write(&args.output_config, winner_json)?;
        println!(
            "\n[SAVED] Configuration saved to: {}",
            args.output_config.display()
        );

        // Save detailed audit report for future web dashboard
        let reports_dir = Path::new("storage/reports");
        fs::create_dir_all(reports_dir)?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let report_path = reports_dir.join(format!("hpo_run_{timestamp}.json"));

        let report = json!({
            "timestamp": timestamp,
            "data_source": args.data.display().to_string(),
            "total_bars": bars.len(),
            "folds_count": folds.len(),
            "trials_evaluated": evaluations.len(),
            "winner": winner,
            "top_candidates": evaluations.iter().take(10).collect::<Vec<_>>()
        });

        fs::write(&report_path, serde_json::to_string_pretty(&report)?)?;
        println!(
            "[SAVED] Detailed audit telemetry saved to: {}",
            report_path.display()
        );

        if args.gcp_bigquery {
            let bq = BigQuerySink::from_env();
            let timestamp_str = format_unix_ms_rfc3339((timestamp * 1000) as i64);
            let rows: Vec<HpoEvaluationRow> = evaluations
                .iter()
                .enumerate()
                .map(|(idx, eval)| HpoEvaluationRow {
                    run_id: format!("hpo_{}_{}", timestamp, idx),
                    timestamp: timestamp_str.clone(),
                    strategy_id: "dollar_bars_cusum".to_string(),
                    parameter_space: serde_json::to_string(&eval.config).unwrap_or_default(),
                    is_sharpe: eval.mean_is_sortino.round_dp(4),
                    oos_sharpe: eval.mean_oos_sortino.round_dp(4),
                    deflated_sharpe_ratio: eval.fitness.round_dp(4),
                    parameter_stability_score: eval.stability_score.round_dp(4),
                    total_trades: 0,
                    win_rate: dec!(0),
                    max_drawdown_pct: dec!(0),
                })
                .collect();
            println!(
                "[*] Streaming {} evaluations to BigQuery (atsnt_bi.hpo_evaluations)...",
                rows.len()
            );
            let _ = bq.insert_hpo_evaluations(&rows).await;
        }

        if args.gcs_upload {
            let gcs = GcsParquetSink::from_env("storage/reports");
            let _ = gcs
                .upload_file(
                    &report_path,
                    &format!("hpo/hpo_run_{}.json", timestamp),
                    "application/json",
                )
                .await;
        }
    }

    Ok(())
}
