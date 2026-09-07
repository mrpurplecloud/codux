# Review Findings Remediation Plan

> **For agentic workers:** Steps use checkbox (`- [ ]`) syntax for tracking. Every task is TDD: write the failing test, watch it fail for the right reason, write the minimal fix, watch it pass, then commit.

**Goal:** Fix the defects that six directory reviews plus two blind-spot audits found in the uncommitted iroh-relay-independence work, without introducing regressions and without adding a workaround beside a mechanism that already exists.

**Architecture:** The change set under repair made the remote transport survive an unreachable relay, made the desktop stores transactional, and reworked mobile terminal/device state. The defects cluster into three shapes: (a) a guard applied to an invariant it does not own, (b) a new mechanism added without deleting the one it obsoletes, (c) tests that cannot fail on the behavior they name. Each fix below removes something rather than adding, wherever the shape allows it.

**Tech stack:** Rust (tokio, iroh 1.x, GPUI desktop runtime), Dart/Flutter mobile, C ABI between them.

**Baseline:** commit `23e13a15`. All work lands on top of the user's uncommitted working tree, which the user has stopped editing.

---

## Status of the original 1–14 list

| # | Item | Status |
|---|---|---|
| 1 | First-run settings disable AI memory | **Already fixed by the user** — duplicate `AISettings` family deleted from `settings/types.rs`, orphan `default_true` removed. `app_commands::memory` test passes. |
| 7 | `headless_screen.rs` zero-coverage edits | **Already reverted by the user** — file is no longer in the diff. |
| 3 | `authorize()` collapse | Task 1. Reachability now pinned. |
| — | `stop`/`reconfigure` block the UI thread | Task 2. **New**, found by the blind-spot re-review. Higher severity than most of the original list. |
| 2 | Upload reply pinned to origin connection | Task 3. |
| 4 | `reconnect_loop` `break` leaks ownership | Task 4. |
| 6 | Legacy migration swallows poisoned queue | Task 5. |
| 12 | `settings.json` non-atomic write | Task 6. |
| 14 | Whole-object `remote` overwrite in three places | Task 7. |
| 8 | Required device write throws, nobody catches | Task 8. |
| 9 | `initializeDevices()` failure aborts bootstrap | Task 9. |
| 10 | `TerminalUploadReplyGuard` can wedge uploads | Task 10. |
| 5 | Batcher ordering guard lost | Task 11. |
| 13 | Six always-green tests | Task 12. |
| 11 | Pairing replay window undocumented | Task 13. |
| — | `pending_pairings` dead code | Task 14. **Upgraded** from "don't fix" — exhaustive grep confirms zero inserts repo-wide, and `pending_pairing_list` is write-never too, so the whole operator confirm/reject flow is unreachable. Pure deletion. |

**Withdrawn** (my earlier report was wrong, confirmed by the re-review):
- "permit held across `publish_blob().await` stalls stop for the transfer duration" — `publish_blob` is a local in-memory insert capped at 20 MiB; the real network op (`fetch_blob`) is already outside the permit.
- "`host_info_preserves_live_candidate_and_injects_current_authentication` asserts the opposite of its name" — binding-time capture is the correct semantics; only the word "current" is ambiguous.

**Deferred to an issue, not this plan:**
- `relayUrl` meaning changed from "configured" to "currently connected", and mobile's `DeviceTransportsReplaced` replaces rather than merges. Whether this can strand a phone depends on an unverified iroh fact: does `EndpointAddr` retain a configured relay that never connected? Task 15 answers that with one test and decides.
- `is_authorized` requiring the credential in two stores that can disagree (pre-existing, wider blast radius than this diff).
- Rust↔Dart numeric constants with no executable consistency check.

---

## Batch 1 — real bugs, one commit each

### Task 1: `authorize()` must not be gated on publication

**Why:** `RemoteHostCallbackAdapter::authorize` wraps a pure settings query in `self.run(...)`, which returns `None` whenever `published == false`, so `unwrap_or(false)` reports **unauthorized without reading the token**. That maps to the `device_unauthorized` wire code, which mobile treats as "stop and require re-pairing, with no automatic retry". The phone pings every 3 s and `on_authorize` runs before the ping exemption, so any unpublished window longer than a few seconds is hit with certainty. Windows are not races but durations: `transition()` sets `published = false` and then waits for in-flight callbacks to drain, while the iroh Router keeps accepting.

The sibling `authorize_web_tunnel` already returns a distinct "no longer active" error for the same condition — the design intent is present, `authorize`'s `bool` return just had nowhere to put it.

This fix **removes** a guard rather than adding one: authorization is a question about persisted settings, not about which transport generation is live.

**Files:**
- Modify: `apps/desktop/runtime/src/remote/host/transport.rs:182-185`
- Test: `apps/desktop/runtime/src/remote/host/tests/transport.rs`

- [ ] **Step 1: Write the failing test.** A runtime whose settings authorize `device-1`/`device-token-1`, with the callback owner not yet published, must still answer `true` from the adapter's `authorize`. Requires `fn authorize` to be `pub(super)` for test visibility, or an equivalent seam that does not widen the public API.
- [ ] **Step 2: Run it, expect FAIL** (`authorize` returns `false` while `is_authorized_device_token` returns `true`).
- [ ] **Step 3: Minimal fix** — call `is_authorized_device_token` directly instead of through `run()`, keeping the `Weak::upgrade` that HEAD had.
- [ ] **Step 4: Run it, expect PASS.** Then `cargo test -p codux-runtime --lib remote::` — all green.
- [ ] **Step 5: Commit.**

### Task 2: `stop_with_message` and the relay setters must not block the UI thread

**Why:** `retire_transport_lifecycle` → `transition()` blocks the calling thread on a condvar until every in-flight callback permit drops. A permit spans the whole synchronous envelope dispatch, which includes `git.invoke` network operations (`push`/`pull`/`fetch`). HEAD's `stop_with_message` never blocked — it was `fetch_add` + `take_transport` + a spawned shutdown. Three UI-thread call sites now inherit the block. `reconnect_remote` already routes through `spawn_blocking`, so the pattern to follow is in the same file.

**Files:**
- Modify: `apps/desktop/src/app/ssh_remote_actions.rs` — `toggle_remote_host` (~:488), `apply_remote_relay_authentication_change` (~:575), and the relay URL / preset setters
- Reference: `apps/desktop/src/app/ssh_remote_actions.rs:696` (`reconnect_remote`, already correct)

- [ ] **Step 1:** Confirm each call site and that `reconnect_remote`'s `spawn_blocking` shape applies unchanged. The GPUI app layer has no test harness for this; the verification is that the runtime-layer behavior is untouched and `cargo test -p codux-runtime --lib remote::` stays green, plus a manual reading that no call site now awaits on the UI thread.
- [ ] **Step 2:** Apply the same `spawn_blocking` wrapper to the identified call sites.
- [ ] **Step 3:** `cargo check -p codux-desktop` and the runtime suite.
- [ ] **Step 4: Commit.** Note in the message that this is a UI-layer change with no runtime semantics touched, and that the runtime-side blocking is intentional (it is what makes ownership transfer safe).

### Task 3: the upload reply must follow the device, not the connection it arrived on

**Why:** `iroh_link.rs:818` captures `senders.control` and publishes the reply through it. Before this change the reply was sent by the app layer via `send_to_peers`, which resolves `peers[device_id]` **at send time** — whatever connection currently owns that device alias. A phone that loses its link mid-upload (the SD-WAN case this whole work targets) reconnects with fresh senders; the host finishes the download, writes the file, injects the path into the PTY, then publishes into a dead queue and logs one line. The phone times out and retries, producing a second file and a second injection. The side effect is not idempotent and the host has no dedup key.

Origin-pinning was the *old* way to stop a stale reply landing on a newer operation. This same change set introduced `requestId` for exactly that purpose, and mobile matches on it. So the fix deletes the mechanism `requestId` obsoletes.

**Files:**
- Modify: `crates/codux-remote-transport/src/iroh_link.rs:809-843`
- Test: `crates/codux-remote-transport/src/iroh_link.rs` tests module

- [ ] **Step 1: Write the failing test.** Upload arrives on connection A for `device-1`; before the reply is published, `device-1`'s alias is rebound to connection B; the reply must arrive on B, not A.
- [ ] **Step 2: Run it, expect FAIL** (reply goes to A, B receives nothing).
- [ ] **Step 3: Minimal fix** — capture `Arc<Self>` plus `reply.device_id` and publish via the alias-resolving send path.
- [ ] **Step 4: Run it, expect PASS.** Then `cargo test -p codux-remote-transport` — all green.
- [ ] **Step 5:** Replace `upload_result_is_published_only_to_captured_origin` (currently vacuous — nothing can reach its `replacement` receiver under any implementation) with this test, or delete it.
- [ ] **Step 6: Commit.**

---

## Batch 2 — low-risk product fixes, grouped commits

### Task 4: `reconnect_loop`'s `break` must release or disclaim ownership

**Why:** The pre-change loop had a `reconnecting.remove(&device_id)` tail that ran on exactly this exit; the diff deleted it without replacement. Every other exit either clears ownership or first proves it no longer owns the slot. Currently unreachable — the only production path that invalidates an attempt removes the whole slot — but the mechanism that made it unreachable-by-construction is the one that was deleted. If reached: `reconnect_worker = Some(worker)` with no worker running, so `ensure_reconnect_loop` never spawns a replacement and the device is stuck in "connecting" until `forget` + re-pair.

**Files:** Modify `apps/desktop/runtime/src/remote/controller_manager.rs:869-871`

- [ ] **Step 1: Write the failing test** — a worker whose attempt is invalidated while its slot survives must not leave `reconnect_worker` claimed.
- [ ] **Step 2: Run it, expect FAIL.**
- [ ] **Step 3:** Replace the bare `break` with an explicit ownership release.
- [ ] **Step 4: Run it, expect PASS** plus the controller suite.

### Task 5: the legacy-migration commit must propagate a poisoned event queue

**Why:** Its sibling twenty lines away does `.map_err(|_| "Remote runtime event queue is poisoned.")?` with a comment saying a committed change must never lose its one refresh, and acquires the queue *before* the store write for that reason. The legacy path writes first and then does `if let Ok(mut events)`, so a poisoned queue leaves a durable attachment with no publication and no retry (the migration version has already advanced). Same invariant, two owners, and the weaker one erases the signal.

**Files:** Modify `apps/desktop/runtime/src/runtime_state/service_remote_projects.rs:418-425`

- [ ] **Step 1: Write the failing test** — poison the queue, run a legacy attachment, assert it returns `Err` and does not advance the migration version.
- [ ] **Step 2–4:** Fail, fix by matching the sibling's ordering and `?`, pass.

### Task 6: `settings.json` must be written atomically

**Why:** `fs::write` truncates then writes. A crash mid-write leaves invalid JSON; `read_snapshot` swallows the parse error and returns an empty map, so every setting reads as default and the next targeted write persists that empty map. Pre-existing, but this change set made `ConfigStore` the single authoritative store — there is no longer an in-memory typed copy to fall back on — so the exposure is higher than it was.

**Files:** Modify `apps/desktop/runtime/src/config.rs:402-408` (and the second write site at ~:423)

- [ ] **Step 1: Write the failing test** — a truncated/invalid settings file must not cause a subsequent write to persist defaults over real data. (If the crash point is not directly testable, test that `write_snapshot` leaves either the old or the new content, never a partial one, by asserting the temp file is renamed rather than the target being opened for truncation.)
- [ ] **Step 2–4:** Fail, fix with temp + `fs::rename`, pass.

### Task 7: the three surviving whole-object `remote` overwrites

**Why:** `raw.insert("remote", to_value(&settings))` drops any key `RemoteSettings` does not model. The relay paths in this same change set were converted to targeted inserts and pinned by a test that plants `futureRemoteField`/`futureField` — but revoking a device or re-registering the host still wipes them. No known field is lost today. It is worth doing anyway because the change set exists partly to repair the same shape of bug one layer up (`AppSettingsStore::save()` destroying paired-device tokens).

**Files:** Modify `apps/desktop/runtime/src/remote/registration.rs:52-55`, `apps/desktop/runtime/src/remote/devices.rs:19-22` and `:47-50`

- [ ] **Step 1: Extend the existing preservation test** to cover device revoke and host re-registration; watch it fail.
- [ ] **Step 2–4:** Fix with targeted inserts, pass.

### Task 8: required device writes must not throw into an unhandled future

**Why:** `applyRequiredDeviceMutation` throws `StateError` when `SharedPreferences.setString` returns false. `_removeDevice` is fired from `.then((confirmed) { ... })` with no handler, so a failed write becomes an unhandled async exception: the device stays listed and the user is told nothing. `_applyBackgroundDeviceMutation` catches and logs; the required path has no equivalent.

**Files:** Modify `apps/mobile/lib/screens/home/state/actions.dart` (`_removeDevice`, `_editDevice`); test in `apps/mobile/test/`

- [ ] **Step 1: Write the failing test** — a store whose write fails must surface a user-visible status, not an unhandled exception.
- [ ] **Step 2–4:** Fail, fix, pass.

### Task 9: a failed normalization must not abort bootstrap

**Why:** `initializeDevices()` is awaited uncaught at `bootstrap.dart:27`. A failed normalization commit throws before `_applyState` runs, leaving the app on a blank startup state. The old `_normalizeAndPersistDevices` ignored the write result and returned the normalized list regardless — normalization is best-effort repair and must not be able to block startup.

**Files:** Modify `apps/mobile/lib/screens/home/state/bootstrap.dart:27`; test in `apps/mobile/test/`

- [ ] **Step 1: Write the failing test** — bootstrap completes and applies state even when the device store's normalization write fails.
- [ ] **Step 2–4:** Fail, fix, pass.

### Task 10: the upload reply guard needs an escape short of restarting the host

**Why:** `retire()` poisons the host/device/session scope whenever `!requestIdSupported && wasSent && !completedFromReply`, and `_finishTerminalUpload` calls it unconditionally. Against a host that predates the `requestId` capability, one mid-flight transport drop makes every later upload in that session report "Previous upload was not confirmed. Restart the computer before retrying". A reconnect means the old in-flight blob is gone, so a fresh connection generation is a safe point to clear the scope.

**Files:** Modify `apps/mobile/lib/services/terminal_upload_operation.dart:179-186` and its caller; test in `apps/mobile/test/terminal_upload_operation_test.dart`

- [ ] **Step 1: Write the failing test** — after a transport close and a new connection generation, an upload to the same session is permitted again.
- [ ] **Step 2–4:** Fail, fix, pass.

---

## Batch 3 — tests, comments, deletions (no product-logic risk)

### Task 11: restore the batcher ordering guard

**Why:** `terminal_input_batcher_test.dart:82` still carries the name `flushes pending printable input before control keys` but its body now switches activation mid-test and asserts the buffered input is *dropped*. No test covers the same-activation ordering any more. The production code is still correct; nothing would go red if a future edit swapped `flush()` and `_send()`. This is the keystroke path.

- [ ] **Step 1:** Add a test under its own name: buffer `git status`, send `\r` on the *same* activation, assert `['git status', '\r']` in that order.
- [ ] **Step 2:** Verify it goes red against a deliberately reordered implementation, then restore.
- [ ] **Step 3:** Rename the repurposed test to describe what it actually asserts.

### Task 12: delete or repair the always-green tests

Each of these passes with and without the behavior it names, which is worse than no test — it reports coverage that does not exist.

- [ ] `controller_store.rs:803` `newer_update_cannot_be_overwritten_by_an_older_write` — asserts mutex serialization; the property it names is already covered by `failed_derived_write_retries_the_latest_snapshot`. Delete.
- [ ] `controller_store.rs:682` `missing_primary_recovers_the_last_known_good_backup` — manufactures the `.bak` with `fs::copy`, while `save_path` only writes it under `#[cfg(windows)]`. Gate the test to Windows, or write the backup on all platforms and keep the test.
- [ ] `iroh_link.rs:3106` `upload_result_is_published_only_to_captured_origin` — superseded by Task 3.
- [ ] `iroh_link.rs:3200` `the_published_candidate_agrees_with_its_endpoint` — compares the implementation to a hand-copy of itself, and `test_host()` disables the relay so both sides are empty. Replace with a test that a known-but-not-yet-connected relay is not published.
- [ ] `apps/agent/src/terminals.rs:1663` `terminal_upload_accepts_legacy_request_without_id` — sets a field that path never reads. Move the assertion to where `TerminalUploadReply.request_id` is branched on, or delete.
- [ ] `apps/mobile/test/remote_protocol_ffi_test.dart:10-33` — most assertions are tautological: both sides of the `expect` come from the same Rust FFI call, and an unknown name returns `""`, so a renamed constant leaves both sides `""` and still passes. Add a comment, or assert against literals.

### Task 13: document the pairing replay window

**Why:** The replaced test inverted a security property. Publishing the ticket before rotating `active` is the right convergence model — rotating into an unpublishable credential would brick pairing permanently — but it means a code+secret a device already presented stays valid until some write succeeds. The current comment says only "active state follows the ticket".

- [ ] Extend the comment at `apps/agent/src/host.rs:194-201` to name the window and why it is preferred over the alternative.

### Task 14: delete the unreachable pairing flow

**Why:** Repo-wide grep finds zero inserts into `pending_pairings` — every site is `clear`/`remove`/`len`. `RemoteSummary::pending_pairing_list` is write-never too. So the desktop's pending-pairing overlay and the whole operator confirm/reject flow (`confirm_pairing`, `reject_pairing`, `finalize_remote_pairing`, `send_pairing_confirmed`) are unreachable; auto-confirm is the only live path. This change set added ~60 lines of owner guards to that dead code.

- [ ] Confirm the grep independently before deleting anything.
- [ ] Delete the dead map, the dead summary field, the dead entry points, the desktop overlay, and the dead `Option<Arc<dyn RemoteTransport>>` return from `prepare_transport_for_pairing` (every path returns `None`, so `pairing.rs:120-122`'s shutdown is dead too).
- [ ] Full suite must stay green; the deletion is only safe if nothing references it.

### Task 15: settle the `relayUrl` question with one test

**Why:** The host now publishes the *currently connected* relay, not the configured one, and mobile's `DeviceTransportsReplaced` replaces the stored candidate wholesale. Whether a phone can be stranded depends on one unverified fact.

- [ ] Write a test that boots a host endpoint against an unroutable relay URL and asserts whether `current_dial_candidate().ticket` still parses to an addr carrying that relay.
- [ ] If the ticket retains it, the concern is already mitigated — record that and stop.
- [ ] If it does not, open an issue for a field-level merge in `device_snapshot_store.dart` rather than changing dial behavior now.

---

## Verification before completion

- [ ] `cargo test --workspace` — no failures.
- [ ] `cd apps/mobile && flutter test` — no failures.
- [ ] `cargo clippy --workspace --all-targets` — warning count not above the pre-work count.
- [ ] `git status --short --untracked-files=all` — `apps/mobile/pubspec.lock` NOT staged (it is the user's own unrelated pending change).
- [ ] Every commit message states which finding it closes and what evidence proved the fix.
- [ ] Report explicitly what was NOT verified: there is still no end-to-end run against a real phone or a real SD-WAN link.
