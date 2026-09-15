---
created_at: 2026-09-13T00:00:00Z
updated_at: 2026-09-13T00:00:00Z
status: proposed
completed_at:
---

实现仅在获得预算并批准方案后开始。任务按 ROI 排列；每项完成后单独验证，避免一次性改动整条连接链路。

## 0. 建立弱网基线和观测

**规模：** 小到中。**风险：** 低。**收益：** 为后续投入提供证据。

**阻塞：** 无。

- [ ] 0.1 固定 RTT、丢包、抖动、短断网、网络切换、direct 失败和 relay 失败测试档位。
- [ ] 0.2 记录 reconnect P50/P95、session restore 成功率、完整 baseline 比例、output gap 率、输入 ACK 延迟与重复率。
- [ ] 0.3 将每个 connection generation 与候选、路径、阶段、attempt 和失败类别关联。
- [ ] 0.4 验证现有日志不会把 `QUIC open` 误报为业务可用。

## 1. 前台 cache-first 恢复

**规模：** 小。**风险：** 低。**收益：** 最高的即时体验收益，wire 不变。

**阻塞：** 0。

- [ ] 1.1 回到前台先发布缓存 screen 和 session metadata。
- [ ] 1.2 无 sequence gap、generation 未变化且缓存未失效时，只做轻量 host/transport 确认。
- [ ] 1.3 只有 gap、generation 不匹配、缓存失效或 host 要求时才请求 baseline。
- [ ] 1.4 覆盖缓存显示、同步失败、旧结果迟到和恢复成功四类行为。

## 2. 连接阶段和 parked reconnect

**规模：** 小到中。**风险：** 中，涉及电量和生命周期。**收益：** 提升长断网可用性。

**阻塞：** 0。

- [ ] 2.1 统一状态为 transport open、protocol ready、host responsive、session restored 和 parked。
- [ ] 2.2 保持单一 reconnect owner，确认一次失败只推进一次 attempt。
- [ ] 2.3 当前退避耗尽后进入低频 parked probe；session 关闭、用户取消、后台策略或电量策略可终止 probe。
- [ ] 2.4 网络变化、回到前台和用户重试能够立即唤醒 parked 状态。
- [ ] 2.5 验证旧 generation 的连接结果不能清理新 generation 的 retry 或覆盖 UI。

## 3. session 连续性和 resume cursor

**规模：** 中到中大。**风险：** 中，涉及跨端状态机。**收益：** 减少完整刷新、流量和恢复等待。

**阻塞：** 1、2。

- [ ] 3.1 确认 host agent session 生命周期独立于 QUIC connection；断线只 detach。
- [ ] 3.2 controller 在恢复请求中携带 session ID、最后连续 `outputSeq` 和 terminal generation。
- [ ] 3.3 host 增加有界 replay ring buffer；cursor 可覆盖时发送 delta，否则明确要求 baseline。
- [ ] 3.4 controller 正确处理 gap、baseline watermark、held live output、重复 ACK 和旧 generation。
- [ ] 3.5 旧 peer 缺少 resume 字段时回退到现有 baseline 路径。

## 4. 输入可靠性

**规模：** 中。**风险：** 中到高，错误语义可能重复执行副作用命令。**收益：** 高，避免输入丢失和重复。

**阻塞：** 2、3 的 session identity 约定。

- [ ] 4.1 controller 为 session 生成单调 `inputId`，重试沿用原 ID。
- [ ] 4.2 host 按 session 和 input ID 做有界去重；重复输入只返回已有 ACK。
- [ ] 4.3 ACK 未到时保留 pending，恢复后重发；数量和时间超过上限时发布 `unconfirmed`。
- [ ] 4.4 输入、resize、Ctrl-C 在 output backlog 下仍优先发送。
- [ ] 4.5 验证 host 重启后不伪造 exactly-once，副作用命令进入明确的未确认状态。

## 5. output 背压和恢复优先级

**规模：** 中。**风险：** 中。**收益：** 高输出、弱网场景下保持可操作性。

**阻塞：** 3、4。

- [ ] 5.1 为 control、输入/resize、terminal output、baseline/replay 定义有界队列和调度优先级。
- [ ] 5.2 output backlog 达到上限时合并中间画面或请求最新 baseline。
- [ ] 5.3 验证不能静默丢失 ANSI 数据，也不能让 bulk replay 阻塞输入。
- [ ] 5.4 统计队列长度、丢弃原因、baseline 触发次数和恢复后的 screen 正确性。

## 6. 条件性 path/heartbeat 调优

**规模：** 中。**风险：** 中到高，可能影响 relay 负载、电量和连接回归。**收益：** 依赖基线数据。

**阻塞：** 0–5 的指标证明现有策略存在瓶颈。

- [ ] 6.1 按失败阶段区分 direct 黑洞、relay 不可达、候选过期和 host 离线。
- [ ] 6.2 仅针对有证据的阶段调整拨号窗口或候选顺序。
- [ ] 6.3 评估 heartbeat 对 NAT 保活、RTT 采样、电量和 relay 流量的影响。
- [ ] 6.4 未出现明确收益前，不增加第二套穿透协议或固定高频 heartbeat。
