# Adding a Strategy

A strategy is one module, one `pub mod` line and one registry line. Nothing else changes. No runner
matches on strategy names: the test `runners_do_not_dispatch_on_strategy_names` in
`crates/backtest/tests/architecture_contract.rs` fails `cargo test` if one starts to. It reads the
ids and aliases from `strategies::registry::strategy_names()`, so a new strategy is covered without
editing the test.

Each strategy is one folder under `crates/strategies/src/`, declared in
`crates/strategies/src/lib.rs`. A strategy says which markets it trades; it is not filed by market.

## 1. Write the strategy

The contract is in `crates/strategies/src/strategy.rs`. Two traits matter.

### `Strategy`: how it reacts to the market

- `name(&self) -> &str`: a human-readable name.
- `compatible_market_types(&self) -> &'static [MarketType]`: the markets it trades, for example
  `&[MarketType::Spot, MarketType::UsdMPerpetual]`. `supports_market_type` checks a requested market
  against this list.
- `input_kind(&self) -> InputKind`: `InputKind::DollarBars` (the default) or `InputKind::Trades`.
  Runners prepare exactly that stream.
- `on_event(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent>`: the one decision method,
  with no default. A strategy states what it does with each kind of event.
- `reset(&mut self)`: restores the initial state.
- `audit(&self) -> Option<serde_json::Value>`, default `None`: optional audit data stored with the run records
  under `strategy_audit`.

A strategy decides one thing: a triple-barrier entry, `domain::OrderIntent`, with its stop, its take
profit and its time barrier. It never sizes the order or checks the account: the shared
`domain::EntryGate` does both, on every runner.

### `OptimizableStrategy`: how it is searched and loaded

Implement it to take part in hyperparameter optimisation, walk-forward analysis and artifacts. The
trait starts at `crates/strategies/src/strategy.rs:134`.

- `fn id() -> &'static str`: the canonical snake_case id used by the CLI flags and stored as the
  `strategy_id` of the run registry. It takes no `self`.
- `type SharedConfig`: configuration shared by every candidate of one run and frozen for it; `()`
  when there is none. It implements `Default`.
- `type Config`: the candidate parameters. It implements `Clone + Send + Sync + Serialize +
  DeserializeOwned + Debug + Display`.
- `type Space: HyperparameterSpace<Config = Self::Config>`: the candidate grid, with
  `generate_candidates` and `perturb_neighbors`.
- `type Extractor: FeatureExtractor`: the frozen first stage. Its `Features` type is what the
  decision stage reads.
- `fn extractor(shared: &Self::SharedConfig) -> Result<Self::Extractor, DomainError>`.
- `fn from_config(shared: &Self::SharedConfig, config: Self::Config) -> Result<Self, DomainError>`.
- `fn on_features(&mut self, features: &<Self::Extractor as FeatureExtractor>::Features) ->
  Option<OrderIntent>`: the decision stage.
- `fn default_space() -> Self::Space`.
- `fn shared_for_market(shared, market) -> Self::SharedConfig`, default: unchanged. A strategy that
  trades several markets narrows its shared configuration here. The swarm drops short selling on
  spot.

`FeatureExtractor` has an associated `Features` type, `on_event(&mut self, event) -> Option<Features>`
and `reset(&mut self)`.

#### Single-stage strategies (dollar bars)

The features are the bars themselves: `type SharedConfig = ()`, `type Extractor = BarPassThrough`
(its `Features` is `DollarBar`), and `on_features` decides on each bar. The removed CUSUM strategies
were of this kind.

#### Two-stage strategies (`swarm_consensus`)

The extractor, `SwarmEnsemble`, turns trades into `ConsensusSample`s, and the decision stage,
`ConsensusPolicy`, decides on them. The optimizer runs the extractor once over the trade tape to
build a feature tape, then evaluates each candidate policy on that tape without recomputing features.
The contract suite checks that the two stages agree with a one-stage run. See
`crates/strategies/src/swarm_consensus/mod.rs`.

A strategy depends only on the `domain` crate and does no I/O: no file system, network, async
runtime or clock. `architecture_contract.rs` enforces this.

## 2. Declare and register it

Declare the module in `crates/strategies/src/lib.rs`:

```rust
pub mod my_strategy;
```

Then import the type and add one line to the macro call in `crates/strategies/src/registry.rs`:

```rust
use crate::my_strategy::MyStrategy;

register_strategies! {
    SwarmConsensusStrategy => ["swarm"],
    MyStrategy => ["my"],      // <- the one line
}
```

The second element lists optional short aliases.

## 3. What you get without further code

- `--strategy <id or alias>` in `backtest` and `run_hpo`.
- Artifact and bare-config loading through `backtest::load_strategy` (files, in
  `crates/backtest/src/wiring.rs`) and `backtest::build_strategy` (text, in
  `crates/backtest/src/registry.rs`).
- The generic contract suite (`crates/strategies/src/contract.rs`), run for every registered strategy
  by `registry::tests::every_registered_strategy_honours_the_contract`. It checks:
  - a non-empty id;
  - a default configuration that builds, and a strategy that describes itself;
  - JSON round-trip of the shared and the candidate configuration;
  - search-space candidates and neighbours that build, within the candidate budget;
  - deterministic replay, and `reset()` matching a fresh instance;
  - the two-stage run agreeing with the one-stage run;
  - well-formed decisions on synthetic and flat feeds.

## 4. What stays yours

- Unit tests specific to the strategy, and a sensible `default_space`.
- Verification at increasing fidelity:
  1. a bar-level backtest, or a trade-resolution one with `backtest --fill-model tick`;
  2. a walk-forward optimisation with `run_hpo`;
  3. a high-fidelity check of the winners with `backtest --fill-model tick`.
