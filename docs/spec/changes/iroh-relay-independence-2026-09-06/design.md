## Context

The mobile controller has exactly one remote transport: iroh. Its connection lifecycle has three stages, and the first one currently gates the other two.

1. **Endpoint acquisition.** A process-shared controller endpoint is built once per relay configuration and reused by every later connection. Building it binds a UDP socket, enables LAN mDNS address lookup, and then waits for the endpoint to register a home relay. The wait is a hard gate: on timeout the endpoint is closed and an error is returned to the caller.
2. **Dial.** A retry loop dials the peer's `EndpointAddr`, parsed from the pairing ticket, with an escalating per-attempt timeout. Exhausting the loop retires the pooled endpoint so the next attempt rebuilds it.
3. **Stream handshake.** Control and terminal streams are opened over the established connection under their own timeout.

Because stage 1 gates stage 2, a controller whose relay is unreachable never dials. The direct addresses that the ticket carries — LAN addresses, and the virtual addresses of any SD-WAN or VPN interface the host is attached to — are parsed and then discarded together with the endpoint.

### Canonical terms

- **Home relay**: the relay server an endpoint has registered with, used to route dials to peers that have no working direct path.
- **Endpoint address**: an endpoint's advertised address set. It is a list, containing every relay URL the endpoint has registered with and every direct IP address it has bound or discovered. It is not a choice between relay and direct.
- **Transport candidate**: the stored description of how to reach one host — its node identity, relay URL, and pairing ticket. A controller keeps one per paired host.
- **Pairing ticket**: a serialized endpoint address captured at pairing time and stored in the transport candidate.
- **Relay-less connect**: a connect attempt made by an endpoint that has no home relay, relying entirely on the direct addresses in the peer's endpoint address.
- **Reconnect authority**: the single component permitted to advance the reconnect attempt counter and schedule the next attempt.

## Problem Statement

Three defects compound into a total, self-perpetuating outage.

**The relay gate makes an available path unreachable.** Upstream treats relay registration as a convenience signal and documents that a connect carrying direct addresses establishes a direct connection without a relay. This repository's own iroh transport tests bind endpoints with relay disabled and connect them successfully — relay-less connect is already proven in-tree. The gate's inline justification is that a relay-routed dial cannot complete without a relay home. That is true of relay-routed dials and false of direct dials, and the code applies it to both.

**Reconnect is scheduled by two authorities.** One failed connect surfaces twice: once as a transport state event, once as a rejected connect future. Each observer schedules a reconnect. Only one timer survives, so the retry rate is correct, but the attempt counter advances twice per failure and backoff reaches its ceiling in half the intended number of failures. The loop has no termination condition, so a host that is unreachable for structural reasons is re-dialled forever at the ceiling interval while the application is in the foreground.

**Stored addresses are frozen at pairing time.** A transport candidate is written once, at pairing, and never updated. A host that changes network, gains an interface, or is re-addressed becomes reachable only through the relay, because the relay is the only channel that can still resolve its current address. When the relay is also unavailable, the controller has no route to discovery at all.

## Solution

### Relay becomes a signal, not a gate

Endpoint acquisition still waits for relay registration, but the wait's result becomes a value rather than a control-flow branch. On timeout the endpoint is retained, a `relay_online: false` marker is recorded alongside it in the pool, and the caller proceeds to dial. The endpoint keeps its own relay reconnection running, so a relay that recovers later brings the pooled endpoint online without a rebuild.

The same change applies to the host transport, with one addition: the host currently requires a resolvable relay URL to publish a transport candidate at all. A host without a relay home SHALL still publish a candidate, carrying its ticket and an empty relay URL. The ticket alone is sufficient for a controller on a shared network to dial it.

### Dial policy is a pure function

Attempt count and per-attempt timeout are derived from relay availability by a pure function that takes the relay state and returns the dial schedule. Relay-routed dials keep the current escalating ladder, which exists because the first relay-routed dial routinely stalls while the relay primes the cross-path. Relay-less dials get a short, flat schedule: a direct address either answers quickly or is unreachable, and the escalating ladder only delays the failure.

This function is the testing seam. It is pure, so it is unit-testable without touching the process-shared endpoint pool, whose static lifetime makes it hostile to parallel tests. End-to-end relay-less connection behaviour is already covered by the existing in-tree tests that bind relay-disabled endpoints and connect them.

The relay-less schedule SHALL be bounded so that total relay-less dial time does not exceed the current relay gate timeout. A user whose relay is unreachable must not wait longer for failure than they do today.

### One reconnect authority

The transport-closed handler becomes the sole reconnect authority. The connect-failure path reports the failure and defers scheduling to that handler rather than scheduling in parallel. Backoff computation moves into a small policy object owned by that authority, following the same pattern as the other extracted mobile controllers: pure state, no widget dependency, directly unit-testable.

The policy gains a termination condition. After a bounded number of consecutive failures the policy stops scheduling and reports an exhausted state; the user-visible connection state becomes resumable rather than perpetually reconnecting. Any successful connection resets the policy.

### Addresses learn to move

Two independent mechanisms, deliberately not merged:

**Client-local last-known-good.** When a connection succeeds, the controller records the transport address that carried it. The next dial merges that address with the ticket's addresses. This requires no protocol change and no host cooperation, so it works against any host build. Its limit is that it only helps for addresses that have worked before.

**Host address announcement.** Once a control link is established, the host sends its current endpoint address to the controller. The controller replaces the stored transport candidate and persists it. This is the general fix: stored addresses track the host's actual network state rather than the state at pairing time. It is additive to the protocol — a controller that ignores the message keeps current behaviour, and a host that never sends it leaves the controller on ticket addresses plus its own last-known-good memory.

The two are kept separate because they fail differently. The local memory survives a host that cannot announce; the announcement survives a first connection from a controller with no memory. Neither subsumes the other.

### What is deliberately not changed

The stream handshake timeout, the terminal and control stream protocol, the blob upload path, and the relay preset list are untouched. The relay remains the default and preferred path whenever it is available: nothing here makes a working relay less used, only makes its absence non-fatal.
