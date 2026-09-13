# Dropdown 选中项高亮色中性化

> 批次内容：所有 Dropdown/Menu/Select 选中项背景由墨色主色衍生深灰块改为「比 hover 深一档」的中性填充。纯 token 层修改，无组件/事件契约变化。

## 现象与根因

Composer 三个下拉（权限模式 / 模型 / 思考强度）及全应用一切 `Dropdown menu={...}` 的选中项高亮来自 antd 默认 token 链（`ui/src/features/chat/Composer.tsx` 仅靠 `selectedKeys` 交由 antd 加 `.ant-dropdown-menu-item-selected`，项目自身从未写过选中背景样式）：

- 选中项背景 = `controlItemBgActive` = `colorPrimaryBg`（`antd/es/theme/util/alias.js:78`）——即从墨色主色 `#1f1f1f` 调色板衍生的第 1 档 tint。antd 调色板对近黑主色的提亮步幅很小，该衍生值落在中深灰（≈ #4c4c4c 一带），呈现为截图中的深灰块；选中文字仍取 `color: colorPrimary`（墨色），深底深字对比很差。
- [docs/sidebar-collapse-animation-and-titlebar-blend](./sidebar-collapse-animation-and-titlebar-blend.md) 曾把 `colorPrimaryBgHover` 置 `transparent` 以消灭「选中+悬停」叠层发黑，但该 token 只派生 `controlItemBgActiveHover`（悬停选中层），静止选中背景未修；且副作用是悬停选中项时背景直接透明消失（普通条目 hover 有浅灰底、选中条目悬停反而无底）。

## 修复

`ui/src/App.tsx` `themeCfg.token` 追加两个显式 alias token 覆盖（机制同 [docs/focus-ring-fix](./focus-ring-fix.md) 的 `controlOutline`：alias token 在 `formatToken` 末尾被用户 token 覆盖，全局生效；已核对 `antd/es/theme/util/alias.js` 末尾 `...overrideTokens`）：

```ts
controlItemBgActive: osDark ? "rgba(255,255,255,0.10)" : "rgba(0,0,0,0.08)",
controlItemBgActiveHover: osDark ? "rgba(255,255,255,0.14)" : "rgba(0,0,0,0.10)",
```

- 三档递进（亮色）：hover `rgba(0,0,0,0.04)` ≈ #f5f5f5 → 选中 `rgba(0,0,0,0.08)` ≈ #eaeaea → 悬停选中 `rgba(0,0,0,0.10)` ≈ #e6e6e6。选中档与项目既有「选中比 hover 深一档」先例 `.ask-opt.picked`（app.css `rgba(127,127,127,0.16)`，合成后同为 #eaeaea）视觉等值。
- [docs/sidebar-collapse-animation-and-titlebar-blend](./sidebar-collapse-animation-and-titlebar-blend.md) 的 `colorPrimaryBgHover: "transparent"` 原样保留（button filled variant / steps 仍在直接消费该 token），仅菜单悬停选中层不再走它的 transparent 派生，恢复「再深半档」的正常悬停反馈。
- 选中文字色不变（墨色 `colorPrimary`，浅灰底上可读）；权限模式条目的橙/红标题着色（app.css 只着色 `> span:not(.desc)`）不受影响。

## 影响面

`controlItemBgActive` 是 antd 全局 alias token，消费方含 dropdown / menu / select / cascader / transfer 等的选中面——本项目内即 Composer 三下拉 + 设置面板等所有 Select 下拉选中项，统一变为中性浅灰，符合「强调色 = 中性墨色、不引入彩色 accent」的全局语言（[docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md)）。暗色模式同比例（白 α 0.10/0.14）自适应。

## 验证

- `pnpm --dir ui test`：254/254 全绿（含权限菜单/模型菜单/力度菜单契约用例）。
- `pnpm --dir ui build`：type check + vite build 通过。
- 手动验证清单（界面不做 GUI 自动点验）：
  1. Composer 权限模式下拉：当前档（如「计划模式」）背景应为浅灰 #eaeaea 一带，明显浅于截图旧态；
  2. 悬停任意未选中条目 → #f5f5f5；悬停选中条目 → #e6e6e6（不再透明消失）；
  3. 模型下拉 / 思考强度下拉 / 设置弹窗各 Select：选中项同样浅灰；
  4. 系统切暗色主题后重复 1–3，选中项为深色底上的白色 10% 中性叠加。
