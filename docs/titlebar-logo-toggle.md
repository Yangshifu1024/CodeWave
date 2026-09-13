# 标题栏批次：会话历史导航移除 + Logo 兼作左栏开合入口 + 折叠宽 36→48

> 批次报告。三项纯前端改动，全部落在标题栏与左栏交界处：移除 [docs/titlebar-content-batch](./titlebar-content-batch.md) 的会话历史导航（按钮 + 双栈 + 快捷键全链路）、左栏开合入口从侧栏内部收口到标题栏 Logo（悬停交换箭头）、折叠宽度 36→48 使 Logo 左右等距。

## 1. 会话历史导航整体移除

[docs/titlebar-content-batch](./titlebar-content-batch.md) 实现的顶栏 ←/→ 会话历史导航（浏览器语义双栈 + Alt+←/→）按下述清单全链路拔除，不留死代码：

- `TopBar.tsx`：删除 `.tb-nav` 容器（后退/前进两 `Button` + Tooltip）与 `canBack`/`canForward` 禁用态选择器（按「栈内存在仍打开的会话」判定的窄订阅）。
- `sessions.ts`：`SessionsState` 删除 `histPast` / `histFuture` / `navBack` / `navForward` 四字段（声明 + 实现）；删除模块级 `navJumping` 标志（nav 自身跳转不录制的开关）、`popValid()`（弹栈跳过已关闭会话）、文件尾部「会话历史录制」`useSessions.subscribe` 块（activeKey 变化即录 prev 入 past、清 future）。未读点清除 subscribe 的注释中 navBack/navForward 措辞同步移除（不变式本身不动——activeKey 变化即清，入口收口语义不受影响）。
- `AppShell.tsx`：全局 keydown 删除 `Alt+←/→` 分支（含「输入框内不劫持」closest 豁免——该豁免为历史导航专属，`Cmd/Ctrl+←/→` 切 Tab 不受影响）。
- `i18n`：`titlebar.back` / `titlebar.forward` 双语键删除（`titlebar.tempSession` 保留）。
- 测试：`titlebar.content.test.tsx` 删除 `describe("会话历史导航（顶栏 ←/→）")` 四用例与 `describe("Alt+←/→ 快捷键")` 一用例，`afterEach` 清栈语句与 `AppShell` / `act` 导入随之清理。

## 2. 左栏开合入口收口到标题栏 Logo

**交互定义**：Logo 常态显示；鼠标悬停时 Logo 淡出、箭头同位浮现（展开态 = `LeftOutlined`「折叠左侧栏」，折叠态 = `RightOutlined`「展开左侧栏」），带 `--ws-hover` 底色反馈与 Tooltip；点击切换 `explorerOpen`。悬停期间 Logo 不在、仅箭头在场，语义即「点这个位置会发生什么」。

**实现**：

- `TopBar.tsx`：logo 图片包进 `<button class="tb-logo-toggle">`（24×24、`position:relative`），内部叠放 `<img.tb-logo>` + 按 `explorerOpen` 切换的 `<LeftOutlined/RightOutlined .tb-logo-swap>`；aria-label / Tooltip 复用现有 i18n `app.collapseLeft` / `app.expandLeft`（文案不变，消费方从侧栏移到标题栏）；onClick 调 `setExplorerOpen(!explorerOpen)`。button 不带 `data-tauri-drag-region`，点击归按钮、周边空白仍可拖窗（drag.js 只认 target 自身标注，与段内其他按钮同机制）。
- `app.css`：新增 `.tb-logo-toggle`（透明背景 + 圆角 6 + `cursor:pointer`）与悬停交换三规则（`.tb-logo` opacity→0 / `.tb-logo-swap` opacity→1 / 容器 `--ws-hover` 底色），0.15s 过渡；同位交换而非并排按钮是折叠态的唯一解——48px 窄轨容不下「Logo + 按钮」并排。
- `AppShell.tsx`：删除展开态 `.sider-body` 顶部 `.panel-toggle-bar.left` 按钮行与折叠态 `.sider-rail` 顶部展开按钮；`SiderRailFoot`（头像 + 任务/统计/设置纵排）保留为窄轨唯一内容，`.sider-nav` 成为 `.sider-body` 首子元素（flex:1 天然占满，顶部 30px 按钮行让位逻辑随之消失）。
- `app.css` 同步清理：`.panel-toggle-bar` 两规则、`.tb-nav` 规则、`.sider-rail` 的 `padding-top:6px`（顶部按钮移除后无消费）删除。
- `titlebar.style.test.ts` 新增契约：`.tb-logo-toggle` relative + pointer、`:hover .tb-logo` opacity:0、`:hover .tb-logo-swap` opacity:1——锁住悬停交换行为；`sidebar.style.test.ts` 删除 `.panel-toggle-bar` 30px 按钮行契约（DOM 与样式均已不存在）。

## 3. 折叠宽度 36 → 48（Logo 左右等距）

折叠态 `.tb-left-seg` 宽度原为 36 = 12px 段内边距 + 24px Logo，右间距 0；左栏 Sider 同吃该值（`SIDER_W_CLOSED` 常量 TopBar/AppShell 共享，上下分界线连续）。改为 **48 = 12 + 24 + 12**，Logo 左右各 12px 等距，左右侧栏视觉重量对称。`transition: width 0.2s` 不变，折叠/展开仍与 Sider 同步过渡。

配套调整：native 回退规则 `padding-left: 10px → 12px`，等距在原生标题栏回退模式下同样成立（展开态视觉差 2px 无感）；macOS 红绿灯让位规则（`min-width: max(78px,…)`）与注释中 36px 表述同步更新——红绿灯让位优先于等距的取舍不变（[docs/titlebar-content-batch](./titlebar-content-batch.md) 既有决策）。

## 测试影响

- `titlebar.content.test.tsx`：「左段宽度对齐」重写为——展开 280 / 折叠 48、`.tb-left-seg .tb-logo-toggle` 的 aria-label 随 explorerOpen 切换（折叠左侧栏/展开左侧栏）、`.tb-nav` 不存在；新增「Logo 点击翻转 explorerOpen 并写 `ws_explorer_open` localStorage」。
- `app.smoke.test.tsx`：侧栏折叠用例改为经标题栏 Logo 开合（选择器加 `.tb-left-seg` 前缀），Sider style 断言 `36px → 48px`。
- 净变化 168 → 164（删 5 增 1）。

## 验证

- `pnpm --dir ui test`：164/164 全绿
- `pnpm --dir ui build`：type check + vite build 通过
- 后端零改动（`cargo test` 不涉及）

## 手动验证清单

1. 顶栏左侧不再有 ←/→ 历史导航按钮；按 Alt+←/→ 不再切换会话（文本输入内外的语义一致，无快捷键冲突提示）。
2. 左栏展开态：鼠标移到标题栏 Logo 上 → Logo 淡出、出现 ← 箭头 + hover 底色 +「折叠左侧栏」Tooltip；点击 → 左栏与标题栏左段同步 0.2s 过渡折叠。
3. 折叠态：左栏与标题栏左段同为 48px，Logo 距左右边缘各 12px 等距；悬停 Logo → 出现 → 箭头 +「展开左侧栏」；点击 → 恢复展开。
4. 折叠/展开状态重启应用后保持（localStorage `ws_explorer_open`）。
5. 折叠态窄轨底部头像 / 任务 / 统计 / 设置四入口仍可用。
6. 标题栏 Logo 周边及两段空白处拖窗正常，Logo 点击不触发拖拽。
7. 会话切换（点击左栏行、Ctrl+±Tab、Ctrl+W 关 Tab 邻位切换）行为正常，未读点清除不受影响。
