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
- [x] 3.4 Verify an unmodified controller build tolerates a candidate with an empty relay URL, and record the observed behaviour in the ticket. **Found a blocker and fixed it:** the mobile selector additionally required a non-empty `url`, which holds the relay SERVER address and says nothing about iroh reachability. A relay-less host's candidate has an empty one, so the phone would have rejected it before the dial path — the one authority on dialability — ever saw it, and shown "re-pair required". Pinned with a regression test. Mobile selection now asks that shared Rust dialability rule through FFI instead of maintaining a field-presence copy.

## 4. Controller Remembers What Worked

**What to build:** A controller that reuses the transport address which last produced a working connection, so a host that moved to an address already seen once is reachable without any host cooperation.

**Blocked by:** 1 — needs the relay-less dial path to be reachable before a remembered address can be exercised.

**Status:** done — see 4.4 for the eviction decision

- [x] 4.1 Add a failing test proving a successful connection records the address that carried it, and the next dial includes that address alongside the ticket's addresses. Run against baseline and record the expected failure.
- [x] 4.2 Persist the last-known-good address on the stored device record as an optional field. Records written by earlier builds SHALL remain readable with the field absent. **Stores a short list rather than a single address:** a host is commonly reachable at different addresses from different places — a LAN address at home, a virtual one over SD-WAN elsewhere — and keeping only the newest would make every change of location forget the address it is about to need again. The field is omitted from the JSON when empty, so a device that never connected stays byte-identical to a record written before it existed.
- [x] 4.3 Merge the remembered address into the dial address set without duplicating an address the ticket already carries. Deduplication is structural: `EndpointAddr` holds a set, so a remembered address the ticket already lists merges away rather than being dialled twice. Unparseable stored entries are dropped rather than failing the dial — stored data outlives the code that wrote it.
- [x] 4.4 ~~Clear or ignore a remembered address that has failed consistently~~ — **handled by recency instead of failure tracking.** Explicit failure counting would need the native dialer to report which address failed, threading per-address outcomes back through the FFI boundary for a small benefit: a stale entry costs one extra address in a dial that already tries several concurrently, is capped at three, and can never displace the ticket's own addresses. Promotion on success ages unused addresses out on its own. Revisit only if a real dial is measured to be slowed by it.
- [x] 4.5 Cover the migration case: a device record from an earlier build loads, connects, and gains the field on first success.

## 5. Host Announces Its Current Address

**What to build:** A host that publishes its current address set on the established control link, so a controller's stored addresses track the host's real network state instead of freezing at pairing time.

**Blocked by:** 3, 4 — needs relay-less host publication and the controller-side address merge to exist first.

**Status:** done — no protocol change was needed; see below

**The announcement shape already existed.** Desktop `host.info` already carried
`transports`; the shared payload builder supported it, while the headless call
site passed an empty list. Both hosts now rebuild that field from their live
endpoint on every reply. The ticket is
`EndpointTicket::from(endpoint.addr())`, evaluated at send time.

So the whole ticket collapsed to: consume the field. That is strictly better
than the planned new message. No new message type, no capability negotiation, no
version gate, and all four peer-version combinations are safe by construction
rather than by agreement.

- [x] 5.1 Add a failing cross-client test: a host whose address set changes after pairing announces the change, and the controller's stored candidate is replaced and persisted. `host_info_candidate_is_adopted_persisted_and_keeps_remembered_addresses` now exercises the canonical host payload through the manager store and reloads it from disk.
- [x] 5.2 ~~Advertise an address-announcement capability~~ — **not needed.** A capability exists so a sender can avoid sending something a receiver cannot handle. Here the field is already always sent; the only question is whether the receiver uses it, which the receiver answers by itself. Adding a flag would have been ceremony around a decision nobody else needs to know.
- [x] 5.3 ~~Add the host-originated announcement message~~ — **superseded:** the existing `host.info` reply is that message. It is already sent on every connection, which is exactly the moment stored addresses need refreshing. Sending on host address change mid-connection was dropped deliberately: while a connection is live its path still works, and when it breaks the reconnect fetches a fresh `host.info` anyway. A push would only matter for a host that changed address without the link noticing, which cannot be delivered to a controller that has no working path.
- [x] 5.4 Replace and persist the controller's stored transport candidate from the announcement, keeping the remembered last-known-good address from ticket 4 as an independent fallback. An empty report is explicitly not an instruction to forget — silence from an older host must not strand the device. Unchanged reports write nothing, since `host.info` arrives on every connection.
- [x] 5.5 Update `docs/protocol.md` with the added message and the capability that gates it, including its version note. Documented the field's meaning and the two safety rules instead — the wire format is unchanged, so there is no version note to add.
- [x] 5.6 Verify all four peer-version combinations and record the observed behaviour: new host with old controller, old host with new controller, new with new, old with old. **New host + old controller:** the field was always sent and previously ignored; unchanged. **Old host + new controller:** a desktop host has sent it since `v1.8.0` (commit `25a93192`, 2026-06-10); an older desktop host, and any headless host predating this change, simply omit it and leave stored candidates untouched. **New + new:** stored addresses follow the host. **Old + old:** untouched.

## 6. Desktop Controller Keeps The Addresses It Needs

**What to build:** A desktop controller that can reach a saved host over a direct path when the relay is gone — the same capability tickets 1, 4, and 5 gave the phone.

**Blocked by:** 1, 4, 5 — this is those tickets applied to the second controller.

**Status:** done

**Why this exists.** Tickets 4 and 5 were written as if there were one controller. There are two. Lowering the relay gate let the desktop reach the dial loop, but it had nothing to dial: `SavedRemoteTransport` had no ticket field at all, so a saved host was stored as node id + relay url and nothing else. Every direct address the host published at pairing time was parsed and thrown away. With the relay down, the desktop had only mDNS, and off-LAN it would report `no relay home, direct addresses unreachable` — the failure the earlier tickets were supposed to remove.

- [x] 6.1 Add failing tests: a confirmed pairing keeps its endpoint ticket; a saved host dials that ticket and its remembered addresses; a record written before either field existed still loads.
- [x] 6.2 Store the ticket and a bounded direct-address list on the saved transport, both `#[serde(default)]` so existing records load unchanged.
- [x] 6.3 Keep the ticket when parsing `pairing.confirmed`, and dial it — `connect_saved` previously hardcoded an empty one, which was honest about the store but wrong about the host.
- [x] 6.4 Remember the address of a working direct path, promoting on reuse and ageing out the unused, matching the mobile policy.
- [x] 6.5 Adopt the candidates the host reports in `host.info`, refusing empty and undialable reports and carrying the remembered addresses across.
- [x] 6.6 Put the adoption behind the manager's own `host_info` entry points rather than at each call site. The manager owns the store, so "saved addresses follow the host" is its invariant; every caller refreshes for free and a caller added later cannot forget to.

**Reconnect deliberately unchanged.** The desktop's reconnect loop was checked against ticket 2 and needs none of it: it is a single loop with a monotonic counter, so it has no double-counting, and its 60s ceiling on a mains-powered workstation carries none of the battery cost that justified terminating on the phone. Not extending scope without evidence.

**Known trade-off.** The address write happens inline on the transport's state callback. It is bounded — only a genuinely new address writes, and the list is capped — but a slow disk would briefly stall that callback. Moving it off-thread would need an owner for the queue; not worth it until a real stall is measured.

## Self-review corrections

A two-axis review (standards + spec) after the five tickets found four real
defects and one process failure. All are fixed above; recorded here because
each was a claim made and then falsified.

**Headless never reported its addresses.** 5.6 originally claimed "both desktop
and headless already send it, from one shared payload builder". The builder is
shared; the agent was passing it `Vec::new()`, with a comment reasoning that the
controller already knew the path it connected on. True of the live connection,
false of the next one — which is the whole point of storing them. Ticket 5 was
therefore half-delivered: it worked against a desktop host and did nothing
against a headless one. The agent now rebuilds its candidates from its live
transport, and carries its relay authentication so the reported set is complete
— a controller replaces its stored set wholesale, so an omitted bearer token
would be lost on the controller side.

**The host published a build-time relay while publishing a live ticket.** The
same defect ticket 1 fixed on the controller, left standing on the host one
commit later: `relay_url` was captured at bind, so a host that started without a
reachable relay and acquired one later kept advertising the empty startup value
— inside the very payload ticket 5 added to make addresses follow the host. Two
fields answering "where is this host" differently. Now one live endpoint read
owns both fields; configured relay intent is not advertised as a working route.

**Adoption bypassed the dialability authority.** `_refreshStoredTransports`
checked only for an empty report before replacing stored candidates. A report
carrying nothing dialable would have overwritten a working set and stranded the
device until re-pairing — the exact failure the extracted rule exists to
prevent. It now asks the native config parser, which applies the dialer's own
rule.

**Unrelated formatting rode along.** A blanket `dart format lib test` reflowed
24 files this change never touched, and inside two files it did touch it
reformatted unrelated types. Reverted; `i18n.dart` went from 363 changed lines
to 10, `remote_models.dart` from 172 to 90. Format only what you edited.

**Three copies of one shape.** `updateHostName`, `replaceTransports`, and
`rememberDirectAddr` were line-for-line variants of the same map-and-preserve
walk. Collapsed onto one `_mutateDevice`, and `_sameTransports` replaced with
`listEquals` from the framework.

Two findings were checked and rejected: `_rememberDirectAddr` reading
`_activeDevice` is safe because stale transport events are already dropped by
the connection-generation guard before reaching it, and the endpoint eviction
after a fully failed dial does not contradict the relay-timeout scenario — that
decision is about a wedged path, not about the relay. The scenario wording was
tightened to say so.
