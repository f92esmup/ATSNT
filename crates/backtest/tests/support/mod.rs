//! Deterministic synthetic market data shared by the integration tests.
//!
//! The tape is a seeded random walk that cycles through calm, trending, bursting, crashing and
//! recovering regimes, so the strategies under test reach every exit path the execution layer
//! has. Prices, quantities and timestamps come only from the seed: the same seed always yields
//! the same tape, byte for byte.

#![allow(dead_code)]

pub mod swarm;

use domain::{DollarBar, DollarBarAggregator, Side, Trade};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// 2026-01-01T00:00:00Z, the first timestamp of every synthetic tape.
pub const TAPE_START_MS: i64 = 1_767_225_600_000;

/// One regime of the synthetic tape: drift and spread of the per-trade move in basis points,
/// and the probability (in percent) that a trade is an aggressive buy.
#[derive(Debug, Clone, Copy)]
struct Regime {
    trades: usize,
    drift_bp: i64,
    spread_bp: i64,
    buy_pct: u32,
}

/// The regimes, replayed in order and then from the start again.
const REGIMES: [Regime; 7] = [
    Regime {
        trades: 6_000,
        drift_bp: 0,
        spread_bp: 2,
        buy_pct: 50,
    },
    Regime {
        trades: 4_000,
        drift_bp: 1,
        spread_bp: 3,
        buy_pct: 62,
    },
    Regime {
        trades: 3_000,
        drift_bp: 0,
        spread_bp: 9,
        buy_pct: 50,
    },
    Regime {
        trades: 4_000,
        drift_bp: -1,
        spread_bp: 3,
        buy_pct: 38,
    },
    Regime {
        trades: 1_500,
        drift_bp: -6,
        spread_bp: 6,
        buy_pct: 20,
    },
    Regime {
        trades: 3_000,
        drift_bp: 2,
        spread_bp: 5,
        buy_pct: 70,
    },
    Regime {
        trades: 5_000,
        drift_bp: 0,
        spread_bp: 4,
        buy_pct: 50,
    },
];

/// `n` aggregated trades of a BTCUSDT-like market starting at 40,000 USDT.
pub fn synthetic_trades(seed: u64, n: usize) -> Vec<Trade> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut price = dec!(40000);
    let mut timestamp = TAPE_START_MS;
    let mut trades = Vec::with_capacity(n);
    let mut regime_idx = 0;
    let mut left_in_regime = REGIMES[0].trades;

    while trades.len() < n {
        if left_in_regime == 0 {
            regime_idx = (regime_idx + 1) % REGIMES.len();
            left_in_regime = REGIMES[regime_idx].trades;
        }
        let regime = REGIMES[regime_idx];
        left_in_regime -= 1;

        let move_bp = regime.drift_bp + rng.gen_range(-regime.spread_bp..=regime.spread_bp);
        price = (price + price * Decimal::from(move_bp) / dec!(10000)).round_dp(1);
        timestamp += rng.gen_range(200..=3_000);
        let side = if rng.gen_range(0..100) < regime.buy_pct {
            Side::Buy
        } else {
            Side::Sell
        };
        let quantity = Decimal::from(rng.gen_range(1..=2_000_u32)) / dec!(1000);
        trades.push(Trade::new(timestamp, price, quantity, side).expect("valid synthetic trade"));
    }
    trades
}

/// Dollar bars of `trades` at `threshold`, built with the production aggregator.
pub fn dollar_bars(trades: &[Trade], threshold: Decimal) -> Vec<DollarBar> {
    let mut aggregator = DollarBarAggregator::new(threshold).expect("positive threshold");
    trades
        .iter()
        .filter_map(|trade| aggregator.process_trade(trade))
        .collect()
}
