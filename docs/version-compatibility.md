# Product Version Compatibility

Codux Desktop, Codux Mobile, and Codux headless agent use independent product
versions:

- Desktop: `apps/desktop/Cargo.toml`
- Mobile: `apps/mobile/pubspec.yaml`
- Headless agent: `apps/agent/Cargo.toml`

Bump only a product whose shipped artifact changes. Desktop release preparation
must not rewrite mobile or headless versions. Internal Rust package versions,
including `codux-runtime` and `codux-wrapper-helper`, are not cross-product
compatibility keys.

Remote compatibility is capability-based. Product versions record tested
combinations; they do not replace the capability contract exposed by
`codux_protocol::host_capabilities()`.

## Compatibility record

Update this table when a cross-product contract changes or a combination is
tested. Use `verified`, `known issue`, or `pending`; never infer untested ranges.

| Desktop | Mobile | Headless agent | Contract | Status | Evidence |
| --- | --- | --- | --- | --- | --- |
| `2.0.3-beta.10` | `2.0.3-beta.10+58` | `2.0.3-beta.9` | `terminalViewport` ownership and `terminalBuffer` baseline during desktop takeover | known issue | Agent TUI accepts desktop input, but desktop screen remains stale after takeover; ordinary shell remains usable. |
| `2.0.3-beta.11` | `2.0.3-beta.10+58` | `2.0.3-beta.9` | Same contract, with desktop baseline reconciliation fix | pending | Local desktop/mobile/headless device test required before marking verified. |
| `2.0.3-beta.12` | `2.0.3-beta.10+58` | `2.0.3-beta.11` | Baseline reconciliation + last-terminal close propagation + viewport owner label | pending | Local build packaged and deployed; device-level verification still required. Host must run beta.11 for `ownerLabel` to reach the desktop takeover placeholder. |
| `2.0.3-beta.13` | `2.0.3-beta.10+59` | `2.0.3-beta.12` | Pending/Unknown takeover state and explicit unknown-device label fallback | pending | Desktop tests and local build pass; device-level short-window and retry verification still required. |
| `2.0.3-beta.14` | `2.0.3-beta.11+60` | `2.0.3-beta.13` | Same wire contract; controller cold connections race available paths with relay startup | pending | Shared transport regression and LAN probe results are recorded in `.ai/tasks/relay-cold-connect.txt`; the installed mobile/desktop/headless product combination still needs device verification. |
| `2.0.3-beta.15` | `2.0.3-beta.12+61` | `2.0.3-beta.14` | Same wire contract; retries retain unfinished handshakes within existing total budgets, and shutdown closes only the owned connection | pending | Final Android transport passed 1/2/4-second RTT direct-path and 4-second relay-only checks; see `android-high-latency-verification.md`. Installed full-product combinations remain unverified. |
| `2.0.3-beta.15` | `2.0.3-beta.13+62` | `2.0.3-beta.14` | Same wire contract; mobile reconciles baseline request geometry after first layout and delayed takeover baselines | pending | Mobile regression evidence is recorded in `.ai/tasks/mobile-takeover-input-row.txt`; installed full-product combinations still require device verification. |
