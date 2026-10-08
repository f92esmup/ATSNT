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
- [ ] **T3.1 — Wire the CLI mode and Paper telemetry bridge.** Add opt-in `--paper`, keep `--mock` unchanged, reject the conflicting combination, and create an in-process session owner with a bounded trade-input boundary. Process supplied trades through `PaperTradingSession` and publish its telemetry through the existing web event path. Prove CLI separation and the end-to-end bridge with offline deterministic tests; do not add a runtime synthetic producer.
- [ ] **T3.2 — Close lifecycle and document usage.** Tie Paper-session task/channel shutdown to graceful server shutdown, await task completion/finalization, and document `--paper`/`--mock` behavior and that a market-data source is not wired yet.

## Routing and change forecast
- **T3.1:** delegated to one bounded `gentle-ai-worker` because the runtime/CLI/telemetry/test implementation spans multiple non-trivial files. Its verification is bounded to exact offline web-crate tests, fmt, and Clippy commands; the parent observes the initial CLI RED already recorded below.
- **T3.2:** delegated to one bounded writer because shutdown coordination and docs/tests span multiple files. Only one writer runs at a time.
- Command-running verification uses `gentle-ai-verify` per action when required by the native risk/verification plan; no parallel writers.
- Initial forecast was approximately 120–250 authored changed lines across the two work units. T3.1 now stages 260 authored lines. Reforecast T3.2 before its work-unit commit; if the accumulated branch or next-slice forecast crosses ~400 lines, apply the default `ask-on-risk` delivery strategy before committing. Keep behavior/tests/docs together; no push/PR is authorized.

## Allowed edit surfaces
- `crates/web/src/main.rs`
- `crates/web/src/lib.rs`
- `crates/web/src/paper.rs` (new, if the implementation needs a focused module)
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
- Run applicable offline checks for the web crate, formatting and Clippy; use an independent verifier for command-running verification as directed by native assessment. Record exact results and any skips.

## Work-unit commits
- Close each subtask only after its behavior and checks pass; keep tests/docs with the behavior they verify. Record each local commit identity below. No push/PR is authorized.

## Evidence
- Exploration handoff: `gentle-ai-explore` mapped CLI/startup in `crates/web/src/main.rs`, Paper session in `crates/backtest/src/paper.rs`, web event projection in `crates/web/src/state.rs`, and existing bridge tests in `crates/web/tests/api_tests.rs`. No tests/builds were run during exploration.
- Parent spot-check confirmed current CLI had only `--mock`; it selected `AppState`, discarded the mock producer handle, and passed only the server's graceful shutdown signal to Axum (`crates/web/src/main.rs`).
- **T3.1 RED observed:** parent added CLI acceptance/conflict tests and `gentle-ai-verify` ran `cargo test --locked --offline -p web cli_ -- --nocapture`; compilation succeeded, `cli_accepts_explicit_paper_mode` failed because `--paper` was unknown (5 passed, 1 failed). The conflict test's pre-implementation pass is not evidence of mutual exclusion because the flag itself was unknown.
- **T3.1 writer result:** `gentle-ai-worker` added CLI mode parsing, `PaperSessionOwner` with bounded input and event forwarding, and deterministic tests. Writer-reported GREEN: `cargo test --locked --offline -p web` passed (37 tests: 5 library, 7 binary, 25 integration); `cargo fmt --check` initially failed on one wrapping difference, then passed after correction; Clippy passed. No runtime producer/exchange source added. Writer reports preserving empty/unknown identity and fixture timestamps. Parent spot-check confirms bounded input, source-waiting startup, test metadata/timestamp assertions, and T3.2 remains: the paper handle/task is currently retained without graceful join/finalization.
- **Independent verification passed:** `gentle-ai-verify` task `muz9sc3r-4-8v9s` independently ran `cargo test --locked --offline -p web` (37 tests: 5 library, 7 CLI, 25 integration; doc-tests 0), `cargo fmt --check`, and `cargo clippy --locked --offline -p web --all-targets -- -D warnings`; all passed. It read all T3.1 changed paths and confirmed scope, mode separation, bounded input, fixture metadata/timestamps, and existing mock regressions. It did not assess T3.2. No new status change was reported.
- **Native assessment:** first ASSESS was unassessable because the new module and feature record were untracked. After staging only the four authorized paths, ASSESS succeeded: medium risk (`executable_change`), `reviewDue:false` (`under_budget`), runtime writer profile small, candidate not consumed. The small-model bias raised verification to high and required an independent verifier; the independent verification above passed. Native review inspect remains required after the T3.1 work-unit commit because RDD is on. No commit has been created yet.

## Status
- In progress: T3.1 implementation and writer/independent checks pass; staged work-unit commit and post-commit native inspect remain. T3.2 shutdown and docs pending.
- Commits: pending.
