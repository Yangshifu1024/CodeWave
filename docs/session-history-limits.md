# 会话历史体积上限：口径、越限阶梯与图片外置

> 2026-09-24 · 回答「8MB 上限会不会不太够」：**口径是对的，但图片那条路既会静默丢图、又会整份丢历史**。本批把图片搬出历史文件（落盘换引用），并给越限补上「可见 + 按轮降级 + 不再整份丢弃」。
> 关联：[session-restore-fidelity](./session-restore-fidelity.md)（工具结果 sidecar；本文是它 P2 清单里「8MB 上限与图片外置」的落地）、[session-cleanup](./session-cleanup.md)、[technical-design](./technical-design.md)、[session-restore-batch1](./session-restore-batch1.md)（批2 范围）。

## 1. 上限口径：8MB 量的是什么

`MAX_HISTORY_BYTES = 8 * 1024 * 1024`（`src-tauri/src/core/sessions/store.rs`），量的是**单会话历史文件 gzip 压缩后**的字节数。

**纯文本够得离谱**：落盘前先 `repair::trim(&mut prepared, TRIM_BUDGET_TOKENS, 2)`，预算 `TRIM_BUDGET_TOKENS = 256 * 1024`（256k token ≈ 1MB 原始文本）→ gzip 后约 **300KB**，上限只被用到约 4%。工具出参另已被 `tools/compact.rs` 压到 HEAD 4KB + TAIL 8KB；主会话 `read` 还有 2000 行/次的预算。**提高上限对纯文本场景零收益。**

**瓶颈全在图片**，两个原因叠加：

1. base64 近似不可压缩。gzip 对 base64 ≈ 0.75 byte/char，而 base64 是原始字节的 4/3 → 相乘 ≈ 1，即 **8MB 上限 ≈ 8MB 原始图片**；
2. 图片在 token 估算里只按 **1600 token/张**计（`util/token_est.rs`）→ 图片几乎**不受 256k trim 约束**，可以在预算内无限堆字节。

附件侧允许单张 5MB、单条最多 4 张、base64 总量 20MB → **2 张 5MB 图 ≈ 10MB gz 即越界**；一次满额发送越界约 2.5 倍。也就是说：**改动前，「前端允许的合法输入」必然触发落盘降级**——这是契约不自洽，不是用户用错了。

## 2. 改动前的越限行为（为什么要修）

两级降级，**两级都只有 `tracing::warn!`**（用户零提示）：

1. `gz > MAX_HISTORY_BYTES` → `repair::sanitize_keep_thinking` **剥图重存** → 附件退化为 `[image/png omitted]` 占位文本，**重开会话后图片全部消失**；
2. 剥图后仍超 → `anyhow::bail!("历史超过 8MB 上限，请新开会话")` → **整份历史不落盘**（文件与索引都不写）；调用方 `core/agent/drive.rs` 的检查点保存只 `tracing::warn!` 吞掉 → 用户表现为「最近几轮凭空消失」，磁盘上也没有任何可考痕迹。

第 2 级在纯文本下也可达：`repair::trim` 的 `keep_last = 2` 意味着**最后两轮永远不会被裁**，所以「单轮塞进巨量文本」照样能顶爆。

## 3. 本批改动

### 3.1 图片外置（根治「合法输入必然降级」）

**落盘专用 DTO**（`core/sessions/persist.rs`）：`PersistedMessage` / `PersistedContent`，图片拆成两态——

- `ImageInline { media_type, data }`：旧数据，或外置失败/超限时的兜底（`#[serde(alias = "image")]` 兼容旧历史的 tag）；
- `ImageBlob { media_type, blob }`：base64 存在 `sessions/<owner>.imgblob/<blob>`。

**为什么另起 DTO 而不是给 `Content` 加字段/变体**：`Content` 同时是内存模型与 wire 模型，加可选字段会撞三协议请求体字节断言，加新变体要改 5-6 处 match 穷尽点。独立 DTO 让 **内存与 wire 零改动**——`load_history` 读回 base64 后才交给上层，前端与 provider 完全看不到引用形态。

**blob 存储**（`core/sessions/image_blobs.rs`）：blob id = `sha256(base64 原文)` 前 32 hex（内容寻址 → 同图自动去重 + 幂等写）；`atomic_write` 落盘；单图 base64 超 `MAX_DATA_CHARS`（7,000,000 字符）时保持内联。

**生命周期**：随会话级联删除（`SessionStore::remove` / `cleanup::delete_session_files` / `purge_non_session_entries` / `orphan_candidates` 四处挂点，顺带补上了此前 `remove` 漏删 `.toolres/` 的缺口）；**不进**右栏「文件」面板。

**GC**：保存成功后按「本次引用集合」回收同会话目录内的未引用 blob；只删**名字恰为 32 位小写 hex** 的文件（避开 `atomic_write` 中转文件与异物）；失败只 warn，不阻断保存。**拒存路径不做 GC**（此时磁盘上仍是上一次成功的历史，删 blob 才是不可逆损失）。

**并发**：`SessionStore` 新增一把 `save_lock`，串行化「外置写 blob → 写历史 → 索引 → GC」整段（锁序恒为 `save_lock → index_lock`）。没有它时，`rename_session` 与 run 内检查点并发会出现「A 的 GC 删掉 B 刚写盘的历史仍引用的图」。

**子代理**：子代理过程历史的 blob owner 为 `<父会话>__<sub>`（子 id 只有 32 bit，裸用会跨会话撞目录 → 一方 GC 删掉另一方的图）。

### 3.2 越限阶梯（不再整份丢弃）

```
gz 超限
 → ① 剥图重存（保留，作旧数据兜底）
 → ② 仍超：trim(prepared, TRIM_BUDGET_TOKENS, 1) 只保最后一轮，记 dropped_rounds
 → ③ 仍超：才拒存（既有文案与测试不变）
```

顺序要点：**先 trim 再外置**（被裁轮次的图片不写 blob）；**GC 必须在历史 `atomic_write` 成功之后**。

### 3.3 越限可见（当场 + 重启后）

- **状态挂索引**：`SessionMeta.history_status`（`Degraded { stripped_images, dropped_rounds, at }` / `Rejected { at, reason }`，serde default + `skip_serializing_if`）。索引是独立文件，**历史写失败时它仍能写成功**——这是「重启后仍可见」的唯一可行载体。下一次**干净**保存自动清空（自愈，不引入「已知晓」交互）。
- **当场可见**：`save_history` 返回结构化 `SaveReport`；`checkpoint` 返回 `Option<SaveReport>`；run 收尾时把**非干净**的结果放进 `run:done` 载荷的可选字段 `history_save`（事件面 29 键名一个都没动），前端据此 push 一条会话内 `notice`。
- **重启后可见**：前端打开会话时读 `SessionMeta.history_status`，在历史重建后补同一条 notice。
- **子代理**：`save_sub_history` 同阶梯但**不拒存**（无 UI 载体），越限只记 `session_log`（恒开启黑匣子）。

## 4. 契约与不回退

- `core/types.rs` 的 `Content` **一行未改**；三协议请求体与字节断言不变。
- 事件面 29 键**键名不变**，`run:done` 只增可选字段。
- 纯文本会话的序列化形态与改动前一致（`PersistedMessage` 与 `Message` 同构，非图片块 tag/字段序相同）。
- 既有钉死用例全部保持：`save_history_still_over_cap_after_stripping_images_is_rejected`（拒存文案与「不留半成品」）、`save_history_over_cap_fallback_keeps_thinking`（剥图不丢思考）、`save_history_image_kept_under_cap_stripped_over_cap`。

## 5. 已知边界

- **降级保存过的历史对旧版本不可读**：新增 tag `image_blob` 是旧版本不认识的枚举变体 → `git revert` 本批后，用新格式存过的会话 `load_history` 会报解析失败。**回滚不是零成本**（需一并回滚数据或接受该会话不可读）。
- **拒存路径写出的 blob 成孤儿**：`to_persisted` 在尺寸判定之前执行，拒存时已写盘的 blob 留待下次成功保存时回收（只多占磁盘）。
- **每次成功保存写两次索引**（`upsert_meta` + `set_history_status`）：检查点热路径上的真实开销，未做「值未变则跳过」的优化。
- **新会话「首存即拒存」时状态无处可挂**：索引条目由首次成功保存创建，此时该会话本就还不在列表里。
- **单图 base64 超 700 万字符**（≈5MB 原图）不外置，仍内联进历史。
- **blob 不做跨会话共享**：同一张图在两个会话各存一份。
- **子代理过程历史的 blob 目录**（`sessions/<父>__<sub>.imgblob/`）在 `orphan_candidates` 里显式跳过，永不按「索引外残留」清理。
- 图片**上 wire 的体积不变**（三协议都要求内联 base64）——外置只解耦磁盘/历史体积。

## 6. 与批2 的边界

**批2 的存储重构已落地**（[session-history-storage](./session-history-storage.md)）：分段 append-only JSONL、取消 8MB 硬上限（改为软告警 200MB / 硬熔断 1GB）、历史 trim 双删与 256k 预算口径、按段分页、旧格式清理入口均已做完（虚拟滚动仍留待后续）。本文描述的上限阶梯里，「按轮降级」与「拒存」两级已随之**移除**，只保留「剥图」作为旧数据兜底；`HistoryStatus::Rejected` 不再产生。

## 7. 验证

- `cargo test`：**1050 passed / 0 failed / 3 ignored**（本批新增 30 条：`image_blobs` 5 / `persist` 6 / `store` 14 / `cleanup` 2 / `drive` 3）
- `pnpm --dir ui test`：**1098 passed / 95 文件**（新增 `history-status-notice.test.ts` 13 条）
- `cargo fmt --check` 干净；`cargo clippy --all-targets` 0 error 且未新增 warning；`pnpm --dir ui run lint` 0 error；`pnpm --dir ui build` 通过
- 契约复核：`events.contract.test.ts` 3 passed；`cargo test provider::` 82 passed（三协议字节/线上形态）；`cargo test save_history_` 8 passed

人工点验清单：

1. 贴 2 张 1MB 截图 → 关掉重开会话 → 图仍在；`~/.codewave/sessions/<id>.imgblob/` 里有两个文件，`histories/<id>.json.gz` 只有几百 KB。
2. 贴 4 张 5MB 图（满额）→ **不出现任何降级提示**，重开后图仍在（契约自洽达成）。
3. 制造文本超限（一轮里连读多个大文件）→ 会话内出现「历史未完整保存」提示；重启重开该会话提示仍在；再正常跑一轮后提示消失。
4. 子代理跑一个大任务 → 抽屉过程流正常；子代理历史越限时只在 `logs/<id>.log` 里留记录，界面无异常。
5. 删掉该会话 → `.imgblob/` 与 `.toolres/` 目录都随之消失；右栏「文件」面板里不出现它们。
6. 打开一个本批之前创建的、含内联图片的旧会话 → 正常加载、图片正常显示（旧 tag `image` 由 alias 兼容）。
