# steer：排队「↑ 立即」由打断改为中途注入

> 需求：用户在 agent 运行中提交新消息时，不再打断当前会话与子代理，而是把新消息并入当前 run（steer），模型处理完再继续。与 Codex CLI / opencode / pi 三家的口径对齐。

## 一、术语

| 词 | 含义 | 边界 |
|---|---|---|
| **steer**（本文档核心） | 运行中把新消息并入当前 run，**不结束 run、不打断子代理** | 用户视角叫「立即」；后端实现是写入 `rt.inject_tx` |
| **取消** | `cancel_run`：结束 run + 级联停掉全部子代理 | 保留为急停（手动停止 / Esc），语义未变 |
| **注入**（inject 通道） | 后端既有通路：`inject_run_message` IPC → `rt.inject_tx` → drive 步循环开头消化 | 与 steer 是**同一机制的两个视角**：用户提交叫 steer，后端通道叫 inject |

steer 与取消的分界：**是否结束当前 run**。steer 不结束，取消结束。

**接纳契约（2026-10-10 修正）**：IPC 成功表示该消息已经进入本 run 的历史并尝试过 checkpoint；写入 mpsc 本身不算成功。请求以消息 + oneshot 收据入队，驱动消化后应答。空闲、已收尾、缓冲满、未消化即取消/出错/触顶的请求明确失败，前端保留条目；未消化消息不进入下一次 run。详见 [steer-race-and-probe-test-fixes](./steer-race-and-probe-test-fixes.md)。

## 二、业界依据（三家都不做「脱离父批次」）

| | 中途发消息 | 子代理 | 汇总 |
|---|---|---|---|
| **Codex CLI** | 不结束 run，`InputQueueActivity::Steer`；`tasks/regular.rs` 单 `loop` 里 `has_pending_input` 就再跑一轮 `run_turn` | 完整 MAv2（spawn/followup_task/send_message/interrupt_agent/wait） | 子代理完成后发消息给父 |
| **opencode** | 不结束 run，`effect/runner.ts` 的 `ensureRunning` 已在跑就只 await；runLoop 每轮重读消息表取 `latest()` user message | task tool 默认 inline 阻塞；background 是实验开关（`OPENCODE_EXPERIMENTAL_BACKGROUND_SUBAGENTS`） | background 完成 → `inject()` 合成 synthetic user message |
| **pi**（earendil-works/pi） | 不结束 run，`steeringQueue`（当前任务内改向）+ `followUpQueue`（任务后） | 未找到内建 | — |
| **CodeWave** | **同 steer** | 维持现状 inline | 子代理 report 本来就回给父 |

**为什么不做「子代理脱离父批次 + 后续轮汇总」**：该形态需要四件套 —— 子代理脱离 `execute_batch` 的 JoinSet（破坏 `batch.rs`「批次必有完整结果、历史不留悬空 tool_use」硬不变量）、跨轮汇总通道、暂存队列、独立取消链（切断 `parent_cancel` 会连带失去急停能力）。三家的取舍一致：不值得。

## 三、六个时序缺口补丁

调研发现 CodeWave 的 inject 通路（后端就绪、前端零调用）有六个缺口，steer 要成立必须全修。

### F — 强制续跑标记（命门，🔴）

`drive.rs` 消化注入后直接进下一次迭代，但主会话 `finish_on_text = true`：模型若输出纯文本 → 走 `Finish` → run 结束。**新消息进了 history，但模型已用旧上下文答完退出**，用户看到的是 steer 没生效。

→ 消化注入时置 `force_continue`；`text_turn_action` 新增 `steer_continue` 参数，**判定顺序先于 `finish_on_text`**、但**后于 `<report>` 显式汇报标记**（模型主动说「完了」是硬信号，不得对着已完成汇报强启新一轮）。
→ **一次性**：标记由 drive 层在**每次进入判定前**消费并清零，**且消费点与本步是否发起工具调用解耦**（在 `run_llm_turn` 之后即读取）。若把消费点放在 `calls.is_empty()` 分支内，模型消化 steer 后发起工具调用时（最常见路径）标记会滞留到某个恰好输出纯文本的步——可能已是「任务完成后的汇报」，于是强启一轮；run 提前结束则标记永不被消费。
→ 仍受 `MAX_TEXT_TURNS` 硬上限约束 —— steer 不得绕过连续无进展防线。**此承诺在最初提交中未兑现，已于 2026-10-10 修正**：普通纯文本 + steer 在 `text_turns >= MAX_TEXT_TURNS` 时回落 `Finish`；未被消化的消息返回失败并留队。原测试只验证 `rejected=true` 的更早分支，现补普通纯文本矩阵。
→ steer 触发的 `Continue` **不得**注入 `<continue-notice>`：该文案「你没有发起工具调用」对 steer 完全不实，且主会话不得收到它是一条既有硬不变量（此前主会话因 `finish_on_text=true` 走不到该分支，steer 让它首次可达）。

### C — 压缩历史替换窗口可中断（🔴）

`core/context.rs` 的 `*rt.history.lock().unwrap() = new_history` 是不可中断的原子替换。既有 `cancel.cancelled()` 只守住了摘要请求那一段。若信号恰在摘要拿到之后、替换之前到达，压缩照样整体替换历史；而注入消化发生在压缩**之前** → 「注入消息已入 history → 被摘要吞掉」时序上可能。

→ 替换前复查令牌；已取消则保留原历史并放弃压缩（走既有 `compact_fail_streak` 失败路径，不新增状态）。

### E — 消化注入后立即 checkpoint（🟡）

常规 checkpoint 每 `CHECKPOINT_EVERY_STEPS` 步一次，注入消息最多等 19 步才落盘，崩溃即丢。Codex 有 `GRACEFULL_INTERRUPTION_TIMEOUT_MS` 宽限期正为此。

→ 消化注入后立即 `checkpoint()`。只在注入分支调用，不影响常态性能。

### A — 退避 sleep 看取消令牌（🟡）

`sleep_backoff` 此前是裸 `tokio::time::sleep`，退避期间取消不生效 —— 累计 7 次 × 10s 意味着用户点了停止最多要等 10s 才有反应。

→ 改 `select!` 挂 `cancel.cancelled()`，返回 `bool`（false = 被取消，调用方直接收尾为 `Cancelled`）。

### B — 消化注入后 attempt 归零（🟡）

`attempt` 按 step 共享、success 即复位。注入即新话题 → 消化注入时归零。

### D — repair 补位位置语义（🟡，核实后**无需改代码**）

调研担心 `repair.rs` 补孤儿 tool_result 会插到已有 Tool 消息之间。**核实结论：补位天然紧邻** —— 循环是「遍历消息 → 遇到含 tool_use 的 assistant 就 push 一条 Tool」，中间不跳任何消息；wire 组装（`stream.rs` 的 `messages_for_request`）把每条 `Role::Tool` 各自折成独立 user 消息、不合并相邻，故产出的仍是合法配对。

→ **只加护栏注释 + 位置语义测试**（`interrupted_result_is_adjacent_to_its_tool_use`）钉死该不变量，明确「不得改为跨消息补或合并进已有 Tool 消息」（那会在中间插入非 tool_result 的 user 消息，Anthropic 明确非法）。

**已达标、不必重做**：批次取消合成 `E_CANCELLED`（`batch.rs`，有测试钉死）· 出网 repair 兜底 · 400 sanitize 免费重试 · 审批取消按拒绝收口 · 子代理级联取消。

## 四、前端改动

- **`runNow`**（`stores/run.ts`）：运行中 → 在 await 前同步占用 `QueueItem.injecting`，再调 `ipc.injectRunMessage`（**不再调 `cancel`、不再置 `pendingItemId`**）。确认成功才出队；失败释放占用并保留条目。自动出队找第一条未占用项；确认在正常 done 后到达时，按 `queueResumeAfterInjection` 补一次自动出队。取消、错误和启动新 run 均清掉标记，防止旧确认误启队列。**不重排剩余队列**。
- **占用期间**：「立即」禁用并转圈，编辑/删除同时禁用且 store 有守卫。占用是进程内状态，磁盘快照只保留 id/text/images；关闭但保留队列时，IPC 的成功/失败仍结算驻留条目，重开不重复发送也不永久占用。
- **带图条目降级**：`inject_run_message` 只收纯文本 → 带图项在运行中**禁用**「立即」按钮（否则是静默 no-op，用户只能靠悬停才知道点了没用）；留队列等 run 结束，tooltip 为 `queue.runNowTipImage`。
- **`run:cancelled` handler**：`pendingItemId` 出队分支删除（Q21 后不可达）。
- **`pendingItemId` 字段保留但停止写入**：删除会波及约 20 个测试文件的状态桶，回归风险高于收益。
- **静默消化**：沿用既有 `run:inject` notice，**不新增事件键、不改 29 键契约**。
- **i18n**：`queue.runNowTip` 文案改写（原文案「打断当前任务并立即执行该条」与新语义矛盾），新增 `queue.runNowTipImage`，zh/en 键集合一致。

## 五、契约改写

- `docs/run-queue-and-ask-revamp.md` §一：队列语义中「↑ 立即 = 打断当前运行（与手动停止等效）」整条改写；「inject 通道与队列语义不同」改为「队列 steer 是 inject 通道的首个前端消费者」。
- 事件面 29 键：不新增、不改名。
- `docs/session-history-storage.md`：checkpoint 节奏补一句「注入消息不等常规节奏，消化即落盘」。
- `docs/mode-gate-and-subagent-sync.md`：澄清 steer 只注入主会话、不影响档位传播。

## 六、验证

下列计数为原 steer 开发期快照；2026-10-10 的时序修复、可重复测试与最新门禁见 [steer-race-and-probe-test-fixes](./steer-race-and-probe-test-fixes.md)。

- 后端 `cargo test`：1161 passed（含新增 `steer_continue_forces_one_more_turn`、`explicit_report_wins_over_steer_continue`、`steer_injected_message_forces_one_more_turn_end_to_end`，以及 repair 位置语义用例）。**端到端用例已做判别力验证**：临时关掉 `force_continue` 置位即转红（`left: 1, right: 2`）。**2 个存量失败与本次改动无关**：`shell_invocation_per_variant`（本机无 Git Bash，`main` 基线即红）、`service_lifecycle`（时序敏感偶发）。
- 代码审查（7 维度）首轮结论**需返工**：3 个 🔴 —— 标记在工具调用路径滞留导致承诺不成立 / steer 的 Continue 打破了主会话不注入 `<continue-notice>` 的硬不变量 / steer 压过 `<report>` 显式汇报。三项均已修复并补上集成层用例。
- 前端 `pnpm test`：1223 passed / 101 files（含重写 1 个 + 新增 3 个队列用例）。
- 前端 `pnpm build`：通过。
- 前端 `pnpm lint`：通过。
- 界面改动不做GUI 自动点验 —— 手动验证清单见下。

## 七、手动验证清单

1. 长任务运行中在 Composer 输入文字回车 → 队列出现条目；点某条「↑ 立即」→ **当前任务不中断**（不出现「已取消」notice），条目消失，出现「已注入 N 条消息」notice，模型接着处理这条消息。
2. 验证命门：让模型即将收尾（输出纯文本）时点「↑ 立即」→ 模型**不会**就此结束，而是再走一轮处理新消息。
3. 派一个长跑的 `explore` 子代理，等它跑到一半时点某条「↑ 立即」→ 子代理**不中断**，继续跑完并出 report；主 run 处理新消息。
4. 点「停止」按钮 → 主 run 与子代理**都**停（急停语义未变）。
5. 给队列项粘图片后点「立即」→ 运行中该按钮**置灰不可点**，tooltip 显示「含图片附件：无法插入当前运行，只能等本轮结束后执行」；run 结束后自动执行。
6. 点「↑ 立即」时若后端注入失败（如会话刚被删）→ 条目留在队列不丢。

## 八、遗留与已知缺口

- `delete_session` / `delete_project` 不遍历 `core.subs`，本次未改（与 steer 无关，steer 不产生 detached 任务）。
- steer 消息在会话恢复后与任务开始时的消息同形，回看时无法区分（需求分析开放问题 1，已确认本期不做）。

## 九、收尾与接纳同步（2026-10-10）

- `SessionRuntime.inject_accepting` 的检查、通道发送、空队列收尾和拒绝未消费项共用 `inject_rx` 的锁。**不能**在 try_send 后只复查原子标志：消息可能已被消费却被误报失败，前端将再发一次。
- `drain_inject` 统一步首与收尾前的历史追加、`run:inject.count`、checkpoint 和收据应答；MutexGuard 不跨 await。
- 纯文本 `Finish` 与 `batch_done` 在有后续步数预算时复查。若消化到消息，复位 retry 预算并续跑；空队列则在同一临界区关闭接纳窗口，再收尾。
- 纯文本复查续跑计入 `text_turns`，达到上限或无剩余步数时不再消费新项。取消、错误、触顶与 panic 均关闭窗口并拒绝未消费项；流式 flush 等待期也不再接纳。
- suggest 收尾被新消息延续时，清掉旧 `suggest_out` 并以现有 `run:suggestions` 事件发空数组，避免旧任务的建议留在新任务结果中；事件键不新增。
- 已被模型请求看到的消息仍遵守 `<report>` 优先级。当前响应途中才到达、尚未被看到的消息由收尾复查处理。
- checkpoint 失败仍沿用既有保存状态上报；注入成功保证本 run 的内存历史归属，不额外宣称磁盘写入必定成功。
- `TabRunState.pendingItemId` 已成为**死字段**（无写入方、无读取方）。保留而非删除是为了不波及约 20 个测试文件的状态桶；后续清理时可直接删除并同步清理状态桶。
- `TextTurnAction::ContinueWithReminder` 全局无构造点（返工前既有的死代码，本次未加剧）。
