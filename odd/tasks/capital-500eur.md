# capital-500eur — configuration and docs for a 500 EUR real account
Objective: make the engine, the HPO space and the docs fit the real budget (500 EUR ~ 562 USDT) and document how to scale it.
Branch: feat/sim-live-fidelity | Delivery: ask-on-risk | Runner: cargo test --workspace

## Specs
- S1: "pues dime la recomendación de parámetro que propones y habra que hacer muy holgados los hiperparámetros porque mi presupuesto actual es ese."
- S2: "Y hay que documentar muy bien como funciona esto para cuando mi presupuesto suba, poder ajustar todo adecuadamente."
- S3: Approved proposal: capital 562 USDT, risk_per_trade_pct 0.005, max_daily_drawdown_pct 0.03, notional caps ~3x equity (~1700 USDT), leverage cap 5x.
- S4: `min_take_profit_pct` becomes a hyperparameter [0.0025, 0.0035, 0.005, 0.007, 0.01]; `z_stop` widened up to 5.0; legacy space JSON without the new field keeps its old behaviour.
- S5: docs/capital-scaling.md: capital-dependent vs independent parameters, the cap formula `cap = risk_usdt / min_stop_pct`, a scaling table (500/1000/2000/5000 EUR), what to re-optimize, checklist to move up.
- S6: "podemos incluir en las tareas lo siguiente:" read-only audit of branches `docs/decouple-bi-consumers` and `feat/baseline-run-id` (worktrees under ATSNT-worktrees), report only, no push/merge/rebase/modify.

## Tasks
- T1 (S4) param space: min_take_profit_pcts + wider z_stop | inline | pending
- T2 (S3,S5) docs/capital-scaling.md + example artifact config for 500 EUR | inline | pending
- T3 (S6) audit of the two branches (read-only report) | inline | pending

## Log
- L1 (2026-10-17): "me parece bien tu propuesta.hagamoslo. Adicionalmente. podemos incluir en las tareas lo siguiente: <audit prompt for docs/decouple-bi-consumers and feat/baseline-run-id>"
- Note: time_barrier_bars stays [100] in the default space (existing deliberate decision and test); widen via the JSON file only if wanted.
