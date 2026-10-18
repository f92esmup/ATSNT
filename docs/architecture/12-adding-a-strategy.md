# Adding a Strategy

A new strategy touches three files: its own file, one `pub mod` line in
`crates/strategies/src/lib.rs`, and an import plus **one registry line** in
`crates/strategies/src/registry.rs`. No runner (backtest, HPO, paper, live) matches on strategy
names; the test `runners_do_not_dispatch_on_strategy_names` in
`crates/backtest/tests/architecture_contract.rs` fails `cargo test` if one starts to, and it takes
the names to look for from the registry, so a new strategy and its aliases are covered without
editing it.

## 1. Write the strategy (`crates/strategies/src/<name>.rs`)

Implement two traits from `crates/strategies/src/strategy.rs`:

- `Strategy`: `name`, `compatible_market_types`, `on_bar(&DollarBar) -> Option<OrderIntent>`, `reset`.
  It consumes finished dollar bars only; ticks never reach it.
- `OptimizableStrategy`: `id()` (snake_case, the name artifacts and CLIs use), the `Config` type
  (`Clone + Send + Sync + Serialize + DeserializeOwned + Debug + Display`, plus `Default`, which
  the registry requires), a `Space` implementing `HyperparameterSpace` (plus
  `Serialize + DeserializeOwned`, which the registry requires), `from_config` and `default_space`.

A strategy depends only on the `domain` crate and does no I/O: no file system, network, async
runtime or clock. `architecture_contract.rs` enforces it.

An `OrderIntent` must carry a positive price, a stop on the losing side and a target on the winning
side of it, and `max_bars_hold >= 1`.

## 2. Declare and register it

In `crates/strategies/src/lib.rs`, declare the module (and, by convention, re-export the type):

```rust
pub mod my_strategy;
```

In `crates/strategies/src/registry.rs`, import the type and add it to the macro call:

```rust
use crate::my_strategy::MyStrategy;

register_strategies! {
    DollarBarsCusumStrategy => ["cusum"],
    OrderFlowImbalanceStrategy => ["ofi"],
    MyStrategy => ["my"],      // <- the one line
}
```

The second element lists optional short aliases. That is all the wiring there is; a
`configs/param_space_*.json` file is optional, for searching a space other than `default_space`.

## 3. What you get without further code

- `--strategy my_strategy_id` (or the alias) in `backtest`, `run_hpo`, `paper_trading` and
  `live_trading`; the id inside a deployment artifact is honoured the same way.
- Artifact and bare-config loading (`backtest::load_strategy`).
- The generic contract suite (`crates/strategies/src/contract.rs`, compiled only in tests), run for every
  registered strategy by `registry::tests::every_registered_strategy_honours_the_contract`: valid
  id, buildable default config, config JSON round-trip, a search space whose candidates and
  neighbours all build, deterministic replay, `reset()` equal to a fresh instance, well-formed
  intents on volatile and on flat bars.

## 4. What stays yours

Strategy-specific tests (does it signal when it should), a sensible `default_space`, and the
fidelity check before trusting any result: bar-level HPO first, then the finalists with
`backtest --fill-model tick` (see README, Step 2b).
