# 会话与项目

本文定义项目归属、会话现场恢复、历史与边车存储、删除清理的长期契约。运行生命周期见[运行与调度](./runtime-and-scheduling.md)，子代理的执行与终态见[子代理](./subagents.md)，权限规则见[工具与安全](./tools-and-safety.md)。

## 1. 项目、工作区与目录授权

项目是“名称 + 单一主目录”，不是多根项目。托管数据随项目放在 `<主目录>/.codewave/`：`project.json`、`temps/`、`logs/`、`memory/`、`skills/`、`tasks/`，以及项目 MCP、经验文件。全局配置、会话索引与历史仍在 `~/.codewave/`。`runtime.data_dir` 恒为全局数据目录，项目托管目录通过 `runtime.project_dir` 访问，不能混用。

- 项目会话必须选项目；创建时验证主目录存在，固化 `SessionMeta.project_id + roots`。`@` 路径引用和 Git 查询以会话快照为准，不能事后用注册表的新主目录替换已有会话工作区。
- 临时会话免选目录；不传 `project_id/workspace` 时工作区为全局数据目录。IPC 仍支持显式 `workspace`。写入权限仍由 fence 与审批档位决定。
- 命令缺省 cwd：项目会话为项目托管目录，临时会话为工作区根。显式 cwd 按读取边界解析。
- 项目“始终允许”的外部目录保存在 `allowed_dirs`，创建或恢复会话时，仅将仍存在的目录并入 `extra_roots/roots`。这是附加访问授权，不改变“项目只有一个主目录”的语义。
- 当前项目注册表通过全局 `projects/directories.json` 找到各主目录内的 `project.json`。不要恢复早期多根模型或旧项目目录扫描迁移逻辑。

实现入口：[projects.rs](../src-tauri/src/core/projects.rs)、[会话 IPC](../src-tauri/src/host/commands/session.rs)、[项目 IPC](../src-tauri/src/host/commands/project.rs)。

### 项目编辑不能丢权限

落盘 `ProjectEntry.allowed_dirs` 是普通数组；保存入参 `ProjectSaveInput.allowed_dirs` 则是可选数组，三态不可合并：

| 入参 | 行为 |
|---|---|
| 缺字段或 `null` | 通过旧 id 的目录索引读取旧项目，保留旧授权 |
| 非空数组 | 显式替换 |
| `[]` | 显式清空 |

前端编辑必须透传 `editing?.allowed_dirs`，不能用 `?? []` 把缺字段变成清空。读取旧值只查对应 id；文件内 id 不匹配时不得继承别的项目授权。项目主目录变化时 `data_dir` 归一到新目录，但旧托管目录及其中数据不会自动搬迁或删除。

保存入口在任何写盘前验证名称和主目录非空；core 再防空主目录污染进程 cwd，id 在读盘前验证。内部保存不额外拒绝历史空名项目，以免已有项目无法创建会话。路径比较优先 canonicalize 两端，避免 Windows verbatim 路径比较失真。

新建项目选目录且名称为空或仅空白时，名称使用共用 `baseName()` 自动填一次；取消选择不改名，已有非空名称不覆盖，编辑已有项目也不联动。项目允许重名，身份始终是 id。

## 2. 数据分层与归属

| 路径（相对全局数据目录） | 事实与用途 |
|---|---|
| `sessions/index.json` | 会话列表、项目/roots 快照、标题、模型记录、运行/中断/历史状态；最多 2000 条 |
| `histories/<id>/0001.jsonl…`、`.segmeta.json` | 主会话 append-only 转录与增量水位 |
| `histories/subs/<parent>/<sub>/` | 子代理过程转录；不进入主会话索引 |
| `histories/<id>.json.gz` | 旧历史读兼容；不是新保存格式 |
| `sessions/<id>.todos.json` | 当前计划 |
| `sessions/<id>.prefs.json` | 显式模型选择与思考力度；不保存权限档 |
| `sessions/<id>.artifacts.json` | 文件写入归属和计划文件登记 |
| `sessions/<root>.toolres/<call_id>.json` | 仅用于界面恢复的完整工具结果 |
| `sessions/<owner>.imgblob/<hash>` | 图片外置内容；子历史 owner 为 `<parent>__<sub>` |
| `ui-state.json` | Tab、草稿、队列、滚动锚点及面板/树等现场态 |
| `running.marker` | 上次进程是否异常退出的标记 |

主会话 runtime 才能通过常规 checkpoint 写主历史和索引：`is_main_session` 必须显式标记，不能靠 id 前缀猜身份。子代理走 `save_sub_history`，计划任务不保存为普通会话；否则左栏会出现空标题“幽灵会话”。启动时 `purge_non_session_entries` 清理早期 `sub_`/`task_` 污染条目。

## 3. 历史存储：只追加，不因上下文预算删转录

实现：[store.rs](../src-tauri/src/core/sessions/store.rs)、[segments.rs](../src-tauri/src/core/sessions/segments.rs)、[repair.rs](../src-tauri/src/core/sessions/repair.rs)。

### 段与水位

每个 JSONL 段首行是 `header`（schema、session、seq、base、时间），后续为 `message`、`compaction` 或 `seal`。段明文不压缩，便于现场诊断。达到 200 条消息或 512 KiB 时封口，但必须延后到安全的工具配对边界，不能把 `tool_use/tool_result` 拆到两段。

内存历史没有稳定消息 id，且 repair、降级与压缩会改写前缀。每次保存先 `prepare_for_save`，比较已写条数与前缀指纹：

1. 前缀匹配且有新增：只追加增量。
2. 长度和前缀均相同：历史不写盘；标题/模型变更仍更新索引。
3. 前缀不匹配：写新的 `base` 段。既有段一律保留，绝不覆盖、截断或删除。
4. 历史变短：新段记 `compaction` 边界和压缩后的完整 `head`，旧转录继续保留。

`.segmeta.json` 保存水位、blob 引用并集、段数/序号/尾段字节数等校验证据。证据失配回退目录扫描，不能只信条数；水位写失败不把已经成功落盘的历史报成失败。

常规 checkpoint 为每 20 步和 run 收尾；steer 消化用户新消息后立即尝试 checkpoint。段封口和收尾执行 sync，并非每行 fsync。不要把 UI 快照的 1–2 秒目标误写成“每个生成帧已耐久落盘”，断电级效果仍需实机验证。

### 展示与出网分离

- `display` 是所有保留的 message 转录，不按 token 裁剪；前端首屏加载最近一段，再通过 `load_session_earlier` 向前翻页。
- `wire` 从最后一个时间线重置点重建，再 repair 和 `trim(256 * 1024 token, keep_last=2)`。保存路径不做这个 trim。
- wire 从尾部读到够用且满足安全轮边界即停；必须与“全量扫描 + repair + trim”逐条、逐字节一致。修改 repair 的局部性时必须重跑对拍，不能让打开会话的读取量重新随总历史线性增长。
- 前端展示消息不得回填模型上下文。稳定渲染键为 `s<段号>:<段内序>`，分页前插不能使用数组下标；边界以 `seq` 关联，不能传 display 下标。
- 前端最多保留 30 个分页页，支持收起更早内容；目前不是虚拟滚动。

### 容错、容量与旧格式

尾部残行丢弃，坏行/坏段跳过，分页报告 `bad_segments`；不能因一个坏段使整个会话打不开。旧 gzip 损坏会记日志并回落空历史，因此“空白”不一定证明原来没有内容。

200 MiB 是软告警线，仍照常写；1 GiB 是高位熔断线，停止新增历史而不删除旧数据。预检查量的是既有历史体积，单次增量可能越过硬线；不要将它描述成严密的逐字节配额。熔断期间仍更新索引状态，但不写历史、不写 blob、不做 GC。子代理过程历史不参与该体积熔断，异常只记日志。

`SaveReport` → `run:done.history_save` 和 `SessionMeta.history_status` 同时提供当场与重启后的提示。扩展状态必须同步修改 `SaveReport::is_clean()`，否则提示会被上层过滤掉。索引或水位更新失败只告警，不把已经写好的历史错误报告为未保存。

旧 `.json.gz` 只读兼容，不自动迁移。新旧并存时，只有 `segments::has_readable_messages()` 证实新段至少有一条可读消息才优先读新格式。设置页“清理旧格式历史”必须用同一判据；空目录、全坏段、只有头和封口的段都不足以删除旧副本。旧格式清理只扫描主历史一层，不处理子历史旧文件。

## 4. 图片与完整工具结果

### 图片外置与 GC

[PersistedMessage/PersistedContent](../src-tauri/src/core/sessions/persist.rs) 是独立落盘 DTO，图片有 `ImageInline/ImageBlob` 两态。不要修改内存/wire 的 `Content` 来实现存储优化，三种 provider 协议仍发送原来的内联 base64。

[image_blobs.rs](../src-tauri/src/core/sessions/image_blobs.rs) 使用 base64 原文 SHA-256 的前 32 位小写 hex 作内容地址，同 owner 内去重、原子写；单图 base64 超 7,000,000 字符或外置失败时保留内联。8 MiB 不再是历史总量上限，但仍作为异常内联图片的剥图兜底阈值；不要承诺所有输入永远无损。

必须守住：

- 保存从图片外置到历史、索引和 GC 全程持 `save_lock`，锁序固定 `save_lock → index_lock`；所有索引修改走 `index_lock`。
- GC 只在历史成功写入后执行，引用集合为“磁盘上所有保留段引用的 blob 并集”，绝不是本次内存历史的集合。
- fallback 只增加段，因此旧段的图片仍活着；熔断/保存失败不得 GC。GC 仅删除名字符合 hash 形状的文件，不碰临时文件与异物。
- 子代理 id 只有短随机值，owner 必须带父会话 `<parent>__<sub>`，避免跨会话同 id 互删图片。
- blob 缺失时加载降级为占位文本；blob 随所属会话级联删除，不出现在文件面板。

### 工具结果 sidecar

模型历史只保存 `compact_for_model` 的瘦身文本。完整 `ToolOutcome` 通过实时事件展示，但大 JSON 头尾截断或 `[error …]` 前缀会使历史无法还原卡片。

[tool_results.rs](../src-tauri/src/core/sessions/tool_results.rs) 在批次构造模型文本时，对“有 data 且模型文本不能解析为 JSON”的结果保存整套 `ok/data/error/warnings`。判定必须在追加 model hint 之前。键用 provider 的 `tool_use.id`，不能用实时随机的 `batch:index`；读写共用 call-id 安全化函数。

- owner 为 `root_session_id ?? id`；子代理工具卡归根会话，计划任务跳过。
- 单文件 2 MiB、单次回读最多 50 个；缺失、非法或超限跳过。旧会话无备份时保留明确占位。
- sidecar 永不进入 `rt.history` 或 wire，不影响 token 计费；不进文件产物列表，随会话删除。
- 主工具卡、子代理卡、子代理抽屉内工具卡共用 `lossyToolKeys/backfillToolOutcomes`。子代理恢复先用 `restored:<callKey>` 时，回填必须将 timeline、subs、subStreams 同步改名为真实 `sub_id`，否则无法拉过程历史。
- 缺结果或 `[interrupted]` 恢复为中性 `E_INTERRUPTED`，真实失败还原错误码；不能把所有历史工具卡显示成成功。
- `duration_ms` 字段已预留，但批次写入仍传 `None`，历史卡耗时不保证恢复。读图工具省略的 `data_url` 仍可能依赖原路径；这不同于用户附件图片的 blob。

## 5. 现场、偏好与中断恢复

[ui_state.rs](../src-tauri/src/core/ui_state.rs) 负责原子保存 `ui-state.json`；损坏文件备份为 `.corrupt`，版本不匹配备份而非删除。前端 [uiState.ts](../ui/src/utils/uiState.ts) 采用 1.2 秒防抖加 2 秒最长等待，避免流式更新不断推迟保存。主题、语言、字体和左右栏开合仍使用既有 localStorage，不双写。

启动顺序不可倒置：先绑定事件，再刷新会话与项目列表，然后 hydrate UI 快照、启用持久化、加载活跃 Tab。只有两份列表都可信时才剔除失效 Tab；读取失败不能伪装“空列表”清空现场。关闭/同 id 重开使用加载代际，迟到历史、Git 或工具结果不得复活已关桶或覆盖新 run。

滚动保存稳定条目键、内容指纹与段内偏移；分页前插先捕获锚点，恢复允许三帧二次校正，失败回落底部。切 Tab 的防抖不得把新会话几何写回旧会话。窗口保存逻辑像素、内尺寸和外框位置；多显示器越界回落可见区域。

权限档不随 prefs 边车恢复，重启重新从全局配置计算初始档位；目标模式已删除，旧文档的目标档例外不再成立。模型/思考力度持久化：`model_choice_recorded` 区分“显式跟随全局”和“没有旧字段”。模型被删则清除 override；旧 Tab 快照仅在尚未记录选择时由 `restore_legacy_model_prefs` 一次性补入。索引 `model_id` 只记录实际使用模型，不能据此推断用户显式选择。

run 中更换模型或思考力度从下一次 LLM 请求生效，当前流及其重试沿用已解析模型；工具审批现读会话偏好。子代理权限同步另见[子代理](./subagents.md#2-权限继承与边界)。

进程异常退出由 [interrupt.rs](../src-tauri/src/core/sessions/interrupt.rs) 根据 marker 将运行会话标为 `crash`；主动中止退出标 `quit`。中断提示由用户清除，不因打开会话自动当作已读。退出握手使用 `ExitGuard`：无 run 的短看门狗、有 run 的 45 秒兜底及二次退出放行必须共享 epoch/confirm，不能在用户选择“等完成”后被迟到超时强行退出；中断保存有 30 秒收尾窗口。

## 6. 标题、导航、文件与日志

### 标题与列表

首条有文本的用户消息启动一次后台自动命名：[title.rs](../src-tauri/src/core/title.rs) 读取内部 title 角色，零工具、20 秒超时，最多输入 2000 字符，清洗为首个有效行、去前缀/引号并截到 20 字符。失败保留首条消息 10 字符兜底；只有当前标题为空或仍为兜底时才覆盖，手动改名与检查必须在同一临界区。删除后 zombie 守卫阻止迟到命名写回。

新会话首次 checkpoint 前可能尚无索引，由 Tab 的 `createdAt` 合成导航项。排序取 `updated_at || created_at`；行内时间只表示最后活跃，新建未发送会话留空，不能用创建时间冒充活跃。仅切会话不重排。异地会话 `done/error` 显示内存未读点，进入即清；取消不产生未读点；运行 spinner 优先于未读点。等待用户期间隐藏行内重命名/删除操作。

### 文件归属

产物不复制、不搬迁，只登记会话写过的绝对路径。create/edit 及文档写入/编辑成功后登记，路径 canonicalize 后按 path 去重，保留 first/last 操作与时间、计数；子代理归根会话，任务 runtime 跳过。登记失败不改变工具成功结果。

`list_session_files` 仅返回 `kind=File`，按最后操作降序，补文件存在与大小；已删除文件灰显，不自动删登记。`kind=Plan` 用于清理归属，不进入文件面板。`writeTick` 变化防抖刷新，切会话用请求代际丢弃旧响应。预览仍走读取根校验；具体文档能力见[文档处理](./documents.md)。

### 日志

会话黑匣子日志常开，不受全局日志级别过滤；项目会话及子代理写项目 `logs/<runtime-id>.log`，临时会话写全局 logs。覆盖 run、LLM、工具、审批、压缩与子代理生命周期；zombie 跳过写入。

详细日志默认关闭，开启后请求/响应单条截 64 KiB，请求使用 `redacted_json()` 屏蔽配置密钥；正文、路径和工具内容仍可能敏感，不能称日志已全面脱敏。全局 `codewave.log.YYYY-MM-DD` 按 14 天清理，仅匹配该命名，`RUST_LOG` 优先于设置级别。日志读取有文件名/id 白名单、2 MiB 尾部读取边界和最多 2000 行；尚无会话日志返回空态。右栏日志仅在可见时轮询，旧会话响应不能覆盖新会话。

## 7. 删除与保留期清理

删除项目级联取消/删除所属会话，并删除项目 `.codewave/` 托管目录，用户主目录的其他代码不动。用户放进托管目录的额外文件、worktree 同样在删除范围内，确认文案必须说清。修改项目主目录不负责搬走旧托管数据。

保留期默认关闭，只接受 1/3/7/14/30 天。判定为 `max(updated_at,last_opened_at) < now - N×24h`，不是自然日；坏时间戳不删，未来时间不删，索引和进程内任一认定运行中则跳过。打开时间写索引节流 10 分钟，不改变导航排序。

触发为启动一次、合法保留期变更后的设置保存、手动清理。设置保存先预览；“暂不清理”只跳过本次，配置仍保存，下次启动仍会清理。手动清理用已保存配置，有未保存修改时禁用。预览与执行必须共用候选口径，包含索引外残留数。

[cleanup.rs](../src-tauri/src/core/sessions/cleanup.rs) 先删托管文件，最后一次性写索引；单条失败保留索引便于重试，不中断整批。删除计划文件只认已登记 `kind=Plan` 且路径通过白名单的 `.md`，绝不删普通产物登记所指的用户文件。正常清理涵盖历史/边车/子历史/blob/toolres、会话日志和合规计划文件；随后清理 runtime 与前端 Tab，防止迟到保存复活。

索引外残留扫描仅在索引可信时启用，排除 `sub_`、`task_` 和子历史 blob owner；段目录年龄不能仅看目录 mtime，还要看内部更新。缺失、损坏索引及遗留 `index.json.corrupt` 都意味着不可信，宁可不删。不得以刚重建的空索引绕过这个保护。

## 8. 排查、回归与开放边界

| 症状 | 首查 |
|---|---|
| 改项目名后外部目录又要审批 | 保存 wire 是否省略/清空 allowed_dirs；旧值读取是否同 id |
| 改主目录后项目消失 | `directories.json` 与新 `.codewave/project.json`、归一后的 data_dir |
| 重开工具卡丢预览或子代理无过程 | provider call id、2 MiB 限制、root owner、合成 sub key 是否已改名 |
| 重开历史空白 | 格式权威探针、坏段计数、旧 gzip 与日志；先保留唯一副本 |
| 图片在压缩/保存后消失 | save_lock、所有保留段引用并集、子 owner、熔断是否误 GC |
| 恢复 Tab 全没了 | 列表是否可信、hydrate 顺序、加载代际与已删除引用过滤 |
| 清理没有回收残留 | 保留期、运行集合、索引信任标记、合法 id、预览 orphan_count |

测试入口：后端 `core::projects`、`core::sessions`、`core::title`、`core::agent`；前端 `projects.contract`、`projectnav.order`、`projectnav.row-states`、`uiState`、`appshell.restore`、`scrollAnchor`、`run.restore-toolresults`、`history-status-notice`、`sessions.cleanup`、`settings.cleanup`、`rightbar.files` 和 `rightbar.log`。命令与全量门禁见[测试指南](./testing.md)，测试计数以当次执行为准。

仍需保留的边界：

- 手动 `SessionStore::remove` 与保留期清理口径不同，前者不删除会话日志和计划文件，且文件删除错误多为 best-effort。任务四份流程文档、未登记旧计划、孤儿日志不自动回收。
- `index.json.corrupt` 会持续阻止孤儿扫描，目前没有显式恢复入口；应先查明损坏并备份，不能把移除保护标记当常规“修复”。启动幽灵清扫的部分旧路径仍直接使用索引 id，编号防御未完全统一。
- 工具耗时、thinking 耗时、会话级 usage 尚无完整跨重启恢复；在途工具/询问不靠磁盘重建，当前进程询问回跳由运行总览处理。
- 索引仍按最多 2000 条截断；索引条目缺失时 `load_session` 会回退单根、无 project_id，磁盘历史尚存不等于项目归属可完整恢复。命令默认 cwd 实现使用 project_dir 本身，但项目提示中仍有把 temps 描述为默认 cwd 的差异；规则图谱只从主 workspace 读取，不因 allowed_dirs 扩展而自动读取所有外部根。
- 注入队列是 32 格内存通道，未消费消息不能跨进程恢复；UI 队列快照是另一份数据。`tmp/compacted/` 暂无清理者，虚拟滚动/内联折叠回看尚未实现；历史中含义不明的 `display_only` 设想不应视为现行契约。
- 新段格式及 image_blob 不被早期版本识别，回退二进制不等于数据格式可回退。索引写失败可留下已写历史但缺列表项；先保全数据再诊断。
- 手动验收需覆盖多显示器/缩放几何、图片/图表异步撑高后的锚点、长会话前翻、断电恢复、项目改目录与删除影响，以及清理取消/保存并离开/重启路径；离线测试不替代这些检查。
