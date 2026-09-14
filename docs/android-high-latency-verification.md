# Android high-latency connection verification

Measured 2026-09-13, using Android Studio SDK ADB at
`/Users/mac/Library/Android/sdk/platform-tools/adb` and a physical FCP-AN10 running
Android 16. Only injected RTTs of 1, 2, and 4 seconds were used in this comparison.
The earlier relay-startup optimization in `07d0c791` is the baseline, not a result
of this change.

Follow-up audit: connection timing alone did not establish stability. An initial
run showed anomalous UDP volume, and a later run was interrupted by Android
moving the diagnostic app into the cached background state. A two-machine audit
reproduced the traffic as repeated QUIC `PATHS_BLOCKED` frames on retained
connections. The diagnostic peer did not close old connections as the production
host does; the production host did not reproduce the flood in the same sequence.
An independent failing test confirmed that controller shutdown itself relied on
the peer closing the connection. That ownership defect is now fixed. Final
Android direct-path and relay-only checks passed after the phone reconnected;
the controlled comparison is recorded below.

## Reproduced defect and change

At 4 seconds RTT, normal propagation plus scheduling takes a QUIC handshake just
beyond the 4-second retry boundary. The previous controller cancelled that
handshake and started another, which encountered the same boundary. With no relay
home, both 4-second attempts could fail despite a reachable peer. Retiring the
endpoint then let a fresh connection succeed, because its existing startup window
allowed the first handshake to continue longer.

Retries now retain unfinished dials while starting the next scheduled attempt.
The first successful QUIC connection wins; leaving the dial scope drops all
remaining futures before application streams are initialized. Failed dials leave
the set without discarding other attempts. Attempt counts, relay startup lifetime,
total retry budgets, stream ACKs, endpoint eviction, and the wire protocol are
unchanged. At most two dials coexist without a reported relay home, or three with
one.

Controller shutdown also explicitly closes its own QUIC connection. Previously
it only dropped its handle; readers and the path watcher retained other handles,
so closure depended on the peer. The shared endpoint remains open for other
controllers and blob serving. This fixes connection ownership rather than adding
a cleanup timer, changing path limits, or modifying Iroh dependencies.

## Device method and scope

An independent `com.duxweb.codux.networklab` APK used the production Dart
`RustControllerTransport`, Android FFI, and shared Rust controller. The installed
`com.duxweb.codux` application and its data were not replaced. The diagnostic peer
spoke the existing control/terminal stream handshake and returned protocol pongs;
it did not run the desktop UI or headless application domain.

For direct-path tests, the peer bound QUIC only to loopback, disabled relay and
address lookup, and accepted traffic through a dedicated LAN UDP proxy. Each
direction added 500, 1000, or 2000 ms. Android supplied only the proxy address and
an unavailable relay URL. Reported selected paths remained the proxy
`10.10.10.16:45231`; Iroh could not upgrade to an unshaped host interface.
Clash/Surge and system routing configuration were not changed.

Each condition used two connections in the same app process and five protocol
pings per successful connection. These are small diagnostic samples, not a
statistical reliability estimate. Timings include real Android FFI polling.
The following Android results predate the shutdown correction; "After" means
the retry change alone, not the final working-tree version.

| Injected RTT | Before: connection ready | After: connection ready | After: median pong RTT |
| --- | --- | --- | --- |
| 1 s | 2.503 s, 2.297 s | 2.305 s, 2.294 s | 1.118 s |
| 2 s | 4.424 s, 4.429 s | 4.423 s, 4.428 s | 2.045 s |
| 4 s | failure at 8.090 s, then 8.595 s | 8.384 s, 8.478 s | 4.064 s |

At 4 seconds RTT, two protocol stages explain approximately eight seconds to
readiness: QUIC establishment followed by acknowledged application stream setup.
The fixed defect added an entire failed connection cycle before that useful work.
The 100 ms idle FFI polling delay was not the largest measured bottleneck and was
not changed.

Relay-only was also measured after the change, with the peer's IP transports
disabled and a local relay. A TCP forwarding process delayed relay byte delivery
by two seconds in each direction on the Android side. This measures QUIC and
application latency over relay; it does not model WAN TCP ACK timing, congestion,
or packet loss. Native path reports confirmed `path=relay`, with no direct route.
The cold connection took 17.187 s, including relay setup; endpoint reuse took
8.416 s. All ten pings completed, median 4.172 s and maximum 4.195 s. The endpoint
did not report a home relay during these dials, although relay delivery worked;
the shorter retry schedule therefore applied. Relay-only before/after timing was
not compared, so this is a compatibility check, not a claimed speedup.

The initial 3% random-loss experiment recorded one failed warm connection and one
successful fresh connection both before and after this change, at 4 seconds RTT.
The successful connections took 9.028 s and 9.034 s respectively, with pong tails
of 11.202 s and 13.029 s. Attributing those failures to retransmission exceeding
the eight-second budget was premature: the loss sequences differed, foreground
state was not recorded, and the after-run proxy count rose from 1,195 packets to
44,908 during the no-loss 4-second condition, then to 104,870 during random loss.
The subsequent two-machine audit below identifies the traffic type and a
diagnostic-peer lifecycle difference; these random-loss samples still do not
establish a reliability comparison.

## Follow-up measurements

Fixed packet-loss cases used 4 seconds RTT and the retry change alone, before
the shutdown correction:

| Dropped response | Connection ready | Protocol pings |
| --- | --- | --- |
| First standalone server Handshake packet | 8.453 s | 3/3 |
| First server Initial datagram | 10.628 s | 3/3 |
| First client Handshake packet | 8.494 s | 3/3 |
| First two large server Initial datagrams, run 1 | 10.518 s | 3/3 |
| First two large server Initial datagrams, run 2 | 10.608 s | 3/3 |

The proxy logged each dropped datagram's direction, size, phase, and time. These
cases did not reproduce the random-loss failure, so they do not justify widening
the timeout. They also lacked continuous foreground logging; successful pongs
prove those exchanges completed, not a general reliability rate.

A subsequent delay-transition run reached its first 4-second connection in
8.497 s, completed two pings, then stopped receiving traffic. Android showed
the diagnostic process in `curProcState=15`, `cached=true`, with another app in
the foreground. Its later timeout is an invalid foreground-network sample.
This run did not establish a cause for the earlier packet-volume anomaly.

Prepared diagnostic A/B APKs now record lifecycle changes and invalidate runs
that enter the background. Both use identical Dart code; the baseline APK embeds
the preserved `07d0c791` Android native library, SHA-256
`aae2baa8fb1edb5fdc5f76856929e2f8f916d44eba7ac57233bb3802264863fc`.
The proxy records packet counts by direction and size. ADB disconnected during
this earlier phase, then reconnected for the final comparison below. No
additional production timeout or RTT cache was introduced.

## Two-machine packet-volume audit

With Android disconnected, a temporary native controller ran on `.18` against
the delayed peer on `.16`. Both are macOS arm64 machines. This is transport
integration evidence, not an Android device result. Only RTTs of 1/2/4 seconds
were used, with two connections and five pings per connection at each RTT.
The controller asserted that every measured pong crossed the configured delay.
A preliminary same-machine attempt was discarded: Iroh discovered a loopback
route that bypassed the proxy. Those timings are not included here.

The baseline controller was built from `07d0c791` in an isolated source copy;
all resolved dependencies match entries in the repository lockfile. Baseline
source SHA-256 for `iroh_link.rs`:
`e84270e1aaf4d2e3c73b943846326d6d086cff01d401ce6ae6812958935036bb`.

Against the original diagnostic peer:

- Baseline: 4-second RTT first connection failed in 8.007 seconds; the next
  connected in 8.388 seconds. 25 pongs completed; 562 proxy packets total.
- Retry change alone: both 4-second connections succeeded in 8.381 and
  8.360 seconds. 30 pongs completed; 69,901 packets total.
- Repeating with per-connection frame counters yielded 51,789 packets and
  30 pongs. The flood consisted of client `PATHS_BLOCKED` and server `PATH_ACK`,
  including traffic on older application connections.

Iroh 1.0.2 queues failed path opens by address, then retries each queued address
on every connection to that peer. Multiple blocked connections can multiply
queued work. The diagnostic peer retained connections after their streams ended,
while the controller relied on the peer to close them. This combination made the
initial timing comparison misleading; successful pongs did not imply low traffic.

A separate isolated source copy ran the actual `RemoteIrohHostTransport`,
including production stream, peer replacement, authentication and close logic.
Only its bind/address-lookup setup was constrained to loopback so the delay could
not be bypassed. No installed desktop/headless service was replaced. The retry
change alone completed all 30 pongs with 468 proxy packets and closed old peers.
The two 4-second connections took 8.354 and 8.352 seconds. Thus the diagnostic
flood is not, by itself, proof of a production retry regression.

The controller shutdown guard failed before the ownership correction with
`shutdown left the connection live until the peer closed it`, then passed after
explicitly closing the owned connection. It also verifies that another live
connection on the same endpoint keeps exchanging protocol messages.

After that correction, the original diagnostic peer scenario completed all
30 pongs with 450 proxy packets, compared with 51,789 and 69,901 before it.
Peer frame counters recorded old connections closing instead of accumulating.
Connection-ready times were 2.234/2.193 seconds at 1-second RTT,
4.336/4.331 seconds at 2-second RTT, and 8.356/8.358 seconds at 4-second RTT.
No Iroh dependency, retry budget, system route, or TUN configuration changed.

With the final controller and production host logic, all 30 pongs completed with
453 proxy packets. Connection times were 2.169/2.205, 4.329/4.335, and
8.359/8.358 seconds for the respective 1/2/4-second RTT conditions.

## Final Android comparison

The physical phone ran the diagnostic app in the leftmost foldable pane at the
user's request: task bounds `Rect(0, 0 - 1069, 2344)`. Android reported the app
`RESUMED` and visible. Lifecycle recording detected no background transition;
every reported sample retained `foreground_valid=true`.

Both APKs contained identical Dart/application payloads and differed only in
`lib/arm64-v8a/libcodux_protocol_ffi.so` apart from signing metadata. The baseline
native hash is recorded above; the final native SHA-256 is
`11eaa49b0357582b82ece071e12304837a5c643dc93d522dfcea5e93e302a30a`.

A preliminary Android attempt discovered an unshaped mDNS route and was rejected
by the measured-RTT assertion. The valid comparison used a fresh synthetic host
identity and disabled mDNS only in the isolated host process via
`CODUX_IROH_DISABLE_LAN_MDNS=1`. Its bound address was verified as
`127.0.0.1:45230`; selected Android paths remained `10.10.10.16:45231`.
Every pong was checked against the requested delay. No loss was injected.

| Injected RTT | Baseline connection ready | Final connection ready | Final pong median |
| --- | --- | --- | --- |
| 1 s | 2.399 / 2.286 s | 2.404 / 2.290 s | 1.113 s |
| 2 s | 4.409 / 4.419 s | 4.422 / 4.511 s | 2.033 s |
| 4 s | failed at 8.066 s / 8.898 s | 8.766 / 8.712 s | 4.061 s |

The final run completed all six connections and 30/30 protocol pongs, with
454 proxy packets total. The baseline completed five connections and 25 pongs,
with 418 packets; its first 4-second connection failed. Counts are diagnostic
samples, not statistical reliability or throughput estimates.

Final Android relay-only verification disabled the diagnostic peer's IP
transports and delayed relay byte delivery by two seconds each way. Both
connections reported `path=relay`. Cold readiness took 17.025 seconds, including
relay setup; endpoint reuse took 8.773 seconds. All 10 pongs completed, median
4.070 seconds and maximum 4.255 seconds, with valid foreground samples. As in the
earlier relay test, TCP ACK timing, WAN congestion and loss were not simulated.

The independent diagnostic APK was removed afterward. USB stay-awake was
restored from temporary `2` to original `0`. Temporary host services and `.18`
diagnostic binaries were removed. The installed official Android app remained
`2.0.3-beta.10` / versionCode `58`; its data and the existing sideload app were
not replaced. Clash, Surge and system routing were unchanged.

## Executable guards

- `slow_handshake_survives_a_retry_boundary`: actual QUIC acceptance delayed just
  beyond the first retry boundary; must connect and complete a protocol ping.
  Failed before the change and passed after it.
- `stalled_handshake_does_not_block_a_retry`: first handshake stalls, next one can
  complete; must retain the existing retry recovery path and complete a ping.
- Both guards now keep the winner live beyond the other dial's delayed arrival
  and complete another ping before shutting down the accept worker.
- `controller_shutdown_closes_its_connection_and_keeps_the_shared_endpoint_live`:
  shutdown closes locally despite retained observer handles, is idempotent, and
  preserves protocol pings on another connection sharing the endpoint.
- Existing transport checks cover offline endpoint reuse budget, relay-only
  availability, cancellation alongside a live connection, both stream ACKs,
  terminal traffic, and blob lifetime.

Run with `cargo test -p codux-remote-transport --lib`. All 68 tests passed after
the shutdown correction. The delayed-handshake checks use real sockets and
timers, not mocked connection results. Earlier, 29 Flutter transport/network/
reconnect/sync tests passed; Dart production source has not changed. Product
compilation (`cargo check -p codux -p codux-agent -p codux-protocol-ffi`) and
headless transport smoke (`cargo run -p codux-agent -- smoke transport`) passed
again for the final Rust changes.

The normal Android release-mode APK was rebuilt and its final copy verified:
`com.duxweb.codux`, `2.0.3-beta.12`, versionCode `61`, arm64, valid local signature.
Its native library matches the final diagnostic APK. APK SHA-256:
`ddeece127c3bad15f2cdfaaef809111a25af30d33f08b823d20ee8df9fef0694`.
It was not installed over the user's official app or published.

Temporary device logs and diagnostic sources are retained under `/tmp` on the
test machine. They are not shipped application code. Product-level Android
navigation, terminal rendering under load, Wi-Fi/cellular switching, and the
installed desktop/headless combinations were not verified by these transport
measurements.
