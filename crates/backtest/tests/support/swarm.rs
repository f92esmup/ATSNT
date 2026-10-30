//! The swarm's deterministic tape and settings, shared by the swarm test binaries.
//!
//! The tape is a seeded random walk of eight bursts a day, with a trend that flips every ten
//! days. It spans more than the swarm's 70-day warm-up when asked for 90 days.

use backtest::BacktestConfig;
use domain::{Side, Trade};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{PolicyConfig, SwarmConfig};

use super::TAPE_START_MS;

pub const DAY_MS: i64 = 86_400_000;
/// 2026-01-01T00:00:00Z.
pub const START_MS: i64 = TAPE_START_MS;

/// A seeded random walk: eight bursts of three trades a day for `days` days, the bursts ten
/// minutes apart so the fast time feeds differ, with a trend that flips every ten days.
pub fn swarm_trades(days: i64) -> Vec<Trade> {
    let mut rng = StdRng::seed_from_u64(11);
    let mut price = dec!(40000);
    let mut trades = Vec::new();
    for day in 0..days {
        let drift = if (day / 10) % 2 == 0 { 8 } else { -8 };
        for k in 0..8 {
            for burst in 0..3 {
                let timestamp = START_MS
                    + day * DAY_MS
                    + k * (DAY_MS / 8)
                    + burst * 600_000
                    + rng.gen_range(0..1000);
                let step = Decimal::new(drift + rng.gen_range(-60..=60), 4);
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

/// Small thresholds so every feed warms within the tape, calibrated before it starts.
pub fn shared() -> SwarmConfig {
    SwarmConfig {
        tick_thresholds: [1, 2, 3, 4],
        volume_thresholds: [dec!(1), dec!(2), dec!(3), dec!(4)],
        range_thresholds: [dec!(0.001), dec!(0.002), dec!(0.004), dec!(0.008)],
        calibration_end_ms: START_MS - 1,
        ..SwarmConfig::default()
    }
}

/// A policy that trades often on a 96-agent swarm.
pub fn busy_policy() -> PolicyConfig {
    PolicyConfig {
        threshold_base: dec!(0.05),
        threshold_step: dec!(0.05),
        hysteresis: dec!(0.02),
        ..PolicyConfig::default()
    }
}

/// The account of the swarm tests: default rules with 100 ms of order latency.
pub fn account() -> BacktestConfig {
    BacktestConfig {
        latency_ms: 100,
        ..BacktestConfig::default()
    }
}
