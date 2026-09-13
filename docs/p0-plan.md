# CodeWave P0 详细技术方案（核心可用闭环 G1–G10）

> 日期：2026-08-30
> 上游文档：`[docs/technical-design](./technical-design.md).md`（基准方案，P0 定义见其 §10）
> 本文粒度：可直接开工——文件路径、类型签名、算法伪代码、测试用例、验收标准
> 时间盒：3 周（Rust 生疏 4–5 周）；每个 G 结束须勾选其 DoD

---

## 1. P0 范围总览

| 目标 | 内容 | 产出 | 依赖 |
|---|---|---|---|
| G1 | 项目脚手架 + CI | 可构建运行的空壳 app + CI 全绿 | — |
| G2 | 配置与数据目录 | `~/.codewave/` 体系 + 模型配置 CRUD | G1 |
| G3 | Provider 层（双协议流式） | openai_chat + anthropic_messages + 重试 | G2 |
| G4 | Agent 主循环 | run/注入/取消/检查点 | G3 |
| G5 | 工具框架 + 8 工具 | read/edit/create/list_files/delete/command/grep/ask | G4 |
| G6 | 安全围栏 v1 + 审批弹窗 | 三层围栏 + ApprovalGate | G5 |
| G7 | 会话持久化 + 修复管线 | 存档/加载/修复 | G4 |
| G8 | git2-rs 引入 + git_status | 构建验证 + 最小命令 | G1（可与 G2–G7 并行） |
| G9 | 基础聊天 UI | 流式对话/工具卡/会话列表/设置 | G4, G5, G7 |
| G10 | 上下文管理 v1 | token 分项 + 自动压缩 | G3, G4 |

依赖图：`G1 → G2 → G3 → G4 → {G5, G7} → G6 → G9 → G10`；G8 并行；G9 与 G6/G7 可交错。

**端到端目标场景（P0 完成的定义）**：打开工作区 → 配置模型 → 对话"修复 X 并跑测试" → Agent 流式回复 → read/edit/command/grep 交替执行（含一次 rm 被拦截改道 delete）→ 完成后重启 app，会话与历史完整恢复。

---

## 2. G1 项目脚手架

### 2.1 仓库结构（本目标产出完整骨架）

```
CodeWave/
├── src-tauri/
│   ├── Cargo.toml                # 单 crate（P0 不拆 workspace）
│   ├── build.rs                  # tauri_build::build()
│   ├── tauri.conf.json
│   ├── capabilities/default.json
│   ├── icons/                    # tauri icon 生成
│   └── src/
│       ├── main.rs               # 薄入口：调 lib::run()
│       ├── lib.rs                # Builder 装配 + specta 导出 bindings.ts
│       ├── host/{mod.rs, commands.rs, events.rs, window.rs}
│       ├── core/{mod.rs, types.rs, agent.rs}          # G4 起 sessions/context/prompt
│       ├── provider/{mod.rs, dto.rs, openai_chat.rs, anthropic.rs, retry.rs, proxy.rs}
│       ├── tools/{mod.rs, registry.rs, batch.rs, compact.rs, pathutil.rs}
│       ├── safety/{mod.rs, fence.rs, approval.rs}
│       └── git/{mod.rs, status.rs}
├── frontend/
│   ├── package.json  vite.config.ts  tsconfig.json  index.html
│   └── src/
│       ├── main.ts  App.vue
│       ├── ipc/{bindings.ts(specta 生成), events.ts, client.ts}
│       ├── stores/{sessions.ts, run.ts, settings.ts, ui.ts}
│       ├── features/chat/  features/tools/  features/panels/
│       ├── i18n/{index.ts, zh-CN.ts, en-US.ts}
│       ├── theme/vars.css
│       └── utils/
├── .github/workflows/ci.yml
└── docs/
```

### 2.2 关键配置

**Cargo.toml 依赖**（feature 精确化，控制编译时间）：

```toml
[dependencies]
tauri = { version = "2.11", features = ["tray-icon"] }   # tray P2 才用，可先不加
tauri-plugin-dialog = "2"
tauri-plugin-single-instance = "2"
tauri-plugin-clipboard-manager = "2"
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
reqwest = { version = "0.12", features = ["json", "socks", "stream", "gzip", "brotli"] }
eventsource-stream = "0.13"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
serde_ignored = "0.1"
schemars = "1"
async-openai = "0.27"
git2 = { version = "0.20", default-features = false }    # G8；不开 ssh
tree-sitter = "0.25"
tree-sitter-bash = "0.25"
grep-regex = "0.1"
grep-searcher = "0.1"
ignore = "0.4"
similar = "2"
flate2 = "1"
tempfile = "3"
keyring = "3"          # P1 启用，P0 引入不调用
thiserror = "2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"
dashmap = "6"
async-trait = "0.1"
```

> 版本号以 `cargo add` 时的最新为准；锁定后写入 Cargo.lock 提交。

**tauri.conf.json 要点**：

```jsonc
{
  "productName": "CodeWave",
  "identifier": "xyz.yangshifu.codewave",
  "app": {
    "windows": [{ "title": "CodeWave", "width": 1280, "height": 800,
                   "decorations": false, "minWidth": 940, "minHeight": 600 }],
    "security": { "csp": "default-src 'self'; img-src 'self' data:; frame-src 'self' blob:" }
  }
}
```

**capabilities/default.json**：仅授权 `core:default`、`dialog:default`、`clipboard-manager:allow-write-text`；**不授权** shell/fs/http 插件（我们的文件与网络操作全在 Rust 侧，不经前端插件）。

**前端 vite.config.ts**：`@vitejs/plugin-vue` + Naive UI 自动导入（`unplugin-vue-components` NaiveUiResolver + `unplugin-auto-import`）+ `tauri-specta` 的 devtools 钩子；dev server 端口固定（如 1420，Tauri 约定）。

**specta 接线**（lib.rs 草图）：

```rust
#[cfg(debug_assertions)]
const SPECTA: tauri_specta::Builder<tauri::Wry> = {
    let b = tauri_specta::Builder::<tauri::Wry>::new()
        .commands(host::commands::collect());
    b
};
// build 时生成 frontend/src/ipc/bindings.ts（Events 也注册进 builder）
```

### 2.3 CI（.github/workflows/ci.yml）

```yaml
on: [push, pull_request]
jobs:
  build:
    strategy:
      matrix:
        include:
          - { os: macos-14, target: aarch64-apple-darwin }     # 主开发平台
          - { os: ubuntu-24.04, target: x86_64-unknown-linux-gnu }
          - { os: windows-2022, target: x86_64-pc-windows-msvc }  # G8 起必绿
    steps:
      - checkout; setup pnpm + node 22; setup rust stable
      - run: pnpm install --frozen-lockfile && pnpm build
      - run: cargo test --workspace          # core/tools/safety 全绿
      - run: pnpm tauri build                # macOS 出产物；其余平台编译验证
```

Linux 需 `libwebkit2gtk-4.1-dev libgtk-3-dev` 等系统依赖；Windows 需 cmake（git2/libgit2-sys 用）——在 CI 中显式安装，本地开发文档同步记录。

### 2.4 DoD
- [ ] macOS `pnpm tauri dev` 起出无边框窗口，热更新可用
- [ ] `cargo test` 通过（先放一个 smoke test）；CI 三平台绿
- [ ] bindings.ts 生成且前端可 invoke 一个 `ping` command 返回字符串

---

## 3. G2 配置与数据目录

### 3.1 数据目录

- 解析：`dirs::home_dir()/.codewave`（跨平台统一点目录，与 D6 一致）；首启创建：`sessions/ histories/ memories/ skills/ stats/ logs/ tmp/`。
- 日志：`tracing_appender::rolling::daily(logs/, "codewave.log")`，保留 14 天（启动时清理）。

### 3.2 ConfigState 定义

```rust
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ConfigState {
    pub schema_version: u32,                 // = 1
    pub models: Vec<ModelConfig>,            // ≥1 个才可发起对话
    pub active_model_id: Option<String>,
    pub proxy: Option<ProxyConfig>,          // {mode: manual|system|none, url}
    pub compact_threshold: f32,              // 0.0–0.95，默认 0.6
    pub approval: ApprovalSettings,          // {enabled: true, confirm_outside_create: true}
    pub validation: ValidationSettings,      // 各语言开关（P1 生效）
    pub ui: UiPrefs,                         // {font_size, accent, language}
}
pub struct ModelConfig {
    pub id: String,                          // uuid
    pub name: String,                        // 显示名
    pub api_format: ApiFormat,               // "openai_chat" | "anthropic_messages"
    pub base_url: String,
    pub keys: Vec<String>,                   // 有序池（P0 只用第一个；P1 全量轮转）
    pub model: String,
    pub max_tokens: u32,                     // 默认 8192
    pub context_window: u32,                 // 默认 128000
    pub reasoning_effort: Option<String>,    // low/medium/high（可空）
}
```

迁移策略：全部字段 `#[serde(default)]`；`schema_version` 预留升级钩子。

### 3.3 实现要点

- `config.rs`：`load()`（不存在→默认+落盘）、`save()`（§原子写）；host 命令 `get_config`（**脱敏 keys**：返回 `has_key: bool` + 尾 4 位，前端不回显明文）、`save_config`（前端传完整结构，含明文 key——私有项目 P0 接受，P1 转 keyring）。
- 原子写 util（`util/atomic.rs`）：写 `tmpfile` 同目录 → `fsync` → `rename`；Windows：目标存在先复制为 `.bak`，rename 失败用 `.bak` 回滚。
- 设置 UI 最小版：`SettingsModal` 仅 `general`（语言/主题/压缩阈值/审批开关）+ `models`（列表 + 表单：name/apiFormat/baseUrl/keys(多行)/model/maxTokens/contextWindow）。

### 3.4 DoD
- [ ] 首启生成目录树与默认 config；改配置→重启不丢
- [ ] 坏 JSON config：启动不崩，回退默认并 `app:config_warning` 提示
- [ ] 模型 CRUD 全流程可用；get_config 不泄露 key 明文

---

## 4. G3 Provider 层

### 4.1 DTO（provider/dto.rs）

```rust
pub enum Role { System, User, Assistant, Tool }
pub enum Content {
    Text(String),
    Thinking { text: String, signature: Option<String> },
    ToolUse { id: String, name: String, args: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    Image { media_type: String, data: String },      // dataURL
}
pub struct Message { pub role: Role, pub content: Vec<Content> }

pub struct StreamRequest {
    pub model: ModelConfig,            // 已选定
    pub system: String,                // 组装后的系统提示词
    pub messages: Vec<Message>,        // 不含 system
    pub tools: Vec<ToolSchemaJson>,    // strict schema
}
pub enum StreamDelta {
    Text(String), Reasoning(String),
    ToolCallBegin { index: usize, id: String, name: String },
    ToolCallArgsDelta { index: usize, fragment: String },
    ToolCallEnd { index: usize },
}
pub struct RunUsage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64 }
pub enum ProviderError { Auth, RateLimited, Server, BadRequest{retryable_after_sanitize: bool}, Network, Cancelled, Protocol(String) }
```

字节稳定性：`Message` 序列化字段顺序由 struct 定义固定；工具列表按 name 排序；不做任何动态 key 的 map。

### 4.2 Provider trait

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    async fn stream(&self, req: &StreamRequest, tx: mpsc::Sender<StreamDelta>,
                    cancel: CancellationToken) -> Result<RunUsage, ProviderError>;
}
```

### 4.3 OpenAiChatProvider

- 用 async-openai 的类型构造请求体，但**自管 HTTP**（reqwest + eventsource-stream），以便统一代理/SSRF/取消语义。
- 流式字段映射：`choices[0].delta.content`→Text；`delta.reasoning_content`→Reasoning（兼容 DeepSeek 风格）；`delta.tool_calls[i]` 按 `index` 聚合：首个含 `id+name` 发 `ToolCallBegin`，`function.arguments` 片段逐个发 `ToolCallArgsDelta`，`finish_reason=="tool_calls"` 时全部发 `ToolCallEnd`。
- 空响应（无 content 且无 tool_calls）视为可重试错误。

### 4.4 AnthropicProvider（自写薄层）

- 端点：`POST {base_url}/v1/messages`，header `anthropic-version: 2023-06-01`；`stream: true`；`system` 为顶层数组（可打 cache_control）。
- 请求转换：`Message`→blocks；ToolResult→`tool_result` block（装 `user` 角色）；ToolUse→`tool_use` block；Image→`source:{type:base64}`。
- **缓存断点注入**：`system` 最后一个 block 与"最后一条非瞬态消息"的最后一个 block 加 `"cache_control":{"type":"ephemeral"}`；瞬态内容（P0 无，预留）不得加在断点之前。
- SSE 事件处理表：

| 事件 | 处理 |
|---|---|
| `message_start` | 取 `usage.input_tokens`（记 cache_read/create） |
| `content_block_start` | 按 index 建 block；`tool_use` 发 `ToolCallBegin` |
| `content_block_delta` | `text_delta`→Text；`thinking_delta`→Reasoning；`input_json_delta`→ToolCallArgsDelta |
| `content_block_stop` | tool_use 块结束→`ToolCallEnd` |
| `message_delta` | `stop_reason`；`usage.output_tokens` |
| `message_stop` | 正常结束 |
| `ping` | 忽略 |
| `error` | 映射 ProviderError |

- `stop_reason: "max_tokens"`→正文尾部追加系统提示（截断发生）；`"pause_turn"`/`"refusal"` 按 Protocol 错误处理并回填说明。

### 4.5 错误分类与重试器（retry.rs）

```
分类：401/403/quota → Auth；408/429/5xx/网络 → 瞬时；400/413/422 → BadRequest（sanitize 后可重试一次）
retry(req):
  for attempt in 0..=6:
    按 key 池选 key（P0：固定第一个）→ stream()
    ├─ Ok → return
    ├─ Auth → 直接上抛（P0 无备选 key；P1 换 key 重试）
    ├─ 瞬时 → sleep(min(500ms * 2^attempt, 10s)) + jitter(0–20%) → emit run:retry → continue
    ├─ BadRequest 且未 sanitize 过 → sanitize_history() → 免费重试一次
    └─ 其他 → 上抛
```

取消：`cancel` token 同时中断 HTTP 流（`tokio::select!`）；返回 `Cancelled`。

### 4.6 代理与 SSRF（proxy.rs）

- `build_client(cfg)`：手动代理 > 系统探测（P0 先实现 macOS `scutil --proxy`；Windows 注册表/Linux env P1）> 直连。
- SSRF 守卫 Connector：`TokioResolver` 解析后逐 IP 检查私网段（v4: 10/8、172.16/12、192.168/16、127/8、169.254/16、100.64/10；v6: ::1、fc00::/7、fe80::/10）→ 拒绝（`allow_private_network=true` 放行，供 Ollama/本地模型）。

### 4.7 测试

- `wiremock` 起本地 mock：① 标准 OpenAI 流（文本+1 个 tool_call 完整序列）② Anthropic 流（含 thinking + tool_use + usage）③ 中途断流（半截 SSE 后断开）→ 断言重试 ④ 429→断言退避时间在窗口内 ⑤ 400→断言 sanitize 被调用。
- 抓包回放：手工抓一次真实 Anthropic 会话存为 `tests/fixtures/anthropic_*.sse`，解析器单测逐事件比对。

### 4.8 DoD
- [ ] 真实 OpenAI 兼容端点与 Anthropic 端点各完成一次流式对话（含一次工具调用往返）
- [ ] mock 测试 ①–⑤ 全绿；取消即时生效（<200ms 停止输出）

---

## 5. G4 Agent 主循环

### 5.1 核心结构（core/agent.rs，字段同 [docs/technical-design](./technical-design.md) §4.1.1）

补充 P0 字段细节：

```rust
pub struct SessionRuntime {
    // …同 [docs/technical-design](./technical-design.md)…
    inject_tx: mpsc::Sender<Message>,      // capacity 32；前端注入失败（Full/Closed）直接报错提示
    live_usage: Mutex<RunUsage>,           // 实时累计（信息条）
    ask_waiters: Mutex<HashMap<AskId, oneshot::Sender<AskAnswer>>>,  // G5/G6 共用
}
```

### 5.2 run_loop 伪代码（展开到可编码级）

```rust
pub async fn run_chat(core: &AgentCore, rt: Arc<SessionRuntime>, user: Message, ch: Channel) {
    let _guard = rt.run_lock.try_lock().expect("single run");
    let cancel = rt.cancel.child_token();
    core.sink.emit(rt.id, Ev::RunStart);
    rt.history.push(user);
    let mut sanitized_once = false;

    for step in 0..MAX_STEPS {
        if cancel.is_cancelled() { mark_cancelled(rt).await; checkpoint(rt).await; emit(RunCancelled); return; }
        drain_injections(rt).await;                                   // try_recv 全部 → push 历史 → emit RunInject
        update_live_breakdown(rt).await;                              // emit TokensUpdate
        if usage_ratio(rt) > cfg.compact_threshold { compact(rt).await?; emit(RunCompacted); }

        let req = build_stream_request(rt)?;                          // system+history+tools
        match stream_with_retry(core, rt, &req, &cancel, &mut sanitized_once).await {
            Ok((assistant_msg, usage)) => {
                rt.history.push(assistant_msg.clone());
                let calls = extract_tool_calls(&assistant_msg);
                if calls.is_empty() { save(rt).await; emit(RunDone{usage}); return; }
                let results = execute_batch(core, rt, calls, &cancel).await;   // §G5
                rt.history.push(Message::tool_results(results));
            }
            Err(Cancelled) => { mark_cancelled(rt).await; checkpoint(rt).await; emit(RunCancelled); return; }
            Err(e) => { save(rt).await; emit(RunError{e}); return; }
        }
    }
    emit(RunError{MaxStepsReached});
}
```

### 5.3 取消与检查点

- `CancelRun(run_id)`：置 cancel token → 流立即断 → 当前步的已完成工具结果保留在历史 → `checkpoint()`：走 §G7 完整保存 + 历史尾部追加 `Message::user("<cancelled/>")`（自有标记格式）。
- 工具执行内部所有阻塞点（子进程 wait、文件 IO）都用 `tokio::select! { _ = cancel.cancelled() => … }` 包裹，确保取消 <1s 生效。

### 5.4 EventSink（host/events.rs）

```rust
#[async_trait]
pub trait EventSink: Send + Sync {
    fn channel_frame(&self, run_id: &SessionId, frame: ChannelFrame);   // delta/tool_progress/usage
    fn emit(&self, session: &SessionId, ev: SessionEvent);              // 低频全局事件
}
```

- 高频节流在 core 侧（`infra/throttle.rs`）：Text/Reasoning 合并按 64ms 时间窗 flush；`tool_progress` 200ms 或 2KB 触发。
- Tauri 实现用 `tauri::ipc::Channel<ChannelFrame>`（start_chat 传入，存入 runtime）；低频用 `app.emit_to("main", ev.name(), payload)`。

### 5.5 DoD
- [ ] 单会话第二个 start_chat 被拒绝并提示；注入消息在当前步结束后进入历史且模型可见
- [ ] Esc 取消 <1s 停流；重启后取消点之前的内容完整
- [ ] 断网触发重试且前端看到 run:retry 并丢弃半截回复

---

## 6. G5 工具框架 + 8 工具

### 6.1 ToolRegistry 与批次执行（tools/registry.rs、batch.rs）

```rust
pub struct ToolRegistry { tools: HashMap<&'static str, Arc<dyn Tool>> }
impl ToolRegistry {
    pub fn schemas_sorted(&self) -> Vec<ToolSchemaJson>;            // 按 name 排序（cache-first）
    pub async fn execute_batch(&self, ctx: &RunCtx, calls: Vec<ToolCall>, cancel: &CancellationToken)
        -> Vec<ToolResultMsg> { /* §6.1.1 */ }
}
```

**6.1.1 execute_batch 算法**：

```
1. 批次预检：
   - Interactive 类（ask）出现 → 必须 calls.len()==1，否则整批拒（每个 call 返回 E_BATCH_POLICY）
   - 写工具按 canonicalize(path) 分组 → 同组 >1 个 → 该组全部拒 E_WRITE_BATCH_CONFLICT
2. 分流：writes（保持原顺序）→ 逐个 await；others → JoinSet + Semaphore(4) 并发
3. 每个 call：
   result = tokio::spawn(AssertUnwindSafe(fut)).catch_unwind().await
            .unwrap_or(E_TOOL_PANIC)                    // panic 兜底
4. 每个完成即 emit tool:result / tool:error（定位键 runId:batchId:callIndex）
5. 组装 ToolResultMsg（模型通道：compact_for_model()）回填历史
```

### 6.2 双通道压缩（tools/compact.rs）

```rust
pub fn compact_for_model(kind: ToolKind, out: &ToolOutcome) -> String {
    // ReadOnly(file read) → 原样（保行号）
    // 文本类：head 4KB + "\n…[truncated N bytes]…\n" + tail 8KB
    // grep：最多 200 条 + Top100 文件计数；command 输出 > 96KB → 落盘 tmp/ 并在结果中给 outputFilePath
    // 总配额：单步所有工具结果合计 128KB，超出从最大者继续压缩
}
```

### 6.3 八个工具设计

每个工具 = `tools/<name>/algo.rs`（纯函数，可单测）+ `tools/<name>/mod.rs`（Tool trait 实现，编排）。

**read**：入参 `{files:[{path, startLine?, endLine?}]}`（1–20 个；startLine 负数=尾部 N 行）。行为：绝对/相对路径经 pathutil 解析；UTF-16 自动转码（BOM/零字节启发式）；输出格式 `<file path> lines a-b:` + 每行 `%6d| ` 行号前缀；图片（png/jpg/webp/gif）≤3MB → Content::Image 注入；>20 文件或单文件 >1MB 拒绝并建议缩小范围。附带给每个文件计算 `version`（见 edit）。

**edit**：
- 入参 `{files:[{path, version, changes:[{oldText?,newText}|{lineRange?,newText}]}]}`
- version 校验：`crockford_b32(sha256(content))[..6]` ≠ 传入值 → E_VERSION_STALE（错误信息要求先重新 read）
- 应用：逐文件 → 逐 change：优先全文唯一匹配 oldText（0 命中→若提供 lineRange 则用行区间替换；>1 命中→拒绝并要求加长上下文）；全部 change 在内存缓冲完成后一次性原子写
- 回滚：写前把原内容存 `tmp/edit-backup/<hash>`，任一文件写失败 → 已写文件全部还原
- 抢救：参数 JSON 解析失败且形似截断（引号/括号不平衡）→ 尝试补全闭合后重解析；仍失败 → 拒绝并回传原始片段长度提示

**create**：`{path, content, overwrite?}`；父目录自动创建；已存在且 !overwrite → 拒绝。

**delete**：`{path, recursive?}`；pathutil 守卫（拒 `/`、工作区根、`.git`、`~`、系统目录、符号链接目标逃逸）；目录非空且 !recursive → 拒绝。

**list_files**：`{path?, maxDepth?=2, limit?=200}`；ignore crate walk（尊重 .gitignore）；输出相对路径树。

**command**：
- shell 发现：macOS/Linux → `bash -lc`（登录 shell 捕获 PATH，超时 5s 探测一次缓存）；Windows → 探测 Git Bash（注册表/常见路径）否则 `powershell -NoProfile -Command`
- 进程：`tokio::process::Command` + 进程组（Unix `setsid`，kill 走 `pgid`；Windows Job Object 句柄挂子进程）；cwd 锁定工作区（chdir 后仍校验 pathutil）
- 超时 `timeoutSeconds` ≤600，默认 120；取消/超时 → 杀进程树
- 输出：stdout/stderr 合并流式（tool_progress 节流）+ 结束后完整出参；总量 >96KB 落盘 `tmp/cmd-output/<id>.txt`
- 执行前过 safety 围栏（G6）

**grep**：`{pattern, path?, glob?, maxMatches?=100(≤200), offset?}`；`ignore::WalkBuilder`（git_ignore on、隐藏文件默认跳过）并行遍历 + `grep_regex::RegexMatcherBuilder(case_smart)` + `grep_searcher::Searcher(line numbers, binary 转换检测)`；输出：逐条 `path:line: text` + 尾部文件命中计数 Top100 + 总数；超限提示用 offset 翻页。

**ask**：`{questions:[{id, question, options?:[{id,label,description?,recommended?}]}]}`（1–5 题）。执行：为每题建 oneshot waiter → emit `ask:opened` → `select!{ answer, cancel → Denied }`；前端在 Composer 上方渲染问答卡；`resolve_ask(ask_id, payload)` 唤醒；答案组装为结构化文本回填模型。

### 6.4 DoD
- [ ] 端到端："读取 X → 修改 → 跑测试" 单次 run 内完成
- [ ] edit：过期 version 被拒；oldText 多义被拒；截断参数被抢救或干净拒绝
- [ ] command：长输出落盘；进程超时被杀（子孙进程一并）
- [ ] grep：.gitignore 生效；200 上限 + 翻页可用
- [ ] ask：UI 问答往返；run 取消时 ask 置为 Denied 且 run 正常收尾
- [ ] tools 算法层单测覆盖 ≥80%（纯函数）

---

## 7. G6 安全围栏 + 审批弹窗

### 7.1 pathutil（tools/pathutil.rs，唯一路径实现）

```rust
pub fn safe_join(root:&Path, rel:&str) -> Result<PathBuf>            // 拒 ..、绝对路径、Windows 保留名/非法字符
pub fn resolve_read(cfg:&WriteRoots, p:&Path) -> Result<PathBuf>     // canonicalize 后须在 read 集内
pub fn resolve_write(cfg:&WriteRoots, p:&Path) -> Result<PathBuf>    // 含中间 symlink 检查
pub fn is_dangerous_delete(p:&Path, workspace:&Path) -> bool         // /、工作区根、.git、~、系统目录
pub struct WriteRoots { pub workspace: PathBuf, pub extra: Vec<PathBuf>, pub data_dir: PathBuf }
```

### 7.2 围栏三层（safety/fence.rs）

```
check(command, cwd, roots) -> Verdict{ Allow | Confirm(Reason) | Block(ErrorCode) }

L1 删除黑名单（Block）：
  解析首个命令词（bash: 直接词；powershell: cmdlet 名）∈ {rm, rmdir, unlink, del, erase, rd,
  remove-item, ri, shred} 或参数含 -delete / -exec rm → Block(E_DELETE_USE_DELETE_TOOL)

L2 写目标分析（tree-sitter-bash）：
  解析 AST → 遍历以下节点收集写目标：
    redirected_statement → file_redirect{destination, descriptor: > >> 2> &>}
    heredoc_redirect → heredoc_body 落点（仅重定向目标文件）
    命令词 ∈ {tee, cp, mv, dd, install, touch} → 其文件参数
    command_substitution / process_substitution → 递归内层
  每个目标：相对 cwd 解析 → canonicalize（不存在→取父目录）→ symlink 中间链检查
    已存在 且 ∉ roots → Block(E_PATH_OUTSIDE)
    不存在 且 ∉ roots → Confirm(OutsideCreate)
    descriptor 为 2>/dev/null 或目标=/dev/null → 放行

L3 高危模式（Confirm，approval.enabled=false 时 Block）：
  chmod/chown 到 777 或系统路径；mkfs*；dd of=/dev/*；
  curl|wget → (bash|sh|zsh) 管道；git push --force；> /etc/*；shutdown/reboot
```

tree-sitter 查询示例（开发时用 tree-sitter CLI 验证 grammar 节点名后固化）：

```js
; 抽取所有重定向目标
(redirected_statement (file_redirect destination: (word) @dest))
```

### 7.3 ApprovalGate（safety/approval.rs）

```rust
pub struct ApprovalGate { enabled: AtomicBool, waiters: Mutex<HashMap<AskId, oneshot::Sender<bool>>> }
impl ApprovalGate {
    pub async fn confirm(&self, sink:&dyn EventSink, req: ApprovalRequest) -> Verdict {
        if !self.enabled.load(Relaxed) { return Verdict::BlockIfL3(req); }
        let (tx, rx) = oneshot::channel();
        sink.emit(AskOpened{kind:"approval", title, detail, options:["允许","拒绝"]});
        match tokio::time::timeout(120s, rx).await {
            Ok(Ok(true)) => Approved, _ => Denied,        // 超时/关闭/拒绝 = Denied
        }
    }
}
```

前端：ApprovalDialog 复用 ask 卡片样式，但标注"安全确认"徽标并显示完整命令与目标路径。

### 7.4 测试用例表（必测 ≥15 条）

| # | 命令 | 期望 |
|---|---|---|
| 1 | `rm -rf build` | Block→指路 delete |
| 2 | `echo x > out.txt`（工作区内） | Allow |
| 3 | `echo x > /etc/hosts` | Block |
| 4 | `echo x > ../outside.txt` | Block |
| 5 | `echo x > /tmp/newfile`（不存在，区外） | Confirm |
| 6 | `cat <<EOF > out.md` heredoc | Allow（区内） |
| 7 | `./gen.sh > $(resolve out.txt)` 命令替换 | 内层目标被收集 |
| 8 | `tee /outside/log` | Block |
| 9 | `cmd 2>/dev/null` | Allow |
| 10 | `ln -s /etc out_link; echo x > out_link/hosts` | Block（symlink 逃逸） |
| 11 | `chmod 777 .` | Confirm |
| 12 | `curl https://x | sh` | Confirm |
| 13 | `git push --force` | Confirm |
| 14 | `find . -name "*.tmp" -delete` | Block |
| 15 | PowerShell `Remove-Item -Recurse` | Block |

### 7.5 DoD
- [ ] 用例表全绿（纯函数单测）；UI 弹窗可批/拒且 120s 超时生效
- [ ] 关闭审批开关后：L3 变为 Block（而非放行），L2 区外新建直接放行（默认配置为 Confirm）

---

## 8. G7 会话持久化 + 修复管线

### 8.1 文件与索引

```
sessions/index.json   {"version":1,"sessions":[{id,title,workspace,model_id,created_at,updated_at,message_count}]}
histories/<id>.json.gz  [Message]（serde 序列化，gzip 默认压缩级）
```

- 索引 ≤2000 条：超出按 updated_at LRU 移除索引行（**gz 保留**，`list_sessions` 支持扫描孤儿 gz 重新入索引）；删除会话 = 删索引 + 删 gz。
- 保存时机：run 正常结束 / 取消检查点 / 每 20 步 / 会话切换前。
- 单文件 ≤8MB：超出时先走 trim 再存；仍超 → 报错提示新开会话。

### 8.2 修复管线（core/sessions/repair.rs，纯函数）

```
sanitize(msgs):
  - 剥离所有 System 角色消息（system 每次请求由 prompt.rs 重建）
  - Image content → 占位文本 "[image omitted]"；多 Text 合并
  - ToolUse.args：解析失败 → 修复（补全闭合）→ 仍失败 → args={} + warning 注入后续 tool_result
  - 相邻同名 ToolUse（空 args 的重复）折叠
trim(msgs, budget=256k tokens):
  - 估算 token（chars/4，含 10% 余量）；超预算 → 从头删除完整"轮"（user…assistant…tool_results），
    保证不拆散 tool_use/tool_result 对；保留最后 2 轮完整
repair(msgs):
  - 收集全部 tool_use id；删除指向不存在 id 的孤儿 ToolResult
  - 删除无 ToolResult 的悬空 ToolUse（在该 assistant 消息后补 tool_result{is_error:"interrupted"}）
加载顺序：load = decompress → repair → trim
保存顺序：save = sanitize → trim → repair → 原子写
```

### 8.3 测试

毒化样本表（手工构造 JSON fixture）：
1. 悬空 tool_use（无结果）→ 加载后补 interrupted 结果
2. 孤儿 tool_result → 删除
3. 截断的 tool args JSON → 修复或占位
4. 历史超预算 → 在轮边界裁剪且配对完整
5. 8MB 压缩极限 → 拒绝路径正确
6. 断电样本（截断 gzip）→ 加载失败隔离该会话，不影响其他会话与索引

### 8.4 DoD
- [ ] 用例 1–6 全绿；重启 app 后会话列表/历史完整恢复
- [ ] index 超 2000 淘汰后仍能从 gz 重新发现

---

## 9. G8 git2-rs 引入与构建验证

- `git/`：`git_status()` —— `Repository::open(workspace)` → `statuses(Status::include_untracked|recurse_untracked_dirs|exclude_submodules)` → `Vec<{path, index:Flag, worktree:Flag}>`（上限 500 条）。
- Windows CI：安装 cmake（choco）+ MSVC；`git2` default-features=false（不启 ssh/zlib 动态链接），libgit2-sys 源码自编译。
- 本地文档：`docs/dev-setup.md` 记录三平台依赖（macOS 自带 clang；Linux `build-essential cmake`；Windows VS Build Tools + cmake）。
- DoD：三平台 CI `cargo test` 含一个真实小 repo 的 status 断言测试；`git_status` command 前端可调用展示。

---

## 10. G9 基础聊天 UI

### 10.1 Pinia store 接口（冻结，供 specta 类型对接）

```ts
// stores/run.ts
interface RunState {
  status: 'idle'|'streaming'|'tooling'|'awaiting_ask'|'cancelling'|'error'
  streamText: string; streamReasoning: string
  toolCalls: Record<string, ToolCallView>      // key = `${runId}:${batchId}:${callIndex}`
  ask?: AskView; usage: { input:number; output:number; contextPct:number }
}
// stores/sessions.ts
interface SessionsState { tabs: Tab[]; activeTabId: string; index: SessionMeta[] }
```

### 10.2 流式管线

- `ipc/events.ts`：`onChannel(runId, frame)` + `onEvent(name, handler)` 薄封装（唯一 import `@tauri-apps/api` 的文件之一）。
- 缓冲：`run.ts` 收 delta 只 push 原始缓冲；`requestAnimationFrame` flush 到响应式状态；流式消息用 `StreamingMarkdown.vue` 独立实例渲染（markdown-it + hljs，mermaid 留 P1）；完成消息 `MessageBody.vue` 渲染并缓存 vnode（LRU 16 条 / 2M 字符）。

### 10.3 组件清单（P0 版）

| 组件 | 职责 |
|---|---|
| App.vue | 布局壳 + 全局快捷键（Esc/T/W/N/←→） |
| AppHeader | 自定义标题栏（拖拽区）+ 会话下拉 + 窗口控制 |
| ChatMessages | 消息列表虚拟滚动（简单窗口化即可） |
| StreamingMarkdown / MessageBody | 流式/完成消息渲染 |
| Composer | 输入框 + 发送/停止 + `/` 命令菜单 + `@` 文件提及（数据走 search_workspace_paths） |
| ToolCallCard | 统一壳：状态图标 + 动词表（toolVerb.ts）+ 折叠体；read/grep/command/edit 四种适配器 |
| AskCard / ApprovalDialog | 问答与安全确认（approval 带徽标） |
| SessionList | 会话下拉/列表（load/delete/rename） |
| SettingsModal | general + models 两分区（G2） |
| ContextInfoBar | 底部：模型名 / 上下文 %（tokens:update） |

### 10.4 DoD
- [ ] 50K 字符流式回复滚动不掉帧（Performance 录制验证）；100+ 消息切换 <100ms
- [ ] 工具卡四种适配器正确展示；ask/approval 往返可用
- [ ] i18n 框架就绪（zh-CN 全量、en-US 框架）；暗色基座 + 1 套强调色

---

## 11. G10 上下文管理 v1

- **分项统计**（core/context.rs，30s 缓存）：
  - system = prompt 组装结果 chars/4；history = 全部消息（图片按 1.6k token/张估）；tool_schemas = schema JSON 总 chars/4；tool_results = 最近一步工具结果字符。
  - 估算值在每步 usage 上报后被校准（比值滑动平均）。
- **自动压缩**：`usage_ratio = (累计估算) / context_window` > 0.6 触发；摘要请求走同 Provider（独立小请求）：
  - 摘要提示词五段结构（自有文案）：`<最新请求>` `<已完成工作>` `<当前状态>` `<下一步>` `<关键文件与路径>`；输出替换整段历史：`[user: <handoff-summary>…</>]`；原历史转存 `tmp/compacted/<id>-<ts>.json.gz` 备查。
  - 失败（超时 3min/错误）→ 不压缩，继续原历史，emit 警告。
- `/compact` 命令：手动触发，保留最后一条 user 消息原文。
- DoD：人为把 threshold 调 0.05 触发压缩 → 对话仍连贯引用早前内容；信息条百分比与请求 payload 一致（抽样对账）。

---

## 12. P0 周计划建议

| 周 | 目标 | 关键产出 |
|---|---|---|
| W1 | G1、G2、G8、G3 | 脚手架+CI 三平台绿；配置体系；双协议流式跑通（mock+真实各一） |
| W2 | G4、G5、G7 | 主循环；8 工具 + 批次执行；持久化与修复管线（测试先行） |
| W3 | G6、G9、G10 | 围栏+审批；完整 UI；压缩；端到端打磨与 DoD 对账 |

风险管理：W1 末若 G3 流式未通（SSE 细节），G4 改用阻塞式单轮先行解耦，不阻塞 W2 工具框架开发（用假 Provider 注入）。

---

## 13. P0 总验收（Definition of Done）

1. **场景 A（编码闭环）**：新会话 →"把 config 加载改为带 schema_version 校验，然后跑 cargo test"→ read/edit/command 依次执行、edit 带正确 version → 测试结果回填 → 总结。
2. **场景 B（安全）**：让 Agent"清理构建产物并删掉临时目录"→ rm 被拦截改道 delete 工具 → 区外写触发确认弹窗 → 拒绝后 Agent 给出替代方案。
3. **场景 C（韧性）**：对话中途拔网 → 自动重试可见 → 恢复后继续；Esc 中断 → 重启 app → 历史完整、可继续追问。
4. 质量门槛：`cargo test` 全绿（core/tools/safety 覆盖率 ≥80%）；CI 三平台绿；无 panic 路径（fuzz 工具参数 30 分钟无崩溃）。
