# 计划批准即定分支（plan-branch-proposal）

> 需求：面对用户的任何需求和修复，都应该在指定计划时同时拟定分支名称，在向用户提问是否执行计划时一并显示，并在用户同意后使用新分支来进行开发工作。
> 本文档 = 需求澄清决策记录 + 契约条款说明；条款落点为标准工作流（S4/S5/S6，`core/prompt.rs` `WORKFLOW_SECTION`）与 plan 档（P3 与批准后段，`core/agent/drive.rs` `<plan-mode>`）。

## 1. 需求澄清决策（用户已拍板）

| 决策点 | 结论 |
|---|---|
| 落点层面 | 产品内置行为（所有 CodeWave 用户生效）：改内置工作流/plan 档提示词条款，不改 ask schema、不改前端 |
| 分支创建方式 | agent 批准后自行执行 `git switch -c`（走 command 工具 + fence 机制兜底）；后端不代执行，`git/` 层保持只读架构 |
| 命名规范 | `<type>/<slug>`（type 对齐 Conventional Commits：feat/fix/docs/refactor/test/chore；slug ≤24 字符，对齐任务目录 slug 约定）；基线 = 当前 HEAD |
| fence 缺口 | plan 档只读白名单按命令首词 `git` 放行、L3 高危名单未覆盖 branch 写形态（`git branch` / `checkout -b` / `switch -c`）——独立立项后续处理，本批次不动 |

## 2. 契约条款

- **S4 / P3 拟定**：方案必须含拟定分支名（git 仓库内 `<type>/<slug>`，slug ≤24 字符、基线当前 HEAD）；非 git 仓库（含临时会话全局目录）注明跳过分支环节，正常开发。
- **S5 / P3 展示与授权**：批准询问（ask 单题 `id="approve_plan"`）的题干与 `plan` 文本列明分支名——前端计划卡与询问气泡经既有 `plan` 字段天然展示，零协议改动。**批准 = 预授权创建并切换计划所示分支**（免二次确认）；commit / push / merge 等仍由用户执行。
- **S6 / 批准后执行**：动工前先执行 `git switch -c <分支名>`；分支已存在则改 `<name>-2` 后缀并在批准询问中说明（重新过批准门）；建分支失败（被拦截/冲突等）如实报告，请用户手动处理或授权。
- **脏工作区**：计划列明未提交改动将随新分支携带；不擅自 stash。

## 3. 边界与授权语义

- 本契约与「AI 默认禁止一切 git 写操作」约定的衔接：既有约定自带豁免通道（用户明确要求或授权时可执行），本契约把「批准计划」定义为对**该分支创建/切换**这一项的显式预授权，范围不外溢。
- fence 现状下 `git switch -c` 非 Plan 档静默放行：批准已构成用户预授权，语义自洽；缺口修复后自动升级为二次确认，条款无需变更。
- 开发中断分支保留不清理，交用户处置；修订循环中分支名可沿用或按用户意见更新，每次批准都重新构成预授权。

## 4. 改动清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/core/prompt.rs` | `WORKFLOW_SECTION`：S4 拟定分支名、S5 批准列明+预授权、S6 先建分支、纪律条款「git 写仅限计划分支创建/切换」；锚点测试增 `<type>/<slug>` 与 `git switch -c`，预算 1750→2000 |
| `src-tauri/src/core/agent/drive.rs` | `<plan-mode>`：P3 拟定分支名+批准列明+预授权；批准后段先 `git switch -c` 再动工 |
| `AGENTS.md` / `agents.md` | 「Git 约定」补预授权例外；需求/缺陷流程要点同步 |
| `docs/standard-workflow.md` | §1 决策表/批准门、§3 注入文本结构、§4 手动验证清单同步 |
| `docs/plan-mode-workflow.md` | §3 流程表 P3/P4 同步 + §7.8 批次记录 |
| `docs/0-README.md` | 登记条目 |

## 5. 验证

- `cargo test`（src-tauri）：全绿，含 `workflow_section_contains_mechanism_anchors`（新锚点 + 预算 2000 护栏）与 plan 档文本断言（`system_extra` 含 `plan-mode`）。
- 手动验证清单见 [standard-workflow §4](./standard-workflow.md) 第 7 条：git 仓库内批准计划 → 询问含分支名 → 先 `git switch -c` 再改动；拒绝计划 → 不产生分支；非 git 目录 → 计划注明跳过。
