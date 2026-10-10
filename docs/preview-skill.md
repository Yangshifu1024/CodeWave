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

计划卡上另加一个独立的「预览」按钮（更省一轮模型往返，但属前端新入口，另议）；预览产物的持久化与版本对比；预览与代码 diff 的联动；导出预览为图片/HTML 文件；自动生成多套视觉风格供挑选。

> 「预览弹框自动弹出」原列于此，已由 §7 交付。

## 7. 自动弹框增强：「先看预览」后 widget 落地即弹（前端信号，后端零改动）

> 本节记录 2026-10-10 追加的增强。解除 §6 中「预览弹框自动弹出」那条非目标。

### 7.1 问题

选「先看预览」后，Agent 会 `render_html` 产出预览 widget，但前端只把它渲染成工具卡，**必须点卡片头部那个很小的「预览」按钮**才打开弹框（见 [html-preview-modal](./html-preview-modal.md)）——卡可能要滚动才看得到，用户明确反馈「很难找到按钮去点击」。

### 7.2 路线选择：run store 一次性信号（**不给 `render_html` 加入参**）

原 §6 曾把自动弹框的建议路线写成「给 `render_html` 增加可选入参并在历史重放时防误开」。实际实现**换成了前端信号**，理由：

| 方案 | 问题 |
|---|---|
| 给 `render_html` 加 `auto_open` 入参 | ① 入参要进 schema，就要与 `tools/registry.rs` 的严格 schema 断言、`collect_unknown_fields` 的未知字段机制全面对齐；② 出参会多一个字段，牵动 `batch.rs` 那条「render_html 大出参必须落 sidecar」的用例；③ **历史重放防误开做不干净**——`render_html` 的入参不进历史、只有出参落盘，恢复态根本无从判断当初是否要自动弹；④ 模型得记得每次都传，漏传就退化成现状 |
| **run store 一次性信号（本实现）** | 全部判断都在**前端本地**，且判据取自用户**当次真实动作**（点了「先看预览」），不是模型自觉 |

信号形状（`ui/src/stores/run.types.ts` 的 `WidgetAutoOpen`，`TabRunState` 上的**可选**字段）：

```ts
// armed = 用户刚选了「先看预览」，等本 run 第一张合格 widget 落地
widgetAutoOpen = { armed: true }
// callKey = 已锁定那张卡，UI 据此自动弹框（弹完 consume 清空）
widgetAutoOpen = { callKey: "<该卡 call_key>" }
```

**两态合一而非两个字段**：armed 关闭与 callKey 落位是同一次写入，天然排除「armed 已关但 callKey 未置」的中间态。

数据流（6 个前端文件，后端零改动）：

```
AskPanel.submitWith 算出 previewOnly → armWidgetAutoOpen(activeKey)
  → render_html 结果到达 → onToolResult 主会话分支判定四条件 → widgetAutoOpen={callKey}
  → ChatMessages 透传 autoOpenCallKey → segments 转发 → ToolCallCard 打开 widgetOpen 并 consume
  → run:done / run:error / run:cancelled 清空信号
```

### 7.3 三条不变量（由「走不同代码路径」保证，非打标记）

判据取自**代码路径差异**，因此无需给 `ToolView` 增任何来源标记字段（沿用本仓 `runMetrics`「只在实时路径累加」的既有惯例）：

1. **历史恢复态永不自动弹** —— `restoreFromMessages` / `applyToolOutcomes` / `backfillToolOutcomes` 都不经过 `onToolResult`。
2. **子代理渲染的 widget 永不自动弹** —— 双保险：`onToolResult` 的 `!t` 早退分支（子代理结果路由进 `subStreams`），且 `SubagentDrawer` 刻意不传那两个 prop。
3. **未 armed 时普通 `render_html` 永不自动弹** —— 用户直接说「画个仪表盘」时行为零变化。

### 7.4 边界规则

- **一次性**：同 run 内仅第一张**合格** widget 自动弹；`armed` 随锁定一并消失（整体替换为 `{callKey}`）。卡头按钮仍在，后续 widget 手动打开。
- **不合格结果不消费信号**：`ok=false` / 无 html / html 为空串 / `{restored:true}` 占位 → 信号继续在本 run 内等下一张。
- **跨轮作废**：三兄弟收尾清空信号，**不跨轮补弹**（含「Agent 收下先看预览却没渲染任何 widget」的情形）。
- **不重弹**：`consumeWidgetAutoOpen` 在卡首次开框即清空 callKey，配合收尾清空，与 [html-preview-modal](./html-preview-modal.md) 点验第 11 条「切 Tab / 切会话不会自动重开」一致，不冲突。
- **乱序保护**：signal 先到、出参后到时，`ToolCallCard` 的 effect 依赖含 `hasWidget`，**空出参时既不弹也不消费**——否则信号会被还没出参的空卡提前吃掉，真 widget 永不弹。
- **弹框不自动关闭**；重发的批准询问在用户关框后于聊天区可见。
- **置位不得顺带切档**：置位代码刻意放在切档闸门 `if (approved || answerEffective)` **之外**——`previewOnly` 会让两者同时为假，塞进去等于永不执行；反过来置位也绝不能碰档位。

### 7.5 改动清单与验证

| 文件 | 改动 |
|---|---|
| `ui/src/stores/run.types.ts` | 新增导出 `WidgetAutoOpen`；`TabRunState` 增**可选**字段 `widgetAutoOpen`（可选是硬约束：约 26 个测试文件逐字构造 `TabRunState`；**不写入 `blank()`**，`ToolView` 零改动） |
| `ui/src/stores/run.ts` | `RunStore` 增 `armWidgetAutoOpen` / `consumeWidgetAutoOpen`（后者**仅 callKey 命中才清**，不匹配静默 no-op）；`onToolResult` 主会话分支内四条件锁定 |
| `ui/src/stores/runHandlers.ts` | `run:done` / `run:error` / `run:cancelled` 三处清空 |
| `ui/src/features/tools/AskPanel.tsx` | `submitWith` 里 `previewOnly` 为真 → 置位（切档闸门之外） |
| `ui/src/features/chat/ChatMessages.tsx` | 取 `autoOpenCallKey` + 稳定 `useCallback` 消费回调，`AssistantMessage`（memo）透传 |
| `ui/src/features/chat/segments.tsx` | `TimelineSegsView` 透传（子代理抽屉不传 = 刻意不弹） |
| `ui/src/features/tools/ToolCallCard.tsx` | 复用既有 `hasWidget` 判据开框并回调消费；`widgetOpen` 仍为卡片本地 state；不直接依赖 store |
| `ui/src/__tests__/runWidgetAutoOpen.test.ts` | 新增 13 个 store 层用例 |
| `ui/src/__tests__/widget.preview.test.tsx` | 新增 5 个自动弹框用例（含乱序与不消费） |
| `ui/src/__tests__/askpanel.test.tsx` | 新增 4 个置位 + 对照 + 不切档回归用例 |

- **后端零改动**：`cargo test` 不受牵连；事件面 29 键、`tool:result` 载荷、i18n、`ToolView`、`render_html` 入参全部不动，也未新增 `features/` 文件（故 `settings.registry.test.ts` 无需登记）。
- 验证：`pnpm --dir ui test` **1325 passed / 108 文件**；`pnpm --dir ui build` 通过（tsc 0 error）；`pnpm --dir ui run lint` **0 error 0 warning**。方案对齐审查零 🔴。

人工点验清单（界面改动不做 GUI 自动点验）：

1. 走到方案批准门 → 点「先看预览」→ Agent 渲染预览后**零点击**即自动弹出大弹框。
2. 关闭弹框 → 聊天区同时可见 widget 卡 + 重发的批准询问；点批准照常切档开工。
3. 连续多张 widget → 仅第一张自动弹，其余靠卡头按钮手动打开。
4. 聊天里直接说「用 render_html 画个仪表盘」（未点先看预览）→ **不**自动弹框。
5. 切 Tab / 切会话回来 → 预览弹框**不**自动重开。
6. 子代理渲染 widget → 不自动弹框。
