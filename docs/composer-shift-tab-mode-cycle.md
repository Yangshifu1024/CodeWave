# 14 · Composer Shift+Tab 切换权限模式

> 需求：底部 Composer 输入框内 `Shift+Tab` 循环切换权限四档（对齐主流编码 Agent 的操作习惯）。纯前端改动，后端无变化。

## §1 行为定义

- 焦点在 Composer 输入框时按 `Shift+Tab`：按 **变更前确认 → 自动编辑 → 计划模式 → 完全访问 →（环回）变更前确认** 顺序循环切换会话级权限模式。
- 档位顺序与权限下拉菜单条目顺序一致（`MODE_ORDER` 常量）。
- 处理置于 `onKeydown` 最前（IME 判断之后）：`@` 提及 / `$` 技能 / `/` 命令菜单打开时同样可切换；`preventDefault` 阻止焦点移出输入框。
- 复用 `updatePrefs`（乐观更新 + 失败回滚 + toast），与菜单点击路径完全同源；运行中切换安全（[docs/composer-toolbar-batch-report](./composer-toolbar-batch-report.md) §2.1 语义）。
- 未聚焦输入框时不响应（不做全局热键），避免与浏览器/WebVim 类习惯冲突；AppShell 全局热键（Esc / Cmd+W / Cmd+L / Cmd+←→）无冲突。
- 权限模式按钮 tooltip 提示快捷键（`composer.modeShortcutHint`，zh/en 双语）。

## §2 改动清单

| 文件 | 改动 |
|---|---|
| `ui/src/features/chat/Composer.tsx` | `MODE_ORDER` 常量 + `onKeydown` 拦截 `Shift+Tab` 循环切换 + 权限按钮 title |
| `ui/src/i18n/zh-CN.ts` / `en-US.ts` | 新增 `composer.modeShortcutHint` |
| `ui/src/__tests__/app.smoke.test.tsx` | 新增用例「Shift+Tab：输入框内循环切换权限四档」（覆盖 auto_edit→plan→full_access→confirm_each 环回 + 橙色高亮 class 同步/清除） |

## §3 验证

- 前端 `pnpm --dir ui test`：**36/36 全绿**（含新增 1 例）。
- 前端 `pnpm --dir ui build`：type check + vite build 通过。
- 后端未改动，`cargo test` 不受影响。

## §4 手动验证清单（GUI 不做自动点验）

1. `pnpm tauri dev`（仓库根；确认无打包版同时运行）。
2. 焦点在输入框，连续按 `Shift+Tab`：按钮文案依次 变更前确认（蓝）→ 自动编辑（橙）→ 计划模式（默认灰）→ 完全访问（红）→ 回到变更前确认。
3. 输入 `@` 弹出提及菜单时按 `Shift+Tab`：仍可切换且菜单不被打断（Tab 键未被焦点导航抢占）。
4. 运行中按 `Shift+Tab`：切换立即生效，下一轮请求按新模式走（如计划模式下模型无法调用写工具）。
5. 鼠标悬停权限按钮：tooltip 显示「Shift+Tab 循环切换权限模式」，且悬停不改变当前档位色。

## §5 权限胶囊按档位着色

> 需求：胶囊不再只高亮完全访问，改为随档位着色，一眼可辨当前权限状态。

- 语义映射：`confirm_each` 变更前确认 = **蓝 `--ws-accent`**（主题主色）/ `auto_edit` 自动编辑 = **橙 `--ws-warn`** / `full_access` 完全访问 = **红 `--ws-err`**（危险）；`plan` 计划模式 = 默认灰不着色（日常档避免常驻高亮）。
- 实现：`Composer.tsx` 新增 `modeClass` 档位→类名映射；`app.css` 三条着色规则（选择器带 `.ant-btn-text`，同特异性后写胜出，压过工具条 hover 变色规则，悬停保持档位色）。
- 颜色全部取自 `--ws-*` token（antd 主题桥接，亮暗自适应），未引入硬编码色值。
- 测试：Shift+Tab 环回用例与权限菜单用例同步断言三档 class 的出现/清除。

> **后续变更（[docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md)）**：全局强调色已墨化（`--ws-accent` = 中性墨色，不再是被误当作品牌色的 antd 默认蓝 #1677ff），§5 中「confirm_each = 蓝」的实际呈现随之变为**墨色**；语义升级为「色彩强度映射风险等级」——无彩色（墨）= 默认档 / 橙 = 需注意 / 红 = 危险，映射关系与着色规则本身未变。详见 [docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md)。
