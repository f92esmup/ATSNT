# Archive

Documents kept for their history. They do not describe the system as it is now, and some of
them name components that no longer exist in the code.

- `SWARM_OPTIMIZATION_ROADMAP.md`: the roadmap of the multi-strategy system, from before the
  swarm-only refactor.
- `strategy/`: the specifications of the strategies removed in that refactor: dollar bars with
  CUSUM, order-flow imbalance, the Langevin potential well and the Ising phase transition. The
  swarm's specification stays in `docs/strategy/03-swarm-consensus.md`.
- `research/02-dynamic-topk-universe-selection-logbook.md`: the logbook of the universe selection
  for the portfolio stack that was removed.
- `architecture/07-web-telemetry-dashboard.md`: a design for a telemetry dashboard. No dashboard
  exists in the code.
- `deployment/`: the systemd units and the journald settings of the paper binary of the portfolio
  stack, and an alert unit that the removed GCP deployment script installed. The units name
  removed binaries; WP-16 writes the unit for the local machine.

The live documents are in `docs/` (see `docs/ROADMAP.md`) and in `docs/plan/`.
