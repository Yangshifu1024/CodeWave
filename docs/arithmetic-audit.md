# 全仓算术/计算点审查清单（budget_notice 同类缺陷排查）

> 类型：代码审查 · 触发：[docs/budget-notice-step-fix](./budget-notice-step-fix.md) `budget_notice` 方向反转缺陷（升序计数器当剩余量用）同类排查 · 方法：三路并行审查（core / provider+tools+safety+host+git / 前端）共 ~180 处计算点，全部可疑项经人工逐一复核 · 基线：[docs/budget-notice-step-fix](./budget-notice-step-fix.md) 修复后工作区（行号以此为据，复核时以符号名为准）
>
> **结论：未发现第二例「方向反转」级缺陷。14 处存疑：1 处仓库自证确认缺陷、1 处用户可触发 panic、其余为边界/潜在/展示类。**

## 一、复核结论速览

| # | 级别 | 位置 | 问题 |
|---|---|---|---|
| 1 | 🔴 | `tools/list_files.rs:226` | `pi += off + 1` 按 1 **字节**推进，多字节字符命中后 `lp[pi..]` 切在非字符边界 → **panic**；中文 @-提及查询即可触发（用户可见） |
| 2 | 🔴 | `core/stats.rs:175-180` | `flush` 的 by_workspace 合并漏并 `cache_read`/`cache_write`（by_model/by_kind/total 均并全五字段）→ 同日重启后工作区 cache 统计**永久清零**；仓库自带 `#[ignore]` 探针测试标注 "Latent bug" |
| 3 | 🟡 | `provider/retry.rs:18` | `attempt > MAX_RETRIES`（attempt 从 0 计已完成重试次数）→ 实际 **7 次**重试，与模块文档 "at most 6 retries" 差一；测试把现行为钉死 |
| 4 | 🟡 | `core/scheduler.rs:50-52,80,197-206` | `every:<n>` 的 `n*86400` 无上界（debug panic/release 回绕）；且 tick 中 `next_after → None` 一律 `tasks.remove` → 溢出或 cron 串损坏时**静默删除循环任务**而非报错 |
| 5 | 🟡 | `core/context.rs:22` | `SUMMARY_MAX_TOKENS = 8000` 声明后**从未使用**——压缩摘要请求实际无输出上限 |
| 6 | 🟡 | `core/sessions/repair.rs:206-208` | `trim` 在 `keep_last == 0` 且仅 1 轮超预算时 `starts[1]` 越界 panic；当前调用方均传 2（潜在），`pub fn` 前置条件未设防 |
| 7 | 🟡 | `tools/command.rs:86` | `probe_bash_login` 注释称 "5s budget"，实际 `Command::output()` **无任何超时**（unix；shell profile 卡死则永久阻塞探测线程） |
| 8 | 🟡 | `tools/service.rs:298` | `child.id().unwrap_or(1)` 兜底值 1：unix 下经 `libc::kill(-pid)` 变成 `kill(-1)` **杀全用户进程**；当前 spawn→取 id 同步路径实际不可达，属注释背书的危险兜底 |
| 9 | 🟢 | `tools/edit.rs:66-79` | `min_indent` 按**字节**、`strip_indent` 按**字符**跳过：多字节空白缩进（U+00A0/U+3000）时过度剥离 → 模糊匹配漏匹配（仅误报安全失败，不误替换） |
| 10 | 🟢 | `tools/subagent.rs:138-145` | 并发上限 check-then-act（load→check→fetch_add，非 CAS）：瞬时可超 MAX_CONCURRENT=4，Drop guard 自愈 |
| 11 | 🟢 | `ui/features/panels/TokenStatsModal.tsx:27` | 「最常用模型」口径 `input+output`，柱状图/总计口径 `input+output+cache_read`——同一弹窗两套基数对不上账 |
| 12 | 🟢 | `ui/features/subagent/SubagentDrawer.tsx:22,53-55` | 宽度在 open 翻转后的 effect 里写 ref，不触发重渲染 → 折叠期间改过窗口尺寸时抽屉以**过期宽度**打开（直到下次流式 tick） |
| 13 | 🟢 | `ui/features/shell/AppShell.tsx:239` + `theme/app.css:9` | `calc(100% - 50px)` 与 `.toolbar height: 50px` 双写，改一处必漏另一处（当前一致，纯漂移风险） |
| 14 | 🟢 | `ui/features/chat/Composer.tsx:72` | 历史召回注释称 "0 = newest, counted from the tail backwards"，实现按 hist 升序索引（0 = 最旧）——代码自洽，注释过期 |

## 二、逐项证据与修复建议

### 🔴1 list_files.rs:226 — 字节/字符推进错位（panic）

```rust
for (qi, qc) in chars.iter().enumerate() {
    match lp[pi..].find(*qc) {          // lp[pi..] 若 pi 非字符边界即 panic
        Some(off) => { … pi += off + 1; } // 多字节 qc 命中后 off+1 落在该字符中间
```

`find(char)` 返回字节偏移；`qc` 为 CJK 等多字节字符时 `off + 1` 指向该字符第 2 字节，下一轮 `lp[pi..]` 直接 panic（byte index is not a char boundary）。触发路径：`@` 提及的路径模糊评分（host/commands.rs 的 list 工作区路径查询），查询含任意命中的非 ASCII 字符即崩。**修复**：`pi += off + qc.len_utf8();`。（初版审查曾疑 `q` 未小写化，复核确认上游 fuzzy_filter 入口已 `query.to_lowercase()`，大小写匹配本就正确，未作改动。）

### 🔴2 stats.rs:175-180 — by_workspace 合并丢 cache 字段（数据丢失）

by_model（167-174）与 by_kind（181-188）合并全五字段、total（189-193）合并全五字段，唯独 by_workspace（175-180）只并 `input/output/runs`。同日多次启动合并旧文件时，工作区 `cache_read/cache_write` 被丢弃。仓库自带证据：`stats.rs:343` `#[ignore] fn flush_should_merge_by_workspace_cache_fields`，注释即 "Latent bug"。**修复**：补两行 `w.cache_read += v.cache_read; w.cache_write += v.cache_write;` 并去掉 `#[ignore]` 让探针转正。

### 🟡3 retry.rs:18 — 重试次数与文档差一

`should_retry(err, attempt)` 在 `attempt > MAX_RETRIES(6)` 才拒绝；调用点（agent.rs:702/892-894/962-965）`attempt` 从 0 起计「已完成重试数」、先检查后自增 → attempt ∈ {0..6} 共放行 **7 次**重试（8 个请求）。模块头注释 "at most 6 retries"。**修复**：`>= MAX_RETRIES`，并把 retry.rs:59-62 的测试断言同步翻转。影响：每个失败 turn 多烧一次退避（最长 10s）。

### 🟡4 scheduler.rs — every 间隔溢出 + None 即静默删任务

- `parse_schedule`（50-52）：`n: u64` 仅校验非 0，`n * 86400` 无上界（debug panic / release 回绕）。
- `next_after`（80）：`chrono::Duration::from_std(*d).ok()?` 对超大时长返回 None。
- `tick`（197-206）：`Some(n) => 更新 next_run；None => tasks.remove(&id)`——注释「once: remove after firing」但 None 同样来自 **Every 溢出**与 **cron 表达式解析失败**（`from_str.ok()?`），循环任务被静默删除。**修复**：parse 期给 `secs` 设上界（如 ≤ 30 天）直接报错；tick 中对 `Every`/`Cron` 的 None 走 warn + 跳过而非 remove，仅 `Once` 删除。

### 🟡5 context.rs:22 — SUMMARY_MAX_TOKENS 死常量

全仓仅声明处一个引用点。压缩摘要请求未带 max_tokens 上限，声明意图（8k 封顶）从未生效。**修复**：在 compact 请求体接入，或删除常量。

### 🟡6 repair.rs:206-208 — trim 在 keep_last=0 时越界

`starts.len() <= keep_last` 的早退与 `while starts.len() > keep_last` 的循环条件之间，`starts[1]` 需要 `starts.len() ≥ 2`；`keep_last=0` 且仅 1 轮时两条件都放行 → `starts[1]` panic。当前三处调用均传 `keep_last=2`（sessions/mod.rs:227/270/292），潜在。**修复**：循环内 `let cut_end = starts.get(1).copied().unwrap_or(msgs.len());`。

### 🟡7 command.rs:86 — bash 登录探针无超时

注释 "probe once with a 5s budget"，代码 `Command::new("bash").arg("-lc")…output()` 是无超时阻塞调用，且包在 `OnceLock` 惰性初始化里——首次 shell 探测时 profile 卡死（网络挂载的 rc 文件等）会永久卡住该线程。unix-only。**修复**：按注释实现 5s 超时（`wait_timeout` 或 spawn + 轮询 kill）。

### 🟡8 service.rs:298 — kill(-1) 危险兜底

`child.id().unwrap_or(1)` 兜底注释只规避了 0（kill(-0) 自伤进程组），却选了更危险的 1：unix 下 `libc::kill(-1, SIG)` 波及当前用户全部进程。spawn→298 行之间无 await，tokio 尚未 reap，`id()` 实际恒为 Some，故当前不可达；但这是靠「巧合的调度时序」而非代码保证。**修复**：`match child.id() { Some(p) => p, None => return 拒绝/直接完成 }`。

### 🟢9-14（轻微）

- **edit.rs**：`min_indent` 计 `l.len() - l.trim_start().len()`（字节），`strip_indent` 用 `chars().skip(n)`。仅当缩进含多字节空白时触发，后果是模糊匹配层漏匹配（唯一命中才替换的安全哲学下，退化为报错不误替）。修复：min_indent 改 `l.chars().count() - l.trim_start().chars().count()`。
- **subagent.rs**：`ACTIVE.load → check → fetch_add` 非原子 CAS，两个同时 spawn 可瞬时 5 并发，Drop guard 自愈、仅 fail-fast 语义略漏。修复：`compare_exchange` 循环或 `fetch_add` 后回滚。
- **TokenStatsModal**：L27/39 用 `input+output`，L57-58/76/89 用 `input+output+cache_read`。要么统一含 cache，要么总计拆注 cache。
- **SubagentDrawer**：`useEffect(() => { if (open) width.current = drawerWidth() }, [open])` 写 ref 不重渲染。修复：改为 `key={open}` 重挂或用 state 存宽度。
- **AppShell/app.css**：50px 双写。修复：CSS 变量 `--ws-titlebar-h` 单源。
- **Composer**：L72 注释与实现的索引方向不符，改注释即可。

## 三、全量清单（OK 项，供逐条复核）

### core 层（44 处，含 5 处存疑已入上表）

- `agent.rs:818/824/1017` budget_notice_step、force_report（`step == max_steps-1`）、last_step（`step+1 == max_steps`）：三处代数等价，步进恒 1，`==` 不会被跳过 ✅
- `agent.rs:1071` `step % 20 == 19` 每 20 步 checkpoint；步进 1 无跳过 ✅
- `agent.rs:729` 注入缓冲 `injected >= 32` break：恰好 32 条封顶，余量留队列 ✅
- `agent.rs:749-750` `compact_threshold.clamp(0.05, 0.95)` + `ratio > threshold`：floor<ceiling 无反转 ✅
- `agent.rs:756/768` `clamp(30,3600)` 秒、`*1000` 转 ms：单位/无溢出 ✅
- `agent.rs:750/797` 压缩失败熔断 `<2` / `>=2` / 成功归零：门限自洽 ✅
- `agent.rs:1122/1186` 退避 `attempt.saturating_sub(1)`：调用点先自增再睡眠，首退避 500ms，事件 delay 与实睡一致 ✅
- `agent.rs:609` `input+output > 0` 跳过零用量统计：u64 无溢出之虞 ✅
- `agent.rs:1370/1393-1433` 装配层 `len()-1`、`ord→ci` 映射：push 前取索引、map 来源封闭无越界 ✅
- `agent.rs:383/512` 标题 10 字符、日志 120 字符截断：`chars()` 非 bytes，CJK 安全 ✅
- `agent.rs:26-33` MAX_STEPS=9999 / INJECT_BUFFER=32 / 64ms 节流 / CHECKPOINT=20：常量与用法一致 ✅
- `context.rs:36/43-46/99/107/180/227` 30s 缓存、`window.max(1)` 防除零、total 三段和（tool_results 不重复计，测试钉死）、ratio 分母=窗口、6000 字符截断、用量为 0 走估算：全部 ✅
- `scheduler.rs:15` TASK_BUDGET_STEPS=30 → notice at 24、force_report at 29，与 [docs/budget-notice-step-fix](./budget-notice-step-fix.md) 测试一致 ✅；`scheduler.rs:81/163` once 边界（严格 `>`、恰好 now 的恢复）✅；`scheduler.rs:230` 15s tick ✅
- `sessions/mod.rs:121-124` 索引 LRU 2000（desc + truncate）✅；`TRIM_BUDGET_TOKENS` tokens 对 trim、`8MiB` 字节对 gzip len：单位各归其位 ✅
- `sessions/repair.rs:194-212` trim 主体：递归重算边界、轮配对保持、终止有保证（keep_last=0 例外见存疑#6）✅
- `util/token_est.rs:14-16` ascii/4 + cjk×3/5 + 10% 余量：与注释一致，saturating 防下溢 ✅
- `stats.rs:104/121/147/227/205-218` 首次+每百条 warn、flush 余量 saturating、512 条触发、查询窗口 min(90)、90 天保留边界：全部 ✅
- `prefs.rs:82-89` effort→ratio 0.2/0.4/0.6/0.8 单调、Max>High 无颠倒 ✅
- `anthropic.rs:70-76` **重点核对项**：`max_tokens <= 1025 → None` 守卫保证 clamp 地板 1024 < 天花板 max_tokens-1 永不反转；pct ≤ 0.8×max_tokens 恒小于天花板（[docs/composer-toolbar-batch-report](./composer-toolbar-batch-report.md) 测试钉死）✅
- `retry.rs:10-13` 500ms×2^n（`min(16)` 防 pow 溢出）cap 10s ✅（次数边界见存疑#3）
- `title.rs` 20/2000 字符、20s 超时、用量回退 input/output 无错位 ✅
- `projects.rs:30-33` 尾斜杠剥离 `len() > 1` 防空 ✅
- `config.rs` 数值默认（32768/128k/0.6/180s/15.0/v2）与下游 clamp 一致 ✅；`config.rs:455-459` key 掩码字符边界回退 ✅
- `memory.rs` 200 条上限 + 80 字符描述 ✅
- `tools/compact.rs:40-47` head+tail 截断 guard 防切片越界、报告字节数精确 ✅
- `core/logging.rs:165-181` `clamp(1,2000)`、saturating_sub、切片有 guard ✅；`session_log.rs:90-104` trunc 按字符边界 ✅；`prompt.rs:177` 1-based 编号 ✅
- `tools/subagent.rs:121` `max_steps.clamp(1,1000)`：clamp 参数序正确，25 → notice at 20 / report at 24 ✅
- `skills/mod.rs:241/263` TTL `elapsed() < ttl` ✅

### provider / tools / safety / host / git 层（~88 处，含 5 处存疑已入上表）

- `anthropic.rs:71/74-75` + `prefs.rs`（同上，clamp 家族重点核对通过）✅
- `retry.rs:10-13` 退避曲线（attempt 0→500ms 测试钉死）✅
- `keys.rs:99-101` 冷却 10s×2^(n-1) cap 30min：saturating_pow + min，n=1→10s ✅；`keys.rs:66` `cool_until <= now` 恢复边界（到点即可用，方向对）✅；`keys.rs:71-75` min_by_key 选最早恢复 ✅
- `sse.rs:28-29` `drain(..=pos)` + `len-1` 剥换行；多字节 UTF-8 续字节 ≥0x80 不可能与 `\n` 混淆，分片撕裂安全 ✅；`sse.rs:54-59` data 行 LF 拼接符合 WHATWG 规则 ✅；`sse.rs:38-45` finish 收尾 lossy 一次 flush ✅
- `dto.rs:133` 错误摘要 500 字符 ✅；`dto.rs:181-187` Tool 块中性判空（文档化意图）✅
- `openai_chat.rs:201-237` 规范序号 `canonical.len()`、raw_slot 映射、ended 去重（GLM 全零 index 场景有测试）✅；`openai_responses.rs:161-163` next_index 单调 ✅
- `proxy.rs` 10s 连接超时 + 限重定向 10：常量 ✅
- `command.rs:251-255/361/384-395/489-493` `clamp(1,600)`、96KB spill、2048B/200ms 双节流、`total_lines` 末行无换行 +1 修正（防空输出）✅；`command.rs:389-395/497-504/540-556` tail 按 chars、`byte_budget = want*4+8`（UTF-8 最坏 4 字节/字符 + 边界余量）seek 读尾：单位/方向全对 ✅；`command.rs:560-598` 24h 清理 + 1h CAS 节流、时钟回拨 saturating ✅
- `list_files.rs:75-76/102-134/154-181` limit/depth cap、恰好 limit 截断、每目录 50 预算 +N more 不占全局配额、50000 全局 + 600s TTL：全部 ✅（评分函数见存疑#1）；`list_files.rs:210` rsplit('/') 在 Windows `\` 路径上惰性（仅降权不崩溃）✅
- `grep.rs:92/162-178/204-256` cap 200/硬顶 5000（fetch_add 前检查）、400 字符行截断、分页 `truncated = total > offset + page.len()`（offset≥total 亦对）：全部 ✅
- `read.rs:88-102` **重点核对项**：1-based 闭区间 `[s,e]` → 0-based 切片 `[s-1..e]`、标签 `s+i`，转换正确；`read.rs:222-227` 负 start 末 N 行精确（i64::MIN 仅 debug panic，理论边角）；`read.rs:43-78` UTF-16 启发式与成对解码无越界；`read.rs:179-199` base64 容量 `div_ceil(3)*4`、6-bit 打包、padding 精确 ✅
- `edit.rs:45-53/97-119/184-215` 行区间校验（拒 0/倒序）、`0..=(len-n)` 窗口有前置 guard、替换 `[..s-1] + new + [e..]` 与 1-based 闭区间精确对应（见存疑#9 的缩进单位小疵）✅
- `http_request.rs:92/102-142/162/202-209` `clamp(1,120)`、`0..=10` 恰 1 主 + 10 跳、50MB 预检、clip 按字符（星形平面有测试）✅
- `net.rs:83-105` 先查后 extend 精确封顶 ✅；`net.rs:29-33` CGNAT (64..=127) 闭区间、v6 fc00::/7 与 fe80::/10 掩码正确（边界测试覆盖 63/64/127/128）✅；`net.rs:52-74` HostThrottle 无忙等/下溢 ✅
- `web_fetch.rs:49-105/163-221` `0..=MAX_REDIRECTS` 与错误文案「超过 10 次」一致、`clamp(1_000,500_000)`、take/count/reported 三处全 chars ✅
- `calculate.rs` tokenizer 边界、除零/模零（含 -0.0）、is_finite 防溢出、一元负号与 `^` 结合性文档化、错误参数序号 `i+1` ✅
- `fuzz.rs:23-43` 变异循环 `% out.len().max(1)`、`pos.min(len-1)` 有非空前置 ✅
- `render_html.rs:46-52` 50000 字符精确边界（50000 过 / 50001 拒）✅
- `create.rs:108-109` 预览 2000 字符、bytes 字段按字节标 ✅；`suggest.rs/plan.rs/validation.rs` 4/80/100/6/2048 各 cap 与注释一致 ✅
- `service.rs:42-56/74-79/96/124-129/333/426` 字节环 pop-front、`tail(n)` saturating、时钟回拨 unwrap_or(0)、16 上限边界、TERM→10s→KILL 宽限循环：全部 ✅（pid 兜底见存疑#8）
- `batch.rs:65-75/138-145/464-470` 同路径写冲突对称检测、写互斥/读并发 4、20 万字节预览阈值：✅
- `wait.rs:33` `1..=3600` 闭区间与文档一致（双边界测试）✅
- `approval.rs:13,62-78` **重点核对项**：AUTO_CONFIRM_AFTER=300s 只自动确认 recommended/allow、永不超时分支编译期剔除、cancel 优先于计时器；虚拟时钟测试双边界验证 ✅
- `fence.rs:108-153/193/391-403/448-499` 引号感知分词（wrapping_sub 只喂 `.get`）、`2>&1`/`&>` 邻接判定、sh -c `depth<2` 递归、Block>Confirm>Allow 只升不降、透明化失败保守 Err：全部 ✅
- `host/commands.rs:568/599/635,718/305-317/381-385` s→ms 转换正确、`min(80)`、1MB 字节门、首 root 主其余附加、3s/50ms 轮询常量 ✅
- `host/events.rs / keyring.rs / notify.rs`：无算术（notify 无重试间隔）✅
- `git/status.rs:44/108-133/163-171` 500 条 take、diff 索引与 get_delta 共用、line_stats 解构序（测试断言 add=1 del=1）、40 位 hex `[..7]`：✅

### 前端（48 处，含 4 处存疑已入上表）

- `stores/run.ts:199/1011/1067/497-501/536-540` elapsed 负值钳 0、last-8 滑窗（push 后 shift）、`round(ratio*1000)/10` 百分比（基数经 context.rs:107 核实为 0..1）、FIFO 队首 dequeue、splice 重排标准式：全部 ✅
- `stores/sessions.ts:230/274/296-299` 关 Tab 邻位钳 0、cycleTab 模回绕（idx=-1 退化安全）、未读「离开才亮、活跃即清」方向正确 ✅
- `stores/settings.ts:15` compact_threshold 0..1 与后端 ratio 同基、滑条 0.1-0.9 同域 ✅
- `Composer.tsx:483` `/100` round/10 = 千分位 1 位小数 k 展示 ✅；`Composer.tsx:221,226/290` 三菜单共享高亮模回绕（menuCount=0 有 guard）、Shift+Tab 模式环（indexOf -1 退化为 0 不崩）✅；`Composer.tsx:342-358` 召回上下界正确（注释漂移见存疑#14）；`Composer.tsx:249,423-427` 图片 5MB/张、4 张、20MB base64 字符预算（注释明示按字符计）：✅
- `ChatMessages.tsx:141,193,219-223` 流式豁免窗 `Date.now()+150`（绝对截止时刻用法正确）、40px 距底双阈值一致（[docs/thinking-scroll-fix](./thinking-scroll-fix.md) 复核通过）✅；`ChatMessages.tsx:23-30` `ts()` 无时间留空不伪造（[docs/message-timestamps](./message-timestamps.md)）✅
- `segments.tsx:59-63/117-125` 计时 `max(1, floor(elapsed/1000))`、尾部游标逆向扫描边界：✅
- `AskPanel.tsx:71,173,282-297/198-237` 页 0 基、翻页钳位、标签 `page+1/length`、键盘环 `opts.length+1`（输入框末位）回绕精确、数字键 `-1` 有界：全部 ✅
- `ProjectNav.tsx:36-38` **重点核对项**：navOrder `(b||"").localeCompare(a||"")` b-a = 活跃倒序，空串兜底创建时间，与 [docs/session-nav-new-top](./session-nav-new-top.md) 契约测试一致 ✅；`ProjectNav.tsx:24-33/97-98,443` relTime 单位阶梯、新会话行 `updated_at:""` 留空不冒充活跃 ✅；`ProjectNav.tsx:233,261` slice + show-more 边界 ✅
- `RightBar.tsx:67/205/216` startedAt 如实标注非冒充活跃、KB 上取整 min 1、贴底 `>= scrollHeight - 24` 方向正确 ✅
- `TopBar.tsx:25-26,44` 侧栏宽度常量单源（CSS 只过渡）✅
- `TokenStatsModal.tsx:79` MM-DD 标签 ✅（口径不一致见存疑#11）
- `ProvidersPanel.tsx:216/362-379` k 展示、active-model 回退链终止于 null ✅
- `ToolCallCard.tsx:68-95/141/201` outcome 优先摘要、ms→s、exitCode===0 方向 ✅
- `SubagentDrawer.tsx:28-31` sig 与 `timeline[length-1]` 精确（宽度见存疑#12）；`SubagentItemCard.tsx:5-6/35` k 展示、`step/maxSteps` 升序计数直接展示无反转 ✅
- `FilesPanel/useSessionFiles/FileViewerModal` 60万/360万/8640万 ms 阶梯、300ms debounce、lastIndexOf>=0 guard：✅
- `utils/diff.ts:13-54` LCS 400×400 降级 + 上下文窗口钳位 ✅；`utils/models.ts:5-29` 摊平顺序=配置优先序（[docs/provider-management-refactor](./provider-management-refactor.md)）✅；`utils/markdown.ts:58-156` 数学定界符扫描移植边界一致 ✅；`utils/diagrams.ts:25-33` 240 FIFO 先删后插 ✅
- `theme/bridge.tsx:10` Rec.601 亮度系数和为 1、非 6 位 hex 回退亮色（安全方向）✅

## 四、方法说明

- 排查模式：升序计数器当剩余量用（[docs/budget-notice-step-fix](./budget-notice-step-fix.md) 同类）、`==` 命中整除刻度可被跳过、clamp 地板>天花板反转、百分比错基数、整除截断、单位错位（ms/s、bytes/chars/tokens）、1-based/0-based 错位、比较器方向、除零。
- 三路并行初筛后，全部 14 处存疑项已人工逐行复核（本文档第二节的证据均来自人工读取的当前代码），OK 项为审查代理逐条给出判定。
- 遗留：[docs/oss-prep-batch](./oss-prep-batch.md) 标注的两个 `#[ignore]` 探针之一（stats by_workspace 合并）即本清单#2，修复后建议转正。
