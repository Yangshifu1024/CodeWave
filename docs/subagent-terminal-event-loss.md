# 子代理终态事件丢失 → 卡片永久「运行中」

> 缺陷修复。分支 `fix/subagent-terminal-event-loss`（基线 HEAD `3019979`）。

## 现象

会话 `a99e0f9b-fcec-41fa-b398-633bde7552d5` 中派出 3 个 explore 子代理。**后端 3 个子代理全部正常返回了 report**
（`drive_agent` 返回 `Ok`、`cleanup.armed = false`、`drop(cleanup)` 已执行、7 个子代理历史完整落盘），
但界面上：

- 3 张子代理卡仍是转圈（`status === "running"`）
- composer 工具条仍显示「3」（数 `status === "running"` 的子代理）
- 子代理卡的停止按钮仍在（该按钮仅在 `status === "running"` 时渲染，`SubagentItemCard.tsx:55`）
- 卡片上的步数（34/40、33/40、21/25）与 token 数**在更新** —— 说明 `sub:step` / `sub:usage` 持续到达

## 决定性判别证据

后端 `src-tauri/src/tools/subagent.rs:562-574`，`sub:usage` 与 `sub:done` 是**同一个 `sink.emit`、同一个 tick、相隔 3 行**：

```rust
:562   sink.emit(... "sub:usage" ...)   // ← token 数在更新（168.8k / 139.2k / 80.8k）
:569   sink.emit(... "sub:done"  ...)   // ← 不生效
```

⇒ `emit_to` 通路健康，**失败的不是发射**。

## 已证伪的假设

| 假设 | 证伪依据 |
|---|---|
| `emit_to` 发射失败 | `sub:usage` 同一 tick 相隔 3 行就成功，通路健康 |
| 事件注册时序（`bindEvents` 串行 `await listen`） | `sub:spawn` 注册位置**更靠后**却成功建卡 —— 能收到 spawn 就必然已完成全部 29 键注册 |
| 桶 / subId 不匹配 | `sub:step`、`sub:usage` 用同一套 `tabs[p.session]` + `subs.find(subId)` 判据，均正常更新 |
| 恢复路径覆写状态 | `run.ts:390/393` 恢复时写死 `status: "done"`，不会重置成 running |
| immer producer 抛错回滚 | `closeRunningTools`（`runFrames.ts:213-223`）全程 `if (st)` 守卫，不会抛错 |
| 打字机 | `TypewriterText` 只作用于助手文本段（`segments.tsx:451`，开关 `streaming && i === tailIdx`）；子代理卡走 `segments.tsx:440` 的 `<SubagentItemCard>`，独立组件 |
| `web_fetch` 无超时导致真挂起 | 3 个子代理均正常返回，非挂起 |

**断点收敛**：`ui/src/stores/runHandlers.ts` 的 `sub:done` handler 内部（尚未由实测最终定位，见「遗留」）。

## 溯源

**不是任何功能需求引入的。**

- `let _ = self.app.emit_to(...)` 来自 `d9dfbf0 Init for opensource` —— 开仓初始代码
- 子代理卡无兜底收尾同样自开仓起存在

属**长期设计缺口**，被「并发派多个子代理」的场景放大暴露（单子代理时终态事件丢帧概率低、且用户不易察觉）。

## 改动

### 1. 兜底修复（核心）

**文件**：`ui/src/stores/runHandlers.ts`

新增 `settleRunningSubs(t)`，接入 `run:done` / `run:error` / `run:cancelled` **三处**收尾点。

主会话早就有兜底（`run:done` → `closeStreamingAssistantItems(t)` + `closeRunningTools(t)`），
**子代理侧此前完全没有** —— 这正是本故障表现为「永久转圈」而非短暂错位的根因：
`sub:done` / `sub:error` 是子代理卡唯一的收尾途径，丢一帧即永久卡死。

```ts
function settleRunningSubs(t: TabRunState): void {
  for (const sub of t.subs) {
    if (sub.status !== "running") continue;
    sub.status = "done";
    if (!sub.ended) sub.ended = "no_report";
  }
  for (const st of Object.values(t.subStreams)) {
    if (st.status === "running") st.status = "done";
  }
}
```

三条设计约束：

- **可无条件收敛**：主 run 收尾时子代理要么已完成（终态事件先到，无影响）、要么已随主 run 级联取消
  （后端 `parent_cancel` 传递），不存在「仍在跑」的真子代理 —— 故不会误杀。
- **`ended: "no_report"` 而非静默改 done**：报告可能已到（`sub:report` 先于 `sub:done` 发射），
  卡片仍应按「未按约定汇报」口径展示橙色警示，不伪装成干净完成（见
  [subagent-text-turn-premature-exit](./subagent-text-turn-premature-exit.md) 的 `ended` 三态语义）。
- **幂等**：已终态的卡不动，重复 `run:done` 无副作用。

### 2. 次生修复

**文件**：`ui/src/stores/runHandlers.ts` 的 `sub:step`

加 `if (sub.status !== "running") return;`。后端 `sub:done`（`subagent.rs:569`）发射**早于**
`progress.abort()`（`:629`，中间隔着 `save_sub_history` + stats 记录），该窗口内到达的迟到 tick 会把
`sub.step` 覆写回轮询采样值，表现为「卡片已翻 ✓ 但步数还在跳」。

**这不是本次卡死的根因**，是独立缺陷 —— 但它会造成用户观察上的混淆，一并修。

### 3. 可观测性（探针）

**文件**：`src-tauri/src/host/events.rs`

```rust
// 改前：发射失败被静默吞掉
let _ = self.app.emit_to(...);

// 改后
if let Err(e) = self.app.emit_to(...) {
    tracing::warn!(target: "sub_diag", event, err = %e, "事件发射失败（前端收不到）");
}
```

**文件**：`ui/src/stores/runHandlers.ts` 的 `sub:done`

加 `[subdiag] enter` / `committed` / `producer THREW` 三条日志与 try/catch（原逻辑逐字不动），
配合后端日志用四象限二分定位断点：

| 后端 emit | 前端 | 判读 |
|---|---|---|
| ERR | 无 enter | 发射失败 → 后端给终态事件加重试 |
| ok | 无 enter | 投递层丢事件 → 查监听生命周期 |
| ok | enter + tab 为空 | 桶被删 → 前端桶守卫 |
| ok | enter + catch 命中 | handler 抛错，探针点名行号 |
| ok | enter + committed | 写入成功未渲染 → 转渲染层 |

**遗留**：这两处探针待实测定位断点后移除（兜底修复不依赖探针）。

## 回归用例

`ui/src/__tests__/run.subagent.test.ts` 新增 5 条：

1. `run:done` 兜底：残留 `running` 的子代理卡收敛为 `done` + `ended: "no_report"`（终态事件丢了也不永久转圈）
2. `run:done` 兜底幂等：已终态的子代理不被改写（不污染正常 `sub:done` 路径的 `ended: "report"`）
3. `run:error` 与 `run:cancelled` 同样收敛残留 `running` 的子代理卡
4. `sub:step` 次生：已收尾的子代理不再被迟到 tick 覆写步数
5. `sub:step` 仍正常更新 `running` 状态子代理的步数（守卫不误伤）

## 验证

| 检查 | 结果 |
|---|---|
| `cargo test` | 1249 passed, 0 warning |
| `pnpm --dir ui test` | 1353 passed（1348 + 5） |
| `pnpm --dir ui run lint` | 通过 |
| `cargo fmt --check` | 通过 |
| `pnpm --dir ui build` | 通过 |

事件键名未变动，29 键契约（`events.contract.test.ts`）不受影响。

## 遗留

- **实测定位最终断点**：需用新代码重启后复现，借助 `[subdiag]` / `sub_diag` 日志确认 `sub:done`
  究竟断在发射、投递还是 handler 内部。兜底修复已保证「即使事件再丢也不永久转圈」，故该项不阻塞交付。
- **`web_fetch` 无超时**（`tools/web_fetch.rs:161-229`）：`guarded_get` 与 `read_body_limited` 无任何
  `timeout` 包装，是唯一无界的网络工具（对比 `http_request.rs:105`、`mcp/manager.rs:990`、
  `command/tool.rs:957` 均有）。对端连上不发数据 → tool task 永久 await → `join.join_next()` 永不返回
  → **真挂起**。与本次故障无关（本次 3 个子代理均正常返回），建议另开任务。
- **子代理工具执行期无墙钟看门狗**：`stall_watchdog` 仅覆盖 LLM 流（`drive.rs:1499`），叠加任一无界工具即成永久挂起。