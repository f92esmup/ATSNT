//! Multi-Asset Dynamic Top-K Universe Selection Portfolio Backtest Runner.
//!
//! Evaluates a dynamically ranked candidate universe (BTC, ETH, SOL, BNB, AVAX, NEAR)
//! on Binance Spot. Every 24 hours (00:00 UTC), assets are ranked cross-sectionally
//! by relative momentum, quantum potential well compression, and volume anomalies.
//! Only Top-K assets (K=3) are authorized for new entries, with a hysteresis tolerance
//! of 2 (retaining open positions up to rank 5).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use adapters::{BinanceParquetReader, MarketDataStream};
use anyhow::{Context, Result};
use backtest::universe_selector::CrossSectionalUniverseSelector;
use backtest::{EventMerger, PortfolioEngine, PortfolioEngineConfig};
use clap::Parser;
use domain::{DollarBar, DollarBarAggregator, PortfolioRiskPolicy, UniverseSelectionPolicy};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use strategies::quantum_well_swing::{QuantumWellSwingConfig, QuantumWellSwingStrategy};
use strategies::{MarketType, Strategy};

#[derive(Parser, Debug)]
#[command(
    name = "run_universe_portfolio",
    about = "ATSNT - Dynamic Top-K Universe Selection Engine Backtest"
)]
struct Args {
    /// Path to universe deployment artifact JSON.
    #[arg(long, default_value = "configs/universe_deployment_artifact.json")]
    artifact: PathBuf,

    /// Initial portfolio capital in USDT.
    #[arg(long, default_value = "10000")]
    capital: Decimal,

    /// Number of top-ranked assets authorized to receive new capital allocations.
    #[arg(long, default_value_t = 3)]
    top_k: usize,

    /// Hysteresis rank tolerance buffer (holds positions up to top_k + tolerance).
    #[arg(long, default_value_t = 2)]
    hysteresis: usize,

    /// Candidate asset symbols comma-separated.
    #[arg(
        long,
        default_value = "BTCUSDT,ETHUSDT,SOLUSDT,BNBUSDT,AVAXUSDT,NEARUSDT"
    )]
    candidates: String,

    /// Historical data root directory.
    #[arg(long, default_value = "data/historical")]
    data_dir: PathBuf,

    /// Whether to disable universe dynamic ranking (benchmark static portfolio).
    #[arg(long, default_value_t = false)]
    disable_universe_filter: bool,
}

#[derive(Debug, Deserialize)]
struct UniverseArtifactFile {
    capital: Decimal,
    risk_policy: PortfolioRiskPolicy,
    execution: ExecutionSettingsFile,
    universe_policy: Option<UniverseSelectionPolicy>,
    assets: Vec<AssetSpecFile>,
}

#[derive(Debug, Deserialize)]
struct ExecutionSettingsFile {
    maker_fee_pct: Decimal,
    taker_fee_pct: Decimal,
    slippage_pct: Decimal,
    btc_breadth_filter: bool,
    btc_correlation_threshold: Decimal,
    btc_correlation_window: usize,
}

#[derive(Debug, Deserialize)]
struct AssetSpecFile {
    symbol: String,
    dollar_bar_threshold: Decimal,
    strategy_config: QuantumWellSwingConfig,
}

/// Loads and aggregates trades into DollarBars with local disk caching.
fn load_or_aggregate_bars(
    symbol: &str,
    data_dir: &Path,
    threshold: Decimal,
) -> Result<Vec<DollarBar>> {
    let cache_dir = Path::new("target/dollar_bars_cache");
    let cache_file = cache_dir.join(format!("{symbol}_{threshold}.json"));

    if cache_file.exists() {
        println!(
            "[*] Loading cached {symbol} bars from {}",
            cache_file.display()
        );
        let file = std::fs::File::open(&cache_file)?;
        let reader = std::io::BufReader::new(file);
        let bars: Vec<DollarBar> = serde_json::from_reader(reader)?;
        println!(
            "    [OK] {symbol}: Loaded {} cached dollar bars",
            bars.len()
        );
        return Ok(bars);
    }

    let asset_path = data_dir.join(symbol);
    if !asset_path.exists() {
        anyhow::bail!(
            "Historical dataset for symbol {symbol} not found at {}",
            asset_path.display()
        );
    }

    println!(
        "[*] Aggregating {symbol} from {} (Threshold: ${})",
        asset_path.display(),
        threshold
    );
    let mut reader = BinanceParquetReader::open(&asset_path).with_context(|| {
        format!(
            "Failed to open Parquet dataset for {symbol} at {}",
            asset_path.display()
        )
    })?;

    let mut aggregator = DollarBarAggregator::new(threshold)
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
        "    [OK] {symbol}: Processed {trade_count} trades into {} dollar bars in {:.2}s ({:.1}M trades/s)",
        bars.len(),
        elapsed,
        (trade_count as f64 / elapsed.max(0.001)) / 1_000_000.0
    );

    let _ = std::fs::create_dir_all(cache_dir);
    if let Ok(file) = std::fs::File::create(&cache_file) {
        let writer = std::io::BufWriter::new(file);
        let _ = serde_json::to_writer(writer, &bars);
    }

    Ok(bars)
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("================================================================================");
    println!("  ATSNT - DYNAMIC TOP-K UNIVERSE SELECTION PORTFOLIO ENGINE");
    println!("  Zero-Toy Quantitative Simulation on Binance Spot (100% Rust / Fixed Decimal)");
    println!("================================================================================\n");

    let artifact_content = std::fs::read_to_string(&args.artifact).with_context(|| {
        format!(
            "Failed to read universe artifact at {}",
            args.artifact.display()
        )
    })?;
    let artifact: UniverseArtifactFile = serde_json::from_str(&artifact_content)?;

    let candidate_list: Vec<&str> = args
        .candidates
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let capital = if args.capital != dec!(10000) {
        args.capital
    } else {
        artifact.capital
    };

    println!("[1/4] Configuring Candidate Assets & Execution Matrix:");
    println!("  • Initial Capital:        ${capital}");
    println!("  • Universe Candidate Pool: {:?}", candidate_list);
    println!("  • Top-K Entry Authorization: Top-{}", args.top_k);
    println!(
        "  • Hysteresis Retention:   Top-{} (K={}+tol={})",
        args.top_k + args.hysteresis,
        args.top_k,
        args.hysteresis
    );
    println!(
        "  • Fees & Slippage:         Maker {:.4}% / Taker {:.4}% / Slippage {:.4}%",
        artifact.execution.maker_fee_pct * dec!(100),
        artifact.execution.taker_fee_pct * dec!(100),
        artifact.execution.slippage_pct * dec!(100)
    );
    println!(
        "  • BTC Breadth Lead Filter: Active (Threshold: {}, Window: {} bars)",
        artifact.execution.btc_correlation_threshold, artifact.execution.btc_correlation_window
    );
    println!(
        "  • Dynamic Ranking Enabled: {}",
        !args.disable_universe_filter
    );
    println!();

    // 2. Load Market Data & Initialize Strategies
    println!("[2/4] Ingesting Market Data & Constructing Strategy Instances...");
    let mut strategies: BTreeMap<String, Box<dyn Strategy>> = BTreeMap::new();
    let mut merger = EventMerger::new();
    let mut total_bars_loaded = 0;

    for symbol in &candidate_list {
        let spec = artifact
            .assets
            .iter()
            .find(|a| a.symbol == *symbol)
            .ok_or_else(|| {
                anyhow::anyhow!("Asset spec not found in artifact for symbol {symbol}")
            })?;

        let bars = load_or_aggregate_bars(symbol, &args.data_dir, spec.dollar_bar_threshold)?;
        total_bars_loaded += bars.len();

        for bar in &bars {
            merger.push(*symbol, bar);
        }

        let strat = QuantumWellSwingStrategy::new(spec.strategy_config.clone())
            .map_err(|e| anyhow::anyhow!("Failed to construct strategy for {symbol}: {e}"))?;
        strategies.insert(symbol.to_string(), Box::new(strat));
    }

    println!(
        "\n    [OK] Chronological Event Stream Merged: {} total bars across {} assets\n",
        total_bars_loaded,
        candidate_list.len()
    );

    // 3. Configure Universe Selection Policy & Portfolio Engine
    println!("[3/4] Initializing Portfolio Engine with Cross-Sectional Ranking...");
    let universe_policy = if args.disable_universe_filter {
        None
    } else {
        let mut policy = artifact.universe_policy.unwrap_or_default();
        policy.top_k = args.top_k;
        policy.hysteresis_tolerance = args.hysteresis;
        Some(policy)
    };

    let benchmark = candidate_list
        .iter()
        .find(|&&s| s.starts_with("BTC"))
        .copied()
        .unwrap_or("BTCUSDT");

    let engine_config = PortfolioEngineConfig {
        initial_capital: capital,
        maker_fee_pct: artifact.execution.maker_fee_pct,
        taker_fee_pct: artifact.execution.taker_fee_pct,
        slippage_pct: artifact.execution.slippage_pct,
        risk_policy: artifact.risk_policy.clone(),
        latency_ms: 100,
        market_type: MarketType::Spot,
        btc_breadth_filter: artifact.execution.btc_breadth_filter,
        btc_correlation_threshold: artifact.execution.btc_correlation_threshold,
        btc_correlation_window: artifact.execution.btc_correlation_window,
        universe_policy: universe_policy.clone(),
        benchmark_symbol: benchmark.to_string(),
    };

    let mut engine = PortfolioEngine::new(engine_config);

    // Explicitly register all candidate assets into the selector
    if let Some(policy) = universe_policy {
        let mut selector = CrossSectionalUniverseSelector::new(policy, benchmark);
        for sym in &candidate_list {
            selector.register_asset(*sym);
        }
        engine = engine.with_universe_selector(selector);
    }

    // 4. Chronological Multi-Asset Simulation
    println!("[4/4] Executing Simulation Loop...");
    let sim_start = Instant::now();

    while let Some(tagged) = merger.pop() {
        if let Some(strat) = strategies.get_mut(&tagged.symbol) {
            let _ = engine.process_event(&tagged.symbol, tagged.event, strat.as_mut());
        }
    }

    let sim_elapsed = sim_start.elapsed().as_secs_f64();
    println!("    [OK] Simulation completed in {:.2}s\n", sim_elapsed);

    // 5. Generate Comprehensive Scorecard
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

    let ending_equity = engine.total_marked_equity();
    let max_drawdown_pct = engine.max_drawdown_pct() * dec!(100);

    println!("================================================================================");
    println!("                      PORTFOLIO PERFORMANCE SCORECARD                           ");
    println!("================================================================================");
    println!("  Initial Capital:       ${:>10.2}", capital);
    println!("  Ending Marked Equity:  ${:>10.2}", ending_equity);
    println!(
        "  Net Realized PnL:      ${:>10.2} ({:>+.2}%)",
        net_pnl, return_pct
    );
    println!("  Max Portfolio Drawdown: {:>10.2}%", max_drawdown_pct);
    println!("  Profit Factor:          {:>10.2}", profit_factor);
    println!(
        "  Trades:                 {:>10} (Wins: {}, Losses: {})",
        total_trades, wins, losses
    );
    println!("  Win Rate:               {:>10.2}%", win_rate);
    println!("--------------------------------------------------------------------------------");
    println!("  PER-ASSET EXECUTION & REALIZED PNL BREAKDOWN:");
    let by_symbol = engine.closed_trades_by_symbol();
    for sym in &candidate_list {
        let sym_trades = by_symbol.get(*sym).map(|v| v.as_slice()).unwrap_or(&[]);
        let sym_wins = sym_trades
            .iter()
            .filter(|t| t.pnl_net > Decimal::ZERO)
            .count();
        let sym_pnl: Decimal = sym_trades.iter().map(|t| t.pnl_net).sum();
        let sym_fees: Decimal = sym_trades.iter().map(|t| t.fees_paid).sum();
        println!(
            "   • {:<8} | Trades: {:>3} (Wins: {:>2}) | Net PnL: ${:>8.2} | Fees: ${:>6.2}",
            sym,
            sym_trades.len(),
            sym_wins,
            sym_pnl,
            sym_fees
        );
    }
    println!("--------------------------------------------------------------------------------");
    println!("  PRE-TRADE ORDER REJECTIONS & RISK POLICIES:");
    for (reason, count) in engine.rejected_orders() {
        println!("   • {:<28}: {:>4} orders", reason, count);
    }

    if let Some(selector) = engine.universe_selector() {
        println!(
            "--------------------------------------------------------------------------------"
        );
        println!("  FINAL UNIVERSE RANKING SNAPSHOT (End of Simulation):");
        for score in selector.current_ranking() {
            let auth = if selector.is_symbol_authorized_for_entry(&score.symbol) {
                "[AUTHORIZED TOP-3]"
            } else if selector.is_symbol_eligible_for_retention(&score.symbol) {
                "[RETAINED TOP-5]"
            } else {
                "[EXCLUDED]"
            };
            println!(
                "   Rank #{}: {:<8} | Composite: {:>+.3} | MomZ: {:>+.2} | QntZ: {:>+.2} | VolZ: {:>+.2} | {}",
                score.rank,
                score.symbol,
                score.composite_score,
                score.momentum_z,
                score.quantum_z,
                score.volume_z,
                auth
            );
        }
    }
    println!("================================================================================\n");

    Ok(())
}
