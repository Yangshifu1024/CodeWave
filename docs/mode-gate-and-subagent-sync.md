# 批准门选档确认 + 子代理档位实时同步 + 计划任务档位修正

> 批次：**权限档位从「隐式副作用」变为「显式、可追溯、可传播的会话状态」**。日期 2026-09-24 · 分支 `feat/mode-gate-subagent-sync` · 完整流水线（S1-S9）。
> 关联：[docs/plan-mode-workflow](./plan-mode-workflow.md)（plan 档批准协议）、[docs/standard-workflow](./standard-workflow.md)（S5 批准门）、[docs/arch-orchestrator](./arch-orchestrator.md)（`switchToAutoEdit` 机制）、[docs/session-pref-switch-toast](./session-pref-switch-toast.md)（prefs 生效时机）、[docs/composer-toolbar-batch-report](./composer-toolbar-batch-report.md)（权限四档语义表）。

## 1. 需求与背景

用户提出两条需求：

1. **模式需要在执行前有一次显示确认**——现状：S5 批准门点「批准开发」后，后端在 `tools/ask/tool.rs` **硬编码**切到 `AutoEdit` 并注入 `[system] …请立即按方案执行`，AI 随即动手；用户对「即将以什么权限执行」没有显式选择权，唯一反馈是 Composer 胶囊变化 + 一条 toast。
2. **切档需要实时更新至子代理**——现状：子代理在 spawn 时克隆父档位快照并一次性构造 `DriveParams`，主会话中途切档不影响在跑子代理；后果是「批准前抢跑的子代理全程只能拿到 `E_TOOL_BLOCKED`」与「用户切到完全访问后子代理仍在等审批」两类体感缺陷。

排查中还发现第三个**既有隐患**：计划任务 runtime 因 `SessionRuntime::new_task` 不设 prefs 而落在 `ApprovalMode::default()` = **Plan** 档；但 `run_task_agent` 自建参数、不调 `main_drive_params`，于是写工具既不被排除也不弹审批，而 fence 活读 prefs 又把白名单外命令全锁成 `E_PLAN_READONLY` —— **写文件放行、命令全锁**，语义自相矛盾且无任何测试守护。本批一并收敛。

## 2. 决策记录（grilling 五轮 + 需求分析）

需求澄清经五轮结构化追问，20 条决策全部落定：

### A 组｜执行前选档确认

| # | 决策 | 理由 |
|---|---|---|
| A1 | **扩展 ask 批准门为三选项**（零新增 UI 组件），不新增独立确认卡 | 复用既有阻塞交互与 `wants_mode_switch` 判定，一次交互完成 |
| A2 | 仅批准门出现选档确认；用户手动 Shift+Tab / 胶囊切档不弹 | 用户自己切档时意图已明确 |
| A3 | **新增显式档位字段**取代子串猜测 | `approved_hit` 的 `s.contains("执行")` 会把任意含「执行」的选项 id 误判为批准 |
| A4 | 模型可请求「完全访问档」选项，用户点击即授权（不额外弹确认框） | 与 ask 既有语义一致（模型声明选项、用户做决定） |
| A5 | 两个批准类选项**都可直提** | 交互最少 |
| A6 | 完全访问选项带**内联风险说明**（复用既有 `Option2.description`，零后端改动） | 风险告知可见且零额外交互 |
| A7 | **允许跨档**：逐项确认档 / 计划档下都能一次跳到完全访问档 | 用户主权 |
| A8 | **不记忆**上次选择，每次批准门都显式选 | 每次执行范围不同 |
| A9 | 批准门的 G2（分析产物）/ G3（todos 基线）门禁**仍校验** | 防完全访问变成绕过工作流纪律的后门 |

### B 组｜子代理实时同步

| # | 决策 | 理由 |
|---|---|---|
| B10 | **双向立即**（切严切宽都立即影响在跑子代理） | 语义直白；接受「批准前抢跑重新可行」的后果 |
| B11 | 粒度：**每个 LLM step 边界** | 进行中的工具调用无法安全撤销 |
| B12 | 触发路径：**所有切档路径** | 无例外路径可钻 |
| B13 | 覆盖面：嵌套子代理跟随**根会话**；计划任务 runtime 独立 | 与 `root_session_id` 归属语义一致 |
| B14 | 字段范围：**仅模式字段**（`model_id` / `reasoning_effort` 仍为 spawn 快照） | 避免跑到一半换模型 |
| B15 | 同步内容：**全同步**（工具集 + 系统块 + fence 判定） | 只做一半会出现「工具集松了但 fence 还锁着」的错位 |
| B16 | 档位真变化时**注入 `[system]` + 记会话日志** | 避免模型因工具集突变而困惑；未变时不污染历史 |
| B17 | 子代理抽屉**显示一行**当前档位 | 让权限状态可见 |
| B18 | 在途审批弹窗按**打开时刻快照**判定 | 与「批次入口快照」先例一致 |

### C 组｜连带处理

| # | 决策 |
|---|---|
| C19 | 计划任务 runtime **显式设为完全访问档**（修复上述矛盾） |
| C20 | **完全访问档下只读角色解锁写能力**，并同步修正其只读提示文案 |

## 3. 实施内容

### 3.1 A 组｜批准门选档

**后端**（`src-tauri/src/tools/ask/tool.rs`）

- `Option2` 新增 `mode: Option<ApprovalMode>`，带 `#[serde(default, skip_serializing_if = "Option::is_none")]`（**wire 上 `None` 时不出现该键**，与前端类型 `mode?: ApprovalMode` 一致）
- 批准类判定 = `mode.is_some()`；选中任一批准类选项 → `approved = true`，目标档位 = 该选项的 `mode`
- **切档目标不再硬编码 `AutoEdit`**：完整路径与轻量路径都按 `switch_target`（选中批准类选项的 `mode`，缺省回落 `AutoEdit`）
- 旧的宽松子串匹配（`is_approve_option` 的 id/label 规则、`approved_hit` 的 `approve`/`执行` 匹配）**保留为兼容兜底**
- `wants_mode_switch` 新增 `mode_requested` 维度：`AutoEdit` / `FullAccess` 档下也能按用户所选目标切档（含 `auto_edit → full_access` 升级）
- `approve_option_id` 语义由「首个命中」改为「主批准项 = 推荐项优先」
- `plan_approval_gate`（G2/G3）**一字未改**
- **`ask:opened` 顶层键与事件键名零改动**（`mode` 随 `questions[].options[]` 自然序列化；事件面仍 29 键）

**提示词三处同步**：`ask/tool.rs` 的 description、`core/prompt.rs` 的 S5 行、`core/agent/drive.rs` 的 `<plan-mode>` 块。后者补上了关键一句「**批准类选项必须带 `mode` 字段声明目标档位**」——否则模型不声明 `mode`，后端回落 `AutoEdit`，用户选「完全访问执行」会被静默降级。`WORKFLOW_SECTION` 的 2000 字预算断言仍通过（1995 字）。**锚点词 `switchToAutoEdit` 与 `id="approve"` 保留**（有测试钉死）。

**前端**（`ui/src/features/tools/AskPanel.tsx` 等）

- 直提：选中带 `mode` 的选项即直接提交（两个档位都可直提），并**合并当前已选状态**（不再丢其他题答案）
- `approved` 判定改为结构化（选中选项 id ∈ 带 `mode` 的选项集合），旧子串/正则路径作为兼容分支保留
- **胶囊同步按实际档位**：新增 `modePath = approvedMode != null` 路径，判定为 `modePath || planPath || lightPath`；不再硬编码 `auto_edit`
  - ⚠️ 这条修复了一个**实质缺陷**：修复前，档位为 `auto_edit` / `full_access` 且 ask 未带 `switch_to_auto_edit` 时两条旧路径都为假 → 前端**不发** `set_session_prefs`，而后端已切档；由于 `updatePrefs` 会推全量 prefs 且后端以「前端为事实源」，之后任意一次 prefs 写入会把后端档位**静默翻回**
  - 顺序契约保持：先 `resolveAsk`，再 `updatePrefs`
- `full_access` 选项**恒定**附加内联风险说明（即使模型自带 `description` 也不被顶掉，防提示注入式淡化）
- `SubView.approvalMode` + 抽屉头部档位行（归档子代理无数据时不渲染）

### 3.2 B 组｜子代理档位实时同步

**核心机制**（`src-tauri/src/core/agent/drive.rs`、`src-tauri/src/tools/subagent.rs`）

- 新增 `SubBase`（基座）：根会话 id / 角色 / `max_steps` / spawn 档位 / **基座排除集**（内部 7 项：`ask`/`subagent`/`plan`/`skill`/`scheduled_task`/`suggest`/`wait`）/ **基座 `system_extra`**（角色纪律块）/ 基座 `idle_policy`
- 新增 `subagent_drive_params(base, parent_prefs) -> DriveParams`：**从基座重建**（排除集 = 基座 ∪ 父档派生 ∪ 角色派生，随后去重；`system_extra` = 纪律块 + 按当前父档重拼 `<plan-mode>`）
  - **绝不增量追加** —— 这是本改造最容易踩的坑：增量追加会让 Plan→AutoEdit 后的旧 `<plan-mode>` 块**永久残留**
  - 幂等、不累积（`scheduled_task` 在基座与 Plan 档派生中重叠，靠去重收敛）
  - spawn 与第一步重算**共用同一装配路径**，保证两者逐字一致（否则第一步就会误触发一次「档位变化」注入）
- 每步重算点三分支：主会话（`main_drive_params`，现状）/ 子代理（`subagent_drive_params` 重建）/ 任务运行（冻结）
- **子 rt 的 prefs 也刷新**（把根会话 `approval_mode` 写入子 rt），使 `ToolCtx::approval_mode()` / `fence_policy()` 与写审批门跟随；**只同步档位**，`model_id` / `reasoning_effort` 保持 spawn 快照
- 父档查询：子 rt 的 `root_session_id` → `AgentCore::session()` → `prefs()`；查不到（父会话已删除）时**保持现有档位、不 panic**
- **档位真变化时**才向子代理历史注入 `[system] 权限模式已变更为 X` + `session_log::info`（未变时历史长度不变）
- 嵌套子代理跟随**根会话**档位（`root_session_id` 指向顶根）
- `sub:step` 事件载荷新增 `approval_mode`（每步上报；**不改键名、不新增事件键**）

**只读角色与完全访问档**（决策 C20）

- `apply_role_policy` / `readonly_extra_excludes` 增加档位入参：**FullAccess 档下只读角色（explore / reviewer / code-reviewer）不再排除 `WRITE_TOOLS`**；`idle_policy` 保持 `NudgeOnly`（与档位解耦）
- 角色纪律块的只读提示句按档切换为授权说明；同时 `<agent-definition>` 正文末尾**追加覆盖声明**（「当前为完全访问档，上述只读约束暂停」），消除「正文说不许写、提示说可以写」的自相矛盾
- **其余三档（`confirm_each` / `auto_edit` / `plan`）下只读角色行为与文案逐字不变**（回归保护）

### 3.3 C 组｜计划任务档位

- `src-tauri/src/core/scheduler.rs`：创建任务 runtime 后显式 `rt.set_prefs(task_prefs())`（FullAccess，仅动档位，model / effort 保持 None 回落全局）
- **行为变更**：计划任务的命令执行能力被放开（此前非白名单命令全被 `E_PLAN_READONLY` 拦），**灾难级命令仍被直接拦截**；写文件能力与现状一致（此前就是放行的）

## 4. 契约变更

| 变更 | 兼容性 |
|---|---|
| `Option2` 新增 `mode`（`skip_serializing_if` 保证 `None` 时不下发该键） | serde default，旧模型/旧测试不受影响 |
| `ask:opened` 载荷：选项携带 `mode` | **不新增顶层键、不改键名**；29 键契约不动 |
| `sub:step` 载荷新增 `approval_mode` | 同上 |
| `DriveParams` 新增 `sub_base`（内部字段） | 无 wire 影响 |
| 无新增依赖、无删除 | — |

## 5. 验证

**自动化**（worktree `feat/mode-gate-subagent-sync`，改动 20 文件 + 1 新测试文件，约 1400 行）

| 门禁 | 结果 |
|---|---|
| `cargo test` | **1025 passed / 0 failed / 3 ignored** |
| `cargo fmt --check` | exit 0 |
| `cargo check --all-targets` | 仅 1 个**存量** warning（`src/tools/postcheck.rs:305` unused import，主工作区同样存在，非本批引入） |
| `pnpm --dir ui test` | **1076 passed / 93 files** |
| `pnpm --dir ui run lint` | 0 error（1 个存量 warning：`features/chat/Composer.tsx:239`） |
| `pnpm --dir ui build` | type check + vite build 通过 |
| `node --test "scripts/**/*.test.mjs"` | 24/24 |

新增测试：Rust 22 例（选档切档 / 跨档 / G2-G3 门仍拦 / 旧形态回归 / `mode` serde / 基座重建不残留 / 排除集幂等 / prefs 跟随且 model-effort 不变 / 档位未变不注入 / 父会话缺失降级 / FullAccess 只读解锁与文案 / 任务档位）、前端 15 例（两档直提 / 胶囊按实际档位 / **高档位下的胶囊同步缺陷回归** / 多题直提合并 / 兜底风险说明 / 抽屉档位行 / `ask:opened` mode 透传 / `sub:step` 档位写入）。

**审查**：S7 两轮（首轮发现 5 🟡 + 1 🟢，返工后复审）。首轮无 🔴，但其中一条被审查与测试**独立**指向同一处（前端胶囊同步缺陷），已按 🔴 级处置修复并补回归用例。

## 6. 已知边界与遗留

1. **`core/agent/mod.rs` 的 re-export**：新增 `SubBase` / `subagent_drive_params` 是必要的（`mod drive` 为私有模块）；`main_drive_params` 被移出 re-export —— 返工时曾尝试加回，实测会触发 `unused import` warning（其唯一外部调用点已随本改造删除），故保留移除状态。
2. **`postcheck.rs:305` 的存量 warning** 与 AGENTS.md「0 warning」表述不符，属本批之外的既有漂移（已在 AGENTS.md 记录）。
3. **未端到端覆盖**：真实 run 内的「每步重算 → 下一步工具集变化」链路（需 mock provider 多步脚本）目前由纯函数级 + 参数重建级测试覆盖；计划任务档位的生产调用点（`run_task_locked`）无端到端测试；`sub:step` 的 `approval_mode` 载荷无后端断言（仅前端 handler 单侧守护）。
4. **worktree 位置**：本批在 `.codewave/worktrees/mode-gate` 内开发。该路径位于项目数据目录内，而「删除项目」会级联清理托管目录 —— 存在被连带删除的风险。
5. **只读角色解锁的语义边界**：完全访问档下只读角色获得写工具，但角色定义正文仍描述「只读调研」姿态（仅追加了覆盖声明）—— 工具可用 ≠ 模型会用，属有意取舍。
6. **档位门 ≠ 批准门**：用户手动 Shift+Tab 切到 `auto_edit` 同样让写工具可用，此时 `approved_plan` 为空 → G3 范围门不触发，仅剩 `E_PLAN_REQUIRED`（todos 非空）约束。这是既有语义，本批未改。

## 7. 手动验证清单（界面改动不做 GUI 自动点验）

1. **批准门选档**：在计划档下让 AI 出方案并进入 S5 批准门 → 卡片上应看到三个选项（「执行方案」/「完全访问执行」/「补充意见」），完全访问项带风险说明行；
2. **选自动编辑档**：点「执行方案」→ 会话档位变为「自动编辑」，胶囊同步，AI 随即开工；
3. **选完全访问档**：点「完全访问执行」→ 胶囊变为「完全访问」，且**不出现档位回弹**（这是本轮修复的缺陷：旧实现在此场景下前端不同步、随后被静默翻回）；
4. **直提**：两个批准类选项都应「点一下就提交」（不需再点提交按钮）；
5. **子代理实时跟随**：在子代理运行期间用 Shift+Tab 切档 → 子代理抽屉头部的档位行应在下一步更新；切到完全访问档后，若子代理是 explore / reviewer，其写工具应解锁；
6. **计划任务**：手动触发一个含非白名单命令的计划任务 → 命令应能正常执行（此前会被 `E_PLAN_READONLY` 拦），灾难级命令仍被拦截。
