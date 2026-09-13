# Composer 草稿按 Tab 隔离 + 「修改」回显图片

> 缺陷修复批次（2026-09-13，分支 `fix/composer-per-tab-draft`）。两个缺陷一次修复：
> ① composer 草稿跨 Tab 串联；② 带图消息点「修改」图片不回显。纯前端改动，IPC 与事件面 27 键零变化。

## 1 背景与根因

### 1.1 缺陷一：草稿跨 Tab 串联

- **现象**：tab A composer 输入内容后切到 tab B，内容跟着过去；粘贴的图片附件同样串联。
- **根因**（两处合谋）：
  - `AppShell.tsx` 的 `{activeKey && <Composer />}` 无 `key` prop，切 Tab 时 React 复用同一组件实例、不卸载重建；
  - 草稿文本/图片存于该常驻单实例的局部 `useState`（`Composer.tsx` / `useComposerAttachments.ts`）。
  - `useComposerHistory.ts` 中注释「草稿暂不按 Tab 隔离——维持现状」表明这是已知取舍，本批次按用户决策推翻。

### 1.2 缺陷二：带图消息「修改」不回显图片

- **现象**：已发送的带图消息点悬停「修改」，文本回填 composer 但图片丢失。
- **根因**：`ChatMessages.tsx` 派发的 `ws:composer-fill` 事件 detail 只带 `{ text }`（消息上的 `images` 数据在手边未携带）；`useComposerEvents.ts` 的 `onFill` 也只回填文本。队列项「编辑」路径（`draftFromQueue`）本就带图回填，两条编辑路径行为不一致。

## 2 决策记录（用户拍板）

| 决策 | 选择 | 说明 |
|---|---|---|
| 草稿保留语义 | 按 Tab 保留 | 切走再切回，各 Tab 草稿各自恢复（非「切走即清空」） |
| 图片附件 | 与文本一并隔离 | 整个 composer 内容（文字+图）按 Tab 独立 |
| 持久化 | 仅内存 | 应用运行期间按 Tab 保留；重启不恢复、关 Tab 随 `dispose` 丢弃 |
| 附件覆盖语义 | 「修改」= 覆盖 | fill 无图时清空现有附件（与文本覆盖一致）；队列编辑无图时保留旧附件（保留旧行为，有意分歧） |

## 3 方案

### 3.1 草稿平行分桶（关键结构决策）

草稿不放进 `TabRunState`，而是 run store 顶层平行分片 `drafts: Record<sessionId, ComposerDraft>`（`ComposerDraft = { text, images: PendingImage[] }`）。

原因：run store 订阅粒度是整桶（`useActiveRun()` 返回 `tabs[key]`）。若草稿放桶内，每次击键 `setDraftText` 都会经 immer 换掉 `tabs[key]` 引用，把重渲染广播给全部整桶订阅者（ChatMessages、RightBar、AskPanel、SubagentDrawer、ContextInfoBar 等 8 处）——流式期间该路径本就高频，但空闲打字也变成全桶广播。平行分片后击键只换 `drafts[key]` 引用，订阅者只有 Composer 自身（`useActiveDraft()`）。

生命周期：`drafts[key]` 惰性创建；`dispose(sessionId)` 同步删除（关 Tab 丢草稿）；无持久化。

### 3.2 显式 key 防竞态（code-reviewer 🔴1 / 🟡2）

`setDraftText` / `setDraftImages` / `clearDraft` / `consumeDraftFromQueue` 均带可选 `sessionId`，缺省读调用时 `activeKey`。两处异步窗口显式传 key：

- **发送清空**：`send()` 目标 Tab 在调用时确定，但 `clearDraft` 原在 `await ipc.startChat()` 完成后才读 `activeKey`——窗口内切 Tab 会清错桶（新会话草稿被清、已发文本在原 Tab「复活」）。Composer 现于 `await` 前捕获 `targetKey` 并显式传入。
- **队列编辑回填**：`editQueueItem` 写 `draftFromQueue` 到 s1 桶，消费在 effect 中异步执行——若提交前切 Tab，内容会写进新会话且 s1 的标记残留（切回二次回填）。effect 现以 `tab?.key` 显式写入与消费。

### 3.3 「修改」带图回填

- `ChatMessages` 的 `onEdit` 派发 detail 增加 `images`（事件名不变，前端内部事件非 27 键契约）；
- `onFill` 经 `recalledImages` 防御还原链（4 张上限 / 20MB base64 预算）回填缩略图，无图时清空现有附件；
- 纯图片消息无「修改」按钮的现状不动（`if (!text) return`）。

### 3.4 顺带修复：切 Tab 菜单残留

提及/技能/子代理候选是 hook 局部 state（查询结果），切 Tab 后残留会把旧会话的菜单顶进新会话。Composer 新增 `tabKey` 变化 effect：清三组候选并复位高亮（历史召回浏览态的复位机制原已存在，行为不变）。

## 4 改动清单

| 文件 | 改动 |
|---|---|
| `ui/src/stores/run.types.ts` | `PendingImage` 自 `useComposerAttachments` 迁入；新增 `ComposerDraft`（含平行分桶理由注释） |
| `ui/src/stores/run.ts` | `drafts` 分片 + `setDraftText`/`setDraftImages`/`clearDraft`（可选 `sessionId`、updater 同形、惰性建桶）；`dispose` 删草稿桶；`consumeDraftFromQueue` 加可选 key；`useActiveDraft()` 订阅 helper |
| `ui/src/features/chat/Composer.tsx` | 草稿改 store 接线（`useActiveDraft`）；发送前捕获 `targetKey`；队列回填 effect 显式锁定目标 Tab；tabKey 菜单清理 effect；ask 覆盖注释更新 |
| `ui/src/features/chat/useComposerAttachments.ts` | `images` 状态改 opts 注入（hook 不再自持）；`PendingImage` 改 re-export |
| `ui/src/features/chat/useComposerEvents.ts` | opts 增 `setImages`/`recalledImages`；`onFill` 带图回填；注释言明与队列编辑的覆盖语义分歧 |
| `ui/src/features/chat/ChatMessages.tsx` | `onEdit` 派发 detail 带 `images` |
| `ui/src/features/chat/useComposerHistory.ts` | 过时注释更新（草稿已按 Tab 隔离），行为不变 |

测试：新增 `ui/src/__tests__/composer.per-tab.test.tsx`（D1–D10）；`usermsg.actions.test.tsx` fill 契约更新（断言 detail 含 `images` / 纯文本 `images: undefined`）；全测试文件 useRun 重置点补 `drafts: {}` 清理（20 处，防模块级单例跨用例泄漏）。

## 5 审查结论与修复（code-reviewer）

🔴 1 项，修复：

- R1 `clearDraft` 在 `await send()` 之后读 `activeKey`，发送窗口内切 Tab 清错桶 → §3.2 显式 key 方案（`clearDraft(sessionId?)` + 发送前捕获）。

🟡 3 项全部采纳：

- Y1 草稿入桶致击键全桶广播重渲染 → §3.1 平行分桶重构；
- Y2 `draftFromQueue` 消费竞态（写错 Tab + 残留二次回填）→ §3.2 回填显式锁定目标 Tab；
- Y3 测试缺口 → 补 D8（insert updater 分支 + 惰性建桶）、D9（ask 覆盖恢复草稿）、D10（队列出队不触碰草稿）。

🟢 记录：`as PendingImage[]` 断言无害保留；语言运行中切换后 fill 图片名仍用旧语言（纯外观，暂缓）；`addImageFiles` 异步窗口闭包旧值与改动前等价（非回归，治本需渐进式 updater 校验，属后续可选）。

## 6 验证

- `pnpm --dir ui test` 全绿：**319 passed / 44 文件**（基线 308，+11：per-tab D1–D10 + fill 契约拆分）；
- `pnpm --dir ui build`（tsc --noEmit + vite build）通过；
- 后端零改动，`cargo test` 不涉及。

### 6.1 手动验证清单（GUI 手动点验，`pnpm tauri dev`）

1. 开两个会话 tab：A 输入文字 + 贴一张图 → 切 B（应为空）→ 切回 A（文字与缩略图都在）；
2. A 发送 → A 草稿清空、消息带图发出；切 B 再切回，B 的草稿不受影响；
3. B 输入内容后关掉 B tab → 从左栏重开该会话 → composer 为空；
4. A 输入 `@` 弹出提及菜单时切到 B → 菜单应收起，B 中无残留候选；
5. 发送一条带图消息 → 悬停点「修改」→ 文本与图片缩略图都回显；再对一条纯文本消息点「修改」→ composer 原有附件被清空；
6. 运行中在 A 提交队列任务 → 队列项「编辑」→ 文本与附件回填 A；切 B 不受染；
7. A 触发 ask（提问卡覆盖输入区）→ 回答后 A 草稿原样恢复；
8. A 空输入按 ↑ 进入历史浏览 → 切 B 切回 → 浏览态已复位、A 草稿仍在；
9. 重启应用 → 所有会话 composer 为空（预期：仅内存）。

## 7 边界与遗留

- 不做磁盘持久化（重启丢草稿为既定语义）；AppShell 不加 `key`（保留实例，切 Tab 不丢焦点）；
- 运行中切换界面语言后，「修改」回填图片的占位名（「历史图片 N」）用旧语言渲染——纯外观，随 `recalledImages` 依赖治理一并处理；
- `useComposerAttachments.addImageFiles` 异步读闭包旧值的窗口与改动前等价（FileReader 读取时长内并发变更会被整体覆盖），治本方案（渐进式 `setImages(prev => ...)` 校验）留作后续可选优化。
