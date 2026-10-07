# Web Workspace Contract Documentation

## Goal
Maintain the canonical `docs/` contract and phased plan for ATSNT's reusable, read-only trading and research workspace. Record confirmed product decisions before any later W0-W6 implementation, while avoiding overlap with active T* work.

## Authorization and boundaries
- The user previously authorized a separate linked worktree for the web-workspace documentation feature.
- This feature is based on committed snapshot `c7ae8f4107b9214a3c7f4fc20193393b65da617c` on branch `docs/web-workspace-contract` in `/home/f92esmup/Projects/ATSNT-web-workspace-contract`.
- The user has now authorized local integration into `main` and a push of `main` to `origin` only after T2b Slice 2 is independently verified/committed, the `docs/07` overlap is reconciled, and the canonical W documentation is integrated and checked. This permits documentation edits and local commits/merges within those gates; it does not authorize application code, W0-W6 implementation, PR creation, or live exchange access.
- Do not edit application source, `odd/tasks/milestone-5-6-closure.md`, or any T* implementation task.
- T2b is complete on `feature/m5-6-reliability-gateway`, with code/test/docs commit `43d85bb` and T-only tracker commit `ea80f62`. Reconcile the W copy of `docs/architecture/07-web-telemetry-dashboard.md` with the committed T2b content; preserve the feature branch's T-only milestone tracker and do not replace it with the W branch's base-snapshot copy.
- This isolated branch has pre-existing changes in `docs/architecture/07-web-telemetry-dashboard.md`; the user selected to defer reconciliation until T2b completion. That gate is now clear. Reconcile the two documented states before integrating this branch, preserving W's factual API/design notes and T2b's snapshot-fidelity, sticky-uncertainty, backlog-discard and no-authoritative-resync contract.
- Do not add order/strategy controls, experiment submission, or claim live capabilities not evidenced by available sources.
- Local documentation commits and local branch integration are authorized after reconciliation and independent checks. Push only `main` after the full documentation integration and final reachability/status checks. Remove auxiliary worktrees/branches or a redundant stash only after confirming intended content is reachable. No PR or W0-W6 implementation is authorized.
- Write product and architecture documentation in English, consistent with existing repository docs; conversation remains Spanish.

## Current T* coordination context
- T2b is complete in commit `43d85bb`. Independent verification passed `cargo test --locked --offline -p web` (32 tests: 3 library, 4 binary, 25 integration), `cargo fmt --check`, `cargo clippy --locked --offline -p web --all-targets -- -D warnings`, and `git diff --cached --check`. The verifier observed GREEN only; the earlier worker's RED/GREEN report is separate evidence. T3-T7 remain unstarted per the user's report.
- The user selected to preserve/verify T2b first and defer `docs/07` reconciliation until afterward. The reconciliation is complete in `6ffe50f`, and the W/T branches are integrated in verified merge `67542cb`. Local `main` was fast-forwarded through `f1b0922` to `856511a`; the push succeeded and an immediate fetch confirmed `origin/main` matched `856511a`.
- Keep the M5/M6 task status and T4-T6 gateway scope in `odd/tasks/milestone-5-6-closure.md`; do not carry forward or restore the W branch's stale base-snapshot tracker as current.
- Preserve the base-snapshot note below and distinguish historical task status from current implementation status.

## Evidence and baseline to preserve
- The existing milestone task tracker contains a proposed W0-W6 plan. W2 consumes T2b/T3 for telemetry resilience and Paper runtime; live account state additionally depends on T6 sources. W3 depends on W1/W2; W4 can proceed independently of Paper runtime; W5 follows W4; W6 must coordinate with T7.
- At the documentation branch's base snapshot `c7ae8f4`, T1/T2a are recorded complete and T2b/T3 pending. This is historical evidence only; the shared T2b worktree has since advanced. T4-T6 are separate gateway work and are not all prerequisites for a read-only Paper dashboard.
- Portal and Econoweb were not found in this repository. The user now confirms a separate-app topology with Portal as a launcher; the location/ownership of the other applications and shared package remains a future implementation detail.
- Keep code facts, confirmed user decisions, proposed architecture, and task-tracker status distinct; note the snapshot used.

## Tasks
- [x] Map current docs and T/W ownership. Read-only explorer reviewed current documentation/code; parent verified clean base HEAD and a single shared worktree before creating the isolated branch.
- [x] Author canonical contract and reconcile references across `docs/`. The writer created the W0-W6 contract and five source-backed documentation updates; initial `git diff --check` passed.
- [x] Normalize machine-specific source links in docs 07/08 to repository-relative links. The writer converted eight confirmed in-repository source links, verified all six targets exist and are tracked, preserved labels and the `#L16` anchor, and found no remaining machine-local ATSNT file URLs.
- [x] Independently verify the initial documentation set, local links, scope boundaries, tracked/untracked whitespace and `git diff --check`. Final verifier passed all six checks and certified all eight relative links; parent reran whitespace checks.
- [x] Record the newly confirmed product decisions in `docs/architecture/09-read-only-web-workspace.md`. The writer updated the W0 decision register and affected navigation, Operations, Laboratory, API-principles, phase-deliverable and T/W-crosswalk sections; only the canonical document was edited.
- [x] Independently verify the amended canonical contract for accurate decision coverage, consistency with the existing W0-W6 plan, scope boundaries, link validity, and whitespace. Independent verifier returned PASS across decision coverage, risk, replay, freshness, W/T crosswalk, source claims, scanability and scope. `git diff --check` passed; explicit whitespace checks found no matches. No application tests/builds run.
- [x] Reconcile the W branch's `docs/architecture/07-web-telemetry-dashboard.md` with T2b commit `43d85bb`: preserve factual API corrections, historical/design boundaries and relative links; include `stale`, snapshot fidelity, sticky uncertainty, backlog discard, continued aggregation and the absence of a producer-authoritative resync source; replace the stale “T2b incomplete” claim. The reconciled W source documentation is commit `6ffe50f`.
- [x] Update the canonical contract's current-state/reconnect wording and this task record to reflect T2b completion while preserving `c7ae8f4` as historical evidence and T3 as pending. The canonical-document correction is included in `6ffe50f`; ODD task-plan baseline is commit `bd24f8c`.
- [x] Independently verify the combined documentation, source claims, relative links, changed paths and whitespace; no code tests/builds were applicable. Verifier PASS; one pre-existing `docs/05:5` trailing space matches HEAD and was left unchanged.
- [x] Commit W documentation in reviewable local work units (each within the review-size budget) and record commit identities. Source-doc commit `6ffe50f` contains 6 files (313 insertions, 15 deletions); ODD plan commit `bd24f8c` and task-status update `e7b92e0` are separate.
- [x] Integrate the T feature branch into the docs branch after the W work units are committed; resolve/check any `docs/07` overlap and confirm the T-only milestone tracker is preserved. Merge commit `67542cb` has W HEAD `e7b92e0` and feature tip `5f3f522` as parents; independent verification found a clean worktree, no unresolved paths, and the exact T-only tracker.
- [x] Integrate the checked docs branch into local `main`, verify all intended commits are reachable and the final tree/status, then perform the user-authorized push to `origin/main`. Local `main` is at `856511a`; all intended commits passed ancestry checks, the remote preflight was a fast-forward, `git push origin main` succeeded, and a post-push fetch confirmed local and `origin/main` matched.
- [ ] Remove auxiliary local worktrees/branches and drop the T2b stash only after verifying all intended content is committed/reachable and the stash is redundant; leave only local `main`. In progress: verify stash redundancy and full reachability before cleanup.

## Confirmed agreement to document
- Topology: Portal, Trading, and Econoweb remain separate applications with a shared, versioned shell/design system. Portal is an app launcher, not a cross-product KPI dashboard.
- Deployment and locale: local, single-user use; Spanish and English UI; display UTC and local time. Do not invent a multi-user authentication system.
- Account contexts: one Binance identity with separate Spot and USDⓈ-M Futures wallet contexts. The selected context controls the global view; do not combine the wallets or imply Spot assets are Futures collateral. User's agreed Futures profile is USDⓈ-M, USDT, isolated, one-way; ATSNT is the sole order sender for that Futures profile.
- Spot valuation: value all Spot assets in USDT for wallet equity/risk with fresh reference prices; show source and age; if valuation input is not reliable, present aggregate equity/risk as unknown. This is not USD-M Futures available margin.
- Risk: caps of 10,000 USDT notional/order and 50,000 USDT notional/position; 1% risk per trade from current selected-wallet equity; 5% session drawdown from its high-watermark. Strategies share a budget within the same wallet and environment; Backtest/Paper/Live environments do not pool one another's session state. Persist the session watermark across process restarts until explicit session close; no daily reset. Compute drawdown from authoritative session-equity evidence; exact PnL, fees, and funding semantics must follow the versioned engine/report contract and must not be invented in the browser.
- Modes and bars: `--paper` is explicit and distinct from `--mock`. Mock uses the same Operations panel with a persistent DEMO label and synthetic values never represented as exchange account values. The engine supplies already formed Dollar Bars; the browser does not form them.
- Strategy/market compatibility: Spot means standard Spot; every strategy declares compatible market type; `DollarBarsCusum_v1` is Futures-only and is rejected in Spot. T6 private account events cover Spot and USD-M Futures.
- Laboratory: saved replay supports bar-level and engine-event views. Persist formed bars and engine events (signals, order/fill lifecycle and resulting position/equity evidence); do not require raw trade/tick archives for v1. Do not launch/recompute experiments from the read-only UI. Display data that reports actually contain; never fabricate replay evidence.
- KPIs: use a standard quantitative summary (Backtest: net return, max drawdown, Sharpe/Sortino, profit factor, expectancy, win rate, trade count and costs; HPO: objective and fold/out-of-sample/parameter stability; Monte Carlo: ending-equity and drawdown percentiles, with ruin probability only when its threshold/method are defined in report evidence). Exact metric formulas and annualization must come from versioned engine/report evidence; do not recompute incompatible metrics in the browser.
- Freshness: display source/connection heartbeat separately from age of the last event and last formed bar. Use configurable thresholds per source; no new Dollar Bar alone means stale. Numeric thresholds must follow each source/producer heartbeat contract, not be guessed globally.
- Operations is read-only: no order mutation, strategy controls, session-close/reset controls, or experiment submission in the web UI.

## Allowed edit surfaces
### Current reconciliation and integration surfaces
- `docs/architecture/07-web-telemetry-dashboard.md` — reconcile the W-branch copy with committed T2b semantics.
- `docs/architecture/09-read-only-web-workspace.md` — correct current-state and reconnect claims without changing product decisions.
- `docs/ROADMAP.md`, `docs/architecture/05-monte-carlo-and-telemetry.md`, `docs/architecture/06-paper-trading-and-realtime-execution.md`, and `docs/architecture/08-live-execution-gateway.md` — preserve and integrate existing source-backed W documentation; edit only if needed for factual consistency.
- `odd/tasks/web-workspace-contract-docs.md` — this feature's ODD task record.

### Explicitly out of scope
- Application code, `odd/tasks/milestone-5-6-closure.md` on the T branch, all T3-T7 implementation, W0-W6 UI implementation, exchange credentials/live calls, PR creation, and unapproved remote branch deletion.

## Verification plan
- Run `git diff --check` for tracked documentation changes in the isolated worktree.
- Check trailing whitespace in `docs/architecture/09-read-only-web-workspace.md` and this task file, including untracked content.
- Independently compare the decision register and affected requirements against every item in “Confirmed agreement to document”; resolved choices must not remain described as unanswered product questions. Preserve truly open technical questions such as package ownership/release flow, report-schema availability, and source-specific numeric heartbeat intervals.
- Verify documentation changes only; preserve the T-only milestone tracker and base-snapshot qualification, and update stale present-tense T2b claims in the W document/task records.
- Check any added in-repository links relative to the canonical document. Existing link verification from the initial pass remains historical evidence; rerun if any existing link targets are changed.
- No Rust/browser tests or builds are applicable to this documentation-only task. Do not run them.
- Local work-unit commits and integration are permitted only after reconciliation and independent checks. Before push, verify the final `main` tree and commit reachability, fetch `origin` and confirm no divergence, and push only `main`; cleanup only after reachability checks.

## Initial pass verification evidence
- Independent final verifier: PASS for allowed paths, W0-W6 completeness, T/W ownership, fact/proposal separation, local link resolution, and visual/read-only boundaries.
- All eight converted repository links resolved; all six distinct targets were tracked; no machine-local ATSNT file URL remained in docs 07/08; `#L16` was preserved.
- Initial `git diff --check` and untracked-file trailing-whitespace checks passed.
- Native ASSESS was unassessable because the canonical contract was untracked; its returned independent-verifier requirement was satisfied by the final PASS.
- Rust/browser tests and builds were not run; initial work was documentation-only.
- Initial isolated branch contained five modified existing docs, the canonical contract, and this task tracker. No app source or T* tracker changed.

## Prior documentation-pass evidence (historical)
- Branch/worktree: `docs/web-workspace-contract` at `/home/f92esmup/Projects/ATSNT-web-workspace-contract`, based on `c7ae8f4107b9214a3c7f4fc20193393b65da617c`.
- During the earlier decision-register pass, the writer updated only the canonical contract. Independent verifier returned PASS; `git diff --check` had no output; trailing-whitespace checks found no matches in the then-untracked contract or task file.
- Native ASSESS was unassessable because the contract was untracked; its risk-gated plan required an independent verifier, which passed. No Rust/browser tests or builds were applicable to that earlier documentation-only pass.
- At that earlier point, ROADMAP and architecture/05-08 edits were pre-existing; the pass changed only architecture/09, and the docs/07/T2b overlap was still blocked.
- The previous pass left changes uncommitted/unmerged while T2b owned `docs/07`; the user later authorized conditional integration after T2b completion.

## T2b reconciliation evidence (current)
- T2b code/test/`docs/07` completed in feature commit `43d85bb`; independent checks passed exactly: `cargo test --locked --offline -p web` (32 tests: 3 library, 4 binary, 25 integration), `cargo fmt --check`, `cargo clippy --locked --offline -p web --all-targets -- -D warnings`, and `git diff --cached --check`. The verifier observed GREEN only; the earlier worker's RED/GREEN report remains distinct. T3-T7 remain unstarted.
- The W writer reconciled only `docs/architecture/07-web-telemetry-dashboard.md` and `docs/architecture/09-read-only-web-workspace.md`. Source-doc commit `6ffe50f` records the complete W documentation unit (6 files, 313 insertions, 15 deletions). `docs/07` retains W factual API/design/history/report-schema notes and relative links, while incorporating T2b's sticky uncertainty and no-resync boundary. `docs/09` distinguishes the `c7ae8f4` snapshot from current T2b behavior and preserves all confirmed product decisions.
- Independent verifier PASS for the reconciled docs, relative links and scope. `git diff --check` passed. No trailing whitespace was found in changed docs or this task file; the only scan match was a pre-existing `docs/architecture/05-monte-carlo-and-telemetry.md:5` space identical in HEAD, left untouched.
- ODD task-plan commit `bd24f8c`, source-doc commit `6ffe50f`, task-status commit `e7b92e0`, integration merge `67542cb`, and integrated-branch record `f1b0922` are on the W/T history. The merge has both W and T histories as parents; the combined W/T tree and T-only tracker were independently verified. Local `main` was advanced to `856511a`; all intended commits passed ancestry checks, `git push origin main` succeeded, and a post-push fetch confirmed `origin/main` matched `856511a`. No cleanup has occurred. Next: verify the stash is redundant and all intended content remains reachable, then remove auxiliary worktrees/branches/stash while preserving the original `main` worktree.
