# 工具卡实时可见与单卡落定（tool-call-card-live-key）

> 类型：缺陷修复（P0 分类：缺陷）· 影响层：`tools/batch.rs`（发射层）+ 前端工具卡渲染与 run 收尾 · 契约影响：**事件面 28 → 29 键**（新增 `tool:start`）/ config schema 零变化 / 无数据迁移。
> 用户报告：调用工具时显示「正在运行」但看不到工具与参数；调用结束后**多出一条**「已使用」，而上一条「正在运行」仍保持运行中样式。

## 1. 根因

同一张工具卡在**两条通道上用了两个不同的 key**，前端按 key 建锚点，于是同一次调用被建成了两张卡。

| 通道 | key 来源 | 位置 |
|---|---|---|
| 运行中卡（进度帧 `Frame::ToolProgress`） | **批内位置 `i`** | `tools/batch.rs` 中 `run_tool(&core, &rt, &call, &batch_id, i, ...)` → `emit_tool_start(..., i)` |
| 结果卡（`tool:result` / `tool:error` 事件） | `NormalizedCall.index` | `tools/batch.rs` 的 `emit_result`：`call_key = format!("{batch_id}:{}", call.index)` |

而 `NormalizedCall.index` 在 **anthropic 协议下等于 SSE `content_block_start` 的内容块下标**（`provider/anthropic.rs` 取 `v["index"]`）。只要该轮 assistant 消息里 thinking / text 块排在 tool_use 之前（extended thinking 常态开启），两者必然错位：thinking 占 0 → tool_use 得 1，于是进度帧 key = `{batch}:0`、结果 key = `{batch}:1`。

前端 `ensureToolAnchorIm`（`stores/runFrames.ts`）以 key 建锚点：

- 第一步（进度帧）建出 `status: "running"` 的卡；
- 第二步（结果事件）key 不同 → **新建**一张 `ok` 卡，旧卡再无任何事件更新，永久停留「正在运行」。

这与截图里「运行中 read / 已使用 read / 运行中 edit / 已使用 edit」四张卡完全吻合。`openai_chat` / `openai_responses` 两条协议因 index ≡ 批内位置而不触发——这正是缺陷「偶发」观感的来源（同一模型换协议即消失）。

**运行中看不到参数**是第二个独立成因：`emit_tool_start` 只发工具名、不发 args；而 `ToolCallCard` 的头部摘要与展开体全部只读 `tool.argsPreview`，该字段此前仅在 `tool:result` / `tool:error` 事件里随 `args_preview` 赋值。

**附带发现的同族观感问题**：`emit_tool_start` 位于审批门 / 计划范围门**之后**，于是 ConfirmEach 档写文件时，审批弹框期间界面上根本没有卡片（只有弹框）。

## 2. 修复方案（决策记录）

方案经 4 轮结构化质询（grilling）收敛，以下 11 项决策逐条锁定：

| # | 决策点 | 结论 | 理由 |
|---|---|---|---|
| 1 | `call_key` 唯一真相 | **批内位置 + `execute_batch` 入口重编号**（`calls[i].index = i`） | 一处改动让进度帧 index、`tool:start` / `tool:result` 的 `call_key`、`ToolCtx.call_index/call_key`、command 工具进度帧全部同源；provider 侧下标不得泄漏到前端 key |
| 2 | 「运行中参数」的通道 | **新增命名事件 `tool:start`**（事件面 28 → 29 键） | 受 `events.contract.test.ts` 双向守护，生命周期语义比复用进度帧更正 |
| 3 | 状态迁移建模 | **单事件带 `phase`，同一 `call_key` 可发两次**：门前 `waiting` → 门通过后 `running` | 事件面只 +1；审批通过无需另造事件 |
| 4 | 空 chunk 起始帧 | **移除**（`emit_tool_start` 只发事件） | 避免两条并行建卡路径（将来任一漂移即重现本缺陷）；`tool_progress` 帧回归纯进度语义（command 真实输出分片） |
| 5 | 事件覆盖范围 | **仅 `run_tool` 层**（MCP + 注册表工具） | 批层硬门拒绝（排除集 / 未知工具 / 批次写冲突 / 计划硬门）本就只有一张结果卡，不必造闪卡 |
| 6 | 「等待确认」视觉 | **橙点**（`--ws-warn`） | 沿用「色彩强度映射风险等级：无彩色=默认、橙=需注意、红=危险」的既有约定 |
| 7 | 「已中断」呈现 | **中性灰 + 「已中断」**，复用既有 `st-neutral` 机制（不新增 status 值） | 同 `ask` 未作答中性化先例：用户自己的取消不该渲染成系统错误 |
| 8 | 防护网范围 | run 收尾时**扫全 Tab**（所有 assistant 项 + 所有子代理流） | 历史脏数据一并清理 |
| 9 | 摘要同名冲突 | **最短可区分后缀**（`a/ui.ts` / `b/ui.ts`），无冲突时与旧 basename 行为逐字节一致 | 修掉「ui.ts, ui.ts」看起来像重复卡的观感 |
| 10 | 历史文档计数 | **只改现行契约文案**，历史批次报告保持快照语义 | 避免大面积回改历史报告 |
| 11 | 恢复路径遗留 running | `messagesToSubStream` 加 `settleRunning` 闸（**仅会话未运行时**落定） | 避免把真正在途的调用误标为中断 |

## 3. 契约（前后端冻结）

新增事件 **`tool:start`**（事件面 28 → 29 键），事件名以**字面量**出现在 `.emit(` 语句内（契约测试正则要求）：

```json
{
  "session": "<会话 id>",
  "batch_id": "<批次 id>",
  "call_index": <批内位置>,
  "call_key": "<batch_id>:<call_index>",
  "tool": "<工具名>",
  "args_preview": "<入参 JSON；>200k 时退化为 {\"_args_truncated\":true,\"hint\":\"...\"}>",
  "phase": "waiting" | "running"
}
```

- 不含 `run_id`（`run_tool` 无该上下文，前端也不消费）。
- 同一 `call_key` 可能**先 `waiting` 后 `running`**（写工具在 ConfirmEach / G3 范围门期间为 waiting，门通过后转 running）；只读工具只收到一次 `running`。
- 被门拒绝的调用不会出现 `running`（随后直接来 `tool:error` 结果事件，前端把该卡翻失败）。
- 非 `run_tool` 路径（批层硬门拒绝 / 未知工具）**不发** `tool:start`。

`Frame::ToolProgress` 结构**未改动**（仍是 `{batch, index, chunk, name}`），只是不再有空 chunk 起始帧——参数只走 `tool:start` 事件，避免字段冗余。

## 4. 改动清单

**后端**
- `tools/batch.rs`：`execute_batch` 入口重编号；新增共用 `args_preview_json`（`emit_result` 改用它，截断规则不变）；`emit_tool_start` 改发 `tool:start` 事件（带 `phase`）；门条件上提为具名布尔 + **G3 闭包 `g3_gate`**（在「发 waiting」与门本体两处各调用一次同一份表达式——既避免判定漂移，也不把条件提前到门 1 的 `await` 之前，从而不产生安全门 fail-open 窗口）。
- `core/agent/drive.rs`：`NormalizedCall.index` 注释改为「批内位置」。
- `core/agent/runtime.rs`：`EventSink::emit` 的过期注释「27 键」→ 29 键。

**前端**
- `ipc/types.ts`：新增 `ToolStartEvent`。
- `stores/run.types.ts`：`ToolView.status` 加 `"waiting"`。
- `stores/runFrames.ts`：新增 `findToolViewInItems` / `updateToolFromStart` / `applyToolStart` / `settleRunningTools` / `closeRunningTools`；`messagesToSubStream(msgs, settleRunning)`。
  **「按 call_key 定位已有卡」在主会话的三条路径上必须一致**（`onToolStart`、`onToolResult`、`tool_progress` 帧）：锚点可能落在更早的 assistant 项（跑批期间 `run:inject` / `sub:error` 会给 items 末尾 push notice，令 `currentAssistantIm` 另建项），只查末项就会为同一次调用建出第二张卡。故把状态迁移拆为 `updateToolFromStart(tool, payload)`（容器已稳定时用），三条主会话路径统一「先 `findToolViewInItems` 命中、未命中才建」。
- `stores/run.ts`：新增 `onToolStart`（先判 `t.running`，迟到开始事件不建卡）；`onToolResult` 主会话分支改「先查后建」（跨 assistant 项复用锚点，修掉跑批期间 `run:inject`/notice 挤出新流式项导致的另开卡）。
- `stores/runHandlers.ts`：`tool:start` 接线；`run:done` / `run:error` / `run:cancelled` 后 `closeRunningTools(t)`；`sub:done` / `sub:error` 后 `closeRunningTools(t, p.sub_id)`（只扫该子流，不误伤主会话在途工具）。
- `features/tools/ToolCallCard.tsx`：`waiting` 动词分支；中性错误码映射泛化（`E_ASK_CANCELLED` / `E_ASK_NOT_ANSWERED` / `E_INTERRUPTED`），`E_INTERRUPTED` 不渲染空 message 的错误行；摘要改最短可区分后缀。
- `utils/path.ts`：新增 `shortestUniqueLabels`。
- `theme/app.css`：`.tool-card.st-waiting .dot`（橙点；**必须显式声明**，否则会落到基类的 `--ws-ok` 绿点被读成「成功」）。
- `i18n/zh-CN.ts` + `en-US.ts`：`tools.waiting`、`tools.interrupted`（双侧同步）。

**文档与计数**：键数文案 28（或陈旧 27）→ 29 共 7 处：`AGENTS.md`、`CONTRIBUTING.md`、`ui/src/ipc/events.ts`、`ui/src/stores/runHandlers.ts`、`ui/src/stores/run.ts`、`src-tauri/src/core/agent/runtime.rs`、`src-tauri/src/host/events.rs`；另 `ui/src/__tests__/appshell.restore.test.tsx` 的注释同步。新增 `docs/tool-call-card-live-key.md` 并在 `docs/0-README.md` 登记。

## 5. 验证

| 项 | 结果 |
|---|---|
| `cd src-tauri && cargo test` | **775 passed / 0 failed / 3 ignored**，0 warning；`cargo fmt --check` 干净 |
| `pnpm --dir ui test` | **732 passed / 71 文件**（基线 702） |
| `pnpm --dir ui build` | `tsc --noEmit && vite build` 通过 |

关键回归用例（均先失败后通过，后端三条以变异验证过断言链）：

- 后端 `start_event_and_result_share_same_call_key`：批内两调用**故意**把 `NormalizedCall.index` 置 5 / 9（模拟 anthropic 内容块下标漂移），断言 `tool:start` 与 `tool:result` 的 `call_key` 逐一相等（`{batch}:0` / `{batch}:1`），且全量条目中不出现 `{batch}:5` / `{batch}:9`。
- 后端 `start_event_phase_is_waiting_when_confirm_required`：ConfirmEach 写工具先 `waiting` 后 `running`；同批只读工具只出现 `running`。
- 后端反向断言三条：`gate_denied_start_event_never_reaches_running`（门被拒不发 `running`）、`batch_level_rejection_emits_no_start_event`（批层硬门不发任何 `tool:start`）、`unknown_tool_emits_no_start_event`；另 `progress_frame_only_carries_real_output_chunks` 钉住「空 chunk 起始帧确已移除」。
- 前端 `run.interleave`：帧与结果同 key → **同一张卡** running → ok 且 items 不增；notice 插队后（`run:inject` / `sub:error`）结果与分片帧仍回原卡、**跨项不另建卡**；`run:done` 把在途卡落定为 `E_INTERRUPTED`；run 结束后迟到的 `tool:start` 不建卡。
- 前端 `runFrames`：`updateToolFromStart` 的相变与迟到守卫（同一引用不重建卡）；已落定卡不被翻回。
- 前端 `run.subagent`：`sub:done` 只收尾该子流，主会话在途卡保持 running。
- 前端 `utils.path`：冲突扩段、空串冲突组保持空串、反斜杠输入、部分可区分组。
- 前端 `events.contract`：键数硬锚点 28 → 29。

## 6. 手动验证清单（界面改动不做 GUI 自动点验）

用 anthropic 模型（开启思考）发一条会「读两个不同目录的同名文件 + 编辑」的消息：

1. **运行中即显示参数**：卡出现时头部就有工具名与文件名摘要（不再是空白），同名文件显示为可区分后缀（如 `a/ui.ts, b/ui.ts`）。
2. **同一张卡落定**：调用结束后**不新增卡片**，原卡由「正在运行」翻为「已使用」并可展开看结果；全程无残留的「正在运行」卡。
3. **command 卡**：同样不再重复（输出分片帧与结果同 key）。
4. **等待确认**：切 ConfirmEach 档写文件 → 审批弹框期间卡片为**橙点「等待确认」**，批准后转「正在运行」，拒绝则翻失败。可在弹框停留期间插入一条消息或触发一次子代理失败（制造 notice 插队），卡仍应只有一张。
5. **已中断**：运行中取消 → 在途工具卡变**中性灰「已中断」**（不是红点失败，也无 `E_INTERRUPTED:` 噪声行）。
6. **子代理过程抽屉**：打开一个「过程里缺配对结果」的归档子代理抽屉，在途调用显示为「已中断」而非永久转圈（主会话历史恢复不在此列，见 §7）。

## 7. 已知遗留（本次刻意不做）

- **主会话历史恢复**仍把「无配对结果」的调用呈现为「已使用」（`stores/run.ts` 的 `restoreFromMessages`），与子代理流的「已中断」语义不一致；改动会影响既有会话的展示口径，另开批次。
- **闸排队**（工具信号量 / 文件写锁等待）不表达为 UI 状态：UI 上的「正在运行」包含「排队中」，`waiting` 只表示审批 / 范围确认。
- **`ToolCtx.call_key` 无生产消费者**（仅测试赋值），本次未清理，保持与 `emit_result` 同源。
- 「等待确认」卡未提供取消按钮（审批弹窗自身已有忽略 / 取消）。
- **`run:retry` 的清场只作用于末位流式项**（`stores/runHandlers.ts`）：若上一尝试期间 notice 插队使工具卡落在更早的 assistant 项，该卡不被 retry 清掉，会存活到 run 收尾被标「已中断」。卡不重复、最终状态正确，仅位置可能停留在重试前；属 retry 清场范围，另开。
- **历史批次报告里的「27 / 28 键」表述保持快照不回改**（含 `docs/lsp-detection-and-settings-ux.md` 的「现为 28 键」）——计数口径以本文件与 `AGENTS.md` 为准（**现行 29 键**）。
