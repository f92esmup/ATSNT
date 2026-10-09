//! Live Real-Time Paper Trading Runner for ATSNT.
//!
//! Subscribes to the live Binance aggregate trade WebSocket stream, aggregates trades into
//! information-driven Dollar Bars, executes strategy signals against simulated liquidity
//! and realistic friction via the [`PaperTradingSession`], and displays live execution telemetry.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use adapters::{
    format_unix_ms_rfc3339, AlertNotifier, AsyncMarketDataStream, BigQuerySink,
    BinanceWebSocketStream, BinanceWsConfig, DollarBarRow, EquitySnapshotRow, TelegramNotifier,
    TradeRow,
};
use anyhow::{Context, Result};
use backtest::{BacktestConfig, PaperTradingConfig, PaperTradingEvent, PaperTradingSession};
use clap::Parser;
use domain::PositionSide;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy, MarketType, Strategy};

#[derive(Parser, Debug)]
#[command(
    name = "paper_trading",
    about = "ATSNT - Real-Time Paper Trading Engine against Live Binance WebSocket"
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

    /// Select Spot; the configured DollarBarsCusum_v1 strategy is currently Futures-only
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Starting paper trading capital in USDT
    #[arg(long, default_value = "10000")]
    capital: Decimal,

    /// Fixed fractional risk percentage per trade (e.g. 0.01 for 1%)
    #[arg(long, default_value = "0.01")]
    risk_pct: Decimal,

    /// Save audit performance report JSON upon session termination
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set
    )]
    save_report: bool,

    /// Stream real-time equity snapshots and trades to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Send execution and barrier exit alerts to Telegram
    #[arg(long, default_value_t = false)]
    telegram_alerts: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    let args = Args::parse();

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
    println!(" Dollar Bar Threshold: ${}", args.dollar_bar);
    println!(" Initial Capital:      ${}", args.capital);
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
        let t = TelegramNotifier::from_env();
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
    let backtest_config = BacktestConfig {
        initial_capital: args.capital,
        risk_per_trade_pct: args.risk_pct,
        ..BacktestConfig::default()
    };

    let paper_config = PaperTradingConfig {
        dollar_bar_threshold: args.dollar_bar,
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

    println!("[*] Connecting to WebSocket: {}", ws_config.stream_url());
    let mut stream = BinanceWebSocketStream::connect(ws_config)
        .with_context(|| "Failed to connect to Binance WebSocket stream")?;

    println!("[*] Engine initialized. Awaiting market trades (Press Ctrl+C to terminate)...\n");

    let session_start_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let session_id = format!("paper_{}_{}", args.symbol.to_lowercase(), session_start_ts);
    let mut trade_counter: u64 = 0;

    let mut total_ticks: u64 = 0;
    let mut total_bars: u64 = 0;
    let mut last_mark_price = Decimal::ZERO;
    let mut last_mtm_print = Instant::now();
    let mtm_interval = Duration::from_secs(1);
    let mut last_entry_info: Option<(PositionSide, Decimal, Decimal, i64)> = None;

    loop {
        tokio::select! {
            biased;

            _ = tokio::signal::ctrl_c() => {
                println!("\n[SHUTDOWN] Interruption signal received (Ctrl+C). Liquidating open positions...");
                break;
            }

            trade_res = stream.next_trade() => {
                match trade_res {
                    Ok(Some(trade)) => {
                        total_ticks += 1;
                        last_mark_price = trade.price;

                        // Print trade tick telemetry
                        println!(
                            "[TICK #{:>6}] ts: {} | price: {:>10} | qty: {:>8} | side: {:<4} | notional: ${:>10}",
                            total_ticks,
                            trade.timestamp,
                            trade.price,
                            trade.quantity,
                            format!("{:?}", trade.side),
                            trade.dollar_value()
                        );

                        // Process trade through PaperTradingSession
                        let events = session.process_trade(&trade);

                        for event in events {
                            match event {
                                PaperTradingEvent::BarFormed(bar) => {
                                    total_bars += 1;
                                    println!(
                                        "\n------------------------------------------------------------\n\
                                         >>> DOLLAR BAR #{:04} FINALIZED <<<\n\
                                         Time Window:  {} -> {} ({} ms)\n\
                                         OHLC:         O: {} | H: {} | L: {} | C: {}\n\
                                         Volume:       {} base | ${} dollar volume\n\
                                         Trade Count:  {}\n\
                                         ------------------------------------------------------------\n",
                                        total_bars,
                                        bar.start_time,
                                        bar.end_time,
                                        bar.end_time - bar.start_time,
                                        bar.open,
                                        bar.high,
                                        bar.low,
                                        bar.close,
                                        bar.volume,
                                        bar.dollar_volume,
                                        bar.trade_count
                                    );
                                    if let Some(bq) = &bq_sink {
                                        let bar_row = DollarBarRow {
                                            bar_id: format!("bar_{:06}", total_bars),
                                            session_id: session_id.clone(),
                                            symbol: args.symbol.to_uppercase(),
                                            start_timestamp: format_unix_ms_rfc3339(bar.start_time),
                                            close_timestamp: format_unix_ms_rfc3339(bar.end_time),
                                            open: bar.open.round_dp(4),
                                            high: bar.high.round_dp(4),
                                            low: bar.low.round_dp(4),
                                            close: bar.close.round_dp(4),
                                            volume: bar.volume.round_dp(4),
                                            dollar_volume: bar.dollar_volume.round_dp(4),
                                            trade_count: bar.trade_count,
                                            duration_ms: (bar.end_time - bar.start_time).max(0),
                                        };
                                        let _ = bq.insert_dollar_bars(&[bar_row]).await;
                                    }
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
                                    let (side_str, entry_p, qty, entry_ts) = match last_entry_info.take() {
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
                                    let holding_duration = ((trade.timestamp - entry_ts) / 1000).max(0);

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
                                        exit_reason, side_str, entry_p, exit_price, qty, holding_duration, net_pnl, total_equity
                                    );
                                    if let Some(t) = &telegram {
                                        let msg = TelegramNotifier::format_position_closed(
                                            &args.symbol,
                                            &exit_reason,
                                            exit_price,
                                            net_pnl,
                                            total_equity,
                                        );
                                        let _ = t.send_alert(&msg).await;
                                    }
                                    if let Some(bq) = &bq_sink {
                                        trade_counter += 1;
                                        let row = TradeRow {
                                            trade_id: format!("{}_t{:05}", session_id, trade_counter),
                                            session_id: session_id.clone(),
                                            strategy_id: selected_strategy.name().to_string(),
                                            symbol: args.symbol.to_uppercase(),
                                            side: side_str,
                                            entry_timestamp: format_unix_ms_rfc3339(entry_ts),
                                            exit_timestamp: format_unix_ms_rfc3339(trade.timestamp),
                                            entry_price: entry_p.round_dp(4),
                                            exit_price: exit_price.round_dp(4),
                                            quantity: qty.round_dp(4),
                                            gross_pnl: net_pnl.round_dp(4),
                                            fees_paid: dec!(0).round_dp(4),
                                            net_pnl: net_pnl.round_dp(4),
                                            exit_reason: exit_reason.clone(),
                                            holding_duration_seconds: holding_duration,
                                        };
                                        let _ = bq.insert_trades(&[row]).await;
                                    }
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
                                        if let Some(bq) = &bq_sink {
                                            let row = EquitySnapshotRow {
                                                timestamp: format_unix_ms_rfc3339(trade.timestamp),
                                                session_id: session_id.clone(),
                                                symbol: args.symbol.to_uppercase(),
                                                cash_equity: session.cash_equity().round_dp(4),
                                                unrealized_pnl: unrealized_pnl.round_dp(4),
                                                total_equity: total_equity.round_dp(4),
                                                drawdown_pct: drawdown_pct.round_dp(4),
                                                active_position_side: session.active_position().map(|p| match p.side {
                                                    PositionSide::Long => "Long".to_string(),
                                                    PositionSide::Short => "Short".to_string(),
                                                }),
                                                active_position_qty: session.active_position().map(|p| p.quantity.round_dp(4)),
                                            };
                                            let _ = bq.insert_equity_snapshots(&[row]).await;
                                        }
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
        if let (Some(bq), Some(last_trade)) = (&bq_sink, closed_trades.last()) {
            trade_counter += 1;
            let side_str = match last_trade.side {
                PositionSide::Long => "Long",
                PositionSide::Short => "Short",
            };
            let row = TradeRow {
                trade_id: format!("{}_t{:05}", session_id, trade_counter),
                session_id: session_id.clone(),
                strategy_id: selected_strategy.name().to_string(),
                symbol: args.symbol.to_uppercase(),
                side: side_str.to_string(),
                entry_timestamp: format_unix_ms_rfc3339(last_trade.entry_time),
                exit_timestamp: format_unix_ms_rfc3339(last_trade.exit_time),
                entry_price: last_trade.entry_price.round_dp(4),
                exit_price: last_trade.exit_price.round_dp(4),
                quantity: last_trade.quantity.round_dp(4),
                gross_pnl: last_trade.pnl_gross.round_dp(4),
                fees_paid: last_trade.fees_paid.round_dp(4),
                net_pnl: last_trade.pnl_net.round_dp(4),
                exit_reason: last_trade.exit_reason.clone(),
                holding_duration_seconds: last_trade.holding_duration_seconds,
            };
            let _ = bq.insert_trades(&[row]).await;
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
    println!(" Dollar Bar Threshold:      ${}", args.dollar_bar);
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

    // 6. Save JSON Audit Telemetry Report
    if args.save_report {
        let reports_dir = Path::new("storage/reports");
        fs::create_dir_all(reports_dir)?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let report_path = reports_dir.join(format!("paper_trading_{timestamp}.json"));

        let report = json!({
            "timestamp": timestamp,
            "symbol": args.symbol.to_uppercase(),
            "stream_mode": if args.spot { "spot" } else { "futures" },
            "dollar_bar_threshold": args.dollar_bar,
            "initial_capital": args.capital,
            "final_equity": args.capital + metrics.net_profit,
            "raw_ticks_processed": total_ticks,
            "dollar_bars_formed": total_bars,
            "backtest_config": backtest_config,
            "strategy_config": strat_config,
            "metrics": metrics,
            "closed_trades": closed_trades,
        });

        fs::write(&report_path, serde_json::to_string_pretty(&report)?)?;
        println!(
            "[SAVED] Audit report successfully written to: {}",
            report_path.display()
        );
    }

    Ok(())
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
    use super::validate_strategy_market;
    use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy, MarketType};

    #[test]
    fn paper_runner_rejects_the_futures_only_strategy_for_spot() {
        let strategy = DollarBarsCusumStrategy::new(DollarBarsCusumConfig::default()).unwrap();

        assert!(validate_strategy_market(&strategy, MarketType::Spot).is_err());
        assert!(validate_strategy_market(&strategy, MarketType::UsdMPerpetual).is_ok());
    }
}
