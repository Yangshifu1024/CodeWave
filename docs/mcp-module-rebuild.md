# MCP 模块重构（会话隔离 · 连接池 · 配置定稿 · 安全门）

日期：2026-09-24 · 分支：`refactor/mcp-rebuild`

本次重构把 MCP 从「挂在 `AgentCore` 上的全局单例 + 无过滤工具注入」改成
**以会话可见集为纲的连接池**，并把配置形状、错误语义、进程回收、安全门一次性定稿。

## 一、为什么改（旧实现的结构性缺陷）

| 缺陷 | 证据（重构前） |
|---|---|
| **跨项目工具泄漏** | `McpManager` 以 server 名为键挂在 `AgentCore`，`tool_defs()` 无过滤；`core/agent/stream.rs` 全量注入 → 项目 A 的 MCP 工具出现在项目 B 的请求里 |
| **打开任意会话就掐断别人** | `connect_mcp` 遍历全部配置，`start()` 先 `stop_all` 级重建 |
| **一条坏数据毁整份文件** | `serde_json::from_str::<McpConfigFile>` 一把梭，任一条目失败即 `continue` 掉整个文件，无任何 UI 反馈 |
| **配置保存有损** | 前端文本域 `split(/\s+/)` 往返 args → 含空格路径被拆坏；未知键（`headers`/`cwd`/`$schema`）保存即丢；未知 `transport` 被静默改写 |
| **无进程树回收** | `TokioChildProcess::new(Command)` 走 `CommandWrap::from(Command)`，wrappers 表为空 → **没有 Job Object**，server 自己 spawn 的孙进程变孤儿 |
| **MCP 调用零安全引用** | `tools/batch.rs` 自述「MCP 分支无审批 / 范围门」 |
| **靠错误文本判重连** | `"channel closed"` / `"send failed"` 子串匹配（历史审查 M10） |
| **结果形状不确定** | 内容是否「恰好是合法 JSON」决定结果是对象还是 `{text:...}` |
| **上下文漏算** | `core/context.rs` 只算内置注册表 schema，MCP schema 不计入压缩阈值 |

## 二、新契约

### 2.1 `mcp.json` 形状（生态通用，`transport` 可推导）

```json
{ "mcpServers": {
    "fs":     { "command": "npx", "args": ["-y", "pkg", "D:\proj"],
                "env": {"K":"V"}, "cwd": "D:\proj", "enabled": true,
                "timeout_ms": 120000, "read_only": false, "always_allow": false,
                "tools": { "mode": "allow", "list": ["read_*"] } },
    "remote": { "url": "https://host/mcp",
                "headers": {"Authorization": "Bearer x"}, "read_only": true } } }
```

- **作用域只有两层**：全局 `<data_dir>/mcp.json` 与项目 `<project_dir>/mcp.json`
  （`project_dir` 即 `<主目录>/.codewave`）。会话可见集 = 两层合并，**同名项目级胜出**。
  工作区级 / `extra_roots` 的 `<root>/.codewave/mcp.json` 兼容读取**已移除**。
- **transport 推导**：有 `url` → `streamable_http`；有 `command` → `stdio`；
  两者并存且未显式声明 → **配置错**（歧义必须暴露）；显式声明优先；`"type"` 作为别名。
- `"transport": "sse"` → **配置错**，hint 点名改用 streamable_http 端点（不支持旧式 SSE）。
- **未知键透传**：条目内未识别的键由 `#[serde(flatten)] extra` 捕获，读写不丢；
  `set_always_allow` 走「读原文 JSON → 只改该键 → 原子写」，**不整份重建**，
  且沿用文件里已有的拼写（`always_allow` / `alwaysAllow`）避免同义键并存。
- **逐条目容错**：单条目反序列化失败只产出一条 `parse` 级 `ConfigIssue` 并跳过该条目，
  同文件其它条目照常生效。
- **`enabled: false`** 条目可见可编辑、不连接。
- server 名**不限制字符集**（`PowerShell.MCP`、中文都允许）——由 `sanitize_component`
  归一化到 provider 合法函数名字符集；两个名字归一化后撞名时发 **warning 级** issue
  并说明会自动加 `__2` 后缀去重（不得因撞名拒绝用户配置）。

### 2.2 工具命名与反查

模型可见函数名 `mcp__<server>__<tool>`（两段均归一化）。调用侧**只查注册表反查**
（`build_tool_defs` 返回的 index），**绝不用 `parse_function` 做字符串切分**——
server 名可含 `__`，切分会错位。

### 2.3 连接池与会话可见集

- 池键 = `(作用域, 项目 id, server 名)`：全局的 `fs` 与某项目的 `fs` 是**不同**条目。
- 会话打开时 `warm()` **并行**建连（不再串行 await，坏配置多时不再阻塞 IPC），
  状态经事件流实时推送。
- 引用计数归零即回收进程；**有引用的条目绝不被淘汰**（宁可短暂超限并 `warn`）。
- 上限默认 **16**（`DEFAULT_MAX_SERVERS`），LRU 淘汰**只作用于引用计数为 0** 的条目；
  被淘汰条目保留为 `Evicted` 状态记录（不占槽位），再次需要时重拉，带 **5 秒防抖**。
- `stop` / `disconnect` / 淘汰 / 全停**都会发状态事件**（旧 `stop_all` 不发 → UI 状态残留）。

### 2.4 错误分类（取代文本子串匹配）

`McpErrorKind` = `config` / `spawn` / `handshake` / `call` / `cancelled`。
`is_connection_class()` 只对 `spawn` / `handshake` 为真 —— **只有连接类错误允许自动重连，
且只重放只读调用**；取消优先于重连判定。rmcp 侧映射：
`ServiceError::TransportSend` / `TransportClosed` → `handshake`；
`Cancelled` → `cancelled`；`Timeout` → `call`。

### 2.5 进程树回收

`process.rs` 显式包裹 `CommandWrap`：Windows `JobObject` + `KillOnDrop` + 本地 `NoWindow`
（补回被 Job Object 覆盖式写掉的 `CREATE_NO_WINDOW`，并保留 `CREATE_SUSPENDED` 供 job 挂载）；
unix `ProcessGroup::leader()`。stdio 连接记录 PID 供诊断。
兜底：Windows `taskkill /T /F /PID`、unix `kill -TERM -- -<pgid>`（失败只 `warn`，不 panic）。

### 2.6 安全门

MCP 工具纳入审批：server 级 `read_only` 或 `always_allow` 声明过则免审，否则**逐次确认**；
审批卡展示 server / 工具 / 参数摘要，`allow_always: true` 时勾选「总是允许」会写回
该 server 所在作用域的条目（粒度 = server）。审批只阻塞**该次** MCP 调用，
其它会话与内置工具不受影响。

### 2.7 IPC 命令

新增（作用域化）：`mcp_list_config(session_id?, scope)` / `mcp_save_config(session_id?, scope, json)` /
`mcp_connect(session_id, names?)` / `mcp_disconnect(session_id, names?)` /
`mcp_reconnect(session_id, name)` / `mcp_test(session_id?, scope, name)` / `mcp_snapshot(session_id)`。
事件：`mcp:status`（增量，载荷含 `session` / `scope` / `name` / `state` / `tools` /
`tools_filtered` / `pid` / `error` / `note`）。

旧命令 `get_mcp_config` / `save_mcp_config` / `connect_mcp` / `mcp_status` **保留为适配器**
（前端尚未迁移，载荷为增量扩展，旧前端可继续工作）。

## 三、验证

| 项 | 结果 |
|---|---|
| `cargo test`（`src-tauri/`） | **1000 passed / 0 failed / 3 ignored**（基线 965 → 新增 35 个 MCP 测试） |
| `cargo fmt --check` | 干净 |
| `cargo check --lib` | 0 error / 0 warning |
| `pnpm --dir ui test` | **1019 passed / 91 文件**（前端未改，零回归） |
| 真进程端到端（node） | stdio 握手 / 工具列表 / 调用 / streamable-http / 取消 / 临时测试连接 / **进程树连带回收**（孙进程被 Job Object 杀掉）全部通过 |

新增测试覆盖点：transport 推导（含歧义与 `sse` 定向报错）、逐条目容错、未知键往返、
`set_always_allow` 不破坏其它键且沿用已有拼写、工具过滤通配、错误分类只对连接类为真、
结果形状确定性、会话可见集隔离、引用计数归零回收、LRU 只淘汰无引用条目、
有引用条目不被淘汰、取消优先、进程树回收。

## 四、剩余项

1. **args / env 表格化**（UI 外观）：当前是「一行一个参数」「K=V 每行一条」的**无损**文本框
   （已修掉旧的按空白切分与 trim 有损），表格 UI 待做。文本框无法表达「值里含换行」的
   环境变量——行模型 `mcpConfig.ts` 已支持，表格化后即可覆盖。
2. **resources / prompts**：按决策列为**非目标**；sampling / elicitation 明确不支持
   （server 反向调模型/请求用户输入是重大安全面，与「纳入安全门」相冲）。

已在本分支完成（原列于此的遗留）：前端面板重构（作用域切换、来源列、状态徽标、
PID 与单 server 断开/重连、测试连接）、`tools/list_changed` 热更新、结构化 content blocks
（图片走 `Content::Image` 进模型通道）、HTTP 请求头注入、旧 4 命令适配器下线、
文档漂移同步（technical-design / p1-plan / arch-orchestrator / settings-search-and-advanced）、
`code-review-findings` 关闭 M10 与 L7。


## 五、与批准方案的偏差（均有理由）

1. **server 名不做字符集校验、不拒绝非法名**。方案原写「限 `[A-Za-z0-9_-]`，非法名拒绝」，
   但分支基线已合并 `3b4a5ce fix(mcp): sanitize MCP tool names to provider-legal charset`：
   该修复明确允许任意 server 名（`PowerShell.MCP`）并归一化到函数名。按方案实现会**回退该修复**，
   使既有用户配置直接失效。故改为「允许任意名 + 归一化 + 撞名告警」，仍然满足「反查不做字符串切分」的意图。
2. **未新建 `mcp/client.rs` / `mcp/pool.rs`**：`mcp/` 在分支基线上**已被拆分**
   （`mod.rs` 17 / `config.rs` 111 / `manager.rs` 511 / `tools.rs` 178），
   且已具备函数名反查表、M10 收窄、名称归一化与两个真进程集成测试。
   故按实际结构在 `manager.rs` 内实现会话可见集与连接池，新增 `error.rs` / `process.rs`，
   不另起文件（方案该节基于过期现状撰写）。
3. **IPC 命令保留旧名适配器**：方案冻结了新命令集且要求「不做半迁移」。
   但前端重构未在本次范围内落地，若直接删除旧命令，应用会当场不可用。
   折中：新命令集完整实现并注册，旧 4 命令保留为薄适配器（载荷增量扩展），
   前端迁移后一并删除。
4. **未做 `src-tauri/tests/mcp_e2e.rs`**：真进程端到端测试放在 `mcp/manager.rs` 的
   测试模块内（沿用分支基线既有的组织方式），覆盖同样的场景。
5. **`mcp:snapshot` 只作为命令实现，未作为事件**：后端没有触发「全量快照事件」的场景
   （`disconnect_all` 无会话上下文，发事件会被前端丢弃），前端可用 `mcp_snapshot` 命令拉取。
