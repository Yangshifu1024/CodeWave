# 会话历史存储：分段 append-only JSONL + 按段分页 + 体积熔断

> 2026-09-24 · 把会话历史从「`histories/<id>.json.gz` 单文件整份覆盖写 + 8MB 硬上限」换成「**`histories/<id>/` 分段 append-only JSONL + 取消上限 + 按段分页 + 软告警/高熔断 + 旧格式清理入口**」。
> 关联：[session-history-limits](./session-history-limits.md)（上一批：图片外置与越限可见，本文取代它的 §2/§3.2）、[session-restore-fidelity](./session-restore-fidelity.md)（P2 清单）、[session-restore-batch1](./session-restore-batch1.md)（§8 批2 清单）、[session-cleanup](./session-cleanup.md)、[technical-design](./technical-design.md)、[mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)（批准门，与本批无关但同属会话面）。

## 1. 为什么要换

改动前的落盘管线有四个结构性毛病：

1. **双删**：落盘前 `trim(256k token, keep_last=2)` 一次、加载后又 trim 一次（`store.rs` 的 `save_history` / `load_history`）。`keep_last = 2` 意味着除末 2 轮外，**超预算的旧轮次在磁盘上就永久消失**——不是「不显示」，是「没了」。
2. **8MB 上限的第三级是拒存**：`bail!("历史超过 8MB 上限，请新开会话")` 后**整份历史不落盘**。
3. **每次检查点整份重写**：每 20 步 + run 收尾各把整份历史 serialize → gzip → 原子替换；崩溃最多丢约 20 步转录。
4. **加载与渲染一次性全量**：`load_session` 返回 `Vec<Message>`，前端 `ChatMessages` 是 `items.map(renderItem)` 整表渲染。

本批的目标不是「换格式」，而是「**落盘不再因预算/上限被裁剪**」；append-only JSONL 与按段读是手段，分页解决的是首屏与内存（不解决 wire 上下文）。

## 2. 磁盘格式

```
histories/<会话 id>/
├── 0001.jsonl          # 段（明文 JSONL，不压缩）
├── 0002.jsonl
└── .segmeta.json       # 水位边车（见 §3）
```

- **每个段文件首行都是头记录**（单段可独立解析）：
  `{"kind":"header","schema":1,"session":"<id>","seq":1,"base":true|false,"at":"<RFC3339>"}`
- **其后每行一条记录**，`kind` 区分：
  - `{"kind":"message","msg":<PersistedMessage>}` —— 复用图片外置那批的落盘 DTO（图片是 `image_blob` 引用或 `image_inline` 兜底）
  - `{"kind":"compaction","head":[…],"source":"compact"|"shrink","at":…}` —— 压缩边界（见 §4）
  - `{"kind":"seal","messages":N,"sig":"<hex>","at":…}` —— 段封口（每段最后一行）
- **不压缩（明文）**：这是刻意的取舍——`grep` / `tail` / 用文本编辑器手工截断崩溃现场的排障价值，是本批收益之一。代价是磁盘占用高于 gzip（可按段压缩作为后续可选项）。
- **切段规则**：按 `SEGMENT_MAX_MESSAGES = 200` 或 `SEGMENT_MAX_BYTES = 512KB` **先触者封口**；**段边界绝不切在 tool_use / tool_result 配对中间**（配对修复是会话级逻辑，切开会让单段无法独立解析）。
- **崩溃语义**：加载时丢弃**尾部残行**（半行）并用最后一个完整记录；非法行跳过该行、坏段跳过该段并给出可见提示（`paging.bad_segments`）——「部分可见」优于「整会话报错」。注意这是**行为变更**：改动前 gz 损坏会让会话打不开。
- **崩溃承诺**（可测）：**最多丢最后一个未封口段的尾部（≤1 段）**。写入策略 = 段封口时 `sync_all()` + run 收尾 sync（不逐行 fsync）。

## 3. 增量游标（本批最大技术风险）

`Message` **没有 id、没有序号**，而内存历史的前缀会被这些路径改写：400 降级阀的 `sanitize*`/`repair` 原地改写与中间插入、保存路径的 `prepare_for_save` 变换、**上下文压缩整体替换** `rt.history`、装载回写。所以「只记已落盘条数」不够。

会话级水位 = `{written: usize, sig: u64, blobs: HashSet<String>}`，每次保存：

1. `prepared = prepare_for_save(history)`（**不再 trim**）；
2. 对 `prepared[..written]` 算**廉价的 64 位快照哈希**与缓存 `sig` 比对（O(n) 的哈希，不序列化整份、不 gzip、不整份写盘）；
3. `len(prepared) > written` 且哈希匹配 → **只 append** `prepared[written..]`（常态路径，O(增量)）；`len == written` 且匹配 → 什么都不做（不再整份重写）；
4. 哈希不匹配（repair 改写 / 压缩替换）→ 写**基线段**：把完整 `prepared` 写成**新的段文件**（`base: true`），并把它记为**时间线重置点**。

**不可违反的不变量**：任何 fallback 都**只新增段，绝不覆盖 / 截断 / 删除既有段**——这是「不丢历史」的底线。

**水位边车** `.segmeta.json`：记录 `{written, sig, blobs, segment_count, last_seq, tail_seq, tail_bytes}`。后四项是**校验证据**（append-only 下任何一次追加必改「段个数 / 最大序号 / 未封口尾段字节数」之一）；校验不过就**回退整目录扫描**（慢但一定对）。这让**冷启动的水位推导也变成有界读取**（终版实测：全新实例追加一条消息只读 202 字节 / 共 8.4MB）。

## 4. 压缩与磁盘历史的关系（本批头号决策）

**问题**：上下文压缩（`core/context.rs`）整体替换 `rt.history`。更改动前，下一次检查点就把「压缩后的小历史」写进磁盘 → **压缩点之前的轮次在磁盘上永久消失**，与「旧轮次不再永久消失」直接矛盾。

**采用**：**磁盘保留完整转录，压缩只记一条边界记录**。

- 写边界记录的时机：保存时检测到「`prepared` 与已落盘前缀不匹配且**变短**」= 历史被压缩 → 写 `{"kind":"compaction","head":[…压缩后全部消息…],"source":"compact"|"shrink"}` 的**基线段**。
  - 为什么存 `head`（压缩后全部消息）而不是单条摘要：`context.rs` 压缩后的形状是「摘要首条 + （`keep_last_user` 为真时）一条尾部 user 克隆」，尾部那条是**已有消息的克隆**、无法按内容区分，只有存完整前缀才能**逐字节还原**。
  - `source`：按「首条是否 handoff 摘要」区分 `compact` / `shrink`（wire 重建处理相同，仅供排障/展示可读）。
- **加载时产出两份**：
  - `wire` = 最后一个重置点之后的时间线 + `repair` + `trim(256k, keep_last 2)` —— 与改动前**重启后的上下文逐字节一致**（token 与计费零变化）；
  - `display` = **全部** message 记录（**不 trim**）——展示侧的完整转录，前端在边界段处画一条「此处上下文已压缩」分隔线，更早内容仍可向前翻页查看。
- 前端契约：`load_session` / `load_session_earlier` 都返回 `boundaries: [{seq, source, at}]`（只含本次覆盖段范围内的边界）；前端按已加载项的稳定键 `s<段号>:<段内序>` 判断分隔线插在哪一条之前——**后端不下发 display 下标**。

## 5. 读路径

### 5.1 按段分页（前端消费）

```ts
loadSession(sessionId, workspace) -> {
  messages: Message[],          // 首屏 display：最近 1 段（不 trim）；legacy 时 = 整份
  paging: { format, loaded_from_seq, segment_count, total_messages, bytes, has_more, bad_segments },
  boundaries: [{seq, source, at}],
}
loadSessionEarlier(sessionId, beforeSeq) -> {
  messages: Message[],          // 严格早于 beforeSeq 的最近一段
  from_seq, has_more, bad_segments, boundaries,
}
```

- **`messages` 一律 display 口径**；**前端不得拿它回填模型上下文**（`rt.history` 由后端从 `wire` 初始化）。
- 前端渲染键是**稳定标识** `s<段号>:<段内序>`（不是数组下标）——否则前插更早内容会让所有下标漂移、React 节点错位。
- 滚动锚点（批1 的 `scrollAnchor`）从「下标 + 内容指纹」改为「**稳定键 + 内容指纹兜底**」，保留 3 帧二次校正；加载更早内容前先捕获锚点，插入后仍锚在同一条消息上。
- 内存有界：`MAX_PAGED_PAGES = 30`（≈6000 条驻留）+ 「收起更早的」。

### 5.2 wire 的有界装载（后端）

旧的 8MB 上限顺带让「打开会话的读取成本」有界；取消上限后若不处理，打开一个几百 MB 的历史就要整份读——**这正是本批要保护的「大会话秒开」**。因此 wire 装载从尾部向前读、够用即停：

- 「够用」= 累计 token 估算 ≥ `TRIM_BUDGET_TOKENS` **且**落在安全轮边界（窗口内没有引用窗口外 `tool_use` 的孤儿 `tool_result`）**且 ≥ 3 轮**（保证全量的 `trim` 至少丢一轮，从而把窗口之前的轮次与开头的非 User 前缀一并丢掉），或已到时间线重置点（此时窗口就是完整 wire）。
- **硬要求**：有界装载的 wire 与「全量扫描 + repair + trim」**逐条逐字节一致**（对拍用例守护）。等价性推导写在 `SessionStore::load_history_wire` 的文档注释里。
- 终版实测：12 段 / 8.4MB 历史打开会话只读 **1.4MB（16.7%）**；历史翻倍时读取量**几乎不变**（与总量脱钩）。

### 5.3 格式裁决与旧格式回落

- 判据**单点**：`SessionStore::reads_new_format(id)` —— 段目录不存在 → 旧格式；段目录存在但**没有旧文件** → 新格式（唯一副本，如实报空历史 + 坏段数，不报错）；**两者并存** → 探针 `segments::has_readable_messages(dir)`（至少一个段含可读 message 记录）才算「新格式权威」。
- 为什么必须查内容：只判「目录存在」会让「空/全坏段目录 + 旧文件」的会话走进新格式路径 → 用户看到**空历史**（数据其实还在旧 `.json.gz` 里）。
- 旧格式（`.json.gz`）：读兼容、**不迁移**；损坏不再整会话失败（记 warn 后按空历史处理）。

## 6. 体积约束：软告警 + 高熔断

- `HISTORY_SOFT_WARN_BYTES = 200 * 1024 * 1024`（软线：**照常写**，只给可见提示）
- `HISTORY_HARD_FUSE_BYTES = 1024 * 1024 * 1024`（硬线：**停止 append**，**绝不删任何已有历史**）
  依据：正常会话是几十 KB ~ 几 MB（200 轮 ≈ 1~2 MB），触达软线即代表异常；硬线是软线 5 倍，只停写、可自愈。
- 状态复用既有可见链路：`HistoryStatus` 新增 `Warned { bytes, threshold, at }` / `Fused { ... }` → `SaveReport.history_status` → `run:done.history_save` → 会话内 notice（当场可见）+ `SessionMeta.history_status`（**重启后仍可见**）→ 体积回落后**自愈清除**。
- **最容易漏的一处**：`SaveReport::is_clean()` 必须同步纳入新状态（`drive.rs` 的上报判据是 `filter(|r| !r.is_clean())`，不改则提示永远发不出去）。
- 熔断期间仍会写索引元数据（`updated_at` 等）与状态，但**不写历史、不 GC**（没写新内容时引用集合为空，做 GC 会删掉旧段仍在引用的图）。
- 子代理过程历史**有意不熔断**（无 UI 载体，越限只记 `session_log`）。

## 7. 生命周期与旧格式清理入口

- 新布局下四处生命周期都改认 `histories/<id>/` **目录**：`cleanup.rs` 的 `delete_session_files` / `orphan_candidates` / `remove_orphan_files`、`store.rs` 的 `remove` / `purge_non_session_entries`；`orphan_candidates` 还按目录 mtime 判定（追加写不改目录 mtime，故不能只看 mtime）。
- **设置页新增「清理旧格式历史」入口**（照保留期清理那套范式：`settingsRegistry` 登记 + `SettingsPage` 预览/执行/确认 + i18n 两侧）：
  - 预览：可回收的会话数与字节数 + **必须保留**的会话数；
  - 执行：删除 + 回收统计 + 被保留（跳过）的数量。
- **铁律**：**绝不删除「没有新格式可读数据」的旧 `.json.gz`** —— 那是该会话历史的唯一可读副本。判据与读路径**同源**（`segments::has_readable_messages`）：空段目录 / 全坏段 / 只有头与封口的段都**不算**「有新格式数据」。
- 扫描范围只覆盖 `histories/<id>.json.gz` 一层，**不含子代理过程历史的旧文件**（`histories/subs/<父>/<子>.json.gz`，随父会话级联删除）。

## 8. 契约与不回退

- `core/types.rs` 的 `Content` 未改；三协议请求体字节不变；`events.contract.test.ts`（事件面 29 键）未改。
- `load_session` 的返回形态**变了**（`Vec<Message>` → `{messages, paging, boundaries}`），前端已同步适配；新增命令 `load_session_earlier`。
- wire 上下文、token 计费与压缩语义**零变化**（分页只作用于前端展示与 IPC 传输；后端 runtime 仍全量装载内存历史）。
- 既有测试只增不减：与本批语义直接冲突的用例按新语义改写（例如「超限拒存」→「超限照常保存」、「历史 trim 掉旧轮次」→「不再 trim」），均在提交信息与审查报告里逐条登记。

## 9. 已知边界

- **回滚不是零成本**：新格式（段目录 + `compaction` / `seal` 记录）旧版本不认识 → `git revert` 后**用新格式存过的会话不可读**。这是与上一批同类边界。
- **`noop` 保存的索引行为**：历史真无变化且标题/模型也没变时，**不写任何索引**（省一次写盘）；若标题变了则仍会刷新（否则改名会丢）。
- **坏历史不再报错**：损坏的旧格式历史按空历史处理（记 warn）；用户无法从界面区分「无历史」与「历史损坏」，只能从 `logs/` 看出。
- **边车 `blobs` 并集在坏段存在时可能是超集**（回退全量扫描时会跳过坏段 → 可能少；方向安全即「多留几张图 / 不误删」）。
- **未封口尾段在冷启动会被读两遍**（`summarize` 的计数回退 + 建 `open_tail`），有界（≤512KB）。
- **有界装载依赖 `repair` 的局部性**：等价性推导假设 `repair` 的改写只落在被丢弃的前缀内（不向列表尾部追加合成消息）。将来若改 `repair` 语义需重新验证。
- **明文不压缩**：磁盘占用高于 gzip；按段压缩是可选的后续项。
- **虚拟滚动仍未做**（用户已拍板本批不做）：批1 的滚动锚点在分页下已重新回归，但「一步跳到最早段」与长列表渲染性能属后续批次。
- **批2 清单仍未完成项**：`display_only`（概念不可考，需求原文目录已不存在）、压缩归档移出 `tmp/`（`~/.codewave/tmp/compacted/` 当前**无清理者**）、后端注入队列持久化（32 格 mpsc 纯内存）。

## 10. 验证

- `cargo test` **1141 passed / 0 failed / 3 ignored**；`pnpm --dir ui test` **1174 passed / 97 文件**；`pnpm prepr` 8/8 硬步骤通过。
- 专项：段切分不拆 tool 配对、append 增量不重写既有字节、前缀改写走基线段且旧段保留、半行/坏行/坏段容错、有界 wire == 全量（对拍）、水位推导 == 全量扫描、打开会话读取量与历史总量脱钩、软告警/硬熔断（硬断时既有段**逐字节不变**）、旧格式回落与清理铁律、分页逐段拼接 == display、锚点插入不漂、稳定键不错位。
- 人工清单：长会话翻到最早、崩溃后只丢末段、超软线提示（当场 + 重启后）、旧会话照常打开、清理旧格式入口的回收统计、`cat histories/<id>/0001.jsonl` 可直接读。
