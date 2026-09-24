# preview 技能 + 批准门第四选项「先看预览」

> 2026-09-24 · 新增内置技能 `preview`，并在两处方案批准门（plan 档 P3 / 标准工作流 S5）加入「先看预览」选项。与 main 的「显式选档」合并后，它是**第四项**（不带 `mode`），见 §3.5。
> 关联：[html-preview-modal](./html-preview-modal.md)（预览产物的承载面：render_html 大弹框）、[standard-workflow](./standard-workflow.md)、[plan-mode-workflow](./plan-mode-workflow.md)、[slash-skills-and-dollar-agents](./slash-skills-and-dollar-agents.md)。

## 1. 需求

用户在方案批准前只能读文字方案，理解成本高、批准带盲签成分；讨论中临时说「预览一下」也没有对应动作。诉求两点：

1. 用户明确要求预览时，若正处于「方案已成型、准备进入开发」的场景，Agent 应先理解方案再给出可视化预览；
2. 方案批准门的询问里增加一个「先看预览」选项。

## 2. preview 技能（内置）

- 落地形态：**内置技能**（`src-tauri/src/skills/mod.rs::builtin_skills()` 的字符串常量表，与 repo-index/doc-convert/xlsx/docx/pdf 同档）——随应用分发、不可删除（与其它内置技能一致），在系统提示的技能列表里以「名称 + description + when」呈现，用户可用 `/preview` 点名，Agent 也可用 `skill` 工具主动加载。
- 触发场景（写进 `whenToUse`）：① 用户要求预览（预览一下 / 看下效果 / 看看界面 / mock 一下）且上下文已有成型方案或明确讨论对象；② 批准门里用户选了「先看预览」。
- 技能正文要点：
  - **该渲染 / 不该渲染**：无成型对象（纯问答、闲聊、方案未成形）先澄清，不硬渲；方案仍在讨论中且用户没要求时不擅自打断。
  - **形态选择**：默认 A = 方案概览（目标 / 范围 / 关键流程或数据流 / 风险与回滚 / 验证方式）；需求以界面交互为主体时升 B = 界面示意，并**强制标注「示意稿 · 未实现」**；两者同卡分区。
  - **渲染纪律**：自包含（无外链 CSS/JS/字体、无 fetch——沙箱无网络无同源）；单次 ≤50000 字符，超限精简或拆成多次 `render_html`，不得因超限放弃；只呈现方案，不改方案内容、不写业务代码、不落盘。
  - **产出之后**：一句话说明；若处于批准门则**重新发起同一批准询问**（看预览 ≠ 批准，绝不静默批准）；同一方案连续 3 次预览后重发时去掉预览项；`render_html` 报错则如实说明 + 用聊天内 markdown 概览降级，**批准门照常存在**。
- 预览产物走既有 `render_html` 工具卡承载（一键打开 90vw × 85vh 大弹框，见 [html-preview-modal](./html-preview-modal.md)）——两件事同批交付才有意义：没有大弹框，预览又回到那个 150px 高的内嵌 iframe。

## 3. 批准门第四选项（与「显式选档」融合）

### 3.1 三处提示词联动（易漏）

| 位置 | 改动 |
|---|---|
| `src-tauri/src/core/prompt.rs` 的 `WORKFLOW_SECTION`（S5 批准门） | 选项增为四个：`approve`（以自动编辑档执行，带 `mode`）/ `approve_full`（以完全访问档执行，带 `mode`）/ `revise`（补充意见）/ `preview`（先看预览，**不带 mode**）；写明选 preview = 不批准也不驳回：先加载 preview 技能渲染预览，再重发同一询问（不计入修订轮次）。`WORKFLOW_SECTION` 有常驻层成本断言，预算 2000 → 2100 并补理由注释 |
| `src-tauri/src/core/agent/drive.rs` 的 `<plan-mode>`（P3 批准门） | 同口径加「先看预览」（选项 id 为 `preview`）与「看预览不是批准，不计入修订轮次，绝不静默批准」 |
| `src-tauri/src/tools/ask/tool.rs` 的 ask description / `single` 描述 | 协议描述由「两个选项」改为三个选项并说明 preview 语义；`single` 描述由「仅用于二选一」放宽为「互斥选项（如批准门三选一）」 |

### 3.2 切档安全（本批最关键的一处）

批准门的选项集合会驱动**权限档切换**，而预览选项必须**既不批准、也不算有效应答**：

- `is_approve_option` 是宽松匹配（id 含 `approve`/`执行`，或 label 含 `执行方案`/`批准方案`/`approve`）。故 preview 选项固定 `id="preview"`、label 用「先看预览」/「Preview first」——**不得**含上述字样，否则会被当成批准项（静默批准 + `plan_approved`）。
- 切档判定 `wants_mode_switch`：Plan 档看批准命中（preview 天然不命中）；**ConfirmEach 档是 `arch 闸标记 || 有效应答`**。这里有个真实陷阱：标准工作流批准门带 `switchToAutoEdit=true`，形状又是标准批准闸（单题 + 含 `approve`）→ **`arch_flag` 单独就能触发切换**，只排除「有效应答」这一条通道是不够的：用户点「先看预览」照样会被切到自动编辑档并按方案开工。
  因此新增 `preview_option_ids` / `preview_only`（仅在批准形询问里识别），并在调用点用 `!preview_only_answer` **同时封掉两条通道**：
  ```rust
  let preview_only_answer = preview_only(&args.questions, &answer);
  let valid_answer = has_valid_answer(&args.questions, &answer) && !preview_only_answer;
  let arch_flag = arch_gate_shape(&args.questions) && args.switch_to_autoedit.unwrap_or(false);
  let switch = !preview_only_answer && wants_mode_switch(mode_at_open, approved, arch_flag, valid_answer);
  ```
  `has_valid_answer` 的通用语义**不改**（它另有用途，改它会外溢）；「只看预览」= 批准形询问里选中的项全是预览项（至少一个）——**补充说明不算表态**（决定意图的是选中的选项；早先把 note 非空判成「不是只看预览」，会让「预览 + 备注」落回有效应答并在 ConfirmEach 批准门里静默批准，已修）。
- 识别与剔除：预览项识别为 **id 优先（大小写不敏感）+ label 兜底**（`先看预览` / `Preview first`）——模型自拟 id 时（本仓有先例）仍认得出；预览项同时从**批准候选**里剔除（`approved_hit` 的 id 集合与下发给前端的 `approve_option_id` 都剔）——它的 label 万一含「执行方案」，宽松批准匹配会命中并造成**前端单边改档**（后端因 preview_only 不切，两侧分叉）。
- 前端同步（`ui/src/features/tools/AskPanel.tsx`）：`submitWith` 的 `anyAnswer`（ConfirmEach 下触发胶囊切档）排除「预览-only」；`pickAndSubmitIfApprove` 扩展为「批准项或预览项点击即提交」——四选项的批准门里，让用户为「先看预览」再点一次「提交回答」纯属多余。

### 3.3 交互语义

- 选「先看预览」→ 渲染预览 → **同回合重发同一批准询问**（题干加一句「预览已更新，是否按计划执行？」）；预览不计入「驳回修订 ≤2 轮」。
- 同一方案连续 3 次预览后，重发询问时去掉预览项并说明原因（防来回空转与 token 浪费）。
- 两处批准门（plan 档 P3 / 标准工作流 S5）口径一致都带预览项。
- 重发询问会按既有机制再落一个 `plan-<时间戳>.md`（`save_plan_file` 对所有 ask 执行，随会话清理）——可接受，不额外处理。
- 已知边界：内置技能优先级最低，用户/项目若存在同名 `preview` 技能会**静默覆盖**内置版（既有机制，本批不改；本机已确认 `~/.codewave/skills` 为空、用户级仅有 `grilling`）。

### 3.4 相邻修复：完整批准路径需「批准项」命中（协议收紧）

排查本轮切档安全时暴露的**既有**问题：完整批准路径的条件原本是 `switch && (Plan || arch_flag)`，而 **ConfirmEach 档的 arch 闸标记（`arch_gate_shape && switchToAutoEdit`）本身就能满足它**。于是在完整流水线批准门（带 `switchToAutoEdit=true` 的标准批准闸形状）里，用户选「补充意见」也会：冻结 todos 基线 → 切自动编辑档 → 注入「方案已批准：请立即按方案执行」→ 返回 `plan_approved: true`。用户要的是改方案，模型收到的却是开工指令。

收紧为 `switch && (Plan || (arch_flag && approved)) && (!gate_shape || approved)`，其中 `gate_shape = arch_gate_shape(questions)`（单题 + 含 `id="approve"`）：**在批准闸上只有真的选中批准项才动档位**——选「补充意见」既不走完整路径（无基线冻结、无注入、无 `plan_approved`），也不走轻量切档（档位保持不变）。

非闸形状的普通 ConfirmEach 询问**语义不变**（任一有效应答 = 放开：只切档 + 记审计日志）；plan 档协议（P3）与 G2/G3 硬门也未改动。前端 `AskPanel` 的 `lightPath` 同步带上同一个 `gateShape` 条件，避免前端把胶囊抬到自动编辑档而后端没动（两侧分叉）。

回归：`revise_option_does_not_switch_mode_in_confirm_each_gate`（闸上选「补充意见」→ 无 `plan_approved`、无「方案已批准」、基线未冻结、档位保持 ConfirmEach）、`non_gate_ask_still_light_switches_in_confirm_each`（非闸形状询问仍轻量切档）、`approve_option_still_switches_mode_in_confirm_each_gate`（闸上选批准仍走完整路径）。

### 3.5 与「显式选档」的融合（main #77）

`preview` 与本仓 main 上的**显式选档**（[mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)：批准类选项带 `mode` 声明目标档位）是同一份批准门协议的两条并行改造线。合并时按「保留显式选档结构 + preview 作为**不带 mode 的第四项**」融合：

| 选项 id | `mode` | 语义 |
|---|---|---|
| `approve` | `auto_edit` | 批准 + 切到自动编辑档（recommended） |
| `approve_full` | `full_access` | 批准 + 切到完全访问档 |
| `revise` | 无 | 不批准、不切档 |
| `preview` | 无 | 不批准也不驳回（先看预览，再重发同一询问） |

**三处都必须排除预览项**（少一处就是「前端切档、后端不切」的单边静默提权，已各有用例钉死）：

1. **结构化通道**：预览项不带 `mode` → `selected_target_mode` 天然返回 `None`；若模型违约给预览项挂 `mode`，前端 `planPath` / `modePath` 也各自乘了 `!previewOnly`；
2. **宽松批准匹配**：`approved_hit`（后端，按选中项 id 子串匹配）与前端 `AskPanel` 的同款匹配都要**先剔除预览项 id**——模型自拟 id 含 `approve` 时（如 `preview_approve`）否则会被算成批准。后端 `approve_ids` 早已剔除，漏的是宽松兜底那条；
3. **轻量切档**：`valid_answer` 与 `switch` 都叠加 `!preview_only_answer`。

**反面纪律**：`approval_shape`（宽松的「批准形」渲染标记，注释明写「无安全语义」）**不得**收紧成剔除预览项——`preview_option_ids` 以它为前置条件，收紧会让「预览项是唯一批准样选项」的形态认不出预览项，反而打开 `valid_answer` 通道把档位静默抬到自动编辑档。有用例注释钉死这条理由。

回归：四选项门下选 `preview` 在「逐项确认档 / 计划档」两个起点都不批准、不切档、不冻结基线；id 自拟为 `preview_approve`、label 为「先看预览」时同样封住（多选载荷亦封）；对照：选 `approve` / `approve_full` 仍按所选档位切换、选 `revise` 不切档。

## 4. 改动清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/skills/mod.rs` | 新增内置技能 `preview`（frontmatter + 正文） |
| `src-tauri/src/core/prompt.rs` | S5 批准门四选项与语义；常驻层预算 2000 → 2100 + 注释 |
| `src-tauri/src/core/agent/drive.rs` | plan 档 P3 批准门同口径 |
| `src-tauri/src/tools/ask/tool.rs` | `preview_option_ids` / `preview_only`；切档判定排除预览-only；批准闸形状收紧（`gate_shape` 下仅批准项动档位，§3.4）；ask 描述措辞 |
| `src-tauri/src/tools/ask/tests.rs` | 新增 10 个用例（预览项非批准项 / preview-only 识别 / ConfirmEach 选预览不切档 / Plan 档选预览不批准 / 预览+备注不切档 / 批准候选剔除 / id 大小写·label 兜底 / 闸上选补充意见不切档 / 非闸形状仍轻量切档 / 对照：选批准仍切档） |
| `ui/src/features/tools/AskPanel.tsx` | 预览项点击即提交；预览-only 不触发胶囊切档 |
| `ui/src/__tests__/askpanel.test.tsx` | 新增 3 个用例（选预览即直提且不切档 / 预览+备注不切档 / 对照：选批准仍直提并切档） |

## 5. 验证

- `cargo test`（`src-tauri/`）：**1020 passed / 0 failed / 3 ignored**（含 `builtin_skills_parse` 对新技能 frontmatter/whenToUse 的校验、`WORKFLOW_SECTION` 预算断言、新增 10 个 ask 门用例；恢复链路的新增用例见 [session-restore-fidelity](./session-restore-fidelity.md)）
- `pnpm --dir ui test`：**1082 passed / 94 文件**（含新增 ask 门用例 5 个）

人工点验清单：

1. 任意会话发 `/preview` → 技能被点名加载（技能列表里可见 `preview`，来源为内置）。
2. 走到方案批准门 → 询问里出现三项：批准开发 / 补充意见 / **先看预览**；键盘 Tab 可移到第三项。
3. 点「先看预览」→ 询问立刻提交、**不切档**（顶栏权限胶囊仍是原档位）→ Agent 渲染预览（工具卡出现，点开是大弹框）→ 同回合重新发起同一批准询问。
4. 在新询问里点「批准开发」→ 照常切到自动编辑档并开工（守门没把正常批准路径改坏）。
5. 连续 3 次选「先看预览」→ 第 4 次询问里不再有预览项并给出说明。
6. 讨论中说「预览一下」（此时有成型方案）→ Agent 先加载 preview 技能再渲染；在没有可预览对象时说「预览一下」→ Agent 先澄清预览对象而不是硬渲。
7. 方案里含界面改动时，预览里出现界面示意且显著标注「示意稿 · 未实现」。

## 6. 非目标

计划卡上另加一个独立的「预览」按钮（更省一轮模型往返，但属前端新入口，另议）；预览产物的持久化与版本对比；预览与代码 diff 的联动；导出预览为图片/HTML 文件；自动生成多套视觉风格供挑选；预览弹框自动弹出（可选增强）。
