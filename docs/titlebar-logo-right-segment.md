# 标题栏 Logo 移至右段段首

> 批次类型：UI 微调（轻量路径）。将标题栏中左侧栏同色段上的 Logo（左栏开合入口）移动到右段段首、标题元素之前；左段退化为纯背景带，两段式背景契约不变。

## 需求

标题栏左段（`.tb-left-seg`）与左侧栏同色（`--ws-bg-nav`）同宽（280/48 跟随 Sider 开合），用户在视觉上将其认知为「左侧栏的一部分」，要求把其中的 Logo 移动到「标题元素前」——即右段（`.tb-main-seg`）段首。

## 改动设计

- **Logo 开关整体迁移**：`tb-logo-toggle`（Logo + 悬停同位交换箭头，[docs/titlebar-logo-toggle](./titlebar-logo-toggle.md) 交互）从左段移入右段第一个子元素位置（标题/胶囊簇之前）。左栏开合功能、`aria-label`、Tooltip、悬停交换交互全部保留。
- **左段退化为纯背景带**：`tb-left-seg` 改为自闭合 `<div />`，仅保留背景色 + 1px 右缘分隔线 + 280/48 宽度过渡（与 Sider 同步），拖拽区标记保留。
- **折叠宽 48 依据更新**：原「12px padding + 24px Logo + 12px 右隙，Logo 水平居中」改为「12 + 24 + 12 方形空带，匹配 Sider 窄轨」——数值不变，注释与语义对齐（docs/titlebar-logo-right-segment）。
- **macOS 交通灯让位零变化**：`html[data-os="macos"] .tb-left-seg` 的 78px clearance / native 回退规则仍作用于左段（纯背景带照常让位）；Logo 迁至右段后不再与 macOS 红绿灯同段，视觉拥挤天然缓解。
- **Windows 控制条让位零变化**：右段尾部 `padding-right: max(10px, right-clearance)` 不动；段首新增 Logo 不影响尾部让位。

## 影响文件

| 文件 | 改动 |
|---|---|
| `ui/src/features/shell/TopBar.tsx` | Logo 开关移至右段段首；左段自闭合；头注释与常量注释同步 |
| `ui/src/theme/app.css` | `.tb-logo-toggle` / native 回退注释同步（规则本体零变化） |
| `ui/src/__tests__/titlebar.content.test.tsx` | Logo 开关选择器 `.tb-left-seg` → `.tb-main-seg`；用例名与注释更新 |
| `ui/src/__tests__/app.smoke.test.tsx` | 侧栏折叠用例两处点击选择器同步；过时注释更新 |

## 契约影响面

- 事件面 27 键 / IPC 命令 / store 结构：**零变化**（纯渲染位置调整）。
- `SIDER_W_OPEN=280 / SIDER_W_CLOSED=48` 常量及 AppShell 消费：**零变化**。
- 样式契约测试（titlebar.style.test.ts）全部断言仍成立：左段过渡/分隔线、右段 flex+min-width、clearance 规则顺序、Logo 同位交换——无需改动。

## 验证

- `pnpm --dir ui test`：226 passed / 0 failed（33 个文件）。
- `pnpm --dir ui build`：type check + vite build 通过。

## 手动验证清单（GUI 改动不做自动点验，由用户验证）

1. 展开左栏：Logo 出现在标题栏右段段首（标题左侧），左侧栏顶部无 Logo。
2. 点击 Logo：左栏折叠为 48px 窄轨，Logo 悬停显示 → 箭头；再点击展开，悬停显示 ← 箭头。
3. 悬停 Logo：图标淡出、箭头同位浮现、有 hover 背景（[docs/titlebar-logo-toggle](./titlebar-logo-toggle.md) 交互不变）。
4. 左栏开合动画期间：标题栏左段宽度与左栏同步过渡，分隔线连续。
5. macOS：普通窗口与最大化状态下交通灯均与左段垂直居中（[docs/oss-prep-batch](./oss-prep-batch.md) 前后批次修复不受影响）；Logo 不与红绿灯重叠。
6. 亮色/暗色主题：左段灰色带与右段背景分色正常。
7. 拖拽：左段空白处、右段标题与 Logo 之间空白处可拖动窗口；点击 Logo/标题/胶囊不误触拖拽。
