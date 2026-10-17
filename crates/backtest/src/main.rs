use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use adapters::{BigQuerySink, BinanceCsvReader, BinanceParquetReader, MarketDataStream};
use anyhow::{Context, Result};
use backtest::{
    now_unix_ms, record_backtest, record_monte_carlo, BacktestConfig, BacktestEngine, DataWindow,
    MonteCarloConfig, MonteCarloSimulator, RunContext, RunSpec, RunType, SamplingConfig,
    SinkFailures, TickSimulation, TickSimulationConfig, DEFAULT_MAINTENANCE_MARGIN_RATE,
};
use clap::Parser;
use domain::futures::FundingSchedule;
use domain::DollarBarAggregator;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{
    DollarBarsCusumConfig, DollarBarsCusumStrategy, MarketType, OptimizableStrategy,
    OrderFlowImbalanceConfig, OrderFlowImbalanceStrategy, Strategy,
};

/// How simulated orders are filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum FillModel {
    /// Bar-level fills through `BacktestEngine`.
    Bar,
    /// Tick-level fills through `TickSimulation`.
    Tick,
}

#[derive(Parser, Debug)]
#[command(
    name = "backtest",
    about = "ATSNT - Quantitative Backtesting Engine (100% Rust)"
)]
struct Args {
    /// Path to historical market data file (.csv or .parquet)
    #[arg(short, long, default_value = "data/sample_trades.csv")]
    data: PathBuf,

    /// Dollar bar threshold (e.g. 200000 for $200k, 1000000 for $1M; auto-adopted from --config artifact if omitted)
    #[arg(long)]
    dollar_bar: Option<Decimal>,

    /// Optional path to JSON strategy configuration (e.g. configs/hpo_results.json)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Strategy to run: "dollar_bars_cusum" (or "cusum") vs "order_flow_imbalance" (or "ofi").
    /// Inferred automatically from --config artifact if omitted.
    #[arg(long)]
    strategy: Option<String>,

    /// Initial cash capital allocation in USD (default: $10,000, or adopted from --config artifact)
    #[arg(long)]
    capital: Option<Decimal>,

    /// Optional override for max session drawdown percentage (e.g. 0.20 for 20%)
    #[arg(long)]
    max_drawdown: Option<Decimal>,

    /// Optional override for risk percentage per trade (e.g. 0.01 for 1.0%)
    #[arg(long)]
    risk_per_trade: Option<Decimal>,

    /// Fill model: "bar" fills at bar granularity (fast, used by the optimizer); "tick" replays
    /// every trade through the live trading session on a simulated venue with order latency and
    /// tape-walk market fills (slow, for validating finalists)
    #[arg(long, value_enum, default_value_t = FillModel::Bar)]
    fill_model: FillModel,

    /// Milliseconds an order takes to reach the market with `--fill-model tick` (an assumption
    /// until measured against live fills)
    #[arg(long, default_value_t = 100)]
    latency_ms: i64,

    /// Funding rate per eight-hour settlement with `--fill-model tick`, used where
    /// `--funding-file` has no record (0.01% is the exchange's calm-market baseline)
    #[arg(long, default_value = "0.0001")]
    funding_rate: Decimal,

    /// Funding history JSON in the `GET /fapi/v1/fundingRate` format, charged at the settlements
    /// it covers with `--fill-model tick`
    #[arg(long)]
    funding_file: Option<PathBuf>,

    /// Enable post-backtest Discrete Event Monte Carlo Stress-Testing
    #[arg(long, default_value_t = false)]
    monte_carlo: bool,

    /// Number of synthetic Monte Carlo paths to simulate
    #[arg(long, default_value_t = 10000)]
    mc_iterations: usize,

    /// Record the run (spec, metrics, trades, equity events and Monte Carlo child run) to BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Exit non-zero if any BigQuery recording fails; failures are reported after
    /// the backtest and Monte Carlo finish
    #[arg(long, default_value_t = false)]
    strict_sinks: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let started_at_ms = now_unix_ms();
    let start_total = Instant::now();

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

    // 1. Load Strategy Configuration & Authoritative Deployment Artifact
    let (
        mut strategy,
        strat_config_val,
        strat_id,
        art_threshold,
        art_symbol,
        art_market,
        art_context,
    ) = if let Some(config_path) = &args.config {
        println!(
            "[*] Loading strategy configuration from: {}",
            config_path.display()
        );
        let raw_content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        let val: serde_json::Value = serde_json::from_str(&raw_content)
            .with_context(|| "Failed to parse configuration JSON")?;

        let strat_id = val
            .get("strategy_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| args.strategy.clone())
            .unwrap_or_else(|| "dollar_bars_cusum".to_string());

        match strat_id.as_str() {
            "order_flow_imbalance" | "ofi" => {
                let loaded =
                    backtest::load_strategy_config::<OrderFlowImbalanceConfig>(config_path)?;
                match &loaded {
                    backtest::LoadedStrategyConfig::Artifact(artifact) => {
                        println!(
                            "[*] Authoritative Deployment Artifact loaded (schema v{})",
                            artifact.schema_version
                        );
                        if let Some(scorecard) = &artifact.hpo_scorecard {
                            println!(
                                "    • HPO Scorecard: IS Sortino={:.2}, OOS Sortino={:.2}, Stability={:.2}, DSR p={:.4} ({})",
                                scorecard.mean_is_sortino,
                                scorecard.mean_oos_sortino,
                                scorecard.stability_score,
                                scorecard.dsr_p_value,
                                if scorecard.is_statistically_significant {
                                    "PASS"
                                } else {
                                    "FAIL"
                                }
                            );
                        }
                    }
                    backtest::LoadedStrategyConfig::Bare(_) => {
                        println!(
                            "[*] Legacy bare hyperparameter configuration loaded (no deployment manifest)"
                        );
                    }
                }
                let strat = Box::new(
                    OrderFlowImbalanceStrategy::new(loaded.strategy_config().clone())
                        .map_err(|e| anyhow::anyhow!("Failed to initialize strategy: {e}"))?,
                ) as Box<dyn Strategy>;
                let cfg_val = serde_json::to_value(loaded.strategy_config())?;
                let threshold = loaded.dollar_bar_threshold();
                let sym = loaded.symbol().map(str::to_string);
                let mkt = loaded.market_type();
                let ctx = loaded.execution_context().copied();
                (
                    strat,
                    cfg_val,
                    OrderFlowImbalanceStrategy::id().to_string(),
                    threshold,
                    sym,
                    mkt,
                    ctx,
                )
            }
            "dollar_bars_cusum" | "cusum" => {
                let loaded = backtest::load_strategy_config::<DollarBarsCusumConfig>(config_path)?;
                match &loaded {
                    backtest::LoadedStrategyConfig::Artifact(artifact) => {
                        println!(
                            "[*] Authoritative Deployment Artifact loaded (schema v{})",
                            artifact.schema_version
                        );
                        if let Some(scorecard) = &artifact.hpo_scorecard {
                            println!(
                                "    • HPO Scorecard: IS Sortino={:.2}, OOS Sortino={:.2}, Stability={:.2}, DSR p={:.4} ({})",
                                scorecard.mean_is_sortino,
                                scorecard.mean_oos_sortino,
                                scorecard.stability_score,
                                scorecard.dsr_p_value,
                                if scorecard.is_statistically_significant {
                                    "PASS"
                                } else {
                                    "FAIL"
                                }
                            );
                        }
                    }
                    backtest::LoadedStrategyConfig::Bare(_) => {
                        println!(
                            "[*] Legacy bare hyperparameter configuration loaded (no deployment manifest)"
                        );
                    }
                }
                let strat = Box::new(
                    DollarBarsCusumStrategy::new(loaded.strategy_config().clone())
                        .map_err(|e| anyhow::anyhow!("Failed to initialize strategy: {e}"))?,
                ) as Box<dyn Strategy>;
                let cfg_val = serde_json::to_value(loaded.strategy_config())?;
                let threshold = loaded.dollar_bar_threshold();
                let sym = loaded.symbol().map(str::to_string);
                let mkt = loaded.market_type();
                let ctx = loaded.execution_context().copied();
                (
                    strat,
                    cfg_val,
                    DollarBarsCusumStrategy::id().to_string(),
                    threshold,
                    sym,
                    mkt,
                    ctx,
                )
            }
            unknown => {
                anyhow::bail!(
                    "Unknown strategy '{}'. Available strategies: 'dollar_bars_cusum', 'order_flow_imbalance'",
                    unknown
                );
            }
        }
    } else {
        let strat_id = args.strategy.as_deref().unwrap_or("dollar_bars_cusum");
        match strat_id {
            "order_flow_imbalance" | "ofi" => {
                println!("[*] Using default Order Flow Imbalance strategy hyperparameters");
                let cfg = OrderFlowImbalanceConfig::default();
                let strat = Box::new(
                    OrderFlowImbalanceStrategy::new(cfg.clone())
                        .map_err(|e| anyhow::anyhow!("Failed to initialize strategy: {e}"))?,
                ) as Box<dyn Strategy>;
                let cfg_val = serde_json::to_value(&cfg)?;
                (
                    strat,
                    cfg_val,
                    OrderFlowImbalanceStrategy::id().to_string(),
                    None,
                    None,
                    None,
                    None,
                )
            }
            "dollar_bars_cusum" | "cusum" => {
                println!("[*] Using default CUSUM + Z-Score strategy hyperparameters");
                let cfg = DollarBarsCusumConfig {
                    rolling_window_len: 20,
                    cusum_vol_multiplier: dec!(1.5),
                    z_entry_threshold: dec!(1.8),
                    z_stop_threshold: dec!(3.0),
                    time_barrier_bars: 15,
                    min_take_profit_pct: dec!(0.0025),
                };
                let strat = Box::new(
                    DollarBarsCusumStrategy::new(cfg.clone())
                        .map_err(|e| anyhow::anyhow!("Failed to initialize strategy: {e}"))?,
                ) as Box<dyn Strategy>;
                let cfg_val = serde_json::to_value(&cfg)?;
                (
                    strat,
                    cfg_val,
                    DollarBarsCusumStrategy::id().to_string(),
                    None,
                    None,
                    None,
                    None,
                )
            }
            unknown => {
                anyhow::bail!(
                    "Unknown strategy '{}'. Available strategies: 'dollar_bars_cusum', 'order_flow_imbalance'",
                    unknown
                );
            }
        }
    };

    // Resolve Dollar Bar threshold: CLI override > Artifact manifest > Fallback default ($200,000)
    let effective_dollar_bar = match (args.dollar_bar, art_threshold) {
        (Some(cli_val), Some(artifact_val)) => {
            if cli_val != artifact_val {
                println!(
                    "[WARNING] CLI override --dollar-bar (${}) overrides artifact threshold (${})!",
                    cli_val, artifact_val
                );
            }
            cli_val
        }
        (Some(cli_val), None) => cli_val,
        (None, Some(artifact_val)) => {
            println!(
                "[*] Authoritative Contract: Adopted Dollar Bar threshold from artifact: ${}",
                artifact_val
            );
            artifact_val
        }
        (None, None) => dec!(200_000),
    };

    // 2. Initialize Dollar Bar Aggregator
    let mut aggregator = DollarBarAggregator::new(effective_dollar_bar)
        .map_err(|e| anyhow::anyhow!("Failed to initialize aggregator: {e}"))?;

    println!(
        "[*] Configured DollarBarAggregator with threshold: ${}",
        effective_dollar_bar
    );

    println!("[*] Strategy loaded: {}", strategy.name());

    // 3. Initialize Backtest Engine with 1:1 realistic friction
    let mut engine_config = if let Some(artifact_ctx) = art_context {
        println!("[*] Adopting 1:1 realistic friction schedule from deployment artifact");
        artifact_ctx
    } else {
        BacktestConfig {
            initial_capital: dec!(10_000),
            maker_fee_pct: dec!(0.0002),    // 0.02%
            taker_fee_pct: dec!(0.0005),    // 0.05%
            slippage_pct: dec!(0.0005),     // 0.05%
            risk_per_trade_pct: dec!(0.01), // 1.0% risk per trade
            max_daily_drawdown_pct: dec!(0.05),
            ..BacktestConfig::default()
        }
    };

    if let Some(cap) = args.capital {
        if cap <= Decimal::ZERO {
            anyhow::bail!("--capital must be strictly positive, got {}", cap);
        }
        println!("[*] Capital Allocation: Override set to ${}", cap);
        engine_config.initial_capital = cap;
    }

    if let Some(dd) = args.max_drawdown {
        if dd <= Decimal::ZERO || dd > Decimal::ONE {
            anyhow::bail!("--max-drawdown must be between 0.0 and 1.0, got {}", dd);
        }
        println!(
            "[*] Max Session Drawdown: Override set to {:.2}%",
            dd * dec!(100)
        );
        engine_config.max_daily_drawdown_pct = dd;
    }

    if let Some(risk) = args.risk_per_trade {
        if risk <= Decimal::ZERO || risk > Decimal::ONE {
            anyhow::bail!("--risk-per-trade must be between 0.0 and 1.0, got {}", risk);
        }
        println!(
            "[*] Risk per Trade: Override set to {:.2}%",
            risk * dec!(100)
        );
        engine_config.risk_per_trade_pct = risk;
    }
    engine_config.validate()?;
    if args.fill_model == FillModel::Tick && args.gcp_bigquery {
        anyhow::bail!(
            "--gcp-bigquery cannot record tick-level runs yet: the run model does not tag the fill model, so they would be indistinguishable from bar-level runs"
        );
    }
    let symbol = art_symbol.unwrap_or_else(|| {
        args.data
            .file_name()
            .and_then(|f| f.to_str())
            .and_then(|s| s.split('-').next())
            .unwrap_or("BTCUSDT")
            .to_uppercase()
    });

    println!("[*] Initial Capital: ${}", engine_config.initial_capital);
    println!("[*] Replaying discrete market trade ticks...\n");

    let mut total_trades_processed = 0usize;
    let mut total_bars_emitted = 0usize;
    let mut data_window = DataWindow {
        source: Some(args.data.display().to_string()),
        ..DataWindow::default()
    };

    let replay_start = Instant::now();
    let (metrics, closed_trades) = match args.fill_model {
        FillModel::Bar => {
            let mut engine = BacktestEngine::new(engine_config);
            let mut last_price = Decimal::ZERO;
            while let Some(trade) = stream.next_trade()? {
                total_trades_processed += 1;
                last_price = trade.price;
                data_window.start_ms.get_or_insert(trade.timestamp);
                data_window.end_ms = Some(trade.timestamp);

                if let Some(bar) = aggregator.process_trade(&trade) {
                    total_bars_emitted += 1;
                    if total_bars_emitted <= 5 || total_bars_emitted % 250_000 == 0 {
                        println!(
                            "  [DollarBar #{:07}] Close: ${:.2} | Vol: {:.2} BTC | Notional: ${:.2}",
                            total_bars_emitted, bar.close, bar.volume, bar.dollar_volume
                        );
                    }
                    engine.process_bar(&mut strategy, &bar);
                    if engine.is_halted() {
                        println!(
                            "\n[!] Monthly circuit breaker tripped: simulation halted early, remaining market data skipped."
                        );
                        break;
                    }
                }
            }
            let mark = (last_price > Decimal::ZERO).then_some(last_price);
            engine.finish_with_trades(mark)
        }
        FillModel::Tick => {
            println!(
                "[*] Fill model: tick-level venue, {} ms order latency, tape-walk market fills",
                args.latency_ms
            );
            let funding = match &args.funding_file {
                Some(path) => {
                    let history = std::fs::read_to_string(path)
                        .with_context(|| format!("Failed to read {}", path.display()))?;
                    adapters::funding_schedule_from_history(&history, args.funding_rate)?
                }
                None => FundingSchedule::constant(args.funding_rate),
            };
            let mut simulation = TickSimulation::new(
                TickSimulationConfig {
                    backtest: engine_config,
                    symbol: symbol.clone(),
                    dollar_bar_threshold: effective_dollar_bar,
                    latency_ms: args.latency_ms,
                    funding,
                    maintenance_margin_rate: DEFAULT_MAINTENANCE_MARGIN_RATE,
                },
                strategy,
            )?;
            while let Some(trade) = stream.next_trade()? {
                data_window.start_ms.get_or_insert(trade.timestamp);
                data_window.end_ms = Some(trade.timestamp);
                simulation.on_trade(&trade).await;
                if simulation.is_halted() {
                    println!(
                        "\n[!] Monthly circuit breaker tripped: simulation halted early, remaining market data skipped."
                    );
                    break;
                }
            }
            total_trades_processed =
                usize::try_from(simulation.ticks_processed()).unwrap_or(usize::MAX);
            total_bars_emitted = usize::try_from(simulation.bars_processed()).unwrap_or(usize::MAX);
            if let Some((side, quantity, entry)) = simulation.open_position() {
                println!(
                    "[!] A {side:?} position of {quantity} @ {entry} was still open when the data ended; it is not part of the metrics."
                );
            }
            simulation.finish()
        }
    };

    print_phase_timing("replay", replay_start);

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

    if !closed_trades.is_empty() {
        println!("\n------------------------------------------------------------");
        println!("                   CLOSED TRADES DETAIL                     ");
        println!("------------------------------------------------------------");
        for (i, t) in closed_trades.iter().enumerate() {
            println!(
                " #{:<2} {:?} qty:{:.4} entry:${:.2} exit:${:.2} ({}) gross:${:.2} net:${:.2} fees:${:.2} hold:{}s",
                i + 1,
                t.side,
                t.quantity,
                t.entry_price,
                t.exit_price,
                t.exit_reason,
                t.pnl_gross,
                t.pnl_net,
                t.fees_paid + t.slippage_paid,
                t.holding_duration_seconds
            );
        }
        println!("------------------------------------------------------------");
    }

    let run_spec = RunSpec {
        strategy_id: strat_id,
        symbol,
        market_type: art_market.unwrap_or(MarketType::UsdMPerpetual),
        sampling: SamplingConfig::DollarBar {
            threshold: effective_dollar_bar,
        },
        strategy_config: strat_config_val,
        execution_context: engine_config,
    };
    let run_ctx = RunContext::new(RunType::Backtest, &run_spec, started_at_ms);
    let recorder = args
        .gcp_bigquery
        .then(|| BigQuerySink::from_env().with_strict(args.strict_sinks));
    let mut sinks = SinkFailures::new(args.strict_sinks);

    if let Some(bq) = &recorder {
        let record_start = Instant::now();
        println!(
            "[*] Recording backtest run {} to BigQuery ({} trades)...",
            run_ctx.run_id,
            closed_trades.len()
        );
        let recorded = record_backtest(
            bq,
            &run_ctx,
            &run_spec,
            &data_window,
            &metrics,
            &closed_trades,
        )
        .await;
        sinks.record("record backtest run", recorded);
        print_phase_timing("record_bigquery", record_start);
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

            let mc_start = Instant::now();
            let simulator = MonteCarloSimulator::new(mc_config);
            let mc_report = simulator
                .run(engine_config.initial_capital, &closed_trades)
                .map_err(|e| anyhow::anyhow!("Monte Carlo error: {e}"))?;
            print_phase_timing("monte_carlo", mc_start);

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

            if let Some(bq) = &recorder {
                let mc_record_start = Instant::now();
                let mc_ctx = run_ctx.child(RunType::MonteCarlo, now_unix_ms());
                println!(
                    "[*] Recording Monte Carlo run {} (child of {}) to BigQuery...",
                    mc_ctx.run_id, run_ctx.run_id
                );
                let recorded =
                    record_monte_carlo(bq, &mc_ctx, &run_spec, &data_window, &mc_report).await;
                sinks.record("record Monte Carlo run", recorded);
                print_phase_timing("record_monte_carlo", mc_record_start);
            }
        }
    }

    print_phase_timing("total", start_total);
    sinks.finish()?;
    Ok(())
}

/// Prints a machine-greppable elapsed-time line for one pipeline phase.
fn print_phase_timing(phase: &str, since: Instant) {
    println!(
        "[TIMING] phase={phase} seconds={:.2}",
        since.elapsed().as_secs_f64()
    );
}
