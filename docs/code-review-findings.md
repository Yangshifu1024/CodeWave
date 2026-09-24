# 代码审查发现（2026-08-30，P1+P2 完成后）

> 审查方式：双代理并行审查（Rust 后端 / Vue 前端），全部 27 个前端源文件与 11.5k 行 Rust 后端。
> 结论：**需修复后合入**。后端 2 Critical + 6 High；前端 3 Critical + 5 High。
> 架构面评价良好（分层纪律、契约一致性、纯函数化与测试意识），问题集中在"真实协议路径无集成测试"与"对抗性用例缺失"。

---

# 一、Rust 后端审查报告

## Critical

**C1. `rt.running` 永不复位，同一会话第二次 run 必然被拒**
- 位置：`src-tauri/src/core/agent.rs:205`（`swap(true)`）；run_chat 结束路径均无复位（drive_agent 重构时从旧 run_chat 尾部遗失）
- 后果：任一会话跑完一次 run 后，第二次 start_chat 永远报"已有运行中的任务"；compact_session 也永远拒绝。**这是重构引入的回归。**
- 建议：run_chat 尾部复位，或 RunningGuard（Drop 复位）；补"run 结束后可再次 start"测试。

**C2. anthropic 流壳每个 chunk 后调用 `parser.finish()`，TCP 分片撕裂 SSE 事件**
- 位置：`src-tauri/src/provider/anthropic.rs:273-275`
- 后果：一行 data 被分片切开时，前半被当完整事件派发（JSON 解析失败 → Protocol 错误），重试同样随机撞分片。openai_chat/openai_responses 无此问题（不调 finish）。
- 建议：删除循环内 finish，仅 EOF 后调用一次。

## High

- **H1** web_fetch 的 SSRF 逐跳校验被共享 client 的自动重定向击穿（第一跳公网过审后 302→内网由 reqwest 内部直跟）。修：改用 `Policy::none()` + 手动逐跳（http_request.rs 已是正确写法）。
- **H2** 审批等待（approval.rs:33，120s）与压缩（context.rs:159，180s）不响应取消——用户点停止后 run 仍挂最长 2 分钟。修：select cancel token。
- **H3** fence L2 只收集 `word|string` 节点，漏 `raw_string`（单引号）与 `concatenation`：`tee '/etc/cron.d/evil'` 绕过写目标检查。
- **H4** fence escalate 无 Allow→Confirm 升级路径：多写目标时先 Allow 后 Confirm 被吞。修：显式 rank 比较。
- **H5** L1 不剥 `sudo/env/nohup/timeout/xargs` 前缀，`sh -c 'rm …'` 不解析 → 删除黑名单可绕过。
- **H6** `$HOME`/`${VAR}` 写目标按字面路径判为区内 → 实际写到 home/系统目录。修：含 `$` 的目标一律 Confirm/Block。

## Medium（M1–M17 摘要）

- M1 批次写冲突检测 canonicalize 未 join workspace（`a.txt` vs `./a.txt` 漏判）
- M2 anthropic 不合并连续同 role 消息（plan 快照/注入消息路径 → 连续 user → 400）
- M3 重试预算 attempt 跨 step 不重置（长 run 后段一次 429 即终止）
- M4 网络错误重试不清空流缓冲（前端看到重复的半截回复）
- M5 空响应不可重试时仍 push 空 assistant 入历史
- M6 force_report 末步 tool_use 已入历史但无 tool_result（内存历史悬空）
- M7 每次 checkpoint 重置 created_at
- M8 会话索引读改写竞态 + 损坏时静默覆盖（需 Mutex + 备份）
- M9 service 输出泵顺序 read 双管道（stderr-only 服务假死）；表满 16 静默插入失败
- M10 MCP 重连条件过宽（业务错误/超时也会重放非幂等工具）；stdio 命令构建死代码 —— **已关闭 2026-09-24**（[mcp-module-rebuild](./mcp-module-rebuild.md)：改为结构化错误分类，只有连接类错误允许重连且只重放只读调用；stdio 命令构建随 `mcp/process.rs` 重写）
- M11 prompt cache：动态 system（lessons.md run 中被写）与单断点削弱 cache 收益（建议 run 级快照 + 滚动断点）
- M12 terminate_tree 同步 sleep 阻塞 async worker；pid=0 时 kill(-0) 杀自身进程组
- M13 SSRF：IPv4-mapped IPv6 逃逸；DNS rebinding 窗口
- M14 运行中删除会话会被 checkpoint 复活（running 时应拒绝）
- M15 select_workspace_dir 阻塞 tokio worker（改 spawn_blocking）
- M16 子代理高危审批永远无法被批准（resolve_ask 不查 core.subs，只能超时拒绝）
- M17 web_fetch 50MB 限制仅在 Content-Length 存在时生效（chunked 绕过）

## Low / Nit（摘要）

L1 get_or_create_session 竞态（用 dashmap entry）；L2 sanitized 尾 4 位字节切片（非 ASCII panic）+ unmask 按索引错位；L3 save_config 明文 key 不即时迁 keyring；L4 重试循环内反复读 keyring；L5 占位符/明文混合态误发占位符；L6 ChannelRegistry 只增不删；L7 save_mcp_config 注释与行为不符（**已关闭 2026-09-24**：随命令重写为 `mcp_save_config`，注释与行为一致）；L8 /etc canonicalize 等值判断失效；L9 trim 递归 O(n²)；L10 子代理/任务 usage 不入 stats；L11 subagent 并发 check-then-add 非原子；L12 command/service 标 ReadOnly 无写互斥；L13 anthropic 空 key 仍发头、openai 新模型需 max_completion_tokens、Responses 未 store:false；L14 fence fallback 下 L2 全失效（可故意语法错误绕过）；L15 stats 退出丢 60s 数据、skills 死代码。Nit：run_task_agent spawn+await 多余、call_key 来源不统一、`$(`白名单条件冗余、tauri 隔离纪律良好。

## 测试盲区（审查确认）

1. anthropic 流壳零集成测试（C2/M2 因此漏网）——需 mock SSE 分片用例（一行 data 跨 chunk）与 role 交替断言
2. fence 对抗性用例缺失（sudo rm/单引号/$HOME/多写目标/语法错误 fallback）——H3–H6 全在盲区
3. 取消语义无用例（审批中取消、压缩中取消）
4. stream_model 不可注入 → running 生命周期（C1）无法测
5. SSE feed/finish 组合语义无回归保护
6. batch 冲突行为测试（非仅纯函数）
7. stderr-only 服务 / 并发 checkpoint 竞态 / KeyPool 混合态无覆盖

---

# 二、Vue 前端审查报告

## Critical

**C1. 事件注册清单与 handler 清单脱钩，11 类后端事件从未被监听**
- 位置：`ipc/events.ts:9-15` vs `stores/run.ts` bindGlobalHandlers
- 未注册：`run:suggestions`、`plan:update`、`mcp:status`、`service:update`、`sub:spawn/step/usage/done/error`、`scheduled:fired/done` —— 后端都在 emit，前端 handler 全是死代码。
- 后果：计划面板永不更新、子代理卡片永不出现、建议 chips 永不显示、MCP 计数恒空、任务 toast 永不弹。
- 建议：`names` 从 `Object.keys(handlers)` 派生；加"后端 emit ↔ 前端注册"契约测试。

**C2. "停止服务"按钮是空壳**
- `ToolCallCard.vue:170-175` 只调 listWorkspaceDir（注释自认 no-op）；后端也无 stop_service 命令。且 service:update 事件未绑定（C1），卡片状态不会翻转。

**C3. WorkspaceExplorer 跨 Tab 状态污染，可写错文件**
- `WorkspaceExplorer.vue:112` 仅 onMounted 加载一次；切 Tab 不重建 → 树停留旧工作区，`save()` 用新会话 activeId 写旧工作区的相对路径 → 落盘到错误文件。
- 建议：watch activeId 清空并重载；openFile 记录所属 sessionId，save 前校验。

## High

- **H1** 全局 Escape 无作用域判断：关弹窗/关菜单/输入法取消组合都会误停运行中任务。修：isComposing/defaultPrevented/最近弹层判断。
- **H2** API key 掩码按索引回写：插行/删行后 `***xxxx` 会还原成另一把 key 的明文（config.rs unmask_from）；前端中途过滤空行导致无法回车加 key。修：行数一致才按索引恢复 + 提交时才过滤。
- **H3** args_preview 截断 2000 字符 → 大参数 edit/create 的 diff 静默消失（JSON.parse 必失败走 catch 空）。修：结构化传递或截断保合法 + truncated 标记。
- **H4** run.tabs 桶只增不删（closeTab 不清、迟到事件重建、st(null) 建 "" 键）→ 长跑内存无上界。修：closeTab 调 dispose；未知 session 事件丢弃。
- **H5** 编辑器打开文件不检查未保存修改（直接覆盖），保存无冲突检测。

## Medium（M1–M8 摘要）

M1 Composer 手动 NPopover 不响应点击外部；`/` 开头菜单恒开吞 Enter。M2 MCP Ready 判断永假（serde snake_case 是 `"ready"`；Error newtype 序列化为对象）+ mcpStatus push 不去重。M3 openSession 无并发防抖、错误未捕获。M4 重开运行中的会话丢 running 态（可并发两个 run；start_chat 建议加原子检查）。M5 toggleSkill 绕过 draft 即时落盘与"取消/保存"语义冲突。M6 NInputNumber 清空为 null → serde 报晦涩错误。M7 提及搜索无防抖/乱序保护。M8 滚动吸底条件覆盖不全、切 Tab 滚动位置丢失。

## Low / Nit（摘要）

L1 bindEvents unlisten 与 keydown 未解绑（HMR 重复注册）；L2 suggest 双发 run:done（两份通知）；L3 v-html 现状可接受但建议补 XSS 用例；render_html iframe 受 CSP `default-src 'self'` 限制可能禁内联脚本（建议实测）；L4 AskPanel selected/notes 不随新 ask 重置；L5 历史恢复卡片伪造 ok 状态；L6 树 loaded 后永不刷新；L7 部分文案未走 i18n；L8 [docs/technical-design](./technical-design.md) §5.2 事件名与实现漂移（run:compacted vs compacting、sub:tool vs sub:step、缺 service:update/scheduled:*/run:suggestions/run:cancelled）；L9 全量 highlight.js 进主 chunk + github-dark.css 亮色模式下代码块仍暗底。Nit：displayName 恒等、argsPreview 双重 parse、ToolCallCard 默认全展开 DOM 压力、:key 用索引、window.prompt 重命名体验、含空格路径提及未加引号、ui.font_size/ui.accent 死配置。

## 测试盲区

1. 事件路由零覆盖（mock listen 不派发）——C1 类回归正由此漏网；建议保留 listen handler 引用手动派发断言 store。
2. Channel 帧（delta 合并/tool_progress 占位）无断言。
3. 多 Tab：closeTab/cycleTab/重复 openSession 幂等。
4. Composer 键盘路径（Enter/isComposing/菜单选择/@ 替换/Escape）。
5. AskPanel 两种提交载荷形状与后端读取字段对齐。
6. SettingsModal keys 过滤与保存载荷快照、取消不落盘。
7. Explorer 懒加载/保存、ToolCallCard 各分支、失败 IPC 降级。

---

# 三、修复优先级建议

1. **P0 级（立刻）**：后端 C1、C2、H1、H2；前端 C1（事件绑定+契约测试）、C3、C2（stop_service 补全）
2. **P1 级**：后端 H3–H6（围栏对抗性修复+用例集）、M1/M2/M10；前端 H1/H2/H4/H5、M2/M5
3. **P2 级**：其余 Medium/Low 按迭代清理；补齐 7+7 项测试盲区（尤其 anthropic mock 分片、fence 对抗用例、事件路由派发测试）

---

# 四、修复记录（2026-08-30 修复轮）

验证基线：`cargo test`（src-tauri/）130 通过、0 warning；`pnpm --dir frontend test` 8 通过（6 冒烟 + 2 事件契约）。
> 更正（见 §五复核轮）：本表 C2 行原声明"已移除流内 finish"与代码不符——首轮修复实际未改到该处，复核轮新增的 anthropic 分片集成测试暴露后已真正修复。

## 后端（全部已修）

| 编号 | 修复内容 | 回归测试 |
|---|---|---|
| C1 | `run_chat` 结束后补 `rt.running.store(false)`，恢复单 run 守卫语义 | `running_flag_resets_after_run_ends`（mock 401 服务器） |
| C2 | ~~anthropic.rs 移除流内 `parser.finish()`~~ → **首轮未实际修复**；复核轮真正移除流内 finish 并在 EOF 统一派发挂起事件 | `anthropic_sse_line_split_across_segments`（新增，TCP 分片撕裂回归） |
| H1 | `guarded_get` 改用 `Policy::none()` 内部客户端（MANUAL_CLIENT OnceLock），不再继承代理/SSRF 策略客户端 | 既有用例 |
| H2 | ① `ApprovalGate::confirm` 接受 `CancellationToken`，`select!` 竞争取消（取消=拒绝）；② `compact_history` 透传 run_token；③ config `unmask_from` 改为按掩码精确匹配（非索引），不匹配行丢弃 | `unmask_index_shift_does_not_swap_keys` |
| H3 | fence 参数收集补 `raw_string`/`concatenation`（去引号）；`rank_of` 显式 Allow<Confirm<Block 升级序 | 7 个对抗性用例 |
| H4 | TRANSPARENT 前缀（sudo/env/nohup/nice/timeout/command/time）剥离后复审；`xargs rm` 拦截；`sh -c` 递归（depth≤2） | 同上对抗用例集 |
| H5 | `$VAR`/`~user` 写目标 → Confirm(HighRisk)，审批关闭时 Block（approval_enabled 流入 FenceCtx） | 同上 |
| H6 | args_preview：超 200k 字符时输出含 `_args_truncated` 的合法 JSON，不再硬截断破坏 JSON | 编译期验证 + 前端配合展示 |
| M1 | batch `canonical_arg_paths`：join workspace + canonical_best_effort | batch canonicalize 用例 |
| M2 | anthropic `merge_adjacent` 合并相邻同角色消息 | `consecutive_same_role_merged` |
| M3 | 成功后 attempt 归零 | — |
| M4 | 重试前 `rt.stream.reset()` | — |
| M5 | 空响应直接收尾、不再回填空消息 | — |
| M6 | force_report 先执行已收工具再 break | — |
| M7 | upsert_meta 保留原 created_at | — |
| M9 | service 双独立 drain 泵 + ticker 节流 emit；表满（16）返回 Err；pid `unwrap_or(1)` 注释说明 | — |
| M10 | MCP 仅对 invalid_session/通道关闭/发送失败重连，业务错误直接返回；删除死代码 stdio 命令构造 | — |
| M12 | `child.id()` 改 `let-else`；terminate_tree 用 block_in_place 包阻塞实现 | — |
| M13 | IPv4-mapped IPv6 归一化后再判私网；新增 `read_body_limited`（超限 E_TOO_LARGE，流式读取不整包进内存） | net 用例 |
| M14 | delete_session 拒绝运行中会话 | — |
| M15 | 文件对话框移入 spawn_blocking | — |
| M16 | resolve_ask 回退扫描 core.subs | — |
| M17 | web_fetch 正文经 `read_body_limited` 限长后 from_utf8_lossy | — |
| 附加 | sub:usage 事件在 sub:done 前补发（契约测试发现） | 见前端契约测试 |

## 前端（全部已修）

| 编号 | 修复内容 | 验证 |
|---|---|---|
| C1 | `bindEvents` 遍历 `Object.keys(handlers)` 动态绑定（替代硬编码 13 个事件名） | 事件契约测试 |
| C2 | ToolCallCard「停止服务」真正调 `ipc.stopService`（后端新增命令） | 冒烟 |
| C3 | WorkspaceExplorer `watch(activeId)` 重置树/打开文件/内容；openFile 携带 sessionId；save 校验归属 | 冒烟 |
| H1 | 全局 Escape：跳过 isComposing/defaultPrevented 及 `.n-modal/.n-drawer/.n-popover/.n-input/textarea/input` 目标 | 冒烟覆盖弹窗场景 |
| H2 | run store `dispose(sessionId)` 删桶；`run:done` 幂等（`if (!s.running) return`） | 契约测试 |
| H3 | ToolCallCard 对 `_args_truncated` 显示「参数过大，无法展示 diff」 | — |
| H4 | sessions `closeTab` 调 `useRun().dispose(key)`；openSession 并发防抖 + 错误捕获 | — |
| H5 | 编辑器打开未保存文件先确认 | — |
| M2 | mcp:status 按 name upsert 去重 | — |
| M5 | toggleSkill 仅改 draft，随保存统一落盘 | 冒烟 |
| 附加 | 新增 `events.contract.test.ts`：双向扫描 Rust `emit(` 事件名 vs 前端 handler keys | 2 用例 |

## 新增测试基础设施

- **事件契约测试**（`frontend/src/__tests__/events.contract.test.ts`）：读取 src-tauri 源码正则提取事件名，与前端 `ipc/events.ts` handler 集合双向比对——新增后端事件漏绑/前端死 handler 均直接红灯。本轮即抓到 `sub:usage` 漏发。
- **fence 对抗用例集**：sudo 前缀、env 透传、`xargs rm`、`sh -c` 嵌套、`$VAR` 目标、`~user` 展开、Allow<Confirm<Block 升级序。
- **前端冒烟 6 件套**：主窗口、设置 5 Tab+表单、抽屉、统计、MCP/Skills、Composer 菜单（Tauri IPC mock）。

## 暂缓项（不在本轮范围，留待后续迭代）

- M11 请求前缓存快照（cache-first 字节稳定性增强）
- M13 DNS-rebinding 防护的独立 Connector（当前仅 IP 字面量+重定向校验）
- 前端 M4：重开运行中会话时 UI 不恢复 running 态（后端 `start_chat` 原子守卫已兜底防并发 run，仅剩展示层体验）
- L 系列按 §一 Low/Nit 清单逐迭代清理（L10 子代理/任务 usage 不入 stats、L13 max_completion_tokens/store:false、L14 fence 语法错误 fallback 下 L2 失效——L1 黑名单仍生效等）
- 测试盲区 §二 中第 2–7 项（Channel 帧断言、Composer 键盘路径、AskPanel 载荷对齐等）

---

# 五、复核轮补修（2026-08-30，逐条对账发现）

对 §四 逐条回查代码后发现的遗漏，全部补修并验证（cargo test 134 通过 / 0 warning，前端 8 通过）：

| # | 遗漏 | 补修 |
|---|---|---|
| 1 | **C2 首轮实际未修**：`parser.finish()` 仍在流内循环（§四表格曾误报已修） | 真正移除流内 finish，EOF 后统一派发挂起事件；新增 anthropic TCP 分片撕裂集成测试（测试先行写法直接复现了该 bug） |
| 2 | **M8 整条漏修且未登记**：会话索引读-改-写无锁（并发 checkpoint/删除互相覆盖）；损坏时静默以空索引覆盖 | SessionStore 增加 `index_lock` 互斥 + `mutate_index` 收敛全部写路径（upsert/remove/rename）；损坏索引先备份为 `index.json.corrupt` 留证再重建；新增并发 upsert 与损坏备份两个测试 |
| 3 | **前端 H2 后半漏修**：keys 输入框输入时立即过滤空行 → 回车永远无法新增 key | 输入过程保留空行，`save()` 提交时才过滤 |
| 4 | **L2 前半漏修**：`mask()` 尾 4 字节切片对非 ASCII key 会 panic | 回退到 char boundary 取尾；新增非 ASCII 掩码测试 |
| 5 | §四 C2 行"既有 SSE 分片用例"声明不实（实际只有 openai 用例） | 即上 #1 的新增测试；本节即为更正记录 |
| 6 | 杂项：2 个测试期 warning（scheduled_task unused import、fuzz unused var） | 清理，恢复 0 warning 基线 |

复核结论：其余 §四 已修条目均经代码核实真实存在（C1/H1–H6、M1–M7/M9–M17、前端 C1–C3/H1–H5/M2/M3/M5）；功能层对 P0（G1–G10）/P1（A–E）/P2（F–I）计划无缺项，P2-J（SSH/server/移动端）为评估后顺延而非遗漏（[docs/p1-p2-implementation-report](./p1-p2-implementation-report.md) §3-J）。
