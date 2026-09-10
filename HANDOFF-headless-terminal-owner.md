# Headless terminal owner 修复 handoff

> 已归档：本文记录实现前调查及失败候选，不再表示当前状态。权威记录为 `.ai/tasks/headless-terminal-owner.txt`；任务现已完成，代码已进入 beta10 本地产物，当前获授权按任务提交并 push 到 `lan`。

日期：2026-09-09

## 归档时状态

- 分支：`main`
- 固定基线与当前 HEAD：`7fed2baded84f5067544148067688a997c682d99`
- owner 修复候选代码已全部撤回。
- 已确认以下文件相对固定基线无 diff：
  - `crates/codux-protocol/src/lib.rs`
  - `apps/desktop/runtime/src/remote/controller.rs`
  - `apps/desktop/runtime/src/runtime_terminal.rs`
  - `apps/desktop/src/terminal/pane.rs`
  - `apps/desktop/src/terminal/view.rs`
  - `apps/desktop/src/terminal/tests/pty_binding.rs`
- 撤回后未跑测试：没有任务代码需要验证。
- 工作区仍有用户原有改动，后续不得覆盖或清理：
  - `AGENTS.md`
  - `.ai/current.md`
  - `.ai/diagnosis-gate.json`
  - `.ai/fix-gate.json`
  - `.ai/protocol.md`
  - `.scratch/research/ai-bugfix-and-design-doctrine.md`

## 原始需求与验收结果

同一 headless terminal session 同时只能有一个交互 owner。

- mobile 与 desktop 同时打开时，非 owner 只显示已有接力提示。
- 非 owner 不显示 terminal 内容，不发送 input 或 resize。
- 用户点击 `Take over` 后，当前设备发送 Force。
- host 确认当前设备成为 owner 后，才恢复内容、input、resize。
- 普通显示发送 Auto；隐藏发送 Release。
- visible owner 每 8 秒发送 Renew。
- ownership event 与 live output 经过同一 per-session 串行通道。
- 每个 `controller.*` 失败都进入显式 Error 状态，发布 UI error，隐藏内容。
- reconnect、旧 callback、相同 generation、尺寸变化最终收敛到 host 权威状态。

## 已确认事实

### Mobile 是可复用契约

相关文件：

- `apps/mobile/lib/screens/home/state/terminal.dart`
- `apps/mobile/lib/screens/home/home_controller.dart`
- `apps/mobile/lib/widgets/components/remote_terminal_pane.dart`
- `apps/mobile/lib/services/terminal_viewport_controller.dart`

已验证行为：

- 本机 owner identity 为 `remote:<deviceId>`。
- `terminal.viewport.state` 是权威结果。
- owner 不匹配时，mobile 不渲染 live grid，不发送 input/resize。
- Auto、Force、Renew、Release 已存在；Renew 周期为 8 秒。
- generation 只拒绝更小值；相等值必须接受，允许从 Awaiting/Error 收敛。
- Force 不能乐观恢复交互；必须等待 host state 确认。

### Desktop 已有接力 UI

相关文件：

- `apps/desktop/src/terminal/render.rs`
- `apps/desktop/src/terminal/element.rs`
- `apps/desktop/src/terminal/view.rs`
- `apps/desktop/src/terminal/pane.rs`

已有能力：

- `render.rs` 已有 handoff placeholder。
- 已有 `Take over` 按钮，调用 `restore_local_viewport()`。
- `local_viewport_owns()` 已决定是否显示 terminal 内容。
- `TerminalElement::prepaint()` 已根据 owner 决定 resize。
- workspace 已调用 `TerminalView::set_render_visible()`。

因此无需新增 UI、router、cache、配置项或通用状态框架。

### 根因位于 desktop hosted owner 接线

当前基线缺口：

- `RuntimeTerminalController` 未暴露 owner identity 和 Auto/Force/Renew/Release。
- `RemoteController` 只按 session 转发 output。
- `terminal.viewport.state` 进入普通 event queue，没有进入 terminal binding。
- output 与 ownership event 不在同一 per-session 串行 callback。
- hosted `TerminalSessionBinding::local_viewport_owns()` 默认返回 `true`。
- hosted `write()`、`resize()` 无条件发送。
- hosted `restore_local_viewport()` 不发送 Force。
- 已有 visibility hook 未连接 hosted Auto/Release/Renew。
- reconnect 没有 owner epoch；旧 callback 可能继续生效。
- desktop remote owner 必须使用 `remote:<RemoteController.device_id>`，不能使用 `"local"` 或固定 `"desktop"`。

## 推荐实现范围

先用一条真实生产路径测试证明接线面，再写生产代码。预计仅触碰以下 owner layer 文件：

1. `crates/codux-protocol/src/lib.rs`
   - 让全部 `terminal.viewport.*` 消息与 output 进入同一 terminal stream。
2. `apps/desktop/runtime/src/remote/controller.rs`
   - 每个 session 只保留一个统一 terminal event forwarder。
   - output 与 viewport state 通过同一 map、同一 callback 顺序转发。
   - 暴露 `remote:<device_id>` 和 Auto/Force/Renew/Release。
3. `apps/desktop/runtime/src/runtime_terminal.rs`
   - 仅通过现有 binding 暴露上述能力。
   - WSL 保持 unmanaged，行为不变。
4. `apps/desktop/src/terminal/pane.rs`
   - 保存最小、host-authoritative hosted owner 状态。
   - Awaiting、other owner、Error 都使 `local_viewport_owns() == false`。
   - owner 确认前拒绝 input/resize。
   - Force 后继续隐藏，直到 state 确认。
   - 更小 generation 和旧 connection callback 无副作用。
5. `apps/desktop/src/terminal/view.rs`
   - 复用 `set_render_visible()`：show 发 Auto，hide 发 Release，visible owner 每 8 秒 Renew。
   - input/paste 在非 owner 时直接拒绝，不能隐式 Force。

若实现要求触碰 mobile、workspace、host、router，或新增 UI、cache、配置、重试、通用抽象，立即停止：根因或 owner layer 尚未锁定。

## 最小纵向实现顺序

每步先写穿过真实生产入口的失败测试，再写最小代码；每步通过后才进入下一步。

1. 协议路由：证明 viewport message 与 output 共用 terminal stream。
2. Controller 串行边界：证明单 forwarder 收到 `output/state/output` 时保序。
3. Hosted 读取门禁：证明 attach 初始 Awaiting，内容隐藏且 input/resize 不发送。
4. 所有权确认：证明 other owner 只显示接力；Force 后仍 Awaiting；local owner state 到达后才恢复。
5. 生命周期：证明 show Auto、hide Release、visible owner Renew。
6. 失败语义：逐一证明 Auto、Force、Renew、Release、input、resize 返回 false 后进入 Error、发布 UI error、隐藏内容。
7. 时序与重连：证明更小 generation 丢弃、相等 generation 可恢复；旧 epoch callback/output 丢弃；visible reconnect 才 Auto；list/open 失败显式 Error。

完成每个纵向切片后搜索新增 symbol 的生产 caller。任何新增方法没有生产 caller，立即停止并判定方案漏接。

## 必测清单

- viewport 消息与 output 同 terminal stream。
- 单 controller forwarder 保持 `output/state/output` 顺序。
- managed attach 初始 Awaiting：只显示接力，不发送 input/resize。
- state 为其他 owner：只显示接力。
- Auto/Force 后仍 Awaiting。
- state 确认本机 owner 后才恢复 render/input/resize。
- hide 发送 Release；show 发送 Auto；visible owner 每 8 秒 Renew。
- Auto、Force、Renew、Release、input、resize 每个失败均断言：Error 状态、`TerminalUiEvent::Error`、内容隐藏。
- 更小 generation 丢弃；相等 generation 可从 Error/Awaiting 恢复。
- reconnect 建立新 epoch；旧 callback/output 丢弃；visible 才 Auto；hidden 不 Auto；list/open 失败显式 Error。
- 非 owner output 可继续镜像内部缓存，但 UI 不渲染。
- 双设备 headless 实测：mobile owner 时 desktop 只显示接力；desktop 接管成功后 mobile 只显示接力；两端不能同时 input/resize。

## 已失败方案，不得续用

### 候选 `686d58cf…`

只加入 protocol/controller 骨架，未接真实 production path。

- owner API 没有生产调用。
- desktop 仍显示内容并发送 input/resize。
- output 与 owner event 使用两个 forwarder map。
- 单元测试无法证明原 bug 已修复。

### 候选 `86e35863…`

加入 hosted 状态代码，但真实调用路径仍漏接。

- `write/resize/local_viewport_owns` 未读取新增状态。
- visibility 与 Renew 只有测试调用。
- hosted Force 未接。
- owner identity 错写为 `"local"`。
- reconnect 仍注册无 binding forwarder。
- controller callback 后又拆成 output/event 两个队列，破坏串行边界。
- `cargo test -p codux-runtime` 曾有 515 passed、4 ignored；该结果只证明内部测试，不能证明真实 desktop 路径。

### 最后中止候选

尝试一次性覆盖完整状态机，未编译前已增长到约 1,311 additions、126 deletions，其中 `pane.rs` 约 682 行、测试约 448 行。范围扩散，已全部撤回。不得从该候选继续。

## 经验与防浪费规则

- 第一判断应验证“desktop hosted 是否只需对齐 mobile 契约”，不能先发明新机制。
- 调查顺序：mobile 权威行为、desktop 已有 UI、host 协议、真实 hosted production path、最后才设计缺口。
- 先快速证真/证伪根因。最早测试必须穿过 render/input/resize 的真实入口。
- 不先写完整状态机。按协议、串行 callback、门禁、生命周期、失败、重连做小纵向切片。
- 绿色 unit tests 不代表真实路径可达。必须检查每个新 symbol 的生产 caller。
- owner 是 host 权威状态；epoch、generation、timer、future 只是协调元数据，不能成为第二权威源。
- ownership event 与 live output 必须从 controller 到 binding 全程保持同一 FIFO；进入 UI 前拆成两个队列仍会重排。
- 失败发布是状态转换的一部分。所有 `return`、stale-result、callback、controller false 分支都需审计。
- reviewer 首次发现本次设计遗漏时，候选判废并回固定基线；不能叠补丁。
- 不把既有缺陷、本次暴露缺陷、本次引入缺陷混为一类。只修当前 owner 根因。
- 固定 diff 后只做两路一次性复审：需求/真实可达路径；并发/失败/回调时序。
- 相关专项测试通过后，仅跑受影响 runtime 主路径和双设备 smoke。不要用无证据的全仓扫描或全套测试拖延 release。

## 下次开始前检查

- 确认 HEAD 仍为预期基线，记录 `git status --short --untracked-files=all`。
- 明确保护用户现有工作区改动。
- 重新读取 mobile owner 契约和 desktop hosted 真实调用链，防止代码漂移。
- 写下预定 owner layer 与文件范围；越界即停。
- 先建立最小失败复现，再开始第一个纵向切片。
- 最终交付前设备实测；本地测试不得冒充双设备或 release 证据。
