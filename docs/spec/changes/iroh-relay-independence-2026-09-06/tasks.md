---
created_at: 2026-09-06T12:52:07Z
updated_at: 2026-09-06T12:52:07Z
completed_at:
---

Implementation starts only after Stage 1 approval. Each section is one tracer-bullet ticket sized for a fresh context. Work blockers first. Baseline: `pre-iroh-relay-gate-20260906`.

Compatibility contract for the whole change: every ticket is independently shippable against unmodified peers. Tickets 1, 2, and 4 are controller-only. Ticket 3 is host-only. Ticket 5 is the only one that adds a protocol message, and it is capability-gated so a host never sends it to a controller that did not advertise support. No ticket requires desktop, headless, and mobile to ship together.

## 1. Controller Dials Without a Relay Home

**What to build:** A mobile controller that connects to its host over a shared LAN or SD-WAN virtual network while its relay is unreachable, instead of failing every attempt at the relay registration timeout.

**Blocked by:** None — can start immediately.

**Status:** done — see 1.4 note for the one design change made during implementation

- [x] 1.1 Add a failing test for the dial-policy decision: given no relay home, the policy SHALL return a short flat schedule whose total budget does not exceed the current relay registration timeout; given a relay home, it SHALL return the existing escalating ladder. Run against baseline and record the expected failure.
- [x] 1.2 Extract the dial policy as a pure function taking relay availability and returning attempt count and per-attempt timeout. Do not widen any public API for this; the function is internal to the transport crate.
- [x] 1.3 Change controller endpoint acquisition so relay registration timeout records an offline marker and returns the endpoint, instead of closing it and returning an error. Log the relay-offline transition at the same level as the existing acquisition logs.
- [x] 1.4 ~~Carry the relay-online marker into the pooled endpoint entry~~ — **superseded during implementation.** Caching the marker on the pooled entry is wrong: iroh keeps retrying the relay in the background, so an endpoint that started relay-less can gain a home while pooled, and a build-time flag would pin the short schedule forever and disagree with the endpoint it describes. Relay state is instead read live off the endpoint at dial time. One real-time decision per invariant; no cached copy to drift.
- [x] 1.5 Drive the dial loop from the policy and report exhaustion of a relay-less dial with an error distinct from the relay-unreachable error.
- [x] 1.6 Verify relay-less end-to-end connect using the existing relay-disabled endpoint test helpers already present in the transport crate's test module. Do not introduce a second harness.
- [x] 1.7 Confirm the controller-side timeout that wraps the native connect still exceeds the worst-case relay-online path budget, and adjust it only if the new policy changes that worst case. **No change needed:** the relay-routed worst case is unchanged at 12s registration + 18s dial = 30s, still inside the 32s controller-side cap; the relay-less path is shorter at 12s + 8s = 20s.

**Known trade-off:** a cold start whose relay is unreachable still spends the full registration timeout before its first dial, because there is no way to learn that the relay will fail without waiting for it. The endpoint is pooled, so this is paid once rather than per attempt. Making the registration wait and the first dial race each other would remove it, at the cost of changing relay-path timing — deliberately out of scope here.

## 2. One Reconnect Authority

**What to build:** A controller whose reconnect backoff advances once per failed connection rather than twice, and which stops retrying after sustained failure instead of re-dialling forever at the ceiling interval.

**Blocked by:** None — can start immediately.

**Status:** done — see 2.3 note for the deduplication key, and the reset-timing note below

- [x] 2.1 Add a failing test proving one connection failure advances the reconnect attempt counter exactly once, whichever observation path reports it first. Run against baseline and record the expected failure.
- [x] 2.2 Extract reconnect backoff and termination into a policy object with no widget dependency, following the existing extracted-controller pattern used elsewhere in the mobile state layer.
- [x] 2.3 ~~Make the transport-closed handler the sole scheduler~~ — **refined during implementation.** Both observations of a failed connect already routed through that one handler, so "which handler schedules" was never the defect. The defect was that a single failure had no identity, and the handler could not tell a second report from a second failure. Failures are now keyed by connection generation inside the policy, which makes deduplication a property of the authority itself rather than a flag on a caller. This also covers the other schedulers (host-probe timeout, host-offline) for free.
- [x] 2.4 Add a termination condition: after a bounded number of consecutive failures the policy stops scheduling and reports an exhausted state. A successful connection resets the policy.
- [x] 2.5 Surface the exhausted state as a resumable connection status rather than a perpetual reconnecting status, and make a user-initiated reconnect reset the policy. Added a distinct string across all ten locales — reusing the existing retry string would have promised a background retry that is no longer pending.
- [x] 2.6 Cover the state-machine reversal case: a failure arriving after the transport was already replaced by a newer connection generation SHALL NOT schedule against the stale target.

**Reset timing corrected.** The schedule previously reset when the transport merely opened. A host that accepts QUIC and then drops the link would reset on every cycle and could never exhaust — the same never-give-up loop this ticket exists to bound. Reset now happens on the first evidence the link is *usable*: the protocol `hello`, or the host answering. Three events resume an exhausted schedule: a network route change, a return to the foreground, and any connect the user asked for.

## 3. Host Starts Without a Relay Home

**What to build:** A desktop or headless host that finishes starting its remote transport and publishes a usable transport candidate even when it cannot reach a relay, so a controller on the same network can still find it.

**Blocked by:** 1 — reuses the dial policy and relay-state handling introduced there.

**Status:** done — grew to cover two contract copies found while verifying 3.4; see notes

- [x] 3.1 Add a failing test proving a host whose relay is unreachable still produces a transport candidate carrying a usable ticket. **Covered at the contract rather than through a live host:** asserting it end-to-end needs a real relay registration to time out, which costs the suite twelve seconds to prove something the contract already states. The dialability contract is tested directly instead, in both languages.
- [x] 3.2 Change host endpoint bind so relay registration timeout is recorded rather than fatal, and the endpoint is retained rather than closed. Also removed the second gate below it, which failed startup outright when no relay URL could be resolved at all.
- [x] 3.3 Allow candidate publication with an empty relay URL when no relay home exists. **Scope grew for a reason:** the acceptance rule existed in three places — the iroh dial path, the desktop controller's ticket parser, and implicitly in the mobile selector — and had already drifted, with the desktop copy treating a whitespace-only ticket as usable. Extracted as one exported, tested function; the desktop parser now delegates to it.
- [x] 3.4 Verify an unmodified controller build tolerates a candidate with an empty relay URL, and record the observed behaviour in the ticket. **Found a blocker and fixed it:** the mobile selector additionally required a non-empty `url`, which holds the relay SERVER address and says nothing about iroh reachability. A relay-less host's candidate has an empty one, so the phone would have rejected it before the dial path — the one authority on dialability — ever saw it, and shown "re-pair required". Pinned with a regression test. The other consumers are clean: mobile transport-kind selection goes through the shared Rust rule via FFI and only inspects `kind`.

## 4. Controller Remembers What Worked

**What to build:** A controller that reuses the transport address which last produced a working connection, so a host that moved to an address already seen once is reachable without any host cooperation.

**Blocked by:** 1 — needs the relay-less dial path to be reachable before a remembered address can be exercised.

**Status:** ready-for-agent

- [ ] 4.1 Add a failing test proving a successful connection records the address that carried it, and the next dial includes that address alongside the ticket's addresses. Run against baseline and record the expected failure.
- [ ] 4.2 Persist the last-known-good address on the stored device record as an optional field. Records written by earlier builds SHALL remain readable with the field absent.
- [ ] 4.3 Merge the remembered address into the dial address set without duplicating an address the ticket already carries.
- [ ] 4.4 Clear or ignore a remembered address that has failed consistently, so a stale address cannot permanently displace the ticket's addresses.
- [ ] 4.5 Cover the migration case: a device record from an earlier build loads, connects, and gains the field on first success.

## 5. Host Announces Its Current Address

**What to build:** A host that publishes its current address set on the established control link, so a controller's stored addresses track the host's real network state instead of freezing at pairing time.

**Blocked by:** 3, 4 — needs relay-less host publication and the controller-side address merge to exist first.

**Status:** ready-for-agent

- [ ] 5.1 Add a failing cross-client test: a host whose address set changes after pairing announces the change, and the controller's stored candidate is replaced and persisted. Run against baseline and record the expected failure.
- [ ] 5.2 Advertise an address-announcement capability from the shared host capability set, so both desktop and headless gain it from one definition. Pin it with a capability assertion following the existing capability test pattern.
- [ ] 5.3 Add the host-originated announcement message. Send it only to controllers that advertised support, and send on connection establishment and on host address change.
- [ ] 5.4 Replace and persist the controller's stored transport candidate from the announcement, keeping the remembered last-known-good address from ticket 4 as an independent fallback.
- [ ] 5.5 Update `docs/protocol.md` with the added message and the capability that gates it, including its version note.
- [ ] 5.6 Verify all four peer-version combinations and record the observed behaviour: new host with old controller, old host with new controller, new with new, old with old.
