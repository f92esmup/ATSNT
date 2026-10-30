//! Generates minimal synthetic parquet trade files for end-to-end pipeline testing.
//!
//! Produces:
//! 1. `data/test/calibration_trades.parquet`: 8 days (2025-12-24 to 2026-01-01) for calibration & pruning.
//! 2. `data/test/evaluation_trades.parquet`: 90 days (2026-01-01 to 2026-04-01) for HPO & Backtest.

use std::path::Path;

use adapters::TapeWriter;
use anyhow::{Context, Result};
use domain::{Side, Trade};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const DAY_MS: i64 = 86_400_000;
/// 2025-12-24T00:00:00Z
const CALIBRATION_START_MS: i64 = 1_766_534_400_000;
/// 2026-01-01T00:00:00Z
const EVALUATION_START_MS: i64 = 1_767_225_600_000;

fn generate_trades(start_ms: i64, days: i64, seed: u64) -> Vec<Trade> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut price = dec!(40000);
    let mut trades = Vec::new();
    for day in 0..days {
        let drift = if (day / 5) % 2 == 0 { 12 } else { -12 };
        for k in 0..8 {
            for burst in 0..3 {
                let timestamp = start_ms
                    + day * DAY_MS
                    + k * (DAY_MS / 8)
                    + burst * 600_000
                    + rng.gen_range(0..1000);
                let step = Decimal::new(drift + rng.gen_range(-50..=50), 4);
                price = (price * (Decimal::ONE + step)).round_dp(2);
                let quantity = Decimal::new(rng.gen_range(50..150), 2);
                let side = if rng.gen_bool(0.5) {
                    Side::Buy
                } else {
                    Side::Sell
                };
                trades.push(Trade::new(timestamp, price, quantity, side).expect("valid trade"));
            }
        }
    }
    trades
}

fn write_parquet(path: &Path, trades: &[Trade]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = TapeWriter::new(path).context("failed to create writer")?;
    for trade in trades {
        writer.write_trade(trade).context("failed to write trade")?;
    }
    writer.finish().context("failed to finish parquet")?;
    Ok(())
}

fn main() -> Result<()> {
    let cal_path = Path::new("data/test/calibration_trades.parquet");
    let eval_path = Path::new("data/test/evaluation_trades.parquet");

    println!("Generating calibration trades (8 days)...");
    let cal_trades = generate_trades(CALIBRATION_START_MS, 8, 42);
    write_parquet(cal_path, &cal_trades)?;
    println!(
        "Written {} calibration trades to {}",
        cal_trades.len(),
        cal_path.display()
    );

    println!("Generating evaluation trades (90 days)...");
    let eval_trades = generate_trades(EVALUATION_START_MS, 90, 123);
    write_parquet(eval_path, &eval_trades)?;
    println!(
        "Written {} evaluation trades to {}",
        eval_trades.len(),
        eval_path.display()
    );

    Ok(())
}
