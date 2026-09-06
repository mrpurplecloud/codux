---
created_at: 2026-09-06T12:52:07Z
updated_at: 2026-09-06T12:52:07Z
---

## Why

A mobile controller cannot connect to its host — not directly, not over relay — whenever the phone runs a third-party SD-WAN or VPN application that captures the default route and DNS. This happens even when the host is reachable on the SD-WAN's own virtual network, and even when the two devices share a LAN.

The cause is a self-imposed precondition, not a transport limitation. The iroh controller transport treats relay registration as a hard gate: it waits for the endpoint to register a home relay, and on timeout it closes the endpoint and returns an error. The dial loop that would have tried the direct addresses carried in the pairing ticket never executes. Field logs show the failure is total and repeating — every attempt ends at exactly the relay timeout, and no dial log line is ever emitted.

The gate contradicts both the upstream contract and this repository's own tests. Upstream documents `online()` as a convenience signal, not a precondition for `connect()`, and states that a connect with direct addresses establishes a direct connection without involving a relay. This repository's existing iroh transport tests already prove it: they bind endpoints with relay disabled and connect them to each other successfully.

Two adjacent defects turn a recoverable outage into a permanent one:

- **Reconnect has two authorities.** A single failed connect is reported both as a transport state event and as a rejected connect future, and each path independently schedules a reconnect. The attempt counter advances twice per failure, so backoff reaches its ceiling in half the expected time, and the retry loop never stops — it re-dials indefinitely at the ceiling interval, draining battery in the background.
- **The pairing ticket never refreshes.** A controller stores the host's address set at pairing time and reuses that snapshot forever. When the host changes network, address, or gains a new interface, the controller has no way to learn the new addresses except through the relay — the very path that may be unavailable.

## What Changes

- Make relay registration a best-effort signal rather than a connect precondition, on both the controller and the host transport. When no home relay is available, keep the endpoint alive and dial the direct addresses already carried by the pairing ticket.
- Never close or discard an endpoint because its relay is unreachable. That endpoint is the only object still capable of a direct connection.
- Derive dial attempt count and per-attempt timeout from whether a relay home exists, so a relay-less connect fails faster than the current gate rather than slower. Direct dialling either completes quickly or the address is unreachable; it does not benefit from the relay path's escalating ladder.
- Report relay-less failures with a distinct error so logs distinguish "relay unreachable and no direct address worked" from "relay unreachable".
- Give reconnect scheduling one owner. A single connection failure SHALL advance the attempt counter once regardless of how many layers observe it.
- Bound the reconnect loop. Sustained failure SHALL stop automatic retries and surface a resumable state instead of retrying forever at the ceiling interval.
- Let a controller remember the transport address that last produced a working connection, and merge it with the ticket's addresses on the next dial. This is client-local and requires no protocol change.
- Add a host-to-controller address announcement on the established control link. The host publishes its current address set; the controller replaces its stored transport candidates and persists them, so stored addresses track host network changes instead of freezing at pairing time.

## Impact

- Affected capability: `remote-transport-connectivity` (new)
- Primary implementation areas:
  - iroh controller transport: endpoint acquisition, relay gating, dial policy
  - iroh host transport: endpoint bind, relay gating, transport candidate publication
  - Mobile connection state: reconnect scheduling, backoff, retry termination
  - Mobile device storage: stored transport candidates, last-known-good address
  - Remote protocol: one added host-originated message type
- Protocol document `docs/protocol.md` requires an update for the added message. The message is additive; a controller that ignores it keeps current behaviour, and a host that never sends it leaves the controller on ticket addresses.
- Stored device records gain optional backward-compatible fields. Records written by earlier builds remain readable, and absent fields fall back to current behaviour.
- No public API is widened for testability. The relay/dial policy is extracted as a pure function so it can be unit tested without touching the process-shared endpoint pool.
- Estimated effort: 3 to 5 engineering days, dominated by the address-announcement round trip and its cross-client regression coverage.

## Baseline

- Annotated tag: `pre-iroh-relay-gate-20260906`
- Commit: `a629cc80` (2.0.3-beta.7), six commits after `v2.0.4`
- Tag excludes generated, untracked `.codegraph/` data and an uncommitted `apps/mobile/pubspec.lock` modification present in the working tree at tagging time.
