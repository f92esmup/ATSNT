# T6 — Normalize Spot and supported USD-M private account events
Branch: `feature/t3-paper-session` | Delivery: `ask-on-risk` (initial forecast: ~380 authored lines)
Runner: `cargo test --workspace --offline`; also `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --offline -- -D warnings`.

## Specs

### S1 — Keep the private-event scope supported and explicit
The user's original task is: “T6 — Normalize Spot and USD-M private account events. Use official fixtures; bound listen-key renewal; define explicit failure, shutdown and reconciliation behavior; never log keys. Futures behavior stays within the supported account profile. Offline adapter tests with fake time/gateway; no live exchange calls.”

Acceptance: normalize only the Spot and supported USD-M events needed by the existing adapter/account profile. Keep event meaning intact (including partial balance snapshots versus balance deltas); do not fabricate missing numeric/time values or claim a complete account snapshot from partial events. The Spot WebSocket API's `subscriptionId`/`event` envelope must be unwrapped and validated. Reject event-time regressions with a reconciliation-required signal and stop processing that stream. Unknown, malformed, or unsupported profile data must be visible as an error or reconciliation-required condition rather than silently dropped or treated as valid state.

### S2 — Use the current signed Spot user-data subscription
The user approved the scope update with: “si, actualizamos”. Binance's Spot listenKey WebSocket stream is deprecated; Spot must instead subscribe through the signed Spot WebSocket API `userDataStream.subscribe.signature` method. USD-M Futures remains on its supported listenKey stream.

Acceptance: send `apiKey`, current millisecond `timestamp`, optional bounded `recvWindow`, and a signature over the alphabetically sorted request parameters (excluding `signature`) in the Spot WebSocket API subscription. Use the official production/testnet WebSocket API endpoints and consume the acknowledged subscription's nested event envelope. Keep credential material and signatures out of logs and error strings. Preserve the existing public behavior where feasible and make any unavoidable contract change explicit in the API and tests. No live exchange calls.

### S3 — Bound USD-M listen-key lifecycle operations
Acceptance: impose request deadlines, validate create/renew response bodies (an HTTP success status alone is insufficient), and use a bounded renewal schedule/retry policy. A failed or expired key is surfaced as a stream failure that requires account reconciliation; it must not be represented as a clean end-of-stream. Keep supported Futures account-profile constraints intact.

### S4 — Make errors, shutdown, and reconciliation observable
Acceptance: expose typed transport/protocol/renewal failures and an explicit shutdown path that does not leave an unowned reader task running. Bound WebSocket writes; explicit shutdown must cancel in-flight renewal, close the socket/key, and join the task, aborting it if the join deadline expires. Dropping the stream must abort rather than detach a stuck reader. A clean local shutdown is not evidence that account state is reconciled. Stream interruption or dropped/invalid events must leave a reconciliation-required signal available to the caller.

### S5 — Verify offline with official-schema fixtures
Acceptance: add compact fixtures derived from official Binance event schemas with provenance recorded in tests/docs. Exercise subscription signing, normalized Spot and USD-M event meaning, malformed/unsupported input, bounded renewals, failure, and shutdown using local fakes/fake time. Do not make external network calls in tests or implementation verification.

## Tasks

- [ ] **T6** — Implement S1–S5 inline; verify the focused adapter tests and workspace checks; commit the coherent work unit on the feature branch. Implementation and checks are complete; work-unit commit and native review remain pending.

## Log

- **L1 — Original request:** “adelante acaba con  t6”
- **L2 — Scope decision:** “si, actualizamos” — migrate Spot from the deprecated listenKey stream to signed Spot WebSocket API subscription; keep USD-M on its supported listenKey path.
- **L3 — Research evidence:** Binance documents the signed Spot WebSocket subscription and parameter fields at https://developers.binance.com/en/docs/catalog/core-trading-spot-trading/api/ws-api/user-data-stream, HMAC signing canonicalization in its official Spot API docs at https://github.com/binance/binance-spot-api-docs/blob/master/web-socket-api.md, and the testnet endpoint at https://github.com/binance/binance-spot-api-docs/blob/master/testnet/general-info.md. The Spot listenKey stream deprecation is at https://developers.binance.com/en/docs/products/margin-trading/change-log. USD-M listenKey behavior remains documented at https://developers.binance.com/en/docs/catalog/core-trading-derivatives-trading-usd-m-futures/api/ws-api/user-data-streams.
- **L3a — Signing detail:** the Spot WebSocket API signature payload includes all request `params` except `signature`, sorted by parameter name and joined as `name=value` pairs with `&`; the configured adapter currently supports HMAC-SHA256 credentials. Binance documents this in its official WebSocket API signing examples.
- **L4 — Start state:** implementation had not yet started. The uncommitted T5 closure update in `odd/tasks/milestone-5-6-closure.md` is pre-existing work and must be preserved. Engram task mirror is pending because the runtime has not registered an authoritative session ID; no session identity will be invented.
- **L5 — Initial verification state:** pending before implementation.
- **L6 — Initial next step:** implement S1–S5, run checks, obtain independent high-risk verification, then record the work-unit commit and assessment outcome.
- **L7 — Independent findings and correction:** the first high-risk verification found that Spot messages were parsed without unwrapping the signed-subscription envelope, event-time regressions were accepted, shutdown could poll a completed oneshot during renewal, and dropping the stream could leave a stuck reader detached. It also found renewal HTTP-200 bodies were not validated; that gap existed at baseline but violated S3. Added regression tests and fixed all five behaviors; subscription and Pong sends now have deadlines, and a shutdown timeout aborts and joins the reader.
- **L8 — Verification:** focused private-stream tests passed (17), listen-key gateway tests passed (7), and the offline workspace suite passed (160 tests, 0 failures). `cargo fmt --all -- --check` and workspace Clippy passed. Independent bounded recheck passed all prior findings in a fresh scratch copy. Tests use loopback/fakes only; no live Binance calls.
- **L9 — Delivery and next step:** the project tracker already records the user's `stacked-to-main` choice for this branch; reuse that cached chain strategy, but do not push or create a PR. The local T6 work unit is 1,908 authored additions/deletions, above the 400-line review budget; PR slicing remains a separate delivery decision. Next, create the local T6 work-unit commit, then assess this committed candidate under the enabled native review switch. Engram mirror remains pending until the runtime registers its authoritative session ID.
