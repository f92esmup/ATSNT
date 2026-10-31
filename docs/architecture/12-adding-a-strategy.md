# Adding a Strategy

A new strategy touches three files: its own file or module under `crates/strategies/src/`, one `pub mod` line in
`crates/strategies/src/lib.rs`, and an import plus **one registry line** in
`crates/strategies/src/registry.rs`. No runner (backtest, HPO, paper, live) matches on strategy
names; the test `runners_do_not_dispatch_on_strategy_names` in
`crates/backtest/tests/architecture_contract.rs` fails `cargo test` if one starts to, and it takes
the names to look for from the registry, so a new strategy and its aliases are covered without
editing it.

## 1. Write the strategy (`crates/strategies/src/<name>.rs` or module)

Implement the unified strategy traits from `crates/strategies/src/strategy.rs`:

### `Strategy` trait
- `name(&self) -> &str`: human-readable identifier.
- `compatible_market_types(&self) -> &'static [MarketType]`: e.g. `&[MarketType::UsdMPerpetual]`.
- `input_kind(&self) -> InputKind`: either `InputKind::DollarBars` (default) or `InputKind::Trades`.
- `on_event(&mut self, event: &MarketEvent<'_>) -> Option<Decision>`: handles market events (`MarketEvent::DollarBar` or `MarketEvent::Trade`) and returns an optional `Decision`.
  - `on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent>` is available as a defaulted shortcut for single-stage dollar bar strategies.
- `reset(&mut self)`: restores initial internal state.
- `audit(&self) -> Option<serde_json::Value>`: optional strategy-specific audit metrics (e.g. swarm agent votes, rule activations), defaults to `None`.
- `emits_targets(&self) -> bool`: returns `true` if the strategy produces `Decision::Target`. Defaults to `false`. Strategies emitting targets are currently gated to backtesting engines (`backtest`) and offline-rejected before network connection by `paper_trading` and `live_trading`.

### Decisions
Strategies emit one of two decision types:
- `Decision::Entry(OrderIntent)`: triple-barrier trade entries carrying limit/market execution price, take profit, stop loss, and maximum holding bars.
- `Decision::Target(TargetPosition)`: target-position models specifying signed integer discrete allocation levels (-3..=3), fractional risk, and aggregate stop distance.

### `OptimizableStrategy` trait
To participate in Hyperparameter Optimization (HPO), walk-forward analysis, and artifact serialization, implement `OptimizableStrategy`:
- `id(&self) -> &'static str`: snake_case identifier used in CLI flags and as the `strategy_id` of the run registry.
- `type SharedConfig`: dataset-wide or calibrated shared configuration (`()` for self-contained strategies, or calibrated feed thresholds such as `SwarmConfig`).
- `type Config`: candidate parameters (`Clone + Send + Sync + Serialize + DeserializeOwned + Debug + Display + Default`).
- `type Space`: hyperparameter grid space implementing `HyperparameterSpace`.
- `type Features`: extracted feature type passed to the decision policy (`DollarBar` or custom feature structs).
- `type Extractor`: implements `FeatureExtractor<Features = Self::Features>`.
- `type Policy`: implements `FeaturePolicy<Features = Self::Features>`.

#### Single-Stage vs. Two-Stage Strategies
- **Single-Stage Strategies** (the CUSUM and order-flow strategies, since removed, were of this kind):
  Features are simply the incoming dollar bars (`type Features = DollarBar`, `type Extractor = BarPassThrough`, `type Policy = Self`, `type SharedConfig = ()`).
- **Two-Stage Strategies** (e.g. `SwarmConsensusStrategy`):
  Separates heavy computation (e.g. aggregating 16 multi-timeframe feeds and evaluating 96 ensemble agents) from policy evaluation. In HPO, the extractor runs **exactly once** over the trade tape to generate a feature tape; thousands of candidate policy thresholds are then evaluated at high speed without recomputing features.

A strategy depends only on the `domain` crate and does no I/O: no file system, network, async runtime, or clock. `architecture_contract.rs` enforces this isolation.

## 2. Declare and register it

In `crates/strategies/src/lib.rs`, declare the module (and, by convention, re-export the type):

```rust
pub mod my_strategy;
```

In `crates/strategies/src/registry.rs`, import the type and add it to the macro call:

```rust
use crate::my_strategy::MyStrategy;

register_strategies! {
    SwarmConsensusStrategy => ["swarm"],
    MyStrategy => ["my"],      // <- the one line
}
```

The second element lists optional short aliases. That is all the wiring needed; a `configs/param_space_*.json` file is optional for searching a space other than `default_space`.

## 3. What you get without further code

- `--strategy my_strategy_id` (or the alias) in `backtest` and `run_hpo` (and in `paper_trading`/`live_trading` if `emits_targets() == false`).
- Artifact and bare-config loading (`backtest::load_strategy`).
- The generic contract suite (`crates/strategies/src/contract.rs`), run for every registered strategy by `registry::tests::every_registered_strategy_honours_the_contract`:
  - valid id and buildable default configuration;
  - config JSON round-trip;
  - search space verification (all candidates and neighbours build deterministically);
  - deterministic replay;
  - `reset()` equal to a fresh instance;
  - well-formed decisions on volatile and flat market feeds.

## 4. What stays yours

Strategy-specific unit tests, a sensible `default_space`, and verification across fidelity tiers:
1. Fast bar-level or trade-resolution backtest.
2. Parallel Walk-Forward Optimization via `run_hpo`.
3. High-fidelity verification of winners using `backtest --fill-model tick`.
