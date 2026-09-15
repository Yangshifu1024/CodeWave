# 供应商级自定义请求头

> 日期：2026-09-15 · 分支：`feat/provider-custom-headers` · 状态：已实施
>
> 背景：OpenCode Go 文档 [「可以在哪里使用？」](https://opencode.ai/docs/zh-cn/go/#可以在哪里使用) 要求非 OpenCode 的编程 Agent 客户端
> ① 发送典型编程 Agent 流量；② 使用自身专属 UA 标识（如 `my-coding-agent/1.0`）而非通用 HTTP 库名；
> ③ 每段对话在 `x-opencode-session` 请求头中发送稳定会话 ID（用于路由与提示词缓存优化）。
>
> 落地方式：不做 opencode 专用硬编码，而是提供**供应商级通用自定义请求头**能力，上述要求由用户在该供应商上自行配置达成。

## 需求

- 每个供应商可配置任意 HTTP 请求头，随该供应商的所有 LLM 请求发送。
- 头值支持动态占位符 `${session_id}`（满足 `x-opencode-session` 的「每会话稳定」语义——静态值无法表达）。
- 默认发送 `User-Agent: CodeWave/<版本> (+local-first desktop agent)`（`src-tauri/src/util/mod.rs::USER_AGENT`），
  且允许被自定义同名头覆盖（满足「专属 UA」）。
- 明文存储与回显（与 `base_url` 同级；用户自行承担放入 token 的风险，文档明示）。

## 数据模型

- `ProviderConfig` 增 `headers: Vec<HeaderPair>`；`HeaderPair { name, value }`（`core/config.rs`）。
  结构体级 `#[serde(default)]` 保证旧 `config.json` 缺字段透明取空（前向兼容，无需迁移代码）。
- `ModelConfig` 增 `headers`（`#[serde(default, skip_serializing_if = "Vec::is_empty")]`），
  由 `ConfigState::flatten` 从所属 `ProviderConfig` 摊平——provider 层只拿得到 `ModelConfig`，必须随摊平携带。
- 前端契约 `ui/src/ipc/types.ts` 同步 `HeaderPair` 与 `ProviderConfig.headers`。

## 请求注入（provider 层）

`provider/headers.rs::apply_request_headers(rb, headers, session_id)`，三个协议适配器
（`anthropic.rs` / `openai_chat.rs` / `openai_responses.rs`）在设好 `Content-Type` 与鉴权头之后调用：

1. 先插入默认 `User-Agent`；
2. 逐个应用自定义头（`HeaderMap::insert`，同名后者胜 → 可覆盖默认 UA）；
3. 值中的 `${session_id}` 替换为会话 uuid（无会话时替换为空串）。

会话 ID 来源：`StreamRequest.session_id`，在三个构造点填 `Some(rt.id.clone())`
（主对话 `core/agent/stream.rs`、自动命名 `core/title.rs`、上下文压缩 `core/context.rs`）。

### 保留名与容错

- **保留名**（`core::config::RESERVED_REQUEST_HEADERS`，大小写不敏感）：
  `content-type`、`authorization`、`x-api-key`、`anthropic-version`、`host`、`content-length`。
  应用时命中即忽略并 `warn`——`rb.headers()` 是覆盖语义（`replace_headers`），跳过保留名是为了**不覆盖适配器已设的协议/鉴权头**
  （Content-Type / Authorization / x-api-key / anthropic-version），而非防重复头（同名 insert 本就不会产生重复）。
  `user-agent` 不在保留名内（允许覆盖）。
- 非法头名/值（reqwest 解析失败）跳过并 `warn`，不使整个请求失败。
- `${session_id}` 无会话时替换为空串，避免把字面占位符发到线上。

## 安全

- `StreamRequest::redacted_json()`（会话 verbose 日志）在遮蔽 key 之外，把**所有自定义头的值置 `***`**（仅保留头名）——
  头值可能被用户填入 token，与 key 同等对待，绝不落明文。
- 头值**明文**存于 `config.json` 并在设置页回显（决策：与 `base_url` 一致，避免像 key 那样掩码带来的编辑复杂度）。
  文档与 UI 提示均明示「请勿放长期密钥」。
- **重定向**：reqwest 跨源重定向只自动剥离 `Authorization` / `Cookie` / `Proxy-Authorization`（仅 path 变化不剥离、
  同源不剥离），**自定义头会随重定向继续发送**；因此勿在自定义头中放长期密钥。本期不改 LLM 请求的 redirect policy
  （部分网关 `base_url` 依赖重定向）；若后续要彻底避免，可对 LLM 请求收紧为不跟随重定向。

## 校验

- 前端 `validateProvider`（`ProvidersPanel.tsx`）：头名非空且为合法 HTTP token、非保留名、不重名；头值无 CR/LF
  且**仅含可见 ASCII**（`0x20..=0x7E`）；整行全空（刚点「添加」未填）视为待填占位跳过。新增表单提交时亮红字；
  编辑表单实时红字；主弹框保存守卫聚合报错。
- 后端 `save_config`（`host/commands/project.rs`）：落盘前对每个供应商调 `core::config::validate_request_headers`，
  失败即返回错误（IPC 校验 + 转调契约）。头值规则与前端一致——非 ASCII / 控制字符值会被
  `reqwest::header::HeaderValue::from_str` 拒绝并静默丢弃，故落盘即拦。

## UI

`ProvidersPanel.tsx` 的 `ProviderFields` 增「自定义请求头」区块：name/value 两列 `Input` + 删除 + 「添加请求头」按钮，
新增与编辑共用；i18n 条目 `customHeaders` / `customHeadersHint` / `addHeader` / `vHeaders`（`zh-CN.ts` / `en-US.ts`）。

## 测试

- `core/config.rs`：`flatten` 透传 headers；缺字段旧 JSON 反序列化为空；`validate_request_headers` 规则矩阵。
- `provider/headers.rs` 单测：默认 UA、UA 覆盖、`${session_id}` 替换（含无会话空串）、保留名不覆盖、非法名跳过。
- `provider/tests_integration.rs`：mock server 捕获真实请求头，断言自定义头与 `x-opencode-session` 上线、
  鉴权头未被破坏、UA 被覆盖；**三个协议（openai_chat / anthropic_messages / openai_responses）均有 on-wire 用例**
  （`custom_headers_sent_on_wire` / `_anthropic` / `_responses`）。
- `dto.rs`：`redacted_json` 遮蔽 header 值。
- `ui/__tests__/providers.panel.test.tsx`：既有头回显、添加行入 draft、保留名实时红字、改合法后校验通过。

## 配置 OpenCode Go 示例

供应商 `base_url = https://opencode.ai/zen/go/v1`（协议按模型选 `openai_chat` / `anthropic_messages` / `openai_responses`），
自定义请求头填：

| 头名 | 头值 |
|---|---|
| `User-Agent` | `CodeWave/0.3.4` |
| `x-opencode-session` | `${session_id}` |

未填 `User-Agent` 时仍会发送默认的 `CodeWave/<版本>`。

## 决策记录

- **通用而非 opencode 专用**：需求方明确要求做成 per-provider 通用能力。
- **占位符支持 `${session_id}`**：静态头无法满足 Go 的「每对话稳定会话 ID」，故引入最小占位符集。
- **明文存储**：与 `base_url` 一致的可见可编辑体验；密钥风险由文档提示 + 日志脱敏兜底。
- **默认 UA**：复用 `util::USER_AGENT`（原 `web_fetch` 内联值提取为共享常量），全应用出网标识统一。
- **子代理请求沿用自身 runtime id**（`rt.id` = `sub_xxx`，非主会话 id）：与 opencode child session 各自持有
  session id 的语义一致，且避免子代理流污染主对话在网关侧的缓存路由。若确认 OpenCode Go 需要按「用户对话」聚合，
  只需把三处 `session_id` 来源改为 `rt.root_session_id.clone().unwrap_or_else(|| rt.id.clone())`
  （一行级改动，**本期不做**；`rt.root_session_id` 已存在：`core/agent/runtime.rs:146`）。

## 审查

- @oracle 独立审查结论：无 🔴 阻断；🟡 三项处置如上——子代理 session id 语义 → 决策记录；重定向剥离说明 → 安全节；
  三协议上线测试 → 测试节补齐。其余为 nit（前端值 ASCII 校验已补，头行 key 与 mock 单次 read 等已处置或明确不做）。
