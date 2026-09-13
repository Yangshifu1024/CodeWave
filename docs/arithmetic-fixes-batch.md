# 计算点修复批次：[docs/arithmetic-audit](./arithmetic-audit.md) 审查清单 14 项全修

> 类型：缺陷修复批次（多文件/跨层，已过 code-reviewer 强制审查）· 基于：[docs/arithmetic-audit](./arithmetic-audit.md) 全仓算术审查 · 后端 8 文件 + 测试、前端 4 文件 · 契约影响：零（事件 27 键 / config schema / IPC 零变化）· 编号说明：原拟 58，与 shell 威胁分析调研撞号，避让改 59

## 1. 修复清单（对应 [docs/arithmetic-audit](./arithmetic-audit.md) 编号）

### 🔴 高优先

| # | 位置 | 修复 |
|---|---|---|
| 1 | `tools/list_files.rs` | 模糊评分推进改 `pi += off + qc.len_utf8()`（原 `off + 1` 字节推进，多字节命中后 `lp[pi..]` 切非字符边界 panic，中文 @-查询可触发）。新增测试 `fuzzy_multibyte_and_case_insensitive` |
| 2 | `core/stats.rs` | `flush` 的 by_workspace 合并补齐 `cache_read`/`cache_write`（与 by_model/by_kind/total 对齐）；`#[ignore]` 探针转正；`flush_merges_into_existing_same_day_file` 夹具将 pending 的 by_workspace cache 计数归零（day() 预置了 7/3，旧缺陷下断言恰好空过，归零后才真正钉住「旧值存活」） |

### 🟡 语义/边界

| # | 位置 | 修复 |
|---|---|---|
| 3 | `provider/retry.rs` | `attempt > MAX_RETRIES` → `>=`（0 基已完成重试计数下 7 次 → 文档声明的 6 次）；`retry_matrix` 断言翻转（MAX_RETRIES 拒绝、MAX_RETRIES-1 放行） |
| 4 | `core/scheduler.rs` | `every:<n>` 解析改 `checked_mul` + 30 天产品上限（溢出从 tick 期静默隔离提前到解析期报错）；tick 中 `parse_schedule` **Err 路径隔离处理**（warn + `next_run=None` 暂停调度），不再与 `once` 共用 None 删除分支——循环任务不再被静默删除。新增 `every_interval_parse_bounds`；`tools/scheduled_task.rs` 语法边界测试同步上界 |
| 5 | `core/context.rs` | `compact_history` 将压缩模型 `max_tokens` 收敛到 `SUMMARY_MAX_TOKENS`（8000）——声明即未接线的死常量生效（注：摘要触顶时 openai 协议会按 [docs/max-tokens-truncation-fix](./max-tokens-truncation-fix.md) 注入截断尾注，落在摘要文本内，可接受） |
| 6 | `core/sessions/repair.rs` | `trim` 的 `starts[1]` → `starts.get(1).copied().unwrap_or(msgs.len())`（`keep_last=0` 且单轮超预算不再越界）；新增 `trim_keep_last_zero_single_round_no_panic` |
| 7 | `tools/command.rs` | `probe_bash_login`（unix）阻塞 `output()` → spawn + 50ms 轮询 + 5s 截止 kill，按注释落实 5s 预算；`try_wait` 返回 `Result<Option<_>>` 需 `Ok(Some)/Ok(None)/Err` 四臂（🔴 审查发现初版误按 Option 匹配，已修） |
| 8 | `tools/service.rs` | `child.id().unwrap_or(1)` → let-else 拒绝（兜底 1 经 `kill(-pid)` 即 `kill(-1)` 波及全用户进程；现无可获取 PID 直接报错） |

### 🟢 轻微

| # | 位置 | 修复 |
|---|---|---|
| 9 | `tools/edit.rs` | `min_indent` 改按字符计数（与 `strip_indent` 的 `chars().skip(n)` 一致，多字节空白缩进不再过度剥离）；新增 U+00A0 一致性测试 |
| 10 | `tools/subagent.rs` | 并发门 load/check/fetch_add → `compare_exchange` CAS 循环（并发 spawn 不再瞬时超 4） |
| 11 | `ui/.../TokenStatsModal.tsx` | topModel 与 byKind 聚合补上 `cache_read`，与柱状图/总计统一为 `input+output+cache_read` 口径 |
| 12 | `ui/.../SubagentDrawer.tsx` | 宽度 useRef+effect → `useMemo(() => drawerWidth(), [open])`（open 翻转当帧即得新宽度；折叠期改窗口尺寸不再以旧宽度打开）。已知取舍：打开期间 resize 不实时刷新，需下次开合（与旧行为一致） |
| 13 | `ui/theme/app.css` + `AppShell.tsx` | 新增 `:root { --ws-titlebar-h: 50px }`，`.toolbar` 高度、AppShell 内容区 `calc(100% - var(...))`、`.notify-stack` 的 `top: calc(var(--ws-titlebar-h) + 10px)` 全部单源消费 |
| 14 | `ui/.../Composer.tsx` | 历史召回注释更正为实际索引方向（升序：0 = 最旧，length-1 = 最新；↑ 向旧 ↓ 向新） |

## 2. code-reviewer 审查结论（强制，7 维度）

- **🔴 1 项（已修）**：`probe_bash_login` 初版将 `try_wait()` 的 `Result<Option<ExitStatus>>` 按 `Option` 匹配——Windows `cargo test` 不编译 cfg(unix) 路径故本地全绿，但 macOS/Linux CI 必红。已改四臂 `Ok(Some)/Ok(None) guard/Ok(None)/Err`，并经独立 rustc 等价片段编译验证（COMPILE OK）。
- **🟡 2 项（已处理）**：① list_files 查询小写化——复核确认上游 fuzzy_filter 入口（line 202）早已 `query.to_lowercase()`，初版审查的「大小写缺陷」论断有误，冗余修改已回退（[docs/arithmetic-audit](./arithmetic-audit.md) §1 已同步勘误）；新增测试保留，作用是钉住上游小写化行为。② scheduler 上限注释改写为诚实的产品级依据（31 天并不会使 chrono 溢出）。
- **🟢 3 项**：service.rs 拒绝文案改为「无法获取子进程 PID」（已改）；repair.rs 循环尾 `return trim(...)` 实为 if 语义（预先存在形状，未动）；SubagentDrawer 宽度打开期不实时刷新（与旧行为一致，非回归）。
- **结论**：修复后可合并。

## 3. 行为变更提示

- **重试**：连续失败场景每 turn 少一次重试（7→6），最长省 ~10s 退避。
- **every 任务**：新建时 >30 天间隔直接报错；**已持久化**的超长 every 任务下次触发后将被隔离（warn 日志 + 暂停调度，任务仍可在任务中心看到）而非继续循环——这是 #4 的预期行为，不是回归。
- **统计口径**：统计弹窗「最常用模型」与分源数值含 cache_read 后会变大，与总计对得上账了。

## 4. 验证

- `cargo test`：**369 passed / 0 failed / 0 warning**（3 ignored 为 cfg(unix) 平台用例与显式探针；本批新增 4 测试 + 1 探针转正 + 2 既有测试边界更新）
- `pnpm --dir ui test`：**247/247 passed**（34 文件）
- `pnpm --dir ui build`：通过（type check + vite build）
- cfg(unix) 探针：独立 rustc 等价片段编译通过（本机结构性无法编译该路径，macOS/Linux CI 首轮兜底）
- 无 GUI 自动点验；手动验证清单见 §5

## 5. 手动验证清单

1. 统计弹窗：确认「最常用模型」数值与总计口径一致（含缓存 token）。
2. 会话中 @ 提及输入中文查询（如「中文」）：不再崩溃，能正常过滤。
3. 子代理抽屉：折叠侧栏状态下改变窗口宽度，再触发子代理打开抽屉——宽度应为当前窗口的 45%（360–560 clamp），非旧值。
4. 标题栏与通知位置观感应无任何变化（50px/60px 值未变，仅改为变量单源）。
5. （可选，任务用户）新建 every 任务输入 `every:31 d` 应报错「every 间隔过大（上限 30 天）」。

## 6. 提交建议

```
fix(core,tools,ui): repair 14 arithmetic defects from [docs/arithmetic-audit](./arithmetic-audit.md) audit

- list_files: advance fuzzy scan by char bytes (CJK @-mention panic)
- stats: by_workspace flush merge carries cache_read/cache_write (probe un-ignored)
- retry: enforce documented 6-retry cap (was 7)
- scheduler: every-interval overflow guarded at parse (30d cap); corrupt schedules quarantine instead of silent delete
- context: wire SUMMARY_MAX_TOKENS into compaction request (was dead constant)
- repair: trim keep_last=0 single-round no longer panics
- command: unix bash login probe bounded to 5s (was unblocking .output())
- service: refuse instead of kill(-1) when child pid unavailable
- edit: min_indent counts chars to match strip_indent (multibyte indent)
- subagent: concurrency gate via CAS (no transient overshoot)
- ui: TokenStatsModal unified token base; SubagentDrawer width via useMemo;
  --ws-titlebar-h single source; history-recall comment corrected
```

> 注：工作区同时含 [docs/budget-notice-step-fix](./budget-notice-step-fix.md)（budget_notice 方向反转）改动，用户可拆分或合并提交，由用户执行 git 操作。
