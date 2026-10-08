# T3 — Explicit In-Process Paper Session

## Goal
Complete only M5/M6 task T3: add an explicit `--paper` runtime that owns an in-process `PaperTradingSession`, keeps `--mock` distinct, routes supplied paper trades into web telemetry, and shuts down cleanly.

## Authorization and decisions
- The user authorized focusing implementation on T3 and its subtasks; T4–T7 and W0–W6 remain out of scope.
- The user's selected runtime behavior is **Paper waits for a source**: `--paper` creates and owns the Paper session but does not start an automatic synthetic trade producer. Deterministic synthetic trades are test fixtures only. No exchange connection or live exchange call is part of T3.
- `--mock` remains the existing direct synthetic-telemetry demo mode; enabling both `--mock` and `--paper` must be rejected.
- Preserve unrelated changes and the retained T2b stash. Do not push, merge, remove branches/worktrees, or touch unrelated M5/M6 tasks.
- Work began from clean `main` at `3036c49` and is isolated on `feature/t3-paper-session`.

## Engineering constraints
- Rust financial values use `Decimal`; no production `unwrap()`/`expect()`; use bounded async channels; tests remain deterministic/offline.
- Do not fabricate strategy/symbol identity or timestamps. Actual Paper events use the session's configured identity and event timestamps; unknown inputs stay absent/unknown.
- Preserve T1/T2a/T2b compatibility and current mock behavior.

## Subtasks
- [x] **T3.1 — Wire the CLI mode and Paper telemetry bridge.** Add opt-in `--paper`, keep `--mock` unchanged, reject the conflicting combination, and create an in-process session owner with a bounded trade-input boundary. Process supplied trades through `PaperTradingSession` and publish its telemetry through the existing web event path. Prove CLI separation and the end-to-end bridge with offline deterministic tests; do not add a runtime synthetic producer.
- [x] **T3.2 — Close lifecycle and document usage.** Tie Paper-session task/channel shutdown to graceful server shutdown, await task completion/finalization, and document `--paper`/`--mock` behavior and that a market-data source is not wired yet.

## Routing and change forecast
- **T3.1:** delegated to one bounded `gentle-ai-worker` because the runtime/CLI/telemetry/test implementation spans multiple non-trivial files. Its verification is bounded to exact offline web-crate tests, fmt, and Clippy commands; the parent observed the initial CLI RED.
- **T3.2:** delegate to one bounded writer because shutdown coordination and docs/tests span multiple files. Only one writer runs at a time.
- Command-running verification follows the native risk/verification plan; no parallel writers.
- T3.1 committed 260 authored lines. Forecast T3.2 from the worker's scoped implementation plan before its commit; if accumulated branch lines or the next-slice forecast crosses ~400 lines, apply the default `ask-on-risk` delivery strategy before committing. Keep behavior/tests/docs together; no push/PR is authorized.

## Allowed edit surfaces
- `crates/web/src/main.rs`
- `crates/web/src/lib.rs`
- `crates/web/src/paper.rs`
- `crates/web/tests/api_tests.rs`
- `docs/architecture/07-web-telemetry-dashboard.md`
- `odd/tasks/milestone-5-6-closure.md` (only to record T3 completion after all acceptance checks pass)
- `odd/tasks/t3-paper-session.md`
- `crates/web/Cargo.toml` only if an already-available workspace dependency cannot support the required channel/session boundary

Do not edit other paths without first reconciling the task scope.

## Verification and acceptance
- For each behavioral work unit, add/observe a meaningful deterministic RED test before implementation, then GREEN and focused regression checks.
- CLI: default mode remains non-mock/non-paper; `--mock` remains separate; `--paper` is explicit; `--mock --paper` fails before server startup.
- Runtime: `--paper` owns one in-process Paper session and waits for a trade source; injecting a deterministic test trade traverses the Paper session and updates the web telemetry state with the real fixture's metadata/timestamps.
- Lifecycle: graceful shutdown closes the paper input, finalizes/joins its task without detached work or a hang, and leaves mock behavior unchanged.
- Documentation names the invocation and explicitly states that no market-data source is connected by this task; no live exchange calls are used.
- Run applicable offline checks for the web crate, formatting and Clippy; use an independent verifier when the native assessment requires it. Record exact results and any skips.

## Work-unit commits
- [x] **T3.1:** `082b24c` (`feat(web): add explicit paper session telemetry`) — CLI/session bridge and tests; writer and independent verification passed; native medium/under-budget assessment and review lineage `review-77257ba3225fb0bd` were approved and acknowledged. Advisory `R3-paper-shutdown` at `crates/web/src/main.rs:88` was informational/non-blocking and is addressed by T3.2.
- [ ] **T3.2:** pending; keep shutdown behavior, tests and invocation docs in its work unit. No push/PR is authorized.

## Evidence
- Exploration handoff: `gentle-ai-explore` mapped CLI/startup in `crates/web/src/main.rs`, Paper session in `crates/backtest/src/paper.rs`, web event projection in `crates/web/src/state.rs`, and existing bridge tests in `crates/web/tests/api_tests.rs`. No tests/builds were run during exploration.
- Parent spot-check confirmed the original CLI had only `--mock`; startup discarded the mock producer handle and passed only the server's graceful shutdown signal to Axum.
- **T3.1 RED:** `cargo test --locked --offline -p web cli_ -- --nocapture` compiled and failed `cli_accepts_explicit_paper_mode` because `--paper` was unknown (5 passed, 1 failed). The conflict test's initial pass was not evidence of mutual exclusion because the flag itself was unknown.
- **T3.1 GREEN and independent checks:** worker and separate verifier both passed `cargo test --locked --offline -p web` (37 tests: 5 library, 7 binary, 25 integration; doc-tests 0), `cargo fmt --check`, and `cargo clippy --locked --offline -p web --all-targets -- -D warnings`. Worker initially observed a formatting failure and corrected it before final GREEN. Parent read `paper.rs` and the startup path, confirming bounded input, no runtime synthetic producer, source-waiting behavior, and fixture metadata/timestamp assertions.
- **Native assessment/review:** the uncommitted ASSESS first failed because files were untracked. After staging the exact four T3.1 paths, ASSESS reported medium risk (`executable_change`), `reviewDue:false` (`under_budget`), runtime writer profile `small`; its verification plan required the independent verifier, which passed. Post-commit ASSESS of `082b24c` against `3036c49c84712ed7fdee5254182369ff6b735bb3` returned the same medium/under-budget result. Native review closed approved and acknowledged; its sole warning was `R3-paper-shutdown`, informational and assigned to T3.2.
- **T3.2 implementation:** `PaperSessionRuntime` retains the bounded input and task; after Axum shutdown (also on bind/serve error), shutdown drops input, drains queued trades, and awaits a consuming `finish(None)` result. No forced liquidation or fabricated final event; `--mock` is unchanged. Architecture docs now show `cargo run --locked --offline -p web -- --paper` and the not-yet-connected source limitation.
- **T3.2 RED:** `cargo test --locked --offline -p web` failed with E0433 at both new shutdown tests because `PaperSessionRuntime` did not exist. Tests require joined finalization metrics, not merely channel closure.
- **T3.2 GREEN/triangulation:** the same test command passed all 40 tests (8 library, 7 binary, 25 integration; 0 doc-tests), including queued-input draining, source-waiting shutdown, and open-position finalization without liquidation or a final event. `cargo fmt --check` initially failed on the shutdown signature; after the targeted formatting correction, it passed. `cargo clippy --locked --offline -p web --all-targets -- -D warnings` passed. Parent review and work-unit commit remain pending.

## Status
- T3.1 complete: commit `082b24c`, offline tests/fmt/Clippy passed, independent verification passed, native review approved and acknowledged.
- T3.2 implementation and required verification complete; parent review disposition and work-unit commit pending. No closure-tracker edits or staging/commits performed.
