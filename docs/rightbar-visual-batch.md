# 右栏视觉批次：侧栏分色 + 页签调序 + 日志滚动根修复 + 信息面板排版

> 批次日期：2026-09-03 · 分支：`feat/rightbar-visual-batch`（独立 worktree）· 纯前端，后端零改动
> 编号说明：分支内曾以 38 落盘，合并时 [docs/session-nav-new-top](./session-nav-new-top.md) 已被 session-nav 批次（a160a26）占用，按仓库先例改号 40 避让。

## 1. 需求与落点

| # | 需求 | 落点 |
|---|---|---|
| 1 | 左侧栏背景 `#ececee` | 新 token `--ws-bg-nav`（亮 `#ececee` / 暗 `colorBgContainer`），AppShell Sider 引用 |
| 2 | 中栏+右栏背景 `#f8f8f8` | 新 token `--ws-bg-main`（亮 `#f8f8f8` / 暗 `colorBgLayout`），Content + `.right-bar`/`.right-rail` 引用 |
| 3 | 页签顺序：信息/日志/文件/变更 | RightBar items 重排；默认 `rbTab="info"` 恰为新首项，无迁移成本 |
| 4 | 信息面板中文字体乱 | 全 sans 化 + 字号收敛两档（标签 11px / 正文 12px）+ 去 letter-spacing |
| 5 | 日志区域仍无法滚动 | 根因 = 10f11fc 误删 `.rb-tabs` 根高度约束 → 恢复并加契约测试锁定（§3） |

## 2. 侧栏分色（亮色定稿色，暗色回自动）

- `bridge.tsx` 新增 `isDarkBase()`：按 `token.colorBgBase` 亮度判定当前算法亮暗（十几行，无新依赖）。
- 固定色只在亮色算法生效；暗色取 `colorBgContainer`/`colorBgLayout`（即改前 `--ws-panel`/`--ws-bg` 的值），**暗色外观与改前完全一致**。
- 顶栏 Header 保持 `--ws-panel` 不在本次需求内，维持白色。

## 3. 日志滚动缺陷根因（10f11fc 回归）

10f11fc 把高度链从 antd5 旧类名迁到 antd6 `body-holder` 时，将 `.rb-tabs { flex:1; min-height:0 }` 一并删除且未恢复：

1. `.right-bar` 为 flex column + `overflow-y:hidden`（滚动权下放各页签内部容器）；
2. `.rb-tabs` 是其唯一 flex 子项，列向 flex 的 `min-height:auto` 解析为 min-content——Tabs 根拒绝收缩到内容以下；
3. body-holder 高度随之被内容撑高，`overflow-y:auto` 永不触发；
4. 溢出被 `.right-bar` 的 `overflow-y:hidden` 直接裁掉 → 全链无滚动条（**变更页签的 diff 滚动同样被卡**）。

修复：恢复根约束，连带恢复 10f11fc 误删的 nav 外观微调（nav `margin-bottom:10px`、页签 `padding:5px 2px; font-size:12px`、extra 右距 8px）。防再犯：`sidebar.style.test.ts` 新增 `.rb-tabs` 根约束契约与右栏背景 token 契约。

## 4. 信息面板排版

成因三件套：`.rb-label` 10.5px + `letter-spacing:0.05em`（中文加字距显松散）；`.rb-path` 走 mono 槽（Latin 等宽字体无中文字形，含中文目录时逐字回退混排错乱）；同屏 10.5/11/11.5/12 四档字号。

改法：`.rb-label` → 11px 去 letter-spacing；`.rb-dim` → 12px；`.rb-path` → 去 mono 改继承 sans、12px（保留 `word-break:break-all` 折长路径）。结果：信息面板全 sans、两档字号。`.rb-dim` 亦为变更/文件页签使用，统一后全局一致。

## 5. 变更文件

| 文件 | 改动 |
|---|---|
| `ui/src/theme/bridge.tsx` | `--ws-bg-nav` / `--ws-bg-main` 双值 token + `isDarkBase()` |
| `ui/src/features/shell/AppShell.tsx` | Sider → `--ws-bg-nav`；Content → `--ws-bg-main` |
| `ui/src/theme/app.css` | `.right-bar`/`.right-rail` 背景；`.rb-tabs` 高度链恢复；`.rb-label`/`.rb-dim`/`.rb-path` 排版 |
| `ui/src/features/shell/RightBar.tsx` | items 重排（信息/日志/文件/变更）+ 头注释 |
| `ui/src/__tests__/app.smoke.test.tsx` | 首项断言「变更」→「信息」，存在列表改 日志/文件/变更 |
| `ui/src/__tests__/sidebar.style.test.ts` | 新增 `.rb-tabs` 根约束 + 右栏背景 token 两条契约 |
| `AGENTS.md` | 必读文档索引追加本条 |

## 6. 验证

- `pnpm --dir ui test`：全绿（含新增两条契约用例，结果见提交时点）。
- `pnpm --dir ui build`：type check + vite build 通过。
- 手动验证清单（界面改动不做 GUI 自动点验）：
  1. 亮色模式：左栏 `#ececee`，中栏/右栏 `#f8f8f8`，顶栏仍为白色；
  2. 系统切暗色：三栏回自动深色，与改前暗色一致；
  3. 右栏页签顺序：信息/日志/文件/变更，默认落在「信息」；
  4. 信息面板：全 sans、无字距突兀、长路径（含中文目录）折行正常；
  5. 日志页签：超长日志可滚动、自动粘底、上翻不被拉回；变更页签 diff 可滚动；
  6. 左右栏折叠/展开：宽度过渡正常，折叠窄轨颜色随新 token。
