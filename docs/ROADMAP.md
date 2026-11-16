# ATSNT roadmap

Status against the gates of [`plan/implementacion-kraken-spot.md`](plan/implementacion-kraken-spot.md).
That document and [`plan/refactor-local-kraken.md`](plan/refactor-local-kraken.md) are the source of
truth; this page summarises them.

## Done

- **G0, plan approved.** Refactor batch 1, nine steps: golden tests of the swarm, architecture
  rules, the run recorder and clock ports, the library free of adapters, the accounting rules in
  `domain`, the tape store renamed, the cloud and Telegram code disconnected, the other strategies
  and the portfolio stack removed, and the Dockerfile and non-swarm configs retired.
- **Batch 2, done** (commits `69c3312` to `e915cb7`). The economics screen is runnable
  (`scripts/gate_a.sh`). Unused configs and dependencies are gone. The metrics moved to `domain`. Clippy denies `unwrap` and
  `expect` outside tests. The run recorder writes to SQLite with `--record`. The cloud scripts are
  retired and their history is in [`archive`](archive/README.md).
- **Cleanup, merged into `main` (`refactor/cleanup-local-venues`, 2026-11-07).** The Binance execution path, the paper, live and parity binaries, and `--bnb-discount` are removed (L3N, commit `940e4de`). The swarm sits under `usdm_perpetual/`, and the spot fees are the Kraken Pro entry tier.

- **Hexagonal consolidation (branch `claude/zen-mayer-keab4c`, 2026-11-16).** A new `application`
  crate holds the session core. The library reads no file and uses no `anyhow`. One pre-trade
  gate, `domain::EntryGate`, sizes and refuses entries for the bar engine and the session; the
  venue only executes. The swarm is a classifier only: the level machine and the whole
  target-position route are removed. It runs on spot, long only, and on perpetuals.

## Gates

| Gate | What it decides | Who runs it | Status |
|---|---|---|---|
| GA | The economics screen: spot costs on the Binance perpetual proxy, long only, February to September 2026. It is a refutation: a positive result does not pass. | The owner, with `scripts/gate_a.sh` | **NO PASA** (2026-10-31, `docs/plan/resultado-prueba-a.md`) |
| GB | Whether the strategy survives twelve months of Kraken data under walk-forward and DSR (WP-08 to WP-12) | The owner and the agent | Stopped: GA did not pass, so plan v4 stops the Kraken route |
| GP | Four weeks of paper trading, then real money only in tranches (WP-13 to WP-17) | The owner and the agent | Stopped with GA |

## Open work (on hold)

GA did not pass, so plan v4 stops the Kraken route. Nothing below is built until the owner decides otherwise.

- **WP-00, owner setup.** A Kraken spot key with no withdrawal rights. `.env` with mode 600. Allow
  `docs.kraken.com` in the network policy: the signing and checksum vectors are read from it.
- **WP-08 to WP-12.** The Kraken trade converter, the public REST backfill, the WebSocket recorder,
  the spot domain (currencies, money, spot rules) and the GB test. The recorder starts as soon as
  GA passes, because the order book cannot be rebuilt afterwards.
- **WP-13 to WP-17.** Request signing, the private REST and WebSocket clients, the spot gateway,
  simulation fidelity (WP-15), local operation, and paper and real trading in tranches.

- **Kraken branch, archived.** `feat/kraken-eur-spot-portfolio` is kept as the tag
  `archive/kraken-eur-spot-portfolio` (commit `b35e929`). It has five commits with an earlier Kraken
  WebSocket client (`kraken_ws.rs`), six EUR pair configs and a sweep CLI. It does not merge with the
  refactor: seven files conflict, all of them removed by it. The remote branch was deleted after the
  owner's confirmation (2026-11-07); the tag keeps its commits. See [`HANDOFF.md`](../HANDOFF.md).

## Decisions

- The swarm is the only strategy. The venue is Kraken spot in EUR, long only, with no leverage.
- Kraken Prop accounts take manual orders only, so they are outside the system.
- GA is a refutation. Its Binance proxy and window are not clean out-of-sample data.
- GB needs twelve months of Kraken tape, in line with the owner's requirement.
- The Binance execution code was removed after GA did not pass (L3N, commit `940e4de`). The
  downloader stays until the Kraken backfill replaces it.

## Known gaps

- Fees are the entry tier the owner reported on 2026-11-07: 0.40 % maker and 0.80 % taker. An
  earlier screenshot showed 0.25 % and 0.40 %. `TradeVolume` replaces them when WP-13 lands.
- Simulation fidelity (queue position, walking the book) is WP-15.
- Spot accounting is not modelled as a ledger: there is one cash balance (`equity`), no EUR balances
  and no fee tiers (WP-11). `ExecutionCore::spot_balances` derives the base and free quote from the
  position, but no engine path reads it. The lot and notional filters are one global set of defaults
  (0.001 minimum quantity and step, 5 minimum notional), not per-pair rules. An entry the margin rule
  cannot fund is dropped without a rejection count (`execution_core.rs:733-735`). There is no spread,
  latency defaults to 0 ms, and maker fills ignore queue priority.
- The spot defaults (`BacktestConfig::spot()`: 1 % risk, 1x leverage) size an entry whose stop is at
  or below 1 % to the whole balance, so its fee and slippage do not fit, except through lot rounding.
  The entry is dropped with no rejection count (`execution_core.rs:733-735`) and the report prints
  `Rejected Signals: none`. Until rejections are counted, a run with no trades under these defaults
  is a sizing artefact, not a strategy result.
- Bar and tick refusals agree by construction since both routes ask `domain::EntryGate` with the
  same `DailyBreaker`, and `tests/gate_parity.rs` pins it. The tick route used to report no
  refusals at all: it did not count them. The routes still fill differently. On the 90-day
  synthetic tape of the goldens the per-trade bar route closes 53 trades long only and the tick
  route 22. The cause is measured: the tick route sent 54 entries, 32 of them timed out and 22
  opened. It fills a market order only against prints of the opposite aggressor and cancels an
  entry that waited 30 s, and that tape has one print an hour. It is a property of the
  synthetic tape, not of the gate. Check the two routes on real tick data before real money.
- The strategy contract still has `input_kind`, `InputKind` and `BarPassThrough`, with no
  strategy that consumes dollar bars. Removing them means removing the dollar-bar mode of
  `backtest` and `run_hpo`, their flags and the `dollar_bar` sampling of artifacts: a decision
  for the owner.
- `SimClock` and the clock argument of `SessionDriver::on_idle` have no caller outside tests:
  no binary runs a live loop.
