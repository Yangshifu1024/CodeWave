# 左右侧栏顶部展开/折叠按钮

> 日期：2026-09-03 · 类型：界面批次（轻量路径） · 涉及：`ui/` 纯前端，后端零改动
>
> 需求：左右侧栏分别在顶部增加一个展开和折叠按钮；侧栏开关入口收敛到侧栏自身（顶栏「文件」按钮与 Composer 命令菜单「文件面板」项一并移除）。

## 一、需求分析（轻量路径）

P0 分类：小型 UI 增强 → 轻量路径（不强制 PM / code-reviewer 子代理，[docs/plan-mode-workflow](./plan-mode-workflow.md)）。

**用户故事**：作为用户，我想通过左右侧栏顶部的按钮一键折叠/展开侧栏，让内容区暂时变宽；折叠状态在重启后保持。

**验收标准（AC）**：

1. 左侧栏顶部（内缘）有折叠按钮，点击后整栏（导航 + 工作区文件）缩为 36px 窄轨；窄轨顶部有展开按钮，点击恢复 280px 全栏。
2. 右侧栏顶部（内缘）有折叠按钮，点击后整栏（信息/日志/文件 Tabs）缩为 36px 窄轨；窄轨顶部有展开按钮，点击恢复 468px 全栏。
3. 折叠/展开状态写入 localStorage，应用重启后保持。
4. 顶栏「文件」按钮与 Composer `/` 命令菜单「文件面板」项移除；侧栏顶部按钮成为两侧栏唯一开关入口。
5. 无活动会话（空态）时侧栏按钮依然可用（旧「文件」按钮 `disabled={!activeKey}` 的限制随按钮一并消失）。

**边界**：

- 折叠右栏时日志标签页随组件卸载自然停止轮询，无需显式停。
- 左栏状态复用 `sessions.explorerOpen`（语义即左栏开合），不新造第二份状态。

**非目标**：折叠过渡动画；侧栏宽度拖拽调节；WorkspaceExplorer 内部结构改动。

## 二、草图（交互形态定稿）

```
展开态（两侧栏顶部新增按钮行，‹ › 为折叠按钮，位于内缘）
┌──────────────┬───────────────────┬─────────────┐
│ 临时会话  [‹] │                   │[›] 信息|日志|文件│
│ 项目树        │      内容区        │             │
│──────────────│                   │             │
│ 工作区文件    │                   │             │
└──────────────┴───────────────────┴─────────────┘

折叠态（侧栏缩为 36px 窄轨，展开按钮仍在窄轨顶部）
┌──┬─────────────────────────────┬──┐
│› │                             │‹ │
│  │           内容区             │  │
└──┴─────────────────────────────┴──┘
```

- 左栏展开态按钮 = `LeftOutlined`（向左收起）；窄轨 = `RightOutlined`（向右展开）。
- 右栏展开态按钮 = `RightOutlined`（向右收起）；窄轨 = `LeftOutlined`（向左展开）。
- 箭头方向即侧栏收拢/展开的方向，与视觉动作一致。

## 三、技术方案

### 1. 状态 + localStorage 记忆

| 状态 | 位置 | 初始值 | 记忆键 |
|---|---|---|---|
| `explorerOpen`（左栏开合，复用） | `stores/sessions.ts` | `localStorage["ws_explorer_open"] !== "0"` | `ws_explorer_open` |
| `rightBarOpen`（右栏开合，新增） | `stores/ui.ts`（面板开关惯例归属） | `localStorage["ws_right_bar_open"] !== "0"` | `ws_right_bar_open` |

- 两态各配 action：`setExplorerOpen(open)` / `setRightBarOpen(open)`，setState 同时写 localStorage（沿用 `ws_lang` 先例）。
- zustand store 是模块级单例，初始值仅在 import 时读一次 localStorage；测试 afterEach 须复位 store 字段并清键。

### 2. 组件改动

- `AppShell.tsx`：左栏改为**两种状态共用 `<Sider>` 外壳**（`width` 280↔36 切换）——展开渲染 `.sider-body`（顶部 `.panel-toggle-bar.left` 折叠按钮行 + 原 `.sider-stack`）；折叠渲染 `.sider-rail` 窄轨（顶部展开按钮）。Sider 外壳不可在折叠态卸载（原因见「七、缺陷修复记录」）。顶栏「文件」按钮删除。
- `RightBar.tsx`：`rightBarOpen === false` 时整体渲染 `.right-rail` 窄轨（FileViewerModal 等一并卸载）；展开态在 Tabs 之前加 `.panel-toggle-bar.right` 折叠按钮行。新增 `useTranslation`（aria-label 走 i18n）。
- `Composer.tsx`：命令菜单 `explorer` 项与 `doCommand` 分支删除。

### 3. 样式（app.css）

- `.sider-body`：Sider 内纵向 flex 容器；`.sider-stack` 由 `height:100%` 改 `flex:1; min-height:0`（给按钮行让位）。
- `.panel-toggle-bar`：30px 高固定行，`.left` 右对齐 / `.right` 左对齐（按钮一律朝内容区内缘）。
- `.sider-rail`：窄轨内层布局（纵向居中、按钮钉顶）；宽度/背景/边框由 Sider 外壳的内联样式提供，**不得自带 width**（见七）。`.right-rail` 独立承担 36px 宽 + 左边框（其父级是 Content 行内 flex，不受 has-sider 机制影响）。

### 4. i18n（zh-CN / en-US 同步）

- 新增 `app.collapseLeft/expandLeft/collapseRight/expandRight`（折叠左侧栏 / 展开左侧栏 / 折叠右侧栏 / 展开右侧栏）。
- 删除 `app.explorer`（顶栏按钮文案）与 `commands.explorer`（菜单项文案）；`explorer.*` 命名空间是 WorkspaceExplorer 面板文案，保留不动。
- 按钮带 `aria-label` + `title`（纯图标按钮可达性约定，测试按 aria-label 定位）。

## 四、测试

前端 101/101 全绿（基线 96 + 5），`pnpm --dir ui build` 通过；后端零改动，不跑 cargo。

| 文件 | 变更 |
|---|---|
| `app.smoke.test.tsx` | 顶栏断言去掉「文件」改为断言 `.toolbar` 不含「文件」；新增用例「侧栏折叠」：左栏折叠→`.project-nav`/`.sider-explorer` 消失 + `.sider-rail` 出现 + localStorage 写 `0`，窄轨展开恢复；右栏同理（`.right-rail`、`.rb-tabs` 卸载）；afterEach 复位两 store 字段并清两个 localStorage 键 |
| `rightbar.files.test.tsx` / `rightbar.log.test.tsx` | 直挂 RightBar 需 `import "../i18n"`（折叠按钮 aria-label 走 t()，否则 NO_I18NEXT_INSTANCE 警告）；files 版 afterEach 复位 `rightBarOpen` |
| `sidebar.style.test.ts`（新增） | CSS 契约（仿 titlebar.style.test.ts，node fs 直读 app.css）：两窄轨 36px 宽 + 边框分隔、`.panel-toggle-bar` 30px 行不伸缩、`.sider-stack` 让位改法 |

## 五、手动验证清单（分步）

1. `pnpm tauri dev` 启动（先确认无其它 dev/打包实例在跑，单实例互斥）。
2. 默认两侧栏展开：左栏顶部右缘有 `‹` 按钮、右栏顶部左缘有 `›` 按钮，hover 有 tooltip（折叠左侧栏/折叠右侧栏）。
3. 点左栏 `‹`：整栏缩为窄轨（导航 + 工作区文件消失），**聊天区与右侧栏不受影响**，窄轨顶部 `›` 可展开恢复。
4. 点右栏 `›`：Tabs 整栏缩为窄轨，窄轨顶部 `‹` 可展开恢复；折叠前若停在「日志」页，展开后回到信息页属预期（rbTab 局部状态随卸载复位）。
5. 折叠右栏后日志不再轮询（日志文件无持续追加）。
6. 完全重启应用：两侧栏保持上次折叠状态（localStorage 记忆）。
7. 顶栏无「文件」按钮；Composer 输入 `/` 打开命令菜单，列表无「文件面板」项，其余命令（压缩/Git/变更/任务/统计）正常。
8. 空态（无任何会话）时左右栏按钮均可切换（不受旧 `disabled` 限制）。
9. 浅色/深色主题切换下窄轨边框与按钮均可见；Windows Snap Layout 与 macOS 红绿灯布局下顶栏不受影响（本批未动顶栏结构）。

## 六、遗留与后续

- 无。折叠动画与宽度拖拽调节为非目标，如有需要另行立项。

## 七、缺陷修复记录（交付后反馈）

**现象**：点击左栏折叠按钮后，右侧栏连同聊天内容区被完全挤压消失（「左侧栏折叠时会把右侧栏完全折叠」）。

**根因**（antd 5.27 源码证据）：

- `.ant-layout` 默认 `flex-direction: column`（`antd/es/layout/style/index.js`），仅 `ant-layout-has-sider` 变体为 `row`；
- `useHasSider`（`antd/es/layout/hooks/useHasSider.js`）只认**真实 `<Sider>` 子元素**（`node.type === Sider`）；
- 首版实现折叠态用普通 `<div class="sider-rail">` 顶替 `<Sider>` → has-sider 判定失败 → 内层 Layout 翻转为纵向 flex → `.sider-rail`（`height:100%` + `flex:none`）占满整列，`Content`（`flex:auto` + `min-height:0`）收缩到 0 高，内容区与右栏全部消失。

**修复**：Sider 外壳两种状态恒保留，`width` 280↔36 切换，内部按状态换渲染 `.sider-body` / `.sider-rail`；`.sider-rail` 退为内层布局类。

**回归守护**：

- 冒烟用例新增断言：左栏折叠态下 `.ant-layout-has-sider` 仍在、`.ant-layout-sider` 内联样式含 36px（旧实现下必红）；
- CSS 契约测试改判 `.sider-rail` 不得自带 `width`（宽度必须来自 Sider width prop，防止结构再滑回普通 div）。

修复后前端 101/101 全绿、`pnpm --dir ui build` 通过。

## 八、后补：右栏折叠动画对齐

首版交付后左栏折叠有动画（antd Sider 自带 `transition: all 0.2s`，`sider.js:29`）、右栏瞬时切换。对齐方案（纯 CSS，无 JS 动画逻辑）：

- `RightBar` 根 div 在两态间**持久换类**（`.right-bar` ↔ `.right-rail`；两个分支都是同组件的根元素，React 复用同一 DOM 节点，只换 className 与子树），两类均声明 `transition: all 0.2s`（与 antd `motionDurationMid` 同节奏），width 468↔36 由 CSS 补间；
- `.right-bar` 补 `overflow-x: hidden`：展开动画的 200ms 内 Tabs 内容（约 440px）宽于增长中的外壳，显式裁剪防止 `overflow-y: auto` 把 overflow-x 隐式算成 auto 而闪横向滚动条；
- 内容切换时机与左栏一致：点击瞬间换内容、外壳宽度补间。

回归守护：`sidebar.style.test.ts` 断言两类 transition 与 overflow-x 裁剪。

## 九、后补：右栏折叠按钮并入标签行

首版实现把右栏折叠钮放在 Tabs 之上独立的一行（`.panel-toggle-bar.right`），多占一行高度。按反馈改用 antd Tabs `tabBarExtraContent={{ left: … }}`——按钮渲染进 `.ant-tabs-nav` 的 `.ant-tabs-extra-content`，位于「信息」页签之前，与草图 §二 一致。`.panel-toggle-bar.right` 样式随之删除（左栏仍用独立行：其顶部无标签行可挂靠）；间距规则 `.rb-tabs .ant-tabs-extra-content { margin-right: 8px }`。冒烟断言 `.rb-tabs .ant-tabs-extra-content button[aria-label="折叠右侧栏"]` 在场防回退。
