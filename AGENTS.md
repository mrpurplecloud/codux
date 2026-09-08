# Codux Agent Instructions

These rules supplement global agent instructions for this repository.

## State-Machine Convergence

Apply this section when one invariant spans asynchronous, concurrent, UI/event,
or multi-file work.

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

Required regression coverage where relevant:

- `A -> B -> A` command reversal while `A` remains in flight.
- Completion after stop, cancellation, replacement, or ownership loss.
- Terminal event before publication or registration completes.
- State change between UI build and input event, including paths without a
  rebuild.
- Failure between two persistent writes, followed by retry.
- Duplicate delivery, restart, and stale completion for durable work.

Review test: if two fields or layers can answer the same state question
differently, design is not complete.
