# CodeWave 技术方案

> 初稿：2026-08-30（决策定稿日）
> 现状同步：2026-09-13 品牌改名定稿后的持续同步，对应版本 **0.8.7**

**本文是双层文档，请按两层读**：

- **§0 决策记录（D1–D8）是历史锚点**，记录 2026-08-29/30 拍板时的原始结论，**不随实现演进而改写**。即使某条决策的执行形态已与当年设想不同（例如 D3 的前端栈、D7 的发布形态），决策史本身仍是判断「当初为什么这么定」的唯一依据；要改决策就改 §0，而不是改现状描述去迁就它。
- **§1–§11 是与当前源码对齐的实现现状**。凡本文描述的模块、工具名、事件键、IPC 命令、依赖版本，均以对应版本的仓库源码为准；引用其他文档一律走 markdown 链接。

---

## 0. 决策记录

| # | 决策点 | 结论 | 日期 |
|---|---|---|---|
| D1 | 桌面框架 | **Tauri 2**（tauri 2.11.x，已稳定） | 2026-08-29 |
| D2 | 后端路线 | **路线 A：纯 Rust 核心**（tokio 异步运行时，单进程，无 sidecar） | 2026-08-30 |
| D3 | 前端栈 | **Vue 3 + Naive UI + Pinia**（状态用 Pinia 分层） | 2026-08-30 |
| D4 | 命令安全策略 | **静态围栏 + 危险命令确认弹窗**（弹窗默认开启，设置可关） | 2026-08-30 |
| D5 | Git 集成 | **git2-rs**（rust-lang/git2-rs，libgit2 绑定）只读集成；P0 前移构建验证，功能 P1 | 2026-08-30 |
| D6 | 包标识与品牌 | bundle id `xyz.yangshifu.codewave`；数据目录 `~/.codewave/`；keyring 服务名 `codewave.yangshifu.xyz`。原值（2026-08-30 拍板）：`work.gitwave.studio` / `~/.wavestudio/` / `studio.gitwave.work`，2026-09-13 随品牌改名 CodeWave 变更（旧目录/钥匙串不自动迁移） | 2026-08-30 |
| D7 | 发布形态 | **私有项目起步**：不做签名/更新服务器，`tauri build` 产物自用；协议暂不设 | 2026-08-30 |
| D8 | 应用名 | **CodeWave** | 2026-08-29 |

---

## 目录

1. [目标与设计原则](#1-目标与设计原则)
2. [总体架构](#2-总体架构)
3. [技术栈与依赖清单](#3-技术栈与依赖清单)
4. [Rust 后端详细设计](#4-rust-后端详细设计)
5. [IPC 协议（Commands 与 Events）](#5-ipc-协议commands-与-events)
6. [前端设计](#6-前端设计)
7. [数据与配置布局](#7-数据与配置布局)
8. [安全模型](#8-安全模型)
9. [构建、CI 与发布](#9-构建ci-与发布)
10. [交付状态与验收](#10-交付状态与验收)
11. [风险清单与缓解](#11-风险清单与缓解)

---

# 1. 目标与设计原则

## 1.1 产品定位

CodeWave 是一个**桌面 AI 编程 Agent**：用户在项目上通过对话驱动 Agent 理解代码、编辑文件、执行命令、搜索内容、管理任务；支持 OpenAI 兼容 API 与 Anthropic API、MCP 扩展、Skills、子代理。

- **项目 = 名称 + 单一主目录**：项目托管数据（配置、temps/logs/memory/skills/tasks）存于主目录下的 `.codewave/`；**临时会话**免目录直接开聊，工作区取全局数据目录（[session-semantics-and-ui-batch](./session-semantics-and-ui-batch.md)）。
- 数据、配置、会话均落在用户本机的 `~/.codewave/`；API key 入系统钥匙串，不落明文（[oss-prep-batch](./oss-prep-batch.md)）。

## 1.2 设计原则（8 条，指导所有实现）

1. **host 单向依赖**：只有 `lib.rs` 与 `host/` 允许 `use tauri::*`；`core/tools/safety/provider` 等纯 Rust 模块不感知框架，可独立单测。
2. **编排与算法分离**：工具的「执行编排」（依赖全局状态的薄壳）与「纯算法核心」（输入→输出的函数）分层，算法层单测覆盖。
3. **双通道工具结果**：前端收完整 JSON；模型上下文收压缩版（head/tail 截断、配额上限）。read 类结果不压缩（保留行号供 edit 定位）。**写入后检查的结论必须走 `outcome.data.check`**——`compact_for_model` 只读 `data`，走 `warnings` 模型读不到（[post-write-check-plan](./post-write-check-plan.md)）。
4. **Cache-first 上下文**：所有进入请求前缀的内容（系统提示词、工具 schema）必须**字节级稳定**（排序确定、序列化确定、瞬态内容后置），以命中 provider prompt cache；`core/prompt.rs` 的六层组装层序固定即为此服务（[prompt-caching-hardening](./prompt-caching-hardening.md)）。
5. **一切皆文件、原子写**：无数据库；JSON + 分段 JSONL；临时文件 + rename；Windows 加 `.bak` 回滚（实现见 `src-tauri/src/util/atomic.rs`）。
6. **防御性历史修复**：任何进入持久化/回放路径的消息数组，必经 sanitize → trim → repair 三段管线，保证 tool_use/tool_result 配对不变式，杜绝「会话毒化成永久 400」。
7. **围栏优先、弹窗兜底**：命令安全先走零交互的静态围栏；仅高危/越界场景触发确认弹窗（D4 决策）。
8. **宽容输入、严格输出**：解析模型工具调用参数时宽容（未知字段→warnings、类型错误自动修复）；写入磁盘与发给模型的内容严格校验。

---

# 2. 总体架构

## 2.1 进程模型

**单进程**：Tauri 2 主进程（Rust）内含全部 Agent 逻辑；WebView 只承载 UI。无 sidecar（D2）。

```
┌──────────────────────── CodeWave.app（单进程）────────────────────────┐
│                                                                         │
│  WebView（React 19 + antd 6 + zustand）                                 │
│  ├─ AppShell（三栏）  ChatMessages  Composer  ToolCallCard  Settings    │
│  │        ▲ invoke(command)                ▲ Channel（高频流）+ emit（低频）│
│  └────────┴────────────────────────────────┴──────────────────────────┐ │
│                                                        IPC（serde JSON）│ │
│  Rust 后端                                                              │ │
│  ┌─ host/ ── 唯一 import tauri：commands/、events(EventSink)、keyring ─┐  │ │
│  │                                                                    │  │ │
│  │  core/（agent 主循环、config、context、prompt、projects、quota、     │  │ │
│  │         sessions、scheduler、stats、openers、title…）              │  │ │
│  │  provider/（三协议适配、多 key 池、代理/SSRF、retry、sse）           │  │ │
│  │  tools/（registry 单一分发入口 + 各工具：算法层/编排层分离）        │  │ │
│  │  safety/（围栏 L0–L3、路径边界、ApprovalGate）                     │  │ │
│  │  git/（git2-rs：status/diff/log 只读）  agents/ mcp/ skills/        │  │ │
│  │  memory/ util/                                                    │  │ │
│  └────────────────────────────────────────────────────────────────────┘  │ │
│   ~/.codewave/（全局）+ <项目主目录>/.codewave/（项目作用域）             │ │
└─────────────────────────────────────────────────────────────────────────┘
```

**已删除的模块**：`lsp/` 目录已随「六语言 LSP 语义校验」整体删除（结论进不了模型上下文，见 [post-write-check-plan](./post-write-check-plan.md)），仅余 4 处注释留痕（`Cargo.toml` / `core/config.rs` / `host/commands/project.rs` / `tools/validation.rs`）。

## 2.2 模块依赖规则

顶层模块（`src-tauri/src/lib.rs`）：`agents` / `core` / `git` / `host` / `mcp` / `memory` / `provider` / `safety` / `skills` / `tools` / `util`。

```
host ──▶ core ──▶ provider
  │        │  └──▶ tools ──▶ safety
  │        └──▶ sessions / context / prompt / projects / quota（core 子模块）
  └──▶ mcp / skills / memory / agents / stats / git / util
禁止：core/tools/safety/provider 反向依赖 host；tools 之间禁止横向调用（经 registry）。
```

---

# 3. 技术栈与依赖清单

> 版本为现状核实值，与 `ui/package.json`、`src-tauri/Cargo.toml` 对齐。

## 3.1 框架与工具链

| 项 | 选型 | 版本 |
|---|---|---|
| 桌面框架 | tauri + tauri-build + tauri-cli | 2.x（`tauri = { features = ["tray-icon"] }`） |
| 前端 | React + Vite + TypeScript | react ^19.3.0 / Vite ^7.0.0 / TS ^6.0.3 |
| UI 组件库 | **antd 6**（+ `@ant-design/icons`） | ^6.6.3 |
| 状态管理 | **zustand**（immer 中间件承载流式高频更新） | ^5.0.0 |
| 包管理 | pnpm（前端）、cargo（Rust） | — |
| Rust 工具链 | edition **2024**，MSRV **1.98** | — |
| Tauri CLI | `pnpm tauri dev` / `pnpm tauri build`（本机无 `cargo-tauri`，须在仓库根执行） | — |

> **D3 的执行结果与决策原文不同**：决策写的是 Vue 3 + Naive UI + Pinia，实际落地为 React + antd + zustand。按 §0 的定位，此处只记录落地形态，不改写决策原文。

## 3.2 Rust crate 清单

| crate | 用途 | 备注 |
|---|---|---|
| tokio（full） | 异步运行时 | Semaphore=工具并发、mpsc=注入队列 |
| tokio-util | CancellationToken | 取消树：run → 工具批 → 子代理 |
| futures / async-trait | 异步抽象 | Provider trait 与 Tool trait 的 async 形态 |
| reqwest 0.13（json, socks, stream, gzip, brotli） | 全部 HTTP | 代理感知 Client 池；SOCKS5 支持 |
| serde / serde_json / serde_ignored / serde_yaml | 序列化 | serde_ignored 收集未知字段；frontmatter 解析 |
| git2 0.21（`default-features = false`） | git 只读（status/diff/log） | D5 决策；**不启用 ssh feature**（无远程操作需求，避免 libssh2 构建负担） |
| tree-sitter 0.27 + tree-sitter-bash 0.25 + **tree-sitter-powershell 0.26** | 命令 AST 解析 | 安全围栏（§8）；PowerShell 与 POSIX 两侧并行维护（[fence-hardening-and-powershell-ast](./fence-hardening-and-powershell-ast.md)） |
| grep-regex + grep-searcher + ignore | grep 工具引擎 | ripgrep 官方库 crate：内嵌同源引擎，纯 Rust，无需捆绑 rg 二进制 |
| similar | diff 计算 | edit 卡片 / ChangesPanel |
| umya-spreadsheet 3 / docx-rs 0.4 / zip 8 / quick-xml 0.41 | Office 读写与**保真修改** | 保真路径 = 解包 → 只替换目标内部文件 → 其余原样搬运重打包，**绝不整份解析重建**（重建会丢图表、数据透视表、迷你图） |
| pdf-extract 0.12（+ 前端 pdfjs-dist 5.4.149） | PDF 读写 | pdf-extract 有已知崩溃记录，调用处**必须**做崩溃隔离 |
| dom_smoothie 0.18 | 网页正文抽取 | web_fetch 的 Readability 实现 |
| rmcp 3.1.4 + process-wrap 10 | MCP 客户端（stdio + streamable-http）+ 子进程树回收 | Job Object / 进程组防孤儿 |
| cron 0.17 | 计划任务表达式校验 | 调度由**自写 tick** 驱动，不引入 tokio-cron-scheduler（该依赖已移除） |
| keyring 3（apple-native / windows-native / sync-secret-service） | API key 存系统钥匙串 | 服务名 `codewave.yangshifu.xyz` |
| flate2 + tempfile | gzip 读兼容、原子写 | — |
| thiserror / anyhow | 错误处理 | 库层 thiserror、host 层 anyhow |
| tracing + tracing-subscriber + tracing-appender | 日志 | 滚动文件 `~/.codewave/logs/` |
| dashmap 6 | 并发 map | SessionRuntime 注册表 |
| chrono / chrono-tz / uuid / dirs / sha2 / base64 / encoding_rs | 时间、ID、路径、哈希、编码探测 | `encoding_rs` 用于导出成 csv 的表格（常见 GBK） |
| 平台侧 | tauri-plugin-{dialog, single-instance, notification, updater}、tauri-plugin-decoration 3；`tauri-winrt-notification`（Windows）、`mac-notification-sys`（macOS）、`libc`（unix） | 自绘标题栏 = `tauri-plugin-decoration` v3 |

**已移除的 crate**：`async-openai`（三协议均自写适配）、`tauri-plugin-clipboard-manager`（剪贴板走浏览器 Clipboard API，声明不注册反而造成虚警与无用 API 面）、`schemars`（工具 schema 手写内嵌）、`tiktoken-rs`（`util/token_est.rs` 自写估算）、`tokio-cron-scheduler`。

## 3.3 前端 npm 依赖

- **运行时**：`@tauri-apps/api` ^2.12.1、`@tauri-apps/plugin-notification` ^2.5.0、`@tauri-apps/plugin-updater` ^2.13.1、`antd` ^6.6.3、`@ant-design/icons` ^6.3.4、`react` / `react-dom` ^19.3.0、`zustand` ^5.0.0、`immer` ^11.1.18、`i18next` ^26.4.2 + `react-i18next` ^17.0.14、`markdown-it` ^15.0.2、`highlight.js` ^11.11.0、`mermaid` ^12.0.0、`katex` ^0.18.7、`pdfjs-dist` 5.4.149（**固定版本**，worker 路径对浮动版本敏感）
- **开发**：`vite` ^7.0.0、`vitest` ^5.0.1、`typescript` ^6.0.3、`eslint` ^10.10.0（+ `typescript-eslint`、`eslint-plugin-react-hooks`）、`happy-dom` ^20.14.3、`@testing-library/react` ^16.2.0
- **版本号四处一致**：`package.json` / `src-tauri/Cargo.toml` / `tauri.conf.json` / `ui/package.json` 均为 0.8.7；bundle id `xyz.yangshifu.codewave`
- **测试基线**：`pnpm --dir ui test` 本地全绿；测试集中在 `ui/src/__tests__/`（**102 个** `*.test.ts(x)`），`features/` 下无测试文件

---

# 4. Rust 后端详细设计

## 4.1 core::agent —— 主循环与运行态

### 4.1.1 模块分工与核心类型

`core/agent/` 按职责拆分：

| 文件 | 职责 |
|---|---|
| `runtime.rs` | `SessionRuntime`（会话运行态）与 **inject 通道**（`inject_tx` / `inject_rx` / `inject_accepting` + `pub async fn inject_message()`） |
| `drive.rs` | 主循环与 steer、档位切换、子代理驱动参数构建 |
| `stream.rs` | `Frame` 流帧模型 |
| `supervise.rs` | 错误信号监管 |
| `text_ask.rs` | 文本形态 ask 兜底（`MAX_QUESTIONS=5` / `MAX_OPTIONS=6` / `CONTROL_TOKEN_NAME_MAX=32`） |
| `guards.rs` | 锁获取的统一错误处理 |

`SessionRuntime` 的关键字段：会话 id 与工作区、取消树根（`CancellationToken`）、注入通道三件套、历史内存态（`Mutex<Vec<Message>>`）、单会话单 run 守卫（`run_lock`）、工具并发闸（`sem_tools`）与文件写串行锁（`file_ops`）、会话偏好（档位 + 模型 + 思考力度）、`zombie` 崩溃/退出残留标记（驱动左栏中断徽标）。

> 注意 `rt.data_dir` **恒为全局**数据目录；项目作用域数据一律走 `rt.project_dir`。

### 4.1.2 主循环状态机

```
StartChat(req)
  → 校验配置 → 抢 run_lock（否则拒）
  → spawn run loop：
    for step in 0..MAX_STEPS:
      ① cancel 检查 → 追加 <cancelled> 标记 → 检查点保存 → 结束
      ② drain 注入队列（steer）→ emit run:inject
      ③ 更新实时 token 明细 → emit tokens:update
      ④ if 上下文用量 > compact_threshold：compact_history()（§4.6）
      ⑤ stream_model()（§4.2）→ 经 Channel 推前端
      ⑥ 无 tool_calls → save_history + emit run:done → 结束
      ⑦ 有 tool_calls → execute_batch()（§4.3）→ 结果回填 → 下一 step
```

**取消语义**：`cancel_run()` 触发 CancellationToken；已完成的工具调用与部分回复经检查点保存；历史追加用户可见的取消标记（区别于报错）。进程崩溃/退出的残留由 `zombie` 标记 + `sessions/interrupt.rs` 处理，前端用 `clear_session_interrupt` 清徽标。

### 4.1.3 steer / run-inject（中途注入，不结束 run）

见 [steer-run-inject](./steer-run-inject.md)。与「打断后重发」的关键差别：

- 排队「↑ 立即」由**打断**改为**中途注入**——文本进注入队列，**不结束 run**，运行中的子代理照跑。
- 注入文本可带 **F 强制续跑标记**：该标记**先于**纯文本收尾判定、**一次性消费**，让模型在收尾前强制再走一轮；仍受纯文本轮次硬上限约束。
- `inject_accepting` 是 IPC 接纳的前置标志，但**不能单独用它判定接纳成功**（须与 `inject_rx` 的锁共同使用）。

### 4.1.4 权限四档

`core/prefs.rs` 的 `ApprovalMode`（serde snake_case，**默认档 = `plan`**）：

| 档 | 语义 |
|---|---|
| `plan` | 计划模式（默认）：只调研与出方案，不做修改。文件写入、后台服务与计划任务工具不可用，MCP 工具不可用；shell 仅放行只读命令白名单；子代理同样仅限只读 |
| `confirm_each` | 逐项确认：文件写入（edit/create/delete 及工作区内 shell 写）前弹审批 |
| `auto_edit` | 自动编辑：工作区内写入直通，fence 高危命令仍需确认 |
| `full_access` | 完全放行：跳过审批弹窗与 fence 确认；灾难级命令仍被直接拦截 |

全局 `approval.enabled` 决定新会话初始档（`true` → `plan`，`false` → `full_access`）。会话权限档重启后回落全局默认。

**子代理档位按 step 边界跟随根会话**：主会话档位变化时传播到在跑子代理，工具集 + `<plan-mode>` 系统块 + `sub_rt.prefs` 三处**从基座（`SubBase`）重建而非增量追加**（增量追加会让旧 `<plan-mode>` 块永久残留）；`model_id` / `reasoning_effort` 仍为 spawn 快照；嵌套子代理跟随根会话、父会话查不到时保持原档位不 panic（[mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)）。

## 4.2 provider —— LLM 适配层

### 4.2.1 统一消息 DTO（协议中立中间表示）

`core/types.rs` 的 `Content` 枚举：

```rust
enum Content {
    Text(String),
    Thinking { text: String, signature: Option<String> },
    ToolUse  { id: String, name: String, args: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    Image { media_type: String, data: String },   // DataURL
}
struct Message { role: Role, content: Vec<Content> }
```

三协议适配器都以此为源/汇。**序列化字节稳定性**是硬约束（cache-first）：字段顺序、空白、排序规则固定（serde 结构体天然保序；列表显式排序）。**改 `Content` 会同时撞三协议字节断言与 match 穷尽点**——图片外置落盘走独立 DTO `PersistedMessage` / `PersistedContent`，内存与 wire 的 `Content` 零改动（§4.5）。

### 4.2.2 Provider trait 与三个实现

| 实现 | 协议 | 基础设施 |
|---|---|---|
| `anthropic` | anthropic_messages | **reqwest + sse 自写薄层**（官方无 Rust SDK） |
| `openai_chat` | openai_chat（兼容 Ollama / DeepSeek / LM Studio 等） | reqwest + sse |
| `openai_responses` | openai_responses | reqwest 自写 |

配套模块：`dto.rs`（协议 DTO）/ `keys.rs`（多 key 池）/ `headers.rs`（自定义请求头，[provider-custom-headers](./provider-custom-headers.md)）/ `proxy.rs`（代理与 SSRF）/ `retry.rs`（退避重试）/ `sse.rs`（SSE 解析，三协议共用）。

> 自写 Anthropic 适配的理由：社区 crate 质量波动且未必覆盖 cache_control 断点；Messages 流式协议面窄（message_start / content_block_delta / message_delta / message_stop），薄层包在 Provider trait 后可随时替换。SDK 选型变更时须全文清扫引用。

### 4.2.3 多 Key 池与故障转移

- 有序 key 池 + `KeyHealth` 表（冷却到期 + 上次错误）。
- 选择策略：取第一个未冷却 key；认证类错误（401/403/quota）长冷却，瞬时错误（429/5xx/网络）短冷却；全池冷却时取最早恢复者直接尝试。
- 适配器内不重试（多 key 时避免 N×N 组合）；重试统一由 `retry.rs` / 主循环调度。

### 4.2.4 Prompt Cache 优化

- Anthropic：最后一个 system 块 + 最后一条非瞬态消息打 `cache_control: {type:"ephemeral"}`；瞬态注入（如剩余预算提示）必须置于缓存断点之后。
- OpenAI Responses：按会话稳定 cache key + 边界锚文本。
- `core/prompt.rs` 六层组装的**层序固定**即前缀字节稳定的保证（§4.7）。

### 4.2.5 代理与 SSRF

- `proxy.rs`：手动配置（HTTP/HTTPS/SOCKS5）> 系统代理探测（macOS `scutil --proxy` / Windows 注册表 / Linux 环境变量）> 无代理；手动配置存在时 fail-closed（探测失败不用直连兜底）（[network-proxy-settings](./network-proxy-settings.md)）。
- **SSRF 守卫**：对目标 IP 做私网段校验（含重定向后逐跳校验），`network.allow_private_network` 开关放行本地模型场景。
- `util` 中的 `USER_AGENT` 为全局 UA；**quota 请求用独立 UA** `CodeWave-Quota/1.0`（见 §4.9）。

## 4.3 tools —— 工具系统

### 4.3.1 Tool trait 与注册表

`tools/tool.rs`：

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> &'static str;          // strict JSON Schema（additionalProperties:false）
    fn kind(&self) -> ToolKind;                // 五类，见下
    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutcome;
}

pub enum ToolKind { ReadOnly, FileWrite, Network, Interactive, Meta }
```

**内置工具共 23 个**，注册点 `tools/registry.rs`（`default_tools()`），名字按字典序硬断言在同文件测试里；`tool_defs()` 按名字排序输出以保证请求稳定（利于 provider 侧 prompt cache 命中）。

| 分类 | 工具 |
|---|---|
| 文件读写 | `read` / `edit` / `create` / `delete` / `list_files` / `grep` / `batch_read` |
| Shell 与服务 | `command` / `service` |
| 网络 | `web_fetch` / `http_request` |
| 计算与渲染 | `calculate` / `render_html` |
| 交互与元 | `ask` / `wait` / `suggest` / `plan` |
| 能力扩展 | `skill` / `subagent` / `scheduled_task` |
| 文档 | `read_document` / `write_document` / `edit_document` |

辅助模块：`tools/batch.rs`（批次执行策略）、`tools/compact.rs`（模型侧压缩）、`tools/validation.rs`、`tools/postcheck.rs`（写入后检查）、`tools/pathutil.rs`（路径边界，唯一实现）、`tools/sanitize.rs`、`tools/net.rs`、`tools/encoding.rs`、`tools/writelock.rs`、`tools/claims.rs`。

`tools/document/` 承载 Office 与 PDF 的读 / 生成 / 保真修改（`docx.rs` / `xlsx.rs` / `pdf.rs` / `patch.rs` / `sheet_edit.rs` / `edit_word.rs` / `write_word.rs` / `workbook.rs` / `formula.rs` / `xml_util.rs` / `backup.rs` 等），界面预览复用 `read_document` 的实现（`read_for_preview`），不给预览单写一套解析（[office-and-pdf-support](./office-and-pdf-support.md)）。

### 4.3.2 edit 工具契约

- 入参：`{ files: [{ path, version, changes: [{ oldText?, newText } | { lineRange?, newText }] }] }`，change 支持按文件分组与多文件批量。
- **乐观并发令牌 `version`**：read 结果附带 6 位 Crockford Base32 的 SHA-256 前缀（`util/crockford.rs`）；edit 时校验，不匹配拒绝（防基于过期内容覆写）。
- 应用顺序、diff 计算、写前回滚快照与截断参数抢救见 [edit-tool-optimization-report](./edit-tool-optimization-report.md)、[max-tokens-truncation-fix](./max-tokens-truncation-fix.md)。

### 4.3.3 批次执行策略

```
一个 assistant 回复含 N 个 tool_calls：
  1. 校验批次：Interactive 工具（ask / wait / suggest）必须是批次唯一调用 → 否则整批拒绝
  2. 同批次对同一物理路径（canonicalize 后）的多个写操作 → 全部拒绝（批次冲突）
  3. FileWrite 类按 toolCallIndex 串行（file_ops 锁）；其余并行（Semaphore）
  4. 每工具：panic catch_unwind → E_TOOL_PANIC；执行完成即 emit tool:result / tool:error
```

### 4.3.4 双通道结果

```rust
struct ToolOutcome {
    ok: bool,
    data: Value,                          // 模型通道只读这里
    error: Option<ToolError>,             // 稳定错误码 E_*
    warnings: Vec<String>,                // 非致命告警（宽松解码忽略的未知字段等）
    extra_model_content: Vec<Content>,    // 仅注入模型的多模态内容（serde skip，不进前端 outcome JSON）
}
// 前端通道：完整 ToolOutcome（tool:result 事件）
// 模型通道：compact_for_model() —— head/tail 截断、grep 条数上限、tool output 总配额；read 不压缩（保行号）
```

### 4.3.5 宽容参数解码

`serde_json` 手动解码：未知字段 → `serde_ignored` 收集为 warnings 回填模型；类型错误（如双重编码字符串）尝试自动修复；修复失败且疑似截断 → 走抢救；否则拒执行并给出可自纠的错误说明。

### 4.3.6 写入后检查

见 [post-write-check-plan](./post-write-check-plan.md)。edit/create 成功后执行用户配置的一条检查命令（`config.PostWriteCheckSettings`，在项目根目录运行，支持 `{file}` 占位符），输出尾部随工具结果的 **`outcome.data.check`** 交给模型（"文件已写入；检查输出：…，请修复"）。

- 捕获上限 `MAX_CAPTURE_BYTES = 1 << 20`（1MB）。
- `SkipReason` 六态：`Disabled` / `EmptyCommand` / `NoProject` / `BlockedByFence` / `Timeout` / `Cancelled`。
- 命令由用户按项目配置、执行环境即项目环境，故不需要写前基准 / 差集 / 就绪判据。默认关闭。
- 历史：曾实现「六语言 LSP 语义校验」，因写后结论只达前端 warnings、模型读不到而废弃，LSP 机制整体删除。

## 4.4 safety —— 安全围栏与确认弹窗（D4 决策）

### 4.4.1 围栏判定链（`safety/fence/check.rs`）

现状是 **L0–L3 四级**（不是原设计的三层）：

| 级 | 内容 |
|---|---|
| **L0** | `plan` 档只读命令白名单。白名单外命令**绝不静默放行**；白名单内也不直接放行，仍下落到 L3 + AST 写目标扫描（如 `git push --force`、隐式写命令） |
| **L1** | 删除黑名单：反混淆后的命令名一出现即 Block（POSIX 与 PowerShell 形态合并维护，含 `find -delete` 等删除语义） |
| **L2** | 写目标分析（tree-sitter-bash + **tree-sitter-powershell** AST）：提取重定向目标、tee/dd/cp/mv 目标、heredoc 落点、命令替换内的写目标；canonicalize（含 symlink 逃逸检测）后不在 write_roots → 拦截；新路径创建放行，`/dev/null` 放行 |
| **L3** | 高危模式（进程控制 / 持久化 / 任意代码执行 / 远程宿主等）→ 触发确认而非直接拦；另按名字做「下载-执行」关联判定（`curl \| sh` 类） |

**实际判定序**：L0 白名单 → 解析链（bash AST / PowerShell AST / cmd / fish 词法兜底）→ 掩码 L3（字面量与注释被抹成空白，字符串数据不得触发）→ AST 遍历（L1/L2）→ 下载-执行关联。

**路径边界**（`tools/pathutil.rs`，唯一实现）：`safe_join` 拒绝 `..` 逃逸、绝对路径注入、Windows 保留名；`resolve_write` canonicalize（含中间 symlink）后必须落在 write_roots 内；危险删除守卫拒绝 `/`、工作区根、`.git/`、系统目录；网络工具 SSRF 走 §4.2.5 同一守卫。

### 4.4.2 确认弹窗门（ApprovalGate）

```rust
pub enum ApprovalVerdict { Approved, Denied }
async fn request(&self, req: ApprovalRequest) -> ApprovalVerdict {
    // emit ask:opened {kind:"approval", …}，挂起等待前端 resolve_ask
    // 默认永不超时、无限等待；ApprovalSettings.auto_confirm 勾选后 5 分钟无响应自动确认推荐项
}
```

- 触发条件（默认开，Settings 可关；关闭后 L3 类直接拦截而非放行）：L3 高危模式命中、越界新建路径（`approval.confirm_outside_create`）、git push（`approval.confirm_git_push`）。
- 弹窗 UI 复用 ask 工具卡片通道（同一 `ask:opened` 事件 + `resolve_ask` command），不新增协议；另有 `approval.command_allowlist`（高级设置）。
- **批准门 = 四选项显式选档**：ask 选项带 `mode: Option<ApprovalMode>` 声明目标档位，批准类判定 = `mode.is_some()`（`id="approve"` 与 `switchToAutoEdit` 锚点仅作兼容兜底）；切档目标取**用户所选选项的 `mode`**。第四项「先看预览」**不带 mode**，语义是**不批准也不驳回**（渲染预览后重发同一询问），必须在结构化通道、宽松批准匹配、轻量切档三处都被排除——少一处即「前端切档、后端不切」的单边静默提权（[preview-skill](./preview-skill.md)、[mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)）。

### 4.4.3 文本形态 ask 兜底

`core/agent/text_ask.rs`：模型不调 ask 工具、直接在正文里输出结构化提问时的兜底解析。上限 `MAX_QUESTIONS=5` / `MAX_OPTIONS=6` / `CONTROL_TOKEN_NAME_MAX=32`（[text-form-ask-fallback](./text-form-ask-fallback.md)）。

## 4.5 core::sessions —— 会话持久化与修复管线

详见 [session-history-storage](./session-history-storage.md)、[session-history-limits](./session-history-limits.md)、[session-restore-fidelity](./session-restore-fidelity.md)。

### 4.5.1 存储格式（无数据库）

模块：`store.rs` / `segments.rs` / `persist.rs` / `repair.rs` / `image_blobs.rs` / `tool_results.rs` / `cleanup.rs` / `interrupt.rs`。

```
~/.codewave/
├── sessions/index.json                        # 索引，MAX_INDEX_ENTRIES=2000（LRU 淘汰）
├── histories/<id>/                            # 分段 append-only JSONL（明文不压缩、无上限）
│   ├── 0001.jsonl …                          # 每段首行 header，段尾可能一条 seal
│   └── .segmeta.json                         # 增量水位边车
├── histories/subs/<父>/<子>/                 # 子代理历史
├── sessions/<owner>.imgblob/<sha256 前 32 hex> # 图片外置（内容寻址去重）
├── sessions/<owner>.toolres/<call_id>.json    # 工具出参 sidecar
└── histories/<id>.json.gz                     # 旧格式：读兼容仍在，不迁移
```

关键不变量：

- **段封口** = `SEGMENT_MAX_MESSAGES=200` 或 `SEGMENT_MAX_BYTES=512KB` 先触者；**绝不切在 tool_use/tool_result 配对中间**。
- **只新增段，绝不覆盖/截断/删除既有段**——任何 fallback（前缀哈希不匹配 → 基线段）也是新增而非改写。这是「不丢历史」的底线。
- **体积线**：软线 `HISTORY_SOFT_WARN_BYTES=200MB` 只提示、**照常写**；硬线 `HISTORY_HARD_FUSE_BYTES=1GB` **只停写、绝不删数据**，体积回落后自愈。
- **加载容错**：丢弃尾部残行、坏行跳过、坏段跳过并提示，**绝不因容错报「历史损坏」**。
- **wire 装载必须有界**：从尾部读到够用即停（`TRIM_BUDGET_TOKENS=256K` 只作用于 wire 侧），`display` 全量不裁、前端按段分页；打开会话不随历史总量线性增长。
- **图片外置**：`PersistedMessage` / `PersistedContent`（`ImageInline` / `ImageBlob` 两态），内存与 wire 的 `Content` 零改动。子代理 blob owner 是 `<父>__<子>`（子 id 只有 32 bit，裸用会跨会话撞目录）。
- **工具出参 sidecar**：键是 **provider 侧 `tool_use.id`**（实时 `call_key` 是批内随机值、不可持久）；写入门槛 = 模型侧文本解析不了（头尾截断、失败 `[error …]` 前缀）；`MAX_CALLS=50` / `MAX_FILE_BYTES=2MB`。**永不参与出网**（不改 `rt.history`、不改工具出参 JSON、不增计费），随会话级联删除，且**不进**右栏「文件」面板。恢复时经 IPC `load_tool_outcomes` 批量回填，前端三个消费方共用一套回填判据（[tool-call-card-live-key](./tool-call-card-live-key.md)）。
- **锁序** `save_lock → index_lock`；保存路径必须持 `save_lock`；GC 用内存快照（不串行化会让并发保存互删对方仍引用的图），引用集合是「磁盘上所有保留段引用的 blob 并集」。
- 状态挂在 `SessionMeta.history_status` + `run:done.history_save`；**新状态必须同步纳入 `SaveReport::is_clean()`**（`drive.rs` 的上报判据是 `filter(|r| !r.is_clean())`，漏改则提示永远发不出去）。
- **旧格式清理入口的判据必须与读路径同源**（`segments::has_readable_messages`）——**无新格式可读数据时绝不删旧文件**，那是唯一副本。

### 4.5.2 修复管线（保存与加载双向执行）

```
sanitize：剥离 system 消息与图片 payload；多 content 块归一；修复截断的 tool args；折叠重复 tool 名
trim    ：仅 wire 侧按 256K token 预算在 user 消息边界裁剪，保证 tool_use/tool_result 配对完整
          （落盘的 display 不裁）
repair  ：剥离悬空 tool_use；丢弃孤儿 tool_result；tool_use id 配对校验
```

## 4.6 core::context —— token 统计与自动压缩

- **ContextBreakdown**：system / history / tool_results / tool_schemas 分项统计，`tokens:update` 推前端信息条；usage 优先取 provider 上报，缺失按字符估算。
- **自动压缩**：用量 > `compact_threshold` 触发；结构化摘要提示词（最新请求 / 已完成 / 当前状态 / 下一步 / 关键文件），替换整段历史并加 handoff 前缀；`compact_session` 命令手动触发（保留最后一条 user 消息）。
- 摘要请求走同一 Provider 层：超时 `DEFAULT_SUMMARY_TIMEOUT_SECS=180`（3min），输出上限 `SUMMARY_MAX_TOKENS=8000`（接线为 `model.max_tokens = min(…, 8000)`）。
- **Composer 工具条数据面分工**：上下文/命中段读**会话级** `TabRunState.usage`（跨 run 累加、不持久化），速率段读**本轮** `TabRunState.runMetrics`（`send()` 处归零）；统计面板总览的耗时三项必须**分子分母同域**（只取 `gen_ms > 0` 的桶），否则子代理/压缩的 output 会把速率抬到数倍（[composer-token-rate](./composer-token-rate.md)）。

## 4.7 core::prompt —— 系统提示词六层组装

| 层 | 内容 |
|---|---|
| 1 | 核心行为规范：优先级总声明、讨论/实现判别、工具策略、批量与编辑纪律、输出风格、安全边界、平台信息（内含标准工作流常驻段） |
| 2 | Skills 元数据（仅 name/description/whenToUse 索引） |
| 3 | 记忆索引（`memories/*.md` 路径 + 描述，条数有上限） |
| 4 | 项目指令：工作区 `CODEWAVE.md` / `AGENTS.md` / `CLAUDE.md`（新名在场时跳过旧名兼容读取） |
| 5 | 项目图谱：`CODEGRAPH.md`（若存在）+ `.codewave/lessons.md` 经验 |
| 6 | 用户自定义提示词 |

组装规则：层序固定 → 前缀字节稳定（缓存优先）；瞬态内容一律后置。`/` 与 `$` 触发符限「消息以它开头」是与模型侧点名契约绑定的（`skills/mod.rs` 的 `prompt_listing` 与 `core/prompt.rs` 的核心提示词都写死该措辞），要放宽必须同步改提示词（[composer-trigger-caret](./composer-trigger-caret.md)）。

## 4.8 git —— git2-rs 模块（D5 决策）

- **定位：只读集成**——status / diff / recent log / user_info，服务「Agent 改动前后的变更展示」，**不做任何写操作**（stage / commit / push 由用户在终端或未来版本进行）。
- IPC：`git_status` / `git_diff`（工作区 vs HEAD）/ `git_recent_log` / `git_user_info`，均带 `sessionId` 参数。
- UI：右栏 `changes` 页签的 `ChangesPanel` 聚合 diff（`features/workspace/` 下**仅此一个文件**）。
- `git/mod.rs` 仅 `pub mod status;`；不启用 ssh feature。
- **`SessionMeta` 的 `project_id + roots` 快照是 git 聚合与 @ 提及的唯一数据源，勿绕过回查注册表。**

## 4.9 mcp / skills / memory / agents / quota / scheduler / stats

| 模块 | 现状要点 |
|---|---|
| **mcp/** | rmcp 客户端（`config.rs` / `manager.rs` / `process.rs` / `tools.rs` / `error.rs`）。`McpTransport { Stdio, StreamableHttp }`；**会话隔离的连接池**（键为 `(作用域, 项目 id, server 名)`）；配置 = 用户级 `~/.codewave/mcp.json` + 项目级 `<主目录>/.codewave/mcp.json` 按作用域叠加；stdio 走 Job Object / 进程组防孤儿；工具以 `[server]` 前缀追加在内置工具后；`mcp_test` 起 → tools/list → 立即回收，不并入连接池、不改正式状态（[mcp-module-rebuild](./mcp-module-rebuild.md)） |
| **skills/** | 扫描优先级（低 → 高，同名后者覆盖）：内置 < `~/.claude/skills` < `~/.agents/skills` < 工作区 `.claude/skills` < 工作区 `.agents/skills` < 全局 `~/.codewave/skills` < 项目 `<主目录>/.codewave/skills`。**内置技能 6 个**：`repo-index` / `doc-convert` / `preview` / `xlsx` / `docx` / `pdf`；托管目录技能可在设置页删除，兼容/内置来源不可删；系统提示词仅注入索引；斜杠命令与 `skill` 工具双通道（[slash-skills-and-dollar-agents](./slash-skills-and-dollar-agents.md)） |
| **memory/** | `~/.codewave/memories/*.md`；无专用工具——目录在 write_roots 白名单，模型直接用 create/edit 维护；索引注入系统提示词 |
| **agents/** | 内置子代理角色注册表**共 9 个**：`explore`（只读）/ `backend-dev` / `frontend-dev` / `app-dev` / `reviewer` / `product-manager` / `code-reviewer` / `tester` / `title`。**`title` 例外**：仅供 `core/title.rs` 自动命名消费，**不进 subagent 角色枚举**（[session-auto-title](./session-auto-title.md)）。编排角色（标准工作流、plan 档）**不是**本表角色，而是内置为常驻提示（[standard-workflow](./standard-workflow.md)、[plan-mode-workflow](./plan-mode-workflow.md)）；子代理不能派生子代理 |
| **quota/** | 订阅额度，**在 `core/` 下不是顶层**。7 家 `ProviderKind`（OpenCodeGo / DeepSeek / MiniMaxIntl / MiniMaxCn / Kimi / Zhipu / Zai）× 六态 `QuotaStatus`（`Ok` / `Error` / `Invalid` / `Rejected` / `NoKey` / `Unsupported`）；`REQUEST_TIMEOUT=10s`；quota 专用 UA `CodeWave-Quota/1.0`（**独立于全局 UA**）；`~/.codewave/quota.json` 记 `last_ok_at`（[quota-from-provider-config](./quota-from-provider-config.md)、[quota-state-heal](./quota-state-heal.md)） |
| **scheduler/** | `core/scheduler.rs`：`cron` crate 校验表达式 + **自写 tick** 推进（**无** tokio-cron-scheduler）；任务随项目**落盘**（`<主目录>/.codewave/tasks/<id>.json`）；全局串行执行（`exec_lock`）；每 run 隔离上下文；暂停开关 / 立即运行 / 执行历史（[tasks-module-polish](./tasks-module-polish.md)、[tasks-local-cron-time](./tasks-local-cron-time.md)） |
| **stats/** | 异步有界队列写 `stats/<date>.json`，按天/模型/工作区聚合，保留期清理（[rightbar-info-refactor-and-subscription-quota](./rightbar-info-refactor-and-subscription-quota.md)） |
| **core/projects.rs** | 目录式项目注册表（名称 + 单一主目录 + `allowed_dirs`） |
| **core/openers/** | 文件管理器与编辑器探测/打开（`list_editors` / `open_in_editor` / `open_dir`） |
| **util/** | `atomic.rs` / `crockford.rs` / `throttle.rs` / `token_est.rs` + `USER_AGENT` |

## 4.10 host —— 唯一框架边界

- `host/commands/`：**全部 `#[tauri::command]`**，16 个子模块 `agents` / `git` / `logs` / `mcp` / `openers` / `preview` / `project` / `quota` / `scheduler` / `session` / `skills` / `stats` / `system` / `ui_state` / `util` / `workspace`；只做参数校验 + 转调 core，不含业务逻辑。
- `host/events.rs`：`EventSink` trait 的 Tauri 实现（`TauriSink`）——高频走 `tauri::ipc::Channel`，低频走 `AppHandle::emit`；core 依赖的是 trait 而非实现。
- `host/keyring.rs`：系统钥匙串读写（服务名 `codewave.yangshifu.xyz`）。
- `host/notify.rs`：系统通知按平台原生直驱（点击回跳走 `notify:activate`），失败回退插件路径。
- 窗口：**自绘标题栏**（`tauri-plugin-decoration` v3，顶栏即标题栏；窗口 `visible:false` 起动，激活失败回退原生框）；单实例插件；托盘 `TrayIconBuilder`。

---

# 5. IPC 协议（Commands 与 Events）

## 5.1 Commands（前端 invoke）

`ui/src/ipc/client.ts` 是**唯一 invoke 入口**，共 **89 条** invoke 命令（组件不得直接 import `@tauri-apps/api`，新增命令一律收敛在此处）。命令名 snake_case，**一个字符不可改**；入参对象键名即 Tauri invoke 参数名；返回类型与 `ipc/types.ts` 的 serde 结构一一对应。按域分组：

| 域 | 代表命令 |
|---|---|
| 通用 | `ping`、`app_version`、`open_data_dir`、`open_url`、`restart_app`、`is_appimage` |
| 项目 | `list_projects` / `save_project` / `delete_project`、`select_workspace_dir`、`check_external_path` / `allow_external_dir` |
| 配置 | `get_config` / `save_config` / `resolve_proxy`、`get_ui_state` / `set_ui_state`、`set_font_prefs` |
| 会话 | `create_session`（双形态）/ `list_sessions` / `load_session` / `load_session_earlier` / `load_subagent_history` / `delete_session` / `rename_session` / `session_running` / `set_session_prefs` / `get_session_prefs` / `restore_legacy_model_prefs` / `clear_session_interrupt` |
| 运行 | `start_chat`（带 Channel）/ `cancel_run` / `inject_run_message` / `resolve_ask` / `compact_session` / `stop_subagent` |
| 清理 | `preview_session_cleanup` / `run_session_cleanup` / `get_cleanup_status` / `preview_legacy_history_cleanup` / `run_legacy_history_cleanup` |
| 文件 | `search_workspace_paths` / `read_workspace_file` / `read_workspace_file_base64` / `read_file_chunk` / `list_session_files` / `select_document_files` |
| 文档 | `preview_document` / `list_document_backups` / `restore_document_backup` |
| Git | `git_status` / `git_diff` / `git_recent_log` / `git_user_info` |
| MCP | `mcp_list_config` / `mcp_save_config` / `mcp_connect` / `mcp_disconnect` / `mcp_reconnect` / `mcp_test` / `mcp_snapshot` |
| Skills / Agents | `list_skills` / `get_skill` / `toggle_skill` / `reload_skills` / `delete_skill`、`list_agents` |
| 任务 / 统计 | `list_scheduled_tasks` / `create_scheduled_task` / `update_scheduled_task` / `delete_scheduled_task` / `set_scheduled_task_enabled` / `run_scheduled_task_now` / `get_token_stats` / `get_token_breakdown` |
| 服务 / 日志 | `stop_service`、`list_log_files` / `read_log_file` / `read_session_log` / `open_logs_dir` |
| 额度 | `quota_snapshots` |
| 打开器 | `open_dir` / `list_editors` / `open_in_editor` / `list_available_shells` |
| 退出拦截 | `list_running_sessions` / `resolve_exit_request` |

> **已删除**：`save_workspace_file` 随 WorkspaceExplorer 编辑器整体移除（[workspace-explorer-removal-and-chat-scrollbar](./workspace-explorer-removal-and-chat-scrollbar.md)）。现存 `read_workspace_file` 是右栏文件面板的**只读**取数，与被删的「读 + 存」语义不同。
>
> **`SessionMeta` 的 `project_id + roots` 快照是 @ 提及 / git 聚合的唯一数据源**，勿绕过回查注册表。`create_session(project_id?, workspace?)` 为双形态；`delete_project` 级联删除（先取消运行中会话）。

## 5.2 事件协议

### Channel 高频帧（`start_chat` 传入，有序、点对点）

`core/agent/stream.rs` 的 `Frame` 模型，承载流式文本 / reasoning 增量、工具事件与 `usage`。`Frame::Usage` 的载荷可**增可选字段**（serde default），**事件键名 `usage` 不变**（契约测试只守键名）。

### 全局事件：**29 键**

定义于 `ui/src/stores/runHandlers.ts`（每族一个 handler 工厂，`run.ts` 的 `bindGlobalHandlers()` 保持唯一注册点并展开它们），由 `ui/src/__tests__/events.contract.test.ts` **双向守护 + 硬断言键数为 29**（`lsp:server_missing` 已随写入后检查删除）。键名不可增删，增删必须同步改断言。

| 族 | 键 |
|---|---|
| run 生命周期 | `run:start` / `run:done` / `run:error` / `run:cancelled` / `run:inject` / `run:retry` |
| 压缩 | `run:compacting` / `run:compacted` / `run:compact_failed` |
| 工具 | `tool:start` / `tool:result` / `tool:error` |
| 交互 | `ask:opened` / `ask:closed` |
| 子代理 | `sub:spawn` / `sub:step` / `sub:report` / `sub:usage` / `sub:done` / `sub:error` |
| 上下文 / 会话 | `tokens:update`、`session:title`、`plan:update`、`run:suggestions` |
| 任务 | `scheduled:fired` / `scheduled:done` |
| 外部状态 | `mcp:status`、`service:update`、`app:exit_requested` |

> **例外**：后端还 emit `notify:activate` 与 `menu:action`，这两键**不在 29 键内**——它们由前端直接 `listen`，不进 `bindGlobalHandlers()` 表，契约测试的 `directListened` 分支放行。因此「事件面 29 键」只在 handler 表口径下成立。

---

# 6. 前端设计

## 6.1 目录结构

```
ui/src/
├── main.tsx / App.tsx             # 壳
├── ipc/                           # 唯一 touch @tauri-apps 的目录
│   ├── client.ts                  # 唯一 invoke 入口（89 条命令）
│   ├── types.ts                   # 前后端契约类型
│   ├── events.ts                  # 通用 bind
│   └── dragdrop.ts                # 拖入窗口 → 真实路径
├── stores/                        # zustand
│   ├── run.ts + run.types.ts + runFrames.ts + runHandlers.ts
│   ├── sessions.ts  settings.ts  tasks.ts  ui.ts  updater.ts
├── features/
│   ├── chat/                      # Composer / ChatMessages / ChatScrollbar / segments /
│   │                              #   QueuePanel / ContextInfoBar / ExternalDirPrompt /
│   │                              #   composerTriggers.ts / composerRefs.ts / composerMetrics.ts
│   ├── shell/                     # AppShell 三栏外壳 / TopBar（两段式自绘标题栏）/
│   │                              #   ProjectNav 左侧导航 / RightBar（4 页签）/ ResizeHandle /
│   │                              #   OpenInEditorSelect / SkillDetailModal /
│   │                              #   useDisplayWidths / useTitlebar
│   ├── panels/                    # SettingsPage（10 页 3 组）/ TasksPage（覆盖式全屏页）/
│   │                              #   ProvidersPanel / McpStatusTable / McpKvTable /
│   │                              #   TokenStatsModal / UpdateModal / AboutSettings /
│   │                              #   FontSettings / schedulePreset.ts / settingsRegistry.ts
│   │                              #   + settings/ 子目录
│   ├── files/                     # FilesPanel / FileViewerModal / PdfView / SheetView /
│   │                              #   parseTable / useSessionFiles
│   ├── tools/                     # AskPanel / ToolCallCard / WidgetPreviewModal / askShape
│   ├── subagent/                  # SubagentDrawer / SubagentItemCard
│   ├── quota/                     # QuotaSection / quotaRow / quotaFormat
│   └── workspace/                 # 仅 ChangesPanel.tsx（工作区 vs HEAD 聚合 diff）
├── i18n/                          # zh-CN（604 行）/ en-US（611 行）
├── theme/                         # bridge.tsx（antd token → --ws-*）/ app.css / native.css
├── utils/                         # 18 个纯函数模块（models / fonts / markdown / diff / …）
├── components/                    # CodeBlock
└── __tests__/                     # 全部测试（102 个 *.test.ts(x)）
```

**要点**：

- **左侧导航是一切会话/项目入口**（`ProjectNav`：临时会话区 + 项目 → 会话树 + 任务区）；顶栏只有 Tab 条与功能按钮（无「选择工作区」「新 Tab」）。
- **工作区文件面板已整体移除**：`features/workspace/` 只剩 `ChangesPanel`，文件树与内嵌编辑器不再存在（[workspace-explorer-removal-and-chat-scrollbar](./workspace-explorer-removal-and-chat-scrollbar.md)）。
- **RightBar 常驻右边栏，4 个页签**：`info`（Git / 模型 / 项目目录 / 会话信息）/ `logs` / `files`（会话产物）/ `changes`。
- **组件样式一律 antd 组件库接管**：`--ws-*` token 由 `theme/bridge.tsx` 桥接，不手搓皮肤 / 自绘调色板；**强调色 = 中性墨色**（亮 `#1f1f1f` / 暗 `#424242`），色彩强度映射风险等级（无彩色 = 默认、橙 = 需注意、红 = 危险）（[ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md)）。
- **字体走 `--ws-font-sans` / `--ws-font-mono` 双槽 token**，用户偏好由 `utils/fonts.ts` 管理，不硬编码字体链（[custom-font-and-titlebar](./custom-font-and-titlebar.md)）。
- **主题跟随系统**。

## 6.2 流式渲染与 Composer（性能与交互关键路径）

```
后端节流 delta → Channel → zustand run.ts 追加缓冲（immer 中间件承载流式高频更新）
  → 合帧 flush 到消息模型 → 仅当前流式消息独立渲染作用域
  → 已完成消息走缓存（mermaid 视口内懒渲染）
```

**Composer 触发符以光标为锚**：`/`、`$`、`@` 的判定与回填一律基于**光标前的片段**（`composerTriggers.ts` + `caretRef`）。点击与方向键移动光标**不过 `onChange`**（靠 `onSelect` / `onClick` / `onKeyUp` 补同步）——引入锚定整段末尾的正则正是「正文里已有内容时拉不起菜单」的根因（[composer-trigger-caret](./composer-trigger-caret.md)）。

**文件入口统一为「原地引用」**：附件按钮与拖入窗口拿到的都是**真实路径**，非图片记进草稿 `refs` 并以 **chip** 展示（输入框里不出现路径），**发送前一刻**才合成 `@路径` 追加正文末尾（不复制副本、不占上下文）；图片因要上 wire 才读成 base64（[composer-file-ref-chips](./composer-file-ref-chips.md)）。网页内 `<input type=file>` 拿不到路径，故附件按钮走原生选择框。

**草稿不只在正文文本里**（文件引用 chip 存 `refs`、图片存 `images`）：凡「覆盖草稿」的链路（`ws:composer-fill`、队列编辑、历史召回）必须连带覆盖它们，只 `setText` 盖不住。

## 6.3 工具卡体系

- `ToolCallCard`：统一壳（状态图标 + 动词表 + 折叠正文）；按工具名注册展示适配器。
- 定位键用 provider 侧 `tool_use.id`（非批内随机 `call_key`）；历史恢复时经 `load_tool_outcomes` 批量回填，主会话工具卡、子代理卡、子代理过程抽屉三个消费方共用一套回填判据（[session-restore-fidelity](./session-restore-fidelity.md)、[tool-call-card-live-key](./tool-call-card-live-key.md)）。
- `AskPanel` 承载 ask 工具与审批弹窗的共用交互（[ask-unified-plan-card-and-answer-switch](./ask-unified-plan-card-and-answer-switch.md)）。

## 6.4 窗口与标题栏

自绘标题栏 + 拖拽区 + 单实例聚焦；顶栏两段式（左段 Tab 条，右段功能按钮与 logo 开关）。

---

# 7. 数据与配置布局

```
~/.codewave/                       # 全局作用域（rt.data_dir 恒指这里）
├── config.json                    # 主配置 schema v2：providers[] 嵌套 ProviderModel
│                                  #   （wire id 即显示名）、active_model_id、proxy、
│                                  #   compact_threshold、ui（含 font_sans / font_mono）、
│                                  #   approval、post_write_check、sessions.retention_days…
├── projects.json                  # 目录式项目注册表（含 allowed_dirs）
├── mcp.json                       # 用户级 MCP 配置
├── quota.json                     # 额度 last_ok_at
├── ui-state.json                  # 会话现场态（Tab 集合 / 草稿 / 面板态 / 窗口几何）
├── sessions/                      # index.json、*.imgblob/、*.toolres/
├── histories/                     # 分段 JSONL（见 §4.5）
├── memories/                      # 跨项目记忆 *.md
├── skills/                        # 用户级 skills
├── stats/                         # <date>.json
├── logs/                          # tracing 滚动日志 + 会话日志
└── temps/                         # 临时会话工作区（命令 cwd）与命令输出落盘

<项目主目录>/.codewave/            # 项目作用域（rt.project_dir）
├── project.json
├── tasks/                         # 计划任务落盘
├── memory/  logs/  skills/
├── mcp.json                       # 项目级 MCP 配置
└── lessons.md                     # 经验（注入系统提示词第 5 层）

工作区内：
  CODEWAVE.md / AGENTS.md / CLAUDE.md   # 项目指令（兼容既有习惯）
```

**关键约定**：

- **配置 schema v2**：`providers: Vec<ProviderConfig>` 嵌套 `ProviderModel`（wire id 即显示名），`active_model_id` 仍指向模型条目 id；`ModelConfig` 只是**运行时摊平形态**（后端 `ConfigState::find_model`，前端 `utils/models.ts`），两端摊平语义需同步改（[provider-management-refactor](./provider-management-refactor.md)）。
- **配置结构变更必须 serde default**（新字段向前兼容）；schema v1 扁平 models 迁移已随全新发布移除，**不再新增**旧数据迁移代码。
- **data_dir 随保存归一化持久化**；legacy 目录回退已移除。
- **`ProjectEntry.allowed_dirs`**（serde default）记用户「始终允许」的项目外目录，会话创建/恢复时并入 `extra_roots`。
- **`SessionStore` 索引写必须走 `index_lock`。**
- **命令 cwd** = 项目数据目录下的 `temps/`（临时会话 = 全局数据目录）；写入仍走 fence 审批。
- **删除项目 = 级联删除其下会话 + 托管目录**（确认弹框列明影响）；**用户代码目录永不动**。

---

# 8. 安全模型（三层防线）

| 层 | 机制 | 来源 |
|---|---|---|
| ① Agent 行为围栏 | §4.4.1：L0 plan 白名单 → L1 删除黑名单 → L2 AST 写目标（bash + PowerShell）→ L3 高危模式 + 下载-执行关联；路径边界 pathutil；SSRF 守卫 | 独立实现 |
| ② 确认弹窗（D4） | ApprovalGate：高危命中 / 越界新建 / git push 触发；复用 ask 通道；默认开、Settings 可关；**四选项显式选档**，预览项在三处都被排除 | 本方案新增 |
| ③ Tauri capability/CSP | capabilities 仅授权所需插件（fs scope 限 `~/.codewave` 与项目目录、shell 默认全禁）；CSP 禁 remote origins；`render_html` 用 sandbox iframe | Tauri 2 原生 |
| 提示词纪律 | 注入防御（工具输出 / 网页内容是数据不是指令）、git push 需确认、敏感文件不读 | §4.7 层 1 |
| 权限四档 | `plan` / `confirm_each` / `auto_edit` / `full_access`，默认 `plan`；档位按 step 边界传播给子代理 | §4.1.4 |

**工具风险分级**（`ToolKind` 五类，决定审批行为）：`ReadOnly` 静默放行 / `FileWrite` 走审批 / `Network` 需确认 / `Interactive` 独占批次 / `Meta` 操作 agent 自身状态（`tools/tool.rs`）。

---

# 9. 构建、CI 与发布

## 9.1 本地开发

```
pnpm install                       # 仓库根
pnpm --dir ui dev                  # 前端 HMR（vite --port 1420 --strictPort）
pnpm tauri dev                     # 整体开发模式（Tauri CLI 走 pnpm，须在仓库根执行）
pnpm tauri build --debug           # 产物
```

`tauri.conf.json` 要点：`identifier: "xyz.yangshifu.codewave"`、`productName: "CodeWave"`、CSP 白名单自资产、`externalBin`：无（ripgrep 用库 crate，无外部二进制）；标题栏自绘。

**单实例互斥**：`tauri dev` 与打包版同 bundle id 不能同时跑；GUI 验证前先确认没有 dev 实例在运行。**界面改动不做 GUI 自动点验**——完成后交付分步手动验证清单，由用户手动验证。

## 9.2 测试与 CI 门禁

| 检查 | 命令 | 说明 |
|---|---|---|
| 后端测试 | `cd src-tauri && cargo test` | 本地全绿；**0 warning 基线**（新增 clippy 警告一律当场修，不要攒）；CI 用 `cargo test --workspace` |
| 前端测试 | `pnpm --dir ui test` | 本地全绿 |
| 前端构建 | `pnpm --dir ui build` | type check + vite build |
| 开 PR 前本地门禁 | `pnpm prepr` | **开 PR 前必须跑完**，逐条覆盖 CI 的两个 workflow（`cargo fmt --check` / `cargo clippy`（软）/ `cargo test --workspace` / `pnpm --dir ui run lint` / `ui test` / `ui build` / `node --test scripts/**` / `pnpm install --frozen-lockfile`）；硬步骤失败即停并给出失败清单 |

**CI 只是复核，不是第一道防线**——lint 与格式类问题绝不该由 CI 首次发现。本地只覆盖当前平台，三平台矩阵差异仍以 CI 为准（[pre-pr-local-gate](./pre-pr-local-gate.md)）。

## 9.3 发布

- **版本号四处一致**：`package.json` / `src-tauri/Cargo.toml` / `tauri.conf.json` / `ui/package.json`；统一由 `pnpm bump <x.y.z>` 升级（[version-bump-and-release](./version-bump-and-release.md)）。
- **git 写操作由用户执行**：commit / push / merge / branch / stash 等 AI 默认不碰。唯一例外是**用户批准计划 = 预授权创建并切换计划所示分支**；commit / push / merge 仍由用户执行（[plan-branch-proposal](./plan-branch-proposal.md)）。
- updater 已接线（`plugins.updater` 节点必须存在，`active: false` 也行，否则启动 panic）；macOS 签名与公证见 [macos-signing-and-notarization](./macos-signing-and-notarization.md)。

> **D7 的执行结果与决策原文不同**：决策写「不做签名/更新服务器」，实际已接入 updater 与签名流程。按 §0 的定位，此处只记录落地形态，不改写决策原文。

---

# 10. 交付状态与验收

> P0 / P1 / P2 三批里程碑已全部交付。本章记录**当前已实现的能力面**与关键验收约束，取代原「剩余工具清单」式的待办罗列。

## 10.1 已交付能力面

| 域 | 状态 |
|---|---|
| 后端主循环 | 完整：run / 取消 / steer 中途注入 / 单会话单 run / 检查点 / 子代理 + 档位同步 |
| 工具 | **23 个内置工具全部实现**（§4.3.1），含 Office 与 PDF 的读 / 生成 / 保真修改 |
| 安全 | 围栏 L0–L3（bash + PowerShell）+ ApprovalGate 四档 + 路径边界 + SSRF 守卫 |
| Provider | 三协议（anthropic / openai_chat / openai_responses）+ 多 key 池 + 代理/SSRF + cache_control |
| 会话持久化 | 分段 append-only JSONL + 修复管线 + 图片外置 + 工具出参 sidecar + 有界 wire 装载 |
| 扩展 | MCP（stdio + streamable-http，会话隔离连接池）、Skills（7 级来源 + 6 内置）、子代理（9 角色） |
| 前端 | 三栏外壳 + 自绘标题栏 + 流式聊天 + 工具卡 + 10 页设置（3 组） + 4 页签右栏 + 覆盖式任务页 + 统计面板 |
| 其它 | 额度查询（7 家）、计划任务、编辑器/文件管理器探测、系统通知、updater |

## 10.2 关键验收约束

- 端到端「读 → 改 → 跑测试」闭环；edit 并发令牌能拒绝过期写入。
- 构造悬空 tool_use 的历史，加载后自动修复且能继续对话。
- 断网重试符合退避曲线；**anthropic SSE 流内绝不调 `parser.finish()`**（由分片撕裂集成测试守护）。
- 长回复滚动不掉帧；打开会话不随历史总量线性增长。
- 历史体积触达硬线只停写、**绝不删数据**；无新格式可读数据时清理入口**绝不删旧文件**。
- 界面改动交**分步手动验证清单**给用户，不做 GUI 自动点验。

## 10.3 明确不做（现状边界）

- **git 写操作**：只读集成，不提供 stage / commit / push。
- **WorkspaceExplorer**：工作区文件面板已整体移除（[workspace-explorer-removal-and-chat-scrollbar](./workspace-explorer-removal-and-chat-scrollbar.md)）。
- **LSP 语义校验**：机制已删除，写后检查改走用户自配命令（[post-write-check-plan](./post-write-check-plan.md)）。
- **旧格式数据迁移**：`histories/<id>.json.gz` 读兼容仍在但不迁移；schema v1 扁平 models 不再受理。
- **SSH 远程工作区、server 模式（core + axum）、移动端**：未实现。

---

# 11. 风险清单与缓解

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R1 | tree-sitter 写目标分析正确性（自研核心） | 围栏漏拦/误拦 | 用例集覆盖重定向/heredoc/命令替换/symlink；bash 与 PowerShell 两侧分别维护用例；L3 一律走弹窗兜底；误拦仅表现为多一次确认 |
| R2 | 自写 Anthropic SSE 适配的协议细节（event 类型、配对、partial json） | 流式中断 | 用例回放真实抓包流；分片撕裂有集成测试守护；Provider trait 隔离可随时换社区 crate |
| R3 | git2/libgit2 三平台构建（cmake） | CI 变重 / Windows 失败 | 构建风险已在 P0 前移验证；不启用 ssh feature |
| R4 | WebKitGTK（Linux）渲染差异 | 冒烟通过但体验差 | Linux 仅保编译 + 基础冒烟 |
| R5 | 会话历史体积失控 | 磁盘占满 / 加载变慢 | 软线只提示、硬线只停写**绝不删数据**；加载有界（wire 侧从尾部读够即停），打开会话不随总量线性增长 |
| R6 | 段封口切在 tool_use/tool_result 配对中间 | 历史损坏 | 封口逻辑显式避开配对中间；任何 fallback 只新增段、不改写既有段；加载端容错跳过坏行坏段，**绝不报「历史损坏」** |
| R7 | 前后端事件键漂移 | 静默功能失效 | `events.contract.test.ts` 双向守护 + 29 键硬断言 |
| R8 | 批准门单边静默提档 | 权限被无声抬高 | 预览项必须在结构化通道、宽松批准匹配、轻量切档三处都被排除，各有测试钉死 |
| R9 | 私有项目无签名 → Gatekeeper 拦截 | 自用安装麻烦 | 签名与公证流程已文档化并接入 updater；不影响开发 |

---

*本文档为 CodeWave 的技术基准与实现现状：§0 是历史锚点（决策原文，不随实现改写），§1–§11 与对应版本的源码对齐。执行中如变更 §0 任一决策，须改 §0 并全文清扫引用；如实现演进，须同步更新 §1–§11 与本文顶部的现状同步说明。*