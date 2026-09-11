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
