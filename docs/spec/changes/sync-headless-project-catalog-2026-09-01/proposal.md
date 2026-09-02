---
created_at: 2026-09-02T06:22:45Z
updated_at: 2026-09-02T06:22:45Z
---

## Why

A Codux Desktop controller and a mobile controller connected to the same Headless host do not currently see the same project catalog. Desktop creates a remote-target project only in its controller-local `ProjectStore`. The Headless host maintains a separate `AgentProjectStore`, exposed through `project.list/add/remove`, so projects created by one side can remain invisible to the other.

This is an incomplete cross-client implementation, not an intentional product boundary. Codux already presents Desktop and Mobile as interchangeable controllers for the same host, and the standard remote protocol already defines project mutation and project-resource subscription messages. The missing pieces are Headless protocol parity, Headless fan-out, and Desktop reconciliation.

## What Changes

- Make each Headless host's persistent project catalog the authority for project membership, name, path, and host project ID.
- Make Desktop remote project add, edit, and remove operations mutate that Headless catalog before changing the controller-local view.
- Advertise and implement the existing `projects` resource subscription on Headless. Send a baseline on subscribe and a versioned full `project.list` snapshot to every subscribed client after each committed mutation.
- Add `project.edit` support to Headless and align mutation acknowledgement with the existing Desktop-host protocol.
- Keep a Desktop projection of each Headless catalog for the current UI and offline display. Store an explicit `hostProjectId` so controller-local IDs never leak into host-scoped terminal, Git, AI, or worktree requests.
- Migrate legacy Desktop remote records once per Headless host. Register unmatched legacy paths idempotently, then reconcile from the authoritative Headless snapshot.
- Serialize Headless catalog mutations and replace the catalog file atomically so simultaneous clients cannot lose updates or overwrite a malformed last-known-good catalog.
- Keep current project selection, ordering, badges, environment settings, pane layout, and window state local to each client.
- Reuse existing Mobile project CRUD, subscription, and list-reconciliation code. Add regression coverage; no Mobile production-code change is expected unless tests expose a contract mismatch.

## Impact

- Affected capability: `headless-project-catalog` (new)
- Primary implementation areas:
  - Headless project persistence and host message dispatch
  - Headless resource subscriptions and multi-client fan-out
  - Desktop remote controller lifecycle and unsolicited project snapshots
  - Desktop project-store projection, ID translation, and legacy migration
  - Mobile protocol regression tests
- Existing shared protocol constants are sufficient: `project.list`, `project.add`, `project.edit`, `project.remove`, `project.updated`, `resource.subscribe`, and resource `projects`.
- Stored Desktop project records gain one optional backward-compatible field, `hostProjectId`, plus a per-host migration marker.
- Estimated effort: 4 to 7 engineering days, dominated by Desktop reconciliation and cross-client integration tests.

## Baseline

- Annotated tag: `pre-headless-project-sharing-20260901`
- Commit: `a958c1ebdeb4e4deefe964d1e40db77b3489c5ef`
- Tag excludes generated, untracked `.codegraph/v2.0.3` data.

