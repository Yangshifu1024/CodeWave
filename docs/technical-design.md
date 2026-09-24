# CodeWave 技术方案 v1.0

> 日期：2026-08-30
> 状态：已按 2026-08-30 决策定稿，作为 CodeWave 实施的基准方案

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
10. [里程碑与验收](#10-里程碑与验收)
11. [风险清单与缓解](#11-风险清单与缓解)

---

# 1. 目标与设计原则

## 1.1 产品定位

CodeWave 是一个**本地优先的桌面 AI 编程 Agent**：用户打开一个工作区目录，通过对话驱动 Agent 理解代码、编辑文件、执行命令、搜索内容、管理任务；支持 OpenAI 兼容 API 与 Anthropic API、MCP 扩展、Skills、子代理。

## 1.2 设计原则（8 条，指导所有实现）

1. **host 单向依赖**：只有 `host/` 模块允许 `use tauri::*`；`core/tools/safety/provider` 等纯 Rust 模块不感知框架，可独立单测，也为将来 server 模式留门。
2. **编排与算法分离**：工具的"执行编排"（依赖全局状态的薄壳）与"纯算法核心"（输入→输出的函数）分层，算法层 100% 单测覆盖。
3. **双通道工具结果**：前端收完整 JSON；模型上下文收压缩版（head/tail 截断、配额上限）。read 类结果不压缩（保留行号供 edit 定位）。
4. **Cache-first 上下文**：所有进入请求前缀的内容（系统提示词、工具 schema、workspace map）必须**字节级稳定**（排序确定、序列化确定、瞬态内容后置），以命中 provider prompt cache。
5. **一切皆文件、原子写**：无数据库；`~/.codewave/` 下 JSON + gzip；临时文件 + rename；Windows 加 `.bak` 回滚。
6. **防御性历史修复**：任何进入持久化/回放路径的消息数组，必经 sanitize → trim → repair 三段管线，保证 tool_use/tool_result 配对不变式，杜绝"会话毒化成永久 400"。
7. **围栏优先、弹窗兜底**：命令安全先走零交互的静态围栏；仅高危/越界场景触发确认弹窗（D4 决策）。
8. **宽容输入、严格输出**：解析模型工具调用参数时宽容（未知字段→warnings、类型错误自动修复）；写入磁盘与发给模型的内容严格校验。

---

# 2. 总体架构

## 2.1 进程模型

**单进程**：Tauri 2 主进程（Rust）内含全部 Agent 逻辑；WebView 只承载 UI。无 sidecar（D2）。

```
┌──────────────────────── CodeWave.app（单进程）────────────────────────┐
│                                                                         │
│  WebView（Vue 3 + Naive UI + Pinia）                                     │
│  ├─ ChatView（流式渲染）  ToolCards  DiffView  Settings  WorkspaceExplorer │
│  │        ▲ invoke(command)                ▲ Channel（高频流）+ emit（低频）│
│  └────────┴────────────────────────────────┴──────────────────────────┐ │
│                                                        IPC（serde JSON）│ │
│  Rust 后端                                                              │ │
│  ┌─ host/ ── 唯一 import tauri：commands、eventSink、窗口/托盘/通知 ──┐  │ │
│  │                                                                    │  │ │
│  │  core/（agent 主循环、会话、上下文、提示词）                          │  │ │
│  │  provider/（三协议适配、多key池、代理/SSRF）                         │  │ │
│  │  tools/（注册表+批次策略+各工具：算法层/编排层分离）                  │  │ │
│  │  safety/（围栏 AST、路径边界、确认弹窗协调）                          │  │ │
│  │  git/（git2-rs：status/diff 只读）  mcp/ skills/ memory/ scheduler/  │  │ │
│  └────────────────────────────────────────────────────────────────────┘  │ │
│   ~/.codewave/（config.json、sessions/、histories/、memories/、stats/）│
└─────────────────────────────────────────────────────────────────────────┘
```

## 2.2 模块依赖规则

```
host ──▶ core ──▶ provider
  │        │  └──▶ tools ──▶ safety
  │        └──▶ sessions / context / prompt（core 子模块）
  └──▶ mcp / skills / memory / scheduler / stats / git
禁止：core/tools/safety/provider 反向依赖 host；tools 之间禁止横向调用（经 registry）。
```

---

# 3. 技术栈与依赖清单

## 3.1 框架与工具链

| 项 | 选型 | 版本基线（2026-08 核实） |
|---|---|---|
| 桌面框架 | tauri + tauri-build + tauri-cli | 2.11.x |
| 前端 | Vue 3 + Vite + TypeScript | Vue 3.5 / Vite 7 |
| UI 组件库 | Naive UI（unplugin 按需导入） | 2.44+ |
| 状态管理 | Pinia | 3.x |
| TS 绑定生成 | tauri-specta（失败则手写 invoke 封装 + zod） | v2 |
| 包管理 | pnpm（前端）、cargo（Rust） | — |
| cargo 镜像 | rsproxy.cn（可选，国内加速；对齐既有镜像偏好） | — |

## 3.2 Rust crate 清单

| crate | 用途 | 备注 |
|---|---|---|
| tokio（full） | 异步运行时 | Semaphore=工具并发、mpsc=注入队列 |
| tokio-util | CancellationToken | 取消树：run → 工具批 → 子代理 |
| reqwest（json, socks, stream, gzip/brotli） | 全部 HTTP | 代理感知 Client 池；SOCKS5 支持 |
| eventsource-stream | SSE 解析 | 三协议适配共用 |
| serde / serde_json / serde_yaml | 序列化 | serde_ignored 收集未知字段；frontmatter 解析 |
| schemars | 工具 JSON Schema | 手动 enforce `additionalProperties:false` |
| async-openai | OpenAI Chat 协议 | 社区主力；仅取其类型与流式，外面包 Provider trait |
| （自写）anthropic 适配 | Anthropic Messages 协议 | reqwest+SSE 自写薄层（无官方 Rust SDK，见 §4.2） |
| rmcp | MCP 客户端（P1） | modelcontextprotocol 官方 Rust SDK；stdio/SSE/streamable-http |
| git2 | git 只读（status/diff/log） | D5 决策；libgit2 由 libgit2-sys 源码自编译（需 cmake），无系统安装依赖 |
| tree-sitter + tree-sitter-bash | 命令 AST 解析 | 安全围栏 §4.4 |
| grep-regex + grep-searcher + ignore | grep 工具引擎 | **ripgrep 官方库 crate**：内嵌同源引擎，纯 Rust，无需捆绑 rg 二进制 |
| similar | diff 计算 | edit 卡片/GitDiffModal |
| dom_smoothie | 网页正文抽取（P1） | web_fetch 的 Readability 实现 |
| cron | 计划任务（P2） | 5 字段表达式校验（内部前补秒位）；调度由自写 tick 驱动，不引入 tokio-cron-scheduler（该依赖已移除） |
| flate2 + tempfile | gzip 会话存档、原子写 | — |
| keyring | API key 存系统钥匙串（P1） | 服务名 `codewave.yangshifu.xyz`；P0 暂明文 config.json |
| thiserror / anyhow | 错误处理 | 库层 thiserror、host 层 anyhow |
| tracing + tracing-subscriber + tracing-appender | 日志 | 滚动文件 `~/.codewave/logs/` |
| dashmap | 并发 map | SessionRuntime 注册表 |
| tiktoken-rs（可选） | token 估算 | 或字符数/4 启发式 + provider usage 校正 |

## 3.3 前端 npm 依赖

`naive-ui`、`pinia`、`vue-i18n`、`markdown-it` + `@traptitech/markdown-it-katex` + `katex`、`mermaid`、`highlight.js`、`ace-builds`、`@tauri-apps/api`、`@tauri-apps/plugin-*`（dialog/notification/clipboard-manager/process）、`diff`（前端行内 diff 展示）。

---

# 4. Rust 后端详细设计

## 4.1 core::agent —— 主循环

### 4.1.1 核心类型

```rust
pub struct AgentCore {
    registry: DashMap<SessionId, SessionRuntime>,
    cfg: RwLock<ConfigState>,
    event_sink: Arc<EventSink>,          // host 注入，core 只依赖 trait
    tools: Arc<ToolRegistry>,
    provider_pool: Arc<ProviderPool>,
    store: Arc<SessionStore>,
}

pub struct SessionRuntime {
    id: SessionId,
    workspace: PathBuf,
    cancel: CancellationToken,           // 取消树根
    inject_tx: mpsc::Sender<Message>,    // 运行中消息注入（容量 32，满则拒绝）
    history: Mutex<Vec<Message>>,        // 会话历史（内存态）
    run_lock: AsyncMutex<()>,            // 单会话单 run 守卫
    sem_tools: Arc<Semaphore>,           // 工具并发上限 4
    file_ops: Arc<AsyncMutex<()>>,       // 文件写串行
    workspace_map: OnceLock<WorkspaceMap>, // 会话内冻结（cache-first）
    model_pref: RwLock<ModelId>,         // 每工作区模型偏好
}
```

### 4.1.2 主循环状态机

```
StartChat(req)
  → 校验配置 → 抢 run_lock（否则拒）
  → spawn run loop：
    for step in 0..MAX_STEPS(=9999):
      ① cancel 检查 → 追加 <cancelled> 标记 → 检查点保存 → 结束
      ② drain 注入队列 → emit run:inject
      ③ 更新实时 token 明细 → emit tokens:update
      ④ if 上下文用量 > CompactThreshold(默认 0.6)：compact_history()（§4.6）
      ⑤ stream_model()（§4.2）
         ├─ Ok(delta 流) → 经 Channel 推前端
         ├─ 可重试错误 → 指数退避 500ms×2ⁿ ≤10s，重试 ≤6 次；emit run:retry
         └─ 400 类错误 → sanitize 历史后免费重试一次
      ⑥ 无 tool_calls → save_history + emit run:done → 结束
      ⑦ 有 tool_calls → execute_batch()（§4.3）→ 结果回填 → 下一 step
```

取消语义：`cancel_run()` 触发 CancellationToken；已完成的工具调用与部分回复经检查点保存；历史追加用户可见的取消标记（区别于报错）。

## 4.2 provider —— LLM 适配层

### 4.2.1 统一消息 DTO（协议中立中间表示）

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

三协议适配器都以此为源/汇。**序列化字节稳定性**是硬约束（cache-first）：字段顺序、空白、排序规则固定（serde 结构体天然保序；列表显式排序）。

### 4.2.2 Provider trait 与三个实现

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    async fn stream(&self, req: StreamRequest, tx: mpsc::Sender<StreamDelta>)
        -> Result<RunUsage, ProviderError>;
}
```

| 实现 | 协议 | 基础设施 | 阶段 |
|---|---|---|---|
| `OpenAiChatProvider` | openai_chat（兼容 Ollama/DeepSeek/LM Studio 等） | async-openai 类型 + eventsource-stream | **P0** |
| `AnthropicProvider` | anthropic_messages | **reqwest + eventsource-stream 自写**（官方无 Rust SDK；仅实现 Messages 流式端点 + cache_control） | **P0** |
| `OpenAiResponsesProvider` | openai_responses | reqwest 自写（含 prompt cache key 路由） | P1 |

> 自写 Anthropic 适配的理由：社区 crate（async-anthropic 等）质量波动且未必覆盖 cache_control 断点；Messages 流式协议面窄（message_start / content_block_delta / message_delta / message_stop），薄层约 600–800 行，包在 Provider trait 后可随时替换。SDK 选型变更时须全文清扫引用。

### 4.2.3 多 Key 池与故障转移

- `ModelConfig.keys: Vec<String>` 有序池；`KeyHealth { cool_until, last_error }` 表。
- 选择策略：取第一个未冷却 key；认证类错误（401/403/quota）冷却 **30min**，瞬时错误（429/5xx/网络）冷却 **10s**；全池冷却时取最早恢复者直接尝试。
- 适配器内不重试（多 key 时避免 N×N 组合）；重试统一由主循环调度。

### 4.2.4 Prompt Cache 优化（P0 即内置）

- Anthropic：最后一个 system 块 + 最后一条非瞬态消息打 `cache_control: {type:"ephemeral"}`；瞬态注入（如剩余预算提示）必须置于缓存断点之后。
- OpenAI Responses（P1）：按会话稳定 cache key + 边界锚文本。
- workspace map（目录快照）按会话冻结：首个请求生成，会话内复用，保证跨 run 前缀字节稳定。

### 4.2.5 代理与 SSRF

- `proxy.rs`：手动配置（HTTP/HTTPS/SOCKS5）> 系统代理探测（macOS `scutil --proxy` 解析 / Windows 注册表 / Linux 环境变量）> 无代理；手动配置存在时 fail-closed（探测失败不用直连兜底）。
- **SSRF 守卫**：自定义 reqwest Connector，对目标 IP 做私网段校验（含重定向后逐跳校验），`allow_private_network` 开关放行本地模型场景。

## 4.3 tools —— 工具系统

### 4.3.1 Tool trait 与注册表

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> &'static str;          // strict JSON Schema（编译期内嵌）
    fn kind(&self) -> ToolKind;                // ReadOnly / FileWrite / Network / Interactive / Meta
    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutcome;
}

pub struct ToolCtx {                           // 编排层注入，算法层不接触
    workspace: PathBuf,
    write_roots: Vec<PathBuf>,                 // 工作区 + extraRoots + ~/.codewave
    runtime: Arc<SessionRuntime>,
    emit: ToolEmitter,                         // tool:progress 通道（节流后）
    approval: ApprovalGate,                    // 确认弹窗门（§4.4）
}
```

- **P0 工具（8 个）**：`read`、`edit`、`create`、`list_files`、`delete`、`command`、`grep`、`ask`（弹窗与提问共用通道，见 §4.4）
- **P1 工具**：`batch_read`、`web_fetch`、`http_request`、`service`、`wait`、`suggest`、`plan`、`calculate`、`render_html`、`skill`
- **P2 工具**：`subagent`、`scheduled_task`、SSH remote 系列

### 4.3.2 edit 工具契约（核心，单独说明）

- 入参：`{ files: [{ path, version, changes: [{ oldText?, newText } | { lineRange?, newText }] }] }`。
- **乐观并发令牌 `version`**：read 结果附带 6 位 Crockford Base32 的 SHA-256 前缀；edit 时校验，不匹配拒绝（防基于过期内容覆写）。
- 应用顺序：跨文件倒序应用；oldText 精确匹配优先，未命中再按 lineRange；逐 change 计算 diff（similar crate）；写前生成回滚快照，失败回滚。
- 截断参数抢救：对疑似被 max_tokens 截断的 JSON 参数尝试补全修复，修复不了才拒执行。

### 4.3.3 批次执行策略

```
一个 assistant 回复含 N 个 tool_calls：
  1. 校验批次：ask/wait/suggest 类 Interactive 工具必须是批次唯一调用 → 否则整批拒绝
  2. 同批次对同一物理路径（canonicalize 后）的多个写操作 → 全部拒绝（批次冲突）
  3. FileWrite 类按 toolCallIndex 串行（file_ops 锁）；其余并行（Semaphore=4）
  4. 每工具：panic catch_unwind → E_TOOL_PANIC；执行完成即 emit tool:result / tool:error
```

### 4.3.4 双通道结果

```rust
struct ToolOutcome { ok, data: Value, error: Option<ToolError>, warnings: Vec<String> }
// 前端通道：完整 ToolOutcome（tool:result 事件）
// 模型通道：compact_for_model() —— head 4KB + tail 8KB 截断、grep ≤200 条、
//          tool output 总配额 128KB/步；read 不压缩（保行号）
```

### 4.3.5 宽容参数解码

`serde_json` 手动解码：未知字段 → `serde_ignored` 收集为 warnings 回填模型；类型错误（如双重编码字符串）尝试自动修复；修复失败且疑似截断 → 走抢救；否则拒执行并给出可自纠的错误说明。

### 4.3.6 写入后检查（P1；2026-09-20 重定）

edit/create 成功后执行用户配置的一条检查命令（`post_write_check`，在项目根目录运行，支持 `{file}` 占位符），
输出尾部随工具结果的 `outcome.data`（`check` / `checks` 字段）交给模型："文件已写入；检查输出：\<尾部\>，请修复"。
命令由用户按项目配置、执行环境即项目环境，故不需要写前基准 / 差集 / 就绪判据。默认关闭。
（历史：P1 曾实现「六语言 LSP 语义校验」，因写后结论只达前端 warnings、模型读不到而废弃，
见 [post-write-check-plan](./post-write-check-plan.md)。）

## 4.4 safety —— 安全围栏与确认弹窗（D4 决策）

### 4.4.1 围栏三层（command 工具执行前）

```
L1 删除黑名单：rm/rmdir/unlink/del/rd/Remove-Item … → 拦截，错误信息指路 delete 工具
L2 写目标分析（tree-sitter-bash AST）：
   - 提取重定向目标（> >> 2> &>）、tee/dd/cp/mv 目标、heredoc 落点、命令替换内的写目标
   - 目标路径 canonicalize（含 symlink 逃逸检测）∉ write_roots 且目标已存在 → 拦截
   - 新路径创建放行；/dev/null 放行
L3 高危模式：chmod 777、mkfs、dd of=/dev/…、curl | sh 类 → 触发确认弹窗（而非直接拦）
```

### 4.4.2 确认弹窗门（ApprovalGate）

```rust
pub enum ApprovalVerdict { Approved, Denied }
async fn request(&self, req: ApprovalRequest) -> ApprovalVerdict {
    // emit ask:opened {kind:"approval", command, reason}
    // 挂起等待前端 resolve（复用 ask 交互协议）
    // [docs/session-nav-row-states](./session-nav-row-states.md)：默认永不超时、无限等待；ApprovalSettings.auto_confirm 勾选后 5 分钟无响应自动确认推荐选项（允许）
}
```

- 触发条件（默认开，Settings 可关；关闭后 L3 类直接拦截而非放行）：
  1. L3 高危模式命中；
  2. L2 中"工作区外**新建**路径"（放行但提示性确认，可配置）；
  3. git push / force 操作（P1 起，提示词+弹窗双保险）。
- 弹窗 UI 复用 ask 工具卡片通道（同一 `ask:opened` 事件 + `resolve_ask` command），不新增协议。

### 4.4.3 路径边界（pathutil 模块，唯一实现）

- `safe_join(root, rel)`：拒绝 `..` 逃逸、绝对路径注入、Windows 保留名；
- `resolve_write(path)`：canonicalize（含中间 symlink）后必须落在 write_roots 内；
- 危险删除守卫：拒绝 `/`、工作区根、`.git/`、`~`、系统目录；
- 网络工具 SSRF 走 §4.2.5 同一守卫。

## 4.5 core::sessions —— 会话持久化与修复管线

### 4.5.1 存储格式（无数据库）

```
~/.codewave/
├── sessions/index.json        # 索引：{id, title, workspace, model, updated_at, …}，≤2000 条（LRU 淘汰）
├── sessions/<id>.snap.json.gz # 会话元信息快照，≤16MB
├── histories/<id>.json.gz     # PersistedMessage[] 的 gzip JSON，≤8MB（图片以 blob 引用落盘）
├── sessions/<id>.imgblob/     # 图片外置 blob（sha256 内容寻址去重；[session-history-limits](./session-history-limits.md)）
```

原子写：`tempfile` + rename；Windows 先 `.bak` 备份。保存时机：run 结束 / 取消检查点 / 每 20 步。

### 4.5.2 修复管线（保存与加载双向执行）

```
sanitize：剥离 system 消息与图片 payload；多 content 块归一；修复截断的 tool args；折叠重复 tool 名
trim    ：按 256K token 预算在 user 消息边界裁剪，保证 tool_use/tool_result 配对完整
repair  ：剥离悬空 tool_use；丢弃孤儿 tool_result；tool_use id 配对校验
```

## 4.6 core::context —— token 统计与自动压缩

- **ContextBreakdown**：system / history / tool_results / tool_schemas 四项分项（30s 缓存），`tokens:update` 推前端信息条；usage 优先取 provider 上报，缺失按字符估算。
- **自动压缩**：用量 > `compact_threshold`(0.6) 触发；结构化摘要提示词（最新请求 / 已完成 / 当前状态 / 下一步 / 关键文件），替换整段历史并加 handoff 前缀；`/compact` 命令手动触发（保留最后一条 user 消息）。
- 摘要请求本身走同一 Provider 层，超时 3min，maxTokens 8000。

## 4.7 core::prompt —— 系统提示词六层组装（全部自有文案）

| 层 | 内容 | 优先级标签 |
|---|---|---|
| 1 | 核心行为规范：优先级总声明、讨论/实现判别、工具策略、批量与编辑纪律、输出风格、安全边界、平台信息 | `<system-prompt priority="core">` |
| 2 | Skills 元数据（仅 name/description/whenToUse 索引） | core 内 |
| 3 | 记忆索引（`memories/*.md` 路径+描述，≤200 条） | core 内 |
| 4 | 项目指令：工作区 `CODEWAVE.md`（新）/ `AGENTS.md` / `CLAUDE.md`（兼容既有生态习惯；改名前旧名 `WAVESTUDIO.md` 仍兼容读取，新名在场时跳过） | `<project-instructions>` |
| 5 | 项目图谱：`CODEGRAPH.md`（若存在）+ `.codewave/lessons.md` 经验 | 低优先级 |
| 6 | 用户自定义提示词 | `<custom-instructions>` |

组装规则：各层文本由本团队撰写；层内列表显式排序；层与层之间固定分隔符 → 整体前缀字节稳定。

## 4.8 git —— git2-rs 模块（D5 决策）

- **定位**：只读集成——status / diff / recent log，服务"Agent 改动前后的变更展示"，不做写操作（stage/commit 由用户在终端或未来版本进行）。
- **P0（G8，构建风险前移）**：引入 git2 依赖，`git_status()` 单命令跑通三平台 CI（libgit2-sys 需要 cmake——macOS/CI 自带；Windows 构建在 P0 验证，问题前置暴露）。
- **P1**：`git_diff(path)`（工作树 vs HEAD，similar 渲染）、`git_recent_log(n)`；UI：工作区变更面板 + edit 工具卡内嵌 diff。
- 不启用 `ssh` feature（无远程操作需求，避免 libssh2 构建负担）。

## 4.9 mcp / skills / memory / scheduler / stats（P1 概要）

| 模块 | 设计要点 |
|---|---|
| mcp/ | rmcp 客户端池；配置 = 用户级 `~/.codewave/mcp.json` + 工作区 `mcp.json` 叠加；stdio（Windows 隐藏窗口 + Job Object 防孤儿）/sse/streamable-http；函数名 `mcp__<server>__<tool>`，三键排序；invalid session 自动重连；工具以 `[server]` 描述前缀追加在内置工具后 |
| skills/ | 扫描：项目 `.codewave/skills/` > 用户 `~/.codewave/skills/` > 工作区 `.agents/skills/`、`.claude/skills/` > 用户级 `~/.agents/skills/`、`~/.claude/skills/`（生态兼容）> 内置（`include_str!` 嵌入 2–3 个自研 skill）；TTL 索引 + invalidate 重载；托管目录技能可删（deletable 标记 + canonicalize 前缀双校验）；YAML frontmatter；系统提示词仅注入索引；斜杠命令与 `skill` 工具双通道 |
| memory/ | `~/.codewave/memories/*.md`；无专用工具——目录在 write_roots 白名单，模型直接用 create/edit 维护；索引注入；`/remember` 辅助 |
| scheduler/ | `cron` crate 校验表达式 + 自写 tick 推进（**无** tokio-cron-scheduler）；任务随项目**落盘**（`<主目录>/.codewave/projects/<project_id>/tasks/<id>.json`，自由会话任务仅进程内）；全局串行执行（`exec_lock`）；每 run 隔离上下文；暂停开关 / 立即运行 / 执行历史（每任务最近 20 条）（P2，见 [docs/tasks-module-polish](./tasks-module-polish.md)） |
| stats/ | 异步有界队列（2048）写 `stats/<date>.json`，90 天保留；按天/模型/工作区聚合（P2） |

## 4.10 host —— 唯一框架边界

- `commands.rs`：全部 `#[tauri::command]`（清单见 §5.1），只做参数校验 + 转调 core，不含业务逻辑；
- `events.rs`：`EventSink` trait 的 Tauri 实现——高频走 `tauri::ipc::Channel`，低频走 `AppHandle::emit`；core 依赖的是 `EventSink` trait（host 注入实现）；
- `window.rs`：无边框窗口控制（`decorations:false` + `data-tauri-drag-region`）、单实例聚焦（single-instance 插件）、通知（notification 插件，失败静默）；
- 托盘：Tauri 2 内置 `TrayIconBuilder`（P2 启用）。

---

# 5. IPC 协议（Commands 与 Events）

## 5.1 Commands（前端 invoke，分阶段）

| 阶段 | Command | 说明 |
|---|---|---|
| P0 | `start_chat(req, on_event: Channel)` | 启动 run；Channel 接收高频流帧 |
| P0 | `cancel_run(run_id)` / `inject_run_message(session, text)` | 取消 / 运行中注入 |
| P0 | `resolve_ask(ask_id, payload)` | 应答 ask/审批弹窗 |
| P0 | `get_config` / `save_config(cfg)` | 配置读写（含模型 CRUD） |
| P0 | `list_sessions` / `load_session(id)` / `delete_session(id)` / `rename_session` | 会话管理 |
| P0 | `compact_session(id)` | 手动压缩 |
| P0 | `search_workspace_paths(query, limit)` | @文件提及的路径索引 |
| P0 | `read_workspace_file(path)` / `save_workspace_file(path, content)` | WorkspaceExplorer 编辑器 |
| P0 | `git_status()` | git2 构建验证 + 最小功能 |
| P0 | `get_token_breakdown(session)` | 上下文信息条 |
| P1 | `switch_model(workspace, model_id)` / `list_models` | 多模型 |
| P1 | `mcp_list_config` / `mcp_save_config` / `mcp_connect` / `mcp_disconnect` / `mcp_reconnect` / `mcp_test` / `mcp_snapshot` | MCP 管理（作用域化；[mcp-module-rebuild](./mcp-module-rebuild.md)） |
| P1 | `list_skills` / `toggle_skill(name, disabled)` | Skills |
| P1 | `git_diff(path)` / `git_recent_log(n)` | git 只读 |
| P1 | `start_service` / `stop_service` / `read_service_log` | 后台进程 |
| P2 | `create_scheduled_task` / `list_scheduled_tasks` / `delete_scheduled_task` | 计划任务 |
| P2 | `get_token_stats(range)` | 统计图表 |
| P2 | `check_updates` 等 | updater（私有阶段可挂空实现） |

## 5.2 事件协议

**Channel 高频帧**（`start_chat` 传入的 Channel，有序、点对点）：

| type | 载荷 | 说明 |
|---|---|---|
| `delta` | `{text, reasoning?}` | 流式文本增量（后端 64ms 时间节流合并） |
| `tool_progress` | `{batch, index, chunk}` | 工具执行中间输出（200ms/2KB 门控） |
| `usage` | `{input, output, cache_hit, cache_miss}` | 单步 usage |

**全局事件**（`emit`，低频生命周期；命名空间 `域:动作`）：

| 事件 | 时机 |
|---|---|
| `run:start` / `run:done` / `run:error` / `run:retry` / `run:inject` / `run:compacted` | run 生命周期 |
| `tool:result` / `tool:error` | 每工具完成（定位键 `runId:batchId:callIndex`） |
| `ask:opened` / `ask:closed` | ask 工具与审批弹窗（共用） |
| `plan:update` | plan 工具状态机 |
| `tokens:reset` | 换模型/清空 |
| `mcp:status` | MCP 连接状态（P1） |
| `sub:spawn` / `sub:done` / `sub:error` / `sub:tool` | 子代理（P2） |
| `app:config_warning` | 配置问题提示 |

类型安全：tauri-specta 从 Rust 生成 `bindings.ts`（command 签名 + 事件载荷类型）；若 specta 与当版 Tauri 兼容性问题，降级为手写 `ipc/` 封装层 + zod 校验（接口面不变）。

---

# 6. 前端设计

## 6.1 目录结构

```
frontend/src/
├── main.ts / App.vue             # 壳：布局 + 全局快捷键 + <300 行
├── ipc/                          # specta 生成 bindings + 事件订阅封装（唯一 touch @tauri-apps 的目录）
├── stores/                       # Pinia
│   ├── sessions.ts               # 会话列表/当前会话/多 Tab
│   ├── run.ts                    # 活跃 run 状态：流缓冲、工具卡、ask 队列、token 条
│   ├── settings.ts               # 配置镜像（启动时 get_config 拉取，保存走 command）
│   └── ui.ts                     # 主题、字号、弹窗开关
├── features/
│   ├── chat/                     # ChatMessages / StreamingMarkdown / Composer / CommandMenu / FileMention
│   ├── tools/                    # ToolCallCard 体系（动词表）、AskCard、ApprovalDialog、ReadGroupCard
│   ├── diff/                     # DiffView、行内 diff、GitDiffModal(P1)
│   ├── workspace/                # 多 Tab 头、WorkspaceExplorer（文件树 + Ace）
│   ├── subagent/                 # SubagentInlineCard（P2）
│   └── panels/                   # SettingsModal（5 分区）、TaskCenter(P2)、TokenStats(P2)
├── i18n/                         # vue-i18n：zh-CN / en-US
├── theme/                        # 暗色基座 + 强调色变量（CSS color-mix 派生）
└── utils/                        # diff 计算、markdown 渲染器工厂、纯函数（配 vitest 单测）
```

## 6.2 流式渲染管线（性能关键路径）

```
后端 64ms 节流 delta → Channel → Pinia run.ts 追加缓冲
  → rAF 合帧（~16ms）flush 到消息模型
  → StreamingMarkdown 组件：仅当前流式消息独立渲染作用域
  → 已完成消息走 RenderBoundary 缓存（LRU 16 条 / 2M 字符），mermaid 视口内懒渲染
```

红线指标：长回复（>50K 字符）滚动不掉帧；100+ 消息会话切换 <100ms。

## 6.3 工具卡体系

- `ToolCallCard`：统一壳（状态图标 + "Running/Used <动词>" 表 + 折叠正文）；按工具名注册**展示适配器**（read→文件预览、edit/create→DiffView、command→终端输出、grep→命中分组、ask→问答卡、approval→确认弹窗卡）。
- 定位：`tool:result` 事件按 `runId:batchId:callIndex` 插桩；流式中的 `tool_progress` 只更新"运行中"占位卡。

## 6.4 快捷键与窗口

Esc 停止 run、`Cmd/Ctrl+T` 新 Tab、`Cmd/Ctrl+W` 关 Tab、`Cmd/Ctrl+N` 新会话、`Cmd/Ctrl+←/→` 切 Tab、`/` 命令菜单、`@` 文件提及。无边框窗口 + 拖拽区 + 单实例聚焦。

---

# 7. 数据与配置布局

```
~/.codewave/
├── config.json          # 主配置：models[]（provider/apiFormat/baseUrl/keys[]/contextWindow/
│                        #   reasoningEffort…）、proxy、compact_threshold、ui、post_write_check 命令…
│                        #   （P0 明文存 key；P1 keyring 迁移，服务名 codewave.yangshifu.xyz）
├── mcp.json             # 用户级 MCP 配置（P1）
├── sessions/  histories/ # §4.5
├── memories/            # 跨项目记忆 *.md（P1）
├── skills/              # 用户级 skills（P1）
├── stats/               # <date>.json（P2）
├── logs/                # tracing 滚动日志
└── tmp/                 # 命令输出落盘等临时文件
工作区内：
  .codewave/           # lessons.md、mcp.json（项目级）、skills/（项目级）
  CODEWAVE.md / AGENTS.md / CLAUDE.md   # 项目指令（兼容既有习惯）
```

配置迁移策略：`Option<T>`/`#[serde(default)]` 承载全部可增字段，旧 config.json 无新字段时透明取默认值。

---

# 8. 安全模型（三层防线）

| 层 | 机制 | 来源 |
|---|---|---|
| ① Agent 行为围栏 | §4.4：删除黑名单 → AST 写目标分析（tree-sitter-bash）→ 高危模式；路径边界 pathutil；SSRF 守卫 | 独立实现 |
| ② 确认弹窗（D4） | ApprovalGate：高危命中/越界新建/git push 触发；复用 ask 通道；默认开、Settings 可关 | **本方案新增** |
| ③ Tauri capability/CSP | capabilities 仅授权所需插件（fs scope 限 `~/.codewave` 与工作区、shell 默认全禁）；CSP 禁 remote origins；render_html 用 sandbox iframe | Tauri 2 原生 |
| 提示词纪律 | 注入防御（工具输出/网页内容是数据不是指令）、git push 需确认、敏感文件不读 | §4.7 层 1 |

---

# 9. 构建、CI 与发布

## 9.1 本地开发

```
pnpm i && pnpm dev          # 前端 HMR
cargo tauri dev             # 整体开发模式
cargo tauri build           # 产物：dmg/app（macOS 优先）
```

tauri.conf.json 要点：`identifier: "xyz.yangshifu.codewave"`、`productName: "CodeWave"`、`decorations:false`、CSP 白名单自资产、`externalBin`：无（ripgrep 用库 crate，无外部二进制）。

## 9.2 CI（GitHub Actions）

- 触发：push/PR；矩阵：`macos-14`（arm64，主开发平台）+ `ubuntu-24.04`（WebKitGTK 冒烟）；`windows-2022` 从 G8 起加入（git2/libgit2 构建验证）。
- 步骤：pnpm install+build → cargo test（core/tools/safety 必须全绿）→ `cargo tauri build`（macOS 出产物，其余平台编译验证）。
- Rust 工具链 stable；可选 rsproxy.cn 加速；Node 20/22。

## 9.3 发布（D7：私有起步）

- 分发：CI artifact 或本地 `cargo tauri build` 产物手动安装；**不做**签名/公证/更新服务器。
- updater 预留：P2 若需要再启用 tauri-plugin-updater（需生成签名密钥与静态更新源，届时补文档）。

---

# 10. 里程碑与验收

## P0 —— 核心可用闭环（预估 3 周；Rust 生疏则 4–5 周）

| 目标 | 内容 | 验收标准 |
|---|---|---|
| G1 | 脚手架：Tauri 2 + Vue3 + Naive UI + Pinia + specta；CI 双平台绿 | macOS 构建出可运行 app；CI lint/test/build 全绿 |
| G2 | 配置与数据目录：config.json 读写、`~/.codewave` 初始化、模型配置 CRUD UI | 重启配置不丢；旧字段透明兼容 |
| G3 | Provider 层：openai_chat + anthropic_messages 流式、SSE、重试退避、usage | 真实 API 流式对话；断网重试符合退避曲线 |
| G4 | Agent 主循环：run/注入/取消/单会话单 run/检查点 | 运行中追加消息生效；Esc 后已完成工具结果保留 |
| G5 | 工具框架 + 8 工具：read/edit/create/list_files/delete/command/grep/ask；批次策略、双通道、panic 兜底 | 端到端"读→改→跑测试"闭环；edit 并发令牌拒绝过期写入 |
| G6 | 安全围栏 v1 + 审批弹窗：三层围栏（tree-sitter-bash）+ ApprovalGate | `rm` 拦截改道；越界写拦截；高危命令弹窗可批可拒 |
| G7 | 会话持久化 + 修复管线 | 构造悬空 tool_use 的历史，加载后自动修复且能继续对话 |
| G8 | git2-rs 引入 + `git_status()` + Windows CI 验证 | 三平台 CI 构建/测试绿（构建风险前置消除） |
| G9 | 基础聊天 UI：流式 Markdown/代码高亮、工具卡 v1、会话列表、设置弹窗、上下文信息条 | 长回复流畅（§6.2 红线）；中英 i18n 框架就绪 |
| G10 | 上下文管理 v1：token 分项统计 + 60% 自动压缩 | 压缩后对话连贯；信息条数字与请求一致 |

## P1（约 3–4 周）

剩余工具（web_fetch/http_request/service/wait/suggest/plan/batch_read/calculate/render_html/skill）、写入后语义校验、多 Key 池、OpenAI Responses 协议、MCP 完整、Skills 完整、记忆、多 Tab 工作区、WorkspaceExplorer、GitDiffModal（git2）、model catalog（models.dev 快照脚本生成）、keyring 迁移、主题与 i18n 完整、审批策略细化为设置项矩阵。

## P2 —— 完全体（约 3 周）

子代理、计划任务、Token 统计图表、托盘/系统通知、updater、SSH 远程工作区、server 模式（core 层 + axum）、移动端可行性评估。

---

# 11. 风险清单与缓解

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R1 | tree-sitter-bash 写目标分析正确性（自研核心） | 围栏漏拦/误拦 | 用例集覆盖重定向/heredoc/命令替换/symlink；L3 一律走弹窗兜底；误拦仅表现为多一次确认 |
| R2 | 自写 Anthropic SSE 适配的协议细节（event 类型、配对、partial json） | 流式中断 | 用例回放真实抓包流；Provider trait 隔离可随时换社区 crate |
| R3 | git2/libgit2 三平台构建（cmake） | CI 变重/Windows 失败 | G8 前移验证；不启用 ssh feature；必要时 Windows 降级系统 git 调用（仅构建兜底，接口不变） |
| R4 | tauri-specta 与当版 Tauri 兼容 | 类型生成受阻 | 接口面按 §5 定义冻结；降级手写 ipc 封装 + zod |
| R5 | WebKitGTK（Linux）渲染差异 | 冒烟通过但体验差 | Linux 仅保编译+基础冒烟，主投 macOS；已知问题清单化 |
| R6 | WKWebView IndexedDB 稳定性 | 会话快照丢失 | 后端文件为唯一权威存储，IndexedDB 仅 UI 缓存 |
| R7 | 私有项目无签名 → macOS Gatekeeper | 自用安装麻烦 | 首次右键打开/ad-hoc 签名文档化；不影响开发 |
| R8 | 单人 Rust 产能 | 里程碑顺延 | P0 严格时间盒；工具算法层先行（纯函数易写易测）；每周里程碑复审 |

---

*本文档为 CodeWave 实施基准；执行中如变更 D1–D8 任一决策，须同步更新对应表格并全文清扫引用。*
