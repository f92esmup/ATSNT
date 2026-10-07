# T Workstream Scope Audit

## Goal
Separate the active M5/M6 T1-T7 engineering closure from the future W0-W6 web-workspace design, correct stale/contradictory T2b status, and prevent the shared milestone tracker from becoming a duplicate W implementation plan.

## Authorization and boundaries
- The user asked to verify whether T2b is necessary, stale or duplicated by the web redesign, and to narrow `odd/tasks/milestone-5-6-closure.md` so W work stays in the separate workspace contract.
- The prior documentation-only work was completed in `/home/f92esmup/Projects/ATSNT-web-workspace-contract`; canonical product decisions are in its unmerged `docs/architecture/09-read-only-web-workspace.md`.
- At initial audit start, implementation worktree `/home/f92esmup/Projects/ATSNT` was on branch `feature/m5-6-reliability-gateway` at HEAD `80ae7e176298d10ee7f160818036f9904012b117`.
- At audit start, staged paths were `crates/web/src/state.rs`, `crates/web/src/ws.rs`, `crates/web/static/app.js`, `crates/web/tests/api_tests.rs`, `docs/architecture/07-web-telemetry-dashboard.md`, and `odd/tasks/milestone-5-6-closure.md`. Preserve all code and docs/07 staged content; do not reset, unstage, edit, or replace those paths.
- At the initial audit, the user authorized only a documentation correction to the milestone tracker: no application source, `docs/07`, W-contract edits, commits, pushes, merges, branch changes, tests or builds. Those limits describe that completed audit phase; the user later separately authorized T2b verification/commit and gated docs integration/push, recorded below.
- Preserve unrelated staged code/index. The initial tracker correction staged only `odd/tasks/milestone-5-6-closure.md` after status checks. Later staging/commits are restricted to separately verified T2b and task-document work units.
- The initial audit did not run tests/builds. Subsequent T2b verification was a separate user-authorized phase and is recorded below.

## Audit findings
- T2b addresses reliability defects in the existing read-only telemetry dashboard: restore snapshot/UI fields, represent broadcast/client lag as uncertainty, keep event consumption alive, prevent stale backlog from being presented as current, and configure mock identity before its first event. It is not W UI implementation and is a correctness prerequisite for later W2 consumption.
- No authoritative producer-side resynchronization source exists. T2b must leave uncertainty sticky until a real complete snapshot exists; it must not invent a resync source.
- At the time of the initial audit, staged code/test changes showed T2b work present in the working tree, but Slice 2 remained open for independent verification and local commit. The tracker contradicted itself by both describing Slice 2 as in progress and later claiming snapshot/state resilience was unimplemented; the correction removed the latter without claiming completion.
- T1-T7 remain the M5/M6 engineering track authorized by the user. T4-T6 are distinct gateway work and are not prerequisites for a read-only Paper dashboard. W2 consumes T2b/T3 outputs rather than reimplementing them; W6 coordinates with T7 where verification overlaps.
- The detailed embedded W0-W6 redesign plan duplicates and is stale against the separate canonical contract. Remove it from the milestone tracker and replace it with a short pointer/boundary. Keep W product decisions and future UI deliverables exclusively in the canonical W contract.
- T7 verifies current M5/M6 behavior, not the future shell or W screens. Do not let “clean worktree” language imply resetting or discarding staged user work.

## Tasks
- [x] Inspect repository state, staged task diff, current tracker text and W contract read-only. Preserve the starting branch/index and obtain independent explorer findings with path/line evidence.
- [x] Narrow `odd/tasks/milestone-5-6-closure.md` to T1-T7 M5/M6 closure only. Preserved T2b's necessary reliability acceptance and open staged-slice status; removed the duplicate detailed W0-W6 plan and stale W0 gates; replaced them with one pointer to the canonical W contract; clarified T2b/T7 and T4-T6 boundaries.
- [x] Independently verify the narrowed tracker against staged T2b evidence and the separate W contract. Verifier PASS: exact six staged paths, HEAD unchanged, no unstaged tracked paths, expected new audit file untracked; both cached/worktree diff checks and whitespace scans clean. Byte-for-byte source preservation was not independently proven, so the record limits its claim to observed paths/status/stat and the path-specific staging action.
- [x] Independently verify and locally commit T2b Slice 2 after subsequent user authorization. Offline web-package tests (32: 3 library, 4 binary, 25 integration), fmt, Clippy and staged diff check all passed. Code/test/docs unit commit: `43d85bb`.
- [x] Update the milestone tracker with T2b completion, verification evidence and the conditional push gates. Commit: `ea80f62` (`docs(odd): close T2b and gate W integration`).

## Initial audit verification plan (completed)
- Check current `git status --short --branch` before and after the tracker edit; preserve all non-target staged paths exactly.
- Confirm the index/worktree agree for the corrected milestone tracker if staging that single authorized path; do not stage any other path.
- Inspect the resulting milestone tracker to ensure it contains T tasks only plus a short W-contract reference; T2b status is not contradictory; T2b remains open at that point pending independent verification/commit; T3-T6 stay distinct; T7 does not take ownership of W implementation or erase unowned staged work.
- `git diff --cached --check` and `git diff --check` passed after the corrected target file was staged; whitespace checks passed for both task files. The initial audit did not run code tests/builds.
- The initial audit left all changes local and uncommitted and reported the staged/unstaged/untracked state. Later authorized commits and current handoff are recorded below.

## Delivery evidence
- Audit explorer read the working-tree source and contract; it did not have Git/CodeGraph tools, so the parent independently verified branch, HEAD, worktree list and staged tracker diff.
- Parent observed branch `feature/m5-6-reliability-gateway`, HEAD `80ae7e176298d10ee7f160818036f9904012b117`, with six staged paths listed above and no unstaged paths at audit start.
- The read-only audit found T2b code is a bounded existing-dashboard reliability task; the detailed W plan and contradictory T2b status in the milestone tracker were stale/duplicative.
- The parent replaced the detailed W section with a one-paragraph crosswalk, removed the obsolete T2b “not implemented” claim, retained T2b Slice 2 as open, and staged only `odd/tasks/milestone-5-6-closure.md` after confirming that it was the sole unstaged tracked path.
- Independent final verification at audit close: PASS. HEAD remained `80ae7e176298d10ee7f160818036f9904012b117`; six expected paths were staged with no unstaged tracked paths; this audit file was untracked. Cached stat was 6 files, 306 insertions and 165 deletions. Both diff checks and whitespace scans passed. At that point, T2b itself remained open; the audit had not run tests/builds or performed a commit/delivery.

## Post-audit continuation after new user authorization
- The user then authorized preserving and independently verifying T2b Slice 2 first, deferring `docs/07` reconciliation until after T2b, and pushing `main` only after T2b and W documentation are integrated and checked. No push, merge to `main`, branch/worktree cleanup, or new W implementation has occurred.
- `gentle-ai-verify` independently ran `cargo test --locked --offline -p web` (32 tests: 3 library, 4 binary, 25 integration), `cargo fmt --check`, `cargo clippy --locked --offline -p web --all-targets -- -D warnings`, and `git diff --cached --check`; all passed. It observed GREEN only; the earlier worker's RED/GREEN report remains separate evidence.
- The T2b code/test/`docs/07` slice was committed locally as `43d85bb` (`fix(web): preserve uncertainty across telemetry gaps`) with five paths, 267 insertions and 8 deletions. The T/W milestone tracker correction was separately committed as `ea80f62` (`docs(odd): close T2b and gate W integration`); this audit continuation is staged as its own task-record documentation unit.
- Native ASSESS for committed range `80ae7e176298d10ee7f160818036f9904012b117..43d85bb` returned medium risk (`executable_change`), `reviewDue:false` (`under_budget`), and a plan not requiring an independent verifier. An independent verifier had already run and passed. `candidate.consumed:false`; no native approval for the code commit is claimed.
- During a separate inspect round trip, lineage `review-b112e510f04e16c2` covered `odd/tasks/milestone-5-6-closure.md` only and was approved/acknowledged. It reviewed the tracker document, **not** commit `43d85bb`.
- Current handoff: T2b is complete: code/test/docs unit `43d85bb` is locally committed; 32 web-package tests, fmt, Clippy and staged diff checks passed. The tracker records that native ASSESS found no review due under the budget; do not infer native code-review approval. Reconcile `docs/07` with the canonical W contract now that T2b is closed, then integrate/check W documentation and perform the user-authorized `main` push. Remove auxiliary refs/worktrees only after verifying all intended work is reachable. The user will separately authorize the next implementation task.
