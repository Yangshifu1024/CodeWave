# ask 交互加固批次：批准形判定单一事实源 + 选项视觉区分 + 补充说明纳入键盘导航

> 背景（用户报告）：① 询问框选项表现为多选——批准形询问（单题「是否按此方案执行？」+ 执行方案/补充意见两选项）本应单选直提，却落入多选 toggle；② 补充说明输入框独立于选项列表下方，未按预期参与键盘导航；③ 视觉上无法区分多选/单选语义。
> worktree：`feat/ask-approve-shape-note-nav`（CodeWave-wt-ask50），基线 3dd6fc9。

## 一、根因分析

### 1.1 批准形误判为多选（正确性缺陷，非仅 UI）

- 前端 `approvalShape` 判定（AskPanel）：`单题 ∧ 存在 id === "approve" 的选项`——**id 精确等值**；
- 后端批准命中 `approved_hit`（ask.rs）：selections 的 **id 子串**匹配 `approve/执行/Approve`——不看 label；
- ask 工具 description 约定模型用 `id="approve"`/`id="revise"`，但**无任何校验兜底**。实际案例：模型给出的 label 守约（「执行方案」）而 id 自拟（不含 approve/执行 子串）→ 前端落回多选 toggle + 批准项直提失效 + **后端批准静默失效**（切档不发生）。两套启发式分散前后端且判定不一致，是结构性脆弱。

### 1.2 补充说明输入框独立（设计溯源，非回归）

[docs/run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md) 定稿即「编号选项 + 补充回答输入（独立行）」；[docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md) ghost 化（透明底线）。但键盘导航环只在选项数组内循环（`% opts.length`），Tab 从最后选项绕回第一项、永远到不了输入框——「独立但不可达」的体验缺口。

## 二、改动

### 2.1 批准形判定：后端单一事实源（ask.rs）

| 项 | 内容 |
|---|---|
| `is_approve_option(o)` | 宽松识别：id（lowercase）含 `approve` ∨ id 含 `执行` ∨ label 含 **全词** `执行方案/批准方案` ∨ label（lowercase）含 `approve`。label 全词而非「执行」单词，防「执行测试」误判 |
| `approval_shape(qs)` | 单题 ∧ 任一选项命中——**仅驱动前端渲染，无安全语义** |
| `approve_option_id(qs)` | 首个命中项 id（前端直提判定用） |
| `ask:opened` payload | 附加字段 `approval: bool` + `approve_id: Option<String>`（先例 [docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md) `switch_to_auto_edit`；零新增事件键，27 键契约不动） |
| `approved_hit` 扩展 | selections 命中 approve 候选 id 集合（宽松识别收集）∨ 既有 id 子串匹配（保留）——**修复批准静默失效** |
| `arch_gate_shape` 不变 | id==="approve" 精确形状仍是 `switchToAutoEdit` 升档 flag 的安全闸（[docs/arch-orchestrator](./arch-orchestrator.md) Y3）；已知边界：label 守约但 id 非严格形状时 ConfirmEach+flag 不切档（与现状一致），Plan 档批准不受影响（走宽松 approved_hit） |

**前端**：`AskState`/`AskOpenedEvent` += `approval/approveId`（run.ts ask:opened 处理器透传）；AskPanel `approvalShape = ask.approval === true || 严格判定兜底`（宽松集 ⊇ 严格集不冲突）、直提条件 `opt.id === approveOptionId`、submitWith 的 approved 判定取并集、hint 按形态切换（单选「回车确认」/ 多选「回车或空格选中」，复用既有两键，i18n 零新增 hint）。

### 2.2 选项视觉区分（AskPanel + app.css）

| 形态 | 指示器 | checked 语义 |
|---|---|---|
| 多选 ask（默认） | `.opt-box.check` 14×14 圆角方框（4px） | `sel.includes(id)`，多项可同时勾选 |
| 批准形 ask | `.opt-box.radio` 正圆 | 唯一选中（radio 互斥） |
| 审批确认（kind=approval） | `.opt-box.radio` | 跟随键盘高亮位（点击即应答，高亮=待确认） |

- 选中视觉：复选框 = accent 墨底 + **底色反转勾线**（CSS border 旋转绘制，不嵌字体字符）；单选框 = accent 边框 + 中心 6px 墨点；全 `--ws-*` token，亮暗自适应；
- 指示器 `pointer-events: none` 纯视觉（整行可点，零嵌套点击）；行 `role="checkbox"/"radio"` + `aria-checked`；
- **recommended「✓」后缀退役 → 「推荐」描边 pill**（`i18n ask.recommended`）——✓ 与勾选视觉歧义必须消解。

### 2.3 补充说明纳入键盘导航环（AskPanel）

- 导航环 `opts.length + 1` 位，末位 = 输入框；Tab/↓ 环回、↑ 回退；数字快选不受影响；
- 输入框位回车/空格 = `noteRef.focus()`（**不提交**，防误触；提交仍走「提交回答」按钮）；
- `Input onFocus` 同步 cursor（鼠标点击/Tab 进入一致）；`.ask-note.kb` 底线浮现强调色（同 focus 视觉，免焦点）；
- 无选项题（schema 允许）：环仅输入框一位（此前完全无键盘导航）；审批形态无输入框，环不变；
- 已知行为：聚焦输入框后 Tab 走浏览器焦点序离开卡片（光标态残留无害，任意点击/按键回环）。

## 三、验证

- 后端：`cargo test` **282 passed / 0 failed**（基线 279 + 新 3：`approve_option_lenient_matching`（含「执行测试」防误伤）/ `approval_shape_lenient_but_gate_strict`（**arch_gate_shape 对 label 守约但 id≠approve 仍 false 安全边界回归**）/ `label_matched_approve_still_switches_plan`（Plan 档 + selections=["execute"] 集成直驱：过 gate → 冻结基线 → 切 AutoEdit → plan_approved））
- 前端：`pnpm --dir ui test` **191 passed**（基线 185 + 新 6：形状标记单选直提与 hint 切换 / 单选互斥 / 多选双勾不互斥 / 键盘环 Tab→kb→回车聚焦→↑回退→环回 / 审批 radio 跟随高亮 / 推荐 pill）；`pnpm --dir ui build` 通过
- 既有 askpanel 16 例零回归（id="approve" 严格形走兜底分支行为不变）；事件契约测试不动即绿

## 四、手动验证清单

1. **批准形（后端标记生效）**：发起一次计划批准询问（模型自拟选项 id）→ 选项前显示**单选框**、hint 为「回车确认」；点「执行方案」免提交直发；选「补充意见」再点「执行方案」→ 旧选中自动取消、直提生效
2. **批准切档**：同上批准后会话切自动编辑档（胶囊变色）——即使选项 id 是自拟英文（如 execute），批准依然生效（此前会静默失效）
3. **多选题**：普通多选询问 → 选项前显示**复选框**、hint 为「回车或空格选中」；勾选两项均保持勾选；提交载荷含两项
4. **键盘环**：Tab 从最后选项 → 补充说明输入框底线浮现；回车聚焦开始输入（不触发提交）；↑ 回退到最后选项；继续 Tab 环回第一项
5. **审批确认**：命令审批三选项显示单选框，checked 跟随高亮移动
6. **推荐标记**：recommended 选项显示「推荐」小 pill，label 无 ✓ 后缀
7. **暗色主题**：勾选态墨底反勾 / 中心墨点对比清晰

## 五、边界与不做

- arch_gate_shape 不宽松化（安全闸）；输入框内回车不提交；ask 工具 schema/description 零改动（宽松匹配已覆盖，不动 prompt 契约）；事件面零新增键
