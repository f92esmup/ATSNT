# Adding a Strategy

A new strategy touches its own file and **one registry line**. No runner (backtest, HPO, paper,
live) matches on strategy names; `runners_do_not_dispatch_on_strategy_names` in
`crates/backtest/tests/architecture_contract.rs` fails the build if one starts to.

## 1. Write the strategy (`crates/strategies/src/<name>.rs`)

Implement two traits from `crates/strategies/src/strategy.rs`:

- `Strategy`: `name`, `compatible_market_types`, `on_bar(&DollarBar) -> Option<OrderIntent>`, `reset`.
  It consumes finished dollar bars only; ticks never reach it.
- `OptimizableStrategy`: `id()` (snake_case, the name artifacts and CLIs use), the `Config` type
  (`Default + Clone + Serialize + Deserialize + Display`), a `Space` implementing
  `HyperparameterSpace`, `from_config` and `default_space`.

An `OrderIntent` must carry a positive price, a stop on the losing side and a target on the winning
side of it, and `max_bars_hold >= 1`.

## 2. Register it (`crates/strategies/src/registry.rs`)

```rust
register_strategies! {
    DollarBarsCusumStrategy => ["cusum"],
    OrderFlowImbalanceStrategy => ["ofi"],
    MyStrategy => ["my"],      // <- the one line
}
```

The second element lists optional short aliases. That is all the wiring there is.

## 3. What you get without further code

- `--strategy my_strategy_id` (or the alias) in `backtest`, `run_hpo`, `paper_trading` and
  `live_trading`; the id inside a deployment artifact is honoured the same way.
- Artifact and bare-config loading (`backtest::load_strategy`).
- The generic contract suite (`strategies::contract::assert_strategy_contract`), run for every
  registered strategy by `registry::tests::every_registered_strategy_honours_the_contract`: valid
  id, buildable default config, config JSON round-trip, a search space whose candidates and
  neighbours all build, deterministic replay, `reset()` equal to a fresh instance, well-formed
  intents on volatile and on flat bars.

## 4. What stays yours

Strategy-specific tests (does it signal when it should), a sensible `default_space`, and the
fidelity check before trusting any result: bar-level HPO first, then the finalists with
`backtest --fill-model tick` (see README, Step 2b).
