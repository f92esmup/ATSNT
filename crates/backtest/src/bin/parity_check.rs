//! Compares the market orders a live run sent with the tick-level simulation of the same orders
//! on the tape of that market, and recommends the order latency that reproduces the real fills.
//!
//! ```text
//! parity_check --tape data/BTCUSDT-aggTrades-2026-10-17.parquet --fills fills.jsonl
//! ```

// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use adapters::{AdapterError, BinanceCsvReader, MarketDataStream, TapeReader};
use anyhow::{Context, Result};
use backtest::parity::{
    best_latency, compare_fills, parse_fills_jsonl, sweep_latency, tape_window,
};
use clap::Parser;
use domain::Trade;
use rust_decimal::Decimal;

#[derive(Parser, Debug)]
#[command(
    name = "parity_check",
    about = "ATSNT - measure how far simulated market fills sit from the fills a live run got"
)]
struct Args {
    /// Tape of the market the live run traded (.csv, .parquet or a directory of parquet files)
    #[arg(long)]
    tape: PathBuf,

    /// Fills recorded by `live_trading --record-fills` (JSON lines)
    #[arg(long)]
    fills: PathBuf,

    /// Order latencies to try, in milliseconds
    #[arg(
        long,
        value_delimiter = ',',
        default_values_t = [
            0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 120, 140, 160, 180, 200, 250, 300, 400, 500
        ]
    )]
    latencies: Vec<i64>,
}

fn open_tape(path: &PathBuf) -> Result<Box<dyn MarketDataStream<Error = AdapterError>>> {
    if path.is_dir() || path.extension().and_then(|s| s.to_str()) == Some("parquet") {
        Ok(Box::new(TapeReader::open(path)?))
    } else {
        let file =
            File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
        Ok(Box::new(BinanceCsvReader::new(BufReader::new(file))))
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let text = std::fs::read_to_string(&args.fills)
        .with_context(|| format!("Failed to read {}", args.fills.display()))?;
    let fills = parse_fills_jsonl(&text).context("Invalid fills file")?;
    let Some((from_ms, to_ms)) = tape_window(&fills) else {
        anyhow::bail!("{} holds no fills", args.fills.display());
    };

    println!(
        "[*] {} observed fills; loading the tape {from_ms}..{to_ms}",
        fills.len()
    );
    let mut stream = open_tape(&args.tape)?;
    let mut tape: Vec<Trade> = Vec::new();
    while let Some(trade) = stream.next_trade()? {
        if trade.timestamp > to_ms {
            break;
        }
        if trade.timestamp >= from_ms {
            tape.push(trade);
        }
    }
    println!("[*] {} tape trades in the window\n", tape.len());
    if tape.is_empty() {
        anyhow::bail!("the tape has no trades in the fills' time window");
    }

    println!(" Latency | Matched | Mean |error| (bps) | Mean bias (bps)");
    println!("---------+---------+-------------------+----------------");
    let scores = sweep_latency(
        &tape,
        &fills,
        args.latencies.iter().copied(),
        backtest::wiring::parity_venue,
    );
    for score in &scores {
        println!(
            " {:>5} ms | {:>3}/{:<3} | {:>17.2} | {:>+14.2}",
            score.latency_ms,
            score.matched,
            fills.len(),
            score.mean_abs_error_bps,
            score.mean_bias_bps
        );
    }

    match best_latency(&scores) {
        Some(best) => {
            println!(
                "\n[*] Best fit: {} ms (mean |error| {:.2} bps over {} fills)",
                best.latency_ms, best.mean_abs_error_bps, best.matched
            );
            println!("    Use it as --latency-ms for --fill-model tick and paper trading.");
            println!("    A positive bias means the simulation is pessimistic, negative optimistic.");
            println!("\n Per-fill detail at the best latency:");
            for c in compare_fills(&tape, &fills, best.latency_ms, backtest::wiring::parity_venue) {
                let show = |v: Option<Decimal>| v.map_or("n/a".to_string(), |v| format!("{v:.2}"));
                println!(
                    "  {:?} {} @ {} live | sim {} | error {} bps | time {} ms",
                    c.observed.side,
                    c.observed.quantity,
                    c.observed.avg_price,
                    c.simulated
                        .as_ref()
                        .map_or("n/a".to_string(), |s| s.avg_price.to_string()),
                    show(c.adverse_error_bps),
                    c.time_error_ms.map_or("n/a".to_string(), |t| t.to_string()),
                );
            }
        }
        None => println!("\n[!] No fill could be simulated on this tape; check that it covers the same market and day."),
    }
    Ok(())
}
