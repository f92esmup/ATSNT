//! Generic behavioural contract every strategy must honour.
//!
//! A strategy that implements [`OptimizableStrategy`] and is listed in the
//! [registry](crate::registry) is checked by [`assert_strategy_contract`] automatically, so a new
//! strategy gets the same guarantees as the existing ones without writing these tests again.
//! The suite feeds deterministic synthetic bars or trades (whichever the strategy declares it
//! consumes) and makes no assumption about when a strategy decides: it pins what must hold
//! whenever it does.

use domain::{DollarBar, OrderIntent, Side, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::strategy::{
    Decision, FeatureExtractor, HyperparameterSpace, InputKind, MarketEvent, OptimizableStrategy,
    Strategy,
};

/// Candidates requested from the search space when checking it.
const CANDIDATE_BUDGET: usize = 25;

/// Deterministic pseudo-random walk of well-formed dollar bars (a fixed linear congruential
/// generator, so the series never changes between runs or machines).
pub fn synthetic_bars(count: usize) -> Vec<DollarBar> {
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
            let buy_share = Decimal::from(next() % 100) / dec!(100);
            let buy_volume = volume * buy_share;
            let sell_volume = volume - buy_volume;
            let dollar_volume = volume * close;
            price = close;
            DollarBar {
                start_time: 1_700_000_000_000 + index as i64 * 60_000,
                end_time: 1_700_000_000_000 + index as i64 * 60_000 + 59_999,
                open,
                high,
                low,
                close,
                volume,
                dollar_volume,
                trade_count: 50 + (index as u64 % 50),
                buy_volume,
                sell_volume,
                buy_dollar_volume: buy_volume * close,
                sell_dollar_volume: sell_volume * close,
            }
        })
        .collect()
}

/// Bars with no range, no price movement and a single trade: the degenerate input a quiet market
/// produces, which must never panic or divide by zero.
pub fn flat_bars(count: usize) -> Vec<DollarBar> {
    (0..count)
        .map(|index| DollarBar {
            start_time: 1_700_000_000_000 + index as i64 * 60_000,
            end_time: 1_700_000_000_000 + index as i64 * 60_000 + 59_999,
            open: dec!(50_000),
            high: dec!(50_000),
            low: dec!(50_000),
            close: dec!(50_000),
            volume: dec!(1),
            dollar_volume: dec!(50_000),
            trade_count: 1,
            buy_volume: dec!(0.5),
            sell_volume: dec!(0.5),
            buy_dollar_volume: dec!(25_000),
            sell_dollar_volume: dec!(25_000),
        })
        .collect()
}

/// Trades that rebuild [`synthetic_bars`]: open, high, low and close of each bar, in that
/// order, a quarter of the bar's volume each, inside the bar's time span.
pub fn synthetic_trades(bar_count: usize) -> Vec<Trade> {
    synthetic_bars(bar_count)
        .iter()
        .flat_map(|bar| {
            let quarter = bar.volume / dec!(4);
            let step = (bar.end_time - bar.start_time) / 4;
            [bar.open, bar.high, bar.low, bar.close]
                .into_iter()
                .enumerate()
                .map(move |(i, price)| {
                    let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
                    Trade::new(bar.start_time + step * i as i64, price, quarter, side)
                        .expect("synthetic bars have positive prices and volume")
                })
        })
        .collect()
}

/// The synthetic input of the kind a strategy consumes.
enum Input {
    Bars(Vec<DollarBar>),
    Trades(Vec<Trade>),
}

impl Input {
    fn synthetic(kind: InputKind, bar_count: usize) -> Self {
        match kind {
            InputKind::DollarBars => Input::Bars(synthetic_bars(bar_count)),
            InputKind::Trades => Input::Trades(synthetic_trades(bar_count)),
        }
    }

    fn flat(kind: InputKind, bar_count: usize) -> Self {
        let bars = flat_bars(bar_count);
        match kind {
            InputKind::DollarBars => Input::Bars(bars),
            InputKind::Trades => Input::Trades(
                bars.iter()
                    .map(|bar| {
                        Trade::new(bar.end_time, bar.close, bar.volume, Side::Buy)
                            .expect("flat bars have positive prices and volume")
                    })
                    .collect(),
            ),
        }
    }

    fn events(&self) -> Vec<MarketEvent<'_>> {
        match self {
            Input::Bars(bars) => bars.iter().map(MarketEvent::DollarBar).collect(),
            Input::Trades(trades) => trades.iter().map(MarketEvent::Trade).collect(),
        }
    }
}

fn replay<T: Strategy>(strategy: &mut T, input: &Input) -> Vec<Option<Decision>> {
    input
        .events()
        .iter()
        .map(|event| strategy.on_event(event))
        .collect()
}

/// The same input decided in two stages, as the optimizer does: the extractor once, then the
/// strategy's decision stage on every feature it produced.
fn replay_in_two_stages<S: OptimizableStrategy>(
    extractor: &mut S::Extractor,
    strategy: &mut S,
    input: &Input,
) -> Vec<Decision> {
    input
        .events()
        .iter()
        .filter_map(|event| extractor.on_event(event))
        .filter_map(|features| strategy.on_features(&features))
        .collect()
}

fn assert_valid_decision(id: &str, decision: &Decision) {
    match decision {
        Decision::Entry(intent) => assert_valid_intent(id, intent),
        // TargetPosition::new already enforces the level, risk and stop invariants; the side
        // must agree with the level.
        Decision::Target(target) => assert_eq!(
            target.side().is_some(),
            target.level() != 0,
            "{id}: a target's side must follow its level"
        ),
    }
}

fn assert_valid_intent(id: &str, intent: &OrderIntent) {
    assert!(
        intent.price > Decimal::ZERO,
        "{id}: intent price must be positive"
    );
    assert!(
        intent.max_bars_hold >= 1,
        "{id}: the time barrier must hold at least one bar"
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
    let kind = strategy.input_kind();
    let input = Input::synthetic(kind, 400);
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
        assert_valid_decision(id, decision);
    }

    // The optimizer decides in two stages; they must agree with the one-stage run.
    let mut extractor = S::extractor(&shared)
        .unwrap_or_else(|error| panic!("{id}: the extractor must build: {error}"));
    let staged = replay_in_two_stages(&mut extractor, &mut build(), &input);
    let direct: Vec<Decision> = first.iter().flatten().cloned().collect();
    assert_eq!(
        staged, direct,
        "{id}: extractor + on_features must decide exactly what on_event decides"
    );

    // Quiet markets must not break it.
    let mut quiet = build();
    for decision in replay(&mut quiet, &Input::flat(kind, 200)).iter().flatten() {
        assert_valid_decision(id, decision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthetic_series_is_deterministic_and_well_formed() {
        let a = synthetic_bars(100);
        let b = synthetic_bars(100);

        assert_eq!(a, b);
        for bar in &a {
            assert!(bar.high >= bar.open.max(bar.close));
            assert!(bar.low <= bar.open.min(bar.close));
            assert!(bar.low > Decimal::ZERO);
            assert_eq!(bar.buy_volume + bar.sell_volume, bar.volume);
        }
        assert!(a.windows(2).all(|w| w[0].end_time < w[1].start_time));
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

        fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
            self.seen += 1;
            if self.seen % 7 != 0 {
                return None;
            }
            let stop = if MODE == 2 {
                bar.close + dec!(10)
            } else {
                bar.close - dec!(10)
            };
            OrderIntent::new(
                bar.end_time,
                Side::Buy,
                bar.close,
                stop,
                bar.close + dec!(10),
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
        type Extractor = crate::strategy::BarPassThrough;

        fn id() -> &'static str {
            "faulty"
        }

        fn from_config(_: &(), _: FaultyConfig) -> Result<Self, domain::DomainError> {
            Ok(Self { seen: 0 })
        }

        fn extractor(_: &()) -> Result<Self::Extractor, domain::DomainError> {
            Ok(crate::strategy::BarPassThrough)
        }

        fn on_features(&mut self, bar: &DollarBar) -> Option<Decision> {
            let decision = self.on_bar(bar).map(Decision::Entry);
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

    #[test]
    fn the_synthetic_trades_rebuild_the_synthetic_bars() {
        let bars = synthetic_bars(50);
        let trades = synthetic_trades(50);
        assert_eq!(trades.len(), 200);
        for (bar, chunk) in bars.iter().zip(trades.chunks(4)) {
            let prices: Vec<_> = chunk.iter().map(|t| t.price).collect();
            assert_eq!(prices, vec![bar.open, bar.high, bar.low, bar.close]);
        }
        assert!(trades.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
    }
}
