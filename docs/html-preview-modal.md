# render_html 大弹框预览

> 2026-09-24 · 前端呈现层改造（`render_html` 工具卡 → 大弹框）。
> 关联：[preview-skill](./preview-skill.md)（同一批次新增的方案预览技能与批准门第三选项）。

## 1. 问题与根因

Agent 用 `render_html` 生成的小组件此前只在工具卡里以沙箱 iframe 呈现：

- `ui/src/features/tools/ToolCallCard.tsx` 的 render_html 分支只给了 `maxHeight: 480`，**没有 `height`**；
- `ui/src/theme/app.css` 里也只有 `.tool-card .widget { background: #fff }` 一条规则。

于是 iframe 高度退化为浏览器默认的 **约 150px**，宽度又被聊天列宽（右栏常驻）挤压——「长宽都不够、看起来很累」的直接原因。

## 2. 方案

### 2.1 卡片内不再内嵌渲染

- render_html 卡的**头部**（收起态即可见）新增「预览」按钮，摘要显示出参 `title`；点击打开大弹框。
- 按钮带 `stopPropagation`：头部点击 = 展开/收起，不能因为点「预览」顺带把卡片展开。
- 展开体只保留信息行（标题 + `{{n}} 字符`），不再渲染 iframe；其他工具卡的展开语义零改动。
- 弹框开关是**卡片本地 state**（先例：`AskPanel` 的 `planOpen`）：卡片卸载（切 Tab / 切会话 / 历史重建）即关闭，天然满足「切走不残留」，也避免为它改动 ui store（模块级单例在测试间残留是已知坑）。
- 无 html 时（历史占位 `{ restored: true }`）**不渲染入口**，头部给一行「历史未保留预览内容」提示——绝不出现「点了报错」。

### 2.2 大弹框（`ui/src/features/tools/WidgetPreviewModal.tsx`）

- `width="90vw"` + `centered` + `footer={null}` + `mask={{ closable: true }}`（antd 6 已废 `maskClosable`）；Esc 走 antd 默认。
- 铺满：`styles.body` 为 `padding: 0; height: calc(85vh - 56px); display: flex; flex-direction: column`（扣掉弹框头部高度 → 总高 ≈85vh；居中后小窗口也不会向上溢出），iframe `width: 100%; height: 100%; border: 0`（占满 `.widget-preview-stage`，后者 `flex: 1; min-height: 0`）——滚动交给 iframe 自身，弹框主体不出纵向滚动条。
- `destroyOnHidden`：关闭即销毁内容 → iframe 卸载，预览里的脚本与动画随之停止。

### 2.3 弹框内三项能力

| 能力 | 实现 | 说明 |
|---|---|---|
| 复制源码 | `navigator.clipboard.writeText(html)` | 成功后按钮态 1.5s 显「已复制」，失败显式「复制失败」（不静默）；反馈手法沿用 `utils/codecopy.ts` 的既有约定 |
| 重新加载 | iframe `key={reloadSeq}` 强制重挂 | iframe 没有 reload API，重设相同 `srcDoc` 在部分 WebView 不触发；重挂不影响弹框与底色设置 |
| 浅色/深色底色 | 容器底色 + `iframe.style.colorScheme` | 默认跟随应用主题（`theme.useToken()` 的 `token.colorBgBase` 亮度判定，手法同 `theme/bridge.tsx`），每次打开重置 |

**保真纪律**：底色只作用于承载画布，**绝不改写 `srcDoc`**——「复制源码」拿到的与模型给的 HTML 逐字节一致；模型 HTML 自带背景时保持原样（这是正确语义，不是缺陷）。沙箱策略不变：`sandbox="allow-scripts"`（无网络、无同源权限）。

## 3. 改动清单

| 文件 | 改动 |
|---|---|
| `ui/src/features/tools/ToolCallCard.tsx` | render_html 分支：去掉内嵌 iframe；头部「预览」入口 + 摘要取 `data.title`；展开体改信息行；挂载弹框组件 |
| `ui/src/features/tools/WidgetPreviewModal.tsx` | 新增（弹框本体 + 三项能力） |
| `ui/src/theme/app.css` | 删死规则 `.tool-card .widget`；新增 `.widget-preview-toolbar/-meta/-actions/-stage/-frame` |
| `ui/src/i18n/zh-CN.ts` / `en-US.ts` | `tools` 段新增 9 个键（preview / previewUnavailable / previewChars / copyHtml / copied / copyFailed / reload / bgLight / bgDark） |
| `ui/src/__tests__/settings.registry.test.ts` | `FEATURE_FILE_SEGMENTS` 登记新文件（`tools.` 段）——未登记即判红 |
| `ui/src/__tests__/widget.preview.test.tsx` | 新增 8 个用例 |

本弹框改造**后端零改动**（工具入参/出参/50000 字符上限/沙箱策略均不变）；同批次另有后端改动（内置 `preview` 技能与批准门第三选项），见 [preview-skill](./preview-skill.md)。

## 4. 历史会话回放（已解决）

> 本节原写「大 widget 无法回放」的已知限制；该限制已由**工具结果原样 sidecar** 解除，见 [session-restore-fidelity](./session-restore-fidelity.md)。

历史里存的仍是模型侧瘦身文本（`src-tauri/src/tools/compact.rs` 的 HEAD 4KB + TAIL 8KB 头尾截断），但这个机制同时解决了它：

- 每次工具调用只要「模型侧文本 != 完整出参 JSON」（即真被截断/剥字段），后端就按 **provider 侧 `tool_use.id`** 在 `~/.codewave/sessions/<会话>.toolres/<call_id>.json` 存一份**完整出参**；`read`/`command` 这类原样透传的工具与小出参不落盘。
- 前端恢复时只对「历史文本解析失败」的调用**批量拉一次**（IPC `load_tool_outcomes`），把完整出参回填进工具卡——widget 的 `data.html` 因此回来了，预览入口与弹框**零特判**即可用。
- 没有备份的会话（本机制上线之前的）仍显「历史未保留预览内容」。

## 5. 验证

自动化（工作树内实测）：

- `cargo test`（`src-tauri/`）：**1020 passed / 0 failed / 3 ignored**
- `pnpm --dir ui test`：**1082 passed / 94 文件**（本片新增 8 个弹框用例 + 5 个批准门用例；恢复链路（sidecar 回填、中断状态）的用例见 [session-restore-fidelity](./session-restore-fidelity.md)）
- `pnpm --dir ui run lint`、`pnpm --dir ui build`：见本批次收尾报告

人工点验清单（界面改动不做 GUI 自动点验）：

1. 让 Agent 调一次 `render_html`（如「用 render_html 画个仪表盘」）→ 卡片头部出现「预览」按钮，卡片内不再有 iframe。
2. 点「预览」→ 弹框 90vw × 85vh，内容铺满，弹框自身无纵向滚动条（长内容在 iframe 内部滚动）。
3. 点「复制源码」→ 粘贴到编辑器，与模型给的 HTML 一致（含 `<script>`）。
4. 点「重新加载」→ 有动画/交互脚本的 widget 从头重跑。
5. 切「深色底」/「浅色底」→ 无自带背景的 widget 画布底色随之变化；自带背景的 widget 外观不变。
6. 暗色主题下打开弹框 → 默认底色为深色；亮色主题下默认浅色。
7. 关闭方式齐备：右上角 ×、点遮罩、Esc；关闭后重新打开，内容从头开始（iframe 已卸载）。
8. 把窗口缩到最小可用尺寸 → 弹框不溢出、工具栏按钮可点。
9. 同一会话里放两张 widget 卡 → 各自可开预览，互不串内容。
10. 5 万字符级别的 widget → 打开、复制、重载都正常。
11. 切换 Tab / 切换会话 → 已打开的预览弹框随之关闭，回到原会话不会自动重开。
12. 重开一个**含大 widget 的会话** → 卡片仍有「预览」入口，点开内容完整（机制上线之前的旧会话没有备份，此时才显示「历史未保留预览内容」）。

## 6. 非目标

内嵌预览的自适应高度；HTML 落盘/下载为文件；用系统浏览器打开；弹框内编辑或格式化源码；弹框缩放/全屏；放宽沙箱（不加 `allow-same-origin`、不加网络能力）；把用户在预览里的操作反馈给模型；预览弹框自动弹出（需给 `render_html` 增加可选入参并在历史重放时防误开，属可选增强）。
