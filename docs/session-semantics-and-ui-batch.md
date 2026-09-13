# CodeWave · 12 会话语义重构 + 界面批次（[docs/session-semantics-and-ui-batch](./session-semantics-and-ui-batch.md)）

> 日期：2026-09-01
> 范围：会话/项目语义重构（单目录 + 数据随项目走 + 临时会话免目录）+ 九项界面调整/缺陷修复。
> 约束遵守：零 git 操作（本批次提交由用户授权后执行）；界面交付手动验证清单（§4）。

---

## 1. 会话/项目语义重构（需求 1.1–1.5）

| 需求 | 实现 |
|---|---|
| 1.1 双形态会话 | 项目会话（挂项目）+ 临时会话并存 |
| 1.2 临时会话免目录 | `create_session(project_id=null, workspace=null)` → workspace 落全局数据目录；左侧「新建临时会话」不再弹目录选择，点击即开聊 |
| 1.3 项目会话必须选目录 | 新建/编辑项目必须选择主目录（保存按钮在未选目录时禁用） |
| 1.4 移除多目录 | `ProjectEntry.directories: Vec<String>` → `directory: String`（旧字段 serde 兼容读取、不再写入）；extra_roots 全链路清空（create_session / task_scope / system prompt 项目段） |
| 1.5 数据随项目走 | `ProjectEntry.data_dir = <主目录>/.codewave`；注册表扫描两个位置（旧位置为基底、新位置覆盖），旧项目保持旧位置可读写，新项目建在项目目录下；`delete_project` 新语义只删 `.codewave/` 子目录 + 旧位置残留，用户代码文件不动 |

**迁移策略（零破坏）**：`load()` 同时读 `~/.codewave/projects/<id>/project.json` 与各已知项目目录下 `.codewave/project.json`，后者优先；不自动搬数据，旧项目首次在新语义下保存时自然以 `data_dir` 为准。

**连带修正**：`agent.rs` / `context.rs` 的 system prompt 项目段改为「Project directory + Scratch dir」单目录表述；`task_scope` 单根语义；RightBar 项目区显示主目录 + 数据目录。

## 2. 界面调整（需求 2.1–2.9）

| 需求 | 实现 |
|---|---|
| 2.1 代码框 github light | hljs 主题 `github-dark.css` → `github.css`；`.md pre`/`.code-block` 浅色底 `#f6f8fa` + 边框 `#d0d7de` + github 语法配色（不走 token，与主题桥接解耦的恒定浅色代码区） |
| 2.2 标题 ≤10 汉字 | `start_chat` 标题截断 `chars().take(10)` |
| 2.3 消息时间戳 | user/assistant 消息 role 行左侧加 `年-月-日 时:分:秒` 浅灰小字（`.role .ts`）；user 消息按发送时刻，assistant 按渲染时刻 |
| 2.4 滚动逻辑 | `onScroll` 维护 atBottom（距底 <40px）；贴底时内容增长自动跟随；离开底部后停止跟随，聊天区底部居中浮现圆形「↓」按钮，点击平滑回底 |
| 2.5 子代理折叠 + 进度 100% | 子代理卡改 antd Collapse（头 = 角色/描述/步数/tokens，体 = 进度条 + 工具链）；完成后进度固定 100%（运行中封顶 99%） |
| 2.6 Todo 入右侧栏 | RightBar 新增「当前计划」节（会话信息下方）；数据源 `TabRunState.todos` 本就按 Tab 隔离，切换会话自动跟随；聊天区内联 PlanPanel 不再重复显示 |
| 2.7 右栏 +30% | `.right-bar` 240px → 312px |
| 2.8 跑马灯撑破滚动区 | 根因：antd Collapse header 是 flex 容器且 `.ant-collapse-header-text` 无宽度约束，`white-space:nowrap` 的跑马灯内容把 header 撑到内容宽 → 整个聊天区出现横向滚动。修复：header `display:flex + max-width:100% + overflow:hidden`、header-text `flex:1 + min-width:0`、marquee `flex:1 1 0 + width:0`；聊天区本身加 `overflow-x:hidden` 兜底 |
| 2.9 发送后滚到底 | 监听 items 增长且末条为新 user 消息 → `requestAnimationFrame` 强制滚底并重置贴底状态 |

## 3. 验证

| 项 | 结果 |
|---|---|
| `cargo test` | ✅ 152 passed / 0 warning（projects/scheduler 测试同步单目录语义） |
| `pnpm --dir ui test` | ✅ 24/24（项目弹框用例改单目录断言） |
| `pnpm --dir ui build` | ✅ tsc + vite |

## 4. 手动验证清单

1. **临时会话**：左侧「新建临时会话」→ 不弹目录选择直接开聊；发送消息可正常对话。
2. **项目会话**：新建项目 → 不选目录时保存禁用；选择目录保存后 → 项目下新建会话 → 对话、文件树、git 均围绕项目主目录。
3. **数据随项目走**：新建项目后检查 `<项目目录>/.codewave/` 出现 temps/logs/memory/skills/tasks 与 project.json；删除项目后该 `.codewave/` 消失、项目目录其余文件完好。
4. **旧项目兼容**：升级前的旧项目仍出现在列表中且可开会话（数据仍在 `~/.codewave/projects/<id>/`）。
5. **标题**：发送长消息 → Tab 与导航标题 ≤10 字。
6. **时间戳**：每条消息 role 行左侧浅灰 `YYYY-MM-DD HH:mm:ss`。
7. **滚动**：贴底时流式输出自动跟随；向上翻阅后出现「↓」按钮，点击回底；发送新消息自动回底。
8. **跑马灯**：思考中标题行内跑马灯滚动，页面无横向滚动条（2.8 回归点）。
9. **子代理**：运行中卡片可展开看进度/工具；完成后进度条 100% 绿色。
10. **右侧栏**：出现「当前计划」节且随会话切换；右栏明显变宽。
11. **代码框**：浅色 github light 样式。
