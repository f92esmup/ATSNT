# Swarm Classifier with Conviction Sizing and Adaptive Triple Barrier

**Objective**: Refactor the Swarm Consensus Strategy (Strategy 03) from a continuous multi-level target state machine ($k \in \{-3 \dots +3\}$ with hysteresis decay) into an **Ensemble Classifier with Conviction Sizing and Adaptive Triple Barrier** (Breakeven & Trailing Stop). This eliminates exit churn and noise liquidations, bounds trade duration to true intraday windows (2h–12h), enforces positive risk-reward ($TP \ge 1.5 \times SL$), and unlocks immediate deployment to **Paper Trading** and **Live Trading** via the existing `OrderIntent` execution gateway.

**Branch**: `main` (direct authorized feature work).  
**Forecasting**: ~800 lines across `crates/domain`, `crates/strategies`, `crates/backtest`, and docs.  
**Test Runner**: `cargo test --workspace`, `cargo test --test golden_execution`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

---

## Specs

- **S1: Ensemble Classifier Decision Stage**:
  - The 96 agents and 16 bar feeds compute the normalized consensus score $C \in [-1.0, +1.0]$.
  - A single conviction threshold $\theta \in [0.30, 0.60]$ gates entries:
    - $C \ge +\theta \implies \text{Long Entry}$ (`Side::Buy`).
    - $C \le -\theta \implies \text{Short Entry}$ (`Side::Sell`).
    - $|C| < \theta \implies \text{Neutral}$ (Wait / `None`).
  - Acceptance: Strategy emits `Decision::Entry(OrderIntent)` on threshold crossing, and `emits_targets()` returns `false`.

- **S2: Sizing by Conviction (Bet Sizing at Entry - AFML Ch. 10)**:
  - Initial position risk is determined **only at entry** based on consensus strength:
    $$\text{risk} = \text{base\_risk} + \left(\frac{|C| - \theta}{1.0 - \theta}\right) \cdot \text{conviction\_risk\_increment}$$
  - Zero continuous target churn or mid-trade decay liquidation during the life of the position.
  - Acceptance: A higher consensus vote at entry calculates a proportionally higher order quantity up to `max_level_risk` (5%).

- **S3: Hard Search Space Constraints (Mathematical & Domain Sanity)**:
  - **Positive Payoff Asymmetry**: $TP \ge 1.5 \times SL$ (no inverted payoff candidates allowed).
  - **Intraday ATR Multipliers**: $SL \in [0.75, 1.50\text{ ATR}]$, $TP \in [1.50, 3.50\text{ ATR}]$.
  - **Vertical Time Barrier**: $T_{\text{max}} \in [2\text{h}, 4\text{h}, 8\text{h}, 12\text{h}]$ (forces exit if trade does not resolve, preventing 26-hour capital lockup).
  - Acceptance: `SwarmPolicySpace::admits()` rejects any candidate violating these constraints.

- **S4: Adaptive Breakeven & Trailing Protection**:
  - **Breakeven Step**: When unrealized price excursion reaches $+1.0\text{ ATR}$ from entry, the Stop-Loss is raised to $\text{Entry Price} \pm \text{RoundTripFees}$ (zero-risk trade).
  - **Step Trailing**: At $+1.5\text{ ATR}$, trail stop to $+0.5\text{ ATR}$; at $+2.0\text{ ATR}$, trail to $+1.0\text{ ATR}$.
  - Acceptance: Unit tested in `SimulatedVenue`, `execution_core`, and `live_exits`.

- **S5: Direct Paper & Live Trading Gate Unlock**:
  - Because the strategy emits `OrderIntent` with self-contained barriers, `ensure_accepts_decisions()` passes.
  - Acceptance: `cargo run -p backtest --bin paper_trading -- --dry-run` and `--config <artifact>` starts without error.

- **S6: 100% Rust, Decimal Arithmetic & Zero Regressions**:
  - Strict compliance with `AGENTS.md`: `rust_decimal::Decimal`, `thiserror`, zero `unwrap` in production code.
  - `golden_execution` byte-identity passes unchanged for `dollar_bars_cusum` and `order_flow_imbalance`.

---

## Tasks & Phases

### Phase 1: Search Space Constraints & Policy Classifier (`crates/strategies`)
- [x] **T1.1 S1, S2**: Implement Classifier Mode in `ConsensusPolicy` (`crates/strategies/src/swarm_consensus/policy.rs`):
  - Add `OrderIntent` generator from consensus score $C$, entry price, and feed-1 ATR.
  - Sizing calculation based on $(|C| - \theta) / (1.0 - \theta)$.
  - Tests: `long_entry_triggers_on_threshold`, `sizing_increases_with_consensus`, `neutral_consensus_waits`.
- [x] **T1.2 S3**: Enforce hard constraints in `SwarmPolicySpace`:
  - `admits()` checks $TP \ge 1.5 \times SL$, $\text{time\_barrier} \le 12\text{h}$, and $SL \le 1.5\text{ ATR}$.
  - Grid includes `time_barrier_seconds: [7200, 14400, 28800, 43200]`.
  - Tests: `admits_rejects_inverted_payoff`, `admits_rejects_excessive_stop`.
- [x] **T1.3 S1, S5**: Update `SwarmConsensusStrategy` in `crates/strategies/src/swarm_consensus/mod.rs`:
  - Set `emits_targets() -> false` when running in classifier mode.
  - Tests: `the_swarm_declares_emits_targets_false`, `boxed_strategy_preserves_classifier_behavior`.

### Phase 2: Adaptive Breakeven & Trailing Stop (`crates/backtest`, `crates/ports`)
- [x] **T2.1 S4**: Add breakeven/trailing progression in `crates/backtest/src/execution_core.rs` and `live_exits.rs`:
  - Track `peak_price` during position lifetime.
  - Update `stop` to `entry_price` once `peak_price >= entry + 1.0 * ATR`.
  - Tests: `position_moves_to_breakeven_on_favorable_excursion`, `breakeven_prevents_loss_on_subsequent_reversal`.
- [x] **T2.2 S4**: Verify `SimulatedVenue` and `BinanceGateway` handle stop updates:
  - In `SimulatedVenue`: update working stop order without cancelling active position.
  - In `BinanceGateway`: `place_exits` can update existing stop leg cleanly.
  - Tests: simulated venue exit update tests.

### Phase 3: Golden Verification & Product Gate Integration
- [x] **T3.1 S6**: Run `cargo test --test golden_execution` to ensure 100% byte-identity for existing strategies.
- [x] **T3.2 S5**: Verify product gate:
  - Run `paper_trading` with a synthetic artifact to prove the Section 17.1 blocker is cleared.
  - Test: `paper_trading_accepts_swarm_classifier_artifact`.

### Phase 4: Production Pipeline & Cloud Batch Run 7
- [x] **T4.1**: Launch Run 7 in Google Cloud Batch with 1,000 candidates and the new Classifier + Triple Barrier space.
- [x] **T4.2**: Verify BigQuery telemetry, inspect trade durations (2h–12h), win rate, and drawdown.
- [x] **T4.3**: Update Research Logbook (`01-swarm-consensus-optimization-logbook.md`).
