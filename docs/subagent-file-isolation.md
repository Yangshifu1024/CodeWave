# 子代理隔离与运行监督批次

> 2026-09-08。本批次收敛自一轮多问题排查：主会话停止无法级联、子代理交叉编辑、派发失败无重试、
> 步数显示失真（120/60）、ask 多题误提交、子代理无停止按钮、缺运行监督、主会话大阅读污染上下文。
> 后端 433 测试 / 前端 267 测试全绿，cargo 0 warning。

## 一、文件写认领制（严格物理隔离）

**需求**：并行子代理不得交叉编辑同一文件；隔离不了的共享文件由主代理亲自完成。

**机制**（`src-tauri/src/tools/claims.rs`，与 writelock 正交——writelock 串行化单次写操作，
认领表锁定整个任务周期的文件范围）：

- 全局注册表 `path → owner(runtime id)` + 反向索引，key 走 `canonical_best_effort`（与 writelock 同源别名归一）。
- `claim(owner, paths)`：单锁内**先全查后全插**；任一路径与既有认领**相同或互为祖先**
  （`Path::starts_with` 双向，覆盖 delete 目录 vs 兄弟写其内文件）；owner 自身幂等。
- edit / create / delete 三个写工具在路径规范化后、写锁前挂接：`!ctx.rt.is_main_session` 才检查，
  冲突返回 `E_FILE_CLAIMED`（终结式文案：不得重试/等待，剔除该文件并在汇报「未完成」中列明）。
  edit 多文件任一冲突整体拒绝（不部分应用）。
- **主会话豁免**——共享文件（配置/锁文件/汇总导出/公共类型）由主代理亲自修改，是隔离模型下
  共享文件的唯一合法出口。
- 释放：`ReleaseGuard`（RAII）在 `drive_agent` 开头对非主 runtime 武装，正常结束与 panic unwind
  都经 Drop 释放；先结束先释放，顺序派发复用同文件不受影响。

**边界（如实记录）**：shell / MCP 写不经认领表（路径无法静态归因），由提示词纪律覆盖；
S6 已写明任务包文件范围互斥为硬约束、共用文件不进任何包、`E_FILE_CLAIMED` 文件待全部返回后由主代理补完。

## 二、派发失败重试硬化

| 失败类 | 旧行为 | 新行为 |
|---|---|---|
| `E_ARGS`（参数错误） | 错误文本回主代理，重试纯靠提示词 | serde alias 兼容 `max_steps`/`clean_context` snake_case 笔误；错误文案附 schema 提示（task/role 非空、maxSteps 1–1000、键名 camelCase）+「不计失败，修正参数立即重发」 |
| `E_SUBAGENT_BUSY`（并发满 4） | 立即快速失败 | `acquire_slot()` 有界等待（30s × 500ms 轮询，`select!` 监听 `ctx.cancel` 可中断）仍满才报错；文案明示「不计失败，可 wait 后原样重发」 |
| 子代理运行失败 | 轮内请求级重试 + 提示词「重试 1 次」 | 不变——机械重跑整任务有重复写文件风险，维持 LLM 裁决 |

## 三、取消级联与子代理手动停止

- **取消级联（缺陷修复）**：`cancel_run` 此前只取消主 runtime 令牌，子代理 drive 循环用自己新建的
  无关令牌，停止按钮形同虚设（子代理照跑到预算耗尽）。现 `DriveParams.parent_cancel` 传入父令牌，
  `drive_agent` 用 `child_token()` 派生本 run 令牌——父取消级联中止子的 LLM 流与审批等待；
  任务 runtime 不传，行为不变。
- **子代理卡停止按钮**：`SubagentItemCard` 运行中时右侧渲染 `.sub-card-stop`（`stopPropagation`
  防误开抽屉；中性色非红色），走既有 `stop_subagent` 命令（此前前端零接线的断线状态）。
- **主代理察觉**：subagent 工具的 Cancelled 分支区分两种情形——
  - 父令牌未取消（用户单独停此子代理）：返回 `E_SUBAGENT_STOPPED`，文案指示主代理
    「用 ask 询问用户是否重新派发，确认前不要自行重启」；
  - 父令牌已取消（级联）：文案「随主会话停止而取消」，不诱导询问（主 run 即将终止）。

## 四、运行监督机制（主/子/任务三路统一，挂 drive_agent）

- **重复操作检测**（`core/agent/supervise.rs`）：`BatchOutcome.call_summary` 携带每调用
  `(工具名, 错误码, args哈希)`；`SupervisionState::feed()` 维护 12 次滑动窗口：
  - 同失败签名窗口内 ≥3 次 → 注入 `<supervision-notice>` 纠偏（同签名一次）；
    纠偏后窗口内累计 ≥5 次 → 硬介入终止 run（「监督介入：循环未收敛」）。
  - 完全相同的成功调用（工具+args 哈希）≥4 次 → 纠偏。
  - 纯状态机，单测覆盖 nudge/escalate/窗口淘汰/签名独立性。
- **流停滞看门狗**：`ThrottledStream` 加 `last_activity` + `touch()`（reset 同步刷新），
  `collect_deltas` 每帧 touch；`run_llm_turn` 把 `stream_model` 改 spawn + select 竞速——
  超 `stall_timeout_seconds`（config 新字段，serde default 300，钳 [5,3600]，照顾本地大上下文
  模型预首 token 慢）无活动 → 取消本次尝试的 child token，按可重试 Server 错误走既有退避
  重试（预算 6 次，耗尽才终止 run）；用户取消与停滞同帧就绪时按用户取消收尾，不误报。
- **可见性**：纠偏消息进历史（主会话聊天可见、子代理经 sub:step 采样）+ session_log 记录；
  不加新事件键（28 键契约不动）。

### 空转看门狗（`feed_batch`；补文档 2026-09-19）

- **进展信号**（`BatchDigest`，由 `core/agent/drive.rs` 的 `batch_digest()` 从本批调用翻译而来）：
  ① 批次内含**非只读工具**（按 `supervise.rs` 的 `READONLY_TOOLS` 名单判名：read / batch_read / grep /
  calculate / list_files / web_fetch / render_html）；② `read` / `batch_read` 读到本 run **首次**的文件
  （文件级去重）。任一命中即清零全部空转计数。
- **`IdlePolicy::Stop`（默认）**：连续无进展 8 步注入 `<supervision-notice>` 纠偏（同一 run 一次）；
  纠偏后累计到 14 步硬终止（`Err` → 主会话 run:error、子代理 `E_SUBAGENT`），并注入
  `<supervision-escalated>` 引导下一 run 先 ask 询问是否继续。
- **`IdlePolicy::NudgeOnly`（只读 run）**：16 步纠偏一次、**空转层永不终止**（失败重复层 3/5 与 6/10、
  `max_steps` 与强制汇报门对只读 run 仍照常生效）；文案明示「只读角色没有写工具…
  本提示不会终止本 run」。空转层不靠终止收尾，预算天花板交给 `max_steps`。
- **同文件连续分段读**：同一文件连续分段读达 10 次 → 专用纠偏（只纠偏、不参与终止判定）。
- **只读角色策略**：`AgentDef.readonly`（`agents/mod.rs`）标记 `explore` / `reviewer` / `code-reviewer`；
  `tools/subagent.rs` 据此置 `DriveParams.idle_policy`，并为这些角色额外排除写工具
  `edit`/`create`/`delete`（保留 `command`，交由 fence 把关）。主会话 / 任务运行 / 可写子代理一律 `Stop`。
- **压缩后免惩罚重读**：自动压缩成功即调 `reset_idle()`——空转计数清零 + 已读文件集合清空
  （重读按「首次读」计），压缩丢细节后的重建上下文不计空转。

> 该机制的键位缺陷（只读 run 被误杀）与只读策略详见 [subagent-idle-watchdog-misfire](./subagent-idle-watchdog-misfire.md)。

## 五、子代理步数显示失真（120/60）

`sub:step` 的 `step` 此前取 `history.len()`——每 drive 步追加 assistant + tool_results 约 2 条
消息，60 步跑满即 120+，用户看到 120/60。**步数预算本身一直被严格遵守**（`for step in 0..max_steps`），
是上报口径错。现 `SessionRuntime.step_count: AtomicUsize`，drive 每步 store，轮询器改读真实步数。

## 六、主会话大量代码阅读强制委派

**需求**：主代理发现需要读大量代码时立即派子代理，不污染主会话上下文。

- **机械闸门**（read.rs）：主会话单次调用累计 `Σ min(请求窗口, 文件总行数)` 超过
  `MAIN_READ_LINE_BUDGET`（2000 行，与单文件默认窗口对齐）→ `E_READ_TOO_BROAD`（终结式文案：
  调研派 explore 子代理只回传结论；为编辑而读用 startLine/endLine 窗口——窗口读取同样取得
  version token，edit 不受影响）。子代理/任务豁免（读代码是其本职）；图片不计行数（独立 3MB 上限）。
  batch_read 是 read 的废弃别名，自动一并覆盖。
- **提示词纪律**：CORE_PROMPT `<tool-policy>` 加「大范围代码阅读立即派 explore 子代理」
  （该层无字数断言、缓存稳定前缀）；read 工具 description 同步说明。

## 七、ask 多题提交分页与回车语义（缺陷修复 + 交互批次）

多题 ask 在**任意页**点击「提交回答」都会把未浏览的题用 recommended 静默填充后一次性 resolve。
现分页感知：单题 = 直接提交（不变）；多题非末页主按钮显示「下一题」仅翻页不 resolve；
仅末页显示「提交回答」组装全量提交。recommended 预选保留（预选 ≠ 提交）。

**回车语义**（追加）：弹出问题时回车响应「下一题或提交」——非批准形问题回车 = 翻页（非末页）/
提交（末页与单题），与主按钮同路径；选项选中改由空格（高亮切换）、数字键、鼠标承担；
批准形完全不变（回车 = 确认高亮项，批准项直提）。补充说明输入框的键盘可达性由
「末槽 Tab 放行自然聚焦」兜底（原「回车聚焦输入框」让位）。提示文案（keyHintMulti/keyHintSingle）
同步更新。

**单选 radio 语义**（追加）：`single: true` 单选题点击已选中项**保持选中**（与批准形一致），
不再出现「再点取消」——选新项即替换；「未作答」路径由忽略按钮承担（清空当前题）。

## 八、验证与手动清单

- 后端 `cargo test`：433 passed / 0 failed / 0 warning（新增 claims/supervise/acquire_slot/
  stalled/throttle touch/read 预算测试；keyring migrate 测试在并行执行下偶发、单测可复现通过，
  与本批无关）。
- 前端 `pnpm --dir ui test` 267 passed（新增 ask 分页提交、子代理停止按钮测试）+ `pnpm --dir ui build` 通过。

手动验证（不做 GUI 自动点验）：

1. **多题 ask**：让模型一次问 3 题 → 首页按钮显示「下一题」，点击只翻页；末页显示「提交回答」，
   提交后三题答案齐全；单题 ask 直接「提交回答」。
2. **子代理停止**：派一个长任务子代理 → 聊天卡右侧出现停止按钮 → 点击后子代理卡转失败态，
   主代理收到 E_SUBAGENT_STOPPED 并用 ask 询问是否重派。
3. **停止级联**：派发多个子代理后点主会话停止 → 所有子代理流式中止、审批等待解除，run 正常出
   run:cancelled。
4. **步数显示**：跑一个多步子代理 → 卡片步数按 1、2、3…真实步数推进，不再出现 120/60。
5. **读预算**：主会话让模型一次读 3 个大文件 → 收到 E_READ_TOO_BROAD 并转派 explore 子代理；
   编辑前单文件读不受影响。
6. **文件隔离**：并行派两个 backend-dev 任务包，故意让范围重叠 → 后写方收到 E_FILE_CLAIMED，
   子代理跳过并在汇报未完成中列明，主代理补完。
