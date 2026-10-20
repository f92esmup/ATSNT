# Swarm consensus strategy (TFM port) on a unified execution model

Objective: port, correct, and extend the thesis's 100-agent CLIPS swarm into a 100% Rust ATSNT strategy with shorts, 16 bar feeds, and target positions. To do it, generalise the repository's execution model so that triple-barrier entries and target positions share one decision type, one strategy contract, one optimiser, one execution core, one trade ledger, and one venue contract. Nothing is duplicated, and the change is the minimum.
The full behaviour is in `docs/strategy/03-swarm-consensus.md` (the "strategy spec"). This file defines what is implemented, in which order, and how each step is proved.
Branch: `feat/swarm-consensus`. Delivery: ask-on-risk. The forecast is about 6,500 authored lines (goldens 300, domain 1,350, strategy contract 400, execution core 1,200, swarm 1,600, optimiser and runners 700, venue and session 700, docs 300), Chain strategy: `feature-branch-chain` (L10). Each phase is a reviewable slice of pull requests chained on `feat/swarm-consensus`; the slice boundaries are recorded in the Log. Test runner: `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

## Specs

- S1 Port the thesis strategy as an idea, not as a replica. User: "quiero inspirarme en esa estrategia para crearla en mi repo actual" and "no quiero una paridad exacta. quiero utilizar la idea, corregirla y mejorarla."
  - Acceptance: every section of the strategy spec is implemented, including the corrections C1–C16 (spec §13).
- S2 Short selling. User: "añadir tambien la capacidad de shorts".
  - Acceptance: mirrored rules are generated from one table (spec §8.3). There is a short axis in the policy (spec §10.3). `allow_short = false` reproduces the long-only thesis axis.
- S3 All the thesis bar types. User: "quiero añadir todos esos tipos de velas también".
  - Acceptance: 16 feeds built from aggTrades, with calibrated thresholds (spec §5), as general-purpose domain components.
- S4 100% Rust and no bias. User: "quiero saber como se traduce a 100% rust y como se haría para que no haya sesgo, que es lo que ocurre si se hace con un if else".
  - Acceptance: no FFI, no rule-engine runtime, and no `f32`/`f64`. Every rule is evaluated with explicit resolution (spec §8.4). Every control in spec §12 has its proof test.
- S5 Conflict policy. User: "la opción correcta es esperar".
  - Acceptance: entry and exit on one axis give 0 on that axis. Opposite entries give `(0, 0)` (spec §8.4).
- S6 No triple barrier for this strategy. User: "sigo tu recomendación fundamentada de no usar la triple barrera".
  - Acceptance: `Decision::Target` gives scale-in, scale-out, flip, an aggregate stop, no take profit, and no time barrier (spec §11).
- S7 Scope. User: "me parece bien empezar con backtest + hpo".
  - Acceptance: `backtest` (bar and tick fill models) and `run_hpo` run the strategy. `paper_trading` and `live_trading` refuse it through one product gate, with the message in spec §17.1.
- S8 Documentation first, in phases. User: "quiero que gastes mucho esfuerzo en declarar con pelos y señales todo todo el funcionamiento de la estrategia en su .md correspondiente y luego otr oarchivo (el de odd) que defina exactamente en cada bloque lo que se va a implementar. Hay que dividirlos en fases porque hay que ser detallistas."
  - Acceptance: the strategy spec and this file. Every task names its files, types, tests, and regression guards.
- S9 Repository philosophy. User: "Ya sabes la filosofía del repo."
  - Acceptance (AGENTS.md): `Decimal` everywhere money or thresholds live; no `unwrap`/`expect` outside tests; `thiserror`; doc comments on public items; fmt and clippy clean; deterministic synthetic tests and no network; zero-toy fills.
- S10 One engine, no duplication, minimal change. User: "esa es la idea, que no se duplique nada y que todo el motor sea único con el mínimo cambio posible. COn los venues por ejemplo. asi qeu si, rehaz el plan con esta filosofía. se muy exhaustivo."
  - Acceptance:
    - exactly one of each component in spec §3.1;
    - no second engine, registry, optimiser, ledger, or venue path;
    - `dollar_bars_cusum` and `order_flow_imbalance` produce **byte-identical** trades, metrics, rejections, HPO rankings, and recording rows before and after every phase, proved by the golden tests of T1.2;
    - pre-existing duplication not needed by this feature is listed (spec §17.2) and left untouched.

Assumptions. These were proposed in the conversation and not objected to; each one is revisable:

- A1 6 agents per feed (96 in total), with stratified assignment.
- A2 Adimensional indicators and the converted genome priors of spec §7.1.
- A3 Clock-aligned time bars. Calibration to the mean duration of 5m, 1h, 4h, and 1d, on a period that ends before the evaluated data.
- A4 Trade only on level change. One aggregate stop at 3 × ATR(1h). Re-arm after a forced exit.
- A5 Campaigns (flat to flat) count as closed trades. Fill count and peak level go to the audit, so the `ClosedTrade` schema is unchanged.
- A6 HPO searches only the 5 policy parameters. The genome is frozen (seed 123456).
- A7 HPO price events are 1 s buckets. Embargo is 1 day. The swarm runs with 100 ms latency. The data starts at least 70 days before the first evaluation.
- A8 Level-3 risk cap `max_level_risk = 0.05` in the search space.
- A9 `BacktestConfig.latency_ms` is a new optional field that defaults to 0, so existing execution contexts and artifacts behave as before.
- A10 New exit-reason strings `TargetExit` and `TargetFlip`, additive to the BI string domain.
- A11 The Binance gateway gets `submit_order` and an optional take profit. Both are unit-tested against the existing scripted server and never exercised live in this feature.

## Tasks

Legend: `[ ]` pending, `[x]` done with evidence. Each task line has its ID, its linked `S#`, its route, and its status. The route is inline unless a Writer-trigger reason is named.

Every behaviour task follows the test-first policy: RED, then GREEN. The **byte-identity guard** applies after every task from Phase 3 on. It means three things: the golden tests of T1.2 pass unchanged, every existing test passes unchanged, and no existing test file is edited except where the task says so.

### Phase 0: Specification

- [x] T0 S8, S10: strategy spec and this file. Route: inline. Commit: `docs(swarm): specify the swarm consensus strategy on a unified execution model` (docs only, as the user asked; structural readback of both files done).

### Phase 1: Safety net (S10)

No refactor starts before this phase is committed. **RED exception**: characterisation tests pass at the baseline by construction. Their proof is a mutation check: alter one fee or slippage operation locally, and every relevant golden must fail.

- [x] T1.1 S10: deterministic synthetic market fixtures. Route: inline. Done: `crates/backtest/tests/support/mod.rs` (`synthetic_trades`, `dollar_bars`); determinism test `the_tape_is_deterministic`; coverage test `the_goldens_cover_every_reachable_exit_and_rejection`. Liquidation was not reached (see the Log).
  - `crates/backtest/tests/support/market.rs` (a new test-support module) provides `synthetic_trades(seed, n)`, a seeded random walk with buy/sell sides and volume bursts.
  - It produces enough dollar bars at a small threshold for CUSUM and OFI to open and close trades through every exit kind: stop loss, take profit, time barrier, and end of simulation. Liquidation and the monthly kill switch are reached with a dedicated crash segment.
  - Tests:
    - the fixture is deterministic;
    - each exit reason appears at least once for CUSUM and for OFI on the golden configs (asserted, so the goldens cover every branch).
- [x] T1.2 S10: golden snapshots. Route: inline. Done: `crates/backtest/tests/golden_execution.rs` with 12 snapshots in `crates/backtest/tests/golden/`; stable over 3 runs; mutation check passed (see the Log).
  - New file `crates/backtest/tests/golden_execution.rs`. Snapshots are JSON files under `crates/backtest/tests/golden/`, written once with `UPDATE_GOLDEN=1` and compared byte for byte otherwise.
  - Each snapshot holds the full `Vec<ClosedTrade>`, the `BacktestMetrics`, `rejected_orders`, and the equity curve points:
    - (a) the bar engine via `run_backtest_slice_with_trades` (`optimization.rs:35`), for CUSUM and for OFI;
    - (b) `TickSimulation` (session + `SimulatedVenue`), for CUSUM and for OFI;
    - (c) a 2-fold, 6-candidate `WalkForwardOptimizer` run for CUSUM: the full ranked candidate list with metrics, fitness, stability, and DSR;
    - (d) the recording rows (`recording.rs` `trade_rows`, `equity_points`, `record_hpo` rows) built from (a) and (c).
  - Proof: the mutation check of the phase header, recorded in the Log.

### Phase 2: General-purpose domain pieces (S3, S1)

**Writer trigger (a)**: two independent units with disjoint edit surfaces, each heavier than starting a worker, launched together. The parent owns git, adds the `pub mod` lines and re-exports in `crates/domain/src/analytics/mod.rs` and `crates/domain/src/lib.rs` after both return, and runs the seam check (the full suite, once).

- [x] T2.1 S3: bar feeds in `crates/domain/src/analytics/feeds.rs`. Route: delegated writer A. Surface: that file only. Done: 14 tests; RED against stubs (10 failed by assertion, 4 negative guards passed vacuously).
  - `FeedBar { start_time, end_time, open, high, low, close, volume, trade_count }`.
  - `trait BarAggregator { fn process_trade(&mut self, &Trade) -> Option<FeedBar>; fn reset(&mut self); }`.
  - `TimeBarAggregator`, `TickBarAggregator`, `VolumeBarAggregator`, `RangeBarAggregator`, with the rules of spec §5.3. Constructors reject non-positive thresholds with `DomainError`.
  - Tests:
    - a time-bar closing trade at exactly `(b+1)·Δ` opens the next bar; `(b+1)·Δ - 1` stays in `b`;
    - empty buckets emit nothing;
    - exact OHLCV and `trade_count`;
    - tick, volume, and range close exactly at the threshold;
    - no volume carry-over;
    - a range jump keeps the true high and low;
    - a flat price never closes a range bar;
    - `reset()` equals fresh;
    - no bar is emitted at the end of input.
- [x] T2.2 S3: calibration in `crates/domain/src/analytics/calibration.rs`. Route: delegated writer A, after T2.1. Surface: that file only. Done: 7 tests; RED against an always-error stub (5 failed, 2 error cases passed vacuously).
  - `FeedThresholds { tick: [u64; 4], volume: [Decimal; 4], range: [Decimal; 4], calibration_end_ms }`.
  - `calibrate(trades: &[Trade], horizons_ms: [i64; 4]) -> Result<FeedThresholds, DomainError>`, following spec §5.4.
  - Tests:
    - exact tick and volume thresholds on uniform synthetic trades;
    - the range threshold hits the target count within 1% on a seeded random walk;
    - deterministic;
    - errors on fewer than 2 trades or a zero duration.
- [x] T2.3 S1: indicators in `crates/domain/src/analytics/indicators.rs`. Route: delegated writer B. Surface: that file only. Done: 14 tests. RED was not observed before GREEN (written in one pass); a mutation check is used instead: 10 single-rule mutations each turned the suite RED.
  - `WilderRsi(14)`, `WilderAtr(14)`, `AtrRatio(50)`, `AlmaSlope(9, 0.85, 6)`, `VwapDeviation(50)`. Each has `update(&FeedBar) -> Option<Decimal>` and `reset()`.
  - `FeedIndicators::update(&FeedBar) -> Option<IndicatorSnapshot>`, warm at bar 64 (spec §6). ALMA weights are precomputed in `Decimal` with `MathematicalOps::exp`.
  - Tests:
    - hand-computed reference vectors on a 20-bar fixture, with the computation in comments;
    - RSI = 100 without losses;
    - ATR uses the previous close across gaps;
    - the ATR ratio is 1 on a constant-range series;
    - ALMA weights against hand values;
    - VWAP deviation is 0 at the VWAP;
    - prices × 10 leave RSI, the ATR ratio, the ALMA slope, and the VWAP deviation identical;
    - warm exactly at bar 64.

### Phase 3: Domain contract types (S6, S10)

Risk: item 3 (shared domain contracts consumed by every runner). The byte-identity guard applies.

- [x] T3.1 S6: decision types in the new file `crates/domain/src/decision.rs`, re-exported from `lib.rs`. Route: inline. Done: 10 tests; RED was 26 compile errors (types missing), then GREEN. `TargetPosition` fields are private with getters so the invariants cannot be bypassed; `DomainError::InvalidTarget` added.
  - `Decision`, `TargetPosition`, `ProtectiveExits`, `MarketEvent<'a>`, `InputKind` (spec §3.2–3.3).
  - `TargetPosition::new` validates `-3 ≤ level ≤ 3`, `risk_fraction = 0 ⇔ level = 0`, `0 ≤ risk_fraction ≤ 1`, and `stop_distance > 0` when `level ≠ 0`.
  - `ProtectiveExits::from_intent(&OrderIntent)` and `ProtectiveExits::for_target(side, fill_price, stop_distance)`.
  - Tests: every validation, accepted and rejected; `from_intent` copies SL, TP, and max bars exactly.
- [x] T3.2 S6, S10: one sizing core in `crates/domain/src/risk.rs`. Route: inline. Done: `quantity_for_risk` extracted (same operation order); `size_target` + `TargetSizing`; 9 new tests (oracle property test over 1,000 LCG inputs, plus 8 `size_target` cases); RED was 14 compile errors (`size_target` missing). Every existing `risk.rs` test and every golden unchanged; workspace 621 passed.
  - A private `quantity_for_risk(equity, risk, stop_distance, price, policy)` is extracted from `size_order`, keeping its operation order (`risk.rs:254-259`). `size_order` calls it, with no behaviour change.
  - New `size_target(target, price, equity, current_qty, session_dd, policy) -> Result<TargetSizing { quantity, clipped }, RiskError>`. It applies the clip semantics of spec §11.3 and the existing breaker check.
  - Tests:
    - every existing `risk.rs` test passes unchanged;
    - a new property test checks that `size_order` equals the pre-refactor formula over 1,000 seeded random inputs (the old formula copied into the test as the oracle);
    - `size_target` sizes, clips, floors, and returns 0 for level 0.

### Phase 4: One strategy contract (S10, S6, S7)

Risk: item 3 (the strategy trait, the registry, and every runner call site). The byte-identity guard applies.

- [x] T4.1 S10: the unified traits in `crates/strategies/src/strategy.rs`. Route: inline. Done (design amended, see L14): `input_kind`, a defaulted `on_bar`, a defaulted `on_event`, `FeatureExtractor`, `BarPassThrough`, a two-stage `OptimizableStrategy` (`SharedConfig`, `Extractor`, `from_config(shared, config)`, `extractor`, `on_features`), and `Box` forwarding. RED: 17 compile errors, then 4 contract tests green.
  - `Strategy` with `input_kind` and `on_event(&MarketEvent) -> Option<Decision>` (spec §3.3). `FeatureExtractor`, `FeaturePolicy`, and the two-stage `OptimizableStrategy` (spec §3.4).
  - ~~`Pipeline<S>` implements `Strategy`.~~ Dropped (L14): the strategy is its own decision stage. `BarPassThrough` implements `FeatureExtractor<Features = DollarBar>`.
  - The `Box<S>` forwarder is updated for every method.
- [x] T4.2 S10: migrate CUSUM and OFI. Route: inline. Done: only the `OptimizableStrategy` impls changed (shared `()`, `BarPassThrough`, `on_features` wraps `on_bar`); signal code and unit tests untouched.
  - `dollar_bars_cusum.rs` and `order_flow_imbalance.rs` keep their inherent `on_bar(&DollarBar) -> Option<OrderIntent>` and their tests untouched.
  - New: `impl FeaturePolicy` wraps the result in `Decision::Entry`, and `impl OptimizableStrategy` sets `SharedConfig = ()`, `Features = DollarBar`, `Extractor = BarPassThrough`, `Policy = Self`, `input_kind = DollarBars`.
- [x] T4.3 S10: call sites and registry. Route: inline. Done: engine and session call `on_event`; an unsupported `Target` is counted under `UnsupportedDecision` in the engine (RED: an empty rejection map, then GREEN) and is ignored with a warning in the session (behaviour guard, no RED: the session already ignored non-entries). The contract suite now covers decisions, `synthetic_trades`, and two-stage equivalence (`Faulty<3>` proves the check). Workspace: 629 passed, goldens unchanged.
  - Every consumer of `on_bar` or `Option<OrderIntent>` moves to `on_event` / `Decision`: `engine.rs:295`, `session.rs:222` and `:344`, `driver.rs:73`, `optimization.rs:46`, `paper.rs:235`, and `contract.rs:86`.
  - Anything that is not `Decision::Entry` is ignored at this phase and counted under the rejection kind `"unsupported_decision"`, so behaviour stays identical.
  - `registry.rs`: the `StrategyVisitor` bound becomes the new `OptimizableStrategy`. `backtest/src/registry.rs` `Loader` calls `S::from_config(&S::SharedConfig::default(), config)` (L14).
  - `contract.rs`:
    - `assert_strategy_contract` runs `Entry` decisions through `assert_valid_intent` and `Target` decisions through `TargetPosition` validation;
    - it feeds `synthetic_bars` or a new `synthetic_trades` according to `input_kind`;
    - the "candidates and neighbours build" check now builds `policy(shared, config)`.
  - Tests:
    - every existing strategies and backtest test passes;
    - the goldens are identical;
    - `architecture_contract.rs` passes (no name dispatch; strategies depend only on the domain).

### Phase 5: One execution core (S6, S10)

Risk: item 3 and item 1-adjacent (the accounting every runner reports). An independent verifier runs at the end of this phase, because it is really high risk. The byte-identity guard applies.

- [x] T5.1 S10: pure extraction from `BacktestEngine` into the new file `crates/backtest/src/execution_core.rs`. Route: delegated writer (context backstop, L15). **No behaviour change.** Done: `ExecutionCore`, `TradeLedger`, `PendingEntry`, `market_fill_price`, and a pure `barrier_exit` over `ProtectiveExits` with optional TP and time limit. Engine fields moved into the core; the engine lends `&mut equity`. 5 new core tests; goldens byte-identical.
  - `ExecutionCore` owns the position, the pending order, `ProtectiveExits`, margin, entry fee and slippage, accrued funding, and the `TradeLedger`.
  - It holds the helpers the map identified, with their operation shapes preserved:
    - `market_fill_price`;
    - the entry fill math (`engine.rs:552-595`);
    - `barrier_exit` (`:453-501`), with `take_profit` and `max_bars` now `Option`;
    - `liquidate_if_due` (`:396-437`), reading `exits.stop`;
    - `settle_funding` (`:354`);
    - the close accounting (`:622-688`).
  - `BacktestEngine` keeps its loop and its breaker, kill-switch, MTM, and excursion code, and delegates to the core.
  - Tests: every `engine.rs` test (`:789-1809`) and `tests/trade_excursion.rs` pass unchanged; the goldens are identical.
- [x] T5.2 S6: price events and decision scheduling. Route: delegated writer (context backstop). Done: `price_event.rs` (`PriceEvent` with `start_ms` and `end_ms`, `bucket_trades`), `BacktestError::InvalidPriceBucket`, `BacktestEngine::process_event`, and `process_bar` as a wrapper. `BacktestConfig.latency_ms` defaults to 0, is skipped when serialised, and is validated `>= 0`. `due_ms = end_ms + latency_ms`. 13 new tests; RED was 18 compile errors; a mutation check on `take_due_pending` was done. Goldens byte-identical; workspace 647 passed.
  - `PriceEvent { start_ms, open, high, low, close }`, `From<&DollarBar>`, `From<&Trade>`, and `bucket_trades(iter, bucket_ms)`.
  - `BacktestEngine::process_event(&PriceEvent, decide: &mut dyn FnMut() -> Option<Decision>)`. `process_bar` becomes `process_event(bar.into(), || strategy.on_event(Bar))`, with the same steps in the same order (spec §11.2).
  - New optional `BacktestConfig.latency_ms`, with serde default 0 and validation `≥ 0`. The pending order carries `due_ms`.
  - Tests:
    - a decision is never filled on its own event;
    - with `latency_ms = 100` the fill is at the first event `≥ ts + 100`;
    - with the default 0, the bar path is unchanged (goldens);
    - old execution-context JSON files without the field still parse (`configs/execution_*.json` round-trip test).
- [x] T5.3 S6: `Decision::Target` in the core (spec §11.3–11.9). Route: delegated writer (context backstop). Done: `PendingTarget`, `fill_target`, a campaign `TradeLedger` branch, `TargetExit` and `TargetFlip`, and the counters `RearmIgnored`, `StopBeyondLiquidation`, `PositionCapClip`. 18 new tests (RED: 18 failed before wiring); 3 mutation checks. The superseded unsupported-target test was replaced. Goldens byte-identical; workspace 664 passed.
  - Sizing at decision time through `size_target`. Target replaces pending. `Δ` at fill.
  - Dust counted under the existing `RiskError` kinds. Flip in two fills. Scale-in and scale-out through `Position::apply_fill`, with margin recomputed.
  - The stop recomputed after each fill. The liquidation clip. Re-arm.
  - Breakers: a daily halt allows reductions only (`DAILY_HALT_REJECTION`); the kill switch keeps today's behaviour.
  - `TradeLedger` campaigns with `TargetExit` and `TargetFlip`. Counters: `position_cap_clip`, `stop_beyond_liquidation`, `rearm_ignored`, `daily_halt_ignored`, `dust`.
  - Tests (each asserts cash, the position, the `ClosedTrade` fields, and the counters):
    - sizing equals `equity · risk / stop` floored;
    - the cap clips;
    - dust is skipped, but 0 always closes;
    - scale-in averages the entry;
    - scale-out realises PnL;
    - a flip produces 2 fills and 2 campaigns;
    - the stop is recomputed;
    - a gap fill at the open;
    - liquidation comes before the stop;
    - the liquidation clip;
    - re-arm ignores same-sign targets until 0 or the opposite sign;
    - a daily halt blocks increases and allows reductions;
    - a 3-fill campaign is 1 `ClosedTrade` with VWAP entry and exit, peak quantity, and summed costs;
    - cash reconciles to the cent on a hand-computed scenario.

- [x] T5.4 S10, S6: Phase 5 hardening from the independent verifier's advisories. Route: delegated writer (context backstop). Done (+579/-203):
  - A1: one `ClosedTrade` builder for entries and targets (an entry is a campaign with one increasing and one reducing fill); one fee helper, one margin helper, one liquidation-price function, one adverse-gap helper.
  - 4b: target increases report slippage like entries (adverse gap from the decision price plus model slippage).
  - A2: a flip whose opening leg does not fill closes as `TargetExit`.
  - A3: margin and liquidation after scale-in/out, target latency, and re-arm after liquidation or kill switch are now pinned.
  - RED: 3 tests (4b, A2×2); 5 behaviour guards passed at once.
  - One existing expectation changed: the multi-fill campaign `slippage_paid` went from 10.271192 to 60.233192 (the 2-point adverse gap).
  - Goldens byte-identical; workspace 672 passed.

### Phase 6: The swarm strategy (S1–S5)

New module `crates/strategies/src/swarm_consensus/` with `mod.rs`, `genome.rs`, `agent.rs`, `rules.rs`, `ensemble.rs`, `audit.rs`, and `policy.rs`. Route: inline. It is one coherent unit, with no parallel reason.

- [x] T6.1 S1, S4: genome (spec §7). Route: delegated writer A (parallel with T6.5). Done: 8 tests; RED by single mutations (the writer coded and tested in one pass).
  - `GenePrior` table, `sample_swarm(seed, agents_per_feed)`, Box–Muller in `Decimal` from `StdRng`, clamps (including `atr_contraction < atr_expansion`), and per-gene clamp-hit counters.
  - Tests:
    - the same seed gives the same swarm, and a different seed a different one;
    - exactly `agents_per_feed` agents per feed, by id range;
    - every gene within its clamp;
    - over 20,000 draws the sample mean is within 4 standard errors;
    - a source-scan test finds no `f32`/`f64` in the module.
- [x] T6.2 S1, S4: macro and micro detectors in `agent.rs` (spec §8.1–8.2). Done: 12 tests; RED against stubbed detectors (8 failed).
  - Tests:
    - one positive and one negative case per state and per pattern;
    - an equal-to-threshold value does not fire;
    - a flat bar is no pin bar (C10);
    - the break of structure needs an earlier bar and uses its margin in ATR units.
- [x] T6.3 S2, S4, S5: rule table, mirror, and resolution in `rules.rs` (spec §8.3–8.4). Done: 13 tests, including rotation and reversal permutations over 500 seeded inputs and reflection symmetry; RED by mutations (a first-match `break`, no noise veto, etc.).
  - Tests:
    - E1–X4 and their mirrors fire on their minimal inputs;
    - X2 and X2′ fire together;
    - permutations (every rotation, plus a reversal) never change a vote over 500 seeded inputs;
    - reflection symmetry over the same inputs;
    - entry and exit together give 0 on the axis;
    - opposite entries give `(0, 0)`;
    - the noise veto;
    - long-only never votes short.
- [x] T6.4 S1, S3: ensemble and audit in `ensemble.rs` and `audit.rs` (spec §9, §16). Done: 15 tests; RED by mutations (readiness checked only on feed 0, stop reference taken from feed 0, ×10 scale, audit sum).
  - `SwarmEnsemble` implements `FeatureExtractor<Features = ConsensusSample>`.
  - Tests:
    - one sample per trade even when several feeds close on it;
    - votes persist;
    - no sample before every feed is warm;
    - `atr_stop_reference` is feed 1's ATR;
    - prices × 10 give identical votes;
    - deterministic replay;
    - `reset()` equals fresh;
    - the audit counters sum to the number of agent evaluations.
- [x] T6.5 S2, S6: `ConsensusPolicy` and `SwarmPolicySpace` in `policy.rs` (spec §10, §14.2). Route: delegated writer B (parallel). Done: 18 tests, including the §18 worked example; RED against `todo!()` (17 failed). The grid is pinned at 1,470 candidates after A8.
  - Tests:
    - the worked example of spec §18, table-driven;
    - a jump from 0 to 3;
    - a cascade from 3 to 0;
    - `H > L1` keeps level 1;
    - long-only never goes negative;
    - the grid count after the A8 filter is pinned;
    - every neighbour is on the grid;
    - it emits only on change.
- [x] T6.6 S1, S10: `SwarmConsensusStrategy` in `mod.rs`. Route: inline. Done (commit d0ef041): `Strategy` (input `Trades`, `on_event` = extractor + `on_features`, `audit`) and `OptimizableStrategy` (`SwarmConfig` / `PolicyConfig` / `SwarmPolicySpace` / `SwarmEnsemble`). One registry line (`swarm`). `Strategy::audit` added with a default of `None`. RED: 7 compile errors, then 3 tests green; the generic contract suite now runs for the swarm.
  - It implements `OptimizableStrategy` with id `swarm_consensus` and alias `swarm`, `input_kind = Trades`. It is registered with **one line** in the existing `register_strategies!` macro, plus `pub mod` in `lib.rs`.
  - `Strategy::audit()` is a new trait method that defaults to `None` and returns `serde_json::Value`. The swarm overrides it with its audit.
  - Tests: the generic contract suite passes for the swarm through the existing `every_registered_strategy_honours_the_contract`; the `SharedConfig` and `Config` JSON round-trip.

### Phase 7: One optimiser and the runners (S6, S7, S10)

Risk: item 3 (CLI flags, artifact schema). The byte-identity guard applies.

- [x] T7.1 S10: the tape in `crates/backtest/src/optimization.rs`. Route: delegated writer X (context backstop; parallel with T7.4). Done: `Tape<F>`, `bar_tape`, `trade_tape`, `run_tape_slice`, `run_optimization_on_tape`, `with_shared_config`. `run_optimization(&bars, ..)` is kept and delegates. RED for the extractor-runs-once counter (0 → 1). Golden (c) unchanged.
  - `Tape<F> { features: Vec<(i64, F)>, prices: Vec<PriceEvent> }`. `WalkForwardOptimizer::run_optimization` takes a tape and, per candidate and slice, replays a fresh `S::policy` through `BacktestEngine::process_event`.
  - `run_backtest_slice_with_trades` and `calculate_parameter_stability` follow.
  - For `DollarBars` strategies the tape holds the bars once (the prices are derived from the same slice without copying them).
  - Tests:
    - golden (c) is identical;
    - `hpo_fold_metrics.rs`, `param_space_contract.rs`, and `recording_contract.rs` pass unchanged;
    - the extractor runs exactly once per run (a counter on a test strategy).
- [x] T7.2 S6, S7: `run_hpo` input preparation. Route: delegated writer X. Done:
  - Dispatch by `input_kind()`. Trades strategies require `--shared-config <file>` and accept `--price-bucket-ms` and `--embargo-ms`.
  - The warm-up and calibration checks (`InsufficientWarmUp`, `CalibrationOverlap`) refuse bad inputs.
  - `SamplingConfig::Trades { price_bucket_ms: Option<i64> }` and the `HpoDeploymentArtifact` wrapper carry `shared_config`. CUSUM/OFI artifacts are byte-identical.
  - RED: unknown CLI flags and missing types.
  - `DollarBars`: today's code, unchanged.
  - `Trades`: stream the trades through the extractor and `bucket_trades`. New flags `--price-bucket-ms` (default 1000) and `--embargo-ms` (default 86,400,000, `Trades` strategies only). The 70-day lead check and the calibration check (spec §15.2) apply.
  - The artifact adds `SamplingConfig::Trades { price_bucket_ms }` and stores the `SharedConfig`. The `DollarBar` variant is unchanged, and an old-artifact round-trip test covers it.
  - Tests:
    - a synthetic `Trades` run ranks deterministically;
    - the fold ranges respect the embargo;
    - both checks refuse bad inputs with explicit messages;
    - the artifact round-trips;
    - the `run_hpo --help` text lists the new flags.
- [x] T7.3 S7: the `backtest` CLI. Route: delegated writer X. Done:
  - `--strategy-config` alias and `--shared-config`.
  - Trades strategies run at trade resolution; `--fill-model tick` is refused until T8.2.
  - A STRATEGY AUDIT block on stdout, and `record_backtest_with_audit` puts `strategy_audit` in the metrics extras (the CLI writes no report file).
  - The loader threads the shared config: artifact first, then the flag, then the default; a conflict is refused.
  - 14 tests in `tests/swarm_runners.rs`.
  - `--strategy swarm_consensus --strategy-config <json>` with `--fill-model bar` runs at trade resolution (each trade is a price event).
  - The report gains `strategy_audit` from `Strategy::audit()`, and nothing for strategies that return `None`.
  - Tests:
    - an end-to-end run on a synthetic fixture produces campaigns and an audit;
    - the `run_contract.rs` output for CUSUM and OFI is unchanged.
- [x] T7.4 S3: the `calibrate_feeds` binary in `crates/backtest/src/bin/calibrate_feeds.rs`. Route: delegated writer Y (parallel). Done: `--data/--from/--to/-o/--base`. The output is validated with `SwarmConfig::validate`. 8 unit tests and 2 CLI tests; RED against stubs (6 + 2 failed).
  - Flags: `--data`, `--from`, `--to` (exclusive), and `-o`. It writes a swarm `SharedConfig` JSON.
  - Tests: the output parses; `calibration_end_ms` is the last trade before `--to`.

### Phase 8: One venue contract and the session (S10, S7)

Risk: item 3 (ports and the live adapter) and item 2-adjacent (live order placement code). An independent verifier runs at the end of this phase. The byte-identity guard applies to golden (b) and to every session, venue, live-exit, and Binance test.

- [x] T8.1 S10: ports and adapters. Route: inline. Done:
  - `crates/ports`: `OrderRequest { side, quantity, reduce_only }`, `ExecutionVenue::submit_order`, and `ExitGateway::place_exits` with `take_profit: Option<Decimal>`.
  - `SimulatedVenue`: `submit_order` handles explicit market orders respecting latency and `reduce_only`. `place_exits` with `None` arms single-leg stops.
  - `BinanceGateway`: `place_market_order` / `submit_order` validates futures-only, sizes externally, handles `reduceOnly`. `place_stop_exit` submits single-leg `STOP_MARKET` conditional order with `closePosition=true`. Mock tests verify parameters.
  - Tests: unit tests for `SimulatedVenue` and `BinanceGateway` exits pass.
- [x] T8.2 S6, S10: `TradingSession` executes targets (spec §11.10). Route: inline. Done:
  - New phase `Phase::Adjusting { position, target, delta, submitted_ms, pending_flip }`. Sizing through domain `RiskPolicy::size_target`.
  - Two-phase flips (reduce-only close followed by opening leg). Exits replaced on fill.
  - Closed trade accounting unified through `TradeLedger::close_campaign`.
  - `SessionDriver::on_market_trade` feeds `MarketEvent::Trade` to `Trades` strategies via `on_trade_event`.
  - `--fill-model tick` enabled for `InputKind::Trades` in backtest CLI.
  - Tests: 42 session unit tests pass; `target_simulation` integration test asserts parity between bar engine and tick engine campaigns.
- [x] T8.3 S7: the product gate. Route: inline. Done:
  - `Strategy::emits_targets(&self) -> bool` default `false`, returns `true` for `SwarmConsensusStrategy`.
  - `LoadedStrategy::ensure_accepts_decisions` refuses target strategies in `paper_trading` and `live_trading` offline prior to network access.
  - Tests: `target_gate_cli` tests (3/3) pass.

### Phase 9: Documentation, data, and verification (S1, S8)

- [x] T9.1 S8, S10: architecture docs. Route: inline. Done:
  - `12-adding-a-strategy.md`: updated with the unified strategy contract, single-stage vs. two-stage strategies, `Decision::Target` and `Decision::Entry`.
  - `04-hpo-and-walk-forward.md`: updated with `OptimizableStrategy`, two-stage execution, and `Tape<F>`.
  - `06-paper-trading-and-realtime-execution.md` and `08-live-execution-gateway.md`: documented the paper/live product gate, `submit_order`, and optional take-profit stops.
  - `13-bi-data-contract.md`: confirmed `TargetExit` and `TargetFlip` documented in the BigQuery schema contract.
  - `README.md`: added `calibrate_feeds` CLI and Step 4b describing the full Swarm Consensus workflow.
- [ ] T9.2 S1: data, calibration, and a local run. Route: inline. Operational task, no code.
  - The period is decided with the user before fetching. Fetch with `fetch_data`, then `calibrate_feeds`, a local `run_hpo`, and the bar and tick backtests of the finalists.
  - Record the audit, the scorecard, and an honest verdict in the Log.
- [ ] T9.3 S1–S10: independent verifier over the whole feature. Route: delegated verifier.
  - It receives this whole document, the baseline commit, and the probe forms. It returns a verdict per `S#`.
  - Then the native review, under the receipt-driven development policy.

## Log

- L1 (2026-10-19) User: "/home/f92esmup/Downloads/TFM_presentacion (1).pdf esta es la presentación de mi tfm y estaba hecho en metatrader 5. quiero inspirarme en esa estrategia para crearla en mi repo actual. Es posible? que cabios recoemiendas o mejoras? se puede?"
- L2 (2026-10-19) User: "Vale, vamos a describir y hacer lacorresponencia de todas las partes. Primero que van a ser los nuevos agentes? el consenso lo hacemos igual no? yo quiero replicar el tfm al detalle. Pero añadir tambien la capacidad de shorts. Y quiero añadir todos esos tipos de velas también. quiero saber como se traduce a 100% rust y como se haría para que no haya sesgo, que es lo que ocurre si se hace con un if else"
  - Evidence from `github.com/f92esmup/tfm-clips` `files/0*_*.clp`:
    - several decision rules can fire together, and `obtener-voto` returns the first `voto` fact, so the CLIPS agenda order decides (C3);
    - `feed = i % 16` overweights feeds 0–3 (C4).
- L3 (2026-10-19) User: "Estoy de acuerdo contigo. la opción correcta es esperar"
  - Evidence:
    - `Ensamble.mqh` formats the ATR genes with `%.2f`, so most thresholds become `0.00` (C1);
    - time bars drift (C2);
    - the warm-up checks only M5 (C8);
    - votes persist per feed.
- L4 (2026-10-19) User: "no quiero una paridad exacta. quiero utilizar la idea, corregirla y mejorarla. asi que vamos por el buen camnino. QUeda algo mas que decidir?"
- L5 (2026-10-19) User: "creo que voy a seguir tu recomendación. Pero no te neitendo con lo del TP? te refieres a usar las triple barrera de lopez del prado?"
- L6 (2026-10-19) User: "me parece perfecto sigo tu recomendación fundamentada de no usar la triple barrera. Que mas queda por determinar?"
- L7 (2026-10-19) User: "me parece bien empezar con backtest + hpo. entonces ya solo queda definir todo bein no? quiero que gastes mucho esfuerzo en declarar con pelos y señales todo todo el funcionamiento de la estrategia en su .md correspondiente y luego otr oarchivo (el de odd) que defina exactamente en cada bloque lo que se va a implementar. Hay que dividirlos en fases porque hay que ser detallistas. Ya sabes la filosofía del repo. Puedes hacerlo?"
  - First plan: separate target traits, `TargetEngine`, and a separate HPO path.
- L8 (2026-10-19) User: "vale, pues ya lo decidiremos mas adelante que periodo descargar. Me preocupa una cosa, actualmente tenemos un repo hexagonal que pretender que todo se pueda reutilizar y sea accesible. com piezas de un puzzle.Lo que he leido es que este va a tener su diseño indepnediente no es asi? esto no hay otra forma de hacerlo no?"
  - Analysis: the first plan duplicated the strategy contract, the registry, the engine (fills, fees, funding, liquidation, breakers), and the HPO path. Alternative: the triple barrier is a special case of a target position.
- L9 (2026-10-19) User: "esa es la idea, que no se duplique nada y que todo el motor sea único con el mínimo cambio posible. COn los venues por ejemplo. asi qeu si, rehaz el plan con esta filosofía. se muy exhaustivo. Al acabar haz commit solo con los docuemntos que has creado aqui"
  - Evidence from the execution map:
    - there are two execution stacks today: `BacktestEngine` (HPO and the bar backtest), and `TradingSession` + `ExecutionVenue` (tick, paper, live);
    - position, fills, fees, funding, liquidation, barriers, breakers, closed-trade accounting, equity, and excursion are each implemented in both;
    - `domain::Position::apply_fill` is unused by both;
    - exit reasons are free strings (`metrics.rs:26`);
    - no end-to-end golden test pins CUSUM or OFI;
    - `ExecutionReport` has no order id, so the session attributes fills by phase;
    - Binance exits are native `STOP_MARKET` / `TAKE_PROFIT_MARKET` orders with `closePosition=true`.
  - Decision: the plan is rewritten as one decision type, one strategy contract with two stages, one sizing core, one execution core with a shared trade ledger, one optimiser tape, and one venue contract (`submit_order`, optional take profit). Golden tests come first. Pre-existing duplication not needed here is listed in spec §17.2.
- T0 committed as b729269. Native review: low risk (non-executable only), approved and acknowledged, lineage review-c98e13ee95dae050.
- L10 (2026-10-19) User: "sobre esa rama de feature."
  - Decision: the chain strategy is `feature-branch-chain`.
- L11 (2026-10-19) User: "Si ejecuta la fase 1"
  - Phase 1 evidence:
    - The synthetic tape (seed 7, 60,000 aggTrades, $1M bars, 1,947 bars) reaches these exits:
      - bar engine: StopLoss, TakeProfit, TimeBarrier, MonthlyCircuitBreaker (stress scenario: risk 5%, monthly limit 3%), and EndOfSimulation (a 45,379-trade prefix that ends inside an order-flow trade);
      - tick path: the same, except EndOfSimulation, because `TickSimulation::finish` never force-closes;
      - rejections: DailyHalt and OrderSizeExceeded.
    - Liquidation was not reached with any valid configuration probed. The stop-before-liquidation rule closes at the stop first. Liquidation stays pinned by the existing engine (`engine.rs` liquidation tests) and venue unit tests.
    - The snapshots match over 3 consecutive runs, including the rayon-parallel walk-forward ranking. Recorded rows are normalised: run ids become `<run-N>` and wall-clock fields `<wall-clock>`.
    - Mutation check:
      - +1e-8 on the bar engine's entry fee fails 3 tests (bar outcomes, backtest recording rows, walk-forward ranking and recording);
      - +1e-8 on the simulated venue's commission fails the tick test;
      - both mutations were reverted.
    - `cargo fmt`: clean. `cargo clippy --all-targets -D warnings`: clean. `cargo test --workspace`: all green.
  - Phase 1 committed as d2209de. Review assessment: tier high, review due (`high_risk`). The user granted consent; one lens (reliability), lineage review-44f793ef2e34b514.
  - The single capture returned `correction_required` without exposing findings. The bound STATUS then stopped with `corrupted_or_unverifiable_authority`. This is a Gentle AI defect, equivalent to the open issue Gentleman-Programming/gentle-ai#5209.
  - The user chose to report it and continue. One occurrence comment was posted on #5209. The captured decline invocation was run once: `declined_this_candidate`. No review receipt exists for d2209de.
- L12 (2026-10-19) User: "continua con la fase 2"
  - Phase 2 evidence:
    - The parent added `DomainError::InsufficientData` before launch.
    - Two parallel writers, each within its declared surface (checked against `git status`).
    - The parent wired `analytics/mod.rs` and `lib.rs` re-exports and added `impl From<&FeedBar> for BarInput`.
    - Seam check: fmt clean, clippy clean, `cargo test --workspace` 601 passed, 0 failed.
  - Deviations accepted from the writers:
    - Indicators take a `BarInput` and the current ATR instead of `&FeedBar`.
    - Lookbacks below 2 are rejected.
    - RSI is computed as `100·G/(G+L)`, one rounding less than the textbook form.
    - Window sums are recomputed per update instead of running sums, to avoid drift.
    - Rounding uses `MidpointAwayFromZero`.
    - Range bisection checks the rounded candidate.
    - A volume threshold that rounds to 0 is `InsufficientData`.
  - Found during the seam check: the HPO recording golden stored the build's `git_sha`, so it changed with every commit. It is now normalised to `<build>`, and the only lines changed in the snapshot are those 7 `git_sha` lines (commit "test(backtest): normalize the build revision in golden recording rows"). A mismatch now writes a `.actual.json` next to the snapshot.
- L13 (2026-10-19) User: "Vale pues ignora la herramienta para el main completo y ya esta. no me lo vuelvas a preguntar. SIque con la fase 3"
  - Decision: whole-branch-vs-main review consents are declined without asking; per-commit review consents are still relayed.
  - Phase 3 evidence: fmt clean, clippy clean, `cargo test --workspace` 621 passed, 0 failed, goldens unchanged (byte-identity guard).
  - Design notes:
    - `size_target` takes the signed current quantity, so the daily breaker can allow reductions and block increases and flips.
    - Order-level limits (minimum lot and notional, maximum order notional) are left to the order that moves the position (Phase 5).
- L14 (2026-10-19) User: "sigue con la fase 4"
  - Design amendment (spec §3.3–3.4 updated):
    - `Pipeline<S>` is dropped; the strategy is its own decision stage.
    - `Strategy::on_bar` keeps a default and `on_event` defaults to `on_bar` wrapped in `Decision::Entry`, so no existing strategy or test strategy needed edits.
  - Rationale: a smaller change with the same single path. The two stages cannot drift, because the contract suite asserts that extractor + `on_features` decide exactly what `on_event` decides.
  - Known interim: `run_backtest_slice` and the strategy loader pass `S::SharedConfig::default()`. Phase 7 (T7.2/T7.3) threads the real shared configuration, which the swarm needs.
  - Phase 4 evidence: fmt clean, clippy clean, `cargo test --workspace` 629 passed, 0 failed, goldens unchanged, `architecture_contract.rs` green.
- L15 (2026-10-20) User: "Continúa"
  - Route: the parent context passed the ~150k backstop, so T5.1 went to a bounded writer (Writer trigger b).
  - The writer stopped on one blocker, as instructed. The engine's own unit tests read and write the moved private field `active_position`.
  - Parent decision: a mechanical migration of those 18 lines (`engine.active_position.` → `engine.active_position().`, and one write → `engine.core.set_position_for_test(..)`). A normalised diff proved that only the access path changed and no assertion changed.
  - The goldens also normalise `git_sha` when it is `null` (a tree without `.git`), as found by the writer.
  - Evidence: fmt clean, clippy clean, `cargo test --workspace` 634 passed, 0 failed, goldens unchanged.
- T5.2 deviations accepted:
  - `PriceEvent` carries `end_ms` as well as `start_ms`, because rollovers, the funding window and exit times read the bar's end time (spec §11.1 amended).
  - `due_ms` lives in the core next to the pending order.
  - Core functions take `impl Into<PriceEvent>`, so existing core tests stay unchanged.
  - Parent spot check: fmt clean, clippy clean, `cargo test --workspace` 647 passed, 0 failed. The only test-file edit is an addition in `execution_config_contract.rs` (no line removed).
- T5.3 deviations reported by the writer (to be confirmed by the user):
  - (a) A Target that arrives while a triple-barrier Entry is open or pending is ignored and counted `UnsupportedDecision`. The spec leaves mixing open.
  - (b) Reported slippage on target fills is `open × slippage_pct`. Entries also add the adverse gap from the signal price.
  - (c) The liquidation clip is applied before the fill, with the same result.
  - Parent spot check: fmt clean, clippy clean, `cargo test --workspace` 664 passed, 0 failed.
- Phase 5 independent verifier: no blockers.
  - Item results: byte identity PASS (goldens unchanged, plus a baseline-vs-HEAD probe over 3 non-default configs, IDENTICAL); no-duplication PASS with advisory A1; spec §11 conformance PASS with advisories; workspace 664 passed.
  - Advisories fixed in T5.4: A1, 4b, A2, A3.
  - Accepted and written into the spec: 4a (a Target during an Entry position is `UnsupportedDecision`), 4c (the liquidation clip before the fill), A6 (a level that sizes to 0 is `TargetExit`, documented).
  - Left: A4 (a flip's closed trade misses its post-exit drawdown sample) and A5 (the reduction leverage fallback); both minor.
- L16 (2026-10-20) User: "yes" (start Phase 6); L17 "continua" (resume after writer A hit an API session limit; its saved work was resumed, not restarted).
  - Phase 6 evidence (commit d0ef041):
    - The parent wrote `sample.rs` (`ConsensusSample`) and the module skeleton first, so the two writers had disjoint surfaces.
    - Seam check: fmt clean, clippy clean, `cargo test --workspace` 743 passed, 0 failed, goldens unchanged.
  - Writer A findings:
    - (1) Doji asymmetry in spec §8.2: a dragonfly doji (C=O=H>L) is a bullish pin bar, but its reflection (a gravestone doji) is not a bearish one, because the bearish detector requires C<O. Implemented exactly as specified. The reflection test uses bars with a body. **Open for the user:** add a symmetric guard?
    - (2) Exact reflection also reflects the RSI genes (`ob' = 100 - os`).
    - (3) Rule firing counts are taken before the noise veto.
    - (4) A warm feed whose next bar yields no snapshot (ATR 0) votes as cold until it recovers.
    - (5) The default `SwarmConfig` tick, volume and range thresholds are placeholders until calibrated.
- L18 (2026-10-20) User: "si, la hacemos simétrica. Y si, continua con la fase 7"
  - Decision: the bearish pin bar is now the exact mirror of the bullish one (`C0 ≤ O0`, `H0 > L0`, positive upper wick). A gravestone doji counts as bearish, as the dragonfly doji counts as bullish. Spec §8.2 updated.
  - Evidence:
    - RED: the new test `dojis_are_pin_bars_symmetrically` failed on the gravestone, then went GREEN.
    - The reflection test fixture now includes dojis (1 in 10) and passes.
    - `cargo test -p strategies` 105 passed; clippy clean.
- Phase 7 evidence:
  - fmt clean, clippy clean, `cargo test --workspace` 783 passed, 0 failed, goldens unchanged.
  - Wording fix to spec §15.2: the literal "70 days before the first feature" is impossible, because the 1d feed warms at about day 64. Evaluation now starts at max(first trade + 70 days, first feature). This keeps the intent (≥ 70 days of lead and never a half-warm swarm).
  - Notes:
    - `run_backtest_slice(_with_trades)` still pass `SharedConfig::default()` (the golden helper is generic; only dollar-bar strategies call them). The optimizer, `run_hpo` and the loader pass the real shared config.
    - Order latency at trade resolution comes from `execution_context.latency_ms`.
    - Architecture docs pending in T9.1.
- Phase 8 evidence:
  - Ports and adapters (T8.1): `OrderRequest`, `ExecutionVenue::submit_order`, optional TP in `ExitGateway::place_exits`. `SimulatedVenue` and `BinanceGateway` implement `submit_order` and stop-only exits.
  - Product gate (T8.3): `Strategy::emits_targets` and `ensure_accepts_decisions` refuse target strategies in `paper_trading` and `live_trading` offline prior to network access.
  - Session target execution (T8.2): `Phase::Adjusting`, `RiskPolicy::size_target`, shared `TradeLedger::close_campaign` accounting, `SessionDriver` trade routing for `Trades` strategies, and `--fill-model tick` enabled for swarm.
  - Verification results:
    - `cargo fmt --check`: clean.
    - `cargo clippy --all-targets -- -D warnings`: clean.
    - `cargo test --workspace`: all tests passed (0 failed).
    - `cargo test --test golden_execution`: all 6 snapshot tests passed; golden snapshots byte-identical.
    - `cargo test --test target_simulation`: passed.
    - `cargo test --test target_gate_cli`: 3/3 passed.
    - `cargo test --test swarm_runners`: 14/14 passed.
  - Independent verifier: VERDICT PASS. Advisories addressed:
    - A1: removed `.unwrap()` in `session.rs` for flip side deduction, adhering to AGENTS.md.
    - A2: relabeled closing campaign to `TargetExit` if the opening leg of a flip fails in `session.rs`.
    - A3: updated stale doc comment on `TargetOnly`.
- Next: Phase 9 (documentation, data download, local validation).
