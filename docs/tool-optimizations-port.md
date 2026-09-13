# — 工具链优化批次报告

> 来源：内部早期原型的跨项目移植评估。
> 本批次落地 3 批共 10 项（token 经济 + run 稳定性 + compaction 强化），其余评估为不移植/后续立项（§5）。

## 1. 批次一：工具输出瘦身（token 经济）

### 1.1 command：`fullOutput` 参数 + 凡截断必落盘（tools/command.rs）

- schema 新增 `fullOutput`（**required**：把「输出是否就是答案」的决策前置到调用时刻）；Rust 侧 `#[serde(default)]`，旧 payload 按 false 安全降级。
- `fullOutput=false`：模型侧 output = 末 2000 chars（`MODEL_TAIL_CHARS`）+ signal line（`[exit N | 共 M 行 | 输出已截断，完整输出：<spill 路径>]`）；`true`：保持原行为（末 16000 chars）。UI 通道不变，始终拿完整 outcome。
- **盲区修复**：原实现输出在 16KB~96KB 区间时被截到尾 16000 chars，却未达 96KB 流式 spill 阈值不落盘——被截掉的头部模型永远拿不回。现在凡截断必补写 spill 文件（`tmp/cmd-output/<uuid>.txt`）。
- spill 时 `buf` 只含头部 96KB，尾部改从 spill 文件读取（`tail_chars_from_file`，最多 want×4+8 字节）——顺带修了原实现 spill 后「tail 取的是中段而非末尾」的问题。
- signal line 行数统计：Collector 增量累计 `total_lines`/`total_bytes`（按 `\n` 计，末行无换行补 1）。
- 新增 `purge_stale_spills()`：spill 落盘时惰性清理 24h 前的旧文件（每小时至多扫一次）。

### 1.2 compact.rs：per-tool 收口（tools/compact.rs）

- `command` 加入豁免：工具内已语义化裁剪，模型收完整 outcome JSON，不再被 head 4KB/tail 8KB 字节截断切碎。
- 顺带修复：原实现 `!ok` 一律只回 `[error ...]`——command 非零退出时 `data.output`（构建失败日志等）全丢。现在 command 错误路径输出 error 行 + data（`output`/`exit_code` 保留）。

### 1.3 list_files：per-directory budget（tools/list_files.rs）

- `DIR_BUDGET = 50`：单目录直属条目超 50 折叠为 `+N more in <dir>` 占位符；占位符置尾分组、不占全局 limit 配额、不计入 `count`；walk 继续进行保证计数准确（深层子目录仍有自己的预算）。
- 修 node_modules 式单目录平铺几百文件吃满全局 limit 200、其他目录看不到的问题。多根下 parent 相对主目录失败时回退绝对路径。
- 遍历逻辑抽为纯函数 `walk_with_budget(base, workspace, limit, depth)`，run() 与单测共用同一实现。

### 1.4 grep：outputMode 三模式（tools/grep.rs）

- `outputMode: content（默认，现状零回归）| files | count`（默认值取保守）：
  - `files`：首次命中顺序去重的文件路径列表，`files_total` 字段；`count`：每文件精确命中数（降序、不为 top-100 截断）。
  - files/count 模式 `offset`/`maxMatches` 按文件分页解释；省略 `file_counts`（冗余）。
- description 引导：大范围探查用 files/count，定位内容用 content。

## 2. 批次二：run 稳定性

### 2.1 主循环 panic 兜底（core/agent.rs）

- **原缺口（严重）**：`run_chat` 裸 `tokio::spawn`，drive 逃逸 panic → task 静默死亡 → `running` 永久卡 true、无 `run:error`、无 checkpoint，会话变砖直到重启进程。
- `run_chat` 内对 `drive_agent` 包 `AssertUnwindSafe(...).catch_unwind()`（**catch 层在收尾之前拦截**，复位 running / checkpoint / `emit run:error("agent 内部错误：...")` 照常执行）。
- `drive_agent` 内加 `DriveUnwindGuard`：panic unwind 时停流式 flush ticker、清 `active_cancel`（正常路径收尾前 disarm——有序收尾需要 await sleep，Drop 无法承担）。
- 收尾路径防锁中毒：`lock_ok()`（`unwrap_or_else(|p| p.into_inner())`）用于 `checkpoint`/`mark_cancelled` 的 history/title 锁，防止 panic 持锁时收尾二次 panic。

### 2.2 子代理 panic 收尾泄漏（tools/subagent.rs）

- **原缺口**：panic 时 `progress.abort()`/`subs.remove`/`sub:done` 全部跳过 → 进度轮询 task 每 800ms 永久 emit `sub:step`、subs 表泄漏、前端子代理卡永远"运行中"。
- 收尾改 `SubCleanupGuard`（Drop）：abort 轮询 + 摘除注册恒执行；armed（异常退出）时补发 `sub:error("子代理内部异常终止")` 让前端收口。正常路径返回前 `disarm` 防双发。

## 3. 批次三：compaction 强化

### 3.1 timeout 可配置（core/config.rs、context.rs）

- `ConfigState.compact_timeout_seconds`（serde default 180，clamp [30, 3600]；结构级 `#[serde(default)]` 保证旧配置兼容）；删除硬编码 `SUMMARY_TIMEOUT` 常量。设置页通用分区新增输入框（`settings.compactTimeout` i18n 中英）。

### 3.2 并发守卫修 TOCTOU（core/agent.rs、host/commands.rs）

- **原缺口**：`compact_session` 的 running 检查与 history 整体替换无互斥（check-then-act），检查通过后立即 start_chat 会让压缩整替历史、丢并发新消息。
- `SessionRuntime` 新增 `compacting: AtomicBool`：手动压缩先 `swap(true)` 占位再复查 running；`start_chat` 在 `running.swap(true)` 成功后检查 compacting，压缩占位期间拒绝新 run；自动压缩同样置位/复位。

### 3.3 事件补全（core/agent.rs、ui/src/stores/run.ts）

- 新增事件键 **`run:compacted`**（成功，payload `tokens_before`）；`run:compacting` payload 扩展 `tokens_before`/`messages`/`timeout_ms`（只加字段，向后兼容）。
- 前端压缩中 notice 带约 tokens；完成推「上下文压缩完成」notice。事件面 25→**26 键**，`events.contract.test.ts` 自动发现机制双向校验通过。

### 3.4 压缩自身 token 计账（core/context.rs、stats.rs）

- **原缺口**：summary 请求的 RunUsage 被直接丢弃，压缩消耗在统计里凭空消失。
- `compact_history` 返回 usage 并在成功路径直接记 stats（**kind=compact**，挂当前模型/workspace）；provider 未回传 usage 时以 `token_est` 估算兜底（transcript≈input、summary≈output）。

### 3.5 失败冷却（core/agent.rs）

- **原缺口**：压缩失败仅 warn 继续，阈值持续超限时每步重试、每次最长阻塞一个 timeout。
- `drive_agent` 内 `compact_fail_streak`：连续失败 ≥2 → 本 run 内不再自动压缩（session_log 提示）；成功即清零。手动压缩不受限。

## 4. 验证

| 项 | 结果 |
|---|---|
| `cargo test`（src-tauri/） | **230 passed / 0 failed**（另 2 个真实 E2E ignored 按需显式跑），0 warning |
| `pnpm --dir ui test` | **63/63 passed**（含事件契约双向校验） |
| `pnpm --dir ui build` | tsc + vite 通过 |
| 新增后端单测 | command×2（盲区落盘/小输出无 spill）、compact×2（command 直通/错误保输出/其他工具仍截断）、list_files×2（预算折叠/根溢出占位）、grep×3（files/count/白名单） |

## 5. 评估后不移植项（及理由）

| 优化项 | 理由 |
|---|---|
| UI 设计 token / mode-sider / caret 系列 | 违反本项目「antd 组件库接管、不手搓皮肤」核心约束 |
| mermaid 编辑器预览 | 本项目聊天+产物弹窗已覆盖且带三级缓存；工作区 TextArea 内嵌渲染属新 feature |
| `timeoutSeconds`→`timeout` 改名 | 每次约 8 token 收益，扰动 schema 稳定性与 provider prompt cache，性价比低 |
| agent-architecture.md | 本项目有自己的 docs 编号体系 |

### 后续立项清单

- 本地 HTTP API（127.0.0.1 + Bearer constant-time + 设置页）
- KB 知识库模式（需先产品定义）
- 剪贴板粘贴文件到工作区（需调研 Tauri 侧剪贴板文件读取）
- 模型目录脚本恢复入库（generate-model-catalog.mjs 已丢失，[docs/composer-toolbar-batch-report](./composer-toolbar-batch-report.md) 仍引用）+ models.dev 快照刷新
- service 已退出保留 post-mortem / Windows GBK 解码核实

## 6. 手动验证清单（界面改动，由用户执行）

> 前置：确认没有 dev 实例与打包版同时运行（单实例互斥）。

1. **设置页新输入**：设置 → 通用 → 「压缩请求超时（秒，30–3600）」：改值 → 保存 → 重开设置页确认回显；输入 10 应被钳到 30。
2. **压缩事件**：选长会话（上下文占比超过压缩阈值）发送消息 → 运行中应看到「上下文压缩中…（约 N tokens）」notice，稍后出现「上下文压缩完成」notice。
3. **fullOutput 行为**（让模型执行验证）：让 agent 跑 `seq 1 5000`（不带 fullOutput 语义的旧会话回放不执行，仅新调用）→ 工具卡输出应带 `[exit 0 | 共 5000 行 | 输出已截断，完整输出：<路径>]`，且该路径文件存在可读。
4. **grep files/count**：让 agent 用 `outputMode:"files"` 或 `"count"` 搜索一个常见词，确认工具卡返回文件列表/计数而非逐行匹配。
5. **list_files 折叠**：在含 node_modules 的项目里让 agent 执行 list_files，返回应出现 `+N more in node_modules`，其他目录条目正常。

## 7. 审查修复（code-reviewer 审查后落地）

> 对本批次做 code-reviewer 七维度审查后的修复。修复后验证：`cargo test` 225 passed / 0 failed / 0 warning（Windows 实测；另有 2 个 `cfg(unix)` 用例仅 macOS 执行、2 个真实 E2E ignored）；`pnpm --dir ui test` 63/63；`pnpm --dir ui build` 通过。

### 🔴 compacting 互斥 panic 卡死（RAII 化）

- **缺口**：自动压缩 `store(true) → compact_history → store(false)`，panic unwind 跳过复位；而 run_chat 的 `catch_unwind` 恰会把 panic 转成「正常」收尾——`compacting` 永久为 true，`start_chat` 一律拒绝（agent.rs）、手动压缩拒绝（compact_session），会话变砖直到重启，与本批次要修的 panic 卡死同类。手动 `compact_session` 无任何 panic 兜底，同症。
- **修复**：`CompactingGuard`（core/agent.rs）——`acquire()` 以 `swap(true)` 原子占位，占位失败返回 `None` **不构造**（误构造的 Drop 会清掉他人占位、破坏互斥）；`Drop` 恒复位。自动压缩与手动 `compact_session` 均改走 guard，防御性占位失败按压缩失败走冷却。
- **回归测试**：`compacting_guard_resets_flag_on_panic_unwind`（panic 注入验证 unwind 后复位 + acquire 占位/互斥/释放语义）。

### 🟡 command 收尾 collector 锁防中毒

- 超时与正常收尾路径 `collector.lock().unwrap()`：pump task 若在持锁期间 panic，锁中毒后收尾 unwrap 二次 panic。改用 `crate::core::agent::lock_ok`（提升为 `pub(crate)` 复用）。

### 🟢 顺带

- signal line 前缀改由调用方给出：正常 `exit N`、超时 `timeout`（原实现超时输出 `[exit timeout | …]` 措辞怪异）。
- grep `OutputMode` 解析收敛为单一 match，去掉不可达的 `Some("content")` 提前分支。

### Windows 平台修复（原开发环境为 macOS，Windows 首跑暴露）

- **`running_flag_resets_after_run_ends`**（master 上即失败，非本批次引入；用 `git worktree` 在 master 单跑复核确认）：mock 服务器 close 时接收缓冲残留未读 POST body，Windows 以 RST 收尾，客户端读不到 401 → 归为可重试错误 → 退避重试超过 5s 截止。修复：mock 先排干请求（100ms 读空闲即认为收完）再应答并显式 `shutdown(Both)`；修复后 0.13s 通过。
- **`per_dir_budget_folds_overflow_without_stealing_quota`**：断言硬编码 `/` 分隔符，Windows 条目为 `\`。修复：断言前把 entries 归一为 `/`。
