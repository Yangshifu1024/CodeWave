# 缺陷修复：只读子代理被空转看门狗误杀（read 入参键写反，E_SUBAGENT）

> 类型：缺陷修复（多文件/跨层，**已完成跨层审查**）｜基线：`main @ 8e7219e`
> 分支：`fix/idle-watchdog-readonly`
> 现场：会话 `fad2f093-c612-46a2-9258-f03c5d9ddbb8` 派出的 `explore` 只读子代理 `sub_46748056` 被硬终止，调研成果全部丢弃

## 1. 现象

一名只读调研子代理在**一切调用都成功**的情况下被判「连续 14 步无实质进展」并终止：

| 时刻 | 事件 |
|---|---|
| 19:32:38.506 | 子代理启动（`explore`，任务正文：「只回传结论，不要改任何文件」） |
| 19:32:38 → 19:33:17 | 33 次工具调用（`grep` 26 / `read` 5 / `list_files` 2），共 14 个工具批次，全部成功 |
| 19:32:58 前后 | 第 8 批后注入 `<supervision-notice>` |
| 19:33:17.915 前后 | 第 14 批后注入 `<supervision-escalated>`，run 以 `Err(Protocol)` 收尾 |
| 19:33:17.988 | 父代理收到工具失败：`E_SUBAGENT` |

日志原文（`~/.codewave/logs/codewave.log.2026-09-18:198`）：

```text
session fad2f093-c612-46a2-9258-f03c5d9ddbb8 tool subagent 失败 [E_SUBAGENT]：子代理执行失败：协议错误：监督介入：纠偏后仍连续 14 步无实质进展（持续重复读取，无任何推进动作），本次 run 终止。已完成部分保留在历史中，请基于现状收尾。
```

子代理历史 `~/.codewave/histories/subs/fad2f093-c612-46a2-9258-f03c5d9ddbb8/sub_46748056.json.gz` 与日志逐条对应；约 39 秒的有效调研（读文件、grep 定位）全部作废。

**同源现场（更早一次，侥幸生还）**：`~/.codewave/histories/subs/05b8c088-c985-4d88-b18a-a0e951077db5/sub_b6b6d69b.json.gz`——`tester` 角色在 `plan` 档下做只读分析（任务正文：「禁止修改任何文件、禁止执行构建/测试命令；只用 read/grep/list_files」），19 次调用全为 `read` / `grep`，第 8 批收到**同一条** notice；它只因在第 14 批前恰好写出 `<report>` 才没被杀。

> **档位不是变量，工具构成才是**（取自 `~/.codewave/ui-state.json` 的 `tabs.items[*].prefs.approval_mode`）：`fad2f093` = `full_access`（**不是 plan 档**），`05b8c088` = `plan`。一次 `full_access` 与一次 `plan` 死在同一条 notice 上，正说明**误杀与档位无关**：`plan` 档的写围栏（`edit`/`create`/`delete` 排除 + `command` 只读白名单）在本事故里从未参与——事故 run 的 33 次调用（`grep` 26 / `read` 5 / `list_files` 2）从未调用 `command`，而进展信号 2 的失效让 `read` 也拿不到「首次读新文件」的加分。

> **逃生阀存在，只是没用上**：`command` 与 `service` 的 `ToolKind` 同为 `ReadOnly`（`tools/command/tool.rs:577`、`tools/service.rs:203`），却**不在**监督的只读名单 `READONLY_TOOLS` 内（`supervise.rs:57-59`：`read` / `batch_read` / `grep` / `calculate` / `list_files` / `web_fetch` / `render_html`）——因此哪怕只是一次 `git status` 式 `command`，也会被算作「非只读工具」而清零全部空转计数。事故 run 全程只用 `read`/`grep`/`list_files`，逃生阀形同不存在。

## 2. 根因

### 2.1 🔴 主因：`batch_digest()` 把 `read` / `batch_read` 的入参键写反（`core/agent/drive.rs`）

`read` 的 wire 契约是 `{"files":[{"path":…,"startLine":…,"endLine":…}]}`（`tools/read.rs:31-35` 的 `Args.files`，即 schema 的 `required: ["files"]`）；`batch_read` 是它的废弃别名，**直接复用 read 的 schema 与实现**（`tools/batch_read.rs:19-28`）。

修复前的 `batch_digest()` 却按工具名把两个分支的取值键写反：

```rust
// 修复前（drive.rs）
if c.name == "read" {
    if c.args["path"].as_str().is_some() {          // ← 真实 read 调用恒为 None：契约里没有顶层 path
        d.read_paths.push(normalize_read_path(&c.args));
    }
} else if c.name == "batch_read" {
    if let Some(files) = c.args["files"].as_array() { /* 只有废弃别名走对了分支 */ }
}
```

后果链：`read_paths` 对真实调用**恒空** → `feed_batch` 的**进展信号 2**（首次读新文件，`supervise.rs:195-205`）从不触发 → 只读 run 的每一步都被计为空转 → 第 8 步纠偏、第 14 步终止。事故 run 的 5 次 `read` 一次都没能清零 `idle_streak`。

连带缺陷：`segment_streak`（同文件连续分段读 10 次专用纠偏，`supervise.rs:207-221`）依赖 `digest.read_paths.last()`，对真实 `read` 而言同样是**死代码**。

### 2.2 🟡 次因：只读 run 没有「非只读工具」逃生阀（`core/agent/supervise.rs`）

进展信号 1（`supervise.rs:190-194`）按**工具名**判定 `READONLY_TOOLS`（`supervise.rs:57-59`：`read` / `batch_read` / `grep` / `calculate` / `list_files` / `web_fetch` / `render_html`）。`explore` 这类角色被要求不写文件、常用工具全在名单内，于是**唯一的进展信号只剩「首次读新文件」**——它一坏，整个机制退化为「读多久就杀多久」。

> 注意边界：`command`（`ToolKind::ReadOnly`，`tools/command/tool.rs:577`）**不在** `READONLY_TOOLS` 内，所以它天然算「非只读进展」；`service`（`ToolKind::ReadOnly`，`tools/service.rs:203`）同样不在名单。也就是说逃生阀存在，但取决于模型肯不肯用 `command`；事故 run 全程只用 `read`/`grep`/`list_files`，逃生阀形同不存在。

### 2.3 缺陷为何逃逸

`supervise.rs` 的既有 6 条空转单测**全部绕过 `batch_digest()`**——它们用 helper 直接构造摘要（`supervise.rs:390-395`）：

```rust
fn idle_step(paths: &[&str]) -> BatchDigest {
    BatchDigest { has_non_readonly: false, read_paths: paths.iter().map(|s| s.to_string()).collect() }
}
```

`read_paths` 由测试手工喂入，而生产链路上真正把调用翻译成 `read_paths` 的 `batch_digest()` 没有任何单测。**状态机全绿，翻译层零覆盖**：测试与缺陷正好错开一层。同理，既有 8 个 `BatchDigest` 相关用例（空转 6 + 失败重复路径）也都不经过真实工具调用形状。

## 3. 修复

### 3.1 进展信号修复（`core/agent/drive.rs:1392-1423`）

`read` 与 `batch_read` 合并为**同一条按 `files` 数组解析**的分支（`drive.rs:1399-1420`），从结构上消除再次写反的可能；`normalize_read_path`（`drive.rs:1426-1434`）语义未变，且现在直接作用于 `files` 数组元素——与旧路径 `json!({"path": f["path"]})` 逐字等价。保留顶层单 `path` 兜底（`drive.rs:1415-1419`），仅当无 `files` 键时生效（历史会话可能留下旧形态调用）。

### 3.2 只读 run 的空转策略（`core/agent/supervise.rs`）

| 策略 | 阈值 | 终止 | 适用 |
|---|---|---|---|
| `IdlePolicy::Stop`（默认，`supervise.rs:46-54`） | `IDLE_NUDGE_AT = 8`（`supervise.rs:33`） | `IDLE_ESCALATE_AT = 14`（`supervise.rs:35`） | 主会话 / 任务运行 / 可写子代理（语义**逐字不变**） |
| `IdlePolicy::NudgeOnly` | `READONLY_IDLE_NUDGE_AT = 16`（`supervise.rs:42`） | **空转层不终止** | 只读角色子代理 |

- `feed_batch` 的终止分支加 `stopping` 守卫（`supervise.rs:226-232`），纠偏文案按策略二选一（`supervise.rs:238-245`）：只读版明确「只读角色没有写工具，反复精读同一批文件属正常节奏，本提示不会终止本 run（空转层不终止；步数预算与失败重复、汇报门仍照常生效）」，不再威胁终止。
- 新增 `SupervisionState::with_idle_policy`（`supervise.rs:110-115`）；`DriveParams.idle_policy`（`drive.rs:60-62`，`Default` = `Stop`，`drive.rs:79`）在 `drive.rs:494` 接线。阈值取 16 而非 8 的理由：只读调研里「重读已读文件」极常见，8 步就提示会持续噪声打断。
- 只读 run 的**空转层**预算天花板交给 `max_steps`；退出机制不丢（见 §7）。

### 3.3 只读角色标记与接线（`agents/mod.rs` + `tools/subagent.rs`）

- `AgentDef` 新增 `pub readonly: bool`（`agents/mod.rs:17-21`）：`explore`（`:50`）/ `reviewer`（`:150`）/ `code-reviewer`（`:207`）为 `true`，其余（`backend-dev` / `frontend-dev` / `app-dev` / `product-manager` / `tester` / `title`）为 `false`。选择标准写在代码注释里：这三个角色正文已声明「不修改文件」；`tester` 正文明确要跑测试命令、`product-manager` 正文无写权限表述，均不入选。`agents/mod.rs:395` 的 `readonly_roles_are_exactly_the_three_analysis_roles` 钉住该集合。
- `tools/subagent.rs:146-156` `idle_policy_for(role)`：经 `is_readonly_role`（`subagent.rs:142-144`，走 `crate::agents::find` 单一事实源——别名 / 大小写 / 空格归一与角色注入同源）→ 只读角色 `NudgeOnly`，其余含未命中 / 空串 → `Stop`。接线统一走 `apply_role_policy(params, role)`（`subagent.rs:172-178`），调用点 `subagent.rs:351`——该装配函数独立成单元以便单测钉住（防后续重排参数构造时静默回归）。
- 写工具排除集不再另立一份名单：`readonly_extra_excludes`（`subagent.rs:161-167`）直接取 `crate::core::agent::WRITE_TOOLS`（`drive.rs:277` = `["edit","create","delete"]`，Plan 档同源），并入 `params.exclude_tools`：**把「只读」从角色自律变成可执行事实**——暴露前过滤（`core/agent/stream.rs:199`）+ 调用时硬拒 `E_TOOL_BLOCKED`（`tools/batch.rs:106-117`），用的都是既有排除通路，无新机制。`command` 刻意保留（只读命令是合法进展信号与自救手段，且 fence 逐条把关）。
- `subagent.rs:121-125`：只读角色的 `<subagent-discipline>` 追加一句「你是只读角色，没有写工具（edit/create/delete 不可用）：不要尝试写文件，把发现写进最终汇报」，避免它反复试探被拒白烧步数。

## 4. 测试

**修前红证据**（修复前跑，随后还原）：

| 命令 | 结果 |
|---|---|
| `cargo test batch_digest` | `batch_digest_counts_read_files` 红：`left: []` / `right: ["a.ts","b.ts"]` |
| `cargo test read_only_run_over_new_files_is_not_idle` | 红，错误即事故同款：「监督介入：纠偏后仍连续 14 步无实质进展…终止」 |

**修后绿**（2026-09-19 实测）：`cargo test batch_digest` 5 passed；端到端 1 passed；`core::agent` 70 passed；`supervise` 19 passed；全量 `cargo test` **700 passed / 0 failed / 3 ignored / 0 warning**（主代理在返工后最终态复跑；并行下既有 flaky `provider::tests_integration::midstream_disconnect_maps_to_network` 偶发红，单跑即过、与本批无关）。

新增 17 条用例（14 条钉不变量 + 3 条钉接线）：

| 用例 | 落点 | 守护的不变量 |
|---|---|---|
| `batch_digest_counts_read_files` | `drive.rs:1659` | `read` 必须按 `files` 数组收集路径（修复前恒空） |
| `batch_digest_counts_batch_read_alias` | `drive.rs:1675` | `batch_read` 与 `read` 走同一解析分支 |
| `batch_digest_path_fallback` | `drive.rs:1692` | 顶层单 `path` 兜底与 `normalize_read_path` 语义一致 |
| `batch_digest_grep_only_yields_no_paths` | `drive.rs:1703` | 非 read 只读调用不产生路径、不算副作用进展 |
| `batch_digest_non_readonly_flag` | `drive.rs:1714` | `edit` / `command` 置进展信号 1 且不计 read 路径 |
| `read_only_run_over_new_files_is_not_idle` | `core/agent/tests.rs:1327`（段头 `:1300`） | 端到端：16 步各读一个**真实存在且互不相同**的新文件不被终止（修前第 14 步即被杀），且全程无纠偏 |
| `readonly_policy_never_escalates` | `supervise.rs:521` | 100 步纯空转：只纠偏一次、空转层永不终止 |
| `readonly_policy_nudges_at_sixteen` | `supervise.rs:537` | 阈值 16，且文案不含「被终止」 |
| `default_policy_is_stop_and_still_escalates_at_fourteen` | `supervise.rs:550` | 默认策略仍 8/14、终止文案不变 |
| `readonly_policy_keeps_progress_signals` | `supervise.rs:567` | 只读策略只改「无进展时怎么处置」，进展信号语义不变 |
| `readonly_roles_are_exactly_the_three_analysis_roles` | `agents/mod.rs:395` | 只读角色集合恰为 `{explore, reviewer, code-reviewer}` |
| `idle_policy_for_role_matrix` | `tools/subagent.rs:697` | 只读角色（含别名/大小写）→ `NudgeOnly`；可写/未命中/空串 → `Stop` |
| `readonly_roles_exclude_write_tools_only` | `tools/subagent.rs:735` | 排除集恰为 `crate::core::agent::WRITE_TOOLS`（`edit`/`create`/`delete`），`command`/`read`/`grep` 仍在 |
| `system_extra_marks_readonly_roles` | `tools/subagent.rs:851` | 只读提示句只对三个只读角色注入 |
| `apply_role_policy_marks_readonly_roles` | `tools/subagent.rs:773` | **接线缺口补测**：`apply_role_policy` 对只读角色同时置 `NudgeOnly` + 写工具排除 |
| `apply_role_policy_leaves_writable_roles_untouched` | `tools/subagent.rs:806` | 可写角色的 `idle_policy` 与排除集均不被改动 |
| `apply_role_policy_is_idempotent_over_parent_excludes` | `tools/subagent.rs:831` | 与父档位排除集合并后去重、重复调用幂等 |

端到端用例用脚本化 SSE 驱动真实 `drive_agent`（`spawn_scripted_sse`），`read` 调用的 args 用真实契约形状 `{"files":[{"path":…}]}`——这正是修复前会被翻译层漏掉的那一格。

## 5. 影响面与不变式

- **事件面 28 键不动**：无新增/改名事件，纠偏仍走历史消息（`<supervision-notice>`）+ `session_log`。
- **失败重复检测阈值不变**：`FAIL_NUDGE_AT` / `FAIL_ESCALATE_AT` / `FAIL_LOOSE_*` / `SAME_CALL_NUDGE_AT` 全部未动。
- **监督判定顺序不变**：先 `feed`（重复失败 / 重复调用）后 `feed_batch`（空转）（`drive.rs:812-854`）。
- **主会话与任务运行零变化**：两者不置 `idle_policy`（`main_drive_params` 走 `DriveParams::default`，`drive.rs:285-292`；`run_task_agent` 显式用默认，`drive.rs:1370`）→ `Stop`，8/14 语义逐字不变（有单测钉住）。
- **可写子代理零变化**：`backend-dev` / `frontend-dev` / `app-dev` / `tester` / `product-manager` 的策略与排除集都不变（`tester` 仍可跑测试命令）。
- **新增的硬约束只有一条**：只读角色子代理不可用 `edit`/`create`/`delete`。这三个角色正文本就声明不写文件，属「自律 → 强制」的落实。
- 配置 schema、会话存储格式、前端契约零变化。

## 6. 验证

```bash
cd src-tauri && cargo test batch_digest            # 5 passed
cd src-tauri && cargo test read_only_run_over_new_files_is_not_idle   # 1 passed
cd src-tauri && cargo test                         # 700 passed / 0 failed / 3 ignored / 0 warning
```

（全量并行跑时既有的 flaky `provider::tests_integration::midstream_disconnect_maps_to_network` 可能偶发红；单跑即过，与本批无关。）

界面/行为改动不做 GUI 自动点验，交付分步手动验证清单：

1. **只读调研不再被杀**：在任意档位派一个 `explore` 子代理做多文件调研（>20 步）→ 卡片步数持续增长到任务完成，无「提前结束/预算耗尽」警示，日志无 `监督介入：纠偏后仍连续 … 步无实质进展`。
2. **空转纠偏仍可见**：让只读子代理反复读同一个文件 → 第 16 步左右出现一条提示，文案含「本提示不会终止本 run」，run 继续跑（不终止）。
3. **可写子代理行为不变**：派 `backend-dev` 做小改动 → 能正常 `edit`；故意让它连续空读 → 第 14 步仍被终止（默认策略未放宽）。
4. **只读角色写工具被拒**：派 `explore` 并指示它「把结论写入 report.md」→ 它拿不到 `edit`/`create`/`delete`（工具列表里没有；硬调则 `E_TOOL_BLOCKED`），纪律块提示「你是只读角色」，最终以 `<report>` 收尾。
5. **主会话与任务不变**：主会话正常读写；让主会话连续空读 → 仍按 8/14 纠偏与终止。

## 7. 已知取舍与遗留

- **本批不做 B（grep 命中新文件算进展）**：需把摘要构造时机从「工具执行前」移到「执行后」，且微调 pattern 即可绕过，收益/成本不划算。
- **本批不做 C（终止时回传部分汇报）**：会动 `E_SUBAGENT` 语义与前端 `sub:` 通道文案，需独立立项。
- **本批不做 D（终止前插一轮强制文本汇报）**：会动 `ended` 三态语义（[subagent-text-turn-premature-exit](./subagent-text-turn-premature-exit.md)）。
- **🟡 只读角色远非密闭：`service` / `http_request` 仍在手边**。只读写角色的排除集只排除落盘的三个写工具（`edit`/`create`/`delete`）；`service` 的 `ToolKind` 是 `ReadOnly`（`tools/service.rs:203`）、`http_request` 同属网络读取类，**二者都不在排除集内**——也就是说**只读子代理仍可起后台服务进程、向远端发请求**（潜在副作用：端口占用、对外网络访问、外来内容进入上下文）。本批**明示选择**「只收写文件能力 + 保留命令」：把 `service` 一并排除会连只读调研常用的探测手段一起割掉（`http_request` 本就在 `READONLY_TOOLS` 名单内，排除它反而制造新的名实错位）。结论：**「只读」≠「密闭」**——它是「不能改工作区文件 + 不能改全局配置」，不是「无任何副作用」。读者不要高估隔离强度。
- **🟡 空转判定只看工具名、不看结果**：被 plan 围栏拒绝的 `command`（`E_PLAN_READONLY`）反而算「有进展」并清零计数（`command` 不在 `READONLY_TOOLS`）；`service`（`ToolKind::ReadOnly`）也不在名单，同属地名与语义错位。彻底修需在摘要里携带调用结果。
- **🟡 只读 run 的兜底天花板不止 `max_steps`**：`MAX_TEXT_TURNS = 3`（`drive.rs:329`）仍生效——只读子代理若连续 3 个回合只输出不带 `<report>` 的文本，会被 `StopWithLimit` 显式失败；`force_report`（`subagent.rs:336`）也保证步数耗尽前有强制汇报轮。即「永不终止」**只针对空转看门狗层**，不针对失败重复层（精确 3/5、宽松 6/10）、预算与收尾纪律。
- **🟡 `segment_streak` 恢复生效——对主会话也是新行为**：修复前它因 `read_paths` 恒空而对真实 `read` 属死代码（阈值 10、只纠偏不终止不变），修后**所有 run（含主会话 / 任务运行 / 可写子代理）**都可能收到「同一文件已连续分段读取 10 次」的专用纠偏。同 run 只提示一次、不参与终止判定，但这是本批的**真实行为变化**，列为观察项：长文件分段阅读的真实触发频率与噪声程度待观察。
- **🟢 观察项**：只读角色被排除写工具后，模型是否出现新的「反复试探不可用工具」行为（当前以纪律提示 + 终结性错误文案兜底）；`NudgeOnly` 下 16 步提示在真实长调研中的噪声程度；以及 `segment_streak` 对主会话的 10 步专用纠偏是否频繁打断正常长文件阅读。

## 8. 审查结论（跨层 code-reviewer，已完成）

- **结论：对齐**——逐条落实方案承诺；**无 🔴**。
- 3 条 🟡 均已在本次返工中处理：① 注释作用域表述（`NudgeOnly` 的「不终止」限定到空转层，写明失败重复层与步数/汇报门不受影响）；② 接线测试缺口（补 `apply_role_policy_*` 三条，钉住「装配函数真的被调用」）；③ 写工具名单未共用（`READONLY_EXCLUDED_TOOLS` 自持名单→ 改取 `crate::core::agent::WRITE_TOOLS` 单一事实源，与 Plan 档同源）。

## 9. 提交建议

```text
fix(agent): 修正空转看门狗读路径键位缺陷（batch_digest 按 files 解析，含端到端回归）
```

```text
feat(agent): 只读子代理角色策略与空转监督补齐（explore/reviewer/code-reviewer + 文档）
```
