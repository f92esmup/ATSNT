# audit-remediation
Objective: resolve every finding of the read-only audit of 2026-10-18 (F1-F13 and alerts A1-A6), in the phased plan the user authorized.
Branch: fix/audit-remediation (from main) | Delivery: ask-on-risk | Test runner: `cargo test --workspace --no-fail-fast` (+ `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `bash scripts/tests/*.sh`)

## Specs
- S1 (F1, MAJOR): The ETL verifies each Binance archive against its published `.CHECKSUM` (SHA-256 of `https://data.binance.vision/...`) before converting it to Parquet. A mismatch fails that archive, removes the corrupt zip and is reported; it never produces a Parquet file.
- S2 (F2, MAJOR): Telegram sends never block the trading loop. Call sites enqueue into a bounded channel drained by its own task; when the channel is full the message is dropped and counted. Send failures never stop trading.
- S3 (F3, MAJOR): Operators are alerted on stream reconnection after 3 consecutive failures or more than 60 s without feed (A2), at most once per 10 min plus one on recovery.
- S4 (F3, MAJOR): A heartbeat is sent every 12 h with equity, open position and last-tick age (A4).
- S5 (F3, MAJOR): Live alerts an order rejection with order id and reason, deduplicated by order id (A3).
- S6 (A5): Paper trading alerts a circuit-breaker halt the same way live does.
- S7 (A6): An `account_ok` transition to false is alerted once, with the verification reason.
- S8 (A1): A systemd unit notifies a process crash, triggered by an `ExecStopPost=` hook on an unclean stop (rewritten after review R4-002 and verifier A3, see L11-L12: before systemd 254 `OnFailure=` under `Restart=always` only fires with a start limit, which can leave trading stopped for good; `ExecStopPost=` behaves the same on every version). Deployment change only; documented, not applied in code.
- S9 (F4, MINOR): `architecture_contract.rs` forbids `std::fs`, `std::net`, `tokio` and `reqwest` in `domain`, `strategies` and `ports`, and requires `ports` to contain only traits.
- S10 (F5, MINOR): Network I/O dependencies of the application crate `backtest` are moved to `adapters`, or each remaining one is an explicit, tested exception.
- S11 (F6, MINOR): The alias `ParameterSpace = strategies::DollarBarsCusumSpace` is removed; its uses name the concrete type or the trait.
- S12 (F10, MINOR): `rand` is a `dev-dependency` of `strategies` if production code does not use it.
- S13 (F8, MINOR): No `expect` in production helpers (`strategies/src/contract.rs:185`) and no `unwrap` in `adapters/src/gcp/telegram.rs:340`.
- S14 (F7 part, MINOR): `liquidation_price` exists once, in `domain`, with characterization tests proving the results are unchanged.
- S15 (F7, MINOR): `binance_gateway.rs` (4141 lines) is split by responsibility with no behaviour change; each extracted module is one commit.
- S16 (F11, MINOR): The README binary table lists every binary in `crates/*/src/bin` and `backtest`.
- S17 (docs): `docs/ROADMAP.md`, `docs/capital-scaling.md` and `docs/architecture/12-adding-a-strategy.md` are checked against the code; discrepancies are listed in the Log.
- S18 (F12/F13, INFO): The BigQuery DDL and the Rust row structs agree column by column. Retention and GCS lifecycle decisions wait for row counts; remote queries need the user's explicit authorization.
- S19 (rule, user): Commits use Conventional Commits and carry no Co-Authored-By or AI attribution lines (user CLAUDE.md overrides the reminder).

## Tasks
- T0 | S19 | inline | done: branch `fix/audit-remediation` from `main`; feature doc committed
- T1.1 | S1 | inline | done, RED observed: 9 compile errors on the missing API (`verify_archive_sha256`, `with_base_url`, `ChecksumMismatch`, `ChecksumUnreadable`)
- T1.2 | S1 | inline | done, GREEN: `.CHECKSUM` fetched first, archive verified before `ZipArchive::new`; a 404 on the checksum keeps the `HTTP 404` text so unpublished days are still skipped
- T1.3 | S1 | inline | done by existing behaviour: `fetch_one` skips when the Parquet exists and a Parquet is only written after verification. Limit: Parquet files written before this change were never verified; `--force` re-converts them
- T2.1 | S2-S5 | inline | done, RED observed: `operator_alerts` tests failed to compile on the missing API (16 errors) and `consecutive_connection_failures_are_published` on the missing `connection_failures` (1 error)
- T2.2 | S2 | inline | done: `operator_alerts::AlertQueue` (capacity 64, `try_send`, drop-and-count, 5 s flush at shutdown); every `send_alert(..).await` in `live_trading` and `paper_trading` now enqueues. Port `AlertNotifier::send_alert` returns a `Send` future (only implementor: `TelegramNotifier`)
- T2.3 | S3 | inline | done: `BinanceWebSocketStream::connection_failures()` (watch, reset on connect) + `FeedMonitor` (60 s / 3 failures / 1 per 10 min / one recovery), wired in live and paper
- T2.4 | S4 | inline | done: heartbeat via `AlertThrottle(12 h)`, first one at start (start notice); live and paper. Graceful stop notice added in both
- T2.5 | S5 | inline | done: live alerts `REJECTED` user-stream updates once per order id (`RejectionAlerts`, last 256 ids) and REST-refused entries (`EntryFailed`, no order id yet) at most once a minute
- T2.6 | S6, S7 | inline | done: paper alerts circuit-breaker trips. S7 already existed: `live_trading.rs` alerts the first `account_ok` drift (audit grep missed it)
- T2.7 | S8 | inline | done: `docs/deployment/atsnt-alert@.service` (verified with `systemd-analyze verify`), not installed. With `Restart=always` `OnFailure=` only fires with a start limit, documented in the unit
- T3.1 | S9 | inline | RED: extend `architecture_contract.rs`
- T3.2 | S10 | inline | move or document network dependencies of `backtest`
- T3.3 | S11 | inline | remove the `ParameterSpace` alias
- T3.4 | S12 | inline | `rand` as `dev-dependency` if unused in production
- T4.1 | S13 | inline | remove the production `expect`/`unwrap`
- T4.2 | S14 | inline | characterization tests, then single `liquidation_price`
- T4.3 | S15 | delegated | split `binance_gateway.rs`, one commit per extracted module
- T5.1 | S16 | inline | README binary table
- T5.2 | S17 | inline | docs-vs-code discrepancy list
- T6.1 | S18 | inline | DDL vs Rust structs, column by column
- T6.2 | S18 | pending | row-count queries: waiting for user authorization
- T6.3 | S18 | pending | retention and lifecycle: waiting for T6.2
- T7 | all | inline | full verification, independent verifier for T1 and T2 (high risk)

## Log
- L1 (2026-10-18, verbatim): "Quiero que resolvamos todos los asuntos que se han detectado en la auditoría. Ese es mi objetivo. puedes crear un plan metodico para abordarlo?"
- L2 (2026-10-18, verbatim): "Autorizo el plan. adelnate con las fases 0 y 1. al acabar preguntame las dudas pendientes"
- L3: Phase 0 done: branch created from `main` (clean tree, 535 tests green at baseline).
- L4: Phase 1 verification: `cargo test --workspace --no-fail-fast` 539 passed, 0 failed (535 + 4 new); `cargo clippy --workspace --all-targets -- -D warnings` clean; `scripts/tests/*.sh` pass; `cargo fmt --check` clean.
- L5: Open for the user after Phase 1: (a) an independent verifier for T1 (high risk) has NOT run yet; (b) whether Parquet files converted before this change should be re-verified with `--force`.
- L6: Native RDD review `review-fc3c5bc4b57eab98` (medium, lens reliability): approved and acknowledged. Advisories: R3-001 (`file.read` does not retry `ErrorKind::Interrupted`), R3-002 (no retry on the `.CHECKSUM` request; archives without a checksum are no longer converted).
- L7 (2026-10-18, user, verbatim): "si quiero la verificación independiente". Independent verifier on `e0c56ff`: S1 PASS, S19 PASS, no blockers. 8 probes passed (matching, uppercase hex, truncated archive, checksum 404 skipped as not archived, archive 404, checksum 403 counted as failure, temp zip removed, URL builders byte-identical to baseline). R3-001 judged out of domain; R3-002 matches baseline (archive GET had no retry either). Note: a daily archive whose `.CHECKSUM` returns 404 is skipped as "not archived"; a monthly one is reported as a failure. Parent spot check: tree clean, `cargo test -p adapters --lib binance_fetcher` 6 passed.
- L8 (2026-10-18, user, verbatim): "Pero los parquet se quedaron como efimeros en el batch. Creo que lo que dices no importa. Todo eso se puede purgar sin riesgo". Verified: `data/` is git-ignored and no code reads those files (paths in code are CLI defaults and string-only test fixtures). The 29 local Parquet files (5.4G) were deleted; `data/sample_trades.csv` kept. Question (b) of L5 is closed.
- L9 (2026-10-18, user, verbatim): "Adelante. completa la fase 2" — approving the proposed values: heartbeat every 12 h (equity, open position, last-tick age); stream alert after 3 failed reconnects or >60 s without data, at most once per 10 min plus one on recovery; one alert per rejected order, deduplicated by order id. Rate limits reuse the existing `crates/backtest/src/alert_throttle.rs`.
- L10: Phase 2 verification: `cargo test --workspace --no-fail-fast` 550 passed, 0 failed (539 + 11 new); `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --check` clean; `scripts/tests/*.sh` pass. Not verifiable here: the binaries' Telegram output end to end (needs a bot token and a live feed). Observed for later (Phase 6): the deployed `docs/deployment/atsnt.service` runs `paper_trading --config configs/hpo_btcusdt_2026.json`; with the deployment gate that artifact may be refused at start, which `Restart=always` would turn into a restart loop.
- L11 (2026-10-18, user, verbatim): "A1 y B1". Native reviews: `review-4779dd99dcb95278` (phase 2 slice, lenses resilience + reliability; escalated to high, item 4) and `review-2cebc3702898a442` (whole branch since `050ada9`, lens reliability), both approved and acknowledged. One reliability capture first failed on authority-lock contention (captures of both lineages ran at once) and passed when the same slot was relaunched alone. Advisories handled in a follow-up commit: R4-001 (a hung delivery held every later alert: 10 s delivery deadline, RED observed then GREEN); R3-001/R3-002 of A (alerts lost on an early error exit: `main` now owns the queue and flushes it on every path); R4-002 (start limit could leave trading stopped: `ExecStopPost=` hook instead, S8 rewritten); R3-001 of B (REJECTED alert lacked a reason: the futures user stream has none, the alert says so and names the status); R3-003 of B (EINTR while hashing: `io::copy`); R3-005/R3-004 (flaky closed-port test: owned listener that drops connections). Not changed: R3-004 of A (paper stop notice "reports an open position before liquidation": paper does not liquidate at shutdown, the notice is accurate; the console line "Liquidating open positions" is pre-existing and misleading, left as is); wiring of the binaries has no test (needs a refactor of the binaries, out of this phase).
- L12: Independent verifier on `c3e1d6c`: S2-S8 and S19 PASS, no blockers; 550 passed, clippy/fmt clean, `systemd-analyze verify` clean; probes passed (hung notifier, failing notifier, FeedMonitor boundaries, RejectionAlerts eviction, `connection_failures` resets to 0 after a local `accept_async` server comes up, trading logic untouched). Advisories acted on: A1 (a flapping feed alerted once per flap; the user-approved text is "como mucho uno cada 10 min": the repeat limit is no longer reset on recovery, RED observed then GREEN); A3 (my unit-file claim was wrong for systemd >= 254, where `RestartMode=normal` makes `OnFailure=` fire on every crash; verified in `man systemd.service` of systemd 261; text corrected, `ExecStopPost=` kept since it works on every version). A2: S5 cannot carry a reason for stream rejections (no field in `FuturesOrderTradeUpdate`); the alert says so. A4: the stop notice and the `EntryFailed` alert are new messages, no existing output changed. Follow-up suite before A1: 551 passed, 0 failed; `operator_alerts` 12 passed after A1.
