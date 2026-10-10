# suggest-mixed-batch · suggest 可与只读工具同批

> 缺陷修复（用户报告 + S9 验证）：suggest 与非 Interactive 工具同批时，原策略把 ask / wait / suggest 一锅烩成「整批 reject」，suggest 自身被一并拒、chip 不会发出、run 收不到 `batch_done` 继续走空批；用户也记不住这条隐式约束。**ask / wait 仍整批 reject**（真独占），**suggest 与 read / grep / list_files 等只读工具同批**：非 suggest 调用 `E_BATCH_POLICY` 拒、suggest 走原路径成功并发 emit `run:suggestions`。

## 设计意图

ask / wait 是 `ToolKind::Interactive`，但其真实独占语义是**两件不同的事**：
- **ask** 阻塞等用户应答，与并发工具语义冲突 → 必须独占；
- **wait** 阻塞 sleep，模型同批并发其他工具会让本批「还在 sleep」就早收尾 → 必须独占；
- **suggest** 仅做「成功 + emit `run:suggestions` + 装入 `BatchOutcome.suggest_items`」，与并发工具**无任何副作用冲突**（不阻塞、不抢文件锁、不改 runtime 状态）。

把三者一锅烩是「按分级标签的简化」，但**与模型侧的合理表达失配**——模型写收尾时大概率会顺手读一个文件做"再确认"，那是它自然表达，不是错。

## 实现要点

### 1. 批次策略（1）按工具名分流（[src-tauri/src/tools/batch.rs](../src-tauri/src/tools/batch.rs)）

```text
has_ask_or_wait && calls.len() > 1
  → 走 ask/wait 真独占：整批 reject 并 return（行为零退化）

has_suggest && calls.len() > 1 && !has_ask_or_wait
  → 走 suggest 单独放行：预置非 suggest 调用的 outcomes 为 E_BATCH_POLICY；
    suggest 调用继续走 (3) 的 spawn 路径，由 (3) 顶部 outcomes[i].is_some() 跳过
    已 reject 的 call，避免 JoinSet 二次挂载

否则（含 calls.len() == 1）→ 维持原行为
```

关键不变量：
- **outcomes 预分配放在 (1)**，(3) 复用同一变量，不再重复 `vec![None; …]`，否则 (1) 的 reject 会被重置；
- **(3) 的 spawn 循环顶部**新增 `if outcomes[i].is_some() { continue; }`——既不重复挂载，也避免 (4) 装 results 时被覆盖；
- **results 顺序**仍由 (4) 按 `calls` 顺序装入，与 (1) 的 reject 项位置一致。

### 2. §8.1 计划门兼容（[docs/main-run-finish-with-pending-todos.md](./main-run-finish-with-pending-todos.md)）

`E_PLAN_PENDING` 门仍在 `tools/suggest.rs::plan_pending_blocks_suggest` 内部、emit 之前判定——suggest 在 (1) 放行后进入 (3) 走原 `SuggestTool::run`，**门**逻辑零改动。**drive.rs:1895-1904** 的 `batch_done = true` 仍由 `BatchOutcome.suggest_items` 单值驱动，**勿新增旁路**的批注依然有效。

### 3. 事件面零改动

- `run:suggestions` 仍由 `suggest.rs` 的 `ctx.core.sink.emit(...)` 发出，路径不变；
- `BatchOutcome.suggest_items: Option<Vec<String>>` 类型不变；
- 前端 `ui/src/stores/runHandlers.ts:290` 的 `"run:suggestions"` 处理器零改动；
- `events.contract.test.ts` 的 29 键白名单零变化。

## 关键测试

新增 `tools::batch::tests` 下 6 条用例（cargo test 全量 1201 passed / 3 ignored）：

| 用例 | 输入 | 期望 |
|---|---|---|
| `suggest_mixed_with_read_rejects_read_passes_suggest` | read + suggest | read `E_BATCH_POLICY`、suggest 装入 `suggest_items`、commit 建议置顶 |
| `suggest_mixed_with_grep_rejects_grep_passes_suggest` | grep + suggest | grep `E_BATCH_POLICY`、suggest ok |
| `suggest_mixed_with_plan_pending_blocks_suggest_via_e_plan_pending` | read + suggest（plan 未收尾） | suggest `E_PLAN_PENDING`、suggest_items=None、read 仍 `E_BATCH_POLICY` |
| `ask_with_read_still_rejects_entire_batch` | ask + read | 两条都 `E_BATCH_POLICY`、results 完整 |
| `wait_with_suggest_still_rejects_entire_batch` | wait + suggest | 两条都拒、suggest_items=None（守 ask/wait 独占零退化）|
| `two_suggests_in_one_batch_executes_both_no_panic` | suggest + suggest | 两条都跑通、results 完整、suggest_items 装入——模型笔误不 panic |

## 模型侧提示词收敛

- `tools/suggest.rs` 的 description 由「必须独占该批次的唯一调用」改为「可与 read/grep/list_files 等只读工具同批（[docs/suggest-mixed-batch]，与 ask/wait 不同——这两个仍独占）」。
- 提示词收紧点是**告诉模型与谁同批合法**（只读工具），不松绑 ask/wait。
- 模型若写出 `ask + suggest` / `wait + suggest` / 两条 suggest，行为契约见上表「关键测试」。

## 验证

- `cargo test --workspace` 全量：1201 passed / 0 failed / 3 ignored（基线 1177 + 新增 24 — 含 §8.1 既有 5 条 suggest 测试仍全绿，本批新增 6 条）
- `cargo clippy --lib --tests`：0 warning
- 端到端 `drive_agent` 路径未动；`BatchOutcome.suggest_items` 在 C1 中被 `drive.rs:1895-1904` 读取后置 `batch_done = true` → 经 `run_chat` 并入 `run:done.suggestions` 字段，由前端 `runHandlers.ts:290` 接收——此接缝由 C1 单测守住（execute_batch 是真正的执行路径，与 drive 之间的唯一接缝是 `BatchOutcome`）。
