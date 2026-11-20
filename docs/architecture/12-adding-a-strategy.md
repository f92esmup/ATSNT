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
- `on_event(&mut self, event: &MarketEvent<'_>, view: &StrategyView, out: &mut Vec<Decision>)`: the
  one decision method, with no default. Runners call it once per market trade; a strategy that
  needs bars builds its own. `view` shows the strategy's open position (side, quantity, entry
  price) and whether an entry is pending, never the balances. The strategy pushes its decisions to
  `out`, which arrives empty and is reused, so deciding allocates nothing.
- `scope(&self) -> StrategyScope`, default `PerInstrument`: how the strategy runs on an account
  that trades several instruments. `PerInstrument` runs one instance per instrument, each seeing
  only its instrument; `CrossSectional` runs one instance that sees every instrument and names the
  instrument in its decisions.
- `reset(&mut self)`: restores the initial state.
- `audit(&self) -> Option<serde_json::Value>`, default `None`: optional audit data stored with the run records
  under `strategy_audit`.

A strategy decides two things (`domain::Decision`):

- `Decision::Enter(OrderIntent)`: a triple-barrier entry, with its stop, its take profit and its
  time barrier. It is ignored while a position is open or an entry is pending.
- `Decision::Exit`: close the open position at market. The trade is labelled `SignalExit`. It is
  ignored while flat. On the bar route the close is a market order that fills at the open of the
  first event due after the decision, with slippage and the taker fee, as entries do; on the tick
  route the session flattens the position on the venue. `tests/signal_exit_parity.rs` pins that
  both routes close alike.

Every event names its instrument: `MarketEvent::Trade { instrument, trade }`, and
`event.instrument()` returns it. A decision may name an instrument too (`OrderIntent::on(id)`,
`Decision::exit_of(id)`); without one it is about the instrument of the event being decided on,
which is all a `PerInstrument` strategy needs. A single-instrument runner ignores a decision that
names another instrument.

It never sizes the order or checks the account: the shared `domain::EntryGate` does both, on every
runner. Against a book of several positions (`domain::Book`) it also applies the portfolio limits
of `domain::PortfolioLimits` (positions open at once, positions on one side, gross exposure), sizes
with the instrument's own order filters (`domain::InstrumentSpec`), and posts margin from the
balance the open positions leave free.

The time barrier is a duration in milliseconds. The constructors (`OrderIntent::new`,
`with_risk`, `with_risk_and_exits`, `limit`) take `max_hold_ms` and store `deadline_ms =
timestamp + max_hold_ms`, which must be later than the decision. The bar route and the tick route
close the position on the first event that ends at or after the deadline, so both apply the same
barrier (`crates/backtest/tests/time_barrier_parity.rs`). There is no barrier counted in bars.

### `OptimizableStrategy`: how it is searched and loaded

Implement it to take part in hyperparameter optimisation, walk-forward analysis and artifacts. The
trait is `OptimizableStrategy` in `crates/strategies/src/strategy.rs`.

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
- `fn on_features(&mut self, features: &<Self::Extractor as FeatureExtractor>::Features, view:
  &StrategyView, out: &mut Vec<Decision>)`: the decision stage, deciding as `on_event` does. When
  several features land in one price bucket of the optimizer's tape, the decisions of the last one
  that decided are the bucket's.
- `fn default_space() -> Self::Space`.
- `fn shared_for_market(shared, market) -> Self::SharedConfig`, default: unchanged. A strategy that
  trades several markets narrows its shared configuration here. The swarm drops short selling on
  spot.
- `fn warm_up_ms(shared) -> i64`, required: the data the strategy needs before its first evaluated
  decision. `run_hpo` evaluates nothing earlier and refuses data that is shorter. The swarm
  declares 70 days; a strategy that decides from its first trade declares 0.
- `fn calibration_end_ms(shared) -> Option<i64>`, default `None`: the last timestamp of the data the
  shared configuration was fitted on. `backtest` and `run_hpo` refuse data that starts at or before
  it, so a calibration never leaks into a result. Declare it whenever the shared configuration is
  fitted to data.
- `fn requires_shared_config() -> bool`, default `false`: `true` when the default shared
  configuration would mislead, so `run_hpo` refuses to run without `--shared-config`. The swarm
  returns `true`: its default feed thresholds are not calibrated.
- `fn contract_fixture() -> Option<(SharedConfig, Config)>`, default `None`: the configuration the
  contract suite runs the strategy with when its defaults never decide on synthetic data. The
  swarm's defaults are sized for BTCUSDT, so it gives small thresholds here.

`FeatureExtractor` has an associated `Features` type, `on_event(&mut self, event) -> Option<Features>`
and `reset(&mut self)`.

#### A strategy with no frozen first stage

Its extractor passes the trade through: `Features` is `Trade`, `on_features` decides on it. Pick
this when there is nothing expensive to compute once and replay.

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

- `--strategy <id or alias>` in `backtest` and `run_hpo`. `run_hpo` requires it and writes its
  artifact to `configs/hpo_<id>_results.json` unless `--output-config` says otherwise. `backtest`
  takes the strategy from the artifact, else from `--strategy`; it picks the only registered
  strategy when there is one and refuses to guess when there are several.
- Artifact and bare-config loading through `backtest::load_strategy` (files, in
  `crates/backtest/src/wiring.rs`) and `backtest::build_strategy` (text, in
  `crates/backtest/src/registry.rs`).
- The generic contract suite (`crates/strategies/src/contract.rs`), run for every registered strategy
  by `registry::tests::every_registered_strategy_honours_the_contract`. It checks:
  - a non-empty id;
  - a default configuration that builds, and a strategy that describes itself;
  - JSON round-trip of the shared and the candidate configuration;
  - search-space candidates and neighbours that build, within the candidate budget;
  - a warm-up that is not negative, and a default calibration that ends before the synthetic data;
  - at least one decision on an input that covers the declared warm-up (a trending walk) and then
    400 synthetic minutes, so the checks below never pass without checking anything;
  - deterministic replay, and `reset()` matching a fresh instance;
  - the two-stage run agreeing with the one-stage run;
  - well-formed decisions on synthetic and flat feeds, with a time barrier after the decision.

## 4. What stays yours

- Unit tests specific to the strategy, and a sensible `default_space`.
- A golden test that pins its trades on a deterministic tape, following
  `crates/backtest/tests/golden_swarm.rs` and its tape in `crates/backtest/tests/support/swarm.rs`.
  The contract checks behaviour, not results: only a golden shows that a change moved the trades.
- Verification at increasing fidelity:
  1. a bar-level backtest, or a trade-resolution one with `backtest --fill-model tick`;
  2. a walk-forward optimisation with `run_hpo`;
  3. a high-fidelity check of the winners with `backtest --fill-model tick`.
