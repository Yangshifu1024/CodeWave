# MCP 与技能

> 当前行为指南；源码核对：2026-10-12。MCP 提供外部工具，skill 提供按需加载的操作知识；两者都不能替代宿主权限检查。模式与批准见[计划与审批](./plan-and-approval.md)，命令/网络边界见[工具与安全](./tools-and-safety.md)。

## 1. MCP 配置、作用域与传输

配置只认全局 `<data_dir>/mcp.json` 和当前项目 `<project_dir>/mcp.json`；project_dir 是主目录下 `.codewave`，不是代码根。项目同名 server 覆盖全局，额外授权目录不产生新配置作用域。配置读取按 server 容错，坏条目单独报错；保存会校验整份目标作用域，出错不能当作成功保存。读取、保存和草稿测试共享纯解析/校验，不创建含草稿内容的临时验证 JSON；读取权限/IO 错误不会伪装成空配置。

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

### 启动信任

stdio 启动本机进程和 streamable-http 建立远端连接都需要用户先在服务器卡片审阅并选择「批准启动」（本次）或「记住此版本」。两者都绑定 `(scope, project_id, name, canonical_config_path, configuration_hash)`；记住许可另绑定全局数据 profile 和权威 revision，存于独立系统钥匙串。配置文件中的 enabled/read_only/always_allow 不授予启动权限，也不能自行声明 trusted。本次批准不新增持久记录，也不撤销已有记住许可；需要降为本次时先撤销原许可。确认框显示宿主返回的快照、来源、版本和启动摘要（command/args/cwd 或脱敏 URL）；env/header 仅展示键名，已识别的敏感参数和 URL 凭据隐藏。

自动 warm、显式连接/重连、保存后的差量应用及直接 ensure 共用启动门；许可不存在时保留停止状态且不产生进程或连接。`mcp_prepare_launch` 发出五分钟有效、总量上限 64 的宿主票据，绑定用途、来源、完整配置及权威 revision；准备不启动进程。`mcp_confirm_launch` 消费票据，错误、过期或重复确认拒绝，配置/项目变化要求重新审阅。模型 ask、auto_confirm、FullAccess 和 probe 成功都不创建记住许可。旧 bool IPC 只能新增本次许可，撤销仍会撤销匹配版本的持久记录。

持久许可在启动、Ready、工具捕获及最终 dispatch 读取当前权威，使用 guard→CONFIG/manager 短锁顺序，原生钥匙串等待不持 CONFIG/manager 锁。配置保存先撤销受影响版本并保留权威 barrier，再在 CONFIG 锁内二次校验文件版本后写入；冲突可以保守失去许可，但不覆盖较新配置。删除、禁用或修改条目均使旧许可失效，恢复旧文件也不会恢复已撤销许可；不变条目和其他来源保留授权。没有外部文件监控；持久许可在准入重验时检查当前文件，外部编辑的本次许可仍在后续重读/差量接纳时检查。

「撤销启动授权」清除正式许可、待确认及活跃 probe，撤下工具并取消握手/服务；已消费但尚未最终接纳的旧批准票据同样失效。持久撤销写入 tombstone，最终提交点再次撤销期间新装入的本次许可。原生写入结果不明时显示「检查授权结果」，该入口不因同一资源的许可状态/revision 刷新而消失。`mcp_launch_operation` 读取原生回执，不重新写入批准；确认原撤销已提交时，只完成一次其本地清理，重复查询不会撤销用户之后新批准的本次许可。钥匙串不可用不自动降级成本次许可；用户仍可显式重新批准本次。「断开」只释放连接，不撤销许可。「始终允许」仅热更新相同目标的政策及对应权威版本，不将它当作新的启动批准。

其它实例在新 admission 时直接检查权威；后台每两秒发起存活许可检查，实际回收延迟还包括钥匙串等待、候选数量与服务关闭，不能承诺固定两秒内停止。已接纳调用、在途握手和服务自主副作用无法追溯回滚；普通配置切换仍保留原 peer 最多五秒排空且不重放。独立管理器/真实 Node fixture 已覆盖恢复、最终调用拒绝和握手晚到；这不代替真实钥匙串、OS 多进程、Windows/Linux 和 GUI 验收。项目提示、技能正文的信任准入仍未接入，不能把 MCP 门等同完整项目信任。

### 持久权威事务与准入

[core/trust.rs](../src-tauri/src/core/trust.rs) 提供版本绑定的权威存储和 admission guard；[host/trust.rs](../src-tauri/src/host/trust.rs) 使用独立 `codewave.yangshifu.xyz.trust.v1` 钥匙串 service，模型凭据账户不能跨 service 读取。桌面在 `lib.rs` 注入此 backend；[manager_authority.rs](../src-tauri/src/mcp/manager_authority.rs) 负责 MCP 准入，[review_service.rs](../src-tauri/src/mcp/review_service.rs) 负责宿主票据确认和回执，均不新增 core→host 依赖。

账户摘要绑定 canonical 全局数据目录与资源类别、用途、canonical 来源、owner 和名字；不同 profile 不共用权威账户，同一 profile 的 canonical 别名共用记录与锁。版本统一接受裸 SHA-256 或来源审计的 `sha256:` 格式并归一。每资源小记录保存 schema、版本、decision、revision 和 operation id；撤销保留 tombstone，CAS 冲突要求重新审阅，同一操作仅在原前置版本与内容一致时重放回执。写入后回读核验，不将系统钥匙串调用返回成功等同于提交确认。

事务在 blocking pool 中持有进程内锁与 `<data_dir>/trust/authority.lock` 的 [OS advisory lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock)。锁文件永不作为授权数据。总等待预算为 5 秒；原生调用超时或取消后，后台任务仍持锁直到结束，写入可能已发生时返回 `TrustOutcomeUnknown`，不能自动改用新操作再次授权。`AuthorityGuard` 供后续调用点在短锁内登记 admission 后释放，不得持有它等待用户、网络或长运行进程。NoEntry、不可用、损坏、拒绝、冲突、超时及结果不明分别处理，无 JSON 或明文回退。

`compare_exchange_guard` 在提交后保留事务锁供短期准入登记；`revoke_batch` 在配置更新前撤销旧记录并保持 barrier，不能在撤销与文件 CAS 之间释放。相同 OS 用户绕过本应用直接调用钥匙串、替换锁文件或操纵外部进程不属于 OS 沙箱保证。真实钥匙串与三平台锁行为仍待分别验收。

## 2. 连接池、运行态与事件

[manager.rs](../src-tauri/src/mcp/manager.rs) 的 pool key 是 `(scope, project_id, name, canonical_config_path)`；会话持有其可见 server 集合，设置页读取来源路径和启动许可版本用于确认。warm 先登记引用再为已获许可的配置并行连接，IPC 返回只表示接收请求，不表示所有 server 已 Ready。

状态为 Starting / Ready / Stopped / Evicted / Error；错误序列化形态与前端类型保持一致。`mcp:status` 是增量通知，载荷含 scope/name、工具数量与详情、过滤数、pid/error/note；恢复或发现丢事件时用 snapshot 取权威快照，不能靠最后一个事件重建整个池。

- 会话删除或手动释放后不再引用的条目，引用归零即停止并取消服务。配置重登记撤下旧工具入口后，旧连接最多排空 5 秒；超出预算才取消。关闭 Tab 仍保持后台运行，不等同删除会话。
- 默认池上限 16，LRU 只淘汰无会话引用者；活跃连接不会为凑上限被杀，因此总数可暂时超限。淘汰态与停止态不同，重连有去抖。
- generation 与 queued-connect 票据阻止“disconnect 后旧握手/排队建连晚到”重新把条目变回 Ready；同一 key 的当前握手单飞。状态发给该 key 的全部引用会话，UI 状态显式绑定会话，切换会话清空旧投影；按 config_revision/generation 拒收旧状态，快照读取以本地事件时钟防止迟到 Starting 覆盖新 Ready，失败只保留同会话已知状态。
- [process.rs](../src-tauri/src/mcp/process.rs) 在 Unix 使用进程组，Windows 使用 Job Object/隐藏窗口与退出清理；不能只杀启动器而留下 server 子进程。
- test 使用当前条目草稿的独立临时连接，不读磁盘同名配置，不改变正式池/工具/pid；详情见下节。

当前 IPC 为 `mcp_list_config`、`mcp_save_config`、`mcp_set_launch_trust`、`mcp_prepare_launch`、`mcp_confirm_launch`、`mcp_launch_operation`、`mcp_discard_launch_review`、`mcp_connect`、`mcp_disconnect`、`mcp_reconnect`、`mcp_prepare_test`、`mcp_test`、`mcp_snapshot`，入口见[host/commands/mcp.rs](../src-tauri/src/host/commands/mcp.rs)。旧 get_mcp_config/save_mcp_config/connect_mcp/mcp_status 适配已移除，不再作为迁移接口维护。

保存通过同一提交锁协调文件版本检查、原子写与完整会话可见集差量接纳。`mcp_list_config` 返回不含原文的 `version`；`mcp_save_config` 必须回传 `expected_version`，冲突时保留草稿并要求重新读取。格式化/键序变化不重启连接，项目修改只触及真正读取该配置路径的 runtime；同项目 id 不同路径保持独立。全局变更不会重建被项目同名项覆盖的连接，disabled 项目覆盖继续遮蔽全局项。

保存回执的 `saved / issues / version / apply_revision / affected_servers` 表示文件已保存、差量计划已接纳，**不代表连接 Ready**。连接失败保留新配置并在目标条目显示 Error，其它 key 不受影响；UI 保存后只读取 snapshot，不再额外全量 connect。普通保存不调用 disconnect_all，该接口仅用于明确的全停。受影响 key 的已发出调用保留原 peer，最多排空 5 秒后取消；外部副作用可能已经发生，不自动重放，也不宣称取消能撤销远端操作。

### 当前草稿测试

测试先经 `mcp_prepare_test` 取得绑定来源、项目和完整草稿版本的单次票据；准备不执行命令或网络操作。用户审阅启动摘要并确认后，`mcp_test` 才消费票据建立临时连接。票据五分钟过期，错误配置/项目消费也使其失效；待确认与正在运行的 probe 合计最多 64。测试不继承正式许可，也不授予正式许可。取消确认或编辑草稿后不发起测试；已确认开始的测试仍按下述预算回收。

编辑器通过与保存共用的 `serializeMcpServer` 发送 `name + config`，包含 args（含空串/空格）、env/headers 多行原值、cwd、显式 transport、timeout、过滤、信任标记和未知字段。空名、重名、重复键、非法 timeout 在启动前阻断；不以静默丢字段使非法草稿看似有效。新建/重命名/disabled 条目无需保存即可测试；project 仅校验当前项目上下文，不读取合并后的同名 global 条目。

测试只做 initialize/tools/list；初始化和列表阶段各上限 30 秒，总预算 60 秒，回收另有 5 秒上限。临时 service 有 RAII 所有权，失败、超时和任务取消均回收；展示/日志不回显原始配置、env/headers 值或远端错误正文。成功只说明点击瞬间的草稿握手和工具发现成功，不能代替正式连接状态。

每份草稿单飞；任何字段修改、关闭重开、切会话/作用域都会使旧结果失效。响应仅在编辑身份、草稿代数与请求身份仍匹配时接纳，不移动保存基线。当前关闭编辑器不会发出立即取消 probe 的 IPC，测试仍可能继续到上述预算，界面已明确提示。测试会启动所填命令或访问所填地址，未保存不等于没有副作用。

## 3. 工具集合、调用与审批

工具定义只来自当前会话可见、Ready、过滤后允许的条目。MCP schema 参与上下文估算，不能只算内置工具。server 的 tools/list_changed 通知仅标 dirty，随后取定义时刷新，不在通知回调里并发改正在消费的集合。

每次 LLM step 通过 `tool_snapshot_for` 逐 server 在权威 guard 与管理器短锁内同时捕获工具与私有 `CallBinding`，汇合后再校验会话、函数映射和连接/config/policy/schema 代数；票据不发送给模型。上下文估算调用 `tool_defs_for` 不覆盖模型请求的票据。schema 可在不同 step 变化，run 级 system_frozen 仍只冻结系统提示。

审批与真正 dispatch 使用同一票据。dispatch 在锁内重验会话可见性、来源/连接/配置/政策/schema 与函数反向映射，再取得原 peer 并登记在途调用；已变更时明确要求重新发起，不能将旧批准重定向到新 endpoint。read_only/always_allow 无人工审批调用也同样检查。dirty 只被可见会话消费；刷新若晚于连接代数变化则丢弃，工具集合发生变化会推进 schema 代数。

MCP 调用走批次层独立审批：server 未声明 read_only、也未 always_allow 时，**即便 FullAccess 也会请求确认**。Plan 排除全部 MCP。选择始终允许时在同一提交协调器内验证原票据，再从最新原作用域文档仅合并允许字段，保留未知键、其它条目与已有键拼写。目标配置发生变化则拒绝补丁；仅政策变化可热更新 policy_revision 并返回原目标的新政策票据，不重建 peer、不刷新 endpoint/generation 来掩盖变化。read_only 与 always_allow 是用户配置的信任边界，不应由模型通过工具描述推断或暗改。

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

### 来源、版本与覆盖审计

[catalog.rs](../src-tauri/src/skills/catalog.rs) 一次扫描同时生成运行索引与管理候选目录。`list_skills/get_skill/reload_skills` 继续只返回启用的最高优先项；`inspect_skills` 返回全部合法候选、按名禁用状态、缺失来源的禁用名和 IO/解析诊断。最高项排在候选首位并标 selected，禁用按名字整体生效，不会回落到低层技能；设置页可展开被覆盖来源，禁用后重开仍可管理。

[resources.rs](../src-tauri/src/resources.rs) 的来源描述包含类别、canonical 身份和 `sha256:` 内容版本。文件版本来自实际读取的原始字节，CRLF 变化也产生新版本；不是 mtime。审计与 get/list 共用同一个缓存快照，实际 `skill` 的 `ToolOutcome.data.resource` 与该快照对账。来源类别描述发现位置，不证明作者可信，也不授予执行权限。

同一 canonical 文件重复扫描时只保留最高优先级来源，即使无 frontmatter、别名目录会产生不同 fallback 名也只保留最高来源的名字。同层同名冲突按规范化路径、再按原始路径稳定排序，最后一项胜出并提供诊断；这取代原来未定义的 read_dir 顺序。无效 YAML、非 UTF-8、断链及读取错误可见，普通无 SKILL.md 子目录不算错误。

设置页仅允许删除当前最高且可删的项目/全局托管来源，不删 builtin、兼容来源、链接条目或链接技能根。请求必须携带预览的来源身份和内容版本；后端在共享索引锁内重扫、核对当前最高项、重读字节，并复验 canonical 根、目标和链接状态。根级单文件 `skills/SKILL.md` 只删文件，目录技能则删除其目录及附属内容，不能删除整个 skills 根。成功后重新审计，显示真实下层回退；按名禁用状态保留。外部进程仍可能在最终路径检查后换链，这不是 OS 级原子删除或沙箱。

### 缓存与禁用生效

SkillIndex 的 get/list/audit 缓存 TTL 为 10 秒；结构化 key 包含 workspace、全局 data_dir、project_dir 和排序去重后的 disabled 集。配置禁用/重新启用发布后，下一次 get/list/audit 不能命中旧政策的缓存，不必等待 TTL 或依赖某个配置入口主动 invalidate；reload/delete 仍强制失效。不同全局数据目录和项目作用域不会串缓存。显式提供的已关闭会话返回错误，不回落全局来源。[tools/skill.rs](../src-tauri/src/tools/skill.rs) 的 `disabled_after_load_should_invalidate_within_ttl` 已启用，覆盖加载后禁用及重新启用。已进入运行历史/冻结系统提示的技能内容不会因此从模型记忆中撤回；这里保证后续获取拒绝，不承诺即时撤销在途步骤。

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

技能重点是来源覆盖、CRLF/frontmatter、禁用缓存即时失效、删除目录限制、未知技能恢复，以及最终模型结果确实含询问规范。原技能附属文件发现仍未交付；不要把历史路线图、旧 IPC 清单或曾经通过的单场景验收当作现行保证。通用门禁见[测试指南](./testing.md)。


### 本轮回归与人工验证

R08/R11 新增回归覆盖：配置版本冲突/非法输入无写入、等价配置与其它项目/shared key 身份不变、同项目 id 异路径、会话释放与迟到 queued-connect、schema/来源变更票据失效、始终允许合并未知字段、已发出调用有界排空且不重放、probe 在初始化/tools-list 阶段取消后的进程回收、UI 精确草稿载荷/单飞/慢旧快新/关闭重开/保存版本及状态代数隔离。

`v0.11.0` 对应提交的完整三平台 CI 已通过，包含真实 Node stdio/HTTP fixture 与宿主后端测试；各平台结果和跳过边界见[自动验证记录](./testing.md#可靠性修复自动验证记录2026-10-12)。本地 Linux 云环境仍缺 GLib/GTK/WebKit/DBus 等系统依赖，不能写成本地完整门禁通过，也不能将 CI/headless 结果写成 GUI 已验收。会话释放进程树回收已有三平台自动证据；初始化/tools-list 期间 probe 取消回收为 Unix 专属测试，Windows 对应边界仍待补验。

手工清单（尚未执行）：
0. 从无记住记录开始，验证本次批准退出即失效；记住相同来源/版本后重启恢复。分别核对 stdio/HTTP 的宿主摘要与敏感值隐藏，取消确认零启动。撤销 Ready/正在握手的服务，工具消失且进程回收，重启不恢复；修改、禁用、删除以及恢复旧配置后均须重新批准，其他来源授权不变。刷新不覆盖未保存草稿。
1. A/B 两项目与临时会话共用 global MCP，B 发起长 echo 时保存 A 私有配置；B/global pid 与工具保持不变。
2. 同一 server 请求等待批准时修改 endpoint 或作用域；批准旧请求应得到“配置已变”，新 endpoint 收到零请求。
3. 编辑已保存可用命令为不存在的命令，不保存直接测试应失败；取消后正式连接保持原 pid。新建/重命名草稿也能测试。
4. 慢 probe 期间改字段、关闭重开或切换会话；旧成功不得出现。关闭后按预算检查临时进程归零。
5. 两编辑器交错保存，以及设置保存与“始终允许”交错；冲突保留草稿，允许补丁不覆盖新 endpoint 或其它字段。
6. 在应用内分别验 Windows Job Object 与 Unix 进程组回收，尤其是初始化/tools-list 期间取消 probe；三平台会话释放回归通过不等于 Windows 已覆盖 Unix 专属 probe 取消用例。
7. 技能页展开同名覆盖，核对来源和版本；禁用保存、重开、重新启用及取消草稿，运行状态与草稿提示应分开。删除最高托管来源后看到正确下层，旧版本确认不得删除新来源；切项目的迟到读取/确认不影响新项目。检查长路径与版本在窄窗/缩放/暗色下可读。GUI 与 Windows 根链接/路径行为仍待人工或对应平台验证。
8. 锁定/拒绝系统钥匙串时，持久许可失败关闭，记住操作显示失败或结果不明；未知结果只能检查原操作，不自动重发批准。本次许可须由用户重新明确批准。检查旧确认在切项目、改内容、关闭页面、重复提交后均不产生许可，撤销与本次批准交错时最终状态一致。
