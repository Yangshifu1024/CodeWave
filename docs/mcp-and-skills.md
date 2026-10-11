# MCP 与技能

> 当前行为指南；源码核对：2026-10-11。MCP 提供外部工具，skill 提供按需加载的操作知识；两者都不能替代宿主权限检查。模式与批准见[计划与审批](./plan-and-approval.md)，命令/网络边界见[工具与安全](./tools-and-safety.md)。

## 1. MCP 配置、作用域与传输

配置只认全局 `<data_dir>/mcp.json` 和当前项目 `<project_dir>/mcp.json`；project_dir 是主目录下 `.codewave`，不是代码根。项目同名 server 覆盖全局，额外授权目录不产生新配置作用域。配置读取按 server 容错，坏条目单独报错；保存会校验整份目标作用域，出错不能当作成功保存。

[config.rs](../src-tauri/src/mcp/config.rs) 保留未知字段，编辑器不得借“格式化”删掉第三方扩展键。主要字段：

| 字段 | 语义 |
|---|---|
| command / args / env / cwd | stdio 子进程；args 是数组，不能按空格拆分 |
| url / headers | streamable-http 端点与请求头 |
| transport（兼容 type） | 显式合法值优先；旧 `sse` 不支持，须明确报错 |
| enabled | 是否连接/暴露工具；false 仍参与作用域覆盖并可编辑 |
| timeout_ms | 调用超时，默认 120,000 ms |
| read_only | 用户对整个 server 的只读信任声明，不是经验证的工具副作用证明 |
| always_allow | 该 server 免逐次审批；保存到原作用域 |
| tools | allow/deny glob 过滤；过滤后的集合才进入模型工具定义 |

未写 transport 时，仅 command 推断 stdio，仅 url 推断 streamable-http；两者同时存在或都没有时拒绝猜测。前端 args/env/headers 的值编辑必须保留换行，不能改回会吞换行的单行 Input；未知字段与显式传输选择必须 round-trip 保留。

server 名允许较宽字符；模型函数名归一为合法字符，碰撞加 `__2` 等后缀并提示。工具名注册时建立反向映射，调用按索引还原 server/tool，**不能靠拆分 `__` 猜原名**，否则带分隔符的合法名称会串到其他 server。

作用域先合并，再按 `enabled` 过滤连接项：项目同名配置即使 `enabled:false` 仍覆盖全局项，不会回落连接全局服务器。

## 2. 连接池、运行态与事件

[manager.rs](../src-tauri/src/mcp/manager.rs) 的 pool key 是 `(scope, project_id, name)`；会话持有其可见 server 集合。warm 先登记引用再并行连接，IPC 返回只表示接收请求，不表示所有 server 已 Ready。

状态为 Starting / Ready / Stopped / Evicted / Error；错误序列化形态与前端类型保持一致。`mcp:status` 是增量通知，载荷含 scope/name、工具数量与详情、过滤数、pid/error/note；恢复或发现丢事件时用 snapshot 取权威快照，不能靠最后一个事件重建整个池。

- 会话移除或重登记后不再引用的条目，引用归零即停止并取消服务。
- 默认池上限 16，LRU 只淘汰无会话引用者；活跃连接不会为凑上限被杀，因此总数可暂时超限。淘汰态与停止态不同，重连有去抖。
- generation 标记阻止“disconnect 后旧握手晚到”重新把条目变回 Ready。
- [process.rs](../src-tauri/src/mcp/process.rs) 在 Unix 使用进程组，Windows 使用 Job Object/隐藏窗口与退出清理；不能只杀启动器而留下 server 子进程。
- test 使用临时连接，返回探测结果后清理，不把测试连接当作长期会话池成员。

当前 IPC 为 `mcp_list_config`、`mcp_save_config`、`mcp_connect`、`mcp_disconnect`、`mcp_reconnect`、`mcp_test`、`mcp_snapshot`，入口见[host/commands/mcp.rs](../src-tauri/src/host/commands/mcp.rs)。旧 get_mcp_config/save_mcp_config/connect_mcp/mcp_status 适配已移除，不再作为迁移接口维护。

**配置保存的实际影响比局部重连更大**：当前保存成功后调用 disconnect_all，再为现有会话 warm 可见配置；不能承诺只重启被编辑 server，也不能保证其他进行中的调用不受影响。更细粒度的差量重连仍需独立实现与并发验收。

## 3. 工具集合、调用与审批

工具定义只来自当前会话可见、Ready、过滤后允许的条目。MCP schema 参与上下文估算，不能只算内置工具。server 的 tools/list_changed 通知仅标 dirty，随后取定义时刷新，不在通知回调里并发改正在消费的集合。

这里有两个应如实保留的当前边界：

1. `tool_defs_for` 实际在每次构建 LLM 请求时调用，不是整个 run 固定一次；run 级 system_frozen 只冻结系统提示，**不保证 MCP schema 在 run 内不变**。
2. 当前先清掉所有条目的 dirty，再刷新本次会话可见且 Ready 的条目；其他不可见条目的通知可能被消费却未刷新。需要多项目/多会话测试，不能仅凭单 server 刷新样本宣称完整实时一致。

MCP 调用走批次层独立审批：server 未声明 read_only、也未 always_allow 时，**即便 FullAccess 也会请求确认**。Plan 排除全部 MCP。选择始终允许时按原全局/项目作用域写回，并兼容保留已有键拼写。read_only 与 always_allow 是用户配置的信任边界，不应由模型通过工具描述推断或暗改。

stdio 是用户配置的本机进程，启动不经过普通 command 的 AST fence；HTTP server 连接也不能直接套用 web_fetch/http_request 的 SSRF 保证。启用前应核对来源、命令、环境变量、工作目录和端点，勿把 MCP 作为绕过项目文件边界的“只读工具”。

[error.rs](../src-tauri/src/mcp/error.rs) 与 manager 区分配置、启动、握手、调用、取消错误；连接错误的分类辅助函数不等于重试已接线。**当前 call 只有一次调用与超时/取消竞争，没有自动重连后重放。** 若以后增加恢复，只能对明确连接类失败且可证明只读的调用谨慎重试；取消、超时和可能已生效的写调用不能盲目重放。

MCP server 的 is_error 返回工具失败；文本和图片进入已有内容通道，audio/resource/resource_link 当前只是可读投影，不能声称完整资源、音频协议支持。resources/prompts、反向 sampling/elicitation 也不在已交付能力内。

## 4. 技能发现、覆盖与管理

[skills/mod.rs](../src-tauri/src/skills/mod.rs) 扫描目录式 SKILL.md 与受支持的单文件技能，去除 frontmatter 后提供正文；CRLF 文件须与 LF 一致。覆盖优先级从低到高为：

1. 内置技能
2. `~/.claude/skills`
3. `~/.agents/skills`
4. 主工作区 `.claude/skills`
5. 主工作区 `.agents/skills`
6. 全局 `<data_dir>/skills`，通常 `~/.codewave/skills`
7. 项目 `<project_dir>/skills`，即 `<主目录>/.codewave/skills`

`data_dir` 始终是全局目录；项目路径单独走 project_dir，不能再混为一谈。extra_roots 不自动扫描技能。列表展示顺序与覆盖优先级不同，不可按“列表排在前面”判断生效来源。内置 preview 也允许被同名自定义技能覆盖；同名时应核对实际来源。

设置页可删除项目/全局托管技能，不删内置与 `.agents/.claude` 兼容目录。删除路径须在 canonical 后仍位于允许的托管根内；禁用与删除是两件事，不能因为技能已禁用就丢掉安全定位所需的信息。

### 缓存的已知限制

SkillIndex 的 get/list 缓存 TTL 为 10 秒，key 只有 workspace/project，未包含 disabled 列表。当前 toggle_skill 与 save_config 不主动 invalidate；因此禁用已加载技能可能在 TTL 内仍可被取得。reload/delete 会失效缓存。不能把“设置已禁用”写成即时撤销保证；[tools/skill.rs](../src-tauri/src/tools/skill.rs) 中 `disabled_after_load_should_invalidate_within_ttl` 仍是 ignored 探针，需修复缓存契约后启用。

## 5. 技能加载、询问规范与输入触发

模型通过 `skill(name, args)` 按需加载正文，返回 `<skill-loaded>` 和隔离的 caller-context；缺失/禁用返回 E_SKILL_NOT_FOUND 并给出可用名称。技能是提示知识，不能拓宽工具集合、批准范围或文件权限；当前没有随正文附送完整附属文件清单。

宿主在加载结果尾部统一注入 ASK_INTERACTION_NORM，目的是让需要多轮收集信息的技能优先使用真实 ask：每次 1–5 个问题、每题最多 6 个选项、推荐项有明确标识，互斥题显式 single=true，开放问题不硬造选项；ask 不可用才退回文本问答。它是条件提示，不是根据关键词硬编码识别技能，也不要求新增 interaction frontmatter。生成/分析型技能不应被强制先问一轮。预览技能及批准后重问的约束见[计划与审批](./plan-and-approval.md)。

Composer 的 `/技能`、`$角色` 与 `@路径` 是写入消息的文本约定，不是绕过模型的隐藏 RPC：

- `/` 与 `$` 只在消息以该符号开头时触发，判断基于光标前片段；回填保留光标右侧正文，不重新用整段末尾正则定位。
- `/` 列出技能，选择后填 `/name`；技能详情在右栏展示，不能直接把所有正文塞入输入框。
- `$` 只列可委派角色，title 等内部角色不可点名；未知角色须解释，不能静默换成另一个角色。
- 异步候选返回须有序号防护，避免旧查询覆盖新光标状态；插入事件追加内容，不覆盖已有草稿。

界面与触发器维护入口见[聊天与界面](./chat-and-interface.md)，角色与运行约束见[子代理](./subagents.md)。

## 6. 验证与仍开放的验收

MCP 的重点是行为序列而非截图：两作用域同名覆盖、名称碰撞回查、配置坏条目、过滤后 schema、一次/始终允许持久化、取消与超时不重放、disconnect/晚握手、关闭会话释放、退出清理子进程、多会话 tools/list_changed、保存配置影响在途调用。Node 测试服务见 [mcp-test-server.mjs](../scripts/mcp-test-server.mjs)；环境缺 Node 导致跳过不能算协议验证通过，Windows 进程树与 Unix 进程组需各自验证。

技能重点是来源覆盖、CRLF/frontmatter、禁用缓存即时失效、删除目录限制、未知技能恢复，以及最终模型结果确实含询问规范。原技能附属文件发现、MCP 差量重连与 dirty 刷新隔离仍未交付；不要把历史路线图、旧 IPC 清单或曾经通过的单场景验收当作现行保证。通用门禁见[测试指南](./testing.md)。
