//! Real-Time Tri-Asset Paper Trading Engine for ATSNT.
//!
//! Subscribes concurrently to live Binance aggregate trade WebSocket streams for BTCUSDT,
//! ETHUSDT, and SOLUSDT, aggregates trades into information-driven Dollar Bars independently,
//! executes strategy signals through the unified [`PortfolioEngine`] managing a shared $500 wallet,
//! enforces strict pre-trade risk policy (max positions, max directional exposure, isolated margin caps),
//! streams execution telemetry to Google Cloud BigQuery, and sends real-time execution alerts and a
//! consolidated 20:00 Madrid daily performance summary to Telegram.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use adapters::{
    now_in_madrid, AsyncMarketDataStream, BigQuerySink, BinanceRecentTrades,
    BinanceWebSocketStream, BinanceWsConfig, GcpSecretManager, RunRecorder,
    SymbolPerformanceSummary, TelegramNotifier,
};
use anyhow::Result;
use backtest::operator_alerts::{AlertQueue, DEFAULT_ALERT_QUEUE_CAPACITY, HEARTBEAT_INTERVAL_MS};
use backtest::{
    load_portfolio_artifact, now_unix_ms, AlertThrottle, BacktestConfig, ClosedTrade, DataWindow,
    PortfolioDeploymentArtifact, PortfolioEngine, PortfolioEngineConfig, RunContext, RunId,
    RunSpec, RunStatus, RunType, SamplingConfig,
};
use chrono::Timelike;
use clap::Parser;
use domain::{DollarBarAggregator, PortfolioRiskPolicy, PositionSide, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{OptimizableStrategy, Strategy, StrategyVisitor};

/// Target instrument symbols monitored in this tri-asset portfolio.
pub const PORTFOLIO_SYMBOLS: [&str; 3] = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];

#[derive(Parser, Debug)]
#[command(
    name = "paper_trading_portfolio",
    about = "ATSNT - Tri-Asset Real-Time Paper Trading Portfolio Engine"
)]
struct Args {
    /// Optional path to unified Portfolio Deployment Artifact JSON (from run_portfolio_hpo)
    #[arg(long)]
    portfolio_config: Option<PathBuf>,

    /// Optional path to BTC strategy configuration JSON / deployment artifact
    #[arg(long)]
    btc_config: Option<PathBuf>,

    /// Optional path to ETH strategy configuration JSON / deployment artifact
    #[arg(long)]
    eth_config: Option<PathBuf>,

    /// Optional path to SOL strategy configuration JSON / deployment artifact
    #[arg(long)]
    sol_config: Option<PathBuf>,

    /// Dollar bar accumulation threshold in USDT for BTC (overrides artifact if provided)
    #[arg(long)]
    btc_dollar_bar: Option<Decimal>,

    /// Dollar bar accumulation threshold in USDT for ETH (overrides artifact if provided)
    #[arg(long)]
    eth_dollar_bar: Option<Decimal>,

    /// Dollar bar accumulation threshold in USDT for SOL (overrides artifact if provided)
    #[arg(long)]
    sol_dollar_bar: Option<Decimal>,

    /// Initial shared capital across the portfolio in quote currency (USDT)
    #[arg(long, default_value = "500")]
    capital: Decimal,

    /// Fixed fractional risk percentage per trade relative to total equity (default: 0.01 = 1.0%)
    #[arg(long, default_value = "0.010")]
    risk_per_trade: Decimal,

    /// Maximum concurrent open positions across the portfolio (default: 3, max 1 per asset)
    #[arg(long, default_value_t = 3)]
    max_positions: usize,

    /// Maximum positions in the same direction (default: 2)
    #[arg(long, default_value_t = 2)]
    max_directional: usize,

    /// Maximum margin allocation per position as fraction of equity (default: 0.25 = 25%)
    #[arg(long, default_value = "0.25")]
    max_slot_margin: Decimal,

    /// Whether to enter using passive limit orders with TIF timeout
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    use_limit_orders: bool,

    /// Limit order Time-In-Force timeout in bars (default: 2)
    #[arg(long, default_value_t = 2)]
    limit_timeout_bars: usize,

    /// Simulated execution latency in milliseconds
    #[arg(long, default_value_t = 100)]
    latency_ms: i64,

    /// Warm up strategy rolling windows with this many minutes of recent trades
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=1440))]
    warmup_minutes: Option<u32>,

    /// Override funding rate charged at eight-hour settlement
    #[arg(long)]
    funding_rate: Option<Decimal>,

    /// Baseline backtest run ID for BI comparison
    #[arg(long, value_parser = RunId::parse)]
    baseline_run_id: Option<RunId>,

    /// Stream lifecycle and closed trade telemetry to Google Cloud BigQuery
    #[arg(long, default_value_t = false)]
    gcp_bigquery: bool,

    /// Send real-time execution alerts and consolidated daily summary to Telegram
    #[arg(long, default_value_t = false)]
    telegram_alerts: bool,

    /// Print individual raw market trade ticks to stdout (verbose debug mode)
    #[arg(long, default_value_t = false)]
    show_ticks: bool,

    /// Whether to run paper trading in Spot market mode
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Whether to apply Binance 25% BNB fee discount (0.075% in Spot)
    #[arg(long, default_value_t = false)]
    bnb_discount: bool,
}

/// A trade tagged with its instrument symbol for MPSC multiplexing.
#[derive(Debug, Clone)]
struct TaggedTrade {
    symbol: &'static str,
    trade: Trade,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut alerts = None;
    let result = run(&mut alerts).await;
    if let Some(alerts) = alerts {
        alerts.shutdown(Duration::from_secs(5)).await;
    }
    result
}

/// Specification of a single asset's execution plan within the multi-asset portfolio.
struct AssetPlan {
    strategy_id: String,
    config: serde_json::Value,
    dollar_bar_threshold: Decimal,
}

/// Dynamic strategy builder visiting the ATSNT strategy registry.
struct StrategyInstantiator<'a> {
    config_val: &'a serde_json::Value,
}

impl<'a> StrategyVisitor for StrategyInstantiator<'a> {
    type Output = Result<Box<dyn Strategy>>;

    fn visit<S>(self) -> Result<Box<dyn Strategy>>
    where
        S: OptimizableStrategy + 'static,
        S::Config: Default,
        S::Space: serde::Serialize + serde::de::DeserializeOwned,
    {
        let cfg: S::Config = serde_json::from_value(self.config_val.clone()).map_err(|e| {
            anyhow::anyhow!("Failed to parse config for strategy '{}': {e}", S::id())
        })?;
        let strat = S::from_config(&S::SharedConfig::default(), cfg)
            .map_err(|e| anyhow::anyhow!("Failed to build strategy '{}': {e}", S::id()))?;
        Ok(Box::new(strat))
    }
}

fn instantiate_strategy(
    strategy_id: &str,
    config_val: &serde_json::Value,
) -> Result<Box<dyn Strategy>> {
    match strategies::dispatch(strategy_id, StrategyInstantiator { config_val }) {
        Some(res) => res,
        None => anyhow::bail!(strategies::unknown_strategy_message(strategy_id)),
    }
}

/// Resolves an individual asset's strategy plan from legacy bare or deployment artifact files.
fn resolve_asset_plan(
    symbol: &str,
    config_path: Option<&PathBuf>,
    cli_threshold: Option<Decimal>,
    fallback_threshold: Decimal,
    use_limit_orders: bool,
    limit_timeout_bars: usize,
) -> Result<AssetPlan> {
    if let Some(path) = config_path.filter(|p| p.exists()) {
        println!(
            "[*] Loading {} strategy configuration from: {}",
            symbol,
            path.display()
        );
        let content = std::fs::read_to_string(path)?;
        let val: serde_json::Value = serde_json::from_str(&content)?;

        let (strategy_id, mut config, bar_threshold) = if let Some(spec) = val.get("spec") {
            let s_id = spec
                .get("strategy_id")
                .and_then(|s| s.as_str())
                .unwrap_or("quantum_well_swing")
                .to_string();
            let s_cfg = spec
                .get("strategy_config")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let threshold = spec
                .get("sampling")
                .and_then(|s| s.get("DollarBar"))
                .and_then(|db| db.get("threshold"))
                .and_then(|t| {
                    if let Some(s) = t.as_str() {
                        s.parse::<Decimal>().ok()
                    } else {
                        t.as_i64().map(Decimal::from)
                    }
                })
                .unwrap_or(fallback_threshold);
            (s_id, s_cfg, threshold)
        } else if let Some(s_id) = val.get("strategy_id").and_then(|s| s.as_str()) {
            let s_cfg = val
                .get("strategy_config")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            (s_id.to_string(), s_cfg, fallback_threshold)
        } else {
            ("quantum_well_swing".to_string(), val, fallback_threshold)
        };

        if use_limit_orders {
            if let Some(obj) = config.as_object_mut() {
                obj.insert("use_limit_entries".to_string(), serde_json::json!(true));
                obj.insert(
                    "limit_time_in_force_bars".to_string(),
                    serde_json::json!(limit_timeout_bars),
                );
            }
        }

        let effective_threshold = cli_threshold.unwrap_or(bar_threshold);
        Ok(AssetPlan {
            strategy_id,
            config,
            dollar_bar_threshold: effective_threshold,
        })
    } else {
        println!(
            "[!] Configuration file for {} not found; applying calibrated baseline",
            symbol
        );
        let default_cfg = serde_json::json!({
            "window_size": 240,
            "ground_state_tolerance": "0.040",
            "min_well_curvature": "0.020",
            "min_upward_force": "0.010",
            "mass_eff": "3.0",
            "hbar_eff": "2.5",
            "target_excitation_level": 1,
            "max_wavepacket_sigma": "0.08",
            "stop_loss_atr_buffer": "2.00",
            "min_take_profit_pct": "0.090",
            "breakeven_trigger_ratio": "0.50",
            "enable_dynamic_breakeven": true,
            "decoherence_time_bars": 90,
            "macro_trend_filter": true,
            "macro_ema_period": 200,
            "macro_slope_window": 30,
            "use_limit_entries": use_limit_orders,
            "limit_time_in_force_bars": if use_limit_orders { Some(limit_timeout_bars) } else { None }
        });
        Ok(AssetPlan {
            strategy_id: "quantum_well_swing".to_string(),
            config: default_cfg,
            dollar_bar_threshold: cli_threshold.unwrap_or(fallback_threshold),
        })
    }
}

async fn run(alerts: &mut Option<AlertQueue>) -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    let args = Args::parse();
    let started_at_ms = now_unix_ms();

    println!("============================================================");
    println!("     ATSNT - Tri-Asset Real-Time Paper Trading Portfolio    ");
    println!("============================================================");
    // 1. Resolve Strategy Configurations & Thresholds
    let portfolio_artifact: Option<PortfolioDeploymentArtifact> =
        if let Some(path) = &args.portfolio_config {
            if path.exists() {
                println!(
                    "[*] Loading unified portfolio deployment artifact: {}",
                    path.display()
                );
                Some(load_portfolio_artifact(path)?)
            } else {
                None
            }
        } else {
            None
        };

    let mut effective_capital = args.capital;
    let mut effective_risk_per_trade = args.risk_per_trade;
    let mut effective_max_positions = args.max_positions;
    let mut effective_max_directional = args.max_directional;
    let mut effective_max_slot_margin = args.max_slot_margin;
    let mut effective_use_limit_orders = args.use_limit_orders;
    let mut effective_limit_timeout_bars = args.limit_timeout_bars;
    let mut effective_elastic_slots = false;
    let mut effective_max_elastic_slot_margin = dec!(0.55);
    let mut effective_max_leverage = dec!(20);

    let mut effective_maker_fee = None;
    let mut effective_taker_fee = None;
    let mut effective_slippage = dec!(0.0005);
    let mut effective_btc_breadth = false;
    let mut effective_btc_corr_thresh = dec!(0.65);
    let mut effective_btc_corr_window = 35;

    let mut btc_from_artifact = None;
    let mut eth_from_artifact = None;
    let mut sol_from_artifact = None;

    if let Some(art) = &portfolio_artifact {
        effective_capital = art.capital;
        effective_max_positions = art.risk_policy.max_open_positions;
        effective_max_directional = art.risk_policy.max_directional_positions;
        effective_risk_per_trade = art.risk_policy.risk_per_trade_pct;
        effective_max_slot_margin = art.risk_policy.max_slot_margin_pct;
        effective_elastic_slots = art.risk_policy.elastic_slot_sizing;
        effective_max_elastic_slot_margin = art.risk_policy.max_elastic_slot_margin_pct;
        effective_max_leverage = art.risk_policy.max_leverage;

        effective_use_limit_orders = art.execution.use_limit_orders;
        effective_limit_timeout_bars = art.execution.limit_timeout_bars;
        effective_maker_fee = Some(art.execution.maker_fee_pct);
        effective_taker_fee = Some(art.execution.taker_fee_pct);
        effective_slippage = art.execution.slippage_pct;
        effective_btc_breadth = art.execution.btc_breadth_filter;
        effective_btc_corr_thresh = art.execution.btc_correlation_threshold;
        effective_btc_corr_window = art.execution.btc_correlation_window;

        for asset in &art.assets {
            let plan = AssetPlan {
                strategy_id: asset.strategy_id.clone(),
                config: asset.strategy_config.clone(),
                dollar_bar_threshold: asset.dollar_bar_threshold,
            };
            match asset.symbol.as_str() {
                "BTCUSDT" => btc_from_artifact = Some(plan),
                "ETHUSDT" => eth_from_artifact = Some(plan),
                "SOLUSDT" => sol_from_artifact = Some(plan),
                _ => {}
            }
        }
    }

    let default_btc_path =
        PathBuf::from("configs/matrix/hpo_btcusdt_2026-hpo-btcusdt-8000000-20261027-184750.json");
    let default_eth_path =
        PathBuf::from("configs/matrix/hpo_ethusdt_2026-hpo-ethusdt-6000000-20261027-132227.json");
    let default_sol_path =
        PathBuf::from("configs/matrix/hpo_solusdt_2026-hpo-solusdt-4000000-20261027-132227.json");

    let btc_plan = if let Some(p) = btc_from_artifact {
        AssetPlan {
            dollar_bar_threshold: args.btc_dollar_bar.unwrap_or(p.dollar_bar_threshold),
            ..p
        }
    } else {
        let btc_path = args.btc_config.as_ref().or(Some(&default_btc_path));
        resolve_asset_plan(
            "BTCUSDT",
            btc_path,
            args.btc_dollar_bar,
            dec!(300_000_000),
            effective_use_limit_orders,
            effective_limit_timeout_bars,
        )?
    };

    let eth_plan = if let Some(p) = eth_from_artifact {
        AssetPlan {
            dollar_bar_threshold: args.eth_dollar_bar.unwrap_or(p.dollar_bar_threshold),
            ..p
        }
    } else {
        let eth_path = args.eth_config.as_ref().or(Some(&default_eth_path));
        resolve_asset_plan(
            "ETHUSDT",
            eth_path,
            args.eth_dollar_bar,
            dec!(200_000_000),
            effective_use_limit_orders,
            effective_limit_timeout_bars,
        )?
    };

    let sol_plan = if let Some(p) = sol_from_artifact {
        AssetPlan {
            dollar_bar_threshold: args.sol_dollar_bar.unwrap_or(p.dollar_bar_threshold),
            ..p
        }
    } else {
        let sol_path = args.sol_config.as_ref().or(Some(&default_sol_path));
        resolve_asset_plan(
            "SOLUSDT",
            sol_path,
            args.sol_dollar_bar,
            dec!(80_000_000),
            effective_use_limit_orders,
            effective_limit_timeout_bars,
        )?
    };

    let market_type = if args.spot || effective_max_leverage <= dec!(1) {
        strategies::MarketType::Spot
    } else {
        strategies::MarketType::UsdMPerpetual
    };

    let (maker_fee_pct, taker_fee_pct, max_leverage) = match (market_type, args.bnb_discount) {
        (strategies::MarketType::Spot, true) => (
            effective_maker_fee.unwrap_or(dec!(0.00075)),
            effective_taker_fee.unwrap_or(dec!(0.00075)),
            dec!(1),
        ),
        (strategies::MarketType::Spot, false) => (
            effective_maker_fee.unwrap_or(dec!(0.0010)),
            effective_taker_fee.unwrap_or(dec!(0.0010)),
            dec!(1),
        ),
        (strategies::MarketType::UsdMPerpetual, true) => (
            effective_maker_fee.unwrap_or(dec!(0.00018)),
            effective_taker_fee.unwrap_or(dec!(0.00045)),
            effective_max_leverage,
        ),
        (strategies::MarketType::UsdMPerpetual, false) => (
            effective_maker_fee.unwrap_or(dec!(0.0002)),
            effective_taker_fee.unwrap_or(dec!(0.0005)),
            effective_max_leverage,
        ),
    };

    println!(" Market Type:          {:?}", market_type);
    println!(" Strategy:             {}", btc_plan.strategy_id);
    println!(" Shared Capital:       ${:.2} USDT", effective_capital);
    println!(" Max Concurrent Pos:   {}", effective_max_positions);
    println!(" Max Directional Pos:  {}", effective_max_directional);
    println!(
        " Risk Per Trade:       {:.2}%",
        effective_risk_per_trade * dec!(100)
    );
    println!(
        " Max Slot Margin:      {:.1}%",
        effective_max_slot_margin * dec!(100)
    );
    if effective_elastic_slots {
        println!(
            " Elastic Slot Margin:  {:.1}% (occupancy <= 1)",
            effective_max_elastic_slot_margin * dec!(100)
        );
    }
    println!(
        " Fee Schedule:         Maker {:.4}% / Taker {:.4}%",
        maker_fee_pct * dec!(100),
        taker_fee_pct * dec!(100)
    );
    println!(
        " Execution Mode:       {}",
        if effective_use_limit_orders {
            format!("Maker Limit (TIF {} bars)", effective_limit_timeout_bars)
        } else {
            "Aggressive Market (Taker)".to_string()
        }
    );
    if effective_btc_breadth {
        println!(
            " BTC Breadth Filter:   Active (Threshold: {}, Window: {} bars)",
            effective_btc_corr_thresh, effective_btc_corr_window
        );
    }
    println!(" Thresholds:");
    println!("   • BTCUSDT: ${}", btc_plan.dollar_bar_threshold);
    println!("   • ETHUSDT: ${}", eth_plan.dollar_bar_threshold);
    println!("   • SOLUSDT: ${}", sol_plan.dollar_bar_threshold);

    // 2. Initialize BigQuery Sink & Telegram Notifier
    let bq_sink = if args.gcp_bigquery {
        let sink = BigQuerySink::from_env();
        println!(
            " BigQuery Telemetry:   {}",
            if sink.is_enabled() {
                "ACTIVE (Streaming runs & trades to atsnt_bi)"
            } else {
                "ENABLED (Pending GCP credentials)"
            }
        );
        Some(sink)
    } else {
        None
    };

    let secret_manager = GcpSecretManager::from_env();
    if args.telegram_alerts {
        let t = TelegramNotifier::from_gcp_or_env(&secret_manager).await;
        println!(
            " Telegram Alerts:      {}",
            if t.is_enabled() {
                "ACTIVE (Connected to Telegram Bot API)"
            } else {
                "ENABLED (Pending TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID)"
            }
        );
        *alerts = Some(AlertQueue::spawn(t, DEFAULT_ALERT_QUEUE_CAPACITY));
    }
    let telegram: &Option<AlertQueue> = alerts;

    // 3. Initialize Strategies
    let mut btc_strat = instantiate_strategy(&btc_plan.strategy_id, &btc_plan.config)?;
    let mut eth_strat = instantiate_strategy(&eth_plan.strategy_id, &eth_plan.config)?;
    let mut sol_strat = instantiate_strategy(&sol_plan.strategy_id, &sol_plan.config)?;

    // 4. Initialize Dollar Bar Aggregators
    let mut btc_agg = DollarBarAggregator::new(btc_plan.dollar_bar_threshold)
        .map_err(|e| anyhow::anyhow!("Invalid BTC threshold: {e}"))?;
    let mut eth_agg = DollarBarAggregator::new(eth_plan.dollar_bar_threshold)
        .map_err(|e| anyhow::anyhow!("Invalid ETH threshold: {e}"))?;
    let mut sol_agg = DollarBarAggregator::new(sol_plan.dollar_bar_threshold)
        .map_err(|e| anyhow::anyhow!("Invalid SOL threshold: {e}"))?;

    // 5. Initialize Multi-Asset Portfolio Engine
    let risk_policy = PortfolioRiskPolicy {
        max_open_positions: effective_max_positions,
        max_directional_positions: if market_type == strategies::MarketType::Spot {
            effective_max_positions
        } else {
            effective_max_directional
        },
        risk_per_trade_pct: effective_risk_per_trade,
        max_slot_margin_pct: effective_max_slot_margin,
        max_leverage,
        elastic_slot_sizing: effective_elastic_slots,
        max_elastic_slot_margin_pct: effective_max_elastic_slot_margin,
        min_order_quantity: dec!(0.001),
        quantity_step_size: dec!(0.001),
        min_order_notional: dec!(5),
    };

    let engine_config = PortfolioEngineConfig {
        initial_capital: effective_capital,
        maker_fee_pct,
        taker_fee_pct,
        slippage_pct: effective_slippage,
        risk_policy,
        latency_ms: args.latency_ms,
        market_type,
        btc_breadth_filter: effective_btc_breadth,
        btc_correlation_threshold: effective_btc_corr_thresh,
        btc_correlation_window: effective_btc_corr_window,
    };
    let mut engine = PortfolioEngine::new(engine_config);

    // 6. Optional Warm-up via Public REST AggTrades
    if let Some(minutes) = args.warmup_minutes {
        let now_ms = now_unix_ms();
        let from_ms = now_ms - i64::from(minutes) * 60_000;
        println!("[*] Warming up indicators with {minutes} minutes of recent trades ({market_type:?})...");

        let rest_client = if market_type == strategies::MarketType::Spot {
            BinanceRecentTrades::spot(false)
        } else {
            BinanceRecentTrades::futures(false)
        };

        let btc_hist = rest_client.fetch("BTCUSDT", from_ms, now_ms).await?;
        let mut btc_bars = 0;
        for t in btc_hist {
            if let Some(bar) = btc_agg.process_trade(&t) {
                btc_bars += 1;
                btc_strat.on_bar(&bar);
            }
        }

        let eth_hist = rest_client.fetch("ETHUSDT", from_ms, now_ms).await?;
        let mut eth_bars = 0;
        for t in eth_hist {
            if let Some(bar) = eth_agg.process_trade(&t) {
                eth_bars += 1;
                eth_strat.on_bar(&bar);
            }
        }

        let sol_hist = rest_client.fetch("SOLUSDT", from_ms, now_ms).await?;
        let mut sol_bars = 0;
        for t in sol_hist {
            if let Some(bar) = sol_agg.process_trade(&t) {
                sol_bars += 1;
                sol_strat.on_bar(&bar);
            }
        }

        println!(
            "    [OK] Warm-up complete: BTC {btc_bars} bars, ETH {eth_bars} bars, SOL {sol_bars} bars"
        );
    }

    // 7. Spawn Concurrent WebSocket Ingestion Tasks
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TaggedTrade>(30_000);
    let is_spot = market_type == strategies::MarketType::Spot;

    for &sym in &PORTFOLIO_SYMBOLS {
        let sym_str = sym.to_ascii_lowercase();
        let sym_static: &'static str = match sym {
            "BTCUSDT" => "BTCUSDT",
            "ETHUSDT" => "ETHUSDT",
            "SOLUSDT" => "SOLUSDT",
            _ => unreachable!(),
        };
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            let ws_cfg = if is_spot {
                BinanceWsConfig::spot(&sym_str)
            } else {
                BinanceWsConfig::futures_production(&sym_str)
            };
            println!(
                "[*] [{sym_static}] Connecting to WebSocket stream: {}",
                ws_cfg.stream_url()
            );

            loop {
                match BinanceWebSocketStream::connect(ws_cfg.clone()) {
                    Ok(mut stream) => {
                        println!("[OK] [{sym_static}] WebSocket connected and streaming trades");
                        while let Ok(Some(trade)) = stream.next_trade().await {
                            if tx_clone
                                .send(TaggedTrade {
                                    symbol: sym_static,
                                    trade,
                                })
                                .await
                                .is_err()
                            {
                                return; // Parent receiver dropped; shutdown task
                            }
                        }
                        eprintln!(
                            "[WARN] [{sym_static}] WebSocket stream disconnected. Reconnecting..."
                        );
                    }
                    Err(e) => {
                        eprintln!("[ERROR] [{sym_static}] Failed to connect WebSocket: {e}. Retrying in 2s...");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        });
    }

    drop(tx); // Drop original sender so channel closes when all background workers terminate

    // 8. Lifecycle & BigQuery Session Registration
    let effective_strategy_id = format!("portfolio_tri_asset_{}", btc_plan.strategy_id);
    let run_spec = RunSpec {
        strategy_id: effective_strategy_id,
        symbol: "PORTFOLIO".to_string(),
        market_type,
        sampling: SamplingConfig::DollarBar {
            threshold: btc_plan.dollar_bar_threshold,
        },
        strategy_config: serde_json::json!({
            "strategy_id": btc_plan.strategy_id,
            "btc_threshold": btc_plan.dollar_bar_threshold,
            "eth_threshold": eth_plan.dollar_bar_threshold,
            "sol_threshold": sol_plan.dollar_bar_threshold,
            "risk_per_trade": effective_risk_per_trade,
            "max_positions": effective_max_positions,
            "max_directional": effective_max_directional,
            "max_slot_margin": effective_max_slot_margin,
            "elastic_slots": effective_elastic_slots,
            "max_elastic_slot_margin": effective_max_elastic_slot_margin,
            "use_limit_orders": effective_use_limit_orders,
            "btc_breadth_filter": effective_btc_breadth,
            "btc_correlation_threshold": effective_btc_corr_thresh,
            "btc_correlation_window": effective_btc_corr_window,
        }),
        execution_context: BacktestConfig {
            initial_capital: effective_capital,
            risk_per_trade_pct: effective_risk_per_trade,
            latency_ms: args.latency_ms,
            ..BacktestConfig::default()
        },
    };

    let run_ctx = RunContext::new(RunType::Paper, &run_spec, started_at_ms)
        .with_baseline(args.baseline_run_id.clone());
    let mut data_window = DataWindow {
        source: Some(if is_spot {
            "Binance Spot (Multi-Stream)".to_string()
        } else {
            "Binance USD-M Futures (Multi-Stream)".to_string()
        }),
        ..DataWindow::default()
    };

    if let Some(bq) = &bq_sink {
        println!(
            "[*] Registering portfolio paper run {} in BigQuery",
            run_ctx.run_id
        );
        record_lifecycle(bq, &run_ctx, &run_spec, RunStatus::Running, &data_window).await;
    }

    if let Some(t) = &telegram {
        t.send(format!(
            "🚀 *ATSNT: Multi-Asset Paper Trading Launched*\n\
             ━━━━━━━━━━━━━━━━━━━━━━\n\
             • *Market*: `{:?}` ({})\n\
             • *Strategy*: `{}`\n\
             • *Assets*: `BTCUSDT`, `ETHUSDT`, `SOLUSDT`\n\
             • *Shared Capital*: `${:.2} USDT`\n\
             • *Max Positions*: `{}` (Max `{}` directional)\n\
             • *Risk Per Trade*: `{:.2}%` ($`{:.2}`)\n\
             • *Thresholds*: BTC ${}, ETH ${}, SOL ${}\n\
             • *Execution*: {}\n\
             • *Breadth Filter*: {}\n\
             ━━━━━━━━━━━━━━━━━━━━━━",
            market_type,
            if is_spot {
                "1.0x, Zero Liquidation"
            } else {
                "20x Leverage"
            },
            btc_plan.strategy_id,
            effective_capital,
            effective_max_positions,
            effective_max_directional,
            effective_risk_per_trade * dec!(100),
            effective_capital * effective_risk_per_trade,
            btc_plan.dollar_bar_threshold,
            eth_plan.dollar_bar_threshold,
            sol_plan.dollar_bar_threshold,
            if effective_use_limit_orders {
                format!("Maker Limit (TIF {})", effective_limit_timeout_bars)
            } else {
                "Market Taker".to_string()
            },
            if effective_btc_breadth {
                format!("Active (BTC Corr >= {})", effective_btc_corr_thresh)
            } else {
                "Disabled".to_string()
            }
        ));
    }

    println!("[*] Engine initialized. Awaiting market trades (Press Ctrl+C to terminate)...\n");

    // 9. Telemetry & Accounting State
    let mut last_mark_prices: HashMap<&'static str, Decimal> = HashMap::new();
    let mut total_ticks: HashMap<&'static str, u64> = HashMap::new();
    let mut total_bars: HashMap<&'static str, u64> = HashMap::new();
    let mut trade_counter: usize = 0;

    let mut current_day = now_in_madrid().date_naive();
    let mut last_daily_report_date: Option<chrono::NaiveDate> = None;
    let mut daily_trades_count: usize = 0;
    let mut daily_wins: usize = 0;
    let mut daily_losses: usize = 0;
    let mut daily_gross_pnl = Decimal::ZERO;
    let mut daily_fees = Decimal::ZERO;
    let mut daily_net_pnl = Decimal::ZERO;

    let mut daily_timer = tokio::time::interval(Duration::from_secs(10));
    let heartbeat = AlertThrottle::new(HEARTBEAT_INTERVAL_MS);
    let session_start_time = Instant::now();

    // 10. Main Asynchronous Event Loop
    loop {
        tokio::select! {
            biased;

            _ = tokio::signal::ctrl_c() => {
                println!("\n[SHUTDOWN] Interruption signal received (Ctrl+C). Terminating portfolio session...");
                let total_eq = engine.total_marked_equity();
                let dd = engine.current_drawdown_pct();
                let pos_desc = format_portfolio_positions(&engine);

                if let Some(t) = &telegram {
                    t.send(format!(
                        "🛑 *ATSNT PORTFOLIO STOPPED*\n\
                         • *Reason*: Operator shutdown\n\
                         • *Equity*: `${:.2}`\n\
                         • *Drawdown*: `{:.2}%`\n\
                         • *Positions*: {}",
                        total_eq,
                        dd * dec!(100),
                        pos_desc
                    ));
                }
                if let Some(bq) = &bq_sink {
                    record_lifecycle(bq, &run_ctx, &run_spec, RunStatus::Stopped, &data_window).await;
                }
                break;
            }

            _ = daily_timer.tick() => {
                let now_ms = now_unix_ms();
                let now_madrid = now_in_madrid();

                // Periodic Heartbeat
                if let Some(t) = &telegram {
                    if heartbeat.allow(now_ms) {
                        let total_eq = engine.total_marked_equity();
                        let dd = engine.current_drawdown_pct();
                        let pos_desc = format_portfolio_positions(&engine);
                        let open_count = engine.open_positions_count();
                        let uptime = session_start_time.elapsed().as_secs();

                        t.send(TelegramNotifier::format_portfolio_heartbeat(
                            args.capital,
                            total_eq,
                            dd,
                            open_count,
                            &pos_desc,
                            uptime,
                        ));
                    }
                }

                // Midnight Day Rollover
                if now_madrid.date_naive() != current_day {
                    current_day = now_madrid.date_naive();
                    daily_trades_count = 0;
                    daily_wins = 0;
                    daily_losses = 0;
                    daily_gross_pnl = Decimal::ZERO;
                    daily_fees = Decimal::ZERO;
                    daily_net_pnl = Decimal::ZERO;
                }

                // 20:00 Madrid Daily Performance Report
                if now_madrid.hour() == 20 && last_daily_report_date != Some(now_madrid.date_naive()) {
                    last_daily_report_date = Some(now_madrid.date_naive());
                    if let Some(t) = &telegram {
                        let total_eq = engine.total_marked_equity();
                        let cash_eq = engine.cash_equity();
                        let dd = engine.current_drawdown_pct();
                        let date_str = now_madrid.format("%Y-%m-%d").to_string();

                        let closed_by_sym = engine.closed_trades_by_symbol();
                        let mut summaries = Vec::new();

                        for &sym in &PORTFOLIO_SYMBOLS {
                            let sym_trades = closed_by_sym.get(sym).map(|v| v.as_slice()).unwrap_or(&[]);
                            let sym_wins = sym_trades.iter().filter(|t| t.pnl_net > Decimal::ZERO).count();
                            let sym_losses = sym_trades.iter().filter(|t| t.pnl_net < Decimal::ZERO).count();
                            let sym_net: Decimal = sym_trades.iter().map(|t| t.pnl_net).sum();
                            let active_desc = match engine.position(sym) {
                                Some(pos) => format!("{:?} {} @ ${}", pos.side, pos.quantity, pos.entry_price),
                                None => "Flat".to_string(),
                            };

                            summaries.push(SymbolPerformanceSummary {
                                symbol: sym.to_string(),
                                trades_count: sym_trades.len(),
                                wins: sym_wins,
                                losses: sym_losses,
                                net_pnl: sym_net,
                                active_position_desc: active_desc,
                                ticks_count: *total_ticks.get(sym).unwrap_or(&0),
                                bars_count: *total_bars.get(sym).unwrap_or(&0),
                            });
                        }

                        let msg = TelegramNotifier::format_portfolio_daily_summary(
                            &date_str,
                            "Paper Trading Portfolio",
                            effective_capital,
                            cash_eq,
                            total_eq,
                            dd,
                            daily_trades_count,
                            daily_wins,
                            daily_losses,
                            daily_gross_pnl,
                            daily_fees,
                            daily_net_pnl,
                            &summaries,
                        );
                        t.send(msg);
                        tracing::info!(target: "telegram", "Queued consolidated 20:00 Madrid daily portfolio summary");
                    }
                }
            }

            Some(tagged) = rx.recv() => {
                let sym = tagged.symbol;
                let trade = tagged.trade;
                last_mark_prices.insert(sym, trade.price);
                *total_ticks.entry(sym).or_insert(0) += 1;

                data_window.start_ms.get_or_insert(trade.timestamp);
                data_window.end_ms = Some(trade.timestamp);

                if args.show_ticks {
                    println!("[TICK] [{sym}] ${} | qty {} | at {}", trade.price, trade.quantity, trade.timestamp);
                }

                // Route trade to the instrument's Dollar Bar Aggregator
                let formed_bar = match sym {
                    "BTCUSDT" => btc_agg.process_trade(&trade),
                    "ETHUSDT" => eth_agg.process_trade(&trade),
                    "SOLUSDT" => sol_agg.process_trade(&trade),
                    _ => None,
                };

                if let Some(bar) = formed_bar {
                    *total_bars.entry(sym).or_insert(0) += 1;
                    let price_event = backtest::PriceEvent::from(&bar);

                    // Step through unified portfolio engine
                    let report = match sym {
                        "BTCUSDT" => engine.process_event("BTCUSDT", price_event, &mut btc_strat),
                        "ETHUSDT" => engine.process_event("ETHUSDT", price_event, &mut eth_strat),
                        "SOLUSDT" => engine.process_event("SOLUSDT", price_event, &mut sol_strat),
                        _ => Default::default(),
                    };

                    // Handle Newly Opened Positions
                    if let Some((pos, exits)) = report.opened_position {
                        println!(
                            "[+] [{sym}] Position Opened: {:?} {} @ ${} | Total Equity: ${:.2}",
                            pos.side,
                            pos.quantity,
                            pos.entry_price,
                            engine.total_marked_equity()
                        );

                        if let Some(t) = &telegram {
                            let sl = exits.as_ref().map(|e| e.stop).unwrap_or(Decimal::ZERO);
                            let tp = exits.as_ref().and_then(|e| e.take_profit).unwrap_or(Decimal::ZERO);
                            let side_str = match pos.side {
                                PositionSide::Long => "Long",
                                PositionSide::Short => "Short",
                            };

                            t.send(TelegramNotifier::format_position_opened(
                                sym,
                                side_str,
                                pos.entry_price,
                                pos.quantity,
                                sl,
                                tp,
                                engine.total_marked_equity(),
                            ));
                        }
                    }

                    // Handle Newly Closed Trades
                    for closed in report.closed_trades {
                        trade_counter += 1;
                        daily_trades_count += 1;
                        if closed.pnl_net > Decimal::ZERO {
                            daily_wins += 1;
                        } else if closed.pnl_net < Decimal::ZERO {
                            daily_losses += 1;
                        }
                        daily_gross_pnl += closed.pnl_gross;
                        daily_fees += closed.fees_paid;
                        daily_net_pnl += closed.pnl_net;

                        let total_eq = engine.total_marked_equity();
                        println!(
                            "[-] [{sym}] Position Closed ({}) @ ${} | Net: ${:.2} | Total Equity: ${:.2}",
                            closed.exit_reason,
                            closed.exit_price,
                            closed.pnl_net,
                            total_eq
                        );

                        if let Some(t) = &telegram {
                            let side_str = match closed.side {
                                PositionSide::Long => "Long",
                                PositionSide::Short => "Short",
                            };
                            let duration_secs = closed.holding_duration_seconds;
                            let fees = closed.fees_paid;

                            t.send(TelegramNotifier::format_trade_summary(
                                sym,
                                side_str,
                                &closed.exit_reason,
                                closed.entry_price,
                                closed.exit_price,
                                closed.quantity,
                                duration_secs,
                                closed.pnl_gross,
                                fees,
                                closed.pnl_net,
                                closed.return_pct,
                                total_eq,
                            ));
                        }

                        if let Some(bq) = &bq_sink {
                            record_closed_trade(bq, &run_ctx, &closed, trade_counter).await;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Formats active open positions across the portfolio into a single concise string.
fn format_portfolio_positions(engine: &PortfolioEngine) -> String {
    let mut parts = Vec::new();
    for &sym in &PORTFOLIO_SYMBOLS {
        if let Some(pos) = engine.position(sym) {
            parts.push(format!("{sym}: {:?} {}", pos.side, pos.quantity));
        }
    }
    if parts.is_empty() {
        "All Flat".to_string()
    } else {
        parts.join(" | ")
    }
}

async fn record_lifecycle<R: RunRecorder>(
    recorder: &R,
    ctx: &RunContext,
    spec: &RunSpec<serde_json::Value>,
    status: RunStatus,
    data: &DataWindow,
) {
    if let Err(e) = backtest::record_run_status(recorder, ctx, spec, status, data).await {
        eprintln!(
            "[WARN] Failed to record portfolio paper run status {}: {e}",
            status.as_str()
        );
    }
}

async fn record_closed_trade<R: RunRecorder>(
    recorder: &R,
    ctx: &RunContext,
    trade: &ClosedTrade,
    number: usize,
) {
    if let Err(e) = backtest::record_closed_trade(recorder, ctx, trade, number).await {
        eprintln!("[WARN] Failed to record portfolio paper trade #{number}: {e}");
    }
}
