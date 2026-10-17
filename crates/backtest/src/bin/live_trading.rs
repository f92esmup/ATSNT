//! Live Execution Gateway Runner for ATSNT on Binance Futures Testnet and Production.
//!
//! Ingests the real-time Binance aggregate trade WebSocket stream, aggregates trades into
//! information-driven Dollar Bars, evaluates algorithmic signals from the selected strategy,
//! validates pre-trade risk policy constraints, dispatches authenticated live orders via [`BinanceGateway`],
//! and listens for execution updates and order fills over the private [`BinancePrivateUserDataStream`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adapters::{
    now_in_madrid, AlertNotifier, AsyncMarketDataStream, BigQuerySink, BinanceGateway,
    BinanceGatewayConfig, BinancePrivateEvent, BinancePrivateUserDataStream,
    BinanceWebSocketStream, BinanceWsConfig, GcpSecretManager, RunRecorder, TelegramNotifier,
};
use anyhow::{Context, Result};
use backtest::driver::SessionDriver;
use backtest::parity::FillRecorder;
use backtest::session::{SessionConfig, SessionEvent, SkipReason, TradingSession};
use backtest::{
    now_unix_ms, record_closed_trade, record_run_status, BacktestConfig, BacktestMetrics,
    DataWindow, ExitEvent, RunContext, RunSpec, RunStatus, RunType, SamplingConfig,
};
use chrono::Timelike;
use clap::Parser;
use domain::{PositionSide, Side};
use ports::ExecutionReport;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{MarketType, Strategy};
use tracing::{error, warn};

#[derive(Parser, Debug)]
#[command(
    name = "live_trading",
    about = "ATSNT - Live Algorithmic Execution Engine against Binance Futures Testnet & Production"
)]
struct Args {
    /// Target trading symbol in lowercase (e.g. btcusdt, ethusdt)
    #[arg(short, long, default_value = "btcusdt")]
    symbol: String,

    /// Dollar bar accumulation threshold in USDT (auto-adopted from --config artifact if omitted)
    #[arg(long)]
    dollar_bar: Option<Decimal>,

    /// Optional path to JSON strategy configuration (e.g. configs/hpo_results.json)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Use official Binance Futures Testnet (defaults to true for capital safety)
    #[arg(long, default_value_t = true)]
    testnet: bool,

    /// Enable live production trading with real funds (DANGER: disables Testnet mode)
    #[arg(long, default_value_t = false)]
    production: bool,

    /// Fixed fractional risk percentage per trade (e.g. 0.01 for 1%).
    /// Overrides the --config artifact; defaults to the artifact value (1% without one).
    #[arg(long)]
    risk_pct: Option<Decimal>,

    /// Explicit capital allocation in USDT for this live session (partitioned virtual wallet).
    /// If omitted, defaults to the full available USDT balance on Binance.
    #[arg(long)]
    capital: Option<Decimal>,

    /// Record the session run (lifecycle, closed trades with equity events, final metrics) to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Send execution and barrier exit alerts to Telegram
    #[arg(long, default_value_t = false)]
    telegram_alerts: bool,

    /// Binance API Key (defaults to BINANCE_API_KEY env var)
    #[arg(long)]
    api_key: Option<String>,

    /// Binance Secret Key (defaults to BINANCE_SECRET_KEY env var)
    #[arg(long)]
    secret_key: Option<String>,

    /// Print individual raw market trade ticks to stdout (verbose debug mode)
    #[arg(long, default_value_t = false)]
    show_ticks: bool,

    /// Append every entry fill (send time, fill time, side, quantity, average price) to this
    /// JSON-lines file, the input of `parity_check` for calibrating the simulator's latency
    #[arg(long)]
    record_fills: Option<PathBuf>,

    /// Strategy to run, by id or alias (inferred from the --config artifact if omitted; the
    /// default otherwise)
    #[arg(long)]
    strategy: Option<String>,

    /// Warm the strategy's rolling windows with this many minutes of recent trades (public REST
    /// `aggTrades` of the same market the stream follows) before trading starts. Without it the
    /// strategy starts cold, unlike a backtest, and needs its full window of live bars first.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=1440))]
    warmup_minutes: Option<u32>,

    /// Verify the Binance account against the strategy profile, print the report and exit
    /// without changing anything.
    #[arg(long, default_value_t = false)]
    verify_only: bool,

    /// Verify the account profile but never change it: any mismatch stops the bot.
    #[arg(long, default_value_t = false)]
    no_auto_fix: bool,
}

/// Seconds between account profile re-verifications while the bot runs.
const ACCOUNT_RECHECK_SECS: u64 = 600;

fn validate_strategy_market(strategy: &impl Strategy, market: MarketType) -> Result<()> {
    if !strategy.supports_market_type(market) {
        anyhow::bail!(
            "Strategy '{}' does not support market type {:?}. Only {:?} is supported.",
            strategy.name(),
            market,
            strategy.compatible_market_types()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,adapters=debug".into()),
        )
        .init();

    // 0. Automatically load credentials from .env if present
    adapters::load_dotenv();

    let args = Args::parse();
    let started_at_ms = now_unix_ms();
    let is_testnet = if args.production { false } else { args.testnet };
    let symbol_upper = args.symbol.to_ascii_uppercase();

    let secret_manager = GcpSecretManager::from_env();

    let api_key = match args.api_key {
        Some(k) if !k.trim().is_empty() => k,
        _ => secret_manager
            .resolve_secret("BINANCE_API_KEY")
            .await
            .unwrap_or_default(),
    };

    let secret_key = match args.secret_key {
        Some(s) if !s.trim().is_empty() => s,
        _ => secret_manager
            .resolve_secret("BINANCE_SECRET_KEY")
            .await
            .unwrap_or_default(),
    };

    if api_key.is_empty() || secret_key.is_empty() {
        eprintln!(
            "\n[ERROR] Binance API credentials not found.\n\
             Please store BINANCE_API_KEY and BINANCE_SECRET_KEY in GCP Secret Manager,\n\
             in your local .env file, or pass --api-key and --secret-key.\n"
        );
        std::process::exit(1);
    }

    // 1. Load Strategy Configuration & Authoritative Deployment Artifact
    let strategy_id =
        backtest::resolve_strategy_id(args.strategy.as_deref(), args.config.as_deref())?;
    if let Some(config_path) = &args.config {
        println!(
            "[*] Loading strategy configuration from: {}",
            config_path.display()
        );
    }
    let loaded = backtest::load_strategy(&strategy_id, args.config.as_deref())?;
    for line in loaded.report_lines() {
        println!("{line}");
    }

    // Contract validation
    if let Some(artifact_sym) = loaded.symbol.as_deref() {
        if !artifact_sym.eq_ignore_ascii_case(&args.symbol) {
            println!(
                "[WARNING] Artifact symbol ({}) does not match session symbol ({})!",
                artifact_sym, symbol_upper
            );
        }
    }
    if let Some(market) = loaded.market_type {
        if market != MarketType::UsdMPerpetual {
            anyhow::bail!(
                "Deployment artifact specifies {:?}, but live_trading currently operates on UsdMPerpetual",
                market
            );
        }
    }

    // Resolve Dollar Bar threshold: CLI override > Artifact manifest > Fallback default ($50,000)
    let effective_dollar_bar = match (args.dollar_bar, loaded.dollar_bar_threshold) {
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
        (None, None) => dec!(50_000),
    };

    let estimated_taker_fee_pct = loaded
        .execution_context
        .as_ref()
        .map(|ctx| ctx.taker_fee_pct)
        .unwrap_or(dec!(0.0005));

    // The artifact is authoritative for sizing and risk limits so live gates orders exactly
    // like the simulation that selected the strategy; only `--risk-pct` may override it.
    let artifact_context = loaded.execution_context.unwrap_or_default();
    let risk_per_trade_pct = args.risk_pct.unwrap_or(artifact_context.risk_per_trade_pct);
    if risk_per_trade_pct <= Decimal::ZERO || risk_per_trade_pct > Decimal::ONE {
        anyhow::bail!("--risk-pct must be greater than 0 and at most 1, got {risk_per_trade_pct}");
    }
    let risk_policy = artifact_context
        .risk_policy()
        .with_context(|| "Invalid risk limits in the execution context")?;

    println!("============================================================");
    println!("     ATSNT - Live Algorithmic Execution Engine             ");
    println!("============================================================");
    println!(" Symbol:               {}", symbol_upper);
    println!(
        " Trading Venue:        {}",
        if is_testnet {
            "Binance USD-M Futures TESTNET (https://testnet.binancefuture.com)"
        } else {
            "Binance USD-M Futures PRODUCTION (https://fapi.binance.com) [REAL CAPITAL]"
        }
    );
    println!(" Dollar Bar Threshold: ${}", effective_dollar_bar);
    println!(
        " Risk per Trade:       {:.2}%",
        risk_per_trade_pct * dec!(100)
    );
    println!(
        " Daily Drawdown Limit: {:.2}% | Max Leverage: {}x",
        artifact_context.max_daily_drawdown_pct * dec!(100),
        artifact_context.max_leverage
    );
    println!(
        " Est. Taker Fee:       {:.4}%",
        estimated_taker_fee_pct * dec!(100)
    );

    let bq_sink = if args.gcp_bigquery {
        let sink = BigQuerySink::from_env();
        println!(
            " BigQuery Telemetry:   {}",
            if sink.is_enabled() {
                "ACTIVE (Recording run to atsnt_bi)"
            } else {
                "ENABLED (Pending GCP_PROJECT_ID / GCP_AUTH_TOKEN)"
            }
        );
        Some(sink)
    } else {
        None
    };

    let telegram = if args.telegram_alerts {
        let t = TelegramNotifier::from_gcp_or_env(&secret_manager).await;
        println!(
            " Telegram Alerts:      {}",
            if t.is_enabled() {
                "ACTIVE (Connected to Telegram Bot API)"
            } else {
                "ENABLED (Pending TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID)"
            }
        );
        Some(t)
    } else {
        None
    };

    let strategy = loaded.strategy;
    validate_strategy_market(&strategy, MarketType::UsdMPerpetual)?;

    println!(
        "[*] Strategy {}: {}",
        loaded.strategy_id, loaded.config_display
    );

    // 2. Initialize Binance Execution Gateway
    let gateway_config = BinanceGatewayConfig {
        api_key,
        secret_key,
        testnet: is_testnet,
        spot: false, // USD-M Perpetual
        recv_window_ms: 5000,
        risk_policy,
        risk_per_trade_pct,
    };
    let gateway = Arc::new(BinanceGateway::new(gateway_config)?);

    // 2b. Verify (and, when safe, prepare) the account before any stream is opened.
    println!("[*] Verifying the Binance account profile for {symbol_upper}...");
    let startup_report = gateway
        .verify_account(&symbol_upper, !args.no_auto_fix && !args.verify_only, false)
        .await
        .with_context(|| "Failed to read the Binance account configuration")?;
    println!("{}", startup_report.render());
    if !startup_report.is_ready() {
        anyhow::bail!(
            "the account does not match the strategy profile; fix the BLOCKING items above and restart"
        );
    }
    if args.verify_only {
        println!("[OK] --verify-only: the account matches the profile; nothing was changed.");
        return Ok(());
    }

    // 3. Query Account Balance & Health Check
    println!("[*] Verifying HMAC signatures & querying account balance...");
    let initial_balance = gateway
        .fetch_balance("USDT")
        .await
        .with_context(|| "Failed to query account balance from Binance")?;

    println!(
        "[OK] Gateway Authenticated! Available USDT Balance on Binance: ${:.2}\n",
        initial_balance
    );

    let effective_capital = match args.capital {
        Some(allocated) if allocated > Decimal::ZERO => {
            if allocated > initial_balance {
                println!(
                    "[WARNING] Requested allocated capital (${:.2}) exceeds available Binance balance (${:.2})! Clamping to available balance.",
                    allocated, initial_balance
                );
                initial_balance
            } else {
                println!(
                    "[*] Capital Allocation: Partitioned ${:.2} USDT for this strategy (Exchange Total: ${:.2} USDT)",
                    allocated, initial_balance
                );
                allocated
            }
        }
        Some(invalid) => {
            anyhow::bail!("--capital must be strictly positive, got {}", invalid);
        }
        None => {
            println!(
                "[*] Capital Allocation: Full account balance adopted (${:.2} USDT)",
                initial_balance
            );
            initial_balance
        }
    };

    // 4. Initialize Private User Data Stream
    println!("[*] Initializing Binance Private User Data Stream for live order updates...");
    let listen_key = gateway
        .create_listen_key()
        .await
        .with_context(|| "Failed to acquire USD-M listenKey")?;
    let mut user_stream =
        BinancePrivateUserDataStream::connect_futures(listen_key, is_testnet, gateway.clone())
            .await
            .with_context(|| "Failed to connect to Binance Private User Data Stream")?;

    // 5. Connect Public Market Stream
    // The public feed must be the market the account trades: testnet and production differ.
    let ws_config = if is_testnet {
        BinanceWsConfig::futures(&args.symbol)
    } else {
        BinanceWsConfig::futures_production(&args.symbol)
    };
    println!(
        "[*] Subscribing to public market trade feed: {}",
        ws_config.stream_url()
    );
    let ws_source = ws_config.stream_url();
    let mut market_stream = BinanceWebSocketStream::connect(ws_config)
        .with_context(|| "Failed to connect to public Binance WebSocket feed")?;

    let run_spec = RunSpec {
        strategy_id: loaded.strategy_id.clone(),
        symbol: symbol_upper.clone(),
        market_type: MarketType::UsdMPerpetual,
        sampling: SamplingConfig::DollarBar {
            threshold: effective_dollar_bar,
        },
        strategy_config: loaded.config_json.clone(),
        execution_context: BacktestConfig {
            initial_capital: effective_capital,
            taker_fee_pct: estimated_taker_fee_pct,
            risk_per_trade_pct,
            ..artifact_context
        },
    };
    run_spec.execution_context.validate()?;
    let run_ctx = RunContext::new(RunType::Live, &run_spec, started_at_ms);
    let mut data_window = DataWindow {
        source: Some(ws_source),
        ..DataWindow::default()
    };
    if let Some(bq) = &bq_sink {
        println!(
            "[*] Recording live run {} to BigQuery (Allocated Capital: ${:.2})",
            run_ctx.run_id, effective_capital
        );
        record_lifecycle(bq, &run_ctx, &run_spec, RunStatus::Running, &data_window).await;
    }

    let mut account_ok = true;
    let mut account_read_failures: u32 = 0;
    let mut account_check = tokio::time::interval(Duration::from_secs(ACCOUNT_RECHECK_SECS));
    account_check.tick().await; // the startup verification already covered the first period
    let mut peak_equity = effective_capital;
    let mut current_equity = effective_capital;
    let session = TradingSession::new(
        SessionConfig {
            symbol: symbol_upper.clone(),
            initial_equity: effective_capital,
            fallback_fee_pct: estimated_taker_fee_pct,
            max_monthly_drawdown_pct: run_spec.execution_context.max_monthly_drawdown_pct,
            halt_retry_ms: 5_000,
            // A market order settles in milliseconds; half a minute without a terminal status
            // means a lost report or a stuck order.
            entry_timeout_ms: 30_000,
        },
        strategy,
    );
    let mut driver = SessionDriver::new(session, effective_dollar_bar)?;
    // Live trades up to this instant were already covered by the warm-up history.
    let mut warmup_cutoff_ms = i64::MIN;
    if let Some(minutes) = args.warmup_minutes {
        let now_ms = now_unix_ms();
        warmup_cutoff_ms = now_ms;
        let from_ms = now_ms - i64::from(minutes) * 60_000;
        println!("[*] Warming the strategy up with {minutes} minutes of recent trades...");
        let history = adapters::BinanceRecentTrades::futures(is_testnet)
            .fetch(&symbol_upper, from_ms, now_ms)
            .await
            .with_context(|| "Failed to fetch the warm-up trades")?;
        let trade_count = history.len();
        let bars = driver.warm_up_trades(history)?;
        println!("[*] Warm-up: {trade_count} trades formed {bars} dollar bars");
    }
    let sink = EventSink {
        telegram: &telegram,
        bq: &bq_sink,
        run_ctx: &run_ctx,
        symbol: &symbol_upper,
        monthly_limit_pct: run_spec.execution_context.max_monthly_drawdown_pct,
        fill_recorder: args.record_fills.as_ref().map(FillRecorder::new),
    };
    let mut total_ticks: u64 = 0;
    let mut total_bars: u64 = 0;
    let mut last_mtm_print = Instant::now();
    let mtm_interval = Duration::from_secs(5);

    let mut current_day = now_in_madrid().date_naive();
    let mut last_daily_report_date: Option<chrono::NaiveDate> = None;
    let mut stats = LiveStats::default();
    let mut daily_timer = tokio::time::interval(Duration::from_secs(10));

    println!("------------------------------------------------------------");
    println!(">>> ATSNT LIVE TRADING ENGINE ARMED AND READY <<<");
    println!("Awaiting market trades and strategy triggers (Press Ctrl+C to terminate)...\n");

    let shutdown_signal = tokio::signal::ctrl_c();
    tokio::pin!(shutdown_signal);

    loop {
        tokio::select! {
            biased;

            // Periodic account re-verification; never changes anything while exposed.
            _ = account_check.tick() => {
                let exposed = driver.session().is_exposed();
                match gateway
                    .verify_account(&symbol_upper, !args.no_auto_fix && !exposed, exposed)
                    .await
                {
                    Ok(report) if report.is_ready() => {
                        account_read_failures = 0;
                        if !account_ok {
                            println!("[OK] Account profile matches again; entries re-enabled.");
                        }
                        account_ok = true;
                        driver.set_account_ok(true);
                    }
                    Ok(report) => {
                        account_read_failures = 0;
                        let first = account_ok;
                        account_ok = false;
                        driver.set_account_ok(false);
                        eprintln!("[CRITICAL] Account profile drifted; new entries are blocked.\n{}", report.render());
                        if first {
                            if let Some(t) = &telegram {
                                let _ = t
                                    .send_alert(&format!(
                                        "🚨 *ATSNT ACCOUNT DRIFT* `{symbol_upper}`: new entries blocked.\n{}",
                                        report.render()
                                    ))
                                    .await;
                            }
                        }
                    }
                    Err(e) => {
                        account_read_failures += 1;
                        warn!(error = %e, failures = account_read_failures, "Account re-verification could not read the exchange; keeping the last verdict");
                        // Three misses in a row (30 min) means the profile is effectively unverified.
                        if account_read_failures == 3 {
                            eprintln!("[CRITICAL] The account profile could not be verified for {} minutes.", 3 * ACCOUNT_RECHECK_SECS / 60);
                            if let Some(t) = &telegram {
                                let _ = t
                                    .send_alert(&format!(
                                        "🚨 *ATSNT* `{symbol_upper}`: the account profile could not be verified for the last {} minutes (last error: {e}).",
                                        3 * ACCOUNT_RECHECK_SECS / 60
                                    ))
                                    .await;
                            }
                        }
                    }
                }
            }

            // Handle graceful shutdown on Ctrl+C
            _ = &mut shutdown_signal => {
                println!("\n[SHUTDOWN] Interruption signal received (Ctrl+C). Terminating engine...");
                let _ = user_stream.shutdown().await;
                break;
            }

            _ = daily_timer.tick() => {
                let now_madrid = now_in_madrid();
                if now_madrid.date_naive() != current_day {
                    current_day = now_madrid.date_naive();
                    stats.reset_day();
                }

                if now_madrid.hour() == 20 && last_daily_report_date != Some(now_madrid.date_naive()) {
                    last_daily_report_date = Some(now_madrid.date_naive());
                    if let Some(t) = &telegram {
                        let active_pos_desc = match driver.session().open_position() {
                            Some((s, ep, q)) => format!("{:?} (qty: {}, entry: ${})", s, q, ep),
                            None => "Flat (No open position)".to_string(),
                        };
                        let dd = if peak_equity > Decimal::ZERO && peak_equity > current_equity {
                            (peak_equity - current_equity) / peak_equity
                        } else {
                            Decimal::ZERO
                        };
                        let date_str = now_madrid.format("%Y-%m-%d").to_string();
                        let msg = TelegramNotifier::format_daily_summary(
                            &date_str,
                            if is_testnet { "Futures Testnet" } else { "Futures Production" },
                            &symbol_upper,
                            stats.trades,
                            stats.wins,
                            stats.losses,
                            stats.gross,
                            stats.fees,
                            stats.net,
                            current_equity,
                            current_equity,
                            dd,
                            &active_pos_desc,
                            total_ticks,
                            total_bars,
                        );
                        let _ = t.send_alert(&msg).await;
                        tracing::info!(target: "telegram", "Dispatched 20:00 Madrid daily performance summary via Telegram");
                    }
                }
            }

            // Ingest private exchange events (Order fills, cancellations, account updates)
            Some(event_res) = user_stream.next_event() => {
                match event_res {
                    Ok(BinancePrivateEvent::FuturesOrderTradeUpdate(update)) => {
                        println!(
                            "\n############################################################\n\
                             # [BINANCE TESTNET] EXECUTION EVENT RECEIVED               #\n\
                             # Symbol:  {:<47} #\n\
                             # Order:   {:<47} #\n\
                             # Side:    {:<47} #\n\
                             # Status:  {:<47} #\n\
                             # Price:   ${:<46} #\n\
                             # Qty:     {:<47} #\n\
                             ############################################################\n",
                            update.symbol,
                            format!("ID {} ({})", update.order_id, update.client_order_id),
                            format!("{:?}", update.side),
                            update.status,
                            update.last_filled_price,
                            update.cumulative_filled_quantity,
                        );

                        let report = ExecutionReport::from(&update);
                        let events = driver.on_execution(gateway.as_ref(), &report).await;
                        sink.handle(events, &mut stats, driver.session().wallet_equity()).await;
                    }
                    Ok(BinancePrivateEvent::FuturesAccountUpdate(acct)) => {
                        for bal in acct.balances {
                            if bal.asset == "USDT" {
                                current_equity = bal.wallet_balance;
                                if current_equity > peak_equity {
                                    peak_equity = current_equity;
                                }
                                driver.on_wallet_balance(current_equity);
                            }
                        }
                    }
                    Ok(BinancePrivateEvent::FuturesListenKeyExpired { .. }) => {
                        account_check.reset_immediately();
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!(error = %e, "Private user data stream warning/error encountered");
                    }
                }
            }

            // Ingest public market trade ticks
            trade_res = market_stream.next_trade() => {
                match trade_res {
                    Ok(Some(trade)) if trade.timestamp <= warmup_cutoff_ms => {
                        // Already part of the warm-up history; feeding it twice would repeat
                        // those trades in the strategy's window.
                        tracing::trace!(target: "live_trading", "Skipped a trade covered by the warm-up");
                    }
                    Ok(Some(trade)) => {
                        total_ticks += 1;
                        data_window.start_ms.get_or_insert(trade.timestamp);
                        data_window.end_ms = Some(trade.timestamp);

                        if args.show_ticks {
                            println!(
                                "[LIVE TICK #{:>6}] ts: {} | price: {:>10} | qty: {:>8} | side: {:<4}",
                                total_ticks,
                                trade.timestamp,
                                trade.price,
                                trade.quantity,
                                format!("{:?}", trade.side)
                            );
                        } else {
                            tracing::trace!(
                                target: "live_trading",
                                tick = total_ticks,
                                price = %trade.price,
                                qty = %trade.quantity,
                                side = ?trade.side,
                                "Live tick processed"
                            );
                        }

                        // The shared loop: monthly kill-switch and emergency close (retried
                        // until confirmed flat), then bar sampling, then the strategy.
                        let step = driver.on_market_trade(gateway.as_ref(), &trade).await;

                        // Periodic MTM logging and BigQuery telemetry
                        if last_mtm_print.elapsed() >= mtm_interval {
                            last_mtm_print = Instant::now();
                            let dd_pct = if peak_equity > Decimal::ZERO && peak_equity > current_equity {
                                (peak_equity - current_equity) / peak_equity
                            } else {
                                Decimal::ZERO
                            };

                            println!(
                                "  [LIVE MTM] Mark: ${:>10} | Equity: ${:>10.2} | Drawdown: {:>5.2}%",
                                trade.price,
                                current_equity,
                                dd_pct * dec!(100)
                            );

                        }

                        if let Some(bar) = &step.bar {
                            total_bars += 1;
                            tracing::info!(
                                target: "live_trading",
                                bar_id = total_bars,
                                open = %bar.open,
                                high = %bar.high,
                                low = %bar.low,
                                close = %bar.close,
                                volume = %bar.volume,
                                dollar_volume = %bar.dollar_volume,
                                "Dollar Bar finalized"
                            );
                            println!(
                                "[LIVE BAR #{:04}] Window: {} -> {} ({} ms) | O: {} | H: {} | L: {} | C: {} | Vol: ${:.0}",
                                total_bars,
                                bar.start_time,
                                bar.end_time,
                                bar.end_time - bar.start_time,
                                bar.open,
                                bar.high,
                                bar.low,
                                bar.close,
                                bar.dollar_volume
                            );
                        }
                        sink.handle(step.events, &mut stats, driver.session().wallet_equity()).await;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!(error = %e, "Market data WebSocket error encountered");
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        }
    }

    println!("\n============================================================");
    println!("     ATSNT LIVE TRADING SESSION TERMINATED                 ");
    println!("============================================================");
    println!(" Symbol:               {}", symbol_upper);
    println!(" Total Ticks Received: {}", total_ticks);
    println!(" Total Dollar Bars:    {}", total_bars);
    println!(" Final Account Equity: ${:.2}", current_equity);
    println!("============================================================\n");

    if let Some(bq) = &bq_sink {
        let closed_trades = driver.session().closed_trades();
        let max_dd_amount = closed_trades
            .iter()
            .map(|t| t.excursion.max_drawdown_amount)
            .max()
            .unwrap_or_default();
        let max_dd_pct = closed_trades
            .iter()
            .map(|t| t.excursion.max_drawdown_pct)
            .max()
            .unwrap_or_default();
        let metrics =
            BacktestMetrics::calculate(effective_capital, closed_trades, max_dd_amount, max_dd_pct);
        record_lifecycle(bq, &run_ctx, &run_spec, RunStatus::Stopped, &data_window).await;
        if let Err(e) = bq
            .record_metrics(&[run_ctx.metrics_row("full", &metrics, None)])
            .await
        {
            println!("[WARN] Failed to record live run metrics: {e}");
        }
    }

    Ok(())
}

/// Per-day trade statistics for the Telegram summary plus the lifetime count of recorded trades.
#[derive(Default)]
struct LiveStats {
    trades: usize,
    wins: usize,
    losses: usize,
    gross: Decimal,
    fees: Decimal,
    net: Decimal,
    recorded_trades: usize,
}

impl LiveStats {
    /// Clears the daily figures at the day rollover; the lifetime trade count is kept.
    fn reset_day(&mut self) {
        *self = Self {
            recorded_trades: self.recorded_trades,
            ..Self::default()
        };
    }
}

/// Turns the session's events into console output, Telegram alerts and BigQuery rows.
struct EventSink<'a> {
    telegram: &'a Option<TelegramNotifier>,
    bq: &'a Option<BigQuerySink>,
    run_ctx: &'a RunContext,
    symbol: &'a str,
    monthly_limit_pct: Decimal,
    fill_recorder: Option<FillRecorder>,
}

impl EventSink<'_> {
    async fn handle(&self, events: Vec<SessionEvent>, stats: &mut LiveStats, wallet: Decimal) {
        for event in events {
            match event {
                SessionEvent::Signal(intent) => println!(
                    "\n************************************************************\n\
                     [STRATEGY SIGNAL] Intent: {:?} at ${}\n\
                     Stop Loss:    ${}\n\
                     Take Profit:  ${}\n\
                     Max Hold:     {} bars\n\
                     ************************************************************",
                    intent.side,
                    intent.price,
                    intent.stop_loss,
                    intent.take_profit,
                    intent.max_bars_hold
                ),
                SessionEvent::SignalSkipped(reason) => println!(
                    "[RISK] Signal skipped: {}",
                    match reason {
                        SkipReason::AccountMismatch =>
                            "the account no longer matches the strategy profile.",
                        SkipReason::Halted =>
                            "trading is halted (monthly circuit breaker or emergency halt).",
                        SkipReason::ExitsPending => "exits or cleanup still pending.",
                        SkipReason::PositionOpen => "Position already open.",
                    }
                ),
                SessionEvent::EntrySubmitted(ack) => {
                    if let Some(recorder) = &self.fill_recorder {
                        // The moment the request left, not the moment the acknowledgement came
                        // back: parity calibration depends on the round trip being visible.
                        // An order whose submission had to be reconciled has no known send
                        // time; it is left out rather than recorded with the arrival time.
                        if ack.sent_at_ms > 0 {
                            recorder.on_entry_submitted(ack.sent_at_ms);
                        } else {
                            recorder.discard_pending();
                        }
                    }
                    println!(
                        "[GATEWAY] Dispatching live order to Binance...\n\
                     \n[ORDER PLACED] Order submitted to Binance!\n\
                     Client Order ID: {}\n\
                     Status:          {}\n\
                     Price:           ${}\n\
                     Quantity:        {}\n",
                        ack.order_id, ack.status, ack.price, ack.quantity
                    );
                }
                SessionEvent::EntryCancelled => println!(
                    "[ORDER] {}: the entry order ended without any fill; no position was opened.",
                    self.symbol
                ),
                SessionEvent::EntryTimedOut {
                    closed_unreported_position,
                } => {
                    let detail = if closed_unreported_position {
                        "the exchange held a position the bot had not been told about; it was closed at market"
                    } else {
                        "no fill was observed; the order was cancelled"
                    };
                    println!(
                        "[CRITICAL] {}: the entry order never reached a terminal status; {detail}.",
                        self.symbol
                    );
                    if let Some(t) = self.telegram {
                        let _ = t
                            .send_alert(&format!(
                                "🚨 *ATSNT* `{}`: an entry order timed out ({detail}).",
                                self.symbol
                            ))
                            .await;
                    }
                }
                SessionEvent::EntryTimeoutFailed(reason) => {
                    error!(error = %reason, "Could not resolve a timed-out entry order");
                    eprintln!(
                        "[CRITICAL] {}: an entry order timed out and could not be resolved ({reason}); retrying.",
                        self.symbol
                    );
                    if let Some(t) = self.telegram {
                        let _ = t
                            .send_alert(&format!(
                                "🚨 *ATSNT* `{}`: a timed-out entry could not be resolved ({reason}); retrying.",
                                self.symbol
                            ))
                            .await;
                    }
                }
                SessionEvent::EntryFailed(reason) => {
                    error!(error = %reason, "Failed to submit live order to Binance");
                    eprintln!("[ERROR] Order placement failed: {reason}");
                }
                SessionEvent::PositionOpened {
                    side,
                    price,
                    quantity,
                    time_ms,
                    stop_loss,
                    take_profit,
                    protection,
                } => {
                    if let Some(recorder) = &self.fill_recorder {
                        if let Err(error) = recorder.on_entry_filled(side, quantity, price, time_ms)
                        {
                            eprintln!("[WARN] Could not record the entry fill: {error}");
                        }
                    }
                    announce_exit_event(self.telegram, self.symbol, &protection).await;
                    if let Some(t) = self.telegram {
                        let side_str = match side {
                            Side::Buy => "Long",
                            Side::Sell => "Short",
                        };
                        let msg = TelegramNotifier::format_position_opened(
                            self.symbol,
                            side_str,
                            price,
                            quantity,
                            stop_loss,
                            take_profit,
                            wallet,
                        );
                        let _ = t.send_alert(&msg).await;
                    }
                }
                SessionEvent::Exit(exit) => {
                    announce_exit_event(self.telegram, self.symbol, &exit).await;
                }
                SessionEvent::PositionClosed(trade) => {
                    stats.trades += 1;
                    if trade.pnl_net > Decimal::ZERO {
                        stats.wins += 1;
                    } else if trade.pnl_net < Decimal::ZERO {
                        stats.losses += 1;
                    }
                    stats.gross += trade.pnl_gross;
                    stats.fees += trade.fees_paid;
                    stats.net += trade.pnl_net;

                    if let Some(t) = self.telegram {
                        let side_str = match trade.side {
                            PositionSide::Long => "Long",
                            PositionSide::Short => "Short",
                        };
                        let msg = TelegramNotifier::format_trade_summary(
                            self.symbol,
                            side_str,
                            &trade.exit_reason,
                            trade.entry_price,
                            trade.exit_price,
                            trade.quantity,
                            trade.holding_duration_seconds,
                            trade.pnl_gross,
                            trade.fees_paid,
                            trade.pnl_net,
                            trade.return_pct,
                            wallet,
                        );
                        let _ = t.send_alert(&msg).await;
                    }

                    stats.recorded_trades += 1;
                    if let Some(bq) = self.bq {
                        if let Err(e) =
                            record_closed_trade(bq, self.run_ctx, &trade, stats.recorded_trades)
                                .await
                        {
                            println!(
                                "[WARN] Failed to record live trade #{}: {e}",
                                stats.recorded_trades
                            );
                        }
                    }
                }
                SessionEvent::MonthlyBreakerTripped { drawdown } => {
                    eprintln!(
                        "[CRITICAL] Monthly circuit breaker tripped at {:.2}% drawdown vs month open: no new positions will be opened. Review and flatten any open position manually.",
                        drawdown * dec!(100)
                    );
                    if let Some(t) = self.telegram {
                        let msg = TelegramNotifier::format_circuit_breaker(
                            self.symbol,
                            drawdown * dec!(100),
                            self.monthly_limit_pct * dec!(100),
                        );
                        let _ = t.send_alert(&msg).await;
                    }
                }
            }
        }
    }
}

/// Appends a lifecycle row for the live run; failures are logged, never fatal.
/// Prints an exit-lifecycle event and alerts on Telegram for the ones an operator must see.
async fn announce_exit_event(telegram: &Option<TelegramNotifier>, symbol: &str, event: &ExitEvent) {
    let (critical, text) = match event {
        ExitEvent::Protected => (false, "Exchange-held stop loss and take profit are live".to_string()),
        ExitEvent::CleanedUp => (false, "Leftover orders cancelled after the exit".to_string()),
        ExitEvent::TimeBarrierClosed => (false, "Time barrier reached: position closed at market".to_string()),
        ExitEvent::Halted => (false, "Emergency halt complete: position flat, orders cancelled, no new entries until restart".to_string()),
        ExitEvent::UnprotectedEntryClosed { reason } => (
            true,
            format!("Protective orders failed ({reason}); position closed at market"),
        ),
        ExitEvent::UnprotectedEntryCloseFailed { reason } => (
            true,
            format!("POSITION MAY BE UNPROTECTED and the emergency close failed ({reason}). Check the account NOW"),
        ),
        ExitEvent::CleanupPending { reason } => (
            true,
            format!("Could not cancel leftover orders ({reason}); retrying on the next bar"),
        ),
        ExitEvent::TimeBarrierCloseFailed { reason } => (
            true,
            format!("Time-barrier close failed ({reason}); exchange SL/TP remain, retrying"),
        ),
        ExitEvent::HaltingFailed { reason } => (
            true,
            format!("Monthly halt could not confirm a flat position ({reason}); retrying"),
        ),
    };
    if critical {
        eprintln!("[CRITICAL] {symbol}: {text}");
    } else {
        println!("[EXITS] {symbol}: {text}");
    }
    if critical || matches!(event, ExitEvent::Halted) {
        if let Some(notifier) = telegram {
            let _ = notifier
                .send_alert(&format!("🚨 *ATSNT EXITS* `{}`: {}", symbol, text))
                .await;
        }
    }
}

async fn record_lifecycle<R: RunRecorder>(
    recorder: &R,
    ctx: &RunContext,
    spec: &RunSpec<serde_json::Value>,
    status: RunStatus,
    data: &DataWindow,
) {
    if let Err(e) = record_run_status(recorder, ctx, spec, status, data).await {
        println!(
            "[WARN] Failed to record live run status {}: {e}",
            status.as_str()
        );
    }
}
