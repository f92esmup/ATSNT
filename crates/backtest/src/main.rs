use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use adapters::{
    format_unix_ms_rfc3339, BigQuerySink, BinanceCsvReader, BinanceParquetReader, GcsParquetSink,
    MarketDataStream, MonteCarloRow, TradeRow,
};
use anyhow::{Context, Result};
use backtest::{BacktestConfig, BacktestEngine, MonteCarloConfig, MonteCarloSimulator};
use clap::Parser;
use domain::{DollarBarAggregator, PositionSide};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy, Strategy};

#[derive(Parser, Debug)]
#[command(
    name = "backtest",
    about = "ATSNT - Quantitative Backtesting Engine (100% Rust)"
)]
struct Args {
    /// Path to historical market data file (.csv or .parquet)
    #[arg(short, long, default_value = "data/sample_trades.csv")]
    data: PathBuf,

    /// Dollar bar threshold (e.g. 200000 for $200k, 1000000 for $1M)
    #[arg(long, default_value = "200000")]
    dollar_bar: Decimal,

    /// Optional path to JSON strategy configuration (e.g. configs/hpo_results.json)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Enable post-backtest Discrete Event Monte Carlo Stress-Testing
    #[arg(long, default_value_t = false)]
    monte_carlo: bool,

    /// Number of synthetic Monte Carlo paths to simulate
    #[arg(long, default_value_t = 10000)]
    mc_iterations: usize,

    /// Optional output file path for the Monte Carlo telemetry report
    #[arg(short, long)]
    report: Option<PathBuf>,

    /// Stream completed trades and Monte Carlo metrics to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Export Monte Carlo trajectories as columnar Parquet to GCS/staging
    #[arg(long, default_value_t = false)]
    gcs_parquet: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    println!("============================================================");
    println!("     ATSNT - Quantitative Backtesting Engine (100% Rust)    ");
    println!("============================================================");
    println!(
        "[*] Loading market data stream from: {}",
        args.data.display()
    );

    let mut stream: Box<dyn MarketDataStream<Error = adapters::AdapterError>> = if args
        .data
        .is_dir()
        || args.data.extension().and_then(|s| s.to_str()) == Some("parquet")
    {
        println!("[*] Format: Apache Parquet (High-throughput columnar)");
        Box::new(BinanceParquetReader::open(&args.data)?)
    } else {
        println!("[*] Format: CSV (Buffered line-by-line)");
        let file = File::open(&args.data)
            .with_context(|| format!("Failed to open {}", args.data.display()))?;
        Box::new(BinanceCsvReader::new(BufReader::new(file)))
    };

    // 1. Initialize Dollar Bar Aggregator
    let mut aggregator = DollarBarAggregator::new(args.dollar_bar)
        .map_err(|e| anyhow::anyhow!("Failed to initialize aggregator: {e}"))?;

    println!(
        "[*] Configured DollarBarAggregator with threshold: ${}",
        args.dollar_bar
    );

    // 2. Initialize Strategy (Dollar Bars + CUSUM + Z-Score)
    let strat_config: DollarBarsCusumConfig = if let Some(config_path) = args.config {
        println!(
            "[*] Loading strategy parameters from: {}",
            config_path.display()
        );
        let file = File::open(&config_path)
            .with_context(|| format!("Failed to open config file {}", config_path.display()))?;
        serde_json::from_reader(file)?
    } else {
        DollarBarsCusumConfig {
            rolling_window_len: 20,
            cusum_vol_multiplier: dec!(1.5),
            z_entry_threshold: dec!(1.8),
            z_stop_threshold: dec!(3.0),
            time_barrier_bars: 15,
        }
    };

    let mut strategy = DollarBarsCusumStrategy::new(strat_config)
        .map_err(|e| anyhow::anyhow!("Failed to initialize strategy: {e}"))?;

    println!("[*] Strategy loaded: {}", strategy.name());

    // 3. Initialize Backtest Engine with 1:1 realistic friction
    let engine_config = BacktestConfig {
        initial_capital: dec!(10_000),
        maker_fee_pct: dec!(0.0002),    // 0.02%
        taker_fee_pct: dec!(0.0005),    // 0.05%
        slippage_pct: dec!(0.0005),     // 0.05%
        risk_per_trade_pct: dec!(0.01), // 1.0% risk per trade
        max_daily_drawdown_pct: dec!(0.05),
    };
    let mut engine = BacktestEngine::new(engine_config);

    println!("[*] Initial Capital: ${}", engine.cash_equity());
    println!("[*] Replaying discrete market trade ticks...\n");

    let mut total_trades_processed = 0usize;
    let mut total_bars_emitted = 0usize;
    let mut last_price = Decimal::ZERO;

    while let Some(trade) = stream.next_trade()? {
        total_trades_processed += 1;
        last_price = trade.price;

        if let Some(bar) = aggregator.process_trade(&trade) {
            total_bars_emitted += 1;
            if total_bars_emitted <= 20 || total_bars_emitted % 100 == 0 {
                println!(
                    "  [DollarBar #{:04}] Close: ${:.2} | Vol: {:.2} BTC | Notional: ${:.2}",
                    total_bars_emitted, bar.close, bar.volume, bar.dollar_volume
                );
            }
            engine.process_bar(&mut strategy, &bar);
        }
    }

    let mark = if last_price > Decimal::ZERO {
        Some(last_price)
    } else {
        None
    };
    let (metrics, closed_trades) = engine.finish_with_trades(mark);

    println!("\n============================================================");
    println!("               QUANTITATIVE PERFORMANCE REPORT              ");
    println!("============================================================");
    println!(" Raw Ticks Processed:       {}", total_trades_processed);
    println!(" Dollar Bars Formed:        {}", total_bars_emitted);
    println!(" Total Trades Executed:     {}", metrics.total_trades);
    println!(" Winning Trades:            {}", metrics.winning_trades);
    println!(" Losing Trades:             {}", metrics.losing_trades);
    println!(
        " Win Rate:                  {:.2}%",
        metrics.win_rate * dec!(100)
    );
    println!(" Profit Factor:             {:.2}", metrics.profit_factor);
    println!(" Payoff Ratio:              {:.2}", metrics.payoff_ratio);
    println!(
        " Mathematical Expectancy:   ${:.2} per trade",
        metrics.mathematical_expectancy
    );
    println!("------------------------------------------------------------");
    println!(" Gross Profit:              ${:.2}", metrics.gross_profit);
    println!(" Gross Loss:                ${:.2}", metrics.gross_loss);
    println!(" Net Profit (Real PnL):     ${:.2}", metrics.net_profit);
    println!(" Total Fees Paid (Friction):${:.2}", metrics.total_fees);
    println!(" Total Slippage Paid:       ${:.2}", metrics.total_slippage);
    println!(
        " Max Drawdown ($):          ${:.2}",
        metrics.max_drawdown_amount
    );
    println!(
        " Max Drawdown (%):          {:.2}%",
        metrics.max_drawdown_pct * dec!(100)
    );
    println!(" Sortino Ratio:             {:.2}", metrics.sortino_ratio);
    println!("============================================================");

    // Save backtest telemetry
    let reports_dir = Path::new("storage/reports");
    fs::create_dir_all(reports_dir)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let backtest_report_path = reports_dir.join(format!("backtest_{timestamp}.json"));

    let bt_json = json!({
        "timestamp": timestamp,
        "data_source": args.data.display().to_string(),
        "dollar_bar_threshold": args.dollar_bar,
        "metrics": metrics,
        "closed_trades": closed_trades,
    });
    fs::write(
        &backtest_report_path,
        serde_json::to_string_pretty(&bt_json)?,
    )?;
    println!(
        "[*] Backtest report saved to: {}",
        backtest_report_path.display()
    );

    if args.gcp_bigquery && !closed_trades.is_empty() {
        let bq = BigQuerySink::from_env();
        let symbol_name = args
            .data
            .file_name()
            .and_then(|f| f.to_str())
            .and_then(|s| s.split('-').next())
            .unwrap_or("BTCUSDT")
            .to_uppercase();
        let session_id = format!("bt_{}_{}", symbol_name.to_lowercase(), timestamp);
        let rows: Vec<TradeRow> = closed_trades
            .iter()
            .enumerate()
            .map(|(idx, ct)| {
                let side_str = match ct.side {
                    PositionSide::Long => "Long",
                    PositionSide::Short => "Short",
                };
                TradeRow {
                    trade_id: format!("{}_t{:05}", session_id, idx + 1),
                    session_id: session_id.clone(),
                    strategy_id: strategy.name().to_string(),
                    symbol: symbol_name.clone(),
                    side: side_str.to_string(),
                    entry_timestamp: format_unix_ms_rfc3339(ct.entry_time),
                    exit_timestamp: format_unix_ms_rfc3339(ct.exit_time),
                    entry_price: ct.entry_price.round_dp(4),
                    exit_price: ct.exit_price.round_dp(4),
                    quantity: ct.quantity.round_dp(4),
                    gross_pnl: ct.pnl_gross.round_dp(4),
                    fees_paid: ct.fees_paid.round_dp(4),
                    net_pnl: ct.pnl_net.round_dp(4),
                    exit_reason: ct.exit_reason.clone(),
                    holding_duration_seconds: ct.holding_duration_seconds,
                }
            })
            .collect();
        println!(
            "[*] Streaming {} closed trades to BigQuery (atsnt_bi.trades)...",
            rows.len()
        );
        let _ = bq.insert_trades(&rows).await;
    }

    if args.gcs_parquet {
        let gcs_sink = GcsParquetSink::from_env("storage/parquet");
        let _ = gcs_sink
            .upload_file(
                &backtest_report_path,
                &format!("backtests/backtest_{}.json", timestamp),
                "application/json",
            )
            .await;
    }

    // 4. Monte Carlo Stress-Testing
    if args.monte_carlo {
        println!("\n============================================================");
        println!(" DISCRETE EVENT MONTE CARLO STRESS TEST (AFML Ch. 12 & 16)  ");
        println!("============================================================");

        if closed_trades.len() < 2 {
            println!(
                "[WARN] Only {} closed trade(s) available. Monte Carlo trade sequence resampling requires >= 2 trades.",
                closed_trades.len()
            );
        } else {
            let mc_config = MonteCarloConfig {
                iterations: args.mc_iterations,
                method: backtest::ResampleMethod::CircularBlockBootstrap { block_size: 5 },
                ruin_threshold_pct: dec!(0.30),
                seed: Some(42),
            };

            let simulator = MonteCarloSimulator::new(mc_config);
            let mc_report = simulator
                .run(engine_config.initial_capital, &closed_trades)
                .map_err(|e| anyhow::anyhow!("Monte Carlo error: {e}"))?;

            println!(
                " Simulations Simulated:     {}",
                mc_report.metrics.total_simulations
            );
            println!(" Resampling Technique:      Circular Block Bootstrap (L=5)");
            println!(" Ruin Threshold:            30.00% Account Drawdown");
            println!("------------------------------------------------------------");
            println!(
                " Historical Max Drawdown:   {:.2}%",
                mc_report.metrics.historical_max_drawdown_pct * dec!(100)
            );
            println!(
                " P05 Drawdown (95% Best):   {:.2}%",
                mc_report.metrics.p05_max_drawdown_pct * dec!(100)
            );
            println!(
                " P50 Drawdown (Median):     {:.2}%",
                mc_report.metrics.p50_max_drawdown_pct * dec!(100)
            );
            println!(
                " P95 Drawdown (95% Stress): {:.2}%",
                mc_report.metrics.p95_max_drawdown_pct * dec!(100)
            );
            println!(
                " P99 Drawdown (99% Stress): {:.2}%",
                mc_report.metrics.p99_max_drawdown_pct * dec!(100)
            );
            println!(
                " Worst Simulated Drawdown:  {:.2}%",
                mc_report.metrics.worst_max_drawdown_pct * dec!(100)
            );
            println!("------------------------------------------------------------");
            println!(
                " Probability of Ruin:       {:.2}%",
                mc_report.metrics.probability_of_ruin_pct
            );
            println!(
                " Median Underwater Trades:  {} trades",
                mc_report.metrics.median_underwater_trades
            );
            println!(
                " P95 Underwater Trades:     {} trades",
                mc_report.metrics.p95_underwater_trades
            );
            println!(
                " Fan-Chart Trajectories:    {} sampled curves",
                mc_report.fan_chart.len()
            );
            println!("============================================================");

            let mc_path = args
                .report
                .unwrap_or_else(|| reports_dir.join(format!("{}.json", mc_report.report_id)));
            if let Some(parent) = mc_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&mc_path, serde_json::to_string_pretty(&mc_report)?)?;
            println!(
                "[*] Monte Carlo fan-chart telemetry saved to: {}",
                mc_path.display()
            );

            if args.gcp_bigquery {
                let bq = BigQuerySink::from_env();
                let mc_row = MonteCarloRow {
                    run_id: mc_report.report_id.clone(),
                    timestamp: format_unix_ms_rfc3339((timestamp * 1000) as i64),
                    strategy_id: strategy.name().to_string(),
                    iterations: mc_report.metrics.total_simulations as u64,
                    resample_method: "CircularBlockBootstrap".to_string(),
                    historical_max_drawdown: mc_report
                        .metrics
                        .historical_max_drawdown_pct
                        .round_dp(4),
                    p50_max_drawdown: mc_report.metrics.p50_max_drawdown_pct.round_dp(4),
                    p95_max_drawdown: mc_report.metrics.p95_max_drawdown_pct.round_dp(4),
                    p99_max_drawdown: mc_report.metrics.p99_max_drawdown_pct.round_dp(4),
                    probability_of_ruin_pct: mc_report.metrics.probability_of_ruin_pct.round_dp(4),
                };
                println!(
                    "[*] Streaming Monte Carlo summary to BigQuery (atsnt_bi.monte_carlo_runs)..."
                );
                let _ = bq.insert_monte_carlo_runs(&[mc_row]).await;
            }

            if args.gcs_parquet {
                let gcs_sink = GcsParquetSink::from_env("storage/parquet");
                let trajectories: Vec<(&str, &[Decimal])> = mc_report
                    .fan_chart
                    .iter()
                    .map(|t| (t.label.as_str(), t.equity_curve.as_slice()))
                    .collect();
                println!("[*] Exporting Monte Carlo trajectories to Parquet / GCS...");
                let parquet_path = gcs_sink
                    .export_monte_carlo_trajectories(&mc_report.report_id, &trajectories)
                    .await?;
                println!(
                    "[SAVED] Monte Carlo trajectories exported to: {}",
                    parquet_path.display()
                );
            }
        }
    }

    Ok(())
}
