# 16 · 计划模式（plan 档）强制阶段流程

> 需求：计划模式严格按流程执行——接到需求 → 调用 product-manager 分析 → 输出需求分析 → 基于分析编写技术方案与计划 → 用户确认后执行。
> 本文档 = 需求分析全文 + 流程定义 + 决策记录，是 plan 档流程的持久契约；`AGENTS.md` 专门代理小节与 core `<plan-mode>` 提示词均引用此文档。

## §1 需求背景与问题定义

### 1.1 现状

plan 档的硬约束只有三件套（`core/agent.rs`）：

1. 工具排除：`edit/create/delete/service/scheduled_task` + MCP 不可用；
2. shell 走 fence 只读白名单（`plan_readonly`），白名单外一律 Confirm；
3. ask 批准协议：方案 ask 选「执行方案」→ 会话切执行档并注入执行指令。

而「调 pm 分析 → 需求分析 → 技术方案 → 批准 → 执行」的阶段序列只是 `<plan-mode>` 提示词的**软约定**，无结构化状态承载、无 gate 校验。

### 1.2 失控点

| 编号 | 失控点 | 根因 |
|---|---|---|
| G1 | 阶段序列是软约束 | 提示词建议，无 gate；模型可跳过 pm 直接给粗方案 |
| G2 | plan todos 无阶段语义 | 「分析是否已产出」无法被系统校验，gate 全靠模型自觉 |
| G3 | 批准后执行无流程约束 | 「按方案执行」是提示词；可偏离 todos、漏掉收尾审查 |
| G4 | 无需求分类路由 | AGENTS.md 三条流程（需求/缺陷/审查）plan 档未区分 |
| G5 | plan 档可被 Confirm 弹窗旁路 | 白名单外命令误点确认即破只读（**本批次不处理**，另行批次） |
| G6 | 阶段状态无持久化 | 流程进度只在对话文本中，压缩/新会话即丢失 |

### 1.3 问题定义

把 plan 档从「提示词建议流程」升级为「指令层强制的阶段流程」：每阶段有明确产物与 gate，模型自检 gate 后推进；为微小改动提供轻量路径，避免流程税。

> 本批次实现范围：指令层（[docs/plan-mode-workflow](./plan-mode-workflow.md) 契约 + AGENTS.md + `<plan-mode>` 提示词）。G2/G3 的系统性 gate（结构化校验、偏航阻断）与 G5 的硬收口另行批次。

## §2 用户故事与验收标准

- **US1 需求流程**：plan 档提出新需求 → AI 先调 product-manager 产出结构化需求分析 → 技术方案可追溯到分析要点 → 未经分析不得发起批准 ask。AC：方案前存在 pm 子代理调用记录；分析报告在聊天中可见。
- **US2 缺陷流程**：报 bug → 第一分析阶段调 tester（复现/根因/影响面/修复建议+回归要点）→ 修复方案含三要素。AC：分类为缺陷的请求先行子代理 role=tester。
- **US3 批准与执行**：选「执行方案」后严格按已确认 todos 执行，完成后按范围决定是否强制 code-reviewer。AC：执行完成后多文件/跨层变更必须出现 reviewer 调用。
- **US4 轻量路径**：微小改动不强制全流程。AC：走轻量路径时在回复中明示；批准 ask 不省略。
- **US5 修订收敛**：选「补充意见」后逐条回应（采纳/不采纳+理由），修订基于上一版做 diff 式更新，不重做需求分析（除非推翻需求前提）。

## §3 流程定义（P0–P6）

### 3.1 需求类主流程

| 阶段 | 产物 | gate（进入下一阶段条件） | 批准者 |
|---|---|---|---|
| P0 接收与分类 | 分类标签：需求/缺陷/问答/混合 + 一句话依据 | 分类结论已声明 | 系统；用户可纠正重路由 |
| P1 澄清（可选） | 澄清问题（ask 或行内提问） | 关键歧义消除，或声明「基于假设推进」并列出假设 | 用户 |
| P2 需求分析 | pm 子代理报告：背景/用户故事/AC/边界/非目标/开放问题 | 报告结构完整；开放问题回流 P1（≤2 轮，超限逐条决策 ask） | 无用户 gate（决策 O3=B） |
| P3 技术方案与计划 | 技术方案（文件级改动点/风险/回滚；git 仓库内拟定分支名 `<type>/<slug>`，slug ≤24 字符、基线当前 HEAD，非 git 仓库注明跳过）+ plan todos 登记（含验证项）+ 验证方式 | todos 非空且含验证项 → 才允许发起批准 ask；批准询问题干与 plan 文本列明分支名 | 用户（ask：执行方案/补充意见；批准 = 预授权创建并切换该分支） |
| P4 执行 | 分支（git 仓库内先 `git switch -c <分支名>`，已存在改 `-2` 后缀并说明、失败如实报告）+ 代码变更 + 测试运行结果 | 每步写操作对应 todo；完成 = 全部 completed + 验证通过 | 已批准，无需再问 |
| P5 审查收尾 | code-reviewer 报告 | 无未决 🔴 项 | 多文件/跨层变更强制（O7=B）；轻量路径不强制 |
| P6 汇报 | 变更摘要 + 验证结果 + 偏差记录（走了哪条路径） | — | — |

### 3.2 缺陷类分支（O4=A 自动分类路由）

P0 分类为缺陷 → P1' tester 分析（复现步骤/根因/影响面/修复建议+回归要点）→ P3 起同主流程。
分类依据必须在 P0 声明；用户纠正后重路由，已产出分析不浪费（tester 报告可直接作为方案输入）。

### 3.3 问答/纯咨询

不进入流程（不建 todos、不调子代理），与 `<plan-protocol>`「Pure Q&A 不需要 todos」一致。

## §4 边界决策（用户已拍板）

| 决策 | 选择 | 说明 |
|---|---|---|
| O1 轻量路径 | **B 阈值轻量** | ≤2 文件、无删除、无新依赖、无跨层改动 → P2 简化为内置简析（不调子代理），P3 保留（方案可一句话级），批准 ask 不省；回复中明示「轻量路径」 |
| O2 执行偏差 | 超范围写操作弹确认 | 从宽：记录偏差，不无谓阻断 |
| O3 需求分析 gate | **B 不设** | pm 分析产出即展示，用户只在方案批准 ask 处确认（单 gate） |
| O4 缺陷路由 | **A 自动分类** | P0 分类路由：缺陷→tester 先行，需求→pm 先行；分类附依据，可纠正 |
| O5 修订循环 | 3 轮未收敛拆条决策 | 每轮逐条回应；拆条 = 争议点多选 ask 逐项收敛 |
| O6 状态持久化 | 文档落盘 + todos 快照 | 分析/方案随批次落 docs/；每回合 todos 快照随用户消息注入；压缩时列入必保留 |
| O7 审查收尾 | **B 多文件/跨层强制** | 轻量路径不强制；其余完成后必须 code-reviewer，🔴 必须修复 |
| O8 跳过口令 | 支持 | 用户显式「直接改/不用分析」最高优先，跳过 P2，仍登记 todos + 保留批准 ask |

## §5 实施

| 文件 | 改动 |
|---|---|
| `[docs/plan-mode-workflow](./plan-mode-workflow.md).md` | 本文档（新增） |
| `AGENTS.md` | 「可用专门代理」流程小节改为引用本文档的阶段化流程 |
| `src-tauri/src/core/agent.rs` | `<plan-mode>` 提示词重写为阶段化指令（P0–P6 + 轻量路径 + 修订收敛）；执行档切换注入语补「严格按已确认 todos 顺序执行」 |
| `agents.md` | 与 `AGENTS.md` 同步（两份并存现状保持） |

## §6 验证

- `cargo test` 全绿（154 基线；若提示词文本被断言引用则同步更新断言）。
- 手动验证（`pnpm tauri dev`）：
  1. plan 档提新需求 → 观察回复依次出现：分类声明 → pm 子代理调用 → 需求分析 → 技术方案 + todos（含验证项）→ 批准 ask；
  2. 报 bug → tester 先行，方案含根因/回归要点；
  3. 提「改个 typo」类微小改动 → 明示轻量路径，无子代理调用，仍有批准 ask；
  4. 选「补充意见」→ 逐条回应修订，不重做需求分析；
  5. 批准后 → 按 todos 执行，多文件改动完成后出现 code-reviewer 调用；
  6. 问答类（如「这个函数干嘛的」）→ 直接回答，不建 todos 不进流程。

## §7 实现记录：系统性硬 gate（G2/G3/G5，第二批次）

> 把上一批次遗留的 G2（分析先行）、G3（执行范围）、G5（只读承诺）从提示词软约束升级为系统强制。
> 挂点全部在系统可判定铺点：批准生效点 / todos 冻结快照 + plan 全量替换 diff / fence 判定出口。

### 7.1 G5：plan 档只读硬拒（safety/fence.rs）

- `check_command_depth` 包一层出口转换：`plan_readonly` 下**一切 Confirm → Block（E_PLAN_READONLY）**，
  错误文本含改道指引（纳入方案待批准后执行 / 换白名单内替代命令），并**点名被拦命令**
  （`command_excerpt`：折叠换行 + 截断 120 字符 + `…`；起因：只有泛化文案时卡片标题会把命令截在半个 token 上，
  日志里也看不出是哪条命令被拦）。
- 覆盖：白名单外命令、灾难/高危命令、白名单命令带写重定向（InsideWrite 旁路口子一并堵死）。
- 语义变化：原「白名单外弹确认，用户误点即执行」→「直接拦截，逃生通道 = 批准切档或用户手动切档」。
- 白名单维持不变（原计划的 pnpm/cargo 等泛化词会放行 `pnpm install`/`cargo build` 本身，已否决）。
- **`gh` 例外：命令名入白名单 + 子命令级二次判定**（`gh_plan_readonly_allowed`）。
  只读形态：`pr view|list|checks|diff|status`、`run view|list`、`release view|list`、`issue/repo/workflow`
  的 `view|list`、`secret/variable/label/cache list`、`auth status`、`config get`、`gist list`、
   `ruleset list|view`、单词 `status`/`search`，以及 `gh api`（只信显式的 `-X get|head`；
   `-X/--method` 写方法（含 `-XPOST`/`--method=DELETE`/引号包裹写法，比对前先归一化）与
   `-f/--field/-F/--raw-field/--input` 请求体参数均判写——gh 有参数且未显式 `-X` 时默认改 POST）。
   其余（`pr merge`、`release create|edit|upload`、`secret set`、`workflow run`、`api -X POST` 等）落回拦截。
   理由：L1-L3 只覆盖文件写/重定向/已知高危命令，**不认识远端写**——一旦整命令放行，plan 档就能合 PR、发版、改 secret。
   保守口径：认不出的形态（裸 `gh`、`gh --version`）一律不放行。
- 分隔符集新增**换行**：`split_unquoted_separators` 把 `\n`/`\r` 当命令分隔符，
  避免 `gh pr view 38\ngh pr merge 38` 被当成一段而绕过子命令门（审查发现的回归）；
  代价是 `\` 续行会被切段而多拦（保守方向）。已知缺口：命令替换/反引号内的 gh 写（`echo $(gh pr merge 38)`）
  不在 L0 覆盖内（既有结构，`echo $(npm i)` 同理），根治需把门下沉到 AST 节点——已在测试里钉住现状。

### 7.2 G2：批准生效点校验（tools/ask.rs + tools/subagent.rs + core/agent.rs）

批准命中（approve/执行）且当前 Plan 档时，依次校验，任一不过**批准不生效**（不切档、不注入）：

1. `E_PLAN_TODOS_REQUIRED`：rt.todos 为空 → 先登记 todos 再重新 ask（G3 前置，Q7=A 仅校验非空）；
2. 例外通道：`lightweight=true`（跳过分析产物校验；todos 非空硬要求不变）；或 `skip_analysis=true` 且**最近一条用户消息命中口令表**
   （不用分析/跳过分析/直接改/无需分析/skip analysis，Q3=A 防模型编造）
3. `E_PLAN_ANALYSIS_REQUIRED`：无分析产物 → 调 pm/tester 后重试；同会话被拒 ≥2 次后错误文本升级提示。

数据源：subagent 成功返回且 role 含 product/tester 且父会话 Plan 档 → 置 `rt.analysis_done`。
澄清类 ask 不受影响（gate 挂批准生效点，非 ask 入口，Q1=A）。

### 7.3 G3：执行范围确认（tools/plan.rs + tools/batch.rs）

- 批准生效时冻结 `rt.approved_plan = todos 标题快照`（从系统上消灭「批准时无基线」状态）；
- 执行档内 plan 全量替换时 `diff_new_todos(baseline, current)`：新增标题 → 置 `rt.scope_expanded`
  （重命名按删+增处理；用户已会话级放行则不置位）；
- batch 写操作前：`scope_expanded && !scope_allowed && 基线存在` → `approval::confirm`（计划外步骤清单 +
  即将执行的写操作）；批准 = `scope_allowed` 会话级放行（后续新增静默纳入）；拒绝 = `E_SCOPE_DENIED`
  （标记保留，下个写再问，Q 决策=确认制与 O2 一致）。
- 子代理不继承基线（无法更新 plan，不存在扩散表达）；重启后内存态丢失 = 退化为上一批次语义，无安全回归。

### 7.4 决策默认值落地（Q1–Q7）

Q1=A 批准生效点校验｜Q2=B todos≤3｜Q3=A 固定口令表｜Q4=白名单不扩充（泛化词有放行风险，见 7.1）｜
Q5=A 仅内存态｜Q6=A 复用 tool 错误展示，零 UI 变更｜Q7=A 仅校验 todos 非空。

### 7.5 新增/变更测试

- fence：`plan_readonly_nonwhitelist_blocks`（原 confirms 用例改 Block）+ `plan_readonly_whitelist_with_writes_blocked` +
  `plan_readonly_block_message_guides`（错误文本含改道指引）；
- ask：gate 五用例（空 todos 拒 / 无分析拒 / 标记放行 / 轻量 todos≤3 / 口令命中校验）；
- plan：`diff_new_todos_finds_additions_only`（保留/删除不置位、新增/重命名检出、空基线）。

### 7.6 审查结论与修复（code-reviewer，第二批次 P5）

🔴 三项均修复：

- R1 `skipAnalysis` 死通道：Args 补 `#[serde(rename_all = "camelCase")]` 对齐 schema 键名（原 serde 默认蛇形，
  模型传 skipAnalysis 被静默丢弃）；新增反序列化回归用例；
- R2 G3 shell 写旁路：范围 gate 扩展到 command 工具——fence 对命令判定为 Confirm（含写目标）时同样弹确认
  （执行档 fence 对区内重定向判 Allow，不扩展则 `echo x > f` 完全绕开）；
- R3 role 子串误命中：改为精确匹配注册名（product-manager/product_manager/product manager/pm/tester）。

🟡 已采纳：Y1 diff 为空时清标记放行（不弹空列表）；Y2 plan 提示词同步 Block 语义；Y3 口令匹配要求分析语义
同现（「直接改」需同现「分析」才认可）；Y4 lightweight 超限分档文案；Y5 重新批准时重置范围状态。
不采纳：Y6 `run:inject` 为上一批次既有行为原样保留（非本次引入），避免事件行为漂移；Y7 batch G3 弹窗路径
依赖用户交互，自动化覆盖成本高，以手工验证清单兜底。

### 7.7 手动验证（GUI）

1. plan 档让模型直接给方案并点「执行方案」→ 批准不生效，错误提示先分析；
2. 说「直接改」后模型带 skipAnalysis 询问 → 批准正常切档；
3. 批准后让模型加一条 todos 外的写步骤 → 出现「计划外步骤确认」弹窗；选允许后续不再问，选拒绝写入被阻；
4. plan 档让模型跑 `pnpm install` → 直接拦截（无确认弹窗），错误文本引导纳入方案。

### 7.8 计划批准即定分支（分支条款增补）

> 需求原文：「面对用户的任何需求和修复，都应该在指定计划时同时拟定分支名称，在向用户提问是否执行计划时一并显示，并在用户同意后使用新分支来进行开发工作」。经澄清：落产品内置行为；agent 批准后自行执行 git 命令（fence 机制兜底）；命名 `<type>/<slug>`（slug ≤24 字符，基线当前 HEAD）；fence 对 branch 写形态的 plan 档缺口独立立项，本批次不动。
>
> 条款落点：`<plan-mode>` P3（方案含分支名，批准询问列明）与批准后段（先 `git switch -c` 再动工）；标准工作流 S4/S5/S6 同条款（[docs/standard-workflow](./standard-workflow.md)）。批准语义 = 预授权创建/切换计划所示分支；commit/push/merge 仍由用户执行。详细决策与边界记录见 [docs/plan-branch-proposal](./plan-branch-proposal.md)。
