// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

//! Backtests one strategy on several instruments with one account: the bar route of
//! `PortfolioEngine`. A per-instrument strategy (the swarm) runs one instance per instrument; a
//! cross-sectional one runs once over all of them. The account's portfolio limits decide how many
//! positions may be open at once.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use adapters::{AdapterError, BinanceCsvReader, TapeReader};
use anyhow::{bail, Context, Result};
use backtest::{
    check_calibration, now_unix_ms, record_backtest_with_audit, results_by_instrument,
    BacktestConfig, DataWindow, MergedTrades, PortfolioEngine, PortfolioStrategies, RunContext,
    RunSpec, RunType, SamplingConfig, TickSimulationConfig,
};
use clap::Parser;
use domain::futures::{FundingFeed, FundingSchedule};
use domain::InstrumentId;
use ports::MarketDataStream;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{MarketType, Strategy};

/// How simulated orders are filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum FillModelArg {
    /// Bar-level fills through `PortfolioEngine`.
    Bar,
    /// Tick-level fills: one live session per instrument on one simulated venue account.
    Tick,
}

#[derive(Parser, Debug)]
#[command(
    name = "portfolio",
    about = "ATSNT - one strategy on several instruments with one account"
)]
struct Args {
    /// One instrument's trades as SYMBOL=PATH (a .parquet file, a directory of them, or a .csv);
    /// repeat it for every instrument. A bare PATH takes its symbol from the file name.
    #[arg(short, long, required = true)]
    data: Vec<String>,

    /// Strategy to run, by id or alias; inferred from --config when it is an artifact
    #[arg(long)]
    strategy: Option<String>,

    /// Optional strategy configuration or deployment artifact
    #[arg(short, long, visible_alias = "strategy-config")]
    config: Option<PathBuf>,

    /// Optional shared (structural) configuration of the strategy, e.g. the swarm's calibrated
    /// feeds
    #[arg(long)]
    shared_config: Option<PathBuf>,

    /// Initial cash capital of the account (default: 10,000, or the artifact's)
    #[arg(long)]
    capital: Option<Decimal>,

    /// Target Spot instead of USD-M Perpetual Futures
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Most positions open at once across instruments [default: 1]
    #[arg(long)]
    max_open_positions: Option<usize>,

    /// Most positions open at once on one side
    #[arg(long)]
    max_same_direction: Option<usize>,

    /// Most gross notional open at once, as a multiple of equity (e.g. 1.5)
    #[arg(long)]
    max_gross_exposure: Option<Decimal>,

    /// Fill model: "bar" (fast, the optimizer's engine) or "tick" (one live session per
    /// instrument on one simulated venue; per-instrument strategies only)
    #[arg(long, value_enum, default_value_t = FillModelArg::Bar)]
    fill_model: FillModelArg,

    /// Milliseconds an order takes to reach the market with `--fill-model tick`
    #[arg(long, default_value_t = 100)]
    latency_ms: i64,

    /// Write the run analytics to this SQLite file, created if missing
    #[arg(long)]
    record: Option<PathBuf>,
}

/// `SYMBOL=PATH`, or a bare path whose file name starts with the symbol.
fn parse_data(spec: &str) -> Result<(InstrumentId, PathBuf)> {
    let (symbol, path) = match spec.split_once('=') {
        Some((symbol, path)) => (symbol.to_uppercase(), PathBuf::from(path)),
        None => {
            let path = PathBuf::from(spec);
            let symbol = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split(['-', '.']).next())
                .unwrap_or_default()
                .to_uppercase();
            (symbol, path)
        }
    };
    let instrument = InstrumentId::new(symbol)
        .with_context(|| format!("--data {spec}: name the instrument as SYMBOL=PATH"))?;
    Ok((instrument, path))
}

fn open_stream(path: &Path) -> Result<Box<dyn MarketDataStream<Error = AdapterError>>> {
    if path.is_dir() || path.extension().and_then(|ext| ext.to_str()) == Some("parquet") {
        Ok(Box::new(TapeReader::open(path)?))
    } else {
        let file =
            File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
        Ok(Box::new(BinanceCsvReader::new(BufReader::new(file))))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let started_at_ms = now_unix_ms();

    let mut sources = Vec::new();
    for spec in &args.data {
        sources.push(parse_data(spec)?);
    }
    let mut instruments: Vec<InstrumentId> = sources.iter().map(|(id, _)| id.clone()).collect();
    instruments.sort();
    if instruments.windows(2).any(|pair| pair[0] == pair[1]) {
        bail!("each instrument may be given once: {instruments:?}");
    }

    let market = if args.spot {
        Some(MarketType::Spot)
    } else {
        None
    };
    let strategy_id =
        backtest::resolve_strategy_id(args.strategy.as_deref(), args.config.as_deref())?;
    let load = || {
        backtest::load_strategy_for_market(
            &strategy_id,
            args.config.as_deref(),
            args.shared_config.as_deref(),
            market,
        )
    };
    let loaded = load()?;
    for line in loaded.report_lines() {
        println!("{line}");
    }
    let target_market = if args.spot {
        MarketType::Spot
    } else {
        loaded.market_type.unwrap_or(MarketType::UsdMPerpetual)
    };
    if !loaded.strategy.supports_market_type(target_market) {
        bail!(
            "Strategy '{}' does not support {target_market:?} (supported: {:?})",
            loaded.strategy.name(),
            loaded.strategy.compatible_market_types()
        );
    }
    let calibration_end = loaded.calibration_end_ms;
    let strategy_config = loaded.config_json.clone();

    let mut config = match loaded.execution_context {
        Some(context) => context,
        None if args.spot => BacktestConfig::spot(),
        None => BacktestConfig {
            initial_capital: dec!(10_000),
            ..BacktestConfig::default()
        },
    };
    if args.spot {
        config.market_type = MarketType::Spot;
        config.max_leverage = Decimal::ONE;
        config.funding_rate_pct = Decimal::ZERO;
        config.maintenance_margin_rate = Decimal::ZERO;
    }
    if let Some(capital) = args.capital {
        if capital <= Decimal::ZERO {
            bail!("--capital must be strictly positive, got {capital}");
        }
        config.initial_capital = capital;
    }
    config.max_open_positions = args.max_open_positions.or(config.max_open_positions);
    config.max_same_direction = args.max_same_direction.or(config.max_same_direction);
    config.max_gross_exposure_pct = args.max_gross_exposure.or(config.max_gross_exposure_pct);
    config.validate()?;

    let mut first = Some(loaded.strategy);
    let mut strategies = PortfolioStrategies::new(&instruments, || match first.take() {
        Some(strategy) => Ok::<Box<dyn Strategy>, anyhow::Error>(strategy),
        None => Ok(load()?.strategy),
    })?;
    let scope = match &strategies {
        PortfolioStrategies::PerInstrument(_) => "one instance per instrument",
        PortfolioStrategies::CrossSectional(_) => "one instance across instruments",
    };
    println!(
        "[*] Strategy {strategy_id} on {} instruments ({scope}), at most {} open at once",
        instruments.len(),
        config.portfolio_limits().max_open_positions
    );

    let mut streams = Vec::new();
    for (instrument, path) in &sources {
        println!("[*] {instrument}: {}", path.display());
        streams.push((instrument.clone(), open_stream(path)?));
    }
    let mut merged = MergedTrades::new(streams);
    let mut window = DataWindow {
        source: Some(args.data.join(",")),
        ..DataWindow::default()
    };
    let mut processed = 0usize;
    let (metrics, trades, audit) = match args.fill_model {
        FillModelArg::Bar => {
            let mut engine = PortfolioEngine::new(config, Vec::new());
            // Each instrument's strategy is told of the funding settlements the engine charges.
            let mut funding: BTreeMap<InstrumentId, FundingFeed> =
                if config.market_type == MarketType::UsdMPerpetual {
                    instruments
                        .iter()
                        .map(|id| {
                            let schedule = FundingSchedule::constant(config.funding_rate_pct);
                            (id.clone(), FundingFeed::new(schedule))
                        })
                        .collect()
                } else {
                    BTreeMap::new()
                };
            while let Some((instrument, trade)) = merged.next_trade()? {
                if let Some(feed) = funding.get_mut(&instrument) {
                    for (time_ms, rate) in feed.due(trade.timestamp) {
                        strategies.on_funding(&engine, &instrument, time_ms, rate);
                    }
                }
                if processed == 0 {
                    check_calibration(calibration_end, trade.timestamp)?;
                }
                processed += 1;
                window.start_ms.get_or_insert(trade.timestamp);
                window.end_ms = Some(trade.timestamp);
                strategies.on_trade(&mut engine, &instrument, &trade);
                if engine.is_halted() {
                    println!("[!] Monthly circuit breaker tripped: simulation halted early.");
                    break;
                }
            }
            let audit = strategies.audit();
            let (metrics, trades) = engine.finish_with_trades(true);
            (metrics, trades, audit)
        }
        FillModelArg::Tick => {
            let PortfolioStrategies::PerInstrument(instances) = strategies else {
                bail!(
                    "the tick route runs per-instrument strategies only; run a cross-sectional \
                     strategy with --fill-model bar"
                );
            };
            println!(
                "[*] Fill model: tick, one session per instrument, {} ms order latency",
                args.latency_ms
            );
            let tick_config = TickSimulationConfig {
                backtest: config,
                symbol: String::new(),
                latency_ms: args.latency_ms,
                funding: FundingSchedule::constant(config.funding_rate_pct),
            };
            let mut simulation =
                backtest::wiring::portfolio_tick_simulation(tick_config, instances)?;
            while let Some((instrument, trade)) = merged.next_trade()? {
                if processed == 0 {
                    check_calibration(calibration_end, trade.timestamp)?;
                }
                processed += 1;
                window.start_ms.get_or_insert(trade.timestamp);
                window.end_ms = Some(trade.timestamp);
                simulation.on_trade(&instrument, &trade).await;
            }
            let audits = simulation.strategy_audits();
            let audit = (!audits.is_empty()).then(|| {
                serde_json::Value::Object(
                    audits
                        .into_iter()
                        .map(|(id, audit)| (id.to_string(), audit))
                        .collect(),
                )
            });
            let (metrics, trades) = simulation.finish();
            (metrics, trades, audit)
        }
    };

    println!("============================================================");
    println!(" Trades processed:          {processed}");
    println!(" Closed trades:             {}", metrics.total_trades);
    println!(" Net Profit:                {:.2}", metrics.net_profit);
    println!(
        " Profit Factor:             {}",
        backtest::format_ratio(metrics.profit_factor)
    );
    println!(
        " Max Drawdown (%):          {:.2}%",
        metrics.max_drawdown_pct * dec!(100)
    );
    println!(
        " Rejected Signals:          {}",
        backtest::format_rejections(&metrics.rejected_orders)
    );
    println!("------------------------------------------------------------");
    for (instrument, (count, net)) in results_by_instrument(&trades) {
        println!(" {instrument:<12} trades: {count:<6} net: {net:.2}");
    }
    println!("============================================================");

    let spec = RunSpec {
        strategy_id,
        symbol: instruments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
        market_type: target_market,
        sampling: SamplingConfig::Trades {
            price_bucket_ms: None,
        },
        strategy_config,
        execution_context: config,
    };
    let context = RunContext::new(
        RunType::Backtest,
        &spec,
        started_at_ms,
        &backtest::wiring::entropy(),
    )
    .with_fill_model(match args.fill_model {
        FillModelArg::Bar => backtest::FillModel::Bar,
        FillModelArg::Tick => backtest::FillModel::Tick,
    });
    let recorder = backtest::wiring::recorder(args.record.as_deref())?;
    record_backtest_with_audit(
        &recorder,
        &context,
        &spec,
        &window,
        &metrics,
        &trades,
        audit.as_ref(),
        now_unix_ms(),
    )
    .await?;
    Ok(())
}
