# 子代理幽灵会话缺陷修复报告

> 日期：2026-09-07 · 类型：缺陷修复（P0 缺陷流程：P0 分类 → tester 复核 → 方案批准 → P4 执行 → 测试验证）

## 1. 现象

项目会话列表出现多个 `(untitled)` 幽灵会话，时间间隔与子代理（subagent 工具）运行时间吻合。子代理是内部运行，本不应出现在会话列表。

## 2. 根因（双重复核确认：主流程走查 + tester 独立复核）

- `tools/subagent.rs` 用 `SessionRuntime::new_sub(&ctx.rt, sub_id)` 创建子代理 runtime（id 形如 `sub_xxxxxxxx`，title 为空串），随后经 `drive_agent` 驱动。
- `core/agent/drive.rs` 的 `checkpoint()` 对传入 runtime 无差别调用 `store.save_history(...)`，其内部无条件 `upsert_meta` 写进 `sessions/index.json`——而前端会话列表直接读该索引。
- 触发点：每 `CHECKPOINT_EVERY_STEPS = 20` 步（`drive.rs` 主循环）+ run 收尾；子代理默认预算 25 步，几乎必命中。
- `project_id/roots` 从父会话继承（`runtime.rs new_sub`）→ 条目挂在项目下；title 空 → 前端渲染 `(untitled)`（`ProjectNav.tsx`）。
- **设计意图冲突**：`store.rs` 子代理历史一节注释明示「不 upsert 会话索引（子代理不是会话，不得出现在 list_sessions）」——正规路径是 `save_sub_history` 边车（`subagent.rs` 正常/panic 两路收尾均正确调用）。`checkpoint()` 缺身份判断即缺陷本体。

### 第二污染源（tester 复核发现）

scheduler 计划任务运行（`core/scheduler.rs` 用 `SessionRuntime::new_task(format!("task_{task.id}"), ...)` 构造隔离 runtime）同样经 `drive_agent` 命中 checkpoint，`task_*` 条目同样泄漏进主索引——且任务运行按设计完全不应有历史落盘（无 `save_sub_history` 对应物）。

### 附带残留

- `histories/sub_*.json.gz`、`histories/task_*.json.gz` 孤儿文件残留磁盘（`discover_orphans` 生产无消费者，不会复活进列表，仅占空间）。
- 子代理内触发 plan 工具会生成 `sessions/sub_*.todos.json` 边车残留。

## 3. 修复

| # | 改动 | 位置 |
|---|---|---|
| 1 | `SessionRuntime` 新增 `is_main_session: bool` 内存态字段（命名风格对齐既有 `is_task_runtime`）：`new()` 默认 `false`；`get_or_create_session()`（真会话唯一入口）置 `true`；`new_sub`/`new_task` 保持 `false` | `core/agent/runtime.rs` |
| 2 | `checkpoint()` 在 zombie 早退之后对 `!is_main_session` 早退——一行判断同时封堵 `sub_*` 与 `task_*` 两处污染源 | `core/agent/drive.rs` |
| 3 | 新增 `SessionStore::purge_non_session_entries()`：持 `index_lock` 清理索引中 `sub_`/`task_` 前缀条目，级联删除对应 gz、todos、artifacts 边车；幂等；在 `lib.rs` setup 中 store 构造后调用（存量一次性修复）。真会话 id 为 uuid，与两类前缀无冲突可能 | `core/sessions/store.rs`、`lib.rs` |
| 4 | 测试夹具 `make_runtime`（主会话语义）同步置 `is_main_session = true`，避免涉及 checkpoint 的测试语义漂移 | `core/agent/test_support.rs` |

运行时判定用显式字段而非 `id.starts_with("sub_")`——前缀匹配仅用于启动时存量清理。

## 4. 回归测试

- `core/agent/tests.rs`：
  - `checkpoint_skips_subagent_runtime` — 子代理 runtime checkpoint 后索引为空、gz 不存在；
  - `checkpoint_skips_task_runtime` — 任务运行同上；
  - `checkpoint_persists_main_session` — 反向防回归：主会话 checkpoint 照常持久化（索引条目 + gz 都在）。
- `core/sessions/store/tests.rs`：
  - `purge_non_session_entries_removes_ghosts_keeps_real` — 幽灵条目连 gz/边车删除、真会话（uuid 形态 id）保留、幂等。

## 5. 验证

- `cargo test`（src-tauri/）：**416 passed / 0 failed / 1 ignored，0 warning**（Windows 基线 243 → 现基线以本地最新全绿为准；4 个新用例全过）。注意：PowerShell 下 `cargo test 2>&1` 的 stderr 包装会造成 exit code 假阳性，以 `cmd /c` 原生重定向的 `CARGO_EXIT=0` 为准。

## 6. 手动验证清单

1. 启动应用（存量条目在启动时被清理）：检查原项目下 untitled 会话是否消失。
2. 任意项目会话中委派一个子代理（如 explore），完成后重开左栏：项目下**不**新增 untitled 会话；子代理过程抽屉仍可正常查看（`save_sub_history` 边车路径不受影响）。
3. 有计划任务到期的项目：任务触发后同样不产生新会话条目。
4. 会话收尾/重命名/删除等常规操作行为不变（`checkpoint_persists_main_session` 守护）。
