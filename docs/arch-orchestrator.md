# 内置 arch 编排智能体与任务产物契约

> **状态（2026-09-07）**：arch 技能已内置化为标准工作流（常驻系统提示，`$arch` 不再存在），见 [standard-workflow](./standard-workflow.md)；本文档保留四文档产物契约与历史契约记录。
>
> 批次报告：新增内置子代理角色注册表（explore/dev/reviewer/product-manager/code-reviewer/tester；后经 [dev-subagent-specialization](./dev-subagent-specialization.md) 拆分为 explore/backend-dev/frontend-dev/app-dev/…）、
> `$arch` 全流程研发编排技能、ask 批准门切档扩展、`.codewave/tasks/<任务目录>/` 四文档产物契约。

## §1 需求与形态决策

需求：增加内置智能体 **arch**（架构师/任务编排）——接收需求后调用 explore 子代理源码调研 → product-manager
需求分析 → arch 自行完成技术方案 → 多个开发子代理（backend-dev/frontend-dev/app-dev，按任务包技术栈选择）并行开发 → reviewer 方案对齐审查 → tester 测试并出报告；
产物落在项目 `<主目录>/.codewave/tasks/<自动生成目录>/`：`requirement.md` / `plan.md` / `review.md` / `report.md`。

关键形态决策（基于机制核实）：

| 决策 | 依据 |
|---|---|
| **arch = 内置技能（`$arch`），主会话扮演编排者** | 子代理不能再派生子代理（subagent 工具对子代理排除，tools/subagent.rs 排除集），嵌套编排需改 runtime/事件路由，成本风险高；主会话天然持有 subagent 工具，技能即可承载编排剧本，零新 IPC/事件/前端 |
| **role 从自由字符串升级为注册表角色** | 新增 `src-tauri/src/agents/mod.rs`；subagent 命中注册表时把角色定义全文注入子代理 system_extra（此前只有 role 名字符串，`.agents/agents/*.md` 只是开发期文档、运行时不读取） |
| **四个 md 一律由 arch 主会话落盘** | 子代理审批弹窗不可达（ask:opened 以 sub_id 为 session，前端无对应 tab），ConfirmEach 档下子代理写文件会静默拒绝；且产物是跨阶段汇总文档，主会话持有全部子代理汇报 |
| **S5 插入批准门，批准后自动切自动编辑档** | ConfirmEach 档下 dev 子代理写码审批不可达 = 流程死锁，切档是可行性前提；与 Plan 档「批准即切 AutoEdit」既有语义（ask.rs）一致；写入控制权保留在用户 |
| **并行 dev 单批 ≤4** | 全局子代理并发上限 MAX_CONCURRENT=4，超限直接报 E_SUBAGENT_BUSY（不排队）；批次内并发上限同为 4 |

## §2 改动清单

### 2.1 新增 `src-tauri/src/agents/mod.rs`（角色注册表）

- `AgentDef { name, description, body }` 静态表，8 个内置角色：
  - `explore`：只读源码调研（模块地图/关键事实带 文件:行号/既有模式/风险未知项）
  - `backend-dev` / `frontend-dev` / `app-dev`（原 `dev` 拆分，见 [dev-subagent-specialization](./dev-subagent-specialization.md)）：按自包含任务包实现，read 先于 edit、限定文件范围、自测、汇报改动清单
  - `reviewer`：方案对齐审查（对齐表 + 🔴🟡🟢 分级 + 二选一结论「对齐/未对齐」）
  - `product-manager` / `code-reviewer` / `tester`：由 `.agents/agents/*.md` 适配——去掉「向用户提问」类指令
    （子代理无提问通道，改为列假设继续）；tester 增加「实际执行测试命令 + 报告真实结果，禁止编造通过状态」
- `normalize_role()`：trim + 小写 + `_`/空格 → `-`；`find()` 别名 `test→tester`、`pm→product-manager`；
  未命中返回 None（调用方保持自由字符串旧行为）。
- arch 不进注册表：它是技能（编排者），不是可派生的子代理角色。

### 2.2 `tools/subagent.rs`

- 新增纯函数 `build_system_extra(role, max_steps, def)`：通用 `<subagent-discipline>` 纪律块在前，
  命中注册表时追加 `<agent-definition name="…">全文</agent-definition>`；纪律块内角色名展示规范名。
- 新增纯函数 `is_analysis_role(role)`：以注册表 `find()` 为单一事实源核对分析角色
  （审查 Y1：别名 test→tester、pm→product-manager 与注入共用同一判定，防 role="test"
  注入 tester 全文却不置分析标记的劈叉）。
- 工具 description 与 schema 的 role 描述列出规范角色清单，引导主代理用规范 role 名。
- **G2 分析标记跨档位置位**：pm/tester 子代理成功返回即置 `analysis_done`（原条件限定 Plan 档）。
  该标记仅在批准生效点被消费，其余档位置位无副作用；arch 流程在 ConfirmEach 档发起批准依赖此标记。

### 2.3 `tools/ask.rs`（批准门切档扩展）

- Args 新增 `switchToAutoEdit`（显式 `#[serde(rename)]`——serde camelCase 对 `autoedit` 段机械产出
  `switchToAutoedit`，与 schema 键不一致，须显式对齐；default None）。
- 新增纯函数 `wants_mode_switch(mode, approved, switch_flag)`：
  Plan 档批准即切（既有语义不变）；ConfirmEach 仅在携带 flag 时切；AutoEdit/FullAccess 不切。
- 切档路径复用既有代码：`plan_approval_gate`（todos 非空 + analysis_done，arch 流程 S2 已满足，
  后端兜底防跳步）→ 重置 G3 范围基线 → prefs 切 AutoEdit → `run:inject` 提示继续执行。
- Plan 档不传 flag 行为完全不变；普通会话不带 flag 的 ask 批准不切档（无行为外溢）。
- **形状约束（审查 Y3）**：`arch_gate_shape()`——ConfirmEach 档的切档仅认可标准批准协议形状
  （单问题 + 选项含 id="approve"），防模型借任意 ask + flag 自由升档；Plan 档不受此限。
- **前端胶囊同步（审查 R1）**：`ask:opened` payload 附加 `switch_to_auto_edit` 字段（不新增事件键）；
  `AskPanel.submit()` 的本地切档同步条件由 `approval_mode === "plan"` 扩为
  `plan || ask.switchToAutoEdit`——否则批准切档后胶囊失真，且后续 `updatePrefs` 全量覆盖会把
  后端 AutoEdit 静默改回（dev 写码审批不可达 → 流程无声卡死）。涉及
  `ui/src/ipc/types.ts`（AskOpenedEvent）、`ui/src/stores/run.ts`（AskState + ask:opened handler）、
  `ui/src/features/tools/AskPanel.tsx`（submit 同步条件）。

### 2.4 `skills/mod.rs` 内置技能新增 `arch`

编排剧本（全文见 `builtin_skills()`），要点：

- **前置检查**（不满足即停止）：临时会话拒绝（产物无处落）；计划档拒绝（写工具被排除，提示切档）。
- **任务目录**：`<主目录>/.codewave/tasks/<YYYYMMDD-HHMMSS>-<slug>/`，slug ≤24 字符、过滤路径非法字符、
  可保留中文；`date +%Y%m%d-%H%M%S` 取时间戳（Windows 用 `Get-Date`）；E_EXISTS 时追加 -2/-3。
  与既有计划任务 JSON（`tasks/<id>.json` 平铺）共存互不感知。
- **阶段流**：S1 explore(40) → S2 product-manager(30) → S3 落盘 requirement.md（关键歧义先 ask 澄清 ≤1 轮，
  选项文本不得含「执行/approve」以免误触批准信号）→ S4 arch 自写 plan.md + plan 登记 todos（含验证项）→
  S5 ask 批准门（`switchToAutoEdit=true`；驳回修订上限 2 轮）→ S6 开发子代理并行（backend-dev/frontend-dev/app-dev 按技术栈选角，60/个，单批 ≤4，>4 分批，
  E_SUBAGENT_BUSY 用 wait 重试）→ S7 reviewer(40) 对齐审查（🔴 返工 1 轮上限，复审不过标注「未对齐+遗留清单」
  继续推进）→ S8 tester(60) 实际执行测试 → S9 收尾汇报（各阶段结论 + 产物绝对路径 + 遗留事项）。
- **纪律**：四个文档只由 arch 落盘；即产即存、阶段失败保留已有产物并如实汇报；AI 零 git；
  同一项目软约束只跑一条 arch 流程；四个文档模板骨架内置。

### 2.5 其他

- `lib.rs`：注册 `pub mod agents;`。
- 前端零改动：`$` 菜单经 `list_skills` 自动出现 arch；子代理卡复用既有 `sub:*` 事件；事件面 24 键不动。

## §3 审查记录（code-reviewer，强制）

多文件跨层变更完成后按 AGENTS.md 调用 code-reviewer 审查，结论与处置：

- **🔴 R1（已修复）**：ConfirmEach 档批准切档后前端权限胶囊不同步，且 `updatePrefs` 全量覆盖会把
  后端 AutoEdit 静默改回 → 见 §2.3 前端胶囊同步链路（ask payload 附加字段 + AskPanel 同步条件扩展）。
- **🟡 Y1（已修复）**：分析角色判定与注册表别名劈叉（role="test"）→ `is_analysis_role()` 改用
  `find()` 单一事实源 + 一致性单测。
- **🟡 Y2（已修复）**：arch 剧本「主目录 = temps 上一级」层级差一级（会写错到 `.codewave/.codewave/`）
  → 改为以系统提示 `<project>` 段 Project directory 为锚 + tasks 与 temps 同级说明。
- **🟡 Y3（已修复）**：ConfirmEach 档 flag 缺形状约束 → `arch_gate_shape()` 硬化（单问题 + approve 选项）。
- **🟡 Y4（已修复）**：AGENTS.md 工程分层表登记 `agents/` 模块。
- **🟢 G1–G6（已修复）**：approved 重复传参、纪律块规范名展示、"超出排队"陈旧注释、剧本补
  「选项 id 亦不得含 approve/执行」、arch 剧本机制锚点回归测试、schema 标注精确键名。

## §4 验证结果

- `cargo test`：200 通过 / 0 warning。**2 个既有失败与本次无关**：`mcp::tests::real_stdio_server_connect_list_call`
  与 `real_streamable_http_server_connect_list_call` 依赖夹具脚本 `scripts/mcp-test-server.mjs`，该文件在仓库中
  不存在（git 历史从无记录），属既有环境缺陷，mcp 模块本次未触碰。修复建议：补交该脚本或给两测试加
  「脚本存在性」前置跳过（同 `node_available()` 模式）。
- `core::agent::tests::running_flag_resets_after_run_ends` 在全量并发跑下偶发失败一次，隔离运行与
  后续两轮全量均通过（时序敏感抖动，本批未触碰该模块）。
- `pnpm --dir ui test`：39/39 通过；`pnpm --dir ui build`：通过（含 AskPanel/run/types 胶囊同步改动）。

## §5 限制与后续

- 阶段推进靠技能文本约束（模型遵循），非后端硬状态机；批准门有后端硬校验兜底（todos + analysis_done）。
- 子代理失败不中断流程：arch 如实汇报中断点与已完成阶段（剧本纪律）。
- 后续可选：任务产物目录在 ProjectNav/任务区的只读列表展示；用户自定义代理目录（`~/.codewave/agents/`）
  低→高优先级合并（仿 SkillIndex）；role 白名单收紧。

## §6 手动验证清单（GUI 不做自动化点验）

1. dev 运行，打开项目会话（普通档），Composer 输入 `$` 确认菜单出现 arch。
2. 输入 `$arch <一个小需求>`：依序出现 explore → product-manager 子代理卡。
3. 检查 `<主目录>/.codewave/tasks/<时间戳>-<slug>/` 下 requirement.md、plan.md 已生成且内容完整。
4. S5 出现 ask 批准弹窗；选「批准开发」后权限胶囊自动变「自动编辑」（R1 修复点）。
5. 多个开发子代理卡并行（≤4/批，角色与技术栈匹配），代码写入工作区成功。
6. review.md、report.md 落盘；tester 阶段实际执行了测试命令（子代理卡展开可见 command 工具调用）。
7. 驳回路径：S5 选「补充意见」→ arch 修订 plan.md 后再次询问（不切档）。
8. 边界：临时会话输入 `$arch` 被拒绝并提示；计划档输入被提示切档；`tasks/` 下旧计划任务 JSON 不受影响。
