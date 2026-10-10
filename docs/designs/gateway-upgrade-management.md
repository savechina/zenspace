# Gateway Upgrade Management — 设计记录

**Date**: 2026-10-10 · **Status**: ACCEPTED (owner decisions recorded below) · **Tasks**: 005 tasks.md Phase 30 (T201-T206)
**Problem**: `brew upgrade zenspace` 替换二进制后，旧版本 `zen serve` gateway daemon 继续运行（launchd KeepAlive 只在进程死亡时重生；brew 不杀进程）。新 CLI 与旧 daemon 之间：协议 minor 未变 ⇒ 静默互通旧行为（零信号）；协议 minor 已变 ⇒ 握手 `-32001 version-mismatch` 但 recovery 无自动化。无 `zen serve restart`，无版本对比可见性。

## 1. 外部参考（primary sources）

### 1.1 Codex `app-server-daemon`（github.com/openai/codex, codex-rs/app-server-daemon/README.md）

| 机制 | codex 做法 |
|---|---|
| daemon 二进制与 CLI 解耦 | daemon 装于 `CODEX_HOME/packages/app-server-daemon/current/bin/codex`；lifecycle 命令用选定 daemon 包，与调用方 CLI 版本无关 |
| 独立 updater 进程 | pidfile-backed（`app-server-updater.pid`）；启动 5min 首查、此后每小时；`settings.json: updater.{autoUpdateEnabled, updateIntervalMinutes}` |
| server-first 重启次序 | 二进制内容变化时**先重启 app-server 再替换 updater 自身映像** |
| 优雅关停 | `shutdownGraceSeconds` 默认 60（clamp 0..=300）：优雅退出请求 → 超时强杀 |
| 双版本可见性 | 每个 lifecycle 命令输出单 JSON：`{backend, socket, local CLI version, running app-server version}` |
| lifecycle 串行锁 | `daemon.lock` 按 CODEX_HOME 串行化所有变更型命令 |
| 幂等 start + 握手即就绪 | start 在 initialize 握手可应答后返回 |
| 握手携带版本 | `InitializeResponse{userAgent(含版本), codexHome, platformFamily, platformOs}` |
| 降级不硬失败 | TUI 附着 daemon 失败 ⇒ 起嵌入式 server 继续 |
| 带外更新的诚实限制 | updater 缺席时无法推断运行中 server 的原二进制身份 ⇒ 文档明示 `daemon restart` 兜底 |
| pin 语义 | 显式 release 清除 latest-channel；updater 持安装锁时重查 pin |

### 1.2 pi（badlogic/pi-mono）

**无 daemon**：每次调用即进程，cwd 即工作区。升级天然无版本偏差。教训（反向）：**能非常驻就不常驻**——zen 的 TUI InApp scheduler、隐式 daemon spawn、CLI 本地执行已是该原则的体现；daemon 仅保留给必须常驻的职责（memvid 单写者、scheduler lease、qqbot channel）。

## 2. zen 现状盘点（file:line，2026-10-10 核实）

**已有**（不重复建设）：
- 握手协商：`SERVER_PROTOCOL_VERSION="1.0"`，`negotiate_version()` = MAJOR 相等 && client minor ≤ server minor，违则 `-32001{serverVersion, reason, recovery}`（`zen-gateway/src/protocol/mod.rs:34-119`）
- 握手 hello 携带 `version: CARGO_PKG_VERSION`（`protocol/mod.rs:182`）；HTTP carrier serverInfo 同（`transport/http.rs:567`）
- 幂等 start（`gateway_is_live()` 预探测，pid 不触碰）+ `StartupLock`（socket bind 前独占）+ readiness=握手（`daemon.rs`）
- SIGTERM drain ≤10s 后带审计取消，exit 0（004 US5）
- launchd LaunchAgent：KeepAlive=true + ThrottleInterval=60（`render_plist`）

**缺口**：
1. 版本信息在线上存在但**无消费者**：`zen serve status` 不显示 daemon 版本；doctor daemon 探针只查存活
2. 无 `zen serve restart`
3. brew upgrade 后旧 daemon 永续（协议未 bump 时零信号）
4. `daemon.pid` 记录 `{pid, start-token}` 无 version/protocol 字段
5. 旧 daemon + 新 CLI 施加 additive migration 的容忍度未审计（`SELECT *`/位置映射风险）

## 3. Owner 决策（2026-10-10）

- **D-A**: `upgrade_policy` 默认 **warn**（gate-closed 原则；drain 会取消在途 turn，不默认替用户做）。`auto-restart` 为显式 opt-in。
- **D-B**: G1-G5 全做（本波，fix batch 后串行）；**G6（codex 式独立 updater 循环 + daemon 包解耦）记录推迟**——brew/cargo 是安装渠道，自更新与包管理器冲突（Constitution XI reuse/minimal）。**Revisit trigger**: 分发渠道超出 brew/cargo，或出现 remote-control 类远程管理需求。
- **D-C**: daemon 二进制与 CLI 同源（单二进制，brew 单渠道）——刻意不同于 codex 的包解耦；等价保障 = G3 检测 + G2 一键 drain-restart。

## 4. 设计（G1-G5 → T201-T205）

### G1 版本可见性（additive）
- `daemon.pid` 记录增加 `version`（CARGO_PKG_VERSION）+ `protocol`（SERVER_PROTOCOL_VERSION）字段；reader 对缺字段容错（= legacy daemon ⇒ 视为"版本未知、早于当前"）。
- `zen serve status` 并排显示：binary version vs running daemon version + `STALE` 标记（daemon version ≠ binary version 时）；human/JSON 双输出（Principle IX）。

### G2 `zen serve restart`
- launchd 安装态 ⇒ `launchctl kickstart -k gui/{uid}/dev.zen.serve`（KeepAlive 重生并解析 PATH 上的新二进制）。
- 非 launchd ⇒ 优雅 stop（既有 SIGTERM drain 语义）+ `StartupLock` 下 start。
- 与既有 start/stop 相同的幂等与就绪语义（readiness=握手 + pid 匹配）。

### G3 stale-daemon 诊断三入口 + policy
- 入口：① `zen serve status`（G1 STALE 标记）；② `zen doctor` 第 9 探针 `daemon-version`（不符 ⇒ WARN + "run `zen serve restart`"；`--json` 机器可读）；③ TUI prewarm/`resolve_client` 收到 `-32001` 时透出 recovery 文案。
- 配置 `[gateway] upgrade_policy = "warn" | "auto-restart"`（env `ZEN_GATEWAY_UPGRADE_POLICY`，5-layer；非法值 ⇒ warn + tracing::warn，绝不静默改变行为——cron-timezone 纪律）。
- `auto-restart` 行为 = 经 G2 路径 drain-restart；安全性依据：drain 带审计取消在途 turn（既有）、scheduler lease 跨进程互斥（既有）、memvid replay checkpoint 幂等（既有）。
- 检测点仅限**显式/launchd daemon**；隐式 spawn 的 daemon 天然运行当前二进制，无需检测。

### G4 迁移竞态审计（证据，非猜测）
- 审计 zen-repo 全部 repository：无 `SELECT *`、无位置列映射（sqlx `query_as` 显式列名）⇒ additive 列对旧 daemon 进程安全；结论写入本文件 + 004 runbook。发现反例则升级为 blocker。

### G5 契约与文档
- 004 `contracts/00` additive 补充：pid 记录形状（含 version/protocol）、health/status 既有 hello version 字段的消费约定；registry 行数不变则无 MINOR bump，本变更本身若增 RPC 字段按 additive-MINOR 政策记录。
- spec/AGENTS.md：`zen serve restart` 命令行文档（含 scope logic 四件套）、Configuration Surface 增 `[gateway] upgrade_policy` 行、升级 runbook（brew upgrade ⇒ restart 的推荐流程）。

### G6（推迟，见 D-B）
codex 式 updater 循环 + 包解耦的完整机制已在 §1.1 记录，供 revisit 时直接取用。

## 5. 拒绝的备选

- **自动 kill 旧 daemon（无 policy 门）**：违反 gate-closed；drain 取消在途任务是用户可感知的副作用。
- **协议 bump 强制拒绝旧 CLI**：现有 negotiate_version 已是该语义（client minor > server minor 拒绝）；反方向（新 CLI vs 旧 daemon、同协议）靠 G3 诊断而非硬拒。
- **双 daemon 并存热迁移**：单写者契约（memvid RW、scheduler lease、StartupLock per ZEN_HOME）结构性排除；codex 亦无此机制。
- **updater 独立进程（G6）**：与 brew 渠道冲突，见 D-B。

## 6. 验收

- `zen serve status`（旧 daemon 运行中，新二进制调用）显示两个版本 + STALE；`zen doctor` WARN 且 exit 1；`zen serve restart` 后 STALE 消失、drain 审计行存在、launchd 态经 kickstart 重生。
- `upgrade_policy=auto-restart` 时检测点自动 drain-restart；非法值回落 warn 并有 warn 日志。
- 全部 additive：旧 pid 文件（无 version 字段）可读；协议无 breaking change；`bin/lint` + `bin/test` 绿。
