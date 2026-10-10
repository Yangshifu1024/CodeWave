# 16 · 计划模式（plan 档）强制阶段流程

> 需求：计划模式严格按流程执行——接到需求 → 调用 product-manager 分析 → 输出需求分析 → 基于分析编写技术方案与计划 → 用户确认后执行。
> 本文档 = 需求分析全文 + 流程定义 + 决策记录，是 plan 档流程的持久契约；`AGENTS.md` 专门代理小节与 core `<plan-mode>` 提示词均引用此文档。

## §1 需求背景与问题定义

### 1.1 现状

plan 档的硬约束只有三件套（`core/agent.rs`）：

1. 工具排除：`edit/create/delete/service/scheduled_task` + MCP 不可用；
2. shell 走 fence 只读白名单（`plan_readonly`），白名单外一律 Confirm；
3. ask 批准协议：方案 ask 选「以自动编辑档执行」→ 会话切执行档并注入执行指令（后续批次扩为**四选项**：两个批准类选项各带 `mode` 声明目标档位 + 无 mode 的「补充意见」+ 无 mode 的「先看预览」，切档目标不再硬编码；批准类判定 = `mode.is_some()`，见 [docs/mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md) 与 [docs/preview-skill](./preview-skill.md)）。**「先看预览」既不批准也不驳回**（渲染预览后重发同一询问，不切档、不计入修订轮次）。

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
- **US3 批准与执行**：选「以自动编辑档执行」（或「以完全访问档执行」）后严格按已确认 todos 执行，完成后按范围决定是否强制 code-reviewer。AC：执行完成后多文件/跨层变更必须出现 reviewer 调用。
- **US4 轻量路径**：微小改动不强制全流程。AC：走轻量路径时在回复中明示；批准 ask 不省略。
- **US5 修订收敛**：选「补充意见」后逐条回应（采纳/不采纳+理由），修订基于上一版做 diff 式更新，不重做需求分析（除非推翻需求前提）。

## §3 流程定义（P0–P6）

### 3.1 需求类主流程

| 阶段 | 产物 | gate（进入下一阶段条件） | 批准者 |
|---|---|---|---|
| P0 接收与分类 | 分类标签：需求/缺陷/问答/混合 + 一句话依据 | 分类结论已声明 | 系统；用户可纠正重路由 |
| P1 澄清（可选） | 澄清问题（ask 或行内提问） | 关键歧义消除，或声明「基于假设推进」并列出假设 | 用户 |
| P2 需求分析 | pm 子代理报告：背景/用户故事/AC/边界/非目标/开放问题 | 报告结构完整；开放问题回流 P1（≤2 轮，超限逐条决策 ask） | 无用户 gate（决策 O3=B） |
| P3 技术方案与计划 | 技术方案（文件级改动点/风险/回滚；git 仓库内拟定分支名 `<type>/<slug>`，slug ≤24 字符、基线当前 HEAD，非 git 仓库注明跳过）+ plan todos 登记（含验证项）+ 验证方式 | todos 非空且含验证项 → 才允许发起批准 ask；批准询问题干与 plan 文本列明分支名 | 用户（ask **四选项**：以自动编辑档执行 / 以完全访问档执行 / 补充意见 / **先看预览**——前两项各带 `mode` 声明目标档位，批准类判定 = `mode.is_some()`；预览项不带 mode，语义是「不批准也不驳回」，渲染后重发同一询问。批准 = 用户在两档中选一（自动编辑 / 完全访问）+ 预授权创建并切换该分支；**预览项必须在结构化通道 / 宽松批准匹配 / 轻量切档三处都被剔除**，否则「前端切档、后端不切」，[docs/mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)、[docs/preview-skill](./preview-skill.md)） |
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
- 分隔符集新增**换行**，并在切分前**归一化行继续**：`split_unquoted_separators` 把 `\n`/`\r` 当命令分隔符，
  防止 `gh pr view 38\ngh pr merge 38` 被当成一段而绕过子命令门（审查发现的回归）；
  同一循环里先把 `\` + 换行（LF/CRLF）吃掉（引号感知：双引号内也移除、单引号内是字面量），
  以免 `grep foo \` + 换行 + `  file.rs` 这类合法只读命令被切段而多拦。
  `\\`（转义反斜杠）整体吞掉，保证其后的换行仍是分隔符（防拼接绕过）。
  残留：PowerShell 反引号续行未归一（按字符无法区分 bash 反引号命令替换）。
  已知缺口：命令替换/反引号内的 gh 写（`echo $(gh pr merge 38)`）
  不在 L0 覆盖内（既有结构，`echo $(npm i)` 同理），根治需把门下沉到 AST 节点——已在测试里钉住现状。

### 7.2 G2：批准生效点校验（tools/ask.rs + tools/subagent.rs + core/agent.rs）

批准命中（批准类选项 = 携带 `mode` 的选项；旧的 id/label 宽松子串匹配作为兼容兜底保留，**且先剔除预览项 id**）且当前 Plan 档时，依次校验，任一不过**批准不生效**（不切档、不注入）：

1. `E_PLAN_TODOS_REQUIRED`：rt.todos 为空 → 先登记 todos 再重新 ask（G3 前置，Q7=A 仅校验非空）；
2. 例外通道：`lightweight=true`（跳过分析产物校验；todos 非空硬要求不变）；或 `skip_analysis=true` 且**最近一条用户消息命中口令表**
   （不用分析/跳过分析/直接改/无需分析/skip analysis，Q3=A 防模型编造）
3. `E_PLAN_ANALYSIS_REQUIRED`：无分析产物 → 调 pm/tester 后重试；同会话被拒 ≥2 次后错误文本升级提示。

数据源：subagent 成功返回且 role 含 product/tester 且父会话 Plan 档 → 置 `rt.analysis_done`。
澄清类 ask 不受影响（gate 挂批准生效点，非 ask 入口，Q1=A）。

### 7.3 G3：执行范围确认（tools/plan.rs + tools/batch.rs）

> 本节已于 2026-09-25 修订（`fix/g3-scope-confirm-noise`）：批准语义由「会话级永久放行」改为
> **并入基线**，并新增判定口径放宽（归一化 + 细化识别）、并发单飞、拒绝本轮消费。
> 详见 §7.9。

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

### 7.9 G3 范围门降噪（`fix/g3-scope-confirm-noise`，2026-09-25）

> 起因：用户反馈「计划外步骤确认」卡反复弹出。排查出四层缺陷叠加，**均为误报而非真实偏离**。

#### 7.9.1 判定口径：归一化 + 细化识别（`tools/plan.rs`）

原口径 `diff_new_todos` 逐字比对冻结基线。但系统提示词自己在改写标题：S4 要求登记「文件级改动点」，
S6 又要求把范围切成人名任务包并补验证项（`core/prompt.rs`）。于是批准基线与执行期标题**语义相同、
字面不同**，每次完整流水线都全量误判。

新增 `normalize_title` + `classify_new_todos`（返回 `New` / `Refinement`），`diff_new_todos` 保留不动：

- 归一化只做**剥装饰壳**（序号前缀、`包 X（role）：` 前缀、中英文标点→空格、空白合并），
  **不做语义相似度匹配**（否则真新增会被放过）；
- 子串方向单向：细化标题 ⊆ 基线条目。`新增 core/x.rs + 单测` → `新增 core/x.rs` 判细化；
  反向（`+ 单测` ⊄ `新增 core/x.rs`）仍判真新增；
- 子串分支有**长度下限 8 字符**（按字符而非字节——中文占 3 字节，按字节算时 `顺手删库`
  （4 字）就有 12 字节直接越过，下限形同虚设；按字符计中英一视同仁）：
  `单测`（2）/ `verify`（6）/ `顺手删库`（4）不再因为是基线子串而免问。
  完整路径 `core/x.rs`（9 字符）仍算细化——「指出哪个文件」是细化最强的证据信号；
- 保留有语义符号：`+`（`cargo test`）、`.`（`core/x.rs`、`v1.2`）、
  `/ - _ @ #`。剥 `.` 会把 `wait_targets.rs` 变成 `wait_targetsrs`，两侧错位后子串判据彻底失效；
- 不误伤：`-1: 负数兼容`（`-` 后不跟分隔符）、`2024 年度统计`（数字后非分隔符）、
  `v2: 修 schema` / `2024: 年度计划`（版本号形态例外于 label 判据）、`1: 步骤`（纯数字不是 label）。

只有出现 `New` 才置 `scope_expanded`；全是 `Refinement` 时静默纳入，但留 `session_log` info
（「范围是安全门，误报可以放过、误放不能无声无息」）。日志如实写「已识别」而非「已并入」——
真正写入基线的动作在 `batch.rs` 的写通道门里，若本 run 无写操作命中写通道则尚未并入。

> **两轮审查逼出来的放行漏洞（均已实测复现后修掉）**：
>
> 第一轮：`looks_like_label` 只「数括号个数」+「以包开头」，**不检查被剥段里有没有正文**——
> `把 API key 硬编码进源码 (顺手)：改 core/x.rs` 剥完剩 `改 core/x.rs` 命中基线 → 判细化 →
> **静默放行且无弹窗**（6 条实测反例）。
>
> 第二轮：括号口堵住了，但「ASCII 短标识」与「包序号」两个口同样不查正文——
> `修 p0 崩溃：更新 README`（判据 C：head 含字母+数字即算标签）、
> `包 A 顺手删库：更新 README`（判据 B：包字头 + 真序号后面还跟中文尾巴）同样整段被丢。
> 「中文子动作 + 一个 ASCII 字母数字 + 冒号」是模型写 todo 的极常见形态。
>
> 现统一由 `has_stray_cjk_outside` 兜底：**标签只允许「包/任务包 字头 + 包序号 + 可选括号角色」
> 一种结构，包字头与括号之外不得携带任何汉字**。不能写成「含 CJK 即否」——真实标签
> `包 A（backend-dev）：` 本身就含「包」字，那样会把本次要修的降噪一起砍掉。
>
> 括号内容另要过 5 道硬条件（非空 / ≤24 字符 / 无空白与中文标点 / 无 CJK /
> 含 ASCII 字母数字）+ 角色白名单（与 `agents::builtin()` 同源）或含 `- _ .`；括号必须**真配对**。
>
> 已知取舍：纯英文语义定语 `删除 migration (refactor)：跑测试` 仍会被剥（与 `backend-dev`
> 形态不可区分），是降噪方向的**有意误报**。

#### 7.9.2 批准语义：并入基线而非会话级永久放行（`tools/batch.rs`）

原语义「批准 = `scope_allowed = true`（本会话永久放行）」，而按钮写「仅允许这一次」——**文案与行为相反**，
用户以为放一次，实际交出整场会话的范围门。改为三态对应三个后果：

| 应答 | 语义 | 行为 |
|---|---|---|
| 允许 | 并入本次范围 | `new_titles` **追加**进 `approved_plan`（保序去重，不替换）+ 清 `scope_expanded`，**不置** `scope_allowed` |
| 始终允许本项目 | 会话级永久放行 | 置 `scope_allowed = true` + 清 `scope_expanded` |
| 拒绝 | 收窄范围 | **消费掉**本轮 `scope_expanded`（本 run 内不再重弹）+ `E_SCOPE_DENIED` |

「追加」而非「替换」是关键：替换会让同一批新标题反复摆到用户面前。拒绝不再保留标记——
保留会让下一条写操作立刻重弹同一张卡。

#### 7.9.3 并发单飞（`tools/batch.rs` + `runtime.rs`）

`command` 工具的 `kind()` 是 `ReadOnly`，走 4 并发信号量闸；而 G3 门在 `run_tool` 内逐 call 独立求值，
**无单飞** → 同批 3~4 条带写目标的命令（`git switch -c` / `mkdir` / 重定向）**同时弹 3~4 张几乎一样的卡**。

新增 `rt.scope_gate_lock: tokio::sync::Mutex<()>`。拆两个闭包：

- `g3_write_channel`：本次调用是否走写通道（工具形态 + 入参，无状态，可无锁求值）
  → 纯只读命令**永不排队等锁**；
- `g3_gate`：状态判据（`scope_expanded` / `scope_allowed` / 基线），**一律在锁内求值**
  → 后到的拿到锁后重新求值为 false 直接放行。

持锁顺序 `file_ops` → `scope_gate_lock`（`execute_batch` 的 spawn 体先取并发闸再进 `run_tool`），
全局恒定且无反向路径。代价：写工具的串行闸会占到用户答完 G3 门（与既有「门 1 写入审批」同形态）。

#### 7.9.4 审批语义类别（`safety/approval.rs` + 前端）

范围门与命令审批共用一组按钮，但后果完全不同。新增 `ApprovalKind { Once, Scope }`，
`ask:opened` 下发 `approval_kind`（**只增字段，事件键名不动**）。前端按它选文案：

- `once`（命令审批）：`仅允许这一次` / `始终允许本项目`（原文案，不变——命令的「始终允许」
  确实持久化到项目作用域的 MCP always_allow）
- `scope`（范围门）：label `本会话始终允许` + desc `纳入本次已批准范围` /
  `本会话后续新增都不再询问`

> 范围门的 label 也必须切：后端 `scope_allowed` 是 `SessionRuntime` 内存态、且下次方案批准会重置，
> **作用域是「本会话」不是「本项目」**。desc 改了而 label 留着旧的，是同一类文案失真换个地方犯。

#### 7.9.5 附带清理

- 删除 `SessionRuntime.scope_denials`——全仓只写不读的死字段（仅 store / reset，无读取者）；
- 弹窗正文重写：三个选项各自的真实后果在正文里写清楚。

#### 7.9.6 新增/变更测试

- `plan.rs`：归一化四条（包前缀 / 序号 / 保留符号 / 标点空白）、分类多条（相等 / 子串 / 真新增 /
  去重保序 / 短片段下限）、**6 条放行反例全部断言 `New`**、真实任务包形态仍 `Refinement`、
  label 判据 9 条认可 + 14 条拒绝；
- `batch.rs`：并入基线后同批不重弹、真新增再拦一次、细化标题不弹窗、**并发单飞只弹一张**
  （数 `ask:opened` 张数）、拒绝后本轮不重弹、`always=true` 置 `scope_allowed`、文案契约、
  `approval_kind` 下发值（`scope` 与 `once` 各一条）；
- `approval.rs`：既有 8 处构造点补 `kind`；
- 前端：范围门文案 / 命令审批不被污染 / `approval_kind` wire 映射。

> 既有 `g3_command_redirect_requires_scope_approval` 改为「应答一次拒绝」真正走完弹窗（原断言靠预取消，
> 实际守的是并发闸短路而非 G3 门本体）；`g3_pure_read_command_skips_scope_gate` 补一条断言：
> 纯只读命令**不得消费掉** `scope_expanded`。

#### 7.9.7 安全边界（未放宽的部分）

- 范围门**不删**、不撤销：真跑偏了照样弹；
- 归一化只剥装饰壳，**不做语义相似度匹配**；
- 子串方向单向（细化 ⊆ 基线），不放过「基线是细化标题的超集」的真新增；
- 重启后内存态丢失 = 退化为原语义，无安全回归。

#### 7.9.8 手动验证（GUI）

1. 完整流水线跑一遍（S4 登记文件级 todos → S6 切成 A/B/C 任务包 + 补验证项）→ **不再弹**范围门；
2. 同批并行跑 `git switch -c` / `mkdir` / 重定向 → **只弹 1 张**（原会弹 3~4 张）；
3. 让模型加一条确实无关的写步骤 → **仍弹**（防降噪过头）；
4. 点「允许」→ 同一批范围后续写不再问；再让模型加真新增 → **必须再问一次**；
5. 点「拒绝」→ 模型收窄范围后不再弹，且错误文案指向收窄；
6. 范围门卡的按钮文案是「纳入本次已批准范围 / 本会话始终允许」，命令审批卡仍是「仅允许这一次 /
   始终允许本项目」。

#### 7.9.9 审查结论（code-reviewer 两轮）

第一轮 🔴：label 判据过宽导致真新增静默放行（见 §7.9.1）——返工后又挖出同一漏洞的另外两个入口
（判据 C / 判据 B），最终由 `has_stray_cjk_outside` 统一兜底，共补 13 条反例断言
（括号口 6 条 + 中文子动作 7 条）。

🟡-2（单飞用例可能假通过）经 **mutation check** 验证无效性不成立：注释掉 `scope_gate_lock` 取锁后
该用例确实变红（4 张卡无人应答 → 批次挂起 → 30s 超时），复原后转绿。原用例能抓到锁失效。
（`command` 是 ReadOnly 走 4 并发信号量闸、许可刚好用满，所以四条确实并行到门。）

🟡-1（拒绝后同批已排队写操作会放行）是**有意取舍**而非缺口，已在用例与注释里显式钉住：
被拒的那条已落 `E_SCOPE_DENIED`、模型会收窄范围重试；同批剩余调用若逐个拦下，用户反而要回答
N 次同一问题。真正的越界写入仍有 fence / 写入门 / 下一 run 重新置位兼底。

> 方法论教训（值得记下来）：**每放宽一条判定，都要问「它会把什么误判成同一类」**。
> 本次两轮返工都是同一个漏洞的不同入口——先堵住括号口，短标识口与包序号口又漏；
> 最后靠「不依赖形态枚举、而是先排除不可能的成分」这条兜底才真正闭合。
> 枚举形态（数括号、看字头、含字母数字）都会漏，因为真实标题的形态空间比枚举大。
