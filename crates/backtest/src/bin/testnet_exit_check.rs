//! Binance USD-M **testnet** harness for the live exit lifecycle (docs/architecture/08, runbook).
//!
//! It drives the real `BinanceGateway` and `ExitManager` with minimal orders. There is
//! deliberately no production option: the gateway is always built with `testnet: true`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adapters::{BinanceAuth, BinanceGateway, BinanceGatewayConfig, GcpSecretManager};
use anyhow::{bail, Context, Result};
use backtest::{ExitEvent, ExitManager, HaltState};
use clap::{Parser, Subcommand};
use domain::{RiskPolicy, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

#[derive(Parser, Debug)]
#[command(
    name = "testnet_exit_check",
    about = "Exercises exchange-held exits and the emergency close on Binance USD-M TESTNET"
)]
struct Args {
    /// USD-M symbol; the harness only supports USDT-margined perpetuals.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Read-only: balance, position size and pending algo orders.
    Inspect,
    /// Runs scenarios A-E (protect, time barrier, exchange close cleanup, halt, failed protection).
    Scenario,
    /// Opens a long with exchange-held exits and exits WITHOUT any cleanup (crash simulation).
    LeaveOpen,
    /// Cancels every order and flattens the position.
    Cleanup,
    /// Read-only account profile verification (the same report the live bot prints at startup).
    VerifyAccount,
    /// Prints the raw private-stream events (type and payload keys) for N seconds.
    CaptureEvents {
        #[arg(long, default_value_t = 90)]
        seconds: u64,
    },
}

/// Raw private-stream observations: event counts by type and the first payloads worth keeping.
#[derive(Default)]
struct RawStats {
    counts: BTreeMap<String, usize>,
    samples: Vec<String>,
    ended: bool,
}

/// Reads the raw user-data stream in the background, without interpreting it.
async fn spawn_raw_capture(gateway: &BinanceGateway) -> Result<Arc<Mutex<RawStats>>> {
    use futures_util::StreamExt;
    let listen_key = gateway.create_listen_key().await?;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("wss://stream.binancefuture.com/ws/{listen_key}"))
            .await?;
    let stats = Arc::new(Mutex::new(RawStats::default()));
    let collected = Arc::clone(&stats);
    tokio::spawn(async move {
        while let Some(message) = socket.next().await {
            let Ok(tokio_tungstenite::tungstenite::Message::Text(text)) = message else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(text.as_ref()) else {
                continue;
            };
            let kind = value
                .get("e")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let Ok(mut stats) = collected.lock() else {
                break;
            };
            let seen = stats.counts.entry(kind.clone()).or_insert(0);
            *seen += 1;
            if kind == "ALGO_UPDATE" || (kind == "TRADE_LITE" && *seen == 1) {
                stats.samples.push(value.to_string());
            }
        }
        if let Ok(mut stats) = collected.lock() {
            stats.ended = true;
        }
    });
    Ok(stats)
}

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

fn record(checks: &mut Vec<Check>, name: &'static str, passed: bool, detail: impl Into<String>) {
    let detail = detail.into();
    println!(
        "  [{}] {name}: {detail}",
        if passed { "PASS" } else { "FAIL" }
    );
    checks.push(Check {
        name,
        passed,
        detail,
    });
}

async fn wait_until<F, Fut>(timeout: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if condition().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn position(gateway: &BinanceGateway, symbol: &str) -> Decimal {
    gateway
        .open_position_amount(symbol)
        .await
        .unwrap_or(Decimal::MAX)
}

async fn algo_count(gateway: &BinanceGateway, symbol: &str) -> usize {
    gateway
        .open_algo_order_count(symbol)
        .await
        .unwrap_or(usize::MAX)
}

/// Signs and sends the harness' own `MARKET` entries.
///
/// The gateway's entry preflight (rightly) refuses the testnet account because it holds
/// non-USDT collateral, and the entry is not what is under test here: the exits, the cleanup
/// and the halt all go through the real gateway and `ExitManager`.
struct EntryClient {
    client: reqwest::Client,
    auth: BinanceAuth,
    base_url: String,
}

/// Exit levels derived from the mark price at entry time.
struct ExitLevels {
    stop: Decimal,
    take: Decimal,
}

impl EntryClient {
    async fn market_order(&self, symbol: &str, side: Side, quantity: Decimal) -> Result<u64> {
        let side = match side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };
        let parameters = format!("symbol={symbol}&side={side}&type=MARKET&quantity={quantity}");
        let query = self.auth.sign_query(&parameters, Some(5000));
        let response = self
            .client
            .post(format!("{}/fapi/v1/order?{query}", self.base_url))
            .header("X-MBX-APIKEY", self.auth.api_key())
            .send()
            .await?;
        let status = response.status();
        let body: serde_json::Value = response.json().await?;
        if !status.is_success() {
            bail!("entry rejected by the exchange: {body}");
        }
        body.get("orderId")
            .and_then(serde_json::Value::as_u64)
            .context("entry response has no orderId")
    }
}

/// Opens a ~1,500 USDT market entry and waits for the position to show up.
async fn open_entry(
    gateway: &BinanceGateway,
    entry: &EntryClient,
    symbol: &str,
    side: Side,
) -> Result<ExitLevels> {
    let mark = gateway.futures_mark_price(symbol).await?.round_dp(1);
    let quantity = ((dec!(1500) / mark) * dec!(1000)).floor() / dec!(1000);
    let barrier = dec!(0.03);
    let levels = match side {
        Side::Buy => ExitLevels {
            stop: (mark * (Decimal::ONE - barrier)).round_dp(1),
            take: (mark * (Decimal::ONE + barrier)).round_dp(1),
        },
        Side::Sell => ExitLevels {
            stop: (mark * (Decimal::ONE + barrier)).round_dp(1),
            take: (mark * (Decimal::ONE - barrier)).round_dp(1),
        },
    };
    let order_id = entry.market_order(symbol, side, quantity).await?;
    println!("  entry {side:?} market order {order_id}: qty {quantity} (mark {mark})");
    let filled = wait_until(Duration::from_secs(30), || async {
        position(gateway, symbol).await != Decimal::ZERO
    })
    .await;
    if !filled {
        bail!("entry did not fill within 30 s; run `cleanup`");
    }
    Ok(levels)
}

async fn flatten_all(gateway: &BinanceGateway, symbol: &str) {
    let _ = gateway.close_position_market(symbol).await;
    let _ = gateway.cancel_exit_orders(symbol).await;
    let _ = gateway.close_position_market(symbol).await;
}

async fn is_flat(gateway: &BinanceGateway, symbol: &str) -> bool {
    wait_until(Duration::from_secs(10), || async {
        position(gateway, symbol).await == Decimal::ZERO
    })
    .await
}

async fn no_algo_orders(gateway: &BinanceGateway, symbol: &str) -> bool {
    wait_until(Duration::from_secs(10), || async {
        algo_count(gateway, symbol).await == 0
    })
    .await
}

async fn has_two_algo_orders(gateway: &BinanceGateway, symbol: &str) -> bool {
    wait_until(Duration::from_secs(10), || async {
        algo_count(gateway, symbol).await == 2
    })
    .await
}

async fn scenarios(
    gateway: &BinanceGateway,
    entry: &EntryClient,
    symbol: &str,
) -> Result<Vec<Check>> {
    let mut checks = Vec::new();

    println!("\n[A] Long entry -> exchange-held SL/TP");
    let levels = open_entry(gateway, entry, symbol, Side::Buy).await?;
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(gateway, Side::Buy, levels.stop, levels.take, 1)
        .await;
    record(
        &mut checks,
        "A1 protected",
        event == ExitEvent::Protected,
        format!("{event:?}"),
    );
    record(
        &mut checks,
        "A2 two algo orders pending",
        has_two_algo_orders(gateway, symbol).await,
        format!("count={}", algo_count(gateway, symbol).await),
    );

    println!("\n[B] Time barrier closes at market and leaves nothing behind");
    let event = manager.on_bar_closed(gateway).await;
    record(
        &mut checks,
        "B1 time barrier closed",
        event == Some(ExitEvent::TimeBarrierClosed),
        format!("{event:?}"),
    );
    record(
        &mut checks,
        "B2 flat",
        is_flat(gateway, symbol).await,
        "positionAmt=0",
    );
    let cleanup = manager.on_position_closed_by_exchange(gateway).await;
    record(
        &mut checks,
        "B3 cleanup",
        cleanup == ExitEvent::CleanedUp,
        format!("{cleanup:?}"),
    );
    record(
        &mut checks,
        "B4 no algo orders",
        no_algo_orders(gateway, symbol).await,
        format!("count={}", algo_count(gateway, symbol).await),
    );

    println!("\n[C] Short entry -> exchange closes it -> leftover orders cancelled");
    let levels = open_entry(gateway, entry, symbol, Side::Sell).await?;
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(gateway, Side::Sell, levels.stop, levels.take, 100)
        .await;
    record(
        &mut checks,
        "C1 protected",
        event == ExitEvent::Protected,
        format!("{event:?}"),
    );
    record(
        &mut checks,
        "C2 two algo orders pending",
        has_two_algo_orders(gateway, symbol).await,
        format!("count={}", algo_count(gateway, symbol).await),
    );
    let _ = gateway.close_position_market(symbol).await?;
    record(
        &mut checks,
        "C3 flat",
        is_flat(gateway, symbol).await,
        "positionAmt=0",
    );
    let cleanup = manager.on_position_closed_by_exchange(gateway).await;
    record(
        &mut checks,
        "C4 cleanup",
        cleanup == ExitEvent::CleanedUp,
        format!("{cleanup:?}"),
    );
    record(
        &mut checks,
        "C5 no algo orders",
        no_algo_orders(gateway, symbol).await,
        format!("count={}", algo_count(gateway, symbol).await),
    );

    println!("\n[D] Monthly emergency halt");
    let levels = open_entry(gateway, entry, symbol, Side::Buy).await?;
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(gateway, Side::Buy, levels.stop, levels.take, 100)
        .await;
    record(
        &mut checks,
        "D1 protected",
        event == ExitEvent::Protected,
        format!("{event:?}"),
    );
    let event = manager.halt(gateway).await;
    record(
        &mut checks,
        "D2 halted",
        event == ExitEvent::Halted,
        format!("{event:?}"),
    );
    record(
        &mut checks,
        "D3 halt state",
        manager.halt_state() == HaltState::Halted,
        format!("{:?}", manager.halt_state()),
    );
    record(
        &mut checks,
        "D4 flat",
        is_flat(gateway, symbol).await,
        "positionAmt=0",
    );
    record(
        &mut checks,
        "D5 no algo orders",
        no_algo_orders(gateway, symbol).await,
        format!("count={}", algo_count(gateway, symbol).await),
    );

    println!("\n[E] Protection rejected by the exchange -> position closed, not left naked");
    let levels = open_entry(gateway, entry, symbol, Side::Buy).await?;
    // A long stop ABOVE the mark would trigger immediately: the exchange must reject it.
    let bad_stop = (levels.take * dec!(1.01)).round_dp(1);
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(gateway, Side::Buy, bad_stop, levels.take, 100)
        .await;
    record(
        &mut checks,
        "E1 unprotected entry closed",
        matches!(event, ExitEvent::UnprotectedEntryClosed { .. }),
        format!("{event:?}"),
    );
    record(
        &mut checks,
        "E2 flat",
        is_flat(gateway, symbol).await,
        "positionAmt=0",
    );
    record(
        &mut checks,
        "E3 no algo orders",
        no_algo_orders(gateway, symbol).await,
        format!("count={}", algo_count(gateway, symbol).await),
    );
    Ok(checks)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let symbol = args.symbol.to_ascii_uppercase();
    if !symbol.ends_with("USDT") {
        bail!("only USDT-margined symbols are supported");
    }

    let secrets = GcpSecretManager::from_env();
    let api_key = secrets
        .resolve_secret("BINANCE_API_KEY")
        .await
        .unwrap_or_default();
    let secret_key = secrets
        .resolve_secret("BINANCE_SECRET_KEY")
        .await
        .unwrap_or_default();
    if api_key.is_empty() || secret_key.is_empty() {
        bail!("BINANCE_API_KEY / BINANCE_SECRET_KEY not found in the environment or GCP");
    }

    let entry = EntryClient {
        client: reqwest::Client::new(),
        auth: BinanceAuth::new(api_key.clone(), secret_key.clone()),
        base_url: String::new(),
    };
    let gateway = Arc::new(BinanceGateway::new(BinanceGatewayConfig {
        api_key,
        secret_key,
        testnet: true,
        spot: false,
        recv_window_ms: 5000,
        risk_policy: RiskPolicy::default(),
        risk_per_trade_pct: rust_decimal::Decimal::new(1, 2),
    })?);
    let entry = EntryClient {
        base_url: gateway.base_url().to_string(),
        ..entry
    };
    println!(
        "[*] Binance USD-M TESTNET ({}), symbol {symbol}",
        entry.base_url
    );

    let stats = if matches!(args.command, Command::Scenario | Command::LeaveOpen) {
        Some(spawn_raw_capture(&gateway).await?)
    } else {
        None
    };

    match args.command {
        Command::Inspect => {
            println!("  USDT balance:  {}", gateway.fetch_balance("USDT").await?);
            println!(
                "  positionAmt:   {}",
                gateway.open_position_amount(&symbol).await?
            );
            println!(
                "  algo orders:   {}",
                gateway.open_algo_order_count(&symbol).await?
            );
        }
        Command::CaptureEvents { seconds } => {
            use futures_util::StreamExt;
            let listen_key = gateway.create_listen_key().await?;
            let url = format!("wss://stream.binancefuture.com/ws/{listen_key}");
            let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
            println!("  capturing raw events for {seconds} s ...");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
            while let Ok(Some(message)) = tokio::time::timeout_at(deadline, socket.next()).await {
                let Ok(tokio_tungstenite::tungstenite::Message::Text(text)) = message else {
                    continue;
                };
                let value: serde_json::Value = serde_json::from_str(text.as_ref())?;
                let kind = value.get("e").and_then(|v| v.as_str()).unwrap_or("?");
                if kind == "ALGO_UPDATE" || kind == "ORDER_TRADE_UPDATE" {
                    println!("  {kind}: {value}");
                } else {
                    let keys: Vec<&String> = value
                        .as_object()
                        .map(|o| o.keys().collect())
                        .unwrap_or_default();
                    println!("  {kind}: keys {keys:?}");
                    if kind == "ACCOUNT_UPDATE" {
                        println!("    {value}");
                    }
                }
            }
        }
        Command::VerifyAccount => {
            let report = gateway.verify_account(&symbol, false, false).await?;
            println!("{}", report.render());
            println!("  ready: {}", report.is_ready());
        }
        Command::Cleanup => {
            flatten_all(&gateway, &symbol).await;
            println!(
                "  positionAmt:   {}",
                gateway.open_position_amount(&symbol).await?
            );
            println!(
                "  algo orders:   {}",
                gateway.open_algo_order_count(&symbol).await?
            );
        }
        Command::LeaveOpen => {
            let levels = open_entry(&gateway, &entry, &symbol, Side::Buy).await?;
            let mut manager = ExitManager::new(symbol.clone());
            let event = manager
                .on_entry_filled(gateway.as_ref(), Side::Buy, levels.stop, levels.take, 100)
                .await;
            println!("  protection: {event:?}");
            println!("  algo orders:   {}", algo_count(&gateway, &symbol).await);
            println!("  LEAVING THE POSITION OPEN: process exits now with no cleanup.");
            std::process::exit(0);
        }
        Command::Scenario => {
            let outcome = scenarios(&gateway, &entry, &symbol).await;
            // Whatever happened, never leave the testnet account with an open position.
            flatten_all(&gateway, &symbol).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
            let checks = outcome?;
            let failed: Vec<&Check> = checks.iter().filter(|c| !c.passed).collect();
            if let Some(Ok(stats)) = stats.as_ref().map(|stats| stats.lock()) {
                println!(
                    "\n[stream] raw events {:?}, ended early: {}",
                    stats.counts, stats.ended
                );
                for sample in &stats.samples {
                    println!("[stream] sample: {sample}");
                }
            }
            println!(
                "\n[summary] {} checks, {} failed",
                checks.len(),
                failed.len()
            );
            for check in &failed {
                println!("  FAILED {}: {}", check.name, check.detail);
            }
            if !failed.is_empty() {
                bail!("{} check(s) failed", failed.len());
            }
        }
    }
    Ok(())
}
