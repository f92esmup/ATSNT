//! Live Execution Gateway Runner for ATSNT on Binance Futures Testnet and Production.
//!
//! Ingests the real-time Binance aggregate trade WebSocket stream, aggregates trades into
//! information-driven Dollar Bars, evaluates algorithmic signals from [`DollarBarsCusumStrategy`],
//! validates pre-trade risk policy constraints, dispatches authenticated live orders via [`BinanceGateway`],
//! and listens for execution updates and order fills over the private [`BinancePrivateUserDataStream`].

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adapters::{
    format_unix_ms_rfc3339, now_in_madrid, AlertNotifier, AsyncMarketDataStream, BigQuerySink,
    BinanceGateway, BinanceGatewayConfig, BinancePrivateEvent, BinancePrivateUserDataStream,
    BinanceWebSocketStream, BinanceWsConfig, EquitySnapshotRow, GcpSecretManager, TelegramNotifier,
    TradeRow,
};
use anyhow::{Context, Result};
use chrono::Timelike;
use clap::Parser;
use domain::{DollarBarAggregator, OrderIntent, RiskPolicy, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy, MarketType, Strategy};
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

    /// Dollar bar accumulation threshold in USDT
    #[arg(long, default_value = "50000")]
    dollar_bar: Decimal,

    /// Optional path to JSON strategy configuration (e.g. configs/hpo_results.json)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Use official Binance Futures Testnet (defaults to true for capital safety)
    #[arg(long, default_value_t = true)]
    testnet: bool,

    /// Enable live production trading with real funds (DANGER: disables Testnet mode)
    #[arg(long, default_value_t = false)]
    production: bool,

    /// Fixed fractional risk percentage per trade (e.g. 0.01 for 1%)
    #[arg(long, default_value = "0.01")]
    risk_pct: Decimal,

    /// Stream real-time equity snapshots and trades to Google Cloud BigQuery
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
}

fn validate_strategy_market(strategy: &DollarBarsCusumStrategy, market: MarketType) -> Result<()> {
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
    println!(" Dollar Bar Threshold: ${}", args.dollar_bar);
    println!(" Risk per Trade:       {:.2}%", args.risk_pct * dec!(100));

    let bq_sink = if args.gcp_bigquery {
        let sink = BigQuerySink::from_env();
        println!(
            " BigQuery Telemetry:   {}",
            if sink.is_enabled() {
                "ACTIVE (Streaming to atsnt_bi)"
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

    // 1. Load Strategy Configuration
    let strat_config: DollarBarsCusumConfig = if let Some(config_path) = &args.config {
        println!(
            "[*] Loading strategy parameters from: {}",
            config_path.display()
        );
        let file = File::open(config_path)
            .with_context(|| format!("Failed to open config file: {}", config_path.display()))?;
        serde_json::from_reader(file)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?
    } else {
        println!("[*] Using default CUSUM + Z-Score strategy hyperparameters");
        DollarBarsCusumConfig::default()
    };

    let mut strategy = DollarBarsCusumStrategy::new(strat_config.clone())?;
    validate_strategy_market(&strategy, MarketType::UsdMPerpetual)?;

    println!(
        "[*] Strategy Hyperparameters: Window={}, VolMultiplier={}, Z_Entry={}, Z_Stop={}, TimeBarrier={}",
        strat_config.rolling_window_len,
        strat_config.cusum_vol_multiplier,
        strat_config.z_entry_threshold,
        strat_config.z_stop_threshold,
        strat_config.time_barrier_bars
    );

    // 2. Initialize Binance Execution Gateway
    let gateway_config = BinanceGatewayConfig {
        api_key,
        secret_key,
        testnet: is_testnet,
        spot: false, // USD-M Perpetual
        recv_window_ms: 5000,
        risk_policy: RiskPolicy::default(),
    };
    let gateway = Arc::new(BinanceGateway::new(gateway_config)?);

    // 3. Query Account Balance & Health Check
    println!("[*] Verifying HMAC signatures & querying account balance...");
    let initial_balance = gateway
        .fetch_balance("USDT")
        .await
        .with_context(|| "Failed to query account balance from Binance")?;

    println!(
        "[OK] Gateway Authenticated! Available USDT Balance: ${:.2}\n",
        initial_balance
    );

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
    println!(
        "[*] Subscribing to public market trade feed: wss://fstream.binancefuture.com/ws/{}@aggTrade",
        args.symbol.to_ascii_lowercase()
    );
    let ws_config = BinanceWsConfig::futures(&args.symbol);
    let mut market_stream = BinanceWebSocketStream::connect(ws_config)
        .with_context(|| "Failed to connect to public Binance WebSocket feed")?;

    let session_start_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let session_id = format!("live_{}_{}", args.symbol.to_lowercase(), session_start_ts);

    let mut aggregator = DollarBarAggregator::new(args.dollar_bar)?;
    let mut peak_equity = initial_balance;
    let mut current_equity = initial_balance;
    let mut total_ticks: u64 = 0;
    let mut total_bars: u64 = 0;
    let mut last_mtm_print = Instant::now();
    let mtm_interval = Duration::from_secs(5);
    let mut active_order_intent: Option<OrderIntent> = None;
    let mut position_open: bool = false;
    let mut active_position_tracker: Option<(Side, Decimal, Decimal, u64)> = None;

    let mut current_day = now_in_madrid().date_naive();
    let mut last_daily_report_date: Option<chrono::NaiveDate> = None;
    let mut daily_trades_count: usize = 0;
    let mut daily_wins: usize = 0;
    let mut daily_losses: usize = 0;
    let mut daily_gross_pnl = Decimal::ZERO;
    let mut daily_fees = Decimal::ZERO;
    let mut daily_net_pnl = Decimal::ZERO;
    let mut daily_timer = tokio::time::interval(Duration::from_secs(10));

    println!("------------------------------------------------------------");
    println!(">>> ATSNT LIVE TRADING ENGINE ARMED AND READY <<<");
    println!("Awaiting market trades and strategy triggers (Press Ctrl+C to terminate)...\n");

    let shutdown_signal = tokio::signal::ctrl_c();
    tokio::pin!(shutdown_signal);

    loop {
        tokio::select! {
            biased;

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
                    daily_trades_count = 0;
                    daily_wins = 0;
                    daily_losses = 0;
                    daily_gross_pnl = Decimal::ZERO;
                    daily_fees = Decimal::ZERO;
                    daily_net_pnl = Decimal::ZERO;
                }

                if now_madrid.hour() == 20 && last_daily_report_date != Some(now_madrid.date_naive()) {
                    last_daily_report_date = Some(now_madrid.date_naive());
                    if let Some(t) = &telegram {
                        let active_pos_desc = match &active_position_tracker {
                            Some((s, ep, q, _)) => format!("{:?} (qty: {}, entry: ${})", s, q, ep),
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
                            daily_trades_count,
                            daily_wins,
                            daily_losses,
                            daily_gross_pnl,
                            daily_fees,
                            daily_net_pnl,
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

                        if update.status == "FILLED" {
                            let is_closing = active_position_tracker.is_some();
                            if !is_closing {
                                // Position opened
                                active_position_tracker = Some((
                                    update.side,
                                    update.last_filled_price,
                                    update.cumulative_filled_quantity,
                                    update.transaction_time_ms,
                                ));
                                position_open = true;

                                if let Some(t) = &telegram {
                                    let side_str = match update.side {
                                        Side::Buy => "Long",
                                        Side::Sell => "Short",
                                    };
                                    let (sl, tp) = match &active_order_intent {
                                        Some(intent) => (intent.stop_loss, intent.take_profit),
                                        None => (Decimal::ZERO, Decimal::ZERO),
                                    };
                                    let msg = TelegramNotifier::format_position_opened(
                                        &symbol_upper,
                                        side_str,
                                        update.last_filled_price,
                                        update.cumulative_filled_quantity,
                                        sl,
                                        tp,
                                        current_equity,
                                    );
                                    let _ = t.send_alert(&msg).await;
                                }
                            } else {
                                // Position closed
                                let (entry_side, entry_price, entry_qty, entry_ts) = active_position_tracker.take().unwrap_or((
                                    update.side,
                                    update.last_filled_price,
                                    update.cumulative_filled_quantity,
                                    update.transaction_time_ms,
                                ));
                                let duration_secs = ((update.transaction_time_ms.saturating_sub(entry_ts)) / 1000) as i64;
                                let gross_pnl = match entry_side {
                                    Side::Buy => (update.last_filled_price - entry_price) * entry_qty,
                                    Side::Sell => (entry_price - update.last_filled_price) * entry_qty,
                                };
                                let fees_paid = (entry_price * entry_qty + update.last_filled_price * entry_qty) * dec!(0.0005);
                                let net_pnl = gross_pnl - fees_paid;
                                let return_pct = if entry_price > Decimal::ZERO && entry_qty > Decimal::ZERO {
                                    net_pnl / (entry_price * entry_qty)
                                } else {
                                    Decimal::ZERO
                                };

                                daily_trades_count += 1;
                                if net_pnl > Decimal::ZERO {
                                    daily_wins += 1;
                                } else if net_pnl < Decimal::ZERO {
                                    daily_losses += 1;
                                }
                                daily_gross_pnl += gross_pnl;
                                daily_fees += fees_paid;
                                daily_net_pnl += net_pnl;
                                position_open = false;
                                active_order_intent = None;

                                if let Some(t) = &telegram {
                                    let side_str = match entry_side {
                                        Side::Buy => "Long",
                                        Side::Sell => "Short",
                                    };
                                    let msg = TelegramNotifier::format_trade_summary(
                                        &symbol_upper,
                                        side_str,
                                        &update.status,
                                        entry_price,
                                        update.last_filled_price,
                                        entry_qty,
                                        duration_secs,
                                        gross_pnl,
                                        fees_paid,
                                        net_pnl,
                                        return_pct,
                                        current_equity,
                                    );
                                    let _ = t.send_alert(&msg).await;
                                }

                                if let Some(bq) = &bq_sink {
                                    let side_str = match entry_side {
                                        Side::Buy => "Buy",
                                        Side::Sell => "Sell",
                                    };
                                    let trade_identifier = update.trade_id.unwrap_or(0);
                                    let row = TradeRow {
                                        trade_id: format!("{}_{}_{}", update.symbol, update.order_id, trade_identifier),
                                        session_id: session_id.clone(),
                                        strategy_id: strategy.name().to_string(),
                                        symbol: update.symbol.clone(),
                                        side: side_str.to_string(),
                                        entry_timestamp: format_unix_ms_rfc3339(entry_ts as i64),
                                        exit_timestamp: format_unix_ms_rfc3339(update.transaction_time_ms as i64),
                                        entry_price: entry_price.round_dp(4),
                                        exit_price: update.last_filled_price.round_dp(4),
                                        quantity: entry_qty.round_dp(4),
                                        gross_pnl: gross_pnl.round_dp(4),
                                        fees_paid: fees_paid.round_dp(4),
                                        net_pnl: net_pnl.round_dp(4),
                                        exit_reason: update.status.clone(),
                                        holding_duration_seconds: duration_secs,
                                    };
                                    let _ = bq.insert_trades(&[row]).await;
                                }
                            }
                        }
                    }
                    Ok(BinancePrivateEvent::FuturesAccountUpdate(acct)) => {
                        for bal in acct.balances {
                            if bal.asset == "USDT" {
                                current_equity = bal.wallet_balance;
                                if current_equity > peak_equity {
                                    peak_equity = current_equity;
                                }
                            }
                        }
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
                    Ok(Some(trade)) => {
                        total_ticks += 1;

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

                            if let Some(bq) = &bq_sink {
                                let snapshot = EquitySnapshotRow {
                                    timestamp: format_unix_ms_rfc3339(trade.timestamp),
                                    session_id: session_id.clone(),
                                    symbol: symbol_upper.clone(),
                                    cash_equity: current_equity.round_dp(4),
                                    unrealized_pnl: Decimal::ZERO.round_dp(4),
                                    total_equity: current_equity.round_dp(4),
                                    drawdown_pct: dd_pct.round_dp(4),
                                    active_position_side: None,
                                    active_position_qty: Some(Decimal::ZERO.round_dp(4)),
                                };
                                let _ = bq.insert_equity_snapshots(&[snapshot]).await;
                            }
                        }

                        // Feed trade to DollarBarAggregator
                        if let Some(bar) = aggregator.process_trade(&trade) {
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
                            // NOTE: Real-time Dollar Bar BigQuery streaming is omitted to preserve network bandwidth.

                            // Strategy signal evaluation
                            if let Some(intent) = strategy.on_bar(&bar) {
                                println!(
                                    "\n************************************************************\n\
                                     [STRATEGY SIGNAL] Intent: {:?} at ${}\n\
                                     Stop Loss:    ${}\n\
                                     Take Profit:  ${}\n\
                                     Max Hold:     {} bars\n\
                                     ************************************************************",
                                    intent.side, intent.price, intent.stop_loss, intent.take_profit, intent.max_bars_hold
                                );

                                // Check if we already have an active open position
                                if !position_open {
                                    println!("[GATEWAY] Dispatching live order to Binance...");
                                    match gateway.place_order(
                                        &symbol_upper,
                                        &intent,
                                        current_equity,
                                        peak_equity,
                                        Decimal::ZERO,
                                    ).await {
                                        Ok(report) => {
                                            println!(
                                                "\n[ORDER PLACED] Order submitted to Binance!\n\
                                                 Order ID:        {}\n\
                                                 Client Order ID: {}\n\
                                                 Status:          {}\n\
                                                 Price:           ${}\n\
                                                 Quantity:        {}\n",
                                                report.order_id,
                                                report.client_order_id,
                                                report.status,
                                                report.price,
                                                report.original_qty
                                            );
                                            active_order_intent = Some(intent);
                                            position_open = true;
                                        }
                                        Err(e) => {
                                            error!(error = %e, "Failed to submit live order to Binance");
                                            eprintln!("[ERROR] Order placement failed: {}", e);
                                        }
                                    }
                                } else {
                                    println!("[RISK] Signal skipped: Position already open.");
                                }
                            }
                        }
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

    Ok(())
}
