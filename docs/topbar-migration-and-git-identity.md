# 顶栏入口迁移 + 左下角 git 身份条

> 批次内容：顶栏「变更/任务/统计/设置」四入口迁移（变更 → 右侧栏新页签；任务/统计/设置 → 左下角固定状态区）+ 左下角新增 git 提交身份条（email 头像 + user.name + user.email）。

## 1. 需求与方案

用户原始需求（逐字）：

1. 将右上角菜单中的「变更」移动到右侧栏中的新 tab；
2. 任务/统计/设置移动到左下角右侧固定位置；
3. 左下角左侧位置显示 git 信息：git.user 和 email，最左侧是 email 的 avatar 头像。

方案取舍（经用户批准）：

- 「变更」弹窗（GitDiffModal）退役，内容面板化为右栏首个页签 `[变更 | 信息 | 日志 | 文件]`；Composer 命令菜单的 diff 入口保留，改为 `showChanges()`（自动展开右栏并落变更页签）；`diffOpen` 状态删除。
- 任务/统计/设置：弹窗与 `tasksOpen/statsOpen/settingsOpen` 状态不动，仅入口从顶栏移到左栏底部 footer 右位（图标按钮，Tooltip + aria-label）。
- 头像：纯本地生成——`email || name` 稳定 hash（31 进制累积取模）映射 hsl 色相 `hsl(h 45% 45%)` + 首字符；均缺失时 `?` + `var(--ws-border)`。零网络请求（无 Gravatar）；色相属数据驱动身份哈希色，不触碰 `--ws-*` 主题 token 体系（code-reviewer 判定可接受）。
- 左栏折叠态（36px 窄轨）：底部降级为纵向头像 + 三入口图标列（`SiderRailFoot`）。
- git 身份读取：新增只读 IPC `git_user_info`；`git2` config 分层快照（repo > global > system，与 `git config` 生效语义一致）；多根项目取主目录；无会话/非 git/读取失败静默降级为「未配置」，绝不报错；仅只读，无写入路径。

## 2. 文件级改动

后端：

| 文件 | 改动 |
|---|---|
| `src-tauri/src/git/status.rs` | 新增 `GitUserInfo` + `user_info(Option<&Path>)`（仓库 config 快照，失败回退 `Config::open_default()`，trim + 空串过滤，键缺失返回 None）+ 2 单测（repo 读取断言 / 缺失静默不 panic） |
| `src-tauri/src/host/commands.rs` | 新增 `#[tauri::command] git_user_info(session_id: Option<String>) -> crate::git::status::GitUserInfo`（有会话读主目录，无会话 None；直接复用 git 层类型，单一事实源） |
| `src-tauri/src/lib.rs` | invoke_handler 注册 `git_user_info` |

前端：

| 文件 | 改动 |
|---|---|
| `ui/src/ipc/client.ts` | 新增 `gitUserInfo(sessionId: string \| null)` |
| `ui/src/stores/ui.ts` | 删 `diffOpen`；新增 `rbTab/setRbTab/showChanges()`（rbTab 上提 store 供外部切换页签） |
| `ui/src/features/shell/RightBar.tsx` | rbTab 由本地 useState 上提为 useUi 订阅；Tabs 首位新增「变更」页签（`t("app.diff")`） |
| `ui/src/features/workspace/ChangesPanel.tsx` | 新文件：承接原 GitDiffModal 内容（多根分组 + CodeBlock patch）；`visible` 激活语义（未激活零 IPC，同 LogPanel）+ reqIdRef 过期守卫 + 失败保留旧数据并提示（区别于「无变更」空态）+ 手动刷新 |
| `ui/src/features/workspace/GitDiffModal.tsx` | 删除 |
| `ui/src/features/shell/AppShell.tsx` | 顶栏移除四按钮；新增 `hashHue/useGitInfo/SiderFooter/SiderRailFoot`；sider-body 底部挂 SiderFooter、窄轨挂 SiderRailFoot；顺带删除死变量 `activeWorkspace`（🟡 清理） |
| `ui/src/features/chat/Composer.tsx` | `doCommand("diff")` → `useUi.getState().showChanges()` |
| `ui/src/i18n/zh-CN.ts` / `en-US.ts` | 新增 `git.notConfigured`（未配置 / Not configured） |
| `ui/src/theme/app.css` | 新增 `.sider-footer/.git-id*/.sider-footer-ops/.sider-rail-foot/.rb-changes*` 样式段 |

测试：

| 文件 | 改动 |
|---|---|
| `ui/src/__tests__/app.smoke.test.tsx` | mountApp 改等「项目」；新增 `clickIconBtn()`（aria-label 定位无文本按钮）；主窗口用例断言顶栏无「设置/统计」、`.sider-footer` 结构与三入口、右栏首页签 = 变更；mock 增 `git_user_info` |
| `composer.history/paste.test.tsx` | afterEach 重置移除 `diffOpen`、加 `rbTab: "info"`（store 单例残留防范） |

## 3. 验证

- `cargo test`（src-tauri/）：**248 passed / 0 failed / 2 ignored**（含新增 2 个 user_info 用例）
- `pnpm --dir ui test`：**110/110 全绿**（18 文件）
- `pnpm --dir ui build`（tsc + vite）：通过
- code-reviewer 七维度审查：**无 🔴**；4 条 🟡 已全部修复（死变量 `activeWorkspace` 删除 / host 层重复 `GitUserInfo` 改为复用 `crate::git::status::GitUserInfo` / 变更页签 label 走 i18n / ChangesPanel 失败保留旧数据 + 错误提示）；修复后三套验证重跑均绿。

## 4. 设计要点与坑

- **行范围编辑事故与恢复**：本批次曾用多变更同文件 lineRange 编辑导致 `ui.ts` 结构错乱、`status.rs` 测试模块交叉破坏（newText 行号随前序变更漂移 + CRLF 行尾不匹配 oldText）。恢复方式：两文件整文件重写（status.rs 经 `git diff --numstat` 核对为 50+/0- 纯新增，原文无损）。**教训：同文件多处改动优先用唯一 oldText 精确匹配，整文件重写后必须与 git 历史核对完整性。**
- rbTab 上提后 LogPanel 的 `visible` 语义不变；右栏折叠时 Tabs 整树卸载天然断轮询，无泄漏。
- `useGitInfo` 在 SiderFooter/SiderRailFoot 各订阅一次，但两者随 `explorerOpen` 三元互斥挂载，同一时刻仅一次 IPC；会话切换/开合侧栏各触发一次毫秒级本地查询，可接受（reviewer 判定，不建议缓存）。
- 事件面 27 键契约未触碰；git 层保持只读。

## 5. 手动验证清单（GUI，由用户执行）

1. `pnpm tauri dev`（确认无 dev/打包实例互斥）；
2. 顶栏：应只剩 Tab 条（无 变更/任务/统计/设置 文字按钮），空白处可拖窗；
3. 右侧栏：页签为 [变更 | 信息 | 日志 | 文件]；点「变更」显示工作区 diff（多根项目按根分组），刷新按钮可重拉；折叠右栏后执行 Composer `/` → 「变更详情」，右栏应自动展开并落在变更页签；
4. 左下角：左侧显示 email 头像（首字符 + 彩底）+ 用户名/邮箱两行；右侧三个图标（任务/统计/设置）点击分别打开对应弹窗；
5. 左栏折叠（36px）：底部出现纵向头像 + 三入口图标，点击行为一致；悬停有 Tooltip；
6. 边界：无会话 / 非 git 目录时头像显示 `?` + 「未配置」，不报错；断网状态头像仍正常（纯本地哈希色）。

## 6. 遗留与后续（🟢 可选，不阻塞）

- ChangesPanel 行为级测试用例（分组/过期守卫/失败保留）可后续补；
- 运行结束自动刷新 diff（订阅 run:finished）可作增强；
- `rbTab` 类型可收紧为四值联合；
- 多根项目 git 身份目前取主目录（主目录非仓库时回退全局配置），多身份聚合不做。
