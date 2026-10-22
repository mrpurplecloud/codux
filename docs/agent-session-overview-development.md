# Agent Session Activity and Overview: Developer Handoff

Updated: 2026-10-22

This guide records the current implementation contract and code map for future
changes to Agent activity, project badges, remote snapshots, and the session
overview. The detailed investigation and revision history live in
[project-badge-investigation-2026-10-20.md](project-badge-investigation-2026-10-20.md).
The active task record is
[.ai/tasks/agent-session-badges.txt](../.ai/tasks/agent-session-badges.txt).

## Current Contract

Codux reports process and interaction state. It does not infer business-task
completion from a finished answer or an idle process.

| State | Meaning | Project badge |
| --- | --- | --- |
| NeedsInput | Runtime has explicit evidence that a user action is pending. | Orange solid square |
| Working | Agent is actively processing a turn. | Blue solid circle |
| Idle | Agent process lives, but no turn or confirmed input action is active. | Neutral hollow circle |
| Unknown | Current source cannot confirm activity, including an unavailable remote host. | Neutral question mark |
| Absent | No managed Agent session exists. | Hidden |

Project aggregation order is `NeedsInput > Unknown > Working > Idle > Absent`.
The same NeedsInput color and shape cover questions, choices, and approvals.
Color is not the only visual cue; color e-ink device acceptance remains open.

NeedsInput detection uses runtime evidence and supported tool events. A natural
language question in an ordinary Agent response may remain Idle. Idle does not
mean the task is done or that the user must respond.

## Recent Output

The overview displays a bounded snippet from the rendered terminal screen:
up to three non-empty lines and 240 Unicode characters. The snippet can contain
a prompt, tool log, or status text. It is not guaranteed to be the assistant’s
last sentence and does not affect activity classification.

## Ownership and Code Map

| Responsibility | Owner |
| --- | --- |
| Shared Agent activity facts and process identity | `crates/codux-runtime-live/src/ai_runtime/store/activity.rs`; `supervisor.rs` |
| Activity record and snapshot wire format | `crates/codux-protocol/src/agent_activity.rs` |
| Host publication and remote subscription | `apps/desktop/runtime/src/remote/host/agent_activity.rs`; `apps/desktop/runtime/src/remote/controller.rs`; `controller_manager.rs` |
| Desktop activity projection and project badges | `apps/desktop/src/app/agent_lifecycle.rs`; `agent_display.rs`; `project_column.rs`; `task_column.rs` |
| Overview window, rows, and session navigation | `apps/desktop/src/app/session_overview.rs`; `status_bar.rs` |
| Project directory/layout publication before navigation | `apps/desktop/src/app/project_actions/switch_load.rs` |

The runtime store owns activity facts. Host snapshots publish the authoritative
set. Desktop holds host-scoped mirrors and projects the same records into
badges, session rows, and the overview.

## Invariants for Follow-up Work

- Scope identity by host, terminal, and Agent instance. A stale instance or
  connection must not replace current activity.
- A disconnected host keeps only previously observed sessions as Unknown.
  Clear stale recent output and disable navigation until the host reconnects.
- Optional `tool` and `recentOutput` fields preserve compatibility with older
  records. Missing text means no recent output, not no session.
- Read terminal screen text outside the runtime store lock. Reuse existing
  sampling and snapshot publication; do not add a second poller or discovery
  path.
- Overview navigation waits for the current project directory and terminal
  layout to publish. Then use existing worktree, mount, and pane identity
  checks. Do not route by terminal ID alone.
- Keep the final-answer status-line experiment withdrawn. Do not add prompt
  injection, per-Agent response parsing, or model classification to distinguish
  finished work from a request for input.
- The desktop pet has separate lifecycle and performance behavior. The
  overview does not reuse its animation, audio, or mouse-through loop.

## Verification and Open Acceptance

Latest targeted desktop checks passed:

- `cargo test -p codux session_overview`: 9 passed.
- `cargo test -p codux terminal_restore`: 4 passed.
- `cargo test -p codux active_terminal`: 2 passed.
- `cargo test -p codux agent_activity`: 3 passed.
- The corresponding Rust formatting check and `git diff --check` passed.

Earlier protocol, shared-store, supervisor, remote activity, and headless
activity results are recorded in
[version-compatibility.md](version-compatibility.md); they were not rerun during
the latest navigation revision.

Source targets Desktop `2.0.3-beta.27` and headless agent `2.0.3-beta.23`.
Mobile remains `2.0.3-beta.18+67`.
Source commit: `16cc092abfe46cea5e0e257e51c6896effcd25bf`.
Tags `desktop-v2.0.3-beta.27` and `headless-v2.0.3-beta.23` point to this
commit and are pushed to the configured LAN remote. No product artifact has
been packaged, installed, or deployed.
Real remote first-open navigation, mouse navigation across projects/worktrees,
and color e-ink readability still need device acceptance. The static color and
grayscale sample is
[agent-session-overview-preview.svg](agent-session-overview-preview.svg); it is
not a GPUI or e-ink screenshot.
