//! Live Real-Time Paper Trading Runner for ATSNT.
//!
//! Subscribes to the live Binance aggregate trade WebSocket stream, aggregates trades into
//! information-driven Dollar Bars, executes strategy signals against simulated liquidity
//! and realistic friction via the [`PaperTradingSession`], and displays live execution telemetry.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use adapters::{
    now_in_madrid, AlertNotifier, AsyncMarketDataStream, BigQuerySink, BinanceWebSocketStream,
    BinanceWsConfig, GcpSecretManager, RunRecorder, TelegramNotifier,
};
use anyhow::{Context, Result};
use backtest::{
    now_unix_ms, BacktestConfig, ClosedTrade, DataWindow, PaperTradingConfig, PaperTradingEvent,
    PaperTradingSession, RunContext, RunSpec, RunStatus, RunType, SamplingConfig,
};
use chrono::Timelike;
use clap::Parser;
use domain::PositionSide;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{
    DollarBarsCusumConfig, DollarBarsCusumStrategy, MarketType, OptimizableStrategy, Strategy,
};

#[derive(Parser, Debug)]
#[command(
    name = "paper_trading",
    about = "ATSNT - Real-Time Paper Trading Engine against Live Binance WebSocket"
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

    /// Select Spot; the configured DollarBarsCusum_v1 strategy is currently Futures-only
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Starting paper trading capital in USDT
    #[arg(long, default_value = "10000")]
    capital: Decimal,

    /// Fixed fractional risk percentage per trade (e.g. 0.01 for 1%)
    #[arg(long, default_value = "0.01")]
    risk_pct: Decimal,

    /// Record the session run (lifecycle, closed trades with equity events, final metrics) to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Send execution and barrier exit alerts to Telegram
    #[arg(long, default_value_t = false)]
    telegram_alerts: bool,

    /// Print individual raw market trade ticks to stdout (verbose debug mode)
    #[arg(long, default_value_t = false)]
    show_ticks: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    let args = Args::parse();
    let started_at_ms = now_unix_ms();

    // 1. Load Strategy Configuration & Authoritative Deployment Artifact
    let (strat_config, loaded_config) = if let Some(config_path) = &args.config {
        println!(
            "[*] Loading strategy configuration from: {}",
            config_path.display()
        );
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
                        if scorecard.is_statistically_significant { "PASS" } else { "FAIL" }
                    );
                }
            }
            backtest::LoadedStrategyConfig::Bare(_) => {
                println!(
                    "[*] Legacy bare hyperparameter configuration loaded (no deployment manifest)"
                );
            }
        }
        (loaded.strategy_config().clone(), Some(loaded))
    } else {
        println!("[*] Using default CUSUM + Z-Score strategy hyperparameters");
        (DollarBarsCusumConfig::default(), None)
    };

    // Contract validation
    if let Some(loaded) = &loaded_config {
        if let Some(strat_id) = loaded.strategy_id() {
            if strat_id != "dollar_bars_cusum" && strat_id != "cusum" {
                anyhow::bail!(
                    "Deployment artifact strategy '{}' is not compatible with paper_trading runner (expected 'dollar_bars_cusum')",
                    strat_id
                );
            }
        }
        if let Some(artifact_sym) = loaded.symbol() {
            if !artifact_sym.eq_ignore_ascii_case(&args.symbol) {
                println!(
                    "[WARNING] Artifact symbol ({}) does not match session symbol ({})!",
                    artifact_sym,
                    args.symbol.to_ascii_uppercase()
                );
            }
        }
    }

    // Resolve Dollar Bar threshold: CLI override > Artifact manifest > Fallback default ($50,000)
    let effective_dollar_bar = match (
        args.dollar_bar,
        loaded_config
            .as_ref()
            .and_then(|c| c.dollar_bar_threshold()),
    ) {
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

    println!("============================================================");
    println!("     ATSNT - Real-Time Paper Trading Engine (Milestone 4)   ");
    println!("============================================================");
    println!(" Symbol:               {}", args.symbol.to_uppercase());
    println!(
        " Market Stream:        {}",
        if args.spot {
            "Binance Spot (wss://stream.binance.com:9443)"
        } else {
            "Binance Futures USDT-M (wss://fstream.binancefuture.com)"
        }
    );
    println!(" Dollar Bar Threshold: ${}", effective_dollar_bar);
    println!(" Initial Capital:      ${}", args.capital);
    println!(" Risk per Trade:       {:.2}%", args.risk_pct * dec!(100));

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

    let secret_manager = GcpSecretManager::from_env();
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

    let selected_strategy = DollarBarsCusumStrategy::new(strat_config.clone())?;
    let market_type = if args.spot {
        MarketType::Spot
    } else {
        MarketType::UsdMPerpetual
    };
    validate_strategy_market(&selected_strategy, market_type)?;

    println!(
        "[*] Strategy Hyperparameters: Window={}, VolMultiplier={}, Z_Entry={}, Z_Stop={}, TimeBarrier={}",
        strat_config.rolling_window_len,
        strat_config.cusum_vol_multiplier,
        strat_config.z_entry_threshold,
        strat_config.z_stop_threshold,
        strat_config.time_barrier_bars
    );

    // 2. Initialize Paper Trading Session
    let backtest_config =
        if let Some(artifact_ctx) = loaded_config.as_ref().and_then(|c| c.execution_context()) {
            BacktestConfig {
                initial_capital: if args.capital != dec!(10_000) {
                    args.capital
                } else {
                    artifact_ctx.initial_capital
                },
                risk_per_trade_pct: if args.risk_pct != dec!(0.01) {
                    args.risk_pct
                } else {
                    artifact_ctx.risk_per_trade_pct
                },
                ..*artifact_ctx
            }
        } else {
            BacktestConfig {
                initial_capital: args.capital,
                risk_per_trade_pct: args.risk_pct,
                ..BacktestConfig::default()
            }
        };

    let paper_config = PaperTradingConfig {
        dollar_bar_threshold: effective_dollar_bar,
        backtest: backtest_config,
        strategy: strat_config.clone(),
        channel_capacity: 10_000,
    };

    let mut session = PaperTradingSession::new(paper_config)
        .map_err(|e| anyhow::anyhow!("Failed to initialize paper trading session: {e}"))?;

    // 3. Connect to Binance WebSocket Feed
    let ws_config = if args.spot {
        BinanceWsConfig::spot(&args.symbol)
    } else {
        BinanceWsConfig::futures(&args.symbol)
    };

    let ws_source = ws_config.stream_url();
    println!("[*] Connecting to WebSocket: {}", ws_source);
    let mut stream = BinanceWebSocketStream::connect(ws_config)
        .with_context(|| "Failed to connect to Binance WebSocket stream")?;

    println!("[*] Engine initialized. Awaiting market trades (Press Ctrl+C to terminate)...\n");

    let run_spec = RunSpec {
        strategy_id: DollarBarsCusumStrategy::id().to_string(),
        symbol: args.symbol.to_uppercase(),
        market_type,
        sampling: SamplingConfig::DollarBar {
            threshold: effective_dollar_bar,
        },
        strategy_config: strat_config.clone(),
        execution_context: backtest_config,
    };
    let run_ctx = RunContext::new(RunType::Paper, &run_spec, started_at_ms);
    let mut data_window = DataWindow {
        source: Some(ws_source),
        ..DataWindow::default()
    };
    let mut trade_counter: usize = 0;
    let mut end_status = RunStatus::Stopped;

    if let Some(bq) = &bq_sink {
        println!("[*] Recording paper run {} to BigQuery", run_ctx.run_id);
        record_lifecycle(bq, &run_ctx, &run_spec, RunStatus::Running, &data_window).await;
    }

    let mut total_ticks: u64 = 0;
    let mut total_bars: u64 = 0;
    let mut last_mark_price = Decimal::ZERO;
    let mut last_mtm_print = Instant::now();
    let mtm_interval = Duration::from_secs(1);
    let mut last_entry_info: Option<(PositionSide, Decimal, Decimal, i64)> = None;

    let mut current_day = now_in_madrid().date_naive();
    let mut last_daily_report_date: Option<chrono::NaiveDate> = None;
    let mut daily_trades_count: usize = 0;
    let mut daily_wins: usize = 0;
    let mut daily_losses: usize = 0;
    let mut daily_gross_pnl = Decimal::ZERO;
    let mut daily_fees = Decimal::ZERO;
    let mut daily_net_pnl = Decimal::ZERO;
    let mut daily_timer = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            biased;

            _ = tokio::signal::ctrl_c() => {
                println!("\n[SHUTDOWN] Interruption signal received (Ctrl+C). Liquidating open positions...");
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
                        let active_pos_desc = match session.active_position() {
                            Some(pos) => format!("{:?} (qty: {}, entry: ${})", pos.side, pos.quantity, pos.entry_price),
                            None => "Flat (No open position)".to_string(),
                        };
                        let mark = if last_mark_price > Decimal::ZERO { last_mark_price } else { args.capital };
                        let total_eq = session.total_equity(mark);
                        let dd = session.engine().current_drawdown_pct();
                        let date_str = now_madrid.format("%Y-%m-%d").to_string();
                        let msg = TelegramNotifier::format_daily_summary(
                            &date_str,
                            "Paper Trading",
                            &args.symbol,
                            daily_trades_count,
                            daily_wins,
                            daily_losses,
                            daily_gross_pnl,
                            daily_fees,
                            daily_net_pnl,
                            session.cash_equity(),
                            total_eq,
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

            trade_res = stream.next_trade() => {
                match trade_res {
                    Ok(Some(trade)) => {
                        total_ticks += 1;
                        last_mark_price = trade.price;
                        data_window.start_ms.get_or_insert(trade.timestamp);
                        data_window.end_ms = Some(trade.timestamp);

                        if args.show_ticks {
                            println!(
                                "[TICK #{:>6}] ts: {} | price: {:>10} | qty: {:>8} | side: {:<4} | notional: ${:>10}",
                                total_ticks,
                                trade.timestamp,
                                trade.price,
                                trade.quantity,
                                format!("{:?}", trade.side),
                                trade.dollar_value()
                            );
                        } else {
                            tracing::trace!(
                                target: "paper_trading",
                                tick = total_ticks,
                                price = %trade.price,
                                qty = %trade.quantity,
                                side = ?trade.side,
                                "Tick processed"
                            );
                        }

                        // Process trade through PaperTradingSession
                        let events = session.process_trade(&trade);

                        for event in events {
                            match event {
                                PaperTradingEvent::BarFormed(bar) => {
                                    total_bars += 1;
                                    tracing::info!(
                                        target: "paper_trading",
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
                                        "[BAR #{:04}] Window: {} -> {} ({} ms) | O: {} | H: {} | L: {} | C: {} | Vol: ${:.0}",
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
                                PaperTradingEvent::SignalGenerated(intent) => {
                                    println!(
                                        "\n************************************************************\n\
                                         [SIGNAL] Intent Generated: {:?} at price ${}\n\
                                         Stop Loss:    ${}\n\
                                         Take Profit:  ${}\n\
                                         Max Bars:     {}\n\
                                         ************************************************************",
                                        intent.side, intent.price, intent.stop_loss, intent.take_profit, intent.max_bars_hold
                                    );
                                }
                                PaperTradingEvent::PositionOpened { side, entry_price, quantity, stop_loss, take_profit } => {
                                    last_entry_info = Some((side, entry_price, quantity, trade.timestamp));
                                    println!(
                                        "[EXECUTION] Position Opened: {:?}\n\
                                         Entry Price:  ${}\n\
                                         Quantity:     {} units\n\
                                         Stop Loss:    ${}\n\
                                         Take Profit:  ${}\n\
                                         Cash Equity:  ${:.2}\n\
                                         ************************************************************\n",
                                        side, entry_price, quantity, stop_loss, take_profit, session.cash_equity()
                                    );
                                    if let Some(t) = &telegram {
                                        let side_str = match side {
                                            PositionSide::Long => "Long",
                                            PositionSide::Short => "Short",
                                        };
                                        let msg = TelegramNotifier::format_position_opened(
                                            &args.symbol,
                                            side_str,
                                            entry_price,
                                            quantity,
                                            stop_loss,
                                            take_profit,
                                            session.cash_equity(),
                                        );
                                        let _ = t.send_alert(&msg).await;
                                    }
                                }
                                PaperTradingEvent::PositionClosed { exit_reason, exit_price, net_pnl, total_equity } => {
                                    let closed_trade_opt = session.engine().closed_trades().last().cloned();
                                    let (side_str, entry_p, qty, _entry_ts, gross_pnl, fees_paid, duration, ret_pct) = match &closed_trade_opt {
                                        Some(ct) => (
                                            match ct.side {
                                                PositionSide::Long => "Long".to_string(),
                                                PositionSide::Short => "Short".to_string(),
                                            },
                                            ct.entry_price,
                                            ct.quantity,
                                            ct.entry_time,
                                            ct.pnl_gross,
                                            ct.fees_paid + ct.slippage_paid,
                                            ct.holding_duration_seconds,
                                            ct.return_pct,
                                        ),
                                        None => {
                                            let (s, ep, q, et) = match last_entry_info.take() {
                                                Some((s, ep, q, et)) => (
                                                    match s {
                                                        PositionSide::Long => "Long".to_string(),
                                                        PositionSide::Short => "Short".to_string(),
                                                    },
                                                    ep,
                                                    q,
                                                    et,
                                                ),
                                                None => ("ClosedPosition".to_string(), exit_price, dec!(0), trade.timestamp),
                                            };
                                            let dur = ((trade.timestamp - et) / 1000).max(0);
                                            let ret = if ep > Decimal::ZERO && q > Decimal::ZERO { net_pnl / (ep * q) } else { Decimal::ZERO };
                                            (s, ep, q, et, net_pnl, Decimal::ZERO, dur, ret)
                                        }
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

                                    println!(
                                        "\n============================================================\n\
                                         [BARRIER EXIT] Position Closed: Reason: {}\n\
                                         Side:         {}\n\
                                         Entry Price:  ${}\n\
                                         Exit Price:   ${}\n\
                                         Quantity:     {} units\n\
                                         Duration:     {}s\n\
                                         Realized PnL: ${:.2} (net of fees and slippage)\n\
                                         Total Equity: ${:.2}\n\
                                         ============================================================\n",
                                        exit_reason, side_str, entry_p, exit_price, qty, duration, net_pnl, total_equity
                                    );
                                    if let Some(t) = &telegram {
                                        let msg = TelegramNotifier::format_trade_summary(
                                            &args.symbol,
                                            &side_str,
                                            &exit_reason,
                                            entry_p,
                                            exit_price,
                                            qty,
                                            duration,
                                            gross_pnl,
                                            fees_paid,
                                            net_pnl,
                                            ret_pct,
                                            total_equity,
                                        );
                                        let _ = t.send_alert(&msg).await;
                                    }
                                    if let (Some(bq), Some(ct)) = (&bq_sink, &closed_trade_opt) {
                                        trade_counter += 1;
                                        record_closed_trade(bq, &run_ctx, ct, trade_counter).await;
                                    }
                                }
                                PaperTradingEvent::CircuitBreakerTripped { scope, drawdown_pct } => {
                                    println!(
                                        "\n[ALERT] {} circuit breaker tripped at {:.2}% drawdown: trading halted, no new positions will open.",
                                        scope,
                                        drawdown_pct * dec!(100)
                                    );
                                }
                                PaperTradingEvent::MarkToMarket { current_price, unrealized_pnl, total_equity, drawdown_pct } => {
                                    // Periodic display to avoid terminal flooding
                                    if last_mtm_print.elapsed() >= mtm_interval {
                                        last_mtm_print = Instant::now();
                                        let side_str = session.active_position().map_or("None", |p| match p.side {
                                            PositionSide::Long => "LONG",
                                            PositionSide::Short => "SHORT",
                                        });
                                        println!(
                                            "  [MTM] Side: {:<5} | Mark: ${:>10} | UnrPnL: ${:>8.2} | Equity: ${:>10.2} | DD: {:>5.2}%",
                                            side_str,
                                            current_price,
                                            unrealized_pnl,
                                            total_equity,
                                            drawdown_pct * dec!(100)
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        println!("\n[*] WebSocket stream connection closed by remote server.");
                        break;
                    }
                    Err(e) => {
                        eprintln!("\n[ERROR] Market data stream error: {e}");
                        end_status = RunStatus::Failed;
                        break;
                    }
                }
            }
        }
    }

    // 4. Terminate Session & Liquidate Open Position
    let mark = if last_mark_price > Decimal::ZERO {
        Some(last_mark_price)
    } else {
        None
    };

    let had_open_position = session.active_position().is_some();
    let (metrics, closed_trades) = session.finish(mark);

    if had_open_position {
        println!(
            "\n[LIQUIDATION] Open position marked to market and liquidated at price ${}",
            last_mark_price
        );
        if let Some(last_trade) = closed_trades.last() {
            let side_str = match last_trade.side {
                PositionSide::Long => "Long",
                PositionSide::Short => "Short",
            };
            if let Some(t) = &telegram {
                let msg = TelegramNotifier::format_trade_summary(
                    &args.symbol,
                    side_str,
                    &last_trade.exit_reason,
                    last_trade.entry_price,
                    last_trade.exit_price,
                    last_trade.quantity,
                    last_trade.holding_duration_seconds,
                    last_trade.pnl_gross,
                    last_trade.fees_paid + last_trade.slippage_paid,
                    last_trade.pnl_net,
                    last_trade.return_pct,
                    args.capital + metrics.net_profit,
                );
                let _ = t.send_alert(&msg).await;
            }
            if let Some(bq) = &bq_sink {
                trade_counter += 1;
                record_closed_trade(bq, &run_ctx, last_trade, trade_counter).await;
            }
        }
    }

    if let Some(bq) = &bq_sink {
        record_lifecycle(bq, &run_ctx, &run_spec, end_status, &data_window).await;
        if let Err(e) = bq
            .record_metrics(&[run_ctx.metrics_row("full", &metrics, None)])
            .await
        {
            println!("[WARN] Failed to record paper run metrics: {e}");
        }
    }

    // 5. Display Quantitative Performance Report
    println!("\n============================================================");
    println!("     ATSNT REAL-TIME PAPER TRADING PERFORMANCE REPORT       ");
    println!("============================================================");
    println!(" Symbol:                    {}", args.symbol.to_uppercase());
    println!(
        " Market Stream:             {}",
        if args.spot { "Spot" } else { "USDT-M Futures" }
    );
    println!(" Dollar Bar Threshold:      ${}", effective_dollar_bar);
    println!(" Initial Capital:           ${:.2}", args.capital);
    println!(" Raw Ticks Processed:       {}", total_ticks);
    println!(" Dollar Bars Formed:        {}", total_bars);
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
    println!(
        " Final Portfolio Cash:      ${:.2}",
        args.capital + metrics.net_profit
    );
    println!("============================================================");

    Ok(())
}

/// Appends a lifecycle row for the paper run; failures are logged, never fatal.
async fn record_lifecycle<R: RunRecorder>(
    recorder: &R,
    ctx: &RunContext,
    spec: &RunSpec<DollarBarsCusumConfig>,
    status: RunStatus,
    data: &DataWindow,
) {
    if let Err(e) = backtest::record_run_status(recorder, ctx, spec, status, data).await {
        println!(
            "[WARN] Failed to record paper run status {}: {e}",
            status.as_str()
        );
    }
}

/// Records one closed trade and its four equity events; failures are logged, never fatal.
async fn record_closed_trade<R: RunRecorder>(
    recorder: &R,
    ctx: &RunContext,
    trade: &ClosedTrade,
    number: usize,
) {
    if let Err(e) = backtest::record_closed_trade(recorder, ctx, trade, number).await {
        println!("[WARN] Failed to record paper trade #{number}: {e}");
    }
}

fn validate_strategy_market(strategy: &impl Strategy, market_type: MarketType) -> Result<()> {
    if strategy.supports_market_type(market_type) {
        return Ok(());
    }

    anyhow::bail!(
        "Strategy '{}' does not support the selected {:?} market; refusing to reinterpret its signals",
        strategy.name(),
        market_type
    )
}

#[cfg(test)]
mod tests {
    use super::{record_closed_trade, record_lifecycle, validate_strategy_market};
    use adapters::InMemoryRunRecorder;
    use backtest::{
        BacktestConfig, ClosedTrade, DataWindow, RunContext, RunSpec, RunStatus, RunType,
        SamplingConfig, TradeExcursion,
    };
    use domain::PositionSide;
    use rust_decimal_macros::dec;
    use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy, MarketType};

    fn spec() -> RunSpec<DollarBarsCusumConfig> {
        RunSpec {
            strategy_id: "dollar_bars_cusum".into(),
            symbol: "BTCUSDT".into(),
            market_type: MarketType::UsdMPerpetual,
            sampling: SamplingConfig::DollarBar {
                threshold: dec!(1_000_000),
            },
            strategy_config: DollarBarsCusumConfig::default(),
            execution_context: BacktestConfig::default(),
        }
    }

    #[tokio::test]
    async fn paper_run_lifecycle_appends_running_then_final_status() {
        let recorder = InMemoryRunRecorder::new();
        let ctx = RunContext::new(RunType::Paper, &spec(), 0);
        let data = DataWindow::default();

        record_lifecycle(&recorder, &ctx, &spec(), RunStatus::Running, &data).await;
        record_lifecycle(&recorder, &ctx, &spec(), RunStatus::Stopped, &data).await;

        let runs = recorder.snapshot().runs;
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (runs[0].status.as_str(), runs[0].finished_at.is_none()),
            ("running", true)
        );
        assert_eq!(
            (runs[1].status.as_str(), runs[1].finished_at.is_some()),
            ("stopped", true)
        );
        assert_eq!(runs[0].run_id, runs[1].run_id);
    }

    #[tokio::test]
    async fn closed_trade_records_one_trade_and_four_equity_events() {
        let recorder = InMemoryRunRecorder::new();
        let ctx = RunContext::new(RunType::Paper, &spec(), 0);
        let trade = ClosedTrade {
            entry_time: 1_000,
            exit_time: 5_000,
            entry_price: dec!(100),
            exit_price: dec!(101),
            quantity: dec!(1),
            side: PositionSide::Long,
            pnl_gross: dec!(1),
            fees_paid: dec!(0.1),
            slippage_paid: dec!(0),
            pnl_net: dec!(0.9),
            return_pct: dec!(0.009),
            exit_reason: "TakeProfit".into(),
            holding_duration_seconds: 4,
            excursion: TradeExcursion::start(1_000, dec!(10_000)),
        };

        record_closed_trade(&recorder, &ctx, &trade, 3).await;

        let rec = recorder.snapshot();
        assert_eq!(rec.trades.len(), 1);
        assert!(rec.trades[0].trade_id.ends_with("_t00003"));
        assert_eq!(rec.series.len(), 4);
    }

    #[test]
    fn paper_runner_rejects_the_futures_only_strategy_for_spot() {
        let strategy = DollarBarsCusumStrategy::new(DollarBarsCusumConfig::default()).unwrap();

        assert!(validate_strategy_market(&strategy, MarketType::Spot).is_err());
        assert!(validate_strategy_market(&strategy, MarketType::UsdMPerpetual).is_ok());
    }
}
