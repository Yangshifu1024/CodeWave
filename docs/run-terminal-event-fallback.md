# run 终态事件丢失 → 整条兜底链一起失效

> 缺陷分析。2026-10-11 深度排查产出，尚未实施。
> 上游：[subagent-terminal-event-loss.md](./subagent-terminal-event-loss.md)（子代理终态事件丢失的定位与三层兜底）。

## 结论

**子代理卡的修复（#127）只堵住了链条的一环。链条本身仍然成立：run 的终态没有独立真值对账，
一旦 `run:done` / `run:error` / `run:cancelled` 任一丢失，前端无路自愈，且 #127 新增的全部
兜底（`settleRunningSubs` / `closeRunningTools` / `closeStreamingAssistantItems`）因为**同样挂在这
三个事件上**而一起失效。**

同一类结构缺口还有一处：`t.ask` 只有 `ask:closed` 一个清理入口，run 收尾不清它。

## 证据

### 1. run 的「运行中」只有一个写入源

`t.running = false` 全仓只出现在三处，全在 `runHandlers.ts` 的 run 收尾 handler 里：

| handler | 行 | 附带清理 |
|---|---|---|
| `run:done` | :166 | `closeStreamingAssistantItems` + `closeRunningTools` + `settleRunningSubs` |
| `run:error` | :209 | 同上 |
| `run:cancelled` | :225 | 同上 |

三处的清理体是同一套。**`t.ask` 不在其中。**

### 2. `t.ask` 只有 `ask:closed` 一个清理入口

`runHandlers.ts` 全仓写 `t.ask` 的地方只有两处：`ask:opened` 设值（:383）、`ask:closed` 清空（:411）。
run 收尾、切 Tab、重开 Tab 都不碰它（`initTab` 已存在则直接 return；`restoreFromMessages` 只覆盖
`items` / `subs` / `streams`）。

**后果**：`Composer.tsx:66` 的 `askActive = !!active.ask` 为真时，提问卡覆盖整个输入区、
Composer 本体不渲染（见该处注释「zcode 式：ask 面板是唯一底部输入」）。
`ask:closed` 丢失 ⇒ 该会话**完全无法输入**。
自救手段：关闭并重开 Tab（`dispose` 删桶 → `initTab` 重建 `blank()`）；切 Tab 无效。

### 3. 对账机制是单向的

后端有可靠真值且独立于事件管线：

```rust
// core/agent/drive.rs:247-249
// Every exit path converges before advertising an idle session.
// C1 修复：drive 结束（成功/失败/取消）都必须复位，否则会话永远拒绝第二次 run
rt.running.store(false, Ordering::SeqCst);
```

在**所有**退出路径上、且在 `run:done` 发射**之前**复位；`session_running` 命令
（`host/commands/session.rs:572`）直接读这个原子量。

但前端只用了它的一半：

```ts
// stores/sessions.ts:157-162
void ipc.sessionRunning(tab.sessionId).then((running) => {
  if (running) useRun.getState().markRunning(tab.sessionId);  // 只置 true
});
```

注释自称「漂移防护」，但只防「后端在跑、前端不知道」，**不防「后端已停、前端还认为在跑」**——
而后者正是本缺陷所属方向。

### 4. 对账的唯一触发点是切 Tab

`sessionRunning` 仅在 `sessions.activate` 里调用；全仓**没有** `visibilitychange` 或
window `focus` 监听。⇒ 「用户一直盯着卡死的界面、不切 Tab」这个最典型的场景，即使把对账改成
双向也不会被触发。

### 5. `run:done` 的幂等守卫会关掉整条兜底链

`run:done` 开头有一道**另两个收尾 handler 都没有**的守卫：

```ts
// runHandlers.ts:155-161
"run:done": (p) => {
  markUnreadIfAway(p?.session);
  const before = get().tabs[p.session];
  if (!before?.running) return; // 幂等：迟到 done 场景
  if (p?.run_id && before.lastDoneRunId === p.run_id) return;
```

| handler | 幂等守卫 |
|---|---|
| `run:done`（:158） | ✅ `if (!before?.running) return;` |
| `run:error`（:204） | ❌ 无 |
| `run:cancelled`（:223） | ❌ 无 |

`sub:spawn` 建卡**不检查** `t.running`（只判 `if (!t) return`）。于是存在一条通路：

**`run:start` 丢失（`t.running` 恒 false）→ `sub:spawn` 正常到达并建卡 → 子代理正常返回 →
`run:done` 在 :158 直接 return → `settleRunningSubs` / `closeRunningTools` /
`closeStreamingAssistantItems` 全部不执行。**

⇒ 子代理卡永久停在「运行中」，且**当前所有兜底路径一个都不会触发**。
（`run:error` / `run:cancelled` 没有这道守卫，所以这条通路专属于 `run:done`，
而正常结束恰恰走的是 `run:done` —— 即最常见的路径。）

守卫本身是对的（防迟到 done 重复出队），问题在于它**早退时不做任何收敛**，把收敛责任
完全押在这一个事件上。

### 6. 后端 ask 路径缺 panic 守卫（与子代理路径不对称）

子代理路径有 `SubCleanupGuard`（Drop 兜底），保证每条路径含 panic unwind 都必然发出
`sub:done` 或 `sub:error`。ask 路径没有：

```rust
// tools/ask/tool.rs:221-231
ctx.core.sink.emit(&ctx.rt.id, "ask:opened", ask_opened);   // :221
let answer = tokio::select! { ... };                          // :223-226
ctx.core.sink.emit(&ctx.rt.id, "ask:closed", ...);            // :227
```

两行之间若 unwind，`ask:closed` 永不发出；而 `batch.rs` 的 `catch_unwind` 会把 panic 转成
正常错误结果 —— **对用户完全看不出发生过异常**。

## 与 #127 的关系

不冲突，是补链。#127 提供的 `tool:result` 二道兜底只覆盖「`sub:*` 没到而 `tool:result` 到了」，
对「`run:*` 没到」完全无效 —— 因为 `tool:result` 同样是 run 驱动的事件，run 状态失真时它也不可信。

| 终态事件丢失 | #127 之后 | 本方案之后 |
|---|---|---|
| `sub:done` | ✅ 两层兜底收敛 | ✅ 同 |
| `sub:error` | ✅ run 收尾 + `tool:result` | ✅ 同 |
| `run:done` / `run:error` / `run:cancelled` | ❌ 永久卡死，全套防御失效 | ✅ ≤ 看门狗间隔自愈 |
| `ask:closed` | ❌ 输入锁死，需关 Tab | ✅ 同走收尾路径，自愈 |

## 修复设计

### 1. 抽出共享收尾函数 `settleRun(t)`

把三处重复的清理体提成一个函数，并**补上 `t.ask = null`**：

```
t.running = false
t.textRecovered = null / t.widgetAutoOpen = null / t.pendingItemId = null   // 各路径原有差异保留在调用侧
closeStreamingAssistantItems(t)
closeRunningTools(t)
settleRunningSubs(t)
t.ask = null        // ← 新增：补 ask 那一格
```

各 handler 自身独有的语义（notice 文案、suggestions、队列续跑等）仍留在调用侧不动。
ask 缺口由此顺带覆盖，不需要单独一处改动。

### 2. 对账改双向

`sessions.ts:157-162` 改为：返回 false 时同样调 `settleRun`。改动就在现有 3 行里。

### 3. 加看门狗

**触发条件不能只看 `t.running`。** 证据 5 表明「`run:start` 丢失 + `sub:done` 丢失」时
`t.running` 恰为 false，此时按 `t.running === true` 轮询的看门狗不会启动，漏洞依旧。

正确判据是**「后端已空闲」且「本地仍有未收敛状态」**：

```
本轮该查的条件 = !sessionRunning(sessionId) && (
    t.running
  || t.ask
  || t.subs.some(s => s.status === "running")
  || 任一 assistant / subStream 里有 running|waiting 的工具卡
)
```

命中即调 `settleRun(t)`。

轮询节奏：只在上述条件的前件可能成立时启动（`t.running` 为真、或存在任何未收敛状态），
间隔数秒；条件完全不成立时停掉，避免空转开销。本地 IPC 单次开销可忽略。

效果：**丢一个 run 终态事件（或 `run:start` + 任一终态）的后果，从「永久卡死」降级为
「最多延迟一个轮询间隔自动恢复」。**

**实现细节**：`runFrames.ts` 目前只有 `settleRunningTools`（写），**没有**对应的
「是否仍有在途工具卡」谓词（读），看门狗需要补一个，判据与 `settleRunningTools` 保持同源：

```ts
// 与 settleRunningTools 同判据（running / waiting），只读不改
export function hasRunningTools(t: TabRunState): boolean
```

扫描范围与 `closeRunningTools(t)` 的无 subId 分支一致（全部 assistant 项 + 全部 subStreams），
避免出现「谓词漏扫、收尾不漏」或反之的不一致。

### 4. 收窄 `run:done` 的早退（可选）

守卫保留（它防的是真实的重复出队 bug），但**早退前先做一次收敛**：若桶存在且有未收敛状态，
调 `settleRun(t)` 再 return。这一条独立于看门狗，能把恢复延迟压到 0；代价是早退路径多一次扫描。

## 回归测试计划

- `settleRun` 单测：直接喂「running + ask 非空 + 在途工具卡 + running 子代理卡」的 Tab，
  断言四类状态一次性收敛（尤其 `t.ask === null`）
- 对账双向：喂 `sessionRunning` 返回 false，断言 `t.running` 被置 false 且 ask 被清
- 看门狗：用假定时器推进，断言轮询在 `t.running === true` 时发生、`false` 时停止
- 幂等：对已收尾的 Tab 重复调 `settleRun` 无副作用

## 遗留

- **子代理终态事件的根因仍未定位**，已收窄到「事件投递丢失」单一分支（推导见
  [subagent-terminal-event-loss.md](./subagent-terminal-event-loss.md)）。本方案**不替代**根因定位，
  它是在无法修复投递的前提下对丢失做免疫。
- 后端 ask 的 Drop 守卫（第 5 条证据）属于独立改动，可与前端三件一并做，也可拆开。