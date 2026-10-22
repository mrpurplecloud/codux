# 项目栏右上角角标：现状调研

当前结论：用户已撤回“回答末尾状态行及逐 Agent 收尾识别”。源码恢复为第 11–12 节的统一角标基础实现；第 13–14 节保留已撤回实验的历史，第 15 节记录回退。尚未打包、安装或完成真实远端和彩墨屏验收。任务执行状态以 [.ai/tasks/agent-session-badges.txt](/Users/mac/Documents/development/github_ref/codux/.ai/tasks/agent-session-badges.txt) 为准。

日期：2026-10-20。源码基线：`e74d31aa`。

范围：桌面端项目图标右上角的 Agent 角标，包括本地和远程状态入口。仅静态调研，没有修改产品源码、运行构建或执行测试；没有把具体误报现象视为已完成运行时复现。

## 结论

当前角标表达的是“项目内各终端最近收到的状态信号的聚合”，并非 Agent 会话的统一状态。角标主要依赖终端 OSC 控制序列；Agent 运行时另有一套会话状态，两者分别维护。因此，偏差不只是颜色选择问题，还涉及状态来源、终端与会话的对应关系、聚合优先级和完成提醒的清除规则。

最直接的几个结果：

- 普通 shell 命令也能触发“工作中”，不要求终端内存在 Agent。
- 一个终端的错误会盖住整个项目内其他终端的运行状态。
- 一个终端在运行时，另一个终端等待授权会被隐藏。
- 绿色表示聚合后剩下完成状态，不表示所有 Agent 会话都已成功完成。
- 完成角标同时承担“完成提醒”，通过特定点击清除，与会话运行状态混在一起。

## 1. 当前颜色与图形

右上角是 10px 角标，展开和折叠项目栏都使用同一绘制函数。

| 状态 | 显示 | 当前含义 |
| --- | --- | --- |
| `Working` | 橙色脉冲点 | 至少一个终端工作中，且没有更高优先级的错误 |
| `Waiting` / `Warning` | 橙色空心圆 | 至少一个终端等待或警告，且没有错误或工作中状态 |
| `Completed` | 绿色实心圆 | 至少一个终端完成，且没有错误、工作中、等待或警告 |
| `Error` | 红色实心圆 | 至少一个终端错误 |
| `Idle` / 无记录 | 不显示 | 没有可显示的终端状态；不能据此区分空闲、未识别、未收到事件 |

颜色常量：橙色 `#E6A35C`，绿色 `#78D891`，红色 `#F87171`。

注意：远程项目右下角还有连接角标。其绿色/橙色/红色表示连接、连接中、断开，与右上角 Agent 状态独立。

证据：[角标绘制](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/project_column.rs:1022)、[远程连接角标](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/project_column.rs:1069)、[颜色常量](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/theme.rs:20)。

## 2. 实际数据链路

1. PTY 输出监听器解析终端 OSC 序列，生成 `TerminalStatusEvent`。
2. `AIRuntimeSupervisor` 发布 `TerminalStatus` 事件；远程主机推送的状态也进入桌面同一应用路径。
3. 桌面以 `terminal_id` 为键维护 `pane_agent_lifecycle`，每个终端只保留一份当前状态。
4. 按 `project_id` 汇总项目内所有 worktree 的终端状态。
5. `project_lifecycle_snapshot()` 更新 `ProjectListState.lifecycle`；项目栏据此绘制。

事件携带终端实例、项目、worktree、时间戳和来源，但没有 Agent 会话 ID 或 turn ID。桌面的 `PaneAgentLifecycle` 也没有保存信号来源，仅在状态变化日志中记录来源。

已有防护包括终端实例匹配和时间戳排序，防止部分旧事件覆盖新实例；这些防护并不验证事件是否对应当前 Agent 会话或当前回合。

证据：[输出监听器](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/terminal_pty/watcher.rs:45)、[事件结构](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/terminal_status.rs:16)、[桌面事件应用](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:156)、[项目聚合](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:339)、[项目列表同步](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/project_column_actions.rs:145)。

## 3. 哪些信号会改变角标

| 来源 | 映射规则 | 局限 |
| --- | --- | --- |
| 进度 OSC `9;4;…` | `0` 完成；`1` / `3` 工作中；`2` 错误；`4` 警告 | 直接采用终端进度语义，没有核验 Agent 回合是否成功结束 |
| 标题 OSC `0;…` | 标题含盲文字符时视为工作中；特定 `Action Required` 前缀视为等待 | 依赖标题文本和工具配置，不是会话协议 |
| 标题变化 | 工作标题变普通标题时视为完成；等待标题变普通标题时清为空闲 | 标题变化被用于推断回合结束或提示消失 |
| 通知 OSC | 匹配 `Approval requested…`、`Codex wants to edit…`、`Plan mode prompt:…` 时等待 | 依赖固定英文文本 |
| shell OSC `133;…` | `C` 命令开始时工作中；`A` 提示符或 `D` 命令结束时空闲 | 普通命令也参与；`D` 后的退出码没有用于区分成功、失败 |
| PTY 退出 | 清为空闲 | 不是统一的 Agent 成功或失败结果 |

监听器安装在运行时注册的终端上，进入上述映射前没有要求已经识别出 Agent。

这里的红色来自 `Error` 状态映射，主要入口是进度 OSC 码 `2`。不能把它直接解释成“Agent 会话失败”。PTY 的 `Error` 事件另走终端活动通道，没有在该监听器里直接转换成红色角标。

证据：[OSC 解析与映射](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/terminal_pty/osc.rs:146)、[信号处理](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/terminal_pty/watcher.rs:89)、[标题推断](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/terminal_pty/watcher.rs:117)、[监听器安装](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/terminal_pty/manager.rs:630)、[终端错误通道](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/bridge/mod.rs:237)。

## 4. 项目聚合规则

当前优先级从高到低：`Error`、`Working`、`Waiting/Warning`、`Completed`、`Idle`。

这是固定优先级，不比较哪个会话更近、是否当前会话、错误是否已处理，也不展示各状态数量。

| 项目内终端状态 | 当前显示 | 用户容易产生的误解 |
| --- | --- | --- |
| A 错误，B 运行 | 红点 | 看起来整个项目失败，B 的运行不可见 |
| A 等授权，B 运行 | 橙色脉冲点 | 需要用户操作的 A 被隐藏 |
| A 完成，B 空闲 | 绿点 | 绿点不证明 B 有任务、或所有任务成功完成 |
| Agent 已被运行时识别为运行，但没有对应 OSC | 可能无点或旧状态 | 角标与会话状态分离 |

这些聚合规则已有单元测试，包括“错误压过工作中”和“工作中压过等待”。因此，部分不直观行为是现有规则，不是绘制层随机出错。

证据：[聚合函数](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:79)、[现有聚合测试](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:544)。

## 5. 绿色、红色如何清除

- 后续终端状态可以覆盖当前状态；`Idle` 事件会直接移除桌面的该终端状态。
- 点击终端条目时，如果该条目显示完成，则把该终端的完成状态清为空闲。
- 点击 worktree 条目时，如果 worktree 的聚合状态是完成，则清除该 worktree 内终端的完成状态。
- 上述点击不清除错误、等待或警告。
- 移除注册终端时清除其记录；远程设备断开时清除对应远程记录。
- 桌面这份状态没有按时间自动过期。持续显示多久，取决于后续事件、上述点击或清理动作。

所以绿色带有“未确认完成提醒”的性质，但没有独立的已读状态。是否正在观看终端，也没有直接参与完成提醒的判定。

证据：[完成清除](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:298)、[终端点击](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/task_column.rs:1051)、[worktree 点击](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/task_column.rs:1224)、[终端移除](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/terminal_worktree_actions/terminal_mount.rs:327)、[远程清理](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:264)。

## 6. 与 Agent 会话状态的具体脱节

运行时已经维护另一条状态链：`AIRuntimeStateStore` 的会话快照、`project_phase`、`completed_phase`。这条链使用 hook、会话探测、转录监测和屏幕信号等信息。

但项目角标没有读取这套项目状态。桌面处理 `TerminalStatus` 事件更新角标，同时单独刷新 `state.ai_runtime_state`。只有后者变化时，不会把会话状态映射回角标。

两套规则还有明确差异：

1. 运行时项目阶段优先展示 `NeedsInput`，再展示 `Running`；角标优先工作中，再等待。
2. 运行时完成阶段检查活跃回合、完成时间和项目级确认时间；角标按终端保留最后一个状态，再由 UI 点击清除。
3. 运行时等待输入可见性有 30 秒期限；角标等待状态本身没有同样的期限。
4. 运行时项目完成阶段能携带 `was_interrupted`；角标的完成状态没有成功、取消、中断之分。

具体例子：OpenCode 插件的 `permission.asked` 分支发送 `needsInput` hook，但该分支没有同步发送等待 OSC。除非工具另有可识别的通知或标题输出，运行时可进入等待，而角标仍保留旧状态。

证据：[桌面两条刷新路径](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/ssh_remote_actions.rs:1338)、[运行时聚合](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/store/summary.rs:107)、[等待期限](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/store/summary.rs:276)、[OpenCode 授权分支](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime-assets/scripts/wrappers/opencode-config/plugins/dmux-runtime.js:445)。

还有一个快照差异：Supervisor 缓存不保存 shell 命令来源的非空闲状态，且 `Idle` 只删除缓存中的工作中或等待状态；但所有事件仍会发布给桌面。桌面则接受 shell 工作中事件，并让 `Idle` 清除任意状态。故 Supervisor 的终端快照与桌面的角标记录，也可能给出不同答案。

证据：[Supervisor 缓存与发布](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/supervisor.rs:299)、[桌面空闲映射](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:386)。

## 7. 后续重设计应先确定的语义

建议先确定以下契约，再选择颜色和图形；此处不展开实现。

1. **角标服务于什么？** 明确区分“正在运行”“需要用户处理”“有未读结果”，避免用一个完成状态同时表示运行结果和未读提醒。
2. **谁拥有会话状态？** 优先评估复用现有运行时会话状态，让 OSC 成为输入证据，而不是由 UI 单独维护另一套会话真相。现有运行时也包含推断和期限规则，不能直接换一个字段就认为准确性问题解决。
3. **多个会话如何展示？** 至少不能让后台运行遮住等待用户处理；错误应关联具体会话、回合和处理情况，不能无限压住新任务。
4. **完成如何确认？** 明确成功、取消、中断、失败，以及当前可见会话是否产生未读提醒；确认应关联具体结果，而不是终端上一个可被后续事件重写的状态。
5. **信息不足如何表达？** 无事件、未识别、失联与确实空闲应有明确区别，不能一律靠“无角标”推断。

后续验证应围绕确定的契约，覆盖：多会话运行与等待并存、旧错误后启动新回合、完成后查看、普通 shell 命令、工具缺少 OSC、远程断开及重连。

## 8. 已确认的设计约束与视觉候选

以下内容补充后续讨论中的约束，不改变前文对现有代码的描述，也不代表已经实现。

### 核心目标

用户需要在同时推进多个项目时，一眼回答两个问题：哪些项目仍有存活的 Agent 会话，哪些项目已经轮到用户参与。角标首先服务于分配注意力，而不是展示工具内部所有状态。

现有代码尚不能可靠满足此目标；本文的视觉方案也尚未实现或通过实机验证。真正有用户操作待办的会话必须优先提示；仅仅能接收下一条消息的空闲会话，不应冒充需要介入。

### 状态语义

- 先判断当前终端对应的 Agent 进程是否存活，再判断会话回合状态。不能用 shell 存活代替 Agent 存活，也不能把进程探测失败解释为进程不存在。
- Agent 进程存活时，区分正在工作、确实待用户输入和空闲。当前任务确实在等用户提供下一条消息、回答问题、授权或确认时，必须使用同一色系。
- 上一轮回答完成不自动归为待用户输入。若当前没有明确的用户操作待办，应归为空闲；不能只因输入框仍可发消息，就提示需要介入。
- 刚启动、尚未回答的会话可以是空闲，但不能据此宣称上一轮或整个任务已完成。
- 已读提醒单独处理；查看结果不能把一个仍然存活、正在等待用户输入的会话改成“无会话”。

现有进程树探测可以作为复用起点：从注册终端的 shell PID 遍历后代进程，识别支持的 Agent 工具；默认每 5 秒执行一次。当前接口返回终端对应的工具名称，并非完整的 Agent PID 与生命周期记录，其准确性和会话对应关系仍需验证。

证据：[进程树探测](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/process_detect.rs:4)、[探测调度](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/supervisor.rs:498)、[探测间隔](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/constants.rs:8)。

### 彩色墨水屏约束

角标必须在静态、低饱和度和灰度条件下仍可辨认，不能依靠颜色或动画单独传递状态。

视觉候选为：工作中使用深蓝实心圆，所有确实等待用户输入的状态使用同一种深橙实心方块。空闲但仍存活的会话使用中性标识，不抢占待介入提示；具体中性色值与形状尚未确定。等待原因在会话条目或提示中说明，不再为同属待办的下一条消息、回答问题、授权分别分配颜色。颜色与形状均为待验证候选，尚未确定最终色值。

- 默认使用静态形状，不依赖脉冲、闪烁、旋转、渐变或透明度变化表达工作中。
- 使用足够面积的实心填充、清晰的深色轮廓和与项目图标分离的浅色边界；避免只靠细空心圈或浅色小点区分。
- 当前 10px 角标偏小，先比较 14–16px 的实际尺寸静态样稿，再确定尺寸与位置，避免遮挡项目图标。
- 同时检查浅色、深色、自定义颜色项目图标，以及项目栏展开和折叠后的效果。
- 灰度预览只能验证形状和明暗辨识；彩色墨水屏的色差、刷新残影和实际可见性必须通过实机确认。目前没有完成实机验证。

### 多会话聚合

按最新目标，建议任一当前存活的 Agent 会话有明确用户操作待办时，项目就优先显示“待我介入”。等待回答问题、等待授权确认，以及明确需要用户提供下一条消息才能继续，都纳入该状态，并使用同一色系。单纯上轮完成且输入框可用，不自动纳入。其他会话仍在工作，不能掩盖这个项目已经需要用户参与。

只有没有会话确实等待用户输入、且至少一个会话正在工作时，项目才显示“Agent 工作中”。如果所有已确认存活的会话都空闲，则显示中性状态，表明有会话但没有已确认待办。确认不存在 Agent 会话时不显示角标；探测失败或信息不足必须保留“未知”，不能冒充没有会话或任务完成。

同项目“1 个待我、2 个工作中”可以在展开项目条目中用简短文字补充，角标保持单一主状态。等待原因保留在会话条目中，不要求用户悬停或打开项目后才能判断是否需要介入。错误状态的聚合另行确定，不能直接沿用旧错误永久压过新工作的规则。

### 进一步改进与验收

1. **固定位置、固定含义。** 所有项目都在同一位置显示角标；“Agent 工作中”和“待我介入”使用不同的静态形状。同一类待输入不再按工具或等待原因变化配色。
2. **保持项目位置稳定。** 先依靠醒目的待我角标快速扫视，不默认随事件自动重排项目，避免用户寻找的目标不断换位置。
3. **提示持续到状态改变。** 单纯查看、选中项目或经过一段时间，不代表用户已经回复或授权。已读可以改变未读提醒，但不能抹掉仍在等待用户输入的事实。
4. **准确关联当前会话。** 进程退出、会话替换、新回合开始后，不能继续用旧完成或旧错误表示当前状态。应复用运行时状态归属机制，避免 UI 再维护一套独立真相。
5. **先验证一眼辨识，再增加信息。** 首版不需要把所有错误类型、等待原因、工具名称和会话数量塞进角标。可先验证两种主状态，只有实际扫视仍有困难时再增加汇总或筛选能力。

验收应使用多个项目并列的真实尺寸静态画面：用户不悬停、不打开项目，能够快速指出所有有 Agent 会话的项目，以及其中需要自己介入的项目。还应覆盖同项目工作与待输入并存、无 Agent 的普通 shell、已结束会话残留信号、展开和折叠项目栏。灰度画面用于检查形状辨识，彩色墨水屏实机用于确认可见性和刷新效果；这些验证目前均未完成。

### 界面复用范围

“待我介入”是状态含义，不是在角标中显示文字。项目角标负责汇总，现有终端列表条目上的状态标识负责定位具体会话；两处统一状态来源、颜色和静态形状。基础方案不新增面板、窗口状态栏或终端内容区覆盖层。

证据：[现有终端条目标识](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/task_column.rs:1089)。

## 9. 远程项目补充调研（2026-10-21）

### 需求与范围

远程项目同样需要在项目栏显示 Agent 会话状态，不要求项目文件存在于本机。远端负责判断 Agent 进程与会话回合状态，本机接收并显示；不能在本机扫描远程项目路径来判断远端是否有 Agent。

本地与远程使用相同状态语义、聚合优先级和视觉方案：有会话待用户输入时优先提示待介入；所有待输入使用同一色系；复用项目角标和终端列表状态标识。右下角远程连接角标继续表达连接情况，右上角表达会话状态，两者不能混用。

桌面 host 与 headless host 已复用同一 `AIRuntimeBridge` 和终端运行时，远端注册 shell PID 后执行进程探测，因此采集层已有复用基础。

证据：[headless 共享运行时](/Users/mac/Documents/development/github_ref/codux/apps/agent/src/host.rs:1043)、[桌面运行时初始化](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime/src/runtime_state/service_bootstrap.rs:47)、[终端注册进程探测](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/terminal_pty/manager.rs:630)。

### 已确认的现状

1. **绘制入口已经支持远程项目。** 右上角绘制只看聚合状态，没有仅本地项目才绘制的条件。`project.exists` 仅影响文字颜色，不能把“文件不在本机”当作角标缺失的代码根因。
2. **项目 ID 转换已经存在。** 远端项目 ID 与本机目录记录 ID 可以不同，但入站消息已有转换，不能直接把二者不同判为故障。
3. **`terminal.status` 不限当前选中项目。** 远端向已连接的控制端广播状态变化，本机汇集各在线连接的事件。未注册当前终端窗口也不是一律拒绝事件。
4. **缺少首次连接和重连快照。** 该通道转发新状态事件，没有在连接建立后补发全部当前会话状态。断连时桌面又会清除该设备的会话标识，因此连接前已经处于等待、之后没有新事件的会话，可能持续没有角标。
5. **已有 `ai.stats` 不能直接承担全项目角标。** `currentSessions.status` 已包含 `needs-input`、`running`、`completed`、`idle`，但当前统计订阅每台设备只跟踪一个项目，也可能限定到 worktree；桌面统计应用路径只处理选中远程项目。这与所有项目常驻可见的目标不同。
6. **`isRunning` 不是进程存活。** 它表示当前是否工作中；Agent 进程存活但等待用户输入时，也会是 false。现有远程会话摘要没有独立表达进程存活或探测未知，不能只靠这个布尔值过滤角标。

上述事实说明，同一方案可以覆盖远程项目，工作重点是统一状态来源和补齐同步，而不是另做一套远程 UI。用户当前环境是否恰好由缺少重连快照触发，仍需运行时证据；本次未连接远端主机，也未复现具体故障。

证据：

- [项目角标共用绘制入口](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/project_column.rs:827)、[展开项目栏入口](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/project_column.rs:922)。
- [入站 ID 转换](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime/src/remote/controller.rs:480)、[项目与 worktree 映射字段](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime/src/remote/project_bridge.rs:20)。
- [全部在线连接的状态接收](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime/src/remote/controller_manager.rs:1145)、[终端实例接收条件](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:379)。
- [桌面 host 的一次性状态广播](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime/src/remote/host/mod.rs:468)、[headless host 转发新事件](/Users/mac/Documents/development/github_ref/codux/apps/agent/src/host.rs:1270)、[断连清理](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/agent_lifecycle.rs:264)。
- [远程会话摘要字段](/Users/mac/Documents/development/github_ref/codux/crates/codux-protocol/src/lib.rs:197)、[会话状态映射](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime_state.rs:516)。
- [桌面 host 单项目统计订阅](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime/src/remote/host/ai_stats.rs:84)、[headless 单项目订阅](/Users/mac/Documents/development/github_ref/codux/apps/agent/src/host.rs:826)、[桌面只应用选中远程项目统计](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/ssh_remote_actions.rs:1496)。

### 建议的最小实现边界

- 复用远端已有运行时的进程探测与会话状态，由同一语义来源生成本地和远程角标状态；不能继续让远程仅靠终端 OSC 猜测。
- 优先复用现有 `terminal.status` 广播、控制端接收、项目 ID 转换与断连处理。首次连接和重连需要补当前全量快照，之后继续接收变化；快照和变化必须覆盖状态删除，并避免旧状态覆盖新状态。
- 当前共享运行时已有全 host 会话快照，以及 `terminal_statuses_snapshot()`；但后者仍是终端信号缓存，不能原样当作新方案的会话真相。未发现现成的全 host 会话状态远程查询；`ai.state` 是指定项目的历史索引状态，也不能替代它。
- 同步信息需要能表达当前会话的进程活性、回合状态和未知情况，并保留所属设备、项目、worktree、终端及实例身份。设备间同名项目或相同远端 ID 不能串状态；当前未证实存在 ID 碰撞。
- 不要求先进入项目、附着终端或订阅终端画面，才能获得项目角标。所有已连接且列在项目栏中的远程项目都应能更新。
- 远程断连时，不能把失联前的工作中或待输入继续当作实时事实，也不能把失联等同于 Agent 已退出；会话状态应进入未知，连接情况由现有连接角标说明。

证据：[共享运行时终端快照接口](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/ai_runtime/bridge/mod.rs:213)、[headless 的历史 AI 状态查询](/Users/mac/Documents/development/github_ref/codux/apps/agent/src/ai_stats.rs:66)。

### 后续验证

- 首次连接时，远端项目 A 已在工作、B 已在等输入，不打开 A/B 也能立即获得正确状态。
- 不切换当前项目，后台远程项目从工作中转为待输入，角标及时更新。
- 断连时状态未知；重连后即使远端没有新输出，也能通过快照恢复真实状态。
- 同项目多个 worktree 和会话状态不同，按待输入优先汇总，终端列表分别显示。
- 多台主机存在同名项目或相同远端 ID 时，状态互不影响。
- Agent 进程退出、会话替换、普通 shell 命令和探测失败，不产生虚假的工作中或待输入。
- 桌面 host 与 headless host 使用同样状态语义，并在实际改动后按独立产品版本规则记录兼容组合。

这些验证尚未执行。本轮仅补充调研、设计边界和术语，不修改产品代码，不构建或部署。

## 10. 回答结束、待输入与任务完成的区别

用户补充了两个不同场景：事情已经做完、会话可以结束；事情尚未完成、确实在等待用户输入。此前把“上轮完成后能接收消息”一律划入待输入，范围过宽，本节及第 8 节已修正该约定。

### 不能仅凭进程或回合结束判断

Agent 进程存活，只证明会话仍在；`turnCompleted` 或终端恢复输入，只证明本轮结束。两种事实都无法可靠判断当前业务任务是否已经做完，也无法证明用户必须继续回复。

例如，“测试通过，修改已交付”与“需要你选择保留哪一种方案”都可能以同样的回合结束事件收尾。区别在于当前是否还有明确的用户操作待办，不能靠等待时长或标题恢复普通文本来决定。

### 建议判定边界

| 已知事实 | 状态含义 | 角标方向 |
| --- | --- | --- |
| Agent 正在思考、生成或执行任务 | 工作中 | 工作色与静态形状 |
| 有明确授权、问题、材料、选择或确认待办 | 待用户输入 | 统一待介入色系 |
| 本轮结束、进程存活，没有已确认用户待办 | 空闲，可能可结束或接新任务 | 中性标识，不宣称业务完成 |
| 任务的完成条件已满足或用户确认完成 | 任务完成 | 不作为必须回复的提醒；是否退出会话另行决定 |
| Agent 进程确认退出 | 无存活会话 | 移除会话存活标识 |
| 证据不足或远端失联 | 未知 | 不冒充无会话、业务完成或明确待输入 |

工具提供的明确等待事件可以作为证据。普通自然语言问题或任务完成结论，若没有稳定的结构化信号，仍需在后续实现中核实各工具能提供什么；不能宣称所有 CLI 已能准确自动区分。文本语气、“有没有问号”或模型自报完成都不能单独作为自动关闭会话的依据。

“任务做完，应该退出”是会话生命周期的另一个决策。本轮不引入自动退出：只有明确的关闭操作或另行确认的生命周期规则才能结束进程，不能因为一轮回答结束就自动杀掉会话。

同色要求继续成立：所有确实需要用户输入的情况使用同一色系；空闲或任务已完成之所以不同，是因为没有已确认的用户操作待办，而不是把不同输入类型分配不同颜色。已读状态仍与会话状态分开，读过问题不代表已经回答问题。

后续应验证：明确提问与无提问的回答结束、授权等待与普通空闲、用户已读但未回复、任务完成但进程未退出，以及同项目空闲和工作会话并存。以上仍是设计边界，未修改或验证产品实现。

## 11. 实施流程与桌宠取舍

用户指定使用 bhchange，禁止 Superpowers。唯一任务状态记录为 [.ai/tasks/agent-session-badges.txt](/Users/mac/Documents/development/github_ref/codux/.ai/tasks/agent-session-badges.txt)，等级 Guarded。此前由本助手创建且未执行的 Superpowers 计划文件已撤下，不再作为执行指令或重复状态记录。

实现扩展现有共享会话 owner、远程资源订阅与 baseline、现有项目和终端标识。已采用全 host 的 `agent.activity` 资源，沿用 `resource.subscribe`、连接身份和项目 ID 转换。订阅立即回复完整基线，复用现有 1 秒 tick 发布完整小快照；未增加独立轮询线程或增量补丁系统。

新快照只读取内存状态。项目已有慢资源发现持有共享锁导致 UI 阻塞的修复，不能为了快照重新在 core 锁内扫描进程、读取转录或访问远程目录。

用户允许在桌宠影响性能或可维护性时移除或禁用；这不是本轮无条件删除桌宠的要求。现有 `pet_enabled`、`pet_desktop_widget`、`pet_static_mode` 开关可优先复用；目前未修改开关，也未测量桌宠性能损耗。若确认只需禁用就能隔离成本，优先使用该路径，不先删除整个子系统或宠物数据。

旧 `terminal.status` 除桌宠外，还被任务监控使用。因此，既有通用终端信号保留原契约；项目角标和会话列表改用统一 Agent 投影。桌宠未删除或禁用，未修改宠物数据，也未测量其性能成本。

证据：[现有桌宠开关](/Users/mac/Documents/development/github_ref/codux/apps/desktop/runtime/src/settings/service_pet.rs:36)、[开关与窗口同步](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/settings_actions.rs:819)、[桌宠动画入口](/Users/mac/Documents/development/github_ref/codux/apps/desktop/src/app/pet_actions.rs:234)、[任务监控消费者](/Users/mac/Documents/development/github_ref/codux/crates/codux-runtime-live/src/agent_worktree/task_monitor.rs:63)。

## 12. 实现结果与验证

### 统一状态

共享 `AIRuntimeStateStore` 拥有会话事实。进程扫描结果与回合状态分别保存；UI 仅镜像和汇总。明确待办按当前终端实例和 Agent 会话身份保留，回合完成本身既不创建也不清除待办。用户回复、恢复执行或会话替换可清除待办；查看项目或经过一段时间不会清除待办。

| 状态 | 含义 | 项目角标及会话标识 |
| --- | --- | --- |
| `NeedsInput` | 已确认有明确用户待办 | 深橙 `#B45309` 实心方块 |
| `Unknown` | 探测失败、尚无快照或远端断连 | 中性问号圆形图标 |
| `Working` | Agent 进程存在，回合正在执行 | 深蓝 `#1D4ED8` 实心圆 |
| `Idle` | Agent 进程存在，没有当前工作或已确认待办 | 中性粗圆环 |
| `Absent` | 扫描确认没有 Agent 进程 | 隐藏角标 |

项目和 worktree 聚合顺序为 `NeedsInput > Unknown > Working > Idle > Absent`。同一项目有工作中与待输入会话时，待输入优先。所有明确输入待办使用同一颜色与形状，不区分回答、选择或授权。没有新增面板、窗口状态栏或角标文字。

标识内部尺寸为 14px，外层为 18px，使用 2px 轮廓和不透明背景。已移除角标的 33ms 脉冲刷新及完成后点击清除逻辑。根据源码尺寸生成并查看了颜色和灰度静态示意，圆点、方块和圆环可通过形状区分。临时样稿为 `/private/tmp/codux-agent-badges-preview.png`；它不是 GPUI 截图或彩墨屏模拟，不能证明实际项目图标遮挡、屏幕色差、残影和物理尺寸效果。

### 本地与远程

desktop host 和 headless host 均发布共享 Bridge 的完整活动快照，覆盖所有已注册终端。控制端在连接发布时订阅，无需打开某个项目；每个当前连接对象持有自己的快照，重连重新获取基线。控制端拒绝较旧版本，快照读取时再转换 host 项目 ID，避免活动基线先于项目目录到达时固化错误 ID。不同 host 的相同终端 ID 在 UI 中隔离。空权威快照移除旧会话；无快照或不支持该资源的旧 host 显示未知。

共享快照只锁内读取内存，进程探测和 transcript 发现仍在状态锁外。快照只在终端语义集合变化时增加版本；相同状态不触发 UI 重绘。进程探测沿用既有约 5 秒周期，不能宣称进程退出立即反映；远程推送沿用约 1 秒 tick。

### Hook 排序修正

实现期间，旧提问在用户回复后重放可重新制造待办。最初扩大 `session.updated_at` 比较范围虽拦住旧事件，却导致既有 `screen_signal_refines_active_turn_only` 回归：屏幕使用当前时间更新后，有效的 `turnCompleted` 被误判为过期。

按 bhchange 停止生产修改、记录反证并获得用户明确批准后，恢复原 `sessionStarted` 判断。新增顺序元数据只比较同工具、终端实例和 Agent 会话的 Hook 时间，不与屏幕或 probe 更新时间混用。旧提问重放、会话身份切换和既有屏幕回归检查均已通过，没有改写原测试预期。

### 已执行验证

| 检查 | 结果 |
| --- | --- |
| `cargo test -p codux-runtime-live ai_runtime::store --lib` | 71 passed；包括慢 I/O 锁隔离、6 项活动状态检查及原屏幕回归 |
| `cargo test -p codux-runtime remote::controller --lib` | 76 passed、4 ignored；忽略项沿用原 opt-in 配置 |
| `cargo test -p codux-runtime agent_activity --lib` | 5 passed；含 host 基线、空集合移除、控制端版本/连接隔离及订阅发送失败清理 |
| `cargo test -p codux-agent agent_activity` | 1 passed；headless provider 使用共享全量快照 |
| `cargo test -p codux-protocol agent_activity --lib` | 1 passed；共享聚合规则 |
| `cargo test -p codux agent_activity` | 3 passed；UI 聚合、host 隔离和静态形状 |
| `cargo test -p codux ai_runtime_status` | 4 passed；既有状态刷新回归 |
| `cargo check -p codux-agent` | 通过 |
| `git diff --check` | 通过 |

远程回归最初有 11 项失败，均来自原测试连接没有接收端：新增基线订阅发送返回失败，正确触发断连处理。测试连接补齐内存 host 后全部通过，并新增发送失败不会留下已发布连接的检查；没有放宽生产路径的失败处理。

Desktop 源版本为 `2.0.3-beta.27`，headless 为 `2.0.3-beta.23`；移动端保持 `2.0.3-beta.18+67`。实际安装版本未改变，跨产品记录见 [version-compatibility.md](version-compatibility.md)。

### 未证明范围

- 没有连接真实远端或逐个 CLI 验证首连、重连和后台会话变化。自动化测试证明上述源码边界，不代表已完成设备级端到端验收。
- 未提供稳定结构化信号的自然语言问题仍不能可靠自动分类；不会仅凭问号、输入框可用或回合完成制造待办。
- 没有实际彩墨屏验收；展开/折叠项目栏的真实画面、图标遮挡、主题差异和刷新残影仍待确认。
- 没有自动判定业务任务完成、自动关闭会话、提交、打包、安装或部署。

## 13. 补齐普通回答中的待输入语义（已撤回方案）

本节及第 14 节只保留历史调查与实验结果，当前产品不包含该机制。用户先批准试验，后因逐 Agent 适配的维护成本要求撤回，回退结果见第 15 节。

### 已核对的正确路径与缺口

- desktop 的 `service_ai_memory.rs::prepare_memory_launch_artifacts_at` 已生成托管启动指令。它通过 `extra_context` 注入环境规则，即使记忆功能关闭仍然有效。headless 的 `apps/agent/src/memory.rs` 使用同一 `MemoryService` 生成按 worktree 隔离的指令文件。
- 已有注入适配包括 Codex developer instructions、Claude/OMP append system prompt、OpenCode/MiMo system transform。CodeWhale、Kimi、Kiro、AGY 当前 driver 没有同样的注入适配，不能声称所有工具都已覆盖。
- Codex 和 Claude probe 均能读取 assistant 正文，OpenCode probe 能读取数据库中的正文及原生完成信息。现有 `assistant_preview` 只保留前三行、最多 180 字，不能直接拿来识别回答末尾状态。
- 各 probe 已维护当前用户消息、回合结束、会话身份及中断信息。新证据应在这些既有边界内提取，交给同一个 store；不应让 UI 猜测终端内容，也不另建远程状态通道。

证据：[共享启动内容](../crates/codux-memory/src/compat.rs)、[desktop 环境指令](../apps/desktop/runtime/src/runtime_state/service_ai_memory.rs)、[headless 启动内容](../apps/agent/src/memory.rs)、[驱动注入策略](../crates/codux-runtime-live/src/ai_runtime/tool_driver.rs)、[预览截断](../crates/codux-runtime-live/src/ai_runtime/probe/preview.rs)、[Codex 原生回合边界](../crates/codux-runtime-live/src/ai_runtime/probe/codex/parse.rs)、[Claude 原生回合边界](../crates/codux-runtime-live/src/ai_runtime/probe/claude.rs)。

### 建议方案：Agent 明确报告本次请求的收尾状态

通过既有托管指令要求 Agent 在最终回答末尾使用一行固定状态说明。其目的不是让程序猜自然语言，而是由掌握任务上下文的 Agent 作语义判断，再让运行时严格读取结构化约定。状态行本身也是用户可读的信息；角标仍不显示文字。

| 报告 | 适用情况 | 角标投影 |
| --- | --- | --- |
| `〔需要你参与〕` | 缺少用户的答案、材料、选择或授权就无法继续当前请求 | `NeedsInput`，沿用统一橙色方块 |
| `〔本次请求已完成〕` | 当前请求的工作与验证已完成，没有必须由用户处理的后续步骤 | `Idle`，沿用中性圆环 |
| `〔尚不能判断〕` 或缺少有效报告 | 无法确认收尾状态、工具没有适配、旧会话未接受新指令，或读取失败 | `Unknown`，不能默认显示成已经完成或无需参与 |

“如果还需要优化，可以告诉我”属于可选后续，不应报告需要用户参与。明确要求“选择 A/B 后才能继续”才属于必须参与。报告只涉及本次请求；不宣称整个项目已经完成，也不自动退出会话。

严格读取当前已结束回合的最后一条 assistant 正文。必须先从完整正文提取状态，再生成短预览；代码块、引用、工具输出、reasoning、旧回合和中途回答里的同样文字都不能触发状态。报告矛盾、格式错误或没有可靠当前回合归属时保留未知。已确认的原生授权/提问待办优先，完成报告不能覆盖尚未解决的原生待办。新用户消息、会话替换和进程退出使旧报告失效；迟到的旧报告不能覆盖新回合。

首轮闭环以已有提示注入和完整最终回答读取能力的 Codex、Claude、OpenCode/MiMo 为可验证范围。其他工具的快照结构补充缺省值，未取得有效报告时显示未知；不在没有实际适配证据时承诺全工具语义识别。旧会话需要新托管指令生效后才能获得报告，不能把改源码描述成对现有进程即时生效。

### 备选与取舍

- **后台工具报告**可以避免回答中的状态行，但需要额外工具调用，并可靠绑定当前回合；现有普通 Hook payload 没有通用回合令牌。不能仅用终端 ID 或写入时间充当当前回合证明，也不能在报告后让后续工具输出把待办擦掉。作为用户不接受状态行时的备选，本轮未选定或实现新命令。
- **额外模型分类**能处理原有无标记回答，但会增加调用、延迟和 provider 依赖；headless 当前没有可直接复用的常驻分类配置。不是本轮建议的首选。
- **关键词或问号规则**无法可靠区分必要问题、引用、反问和可选邀请，不作为确定待办或任务完成的依据。

用户已批准状态行方案。实现复用托管指令和原生转录解析，没有新增后台报告命令、RPC 或分类模型调用。

### 验证约定

未修改生产版本的新基线：probe 73 passed、desktop `launch_artifacts` 2 passed、headless `memory::tests` 2 passed。首次 desktop filter 不匹配，0 项运行，未记作通过；修正过滤后运行了上述 2 项。

实施后必须验证：必要提问与可选邀请、明确完成与无报告、超过 180 字的回答末尾、引用/代码/工具输出中的伪标识、回合未结束、用户已经回复、旧报告晚到、会话替换和中断。已有 store 71 项基线、慢 I/O 锁隔离和原屏幕/Hook 排序契约保持。远程继续消费同一活动快照，原聚合、配色和连接隔离不另建分支。

## 14. 收尾状态行实现与试验结果（已撤回历史）

以下是撤回前的实现与试验，不代表当前源码或当前发布契约。

### 已实现

共享 `agent_activity_ai_directive()` 注入 desktop 与 headless 的托管环境指令，即使记忆功能关闭也生效。它要求最终回答以一行固定状态说明收尾；必要回答、选择、授权或材料才使用 `〔需要你参与〕`，可选的“有需要可以继续问我”使用完成报告，无法判断或其他阻塞使用未知报告。当前用户指令优先，不强迫覆盖用户明确要求的输出格式。

Codex、Claude、OpenCode/MiMo 从原生最终回答提取 `AgentTurnDisposition`，通过可选的 `AIRuntimeContextSnapshot.assistant_turn_disposition` 传给现有 store。提取发生在 180 字预览截断之前。未完成回合、reasoning、工具结果、引用、代码块和冲突报告不能制造确定收尾结论。Codex 也排除 commentary/analysis 阶段；Claude 的增量合并不会沿用已被最新无标记回答替换的旧报告；OpenCode/MiMo 按消息时间选择最新回答，打开的工具调用不算有效最终报告。

原 `agent_input` 事实扩展为同一 owner 内的 attention 记录，保留实例/会话身份、原生待办和有完成时间的报告。原生待办优先于完成报告；新用户输入或恢复执行会清除旧事实，较旧报告不能覆盖较新回合。只改变报告也会作为有效状态变化发布，不要求 token 或其他会话字段同时变化。最终活动仍沿用现有 `agent.activity` 快照、远程连接隔离、聚合和静态角标。

### 可重复的试验

已用真实格式的 Codex JSONL 转录，经 `probe_runtime`、`AIRuntimeStateStore` 和活动投影执行以下序列。这是本地 fixture 试验，不是实际模型或已安装应用的屏幕验收。

| 最终回答 | 结果 |
| --- | --- |
| 超过 180 字的说明，最后要求选择 A/B，并带 `〔需要你参与〕` | `NeedsInput`，橙色方块 |
| 检查通过，只邀请用户按需继续交流，并带 `〔本次请求已完成〕` | `Idle`，中性圆环 |
| 只有“请选择 A/B？”但缺少报告 | `Unknown`，不猜测是否完成 |
| 仅在代码示例中出现 `〔需要你参与〕` | `Unknown`，不误当当前待办 |

还验证了回复后旧报告晚到、下一轮缺少报告、重复快照不重复改变状态、原生待办不能被完成报告覆盖、实例替换、错误会话身份和中断回合。Claude 与 OpenCode/MiMo 使用各自原生记录和 SQLite 文本 part 进行解析检查。

### 自动化结果

| 命令 | 结果 |
| --- | --- |
| `cargo test -p codux-runtime-live agent_disposition --lib` | 9 passed |
| `cargo test -p codux-runtime-live ai_runtime::probe --lib` | 78 passed；修改前为 73 |
| `cargo test -p codux-runtime-live ai_runtime::store --lib` | 75 passed；前一版为 71，含原屏幕回归和慢 I/O 锁隔离 |
| `cargo test -p codux-runtime-live wrapper_applies --lib` | 6 passed；既有启动权限、参数及提示注入路径 |
| `cargo test -p codux-runtime launch_artifacts --lib` | 2 passed；包括 memory 关闭时的三种状态说明 |
| `cargo test -p codux-agent memory::tests --bin codux-agent` | 2 passed；headless 托管指令与 worktree 隔离 |
| `cargo test -p codux-runtime agent_activity --lib` | 5 passed |
| `cargo test -p codux-agent agent_activity --bin codux-agent` | 1 passed |
| `cargo test -p codux agent_activity` | 3 passed；包含桌面编译与 UI 状态契约 |
| `git diff --check` | 通过 |

### 生效边界

本次没有打包、安装或部署；当前正在运行的旧应用和远端服务不会因为源码更新而获得新逻辑。更新产品后，新启动且接受托管指令的支持会话才可以稳定使用报告约定；旧会话、输出格式覆盖或 Agent 没有遵循约定时，结束回合显示未知，不假装已经完成。CodeWhale、Kimi、Kiro、AGY、OMP 暂无本轮完整语义闭环，保留未知；它们原有明确授权/提问信号仍优先提示待介入。

尚未用实际模型测量状态行的遵守率或误报率，也未做真实远端和实际彩墨屏验收。报告“本次请求已完成”仍是 Agent 声明，不是自动验收整个项目的证明；没有引入自动退出会话。

## 15. 用户要求撤回收尾状态行

用户认为逐个 Agent 适配太麻烦，明确要求撤回。此次仅撤回第 13–14 节的附加实验，保留第 12 节的本地/远程统一活动快照、项目与会话静态标识，以及已验证的同源 Hook 排序修正。

已移除托管状态行指令、`AgentTurnDisposition` 类型和快照字段、Codex/Claude/OpenCode/MiMo 专用收尾解析、对应测试及 store 中的报告记录。23 个实验独有文件恢复到实验前的 HEAD 内容；共享文件只移除实验增量。生产源码中已不存在相关状态行或类型/解析器引用。

普通回答结束恢复为“没有明确待办时空闲”，空闲不代表整个任务已完成；缺少末尾报告不再制造未知。明确授权/提问等待仍沿用原生信号，探测失败或远端不可用仍显示未知。不引入替代语义分类、额外模型或自动退出。未对已安装应用、远端服务或用户会话作任何修改。

回退后 Store 71 项、probe 73 项和远程活动资源 5 项检查通过。基础角标版本仍保留 Desktop `2.0.3-beta.27`、headless `2.0.3-beta.23` 的源码增量；移动端不变。收尾实验的旧测试结果仅作为历史保留，不再作为当前支持能力。

## 16. 会话总览浮窗（源码已实现，实机待验收）

### 当前保留的效果

本地和已连接远端共用同一活动状态：橙色实心方形表示已有明确待处理输入，蓝色实心圆形表示工作中，中性空心圆表示进程仍在但当前空闲，中性问号表示无法确定；没有 Agent 时隐藏。项目内任一会话有明确待办时，项目优先显示橙色。进入项目后，现有会话列表分别显示各会话状态。

这能帮助用户快速找到运行时已经识别的授权或提问等待，但不能保证找出所有需要参与的会话。普通回答中的自然语言提问仍可能显示空闲；空闲同时包含“这次工作已结束”和“没有结构化提问信号但实际仍需用户回答”。前述按 Agent 收尾报告的实验已撤回，不恢复。源码改动尚未打包、安装或部署，已安装应用未获得本次效果；真实远端及彩墨屏验收也未完成。

### 建议的可观察行为

增加可开关、可拖动、置顶的独立会话总览浮窗。默认约 360 像素宽，内容过多时滚动；背景使用较高不透明度的半透明底色，文字和状态标识保持不透明。浮窗与桌宠开关、动画和对话逻辑无关，不增加模型调用。

浮窗列出 Codux 管理的本地及已连接远端 Agent 会话，包括未打开的项目；不包含普通 shell、未连接设备或 Codux 未管理的外部终端。每行显示项目、设备、会话标识、当前状态和最近输出片段。明确待输入置顶，其后为空闲、未知、工作中；同组保持稳定顺序，不因每个新字符改变行位置。用户可扫描空闲会话的输出，自行判断是否需要继续参与。

每行最多显示最近三个非空终端行、最多 240 个字符。内容名称明确为“最近输出”，不是“最后一句回答”。不新增 Agent 专用解析，不要求 Agent 输出状态行，不通过问号或文本内容另行推断状态。现有渲染屏幕可提供纯文本，但底部可能包含输入提示、工具日志和状态栏；这一方案不能保证显示最后一句 assistant 自然语言。若用户必须获得精确最后一句回答，需要另议数据契约，不能承诺通用截屏已经满足。

点击一行激活主窗口，定位对应设备、项目、worktree 和终端 pane，并聚焦输入。定位复用已有项目/worktree 加载及终端挂载流程；异步加载结束后，只完成最新一次点击。会话已退出、实例已替换或远端已断开时，不跳转到同 ID 的其他会话，也不清除明确待办。

### 已核对的复用路径与缺口

- `apps/desktop/src/app/pet_actions.rs:299` 已有独立窗口创建，使用 `WindowKind::PopUp`；`main.rs` 已有原生模糊透明背景。只复用窗口能力，不能直接复制桌宠的完整 `CoduxApp`、动画、50 毫秒鼠标穿透循环及语音循环作为总览实现。
- `crates/codux-runtime-live/src/ai_runtime/registry.rs:74` 的 `screen_text` 从现有终端渲染屏幕取得纯文本，不依赖 Agent 类型。读取需要等屏幕 worker 回复，必须保持在 store core 锁外；优先复用 supervisor 已有屏幕采样，避免为浮窗重复抓取。
- `latest_assistant_preview` 当前是回答开头最多三行、180 字符，且 Hook 可以清空它；不能直接命名为最后一句回答。
- 现有全 host `agent.activity` 已负责首连、重连、版本和删除收敛，但 `AgentActivityRecord` 没有文本字段。若实施，需添加可选的有界最近输出字段，沿用同一资源；旧 host 无文本时显示“暂无最近输出”，不把未支持误报为无会话。移动端不增加界面。
- `terminal_accessors.rs` 已有选择 runtime terminal 和聚焦方法；`worktree_actions.rs:889` 已有 worktree 切换，加载结果使用 `project_switch_generation` 防止过期结果生效。浮窗必须接入这些正确路径；只改 active terminal ID 不足以跨项目、跨设备定位。
- 已查相关窗口和 worktree 文件近期历史，包括 `1116460e` 的 workspace/pet 状态修正和 `.ai/tasks/desktop-terminal-list-sync.txt` 的 host 集合同步记录；沿用既有 owner 和生命周期，不新建第二套会话发现、状态判断或连接管理。

### 实施边界与验收

状态事实仍由共享运行时持有，浮窗只展示；最近输出由 host 端现有终端屏幕产生，保存在会话的易失投影中，不新增持久化。主窗口每秒发布显示差异；文本沿用屏幕事件和已有 5 秒 supervisor 采样，不能把一秒 UI 检查误称为所有输出一秒内可见。仅状态或片段变化时更新界面，不逐字符刷新、不持续动画、不新增轮询线程。快照读取不重复进行慢 I/O；跨 host 的身份包含设备及实例。

验收覆盖：本地及已连接远端未打开项目仍出现；明确待办始终置顶；文本变化动态可见；普通 shell 不出现；退出移除；断连标未知且旧文本不能冒充实时内容；旧 host 缺字段仍可兼容；跨项目/worktree/pane 点击准确；连续点击 A、B 后迟到的 A 不抢焦点；关闭浮窗后无独立刷新任务残留。静态样稿需检查正常和灰度对比，实际彩墨屏仍需设备验收，不能以截图代替。

用户已明确“是”，批准新增浮窗及通用最近输出的语义差异。源码已增加状态栏“会话总览”开关、独立约 360 × 520 像素置顶窗口、94% 不透明背景、静态高对比标识、状态分组、片段展示，以及当前连接和终端实例复核。浮窗关闭后没有独立计时或刷新任务；不启动桌宠逻辑，不增加模型调用。旧 host 缺少新增可选字段时仍可反序列化，显示“暂无最近输出”。这些都是源码能力，尚未打包、安装或部署。

本轮通过：共享片段与活动目标检查 8 项、协议兼容与文本版本检查 2 项、store 73 项、supervisor 11 项、远端 activity 5 项、headless activity 1 项、desktop activity 3 项、浮窗排序/断连/身份逻辑 3 项；新增完成回合仍采样的 supervisor 用例单独运行 1 项后，也包含在 11 项检查中，不重复累计。桌面和 headless 编译包含在对应测试中，rustfmt 检查及 git diff --check 通过。没有真实鼠标点击、真实远端首开项目或彩墨屏验收，不能把纯逻辑测试宣称为端到端导航通过。

### 导航时序缺口及已批准修正

修正前，首次选择远程项目时，`selection.rs` 先使用 `cached_worktrees_from_state` 的本地目录缓存，再由 `project_actions/switch_load.rs` 并行请求新的工作区目录和终端布局。`hosted_worktree_summary` 在远端请求完成后才同步目录。浮窗导航只等待终端挂载，不等待这两个项目加载结果发布：目标工作区未缓存时，下一次 tick 会过早提示“无法定位”；缓存暂时完整时，也不能据此证明后续同 generation 加载不会覆盖当前工作区或布局。

因此修正前的纯逻辑测试不足以证明跨项目点击定位。不能用刷新重试提示替代这项验收，也不使用延时猜测、额外轮询或第二套远程加载器补偿。

用户明确批准第 4 次修订后，已接入 `apps/desktop/src/app/project_actions/switch_load.rs`。原先两个 worker 重复读取同一项目的工作区目录；现在复用其中一个，读取一次目录，再读取该目录对应的终端布局，在同一个主窗口回调先发布目录再发布布局。只有当前 generation 的实际发布完成，`project_session_load` 才解除导航等待；源错误及后台失败保留并显示，旧 generation 的结果不释放新等待。原本可能反序的两次独立发布已消除，不再为它们增加两组完成标志。

发布完成后，导航继续使用当前唯一目标，再选择工作区和终端；同项目的新点击不重置项目加载进度。`terminal_layout_loading` 仍只负责之后的挂载等待，实际 pane 实例校验仍保护焦点不能落到替换会话。两者没有被项目发布信号替代。

已缓存的布局和 `terminal_restored_generation` 继续保护普通项目切换。若当前导航目标缺失，而本次真实布局确认包含目标，加载 owner 仅移除该 scope 的过时布局缓存，再复用原恢复路径；切换到其他 worktree 时，也不允许缺少目标的缓存遮蔽新布局。没有自动选择首个会话、固定延时或额外加载器。

### 本轮验证与剩余边界

| 检查 | 结果 |
| --- | --- |
| `cargo test -p codux session_overview` | 9 passed；含原 3 项及新增发布等待、A/B/A 旧结果、失败传播、worktree generation、过期缓存场景 |
| `cargo test -p codux terminal_restore` | 4 passed |
| `cargo test -p codux active_terminal` | 2 passed |
| `cargo test -p codux agent_activity` | 3 passed |
| rustfmt 对本轮 5 个生产文件的 `--check`；`git diff --check` | 通过 |

这轮没有改变共享片段、协议或 headless；上一轮对应检查保留为历史证据，不重复声称本轮重跑。桌面编译包含在上述检查中。生产代码已冻结，未提交、打包、安装或部署。

[彩色与灰度静态样稿](agent-session-overview-preview.svg) 已保存，宽度按每窗 360 像素绘制；它不是 GPUI 实机截图。SVG XML 检查通过。暗色样稿 94% 不透明背景叠加纯白底时，正文对比度约 12.62:1，次要文字约 5.84:1；数值不代表彩墨屏的实际表现。当前浏览器工具无可用浏览器，未完成截图视觉核验。真实跨项目/worktree/pane 鼠标操作、实际远端首开目录和实际彩墨设备仍需验收，不用纯逻辑检查或样稿替代。

## 调研边界（初次调查时）

- 已核对源码入口、聚合、绘制、清除、会话状态旁路和现有相关测试内容；未运行这些测试。
- 尚未采集用户具体误报时的终端事件与状态日志，不能断言每一次红绿切换都来自同一个原因。
- 本会话没有提供 `codegraph_explore` 或 projectmem 工具；使用 context-mode 与源码检索交叉定位。
- 报告保存在仓库 `docs/` 目录，产品源码未修改。
