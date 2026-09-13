# — title 子代理：会话自动命名批次报告

## 0. 背景与需求

新会话开始、用户输入首条消息后，自动为会话起一个精炼标题（最好不超过 20 个汉字），替代现状的「首条消息前 10 字符截断」启发式。

**形态决策**：title 以「子代理角色」落地——人设定义进 `agents/` 内置角色注册表（`AgentDef { name: "title" }`），但**不经 subagent 工具委派**（不进其角色枚举、不跑完整 agent 循环），而是由 `core/title.rs` 做一次性 LLM 调用直接消费角色定义全文。理由：

- 起标题是零工具、单轮文本任务，走 `subagent` 工具（独立 runtime + 多步循环 + sub:* 事件流）是浪费，且会在界面冒出无意义的子代理卡片；
- 仓库已有同形范本：`context::compact_history` 的后台一次性 `stream_model` 调用（零工具 / 独立取消令牌 / 超时保护），title 沿用该模式。

## 1. 角色注册：agents/mod.rs

- 新增导出 `pub const TITLE_BODY`（单一事实源）：人设含 `## 角色/任务/工作方式/输出格式/约束`，核心契约——只输出一行标题、≤20 字符、语言跟随用户消息（中文消息给中文标题）、不加引号/前后缀、严禁输出标题以外内容。
- `builtin()` 追加 `AgentDef { name: "title", description, body: TITLE_BODY }`；模块头注释说明 title 例外语义（仅供 core/title.rs 消费，主代理不得委派）。
- `subagent` 工具的 description/schema 角色枚举**不变**（手工维护的 6 角色清单），主 LLM 不会被引导调用 title。
- 测试 `six_builtin_roles_present` → `builtin_roles_present`（7 角色）；单测 `registry_body_shares_single_source` 锚定注册表 body === TITLE_BODY。

## 2. 生成与落位：core/title.rs（新模块）

- `sanitize_title`：模型输出 → 首个非空行 → 剥最外层成对包裹引号（「」『』“”‘’及半角）→ 截断 20 字符 → 空则 None。
- `should_replace_title(current, fallback)`：仅当当前标题为空或仍是兜底值才落位——**生成期间用户手动重命名则放弃覆盖**。
- `generate_and_apply(core, rt, user_text, fallback_title)`：
  - 模型 = 会话有效模型（override 优先，与 compact 一致）；输入 = 用户消息截 2000 字符 + 生成指令；
  - `StreamRequest { tools: [], reasoning_effort: None }` + mpsc 收集 + 独立 `CancellationToken`（用户停止 run 不连带取消命名）+ 20s 超时（超时即放弃，保留兜底）；
  - 成功路径：zombie 复查 → 覆盖守卫 → 写 `rt.title` → 已入索引则 `rename_in_index`（轻量，只动索引不重写历史 gz；未入索引由 run 收尾 checkpoint 兜底）→ `stats.record(kind=title)`（usage 缺失时按输入/输出估算兜底，[docs/tool-optimizations-port](./tool-optimizations-port.md) 计账哲学）→ `session_log` 留痕 → emit **`session:title`** `{ session, title }`；
  - 失败/超时/输出无效：session_log warn 后静默返回，10 字符兜底标题保留。

## 3. 触发点：core/agent.rs run_chat

磁盘历史加载块之后、`push(user)` 之前判定 `history_was_empty`（= 本会话首条消息，重开的旧会话不误触发）；提取用户消息文本，push 后 `tokio::spawn(crate::core::title::generate_and_apply(...))` **并行于主 run**——标题秒级返回，不等 run 完成；首条消息仅图片无文本则跳过。

## 4. 既有缺陷顺带修复：load_session 标题回填（host/commands.rs）

重开旧会话时 runtime `rt.title` 不回填（为空），再发消息会被 `start_chat` 的 10 字符启发式覆盖既有标题并随 checkpoint 持久化——该缺陷会直接破坏自动命名的产物。修复：`load_session` 从 meta 捕获 `title`，runtime 标题为空时回填。

## 5. 事件面与前端（26 → 27 键）

- 新事件键 **`session:title`**（`events.contract.test.ts` 双向契约自动覆盖）。
- `ui/src/stores/run.ts` `bindGlobalHandlers` 新增 handler → `useSessions.getState().applyTitle(session, title)`。
- `ui/src/stores/sessions.ts` 新增 action `applyTitle`：空标题忽略；同时更新会话列表 meta 与已打开 Tab 的 title → Tab 条与 ProjectNav 会话树即时刷新（新会话首个 checkpoint 前侧栏靠 Tab.title 合成 meta 显示，两条显示路径均覆盖）。
- 测试 `ui/src/__tests__/sessions.title.test.ts`（3 用例：事件同步 meta+Tab / 空标题忽略 / 未开 Tab 会话只更新列表不误建）。

## 6. 改动文件清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/agents/mod.rs` | TITLE_BODY 常量 + title 角色 + 测试更新 |
| `src-tauri/src/core/title.rs` | 新模块：sanitize / should_replace / generate_and_apply + 5 单测 |
| `src-tauri/src/core/mod.rs` | 注册 `pub mod title` |
| `src-tauri/src/core/stats.rs` | `KIND_TITLE = "title"` |
| `src-tauri/src/core/agent.rs` | run_chat 首条消息判定 + 并行 spawn |
| `src-tauri/src/host/commands.rs` | load_session 标题回填 |
| `ui/src/stores/run.ts` | `session:title` handler |
| `ui/src/stores/sessions.ts` | `applyTitle` action |
| `ui/src/__tests__/sessions.title.test.ts` | 新测试（3 用例） |
| `src-tauri/src/tools/subagent.rs` | 审查修复：role="title" 硬拒绝（E_ARGS） |
| `ui/src/features/panels/TokenStatsModal.tsx` | 审查修复：kind 标签补 compact/title |

## 7. 验证

- `cargo test`：全绿 235 passed / 0 failed / 2 ignored（真实 GLM E2E 为显式运行的 ignored 项）。
- `pnpm --dir ui test`：93/93 全绿（含契约测试双向通过）；`pnpm --dir ui build`：type check + vite build 通过。
- 边界：run 队列仅首条触发一次；生成期间删除会话（zombie 检查 + delete_session 补置 zombie + rename_in_index 对缺失 id 无害）；生成期间手动改名（单临界区守卫放弃覆盖）；模型未配置（start_chat 已拦，generate 亦有防御）。
- 成本：每次新会话首条消息多一次小调用（输入 ≈ 截断消息 + 提示词，输出 ≤ 20 字符），统计面板 kind=title（「自动命名」）可见。

## 8. 手动验证清单（界面改动，由用户执行）

1. 新会话（项目会话 / 临时会话各一）发首条消息 → 数秒内 Tab 标题与会话树标题变为生成标题（≤20 字、非前 10 字截断）。
2. 发出首条消息后立即手动重命名 → 自动标题不覆盖手动命名。
3. 重开已有标题的旧会话再发消息 → 标题保持不变（回填修复生效）。
4. 断网或停掉模型服务发首条消息 → 保留 10 字符兜底标题，run 本身不受影响（RightBar 日志可见「自动命名失败/超时」warn）。
5. 统计弹窗按 kind 查看出现「自动命名」计账条目。

## 9. 审查修复（code-reviewer 审查后落地）

- 🟡 **rt.title 守卫 check-then-act 分锁**（title.rs）：`should_replace_title` 检查与写入原先两次取锁，中间可被 `rename_session` 插入导致手动命名被覆盖——合并为单临界区（`lock_ok`，防锁中毒）。次级竞态（checkpoint 读 title 与 rename_in_index 交错索引短暂回退兜底值，后续 checkpoint 自愈）已在注释说明。
- 🟡 **delete_session 不置 zombie**（host/commands.rs）：run 结束后自动命名后台任务最长存活 20s，删除会话后仍会写 `logs/<id>.log` 与 stats 计账（复活已删会话痕迹）——镜像 delete_project 的 H5 写法补 `zombie.store(true)`。
- 🟡 **8 处注释 [docs/custom-font-and-titlebar](./custom-font-and-titlebar.md) → [docs/session-auto-title](./session-auto-title.md) 勘误**（[docs/custom-font-and-titlebar](./custom-font-and-titlebar.md) 已被自定义字体批次占用）。
- 🟢 **subagent 硬拒绝 role="title"**（tools/subagent.rs）：`agents::find` 能命中 title，主 LLM 显式委派会跑无意义的命名专家循环——返回 E_ARGS 把「不得委派」软契约变成硬约束。
- 🟢 **统计面板标签补全**（TokenStatsModal.tsx）：kind 标签映射补 `compact: 上下文压缩`、`title: 自动命名`（compact 属既有欠账顺带补上）。
- 🟢 **sanitize 剥「标题：」前缀**（title.rs）：模型偶尔无视"不加前后缀"约束，清洗链补 `strip_title_prefix` + 单测。
