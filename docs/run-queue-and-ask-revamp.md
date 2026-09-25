# 运行队列 + 询问窗口重构（命令白名单 + 计划卡片）

> 需求背景（用户参考截图）：① AI 任务执行过程中允许继续输入并提交新内容，进入队列顺序完成；② 询问/审批窗口按参考样式重构（编号选项 + 键盘导航 + 确认按钮），并新增「始终允许本项目」；③ plan 档方案批准时展示计划卡片，计划自动落盘为文件并提供「查看完整计划」快捷按钮。

## 一、运行队列（纯前端，后端零改动）

- **语义**（用户确认）：运行中在 Composer 输入并提交（含图片附件）→ 进入队列；当前任务 `run:done` 后自动出队执行下一条；`run:error` / `run:cancelled` 后队列**暂停保留**（不出队），由用户「继续执行」恢复；条目「↑ 立即」= **打断当前运行并立即执行该条**（被打断任务不回队列，与手动停止等效），其余条目在该条完成后继续顺序执行。
- **实现**（`ui/src/stores/run.ts` + `features/chat/QueuePanel.tsx`）：
  - `TabRunState` 新增 `queue: QueueItem[]`（`{id, text, images?}`，附件 base64 与 send 参数同构）、`pendingItemId`、`draftFromQueue`；
  - `send(text, images, targetKey?)`：运行中不再拒绝，入队受理；`targetKey` 供出队定向到源会话（后台 Tab 队列完成不抢当前焦点，完成通知机制已有覆盖）；
  - `run:done` handler（既有幂等守卫防 suggest 双发）末尾 `runQueueNext` 自动出队；`run:cancelled` 检查 `pendingItemId`（「立即」路径）出队执行，否则保持暂停；
  - `runNow`（空闲直发/运行中记 pendingItemId + cancel）、`removeQueueItem`、`editQueueItem`（文本回填 Composer 并聚焦，附件不回填）、`consumeDraftFromQueue`；
  - QueuePanel 渲染于 Composer 顶部：条目 = 把手 + 文本（省略+title）+ 📎 附件数 + 「立即/编辑/删除」；暂停态显示提示 + 「继续执行」。运行中 placeholder 切换为「继续输入以排队后续修改」。
  - **拖拽排序**（`queue-item` HTML5 DnD，**仅 grip 按下后启动**）：原生 setPointerCapture + draggable=true，onDragStart 受 `gripArmed` 守卫；不引入新依赖（[fix/queue-panel-width-and-drag]）。原“`draggable={draggingId===q.id}`首改拖死锁”由那场修复拔除，现每条 item 始终 draggable=true。
- **不做独立面板宽度**：`.queue-panel` 边距与 `.composer` 同步为 `0 15% 8px`（与中栏同一公式），输入区与中栏同一容器宽度，窗口越宽越错位的旧问题拔除。
- 队列存于前端 Tab 运行态，应用重启不保留（运行中任务本就不跨重启）。
- 既有 `inject` 通道（运行中「插入当前对话流」语义、仅纯文本）保持不变，与本队列语义不同。

## 二、询问窗口重构（`features/tools/AskPanel.tsx` 重写）

- **approval 形态**：「安全确认」Tag + 标题 + 「⧖ 等待确认」状态行 + detail 等宽命令块 + 编号选项列表（1. 允许——仅允许这一次 / 2. 始终允许本项目——后续相同命令不再询问 / 3. 拒绝——这次先拒绝；选中高亮）+ 底部键位提示 + 「确认」主按钮。
- **ask 形态**：Tag + **分页器 ‹ n/N ›（多问题逐页作答）** + 编号选项（多选 toggle，recommended 带 ✓）+ 补充回答输入 + 「忽略」（清空当前题作答并进下一页）+「提交回答」。既有批准切档副作用（approve 命中 → updatePrefs auto_edit，[docs/arch-orchestrator](./arch-orchestrator.md) R1）原样保留。
- **键盘导航**：卡片 `tabIndex=0` + onKeyDown（↑↓/Tab 移动高亮、数字键快选、approval Enter 确认、ask Enter/Space toggle）；焦点在输入框（INPUT/TEXTAREA）时不拦截；不抢 Composer 焦点，鼠标始终可用。
- **载荷扩展**：`ask:opened` 新增 `allow_always`（仅命令审批 true；文件写入确认/服务命令/范围确认保持 false 不渲染第三选项）与 `plan_file`；`AskState` 加 `allowAlways / planFile`。事件键零新增。

## 三、「始终允许本项目」命令白名单

- **存储**（`core/config.rs`）：`approval.command_allowlist: Vec<String>`（serde default 兼容旧配置）；粒度 = 整条命令 trim 后全文相等匹配（保守可预期）。
- **判定与写入**（`tools/command.rs`）：fence 判 `Confirm` 后先查白名单——命中跳过询问直接放行（**灾难级命令不适用白名单**，仍走确认）；`confirm` 返回 `always=true` 时命令写入白名单并 `ConfigState::save()` 持久化（去重；持久化失败不阻塞本次已批准执行）。
- **approval 接口**（`safety/approval.rs`）：`ApprovalRequest` 加 `allow_always`；`confirm` 返回 `ConfirmOutcome { approved, always }`（batch.rs 两处 / service.rs 一处调用点已适配，均 `allow_always: false`）。
- **设置页管理**（`SettingsModal` 安全分区）：白名单列表展示 + 逐条删除，随保存持久化。

## 四、计划卡片与计划文件（[docs/run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md) 新增）

- **落盘由系统硬保证**：plan 档写入工具不可用（只读，D4），AI 无法自行 create 方案文件——`tools/ask.rs` 在**计划批准形态**（单问题含 `approve` 选项，即 `arch_gate_shape`）的 ask 打开时，把 `questions[0].question`（方案全文，模型协议要求写在 question 字段）写入 `<workspace>/.codewave/tasks/plan-<UTC时间戳>.md`（`resolve_write` 根校验 + 原子写；失败返回 None 降级，不影响 ask 流程）。ask 工具 description 已更新协议说明。
- **计划卡片**（AskPanel，`kind=ask && planFile` 存在）：「📅 计划」标题 + 复制全文按钮（navigator.clipboard）+ 方案文本 markdown 渲染（max-height 截断滚动）+ 「查看完整计划 →」按钮打开 **FileViewerModal**（本批次起接受任意 path，不再要求 SessionFileEntry；RightBar/FilesPanel 调用点同步适配）。修订循环每次 ask 落盘新文件，历史可溯。

## 五、测试

- 后端（221 全绿 / 0 warning）：`command_allowlist_skips_approval`（ConfirmEach 档白名单命中跳过审批真实执行 / 白名单外预取消 = 拒绝）；`confirm_parses_always_with_server_side_gate`（载荷解析 + `allow_always=false` 时 always 服务端钳制）；`save_plan_file` 落盘（路径在 workspace 内 + 内容含方案）；`default_roundtrip_and_compat` 补 allowlist serde default 断言。
- 前端（73 全绿 + build 通过）：新增 `run.queue.test.ts`（入队含附件 / done 出队附件随发 / error 暂停 + 继续执行 / 「立即」打断 → cancelled 后执行 / 编辑回填 / **done 双发按 run_id 去重回归**）；新增 `askpanel.test.tsx`（三选项与 `{approved, always}` 载荷 / allowAlways=false 降级 / Enter 确认 / 计划卡片与查看按钮 / ask 提交 answers）；`app.smoke.test.tsx` ask 用例适配 `.ask-card`；既有测试文件状态桶补齐新字段。
- 双代理代码审查（7 维度）通过：1 🔴（run:done suggest 双发 × 队列出队竞态）+ 4 🟡 全部修复（见 §五·补）；已知环境敏感：`running_flag_resets_after_run_ends`（既有 mock-HTTP 测试）单测反复重跑偶发超时，全量稳定通过，与本批次改动路径无关。

### 审查修复（§五·补）

- **🔴 run:done 双发去重**：done1 同步出队会把 `running` 乐观置回 true（新 run），旧 `!running` 幂等守卫对 done2 失效 → 二次放行错误复位 running 并重复出队（队列 ≥2 条时条目静默丢失）。修复：`TabRunState.lastDoneRunId` 按 `run_id` 去重（`run.ts`）。
- **🟡 pendingItemId 残留**：「立即」与自然完成竞态时标记残留，后续手动停止会跳队执行旧条目 → `run:done` / `run:error` 分支统一清 `pendingItemId`。
- **🟡 白名单「本项目」语义**：匹配键改为 `cwd + \u{1} + 命令全文`（会话 cwd = 项目托管目录，随项目走 → 白名单不跨项目命中）；设置页展示拆分命令与生效目录。
- **🟡 计划文件同秒覆盖**：文件名加毫秒 + 4 位随机后缀，修订/并行场景不再互相覆盖。
- **🟡 测试缺口**：补 done 双发回归（前端）与 confirm always 门控（后端）；白名单写回与 save_config 的 read-modify-write 既有竞态窗口已在代码注释声明（与 toggle_skill 同款既有模式，留待统一收敛）。

## 六、手动验证清单

1. 长任务运行中在 Composer 输入文字（可粘图）回车 → 输入框上方出现队列条目（📎×N 标记），placeholder 变为「继续输入以排队后续修改…」；连提多条按序排列。
2. 当前任务完成后下一条自动开始执行；全部执行完队列清空。
3. 点某条「↑ 立即」→ 当前任务被取消（出现「已取消」）→ 该条立即执行 → 其余队列继续；点 ✏️ → 文本回填输入框、条目移除；点 🗑️ → 条目移除。
4. 让任务失败或手动停止 → 队列保留并显示「队列已暂停」→ 点「继续执行」恢复顺序执行。
5. ConfirmEach 档触发命令审批 → 新卡片样式（需要权限/等待确认/命令块/编号三选项）→ 点击卡片后按 Tab/↑↓ 移动高亮、数字 1-3 快选、回车确认；选 2（始终允许本项目）→ 确认后命令执行，同命令再次执行不再询问。
6. 设置 → 安全 → 可见「命令白名单」列表 → 删除某条 → 保存后该命令恢复询问。
7. plan 档走完整流程（分析 → 方案 → ask 批准）→ 批准卡片上方出现「📅 计划」卡片（方案全文渲染）→ 复制按钮可用 → 点「查看完整计划」→ 弹窗打开 `.codewave/tasks/plan-*.md`（且该文件出现在右侧栏「文件」标签）。
8. 多问题 ask（≥2 问题）→ 分页器 ‹ n/N › 翻页作答；「忽略」清当前题并进下一页。

## 七、提交信息（用户执行）

```
feat(queue): 运行队列 + 询问窗口重构 + 命令白名单 + 计划卡片
```

## 缺陷修复：顺序出队报「该会话已有运行中的任务」

- **根因**：`run:done` 曾对同一 run 发两次——`drive_agent` 在 suggest 收尾路径先发一次（此时 `running` 尚未复位，且距复位还有 ≥64ms 流式收尾 + checkpoint），`run_chat` 复位 `running` 后再发一次。前端 done① 触发 `runQueueNext` 出队下一条并调 `startChat`，恰好撞进未复位窗口 → 后端拒绝，且该条已出队（报错 + 丢条目双重损伤）；done② 被 `lastDoneRunId` 去重忽略，无法补救。
- **修复**：`drive_agent` 不再发 `run:done`，suggest 建议随返回值交出；`run_chat` 在 `running.store(false)` 与 checkpoint 之后发**唯一一次** done（suggestions 并入载荷）。done/cancelled 全部严格晚于复位，出队不再有竞态窗口。
- **验证**：`cargo test` 228 全绿 / 0 warning；手动项——队列两条任务顺序执行，第一条完成后第二条应立即开跑且无报错（suggest 正常生成的会话重点回归）。
