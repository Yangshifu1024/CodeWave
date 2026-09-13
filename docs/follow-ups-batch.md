# 后续建议收敛批次（drive_agent 提函数 / mod.rs 存量 / clippy 清零 / CI 行数闸门）

> 缘起：[docs/long-file-split-and-edition-2024](./long-file-split-and-edition-2024.md) §7 登记的四项后续建议一次收敛。全程延续「纯搬移/机械修复优先、行为零变化」纪律，
> 每个 workstream 独立可回退。终态：`cargo test` **393 passed / 0 warning**（edition 2024）、
> `cargo clippy --all-targets` **0 findings**、CI 新增行数闸门本地模拟通过。前端本批次零改动。

## 1 · drive_agent 按语义段提函数（[docs/long-file-split-and-edition-2024](./long-file-split-and-edition-2024.md) §4 遗留）

`core/agent/drive.rs` 的 drive_agent 主循环（原 ~420 行）拆出三个语义段，helper 函数体逐字搬移、
仅做机械转换（`break 'steps` → `return Err`、引用参数解引用）：

| 提取函数 | 原段落 | 说明 |
|---|---|---|
| `run_llm_turn` | ⑦ 流式请求 + per-turn 重试/backoff（~145 行） | 可变状态经 `&mut` 穿引用：`run_usage` 跨内部重试持续累计、`attempt` 成功即清零（M3）、`sanitized_once` 一次性历史 sanitize/repair（M2）；`Ok(Assembled)` / `Err(结束性错误)` 与原 `break 'steps` 流严格对应 |
| `step_auto_compact` | ③④ 上下文 breakdown 发射 + 阈值自动压缩（含失败冷却/压缩互斥/进度事件） | `compact_fail_streak` 经 `&mut` 穿引用；`emit_events` 守卫随迁为函数首行 |
| `run_tool_batch` | ⑨ 工具批次执行 + 批后流程（~48 行） | 返回 `(suggest items, 是否结束本轮 run)`；`last_step` 优先于 suggest，与原 break 顺序严格一致；main_session 参数逐 step 重算逻辑随迁 |

- drive_agent 本体降至 ~250 行，八段编号注释（①–⑨）保留，主循环骨架一屏可读。
- 顺带修正：`step_auto_compact` 的 `run_id` 参数实际未被压缩事件 payload 使用，未迁入。
- 验证：提取后 `cargo test` 393 passed / 0 warning 一次通过。

## 2 · 存量 mod.rs 收敛（mod.rs 只做导出/测试声明）

| 文件 | 收敛前 | 收敛后 |
|---|---:|---|
| tools/mod.rs | 272 行（声明 + ToolKind/ToolError/ToolOutcome/ToolCtx/trait Tool/collect_unknown_fields + 内联测试） | mod.rs 32 行（声明 + `mod tool; pub use tool::*;`）+ tool.rs 165 行 + tool/tests.rs 75 行 |
| core/sessions/mod.rs | 681 行（声明 + 3 常量 + SessionMeta/SessionIndex/SessionArtifact/ArtifactOp + SessionStore + gzip_history + 内联测试） | mod.rs 12 行（`pub mod repair; mod store;` + 使用中符号的 re-export）+ store.rs ~380 行 + store/tests.rs 288 行 |

- 外部路径 `crate::tools::Tool`、`crate::core::sessions::SessionStore` 等经 re-export 逐字不变，全仓消费点零改动。
- sessions 的 re-export 只保留经 `crate::core::sessions::` 路径实际消费的符号（ArtifactOp/SessionMeta/SessionStore）——
  `lib.rs` 中 `mod core` 为私有模块，无人使用的 pub re-export 会触发 rustc `unused_imports`（edition 2024 下该规则覆盖 pub use）。

## 3 · clippy 清零（存量 1 error + 51 warning → 0）

- `cargo clippy --fix --all-targets` 自动修复 37 处（needless_borrow/cloned_ref_to_slice_refs/clone_on_copy/
  derivable_impls/unnecessary_to_owned/unused pub re-export 裁剪等）。
- 手工修复 15 处：
  - `sessions/repair.rs` never_loop（**唯一 error**）：`while` 循环体每条路径都经 `return trim(..)` 递归重入、从不迭代 →
    改为单轮 `if`（clippy 已证实两形态等价），递归语义与 [docs/arithmetic-audit](./arithmetic-audit.md) 审计后的行为零变化；
  - `field_reassign_with_default` ×4（drive.rs / agent+prompt+dto 测试）→ 结构体更新语法 `..Default::default()`；
  - `type_complexity` ×3 → 具名别名（scheduler `TaskScope`、mcp `McpReady`、skills `SkillCache`，附语义注释）；
  - `too_many_arguments`（prompt::assemble，8 参为固定层序）→ 定点 `#[allow]` + 理由注释；
  - `sort_by` → `sort_by_key` + `Reverse` ×2；`while_let_loop`（sse.rs）、`collapsible_if`（drive.rs）、
    `match` 单模式 → `if let`（config.rs）、doc 列表续行缩进（logging.rs `+` → `plus`）。
- 残余已知取舍：无。CI lint.yml 对 clippy 仍是 continue-on-error，但本仓库 clippy 现为零噪声，任何新告警即显形。

## 4 · CI 行数上限闸门（防回潮）

`.github/workflows/ci.yml` 新增 `line-limit` job（ubuntu，纯脚本无构建开销）：
- 扫描 `git ls-files` 的 `.rs/.ts/.tsx`，排除 target/node_modules/dist/gen 与测试文件（`tests.rs`/`tests_integration.rs`/
  `fuzz.rs`/`e2e_glm.rs`/`*.test.ts(x)`）；
- **>800 行**：GitHub `::warning` 标注（软上限，引导拆分）；**>1500 行**：`::error` 并令 job 失败（硬上限防失控）。
- 当前存量产生 3 条预期 warning：fence/check.rs 1133、agent/drive.rs 901、tools/batch.rs 820（均为已知大文件，
  job 保持绿色）；本地模拟 rc=0 验证通过。

## 5 · 验证记录

| 检查 | 结果 |
|---|---|
| `cargo test` | 393 passed / 0 failed / 3 ignored（每个 workstream 落地后各跑一轮，全绿） |
| `cargo check --tests`（rustc） | 0 warning |
| `cargo clippy --all-targets` | **0 findings**（clean 后全量重编验证，非缓存计数） |
| line-limit 本地模拟 | rc=0，3 条预期 warning |
| 前端（`pnpm test` / `build`） | 本批次零改动（上一绿态沿用：254 passed） |

## 6 · 后续建议状态

[docs/long-file-split-and-edition-2024](./long-file-split-and-edition-2024.md) §7 四项建议全部关闭。新的可选项（不急）：
- fence/check.rs（1133）与 drive.rs（901）的进一步拆分——line-limit job 会持续以 warning 提示，直至收敛；
- lint.yml 的 clippy/rustfmt 步骤可考虑摘掉 `continue-on-error`（现在 clippy 已零噪声，可升格为硬门槛）。

## 7 · 提交信息草案（git 由用户执行）

```
refactor(agent): extract run_llm_turn/step_auto_compact/run_tool_batch from drive_agent（[docs/follow-ups-batch](./follow-ups-batch.md)）
refactor(tools,sessions): reduce mod.rs to pure declaration hubs, logic into store/tool modules（[docs/follow-ups-batch](./follow-ups-batch.md)）
fix(lint): resolve all clippy findings incl. never_loop in sessions repair（[docs/follow-ups-batch](./follow-ups-batch.md)）
ci: add line-limit job (warn >800, fail >1500 non-test source lines)（[docs/follow-ups-batch](./follow-ups-batch.md)）
docs: add [docs/follow-ups-batch](./follow-ups-batch.md) follow-ups batch report
```
