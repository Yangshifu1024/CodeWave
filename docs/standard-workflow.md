# 标准工作流：arch 技能内置化

> 批次报告：移除 `$arch` 技能，研发流水线转为常驻内置标准流程（分档路由 + 尽量并行）。
> 关联：[arch-orchestrator](./arch-orchestrator.md)（arch 技能历史契约，四文档产物契约仍有效）。

## 1. 需求与决策

用户需求：移除 arch，将技能流程转为内置标准流程。经澄清确认的决策：

| 决策点 | 结论 |
|---|---|
| 适用范围 | 分档路由：轻量（≤2 文件/无删除/无新依赖/无跨层）直接实现；任一维度超阈自动走完整流水线 S1-S9；用户显式说法可升/降档；拿不准默认轻量。计划批准即定分支：S4 拟定分支名（git 仓库内 `<type>/<slug>`），S5 批准询问列明并构成建分支预授权，S6 动工前先 `git switch -c`（[docs/plan-branch-proposal](./plan-branch-proposal.md)） |
| 产物 | 完整流水线保留 `.codewave/tasks/` 四文档；轻量路径不落盘；临时会话可跑流水线但跳过落盘（S9 说明） |
| 批准门 | S5 保留 ask 批准门：**三个选项**（两个批准类选项各带 `mode` 字段声明目标档位——「执行方案」= `auto_edit`、「完全访问执行」= `full_access`，再加无 mode 的「补充意见」；批准类判定 = `mode.is_some()`，锚点 `switchToAutoEdit` 与 `id="approve"` 保留为兼容兜底；驳回修订 ≤2 轮）；批准 = 用户在两档中选一（自动编辑 / 完全访问）+ 预授权创建并切换计划所示分支（[docs/mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)） |
| 注入位置 | 常驻系统提示第 1 层（`core/prompt.rs` 编译期常量 `WORKFLOW_SECTION`，字节稳定缓存友好） |
| 并行（用户两轮补充） | S1 explore ∥ S2 product-manager 同批；S6 任务包并发额度（≤4）内拉满、三角色混派、真实依赖才顺序、超额补位不等整批；S7 reviewer ∥ S8 tester 同批 |

## 2. 改动清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/skills/mod.rs` | 删除 arch 技能条目（repo-index/doc-convert 保留）与 `arch_skill_contains_mechanism_anchors` 测试；模块注释指向标准工作流。`$arch` 与 `$` 菜单条目随之消失（前端动态扫描，零前端改动） |
| `src-tauri/src/core/prompt.rs` | 新增 `WORKFLOW_SECTION` 常量（首版 1510 字；现含分支条款，预算 2000 有护栏测试）拼入第 1 层；`workflow_section_contains_mechanism_anchors`（锚点随迁 + `agents::find` 注册表联动 + 字数护栏）；`stable_prefix_and_layers` 补第 1 层常驻断言 |
| `src-tauri/src/tools/ask/tool.rs` | 仅 LLM 可见措辞：「arch 批准闸」→「完整流水线批准门」（`arch_gate_shape` 函数名、`switch_to_auto_edit` 字段、前端同步逻辑等机制零改动） |
| `AGENTS.md` | 必读文档行：standard-workflow 为主入口、arch-orchestrator 降为历史契约；工程分层表 agents/ 行注同步 |

机制原样复用（零改动）：ask 的 `arch_gate_shape`/`switchToAutoEdit` 切档与前端胶囊同步、subagent 的 G2 分析产物标记、agents 角色注册表、plan 档 P0-P6 协议、fence/approval。

## 3. 注入文本（WORKFLOW_SECTION 全文见 core/prompt.rs）

结构：分档路由（轻量/完整/计划档互斥）→ 完整流水线 S1-S9（S1∥S2、S6 并行拉满、S7∥S8；S4 拟定分支名、S5 批准询问列明分支名并构成建分支预授权、S6 动工前 `git switch -c`）→ 产物与纪律（任务目录规则、四文档、临时会话豁免、即产即存、git 写仅限计划分支创建/切换、单流程互斥）。机制锚点词（switchToAutoEdit / E_SUBAGENT_BUSY / id="approve" / 四文档名 / 三开发角色名 / `<type>/<slug>` / git switch -c）由测试钉死，防文本与后端机制漂移。

## 4. 验证

- `cargo test`（src-tauri）：全绿（字数护栏测试先行守护；分支条款入 S4/S5/S6 时预算 1750→2000 并注释理由，Windows 实测 538 passed / 0 failed / 0 warning）。
- `pnpm --dir ui test` + `pnpm --dir ui build`：基线全绿（本轮未触前端，无需复核）。
- code-reviewer 审查：无 🔴；5 🟡（agents/mod.rs 失真注释、注入位置/顺序断言缺失、Windows 时间戳变体丢失、计划档切档表述歧义、0-README 插入位置破坏子条目）+ 5 🟢，全部采纳修复并复核。
- 手动验证清单：
  1. 新会话提大型需求 → 观察自动进入完整流水线（explore 与 product-manager 两卡同时运行）；
  2. S6 多角色多卡同时运行（如 2×backend-dev + 1×frontend-dev）；S7/S8 两卡同时运行；
  3. `$` 菜单不再出现 arch；`$arch` 不再可触发；
  4. 一行小改动 → 直接实现（无 S1-S9、无 tasks 落盘）；
  5. 临时会话提大需求 → 流水线可跑、S9 说明「产物未落盘」；
  6. 计划档提实现需求 → 仍走 P0-P6，不启动流水线；
  7. git 仓库内批准计划 → 批准询问含分支名，随后先执行 `git switch -c` 再开始改动；拒绝计划 → 不产生分支；非 git 目录 → 计划注明跳过分支。

## 5. 收尾状态

- [x] code-reviewer 审查（跨文件变更）：无 🔴，5 🟡 + 5 🟢 全部修复
- [x] 前端 test + build 复核：262 passed + build 成功
