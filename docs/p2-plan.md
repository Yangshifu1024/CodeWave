# CodeWave P2 详细技术方案（完全体，约 3 周）

> 日期：2026-08-30
> 上游：`[docs/technical-design](./technical-design.md).md` §10（P2 定义）、`[docs/p0-plan](./p0-plan.md).md`、`[docs/p1-plan](./p1-plan.md).md`
> 前提：P0/P1 全部 DoD 通过。P2 目标 = 子代理/调度/统计等高阶能力 + 桌面完整体验；SSH 与移动端为评估/可选项

---

## 1. P2 范围总览

| 组 | 内容 | 优先级 |
|---|---|---|
| F 委派执行 | 子代理系统（subagent 工具 + 运行时 + UI） | 高 |
| G 自动化 | 计划任务（scheduled_task + scheduler + 任务中心） | 高 |
| H 度量 | Token 统计（采集/落盘/图表） | 中 |
| I 桌面完整 | 托盘、系统通知、updater | 中 |
| J 扩展边界 | SSH 远程工作区、server 模式、移动端评估 | 低（可裁剪） |

---

## 2. F 组：子代理系统

### 2.1 工具 schema（tools/subagent/）

```json
{ "task": "string（必填，自包含任务描述）",
  "role": "string（必填，如 code reviewer / test runner）",
  "maxSteps": "integer（必填，默认 25，硬上限 1000）",
  "description": "string（卡片标签，≤40 字）",
  "cleanContext": "boolean（默认 true：不注入父会话历史）" }
```

### 2.2 运行时（core/subagent.rs）

- **复用主循环**：P0 的 `run_loop` 抽为参数化函数 `drive_agent(DriveCtx)`，子代理与主会话共用同一实现，仅注入不同 `DriveCtx`：

```rust
pub struct DriveCtx {
    pub runtime: Arc<SessionRuntime>,       // 子代理有自己的轻量 runtime（无 inject、独立 cancel 子树）
    pub system_extra: String,               // 角色注入 + 子代理纪律（原创文案：不得询问用户、不得派生
                                            // 子代理、不得写全局记忆、完成后必须输出最终报告）
    pub tool_filter: ToolFilter,            // 排除集：ask / subagent / scheduled_task / plan / skill
                                            // （P1 后 MCP 工具可用）
    pub budget: StepBudget,                // maxSteps；剩余 20% 时注入一次低预算提醒（瞬态，断点后）
    pub on_event: SubEventTx,              // sub:* 事件流
}
```

- **并发**：全局子代理信号量 4；超出排队（工具结果中告知排队位置）。
- **预算耗尽**：最后一次请求改为"强制汇报"指令（忽略工具循环，直接总结已完成的与未完成的）。
- **取消**：`StopSubagent(runId, subId)` → 子 cancel 树取消 → 汇总已产出为部分报告。
- **结果回填**：最终报告文本（≤ 模型配额）作为 subagent 工具的 ToolResult 回父会话；完整过程不进父历史。

### 2.3 事件与 UI

- 事件：`sub:spawn{subId, role, description, maxSteps}` / `sub:step{subId, step, toolName}` / `sub:tool{subId, call}`（复用工具卡数据，最近 8 条滚动）/ `sub:done{subId, usage}` / `sub:error{subId, err}`。
- UI `SubagentInlineCard.vue`：父消息流内嵌卡（角色徽标 / 步数进度 / token 计 / 最近工具行 / 停止按钮）；并行多个则堆叠。

### 2.4 DoD
- [ ] "用 code reviewer 子代理审查 auth 模块"→ 内联卡实时进度 → 父会话收到结构化报告
- [ ] maxSteps=3 的用例：低预算提醒出现、强制汇报轮触发、无死循环
- [ ] 4 个并行子代理 + 第 5 个排队语义正确；Stop 生效

---

## 3. G 组：计划任务

### 3.1 工具与调度（tools/scheduled_task/ + core/scheduler.rs）

- schema：action 三选一：`create{name, instruction, schedule}` / `list` / `delete{id}`。
- schedule 文法：`cron:<5 段表达式>` | `every:<n m|h|d>` | `once:<ISO8601 本地时间>`；解析校验（cron 用 tokio-cron-scheduler 自带校验）。
  - **后续修正（2026-09-22）**：实际用的是 `cron` crate 的 `Schedule::from_str`（自写 tick 推进）；`tokio-cron-scheduler` 从未被引用，已从 `Cargo.toml` 移除。见 [docs/tasks-module-polish](./tasks-module-polish.md)。
- `TaskTable{id→{name, instruction, schedule, next_run, last_result}}`。
- **执行语义**：
  - 进程本地（重启即清，启动时提示"有 N 个任务未恢复"不自动重建）；
    - **后续修正（2026-09-22）**：项目任务已落盘（`<主目录>/.codewave/projects/<project_id>/tasks/<id>.json`），重启后仍在；仅自由会话任务保持进程内。见 [docs/tasks-module-polish](./tasks-module-polish.md)。
  - 全局串行队列（同时最多 1 个任务 run，避免与用户 run 抢工作区写）；
  - 每 run：全新隔离上下文（system = 核心提示词 + 任务指令；无任何会话历史）；工具集 = 内置全套（无 MCP 以免副作用扩散，文档明示）；独立 StepBudget（默认 30）；
  - 结果：完成 → 通知（I 组 notification）+ `scheduled:done` 事件 + 结果摘要存 `stats/` 附带日志 `tmp/tasks/<id>.log`。
- 工作区约束：任务 run 的 write_roots 与创建它的会话一致（快照保存）。

### 3.2 UI（TaskCenterPanel）
- 面板：任务列表（名称/计划/下次执行/最近结果状态）+ 新建表单 + 运行日志查看；入口在 AppHeader 图标。

### 3.3 DoD
- [ ] `every:1m` 任务连续 3 次准时触发并产出摘要；delete 后停止
- [ ] 任务执行期间用户会话可继续（互不阻塞，任务排队不越权写用户会话历史）

---

## 4. H 组：Token 统计

### 4.1 采集与落盘（core/stats.rs）
- 写路径：run 结束时投递 `UsageRecord{ts, session, model_id, provider, workspace, input, output, cache_read, cache_write, runs:1}` 到有界 mpsc（2048，满则丢弃并计数告警——不阻塞热路径）。
- **生成计时（2026-09-21 追加，[composer-token-rate](./composer-token-rate.md)）**：主会话每个 LLM step 另带 `UsageTiming{gen_ms, ttft_ms, ttft_count, steps}`（成功那次尝试的窗口；`duration_ms` 缺失/为 0 的步整步不计）——`UsageRecord` 字段不变，计时经 `StatsCollector::record_timed` 与记录同行入队；仅主会话 `run_chat` 调用它，`compact`/`title`/`task`/`sub` 仍走 `record`（计时为 0，聚合时整桶排除）。
- writer task：聚合计数器，每 60s 或 512 条 flush 到 `stats/YYYY-MM-DD.json`：

```json
{"date":"2026-09-15","by_model":{"model-id":{"input":…,"output":…,"cache_read":…,"cache_write":…,"runs":…,"gen_ms":…,"ttft_ms":…,"ttft_count":…,"steps":…}},
 "by_workspace":{…},"by_kind":{…},"total":{…}}
```

- `gen_ms`/`ttft_ms`/`ttft_count`/`steps` 均为 `#[serde(default)]`：旧文件缺字段反序列化为 0（不丢旧值）；flush 合并旧文件走 `ModelAgg::merge`，四个桶（`by_model`/`by_workspace`/`by_kind`/`total`）逐桶累加。

- 启动合并：存在未 flush 的当日临时计数先并档；90 天前的文件清理（保留当月 1 号快照）。

### 4.2 查询与图表
- `get_token_stats(range)`：服务端聚合返回（避免前端拉 90 个文件）。
- UI `TokenStatsModal`：30 天柱状图（日总量）、模型占比环图、工作区占比环图——自绘 SVG（无图表库依赖），naive-ui Modal 容器。
- **总览的耗时三项（2026-09-21，[composer-token-rate](./composer-token-rate.md)）**：平均生成速率 / 均步耗时 / 平均 TTFT，**分子分母同域**——逐天遍历 `by_kind` 桶、只取 `gen_ms > 0` 的桶（今天即 `main` 桶），子代理/压缩/命名/任务不带计时的 output 一律不进分子；各分组表不加速度列。

### 4.3 DoD
- [ ] 跑 20 个 run 后当日文件与 UI 数字一致（对账脚本抽样）；重启不丢已 flush 数据
- [ ] 队列打满路径有告警日志且不卡聊天

---

## 5. I 组：桌面完整体验

### 5.1 托盘
- `TrayIconBuilder`：菜单 = 显示主窗口 / 新会话 / 退出；左键单击 → 显示+聚焦（配合 single-instance 已有逻辑）；关闭按钮行为改为隐藏到托盘（设置项 `ui.close_to_tray`，默认 off）。

### 5.2 系统通知（notification 插件）
- 触发矩阵：run 完成/失败/需确认（ask 挂起）且**窗口未聚焦** → 通知（标题=工作区名，正文=摘要）；点击通知 → 聚焦窗口；声音跟随系统默认；通知失败静默（tracing warn）。

### 5.3 updater（私有阶段可选项，文档化但默认关闭）
- tauri-plugin-updater + `tauri signer generate` 密钥（本地保管，公钥入 conf）；更新源 = 静态 JSON（对象存储或私有 GitHub Release 资产）。
- 实现 `check_updates` command 真接插件；设置页"检查更新"按钮；**D7 私有阶段默认不配 endpoint**（command 返回 disabled），文档记录启用步骤。

### 5.4 DoD
- [ ] 托盘显示/隐藏/退出全链路；close_to_tray 开启后行为正确
- [ ] 后台 run 完成且窗口未聚焦 → 收到通知，点击聚焦
- [ ] updater 关闭态下 UI 不报错；本地起一个静态 JSON 源可完成一次端到端假升级（版本号回退法验证）

---

## 6. J 组：扩展边界（可裁剪，单项评估后决定做/不做）

### 6.1 SSH 远程工作区（remote_* 工具族）
- **形态**：会话绑定 `remote target`（`ssh alias:/abs/path`，存会话配置）；工具：`remote_read_file / remote_create_file / remote_edit / remote_delete_path / remote_run_command`。
- **实现选型**（按序评估，取第一个满足者）：
  1. 系统 `ssh` 子进程 + ControlMaster（macOS/Linux 复用连接；Windows OpenSSH 无 ControlMaster → 每命令一连接，可接受）——推荐：零新依赖、复用用户 ssh 配置/agent；
  2. `russh` crate 全内存实现（备选：Windows 体验优化时再上）。
- 读写协议：读 = `ssh <t> 'cat <path>'`；写 = sftp 子系统（`sftp -b batch`）或 base64 stdin 管道；命令执行复用 P0 command 编排但 cwd/root 校验走远程路径前缀白名单。
- 安全校验：命令仍过 L1/L3（L2 AST 分析仅对 bash 远端可靠时启用，否则降级 L1+L3+审批）；路径白名单 = 声明的远程根。
- UI：新建会话时目标选择"本地目录 / SSH 目标"；Header 显示 target 徽标。

### 6.2 server 模式（复用回报）
- feature flag `server`：`host/` 不参与，`axum` 暴露 `POST /v1/chat`（SSE 响应 = Channel 帧协议复用）+ `GET/POST /v1/sessions…` 最小 REST；鉴权：本机回环 + 随机 token（首启打印）。
- Docker：多阶段构建（rust:alpine → distroless/scratch + ca-certificates）；仅 core+git 能力，无工作区浏览 UI。
- 决策点：实现前评估是否确有 headless 需求；无则挂起（成本低、价值也低，不阻塞 P2 收尾）。

### 6.3 移动端评估（只评估，不承诺）
- 结论框架：Tauri 2 支持 iOS/Android，但 CodeWave 核心价值依赖本地 shell/文件系统（command、工作区写）在移动端受限；评估输出 = 一页报告（UI 响应式现状、可用的"遥控本机 agent"形态、阻塞清单），**不投入开发**。

### 6.4 DoD（若实施）
- [ ] SSH：本地→远程 Linux 完成一次"读-改-跑"闭环；断网时错误干净可重试
- [ ] server：curl 发起对话收到 SSE 流；token 鉴权拒绝未授权请求

---

## 7. P2 验收（Definition of Done）

1. **场景 H（委派）**：主会话并行派 3 个子代理（审查/测试/文档）→ 卡片各自进度 → 汇总报告被主 Agent 整合成最终答复；Stop 单个子代理不影响其余。
2. **场景 I（自动化）**：建 `every:1d` 巡检任务 →（改系统时间或缩至分钟级验证）→ 自动执行并通知；任务中心可见历史。
3. **场景 J（度量）**：两周真实使用后统计页数字与 jsonl 抽样一致；按模型/工作区维度可解释。
4. **场景 K（桌面）**：最小化到托盘后台跑长任务 → 完成通知 → 点击回到窗口，全程无窗口闪烁（Windows CREATE_NO_WINDOW 回归）。
5. 质量门槛：全模块 `cargo test` 绿；三平台打包产物产出（macOS dmg / Linux deb+AppImage / Windows NSIS）；性能回归：子代理并行 4 路时主会话流式无卡顿（帧间隔 P95 < 32ms）。

---

## 8. 里程碑节奏（P2，3 周）

| 周 | 内容 |
|---|---|
| W1 | F 组子代理（运行时复用改造 + UI）+ H 组统计（采集先行，图表后置） |
| W2 | G 组计划任务 + I 组托盘/通知/updater |
| W3 | J 组按评估结论取舍（SSH 或 server 择一实施，另一个仅留设计）+ 三平台打包打磨 + P2 验收对账 |

> J 组默认裁剪建议：SSH 远程若当前无跨机开发场景则顺延；server 模式同理。P2 的"完成"允许 J 组以"评估报告 + 设计留存"形式收尾。
