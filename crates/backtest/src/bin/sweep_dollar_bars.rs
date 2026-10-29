use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use adapters::{BinanceParquetReader, MarketDataStream};
use anyhow::Result;
use backtest::{
    CrossSectionalUniverseSelector, EventMerger, PortfolioEngine, PortfolioEngineConfig,
};
use clap::Parser;
use domain::{DollarBar, DollarBarAggregator, PortfolioRiskPolicy, UniverseSelectionPolicy};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use strategies::quantum_well_swing::{QuantumWellSwingConfig, QuantumWellSwingStrategy};
use strategies::{MarketType, Strategy};

#[derive(Parser, Debug)]
#[command(
    name = "sweep_dollar_bars",
    about = "Exhaustive Dollar Bar Resolution Sweep for Kraken EUR Universe"
)]
struct Args {
    #[arg(
        long,
        default_value = "configs/kraken_universe_deployment_artifact.json"
    )]
    artifact: PathBuf,

    #[arg(long, default_value = "200")]
    capital: Decimal,

    #[arg(long, default_value = "data/historical")]
    data_dir: PathBuf,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct UniverseArtifactFile {
    capital: Decimal,
    risk_policy: PortfolioRiskPolicy,
    execution: ExecutionSettingsFile,
    universe_policy: Option<UniverseSelectionPolicy>,
    assets: Vec<AssetSpecFile>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ExecutionSettingsFile {
    maker_fee_pct: Decimal,
    taker_fee_pct: Decimal,
    slippage_pct: Decimal,
    btc_breadth_filter: bool,
    btc_correlation_threshold: Decimal,
    btc_correlation_window: usize,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AssetSpecFile {
    symbol: String,
    dollar_bar_threshold: Decimal,
    strategy_config: QuantumWellSwingConfig,
}

fn load_or_aggregate_bars(
    symbol: &str,
    data_dir: &Path,
    threshold: Decimal,
) -> Result<Vec<DollarBar>> {
    let cache_dir = Path::new("target/dollar_bars_cache");
    let cache_file = cache_dir.join(format!("{symbol}_{threshold}.json"));

    if cache_file.exists() {
        let file = std::fs::File::open(&cache_file)?;
        let reader = std::io::BufReader::new(file);
        let bars: Vec<DollarBar> = serde_json::from_reader(reader)?;
        return Ok(bars);
    }

    let asset_path = data_dir.join(symbol);
    if !asset_path.exists() {
        anyhow::bail!(
            "Historical dataset for {symbol} not found at {}",
            asset_path.display()
        );
    }

    println!("[*] Aggregating {symbol} (Threshold: €{threshold})...");
    let mut reader = BinanceParquetReader::open(&asset_path)?;
    let mut aggregator = DollarBarAggregator::new(threshold)
        .map_err(|e| anyhow::anyhow!("Invalid threshold {threshold}: {e}"))?;
    let mut bars = Vec::new();
    while let Some(trade) = reader.next_trade()? {
        if let Some(bar) = aggregator.process_trade(&trade) {
            bars.push(bar);
        }
    }

    let _ = std::fs::create_dir_all(cache_dir);
    if let Ok(file) = std::fs::File::create(&cache_file) {
        let writer = std::io::BufWriter::new(file);
        let _ = serde_json::to_writer(writer, &bars);
    }

    Ok(bars)
}

#[derive(Debug, Clone)]
struct ResolutionProfile {
    name: String,
    btc_thresh: Decimal,
    eth_thresh: Decimal,
    sol_thresh: Decimal,
    bnb_thresh: Decimal,
    avax_thresh: Decimal,
    near_thresh: Decimal,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct SweepResult {
    profile_name: String,
    btc_bars: usize,
    eth_bars: usize,
    sol_bars: usize,
    near_bars: usize,
    total_trades: usize,
    wins: usize,
    win_rate: f64,
    net_pnl: Decimal,
    roi_pct: f64,
    profit_factor: f64,
    max_dd_pct: f64,
    fees_paid: Decimal,
    pnl_btc: Decimal,
    pnl_eth: Decimal,
    pnl_sol: Decimal,
    pnl_bnb: Decimal,
    pnl_avax: Decimal,
    pnl_near: Decimal,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("================================================================================");
    println!("  ATSNT - EXHAUSTIVE DOLLAR BAR SWEEP FOR KRAKEN EUR UNIVERSE");
    println!("  Systematic Multi-Frequency Cross-Sectional Grid Optimization");
    println!("================================================================================\n");

    let artifact_content = std::fs::read_to_string(&args.artifact)?;
    let artifact: UniverseArtifactFile = serde_json::from_str(&artifact_content)?;

    // Define candidate resolution profiles spanning from Ultra-Fast to Deep-Swing
    let mut profiles: Vec<ResolutionProfile> = Vec::new();

    // 1. Uniform Frequency Sweeps across 300 days (Daily Volumes: BTC 18.3M, ETH 11.1M, SOL 3.19M, BNB 1.35M, AVAX 90k, NEAR 106k)
    let freq_targets = [
        (
            "40 bars/day (~36m)",
            dec!(450_000),
            dec!(280_000),
            dec!(80_000),
            dec!(35_000),
            dec!(2_500),
            dec!(2_600),
        ),
        (
            "35 bars/day (~41m)",
            dec!(520_000),
            dec!(320_000),
            dec!(90_000),
            dec!(40_000),
            dec!(2_800),
            dec!(3_000),
        ),
        (
            "30 bars/day (~48m)",
            dec!(600_000),
            dec!(370_000),
            dec!(105_000),
            dec!(45_000),
            dec!(3_200),
            dec!(3_500),
        ),
        (
            "25 bars/day (~58m)",
            dec!(730_000),
            dec!(440_000),
            dec!(130_000),
            dec!(55_000),
            dec!(3_800),
            dec!(4_200),
        ),
        (
            "20 bars/day (~1.2h)",
            dec!(915_000),
            dec!(555_000),
            dec!(160_000),
            dec!(70_000),
            dec!(4_500),
            dec!(5_300),
        ),
        (
            "18 bars/day (~1.3h)",
            dec!(1_000_000),
            dec!(600_000),
            dec!(180_000),
            dec!(75_000),
            dec!(5_000),
            dec!(6_000),
        ),
        (
            "15 bars/day (~1.6h)",
            dec!(1_200_000),
            dec!(740_000),
            dec!(210_000),
            dec!(90_000),
            dec!(6_000),
            dec!(7_000),
        ),
        (
            "12 bars/day (~2.0h)",
            dec!(1_500_000),
            dec!(925_000),
            dec!(265_000),
            dec!(110_000),
            dec!(7_500),
            dec!(9_000),
        ),
        (
            "10 bars/day (~2.4h)",
            dec!(1_800_000),
            dec!(1_110_000),
            dec!(320_000),
            dec!(135_000),
            dec!(9_000),
            dec!(10_500),
        ),
        (
            "8 bars/day  (~3.0h)",
            dec!(2_300_000),
            dec!(1_380_000),
            dec!(400_000),
            dec!(170_000),
            dec!(11_000),
            dec!(13_000),
        ),
        (
            "6 bars/day  (~4.0h)",
            dec!(3_000_000),
            dec!(1_850_000),
            dec!(530_000),
            dec!(225_000),
            dec!(15_000),
            dec!(18_000),
        ),
        (
            "4 bars/day  (~6.0h)",
            dec!(4_500_000),
            dec!(2_750_000),
            dec!(800_000),
            dec!(340_000),
            dec!(22_000),
            dec!(26_000),
        ),
    ];

    for (name, btc, eth, sol, bnb, avax, near) in freq_targets {
        profiles.push(ResolutionProfile {
            name: format!("Uniform: {name}"),
            btc_thresh: btc,
            eth_thresh: eth,
            sol_thresh: sol,
            bnb_thresh: bnb,
            avax_thresh: avax,
            near_thresh: near,
        });
    }

    // 2. Asymmetric Cross-Frequency Sweeps (BTC/ETH at swing frequency, Altcoins at slower/faster frequency)
    let btc_eth_combos = [
        ("BTC 18b / ETH 18b", dec!(1_000_000), dec!(600_000)),
        ("BTC 25b / ETH 25b", dec!(730_000), dec!(440_000)),
        ("BTC 15b / ETH 15b", dec!(1_200_000), dec!(740_000)),
        ("BTC 10b / ETH 10b", dec!(1_800_000), dec!(1_110_000)),
        ("BTC 30b / ETH 30b", dec!(600_000), dec!(370_000)),
    ];

    let alt_combos = [
        (
            "Alts 10b/day",
            dec!(320_000),
            dec!(135_000),
            dec!(9_000),
            dec!(10_500),
        ),
        (
            "Alts 15b/day",
            dec!(210_000),
            dec!(90_000),
            dec!(6_000),
            dec!(7_000),
        ),
        (
            "Alts 20b/day",
            dec!(160_000),
            dec!(70_000),
            dec!(4_500),
            dec!(5_300),
        ),
        (
            "Alts 8b/day",
            dec!(400_000),
            dec!(170_000),
            dec!(11_000),
            dec!(13_000),
        ),
        (
            "Alts 5b/day (Slow)",
            dec!(600_000),
            dec!(200_000),
            dec!(20_000),
            dec!(20_000),
        ),
    ];

    for (be_name, btc, eth) in btc_eth_combos {
        for (alt_name, sol, bnb, avax, near) in alt_combos {
            profiles.push(ResolutionProfile {
                name: format!("{be_name} + {alt_name}"),
                btc_thresh: btc,
                eth_thresh: eth,
                sol_thresh: sol,
                bnb_thresh: bnb,
                avax_thresh: avax,
                near_thresh: near,
            });
        }
    }

    println!(
        "Total Resolution Profiles to Evaluate: {}\n",
        profiles.len()
    );

    let mut results: Vec<SweepResult> = Vec::new();
    let total_start = Instant::now();

    for (idx, p) in profiles.iter().enumerate() {
        print!(
            "[{:02}/{:02}] Evaluating Profile '{}'...",
            idx + 1,
            profiles.len(),
            p.name
        );

        // Pre-load / aggregate bars for each symbol
        let btc_bars = load_or_aggregate_bars("BTCEUR", &args.data_dir, p.btc_thresh)?;
        let eth_bars = load_or_aggregate_bars("ETHEUR", &args.data_dir, p.eth_thresh)?;
        let sol_bars = load_or_aggregate_bars("SOLEUR", &args.data_dir, p.sol_thresh)?;
        let bnb_bars = load_or_aggregate_bars("BNBEUR", &args.data_dir, p.bnb_thresh)?;
        let avax_bars = load_or_aggregate_bars("AVAXEUR", &args.data_dir, p.avax_thresh)?;
        let near_bars = load_or_aggregate_bars("NEAREUR", &args.data_dir, p.near_thresh)?;

        let n_btc = btc_bars.len();
        let n_eth = eth_bars.len();
        let n_sol = sol_bars.len();
        let n_near = near_bars.len();

        let mut merger = EventMerger::new();
        for b in &btc_bars {
            merger.push("BTCEUR", b);
        }
        for b in &eth_bars {
            merger.push("ETHEUR", b);
        }
        for b in &sol_bars {
            merger.push("SOLEUR", b);
        }
        for b in &bnb_bars {
            merger.push("BNBEUR", b);
        }
        for b in &avax_bars {
            merger.push("AVAXEUR", b);
        }
        for b in &near_bars {
            merger.push("NEAREUR", b);
        }

        let mut strategies: BTreeMap<String, Box<dyn Strategy>> = BTreeMap::new();
        for spec in &artifact.assets {
            let strat = QuantumWellSwingStrategy::new(spec.strategy_config.clone())?;
            strategies.insert(spec.symbol.clone(), Box::new(strat));
        }

        let engine_config = PortfolioEngineConfig {
            initial_capital: args.capital,
            maker_fee_pct: artifact.execution.maker_fee_pct,
            taker_fee_pct: artifact.execution.taker_fee_pct,
            slippage_pct: artifact.execution.slippage_pct,
            risk_policy: artifact.risk_policy.clone(),
            latency_ms: 100,
            market_type: MarketType::Spot,
            btc_breadth_filter: artifact.execution.btc_breadth_filter,
            btc_correlation_threshold: artifact.execution.btc_correlation_threshold,
            btc_correlation_window: artifact.execution.btc_correlation_window,
            universe_policy: artifact.universe_policy.clone(),
            benchmark_symbol: "BTCEUR".to_string(),
        };

        let mut engine = PortfolioEngine::new(engine_config);
        if let Some(ref policy) = artifact.universe_policy {
            let mut selector = CrossSectionalUniverseSelector::new(policy.clone(), "BTCEUR");
            for sym in ["BTCEUR", "ETHEUR", "SOLEUR", "BNBEUR", "AVAXEUR", "NEAREUR"] {
                selector.register_asset(sym);
            }
            engine = engine.with_universe_selector(selector);
        }

        while let Some(tagged) = merger.pop() {
            if let Some(strat) = strategies.get_mut(&tagged.symbol) {
                let _ = engine.process_event(&tagged.symbol, tagged.event, strat.as_mut());
            }
        }

        let closed = engine.closed_trades();
        let total_trades = closed.len();
        let wins = closed.iter().filter(|t| t.pnl_net > Decimal::ZERO).count();
        let win_rate = if total_trades > 0 {
            (wins as f64 / total_trades as f64) * 100.0
        } else {
            0.0
        };
        let net_pnl = engine.total_realized_pnl();
        let roi_pct = (net_pnl / args.capital * dec!(100))
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0);
        let max_dd_pct = (engine.max_drawdown_pct() * dec!(100))
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0);

        let gross_win: Decimal = closed
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
            (gross_win / gross_loss)
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0)
        } else if gross_win > Decimal::ZERO {
            99.9
        } else {
            0.0
        };

        let fees_paid: Decimal = closed.iter().map(|t| t.fees_paid).sum();

        let by_sym = engine.closed_trades_by_symbol();
        let pnl_btc = by_sym
            .get("BTCEUR")
            .map(|v| v.iter().map(|t| t.pnl_net).sum())
            .unwrap_or(Decimal::ZERO);
        let pnl_eth = by_sym
            .get("ETHEUR")
            .map(|v| v.iter().map(|t| t.pnl_net).sum())
            .unwrap_or(Decimal::ZERO);
        let pnl_sol = by_sym
            .get("SOLEUR")
            .map(|v| v.iter().map(|t| t.pnl_net).sum())
            .unwrap_or(Decimal::ZERO);
        let pnl_bnb = by_sym
            .get("BNBEUR")
            .map(|v| v.iter().map(|t| t.pnl_net).sum())
            .unwrap_or(Decimal::ZERO);
        let pnl_avax = by_sym
            .get("AVAXEUR")
            .map(|v| v.iter().map(|t| t.pnl_net).sum())
            .unwrap_or(Decimal::ZERO);
        let pnl_near = by_sym
            .get("NEAREUR")
            .map(|v| v.iter().map(|t| t.pnl_net).sum())
            .unwrap_or(Decimal::ZERO);

        let res = SweepResult {
            profile_name: p.name.clone(),
            btc_bars: n_btc,
            eth_bars: n_eth,
            sol_bars: n_sol,
            near_bars: n_near,
            total_trades,
            wins,
            win_rate,
            net_pnl,
            roi_pct,
            profit_factor,
            max_dd_pct,
            fees_paid,
            pnl_btc,
            pnl_eth,
            pnl_sol,
            pnl_bnb,
            pnl_avax,
            pnl_near,
        };

        println!(
            " -> Trds: {:2} | WR: {:4.1}% | PnL: {:+6.2}€ ({:+5.1}%) | PF: {:4.2} | MaxDD: {:4.1}%",
            res.total_trades,
            res.win_rate,
            res.net_pnl,
            res.roi_pct,
            res.profit_factor,
            res.max_dd_pct
        );

        results.push(res);
    }

    let elapsed_sec = total_start.elapsed().as_secs_f64();
    println!(
        "\nSweep completed in {:.2}s across {} configurations.\n",
        elapsed_sec,
        results.len()
    );

    // Sort by Net PnL descending
    results.sort_by_key(|a| std::cmp::Reverse(a.net_pnl));

    println!("==================================================================================================================================================");
    println!("                                          TOP 15 DOLLAR BAR RESOLUTION PROFILES (BY NET PNL)                                                     ");
    println!("==================================================================================================================================================");
    println!(
        "{:<4} | {:<32} | {:>5} | {:>6} | {:>9} | {:>8} | {:>6} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7}",
        "Rank", "Resolution Profile", "Trds", "WinRate", "Net PnL", "ROI %", "PF", "MaxDD%", "BTC PnL", "ETH PnL", "SOL PnL", "NEAR PnL"
    );
    println!("--------------------------------------------------------------------------------------------------------------------------------------------------");

    for (i, r) in results.iter().take(15).enumerate() {
        println!(
            "#{:<3} | {:<32} | {:>5} | {:>5.1}% | {:>8.2}€ | {:>+7.2}% | {:>6.2} | {:>6.2}% | {:>7.2} | {:>7.2} | {:>7.2} | {:>7.2}",
            i + 1,
            r.profile_name,
            r.total_trades,
            r.win_rate,
            r.net_pnl,
            r.roi_pct,
            r.profit_factor,
            r.max_dd_pct,
            r.pnl_btc,
            r.pnl_eth,
            r.pnl_sol,
            r.pnl_near
        );
    }
    println!("==================================================================================================================================================");

    Ok(())
}
