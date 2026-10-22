# Codux Agent Instructions

These rules supplement global agent instructions for this repository.

## Product Versioning

- Desktop, mobile, and headless agent are independently versioned products.
  Bump only products whose shipped artifact changes; never synchronize versions
  merely because another product releases.
- Rebuilding an unchanged product may retain its existing version. A source
  change included in that product's shipped artifact requires its own bump.
- Record cross-product requirements and verified combinations in
  `docs/version-compatibility.md`. Equal version numbers never imply
  compatibility.

## Local Delivery

- Reuse `tools/local-delivery.py` for local macOS desktop packaging, Android
  sideload delivery, and existing macOS headless service upgrades. Commands and
  verification boundaries are in `tools/LOCAL-DELIVERY.md`; do not recreate
  one-off deployment scripts for these paths.
- Local Android delivery targets `com.duxweb.codux.sideload` and uses Android
  Studio SDK ADB. A different package requires an explicit user request.
- Apply the product versioning rules before building. Delivery scripts read
  source versions; they do not increment versions on every rebuild.

## Agent Session Activity

- Before changing Agent process activity, remote activity snapshots, project
  badges, or the session overview, read [CONTEXT.md](CONTEXT.md) and
  [the developer handoff](docs/agent-session-overview-development.md).
- The shared runtime owns activity facts. Desktop UI mirrors and summarizes
  snapshots; recent terminal text does not prove user input is required or work
  is complete.

## State-Machine Convergence

Apply this section only when reproduced evidence or an accepted contract shows
that the authorized invariant spans asynchronous, concurrent, UI/event, or
multi-file work. Mere presence of asynchronous code or a hypothetical race does
not activate it. This section constrains authorized work; it never authorizes
new behavior, files, mechanisms, or verification.

- Give each invariant one semantic owner. That owner reconciles actual state
  toward current intent.
- Treat epochs, serials, in-flight futures, cached widget properties, callback
  results, and individual write results as coordination metadata, not separate
  sources of truth.
- A superseded operation must not silently return and strand newer intent.
  Discard stale effects, then continue reconciliation until actual state matches
  current intent.
- Publication is part of completion. Do not report success, clear retry state,
  or stop a worker until result is still current, live, and published.
- Multi-step mutations must converge after interruption. Order steps so a
  partial failure preserves evidence needed for an idempotent retry; never make
  later cleanup depend only on whether an earlier step changed data in the same
  run.
- Make one real-time decision per invariant. Cached UI state may describe a
  view, but must not independently authorize an event when current state is
  checked elsewhere.
- When adding a guard, future, flag, counter, fallback, or callback check,
  identify existing mechanisms protecting the same invariant. Remove obsolete
  compensation. Keep multiple guards only when they protect distinct invariants
  or trust boundaries, and document that distinction.
- Preserve rejection and failure signals across abstraction boundaries. Do not
  erase a boolean or error and then let the caller assume success.

Regression candidates, not default requirements. Run only cases tied to the
reproduced failure or changed contract. Establish baseline first; a pre-existing
failure does not authorize another fix.

- `A -> B -> A` command reversal while `A` remains in flight.
- Completion after stop, cancellation, replacement, or ownership loss.
- Terminal event before publication or registration completes.
- State change between UI build and input event, including paths without a
  rebuild.
- Failure between two persistent writes, followed by retry.
- Duplicate delivery, restart, and stale completion for durable work.

Review test within authorized invariant: if two fields or layers can answer the
same state question differently, design is not complete.
