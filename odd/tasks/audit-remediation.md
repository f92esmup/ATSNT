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
- S8 (A1): A systemd `OnFailure=` unit notifies a process crash. Deployment change only; documented, not applied in code.
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
- T2.1 | S2 | inline | RED: a notifier that sleeps 10 s does not delay the loop
- T2.2 | S2 | inline | GREEN: bounded channel plus drain task; drop-and-count when full
- T2.3 | S3 | inline | reconnection alert with rate limit
- T2.4 | S4 | inline | 12 h heartbeat
- T2.5 | S5 | inline | order rejection alert, deduplicated by order id
- T2.6 | S6, S7 | inline | paper halt parity and `account_ok` transition alert
- T2.7 | S8 | inline | `OnFailure=` unit file under `scripts/` (documented, not deployed)
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
