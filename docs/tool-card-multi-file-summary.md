# 工具卡头部批量文件名展示（read/edit 多文件 summary 修复）

> 分支 `feat/toolcard-multi-file-summary`（worktree `CodeWave-wt-toolcard`）。纯前端改动，后端零变化。

## 1. 背景与症状

用户报 bug：模型一次读取了多个文件，聊天界面只显示了一次工具调用——一个「已使用 读取 read credentials.rs」头部下挂了两个文件的预览块（credentials.rs 与 remote.rs）。

## 2. 根因（P0 分类：非缺陷，展示语义误导）

- `read` 是**批量工具**：schema `files[1..20]`（`read.rs:112-134`），[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §3.2 记录的主动设计（单次调用批量读取，降低往返）。
- 本次模型一次调用传了 2 个路径 → 后端发 **1 条** `tool:result`（`outcome.data.files` 含 2 条目，`read.rs:236-249`）→ `ToolCallCard.tsx` 对 read 卡逐项渲染文件块（设计内）。
- **一个卡挂两个文件块恰恰证明数据链路完整**。误导点在头部 summary：`ToolCallCard.tsx` 旧逻辑兜底取 `args.files[0].path`，多文件时只显示第一个文件的**完整路径**。
- 反证排除「多调用被合并」类 bug：若两次独立调用的 `call_key` 碰撞，第二次 outcome 会**覆盖**第一次（`run.ts` `onToolResult` 直接赋值），只会显示 1 个文件块而非 2 个，与截图不符。实时路径 call_key = `{batch_id}:{index}`（`batch.rs`，每批次 uuid 唯一）必不重复。

### 「一次批量调用」vs「多次独立调用」的判别方法

查会话日志（RightBar 日志查看器 / `~/.codewave` 会话黑匣子）：每次工具执行记一行 `tool read ok/ERR args=...`。一次批量调用 = 一行、args 内 files 数组多元素；多次调用 = 多行。历史保存侧 `repair.rs` 对同 id 重复 tool_use 折叠时会记 warning「重复 tool_use 已折叠」。

## 3. 受影响工具面

| 工具 | 是否受影响 | 说明 |
|---|---|---|
| read / batch_read | ✅ | `args.files` 数组（batch_read 为 read 别名，直接转发） |
| edit | ✅ | `args.files` 数组（多文件原子编辑 + 回滚，`edit.rs:236`），同一 summary 分支 |
| create / delete / list_files | ❌ | `args.path` 单路径，完整路径保留（写入定位需要目录上下文） |
| command / grep / web_fetch / http_request / calculate / wait / service / render_html / skill | ❌ | 单主体入参或专属渲染 |
| ask / suggest / plan | ❌ | 数组入参但有专属组件（AskPanel / 建议卡 / 计划卡片），不走该 summary 分支 |
| mcp__* | ❌ | 入参无约定，无 summary |

## 4. 改动（`ui/src/features/tools/ToolCallCard.tsx`）

1. **summary 重构**：read/batch_read/edit 归入「文件清单型」——头部列出**全部文件的 basename**（`split(/[\\/]/).pop()`，兼容 Windows/POSIX 分隔符），以 `", "` 连接，不再显示目录路径。
2. **清单来源 outcome 优先**：完成后 read 取 `data.files[].path`、edit 取 `data.edited[]`（`edit.rs` 成功 outcome 为 `{edited:[paths], count}`）；运行中（outcome 未到）回退解析 `argsPreview` 的 `args.files[].path`。修复同族隐患：大入参的 argsPreview 被后端替换为 `{"_args_truncated":true,...}`（`batch.rs:463`），旧逻辑此时解析失败 → 头部连一个文件名都没有。
3. **悬停全量**：summary `<span>` 增加 `title={summary}`；超长列表由既有 CSS ellipsis（`app.css` `.summary`）截断，悬停可见完整清单。
4. 旧 `args.files?.[0]?.path` 分支保留在非内置工具兜底链末位（防 MCP 工具恰好带 files 入参时行为回退）。

## 5. 测试

新增 `ui/src/__tests__/toolcallcard.test.tsx`（8 用例）：read 多文件（含 Windows/POSIX 混合分隔符）、单文件、运行中回退 args、edit outcome `edited` 优先、edit 运行中回退、入参截断后仍列全、create 完整路径回归、title 与文本一致。

全套 `pnpm --dir ui test` 136/136 全绿；`pnpm --dir ui build`（type check + vite build）通过。后端零改动。

## 6. 备查：本次不改的理论边角（探索中发现）

- **历史恢复路径按 tool_use id 分组**：`restoreFromMessages` 用 provider call id 作 callKey，若模型对多个 tool_use 下发相同/空 id，历史渲染会合并成一张卡；保存时 `repair.rs` 还会把同 id 重复 tool_use 折叠丢弃。
- **openai_chat 空 id canonical index 碰撞**：`OcAccum` 对两个带相同 id（含空串）的流式 tool_calls 会映射到同一 canonical index（`openai_chat.rs` 兜底逻辑的两个风险点：无 id 首帧不发 ToolCallBegin、`entry(id).or_insert(next)` 撞车）。
- **read 失败隔离未实现**（[docs/tools-optimization-and-gap-fill-plan](./tools-optimization-and-gap-fill-plan.md) A3 计划项）：批量中一个文件失败 = 整个调用报错，其余文件结果全丢。

## 7. 手动验证清单

1. 让模型批量读取 2+ 文件 → 工具卡头部显示所有文件名（不含路径），悬停可见完整列表；展开后各文件块仍显示完整路径 + 行号范围；
2. 让模型一次编辑多个文件 → 头部列出全部被编辑文件名；
3. 单文件 read → 头部仅显示该文件名；create / delete → 仍显示完整路径；
4. 超大入参场景（如一次读十几个大文件）→ 完成后头部仍能列出文件名；
5. 历史会话重开 → 展示一致。
