# 日志增强：会话级细粒度日志 + 全局诊断日志升级

> 分支 `feat/session-level-logging`。改动背景：原运行日志仅有 tracing 单通道（默认 info、只写 `~/.codewave/logs/codewave.log.YYYY-MM-DD`、约 23 处稀疏埋点、无 error 级、无保留清理、UI 无查看入口），排障时既看不到单次请求/工具的细节，也无法按会话归因。

## 一、会话级细粒度日志（新能力）

每会话一个黑匣子文件，**不受全局级别过滤、常开**：

- **落点**：`core/session_log.rs`，沿用 W5 分流——项目会话与其子代理（继承 project_dir）写 `项目托管目录/logs/<session-id>.log`，自由/临时会话写 `~/.codewave/logs/<session-id>.log`；`zombie` 会话跳过（H5 语义）。进程级写锁防行交错，失败静默；目录存在性有进程级缓存，热路径免每行 `create_dir_all`。
- **格式**：`[2026-09-01T12:00:00.123] [LEVEL] line`，级别 INFO/WARN/ERROR。
- **事件面**（埋点位置）：
  - run 生命周期：开始（权限档/模型/首行消息）、完成（tokens+耗时）、取消、失败 —— `core/agent.rs run_chat`
  - **每次 LLM 请求**：`step N llm 完成 model=… 耗时 …ms tokens in/out cache r/w`；重试、空响应、BadRequest sanitize、最终失败各自一行（错误截 500 字）—— `drive_agent` 流式循环
  - **每次工具调用**：name + ok/ERR + 耗时 + 参数摘要（截 400 字）；失败另记 `[code]` 行 —— `tools/batch.rs emit_result`（覆盖策略拒绝/未知工具/panic 兜底全部路径）
  - 审批：提出/批准/拒绝/超时(120s warn)/随 run 取消 —— `safety/approval.rs confirm`
  - ask 问答：回答摘要、方案批准切档（Plan/ConfirmEach → AutoEdit + todos 基线数）—— `tools/ask.rs`
  - 自动压缩：触发（ratio/threshold）/完成/失败 —— `drive_agent` ④
  - 子代理：spawn（role/steps/task 摘要）记入父会话日志；返回/失败带 usage；子代理自身 LLM/工具轨迹落其独立文件 —— `tools/subagent.rs`
- **详细模式**：`log.session_verbose`（默认 false）。开启后每次 LLM 请求全文与响应全文入会话日志，单条截 64 KiB。仅排障 prompt/模型问题时开启。**请求全文经 `StreamRequest::redacted_json()` 脱敏**（keys 全部替换 `***`）——API key 永不落盘（AGENTS.md「key 不落明文」约束，含回归测试）。

## 二、全局诊断日志升级

- **级别热切换**：`config.json` 新增 `log { level, session_verbose }`（serde default 兼容旧配置）。`core/logging.rs` 用 `tracing_subscriber::reload` 层持有 `Handle`（OnceLock）；`save_config` 检测 `log.level` 变化即时 reload。**`RUST_LOG` 环境变量存在时完全优先**，设置项不覆盖（开发者场景）。设置页「通用」新增「日志级别」下拉与「会话详细日志」开关。
- **14 天保留清理**（补 [docs/p0-plan](./p0-plan.md) §5.2 承诺）：`prune_logs_dir(dir, now, keep_days=14)` 只删文件名严格匹配 `codewave.log.YYYY-MM-DD` 且日期早于保留期的滚动文件——当前活跃文件（日期=今天）天然不动，会话日志与用户文件不误删。启动时执行，失败仅 warn。
- **关键路径埋点**（tracing；与会话日志分工：全局简明、会话细粒度）：
  - `provider/mod.rs stream_model`：完成 debug（model/耗时/tokens）、失败 warn（瞬态，重试由上层控制）
  - `drive_agent`：重试 warn、最终失败 **error**（补齐此前 error 级 0 处的缺口）、空响应/sanitize warn
  - `safety/approval.rs`：审批超时 warn
  - 工具失败：`emit_result` 内 warn

## 三、IPC 与 UI

- 新命令（`host/commands.rs`，已注册 `lib.rs`）：
  - `list_log_files()`：白名单过滤的全局滚动日志列表（name/size/modified，最新在前）
  - `read_log_file(name, tail_lines?)`：名称白名单 `codewave.log[.YYYY-MM-DD]`；尾部至多 2 MiB → 按行截取（默认 500 / 上限 2000），`truncated` 标记更早内容
  - `read_session_log(session_id, project_id?, tail_lines?)`：id 白名单 `[A-Za-z0-9_-]+`（防穿越）；优先内存 runtime 落点，回退按 project_id 解析托管目录；**文件尚不存在（会话没跑过）返回空内容而非错误**（前端空态依赖此语义，有测试守护）
  - `open_logs_dir()`：explorer / open / xdg-open，不引新插件
- **RightBar** 新增「运行日志」区（默认折叠，折叠态零 IPC）：范围切换「会话 | 全局」、自动刷新（2s，可关）、手动刷新、打开目录、WARN/ERROR 行着色、粘底滚动（上翻不拽回，[docs/thinking-scroll-fix](./thinking-scroll-fix.md) 同款体验）。
- 契约：`ui/src/ipc/types.ts` 新增 `LogConfig`（并入 `ConfigState.log`）、`LogFileEntry/LogFileContent`；`client.ts` 四个封装。

## 四、验证

- 后端：`cargo test` 全绿 / 0 warning（新增：session_log 写入与 zombie 跳过、trunc 边界、prune 只删过期、日志名/会话 id 白名单、read_tail 截取、**verbose 请求全文脱敏（key 不落盘）**、会话日志缺失返回空态）。
- 前端：`pnpm --dir ui test` 全绿（新增 RightBar 日志区 3 用例：折叠零 IPC / 展开加载 / **切会话后过期响应守卫**）；`pnpm --dir ui build`（tsc + vite）通过。
- 审查：code-reviewer 七维审查通过（🔴 verbose 明文 key 泄漏已修复；🟡 空态误报/轮询竞态/热路径开销/host 分层下沉/文档失配均已随批修复）。
- 注：worktree 内首次跑测需 `scripts/mcp-test-server.mjs`（该文件被 gitignore，仅存在于主工作区，需手动复制）。

## 五、手动验证清单（界面改动不做自动点验）

1. **级别热切换**：设置 → 通用 → 日志级别改 `debug` 保存 → RightBar 全局视图/日志文件即时出现 debug 行；改回 `info` 恢复。
2. **RUST_LOG 优先**：设 `RUST_LOG=error` 启动 → 改设置级别不生效。
3. **会话日志**：任意会话发消息跑一轮 → RightBar「运行日志」展开，会话视图应出现 run 开始/llm 完成/工具行/完成统计；自由会话与项目会话均生效。
4. **自动刷新**：run 进行中保持展开 → 内容 2s 增长且自动粘底；手动上翻后不再被拽回。
5. **详细模式**：设置开「会话详细日志」→ 下一轮 run 的会话日志出现「请求全文/响应全文」行；关闭后恢复元数据行。
6. **全局视图**：切「全局」→ 文件下拉列出按天滚动文件，选文件看尾部，「目录」按钮打开 `~/.codewave/logs`。
7. **14 天清理**：在 `~/.codewave/logs` 手动放置一个改名为 `codewave.log.2020-01-01` 的文件 → 重启后消失；当日活跃文件与会话日志不受影响。
8. **旧配置兼容**：用无 `log` 字段的旧 `config.json` 启动 → 正常加载，级别默认 info。

## 六、遗留与后续

- 会话日志随会话删除而清理暂未实现（`delete_session` 目前只删索引与历史）——会话日志在保留策略外会持续累积，后续可在删除会话/项目级联时一并移除。
- `RingLog`（tools/service.rs 内存环形缓冲）维持现状，未接入文件日志。
