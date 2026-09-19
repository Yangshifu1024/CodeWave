# CodeWave P1 详细技术方案（约 3–4 周）

> 日期：2026-08-30
> 上游：`[docs/technical-design](./technical-design.md).md` §10（P1 定义）、`[docs/p0-plan](./p0-plan.md).md`（P0 完成后启动）
> 前提：P0 全部 DoD 通过。P1 目标 = 补齐日常使用功能面

---

## 1. P1 范围总览与依赖

| 组 | 内容 | 依赖 |
|---|---|---|
| A 工具补全 | web_fetch / http_request / service / wait / suggest / plan / batch_read / calculate / render_html / skill | P0 G5 |
| B 质量增强 | 写入后校验、多 Key 池、OpenAI Responses 协议 | P0 G3/G5 |
| C 扩展生态 | MCP 完整、Skills 完整、记忆 | P0 G4 |
| D 工作区体验 | 多 Tab、WorkspaceExplorer、路径索引、GitDiff | P0 G8/G9 |
| E 基础设施 | model catalog、keyring、i18n/主题完整、审批设置矩阵 | P0 G2 |

组间无强依赖，可按 A→B→C→D→E 或任意顺序推进；建议单周一个组 + 交叉。

---

## 2. A 组：工具补全（10 个）

每个工具延续 P0 惯例：`tools/<name>/algo.rs`（纯函数 + 单测）+ `mod.rs`（编排）。schema 一律 strict（`additionalProperties:false`）。

### 2.1 web_fetch
- 入参：`{url, maxChars?=60000}`。
- 管线：reqwest GET（UA `CodeWave/0.1`，10s 连接 / 30s 总超时，**SSRF 守卫**，重定向逐跳校验并剥离 Authorization/Cookie 头）→ content-type 过滤（仅 text/html、text/plain、application/xhtml）→ ≤50MB → `dom_smoothie::Extractor` 抽正文 → 输出标题 + 正文（≤maxChars，超限截断提示）。
- 限流：`HashMap<host, Instant>` 每 host 最小间隔 1s，命中则等待。
- 测试：本地 fixture HTML（含导航噪声/正文/代码块）断言抽取；SSRF 用例（重定向到私网被拒）。

### 2.2 http_request
- 入参：`{url, method?=GET, headers?, query?, body?, timeoutSeconds?=60}`。
- 响应输出：状态行 + 响应头（脱敏 set-cookie）+ 体预览（JSON 尝试 pretty 截断 24KB；文本截断 96KB；二进制→大小+MIME）。
- 同样过 SSRF 守卫；方法仅允许 GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS。

### 2.3 service（后台长进程）
- 入参 action：`start{name, command, cwd?}` / `stop{id}` / `list` / `read{id, tailBytes?=8192}`。
- `ServiceTable`：`DashMap<ServiceId, ServiceHandle{child, job(win), name, started_at, ring: RingBuffer<512KB>}>`；stdout/stderr 合并后台任务持续写入 ring + emit `service:update`（节流）。
- stop：SIGTERM/软终止 → 10s 宽限 → SIGKILL/Job 强杀（进程树）。
- 与 P0 command 工具同围栏；启动成功返回 `svc_xxx` id。

### 2.4 wait
- `{seconds(1–3600), reason?}`；cancel-aware sleep（select cancellation）；Interactive 类（批次唯一）。

### 2.5 suggest
- `{items:[1–4 strings]}`；执行即把建议写入 `run:done` 载荷，前端渲染 chips；**语义：成功执行suggest 即结束 run**（主循环特判：本步无其他 tool_call 且 suggest 成功 → 收尾）。

### 2.6 plan（todo 状态机）
- 入参：`{todos?: [{title, status: pending|in_progress|completed}]}`；空参 = 读取当前列表。
- 存储：`SessionRuntime.todos`（内存）+ 会话快照持久化；校验：同刻最多一个 in_progress、title 非空、≤100 项。
- 注入：每次 tool_result 返回全量列表；新 user 回合首请求在消息前附加一次当前 plan 快照（瞬态，置于缓存断点之后）。
- 事件 `plan:update` 推前端 Plan 面板。

### 2.7 batch_read
- 仅作 `read` 的别名注册（P0 read 已支持批量），保持 schema 层面兼容；文档标注 deprecated。

### 2.8 calculate
- 自写递归下降求值器（`tools/calculate/algo.rs`）：文法 = expr(add/sub) → term(mul/div/mod) → factor(pow/^ 右结合) → unary(−) → atom(number | const(pi,e) | func(...))；函数集 sqrt/cbrt/ln/log/log2/log10/exp/abs/floor/ceil/round/sin/cos/tan/asin/acos/atan/atan2/min/max；f64，除零/溢出/NaN → 干净报错。**绝不 eval**。
- 500 用例表（含边界：`2^10^2` 右结合、精度显示 12 位有效数字）。

### 2.9 render_html
- `{title?, html(≤50k chars)}`；后端仅校验长度并入 `tool:result`；前端 `HtmlRenderCard` 用 `<iframe sandbox="allow-scripts" srcdoc>`（无 allow-same-origin，CSP 已禁外域网络），高度自适应上限 480px。
- 安全备注：P0 CSP `frame-src 'self' blob:` 需扩展允许 srcdoc（`frame-src srcdoc` 或 blob 包装），实现时验证。

### 2.10 skill
- `{skill: name, args?}`；从 Skills 索引（§5）加载对应 SKILL.md 全文 → 以 `<skill-loaded name="…">…</skill-loaded>` 文本回填模型；未知名返回可用列表。
- 斜杠通道：`/name` 或 `/skill:name` 在 Composer 命令菜单触发同一路径。

**DoD（A 组）**：10 工具全部端到端可用；suggest 正确收尾 run；plan 面板实时更新；calculate 500 用例全绿；render_html 沙箱逃逸自测（尝试 fetch 外域失败）。

---

## 3. B 组：质量增强

### 3.1 写入后校验（tools/validation.rs）
- 触发：edit/create 成功后按扩展名路由：

| 语言 | 检查 | 超时 |
|---|---|---|
| python | `python -m py_compile <file>` | 10s |
| rust | `rustc --edition 2021 --emit=metadata --crate-type lib <file>`（独立文件级，workspace 编译太重） | 30s |
| ts/tsx/js/jsx | `npx tsc --noEmit --skipLibCheck <file>`（存在 tsconfig 时用项目配置） | 30s |
| vue | 抽 `<script>` 块按 ts 检查（简化） | 20s |
| go | 仅当位于 go module 内：`go vet <file>` | 30s |
| json | serde_json 解析 | 1s |

- 失败输出压缩为 ≤2KB 回填："文件已写入；语法校验失败：…请修复"（**历史文案**：当时实现回填给模型的原话；用户可见的现行定名是「写入后语义校验」，见 [settings-terminology](./settings-terminology.md) §1）；工具结果 warnings 标注 `validation=failed`。
- Settings 每语言开关（P0 已预留 ValidationSettings）。

### 3.2 多 Key 池与故障转移（provider/keys.rs）
```rust
struct KeyPool { keys: Vec<String>, health: Vec<KeyHealth> }   // 有序
struct KeyHealth { cool_until: Instant, last_error: Option<String>, consecutive: u32 }
impl KeyPool {
    fn pick(&self) -> usize {  // 第一个未冷却；全冷却 → cool_until 最小者
    }
    fn report(&mut self, idx: usize, err: Option<&ProviderError>) {
        // Auth(401/403/quota) → cool 30min；RateLimited/Server/Network → cool 10s 且 consecutive++ 指数退避(10s,20s,40s…上限30min)
    }
}
```
- 主循环重试器对接：Auth/RateLimited 时先 `report` 再 `pick` 下一个 key 重试（同一退避预算内）；全部冷却 → 上抛并提示。
- 测试：3-key mock（key0 一直 401、key1 429 两次后成功、key2 健康）→ 断言最终成功且调用序列符合冷却策略。

### 3.3 OpenAI Responses 适配（provider/openai_responses.rs）
- `POST {base}/v1/responses`，`stream:true`；input 结构：`[{role, content:[{type:"input_text"|"output_text"}]}, {type:"function_call"|"function_call_output"}]`。
- 事件映射（SSE）：`response.output_text.delta`→Text；`response.reasoning_summary_text.delta`→Reasoning；`response.output_item.added(function_call)`→ToolCallBegin；`response.function_call_arguments.delta`→ArgsDelta；`response.completed`→usage(input/output/cache)。
- 缓存路由：每会话稳定生成 `prompt_cache_key`（session uuid）随请求发送；工具列表排序不变。
- 模型配置 `api_format` 增加 `"openai_responses"`；设置表单下拉同步。

**DoD（B 组）**：改一个 TS 文件引入语法错误 → Agent 收到校验提示并自修复；多 key 故障切换 mock 全绿；真实 Responses 端点完成一次带工具的会话。

---

## 4. C 组：MCP 完整（mcp/）

### 4.1 配置
```
~/.codewave/mcp.json      {"mcpServers":{"name":{"transport":"stdio","command":"…","args":[…],"env":{…}}
                                          | {"transport":"sse"|"streamable-http","url":"…"}}}
工作区 .codewave/mcp.json 同 schema，同名时项目覆盖用户级
```
- UI：Settings 增加 `mcp` 分区——表单编辑器（名称/类型/command|url/args 表格/env 表格）+ 原始 JSON 切换；变更后热重载（先停旧再起新）。

### 4.2 生命周期（mcp/manager.rs）
- rmcp：`().serve(transport).await` — stdio 用 `TokioChildProcess`（Windows：CREATE_NO_WINDOW + Job Object 挂管防孤儿）；sse / streamable-http 用 rmcp 对应 transport，经代理感知 reqwest。
- 超时：initialize 与 list_tools 各 30s；失败 server 标记 `error` 不阻塞其余。
- 状态机：`starting → ready → error(reason) → reconnecting…`；任何调用遇 invalid session → 自动 reconnect 一次（30s 超时）→ 仍失败上报。事件 `mcp:status{name,state}`。
- 工具注入：`mcp__{server}__{tool}`；描述前缀 `[server]`；schema 原样（归一化：补 required 数组、递归封 additionalProperties:false）；**全部工具（内置+MCP）按名称字典序**合并（cache-first）。
- Plan mode 不适用（P0 无 plan mode）；审批弹窗不拦 MCP 调用（由 server 自身能力决定，提示词声明风险）。

### 4.3 DoD
- [ ] 接入 2 个真实 stdio server（如 filesystem、fetch 类开源 server）+ 1 个 HTTP server：工具出现、调用往返、断连自动重连
- [ ] 工具列表字节稳定性回归测试通过（server 顺序无关）

---

## 5. C 组：Skills 完整（skills/）与记忆（memory/）

### 5.1 Skills
- 目录扫描优先级：项目 `.codewave/skills/` > 用户 `~/.codewave/skills/` > 用户 `~/.claude/skills/`（生态兼容，只读）> 内置（`include_str!`）。
- `SKILL.md` frontmatter：`name / description / whenToUse（兼容 when_to_use）/ disabled?`；解析失败跳过并警告。
- 运行时：`SkillIndex{name→{path, meta}}`（启动构建 + 目录 watcher 失效重扫）；系统提示词第 2 层仅注入三字段列表；`disabled_skills: Vec<String>` 存 config（toggle_skill 命令）。
- 内置自研 skill（P1 交付 2 个，全部原创文案）：`repo-index`（引导 Agent 生成/更新项目功能→文件索引文档）、`doc-convert`（常见文档格式转换的操作指引，配合命令工具实现）。

### 5.2 记忆
- `~/.codewave/memories/*.md`，frontmatter `name/description`；数据目录在 write_roots 白名单 → 模型直接 create/edit 维护。
- `MemoryIndex`：启动扫描（≤200 条）注入系统提示词第 3 层；文件变更 watcher 增量更新；`/remember <内容>` 命令 → 帮助性引导模型写一条记忆（预填 Composer）。

**DoD**：安装一个第三方 skill 目录 → 出现在 `/skills` 列表并可通过斜杠与 skill 工具双通道加载；记忆跨项目会话生效。

---

## 6. D 组：工作区体验

### 6.1 多 Tab 工作区
- `Tab{sessionId, workspace, title}`；新建 Tab = 新会话 + 选目录（dialog 插件）；每 Tab 独立 run 状态（`run.ts` 改为 `Record<tabId, RunState>`）。
- ChatMessages 用 `v-show` 常驻保滚动位置；快捷键（P0 已挂）补 `Cmd/Ctrl+P` Tab 切换面板。

### 6.2 WorkspaceExplorer
- 文件树：懒加载子目录（`list_workspace_dir` 新 command，ignore walk 单层）；虚拟滚动（naive-ui Tree 或自研）；双击 → Ace 编辑器 Tab 打开（内存脏标记）。
- 保存：`save_workspace_file`（写前 pathutil 校验 + 原子写）；外部变更检测（notify watcher，简化：切换窗口聚焦时 mtime 比对提示重载）。
- 编辑器 Ace：暗色主题、语言自动按扩展名。

### 6.3 路径索引（@文件提及）
- `search_workspace_paths(query, limit=30)`：ignore walk 全量路径（≤50k 条）缓存（10min TTL，.git 事件失效）→ 大小写不敏感子序列匹配（fuzzy 打分：连续命中加分、文件名命中加分）→ Top N。
- 大仓库构建 >8s → 降级为按需 walk（首次查询即时构建部分索引并后台补全）。

### 6.4 GitDiff（git/）
- `git_diff(path?)`：`diff_tree_to_workdir_with_index` → 每文件 patch（上下文 3 行）→ `{path, status, additions, deletions, hunks[]}`；上限 100 文件/2000 hunk。
- `git_recent_log(n=20)`：revwalk → `{short_id, summary, author, time}`。
- UI：`GitDiffModal`（工作区变更总览）+ AppHeader git 徽标（变更数）；审批弹窗中 git push 类确认附带当前 diff 摘要。

**DoD**：双 Tab 双工作区并行对话互不串扰；@提及 <50ms 响应（预热后）；GitDiffModal 与真实仓库变更一致。

---

## 7. E 组：基础设施

### 7.1 model catalog
- `scripts/generate-model-catalog.mjs`（Node 22）：输入 `docs/model_api.json`（models.dev 手动快照，脚本头注明再生成命令与日期）→ 过滤（有 api 格式映射价值的 provider）→ `frontend/src/data/modelCatalog.json`：`[{provider, models:[{id, context_window, max_tokens_out?}]}]`。
- 设置表单 models 分区加"从目录填充"（级联选择 provider→model，自动回填 contextWindow）。

### 7.2 keyring 迁移
- keyring crate；服务 `codewave.yangshifu.xyz`，账户 = model.id。
- 启动迁移：`config.models[].keys` 非空且 keyring 对应账户空 → 逐条写入 → config 中该模型 keys 置 `["__keyring__"]` 占位。
- `resolve_keys(model)`：占位时从 keyring 读取；keyring 错误 → 回退提示用户手填（config 直填仍受支持，设置页提供"仅存本文件"勾选项）。
- 平台：macOS Keychain / Windows 凭据管理器 / Linux Secret Service（不可用则明确提示并回退）。

### 7.3 i18n / 主题 / 审批矩阵
- i18n：en-US 词条补全（CI 加 lint：词典 key 集合一致性校验）；模型输出不翻译。
- 主题：暗色基座 + 7 套强调色（amber/cool/emerald/violet/rose/cyan/slate），CSS `color-mix` 派生 hover/active 阶梯；`ui.accent` 持久化。
- 审批设置矩阵（Settings→security 分区）：

| 设置项 | 默认 | 说明 |
|---|---|---|
| approval.enabled | on | 总开关（off 时 L3 类直接拦截） |
| approval.confirm_outside_create | on | 区外新建路径确认 |
| approval.confirm_git_push | on | push/force 确认（git 写操作出现后生效） |
| validation.* | py/on rust/on ts/on go/on json/on | 校验开关 |

**DoD（E 组）**：换机后 keyring 迁移一次成功；en/zh 词条 lint 绿；7 主题切换即时生效。

---

## 8. P1 验收（Definition of Done）

1. **场景 D（扩展）**：配置一个 stdio MCP server → Agent 调用其工具完成任务；安装第三方 skill → `/skillname` 加载生效。
2. **场景 E（网络）**：让 Agent"抓取某文档页并总结"→ web_fetch 正文干净；"调用一个 REST API"→ http_request 返回结构化预览。
3. **场景 F（韧性）**：配 3 个 key（一个失效）→ 对话中首个失效自动切换无感。
4. **场景 G（工作区）**：双 Tab 双工作区并行任务；@文件、GitDiff、编辑器保存全流程可用。
5. 质量门槛：新增模块单测覆盖率 ≥75%；cache-first 回归（同会话连续 3 次请求前缀 byte-equal 测试）绿；CI 三平台绿。
