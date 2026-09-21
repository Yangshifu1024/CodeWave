# Composer 文件引用 chip 化 + Windows verbatim 前缀清理

> 日期：2026-09-21。需求（用户原话）：「选择附件后，附件路径在 composer 中的样式很难看，期望参考 codex 或其他工具的显示方式」——截图里输入框被一整条 `@\\?\C:\Users\…\支付订单_10W.xlsx` 占满。
> 关联：[office-and-pdf-support](./office-and-pdf-support.md)（文件入口「原地引用」的原始约定）、[slash-skills-and-dollar-agents](./slash-skills-and-dollar-agents.md)（`@` 提及的文本信号契约）、[run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md)（队列「编辑」回填）、[composer-toolbar-batch-report](./composer-toolbar-batch-report.md)（卡片式输入与附件条）。

## 1. 问题与决策

改造前：非图片文件是**原地引用**，而「引用」就是正文文本——选完附件往输入框尾部追加一行 `@路径`。项目外的文件走 canonicalize 取绝对路径，于是 Windows 上出现 `\\?\C:\Users\…`：既把用户的问题挤出视野，又像一个坏掉的路径。

| 决策 | 内容 | 理由 |
|---|---|---|
| D1 | 引用从正文**解耦**为独立数据结构（草稿 `refs[]`），输入框里以 chip 展示 | 纯 `Input.TextArea` 无法对局部文本着色/折叠，想不显示长路径就只能不让它进正文 |
| D2 | **发送前一刻**才把引用合成 `@<ref>` 追加到正文末尾 | 后端 `startChat`、提示词语义、模型所见字节全部不变——零协议改动、零后端回归面 |
| D3 | chip 与图片附件**同一个容器、同一套 `.attach-chip` 视觉** | 用户诉求；且图片缩略图与文件胶囊本就是同一类「待发内容」 |
| D4 | 文件 chip 只显示**文件名**，悬停（`title`）看完整引用路径 | 与既有的「basename 展示 + title 全路径」范式一致（工作目录胶囊、右栏文件列表） |
| D5 | 后端在**呈现与引用的出口**去掉 Windows verbatim 前缀 | `\\?\` 是 Windows `canonicalize` 的产物、对人和模型都是噪声；内部边界比较仍用 canonical 形态 |
| D6 | 判定/解析是**保守启发式 + 门禁** | 宁可漏判（留在正文）也不误判；回填解析还要过「能逐字节还原原文」的往返门禁（`recoverRefs`），宁可不好看也不搬动/截断用户写的内容 |

## 2. 语义与契约

```
选文件（附件按钮 / 拖入 / @ 提及选文件）       发送
        ↓                                    ↓
  草稿 refs[]  ── chip 展示 ──┐        正文 = mergeRefs(text, refs)
                              │        = "<正文> @<ref1> @<ref2> …"
  正文 text（人写的内容）──────┘         （与改造前 appendRefs 的落点一致）
```

- **正文不含路径**：三个入口只往 `refs` 里记，不往 `text` 里打字。
- **发送合成**：`ui/src/features/chat/composerRefs.ts::mergeRefs`——正文在前、引用空格相连追加末尾；正文里已经写着同一个 `@ref`（手打）时不重复追加；`refs` 内部去重。
- **发送守卫**：`hasDraft = !!text.trim() || refs.length > 0`——只挂引用不写正文也能发送（否则会出现「附件挂上了却发不出去」的回归）。
- **`@` 提及分叉**：选**目录**仍写 `@目录/ `（用户要继续拼路径的字面输入）；选**文件**→ 抽成 chip。目录用文本是有意为之，不是漏改。注意：搜索候选（`search_workspace_paths`）里可能含目录条目、且不带 `is_dir` 标记，这类条目按文件处理进 chip（与改造前一样无法继续拼路径，不属回归）。
- **回填解析（门禁式）**：历史召回（↑↓）、队列「编辑」、用户消息「修改」（`ws:composer-fill`）三条回填链路都先经 `recoverRefs` 抽回 chip；该函数只在「解析→再合成」能**逐字节还原原文**时才采用结果（即只认 app 自己产出的「正文 + 末尾 `@引用`」形态），否则正文原样留着不动。`ws:composer-insert`（技能追加）保留现有引用不动。
- **判定规则**（`isRefToken`）：`@` 打头且「无分隔符时末段带扩展名」或「有分隔符时末段带扩展名 / 整体是绝对形态（`/`、`\\`、`X:\`、`~`）」。`@types/node`、`@scope/pkg`、`@用户名` 都不是引用。
- **在末尾形态下往返无损**：`mergeRefs(splitRefs(t))` 对「引用在末尾、单空格分隔」的文本**逐字节等于原文**；其它形态（引用在句中、路径含空格、引用独占一行）解析会搬动/截断/多出空行，因此**回填一律走 `recoverRefs` 门禁**把它们排除（不解析、原样保留）。

## 3. 改动清单

前端：

| 文件 | 改动 |
|---|---|
| `ui/src/features/chat/composerRefs.ts` | 新增：`isRefToken` / `splitRefs` / `mergeRefs` / `recoverRefs`（回填门禁）/ `addRefs`（纯函数，无 React 依赖） |
| `ui/src/stores/run.types.ts` | `ComposerDraft` 增 `refs: string[]` |
| `ui/src/stores/run.ts` | 新增 `setDraftRefs`；`setDraftText`/`setDraftImages` 防御创建、`clearDraft`、`EMPTY_DRAFT` 同步带 refs |
| `ui/src/utils/uiState.ts` | 草稿快照带 `refs`（可选字段 + 消费处兜底，旧快照不炸）；`contentOf`/`applyRetainedContent`/`tabHasPendingContent` 计入引用 |
| `ui/src/features/chat/Composer.tsx` | chip 渲染（图标 + 文件名 + ×，`title` 全路径）；`removeRef`；发送走 `mergeRefs`；`hasDraft` 计引用；队列回填解析；提及选文件入列 |
| `ui/src/features/chat/useComposerAttachments.ts` | `appendRefs`（写正文）→ `onRefs`（写 refs） |
| `ui/src/features/chat/useComposerMentions.ts` | `pickMention` 按 `isDir` 分叉；新增注入 `addFileRef` |
| `ui/src/features/chat/useComposerHistory.ts` | `applyRecall` 经 `recoverRefs` 解析引用；草稿快照与还原带上 refs |
| `ui/src/features/chat/useComposerEvents.ts` | `ws:composer-fill`（「修改」= 覆盖语义）连带覆盖 refs（chip 不在正文里，光 `setText` 盖不住）；`insert` 仍保留引用 |
| `ui/src/theme/app.css` | `.attach-chip.ref-chip` 图标样式（复用同一套 chip 视觉，无新增色彩变量） |
| `ui/src/i18n/{zh-CN,en-US}.ts` | `composer.removeRef` 一个键 |

后端（只动「呈现与引用写法」的出口）：

| 文件 | 改动 |
|---|---|
| `src-tauri/src/tools/pathutil.rs` | 新增 `strip_verbatim_prefix`（`\\?\C:\x` → `C:\x`、`\\?\UNC\srv\share` → `\\srv\share`；幂等；仅认盘符/UNC 形态，Unix 上把 `\\?\` 当普通文件名的相对路径不受影响） |
| `src-tauri/src/host/commands/workspace.rs` | `external_path_payload` 的 `dir` 与 `ref` 出口去前缀；`inside` 判定与内部 canonical 形态不变 |
| `src-tauri/src/tools/list_files.rs` | 多根会话下 `@` 提及候选的绝对路径出口去前缀 |

## 4. 为什么不这样做（被否决的方案）

- **只去前缀、路径仍留在输入框**：正文照样被长路径挤满，没解决主诉。
- **在 textarea 之上叠镜像层给 `@路径` 上色**：仍要显示整条路径，且要同步行高/滚动，复杂且不解决问题。
- **chip 只读镜像正文里的 `@token`**：想删 chip 就得改正文；且删除后 token 仍在正文里，双向绑定反而更乱。
- **把引用改成相对额外根或短名再在发送时展开**：相对路径的跨根解析（`resolve_read`）语义会变——同名文件可能落到主根上，属于安全边界改动，风险不成比例。

## 5. 已知限制与遗留

1. **手打的 `@路径` 不 chip 化**（有意）：只有 UI 产出的引用会变 chip；粘贴的纯文本、手打形态（引用在句中/路径含空格/引用独占一行）均不解析——宁可不好看，也不搬动用户写的内容。
2. **文件名含空格** 的引用仍受 `@` token「遇空格即断」的既有缺陷影响：这类形态不会被回填解析（早先版本会把它截成半个路径再搬走，已由 `recoverRefs` 门禁挡住），但手打时依旧无法引用带空格路径（老问题，协议级修复另案）。
3. **引用位置统一到末尾**：改造前「先选附件后打字」会把引用留在正文**前**面；现在一律追加末尾。模型对位置不敏感，且换来往返稳定。
4. **提及搜索命中目录时按文件处理**（进 chip，没有尾斜杠）：搜索接口不返回 `is_dir`，与改造前「插入 `@dir ` 也拼不了路径」等价，不属回归；第一级「📁 根目录」条目仍写文本。
5. **会话里的用户气泡**仍显示发送时合成的 `@路径`（与改造前一致，属未变行为）。
6. `\\?\` 前缀只在 Windows 产生；macOS/Linux 上该清理是 no-op。

## 6. 验证

- Rust：`cargo test`（`tools/pathutil.rs::verbatim_prefix_stripped_for_display`、`host/commands/workspace.rs::external_path_payload_marks_inside_and_outside`——后者断言 extra 根引用去前缀后仍能通过 `resolve_read`）。
- 前端：`pnpm --dir ui test`（`composer.refs.test.ts` 判定/往返/`recoverRefs` 门禁四例；`composer.fileref.test.tsx` chip 展示、发送合成、仅引用可发送、× 移除、去重、图片与文件同区域、提及选文件、`ws:composer-fill` 覆盖引用、放行三态；`composer.history.test.tsx::H14` 召回解析与退出还原；`composer.per-tab.test.tsx::D2b` 按 Tab 隔离；`uiState.test.ts`「只有引用 chip 的草稿保留与重开回填」「旧快照缺 refs 不炸」「`tabHasPendingContent` 计引用」）、`pnpm --dir ui build`、`pnpm --dir ui run lint`。
- 手动（Windows）：项目外选 xlsx → chip 显示文件名、悬停无 `\\?\`；只挂附件不写正文能发送；多附件换行且与图片同行区；点 × 移除；↑ 召回带引用的历史消息仍显示 chip；队列「编辑」回填仍是 chip；对一条带附件的旧消息点「修改」→ chip 跟着回来且不夹带上一份草稿的引用。

## 7. 审查纪要（code-reviewer）

首轮审查结论：**需返工（1 🔴）**，已全部处理：

| 级别 | 问题 | 处置 |
|---|---|---|
| 🔴 | `ws:composer-fill`（用户消息「修改」）只覆盖文本与图片，**没覆盖 refs**——chip 不在正文里，`setText` 盖不住，草稿里未发送的旧引用会被悄悄带进改后的消息 | `useComposerEvents` 接入 `setRefs`，fill 时用 `recoverRefs(detail.text).refs` 一并覆盖（顺手让「修改」回填也 chip 化）；`insert`（追加语义）保持不动 |
| 🟡 | `splitRefs` 的「往返无损」只在特定形态成立：路径含空格会被 token 截断成半个路径、引用独占一行会多出空行、句中引用会被搬走——回填路径会**损坏用户内容** | 新增 `recoverRefs` 门禁（`mergeRefs(解析结果) === 原文` 才采用），三条回填链路改走它；文档措辞由「无损」收窄为「末尾形态逐字节相等」 |
| 🟡 | 提及搜索命中目录被当文件 chip，与文档「选目录写文本」不符 | 文档补明：搜索接口不带 `is_dir`，这类条目按文件处理（与改造前等价，非回归）；第一级根目录条目仍写文本 |
| 🟡 | `AGENTS.md` 仍写「非图片文件只在输入框插一行 `@路径`」 | 已改为 chip + 发送前合成 |
| 🟡 | 关键风险缺测试：提及选文件、仅 refs 草稿的关 Tab 保留/重开、fill 覆盖 | 均补用例（见 §6） |
| 🟢 | 已采纳：`uiState` 回填时对 refs 去重（防手工编辑过的快照触发 React key 冲突）；`refIcon` 去掉笔误项 `csvx`；`pickMention` 只认行首/空白后的 `@`（避免切开 `me@x.com`） | |
| 🟢 | 未采纳：`UNC` 大小写不敏感（Windows canonicalize 恒输出大写，收益极低）；`list_files` 模糊搜索的路径分隔符加权（既有问题、与本批无关） | |
