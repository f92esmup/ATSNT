//! Binance USD-M **testnet** harness for the live exit lifecycle (docs/architecture/08, runbook).
//!
//! It drives the real `BinanceGateway` and `ExitManager` with minimal orders. There is
//! deliberately no production option: the gateway is always built with `testnet: true`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adapters::{
    BinanceGateway, BinanceGatewayConfig, BinancePrivateEvent, BinancePrivateUserDataStream,
    GcpSecretManager,
};
use anyhow::{bail, Context, Result};
use backtest::{ExitEvent, ExitManager, HaltState};
use clap::{Parser, Subcommand};
use domain::{OrderIntent, RiskPolicy, Side};
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
}

#[derive(Default)]
struct StreamStats {
    order_updates: usize,
    algo_updates: usize,
    account_updates: usize,
    errors: usize,
    ended: bool,
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

/// Opens a marketable-limit entry with `barrier_pct` wide exits and waits for the fill.
async fn open_entry(
    gateway: &BinanceGateway,
    symbol: &str,
    side: Side,
) -> Result<(OrderIntent, Decimal)> {
    let mark = gateway.futures_mark_price(symbol).await?.round_dp(1);
    let equity = gateway.fetch_balance("USDT").await?;
    let barrier = dec!(0.03);
    let (price, stop, take) = match side {
        Side::Buy => (
            (mark * dec!(1.002)).round_dp(1),
            (mark * (Decimal::ONE - barrier)).round_dp(1),
            (mark * (Decimal::ONE + barrier)).round_dp(1),
        ),
        Side::Sell => (
            (mark * dec!(0.998)).round_dp(1),
            (mark * (Decimal::ONE + barrier)).round_dp(1),
            (mark * (Decimal::ONE - barrier)).round_dp(1),
        ),
    };
    let intent = OrderIntent::new(0, side, price, stop, take, 100)?;
    let report = gateway
        .place_order(symbol, &intent, equity, equity, Decimal::ZERO)
        .await
        .context("entry order was rejected")?;
    println!(
        "  entry {:?} submitted: order {} qty {} @ {} (mark {mark})",
        side, report.order_id, report.original_qty, report.price
    );
    let filled = wait_until(Duration::from_secs(30), || async {
        position(gateway, symbol).await != Decimal::ZERO
    })
    .await;
    if !filled {
        bail!("entry did not fill within 30 s; run `cleanup`");
    }
    Ok((intent, mark))
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

async fn scenarios(gateway: &BinanceGateway, symbol: &str) -> Result<Vec<Check>> {
    let mut checks = Vec::new();

    println!("\n[A] Long entry -> exchange-held SL/TP");
    let (intent, _) = open_entry(gateway, symbol, Side::Buy).await?;
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(gateway, Side::Buy, intent.stop_loss, intent.take_profit, 1)
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
    let (intent, _) = open_entry(gateway, symbol, Side::Sell).await?;
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(
            gateway,
            Side::Sell,
            intent.stop_loss,
            intent.take_profit,
            100,
        )
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
    let (intent, _) = open_entry(gateway, symbol, Side::Buy).await?;
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(
            gateway,
            Side::Buy,
            intent.stop_loss,
            intent.take_profit,
            100,
        )
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
    let mark = gateway.futures_mark_price(symbol).await?.round_dp(1);
    // A long stop ABOVE the mark would trigger immediately: the exchange must reject it.
    let bad_stop = (mark * dec!(1.02)).round_dp(1);
    let (intent, _) = open_entry(gateway, symbol, Side::Buy).await?;
    let mut manager = ExitManager::new(symbol);
    let event = manager
        .on_entry_filled(gateway, Side::Buy, bad_stop, intent.take_profit, 100)
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

    let gateway = Arc::new(BinanceGateway::new(BinanceGatewayConfig {
        api_key,
        secret_key,
        testnet: true,
        spot: false,
        recv_window_ms: 5000,
        risk_policy: RiskPolicy::default(),
    })?);
    println!("[*] Binance USD-M TESTNET, symbol {symbol}");

    let stats = Arc::new(Mutex::new(StreamStats::default()));
    if matches!(args.command, Command::Scenario | Command::LeaveOpen) {
        let listen_key = gateway.create_listen_key().await?;
        let mut stream =
            BinancePrivateUserDataStream::connect_futures(listen_key, true, gateway.clone())
                .await?;
        let collected = Arc::clone(&stats);
        tokio::spawn(async move {
            while let Some(event) = stream.next_event().await {
                let Ok(mut stats) = collected.lock() else {
                    break;
                };
                match event {
                    Ok(BinancePrivateEvent::FuturesOrderTradeUpdate(_)) => stats.order_updates += 1,
                    Ok(BinancePrivateEvent::FuturesAlgoUpdate { .. }) => stats.algo_updates += 1,
                    Ok(BinancePrivateEvent::FuturesAccountUpdate(_)) => stats.account_updates += 1,
                    Ok(_) => {}
                    Err(_) => stats.errors += 1,
                }
            }
            if let Ok(mut stats) = collected.lock() {
                stats.ended = true;
            }
        });
    }

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
            let (intent, _) = open_entry(&gateway, &symbol, Side::Buy).await?;
            let mut manager = ExitManager::new(symbol.clone());
            let event = manager
                .on_entry_filled(
                    gateway.as_ref(),
                    Side::Buy,
                    intent.stop_loss,
                    intent.take_profit,
                    100,
                )
                .await;
            println!("  protection: {event:?}");
            println!("  algo orders:   {}", algo_count(&gateway, &symbol).await);
            println!("  LEAVING THE POSITION OPEN: process exits now with no cleanup.");
            std::process::exit(0);
        }
        Command::Scenario => {
            let outcome = scenarios(&gateway, &symbol).await;
            // Whatever happened, never leave the testnet account with an open position.
            flatten_all(&gateway, &symbol).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
            let checks = outcome?;
            let failed: Vec<&Check> = checks.iter().filter(|c| !c.passed).collect();
            if let Ok(stats) = stats.lock() {
                println!(
                    "\n[stream] order updates {}, algo updates {}, account updates {}, parse errors {}, ended early: {}",
                    stats.order_updates,
                    stats.algo_updates,
                    stats.account_updates,
                    stats.errors,
                    stats.ended
                );
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
