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

## Tasks

- [x] T1 S8: batch orchestrator preflights and retry policy, tests and docs. Route: delegated writer (parallel unit, disjoint surface `scripts/gcp`, `scripts/tests/batch_job_test.sh`, `docs/architecture/10-gcp-cloud-architecture.md`); the parent re-applied the grants. Commit 496e374. batch_job_test 38 -> 48 passed (RED 9 then GREEN).
- [x] T2 S1, S6: metrics (Sortino, profit factor, rejected orders), engine counter, callers, recording. Route: inline. Commit 34836a9, together with T4 because the optimizer does not compile without the metric types. RED: metrics_contract and the engine test did not compile.
- [x] T3 S7, S5 neighbours: strategy signal guard and grid neighbours. Route: inline. Commit d4f298b. RED: 4 failing tests, then GREEN with strategies at 26 passed.
- [x] T4 S2-S5: optimizer eligibility, fitness, stability, DSR, run_hpo selection, report and recording. Route: inline. Commit 34836a9. Workspace: 505 passed, 0 failed. Clippy -D warnings clean.
- [ ] T5 S9: docs, local comparison run, relaunch command. Route: inline.

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
