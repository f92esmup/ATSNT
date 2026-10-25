use std::path::PathBuf;
use std::time::Instant;

use adapters::{BinanceParquetReader, MarketDataStream};
use anyhow::{Context, Result};
use backtest::{EventMerger, PortfolioEngine, PortfolioEngineConfig};
use clap::Parser;
use domain::{DollarBarAggregator, PortfolioRiskPolicy};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::langevin_potential::{LangevinConfig, LangevinPotentialStrategy};

#[derive(Parser, Debug)]
#[command(
    name = "run_portfolio",
    about = "ATSNT - Multi-Asset Algorithmic Portfolio Backtesting Engine"
)]
struct Args {
    /// Path to BTC historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/BTCUSDT")]
    btc_data: PathBuf,

    /// Path to ETH historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/ETHUSDT")]
    eth_data: PathBuf,

    /// Path to SOL historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/SOLUSDT")]
    sol_data: PathBuf,

    /// Optional path to BTC strategy config JSON (e.g. configs/hpo_langevin_swing_v2_winner.json)
    #[arg(long)]
    btc_config: Option<PathBuf>,

    /// Optional path to ETH strategy config JSON
    #[arg(long)]
    eth_config: Option<PathBuf>,

    /// Dollar bar accumulation threshold for BTC (default: $8M for day trading, $25M for swing)
    #[arg(long, default_value = "8000000")]
    btc_dollar_bar: Decimal,

    /// Dollar bar accumulation threshold for ETH (default: $3M for day trading, $10M for swing)
    #[arg(long, default_value = "3000000")]
    eth_dollar_bar: Decimal,

    /// Dollar bar accumulation threshold for SOL (default: $1.5M for day trading, $5M for swing)
    #[arg(long, default_value = "1500000")]
    sol_dollar_bar: Decimal,

    /// Initial shared capital in quote currency (USDT)
    #[arg(long, default_value = "10000")]
    capital: Decimal,

    /// Maximum concurrent open positions across the portfolio (default: 5)
    #[arg(long, default_value_t = 5)]
    max_positions: usize,

    /// Maximum positions in the same direction (default: 3)
    #[arg(long, default_value_t = 3)]
    max_directional: usize,

    /// Risk percentage per trade relative to total equity (default: 0.010 = 1.0%)
    #[arg(long, default_value = "0.010")]
    risk_per_trade: Decimal,

    /// Maximum margin allocation per position as fraction of equity (default: 0.20 = 20%)
    #[arg(long, default_value = "0.20")]
    max_slot_margin: Decimal,

    /// Whether to enter using passive limit orders with TIF timeout
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    use_limit_orders: bool,

    /// Limit order Time-In-Force timeout in bars (default: 2)
    #[arg(long, default_value_t = 2)]
    limit_timeout_bars: usize,

    /// Maximum bar holding duration before horizontal barrier timeout (default: 45 for day trading)
    #[arg(long, default_value_t = 45)]
    time_barrier_bars: usize,
}

fn load_asset_bars(
    symbol: &str,
    path: &PathBuf,
    dollar_bar_threshold: Decimal,
) -> Result<Vec<domain::DollarBar>> {
    println!(
        "[*] Loading and aggregating {symbol} from {}",
        path.display()
    );
    let mut reader = BinanceParquetReader::open(path).with_context(|| {
        format!(
            "Failed to open Parquet dataset for {symbol} at {}",
            path.display()
        )
    })?;

    let mut aggregator = DollarBarAggregator::new(dollar_bar_threshold)
        .map_err(|e| anyhow::anyhow!("Invalid dollar bar threshold: {e}"))?;
    let mut bars = Vec::new();
    let mut trade_count = 0usize;
    let start = Instant::now();

    while let Some(trade) = reader.next_trade()? {
        trade_count += 1;
        if let Some(bar) = aggregator.process_trade(&trade) {
            bars.push(bar);
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "    [OK] {symbol}: Processed {trade_count} trades into {} dollar bars in {:.2}s ({:.0} trades/s)",
        bars.len(),
        elapsed,
        trade_count as f64 / elapsed.max(0.001)
    );

    Ok(bars)
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("============================================================");
    println!(" ATSNT - Multi-Asset Algorithmic Portfolio Engine");
    println!("============================================================");
    println!("Initial Capital:       ${:.2} USDT", args.capital);
    println!("Max Open Positions:    {}", args.max_positions);
    println!("Max Directional:       {}", args.max_directional);
    println!(
        "Risk Per Trade:        {:.2}%",
        args.risk_per_trade * dec!(100)
    );
    println!(
        "Max Slot Margin:       {:.1}%",
        args.max_slot_margin * dec!(100)
    );
    println!(
        "Execution Mode:        {}",
        if args.use_limit_orders {
            "Maker Limit (TIF Timeout)"
        } else {
            "Aggressive Market (Taker)"
        }
    );
    println!("Time Barrier:          {} bars", args.time_barrier_bars);
    println!("------------------------------------------------------------");

    // 1. Resolve strategy configs and dollar bar thresholds
    let (mut btc_cfg, btc_threshold) = if let Some(path) = args.btc_config.as_ref() {
        let loaded = backtest::load_strategy_config::<LangevinConfig>(path)?;
        (loaded.strategy_config().clone(), args.btc_dollar_bar)
    } else {
        (
            LangevinConfig {
                window_size: 100,
                bandwidth_factor: dec!(2.0),
                decay_eta: dec!(0.01),
                vacuum_epsilon: dec!(0.001),
                beta_entry: dec!(0.60),
                alpha_dissociation: dec!(0.25),
                zeta_thermal_tp: dec!(0.10),
                lambda_ema_period: 20,
                time_barrier_bars: args.time_barrier_bars,
                min_take_profit_pct: dec!(0.008),
                min_well_depth_pct: dec!(0.010),
                enable_kramers_breakout: true,
                kramers_lambda_threshold: dec!(1.5),
                kramers_tp_multiple: dec!(1.0),
                opposite_wall_fraction: dec!(1.00),
                enable_dynamic_breakeven: true,
                breakeven_distance_fraction: dec!(0.50),
                macro_trend_filter: true,
                macro_ema_period: 200,
                macro_slope_window: 50,
                kinematic_exhaustion_window: 6,
                min_wall_absorption_ratio: dec!(0.52),
                use_limit_entries: args.use_limit_orders,
                limit_time_in_force_bars: if args.use_limit_orders {
                    Some(args.limit_timeout_bars)
                } else {
                    None
                },
            },
            args.btc_dollar_bar,
        )
    };

    btc_cfg.breakeven_distance_fraction = dec!(0.50);
    btc_cfg.enable_dynamic_breakeven = true;
    btc_cfg.use_limit_entries = args.use_limit_orders;
    btc_cfg.limit_time_in_force_bars = if args.use_limit_orders {
        Some(args.limit_timeout_bars)
    } else {
        None
    };

    let (mut eth_cfg, eth_threshold) = if let Some(path) = args.eth_config.as_ref() {
        let loaded = backtest::load_strategy_config::<LangevinConfig>(path)?;
        (
            loaded.strategy_config().clone(),
            loaded.dollar_bar_threshold().unwrap_or(args.eth_dollar_bar),
        )
    } else {
        let mut cfg = btc_cfg.clone();
        cfg.min_take_profit_pct = dec!(0.009);
        cfg.min_well_depth_pct = dec!(0.012);
        (cfg, args.eth_dollar_bar)
    };

    eth_cfg.breakeven_distance_fraction = dec!(0.50);
    eth_cfg.enable_dynamic_breakeven = true;
    eth_cfg.use_limit_entries = args.use_limit_orders;
    eth_cfg.limit_time_in_force_bars = if args.use_limit_orders {
        Some(args.limit_timeout_bars)
    } else {
        None
    };

    let mut sol_cfg = btc_cfg.clone();
    sol_cfg.min_take_profit_pct = dec!(0.009);
    sol_cfg.min_well_depth_pct = dec!(0.012);
    sol_cfg.breakeven_distance_fraction = dec!(0.50);
    sol_cfg.enable_dynamic_breakeven = true;
    sol_cfg.use_limit_entries = args.use_limit_orders;
    sol_cfg.limit_time_in_force_bars = if args.use_limit_orders {
        Some(args.limit_timeout_bars)
    } else {
        None
    };

    // 2. Ingest and aggregate market data for each asset
    let btc_bars = load_asset_bars("BTCUSDT", &args.btc_data, btc_threshold)?;
    let eth_bars = load_asset_bars("ETHUSDT", &args.eth_data, eth_threshold)?;

    let sol_bars = if args.sol_data.exists() {
        Some(load_asset_bars(
            "SOLUSDT",
            &args.sol_data,
            args.sol_dollar_bar,
        )?)
    } else {
        None
    };

    // 3. Synchronize chronological stream using EventMerger
    println!("[*] Merging multi-asset streams into chronological priority queue...");
    let mut merger = EventMerger::new();
    let total_bars = btc_bars.len() + eth_bars.len() + sol_bars.as_ref().map_or(0, |v| v.len());

    for bar in &btc_bars {
        merger.push("BTCUSDT", bar);
    }
    for bar in &eth_bars {
        merger.push("ETHUSDT", bar);
    }
    if let Some(bars) = sol_bars.as_ref() {
        for bar in bars {
            merger.push("SOLUSDT", bar);
        }
    }
    println!("    [OK] Stream merged: {total_bars} total events ready for simulation");

    // 4. Initialize Strategy Instances per symbol
    let mut btc_strat = LangevinPotentialStrategy::new(btc_cfg)?;
    let mut eth_strat = LangevinPotentialStrategy::new(eth_cfg)?;
    let mut sol_strat = LangevinPotentialStrategy::new(sol_cfg)?;

    // 4. Initialize Portfolio Risk Policy & Engine
    let risk_policy = PortfolioRiskPolicy {
        max_open_positions: args.max_positions,
        max_directional_positions: args.max_directional,
        risk_per_trade_pct: args.risk_per_trade,
        max_slot_margin_pct: args.max_slot_margin,
        max_leverage: dec!(20),
        min_order_quantity: dec!(0.001),
        quantity_step_size: dec!(0.001),
        min_order_notional: dec!(5),
    };

    let engine_config = PortfolioEngineConfig {
        initial_capital: args.capital,
        maker_fee_pct: dec!(0.0002), // 0.02%
        taker_fee_pct: dec!(0.0005), // 0.05%
        slippage_pct: dec!(0.0005),
        risk_policy,
        latency_ms: 100,
    };

    let mut engine = PortfolioEngine::new(engine_config);

    // 5. Run Chronological Multi-Asset Simulation Loop
    println!("[*] Executing multi-asset portfolio simulation...");
    let sim_start = Instant::now();

    while let Some(tagged) = merger.pop() {
        match tagged.symbol.as_str() {
            "BTCUSDT" => engine.process_event("BTCUSDT", tagged.event, &mut btc_strat),
            "ETHUSDT" => engine.process_event("ETHUSDT", tagged.event, &mut eth_strat),
            "SOLUSDT" => engine.process_event("SOLUSDT", tagged.event, &mut sol_strat),
            _ => {}
        }
    }

    let sim_elapsed = sim_start.elapsed().as_secs_f64();
    println!("    [OK] Simulation completed in {:.2}s", sim_elapsed);

    // 6. Generate Performance Report
    let closed = engine.closed_trades();
    let total_trades = closed.len();
    let wins = closed.iter().filter(|t| t.pnl_net > Decimal::ZERO).count();
    let losses = closed.iter().filter(|t| t.pnl_net < Decimal::ZERO).count();
    let win_rate = if total_trades > 0 {
        (wins as f64 / total_trades as f64) * 100.0
    } else {
        0.0
    };

    let gross_profit: Decimal = closed
        .iter()
        .filter(|t| t.pnl_net > Decimal::ZERO)
        .map(|t| t.pnl_net)
        .sum();
    let gross_loss: Decimal = closed
        .iter()
        .filter(|t| t.pnl_net < Decimal::ZERO)
        .map(|t| t.pnl_net.abs())
        .sum();
    let profit_factor = if gross_loss > Decimal::ZERO {
        gross_profit / gross_loss
    } else if gross_profit > Decimal::ZERO {
        dec!(99.99)
    } else {
        dec!(0)
    };

    let net_pnl = engine.total_realized_pnl();
    let max_dd_pct = engine.max_drawdown_pct() * dec!(100);

    // Monthly PnL aggregation
    let mut monthly_pnl: std::collections::BTreeMap<String, (Decimal, usize)> =
        std::collections::BTreeMap::new();

    for trade in closed {
        let month_str = chrono::DateTime::from_timestamp_millis(trade.exit_time)
            .map(|dt| dt.format("%Y-%m").to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        let entry = monthly_pnl.entry(month_str).or_insert((Decimal::ZERO, 0));
        entry.0 += trade.pnl_net;
        entry.1 += 1;
    }

    let calc_stats = |trades: &[&backtest::ClosedTrade]| {
        let count = trades.len();
        let wins = trades.iter().filter(|t| t.pnl_net > Decimal::ZERO).count();
        let losses = trades.iter().filter(|t| t.pnl_net < Decimal::ZERO).count();
        let win_rate = if count > 0 {
            (wins as f64 / count as f64) * 100.0
        } else {
            0.0
        };
        let gross_p: Decimal = trades
            .iter()
            .filter(|t| t.pnl_net > Decimal::ZERO)
            .map(|t| t.pnl_net)
            .sum();
        let gross_l: Decimal = trades
            .iter()
            .filter(|t| t.pnl_net < Decimal::ZERO)
            .map(|t| t.pnl_net.abs())
            .sum();
        let pf = if gross_l > Decimal::ZERO {
            gross_p / gross_l
        } else if gross_p > Decimal::ZERO {
            dec!(99.99)
        } else {
            dec!(0)
        };
        let net: Decimal = trades.iter().map(|t| t.pnl_net).sum();
        let mut exit_reasons: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for t in trades {
            *exit_reasons.entry(t.exit_reason.clone()).or_default() += 1;
        }
        (count, wins, losses, win_rate, pf, net, exit_reasons)
    };

    let by_symbol = engine.closed_trades_by_symbol();

    println!("============================================================");
    println!(" PORTFOLIO BACKTEST PERFORMANCE REPORT");
    println!("============================================================");
    println!(" Total Closed Trades:  {}", total_trades);
    println!(
        " Win Rate:             {:.2}% ({wins}W / {losses}L)",
        win_rate
    );
    println!(" Profit Factor:        {:.2}", profit_factor);
    println!(" Net Realized PnL:     {:+.2} USDT", net_pnl);
    println!(" Max Portfolio DD:     {:.2}%", max_dd_pct);
    println!(
        " Final Total Equity:   {:.2} USDT",
        engine.total_marked_equity()
    );
    println!("------------------------------------------------------------");
    println!(" SYMBOL ATTRIBUTION:");
    let mut sorted_symbols: Vec<_> = by_symbol.keys().collect();
    sorted_symbols.sort();
    for sym in sorted_symbols {
        let sym_trades = &by_symbol[sym];
        let refs: Vec<&backtest::ClosedTrade> = sym_trades.iter().collect();
        let stats = calc_stats(&refs);
        println!(
            "   {sym:<8}: {} trades | Win: {:.1}% ({}/{}W) | PF: {:.2} | PnL: {:+.2} USDT",
            stats.0, stats.3, stats.1, stats.0, stats.4, stats.5
        );
        print!("     Exits: ");
        for (r, c) in &stats.6 {
            print!("{r}: {c} | ");
        }
        println!();
    }
    println!("------------------------------------------------------------");
    println!(" MONTHLY PERFORMANCE BREAKDOWN:");
    for (month, (pnl, cnt)) in &monthly_pnl {
        println!("   {month}: {cnt:>2} trades | PnL: {:+8.2} USDT", pnl);
    }
    println!("------------------------------------------------------------");
    println!(" REJECTIONS & CONCURRENCY STATS:");
    for (reason, count) in engine.rejected_orders() {
        println!("   {reason:<26}: {count}");
    }
    println!("============================================================");

    Ok(())
}
