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
| ~~事件注册时序（`bindEvents` 串行 `await listen`）~~ ~~`sub:spawn` 注册位置更靠后却成功建卡~~ | **本轮复核：原证伪不成立，已撤回。** `bindGlobalHandlers()` 的展开顺序是 `runLifecycle → compact → tool → ask → sub → misc`，`subHandlers` 内部字面量顺序为 `sub:spawn → sub:step → sub:report → sub:usage → sub:done → sub:error`（`runHandlers.ts:419-515`）——**`sub:spawn` 排在 `sub:done` 之前**，所以「收到 spawn」只能推出「注册已推进到 `sub:spawn`」，**推不出 `sub:done` 已注册**。且 `Object.keys` 保持插入序，串行 `await listen` 正是按此序推进。该假设**重新列为待查项**（见「遗留」的 `bindEvents` 脆弱性）。 |
| 桶 / subId 不匹配 | `sub:step`、`sub:usage` 用同一套 `tabs[p.session]` + `subs.find(subId)` 判据，均正常更新 |
| 恢复路径覆写状态 | `run.ts:390/393` 恢复时写死 `status: "done"`，不会重置成 running |
| immer producer 抛错回滚 | `closeRunningTools`（`runFrames.ts:213-223`）全程 `if (st)` 守卫，不会抛错 |
| 打字机 | `TypewriterText` 只作用于助手文本段（`segments.tsx:451`，开关 `streaming && i === tailIdx`）；子代理卡走 `segments.tsx:440` 的 `<SubagentItemCard>`，独立组件 |
| `web_fetch` 无超时导致真挂起 | 3 个子代理均正常返回，非挂起 |

### 本轮追加：重复条目假设的排除与断点收窄

`sub:spawn`（`runHandlers.ts:423`）是**无条件 `push`**——不做 upsert、不按 `sub_id` 去重（`restoreFromMessages`
的归档路径反而有 `if (!t.subs.some(...))` 去重）。而**所有**读侧都是 `subs.find(x => x.subId === …)`，即**取首个匹配**。
两者合起来是一个潜在的不一致：若同一个 `sub_id` 在 `subs` 里存在两条，`sub:done` 只会把**第一条**收敛，
composer 的运行中计数却会把**第二条**算进去 ⇒ 症状看起来「该收敛的没收敛」。

**但本症状可证伪该假设**：子代理抽屉的 `running` 同样取自 `subs.find`（`SubagentDrawer.tsx:23-25`）。
用户观察到的是「**抽屉顶部也在 loading**」，即 `find` 返回的那一条**本身就是 running**。
若真存在重复条目，`find` 会命中先 push 的那条，而它正是 `sub:done` 会收敛到 `done` 的那条 ⇒ 抽屉不该再转圈。

⇒ **抽屉转圈这条证据反证 `subs` 里只有一个条目，且它卡在 `running`。**

由此可再推进一步，构成对断点的强约束：

1. 全仓**只有** `sub:spawn` 会把 `subs[].status` 写成 `"running"`（其余写入点只有 `done` / `error`，见上表）
2. 抽屉 loading ⇒ 该条目存在且为 running ⇒ `sub:done` producer 里的 `t.subs.find(...)` 会命中**同一条**
3. 命中后无条件执行 `sub.status = "done"`，且该 producer 内无任何可抛语句（已证伪）⇒ 必然提交
4. 与「抽屉仍在 loading」**矛盾**

⇒ **`sub:done` 的 producer 从未在这张 Tab 上执行过**。同时，因为抽屉能渲染出这条 running 的卡，
说明 `s.tabs[p.session]` 在事件时刻**是存在的**（`if (!t) return` 那条早退也不成立）。

**断点由此收敛到事件投递边界本身**：`sub:done` 没有到达前端 handler（监听未注册 / IPC 派发丢失），
而非 handler 内部出错。这与上表「已证伪」的各项不冲突——它们各自排除的是 handler **之内**的环节。

> 待实测确证（需实机复现，见「遗留」）：二分探针的四种落点里，只有「后端有warn、前端无 enter」
> 与本推导相容；其余三种（无 enter 无 warn / 有 enter 无 committed / 有 committed 但仍转圈）
> 均已被上面的推导排除。这把探针复现的**预期结论收窄到单一分支**，也说明复现时应优先确认
> 前端是否真的收到过 `sub:done`。

**断点收敛**：事件投递边界（`sub:done` 未到达前端 handler）—— 待实测最终定位，见「遗留」。

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

### 2. tool:result 二道兜底（补强，覆盖 run 收尾前的整段窗口）

**文件**：`ui/src/stores/run.ts` 的 `onToolResult`（主会话分支）

第 1 条的兜底只挂在 `run:done` / `run:error` / `run:cancelled` 上，即**主 run 收尾时**才收敛。但故障表现是
「子代理早已返回、主 run 还在继续跑」——实测单个 run 可长达 `6865s`（会话 `a99e0f9b`），这段窗口里卡片
照样一直转圈，composer 计数也照样不归零，与修复前的观感完全一致。兜底必须挂在**子代理自己的结束信号**上。

`tool:result` 正是这样的信号：后端 `sub:done` 在 `tool.run` 返回**前** emit，`tool:result` 由 batch 层拿到返回值后
**紧接着** emit（`batch.rs` 的 `run_tool` → `emit_result`），两者出自**同一个 task 的相邻位置**。而「subagent
工具返回」本身即证明该子代理已结束。故在 `onToolResult` 里据 outcome 就地收敛：

- 判据：`p.tool === "subagent"`（位于主会话分支；子代理内部工具已在 `!t` 早退分支返回，不会误入）
- 幂等守卫：`sub.status !== "running"` 直接跳过 —— 正常 `sub:done` 先到时卡片已是终态，此处不碰，
  **不会**用兜底口径覆盖 `sub:done` 带回的 `ended` / `steps_used` / `report`
- 逐项回填：`steps_used`、`report`（仅在空时补）、`subStreams[*].status`，并对该子流调
  `closeRunningTools`（只扫该子流，不误伤主会话在途工具）

#### `ended` 回填：刻意**不做枚举白名单过滤**（跨 PR 语义约束）

`ended` 的取值来自**后端刚发出的实时事件载荷**，不是恢复路径的任意历史 JSON。回填时**保留后端送来的任意
字符串**，只在非字符串（缺失 / 脏值）时落 `no_report`。

原因是一条已经踩过的坑：白名单式过滤会在后端**扩展 `ended` 枚举时把新值静默漏判**成 `no_report`。
PR #126（`fix/subagent-budget-ended`）正是把 `ended` 从三值扩为四值、新增 `partial`（撞顶才交汇报 = 成果在手），
其文档已明确警告：

> 白名单式——后端新增 `partial` 时静默漏判，落回绿勾，即本 bug 在新值上重现。

若本兜底按三值白名单过滤，合入 #126 后 `partial`（成果在手、未必做完）会被改写成 `no_report`（未交汇报）——
**主代理会据此重派已在跑的任务**，正是 #126 要消除的那种重复劳动。故此处只做 `typeof === "string"` 的形状守卫，
不做值域过滤：当前三值分支下行为不变，#126 合入后 `partial` 无需改动即被正确保留。

对应回归用例（`run.subagent.test.ts`）钉死了运行时行为：`ended: "partial"` 原样保留不被改写（当前分支类型
仍是三值，用例经 as-any 载荷构造），`ended: 42` 这类脏值落 `no_report`。

**覆盖范围 = 仅成功路径**：后端失败/取消分支的 `ToolOutcome::err` **不带 `sub_id`**（`E_SUBAGENT_STOPPED` /
`E_SUBAGENT`，见 `subagent.rs`），故这两路仍由 `sub:error` + 第 1 条兜底接管。补齐失败路径需在 err 的 data 里
也带 `sub_id`（契约变更，另议）。

三道防线自此完备：`sub:done` / `sub:error`（正常路径）→ `tool:result`（子代理结束的同 task 邻帧）→
run 收尾（最终网）。

### 3. 次生修复

**文件**：`ui/src/stores/runHandlers.ts` 的 `sub:step`

加 `if (sub.status !== "running") return;`。后端 `sub:done`（`subagent.rs:569`）发射**早于**
`progress.abort()`（`:629`，中间隔着 `save_sub_history` + stats 记录），该窗口内到达的迟到 tick 会把
`sub.step` 覆写回轮询采样值，表现为「卡片已翻 ✓ 但步数还在跳」。

**这不是本次卡死的根因**，是独立缺陷 —— 但它会造成用户观察上的混淆，一并修。

### 4. 可观测性：发射失败不再静默

**文件**：`src-tauri/src/host/events.rs`

```rust
// 改前：发射失败被静默吞掉
let _ = self.app.emit_to(...);

// 改后
if let Err(e) = self.app.emit_to(...) {
    tracing::warn!(event, err = %e, "事件发射失败（前端收不到）");
}
```

这是**永久性改进**，随本 PR 常驻：29 键事件面里绝大多数是高频事件，丢几帧无感，
因此发射失败长期无人察觉；只有 `sub:done` 这类**一生只发一次**的终态事件，丢一帧就是永久卡死。

> **关于诊断探针**：定位期曾在 `runHandlers.ts` 的 `sub:done` 加过 `[subdiag] enter` / `committed` /
> `producer THREW` 三条 `console.log` 与 try/catch 包裹，后端 warn 也曾带 `target: "sub_diag"`。
> 合并进 `main` 前**已整体摘除**（前端日志与 try/catch 全删，后端只保留 warn 本身、去掉探针专用 target），
> 避免发布版持续输出诊断日志。排查阶段在排查分支临时加回即可，用完即弃。
> 下面的四象限表保留为**定位方法论**，下次复现时直接照用。

四象限二分定位断点（前端探针日志 ↔ 后端 emit 结果）：

| 后端 emit | 前端 | 判读 |
|---|---|---|
| ERR | 无 enter | 发射失败 → 后端给终态事件加重试 |
| ok | 无 enter | 投递层丢事件 → 查监听生命周期（见「遗留」的 `bindEvents` 脆弱性） |
| ok | enter + tab 为空 | 桶被删 → 前端桶守卫 |
| ok | enter + catch 命中 | handler 抛错，探针点名行号 |
| ok | enter + committed | 写入成功未渲染 → 转渲染层 |

## 回归用例

`ui/src/__tests__/run.subagent.test.ts` 新增 12 条（run 收尾兜底 5 + tool:result 二道兜底 7）：

**run 收尾兜底（5 条）**

1. `run:done` 兜底：残留 `running` 的子代理卡收敛为 `done` + `ended: "no_report"`（终态事件丢了也不永久转圈）
2. `run:done` 兜底幂等：已终态的子代理不被改写（不污染正常 `sub:done` 路径的 `ended: "report"`）
3. `run:error` 与 `run:cancelled` 同样收敛残留 `running` 的子代理卡
4. `sub:step` 次生：已收尾的子代理不再被迟到 tick 覆写步数
5. `sub:step` 仍正常更新 `running` 状态子代理的步数（守卫不误伤）

**tool:result 二道兜底（7 条）**

6. `sub:done` 丢失时按 `outcome.data.sub_id` 就地收敛，并回填 `ended` / `steps_used` / `report` +
   `subStreams[*].status`
7. 幂等：`sub:done` 已收尾的卡不被兜底改写（`ended` / `step` / `report` 均保持正常路径带回的值）
8. 守卫不误伤：非 `subagent` 工具的结果即使携带 `sub_id` 也不改动子代理状态
9. 失败路径不由兜底收敛：`outcome` 不带 `sub_id` 时保持 `running`（仍交 `sub:error` / run 收尾兜底）
10. `outcome` 缺 `ended` 时保守落 `no_report`，不伪装成干净完成
11. **`ended` 原样保留后端送来的值，不按枚举白名单过滤**：`ended: "partial"` 不被改写成 `no_report`
    （钉死上文那条跨 PR 语义约束，防 #126 合入后回归）
12. `ended` 非字符串（脏值，如 `42`）时落 `no_report`，不放行

**反向验证**：把兜底判据临时改为 `if (false && …)` 后重跑，用例 6 与 10 失败、其余守卫用例仍通过 ——
证明新用例确实在测兜底本身，不是恒绿断言。

## 验证

| 检查 | 结果 |
|---|---|
| `cargo test` | 1249 passed, 0 warning（本轮未动后端，沿用上一轮结果） |
| `pnpm --dir ui test` | 1383 passed（1378 + 5 条 tool:result 二道兜底），111 个测试文件全绿 |
| `pnpm --dir ui run lint` | 通过（0 error / 0 warning） |
| `cargo fmt --check` | 通过（未动 Rust） |
| `pnpm --dir ui build` | 通过（`tsc --noEmit` + vite build） |

事件键名未变动，29 键契约（`events.contract.test.ts`）不受影响。

## 遗留

- **失败路径的兜底缺口**：二道兜底依赖 `outcome.data.sub_id`，而后端 `ToolOutcome::err` 不带该字段
  （`E_SUBAGENT_STOPPED` / `E_SUBAGENT`），故**被用户手动停止**或**provider 报错**的子代理仍只由
  `sub:error` + run 收尾兜底覆盖，补强未惠及该两路。补齐需在 err 的 data 里也带 `sub_id`
  （对外多一个字段，属契约变更，另议）。
- **实测定位最终断点**：需在排查分支上重启后复现，借助临时加回的 `[subdiag]` / 后端 emit warn
  确认 `sub:done` 究竟断在发射、投递还是 handler 内部（探针已随本 PR 摘除，方法论见「改动 §4」）。
  兜底修复已保证「即使事件再丢也不永久转圈」，故该项不阻塞交付。
  **已把预期结论收窄到单一分支**（见「本轮追加」一节的推导）：只可能是「后端无 warn + 前端无 enter」，
  其余三种落点均已被推导排除。复现时应优先确认前端**是否真的收到过** `sub:done`，
  可在前端额外全局挂一个 `listen("sub:done")` 空 handler 与之对照——
  两者都没有 ⇒ 事件根本没进 webview；有 enter 却无其它 ⇒ 转查 handler 表是否被换掉。
- **`sub:spawn` 无条件 `push` vs 读侧 `find` 取首个匹配**（潜在不一致，非本次故障）：写入侧不按 `sub_id`
  去重（`runHandlers.ts:423`），读侧一律 `subs.find(...)` 取首个。一旦将来出现重复条目，
  收敛与计数就会落在**不同条目**上（表现为「该收敛的没收敛」）。本次症状已被抽屉 loading 反证为单条目，
  但该结构本身脆弱：建议后续把 `sub:spawn` 改为 upsert（存在则只补字段、不再 push）。
- **`bindEvents` 零错误处理**（`ui/src/ipc/events.ts:10-16`，本轮新发现的独立隐患，**与本次故障是否相关尚未证实**）：
  ```ts
  for (const name of Object.keys(handlers)) {
    unlistens.push(await listen(name, (e) => handlers[name](e.payload)));
  }
  ```
  29 个 `listen` **串行 await**，且**单个 reject 即中断整个循环** —— 该键及其**之后所有键**都不会注册；
  同时调用方 `AppShell.tsx:281` 的 `await bindEvents(...)` 同样没有 try/catch，IIFE 一旦抛出，
  `unlistens` 永不赋值、后续整条初始化链（配置加载 / 会话与项目刷新 / ui-state 恢复）全部静默跳过，
  只剩一个未处理的 Promise rejection。

  这与本故障的症状**部分吻合**：`sub:usage` 已注册并持续到达，而紧随其后的 `sub:done` 未注册 ⇒
  两者行为分叉。但该模式会同时打死 `sub:error` 与 `miscHandlers` 的 3 键（`mcp:status` / `service:update` /
  `app:exit_requested`），影响面偏大、用户通常会发现，故**尚未确证**。

  本轮已核实并排除的两个相邻因素：主 effect 依赖数组为 `[]`（`AppShell.tsx:334`），不存在依赖变更导致的重绑窗口；
  `main.tsx` **未启用 StrictMode**，dev 下也没有双挂载的注册空窗期。

  建议（下一轮随根因一并处理，本轮按用户划定的范围不动代码）：把 `bindEvents` 改为
  `Promise.allSettled` + 逐键 try/catch，失败的键单独 `console.error` 标出，**不让一个键拖垮其余 28 键**。
  诊断价值也高——一旦有键注册失败，devtools 里会直接点名是哪个事件键。
- **静态排除记录**（供下一轮定位时省去重复）：已用 immer 实测复刻 `sub:done` 的 producer（改 status +
  `closeRunningTools` 遍历 draft 的 `toolsMap`），确认**不会抛错回滚**，`sub.status` 必然收敛为 `done`；
  `RunUsage` 全为 u64（无浮点），`json!` 构造不会 panic；`batch.rs` 有 `catch_unwind` 且
  `SubCleanupGuard::drop` 补发 `sub:error`，panic 路安全；子代理工具描述明确「不能派生子代理」，无嵌套场景；
  恢复路径 `restoreFromMessages` 写死 `status: "done"`，不会把卡重置成 running。
- **`web_fetch` 无超时**（`tools/web_fetch.rs:161-229`）：`guarded_get` 与 `read_body_limited` 无任何
  `timeout` 包装，是唯一无界的网络工具（对比 `http_request.rs:105`、`mcp/manager.rs:990`、
  `command/tool.rs:957` 均有）。对端连上不发数据 → tool task 永久 await → `join.join_next()` 永不返回
  → **真挂起**。与本次故障无关（本次 3 个子代理均正常返回），建议另开任务。
- **子代理工具执行期无墙钟看门狗**：`stall_watchdog` 仅覆盖 LLM 流（`drive.rs:1499`），叠加任一无界工具即成永久挂起。