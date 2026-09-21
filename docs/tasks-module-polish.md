# 计划任务模块：弹框 → 独立页 + 周期选择器（tasks-module-polish）

> 批次：计划任务模块优化（2026-09-22）。起点是「文案说假话 + 状态看不见 + 任务不可维护」，
> 中途用户追加两条范围：**弹框改独立页**、**周期不用手写 cron**。
> 分支 `feat/tasks-module-polish`，基线 `main` @ `65ce4aa`（PR #70 合并后）。
> 需求依据见 `.codewave/tasks/20260922-024632-tasks-module-polish/requirement.md`，方案见同目录 `plan.md`。

## 1. 拓扑：从弹框到覆盖式全屏页

`TaskCenterPanel`（Modal）退役，改为 `TasksPage`——与设置页**同一范式**的覆盖式全屏页：

```
Layout (100%)
├── Header.toolbar               ← 顶栏（自绘标题栏）：拖动/最大化/关闭全程可用（刻意不在覆盖层内）
└── Layout (calc(100% - titlebar), position: relative)
    ├── Sider        .workspace-covered   ← 左栏（挂载保留，只隐可见性）
    ├── Content      .workspace-covered   ← 中栏 + RightBar
    ├── ResizeHandle ×2 .workspace-covered ← 栏宽分隔条（同时 tabIndex=-1）
    ├── SettingsPage (.settings-shell)    ← 设置页（打开时挂载）
    └── TasksPage    (.tasks-shell)       ← 任务页（打开时挂载；后挂 = 同开时压在设置页之上）
```

**硬约束（本批的 🔴 级返工点）**：`.tasks-shell` 是 `absolute; inset: 0`，**必须挂在上面那个内层
`Layout`（`position: relative`）里**。挂到外层 `Layout` 上时它相对外层定位，会连自绘标题栏一起盖住，
且让位集合与设置页不一致（表现为「页盖住了、键盘焦点却还在工作区里」）。守门用例在
`app.smoke.test.tsx`：断言 `.tasks-shell` 的父节点就是 `has-sider` 那个布局，且 `.workspace-covered`
精确集合 = `[Sider, Content, 分隔条 ×2]`。

两页共用同一个让位开关（`AppShell.tsx` 的 `workspaceCovered = settingsOpen || tasksOpen`）：
Sider / Content / 两条分隔条一起让位，绝不 `display: none`（会让 ResizeObserver 测到 0 尺寸、
滚动容器错乱，进而干扰运行中任务——见 [settings-fullscreen-shell](./settings-fullscreen-shell.md)）。

### 页内结构

| 区 | 类 | 说明 |
|---|---|---|
| 操作条 | `.tasks-actions` | 返回工作区 + 标题 + 右侧「新建任务」；禁用原因**就地**写在按钮旁（不藏 Tooltip） |
| 内容区 | `.tasks-body` | 自己滚（操作条恒在列内可点）；错误行常驻 + 旧列表照旧显示 |
| 任务行 | `.task-row` | 两级：`.task-row-main`（整块可点展开历史，`role=button` + `aria-expanded`）与 `.task-row-actions`（同级兄弟，不做「button 里嵌按钮」的无效语义） |
| 历史 | `.tasks-history` | 时间 · 状态 · 来源（定时/手动）· `out_tokens`；摘要另起一段（`pre-wrap` + 限高滚动） |

行内容：名称 · 周期描述 · 项目归属 · 状态标签（运行中/正常/失败/已跳过）· 下次触发 · 暂停开关 ·
立即运行 · 编辑 · 删除。

## 2. 周期选择器（不再手写 cron）

`ui/src/features/panels/schedulePreset.ts` 是后端 `parse_schedule` 文法的**前端镜像**（只读语义，不改后端）：

| preset | 表达式 |
|---|---|
| 每天 | `cron:<分> <时> * * *` |
| 每周 | `cron:<分> <时> * * <周列表>`（`1,3,5`，0/7 = 周日） |
| 每月 | `cron:<分> <时> <日> * *` |
| 每隔 | `every:<n> <m\|h\|d>`（**空白是文法的一部分**，`every:30m` 非法） |
| 一次 | `once:<本地 RFC3339>` |
| 自定义 | 原样透传 |

- `presetToExpr` / `exprToPreset` 双向映射；**认不出的表达式一律回落 `custom`**（原样保留用户写法，
  绝不臆造语义）——范围与步进（`1-5`）、6 字段 cron、`every:30m`、越界数值都落 custom。
- 上限与后端一致（`EVERY_LIMITS`：分钟 ≤ 43200、小时 ≤ 720、天 ≤ 30），越界就地提示且不许保存。
- `describeExpr(expr, t)` 把表达式翻成人类可读文案；**`t` 由调用方注入**——早先它把中文写死在纯函数里，
  英文界面会露中文（本批修正）。
- 表单控件用原生 `<input type="time">` / `<input type="datetime-local">`（不引 antd TimePicker）：
  值就是字符串、与 preset 的时间串同形，省一层 Date 往返与格式化歧义。**接受代价**：控件外观跟随
  平台（Windows 上是 WebView 原生钟点选择器），与 antd 皮肤不完全统一。

## 3. 状态语义（唯一映射，禁各写一份）

`ui/src/stores/tasks.ts` 导出两个纯函数，任务页与左栏共用：

- `statusKind(status)` → `none`（从未跑过，不显示）/ `neutral`（`ok`、`skipped`）/ `error`（其余非空）；
- `statusLabel(status, t)` → `正常` / `失败` / `已跳过`（i18n），**未知状态原样回显**（不臆造翻译）。

色彩强度只映射风险等级：无彩色 = 默认、橙 = 需注意、红 = 危险。因此正常收尾用**中性标签**，
只有失败是红；左栏 `.task-dot` 同理——只有 `.err` 上色，`.task-dot.ok` 这条死规则已删。

## 4. 数据面

### 4.1 单一数据源（前端）

`ui/src/stores/tasks.ts`：`items` / `loading` / `error` / `runningIds` + `load` / `applyFired` /
`applyDone` / `upsertLocal` / `removeLocal`。任务页与左栏都从这里取数，**列表刷新只有 `load` 一条路径**。

- `load()` 失败**只置 error、不动 items**：读盘失败必须与「确实没有任务」区分开，否则一次瞬时失败
  会把列表擦成空态，用户会以为任务被删了；非数组载荷（未 mock 的命令返回 null）也按空列表兜住。
- `applyDone` 先认 id、旧载荷无 id 时退回按 name 匹配；清运行标记后 fire-and-forget 再 `load()` 一次
  ——事件只带收尾摘要，`runs` / `next_run` 是后端落盘后才有的。
- 创建 / 编辑 / 启停成功走 `upsertLocal`（免整表刷新的闪烁与滚动跳位）。
- **两个 `scheduled:*` toast 已删除**：提示改由界面状态承载（`notice.taskFired` / `notice.taskDoneName`
  随之零引用并双侧删除）。事件键名与载荷字段**一个都没动**。

### 4.2 落盘与历史（后端）

- 新字段：`enabled`（`#[serde(default = "default_true")]`——裸 default 会让旧任务被误判为暂停）、
  `runs: Vec<TaskRun>`（`#[serde(default)]`）。`TaskRun{at, status, summary, source, out_tokens}`，
  每任务保留最近 `MAX_TASK_RUNS = 20` 条（新的在前）。
- **落盘补齐**：`next_run` 推进后 / 执行结束写 `last_*` + `runs` 后 / `update` 后 / `set_enabled` 后
  四处都 persist（先克隆快照 → 释放 `tasks` 锁 → 再写盘，不在持锁时写盘）。
- `tick()` 跳过 `!enabled` 的任务（不执行、不写状态、不记历史、不发事件）。
- 新命令：`update_scheduled_task` / `set_scheduled_task_enabled` / `run_scheduled_task_now`
  ——三者都**不接收 `session_id`**（操作的是全局任务表），这是「只禁用新建」成立的前提。
- `trigger_now`：`exec_lock.try_lock()` 失败即返回「已有任务正在运行，请稍后再试」；`tokio::spawn`
  持 guard 跑 run 路径（`source = manual`），**不触碰 `next_run`**。为此把 `run_task` 拆成
  「加锁外壳 / 假定持锁内核」，supervisor 与手动触发各走一条。
- 落盘位置：`<主目录>/.codewave/projects/<project_id>/tasks/<id>.json`——**任务随项目落盘，重启后仍在**；
  只有自由会话（无 `project_id`）创建的任务仍是进程内。

## 5. Esc 与让位（三层收口）

1. `AppShell` 的全局 Esc 分支：`settingsOpen || tasksOpen` 为真时**直接返回**（事件目标可能是 `body`，
   光靠 `closest` 白名单会漏），白名单同时补 `.tasks-shell`；
2. `TasksPage` 在**捕获阶段**注册 window keydown 并 `preventDefault()`，抢在 AppShell 冒泡监听之前收口；
3. 行为链：弹窗（编辑）开着 → 归弹窗；Select/Dropdown/Popconfirm 浮层开着 → 让浮层先关；否则关闭页面
   返回工作区。**任何情况下都不停止运行中会话**。

## 6. 文案修正（本批的起点）

| 键 | 之前 | 现在 |
|---|---|---|
| `tasks.scheduleHint` | 示例写 `every:30m`（后端文法要求带空白，照抄必报错） | `every:30 m` 等**合法**示例 |
| `tasks.empty` | 「重启后清空」（任务其实随项目落盘） | 去掉该句 |

## 7. 已知取舍与偏差（登记）

- **`once` 任务的「不再触发」**：仅在 `enabled` 为真、`next_run` 为空且表达式是 `once:` 时显示。
  它是「一次性任务执行完（或被跳过）后后端清空 `next_run`」的真实语义；若后端仍留 `next_run`，
  该行会显示具体时刻而不是「不再触发」——两条路径都已在 `tasks.page.test.tsx` 钉住，不做臆测兜底。
- **删除失败会重新拉一次列表**：失败不摘行（任务还在，可原样重试），但同时 `load()` 一次对齐后端真相
  ——失败原因可能是「已被别处删掉」，本地那份就成了幽灵行。
- **项目归属三态**：`未归属项目`（无 `project_id`）/ `项目信息未加载`（注册表读盘失败，
  `projectsLoadFailed`）/ `项目已不存在`（注册表可信且确实查无此 id）。把「没读到」说成「被删了」
  会让用户以为任务悬空。
- **后端错误串为中文**：en-US 界面会看到中文错误原文（不做错误码，另开批次）。
- **自由会话不能新建计划任务**：任务必须归属项目才落盘，否则重启即丢；界面显式禁用 + 说明，
  查看 / 编辑 / 立即运行 / 删除不受限。
- **无人值守审批**：任务执行中撞上写操作审批会一直等（全局串行因此被阻塞），本批只在界面与文档写清。

## 8. 验证

- `cd src-tauri && cargo test`：**944 passed / 0 failed / 3 ignored**（新增 scheduler 用例：
  暂停被 tick 跳过、`update` 改计划重算 `next_run`、`set_enabled` 启用重算 + once 过期报错、
  `runs` 顺序与 20 条上限、旧 JSON 反序列化默认启用、`trigger_now` 被占拒绝、落盘后可读回）。
- `pnpm --dir ui test`：**89 文件 / 993 用例全绿**（`tasks.page.test.tsx` 取代已删除的
  `tasks.center.test.tsx`；新增 `stores.tasks.test.ts` / `schedulePreset.test.ts` /
  `projectnav.tasks.test.tsx`；`app.smoke.test.tsx` 补任务页覆盖式挂载的回归断言）。
- `pnpm --dir ui build` / `ui lint`：绿。
- **手动验证清单**（界面改动不做 GUI 自动点验，交付用户手动确认）：
  ① 左栏任务区点行 → 独立页打开，标题栏仍可拖动/最大化/关闭；
  ② 页面打开时左栏与中栏隐藏、Esc 关闭页面且**不停**运行中会话；
  ③ 新建任务只选周期与时间，保存后列表描述可读（每天 09:00 / 每周一、三、五 09:00 / 每 30 分钟 / 仅一次 …）；
  ④ 编辑既有任务：能反解出周期；认不出的表达式落在「自定义表达式」且原样保存；
  ⑤ 暂停开关 → 行显示「已暂停」，到点不执行；启用后下次触发重算；
  ⑥ 立即运行 → 行出现「运行中」，结束后状态与历史即时更新；被占时给出后端原文；
  ⑦ 展开历史：最近 20 条、来源区分定时/手动、失败行红、摘要限高滚动；
  ⑧ 删除失败有提示且列表重新对齐；
  ⑨ 无会话 / 临时会话时只有「新建」禁用且原因正确；
  ⑩ 重启应用后状态、下次触发、历史仍在（项目任务）。

## 9. 同时修正的文档漂移

- [technical-design](./technical-design.md)：依赖表与 `scheduler/` 一行的「tokio-cron-scheduler +
  进程本地（重启即清）」→ 改为 `cron` crate + 自写 tick + 任务随项目落盘。
- [p1-p2-implementation-report](./p1-p2-implementation-report.md) §6「计划任务为进程本地」标注**已失效**。
- [p2-plan](./p2-plan.md) §3 两处历史决策加「后续修正」注记。
- `src-tauri/Cargo.toml`：`tokio-cron-scheduler` **从未被引用**，本批移除（供应链与构建成本白担）。
