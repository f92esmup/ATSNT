//! The `parity` module of the application core, run on the simulated venue the wiring builds.

use backtest::parity::*;
use backtest::wiring::parity_venue;
use domain::{Side, Trade};
use ports::FillLog;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn print(timestamp: i64, price: Decimal, quantity: Decimal, side: Side) -> Trade {
    Trade::new(timestamp, price, quantity, side).unwrap()
}

/// A tape whose offers climb 0.1 every 50 ms, so every extra millisecond of latency costs
/// a buyer money.
fn rising_tape() -> Vec<Trade> {
    (0..40)
        .map(|i| {
            print(
                1_000 + i * 50,
                dec!(100) + Decimal::from(i) / dec!(10),
                dec!(1),
                Side::Buy,
            )
        })
        .collect()
}

fn buy(submitted_ms: i64, filled_ms: i64, quantity: Decimal, avg_price: Decimal) -> ObservedFill {
    ObservedFill {
        submitted_ms,
        filled_ms,
        side: Side::Buy,
        quantity,
        avg_price,
    }
}

#[test]
fn an_order_fills_on_the_first_print_after_its_latency() {
    let order = buy(1_000, 1_100, dec!(1), dec!(100.2));

    let sim = simulate_market_order(&parity_venue(100), &rising_tape(), &order).unwrap();

    // 100 ms after 1_000 is the print at 1_100, priced 100.2.
    assert_eq!(sim.avg_price, dec!(100.2));
    assert_eq!(sim.filled_ms, 1_100);
    assert_eq!(sim.quantity, dec!(1));
}

#[test]
fn a_large_order_walks_several_prints_at_their_volume_weighted_price() {
    let order = buy(1_000, 1_100, dec!(2), dec!(100.25));

    let sim = simulate_market_order(&parity_venue(100), &rising_tape(), &order).unwrap();

    // Prints at 1_100 and 1_150: (100.2 + 100.3) / 2.
    assert_eq!(sim.avg_price, dec!(100.25));
    assert_eq!(sim.filled_ms, 1_150);
}

#[test]
fn an_order_the_tape_cannot_fill_has_no_simulated_fill() {
    let order = buy(1_000, 1_100, dec!(1000), dec!(100));

    assert!(simulate_market_order(&parity_venue(0), &rising_tape(), &order).is_none());
}

#[test]
fn the_adverse_error_is_positive_when_the_simulation_is_worse_than_live() {
    // Live bought at 100.0; the simulation paid 100.2 (20 bps worse).
    let order = buy(1_000, 1_000, dec!(1), dec!(100));

    let comparison = compare_fills(&rising_tape(), &[order], 100, parity_venue);

    assert_eq!(
        comparison[0].adverse_error_bps.unwrap().round_dp(2),
        dec!(20)
    );
    assert_eq!(comparison[0].time_error_ms, Some(100));
}

#[test]
fn a_sell_is_pessimistic_when_the_simulation_sold_lower() {
    let tape: Vec<Trade> = (0..10)
        .map(|i| {
            print(
                1_000 + i * 50,
                dec!(100) - Decimal::from(i) / dec!(10),
                dec!(1),
                Side::Sell,
            )
        })
        .collect();
    let order = ObservedFill {
        submitted_ms: 1_000,
        filled_ms: 1_000,
        side: Side::Sell,
        quantity: dec!(1),
        avg_price: dec!(100),
    };

    let comparison = compare_fills(&tape, &[order], 100, parity_venue);

    // Sold at 99.8 against a live 100.0.
    assert_eq!(
        comparison[0].adverse_error_bps.unwrap().round_dp(2),
        dec!(20)
    );
}

#[test]
fn the_sweep_prefers_the_latency_that_reproduces_the_live_prices() {
    // Live fills were generated with a true latency of 150 ms.
    let tape = rising_tape();
    let fills = vec![
        buy(1_000, 1_150, dec!(1), dec!(100.3)),
        buy(1_500, 1_650, dec!(1), dec!(101.3)),
    ];

    let scores = sweep_latency(&tape, &fills, [0, 50, 100, 150, 200, 300], parity_venue);
    let best = best_latency(&scores).unwrap();

    assert_eq!(best.latency_ms, 150);
    assert_eq!(best.mean_abs_error_bps, Decimal::ZERO);
    assert_eq!(best.matched, 2);
    let zero = scores.iter().find(|s| s.latency_ms == 0).unwrap();
    assert!(
        zero.mean_bias_bps < Decimal::ZERO,
        "too little latency is optimistic"
    );
}

#[test]
fn nothing_is_recommended_when_no_fill_could_be_simulated() {
    let fills = vec![buy(1_000, 1_100, dec!(1000), dec!(100))];

    let scores = sweep_latency(&rising_tape(), &fills, [0, 100], parity_venue);

    assert!(best_latency(&scores).is_none());
}

#[test]
fn observed_fills_round_trip_through_json_lines() {
    let fill = buy(1, 2, dec!(0.5), dec!(100.25));

    let json = serde_json::to_string(&fill).unwrap();

    assert_eq!(serde_json::from_str::<ObservedFill>(&json).unwrap(), fill);
}

#[test]
fn jsonl_fills_parse_and_skip_blank_lines() {
    let a = buy(1, 2, dec!(0.5), dec!(100));
    let b = buy(3, 4, dec!(0.25), dec!(101));
    let text = format!(
        "{}\n\n{}\n",
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );

    assert_eq!(parse_fills_jsonl(&text).unwrap(), vec![a, b]);
    assert!(parse_fills_jsonl("not json").is_err());
}

#[test]
fn the_tape_window_surrounds_every_fill() {
    let fills = vec![
        buy(5_000, 5_100, dec!(1), dec!(1)),
        buy(9_000, 9_400, dec!(1), dec!(1)),
    ];

    assert_eq!(tape_window(&fills), Some((4_000, 69_400)));
    assert_eq!(tape_window(&[]), None);
}

/// A fill log kept in memory, one record per line.
#[derive(Default)]
struct MemoryLog(std::sync::Mutex<String>);

impl MemoryLog {
    fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

impl FillLog for &MemoryLog {
    type Error = String;

    fn append(&self, record: &str) -> Result<(), String> {
        let mut text = self.0.lock().unwrap();
        text.push_str(record);
        text.push('\n');
        Ok(())
    }
}

#[test]
fn a_discarded_submission_is_never_paired_with_a_later_fill() {
    let log = MemoryLog::default();
    let recorder = FillRecorder::new(&log);
    recorder.on_entry_submitted(1_000);

    // The next order's send time is unknown: nothing may be recorded for it.
    recorder.discard_pending();
    let fill = recorder
        .on_entry_filled(Side::Buy, dec!(1), dec!(100), 1_200)
        .unwrap();

    assert_eq!(fill, None);
    assert!(log.text().is_empty());
}

#[test]
fn the_recorder_pairs_a_submission_with_its_fill_and_appends_json_lines() {
    let log = MemoryLog::default();
    let recorder = FillRecorder::new(&log);

    // A fill with no noted submission records nothing.
    assert_eq!(
        recorder
            .on_entry_filled(Side::Buy, dec!(1), dec!(100), 5)
            .unwrap(),
        None
    );
    recorder.on_entry_submitted(1_000);
    let first = recorder
        .on_entry_filled(Side::Buy, dec!(0.5), dec!(100.25), 1_120)
        .unwrap()
        .unwrap();
    recorder.on_entry_submitted(2_000);
    recorder
        .on_entry_filled(Side::Sell, dec!(0.25), dec!(99.5), 2_090)
        .unwrap();

    let fills = parse_fills_jsonl(&log.text()).unwrap();
    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0], first);
    assert_eq!(fills[0].submitted_ms, 1_000);
    assert_eq!(fills[1].side, Side::Sell);
}
