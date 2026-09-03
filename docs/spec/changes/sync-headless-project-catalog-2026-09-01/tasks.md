---
created_at: 2026-09-02T06:22:45Z
updated_at: 2026-09-02T14:10:00Z
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

Corrected under ticket 5: 2.5 also let an add fall back to a local write when the
host could not be reached, which contradicts "Disconnected cache is read-only" in
the delta spec. A host this desktop has already synced a catalog with now refuses
the add instead — see the ticket 5 notes.

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

**Status:** done — commit `3fe943ad`

- [x] 3.1 Add failing fixture with legacy remote record lacking `hostProjectId` and an empty Headless catalog.
- [x] 3.2 Add per-device migration version and run migration only after valid upgraded-Headless baseline.
- [x] 3.3 Register legacy paths through idempotent Headless add; existing Headless path wins name and host ID.
- [x] 3.4 Attach host IDs while preserving Desktop IDs, badges, environment settings, ordering, and selection when still valid.
- [x] 3.5 Persist marker only after all registrations and final reconciliation succeed; partial failure must retry without pruning legacy records.
- [x] 3.6 Repeat migration and reconnect tests; assert one entry per normalized path and no resurrection after marker plus Headless removal.
- [x] 3.7 Test two controllers migrating disjoint legacy records; final Headless catalog contains union once.

Registration runs before the first reconciliation of a connection rather than
after its baseline: a reconciliation that ran first would build a second
projection for a catalog entry the legacy record is about to be attached to, and
the record's Desktop ID — the one layouts and selection point at — would be the
one left behind.

One case the ticket text does not name. Headless deduplicates by its own
normalized path, so two legacy records (`/srv/alpha` and `/srv/alpha/`) can come
back holding one host ID. Only one record may project a catalog entry, so the
later one is dropped in favour of the record already projecting it; keeping both
would show one host path twice and translate one host ID into two Desktop IDs.

## 4. Bidirectional Live Edit and Remove

**What to build:** Desktop and Mobile can edit or remove projects from either controller, and every connected controller reconciles live while keeping its own selection.

**Blocked by:** 2. Desktop Authoritative Add and ID Bridge.

**Status:** done — commit `8d3ec643`

- [x] 4.1 Add Desktop remote edit/remove paths that wait for Headless commit and consume authoritative snapshot.
- [x] 4.2 Add two-direction integration cases: Desktop mutation updates Mobile-style client; Mobile-style mutation updates Desktop projection.
- [x] 4.3 Verify same-path retry stays idempotent, edit keeps host ID, and edit path collision returns error without broadcast.
- [x] 4.4 Verify stale and duplicate snapshot versions cannot roll Desktop or Mobile backward.
- [x] 4.5 Verify selection changes remain per-client and project removal does not delete host directory or change unrelated active sessions.
- [x] 4.6 Extend existing Mobile tests for Headless capability, edit acknowledgement, and externally-originated `project.list`; modify production code only if test demonstrates mismatch.

One production change 4.6 forced, and it deviates from the design text. Design
decision "Full versioned snapshots over existing resource subscription" says new
clients subscribe with `baseline: true`, and Headless only answered a baseline
subscribe. Mobile subscribes without `baseline` and waits for the `project.list`
the Desktop host has always answered a `projects` subscribe with, so against the
upgraded Headless it would have shown an empty catalog until some other
controller mutated it. Headless now answers every `projects` subscribe with the
current catalog, which is also what keeps already-shipped Mobile builds working
(5.2). The mismatch is pinned by an Agent test, not a Mobile one: only the host
side can demonstrate it.

Two things the ticket text does not name. An edit that moves a record to another
runtime target is not a catalog edit. It creates an independent project through
the new target's normal add flow while preserving the original record: a
projection remains a projection with its host project id, and a local record
remains local. The host keeps its own catalog membership, which is not this
controller's to remove. And a mutation on a projection whose host is unreachable
now fails instead of
falling back to a local write: the record exists only because the host listed
it, so a local edit or removal would diverge silently and be undone by the next
snapshot.

4.5's "unrelated active sessions" is covered where that scoping lives rather
than duplicated here: `remove_device_and_scope_clear_subscriptions`
(codux-runtime-core) proves one project's removal leaves another project's
subscribers, and `project_close_cleans_workspace_cache_for_root_and_worktrees`
covers the controller-side cleanup, which removal shares unchanged.

## 5. Compatibility and Release Gate

**What to build:** Upgrade remains safe across versions, hosts, reconnects, and supported operating systems, with original symptom locked by runnable checks.

**Blocked by:** 3. Legacy Desktop Projection Migration; 4. Bidirectional Live Edit and Remove.

**Status:** automated gates done — commit `8a43eccf`; manual matrix 5.6 and 5.7 outstanding

- [x] 5.1 Verify new clients fall back to current one-shot/local behavior when host does not advertise project subscriptions.
- [x] 5.2 Verify old clients can still call one-shot `project.list/add/remove` against upgraded Headless and ignore additive messages safely.
- [x] 5.3 Verify equal host IDs from two Headless devices remain isolated by device ID.
- [x] 5.4 Verify disconnect keeps Desktop cache, reconnect resets version epoch, and Headless restart restores catalog.
- [x] 5.5 Run focused Agent, Desktop runtime, protocol, and Mobile test suites; record exact commands and results.
- [ ] 5.6 Run manual matrix: Desktop add, Mobile add, both edit/remove directions, reconnect, Headless restart, and two simultaneously connected controllers.
- [ ] 5.7 Run manual host-path matrix on Unix and Windows where available; confirm host-native path deduplication and stable IDs.
- [x] 5.8 Confirm no `[DEBUG-...]` instrumentation or throwaway harness remains.
- [x] 5.9 Update change metadata only after every automated gate passes; archive and update truth specs only after release under Stage 3.

Where each automated gate lives:

- 5.1 `a_host_without_the_capability_keeps_local_project_creation` (already present) plus
  `a_host_without_the_capability_leaves_existing_records_alone`: a host that does
  not advertise `projects` is never subscribed to, never migrated to, and never
  prunes what this desktop already holds for it.
- 5.2 `a_client_that_never_subscribes_still_uses_the_one_shot_project_api`
  (codux-agent): add, list, and remove answer a client that never subscribes, and
  the messages this change added to those flows are additive.
- 5.3 `two_hosts_minting_the_same_project_id_stay_separate_projections`: two hosts
  that both minted `p-host-1` produce two projections, and one host's removal
  leaves the other's alone.
- 5.4 `a_disconnected_desktop_still_shows_its_cached_projections` and
  `a_restarted_host_reconciles_from_a_reset_version_epoch`; the host half is
  `a_restarted_store_serves_the_persisted_catalog_from_a_fresh_version_epoch`
  (codux-agent, ticket 1).

5.5, run from the repository root on macOS 24.6.0 (darwin, arm64):

- `cargo test -p codux-runtime` — 425 passed, 4 ignored.
- `cargo test -p codux-agent` — 39 passed.
- `cargo test -p codux-protocol` — 15 passed.
- `cargo test -p codux-runtime-core` — 47 passed.
- `cargo test -p codux` — 247 passed.
- `flutter test` in `apps/mobile` — 306 passed.
- `cargo clippy -p codux-runtime -p codux-agent --all-targets` — no warning in the
  changed files.
- `cargo fmt --all` — clean.

5.8: no `[DEBUG-` marker and no throwaway harness in `apps/` or `crates/`; the
fake host in the desktop tests is the same seam tickets 2 to 4 already used.

A review of tickets 1 to 4 found six defects the gates above did not cover. Each
was fixed by removing the thing that could disagree rather than by adding a
guard around it:

- The host minted a snapshot's resource version after releasing the store lock,
  so a delayed fan-out could publish an old catalog under a new version and
  clients would lose an entry that is on disk. The version is now minted with
  the catalog inside the store's mutation lock and travels on `ProjectCommit`;
  the separate resource counter for `projects` is gone.
- Desktop recorded a snapshot's version before reconciling it, and outside the
  reconciliation. A failed apply consumed the version for good, and two applies
  could interleave. Check, write, and record now happen under one hold of the
  sync-state lock, and parsing happens before it.
- A remote add whose host was unreachable fell back to a local record that no
  later connection could upload and no snapshot would ever mention. Ownership is
  now one question with three answers (`ProjectCatalogOwner`): live, unreachable,
  or not a catalog. Only the last keeps the controller-local flow.
- A reconnected controller carried an empty project-id bridge until its baseline
  landed, so an early terminal, Git, AI, or worktree request could cross it with
  a Desktop ID. Every live connection is now published through one path that
  loads the bridge from the persisted projections first.
- An edit or removal raised on another controller reconciled the store and
  stopped there, leaving watchers, project activity, workspace layouts, and
  caches behind. Reconciliation returns what changed, and both it and an
  explicit close run the same `finish_project_removal`.
- Path-keyed remote routing had its own `.find()` and could pick another host's
  project when two hosts serve the same path. It now goes through the store's
  one path resolver, where the selected project wins and an ambiguous path is an
  error.

5.6 and 5.7 are not run: they need a Desktop build, a phone, a running Headless
host, and a Windows host, none of which exist in this environment. Their
automated counterparts — Desktop-and-Mobile-direction mutations, reconnect,
restart, two connected controllers, and path deduplication — pass, but a passing
in-process test is not device evidence, so both stay unchecked for whoever has
the hardware. The release under Stage 3 still owes them.
