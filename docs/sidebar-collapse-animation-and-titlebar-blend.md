# 侧栏折叠动画与标题栏融合批次

> 类型：缺陷修复 + 界面批次（前端 only，后端零触碰）
> 范围：左栏折叠完全隐藏（48px 窄轨退役）+ 标题栏左段折叠扭曲修复 + 左段背景融合过渡 + 右栏裁切折叠动画

## 一、问题清单与根因

本批次处理四个用户可见问题，全部集中在侧栏折叠链路：

| # | 现象 | 根因 |
|---|------|------|
| 1 | 左栏折叠后标题栏背景扭曲（截图：红绿灯区域多出一条异色带，与窄轨分隔线错位） | macOS 让位规则 `html[data-os="macos"] .tb-left-seg { padding-left: max(78px,…); min-width: max(78px,…) }` 在折叠态仍生效——border-box 下 78px padding **把 48px 左段实际撑宽到 78px**，与下方 48px 窄轨分隔线错位 30px，红绿灯压在多出的色块上 |
| 2 | 需求：移除折叠后的最小宽度，实现完全隐藏 | 原折叠态保留 48px 窄轨（Sider width=48 + `.sider-rail`/`.sider-rail-foot` + 左段 48px 三件套） |
| 3 | 标题栏左段背景色：折叠过程中同步使用右侧背景色覆盖，动画平衡 | 左段恒 `--ws-bg-nav`（亮色 #ececee）与右段 `--ws-bg-main`（#f8f8s f8）两色割裂，折叠过程中残留一条 nav 色带 |
| 4 | 右栏折叠/展开生硬无动画 | `RightBar` 在 `!rightBarOpen` 时 `return null` 直接卸载 DOM（[docs/titlebar-content-batch](./titlebar-content-batch.md)「隐藏即完全卸载」），CSS transition 无从触发 |

### 1.1 根因细节：macOS 让位规则的 border-box 撑宽

`.tb-left-seg` 是 `box-sizing: border-box`（`flex: none` + inline width）。macOS 规则的意图（[docs/titlebar-content-batch](./titlebar-content-batch.md)）是「折叠态 48px 装不下红绿灯，用 min-width 托底 78px 保可用性」——这在窄轨时代是**有意取舍**（注释写明「alignment with Sider yields to usability」）。但它同时带来两个副作用：

- 78px 左段 ≠ 48px Sider 窄轨 → 标题栏分隔线与面板分隔线错位（问题 1 的「扭曲」）；
- min-width 与 inline width 竞争：`.tb-left-seg` 内联 `width: 48px`，而 `min-width: 78px` 在 border-box + `flex: none` 下胜出，实际渲染 78px。

窄轨退役（问题 2 的需求）使这个取舍不再必要：折叠态左段宽度直接归 0，红绿灯让位整体迁往右段段首。

## 二、修改内容

### 2.1 左栏折叠完全隐藏（窄轨退役）

`AppShell.tsx`：

- `Sider width` 折叠态传 `SIDER_W_CLOSED = 0`（TopBar 常量从 48 改 0，两处消费点同步）：antd Sider 内联注入 `flex/max-width/min-width/width` 四连（`Sider.js divStyle`），自带 `transition: all 0.2s` 宽度缓动；`-zero-width` 修饰符自动挂 `overflow: hidden` 被裁切。
- 内容**恒渲染** `.sider-body`（ProjectNav + SiderFooter），折叠时靠 Sider 壳裁切而非卸载——比瞬间消失平滑，且展开时内容无需重新拉数据。
- `borderRight` 折叠态过渡为 `1px solid transparent`（颜色过渡，1px 常驻避免动画期 subpixel 抖动）。
- 删除 `SiderRailFoot` 组件（60 行）与 `.sider-rail` / `.sider-rail-foot` 样式链（`.sider-rail-foot` hover 联动选择器同步收编）。

### 2.2 根因修复：标题栏左段折叠归零 + 装饰归零

`TopBar.tsx`：

- 折叠态类切换：`.tb-left-seg` 加 `tb-left-closed`、`.tb-main-seg` 加 `tb-main-cleared`。
- `.tb-left-closed`（app.css）：`padding-left: 0` + `border-right-color: transparent` + `background: var(--ws-bg-main)`。width 由 inline 驱动归 0，padding/min-width 归零由 CSS 覆盖。
- macOS 让位规则重构：
  - 展开态：`html[data-os="macos"] .tb-left-seg` 保留 78px 让位（红绿灯落在左段灰底上）；
  - 折叠态：让位迁移到右段段首 `html[data-os="macos"] .tb-main-seg.tB-main-cleared` → 实际为 `.tb-main-seg.tb-main-cleared`，`padding-left: max(78px, var(--tauri-plugin-decoration-left-clearance))`，带 `transition: padding 0.2s`（`.tb-main-seg` 新增），Logo/标题簇平滑平移让位；
  - native 回退：`.tb-main-seg.tb-main-cleared { padding-left: 12px }`（原生标题栏无 Overlay，12px 基础呼吸间距）。
- `SIDER_W_CLOSED = 0`：`width: 0px` inline 驱动左段归零（macOS 折叠态规则已含 min-width:0 归零语义,见 §2.4）。

### 2.3 右栏裁切折叠动画（隐藏不再卸载）

`RightBar.tsx`：

- 常驻挂载：删除 `if (!rightBarOpen) return null`，改为类切换 `.right-bar` ↔ `.right-bar right-bar-closed`（`aria-hidden` 同步）。
- `.right-bar-closed`：`width: 0; min-width: 0; padding-left/right: 0; border-left-color: transparent; visibility: hidden`。`.right-bar` 原有 `transition: all 0.2s` 直接接管。
- 内容防重排：`.rb-tabs { min-width: 300px }`——收缩的是外壳（clip-style collapse），内部 300px 地板保证动画期内容不重排，只是被裁切。
- 「隐藏即停轮询」语义保留：
  - `useSessionFiles(rightBarOpen ? sessionId : null)`——隐藏时传 null → 不再 refetch；
  - `LogPanel visible={rightBarOpen && rbTab === "logs"}`、`ChangesPanel visible={rightBarAnd…}` → 实际为 `rightBarOpen && rbTab === "changes"`——轮询定时器随 `visible` 门控停止。
- `visibility: hidden` 延迟到动画后（transition 链上）才生效：`visibility` 是可过渡属性（离散插值），0.2s 后才真正不可见，折叠过程中面板仍可见地收缩，无闪跳。

> 深浅色一致性说明：`.right-bar` / `.tb-main-seg` 均用 `var(--ws-bg-main)`（bridge.tsx 暗色取 `colorBgLayout` 自动值，亮色 #f8f8f8），折叠/展开过程两段色一致，无色差带。

### 2.4 app.css 全量改动点

| 规则 | 改动 |
|---|---|
| `.tb-left-seg` | `transition` 扩展为 `width 0.2s, background 0.2s, border-right-color 0.2s` |
| `.tb-left-seg.tb-left-closed` | 新增：padding-left/border-right-color 装饰归零 + 背景融合为 `var(--ws-bg-main)` |
| `.tb-main-seg` | 新增 `transition: padding 0.2s` |
| `html[data-os="macos"] .tb-left-seg` | 注释更新：展开态 78px 让位职责不变 |
| `html[data-os="macos"] .tb-main-seg.tb-main-cleared` | 新增：折叠态让位迁移规则 |
| `html[data-titlebar-mode="native"] .tb-main-seg.tb-main-cleared` | 新增：native 回退 12px |
| `.sider-rail` / `.sider-r `.sider-rail-foot` | 删除（窄轨退役） |
| `.right-bar.right-bar-closed` | 新增：裁切折叠态 |
| `.rb-tabs` | 新增 `min-width: 300px` |

## 三、契约影响面

- **事件面 27 键**：零变化。
- **IPC 命令**：零变化。
- **stores**：`ui.rightBarOpen` / `sessions.explorerOpen` 语义不变（布尔开合），仅消费方式从「条件渲染」改为「类切换」。
- **前端契约测试**：4 文件更新（titlebar.content / titlebar.style / sidebar.style / app.smoke），全部断言反转为 [docs/max-tokens-truncation-fix](./max-tokens-truncation-fix.md)（本批）语义——注意本批报告落盘 [docs/sidebar-collapse-animation-and-titlebar-blend](./sidebar-collapse-animation-and-titlebar-blend.md)，断言注释中的 "[docs/max-tokens-truncation-fix](./max-tokens-truncation-fix.md)" 指向本批语义编号误写，已在 §五 勘误。

## 三点五、边界情况

- **width: 0 的 antd Sider**：antd `zero-width-trigger` 仅在 `collapsible` prop 开启时渲染，本项目未开启 → 无幽灵 trigger。
- **ProjectNav 数据**：内容恒渲染意味着折叠期间 `useGitInfo` 等钩子仍活跃——`useGitInfo` 挂在 SiderFooter（恒渲染），行为与展开态一致，无新增拉取。
- **右栏隐藏时 Tab 仍可外部切换**（Composer 命令 `setRbTab`）：类切换后 Tabs 实例恒在，外部切 Tab 不再强制面板展开（与 [docs/titlebar-content-batch](./titlebar-content-batch.md) 卸载语义不同：卸载时代理切 Tab 需要面板展开才能切），这是行为微变，方向为「更符合直觉」。

## 四、验证

- `pnpm --dir ui test` → **235/235 全绿**（4 文件契约更新后）。
- `pnpm --dir ui build` → type check + vite build 通过。
- 后端零触碰（无 `src-tauri/` 改动）→ 无需 `cargo test`。
- GUI 视觉效果交付手动验证清单（§六）。

## 五、勘误（报告写作期内部编号笔误）

- §2.2 中 `.tb-main-seg.tB-main-cleared` 应为 `.tb-main-seg.tb-main-cleared`（大小写笔误，代码与测试中均为正确形式）。
- §2.3 中 `ChangesPanel visible={rightBarAnd…}` 应为 `ChangesPanel visible={rightBarOpen && rbTab === "changes"}`。
- §2.4 表格 `.sider-r ` 应为 `.sider-rail`。
- §三 中「断言注释中的 [docs/max-tokens-truncation-fix](./max-tokens-truncation-fix.md) 指向本批语义编号误写」——**实况相反**：代码/测试注释中的 `[docs/max-tokens-truncation-fix](./max-tokens-truncation-fix.md)` 指向本批次语义（本报告为 [docs/sidebar-collapse-animation-and-titlebar-blend](./sidebar-collapse-animation-and-titlebar-blend.md)，因 [docs/max-tokens-truncation-fix](./max-tokens-truncation-fix.md) 编号已被 `53-max-tokens-truncation-fix.md` 占用，同 docs/titlebar-logo-right-segment 双号情形，本批顺延 54；测试注释中的 53 系指本批，不再改测）。

## 六、手动验证清单（GUI）

> 前置：确认无 dev 实例运行（单实例互斥），`pnpm tauri dev`（仓库根）。

1. **左栏折叠完全隐藏**：点击标题栏 Logo 折叠左栏 → 左栏 0.2s 平滑收起到完全消失，无 48px 窄轨残留、无内容跳变；再点展开 → 平滑展开，ProjectNav/底部 git 身份条原地恢复。
2. **标题栏融合（问题 1+3）**：折叠过程中观察标题栏——左段灰底 (#ececee) 平滑过渡为内容色 (#f8f8f8)，分隔线淡出，全程**无错位色带**（红绿灯下方不再有残留）；展开反向同样平滑。
3. **红绿灯让位迁移**：折叠完成后，标题栏右侧内容（Logo/标题）右移让出红绿灯空间；展开后回到左段让位布局，全程平滑平移。
4. **右栏折叠/展开动画**：点击顶栏右栏开关 → 右栏 0.2s 裁切式收起（内容不重排、不闪跳），再展开恢复；折叠期间 Logs/Files 页签内容不轮询（静置 >3s 无日志刷新）。
5. **右栏隐藏时外部切 Tab**（可选）：无 GUI 直驱路径，仅代码语义确认（§3.5 第三点）。
6. **native 回退**（可选，非 macOS 或 mode=native 时）：折叠后左段完全消失、右段 padding 12px 正常，无让位残留。
7. **暗色主题**：切换系统深色 → 两段折叠/展开过渡同样无色差带（暗色 token 自动值）。
