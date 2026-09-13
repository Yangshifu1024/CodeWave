# 移除左栏工作区文件面板 + 聊天区滚动条主题化

> 日期：2026-09-03 · 类型：界面批次（轻量路径） · 涉及：`ui/` 纯前端，后端零改动（IPC 命令保留未删）
>
> 需求（两条）：移除左侧栏底部的工作区文件；中间聊天区域滚动条背景色一致。

## 一、移除工作区文件面板（WorkspaceExplorer）

左栏自 P0 起为上下两段（导航 55% + 工作区文件 45%），文件树/内联编辑器与右栏「文件」页签、@ 提及功能高度重叠，按需求整体移除：

- `AppShell.tsx`：删除 `.sider-explorer` 区块与 `.sider-stack` 包裹层，左栏 = 顶部折叠按钮行 + `.sider-nav`（ProjectNav 独占全高）。
- 删除组件 `features/workspace/WorkspaceExplorer.tsx`（文件树 + 内联编辑器 + 保存），该目录仅余 GitDiffModal。
- 清理：i18n 删 `explorer.*` 命名空间与 `app.workspaceNone`；CSS 删 `.explorer*` 全块、`.explorer-empty`、`.sider-stack`/`.sider-explorer`；ipc client 删孤儿方法 `saveWorkspaceFile`/`listWorkspaceDir`（`readWorkspaceFile` 保留——右栏产物查看在用）。后端 `save_workspace_file`/`list_workspace_dir` 命令不动（零后端改动约定），仅前端不再调用。
- 产物 bundle 减小约 60KB（antd Tree 随组件移出主包）。

## 二、聊天区滚动条主题化

原状：`.chat-messages` 无任何滚动条样式，WebView 默认滚动条轨道底色与聊天区背景（`--ws-bg`）不一致，视觉突兀。改为：

```css
.chat-messages { scrollbar-width: thin; scrollbar-color: var(--ws-border) transparent; }
.chat-messages::-webkit-scrollbar { width: 8px; height: 8px; }
.chat-messages::-webkit-scrollbar-track { background: transparent; }
.chat-messages::-webkit-scrollbar-thumb { background: var(--ws-border); border-radius: 4px; }
.chat-messages::-webkit-scrollbar-thumb:hover { background: var(--ws-dim); }
```

轨道透明 → 滚动条背景与内容区恒一致（浅/深主题均适用）；thumb 用 `--ws-border`、hover 加深为 `--ws-dim`。WebKit 系（WebView2/WKWebView）走 `::-webkit-scrollbar`，`scrollbar-width/color` 作标准语法兜底。

## 三、测试

前端 110/110 全绿（107 + chat.style 新增 3 例）、`pnpm --dir ui build` 通过。

| 文件 | 变更 |
|---|---|
| `app.smoke.test.tsx` | 侧栏折叠用例去掉 `.sider-explorer` 断言 |
| `sidebar.style.test.ts` | `.sider-stack` 契约改判 `.sider-nav`（flex:1 + min-height:0 占满） |
| `chat.style.test.ts`（新增） | 滚动条契约：轨道透明、thumb 主题 token + 圆角 + hover、标准语法兜底 |

## 四、手动验证清单

1. 左栏只剩导航（临时会话 + 项目树 + 任务区），无「工作区文件」段落、无上下分割线；导航内容超长时整栏滚动。
2. 左栏折叠/展开、重启记忆一切照旧（[docs/sidebar-toggle-buttons](./sidebar-toggle-buttons.md) 行为不变）。
3. 聊天区发送长消息使内容溢出：滚动条轨道与聊天背景同色（浅色/深色主题各看一次），thumb 为主题边框色细条、hover 加深、圆角。
4. 右栏「文件」页签点击 md/图片仍可正常查看（readWorkspaceFile 未受影响）；@ 提及文件路径搜索正常（search_workspace_paths 未动）。
