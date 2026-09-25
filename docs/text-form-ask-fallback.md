# 文本形态 ask 兜底（BYOK 端点把工具调用当正文透传）

> 缺陷来源：用户实测报告——会话 `5da292d8`（provider `MiniMax` / `api_format=anthropic_messages` / `base_url=https://api.minimax.cn/anthropic` / 模型 `MiniMax-M3`）里模型把 `ask` 调用**当正文 XML** 输出：提问卡不出现、没有任何提示，用户只看到裸 `<ask>…</ask>`。

## 一、结论

某一轮 assistant 回复**没有任何 `tool_use`**、正文末尾却漂着一段**结构完整**的 `<ask>…</ask>` 时，CodeWave 现在把它**恢复成等价的 ask 调用**，交给既有的工具执行链路：卡片、应答通道、G2/G3 门、`mode` 切档、`switchToAutoEdit` 批准、计划落盘**全部复用同一份实现**，与真实工具调用**逐字段同形**。

判定一律保守——只认「模型把自己要问的问题写成独占段落、写在正文末尾」这一唯一可确定形态；块级歧义一律放弃，退回既有行为。

## 二、证据链（为什么不是「模型不会用工具」这么简单）

1. **不是请求侧漏发 tools**：`provider/anthropic.rs:69` 只在 `req.tools` 为空时省略该字段；`exclude_tools.push("ask")` 只出现在**目标档执行期**（`core/agent/drive.rs` 的 `apply_goal_mode`），而该会话是普通 plan 档。旁证：同一会话前后 **8 次以上** `ask` 都是正常结构化 `tool_use`（用户还答过 `test_first`），`plan` / `read` / `subagent` / `web_fetch` 也全正常。
2. **不是解析或落盘丢帧**：若 SSE 里存在 `tool_use`，同一会话解析得出来；坏轮（`2026-09-25T03:29:29Z`）落盘的 assistant 消息 content **只有 `{"type":"text"}`**，XML 是以文本增量经 `asm.push_text` 进来的——落盘即原文，不是渲染或持久化产物。全仓（代码 / 提示词 / 技能 / 任务文档）**零命中** `<ask>` / `<questions>`，即这套 XML 协议不是 CodeWave 给的。
3. **触发时机可复**：坏轮紧跟在**同批次两次 `web_fetch` 双 404** 之后（03:29:23 调用 → 03:29:27 `is_error`），模型被工具报错打断后掉出了协议；下一轮它自己承认「**没有按规范输出提问**……没有把它包装成正确结构」。
4. **端点侧（推断，非实证）**：`api.minimax.cn` 不在本仓库记录的官方 CN 域名（`core/quota/mod.rs` 是 `api.minimaxi.com` / `minimaxi.com`）；该端点行为带「自带一层 harness」特征——模型正文混入私有特殊 token（实测 `]<]minimax[>[`，见下节）、思考块引用本仓库零命中的英文身份提示。最可能的机制是：端点把工具描述 prompt 化、再把输出里的 XML 解析回 `tool_use`，那一轮解析失败，原始 XML 被当普通文本透传。**要钉死必须抓原始 SSE**，而 CodeWave 不落盘原始请求/响应——这正是本批新增 step 级诊断日志的动机。

## 三、实现

### 1) 纯函数解析器 `core/agent/text_ask.rs`（新增）

- `salvage_text_ask(text) -> Option<TextAsk>`（`args` + 被剥离的块原文）、`strip_block(text, block)`。
- 判据（全满足才兜底）：
  1. 完整配对 `<ask>…</ask>`（开标签可带属性、忽略），块内不再出现第二个 `<ask`；
  2. **块独占起始行**（行首即块，或前一字符是换行）——行内代码 / 引述里的完整示例因此不触发；
  3. **块之后只剩空白**——模型把调用写在正文末尾，而「讲解协议」的正文后面通常还有话；
  4. 不在 Markdown 代码围栏内；
  5. 至少解析出 1 题；题数 ≤ 5、每题选项 ≤ 6。
- 字段顺序无关（先摘出 `<options>` 子树，再把前缀与后缀拼回，避免题目级与选项级同名的 `id` 互相污染）；`<recommended>` 容 `true|yes|1` / `false|no|0`；缺 `<mode>` 不填（回落既有 `selected_target_mode` → AutoEdit 语义）；**非法 `mode` 丢弃**（否则整条 args 反序列化失败）。
- **口径**：块级问题整条放弃；**单项残缺逐项丢弃**（缺 `id`/`question` 的题、缺 `id`/`label` 的选项）——应答协议按 id 关联（`answers[qid].selections`），没有 id 的项本来就答不了，丢掉它比整条不兜底更贴近用户预期。
- **私有控制 token 清洗**：删 `<]name[>` 形 token 及紧邻残留，**只作用于解析出的字段值**。实测形态是 `]<]minimax[>[`（`name` 段 ≤32 字符）；注意尾部那个 `<` 是**闭合标签的开头**，不是 token 的一部分，故尾部 `[` / `<` 各自独立可选消费（成对消费会剩 `[`）。

### 2) 接入点：`core/agent/drive.rs`（唯一插入位置）

每步拿到 `Assembled` 之后、组装 assistant 消息**之前**判定，四条门全满足才生效：

| 门 | 判据 | 为什么 |
|---|---|---|
| 主会话 | `params.main_session` | 子代理没有 ask（工具集里没有），弹卡也没有 UI 桶 |
| ask 可用 | `exclude_tools` 不含 `ask` | 目标档执行期「零提问」、角色策略等排除方**一律共用这份名单**，将来新增排除方无需改这里 |
| 本回合无调用 | `assembled.tool_calls.is_empty()` | 已有真实调用时再补一个会变成两张卡 / 两次切档 |
| 每 run 一次 | `text_ask_used` 标志 | 既救「端点偶发失灵」，也不给提示注入留反复重试的窗口 |

命中动作：就地剥掉正文里的块（**找不到就不兜底**——「正文留着协议原文却多出一个调用」比不兜底更糟）→ 追加合成 `AssembledToolCall{ name:"ask", id:"text_ask_<run_id>_<step>" }` + `AsmBlock::Tool` → 写 `rt.text_ask_block`（供前端补剥）→ 记 `session_log::warn`（含块首 200 字样本，便于事后判定「什么内容触发了这张卡」）。

**其后零改动**：`calls` 非空即自然走既有的 `run_tool_batch` → 真实 `AskTool`，所以「完全等价」（含 `mode` 切档与 `switchToAutoEdit` 批准）是结构保证，而不是再实现一遍。

### 3) 前端补剥（为什么不能只剥一次）

后端剥的是**落盘历史**，而这段协议原文早已随流式帧进了**当轮气泡**（帧已下发、无法回收）。且正文经 **64ms 节流**下发（`stream_flush_loop`，run 收尾才最终冲刷），而 `ask:opened` 在流结束后**几毫秒**就到——最后那个窗口里的尾巴（往往正是 `</ask>`）会在 `ask:opened` **之后**才到达。**只剥一次必然漏**（这与 `features/chat/segments.tsx` 的 `stripReportMarkers` 注释同源：剥离放在渲染时，因为增量会把标记切两片）。

做法：`ask:opened` 的可选字段 `text_recovered`（携带被剥离的块原文；**普通 ask 不出现该键**）→ 前端把它记为**当轮状态**（`tab.textRecovered`）→ 在**每帧文本增量落地之后**与 `ask:opened` 当刻各剥一次（幂等：`includes` 守卫 + `replace(..., "") + trimEnd()`，与后端 `strip_block` 同口径）→ `run:done` 清空。事件键名与 30 键不动，只增一个可选载荷字段。

### 4) 可诊断性：step 级响应形态日志

每步记一行 `step N 响应形态 tools_sent=… tool_calls=… names=[…] text≈…字 recovered=…`，其中工具调用口径取**兜底之前**的原始响应（否则补出来的调用会把「端点真的返回了 tool_use」与「我们补的」混在一起）。`cargo test` 之外，这行日志是「为什么模型没返回 tool_use」这类问题的第一现场。

## 四、反向纪律（勿再破）

- **绝不为 `edit` / `command` 等写类工具做文本兜底**：那会把「看不到问题」升级为「任意代码执行」（模型复述一段被读进来的 XML 即触发执行）。只做 `ask`，且只做「提问」这一种语义。
- 不碰 provider 层；不改 `approval_shape` / `is_preview_option` / `preview_option_ids` / `selected_target_mode` / `plan_approval_gate` / `text_turn_action` 决策矩阵；事件面 **30 键与 `ask:opened` 键名不动**（只增可选字段 `text_recovered`）。
- `rt.text_ask_block` 是**当轮**标注：run 起点复位 + `AskTool` 打开卡片时 `take()`，真实 ask 绝不带 `text_recovered`（有 e2e 钉死）。别把它挪进 `SessionPrefs`（prefs 是前端整体替换写的事实源，会被补丁冲掉）。
- 解析器只认**精确标签形态**（`<tag>` / `</tag>`，内层带属性即放弃）；`<ask>` 必须独占起始行——别为了「多救几个案例」把判据放宽，误弹卡（把讲解协议的正文当成提问）比漏救更伤用户。

## 五、已知边界

1. **提示注入面（用户已拍板接受）**：文本形态的 ask **完全等价**于工具调用，即采信其中的 `mode` 切档与 `switchToAutoEdit` 批准语义。因此模型复述被读进来的 `<ask>` XML 也会拉起卡片。四道门（无 tool_use / 仅主会话 / ask 可用 / 每 run 一次）+ 严格判据把它收窄到「独占段落 + 位于末尾」；该模型本就能直接调用 ask，文本通道**不额外增权**，但「复述」比「主动调用」门槛更低——这是明知的取舍。
2. **跨段不剥**：同一轮的块若被 `thinking` 增量切成两个 text segment（前端）或被 thinking 块分隔（后端），两侧都放弃剥离——不会出现「历史切了、气泡没切」的单边不一致，但气泡可能残留半段。前端有测试记录该行为。
3. **气泡尾部残留**：兜底命中时，后端把块从历史里剥掉、前端在**当轮**剥掉；`run:error` / `run:cancelled` 不走 `run:done` 清理路径（残留值只会剥逐字相同的协议原文，无副作用）。
4. **控制 token 只清字段值**：正文（块之外）里混入的 `]<]minimax[>[` 不清理——那是端点/模型侧问题，不该由渲染层猜。
5. **不落盘原始 SSE**：端点侧「为什么不转 `tool_use`」仍只能靠 step 级日志 + 用户自跑实验推断。

## 六、端点判别实验（用户可自跑；需 API Key，agent 不代跑）

```bash
curl -sS -N https://api.minimax.cn/anthropic/v1/messages \
  -H "x-api-key: $KEY" -H "anthropic-version: 2023-06-01" -H "content-type: application/json" \
  -d '{"model":"MiniMax-M3","max_tokens":512,"messages":[{"role":"user","content":"用 ask 工具向我提一个问题"}],"tools":[{"name":"ask","description":"Ask the user","input_schema":{"type":"object","properties":{"questions":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string"}}}},"required":["questions"]}}}]}'
```

- 返回 `"type":"tool_use"` → 端点会转换，问题在模型本身；
- 返回 `"type":"text"` 且内含 `<ask>…</ask>` → 端点不转换（主因在端点）；
- 再把 `base_url` 换成官方 `https://api.minimaxi.com/anthropic`（国际 `https://api.minimax.io/anthropic`）复跑对比。注意仓库记录的官方 CN 域名是 `api.minimaxi.com` / `minimaxi.com`，**没有 `minimax.cn`**。

## 七、验证（本批实测）

| 命令 | 结果 |
|---|---|
| `cd src-tauri && cargo test` | **1252 passed / 0 failed / 3 ignored**（新增：解析器 11 例 + 端到端 7 例） |
| `pnpm --dir ui test` | **1211 passed / 100 文件**（新增 `run.text-ask-fallback.test.ts` 7 例） |
| `pnpm --dir ui build` | ✅ type check + vite build |
| `pnpm --dir ui run lint` | 0 error（仅 `Composer.tsx:240` 存量 warning） |
| `cargo fmt --check` / `cargo clippy` | 干净；本批文件 0 新告警（存量 94 条为软门） |

端到端用例（`core/agent/tests.rs`，fixture 用会话原文含 `]<]minimax[>[`）：命中即拉起卡片且历史与真实调用同形 / 有真实调用不兜底 / 子代理不生效 / `ask` 被排除时不生效 / 围栏内不认 / 每 run 一次 / 畸形放弃 / `text_recovered` 只出现在恢复路径。

## 八、遗留（未做）

1. **`error`/`cancelled` 路径的当轮状态清理**（各 1 行，观感项）。
2. **解析器上限与 ask schema 的双份常量**（`MAX_QUESTIONS`/`MAX_OPTIONS` vs `tools/ask/tool.rs` 的 schema `maxItems`）——建议补一条「读 schema 字符串断言一致」的 pin 测试。
3. **`parse_mode` 与 schema 枚举的等价性**未用测试钉死（当前恰好一致：`ApprovalMode` 五个变体的 serde 名 = schema enum 五项）。
4. **合成 id 形态**依赖 `run_id` 为 uuid（`text_ask_<uuid>_<step>`）；若将来换调用方，需复核 id 字符集与长度约束（`.toolres/<call_id>.json` 作文件名）。
5. **气泡渲染层**：整条正文就是块时会留下一个空 text 段（无视觉影响，未删段）。
