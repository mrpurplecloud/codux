# 远程连接体验参考项目调研

调研目标：寻找可借鉴 UU 远程这类产品的连接建立、网络穿透、会话恢复、低延迟传输和运维体验的 GitHub 开源项目。Codux 当前是桌面/手机控制端加桌面或无界面主机，使用 iroh 的直连/中继链路传输终端、文件和工作区状态；因此下面区分“连接架构参考”和“屏幕采集/编码参考”。

资料优先使用项目官方 README、官方架构文档、源码仓库和 GitHub API。活跃度数字是 GitHub API 快照，时间为 2026-09-13（UTC 时间可能显示为 2026-09-14）；star、fork、推送时间会变化，不能替代代码审查。

本轮范围收窄：Codux 的主场景是跨网共享终端 agent 会话。iroh 保持唯一数据面；优先借鉴终端弱网体验和会话恢复，图形远控项目只作为条件性参考。

## 结论先行

| 优先级 | 项目 | 与 Codux 的关系 | 最值得借鉴的部分 | 不建议直接照搬 |
| --- | --- | --- | --- | --- |
| P0 | [RustDesk](https://github.com/rustdesk/rustdesk) + [rustdesk-server](https://github.com/rustdesk/rustdesk-server) | 与 UU 远程最接近的完整远程控制产品 | ID/rendezvous 与 relay 分工、直连优先、服务端自托管、配对与连接诊断 | RustDesk 自有协议和会话模型；Codux 保留 iroh |
| P0 | [Mosh](https://github.com/mobile-shell/mosh) | 与 Codux 同为弱网终端体验问题 | 间歇连接、网络漫游、预测回显、按键行编辑、UDP 会话保持 | Mosh 的预测回显只适合可安全预测的普通 shell 输入；TUI 不应盲目本地回显 |
| P0 | [NetBird](https://github.com/netbirdio/netbird) | 纯网络层参考，解决跨 NAT、候选地址、relay fallback | Management/Signal/Relay 分层、ICE/STUN 候选发现、WireGuard 加密覆盖网 | 不把 Codux 改造成 VPN；不为终端产品引入完整控制面 |
| P1 | [Sunshine](https://github.com/LizardByte/Sunshine) + [Moonlight PC](https://github.com/moonlight-stream/moonlight-qt) | Parsec 类开源替代；适合未来图形/交互式桌面流 | host/client 解耦、硬件编码、低延迟媒体管线、Web UI 配对、按能力协商 codec | Codux 当前是终端/状态控制，不应为借鉴视频经验而替换控制链路 |
| P1 | [Eternal Terminal](https://github.com/MisterTea/EternalTerminal) + [tmux](https://github.com/tmux/tmux) | 终端会话持久化参考 | 自动重连、断线不打断会话、detach/reattach、host 端会话 owner | Eternal Terminal 依赖 TCP 开放端口；只借鉴会话语义，不引入其网络前提 |
| P1 | [MeshCentral](https://github.com/Ylianst/MeshCentral) | 多设备远程管理和会话恢复参考 | 常驻 agent、设备在线状态、浏览器控制、terminal/file/desktop 一体化、TLS/WebRTC/relay 文档化 | 中央服务器模型与 Codux 本地优先模型不同 |
| P1 | [Apache Guacamole](https://github.com/apache/guacamole-client) + [guacamole-server](https://github.com/apache/guacamole-server) | 浏览器控制面和协议适配参考 | `guacd` 独立代理、统一文本协议、RDP/SSH/VNC 适配、WebSocket/HTML5 入口 | Guacamole 的网关协议不是 Codux 的领域协议 |
| P2 | [TigerVNC](https://github.com/TigerVNC/tigervnc)、[wayvnc](https://github.com/any1/wayvnc)、[KDE krfb](https://github.com/KDE/krfb) | 图形采集、输入注入、Wayland/X11 兼容性参考 | 编码、RFB、Wayland 输入设备、TLS/认证边界 | VNC 本身不解决现代 NAT 穿透、设备配对或 Codux 会话语义 |
| 观察 | [Remotely](https://github.com/immense/Remotely) | Web 运维和脚本自动化参考 | .NET/Blazor/SignalR、组织/设备模型、移动端远控交互 | GitHub API 显示最近推送为 2024-12-17，当前活跃度低于上述项目 |
| 排除 | [HopToDesk](https://github.com/hoptodesk/hoptodesk) | 产品定位相似 | 目前无法从仓库验证可复用实现 | API 根目录当前仅列出 `LICENSE`、`chocolatey`、`res`，无 README 或应用源码，不能据此评估连接实现 |

## 项目笔记

### 0. Mosh、Eternal Terminal、tmux：终端场景优先

[Mosh](https://github.com/mobile-shell/mosh) 的官方 README 直接针对间歇连接、网络漫游、预测回显和按键行编辑。它是 Codux 弱网“输入要跟手”的最佳体验参考：连接暂时丢失时保留会话，网络地址变化后继续使用，并对可以安全预测的普通 shell 输入减少等待。

Codux 不应直接移植 Mosh 的 UDP 或 SSH。iroh 已经提供加密 QUIC、路径切换和 relay；可借鉴的是输入策略：普通 shell 可研究有限预测回显，TUI/备用屏幕必须等待 host authoritative output，避免本地假状态覆盖 screen baseline。

[Eternal Terminal](https://github.com/MisterTea/EternalTerminal) 的核心承诺是“自动重连且不中断会话”，并提供独立的 protocol、terminal multiplexer 和 IPC 设计文档。它使用 TCP 且要求服务端开放端口，所以只借鉴 session owner、重连后的 stream reattach 和终端状态恢复，不借鉴网络部署方式。

[tmux](https://github.com/tmux/tmux) 只解决 host 端 detach/reattach：终端脱离控制端后继续运行，之后重新附着。Codux 已有 agent/session store，重点是验证“连接断开不等于 agent session 结束”，并让恢复后的首个 baseline 成为唯一权威画面。

### 1. RustDesk：最接近 UU 远程的完整样本

RustDesk 官方 README 将项目定义为 Rust 编写的远程桌面方案，强调无需复杂配置、用户掌控数据，并支持使用官方 rendezvous/relay、自己部署，或编写自己的服务端。[官方 README](https://github.com/rustdesk/rustdesk/blob/master/README.md)

其服务端仓库把职责拆成三个可执行文件：`hbbs` 是 ID/Rendezvous server，`hbbr` 是 relay server，`rustdesk-utils` 提供 CLI 工具。`hbbs`、`hbbr` 支持命令行、环境变量和配置文件；`ALWAYS_USE_RELAY` 可以显式关闭直连。这个拆分适合作为 Codux 连接诊断和服务端部署文档的参照。[官方 server README](https://github.com/rustdesk/rustdesk-server/blob/master/README.md)

可借鉴：

- 把“发现对端”“尝试直连”“中继转发”作为不同阶段和不同诊断结果展示。用户知道当前卡在发现、打洞、握手还是 relay。
- 保留多个候选地址，让直连失败后自动进入 relay；连接详情显示当前路径，避免把“已建立 QUIC”误报成“应用可用”。
- 配对、服务端地址、密钥和 relay 选择都做成可操作的设置，而不是把失败归因于一个笼统的“网络错误”。
- 自托管部署提供独立的 relay/rendezvous 文档和可观察配置；Codux 可继续用 iroh，只借鉴可见性和运维入口。

风险：RustDesk 主仓库和服务端均为 AGPL-3.0；RustDesk Server Pro 另有商业版本。复用代码、协议或资源前必须逐文件核对许可证和依赖。[主仓库 LICENCE](https://github.com/rustdesk/rustdesk/blob/master/LICENCE) · [server LICENSE](https://github.com/rustdesk/rustdesk-server/blob/master/LICENSE)

GitHub API 快照：主仓库约 123k stars/19k forks，server 约 10.4k/2.6k，均未归档且持续推送，成熟度高；数字仅作筛选信号。

### 2. NetBird：连接网络层最值得借鉴

NetBird 不是远程桌面，而是 WireGuard 覆盖网络。官方 README 明确描述：每台机器运行 agent；Management Service 保存网络状态并下发更新；agent 使用 ICE 和 STUN 发现 P2P 候选；Signal Service 交换端到端加密的候选信息；NAT 穿透失败时回退 Relay Service，并通过 relay 建立安全 WireGuard 隧道。[官方 README 的内部架构说明](https://github.com/netbirdio/netbird/blob/main/README.md)

这正好对应 Codux 的“relay 不应成为直连前置条件”和“候选地址会变化”问题。建议借鉴边界：

- 把候选集合、候选来源、最近成功路径和当前选中路径视作连接状态的一部分，并在 UI/日志中可见。
- 让直接路径和 relay 路径有独立健康度与失败原因；一次 relay 失败不能抹掉仍可用的 direct candidate。
- 发生网络切换时刷新候选并立即触发一次恢复，而不是等待旧退避定时器自然到期。
- 将控制面、候选交换和数据面分开思考；Codux 不需要引入 NetBird 的 Management/Signal 服务，只需继续让 iroh 拥有数据面。

NetBird README 带有 BSD-3 badge，但 GitHub API 的自动识别结果为 `NOASSERTION`；若复用代码或依赖，必须以仓库 `LICENSE` 和文件级声明为准，不按 badge 推断整体许可证。[LICENSE](https://github.com/netbirdio/netbird/blob/main/LICENSE)

### 3. Sunshine + Moonlight：低延迟图形流和 Parsec 类体验

Sunshine 官方 README 将其定义为面向 Moonlight 的自托管 game-stream host，支持 AMD、Intel、NVIDIA 硬件编码，也可软件编码；提供 Web UI 做配置和客户端配对，且可从本机或移动设备完成配对。[Sunshine README](https://github.com/LizardByte/Sunshine/blob/master/README.md)

Moonlight PC 是对应的开源 PC client。官方 README 列出的能力包括硬件加速解码、H.264/HEVC/AV1、HDR、7.1 音频、多点触控、最多 16 个手柄，以及“游戏 pointer capture”和“remote desktop direct mouse control”两种输入方式。[Moonlight README](https://github.com/moonlight-stream/moonlight-qt/blob/master/README.md)

可借鉴：

- host/client 能力协商：编码器、解码器、分辨率、HDR、输入能力在连接前或连接早期明确协商，失败时给出可解释的降级原因。
- 低延迟体验不只靠协议：采集、编码、传输、解码、输入回传分别测量，UI 显示当前模式和关键延迟。
- 配对和日常连接分离；Web UI、短流程配对、移动端配对降低首次连接成本。
- 为交互式内容保留独立媒体流，不让大块画面数据阻塞控制消息。

Sunshine 和 Moonlight 均为 GPL-3.0，项目很活跃（API 快照约 41k/18.6k stars，均未归档）。它们对 Codux 的终端协议不是直接替代品；只有 Codux 未来需要远程桌面画面、鼠标或高帧率预览时，才值得进一步研究其编码和输入实现。[Sunshine LICENSE](https://github.com/LizardByte/Sunshine/blob/master/LICENSE) · [Moonlight LICENSE](https://github.com/moonlight-stream/moonlight-qt/blob/master/LICENSE)

### 4. MeshCentral：多设备管理、常驻 agent 和浏览器入口

MeshCentral 官方 README 描述完整的 Web 计算机管理站点：自建 Web server，安装 agent 后设备自动出现在设备组中，并提供 Web remote desktop、terminal 和 file management。[官方 README](https://github.com/Ylianst/MeshCentral/blob/master/readme.md)

官方 README 还链接了 Design and Architecture Guide，覆盖设计总览、依赖、证书、TLS、agent-to-server handshake、browser-to-agent relay、WebRTC 和 messenger service。[架构指南](https://meshcentral.com/docs/MeshCentral2DesignArchitecture.pdf)

可借鉴：

- 设备列表以在线、离线、最近在线、能力和版本为一等状态；用户先选设备，再进入终端或文件等工作域。
- agent 常驻，控制端重连后恢复既有设备和会话，而不是每次重新配对、重新创建工作对象。
- 将浏览器/控制端到 agent 的 relay、WebRTC、TLS 和认证分别记录，连接失败时给出阶段化诊断。
- 远程桌面、terminal、文件传输共享设备身份，但使用独立通道和权限；这与 Codux 的 control/terminal/blob/web-tunnel 分层方向一致。

MeshCentral 为 Apache-2.0；API 快照约 7.2k stars/987 forks，未归档且持续推送。它依赖中央服务器和数据库，适合借鉴设备管理和可观察性，不适合直接替换 Codux 的本地优先、P2P/relay 数据面。[LICENSE](https://github.com/Ylianst/MeshCentral/blob/master/LICENSE)

### 5. Apache Guacamole：协议适配和浏览器控制面

`guacamole-client` 是 Apache Guacamole 的 HTML5 Web application、官方扩展和 JavaScript library；官方 server README 说明 `guacd` 是 proxy daemon，`libguac` 和协议支持库组成 server。由于浏览器 JavaScript 不适合直接高效处理 VNC 等二进制协议，Guacamole 定义了便于 JavaScript 处理的文本协议，由 `guacd` 在任意后端协议与 Guacamole 协议间转换。[client README](https://github.com/apache/guacamole-client/blob/main/README) · [server README](https://github.com/apache/guacamole-server/blob/main/README)

官方 server README 列出 RDP、SSH、Telnet、VNC，以及 VNC/RDP 的 SFTP 文件传输和 WebP/FFmpeg 等可选能力。可借鉴：

- 将浏览器连接、统一会话协议、后端协议代理拆层；任一后端协议异常不会污染控制面状态。
- 大数据通道使用明确的 framing、背压和代理边界；控制消息和文件/画面流不共用无界队列。
- 把协议能力列表和部署依赖做成可检查的诊断结果，避免“页面能打开但目标协议不可用”。

Guacamole client/server 均 Apache-2.0，server API 快照约 4k stars，client 约 1.7k，未归档并持续推送。它适合参考 Codux Web Tunnel、远程终端网关和能力诊断，不应替换 Codux 的加密 iroh 链路。

### 6. TigerVNC、wayvnc、KRFB：图形后端的窄领域参考

- [TigerVNC README](https://github.com/TigerVNC/tigervnc/blob/master/README.rst) 将 VNC 定义为跨平台远程显示系统，说明其高性能版本基于 RealVNC 4/X.org，并用 libjpeg-turbo 加速 Tight encoding；同时包含 `vncviewer`、X11 `Xvnc` 和 Wayland `w0vncserver`。适合研究编码、帧更新和 X11/Wayland server 拆分。仓库 GPL-2.0，约 7.5k stars/1.2k forks，持续维护；Windows `winvnc` 在 README 中明确标为 unmaintained，不能把整仓库的成熟度外推到每个平台。[README](https://github.com/TigerVNC/tigervnc/blob/master/README.rst) · [LICENSE](https://github.com/TigerVNC/tigervnc/blob/master/LICENCE.TXT)
- [wayvnc README](https://github.com/any1/wayvnc/blob/master/README.md) 是 wlroots Wayland compositor 的 VNC server，连接已运行的 Wayland session，创建 virtual input devices，通过 RFB 暴露单一 display，也支持没有物理显示的 headless session。README 要求公网使用认证，并记录 VeNCrypt/TLS、用户名密码和配置文件。它是 Codux 未来支持 Wayland 图形采集、输入和 TLS 边界的好小样本；ISC License，约 1.8k stars，持续维护。[README](https://github.com/any1/wayvnc/blob/master/README.md) · [LICENSE metadata](https://api.github.com/repos/any1/wayvnc)
- [KDE krfb README](https://github.com/KDE/krfb/blob/master/README) 是小型 RFB/VNC server，目标是共享当前 X11 session，而不是创建新 X11 session，后端使用 libvncserver。GitHub API license 字段为 `NOASSERTION`，仓库同时有 `COPYING`/`COPYING.DOC`/`COPYING.LIB`，且 `COPYING` 为 GPL-2.0 文本；复用前必须按文件核对 KDE 许可。GitHub 仓库约 86 stars，虽推送活跃但不宜把 GitHub 数字当作其在 KDE 生态中的完整成熟度。[README](https://github.com/KDE/krfb/blob/master/README) · [COPYING](https://github.com/KDE/krfb/blob/master/COPYING)

这三个项目都不负责现代互联网下的 rendezvous、NAT traversal、设备配对或 Codux 的长时终端会话恢复；它们只应进入“图形后端研究”清单。

### 7. Remotely：Web 运维和脚本交互样本，但活跃度偏低

Remotely 官方 README 将其定义为基于 .NET、Blazor 和 SignalR Core 的 remote control 与 remote scripting solution，提供 Docker quickstart、组织/用户/设备模型和移动端基本点击交互说明。[官方 README](https://github.com/immense/Remotely/blob/master/README.md)

可借鉴其 Web 控制面、SignalR 实时消息、组织和设备分组、反向代理/HTTPS 配置文档，以及移动端对点击、长按、拖动的明确交互映射。GitHub API 显示仓库未归档但最近推送时间为 2024-12-17，约 5.1k stars/1.6k forks；参考架构前先验证依赖和当前安全状态。仓库标记 GPL-3.0。[LICENSE](https://github.com/immense/Remotely/blob/master/LICENSE)

### 8. HopToDesk：定位相似，但当前仓库不可用于实现调研

GitHub API 显示 `hoptodesk/hoptodesk` 未归档，默认分支为 `main`，许可证识别为 AGPL-3.0；但 [API 根目录](https://api.github.com/repos/hoptodesk/hoptodesk/contents?ref=main) 当前仅返回 `LICENSE`、`chocolatey`、`res`，没有 README 或应用源代码，star/fork 均为 0。不能据此断言其产品实现质量，也不能把它当作 RustDesk 的可验证 fork。若后续仓库补回源码，再单独复查。

## Codux 当前架构映射

以下建议以 Codux 已有实现为前提，目标是提升连接体验，不是替换 iroh：

| 外部经验 | Codux 已有对应物 | 建议优先级与边界 |
| --- | --- | --- |
| RustDesk 的 rendezvous/relay 分层、NetBird 的候选集合和路径状态 | `iroh` endpoint、relay/direct candidate、`direct_addrs`、host transport refresh | P0：继续以 iroh 为唯一数据面；完善“候选来源、尝试顺序、当前路径、失败阶段、最近成功地址”的统一诊断模型。不要再增加一套自有穿透协议。 |
| RustDesk/NetBird 的直连优先和 relay fallback | `crates/codux-remote-transport/src/iroh_link.rs` 的 `DialSchedule`、relay-less dial、LAN mDNS、last-known-good 地址 | P0：保留现有直连/中继策略，重点验证候选变化、VPN/DNS、网络切换和 relay 不可用时的用户可见状态。 |
| 低延迟产品的阶段化指标 | `transportPing`/`transportPong`、`latency:rtt=...;path=...`、移动端 latency UI | P1：把 RTT 与路径、连接阶段关联；后续可增加抖动/丢包或最近样本，但先不把单次 ping 当作“业务可用”证明。连接 ready 仍需 host.info/hello 和应用 ACK。 |
| 可靠重连与可恢复状态 | `apps/mobile/lib/services/reconnect_policy.dart`、`connection.dart` 的 generation 去重、指数退避、exhausted/resume | P1：沿用单一 `ReconnectPolicy`；区分 transport open、protocol ready、host responsive、session restored。不要让多个观察者各自推进 attempt。 |
| MeshCentral 的 agent 会话恢复、Guacamole 的代理分层 | Codux host/agent、control stream、terminal stream、blob、Web Tunnel | P1：继续拆分控制、终端、文件和 Web Tunnel；每条通道报告自己的 ready/failure，避免大文件或预览流阻塞 control/terminal。 |
| 会话同步而非“重新打开页面” | `host.info.transports`、资源订阅、project/worktree snapshot、sync generation | P1：把 host snapshot、project/resource snapshot 和 connection generation 作为恢复边界；旧连接完成的结果不得覆盖新 generation，只有仍当前且已发布的 snapshot 才能清理 retry。 |
| 终端可靠显示 | `terminal` baseline chunking、`screenData`、`outputSeq`、`terminal.output.ack`、viewport `generation`/ownership | P1：继续保持 baseline 与 live output 分层；ACK 只证明接收，baseline 完成后才报告恢复；丢帧、staleOutput、baselineFailed 都走同一补偿路径。不要引入第二个终端“真相源”。 |
| 移动端后台连接体验 | Android `BackgroundKeepAlive` foreground service；iOS 明确无法保持 UDP 进程 | P1：明确展示“后台保持连接”开关、系统授权结果和断线后的恢复状态；保持现有平台边界，不承诺 iOS 持久在线。 |
| Sunshine/Moonlight 的媒体优化 | 当前 Codux 主要传终端和结构化状态，不是视频流 | P2/条件触发：只有产品明确增加远程桌面画面/鼠标输入，才研究硬件编码、codec 协商、自适应分辨率和独立媒体流；不因 UU 远程体验而改造现有终端协议。 |
| Guacamole/RustDesk 的首次连接 UX | Codux 已有 ticket/QR 配对、relay preset、主机端安装和 Web Tunnel | P2：把首次配对、连接测试、候选诊断、权限/设备名和重试入口收成短流程；协议字段保持向后兼容，未知 capability 继续可忽略。 |

## 后台与前台恢复预估

这是工程目标，不是当前真机统计；需要在 Android 原生机、激进省电厂商和 iOS 上分别验证。

| 场景 | 当前行为 | 吸收终端项目经验后的预期 |
| --- | --- | --- |
| Android，前台服务获准，网络未变 | iroh 链路通常继续存活；回前台仍会重新请求列表、host.info，并为可见 terminal 请求 baseline | 保留缓存 screen，只做轻量 ping/状态确认；无 sequence gap 时不请求 baseline，回前台接近即时可用 |
| Android，短暂断网或 Wi-Fi/蜂窝切换 | route refresh 触发恢复；可能经历一次 transport reconnect 和 baseline | 用 iroh path 事件加 resume cursor；优先恢复同一 session，通常只显示短暂恢复状态，不刷新页面 |
| Android，前台服务被拒或厂商杀进程 | 连接断开，回前台重连 | 无法保证链路不断；缓存先显示，连接恢复后只补一次 delta/baseline，避免用户重新打开项目 |
| iOS 进入后台 | 进程可能被系统挂起，UDP/QUIC 不能保持 | 不承诺持久在线；回前台立即显示缓存，后台重连完成后原子收敛 screen。页面无需 reload |
| 高 RTT、输出拥塞 | shell 输入和 TUI 都受网络往返影响 | 普通 shell 可有限预测回显；TUI 不预测，只提高 input/resize 优先级。感知延迟下降，TUI 仍受 RTT 下限约束 |

粗略目标：Android 前台服务正常时，10 分钟内回前台不发生 transport reconnect；发生短断线时，session 不重建、页面不 reload；iOS 只保证“缓存先恢复、网络后收敛”。具体秒数取决于 relay RTT、系统省电策略和 host 是否仍在线。

## 建议验证顺序

1. **弱网终端输入**：高 RTT、短暂丢包、网络切换下，普通 shell 输入保持可用；TUI 不产生错误预测回显，恢复后 baseline 覆盖到最新 screen。
2. **断线会话恢复**：连接断开时 agent session 继续在 host 运行；重连后用 snapshot/baseline/ACK 恢复，旧 generation 的迟到结果不得覆盖新状态。
3. **连接诊断闭环**：记录并展示 `resolving candidate`、`direct dial`、`relay dial`、`QUIC open`、`protocol ready`、`host responsive`、`session restored`；每阶段有明确 timeout、错误类别和当前 generation。
4. **网络切换回归**：Wi-Fi、蜂窝、VPN/SD-WAN、relay 不可用、host 地址变化、direct 到 relay 和 relay 到 direct；验证 `direct_addrs`、host transport refresh、last-known-good 不会相互覆盖。
5. **后台与电量**：Android 前台服务启动/拒绝/停止和进程恢复；验证 iOS 进入后台后的明确断线与回前台重连提示。
6. **仅在图形需求落地后**：再做 Sunshine/Moonlight 风格的采集、编码、输入和媒体 QoE 基准；TigerVNC/wayvnc/KRFB 只作为后端实现对照。

## 许可证与复用边界

项目许可证仅表示仓库元数据或根许可证的初步结果。RustDesk/HopToDesk/Remotely 的 copyleft 约束与网络交互有关；Sunshine/Moonlight/TigerVNC/KRFB 也含第三方组件和文件级声明；Guacamole、MeshCentral、NetBird、wayvnc 的仓库可能包含不同许可文件。当前结论是“借鉴架构和产品流程”，不是批准复制代码。真正复用前需要锁定 commit、执行许可证扫描、核对依赖传递许可，并记录到 Codux 的依赖清单。
