# 顶栏标题栏内容批次（两段式背景 + 会话历史导航）

> 对照参考截图实现顶栏内容；Windows / macOS 布局差异复用 [docs/custom-font-and-titlebar](./custom-font-and-titlebar.md) 自绘标题栏既有机制。
> 需求确认结论：←→ 实现会话历史导航；分支胶囊只读展示（无 Dropdown）；项目胶囊纯展示不可点；右侧图标仅保留右栏开合切换；无更多菜单。

## 1. 需求与截图元素映射

截图顶栏（从左到右）与 CodeWave 落点：

| 截图元素 | 实现 |
|---|---|
| Logo（Z 字块） | `src-tauri/icons/StoreLogo.png` 复制为 `ui/src/assets/store-logo.png`，24px 圆角 `<img>` |
| ← / → | 会话切换历史导航（本批次新增，见 §3） |
| 粗体会话标题 | `useActiveTab().title`，ellipsis + 悬停全文；无活动会话显示 dimmed "CodeWave" |
| 📁 项目名胶囊 | **Git worktree 语义**：显示会话工作目录 basename（`basename(tab.workspace)`，悬停显完整路径）；临时会话（`projectId=null`）显示「临时会话」；纯展示不可点 |
| ⎇ 分支胶囊 | `useActiveRun().gitEntries.branch`（与右栏 InfoPanel 同数据源，零新增 IPC）；仅 `repo=true` 且 branch 非空时渲染；**纯展示无 Dropdown** |
| ⋯ 更多菜单 | **按需求移除，不实现** |
| 右侧图标组 | 仅保留面板布局切换 = 右栏开合（`PicRightOutlined`，`setRightBarOpen` 翻转 + localStorage），tooltip 复用 `app.collapseRight/expandRight` |
| 窗口控制 − □ × | 零改动：Windows 由 tauri-plugin-decoration 注入，macOS 原生红绿灯 Overlay |

## 2. 两段式背景与平台差异

截图关键特征：标题栏有**两段背景色** —— 左段（Logo/箭头区）与左栏同色，右段与内容区同色，分界线与侧栏右缘连续。

- 左段 `.tb-left-seg`：`background: var(--ws-bg-nav)`，宽度内联 280/36（`SIDER_W_OPEN/SIDER_W_CLOSED` 常量与 `AppShell` 的 `Sider width` 同源导出，杜绝两处硬编码漂移），`transition: width 0.2s` 与 [docs/sidebar-toggle-buttons](./sidebar-toggle-buttons.md) 的 Sider 过渡同步，`border-right: 1px solid var(--ws-border)` 延续 Sider 分隔线。
- 右段 `.tb-main-seg`：`flex:1 + min-width:0`，背景 `var(--ws-bg-main)`（追加调整：与中右栏内容区同色，亮色 #f8f8f8）；标题簇从分界线右侧开始（与截图「标题对齐内容区左缘」一致）。
- `.toolbar` 自身**零 padding 零背景**：背景与平台让位全部下沉到段容器，左段分界线才能上下连续（旧 `6px` 纵向 padding 与左右 clearance padding 均移除）。

平台差异（无新增分支逻辑，全部复用 [docs/custom-font-and-titlebar](./custom-font-and-titlebar.md) clearance 机制，规则迁移到段容器）：

| 平台/模式 | 规则 |
|---|---|
| Windows（custom） | 插件注入控制条叠于右上，`.tb-main-seg { padding-right: max(10px, var(--tauri-plugin-decoration-right-clearance, 0px)) }` 自动让位 |
| macOS（custom） | 红绿灯 Overlay 落在左段上：`html[data-os="macos"] .tb-left-seg { padding-left/min-width: max(78px, clearance) }`；折叠态 36px 容不下红绿灯，`min-width` 撑住让位（与 Sider 对齐让位于可用性，见 §6 取舍） |
| native 回退 | `html[data-titlebar-mode="native"]` 撤销全部让位（回落 10px），规则后写胜出 |

**拖拽语义**：Tauri drag.js 只认 target 自身的 `data-tauri-drag-region` —— `.tb-left-seg` / `.tb-main-seg` / `.tb-flex`（中段弹性区，`align-self: stretch` 全高）均带标注：段内空白处可拖窗，按钮/胶囊以自身为 target 不受影响；原 `titlebar-drag-zone`（inset:0 垫底）保留兜底。

## 3. 会话历史导航（← / →）

`ui/src/stores/sessions.ts`：

- 状态：`histPast: string[]` / `histFuture: string[]`（浏览器语义双栈，上限 50 条裁剪）。
- 录制：模块底部 `useSessions.subscribe` 监听 `activeKey` 变化 —— prev 会话入 `histPast`、清空 `histFuture`；`navBack/navForward` 内部跳转（模块级 `navJumping` 标志）与初始激活（prev 为空）不录。
- `navBack()/navForward()`：模块级 `popValid()` 弹栈时**连续跳过已关闭会话**的失效条目，找到第一个仍打开的目标；跳转时当前会话按方向搬入对侧栈。
- 快捷键：`Alt+←/→`（AppShell 全局 keydown），`input/textarea/[contenteditable]` 内不劫持（macOS Option+箭头是词间移动）；现有 `Ctrl/Cmd+←/→` cycleTab 保持不变。

## 4. 改动清单

| 文件 | 内容 |
|---|---|
| `ui/src/features/shell/TopBar.tsx` | **新增**：两段式顶栏内容（Logo/历史导航/标题/双胶囊/右栏切换），导出 `SIDER_W_OPEN/SIDER_W_CLOSED` |
| `ui/src/features/shell/AppShell.tsx` | Header 内接 `<TopBar />`（删 `.flex` 占位）；Sider 宽度改用共享常量；新增 Alt+←/→ 键绑定 |
| `ui/src/stores/sessions.ts` | 历史导航状态与动作 + subscribe 录制 |
| `ui/src/ipc/client.ts` | `git_status` 返回类型补 `branch?: string \| null`（后端 `host/commands.rs:723` 早已返回，仅声明对齐） |
| `ui/src/utils/path.ts` | **新增**：`baseName()`（反斜杠/POSIX 归一取末段目录名）；sessions.ts `dirName` 与 TopBar 胶囊统一消费 |
| `ui/src/assets/store-logo.png` | **新增**：自 `src-tauri/icons/StoreLogo.png` 复制 |
| `ui/src/theme/app.css` | `.toolbar` padding/背景下沉；新增 `.tb-left-seg/.tb-main-seg/.tb-*/.tb-flex` 与 macos/native 段级规则；恢复全局 `.flex`（顶栏不再用，但 ProvidersPanel/TaskCenterPanel/SettingsModal 行内布局仍依赖） |
| `ui/src/i18n/zh-CN.ts` / `en-US.ts` | 新增 `titlebar.back/forward/tempSession` |
| `ui/src/__tests__/titlebar.content.test.tsx` | **新增** 10 用例（见 §5） |
| `ui/src/__tests__/titlebar.style.test.ts` | 契约迁移：macos/native 顺序断言从 `.toolbar` 改到 `.tb-left-seg`/`.tb-main-seg`，新增两段背景契约（过渡/分隔线/弹性占满）；`.toolbar relative`、drag-zone、内容层 z-1 三条不变 |

## 5. 验证

- `pnpm --dir ui test`：**155/155 全绿**（22 文件，含本批次新增 10 用例：两段 drag-region 与空态占位、左段宽度与 Sider 对齐（280/36 + 折叠隐藏导航钮）、标题/worktree 胶囊 basename 与完整路径 title、分支胶囊、临时会话文案、非仓库隐藏分支、录制订阅录制/消亡不录、死栈禁用与剪枝、历史导航边界禁用与切换（含失效条目跳过）、Alt+←/→ 键盘路径（含输入框豁免）、右栏切换翻转 + localStorage）。
- `pnpm --dir ui build`：type check + vite build 通过。
- 测试基建备注：裸 `store.setState` 断言 DOM 必须用 `act()` 包裹 —— React 19 在 act 外不刷新外部 store 重渲（fireEvent 内部已含 act）。

## 5.1 代码审查（code-reviewer）修复项

| 级别 | 问题 | 修复 |
|---|---|---|
| 🔴1 | `.toolbar` 删除 padding 声明后，antd 6 `Layout.Header` 默认 `padding: 0 50px` 回归生效，左段不再贴窗口左缘、两段分界线错位（测试不加载 CSS 故全绿仍漏网） | `.toolbar` 显式恢复 `padding: 0` + `titlebar.style.test.ts` 新增契约断言防回归 |
| 🔴2 | Alt+←/→ 属 WebView2 默认浏览器加速键集，Windows 上按键可能被 webview 层吞掉（页面 keydown 收不到） | 无法静态确认，列入手动验证清单第 1 项；若失灵：建窗时关闭 WebView2 浏览器加速键，或改绑 Alt+Shift+←/→（勿用 Ctrl+Alt+←/→，Intel 核显屏幕旋转热键） |
| 🟡3 | 关闭 Tab（含关闭最后一个 → activeKey=null）会把死会话 key 录入 histPast，「按钮亮但点了没反应」 | 录制订阅增加 `!state.activeKey` 早退（消亡非导航）；按钮禁用态改为「栈内存在仍打开会话」判定（`canBack/canForward`），纯死栈直接禁用 |
| 🟡4 | 录制订阅 / 键盘路径零直接测试；seedRun 手写 run 桶形状脆裂 | 补 4 用例（录制+消亡不录 / 死栈禁用+剪枝 / 键盘触发+输入框豁免 / navJumping 不回录）；seedRun 改用 `initTab()` 兜底建桶后覆写 gitEntries |
| 🟡5 | keydown `e.target` 可能非 Element（document 无 closest） | `target instanceof Element` 守卫 |
| 🟡6 | TopBar `baseName` 与 sessions.ts `dirName` 同语义两份实现且行为分叉（后者不归一反斜杠） | 抽共享 `ui/src/utils/path.ts` `baseName()`，两处统一（顺带修复 Windows 路径会话标题兜底取到整条路径的隐患） |
| 🟢 | navJumping 正确性依赖 zustand set 同步通知；接口注释错位；drag 垫层职责不清 | 均已在注释中写明 |

### 手动验证清单（GUI 不做自动点验）

1. **Windows Alt+←/→（审查 🔴2，最优先）**：切换几个会话后按 Alt+←/→，确认能回退/前进。若无效（被 WebView2 浏览器加速键吞掉），需建窗时关闭加速键或改绑 Alt+Shift+←/→。
2. Windows：顶栏右段不压右上角窗口控制按钮；左段灰色区从窗口左缘 0px 开始（审查 🔴1 回归点）；中段空白拖窗流畅；拖拽层双击最大化；悬停最大化 ≥500ms 出 Snap Layout 四宫格。
3. macOS：红绿灯落在左段灰色区上且不与 Logo/箭头重叠；左栏折叠后红绿灯仍不被遮挡（左段 min-width 撑住，此时左段比分界线宽属预期）。
4. 两段分界线与左栏右缘上下连续；折叠/展开左栏时顶栏左段 0.2s 同步过渡。
5. 会话间多次切换后 ← 可回退、→ 可前进，到边界置灰；关闭中间会话后回退能跳过它直达上一个仍打开的会话；关闭全部会话后两键不因死条目误亮；输入框内 Alt+箭头仍为词间移动。
6. 项目会话显示「目录名」胶囊（悬停完整路径）+ 分支胶囊；临时会话显示「临时会话」、无分支胶囊；非 Git 仓库项目会话同样无分支胶囊。
7. 右栏切换按钮开合右栏并记忆，折叠后右栏完全不可见（无窄轨残留，重开唯一入口是顶栏按钮）；标题与左段分界线之间有间距；native 回退模式下顶栏让位全部撤销、布局不破。

## 6. 追加视觉调整（同批次回访）

1. **标题左侧间距**：`.tb-main-seg` 增 `padding-left: 12px`，标题与左段分界线之间留出呼吸空间。
2. **移除右栏页签行折叠按钮**：RightBar Tabs 的 `tabBarExtraContent.left`（「信息」左侧的 RightOutlined 按钮）删除。
3. **右栏隐藏即完全卸载**：`rightBarOpen=false` 时 RightBar `return null`（[docs/sidebar-toggle-buttons](./sidebar-toggle-buttons.md) 的右栏 36px 窄轨废止，左栏窄轨保留）；顶栏切换按钮成为右栏**唯一开合入口**。
4. **用户消息悬停操作**：悬停用户消息行，气泡右下浮现 复制 / 修改 两按钮（默认 `visibility:hidden + opacity:0` 防透明误点）。复制 = `navigator.clipboard.writeText(原文)`，成功后图标切换 ✓ 反馈 1.2s；修改 = 派发 `ws:composer-fill` 自定义事件，Composer 监听后**文本覆盖草稿**（同队列「编辑」语义）并聚焦到末尾，不自动发送；纯图片消息不渲染按钮。实现：ChatMessages `UserMessage` + Composer 回填监听 + `.user-msg-actions` 样式 + i18n `chat.copy/editInComposer`；新增 `usermsg.actions.test.tsx` 4 用例与 `chat.style.test.ts` 悬浮样式契约。
5. **信息面板删分支行 + 行级字体统一**：Git 段的 `⎇ 分支` 加粗行删除（分支由顶栏胶囊展示，信息面板仅保留「N 处改动 / 非 Git 仓库」）；`.rb-value` 类随之退役；取值行统一为 12px（`.rb-line` 显式补 `font-size: 12px`，与 `.rb-path`/`.rb-dim`/`.rb-todo` 对齐）——标签 11px dim、取值 12px 两档定稿。
6. **右栏默认宽度收窄**：`.right-bar` 468px → **328px**（原值 70%），diff 代码块窄版注释同步。
7. **屏蔽 WebView 默认右键菜单**：AppShell 挂全局 `contextmenu` preventDefault，去掉刷新/检查等浏览器入口（调试走 devtools 快捷键不受影响）；smoke 测试断言 `defaultPrevented`。

影响面：`.right-rail` CSS 块删除；`sidebar.style.test.ts` 契约更新（窄轨断言移除 + 新增「窄轨样式不得回归」反断言）；`app.smoke.test.tsx` 折叠流程用例改走顶栏按钮（折叠后 `.right-rail` 应不存在、经顶栏「展开右侧栏」恢复）；RightBar 清理 `useTranslation`/图标导入。验证 **154/154 全绿** + build 通过（两契约用例合并，总数 -1）。

## 7. 已知取舍

- macOS 折叠左栏时左段 `min-width: 78px` > Sider 36px：分界线短暂不连续，换取红绿灯不遮挡（红绿灯无法随前端布局移动）。
- 分支胶囊数据源随 `openSession` / `run:done` 刷新（与 InfoPanel 一致），run 期间分支切换（worktree 外部操作）不实时感知。
- `histPast/histFuture` 为内存态，重启不恢复（与 Tab prefs 同策略）。
