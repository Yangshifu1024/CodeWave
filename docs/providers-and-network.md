# 供应商、协议与网络维护指南

核对日期：2026-10-12。本文以当前源码为准，集中说明供应商与模型配置、凭据、协议修复、缓存、代理和订阅额度。设置页的导航、草稿、弹框与公共视觉契约见 [设置页维护指南](./settings-guide.md)。历史批次的测试数量、分支名和当时验收结果不作为当前基线。

## 1. 配置模型与保存边界

- 配置使用 schema v2：`ConfigState.providers` 包含 `ProviderConfig`，每个供应商持有协议、Base URL、key 池、自定义请求头及 `ProviderModel` 列表。`active_model_id` 指向模型条目的 UUID；发到上游的 `ProviderModel.model` 同时作为显示名，不能与内部 UUID 混用。
- 三种协议为 `openai_chat`、`anthropic_messages`、`openai_responses`。模型定义以用户输入为准，没有 models.dev 内置目录。`vision` 是实际图片输入开关；`video` 目前只是保留标记，不能据此承诺视频发送能力。
- `ModelConfig` 只是运行时摊平形态，由 `ConfigState::find_model` / `flatten` 生成；前端 `flattenModels` / `findModel` 维护相同供应商序和模型序。改字段时同时检查两端摊平、IPC 类型和所有请求构造点。
- 新模型默认 `max_tokens = 32768`、`context_window = 128000`。显式存量值保持原样；默认值变化不会自动提高旧模型的输出上限。
- schema v1 扁平模型与 legacy key 账户迁移已经移除。`load` 把版本字段归一为 2，不代表会迁移旧 `models`；不要将历史迁移流程重新写成当前支持能力。新字段仍须维护 serde 缺省语义。

源码：[config.rs](../src-tauri/src/core/config.rs)（`ProviderConfig`、`ProviderModel`、`ConfigState::flatten/load`）、[types.ts](../ui/src/ipc/types.ts)、[models.ts](../ui/src/utils/models.ts)、[ProvidersPanel.tsx](../ui/src/features/panels/ProvidersPanel.tsx)。

### 表单与校验

供应商列表是卡片。新增和编辑都在单层弹框中暂存；模型编辑替换同一弹框的页体，而非叠加第二个弹框。新增确认或编辑“完成”才并入设置页主草稿，弹框取消丢弃该次编辑；真正落盘仍需设置页顶部保存。

字段顺序为名称 → API 格式 → Base URL → API Key → 自定义请求头 → 模型列表。切换协议仅在 URL 为空或等于已知默认端点时替换 URL，自定义端点保留。当前前端要求：

- 名称 trim 后非空；Base URL 非空且匹配 HTTP(S) 形式，允许 localhost。
- 至少一行新 API Key 或一组已保存凭据；编辑框只接收新输入，已保存池单独显示状态；模型列表至少一个。当前 UI 不允许直接保存无 key 供应商，即使协议层仍能向无鉴权端点发请求。
- 模型编辑提交要求非空 wire id。新增供应商在字段改过或尝试提交后显示错误；编辑实时显示错误。顶部保存对全部供应商重新校验，失败跳到供应商页且不落盘。
- 顶部保存过滤 key 空行、归一头名/头值，并在 active 模型缺失时回退首个模型，无模型时设为 null。后端校验自定义请求头、供应商/模型标识唯一性、凭据意图、引用归属与版本；端点等产品表单校验仍不能全部宣称为后端不变量。

## 2. API Key 与 key 池

keyring service 为 `codewave.yangshifu.xyz`。当前 v2 旧占位配置在 `credential_ref = null` 时继续读取供应商 UUID 账户；新池使用后端生成的 `<provider>:credential:<generation>` 不可变账户。新字段有 serde 缺省，不恢复 schema v1 或按模型账户迁移。主对话、标题、压缩与订阅额度都按同一引用解析；空 keys 立即解除引用，不回读旧账户。

### 编辑语义

供应商弹框把“已保存密钥”与本次新输入分开。`CredentialEdit` 显式区分 `keep`、`append { new_keys }`、`replace { new_keys }`、`clear`；取消弹框不提交，失败保留输入。已有供应商可选追加或整池替换；UI 仍要求有认证凭据，清空可通过删除供应商完成，未开放无认证端点。

- `keep` 不读取/重写已安全存储的池；有当前明文时例外，必须先安全整理。
- `append` 必须读到完整旧池，按旧优先、新输入其次稳定去重，trim 空行而不改变大小写。回读失败阻止整个提交，绝不当作空池。
- `replace` 不要求旧池可读，只保存明确的新池；空替换与 `clear` 都解除引用。
- 当前混合“占位+明文”的安全整理保留既有效的明文优先顺序，与新追加语义分开。
- 内部字符串适配器仅用于当前发布入口兼容；无归属占位符、未知或歧义掩码返回冲突，不能静默丢弃。新表单不编辑掩码或占位符。

### 统一安全提交

[config_commit.rs](../src-tauri/src/core/config_commit.rs) 的 `ConfigCoordinator` 持有提交互斥锁，所有 `config.json` 写者（页面、字体、角色、技能、命令白名单与启动整理）先读最新配置。锁序固定为提交锁 → 短期配置读写锁；调用系统钥匙串和磁盘时不持 cfg 锁，异步入口放在阻塞工作线程。

1. 校验版本、字段所有权、意图和所有需要保留的池。
2. 原子写入非秘密提交意向，再创建新账户，完整写入并读回核验。多供应商任一个失败都不发布配置。
3. 配置专用写入器拒绝真实 API Key，只写占位与引用；用临时文件替换，不复制旧文件为 `.bak`，包括首次整理旧明文。文件替换是提交点，失败时旧文件仍读取旧池。
4. 发布相同内存配置，递增全局与受影响域版本；只按实际变化更新日志、HTTP client 或 KeyPool。回执包含权威脱敏快照、版本、清理结果和警告。store 只接收该快照，绝不把输入当作已保存配置。
5. 已提交后清理/回执更新失败作为“已保存、后处理待核对”警告；操作 UUID 和配置内 `last_operation_id` 支持丢失响应后的 `get_config_operation` 对账，已知操作不重复清理。未知或过期回执要求重新读取核对，不盲重放。

钥匙串不可用时没有明文降级。当前文件已有明文则启动走同一协议，失败保留可恢复原件并显示存储警告；此后字体等旁路也必须先安全整理才能写配置。该保证不擦除历史备份、交换区、旧外部副本、自定义请求头或代理密码，见 [SECURITY.md](../SECURITY.md)。

恢复记录不含秘密，只登记应用生成的账户、操作、阶段与回执。运行时压缩历史无账户回执，保留最近 128 条及所有携带账户的记录；最多 512 条，待回收账户占满队列时明确阻止新写入并要求重启清理。启动清理后继续保留当前引用/删除失败账户的记录。仅启动时回收确认不被当前配置引用的生成账户；配置不可读或格式未知时不删除，并锁定本进程所有配置写入；不能用容错加载的默认值覆盖原文件。修复/还原文件后须重启重新建立权威配置，字体缓存迁回也不例外。过期操作 UUID 的历史以固定 64 KiB 位图保存（序列化文件可能略大）；它只散列非秘密 UUID，永不自动清除/轮换。过滤器可能保守误判新 UUID，此时返回 `OperationExpired`，要求重读核对后明确开始新操作；旧字体等无域版本操作不能绕过过期检查重放。位图达到 50% 占用时返回 `OperationHistoryFull` 并阻止配置写入，新 UUID 或重启也不会解除；必须保留恢复记录，等待提供显式历史恢复协议的版本，不可手工清空记录。该有界、拒绝优先策略替代新增服务端操作票据 API，宁可少量安全拒绝也不丢弃过期操作的防重放证据。

当前 v2 旧 UUID 账户在被替换/解除引用时先登记退役记录，也只在下一次启动核对后清理；非标准历史账户名保守保留，不通过枚举删除不明账户。回收失败可重试并显示警告。

KeyPool 以供应商 id 分组，同供应商模型共享健康状态：优先首个未冷却 key；全池冷却则选择最早恢复者。认证错误冷却 30 分钟；其它上报失败按 10 秒指数增加、封顶 30 分钟；成功重置该 key。供应商配置实际变化才 `reset_all`，角色/字体改动不重建连接、不触发会话清理。认证错误本次 run 不自动换 key 重试；冷却影响后续选择。

### 凭据边界与回归

- 删除供应商/清空池只撤销新配置解析，已捕获旧 `ModelConfig` 的在途轮次或重试仍可用旧账户直至结束；不承诺即时撤销。
- `resolve_provider_keys` 回读失败返回错误，供额度显示 `invalid`；模型 `resolve_keys` 仍以空 key 的请求错误链展示故障，二者错误语义不要混用。
- 成功写入版本化引用后不能直接退回不理解 `credential_ref` 的旧二进制；需修补版/前向修复，不双写旧账户换取兼容。
- 回归入口：`core::config_commit::tests` 的秘密字节扫描、文件/读回/多供应商失败、启动整理、崩溃阶段重建、并发追加、字段域冲突和旧快照测试；`providers.panel.test.tsx`、`settings.cleanup.test.ts`、`SubagentModelsPanel.test.tsx` 及设置整页测试覆盖表单与响应顺序。真实钥匙串三平台及 Windows 配置替换/断电语义仍需人工验收。通用文件写入器的 Windows 独占锁回归已通过，但它不等于配置专用替换器或真实钥匙串已验收；范围见[本次自动验证记录](./testing.md#可靠性修复自动验证记录2026-10-12)。

源码：[host/keyring.rs](../src-tauri/src/host/keyring.rs)、[host/commands/project.rs](../src-tauri/src/host/commands/project.rs)、[config_commit.rs](../src-tauri/src/core/config_commit.rs)、[provider/keys.rs](../src-tauri/src/provider/keys.rs)。

## 3. 供应商级自定义请求头

所有三协议 LLM 请求在协议头和鉴权头之后调用 `apply_request_headers`：先设置默认 `User-Agent: CodeWave/<版本> (+desktop agent)`，再应用用户头；同名后者覆盖，所以自定义 User-Agent 可覆盖默认值。`${session_id}` 在请求时替换为 runtime id，无会话时为空串；主对话、标题、压缩都有接线，子代理使用自己的 runtime id，不自动归并到根会话。

保留名大小写不敏感：`content-type`、`authorization`、`x-api-key`、`anthropic-version`、`host`、`content-length`。这些字段不能由自定义头覆盖。前端和 `save_config` 校验合法 HTTP token、非保留名、无重复头名、值无换行且仅含可见 ASCII；整行空白占位跳过。请求层仍防御性忽略非法名/值并记警告，避免破坏整条请求。

`StreamRequest::redacted_json` 隐去 key 和全部自定义头的值，仅留头名；但头值在配置与设置页仍是明文。LLM 共享 client 当前允许最多 10 跳重定向，自定义头不能假定会在跨源重定向时像标准鉴权头一样被剥离。日志脱敏不等于磁盘加密或重定向隔离。

通用示例：需要按会话路由的网关可配置 `x-opencode-session` 为 `${session_id}`，通常无须覆盖默认 UA。协议与端点仍按所用模型的服务要求配置，不硬编码某家供应商特例。

源码：[headers.rs](../src-tauri/src/provider/headers.rs)、[dto.rs](../src-tauri/src/provider/dto.rs)（`redacted_json`）、[util/mod.rs](../src-tauri/src/util/mod.rs)（`USER_AGENT`）、三协议适配器。

## 4. 错误、重试与协议完整性

### 错误分类和修复入口

`ProviderError::from_status` 优先提取非空 `error.message`，否则使用最多 500 字符的响应片段；常见错误分支附 HTTP 状态，空体明确写“服务未返回原因”。401/403 为 `auth`，402 为 `billing`，408/429 为 `rate_limited`，400/413/422 为 `bad_request`，5xx 为 `server`，其它 HTTP 状态走 `protocol`。

`run:error` 保留既有字段并携带 `kind`。前端只为 auth/billing 错误卡追加引导与“打开模型设置”，不自动弹出设置；该入口打开供应商页，仍不是供应商行级深链。普通网络错误没有这个按钮。无参 `showSettings()` 现在保持当前页，不再重置到旧“通用”页。

Auth、Billing、Cancelled 不进入通用重试；网络、限流、服务端和协议错误最多重试 6 次，退避封顶 10 秒。BadRequest 另有每 run 一次 sanitize/repair 修复重试，不占通用重试计数。流内错误的分类由各适配器解析器决定，不应把 HTTP 状态分类套到 HTTP 200 的 error 帧上。

源码：[dto.rs](../src-tauri/src/provider/dto.rs)、[retry.rs](../src-tauri/src/provider/retry.rs)、[drive.rs](../src-tauri/src/core/agent/drive.rs)（`run_llm_turn`）、[runHandlers.ts](../ui/src/stores/runHandlers.ts)、[ChatMessages.tsx](../ui/src/features/chat/ChatMessages.tsx)。

### 输出上限与截断

`max_tokens` 控制一次回复的输出预算，Anthropic 与通常的 OpenAI Chat 请求以同名字段发送，Chat 对名称以 o1/o3/o4/gpt-5 开头的模型改用 `max_completion_tokens`，Responses 使用 `max_output_tokens`。这属于适配器的当前名称启发式，不是模型能力自动发现。`context_window` 用于上下文占用和压缩阈值，不提高输出预算；虚增窗口只会推迟压缩并低估占比。

Anthropic thinking 与正文共用输出预算，力度按 low/medium/high/max 映射 20%/40%/60%/80%，思考预算夹在 1024 到 `max_tokens - 1`；`max_tokens <= 1025` 不启用 thinking。OpenAI 协议 high/max 均映射为 high。输出不足时应核对端点能力与最大输出值，而不是只改上下文窗口。

三协议明确截断信号统一追加 `MAX_TOKENS_NOTICE`：Anthropic 的 `stop_reason=max_tokens`、Chat 的 `finish_reason=length`、Responses 的 `response.incomplete` 且 reason 为 `max_output_tokens`。其它 incomplete 原因不冒充长度截断。该尾注与 SSE 未正常收尾导致的网络/协议失败是不同情况。

### 空消息和重试请求重建

必须保留三层防御：

1. 历史 `repair` 删除孤儿 ToolResult、紧随悬空 ToolUse 所在 assistant 补 `[interrupted]` 结果，并仅删除 content 全空的 assistant 消息；不能顺手删空 Tool 消息。
2. `messages_for_request` clone 历史后在出网副本上 repair，正常路径不改写 `rt.history`；连续构建的修复结果应幂等、字节稳定。计划瞬态快照只进请求副本。
3. Chat wire 转换在正文空且无 tool_calls 时跳过 assistant，包括只有思考的消息；另两协议也有空消息防御测试。组装出的空 assistant 不入历史，空响应按已有重试/终止路径处理。

BadRequest 分支修了 `rt.history` 后，**必须**调用 `refresh_request_messages` 更新 `req.messages`，不能继续重发旧快照。重建保留原请求尾部瞬态消息；不重算 `cache_gen_index`，至多影响本次缓存断点，不能影响正确性。

不可解析调用不伪造有效 ToolUse。全部调用被拒时注入 `<tool-args-rejected>` 用户消息并有界续跑，不能因已有正文就静默成功。**混合批次仍有缺口**：坏调用的合成 ToolResult 没有对应 ToolUse，会被出网 repair 当孤儿删除；当前只有日志取证，模型看不到该项拒绝原因。旧现场悬空 `tool_call_id` 的确切来源也未被证实，不可将“可能与围栏/审批相关”写成已定位根因。

源码：[stream.rs](../src-tauri/src/core/agent/stream.rs)（`messages_for_request`、`refresh_request_messages`、`build_assistant_message`）、[repair.rs](../src-tauri/src/core/sessions/repair.rs)、[drive.rs](../src-tauri/src/core/agent/drive.rs)（`text_turn_action`、`run_tool_batch`）。

## 5. reasoning_content 回传

OpenAI Chat 历史 assistant 有非空 Thinking 时按到达顺序以两个换行拼成 `reasoning_content`；没有思考时不写该键，也不发空串/null。正文与思考的原始交错顺序无法由独立字段完整表达。仅思考、无正文和工具的 assistant 仍不上 wire。Anthropic 与 Responses 不使用这条回传逻辑。

`sanitize_for_save` 保留图片与思考，历史中实际存在的思考可在重开后继续回传。当前历史存储是分段 JSONL 加图片 blob，不能沿用旧文档的 gzip、落盘 trim 和 8MB 上限叙述；wire 裁剪与落盘保留是不同边界。旧版本已删除的思考无法凭空恢复，要求回传的端点对这类旧会话仍可能报错。

400 文案分类大小写不敏感：含 `reasoning_content` / `thinking mode` 且含 `passed back` 视为 Demanded，复位 `reasoning_rejected` 并用 `sanitize_keep_thinking`；相关但无回传要求则为 Rejected，置位并走 `sanitize`；不相关则不改标记、走通用 sanitize。修复后同样必须重建请求。

粘性标记是会话级内存态，跨 step/run，重启后重新学习；子代理创建时继承父标记，计划任务从 false 起步。日后的请求只在副本剥思考，但**触发 BadRequest 的一次性 sanitize 会修改内存历史**，不能笼统承诺所有降级都不丢思考。文案启发式、已丢失数据和仅一次修复预算也意味着“自动恢复”不是成功保证。

源码：[openai_chat.rs](../src-tauri/src/provider/openai_chat.rs)（`convert_message`）、[drive.rs](../src-tauri/src/core/agent/drive.rs)（`classify_reasoning_400`、`update_reasoning_sticky`）、[runtime.rs](../src-tauri/src/core/agent/runtime.rs)、[repair.rs](../src-tauri/src/core/sessions/repair.rs)。

## 6. Prompt 缓存与用量口径

- `build_stream_request` 首步构建 `system_core` 后写入 `system_frozen`，同 run 复用；新 run 清空后重建。因此运行中修改项目指令、技能/记忆索引等稳定前缀，通常到下一 run 才进入该主块。锁内 clone 必须先落局部变量，不能在持 guard 的 match 分支里再锁自己。
- 工具按名称排序。Anthropic 的断点最多四处：末工具、稳定 system 主块、末条非瞬态消息末块、可选历史代际末块；`system_extra` 单列且不标缓存。内部消息转换未产生 wire 消息时不标代际断点。相邻同角色消息在适配层合并。
- 代际锚点历史不足 16 条禁用，目标取 `min(n-8, 3n/4)`，在有效范围内按约四分之一漂移阈值滞回前移，压缩使锚点无效时重选；run 开始也会清空锚点。断点用于稳定前缀，不是“空闲超过缓存 TTL 后必定命中”的保证；代码未设置 1 小时 TTL。
- Responses 使用会话 id 的 `prompt_cache_key`，并发送 `store:false`，不使用 `previous_response_id` 服务端历史链。Chat 侧依靠前缀稳定满足兼容端点的自动缓存条件。
- Anthropic `message_delta.usage` 是累计值，已给字段覆盖 start 值、未给字段保留，不能累加或用缺省零抹掉。解析 SSE 时仍禁止在流内对 Anthropic parser 调 `finish()`。

用量公式由 [models.ts](../ui/src/utils/models.ts) 的 `cacheSemanticsOfFormat` / `cacheDenominator` 统一：Anthropic 真输入为 input + cache_read + cache_write；OpenAI 的 input 已含 cache_read。命中率为 cache_read 除以对应真输入，不能重复加 OpenAI 缓存。统计按 by_model 归一；查不到协议的已删模型排除在命中率之外，但统计总量仍按 OpenAI 口径兜底，不应声称已删模型的历史总量完全精确。按来源拆分使用 output。

源码：[stream.rs](../src-tauri/src/core/agent/stream.rs)、[anthropic.rs](../src-tauri/src/provider/anthropic.rs)、[openai_responses.rs](../src-tauri/src/provider/openai_responses.rs)、[TokenStatsModal.tsx](../ui/src/features/panels/TokenStatsModal.tsx)。

## 7. 代理决议、范围和热生效

唯一决议入口是 [provider/proxy.rs](../src-tauri/src/provider/proxy.rs)：

| 配置 | 实际行为 |
|---|---|
| `proxy:null` | 不干预 reqwest builder，保留库的系统代理默认行为 |
| `mode:none` | 显式 `no_proxy()`，应用 client 直连 |
| `mode:system` | 探测成功使用显式代理；未探测到则显式直连 |
| `mode:manual` | 使用 trim 后配置值，不回退系统探测；空串直连，非法代理解析失败记录警告后直连 |

不要把 manual 描述为安全意义的 fail-closed：当前无有效代理时可能直连；client 构建失败还会回退 `reqwest::Client::new()`。三卡片默认将 null 显示为系统代理，但不改模式直接保存不保证把 null 固化为显式配置。URL 校验只是 `http://`、`https://`、`socks5://`、`socks5h://` 前缀检查，不是完整地址/可达性验证；manual 空串允许保存。

显式代理固定 bypass `localhost,127.0.0.1,::1`；不要把该保证扩展到未干预 builder 的 null 路径。内网访问仍受独立 `network.allow_private_network` 控制，代理 bypass 不是内网授权。

系统探测：Windows 读取当前用户 Internet Settings 的 ProxyEnable/ProxyServer；macOS 解析 `scutil --proxy`；Linux 依次读取 `https_proxy`、`HTTPS_PROXY`、`all_proxy`、`http_proxy`。Windows 分协议值优先 socks > https > http；没有 PAC 解析和完整系统例外列表，也没有运行时变更监听。

接入面包括三协议 LLM、标题/压缩、额度、`web_fetch`/`http_request`、MCP streamable-http。网络工具用 redirect none 保留逐跳 SSRF 校验；stdio MCP 不走该 HTTP client。保存配置无条件重建共享 client 并重新探测系统代理，运行中已持有旧 clone 的请求不受影响；已有 MCP HTTP 连接须考虑其生命周期，不承诺全部存量连接瞬时切换。

更新检查通过 `resolve_proxy` 把非空代理交给 updater，同一 Update 的下载/安装复用它。**无代理模式仍不能强制 updater 直连**：插件未收到 proxy 时可能继续跟随系统代理，当前 UI 已明确提示。代理 URL 可能含 userinfo，应用自己的 URL 摘要做脱敏，但配置仍是明文。

源码：[project.rs](../src-tauri/src/host/commands/project.rs)（`save_config`、`resolve_proxy`）、[updateCheck.ts](../ui/src/utils/updateCheck.ts)、[SettingsPage.tsx](../ui/src/features/panels/SettingsPage.tsx)、MCP 和网络工具的 client 注入点。

## 8. 订阅额度

额度只读取 CodeWave 的 `config.providers` 与对应 keyring，不读 OpenCode 的环境变量/配置/auth.json。供应商 UUID 是快照 id，`quota_snapshots` 接受当前供应商 UUID 作为排序提示。先判 Base URL 能否匹配适配器，再解析凭据；不支持项不会触发 keyring 回读或网络请求。

适配器为 OpenCode Go、DeepSeek、MiniMax 国际/中国、Kimi、智谱、Z.ai。判定只看 Base URL 主机名（含子域），不看路径、模型名或 API 格式；具体白名单以 `ProviderKind::base_url_hosts` 为准，包括 `kimi.com`。请求发到适配器内置官方额度端点，并非用户 Base URL；自建兼容网关通常归 unsupported，不提供自定义额度端点或手动选适配器。

只取首枚解析后 key，不轮换、不为每把 key 各显示一行。每家请求 10 秒超时，所有可查询家并发，失败互不影响，无额外并发上限/退避。key_source 按实际取用的 key 区分 config/keyring。

| status | 判据与展示 |
|---|---|
| `ok` | 请求成功；摘要选最紧张窗口，可展开额度/重置倒计时 |
| `error` | 网络、超时、5xx、429、解析错误，或曾成功过的 401/403/404；保留原因和重试入口 |
| `invalid` | keyring 回读失败，不等同于未配置 |
| `rejected` | 从未成功过的 401/403/404，提示可能非订阅账号，不把推断说成确定套餐事实 |
| `no_key` | 支持适配器但没有解析出 key，给设置入口 |
| `unsupported` | Base URL 空或不命中白名单，解释原因且不发请求 |

可查询类在前，当前供应商在该组置顶，其余按配置序；unsupported 殿后，超过三家折叠汇总。重名按主机名、再按序号消歧。错误行有历史成功时显示 last_ok_at；rejected 正常无该值，显示从未成功。窗口级 rate-limited/exceeded/blocked/limited 归一为限流提示，未知状态原样显示。

右栏信息页可见时进入刷新一次、每五分钟刷新、允许手动刷新；供应商 id/name/base_url/keys 或当前模型变化触发 500ms 去抖刷新，隐藏即停。设置按钮通过 `showSettingsAt("providers", "providers.<uuid>")` 定位卡片，不直接打开编辑弹框；动态锚点不参与静态搜索。

源码：[quota/mod.rs](../src-tauri/src/core/quota/mod.rs)、[quota/providers](../src-tauri/src/core/quota/providers)、[QuotaSection.tsx](../ui/src/features/quota/QuotaSection.tsx)、[quotaRow.ts](../ui/src/features/quota/quotaRow.ts)。

### quota.json 的一致性和自愈

`~/.codewave/quota.json` 只存 version、provider_id、last_ok_at，不存密钥。它是“曾成功过”的持久事实；批次末 `commit` 在进程内同一互斥锁中重新 load、清孤儿、合并本批成功者、原子写一次。无成功者且无孤儿不写盘；锁内只有同步文件 IO，不跨 await。权限由公共原子写入实现设置，Unix 为 0600。

启动 `heal` 与 commit 共用锁：

- 文件不存在或读不到不创建；数值意义高于当前 schema 的版本跳过自愈，不覆盖/隔离。
- JSON 损坏、顶层非对象、verified 为非数组且非 null 等不可读文件形态隔离为 `quota.json.corrupt`，不立即重建；旧隔离副本可被覆盖，只作证据，不是持久不可信标记。
- 元素级类型错逐条丢弃；空白 provider_id、非法 RFC3339 时间清除；合法偏移和小数秒必须保留。
- version 形态非法或 verified:null 等按保留结果重写到可读结构；不能只以“是否删过记录”决定写盘，必须保证修复成功后的内容能被严格 load 读回。
- 健康文件零写盘、零日志；隔离/写失败只 warn，不阻塞启动，原先回退默认的行为仍可能存在。未来版本跳过与 IO 失败是“自愈后一定可读”描述必须注明的例外。

源码：[quota/state.rs](../src-tauri/src/core/quota/state.rs)、[lib.rs](../src-tauri/src/lib.rs)。不对真实用户 quota.json 做故障注入；测试用临时目录。日志只记错误类别/行列摘要，不回显文件内容。

## 9. 排查与验证

| 现象 | 首查路径 |
|---|---|
| 401/403 | 协议、Base URL、供应商 key/keyring 可读性；检查上游可读原因；保存修正后再重试 |
| 402 | 供应商余额/订阅状态；不是网络重试能解决的错误 |
| 回复带截断尾注 | 实际模型的 max_tokens、端点上限、thinking 占额；不要只提高 context_window |
| BadRequest 重试与首次 body 相同 | `refresh_request_messages` 是否仍在 sanitize/repair 之后；请求是否拿到新历史 |
| 多轮 thinking 400 | Chat 历史 Thinking 是否存在、分类是 Demanded 还是 Rejected、旧历史是否已丢数据 |
| 开代理仍有直连 | null/none/manual 空串/非法回退、固定 loopback 例外、updater 限制、存量连接持旧 client |
| 额度显示被拒而非错误 | 首枚 key、适配器匹配、官方端点响应，以及 quota.json 的 last_ok_at 是否仍有效 |
| 代理模式名 DOM 有字但肉眼不可见 | Radio.Group 的 font-size:0 继承；须保留 `.proxy-mode-list { font-size: var(--ws-font-size) }` 与 theme bridge |

自动化保护入口：config 与 keyring 测试；headers 单测和 [provider/tests_integration.rs](../src-tauri/src/provider/tests_integration.rs) 的三协议 on-wire 头测试；dto/retry/keys/proxy 单测；三协议截断与 thinking wire 测试；stream/repair/drive 的幂等、空消息、重建、400 分类与缓存冻结测试；quota tests/state 自愈并发测试；[providers.panel.test.tsx](../ui/src/__tests__/providers.panel.test.tsx)、[settings.network.test.tsx](../ui/src/__tests__/settings.network.test.tsx)、[proxyCard.style.test.ts](../ui/src/__tests__/proxyCard.style.test.ts) 及额度测试。

代码变更按范围执行 `cargo test`（src-tauri）与 `pnpm --dir ui test` / `pnpm --dir ui run lint` / `pnpm --dir ui build`；开 PR 前执行仓库完整门禁。本文只记录源码核验，不把旧批次全绿计数当作本次执行结果。GUI 按仓库约定由用户手工验证：

1. 供应商/模型新增、编辑完成与取消、删除 active 后回退；重开核对掩码与请求；在隔离测试环境验证 keyring 成功与失败，勿展示真实凭据。
2. 错 key 和余额错误出现引导，普通网络错误不抢焦点；修正保存后可再发请求。
3. 三协议小输出预算触发截断提示；Chat 多步、多轮和重开后的 thinking；拒收/要求回传端点按实际可用条件验证。
4. 三种代理模式热保存、HTTP/SOCKS、loopback、远程 MCP、更新请求，并单独核对 updater 的 none 例外。
5. 额度六态、首 key 来源、同名消歧、隐藏停止刷新、配置后重拉、卡片深链，以及临时数据目录下的损坏隔离和合法时间保留。

## 10. 开放事项与非保证

尚未闭环：凭据先落盘与迁移失败明文；占位+新 key 的迁移合并；无 key 本地端点的表单支持；认证错误供应商行深链；流内错误细分类；混合批次坏调用反馈和旧悬空调用现场根因；thinking 文案启发式与真实上游覆盖；Responses 显式 `event: message` 包装可能不按 data.type 分派；独立 thinking 预算；updater 强制直连；代理凭据 keyring、PAC/完整 bypass/系统变更监听和保存前连通性检测。

当前不提供服务端 Responses 历史链、1 小时 Anthropic 缓存设置、自定义额度端点、额度历史趋势/告警或额度请求并发退避。缓存命中受上游策略和有效期影响，源码断点不构成命中率承诺。
