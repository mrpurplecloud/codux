---
created_at: 2026-09-13T00:00:00Z
updated_at: 2026-09-13T00:00:00Z
status: proposed
---

## Problem Statement

Codux 的主场景是多机、跨网共享终端 agent session。iroh 已经提供加密 QUIC、直连、relay、重传和路径切换，但弱网下仍可能出现四类体验断裂：短暂丢包被用户感知为断线，重连后重复拉取完整 terminal baseline，未确认输入在多次重试后被丢弃，移动端回到前台时显示旧状态并等待刷新。

当前连接恢复还存在一个可用性边界：`ReconnectPolicy` 在有限次数后进入 `exhausted`，长时间断网时不再自动探测。现有 `outputSeq`、ACK、baseline 和 generation 已经具备恢复所需的大部分状态，但尚未形成统一的 resume 协议和输入幂等语义。

目标是让 transport 断开只表示链路暂时不可用，让 host 上的 agent session 继续运行；链路恢复后优先恢复同一 session 和增量状态。方案继续使用 iroh 作为唯一数据面，不引入第二套穿透或远程桌面协议。

## Solution

建立一条分阶段、可观测、可恢复的连接路径：

1. 控制端保留缓存 screen 和 session 标识。回到前台或重新连上时，先显示缓存，再确认 transport、protocol、host 和 session 状态。
2. 重连携带最后一个连续收到的 `outputSeq`。host 在有界回放窗口内发送 delta；窗口不足时发送一次 baseline。generation 防止旧连接结果覆盖新连接。
3. 输入继续使用 `inputId`，由 host 在同一 session 内去重。控制端只在收到 ACK 后清理 pending；超过有界保留期后显示未确认状态，不无限重放有副作用的命令。
4. control、输入、resize、Ctrl-C 获得高于大段 terminal output 的发送优先级。输出队列达到上限时合并中间画面或要求 baseline，不能静默丢失 ANSI 数据。
5. `ReconnectPolicy` 继续使用指数退避和单一调度 owner。达到当前上限后进入低频、可取消的 parked probe，直到 session 关闭、用户取消或 host 恢复。
6. 连接状态区分 `QUIC open`、`protocol ready`、`host responsive`、`session restored`，并记录 direct/relay、候选、RTT、失败阶段和 generation。

## User Stories

1. 作为终端用户，我希望短暂丢包不会让正在运行的 agent session 被重建，以便继续当前工作。
2. 作为终端用户，我希望 Wi-Fi、蜂窝网络或 VPN 切换后自动恢复，以便不重新打开项目。
3. 作为终端用户，我希望回到前台时立即看到最近 screen，以便不把等待同步误认为页面失效。
4. 作为终端用户，我希望没有 output gap 时不重新下载完整 baseline，以便恢复过程更快且更省流量。
5. 作为终端用户，我希望断线期间 host 上的 agent 继续运行，以便恢复后看到同一个 session。
6. 作为终端用户，我希望输入 ACK 丢失时不会重复执行同一个输入，以便 Enter、Ctrl-C 和命令行操作安全。
7. 作为终端用户，我希望大量日志输出不会堵住键盘输入和窗口大小变化，以便弱网下仍能控制 agent。
8. 作为终端用户，我希望 relay 不可用但 direct 可用时仍能连接，以便单一路径故障不造成整体不可用。
9. 作为终端用户，我希望 relay 和 direct 都不可用时看到明确的恢复状态，以便知道是网络问题而不是 session 丢失。
10. 作为维护者，我希望知道失败发生在候选、拨号、QUIC、协议还是 session 恢复阶段，以便根据数据调优而不是猜测。
11. 作为维护者，我希望旧版本 controller、host 仍能连接，以便恢复功能可以分批发布。
12. 作为移动端用户，我希望 Android 后台保活时回前台无需 transport reconnect，以便切换应用不打断工作。
13. 作为 iOS 用户，我希望系统挂起后回前台先看到缓存，再自动收敛到 host 状态，以便不需要手动刷新页面。

## Implementation Decisions

- iroh 继续作为唯一数据面。保留现有 direct、relay、LAN mDNS、共享 endpoint 和 path watcher；不移植 Mosh 的 UDP，不引入 RustDesk、VNC 或视频流协议。
- 恢复的语义 owner 是连接/session 恢复协调器。transport、协议、UI 回调只能报告观察结果，不能各自推进 attempt、清理 retry 或发布恢复成功。
- session identity 与 transport connection identity 分离。连接 generation 用于丢弃迟到结果，session ID 用于找到同一个 host agent session。
- terminal resume 使用已有 `outputSeq`、ACK、baseline、generation。host 只保留有界 replay ring buffer；无法覆盖 cursor 时明确要求 baseline。
- ACK 分层：output ACK 表示接收连续水位，baseline 完成后才报告 session restored；input ACK 表示 host 已接受该 `inputId`，重复 `inputId` 只返回已有结果，不重复执行。
- 输入 pending 队列有界且有保留期。host 进程重启或 session owner 改变后，不宣称副作用命令 exactly-once；应显示未确认并交给用户决定。
- control、输入、resize、Ctrl-C 与 terminal output 保持独立调度路径。输出拥塞时通过背压或 baseline 重建保持 screen 正确性。
- 前台恢复采用 cache-first。只有检测到 sequence gap、generation 不匹配、缓存过期或 host 明确要求时才拉 baseline。
- relay/direct 拨号窗口和 heartbeat 初始保持现状。先收集按阶段的成功率、P95 恢复时间、RTT、抖动和失败原因，再做自适应调整。
- wire 变更采用可选字段或可忽略消息，旧 peer 继续走 baseline 和现有重连路径。实现时同步更新版本兼容性记录。

### ROI 顺序

| 顺序 | 工作项 | 规模 | 风险 | 预期回报 |
| --- | --- | --- | --- | --- |
| 1 | 前台 cache-first、无 gap 跳过 baseline | 小 | 低 | 立即改善回前台体验，协议不变 |
| 2 | 连接阶段、路径和失败原因指标 | 小到中 | 低 | 为后续调优提供证据，减少误判 |
| 3 | `exhausted` 后 parked probe、session 连续性检查 | 小到中 | 中 | 长断网自动恢复，避免手动重开项目 |
| 4 | resume cursor、delta、baseline 兜底 | 中到中大 | 中 | 减少完整刷新和流量，缩短恢复时间 |
| 5 | `inputId` host 去重和有界重发 | 中 | 中到高 | 降低输入丢失和重复执行风险 |
| 6 | output 背压、合并和输入优先级 | 中 | 中 | 高输出场景下保持可操作性 |
| 7 | 自适应拨号、heartbeat 和候选评分 | 中 | 中到高 | 只在指标证明存在瓶颈时投入 |

规模按一个熟悉代码库的工程师粗估，未包含跨平台真机回归；实现前仍需以弱网基线数据校正。

## Testing Decisions

- 先建立可重复弱网场景：100–800 ms RTT、1–5% 丢包、抖动、短断网、Wi-Fi/蜂窝切换、VPN 地址变化、direct 失败、relay 失败。
- 连接测试验证阶段收敛：transport open 不得单独代表可用；只有 host responsive 和 session restored 成功后才清理恢复状态。
- terminal core 测试验证连续 cursor、delta 回放、baseline 兜底、重复 ACK、gap、旧 generation 迟到结果和恢复期间 live output 排序。
- host/controller 集成测试验证同一 session 不被重建，旧版本 peer 忽略可选恢复字段仍能显示 baseline。
- 输入测试验证 ACK 丢失、输入重发、重复 `inputId`、队列过期、host 重启后的未确认状态；不得只测试传输层成功。
- Android 真机验证前台服务获准、被拒、进程被杀和网络切换；iOS 真机验证后台挂起、回前台 cache-first 和原子收敛。
- 记录 reconnect P50/P95、session restore 成功率、完整 baseline 比例、output gap 率、重复输入率、首屏可用时间、后台耗电和 heartbeat 流量。

## Out of Scope

- 替换 iroh、引入自有 UDP、WireGuard、RustDesk 协议、VNC 或视频串流。
- 在 TUI 上强行使用 Mosh 式预测回显。普通 shell 的有限预测可另行评估，不能成为本方案前置条件。
- 保证 iOS 进程在后台持续运行。
- 在 host 进程崩溃、session owner 更换或外部命令已有副作用后保证 exactly-once。
- 没有测量依据时修改 4/6/8 秒拨号窗口、relay 列表或固定 heartbeat 周期。
- 远程桌面画面、音频、鼠标和高帧率媒体链路。

## Further Notes

- 这是未来预算项目，当前只保存设计，不实现、不提交、不发布。
- 最大收益来自恢复同一 session、避免无必要 baseline、保证输入不重复；路径参数微调应排在这些工作之后。
- 高 RTT 的感知延迟可以下降，但物理 RTT 不会消失。host 不可达、relay 不可达且没有 direct 路径时，系统只能保持明确可恢复状态。
- 设计依据包括 Codux 当前 iroh transport、terminal sequence/baseline、移动端 reconnect/后台保活能力，以及 Mosh、Eternal Terminal、tmux、RustDesk、NetBird 的公开实现经验。相关调研见 `docs/remote-project-research.md`。
