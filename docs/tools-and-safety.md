# 工具执行与安全边界

> 当前行为指南；源码核对：2026-10-11。本页说明工具执行、文件与命令围栏、网络边界和写后检查。工作流批准见[计划与批准](./plan-and-approval.md)，Office/PDF 见[文档工具](./documents.md)，扩展工具见[MCP 与技能](./mcp-and-skills.md)。

## 1. 工具接线与结果通道

[注册表](../src-tauri/src/tools/registry.rs) 静态注册 23 个内置工具，按名字稳定排序输出 schema。新增工具须登记注册表、严格 schema 与对应契约测试；schema 的对象层使用 `additionalProperties:false`，运行时宽松解码发现未知顶层字段则附 warnings。不要把旧优化方案里未实现的字段当作 API。

| 领域 | 工具 | 当前要点 |
|---|---|---|
| 文本/目录 | read、batch_read、list_files、grep | batch_read 是 read 的兼容别名；大范围调研优先 grep 的 files/count 或子代理 |
| 写入 | create、edit、delete | roots 校验、主会话计划门、档位审批、写认领与相应写锁 |
| 命令/进程 | command、service | 先 fence；command 有界运行，service 管常驻进程 |
| 网络 | web_fetch、http_request | 前者取网页正文，后者请求 API；共享 SSRF 与按 host 节流 |
| 编排 | plan、ask、subagent、wait、suggest、scheduled_task、skill | 各自保留独立生命周期与门禁，不能仅凭 kind 推断是否有副作用 |
| 展示/计算 | render_html、calculate | 沙箱 HTML 展示；无 shell 的表达式计算 |
| Office/PDF | read_document、write_document、edit_document | 独立结构化通道，不能交给 read 输出乱码 |

[ToolKind](../src-tauri/src/tools/tool.rs) 为 ReadOnly / FileWrite / Network / Interactive / Meta。它是调度分类，不是效果证明：command 为 ReadOnly 但可能执行写命令；Network 不是“必然只读”，也没有一个通用 Network 自动审批门。新工具的真实风险要接入对应执行路径，不靠 enum 注释保护。

`ToolOutcome = ok + data + error + warnings + extra_model_content` 有两条消费通道：前端收完整结构化结果，模型经 [compact_for_model](../src-tauri/src/tools/compact.rs) 收文本。

- command 在工具内部裁剪，因此不再二次头尾截断；非零退出仍把 error 与 data.output/exit_code 一起交给模型。
- read/batch_read 保持文本行号与完整所选窗口；图片条目剥去 data_url，禁止把 base64 作为普通文本送模型烧 token。图片经出网副本的 `route_tool_images` 放到紧随的 User 消息，受模型 vision 设置控制，历史不改写。
- 其他成功工具只序列化 data，保留头 4 KiB + 尾 8 KiB。截断后不再是可靠完整 JSON；失败通常只剩错误行。
- warnings **不会自动进入模型文本**。要求模型据此修复的结论必须进 data 或真实 ToolResult content，并在 provider 请求体/模型压缩文本上断言可达。
- 完整 ToolOutcome 的恢复边车只供 UI 恢复，**不等于模型可读取的截断全文文件**。两种保存目的不可混淆，见[会话与项目](./sessions-and-projects.md)。

### 批次调度

[execute_batch](../src-tauri/src/tools/batch.rs) 先做策略/冲突/排除/计划硬门，再执行：FileWrite 在 runtime 的 file_ops 下串行，其余并发上限 4，调用独立 panic 兜底为 `E_TOOL_PANIC`。取消必须收齐每个 tool_use 的结果，未完成调用补 `E_CANCELLED`，不留下悬空配对。

ask/wait 与其他调用同批时整批 `E_BATCH_POLICY`。suggest 混批时只允许 suggest 自己继续，**其余调用被拒绝，并不是让只读工具一起执行**；若包含 ask/wait，仍整批拒绝。suggest 的计划未收尾门仍在 emit 前检查；成功后只通过 `BatchOutcome.suggest_items` 驱动正常收尾，不能旁路发终态。两条 suggest 不 panic，但不应主动构造此形态。当前 suggest 工具描述仍称可与 read/grep/list_files 同批，这与批次执行分支不一致；实际以批次拒绝结果为准，提示文案尚待另行修正。

同批写冲突当前比较规范化路径集合拼接结果，不能夸成已完整覆盖任意两个多文件集合的部分重叠。跨 runtime 同文件的 edit/create/delete/write_document/edit_document 与用户文档还原共用进程级 writelock；并行开发的写认领见[子代理](./subagents.md)。

## 2. 文件边界与文本编辑

[WriteRoots/pathutil](../src-tauri/src/tools/pathutil.rs) 是路径处理入口：工作区主目录快照 + 全局数据目录 + 用户授权 extra_roots。相对写路径基于工作区，显式绝对写路径必须规范化后落在根内；safe_join 拒绝 `..`、控制字符及 Windows 保留名。

- 相对读取先查主根，再按 extra_roots 顺序查首个存在文件；绝对读取走 canonical 边界校验。
- 相对读取选中首个存在候选后，也须与绝对读取一样通过 canonical roots 校验；主根/额外根中的 symlink 指向根外会返回 `E_PATH_OUTSIDE`，指向另一个已授权根则可读。保持主根优先，不因首候选越界而悄悄改读其它同名文件。路径校验与打开之间仍存在外部进程换链窗口，不是 OS 沙箱保证。
- 新增项目外目录是显式用户界面流程，授权读/写/列整组能力；不是任意 read/edit 遇到 E_PATH_OUTSIDE 都会自动弹窗扩根。根目录、家目录本身与应用托管数据目录有广度拒绝，详见[文档与外部引用](./documents.md)。
- FullAccess 不解除文件工具的 roots 校验。shell 的区外新建策略是另一条路径，不能套用到 create/edit。
- delete 先过 roots 与危险删除守卫，拒绝根目录、任一工作区/extra 根自身、这些根的 `.git`、家目录与系统目录。shell 删除黑名单不允许用确认绕过，应该走专用 delete 工具。

### read、list_files、grep

read 一次 1–20 文件，返回原字节生成的 6 字符 version token；默认文本窗口最多 2,000 行，显式 startLine 可缩窗或用负数取尾行。无 startLine 的大于 1 MiB 文本拒绝；这不是流式文件读取，当前先读全文件再截行。主会话另有单调用累计 2,000 行预算，超限整调用 `E_READ_TOO_BROAD`，子代理/任务 runtime 豁免。单文件出错仍会整调用失败，**尚无 per-file 部分成功**；当前没有 2,000 字符的单行裁剪。

图片白名单 png/jpeg/webp/gif，工具读取单张上限 3 MiB；附件入口的上限是另一层，不能相互代替。二进制按文档扩展名和内容 NUL 探测拒绝；带 BOM 的 UTF-16 不能误杀。文本编码走 BOM → 合法 UTF-8 → 有替换符比例护栏的 GBK → unknown，GBK 属推断，不保证 Big5/Shift-JIS 等旧编码正确。

list_files 默认深度 2、上限 8，默认 200 条、上限 2,000；直属单目录预算 50，超出以 `+N more in <dir>` 折叠且不占全局真实条目数。当前没有 glob 参数；有模式需求用 grep 或明确下钻。

grep 内嵌 ripgrep 库，不依赖外部 rg；smart-case、glob 过滤、NUL 二进制停止、硬计数上限 5,000、命中行预览 400 字符。content 按命中分页并附热点文件；files/count 按文件分页。命中到硬上限后的统计不是整仓完整总数；当前遍历仍单线程，不保留旧方案“已并行”的承诺。

### create/edit 的并发、匹配与回滚

create 默认拒绝已有文件，显式 `overwrite:true` 才覆盖。edit 支持多文件多 change，先 read 获取 version，再以唯一 oldText 或 1-based 闭区间 lineRange 指定位置。version 基于**原始字节单次哈希**，不能先归一 EOL 或重复哈希。

[edit](../src-tauri/src/tools/edit/tool.rs) 匹配先统一 LF，写回恢复原 EOL；含任一 CRLF 的混合文件会整体写回 CRLF。精确 oldText 零命中后，依次尝试整块缩进平移、逐行首尾空白归一；每级只接受唯一命中，多命中要求补上下文，绝不猜。newText 按模型输入保留缩进；不引入 Levenshtein、转义归一等激进容错，以免错改。oldText 与 newText 完全相同报错；首行 lineRange 替换保留 UTF-8 BOM。

过期 version 不必一律拒绝：若新内容上仍能唯一应用则警告放行，否则 `E_VERSION_STALE` 要求重读。注意这些模糊/stale warnings 当前仍是 warnings 通道，不能承诺模型必然读到。

[writelock](../src-tauri/src/tools/writelock.rs) 以 canonical path 排序去重获锁，读、预演、备份、写入、回滚都在锁内，锁内重读最新内容以封闭跨 runtime TOCTOU。它与单 runtime file_ops 是两层互补，不能互相替代；外部编辑器不受进程内锁控制。edit 先整组预演再备份、倒序写，失败尝试回滚已写文件，成功清临时备份。不要把它描述为跨文件文件系统原子事务或 Office 的持久版本库。

ConfirmEach 的 edit/create 预览复用实际变更算法生成 unified diff；新建文件显示内容预览，失败退回 JSON。批准前预演与锁内正式执行间可能有外部修改，正式执行仍须重新校验，不能以预览结果直接覆盖文件。

## 3. Shell 选择与命令输出

[command/tool.rs](../src-tauri/src/tools/command/tool.rs) 支持自动探测与用户 shell 选择，实际执行和环境说明共用 resolve_shell/shell_invocation。Windows Git Bash 按固定路径 → 遍历 PATH 中 git.exe 反推安装根 → 含 git 路径的 bash.exe 兜底；反推必须过滤空根，并尝试全部 git 命中，避免首个非目标安装遮挡后续真实安装。相关确定性夹具在[command/tests.rs](../src-tauri/src/tools/command/tests.rs)；网络驱动器、链接安装布局尚需实机验证。

默认 cwd：项目会话为 `<主目录>/.codewave/`（project_dir），临时会话为 workspace；显式 cwd 经读取边界解析。模型需要在代码根跑命令时应明确 cwd。部分环境提示仍称 temps 是默认 cwd，与真实实现有漂移，不能继续扩大为文档约定。

command 的 timeoutSeconds 默认 120、范围 1–600 秒；`fullOutput` 在 schema 中 required，旧载荷省略按 false。false 给模型末 2,000 字符，true 给末 16,000 字符，**true 也不是无限全文**。输出收集超过 96 KiB 则 spill；未达该阈但超模型预算也补写，做到 command 的“凡截断必落盘”。signal line 包含退出状态、总行数和完整输出路径。临时 spill 24 小时后惰性清理，每小时最多扫一次；不是持久档案。

stdout/stderr 各持一个 [OutputSanitizer](../src-tauri/src/tools/sanitize.rs)，跨块保留 UTF-8 尾部，去 CSI/OSC/两字符转义、裸回车与其他控制字符。非法字节仍容错；裸 `\r` 只丢弃，不模拟终端原地重画，因此进度条可能串接。不能为此全局强设 NO_COLOR/FORCE_COLOR，避免改变用户命令环境。service 环日志复用清洗器。

工具开始与结果须共享真实 call_key/名称。等待审批显示 waiting，真正执行显示 running；空输出工具也必须可见。命令首个输出帧不等 2 KiB，后续按字节与时间节流；结果落定时主/子通道都清 progressTail。取消/超时执行进程树终止并收尾，不先报取消再遗留进程；具体跨平台验证见[运行时与调度](./runtime-and-scheduling.md)。

## 4. fence：静态分析，不是 OS 沙箱

唯一判定入口：[check_command_policy](../src-tauri/src/safety/fence/check.rs)。结果为 Allow / Confirm(reason) / Block(code,message)，不能把 Allow 当成用户授权或“命令绝无副作用”的证明。

当前分析顺序：Plan L0 名单 → 无错误的 Bash AST，否则尝试 PowerShell AST → 对字面量掩码后做 L3 → AST 命令/重定向遍历做 L1/L2 → 下载与解释器关联。双解析失败才走词法 fallback；Plan 出口统一将 Confirm 改成 `E_PLAN_READONLY`，文案包含折叠/截断至 120 字符的命令摘要与改道说明。

### 判定规则与维护理由

| 风险面 | 当前规则 |
|---|---|
| 删除 | rm/rmdir/unlink、del/erase/rd、Remove-Item/ri、shred 等 L1 拒绝；find 删除与 xargs 删除也要覆盖 |
| 显式写 | WRITE_ALL 全位置参数，WRITE_LAST 末目标，PowerShell WRITE_FIRST 按全部非 flag 参数保守检查；重定向同样走目标校验 |
| 隐式写 | gzip/bzip2/xz/zstd/lz4 等压缩族默认 Confirm；明确 stdout/测试等只读 flag 才豁免，keep/recursive 仍有写；sed -i、tar 解包、unzip 解包、cpio copy-in 同样检查 |
| 高危 | 进程/服务/持久化、ssh/scp/sftp 远端行为、任意代码解释、下载执行管道、危险权限、强推等 Confirm；`--force-with-lease` 不误判为 `--force` |
| 灾难 | mkfs/dd 写设备、磁盘分区、电源、/etc 写入等；FullAccess 在消费点仍拒绝，其他可审批档须亲自回答，永不 auto-confirm/白名单放行 |
| 已识别的不确定性 | 透传前缀无法判定的 flag-值、变量/特定展开写目标、未识别 WSL 入口等不得静默 Allow，升级 Confirm，Plan 下变 Block |

命令名反混淆覆盖引号拼接、转义、默认 IFS 近似；不能只在原始源码上做字面子串。字面量掩码用于避免 `echo "mkfs"`、提交说明等散文误报；**命令名/真实写目标必须另做结构化判定**，否则给命令名或 /etc 目标加引号就会绕过掩码扫描。PowerShell 路径仍保留 L1 词法网；下载与解释器关联也要覆盖 AST 短路分支。

shell/sh/powershell、cmd 与 WSL 内层按可识别形态递归检查，深度有界；不模拟任意解释器、awk/make 脚本或远端 shell。新增白名单须审计三类能力：输入/参数可子执行、隐藏参数写、环境变量可选择执行体。不能为了方便把 npm/pnpm/cargo 这类泛化名称整体放入只读名单。

### 写目标四象限

目标先挂在 canonical cwd，再做词法归一与真实落点比较，Windows verbatim 前缀和 macOS `/var → /private/var` 必须在同一命名空间比较：

- 词法在根内、canonical 在根外：链接逃逸 Block。
- 两者在根内：继续扫描中间链接；悬空或无法证明落根内的链接 Block，再按档位 Allow/InsideWrite。
- 词法在根外、canonical 回到根内：按真实根内落点。
- 两者在根外：已存在目标 Block；新目标按 confirm_outside_create 确认/允许。`../` 到不存在根外目标属于此类，不假称符号链接逃逸。

`/dev/null` 与 PowerShell `$null` 是输出丢弃例外；变量展开目标不能借这条例外通行。**这是已识别写目标的静态规则，不等于捕获任意进程实际写文件。**

### Plan 的 gh 子命令门

gh 不能仅按名字放行，因为远程写不一定命中文件/高危规则。允许表集中于 PR/issue/repo/run/release 等只读 view/list/checks/diff/status 与限定查询；裸 gh、未知全局参数形态保守拒绝。gh api 无写参数可 GET，显式方法仅 GET/HEAD；写方法、请求体字段、未知变量方法均不放行，带 `-f/-F/--field/--raw-field/--input` 即使声称 GET 也保守拒绝。

引号与紧凑方法 flag 须归一；命令切段包括换行，并先按引号语义处理反斜杠续行，防第二条写命令藏在第一条只读命令后。PowerShell 反引号续行仍有识别边界。

### 不能隐去的残留

- **双 AST 失败不是 fail-closed**：fallback_scan 未命中词法危险规则仍返回 Allow。已识别动态参数会升级，不代表所有未知 shell 形态都会拒绝。
- L0 仍按顶层段首词：`echo $(gh pr merge …)` 等内层远端写没有同样的 gh 白名单门；git 仍是整名字白名单，普通 push、建分支等不在完整子命令写分类中。
- awk system/输出语句、make recipe、嵌入 here-string/heredoc 脚本、运行时 IFS、深层宿主嵌套与各 shell 方言不构成完备分析。
- WRITE_LAST 不完整刻画多目标 chmod/ln 等；下载组合短 flag 内嵌 `-o`、sort -o、date 设置时钟等参数能力仍须独立审计。包管理器安装脚本没有通用文件落点探测，不能声称由四象限兜底覆盖全部写入。
- 命令摘要/日志可能含凭据，当前没有统一参数脱敏；敏感命令不应靠“摘要短”假定安全。

以上是开放边界，不是鼓励调用方绕过。改进时要先给出反例与判别力测试，再调整分析器，不能只追加宽松白名单。

### 宿主内部状态写保护

[protected_state.rs](../src-tauri/src/core/protected_state.rs) 区分普通产物与宿主维护的事务状态。作用域来自 `WriteRoots` 中的全局 `data_dir` 和会话快照 `project_dir`，不因为某个目录名为 `.codewave` 就将它当成宿主目录；`extra_roots` 不能解除内部状态保护。

全局保护包括配置与凭据提交 journal、MCP 配置、运行标记、界面状态及其已定义备份、项目发现索引，以及 sessions/histories/stats/locks/instances/trust/credential-leases 状态树。项目保护包括 `project.json`、`mcp.json`、内部协调树和 `tasks/` 直属的合法任务 id `.json` 文件（scheduler 状态命名空间）；任务计划/报告 `.md` 和子目录中的普通产物仍可写。memory、skills、lessons、temps、logs 不因位于托管目录就整树封锁。变更宿主状态须走对应设置、计划任务或会话业务入口。

普通文件写路径统一经 `resolve_write`，创建、编辑、删除、Office 写入/保真修改/备份还原在适用锁内及落盘前复验。检查覆盖目标、受保护后代及可替换/删除状态的目录祖先，同时检查词法入口和链接落点，缺失目标也不能通过链接绕过创建保护；普通读取边界保持原规则。命中返回 `E_INTERNAL_STATE`，完全访问档和额外目录授权均不能豁免。

命令围栏在普通 L0/L3 确认早退之前扫描已识别的内部写目标，不能让 FullAccess、命令白名单或用户配置的写后检查把它降为可确认操作。扫描覆盖输出重定向、可识别复制/移动目标及移动源、命名输出参数和部分原地修改形态；字面量目录切换使用有界的可能目录集合，保守检查条件分支，子 shell 不向调用方传播 cwd。command/service 在审批等待及 shell 探测后、启动前再次复验，service 被拒时不登记进程。

POSIX shell 的 `-c`（含 `-lc` 等组合）只扫描首个脚本，后续 argv 仍是数据；PS/CMD 的多参数命令同时扫描脚本片段拼接与保留参数引用的投影，避免含空格路径或“完整脚本 + 后续参数”丢失写目标。PowerShell 字符串 `-Command` 后的参数都属于待执行命令，见 [Microsoft 参数说明](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_pwsh?view=powershell-7.5)。这仍是保守静态投影，不模拟全部 shell 执行语义。

这不是 OS 沙箱：任意解释器、动态展开、脚本文件、MCP 或不合作的外部进程仍可能有静态分析无法识别的副作用，最终路径检查之后也存在外部换链窗口。明确 `ln` / `cp --link` 等创建受保护源别名的形态会被拒绝，但 canonical 路径不识别外部预先创建的硬链接 inode 别名。macOS 与 Windows 对保留命名空间保守折叠大小写，macOS 大小写敏感卷上的同名变体可能因此被多拦。此保护本身不构成持久授权的真实性证明；持久信任仍需独立权威存储、版本绑定、真实用户确认及撤销事务，不能将普通磁盘 JSON 的 `trusted` 字段当作许可。

## 5. 网络：逐跳 SSRF 的范围

[net.rs](../src-tauri/src/tools/net.rs) 及其[统一请求执行器](../src-tauri/src/tools/net/request.rs)只用于 web_fetch/http_request；Provider 的用户配置端点与 MCP 网络栈**不自动继承它**。默认拒私网/回环/链路本地/未指定/CGNAT 等已列地址，IPv4-mapped IPv6 先归一；域名解析后任一结果为私网则拒绝。allow_private_network 是显式设置放宽，不是用户在消息中写 localhost 即自动开放。

两个工具仅允许 HTTP(S)，拒绝 URL 内嵌凭据，关闭自动重定向，最多跟随 10 跳；要求本地解析的路由每跳重新解析并校验全部目标地址，以该地址集合构造本跳客户端，保持原域名用于 Host/TLS 校验，不能二次解析成其它连接目标。允许内网时，远端 DNS 代理保持代理解析，不先要求本地 DNS 成功。每 host 最少 1 秒节流。两工具的 `timeoutSeconds` 默认 60、范围 1–120，统一总预算覆盖代理探测、DNS、节流、全部跳转和正文读取；取消优先，取消返回 `E_CANCELLED`，超时返回 `E_NETWORK`。macOS 显式系统代理探测为可取消子进程；默认原生系统探测及其它平台探测在阻塞工作线程执行，请求取消后原生查询可能继续结束。这不等于批次层已有任意工具的统一总时限，也不包括正文 Readability 的同步 CPU 处理。

本地 DNS SOCKS4/5 的连接器只使用首个解析地址，因此执行器按已校验顺序逐个尝试候选；仅连接阶段错误（包括TLS握手失败）才推进下一地址。每次仍用同一代理快照和原URL/域名，不重新解析、不改IPv4优先、不静默直连。所有候选共享上述总预算/取消并逐次节流。HTTP请求已发出后的断流、状态码失败或正文读取错误均不触发地址重放，避免重复POST副作用。真实SOCKS协议、TLS握手及不重放回归见[socks_tests.rs](../src-tauri/src/tools/net/socks_tests.rs)；这不是对任意请求失败的通用重试。

两工具均使用 `read_body_limited` 对**解压后的流式正文**实施 50 MiB 硬上限，无 Content-Length/分块响应同样受限；声明长度超限则提前拒绝。web_fetch 的 HTML 用 Readability，失败退化剥标签，默认 maxChars=60,000、范围 1,000–500,000，目前不支持 markdown 格式参数或图片内容通道；新增 `final_url` 标明实际取回地址，原 `url` 保留用户输入。

http_request 的方法白名单为 GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS；JSON 预览 24 Ki 字符、文本 96 Ki 字符，二进制只报体积和 MIME；响应头只对 set-cookie/www-authenticate 做脱敏。body 是 JSON 值，字符串也按 JSON 字符串发送；query 的名称和值均编码。Host、Content-Length、Transfer-Encoding 和 Proxy-Authorization 不允许由 caller 头覆盖。

重定向与代理契约：

1. 只跟随 301/302/303/307/308；301/302 仅 POST 转 GET，303 除 HEAD 外转 GET，转换后清除 body 和实体头；307/308 同源保留方法与 body。跨源（含端口变化）清空**全部 caller 自定义头**，避免漏掉自定义凭据；若仍需重放 body，返回 `E_REDIRECT_UNSAFE`。HTTPS 降级到 HTTP 同样拒绝，已剥头不会在后续跳回原源时恢复。
2. 每次工具执行使用与 reqwest 同源的 hyper-util matcher 固化系统/环境/NO_PROXY 路由，显式代理保留原决议及回环直连例外。显式地址先校验协议和主机，统一 IDNA 与 SOCKS 缺省端口；不受支持的配置明确报错，不能把 matcher 丢弃配置误认作直连。客户端只使用该快照，构建失败不回退绕开代理的直连。
3. 直连、本地 DNS SOCKS4/5 使用已校验地址。HTTP/HTTPS CONNECT、SOCKS4a/5h 会在代理端解析域名，不能由 `resolve_to_addrs` 证明实际目标；禁止内网时，对这类代理的域名目标明确返回 `E_SSRF_PROXY_UNSUPPORTED`，不发送、不静默直连。可改用本地 DNS SOCKS，或由用户明确开启 allow_private_network 放宽策略。IP 字面量没有目标 DNS 二次解析。该策略不宣称约束恶意代理或底层网络路由，真实代理联调仍需单独验收。
4. Network kind 没有统一触网审批，Plan 也未排除 http_request；所以“计划档只读”不能扩展成对任意网络写的完整技术保证。端点访问和授权是不同问题。

## 6. 写入后检查

当前没有常驻 LSP、服务器安装/重探、lsp_* IPC 或写前诊断基线。用工程级显式命令取代单文件编译与 LSP：避免脱离项目上下文误报、冷启动未就绪，以及旧 warnings 通道“UI 看见、模型看不见”。

配置见 [PostWriteCheckSettings](../src-tauri/src/core/config.rs)：全局 `post_write_check`，enabled 默认 false、command 空、timeout_seconds=30、tail_chars=3000。不是每语言/每项目单独命令。接线仅 create/edit 成功之后；command、Office 工具、外部编辑器保存不自动触发。

[validation::run](../src-tauri/src/tools/validation.rs) 的顺序不能改错：

| 条件 | 行为 |
|---|---|
| 命令关闭或空，目标是 JSON | 内置 JSON 解析，无需项目 |
| 关闭且非 JSON | skipped=disabled |
| 开启但命令空，非 JSON | skipped=empty-command |
| 开启且非空，临时会话 | 所有文件 no-project，**JSON 也不再回落解析** |
| 项目命令含 `{file}` | 每个被写文件串行执行 |
| 项目命令不含 `{file}` | 整个工具调用只执行一次，edit 结果 path=null |

命令运行在工作区根，即项目代码主目录，**不是 command 默认的托管目录**。`{file}` 按当前 shell 引用，用户无需再加引号；当前替换值沿用工具入参展示路径，绝对输入不会自动变成相对路径。用户配置命令在该执行器里视为已授权：fence Block 才拒绝，Confirm 不再逐次弹卡。这包括检查命令的潜在 `--fix/--write` 副作用，名称“检查”不构成只读约束，也不能套用 command 的所有 Confirm 消费规则。

结果必须放在 create 的 `data.check` 或 edit 的 `data.checks=[{path,check}]`，check 字段为 ran/ok/command/output/skipped。未跑、未完成、失败、通过必须分别呈现：

- fence Block：ran=false、blocked-by-fence；启动失败 ran=false、ok=false、skipped=null，output 解释。
- 非零退出：ran=true、ok=false；超时/取消已启动则 ran=true、ok=false，skipped=timeout/cancelled，保留尾部。
- 只有实际完成且成功才能说通过；skipped=null 自身不是成功证明。
- 文件写入已成功，即使检查失败，外层 ToolOutcome.ok 仍为 true，不回滚、不伪装成写入失败，否则通用失败压缩会丢 data。
- stdout/stderr 并发收集到滚动 1 MiB 尾部，再取 tail_chars；不保证两流严格时间顺序。普通工具结果仍有整体头尾预算，不能保证所有多文件检查输出都完整送到模型。
- Unix 进程组、Windows 隐藏启动，超时/取消终止进程树并等待输出泵收尾。平台保证以实际覆盖为准，Unix 孤儿进程测试不能代替 Windows 验收。

回归入口：[postcheck.rs](../src-tauri/src/tools/postcheck.rs)、[validation.rs](../src-tauri/src/tools/validation.rs)、[create.rs](../src-tauri/src/tools/create.rs)、[edit/tests.rs](../src-tauri/src/tools/edit/tests.rs)。必须断言最终 compact_for_model 文本含检查结论，不只断言 warnings。人工可用一次性项目的 `echo checked:{file}` 与 `echo check-failed; exit 1` 验证；实际 lint/typecheck 命令须先在目标工程根测试，不提供未经验证的万能命令。

## 7. 后续工作如何取舍

已落地的低风险编辑容错、锁、diff 预览、二进制拒绝、command spill 与清洗应继续守护。仍未交付的旧方案集中保留为候选：read 局部错误/超长单行保护、通用模型截断全文可取回、list_files glob、动态工具描述/workdir 别名、web_fetch markdown/图像、技能附属文件清单、独立 web_search、后台子代理续接、按模型裁剪工具。只有获得新需求与验收标准后再做，不能把整张旧排期表当现有承诺。

不另造 invalid/plan_exit 工具，已有错误结果与 ask 批准协议覆盖；不把所有工具强行塞进同一截断层，保留语义化双通道；不恢复已删除的 LSP 架构或引入激进模糊替换。shell 判定表、SSRF、模式门与真实 provider wire 的测试要共同覆盖，单层单测全绿不足以证明安全或模型可见性。通用门禁见[测试指南](./testing.md)。
