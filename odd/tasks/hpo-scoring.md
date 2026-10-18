# HPO scoring and relaunch hardening

Objective: make the walk-forward HPO rank candidates by honest, measurable out-of-sample quality, and make the Batch relaunch fail fast instead of failing after the search.
Branch: `feat/hpo-scoring`. Delivery: ask-on-risk (forecast about 700 authored lines, a chain strategy is asked before the next commit beyond 400). Test runner: `cargo test --workspace`, `scripts/tests/*.sh`.

## Specs

- S1 Sortino without sentinels. Per-trade returns on entry equity (`pnl_net / entry_equity`). `sortino = mean(r) / sqrt(sum(min(r,0)^2) / N)` over all N trades. It is `None` (NULL in BigQuery) when undefined: no trades, or zero downside. `profit_factor` is `None` when there are no losing trades. This closes BI contract gap G5 ("Return NULL ... when the ratio is undefined; document the formula and its bounds").
- S2 Minimum trades. A candidate with fewer than N closed trades in any fold, in-sample or out-of-sample, is ineligible. N is configurable (`--min-trades-per-fold`, default 30). Ineligible candidates are recorded but never win. When no candidate is eligible, `run_hpo` fails with an explicit error.
- S3 Fitness from out-of-sample. `fitness = max(0, mean OOS sortino) * stability` when the candidate is eligible and its mean IS sortino is > 0; otherwise 0.
- S4 Real Deflated Sharpe Ratio (Bailey and Lopez de Prado 2014). SR_hat is the per-trade Sharpe of the candidate's concatenated OOS trades. T is the number of those trades. Skewness and kurtosis come from those returns. SR0 comes from the variance of SR_hat across all trials and from N trials.
- S5 Stability. The neighbours are the adjacent grid values of every one of the 6 parameters. `stability = clamp(mean(max(0, neighbour sortino)) / base sortino, 0, 1)` on fold 0's train slice. It is 0 when the base sortino is not positive. A neighbour without a defined sortino, or below the minimum trades, counts as 0.
- S6 Rejected orders are counted. `BacktestMetrics.rejected_orders` counts rejections per `RiskError` reason. It is recorded with the run metrics.
- S7 Strategy. Skip the signal when |z| >= z_stop, because the stop would sit on the wrong side of the entry. Skip it instead of using the 10% fallback stop when the stop would be <= 0.
- S8 Batch infra:
  - Before submitting, the orchestrator verifies that `atsnt-batch` has `bigquery.dataEditor` on the four write tables (when BigQuery is on).
  - Before submitting, the orchestrator verifies that the winner artifact object does not exist yet.
  - Batch retries only on Spot preemption.
  - The grants on the recreated tables are re-applied. User: "Sobre los permisos, debe haber una cuenta de servicio creada para ello. Porque no es la primera vez que lanzó el Job."
- S9 Docs: the HPO guide, capital scaling, the BI contract and the GCP architecture describe the new behaviour. A local comparison on one month runs before the relaunch.

- S10 (added under L4, "arregla todo con el objetivo de tener un resultado ... que tenga sentido"): the search space must be able to trade. Rolling windows span intraday horizons (100 to 1,600 $1M bars), and the rolling window update is O(1) so these windows stay affordable.

## Tasks

- [x] T1 S8: batch orchestrator preflights and retry policy, tests and docs. Route: delegated writer (parallel unit, disjoint surface `scripts/gcp`, `scripts/tests/batch_job_test.sh`, `docs/architecture/10-gcp-cloud-architecture.md`); the parent re-applied the grants. Commit 496e374. batch_job_test 38 -> 48 passed (RED 9 then GREEN).
- [x] T2 S1, S6: metrics (Sortino, profit factor, rejected orders), engine counter, callers, recording. Route: inline. Commit 34836a9, together with T4 because the optimizer does not compile without the metric types. RED: metrics_contract and the engine test did not compile.
- [x] T3 S7, S5 neighbours: strategy signal guard and grid neighbours. Route: inline. Commit d4f298b. RED: 4 failing tests, then GREEN with strategies at 26 passed.
- [x] T4 S2-S5: optimizer eligibility, fitness, stability, DSR, run_hpo selection, report and recording. Route: inline. Commit 34836a9. Workspace: 505 passed, 0 failed. Clippy -D warnings clean.
- [x] T6 S10: O(1) RollingZScore (ecc8e28, equivalence test against the two-pass statistics over 5,000 prices) and window grid [100, 160, 250, 400, 650, 1000, 1600] (RED on the grid assertion, then GREEN). Workspace: 507 passed.
- [x] T5 S9: docs (04, 10, 13 -> contract v2, capital-scaling), local comparison (L7, L8), relaunch (L9-L11). Route: inline.
- [x] T7 (found in v2 results): DSR over eligible trials only, plus a fitness tie-break by OOS Sortino. Commit fce2b78; workspace 509 passed.

## Log

- L1 (2026-10-18) User: "Adelante investiga exhaustivamente y preparamos el relanzamiento" (after the failed job `hpo-btcusdt-2026-500eur`: BigQuery 403, 3 Batch attempts, winner with sentinel Sortino 100/60 and DSR p=0).
- L2 (2026-10-18) User: "Sobre los permisos, debe haber una cuenta de servicio creada para ello. Porque no es la primera vez que lanzó el Job. Con respecto a los otros. Se exhaustivo y sigue la filosofía del proyecto. Pero arréglalos todos".
- L3 Evidence:
  - The job ran as `atsnt-batch`. Its table-level `bigquery.dataEditor` grants were lost when Phase 1 recreated the tables: all four policies are empty (etag ACAB).
  - `maxRetryCount` 2 re-ran a deterministic failure.
  - Scoring audit:
    - fitness used IS only (optimization.rs:258);
    - the DSR was fed the mean OOS Sortino and the total bar count (opt.rs:263);
    - Sortino 100 is a sentinel (metrics.rs:238);
    - stability perturbed 2 parameters and returned 1.0 on flat sentinels;
    - sizing rejections were silent (engine.rs:524-533).
- L4 (2026-10-18) User: "Vale, ya sabes mucho de como tomo las decisiones. Así que arregla todo con el objetivo de tener un resultado (después de ejecutar el hpo) que tenga sentido. Me voy a dormir. Así que quiero que ininterrumpidamente tomes decisiones y luego me las presentas para verlo por la mañana. Eres capaz de llevar a cabo la refactorizaxuon y fix solo?"
  - Decision: work autonomously, record every decision here, and relaunch the HPO from the feature branch with a new job name. Push and merge stay with the user.
- L5 Grants re-applied: `bq add-iam-policy-binding` dataEditor for atsnt-batch on runs, run_metrics, trades and run_series. The read-back shows the SA on all four.
- L6 Decisions taken while the user was asleep (each to be reviewed):
  - The Sortino return is `pnl_net / entry equity`, not `return_pct` on notional. Sizing risks equity, so this is the return the account sees.
  - A fold with an undefined Sortino counts 0 in the fold mean. It is conservative, and it is practically impossible once a fold has 30 or more trades.
  - `mean_oos_profit_factor` is pooled (sum of profits over sum of losses across folds), not a mean of ratios. The field name is kept for the BigQuery and artifact consumers.
  - Stability is skipped (0) for ineligible candidates. It costs up to 12 backtests per candidate.
  - The DSR keeps its 5% threshold, but significance does NOT gate the winner. It is reported, so a non-significant winner is visible but still deployable by the user's decision.
  - `rejected_orders` is a new nullable JSON column in run_metrics, written only when non-empty, like `halted`. The DDL has an idempotent ALTER.
  - Delivery: the branch passed about 400 authored lines; ask-on-risk would ask for a chain strategy. The user is asleep, so the commits stay on `feat/hpo-scoring` as logical units, and the PR/slicing decision is left to the user.
  - The RDD consent for these commits cannot be given while the user is asleep. It is recorded as pending, not skipped.
- L7 Local comparison (BTCUSDT 2026-01, 24 candidates, old window grid 14-40): 0 of 24 candidates were eligible, and most made 0 OOS trades. The old 100 cap had hidden this.
  - Probe on 2026-01 with a fixed config (vol 1.5, Ze 2.0, Zs 3.5, Tb 1000), trades per window and take-profit floor:
    - w20: 4 / 0
    - w100: 34 / 1
    - w400: 28 / 20
    - w1600: 42 / 53
  - Cause: the target (rolling mean of 14-40 bars, 2-5 min) sits closer than min_tp (0.25%-1%).
  - Every probe lost about 15% in January, which is the monthly kill switch. The strategy may have no edge, and the HPO must say so honestly.
  - Decision: rescale the window grid to day-trading horizons (S10), and make the rolling variance O(1) so the HPO time does not grow with the window.
- L8 Local Jan-Feb sweep with the new space (40 candidates, min 10 trades per fold, 4 cores, 11.6 min):
  - 12 of 40 candidates were eligible, with the weakest fold at 19-63 trades.
  - Every one loses: OOS per-trade Sortino about -0.4 to -0.6, DSR p 1.
  - Backtests of 3 top configs on Jan-Feb: net about -85 USDT, which equals fees (~40) plus slippage (~42); the pre-cost PnL is about 0. Win rate is about 29%. All runs hit the monthly kill switch (-15%).
  - Conclusion: the strategy has no gross edge at $1M bars, so no cost assumption makes it profitable.
  - Decision: run_hpo still writes the best eligible candidate with its honest scorecard, so the holdout backtest gives the Sep-Oct evidence. A deployment gate in paper/live (refuse fitness 0 or a non-significant DSR) is left as a recommendation, because it is a product decision for the user.
- L9 Relaunch: job `hpo-btcusdt-2026-500eur-v2`, image 780661f91b86, submitted 23:16 UTC. Both new preflights passed live (artifact absent; table-level grants on the 4 tables).
- L10 Job v2 (`hpo-btcusdt-2026-500eur-v2`, HPO run 01a14c33-e1ef-...): SUCCEEDED in 13 min, all sinks OK, no retries.
  - 25 of 120 candidates were eligible, and none had a positive OOS Sortino.
  - BigQuery showed ineligible trials with 2 OOS trades at a per-trade Sharpe of 15-22 and p = 0. They inflated V[SR], so every other trial got p = 1.
  - With every fitness at 0, the winner was arbitrary (OOS -0.62, while -0.32 existed). Both are fixed in fce2b78.
- L11 Job v3 (`hpo-btcusdt-2026-500eur-v3`, HPO run 01a14c45-c73e-747c-9d4e-f8a97002c1cd): SUCCEEDED, 25/120 eligible.
  - Winner (least bad): W:1600|V:0.8|Ze:1.7|Zs:5.0|Tb:2000|MinTp:0.005, OOS Sortino -0.32, 778 OOS trades, weakest fold 100, DSR p 1.
  - The whole top 5 sits at the grid edge W = 1600, and Tb = 2000 recurs.
  - Holdout Sep-Oct 17: 155 trades, win rate 51%, net -85.00 USDT; the monthly kill switch tripped. Fees 59.10 and slippage 61.64, so the pre-cost PnL is +35.74 (+0.23 per trade, against 0.78 per trade in costs). Monte Carlo P95 drawdown is 22.7% and the probability of ruin 0.01%.
  - Verdict: there is a small gross edge, but costs are about 3.4 times it. Not deployable. Artifact: gs://atsnt-lake-mi-facturador-bot-01/artifacts/hpo_btcusdt_2026-hpo-btcusdt-2026-500eur-v3.json.
  - Sensitivity, computed by hand from the holdout figures (not simulated):
    - slippage at 0.01% per side: net about -35;
    - plus maker entries (0.02%): net about -17;
    - so a cost cut alone does not make it profitable.
- Next steps (the user's decisions):
  - (a) Deployment gate in paper/live: refuse an artifact with fitness 0 or a non-significant DSR. The pipeline banner still says "Next Production Step: Run real-time paper trading".
  - (b) Calibrate slippage with real fills (phase 4) before any further HPO.
  - (c) Strategy direction: maker entries, or a longer horizon (the optimum sits at the grid edge).
  - (d) Push/PR of feat/hpo-scoring (about 1,600 changed lines in 15 commits; slicing to be decided).
  - (e) RDD review consent for the branch.
- L12 RDD review (user granted consent):
  - Lineage review-6a4a483426ff051a, four lenses, approved and acknowledged; authority burned.
  - Risk: no findings.
  - Advisory findings, all non-blocking:
    - Readability R2-001..R2-010.
    - Reliability R3-001 (analytics.rs:127-130).
    - Resilience R4-001: a run_metrics insert with rejected_orders fails on a dataset without the DDL. The deployed dataset has the DDL; a column preflight in the Batch launcher is follow-up work.
    - Resilience R4-002: retrying only on 50001 also stops retrying transient sink 5xx errors after the search.
