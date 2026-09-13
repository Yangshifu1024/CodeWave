# 缺陷修复：budget_notice 低预算提醒在「消耗 20%」时误触发

> 类型：缺陷修复 · 影响层：`core/agent.rs`（drive_agent 主循环）· 契约影响：零（合成消息内容 / 事件面 / config schema 零变化）· 前端零变化

## 1. 问题现象

代码评审中发现的正确性缺陷：`<budget-notice>` 低预算提醒的实际触发点与注释、规划文档声明的「剩余 20%」方向相反——子代理与计划任务在步数预算**刚消耗 20%** 时就被注入「步数预算即将耗尽，请尽快收敛并输出汇报」，可能诱导其过早收尾、输出浅薄汇报，白白浪费剩余 80% 预算。

## 2. 根因

主循环 `for step in 0..params.max_steps`（agent.rs）中 `step` 是 **0 起始递增**计数器，语义为「已消耗步数」；而触发条件写成了：

```rust
if params.budget_notice && step == params.max_steps / 5 {
```

`max_steps / 5` 是预算的 20% 刻度，落在递增计数器上即「已消耗 20%」。注释 "once at 20% remaining"（[docs/p2-plan](./p2-plan.md) §2 规划「剩余 20% 时注入一次低预算提醒」）与 [docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §420 的 `budget_notice` 描述均按「剩余」语义书写——大概率是作者把 `step` 心算成了倒计时（剩余步数）。

量化对照（0 起始递增）：

| 场景 | max_steps | 旧条件触发点 | 实际语义 | 声称语义 |
|---|---|---|---|---|
| 子代理默认档 | 25 | step 5（第 6 步） | 剩 20 步（80%） | 剩 5 步（20%） |
| 计划任务档 | 30 | step 6（第 7 步） | 剩 24 步（80%） | 剩 6 步（20%） |

配套的 `force_report`（`step == max_steps - 1`，最后一步强制汇报）方向正确，不受影响。

## 3. 修复内容

1. **新增纯函数 `budget_notice_step(max_steps) -> usize`**（agent.rs，`apply_plan_mode` 与 `drive_agent` 之间）：返回 `max_steps - max_steps / 5`，即「剩余 20%」对应的 step 刻度；文档注释明示 `step` 的消耗计数语义与 `max_steps < 5` 时落在循环外不触发的边界行为。
2. **触发条件改为 `step == budget_notice_step(params.max_steps)`**（原 `step == params.max_steps / 5`），注释保持 "once at 20% remaining"（修复后名副其实）。
3. **边界行为**：
   - `max_steps ∈ [5, 9]` 时 notice step 与 force_report 的最后一步重合，两条消息同步注入；`<final-report>` 的「停止调用工具」指令语义优先，无害，不加额外逻辑。
   - `max_steps < 5` 时 `budget_notice_step ≥ max_steps`，条件在 `0..max_steps` 内永不命中，提醒不触发（预算本身太小，提醒无意义；旧代码在此区间反而会 step 0 即触发）。

## 4. 测试

| 测试 | 落点 | 覆盖 |
|---|---|---|
| `budget_notice_step_at_20_percent_remaining` | agent.rs tests | 25→20 / 30→24 / 1000→800 三档刻度 + `max_steps < 5` 落循环外 |

原缺陷无测试钉住（`budget_constants` 仅断言 25/1000/4 常量），本测试把方向语义钉死防回漂。

## 5. 影响面与不变式

- 仍是一条 user 角色合成消息，文案、每 run 一次（精确 step 命中）语义不变。
- `DriveParams.budget_notice` 开关与消费方不变：子代理（tools/subagent.rs `budget_notice: true`）+ 计划任务（task run，`TASK_BUDGET_STEPS = 30`）；主会话依旧默认关闭。
- 前端、事件面 27 键、config schema 零变化。

## 6. 验证

- `cargo test`：364 passed / 0 failed / 0 warning（新增 1 例点名通过；4 ignored 为 cfg(unix) 平台用例与显式探针）。
- 无前端改动，`pnpm --dir ui test` 不涉及。
- 无 GUI 行为变化（触发点前移后移对界面不可见），无需手动验证清单。

## 7. 提交建议

```
fix(agent): budget notice fires at 20% remaining, not 20% consumed（[docs/budget-notice-step-fix](./budget-notice-step-fix.md)）
```
