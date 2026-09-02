## Context

Codux has two project stores with different ownership rules:

- Desktop `ProjectStore` owns rich controller records, including runtime target, local UI metadata, worktrees, and selection.
- Headless `AgentProjectStore` owns a small persistent list of `{ id, name, path }` used by Mobile and by Headless project-scoped operations.

Desktop's remote-folder save path currently writes only a `ProjectRecord` with a remote runtime target. It does not call `project.add` on Headless. Headless handles `project.list`, `project.add`, and `project.remove`, but not `project.edit`; mutation results return only to the requester, and `host.info` advertises only terminal resource subscriptions. Mobile already emits all project CRUD envelopes, subscribes to resource `projects` when advertised, and replaces its list from `project.list` snapshots.

Current Desktop project IDs and Headless project IDs are also different. Desktop derives an ID from controller-side project data and runtime target. Headless derives and persists its own path-based ID. Passing a Desktop ID as a Headless `projectId` therefore relies on path fallbacks and can misroute ID-only terminal, Git, AI, or worktree operations.

### Canonical terms

- **Headless project catalog**: persistent set of projects directly served by one Headless host.
- **Host project ID**: stable ID stored by that Headless host and used in every host-scoped protocol request.
- **Desktop project ID**: controller-local ID used by Desktop UI state and local relationships.
- **Remote project projection**: Desktop cache record representing one Headless catalog entry.
- **Selected project**: per-client UI choice. Selection is not catalog membership and is not shared.

## Problem Statement

Users expect every controller connected to one Headless host to see and manage one shared project catalog. Today, a project created from Desktop can exist only on Desktop, while a project created through Headless can exist only in Headless and Mobile views. Switching controller loses discoverability and makes project-scoped IDs inconsistent.

## Solution

Treat Headless as the single writer for its project catalog. Controllers subscribe to versioned full snapshots and keep only local projections. Every remote mutation commits on Headless first; only a successful authoritative snapshot changes controller state. Desktop translates its local project ID to explicit `hostProjectId` at the transport boundary. One-time migration registers existing Desktop-only remote records without duplicating paths.

## Goals / Non-Goals

### Goals

- Every current client sees the same project membership, name, path, and host ID for one Headless host.
- Connected clients receive changes without reconnecting or polling.
- Reconnect and Headless restart reproduce the same catalog.
- Desktop keeps current UI architecture through a synchronized local projection.
- Project-scoped requests always carry the Headless ID expected by the host.
- Legacy Desktop remote records migrate without duplicates and without losing controller-only metadata.
- Concurrent client mutations cannot lose unrelated updates or corrupt persistence.
- Older clients and older Headless versions retain current behavior through capability-based fallback.

### Non-Goals

- Sharing selected project, project order, badges, environment settings, terminal layout, window layout, or other client preferences.
- Synchronizing project catalogs across different Headless hosts.
- Chained Desktop-to-Headless-to-another-host forwarding.
- Cloud account synchronization.
- Deleting project directories or terminating existing terminal sessions when catalog membership is removed.
- Replacing the full-snapshot protocol with event sourcing or per-field conflict resolution.

## User Stories

1. As a Desktop user, I want a Headless project I add to appear on Mobile, so that I can continue from another controller.
2. As a Mobile user, I want a Headless project I add to appear on Desktop, so that both controllers expose the same host state.
3. As a connected user, I want project edits and removals to appear without reconnecting, so that lists do not become stale.
4. As a returning user, I want the catalog to survive Headless restart, so that controller choice does not affect persistence.
5. As a Desktop user, I want cached Headless projects to remain visible while disconnected, so that temporary network loss does not erase navigation state.
6. As a Desktop user, I want disconnected remote mutations to fail clearly, so that a local phantom project is not mistaken for committed host state.
7. As a user with two Headless hosts, I want their catalogs isolated, so that equal paths or IDs on different machines never collide.
8. As a user with existing Desktop-only remote records, I want them registered once after upgrade, so that enabling sharing does not discard prior setup.
9. As a user editing a project path or name, I want its host ID to remain stable, so that project-scoped history and requests keep working.
10. As a user adding the same host path twice, I want one catalog entry, so that retries and concurrent controllers remain idempotent.
11. As a user removing a project, I want only catalog membership removed, so that source files remain untouched.
12. As a user switching selected projects on one client, I want other clients' current views unchanged, so that controllers do not fight for focus.
13. As a user on mixed versions, I want one-shot `project.list` to continue working, so that upgrades can roll out incrementally.
14. As a maintainer, I want a deterministic two-client regression test, so that the original cross-client visibility bug cannot return.
15. As a maintainer, I want failed or malformed persistence to preserve the last valid catalog, so that recovery never silently overwrites user data.

## Implementation Decisions

### Decision: Headless owns one catalog per host

Headless is authoritative for shared fields: membership, host project ID, name, and path. Catalog scope is one running host identity and its data directory. Different hosts never merge automatically.

A controller may cache these fields, but cannot create authoritative remote membership while disconnected. This matches terminal and file ownership: Headless is the machine that can serve the path.

Alternative considered: merge every controller's local project store. Rejected because no stable conflict authority exists, stale clients can resurrect deleted entries indefinitely, and controller-only metadata would leak across devices.

### Decision: Full versioned snapshots over existing resource subscription

Headless advertises resource `projects` in `host.info`. New clients send `resource.subscribe` with `resource: "projects"` and `baseline: true`. Headless replies with a versioned `project.list` baseline.

After a successful add, edit, or remove, Headless increments project resource version once and sends the same logical catalog snapshot to all project subscribers plus the mutation source. Clients reconcile only snapshots newer than the last applied version for the current host runtime instance. Version guards reset after reconnect or runtime-instance change.

Full snapshots are retained because catalogs are small, reconciliation is idempotent, and dropped intermediate messages do not require replay. Existing `RuntimeSubscriptionRouter` and Desktop-host project broadcast behavior are prior art.

### Decision: Use standard mutation acknowledgement plus authoritative snapshot

Headless supports `project.add`, `project.edit`, and `project.remove`. Successful mutations send `project.updated` to the requester and then publish `project.list`. Failed validation or persistence sends `error` and publishes nothing.

Old clients may ignore `project.updated`; they still receive the source-inclusive `project.list` snapshot. One-shot `project.list`, `project.add`, and `project.remove` remain supported. This aligns Headless with existing Desktop-host semantics instead of creating a second protocol.

### Decision: Separate Desktop ID from host ID

Remote `ProjectRecord` gains optional serialized `hostProjectId`. Desktop keeps its existing local ID for UI state, selected project, layout, and controller-local relationships. Fresh projections derive a stable local ID from `(deviceId, hostProjectId)`. Migrated records preserve their current local ID.

At the remote transport boundary, every project-scoped outbound envelope uses `hostProjectId` as `projectId`. Inbound Headless records and worktree relationships map that host ID back to the Desktop projection ID. Path remains a compatibility and execution hint, not identity.

Alternative considered: reuse Headless ID as Desktop `ProjectRecord.id`. Rejected because two Headless hosts can produce the same path-derived ID and because replacing legacy Desktop IDs would orphan local layout and selection references.

### Decision: Desktop keeps a projection, not a second authority

A single project-store reconciliation operation applies one Headless snapshot for one device:

1. Match managed records by `(deviceId, hostProjectId)`.
2. Preserve matched Desktop IDs and controller-only fields.
3. Update shared name and path from Headless.
4. Add missing projections with stable composite-derived Desktop IDs.
5. Remove managed projections absent from the authoritative snapshot.
6. Leave local, WSL, other-host, and not-yet-migrated legacy records untouched.

Apply reconciliation in one store transaction. Existing selection fallback handles removal of a selected projection. While disconnected, the last projection remains a read-only cache.

### Decision: Remote mutations commit before local projection changes

Desktop add, edit, and remove flows branch on runtime target. Local and WSL flows remain unchanged. For Headless projects:

1. Send mutation to Headless using host ID and host path.
2. Wait for success acknowledgement or authoritative list.
3. Reconcile returned or broadcast snapshot.
4. Select or refresh the resulting Desktop projection.

No optimistic project-store mutation occurs. Network, validation, or persistence failure leaves Desktop projection unchanged and surfaces an error.

### Decision: One-time union migration, then strict Headless authority

Desktop stores a migration version per Headless device. On first successful connection to a Headless version advertising project subscriptions:

1. Pull a baseline catalog.
2. Submit every legacy remote record without `hostProjectId` through idempotent `project.add`.
3. Let Headless deduplicate by host-native normalized path. Existing Headless entries retain their current name and ID; migration does not rename them.
4. Pull or await the resulting snapshot and attach host IDs while preserving Desktop IDs and local metadata.
5. Persist the migration marker only after every record and final reconciliation succeed.

Retries are safe. Partial failure keeps the marker unset and does not prune legacy records. Later connections use strict Headless snapshots and do not re-upload a deleted projection.

Trade-off: first migration from several controllers forms a union of their legacy records. This favors preservation during upgrade. After each controller records migration completion, Headless removal remains authoritative.

### Decision: Stable, serialized Headless persistence

Headless constructs one shared project store for the host process. A mutation lock covers load, validation, deduplication, edit/remove, persistence, and committed snapshot creation. Broadcast happens only after durable save succeeds and after releasing the store lock.

`project.add` is idempotent by host-native normalized path and does not rename an existing entry. `project.edit` preserves host project ID and rejects a path already owned by another entry. `project.remove` is idempotent and never deletes filesystem content.

Missing catalog file means an empty catalog. Existing unreadable or malformed catalog means an error, not an empty catalog; mutations must not overwrite it. Save uses a same-directory temporary file and atomic replacement so readers observe either old or new complete JSON.

### Decision: Client state remains local

Shared snapshots exclude selected project, ordering, badges, environment variables, pane layout, window layout, and transient editor state. Headless catalog changes can invalidate a removed selection on each client, but one client's selection never changes another client's selection.

### Decision: Mobile production code should remain unchanged

Mobile already emits add/edit/remove messages, subscribes to `projects` when the capability is advertised, handles `project.updated`, parses `project.list`, and applies resource-version guards. Work should add regression tests around Headless capability and cross-client snapshots. Production changes require a demonstrated contract mismatch.

## End-to-End Flows

### Connect and reconcile

1. Controller connects and reads `host.info`.
2. If `projects` subscriptions are advertised, controller subscribes with baseline.
3. Headless registers device and sends current versioned catalog.
4. Desktop runs pending legacy migration, then atomically reconciles its projection.
5. Mobile replaces its in-memory project list through existing parsing.

If capability is absent, Desktop preserves current local behavior and performs one-shot `project.list` where already supported. Full sharing is enabled only after Headless upgrade.

### Add or edit

1. Client sends `project.add` or `project.edit`.
2. Headless validates and serializes mutation against latest catalog.
3. Headless atomically saves catalog.
4. Headless sends acknowledgement to source.
5. Headless emits one newer full snapshot to subscribers and source.
6. Desktop and Mobile reconcile snapshot; per-client selection stays local.

### Remove

Removal follows same commit and fan-out sequence. It removes catalog membership and subscription state associated with that project where appropriate. Existing filesystem data remains. Existing terminal-session lifecycle keeps current behavior.

### Restart and reconnect

Headless loads persisted catalog once. A new runtime instance starts a new resource-version epoch. Clients reset their version guard, request a baseline, and reconcile. Desktop cached projection may be displayed before baseline but cannot override it after connection.

## Testing Decisions

Tests assert external behavior through highest practical seams. Internal helper tests supplement them only for path identity, migration, and atomic-store edge cases.

### Primary red/green seam

Use Headless host with in-memory remote transport and two logical device IDs:

1. Both devices subscribe to project resource.
2. Device A adds a project using Desktop-style payload.
3. Device B receives same versioned `project.list` without issuing another request.
4. Edit and remove repeat same assertion.
5. Recreating host store from same temporary data directory returns final catalog.

This test fails on baseline because Headless neither advertises project subscriptions nor broadcasts mutations. It directly captures reported symptom.

### Required regression coverage

- Subscribe returns a baseline and unsubscribe/disconnect stops future fan-out.
- One mutation produces one shared resource version across every recipient.
- Out-of-order or duplicate snapshots do not roll a client backward.
- Two concurrent adds to different paths preserve both entries.
- Concurrent same-path adds produce one entry and one stable host ID.
- Edit preserves ID and rejects collision with another normalized path.
- Failed save emits no success acknowledgement or newer snapshot.
- Malformed existing JSON is reported and not overwritten.
- Restart preserves catalog and reconnect resets version epoch safely.
- Desktop remote add does not create a local phantom when Headless rejects it.
- Desktop projection keeps local ID and metadata while shared name/path update.
- Terminal, Git, AI, and worktree requests use `hostProjectId` even when Desktop ID differs.
- Legacy migration is idempotent, preserves Desktop IDs, and never duplicates a path.
- After migration marker is stored, a Headless deletion is not resurrected on reconnect.
- Equal host project IDs on two devices remain isolated by device ID.
- Mobile existing CRUD and subscription tests pass without production changes.
- Local, WSL, and Desktop-host project behavior remains unchanged.

## Risks / Trade-offs

- **Desktop unsolicited-event seam**: Remote controller currently emphasizes request/reply. Receiving project snapshots may require a narrow event callback or channel. Keep it project-resource-specific unless an existing generic resource stream already fits.
- **ID translation blast radius**: Project-scoped operations are spread across terminal, Git, AI, file, and worktree services. Centralize translation at existing remote target resolution instead of patching individual UI callers.
- **Migration resurrection**: First upgrade intentionally unions legacy controller records. Migration marker prevents future resurrection, but a stale record removed on Headless before that controller's first upgrade can return once. Avoiding this would require server tombstones unavailable in current data.
- **Atomic replacement portability**: Same-directory replacement must be verified on Windows and Unix. Use existing dependencies or standard platform support before adding a new dependency.
- **Version epoch**: Resource versions can restart with Headless process. Clients must scope comparison to runtime instance, not persist a single number forever.
- **Catalog corruption visibility**: Changing malformed JSON from silent empty fallback to explicit error can surface existing damage. This is intentional to prevent overwrite; error text must point to catalog path and recovery action.

## Migration Plan

1. Land additive Headless capability, edit, subscription, broadcast, and persistence safeguards.
2. Land Desktop `hostProjectId`, transport translation, snapshot reconciliation, and capability-gated remote mutations.
3. Run one-time legacy migration after first valid baseline from upgraded Headless.
4. Keep fallback for Headless without project-subscription capability.
5. Remove no legacy fields in this change. Optional `hostProjectId` allows rollback to baseline code without making records unreadable.

## Work Estimate

- Headless store, edit parity, subscription, fan-out: 1 to 1.5 days
- Desktop projection, ID translation, mutation flow: 1.5 to 2.5 days
- Legacy migration and compatibility fallback: 0.5 to 1 day
- Automated regression suite and manual multi-device matrix: 1 to 2 days
- Total: 4 to 7 engineering days

Largest uncertainty: Desktop transport path for unsolicited versioned snapshots. If reusable event plumbing already exists, estimate trends toward lower bound.

## Out of Scope

No terminal-session sharing changes, project metadata synchronization, cross-host catalog merge, cloud persistence, or directory deletion.

## Further Notes

No blocking design question remains. Stage 2 must follow `tasks.md` in order and stop if implementation evidence contradicts ID translation or unsolicited-event assumptions.

