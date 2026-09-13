# Composer 粘贴图片 + 历史消息方向键召回（arch 批次报告）

> 流程：$arch 全流程编排（S1 调研 → S2 需求分析 → S3 澄清落盘 → S4 方案 → S5 批准 → S6 串行开发 → S7 对齐审查 → S8 测试 → S9 汇报）。
> 过程产物：`.codewave/tasks/20260831-171839-composer-paste-history/`（requirement.md / plan.md / review.md / report.md）。

## 需求与裁决

1. **Composer 粘贴图片**：聚焦输入框直接 Cmd/Ctrl+V 粘贴截图/图片，走既有附件链路。
2. **↑↓ 切换历史消息**：空输入按 ↑ 召回最近一条已发送消息，浏览态内 ↑↓ 翻页、Esc/↓ 越界恢复草稿，召回后 Enter 作为新消息发送。

用户澄清三项裁决：召回**带图恢复**；非视觉模型**软阻断**（`vision === false` 显式标注才拦，缺失放行）；粘贴**本期仅图片**（非图片 toast 引导 @ 引用路径）。

## 改动清单

| 文件 | 内容 |
|---|---|
| `ui/src/features/chat/Composer.tsx` | ① `addFiles` 拆出共享校验链 `addImageFiles`（image/*、单张 ≤5MB、≤4 张、base64 总额 ≤20MB、FileReader 反构），文件选择器与粘贴两入口行为与文案完全一致；② 新增 `onPaste`：clipboardData files+items 双通道扫描（WKWebView UTI 兼容）+ `name|size|type` 去重，无文件不干预（纯文本默认粘贴），有文件 preventDefault，图片附加、非图片提示一次 `composer.pasteFilesHint`；③ `send()` vision 软阻断：`images.length > 0 && effectiveModel?.vision === false` → warning + return；④ 历史召回：`histIdx` 浏览态索引 + `draftRef` 草稿快照（text+images），`recallHistory/recalledImages/applyRecall/exitRecall`，onKeydown 守卫链保持 IME → Shift+Tab → Escape（浏览态退出前置）→ 菜单 ↑↓/Enter/Tab → 召回 ↑↓（空输入触发、最旧停留不回绕、越界恢复草稿）→ Enter 发送；`onInputChange` 编辑退出浏览态；`tab?.key` 切换重置；发送受理后指针复位；带图恢复 `{mediaType,data} → PendingImage`（`composer.recalledImage` 命名，防御性 4 张/20MB 截断） |
| `ui/src/i18n/zh-CN.ts` / `en-US.ts` | 新增 `composer.pasteFilesHint` / `composer.visionUnsupported` / `composer.recalledImage` 三键 |
| `ui/src/__tests__/composer.paste.test.tsx`（新） | 5 用例：粘贴图片出缩略图 / 非图片提示 / 纯文本不拦截 / vision=false 拦截 / vision 缺失放行 |
| `ui/src/__tests__/composer.history.test.tsx`（新） | 10 用例：H1–H7、H10、H13 及召回后 Enter 发送 |

**契约面零变更**：无新 IPC、无类型/事件面（24 键）变更、零后端改动（reviewer grep 核实）。

## 关键设计决策

- **优先级链**（自高到低）：IME isComposing 守卫 → Shift+Tab 权限循环 → @/$ 菜单 ↑↓/Enter/Tab → 历史召回浏览态 → Enter 发送；粘贴为独立事件通道，与所有键盘态并行。
- **有意不做失焦退出浏览态**：点击发送按钮会先 blur，若失焦恢复草稿会把已召回文本在发送前改掉，属负收益。
- **召回数据源为前端内存 `run.items` 的 user 条目**（notice 化消息天然排除）；重开会话由 `restoreFromMessages` 重建，同样可召回。

## 验证结果

- 前端 `pnpm --dir ui test`：**54/54 全绿**（基线 39 + 新增 15）；`pnpm --dir ui build` 通过。
- 后端 `cargo test`：204 中 2 个稳定失败均与本变更无关——`mcp::tests::real_*` 依赖 `scripts/mcp-test-server.mjs`（被 `.gitignore` 整目录排除，未入库，node `MODULE_NOT_FOUND`）；`running_flag_resets_after_run_ends` 时序 flaky（复跑即过）。**遗留事项：gitignore 收窄或 `git add -f` 该脚本，由用户执行**。
- reviewer 对齐审查：**对齐**，0 🔴 / 0 🟡 / 5 🟢（去重键理论碰撞、防御分支语义注释、P6 断言强度、draftRef 不可变约定注释、索引 undefined 守卫——均记录不阻塞）。

## GUI 手动验证清单

见 report.md（9 步：截图粘贴缩略图、非图片 @ 引导、纯文本、↑↓ 翻页/越界恢复、Esc 恢复、带图召回、vision=false 拦截、中文输入法组合期方向键、召回后 Enter）。
