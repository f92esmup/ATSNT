# ATSNT roadmap

Status against the gates of [`plan/implementacion-kraken-spot.md`](plan/implementacion-kraken-spot.md).
That document and [`plan/refactor-local-kraken.md`](plan/refactor-local-kraken.md) are the source of
truth; this page summarises them.

## Done

- **G0, plan approved.** Refactor batch 1, nine steps: golden tests of the swarm, architecture
  rules, the run recorder and clock ports, the library free of adapters, the accounting rules in
  `domain`, the tape store renamed, the cloud and Telegram code disconnected, the other strategies
  and the portfolio stack removed, and the Dockerfile and non-swarm configs retired.
- **Batch 2, in progress.** The economics screen is runnable (`scripts/gate_a.sh`). Unused
  configs and dependencies are gone. The metrics moved to `domain`. Clippy denies `unwrap` and
  `expect` outside tests. The run recorder writes to SQLite with `--record`. The GCP scripts are
  retired and the history is in [`archive`](archive/README.md).

## Gates

| Gate | What it decides | Who runs it | Status |
|---|---|---|---|
| GA | The economics screen: spot costs on the Binance perpetual proxy, long only, February to September 2026. It is a refutation: a positive result does not pass. | The owner, with `scripts/gate_a.sh` | Pending |
| GB | Whether the strategy survives twelve months of Kraken data under walk-forward and DSR (WP-08 to WP-12) | The owner and the agent | Not started; needs GA and the Kraken history depth |
| GP | Four weeks of paper trading, then real money only in tranches (WP-13 to WP-17) | The owner and the agent | Not started |

## Open work

- **WP-00, owner setup.** A Kraken spot key with no withdrawal rights. `.env` with mode 600. Allow
  `docs.kraken.com` in the network policy: the signing and checksum vectors are read from it.
- **WP-08 to WP-12.** The Kraken trade converter, the public REST backfill, the WebSocket recorder,
  the spot domain (currencies, money, spot rules) and the GB test. The recorder starts as soon as
  GA passes, because the order book cannot be rebuilt afterwards.
- **WP-13 to WP-17.** Request signing, the private REST and WebSocket clients, the spot gateway,
  simulation fidelity (WP-15), local operation, and paper and real trading in tranches.

## Decisions

- The swarm is the only strategy. The venue is Kraken spot in EUR, long only, with no leverage.
- Kraken Prop accounts take manual orders only, so they are outside the system.
- GA is a refutation. Its Binance proxy and window are not clean out-of-sample data.
- GB needs twelve months of Kraken tape, in line with the owner's requirement.
- The Binance execution code goes after GA, whatever its outcome. The downloader stays until the
  Kraken backfill replaces it.

## Known gaps

- Fees are the base tier of the owner's screenshot: 0.25 % maker and 0.40 % taker. `TradeVolume`
  replaces them when WP-13 lands.
- Simulation fidelity (queue position, walking the book) is WP-15.
- `parity_check`, `paper_trading` and `live_trading` speak Binance until WP-14 and WP-15.
