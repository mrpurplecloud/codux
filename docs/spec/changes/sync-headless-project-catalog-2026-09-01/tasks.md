---
created_at: 2026-09-02T06:22:45Z
updated_at: 2026-09-02T08:00:00Z
completed_at:
---

Implementation starts only after Stage 1 approval. Each section is one tracer-bullet ticket sized for a fresh context. Work blockers first. Baseline: `pre-headless-project-sharing-20260901`.

## 1. Headless Shared Catalog Contract

**What to build:** A Headless host that persists one authoritative catalog and pushes each committed mutation to two connected controllers.

**Blocked by:** None — can start immediately.

**Status:** done — commit `71b4344b`

- [x] 1.1 Add deterministic two-device integration test: both subscribe, device A adds, device B receives newer `project.list`. Run against baseline and record expected failure.
- [x] 1.2 Advertise `projects` resource subscription and return versioned baseline on subscribe.
- [x] 1.3 Add Headless `project.edit`; use `project.updated` acknowledgement and source-inclusive full-snapshot fan-out for add, edit, and remove.
- [x] 1.4 Reuse shared subscription/version routing; remove device subscriptions on disconnect and project-scoped state on removal.
- [x] 1.5 Serialize catalog read-modify-save; make add path-idempotent, edit ID-stable, and remove filesystem-safe.
- [x] 1.6 Treat malformed existing JSON as error and save through same-directory atomic replacement.
- [x] 1.7 Extend test through edit, remove, unsubscribe, concurrent adds, save failure, and store recreation after restart.
- [x] 1.8 Verify existing Headless terminal, Git, AI, and file integration tests still pass.

Out of the written scope but required by 1.4: the agent's `runtimeInstanceId` was a constant, so a restarted host reused a controller's stale version guard. It is now unique per host process.

## 2. Desktop Authoritative Add and ID Bridge

**What to build:** Desktop connects to upgraded Headless, imports its baseline, adds a remote project on Headless first, and routes project-scoped work with Headless ID.

**Blocked by:** 1. Headless Shared Catalog Contract.

**Status:** done — commit `94fe2eb5`

- [x] 2.1 Add failing Desktop integration test where Desktop ID differs from host ID; assert remote add commits on Headless before local projection appears.
- [x] 2.2 Add optional `hostProjectId` to remote project records without changing local or WSL schema behavior.
- [x] 2.3 Add one-transaction reconciliation for a Headless snapshot scoped by device ID; preserve controller-only metadata and existing Desktop IDs.
- [x] 2.4 Subscribe with baseline when Headless advertises `projects`; apply only newer snapshots within current runtime-instance epoch.
- [x] 2.5 Route remote add through Headless and reconcile success; retain local store unchanged on remote failure.
- [x] 2.6 Centralize outbound project ID translation so terminal, Git, AI, file, and worktree operations use `hostProjectId`.
- [x] 2.7 Map inbound Headless project/worktree relationships back to Desktop projection ID.
- [x] 2.8 Verify Desktop-created project appears to second subscribed device without reconnect.

Out of the written scope but required to avoid a regression: a Desktop host also
advertises the `projects` resource, so adopting every advertised catalog would
have imported another desktop's project list and pruned local records against
it. Catalog sharing is therefore gated on `host.info.app` being the headless
agent (`REMOTE_APP_AGENT`, new shared protocol constant), which keeps the
"Desktop-host project behavior remains unchanged" requirement true.

The version guard is scoped to the controller's connection epoch rather than to
a separately tracked runtime-instance id: a host restart always drops the link,
so a new epoch is exactly what a new runtime instance produces.

Two things 2.6/2.7 forced that the ticket text does not name. The host calls a
project's default worktree by the project's own id, so once outbound requests
carry the host id, the worktree ids coming back are host-scoped too; the id
bridge therefore also translates `selectedWorktreeId` and the `id` of a record
listed under a `projectId`. And 2.5 cannot read the cached capability probe: a
user can add a project in the first moments of a connection, so the host is
asked at mutation time. An unreachable host answers nothing and the add keeps
the pre-change local behaviour.

## 3. Legacy Desktop Projection Migration

**What to build:** Existing Desktop-only remote records become Headless entries once, without duplicates or lost Desktop metadata, then Headless becomes strict authority.

**Blocked by:** 2. Desktop Authoritative Add and ID Bridge.

**Status:** ready

- [ ] 3.1 Add failing fixture with legacy remote record lacking `hostProjectId` and an empty Headless catalog.
- [ ] 3.2 Add per-device migration version and run migration only after valid upgraded-Headless baseline.
- [ ] 3.3 Register legacy paths through idempotent Headless add; existing Headless path wins name and host ID.
- [ ] 3.4 Attach host IDs while preserving Desktop IDs, badges, environment settings, ordering, and selection when still valid.
- [ ] 3.5 Persist marker only after all registrations and final reconciliation succeed; partial failure must retry without pruning legacy records.
- [ ] 3.6 Repeat migration and reconnect tests; assert one entry per normalized path and no resurrection after marker plus Headless removal.
- [ ] 3.7 Test two controllers migrating disjoint legacy records; final Headless catalog contains union once.

## 4. Bidirectional Live Edit and Remove

**What to build:** Desktop and Mobile can edit or remove projects from either controller, and every connected controller reconciles live while keeping its own selection.

**Blocked by:** 2. Desktop Authoritative Add and ID Bridge.

**Status:** ready

- [ ] 4.1 Add Desktop remote edit/remove paths that wait for Headless commit and consume authoritative snapshot.
- [ ] 4.2 Add two-direction integration cases: Desktop mutation updates Mobile-style client; Mobile-style mutation updates Desktop projection.
- [ ] 4.3 Verify same-path retry stays idempotent, edit keeps host ID, and edit path collision returns error without broadcast.
- [ ] 4.4 Verify stale and duplicate snapshot versions cannot roll Desktop or Mobile backward.
- [ ] 4.5 Verify selection changes remain per-client and project removal does not delete host directory or change unrelated active sessions.
- [ ] 4.6 Extend existing Mobile tests for Headless capability, edit acknowledgement, and externally-originated `project.list`; modify production code only if test demonstrates mismatch.

## 5. Compatibility and Release Gate

**What to build:** Upgrade remains safe across versions, hosts, reconnects, and supported operating systems, with original symptom locked by runnable checks.

**Blocked by:** 3. Legacy Desktop Projection Migration; 4. Bidirectional Live Edit and Remove.

**Status:** blocked

- [ ] 5.1 Verify new clients fall back to current one-shot/local behavior when host does not advertise project subscriptions.
- [ ] 5.2 Verify old clients can still call one-shot `project.list/add/remove` against upgraded Headless and ignore additive messages safely.
- [ ] 5.3 Verify equal host IDs from two Headless devices remain isolated by device ID.
- [ ] 5.4 Verify disconnect keeps Desktop cache, reconnect resets version epoch, and Headless restart restores catalog.
- [ ] 5.5 Run focused Agent, Desktop runtime, protocol, and Mobile test suites; record exact commands and results.
- [ ] 5.6 Run manual matrix: Desktop add, Mobile add, both edit/remove directions, reconnect, Headless restart, and two simultaneously connected controllers.
- [ ] 5.7 Run manual host-path matrix on Unix and Windows where available; confirm host-native path deduplication and stable IDs.
- [ ] 5.8 Confirm no `[DEBUG-...]` instrumentation or throwaway harness remains.
- [ ] 5.9 Update change metadata only after every automated gate passes; archive and update truth specs only after release under Stage 3.

