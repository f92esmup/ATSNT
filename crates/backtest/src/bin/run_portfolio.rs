use std::path::PathBuf;
use std::time::Instant;

use adapters::{BinanceParquetReader, MarketDataStream};
use anyhow::{Context, Result};
use backtest::{load_portfolio_artifact, EventMerger, PortfolioEngine, PortfolioEngineConfig};
use clap::Parser;
use domain::{DollarBarAggregator, PortfolioRiskPolicy};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{MarketType, OptimizableStrategy, StrategyVisitor};

#[derive(Parser, Debug)]
#[command(
    name = "run_portfolio",
    about = "ATSNT - Multi-Asset Algorithmic Portfolio Backtesting Engine"
)]
struct Args {
    /// Strategy identifier or alias (e.g. "quantum_well_swing", "quantum", "langevin_potential", "langevin")
    #[arg(long, default_value = "langevin")]
    strategy: String,

    /// Whether to run in Spot market mode (strict 1x leverage, 0% funding, 0.10% fees)
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Whether to apply Binance 25% BNB fee discount (0.075% in Spot, 0.018%/0.045% in Futures)
    #[arg(long, default_value_t = false)]
    bnb_discount: bool,

    /// Path to BTC historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/BTCUSDT")]
    btc_data: PathBuf,

    /// Path to ETH historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/ETHUSDT")]
    eth_data: PathBuf,

    /// Path to SOL historical market data (.parquet file or directory)
    #[arg(long, default_value = "data/historical/SOLUSDT")]
    sol_data: PathBuf,

    /// Optional path to BTC strategy config JSON
    #[arg(long)]
    btc_config: Option<PathBuf>,

    /// Optional path to ETH strategy config JSON
    #[arg(long)]
    eth_config: Option<PathBuf>,

    /// Optional path to SOL strategy config JSON
    #[arg(long)]
    sol_config: Option<PathBuf>,

    /// Optional path to Portfolio Deployment Artifact JSON (adopts all assets, thresholds, fees, risk policies, and configs)
    #[arg(long)]
    portfolio_config: Option<PathBuf>,

    /// Dollar bar accumulation threshold for BTC (default: $8M for day trading, $25M for swing)
    #[arg(long, default_value = "25000000")]
    btc_dollar_bar: Decimal,

    /// Dollar bar accumulation threshold for ETH (default: $8M for day trading, $20M for swing)
    #[arg(long, default_value = "20000000")]
    eth_dollar_bar: Decimal,

    /// Dollar bar accumulation threshold for SOL (default: $1.5M for day trading, $4M for swing)
    #[arg(long, default_value = "4000000")]
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

struct PortfolioRun<'a> {
    args: &'a Args,
    market_type: MarketType,
}

impl<'a> StrategyVisitor for PortfolioRun<'a> {
    type Output = Result<()>;

    fn visit<S>(self) -> Result<()>
    where
        S: OptimizableStrategy + 'static,
        S::Config: Default,
        S::Space: serde::Serialize + serde::de::DeserializeOwned,
    {
        run_portfolio_with_strategy::<S>(self.args, self.market_type)
    }
}

fn run_portfolio_with_strategy<S>(args: &Args, market_type: MarketType) -> Result<()>
where
    S: OptimizableStrategy + 'static,
    S::Config: Default,
    S::Space: serde::Serialize + serde::de::DeserializeOwned,
{
    let shared = S::SharedConfig::default();

    let portfolio_artifact = if let Some(path) = args.portfolio_config.as_ref() {
        Some(load_portfolio_artifact(path)?)
    } else {
        None
    };

    // 1. Resolve strategy configs and dollar bar thresholds per symbol
    let (btc_cfg, btc_threshold, eth_cfg, eth_threshold, sol_cfg, sol_threshold) =
        if let Some(artifact) = portfolio_artifact.as_ref() {
            let mut btc_c = S::Config::default();
            let mut btc_t = args.btc_dollar_bar;
            let mut eth_c = S::Config::default();
            let mut eth_t = args.eth_dollar_bar;
            let mut sol_c = S::Config::default();
            let mut sol_t = args.sol_dollar_bar;

            for asset in &artifact.assets {
                match asset.symbol.as_str() {
                    "BTCUSDT" => {
                        btc_c = serde_json::from_value(asset.strategy_config.clone())
                            .context("Failed to deserialize BTC strategy config from artifact")?;
                        btc_t = asset.dollar_bar_threshold;
                    }
                    "ETHUSDT" => {
                        eth_c = serde_json::from_value(asset.strategy_config.clone())
                            .context("Failed to deserialize ETH strategy config from artifact")?;
                        eth_t = asset.dollar_bar_threshold;
                    }
                    "SOLUSDT" => {
                        sol_c = serde_json::from_value(asset.strategy_config.clone())
                            .context("Failed to deserialize SOL strategy config from artifact")?;
                        sol_t = asset.dollar_bar_threshold;
                    }
                    _ => {}
                }
            }
            (btc_c, btc_t, eth_c, eth_t, sol_c, sol_t)
        } else {
            let (btc_cfg, btc_threshold) = if let Some(path) = args.btc_config.as_ref() {
                let loaded = backtest::load_strategy_config::<S::Config>(path)?;
                (
                    loaded.strategy_config().clone(),
                    loaded.dollar_bar_threshold().unwrap_or(args.btc_dollar_bar),
                )
            } else {
                (S::Config::default(), args.btc_dollar_bar)
            };

            let (eth_cfg, eth_threshold) = if let Some(path) = args.eth_config.as_ref() {
                let loaded = backtest::load_strategy_config::<S::Config>(path)?;
                (
                    loaded.strategy_config().clone(),
                    loaded.dollar_bar_threshold().unwrap_or(args.eth_dollar_bar),
                )
            } else {
                (btc_cfg.clone(), args.eth_dollar_bar)
            };

            let (sol_cfg, sol_threshold) = if let Some(path) = args.sol_config.as_ref() {
                let loaded = backtest::load_strategy_config::<S::Config>(path)?;
                (
                    loaded.strategy_config().clone(),
                    loaded.dollar_bar_threshold().unwrap_or(args.sol_dollar_bar),
                )
            } else {
                (btc_cfg.clone(), args.sol_dollar_bar)
            };
            (
                btc_cfg,
                btc_threshold,
                eth_cfg,
                eth_threshold,
                sol_cfg,
                sol_threshold,
            )
        };

    let mut btc_strat = S::from_config(&shared, btc_cfg)
        .map_err(|e| anyhow::anyhow!("Failed to construct BTC strategy: {e}"))?;
    let mut eth_strat = S::from_config(&shared, eth_cfg)
        .map_err(|e| anyhow::anyhow!("Failed to construct ETH strategy: {e}"))?;
    let mut sol_strat = S::from_config(&shared, sol_cfg)
        .map_err(|e| anyhow::anyhow!("Failed to construct SOL strategy: {e}"))?;

    // Verify strategy supports chosen market type
    if !btc_strat.supports_market_type(market_type) {
        anyhow::bail!(
            "Strategy '{}' does not support market type {:?}. Compatible market types: {:?}",
            btc_strat.name(),
            market_type,
            btc_strat.compatible_market_types()
        );
    }

    // 2. Ingest and aggregate market data for each asset
    let btc_bars = if args.btc_data.exists() {
        load_asset_bars("BTCUSDT", &args.btc_data, btc_threshold)?
    } else {
        Vec::new()
    };

    let eth_bars = if args.eth_data.exists() {
        load_asset_bars("ETHUSDT", &args.eth_data, eth_threshold)?
    } else {
        Vec::new()
    };

    let sol_bars = if args.sol_data.exists() {
        Some(load_asset_bars("SOLUSDT", &args.sol_data, sol_threshold)?)
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

    // 4. Initialize Portfolio Risk Policy & Engine
    let (capital, risk_policy, maker_fee_pct, taker_fee_pct, slippage_pct) = if let Some(artifact) =
        portfolio_artifact.as_ref()
    {
        (
            artifact.capital,
            artifact.risk_policy.clone(),
            artifact.execution.maker_fee_pct,
            artifact.execution.taker_fee_pct,
            artifact.execution.slippage_pct,
        )
    } else {
        let (maker_fee_pct, taker_fee_pct, max_leverage) = match (market_type, args.bnb_discount) {
            (MarketType::Spot, true) => (dec!(0.00075), dec!(0.00075), dec!(1)),
            (MarketType::Spot, false) => (dec!(0.0010), dec!(0.0010), dec!(1)),
            (MarketType::UsdMPerpetual, true) => (dec!(0.00018), dec!(0.00045), dec!(20)),
            (MarketType::UsdMPerpetual, false) => (dec!(0.0002), dec!(0.0005), dec!(20)),
        };

        let policy = PortfolioRiskPolicy {
            max_open_positions: args.max_positions,
            max_directional_positions: if market_type == MarketType::Spot {
                args.max_positions
            } else {
                args.max_directional
            },
            risk_per_trade_pct: args.risk_per_trade,
            max_slot_margin_pct: args.max_slot_margin,
            max_leverage,
            min_order_quantity: dec!(0.001),
            quantity_step_size: dec!(0.001),
            min_order_notional: dec!(5),
        };
        (
            args.capital,
            policy,
            maker_fee_pct,
            taker_fee_pct,
            dec!(0.0005),
        )
    };

    let engine_config = PortfolioEngineConfig {
        initial_capital: capital,
        maker_fee_pct,
        taker_fee_pct,
        slippage_pct,
        risk_policy,
        latency_ms: 100,
        market_type,
    };

    let mut engine = PortfolioEngine::new(engine_config);

    // 5. Run Chronological Multi-Asset Simulation Loop
    println!("[*] Executing multi-asset portfolio simulation...");
    let sim_start = Instant::now();

    while let Some(tagged) = merger.pop() {
        match tagged.symbol.as_str() {
            "BTCUSDT" => {
                let _ = engine.process_event("BTCUSDT", tagged.event, &mut btc_strat);
            }
            "ETHUSDT" => {
                let _ = engine.process_event("ETHUSDT", tagged.event, &mut eth_strat);
            }
            "SOLUSDT" => {
                let _ = engine.process_event("SOLUSDT", tagged.event, &mut sol_strat);
            }
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
        Decimal::new(999, 0)
    } else {
        Decimal::ZERO
    };

    let net_pnl = engine.total_realized_pnl();
    let return_pct = if capital > Decimal::ZERO {
        (net_pnl / capital) * dec!(100)
    } else {
        Decimal::ZERO
    };

    let total_fees: Decimal = closed.iter().map(|t| t.fees_paid).sum();
    let total_slippage: Decimal = closed.iter().map(|t| t.slippage_paid).sum();
    let total_funding: Decimal = closed.iter().map(|t| t.funding_paid).sum();

    println!("============================================================");
    println!(" PORTFOLIO BACKTEST SUMMARY RESULTS");
    println!("============================================================");
    println!("Strategy:              {}", btc_strat.name());
    println!("Total Closed Trades:   {}", total_trades);
    println!(
        "Win Rate:              {:.2}% ({wins} wins, {losses} losses)",
        win_rate
    );
    println!("Net Realized PnL:      {:+8.2} USDT", net_pnl);
    println!("Return on Capital:     {:+8.2}%", return_pct);
    println!("Profit Factor:         {:.2}", profit_factor);
    println!(
        "Max Drawdown:          {:.2}%",
        engine.max_drawdown_pct() * dec!(100)
    );
    println!("Ending Cash Equity:    ${:.2} USDT", engine.cash_equity());
    println!("Total Fees Paid:       ${:.2} USDT", total_fees);
    println!("Total Slippage Paid:   ${:.2} USDT", total_slippage);
    println!("Total Funding Paid:    ${:.2} USDT", total_funding);
    println!("------------------------------------------------------------");

    if market_type == MarketType::Spot {
        println!(" SPOT DUAL BALANCES (FREE QUOTE / FREE BASE):");
        for sym in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            let (free_quote, free_base) = engine.spot_balances(sym);
            println!(
                "   {sym:<8}: Free Quote: ${:.2} USDT | Free Base: {:.6}",
                free_quote, free_base
            );
        }
        println!("------------------------------------------------------------");
    }

    // Monthly returns breakdown
    let mut monthly_pnl: std::collections::BTreeMap<String, (Decimal, usize)> =
        std::collections::BTreeMap::new();
    for t in closed {
        let dt = chrono::DateTime::from_timestamp_millis(t.exit_time).unwrap_or_default();
        let key = dt.format("%Y-%m").to_string();
        let entry = monthly_pnl.entry(key).or_insert((Decimal::ZERO, 0));
        entry.0 += t.pnl_net;
        entry.1 += 1;
    }

    println!(" PER-SYMBOL BREAKDOWN:");
    let by_symbol = engine.closed_trades_by_symbol();
    for (sym, trades) in &by_symbol {
        let cnt = trades.len();
        let sym_wins = trades.iter().filter(|t| t.pnl_net > Decimal::ZERO).count();
        let sym_wr = if cnt > 0 {
            (sym_wins as f64 / cnt as f64) * 100.0
        } else {
            0.0
        };
        let sym_pnl: Decimal = trades.iter().map(|t| t.pnl_net).sum();
        let sym_fees: Decimal = trades.iter().map(|t| t.fees_paid).sum();
        println!(
            "   {sym:<8}: {cnt:>2} trades | WinRate: {sym_wr:>5.1}% | PnL: {sym_pnl:+8.2} USDT | Fees: ${sym_fees:.2}"
        );

        let refs: Vec<&backtest::ClosedTrade> = trades.iter().collect();
        let sl_trades: Vec<_> = refs
            .iter()
            .filter(|t| t.exit_reason == "StopLoss" || t.exit_reason == "BreakevenStop")
            .collect();
        if !sl_trades.is_empty() {
            let be_sls = sl_trades
                .iter()
                .filter(|t| t.pnl_net >= Decimal::ZERO)
                .count();
            let true_sls = sl_trades.len() - be_sls;
            let avg_sl_pct: Decimal = sl_trades.iter().map(|t| t.return_pct).sum::<Decimal>()
                / Decimal::from(sl_trades.len());
            let avg_sl_sec: i64 = sl_trades
                .iter()
                .map(|t| t.holding_duration_seconds)
                .sum::<i64>()
                / sl_trades.len() as i64;
            let died_under_5m = sl_trades
                .iter()
                .filter(|t| t.holding_duration_seconds < 300)
                .count();

            let had_positive_mfe = sl_trades
                .iter()
                .filter(|t| t.excursion.peak_equity > t.excursion.entry_equity)
                .count();
            let avg_peak_gain_pct: Decimal = if had_positive_mfe > 0 {
                sl_trades
                    .iter()
                    .filter(|t| t.excursion.peak_equity > t.excursion.entry_equity)
                    .map(|t| {
                        (t.excursion.peak_equity - t.excursion.entry_equity)
                            / t.excursion.entry_equity
                    })
                    .sum::<Decimal>()
                    / Decimal::from(had_positive_mfe)
            } else {
                Decimal::ZERO
            };

            println!(
                "     SL Profile: Total: {} (True Losses: {} | Breakeven/Win SLs: {})",
                sl_trades.len(),
                true_sls,
                be_sls
            );
            println!(
                "                 Avg Loss: {:.2}% | Avg Hold: {}s ({:.1}m) | <5m Kills: {}/{} ({:.0}%)",
                avg_sl_pct * dec!(100),
                avg_sl_sec,
                avg_sl_sec as f64 / 60.0,
                died_under_5m,
                sl_trades.len(),
                (died_under_5m as f64 / sl_trades.len() as f64) * 100.0
            );
            println!(
                "                 Had Positive Excursion (MFE): {}/{} ({:.0}%) | Avg Peak Gain: +{:.2}%",
                had_positive_mfe,
                sl_trades.len(),
                (had_positive_mfe as f64 / sl_trades.len() as f64) * 100.0,
                avg_peak_gain_pct * dec!(100)
            );
        }
        let tp_trades: Vec<_> = refs
            .iter()
            .filter(|t| t.exit_reason == "TakeProfit")
            .collect();
        if !tp_trades.is_empty() {
            let avg_tp_pct: Decimal = tp_trades.iter().map(|t| t.return_pct).sum::<Decimal>()
                / Decimal::from(tp_trades.len());
            let avg_tp_sec: i64 = tp_trades
                .iter()
                .map(|t| t.holding_duration_seconds)
                .sum::<i64>()
                / tp_trades.len() as i64;
            println!(
                "     TP Profile: Avg Gain: {:.2}% | Avg Hold: {}s ({:.1}m)",
                avg_tp_pct * dec!(100),
                avg_tp_sec,
                avg_tp_sec as f64 / 60.0
            );
        }
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

fn main() -> Result<()> {
    let args = Args::parse();

    let (strategy_id, capital) = if let Some(path) = args.portfolio_config.as_ref() {
        if let Ok(art) = load_portfolio_artifact(path) {
            (
                art.assets
                    .first()
                    .map(|a| a.strategy_id.clone())
                    .unwrap_or_else(|| args.strategy.clone()),
                art.capital,
            )
        } else {
            (args.strategy.clone(), args.capital)
        }
    } else {
        (args.strategy.clone(), args.capital)
    };

    let market_type = if args.spot {
        MarketType::Spot
    } else {
        MarketType::UsdMPerpetual
    };

    println!("============================================================");
    println!(" ATSNT - Multi-Asset Algorithmic Portfolio Engine");
    println!("============================================================");
    println!("Market Type:           {:?}", market_type);
    println!("Strategy:              {}", strategy_id);
    println!("Initial Capital:       ${:.2} USDT", capital);
    println!("BNB Fee Discount:      {}", args.bnb_discount);
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

    let run = PortfolioRun {
        args: &args,
        market_type,
    };
    match strategies::dispatch(&strategy_id, run) {
        Some(res) => res?,
        None => anyhow::bail!(strategies::unknown_strategy_message(&strategy_id)),
    }

    Ok(())
}
