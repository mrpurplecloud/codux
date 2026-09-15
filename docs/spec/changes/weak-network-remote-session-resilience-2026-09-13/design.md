## Context

Codux controller 与 desktop/headless host 通过 iroh QUIC 共享终端 agent session。连接包含候选发现、direct/relay 拨号、QUIC 建链、control/terminal stream 握手、host 响应和 terminal 状态恢复几个阶段。现有实现已经有：

- 共享 endpoint、relay/direct candidate、LAN mDNS、path watcher；
- control 与 terminal 独立 stream；
- `ReconnectPolicy`、connection generation、RTT 和 direct/relay 日志；
- terminal `outputSeq`、ACK、baseline、gap 检测和 generation；
- Android `BackgroundKeepAlive`。

缺口不在基础传输，而在断线后的语义：哪一个 session 继续运行、从哪个 output cursor 恢复、输入是否已经执行、何时可以把连接标成可用，以及前台恢复是否必须刷新。

## Design Goals

1. 短暂丢包、路径变化和短断网不重建 agent session。
2. 恢复优先使用 delta，只有无法覆盖 cursor 时才使用 baseline。
3. 输入不因 ACK 丢失而重复执行；无法确认时显式呈现不确定状态。
4. 输出拥塞不阻塞 control、输入、resize 和 Ctrl-C。
5. 回到前台先可见，再同步；Android 尽量保持连接，iOS 遵守系统挂起边界。
6. 所有恢复结果由一个协调器按当前 generation 发布。

## Canonical State

### Connection state

```text
stopped
  -> connecting
  -> transport_open
  -> protocol_ready
  -> host_responsive
  -> restoring_session
  -> ready

connecting/transport_open/protocol_ready/host_responsive/restoring_session
  -> connecting        (可恢复失败，generation 递增)
  -> parked             (达到重试上限，保留 session intent)
  -> stopped            (用户关闭或 session 被取消)

parked
  -> connecting         (前台、网络变化、用户重试或低频 probe)
```

`transport_open` 只表示 QUIC 已建立；`ready` 只在 host 响应且 terminal 恢复结果已发布后成立。连接 generation 是迟到结果的失效标记，不是第二份业务状态。session ID 始终代表 host 上的 agent 生命周期。

### Recovery flow

```text
network/app lifecycle event
  -> recovery coordinator
  -> keep current iroh path when healthy
  -> refresh candidates when route changes
  -> direct/relay dial
  -> protocol handshake
  -> host.info and session identity check
  -> send last contiguous output cursor
  -> delta replay OR baseline
  -> apply held live output in sequence
  -> publish ready
```

任何旧 generation 的 dial、host.info、baseline 或 output 完成事件都只能被丢弃，不能清理新 generation 的 retry，也不能覆盖当前 screen。

## Terminal Resume Contract

### Controller sends

- session ID；
- 当前 connection generation；
- 最后一个连续应用的 `outputSeq`；
- 当前 terminal viewport/baseline generation；
- 是否允许 delta，不允许时直接请求 baseline。

### Host chooses

- cursor 在 replay ring buffer 内：按序发送缺失 delta，随后发送当前 live output；
- cursor 已过期、session generation 不匹配或 replay 不可用：发送一次 baseline，并附带覆盖到的 output watermark；
- session 不存在：返回明确的 session-not-found，不能伪装成空 screen。

### Controller applies

1. 连续 output 直接应用并推进 contiguous cursor。
2. 发现 gap 时暂停发布后续 screen，保留有界 live output，启动一次 delta/baseline 恢复。
3. baseline 先替换 screen，再按 watermark 过滤已覆盖数据，最后按序重放保留的 live suffix。
4. 只有 baseline 或 delta 已应用、ACK 已发布、screen 已更新时才报告 `session restored`。
5. replay 超出有界缓存时允许丢弃中间历史，但必须以 baseline 恢复最终 screen；不能静默丢失造成错误终端状态。

## Input Contract

- controller 为每个 session 生成单调递增 `inputId`，断线重试沿用原 ID；
- host 按 `sessionId + inputId` 保留有界去重记录；首次输入入队，重复输入只返回已有 ACK；
- ACK 丢失时 controller 保留 pending 并重发，连接恢复后仍可安全确认；
- pending 队列有数量和时间上限；超限后标记 `unconfirmed`，不自动无限重放；
- host session 重启会清空内存去重记录。跨 host 崩溃的 exactly-once 不在保证范围内，副作用命令需要用户确认；
- 输入、resize、Ctrl-C 使用高优先级队列，不能等待大段 output 完成。

## Output Quality of Service

Control、输入/resize、terminal output 继续分离。调度器按优先级和公平性发送：

1. control health、session handshake、输入和 resize；
2. terminal output；
3. baseline/replay bulk transfer。

每个队列有界。output backlog 达到上限时暂停低价值中间帧并请求最新 baseline；屏幕正确性由 sequence/generation/baseline 保证，不能靠静默丢包维持“看似在线”。

## Path and Retry Policy

- iroh 连接仍是唯一数据面；健康路径优先保持，不因网络接口变化立即重建；
- route change 时刷新 candidate，direct 和 relay 健康度分别记录；
- 现有拨号时序先保持，按阶段指标决定是否缩短黑洞 direct 的等待；
- reconnect attempt 只有一个 owner，使用现有指数退避和 jitter；
- 达到当前上限后进入 parked probe。probe 必须可被 session 关闭、用户取消、后台策略和电量策略终止；
- `transportPing/Pong` 只表示链路 RTT。恢复状态还需 protocol、host 和 session ACK。

## Foreground and Background

- 前台恢复立即复用缓存 screen 和 session metadata；
- 无 gap、generation 未变化、缓存未失效时，只做轻量 host/transport 确认；
- 有 gap 或 host 要求时，进入 baseline/delta 恢复；
- Android 前台服务获准时保持 endpoint 和 reconnect coordinator；服务被拒或进程被杀时，回前台走同一 cache-first 恢复；
- iOS 进入后台可能被系统挂起。设计保证恢复路径幂等和页面无需 reload，不承诺后台 QUIC 常驻。

## Observability

每次 connection generation 关联以下信息：candidate 来源、direct/relay、拨号阶段、失败类别、RTT、抖动/丢包样本、attempt、session ID、resume cursor、baseline/delta 选择和最终恢复结果。用户界面只展示可行动的阶段和下一步；原始错误保留在受保护日志中。

## Compatibility and Rollout

第一阶段只改本地前台恢复和诊断，不改 wire。第二阶段增加可选 resume 元数据；旧 peer 忽略时继续走 baseline。第三阶段增加 host 输入去重和 output replay；每个字段允许缺失并回退到当前行为。实现涉及 mobile、desktop/headless host 和 shared terminal core 时，单独更新版本兼容性记录，不同步无关产品版本。

## Failure Boundaries

- host、relay 和所有 direct candidate 同时不可达时，只能保持 parked/恢复状态；
- host session 已被外部杀掉时，不能伪造 session continuity；
- host 崩溃后无法凭内存 ACK 推断副作用命令是否执行；
- RTT 很高时只能改善队列和感知延迟，不能突破网络物理下限；
- iOS 系统挂起期间不能依赖后台 timer 或 UDP 保活。
