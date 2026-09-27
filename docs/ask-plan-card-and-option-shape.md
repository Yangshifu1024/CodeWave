# 35 · 计划卡与选项形态：真实方案 + 互斥语义贴合

> 缺陷修复批次（与 [run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md) / [notification-click-reveal](./notification-click-reveal.md) 互补）：
> ① 计划卡与「查看完整计划」弹窗只显示短题干、不是真正的方案；② ask 选项一会儿被画成 radio 一会儿被画成 checkbox，
> 不贴合语义。

## 一、缺陷现象

### 缺陷 A · 计划卡显示的不是真正方案

用户实测（2026-09-27 截图 + 落盘文件 `.codewave/tasks/plan-20260927-053537.012-8e8d.md`）：

- 聊天区出现「📅 计划」卡片，正文只有一行：`我把方案落到哪种粒度？`
- 点「查看完整计划 →」弹窗内容只有 `# 计划` + 这一行题干。
- 而该轮模型实际输出了约 6.7k 字的方案正文。

历史 `.codewave/tasks/` 下 62 个 `plan-*.md` 中有 28 个 < 300 字节（最小 42 字节），说明这个缺陷**长期、成规模存在**。

### 缺陷 B · 选项单/多选判据贴合语义

同一类语义问题（如「我把方案落到哪种粒度？」四选项本应互斥）一会儿被画成 radio、一会儿被画成 checkbox，不贴合用户预期。

## 二、根因

### 根因 A · 计划卡与 plan 字段契约错位

1. **后端 `src-tauri/src/tools/ask/tool.rs`** 把落盘泛化到**所有** ask；内容源 `plan_body` 在显式 `plan` 字段为空时**回退成题干拼接**。
2. `plan` 参数 schema 说明写死「plan 档**批准形**询问必须携带」→ 模型据此在普通讨论型 ask 不传 → 落盘内容只剩题干。
3. 前端 `ui/src/features/tools/AskPanel.tsx` 只要后端下发了 `plan_file` 就渲染计划卡；卡片正文是**前端自己拼的题干**，与后端落盘文件同源且同病。

### 根因 B · 选项互斥仅看 `single` 字段

渲染优先级：`approvalShape`（批准形）> `cur.single === true` > 多选默认。模型对 `single` 声明不稳定（甚至缺省），导致同语义问题形态不一致；`toggle()` 也只看 `q?.single`，**渲染与行为分裂**——用户看到 radio 但点两下还能选两个。

## 三、修复方案

### 修复 A · 计划卡内容源优先级 + 无真实方案不渲染

**内容源优先级**（任一不通过即视整体无效，向下走）：

1. 显式 `plan` 字段（trim 后非空即采信）——既有的批准形契约
2. 本轮 assistant 正文兜底——仅当正文通过有效性判据才采信：
   - 归一化（trim + 连续空白折叠）后与题干拼接**不相等**
   - 长度**不短于**题干拼接长度
   - 正文**不**含 `<ask` / `</ask`（防文本形态 ask 兜底路径里 `<ask>` 示例残留）
3. 仍拿不到真方案 → **不落盘也不渲染计划卡**（消除空壳）

正文兜底读数路径：ask 工具运行时 `rt.history` 尾部即当轮 assistant 消息——`core/agent/drive.rs` 在 `run_tool_batch` 之前把 assistant 消息 push 进历史。**子代理约束**：一律读 `ctx.rt` 自己的 history（将来子代理若放开 ask，绝不能误读根会话的正文）。

**事件契约**：

| 事件 / 类型 | 字段 | 类型 | 语义 |
|---|---|---|---|
| `ask:opened` | `plan_file` | `string \| null` | 后端计划文件路径；**无真实方案时为 null** |
| `ask:opened`（新增） | `plan_body` | `string`（None 不下传） | 真实方案正文（与 `plan_file` 同源）；前端用它渲染卡片正文与「复制计划全文」 |
| `AskState`（前端） | `planBody` | `string \| null` | 对应 `plan_body`；空则不渲染计划卡 |

事件键名**不增不改**，`events.contract.test.ts` 只守键名，`plan_body` 是新增可选字段，旧载荷仍可解析（旧值等价于「无方案」）。

### 修复 B · 选项互斥语义推断

**形态优先级**：

1. 批准形（`approvalShape`，最高优先级，与切档/直提协议联动）
2. 显式 `single: true`（模型声明 radio）
3. 显式 `single: false`（模型声明强制多选，覆盖启发式）
4. 启发式推断（`cur.single` 缺省时）：
   - 选项 id 含 `mutually_exclusive` / `single_choice`（大小写不敏感）→ radio
   - 选项 label 含「只能」/「单选」/「要么…要么…」（后者成对出现）→ radio
   - 题干以「选一个」/「哪一种」/「走哪种」/「选中 X」/「是否按…执行」/「选哪个」收尾 → radio
   - 选项数 ≤ 4 且**无任何**选项 label 含「同时 / 可多 / 多选 / 哪些」→ radio
   - 其余 → checkbox（多选默认）
5. 多选默认兜底

**关键纪律**：`toggle()` 与渲染分支复用同一份 `inferSingle(q)`，**禁止渲染 / 行为分裂**——「看着 radio 实际能选两个」是新的反直觉缺陷。

工具描述同时增「选项互斥语义」契约段，强互斥场景（粒度 / 方向 / 档位选择）请显式 `single: true` 以避免启发式误判。

## 四、文件级改动清单

| 层 | 文件 | 动作 |
|---|---|---|
| BE | `src-tauri/src/tools/ask/tool.rs` | 新增 `current_turn_text` / `usable_plan_body` / `is_usable_turn_body`；删旧 `plan_body`；改写 `run()` 落盘分支；`ask_opened` 增字段 `plan_body`；工具 `description` 与 `plan` 参数说明改写 |
| BE | `src-tauri/src/tools/ask/tests.rs` | 改写 `plan_body_falls_back_to_questions_when_plan_blank` 为新语义；新增 4 条 `usable_plan_body` / `is_usable_turn_body` 用例 |
| FE | `ui/src/ipc/types.ts` | `AskOpenedEvent` 增可选 `plan_body?: string` |
| FE | `ui/src/stores/run.types.ts` | `AskState` 增 `planBody?: string \| null` |
| FE | `ui/src/stores/runHandlers.ts` | `ask:opened` 映射 `planBody: p.plan_body ?? null` |
| FE | `ui/src/features/tools/AskPanel.tsx` | 计划卡正文改用 `ask.planBody`；删前端「题干拼方案」；`isPlan` 改为「有 planBody」；选项渲染 / `toggle()` 共用 `inferSingle` |
| FE | `ui/src/features/tools/askShape.ts`（新） | `inferSingle(q)` 纯函数 |
| FE | `ui/src/__tests__/askpanel.test.tsx` | 既有 `planFile` seed 补 `planBody`；新增「卡片正文来自 planBody」「无 planFile/planBody 无卡片」「互斥推断」三条 |
| FE | `ui/src/__tests__/run.ask-mode.test.ts` | 既有 `plan_file` 映射用例补 `plan_body` 断言 |
| DOC | `docs/0-README.md` | 在「ask / 审批交互」组登记本文件 |
| DOC | `docs/run-queue-and-ask-revamp.md` | 「计划卡与计划文件」一节加一句指向本文件（不重写历史结论） |
| DOC | `docs/notification-click-reveal.md` | 同上 |

## 五、回归要点

- 批准门 ask 的计划文件必须是真方案（显式 `plan` 优先）。
- 无 `plan_body` / `plan_file` 时前端无卡片、无「查看完整计划」按钮、不崩。
- 显式 `single: true` / `single: false` / 批准形三处优先级保持与本批一致（不倒退）。
- `toggle()` 与渲染共用 `inferSingle`，不允许分裂。
- 计划文件归属、清理链路、`ArtifactKind::Plan` 行为零变化。
- 文本形态 ask 兜底（`<ask>` XML 救回路径）不被本批落盘语义污染——含 `<ask` 的正文不采信。

## 六、风险与回滚

| 风险 | 处置 |
|---|---|
| 正文兜底把「闲聊 / 过程叙述」当成方案 | 三重判据过滤；不过 → 不落盘（正常分支、不打 warn） |
| `plan_body` 字段兼容旧载荷 | 可选字段（`skip_serializing_if` + 前端 `?? null`），旧值等价「无方案」→ 不渲染 |
| 启发式误判把多选题画成 radio | 优先级「显式 `single=false` > 启发式」；测试覆盖典型题面 |
| 启发式漂移导致旧 seed 形状变化 | 既有 seed 多数声明了 `single` 或命中关键词，启发式兜底不命中，无回归 |
| 清理链路 | 不动 `register_plan_artifact` / `cleanup.rs`；不落盘则不登记边车 |

回滚：单 commit 内 2 BE + 4 FE + 1 新 FE，`git revert` 完整回退；计划文件与边车格式无迁移。

## 七、验证方式

- 后端：`cd src-tauri && cargo test --lib tools::ask::`（42 passed / 0 failed，含 4 条新增）；全量 `cargo test --workspace` 全绿。
- 前端：`pnpm --dir ui test`（askpanel 74/74 + run.ask-mode 2/2 + run.text-ask-fallback 7/7）。
- 构建：`pnpm --dir ui build` 通过。
- 审查：code-reviewer 对照本方案审查（🔴 必须修）。
- 手动验证清单：见 §八。

## 八、手动验证清单

| 场景 | 预期 |
|---|---|
| 批准门 ask，模型携带显式 `plan` 字段 | 计划卡正文 = `plan` 内容；「查看完整计划」弹窗 = 同源 |
| 批准门 ask，模型未携带 `plan` 但本轮正文 > 6.7k 字 | 计划卡正文 = 正文；「查看完整计划」弹窗 = 同源 |
| 普通讨论型 ask，模型未携带 `plan`、本轮正文只是题干复读 | **不渲染计划卡、无「查看完整计划」按钮** |
| 普通讨论型 ask，正文里含 `<ask>` 示例 | **不渲染计划卡**（避免示例 XML 出现在卡片里） |
| 「我把方案落到哪种粒度？」四选项无 `single` | radio（启发式） |
| 「请勾选所有适用的特性」三选项无 `single` | checkbox（启发式：选项多 + 题干含「勾选所有」） |
| 显式 `single: false` 的题 | checkbox（覆盖启发式） |
| radio 形态下连续点两个不同选项 | 仅最后一个保留（toggle 与渲染一致） |

## 九、相关文档

- [run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md) — 计划卡的诞生背景与旧版实现
- [notification-click-reveal](./notification-click-reveal.md) — 通知点击回跳（与本批无直接耦合，但落盘文件名格式同源）
- [ask-approval-shape-note-nav](./ask-approval-shape-note-nav.md) — 批准形判定单一事实源；本批沿用其 `approvalShape` 与 `gateShape` 优先级
- [ask-unified-plan-card-and-answer-switch](./ask-unified-plan-card-and-answer-switch.md) — 所有 ask 落盘计划文件的旧批；本批是该方案的「真实方案源」补丁
