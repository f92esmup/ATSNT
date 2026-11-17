//! Generic behavioural contract every strategy must honour.
//!
//! A strategy that implements [`OptimizableStrategy`] and is listed in the
//! [registry](crate::registry) is checked by [`assert_strategy_contract`] automatically, so a new
//! strategy gets the same guarantees as the existing ones without writing these tests again.
//! The suite feeds deterministic synthetic trades and makes no assumption about when a strategy decides: it pins what must hold
//! whenever it does.

use domain::{OrderIntent, Side, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::strategy::{
    FeatureExtractor, HyperparameterSpace, MarketEvent, OptimizableStrategy, Strategy,
};

/// Candidates requested from the search space when checking it.
const CANDIDATE_BUDGET: usize = 25;

/// One synthetic minute: the open, high, low and close the trades below are cut from.
struct Minute {
    start_time: i64,
    end_time: i64,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Decimal,
}

/// Deterministic pseudo-random walk of well-formed minutes (a fixed linear congruential
/// generator, so the series never changes between runs or machines).
fn synthetic_minutes(count: usize) -> Vec<Minute> {
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as i64
    };
    let mut price = dec!(50_000);
    (0..count)
        .map(|index| {
            let open = price;
            let step = Decimal::from(next() % 41 - 20) * dec!(2.5);
            let close = (open + step).max(dec!(1));
            let wick_up = Decimal::from(next() % 15);
            let wick_down = Decimal::from(next() % 15);
            let high = open.max(close) + wick_up;
            let low = (open.min(close) - wick_down).max(dec!(0.5));
            let volume = Decimal::from(5 + next() % 20);
            // The original series drew a buy share here; the draw is kept so the prices do not move.
            let _ = next();
            price = close;
            Minute {
                start_time: 1_700_000_000_000 + index as i64 * 60_000,
                end_time: 1_700_000_000_000 + index as i64 * 60_000 + 59_999,
                open,
                high,
                low,
                close,
                volume,
            }
        })
        .collect()
}

/// Trades that walk [`synthetic_minutes`]: open, high, low and close of each minute, in that
/// order, a quarter of the minute's volume each, inside its time span.
pub fn synthetic_trades(minutes: usize) -> Vec<Trade> {
    synthetic_minutes(minutes)
        .iter()
        .flat_map(|minute| {
            let quarter = minute.volume / dec!(4);
            let step = (minute.end_time - minute.start_time) / 4;
            [minute.open, minute.high, minute.low, minute.close]
                .into_iter()
                .enumerate()
                .map(move |(i, price)| {
                    let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
                    Trade::new(minute.start_time + step * i as i64, price, quarter, side)
                        .expect("synthetic minutes have positive prices and volume")
                })
        })
        .collect()
}

/// Trades with no price movement, one a minute: the degenerate input a quiet market produces,
/// which must never panic or divide by zero.
pub fn flat_trades(count: usize) -> Vec<Trade> {
    (0..count)
        .map(|index| {
            Trade::new(
                1_700_000_000_000 + index as i64 * 60_000 + 59_999,
                dec!(50_000),
                dec!(1),
                Side::Buy,
            )
            .expect("flat trades have positive prices and volume")
        })
        .collect()
}

fn events(trades: &[Trade]) -> Vec<MarketEvent<'_>> {
    trades.iter().map(MarketEvent::Trade).collect()
}

fn replay<T: Strategy>(strategy: &mut T, input: &[Trade]) -> Vec<Option<OrderIntent>> {
    events(input)
        .iter()
        .map(|event| strategy.on_event(event))
        .collect()
}

/// The same input decided in two stages, as the optimizer does: the extractor once, then the
/// strategy's decision stage on every feature it produced.
fn replay_in_two_stages<S: OptimizableStrategy>(
    extractor: &mut S::Extractor,
    strategy: &mut S,
    input: &[Trade],
) -> Vec<OrderIntent> {
    events(input)
        .iter()
        .filter_map(|event| extractor.on_event(event))
        .filter_map(|features| strategy.on_features(&features))
        .collect()
}

fn assert_valid_intent(id: &str, intent: &OrderIntent) {
    assert!(
        intent.price > Decimal::ZERO,
        "{id}: intent price must be positive"
    );
    assert!(
        intent.max_bars_hold >= 1 || intent.deadline_ms.is_some_and(|at| at > intent.timestamp),
        "{id}: the time barrier must hold at least one bar or end after the decision"
    );
    match intent.side {
        Side::Buy => assert!(
            intent.stop_loss < intent.price && intent.take_profit > intent.price,
            "{id}: a long needs stop < price < target, got {intent:?}"
        ),
        Side::Sell => assert!(
            intent.stop_loss > intent.price && intent.take_profit < intent.price,
            "{id}: a short needs target < price < stop, got {intent:?}"
        ),
    }
}

/// Checks everything the engines, the optimizer and the artifact format rely on.
///
/// # Panics
///
/// Panics with the strategy id and the broken rule when the contract is violated.
pub fn assert_strategy_contract<S>()
where
    S: OptimizableStrategy + 'static,
    S::Config: Default,
{
    let id = S::id();

    // Identity.
    assert!(!id.is_empty(), "a strategy needs a canonical id");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
        "{id}: the canonical id must be snake_case ASCII"
    );

    // The default configuration builds and the strategy describes itself.
    let shared = S::SharedConfig::default();
    let default_config = S::Config::default();
    let strategy = S::from_config(&shared, default_config.clone())
        .unwrap_or_else(|error| panic!("{id}: the default configuration must build: {error}"));
    assert!(
        !strategy.name().is_empty(),
        "{id}: the strategy needs a display name"
    );
    assert!(
        !strategy.compatible_market_types().is_empty(),
        "{id}: the strategy must declare at least one compatible market"
    );
    for market in strategy.compatible_market_types() {
        assert!(
            strategy.supports_market_type(*market),
            "{id}: supports_market_type disagrees with compatible_market_types"
        );
    }
    assert!(
        !default_config.to_string().is_empty(),
        "{id}: the configuration needs a Display form for reports"
    );

    // The configuration survives the artifact format.
    let json = serde_json::to_string(&default_config)
        .unwrap_or_else(|error| panic!("{id}: the configuration must serialize: {error}"));
    let restored: S::Config = serde_json::from_str(&json)
        .unwrap_or_else(|error| panic!("{id}: the configuration must deserialize: {error}"));
    S::from_config(&shared, restored)
        .unwrap_or_else(|error| panic!("{id}: a round-tripped configuration must build: {error}"));
    let shared_json = serde_json::to_string(&shared)
        .unwrap_or_else(|error| panic!("{id}: the shared configuration must serialize: {error}"));
    let _: S::SharedConfig = serde_json::from_str(&shared_json)
        .unwrap_or_else(|error| panic!("{id}: the shared configuration must deserialize: {error}"));

    // The search space only proposes buildable configurations, within its budget.
    let space = S::default_space();
    let candidates = space.generate_candidates(CANDIDATE_BUDGET);
    assert!(
        !candidates.is_empty(),
        "{id}: the search space produced no candidate"
    );
    assert!(
        candidates.len() <= CANDIDATE_BUDGET,
        "{id}: the search space ignored its candidate budget ({} > {CANDIDATE_BUDGET})",
        candidates.len()
    );
    for candidate in &candidates {
        S::from_config(&shared, candidate.clone())
            .unwrap_or_else(|error| panic!("{id}: candidate {candidate} must build: {error}"));
    }
    for neighbor in space.perturb_neighbors(&default_config) {
        S::from_config(&shared, neighbor.clone())
            .unwrap_or_else(|error| panic!("{id}: neighbor {neighbor} must build: {error}"));
    }

    // Behaviour on its input: deterministic, resettable and well-formed.
    let input = synthetic_trades(400);
    let build = || {
        S::from_config(&shared, default_config.clone()).expect("the default configuration builds")
    };

    let first = replay(&mut build(), &input);
    let second = replay(&mut build(), &input);
    assert_eq!(
        first, second,
        "{id}: two fresh instances must emit the same decisions"
    );

    let mut reused = build();
    replay(&mut reused, &input);
    reused.reset();
    assert_eq!(
        replay(&mut reused, &input),
        first,
        "{id}: reset() must restore the state of a fresh instance"
    );

    for decision in first.iter().flatten() {
        assert_valid_intent(id, decision);
    }

    // The optimizer decides in two stages; they must agree with the one-stage run.
    let mut extractor = S::extractor(&shared)
        .unwrap_or_else(|error| panic!("{id}: the extractor must build: {error}"));
    let staged = replay_in_two_stages(&mut extractor, &mut build(), &input);
    let direct: Vec<OrderIntent> = first.iter().flatten().cloned().collect();
    assert_eq!(
        staged, direct,
        "{id}: extractor + on_features must decide exactly what on_event decides"
    );

    // Quiet markets must not break it.
    let mut quiet = build();
    for decision in replay(&mut quiet, &flat_trades(200)).iter().flatten() {
        assert_valid_intent(id, decision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthetic_series_is_deterministic_and_well_formed() {
        let a = synthetic_trades(100);

        assert_eq!(a, synthetic_trades(100));
        assert_eq!(a.len(), 400);
        assert!(a.iter().all(|trade| trade.price > Decimal::ZERO));
        assert!(a.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
    }

    use crate::strategy::MarketType;
    use serde::{Deserialize, Serialize};

    /// A small strategy with a switchable defect: `0` is well behaved, `1` forgets to clear its
    /// state on `reset`, `2` emits a long whose stop sits above the entry, `3` decides nothing
    /// in its two-stage form.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct FaultyConfig;

    impl std::fmt::Display for FaultyConfig {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "faulty")
        }
    }

    #[derive(Debug, Clone, Default)]
    struct FaultySpace;

    impl HyperparameterSpace for FaultySpace {
        type Config = FaultyConfig;

        fn generate_candidates(&self, _: usize) -> Vec<FaultyConfig> {
            vec![FaultyConfig]
        }

        fn perturb_neighbors(&self, _: &FaultyConfig) -> Vec<FaultyConfig> {
            Vec::new()
        }
    }

    /// The feature is the trade itself.
    struct TradePassThrough;

    impl FeatureExtractor for TradePassThrough {
        type Features = Trade;

        fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<Trade> {
            let MarketEvent::Trade(trade) = event else {
                return None;
            };
            Some((*trade).clone())
        }

        fn reset(&mut self) {}
    }

    struct Faulty<const MODE: u8> {
        seen: usize,
    }

    impl<const MODE: u8> Strategy for Faulty<MODE> {
        fn name(&self) -> &str {
            "faulty"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent> {
            let MarketEvent::Trade(trade) = event else {
                return None;
            };
            self.seen += 1;
            if self.seen % 7 != 0 {
                return None;
            }
            let stop = if MODE == 2 {
                trade.price + dec!(10)
            } else {
                trade.price - dec!(10)
            };
            OrderIntent::new(
                trade.timestamp,
                Side::Buy,
                trade.price,
                stop,
                trade.price + dec!(10),
                5,
            )
            .ok()
        }

        fn reset(&mut self) {
            if MODE != 1 {
                self.seen = 0;
            }
        }
    }

    impl<const MODE: u8> OptimizableStrategy for Faulty<MODE> {
        type SharedConfig = ();
        type Config = FaultyConfig;
        type Space = FaultySpace;
        type Extractor = TradePassThrough;

        fn id() -> &'static str {
            "faulty"
        }

        fn from_config(_: &(), _: FaultyConfig) -> Result<Self, domain::DomainError> {
            Ok(Self { seen: 0 })
        }

        fn extractor(_: &()) -> Result<Self::Extractor, domain::DomainError> {
            Ok(TradePassThrough)
        }

        fn on_features(&mut self, trade: &Trade) -> Option<OrderIntent> {
            let decision = self.on_event(&MarketEvent::Trade(trade));
            if MODE == 3 {
                None
            } else {
                decision
            }
        }

        fn default_space() -> FaultySpace {
            FaultySpace
        }
    }

    #[test]
    fn a_well_behaved_strategy_passes() {
        assert_strategy_contract::<Faulty<0>>();
    }

    #[test]
    #[should_panic(expected = "reset() must restore")]
    fn a_strategy_that_keeps_state_across_reset_is_rejected() {
        assert_strategy_contract::<Faulty<1>>();
    }

    #[test]
    #[should_panic(expected = "a long needs stop < price < target")]
    fn an_intent_with_the_stop_on_the_wrong_side_is_rejected() {
        assert_strategy_contract::<Faulty<2>>();
    }

    #[test]
    #[should_panic(expected = "extractor + on_features must decide exactly what on_event decides")]
    fn a_strategy_whose_two_stages_disagree_is_rejected() {
        assert_strategy_contract::<Faulty<3>>();
    }
}
