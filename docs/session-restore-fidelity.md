# 会话完整恢复：盘点 + 工具结果原样 sidecar

> 2026-09-24 · 「关掉重开会话后信息是否有损」的全面盘点，及其第一批修复（P0）。
> 关联：[html-preview-modal](./html-preview-modal.md)（widget 预览的历史回放由本文机制解决）、[session-artifacts-and-files-tab](./session-artifacts-and-files-tab.md)、[session-cleanup](./session-cleanup.md)、[session-restore-batch1](./session-restore-batch1.md)。

## 1. 盘点：哪些信息没有完整落盘

### A. 内容被瘦身（历史里只存「模型侧瘦身文本」）

| 项 | 现状 | 恢复后表现 |
|---|---|---|
| **工具卡全量出参（`ToolOutcome`）** | 完整 JSON 只走 `tool:result` 事件，**没有任何落盘通路**；历史里存的是 `compact_for_model` 产物（`src-tauri/src/tools/compact.rs`：通用分支 HEAD 4KB + TAIL 8KB 头尾截断） | 出参 >约 12KB 的卡片全部退化：`render_html`（大 html）、`grep` 大量命中、`web_fetch`/`http_request` 正文、`read_document`、`edit` 的 edited 列表、`ask` 载荷、子代理 `report` → 前端只拿到 `{restored:true}` 占位（`ui/src/stores/run.ts` 的 `restoredToolData`） |
| 工具卡耗时 `durationMs` / `status: waiting` | 只在帧里，无落盘 | 历史卡无耗时；看不出「当时在等审批」 |
| 图片 `data_url` | 刻意不入历史（恢复时按路径重读） | 文件被移动/删除即丢；**附件图片已改为外置 blob**（不再进历史文件，8MB 上限不再被图片顶到，[session-history-limits](./session-history-limits.md)） |
| 子代理 `report` | 走父历史工具结果文本（>12KB 被头尾截断） | 汇报正文中段被截（`sub_id` 因放 data 首位得以保住，关联不断）→ 已由 §2.4 的 sidecar 回填补全 |

### B. 结构与状态

| 项 | 现状 | 恢复后表现 |
|---|---|---|
| **无结果 / 被中断的调用** | 恢复路径忽略结果缺失，统一按「已使用」 | 卡片绿色「已使用」，与子代理流的「已中断」矛盾（`ui/src/stores/run.ts` 的既有遗留注释） |
| 真实失败结果 | 结果文本是 `[error E_XXX: 说明]`（`command` 失败例外：`data` 仍带 `output`/`exit_code`） | 卡片只剩「已使用」，错误码、说明与出参都丢 |
| 空 assistant 消息 | `core/sessions/repair.rs` 直接丢弃 | 取消/校验失败后「提问 → 无回答」跳空 |
| `SessionMeta.interrupted` | 只在用户点击「知道了」时清除（`AppShell.tsx` / `ProjectNav.tsx` 调 `clearSessionInterrupt`），无自动清除 | **正常**：重启后横幅与左栏徽标照常出现（不属恢复缺口） |
| 历史 trim | 保存与加载各 trim 一次（256k 预算、保留最后 2 轮） | 旧轮次永久消失 |
| checkpoint 粒度 | 每 20 步一次 | 崩溃最多丢约 20 步转录 |
| 8MB 硬上限 | 超限先剥图，仍超则拒绝保存且只 warn | 整轮可能丢失且无 UI 提示 |
| thinking 耗时 | 纯内存 | 恢复后每个思考块显示「已完成 1s」占位 |

### C. 会话级统计与运行态

- **会话级 `usage`（工具条上下文% / 命中率）无落盘**（`core/stats.rs` 只有 by_model/by_workspace/by_kind/day，无 `by_session`）→ 重启后两段空白。
- `runMetrics`（本轮速率）按轮归零——语义如此，不算缺陷。
- todos 双源（后端 `<id>.todos.json` + `ui-state` 快照）可能不一致。
- 运行中会话重开：只恢复 `running` 布尔，不重建在途工具卡/ask。

### D. 后端运行时态

- 后台服务纯内存、无退出 stop-all → 重启列表空、可能孤儿进程（未做进程实验）。
- 自由会话的计划任务不落盘 → 重启即丢。
- MCP 每会话可见集按配置现算（设计如此）；`reasoning_rejected` 粘性标记为会话级内存（可接受）。

## 2. 本批修复（P0 + 子代理接入）

### 2.1 工具结果原样 sidecar（一处机制解决 A 组全部条目）

- **模块**：`src-tauri/src/core/sessions/tool_results.rs`；**目录**：`~/.codewave/sessions/<owner>.toolres/<call_id>.json`。
- **键用 provider 侧 `tool_use.id`**：恢复后前端手上唯一稳定的标识就是它（`ui/src/stores/run.ts` 的 `callKey = c.id`）；实时 `ctx.call_key` 是批内随机的 `batch:index`，不可持久。`sanitize_call_id` 只保留 `[A-Za-z0-9_-]` 并截断，**写路径与读路径共用**同一函数。
- **写入点**：批次层组装模型侧文本处（`src-tauri/src/tools/batch.rs::model_content`）——它同时掌握完整 outcome 与 `call.id`。
- **写入门槛**：`should_persist` = 「模型侧文本**解析不成 JSON**」。判据与前端 `restoredToolData` 的失败条件同源：
  - 头尾截断（截断提示插在 JSON 中间）必破坏结构 → **落盘**；失败但有出参的调用（失败 `command` 的 `[error …]` + JSON 体）也解析不了 → **落盘**；
  - `read`/`batch_read` 原样透传、读图那种「合法 JSON 但剥了 `data_url`」、未触发截断的小出参、没有出参的失败调用 → **不落盘**（磁盘与回读量因此有界）。
  判定在追加 model hint（plan 软提醒）**之前**做，否则带提醒的结果会被误判。
- **存的是整套 `ToolOutcome`**（`ok`/`data`/`error`/`warnings`）：前端连成功/失败状态一起还原（失败卡不再被当成成功卡）。单文件超 2MB（与读取侧同阈值）直接跳过并记 warn——写了也读不回来。
- **owner** = `root_session_id ?? id`（子代理产生的卡片跟主会话走，与产物登记同一口诀）；计划任务运行态跳过（无产物消费方）。
- **生命周期**：随会话删除（`core/sessions/cleanup.rs::delete_session_files` 里的目录级联 + 索引外残留扫描认 `.toolres`）；**不进右栏「文件」面板**（那里读的是产物登记边车）。
- **回读**：IPC `load_tool_outcomes(session_id, call_ids)`（`host/commands/session.rs`）；`tool_results::load_many` 按入参顺序返回**实际存在的**条目，缺失/非法键/超限文件静默跳过，并设 `MAX_CALLS = 50` 条数上限与单文件 2MB 上限。
- **前端接入**：`ui/src/stores/run.ts::restoreFromMessages` 在历史重建时收集「解析失败」的调用（`lossyToolKeys(msgs)`，判据与 `restoredToolData` 同源：文本在、却解析不出 JSON 对象），**一次批量** IPC 拉回后按 `callKey` 回填 `toolsMap`（含 `durationMs`）。先落占位、后补内容，首帧不阻塞；没有备份（旧会话/已清理）就保持占位，卡片沿用既有「历史未保留预览内容」提示。
- **顺带收益**：`render_html` 的 widget 预览在重开会话后**零特判**即可恢复（`data.html` 回来了，卡片入口与弹框不需要任何改动）。

### 2.2 恢复的 status 语义对齐

- **无结果 / 被中断**（后端 `repair` 补的 `[interrupted] …` 文本，或整条结果缺失）→ 卡片落 `E_INTERRUPTED`（中性灰「已中断」），与子代理流一致；不再谎报「已使用」。注意**不能用 `is_error` 当判据**：它同样覆盖真实失败。
- **真实失败**（`[error E_XXX: 说明]`）→ 从文本里还原错误码与说明，卡片按失败呈现（`data` 早已不在历史里）。

### 2.3 开销边界（为什么不会让上下文 / 负载爆炸）

1. **永不参与出网**：sidecar 不进 `rt.history`、不进 wire → 模型上下文、token 计费、256k trim 预算与改动前逐字节相同。
2. **只在模型侧文本解析不了时才落盘**：`read` / 原样透传的 `command` / 读图 / 未截断的小出参都不产生文件。
3. **只回读解析失败的调用**，且一次批量 + 条数/单文件双上限；能被历史文本解析的卡片一次 IPC 都不发。
4. **量级与实时一致**：拉回来的数据 ≈ 当年实时运行时前端本来就持有的那份；渲染路径与实时完全相同。
5. **磁盘有界且自清理**：随会话删除，无额外容量策略（与计划文件同生命周期）。

### 2.4 子代理卡与过程抽屉走同一条通路

- **子代理卡（`subagent` 调用）**：父历史里那份 outcome 文本被截断时，`restoreFromMessages` 无从解析 `sub_id`，只能落合成 key `restored:<callKey>`（抽屉降级展示 task，无过程流）。现在 sidecar 行回来后再做一次**改名**（`renameRestoredSubs`）：timeline 的 `sub` 锚点、`subs` 条目与 `subStreams` 桶一起从合成 key 换成真实 `sub_id`，并补回被截断的 `report` —— 抽屉因此能拉到真实过程流（`load_subagent_history` 认真实 id），汇报正文也完整。
- **过程抽屉内的工具卡**：`openSubDrawer` 按需拉回子代理历史后，同样对 `lossyToolKeys(msgs)` 发一次批量 IPC（会话 id 用**根会话**——子代理结果本就存在根会话的 `.toolres` 下，`owner = root_session_id ?? id`），把抽屉里的工具卡出参换成完整版。
- 两处与主会话卡片共用 `backfillToolOutcomes` / `applyToolOutcomes`：失败静默、无 Tauri 运行时（纯 store 测试）兜底，回填语义只有一份。

## 3. 已知边界（本批未覆盖）

- **超过 2MB 的出参不备份**（写入侧与读取侧同阈值，直接跳过并记 warn）：极大文档/响应体仍会退回占位。
- **`durationMs` 采集未做**：sidecar 记录里字段已留（`duration_ms`），需要把每次调用的耗时一并带进批次层的结果向量（涉及 4 处同源赋值），列为 P1；因此回填后历史卡片仍无耗时（`duration_ms` 为 null 时前端不覆盖原值）。
- **旧会话（本机制上线前）无备份**：`.toolres` 里没有文件，回读返回空 → 卡片保持占位文案（行为与接入前一致，不报错）。
- 图片仍按路径重读（原设计）；`data_url` 不入历史。
- 历史 trim / checkpoint 粒度属于结构性话题，见 §4；**8MB 上限与图片外置已落地**（[session-history-limits](./session-history-limits.md)：附件图片搬出历史文件 + 越限可见 + 按轮降级，不再整份静默丢弃）。

## 4. 未做（分级）

**P1**：空 assistant 保留（历史里保留、发送前再丢）；`SessionMeta.interrupted` 不再被加载即清（需先定义「何时算已读」）；会话级 usage 落盘（工具条跨重启）；thinking 耗时落盘；`durationMs` 采集；运行中会话的在途卡片/ask 重建。
**P2（与 [session-restore-batch1](./session-restore-batch1.md) 的「批2 范围」重叠，建议单独批次）**：历史 trim 双删与 256k 预算口径；checkpoint 粒度 → append-only JSONL；后台服务重启清理；自由会话计划任务落盘。

## 5. 验证

- `cargo test`：**1020 passed / 0 failed / 3 ignored**（新增 `tool_results` 模块 5 条 / 清理随会话删除 1 条 / 回读响应体 1 条；子代理接入这批未动后端）
- `pnpm --dir ui test`：**1085 passed / 94 文件**（`run.restore-toolresults.test.ts` 共 11 条：截断→回填、无备份保持占位、可解析不触发回读、被中断落 `E_INTERRUPTED`、结果缺失同前、真实失败还原错误码、`lossyToolKeys` 判据、子代理卡改名+补 report、抽屉内工具卡回填；另按新语义更新 `run.interleave.test.ts` 的两处既有断言）
- `pnpm --dir ui run lint` 0 error、`pnpm --dir ui build` 通过

人工点验清单：

1. 让 Agent 调 `render_html` 生成一个 >12KB 的 widget → 关掉重开会话 → 卡片仍在，点「预览」能看到完整内容。
2. 同一会话里验一条大 `grep` 命中卡 / 一条大 `web_fetch` 正文卡 → 重开后内容同样完整。
3. 造一条被中断的调用（运行中取消）→ 重开后卡片显示「已中断」（中性灰），不是「已使用」。
4. 确认普通小工具卡（如 `read` 小文件）**没有**产生 sidecar 文件（`~/.codewave/sessions/<id>.toolres/` 里只有被截断的那些）。
5. 删掉该会话 → `.toolres/` 目录随之消失；右栏「文件」面板里从来不出现它。
6. 跑一个 `subagent`（任务量足以让 report >12KB）→ 关掉重开会话 → 子代理卡仍是同一条、汇报正文完整，点卡片打开过程抽屉能看到子代理内部的工具卡（含大出参）。
