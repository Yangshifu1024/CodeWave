# CodeWave · thinking 上游 reasoning_content 回传修复

> 日期：2026-09-16
> 缺陷：调用 OpenAI 兼容 thinking 上游（DeepSeek 系模型经中转）时请求被拒 ——
> `Error from provider: Upstream request failed: [invalid_request_error] The reasoning_content in the thinking mode must be passed back to the API. (HTTP 400)`
> 分支：`fix/reasoning-passthrough`（基线 `ee81603`）
> 约束遵守：前端零改动；界面不做 GUI 自动点验（§7 交付手动清单）；git 写操作仅限已批准的分支创建/切换

---

## 1. 根因

三层各丢一次思考，导致「要求回传思考」的上游每次都拿不到数据：

| 层 | 位置 | 原状 | 后果 |
|---|---|---|---|
| 出站映射 | `provider/openai_chat.rs` `convert_message` 的 assistant 分支 | 只取 `text_joined()`（仅 `Content::Text`）与 `Content::ToolUse`，`Content::Thinking` 落入 `_ => None`；**全仓出站从不产出 `reasoning_content`** | 同一 run 第 2 步 / 同会话第 2 次提问起 → 必然 400 |
| 落盘 | `core/sessions/repair.rs` `sanitize_for_save` | 无条件丢弃 `Content::Thinking` | 重启后连「可回传的数据」都没了 → 仍 400 |
| 400 兜底 | `core/agent/drive.rs` BadRequest 分支 | 兜底调 `sanitize`（同样丢思考）后重发 | 对该错误类型无效（只会丢得更干净）；且 `sanitized_once` 每 run 只放行一次 |

**首轮全新会话的第 1 步不会 400**：出网 messages 里没有 assistant 消息（system + 用户消息），上游无从要求回传。

触发条件（原状）：

- 同一 run 内多步工具调用（第 2 步请求携带上一步的 assistant 消息）；
- 同一会话内第 2 次用户提问（进程未重启）；
- 重启/重开会话后继续提问（落盘已丢思考）；
- 以及任意 400 之后：兜底会就地抹掉内存历史里的思考，使本 run 再无数据可回传。

## 2. 方案（定稿决策）

1. **回传策略 = 自动，不新增任何配置项**：历史 assistant 消息只要含非空思考块就回传 `reasoning_content`。触发条件天然严格（历史有思考块 ⟺ 上游此前确实返回过 reasoning），对不返回 reasoning 的端点（如 OpenAI 官方非思考模型）零影响。
2. **思考随会话落盘**（`sanitize_for_save` 不再剥离），重启后仍能回传。
3. **降级阀保留并升级为会话级粘性**：`sanitize`（BadRequest 兜底变体）继续丢弃思考，作为「上游拒收该字段」时的降级通道，一次命中后本会话不再回传（§4）。
4. **不动的边界**：anthropic（`anthropic.rs` 出站因 thinking 签名约束继续丢弃）与 `openai_responses`（同样丢弃）本批次不碰。

## 3. 改动清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/provider/openai_chat.rs` | assistant 出站按序拼接非空思考块 → `reasoning_content`（§3.1） |
| `src-tauri/src/core/sessions/repair.rs` | sanitize 三变体分叉：落盘保思考（§3.2） |
| `src-tauri/src/core/sessions/store.rs` | 8MB 回退改「剥图留思考」 |
| `src-tauri/src/core/agent/drive.rs` | 400 文案分类 `Reasoning400` + 粘性标记更新 + 择变体修复 |
| `src-tauri/src/core/agent/runtime.rs` | 会话级 `reasoning_rejected` 字段（`new_sub` 继承父会话） |
| `src-tauri/src/core/agent/stream.rs` | 出网副本按粘性标记剥思考（`drop_thinking_blocks`） |
| 测试 4 文件 + 前端 0 文件 | 见 §6 |

### 3.1 出站回传（`openai_chat.rs:124-141`）

`convert_message` 的 assistant 分支在既有 `content`/`tool_calls` 输出之后追加：

```rust
let reasoning = m.content.iter()
    .filter_map(|c| match c {
        Content::Thinking { text } if !text.is_empty() => Some(text.as_str()),
        _ => None,
    })
    .collect::<Vec<_>>()
    .join("\n\n");
// ...
if !reasoning.is_empty() {
    v["reasoning_content"] = json!(reasoning);
}
```

三条刻意保持的不变量：

- **空守卫不放宽**（`text.is_empty() && tool_uses.is_empty()` → 整条不上 wire）：仅含思考块的 assistant 消息继续不发，避免产生 `content: null` 且无 `tool_calls` 的非法消息；
- **没有思考块时不写该键**（不发空串、不发 null）：纯文本与工具调用消息的 wire 字节与改动前完全一致（对前缀缓存友好）；
- `build_body` 仍是纯函数，不修改 `req.messages`。

连接符取舍：多个思考块以 `"\n\n"` 拼接（与 `text_joined()` 的 `"\n"` 不同）。Chat Completions 协议上「思考」与「正文」是独立字段，思考↔正文的原始交错顺序本就不可表达；`thinking,text,thinking` 这类交错会被压平为 `first\n\nsecond` + `answer`，这是协议上限而非实现取舍（已由单测钉死）。

### 3.2 sanitize 三变体分叉（`repair.rs:33/42/49`）

`sanitize_inner(msgs, strip_images, strip_thinking)` 增加第二个开关；丢弃点从无条件的 `Content::Thinking { .. } => None` 变为 `Content::Thinking { .. } if strip_thinking => None`：

| 变体 | 岗位 | 剥图 | 丢思考 | 理由 |
|---|---|---|---|---|
| `sanitize` | BadRequest 降级兜底（`drive.rs`） | ✅ | ✅ | 上游若**不认**该字段，剥掉思考后重试即成功——降级阀语义必须保留 |
| `sanitize_for_save` | 会话落盘（`store.rs`） | ❌ | ❌ | 思考是回传 `reasoning_content` 的数据源，落盘剥离即永久丢失 |
| `sanitize_keep_thinking` | 存储 8MB 超限回退（`store.rs:413`） | ✅ | ❌ | 回退只为把转录压进上限，剥图已足够减负，不该顺手丢思考 |

### 3.3 8MB 回退（`store.rs`）

`save_history` 的超限分支由 `repair::sanitize` 改为 `repair::sanitize_keep_thinking`：避免「超大历史 → 该会话永久失去回传能力 → 此后每次多轮都 400」。剥图后仍超限则维持原样 `bail!("历史超过 8MB 上限，请新开会话")`（该分支已补测试覆盖）。

## 4. 降级阀与自愈链（本批次的核心机制）

「回传」与「拒收」是同一字段上的两种相反立场，必须都能收敛：

```
drive.rs：400 文案分类 Reasoning400（大小写不敏感）
  ├── Rejected  ：提到 reasoning_content / thinking mode，且不含「必须回传」语义
  │               → 置位会话粘性标记；出网副本此后剥掉思考
  ├── Demanded  ：含 must be passed back 语义（缺了才 400）
  │               → 复位标记（自愈阀）；修复变体改用 sanitize_keep_thinking 保住思考
  └── Unrelated ：与该字段无关 → 标记不变，走既有 sanitize 修复
```

- **粘性标记**：`SessionRuntime::reasoning_rejected: AtomicBool`（`runtime.rs:130`）。跨 step、跨 run 保持；会话级内存态，进程重启后重新学习一次。
- **出网侧生效点**：`stream.rs::messages_for_request` 在 clone 出历史、`repair_before_send` 之前，按标记剥掉**出网副本**里的思考（`rt.history` 原样保留，转录与 UI 不受影响）。
- **为什么必须是会话级**：粘性标记若是 run 级（初版实现），「拒收型」端点在 step N 修好、step N+1 因新思考再 400 时阀门已用尽且 `BadRequest` 不可重试 → run 直接失败。会话级粘性使「每次请求都安全」。
- **误判的后果不对称**（`drive.rs` 注释已写明）：`Rejected → Demanded` 误判退化为「多一次 400 + 重试」；反向误判会让会话短暂停止回传思考，由 `Demanded` 复位自愈（下一请求重新带上思考；若该端点实际是拒收型，会被重新判为 `Rejected` 再置位——两个方向对称收敛）。
- **子代理继承**：`runtime.rs::new_sub` 继承父会话当前标记值（同一进程内端点特性一致，子代理首个请求不必再撞一次 400）；`new_task` 无父 runtime，从 false 起步自行学习。

## 5. 行为变更与兼容性

| 项 | 结论 |
|---|---|
| 配置 schema | **零变化**（不新增字段，无迁移） |
| 存储格式 | 零变化：`Content::Thinking` 变体本就存在，思考此前只是被 sanitize 剥掉；旧数据读取不受影响 |
| 前端 | **零生产代码改动**：`stores/run.ts` 的历史恢复路径本就支持 `type: "thinking"` |
| 思考落盘的新增影响 | ① 转录体积增大（受 `trim` 预算与 8MB 上限约束）；② 前端重启后可见思考块（此前被剥离，不可见）；③ `est_tokens_message` 本就计入思考，落盘后重载历史的估算更贴近真实占用 → 更早触发裁剪/压缩，属预期代价 |
| 隐私提示 | 思考内容现在会写入本地会话历史文件（`~/.codewave/histories/*.json.gz`，本地 gzip，不出本机）；verbose 会话日志的请求体全文含该字段（默认关闭） |
| anthropic / openai_responses | 出站行为零变化（继续丢弃思考） |
| 空 assistant 不变量 | 未放宽：出网副本先剥思考、后 `repair`，仍由 wire 层守卫拦住空消息 |

## 6. 验证

| 项 | 结果 |
|---|---|
| `cargo test -- --test-threads=1`（`src-tauri/`） | ✅ **639 passed / 0 failed / 2 ignored** |
| `cargo check --lib --all-targets` | ✅ 0 warning |
| `pnpm --dir ui test` 等价命令（`ui` 下 `vitest run`） | ✅ 52 文件 / 429 passed / 0 failed（前端零改动；见下方环境说明） |
| `pnpm --dir ui build` 等价命令（`tsc --noEmit` + `vite build`） | ✅ exit 0 / built |
| `git diff --check` | ✅ 无空白错误 |

**环境说明（非本次改动问题）**：本机 `ui/node_modules/.pnpm/mermaid@12.0.0` store 缺失（依赖未装全），`pnpm --dir ui test|build` 会先触发 install 并卡在 registry.npmjs.org 的 mermaid tarball 下载（26MB，网络慢）而失败，故改用 `ui/node_modules/.bin/vitest`、`tsc`、`vite` 直连取得等价结果。建议方便时执行一次 `pnpm --dir ui install` 恢复规范命令。

新增/改写用例（后端）：

| 文件 | 用例 |
|---|---|
| `provider/openai_chat.rs` | `assistant_thinking_becomes_reasoning_content`（且思考文本与工具 id 取不同值以保辨别力）、`assistant_reasoning_content_joins_thinking_blocks_in_order`、`assistant_without_thinking_has_no_reasoning_content_key`、`assistant_with_only_thinking_stays_off_wire` |
| `provider/tests_integration.rs` | `reasoning_content_passthrough_from_history_to_body`（内部历史 → `build_body` 端到端，含 tool 配对） |
| `core/sessions/repair.rs` | `sanitize_for_save_keeps_thinking`、`sanitize_drops_thinking_as_degrade_valve`、`sanitize_keep_thinking_strips_images_but_keeps_thinking`、`thinking_survives_sanitize_pipeline_roundtrip` |
| `core/sessions/store/tests.rs` | `save_history_over_cap_fallback_keeps_thinking`（真实保存→读回）、`save_history_still_over_cap_after_stripping_images_is_rejected` |
| `core/agent/stream.rs` | `repair_before_send_keeps_thinking_blocks`、`repair_before_send_is_idempotent`、`drop_thinking_blocks_keeps_everything_else`、`messages_for_request_drops_thinking_when_reasoning_rejected`（含 `rt.history` 未被改写断言） |
| `core/agent/drive.rs` | `classify_reasoning_400_*`（三分支 + 大小写）、`update_reasoning_sticky_sets_resets_and_keeps`、`sticky_mark_self_heals_when_reasoning_is_demanded`（误判 → 锁死症状 → 自愈全链） |
| `util/token_est.rs` | `message_estimates_include_thinking` |

**未覆盖（明确声明）**：真实上游 HTTP 端到端（缺 `reasoning_content` 真被 400、拒收型端点真被拒）无自动化用例；真机 GUI 多轮对话不在自动化能力内（§7）。

### 既有 flaky（本次范围外；**已于 2026-09-21 修复**）

`provider::tests_integration::midstream_disconnect_maps_to_network` 在**默认并行**执行下会间歇失败（`got Server("")`）。三重证据判定与本次改动无关：

1. 单独跑 4/4 通过；
2. `--test-threads=1` 串行全量 639 passed / 0 failed；
3. **跳过本次全部新增用例**后并行仍复现（19 filtered，1 failed）。

**已修（2026-09-21，分支 `test/provider-midstream-flaky`）**：根因经复现坐实（改前 `cargo test --lib provider::` 连跑 20 次红 4 次，恒为 `got Server("")`），两条且均与产品行为无关——① mock 只 `read` 一次就写半截响应并立即 `drop(sock)`：带未读数据 close 在 Windows 上发 RST，客户端因此忽而 RST 忽而 EOF；② listener 随任务结束被释放，而并发用例也绑 `127.0.0.1:0`，同一临时端口可能被另一条用例的 mock 抢到并用它自己的脚本（含空 body 的 5xx）应答——客户端拿到与本地 mock 无关的响应，经 `from_status(5xx, "")` 归为 `Server("")`。修法：listener 用 `Arc` 持有并活到用例结束 + 读干请求头（到 `\r\n\r\n`）+ `shutdown()` 写半部优雅收尾 + 断言接受集纳入 `Server(_)`（`retry.rs` 同样视其为可重试，本用例只承诺「不被误判为硬失败」）。验证：改后 30 次模块连跑 + 8 次全量跑 0 红，`pnpm prepr` 8/8；教训已写入 [AGENTS.md](../AGENTS.md) 踩坑清单。
## 7. 手动验证清单（GUI 不做自动点验）

前置：`pnpm tauri dev`，会话模型选 `deepseek-v4.1-flash`（provider `OpenCode Go`，`api_format = openai_chat`）。

1. **多步工具调用**：发一个需要多步工具的任务（如「读一下 X 再改 Y」）→ 第 2 步起不再出现 `[invalid_request_error] ... reasoning_content ...` 400，任务能跑完。
2. **同会话多轮**：同一会话再发第二条用户消息 → 仍正常（此前必 400）。
3. **重启后继续**（本次修复的关键场景）：完成一轮 → 退出应用 → 重开 → 打开该会话继续提问 → 不再 400。
4. **重启后历史显示**：重开后历史里的「思考过程」折叠面板仍在（本批次起思考随会话落盘；此前重启后不显示）。
5. **不返回 reasoning 的端点回归**：切到任意普通（非 thinking）模型跑一轮多步任务 → 行为与改动前一致（请求体里不会多出 `reasoning_content`）。
6. **拒收型端点（如有）**：若某端点不认该字段 → 首次请求 400 后自动剥思考重试成功，日志出现「该上游拒收 reasoning_content，本会话后续请求不再回传思考」，此后各步不再重复 400。
7. **子代理**：`$explore` / 多文件任务派子代理 → 子代理过程不因该字段报错。

## 8. 遗留与已知边界

1. **存量旧会话无法回溯修复**：升级前落盘的会话，其历史里的思考已被剥离，对「要求回传」的上游仍可能在多轮时报 400（且兜底只能删不能补）→ 需新开会话。属数据不可逆损失，无法自动补全。
2. **错误文案启发式**：`classify_reasoning_400` 依赖关键词（`reasoning_content` / `thinking mode` / `passed back`）。若某上游用完全不含这些词的文案拒收该字段，则判为 `Unrelated` → 退化为「400 一次 + 重试」的今日行为。
3. **进程重启后重新学习**：粘性标记是会话级内存态，重启后拒收型端点会再撞一次 400 再自愈。
4. **上游校验粒度未实测**：本实现按「有思考就带、没有就不带」的最保守形态实现。若某上游要求**每条** assistant 消息都必须带该键（含空串），需追加「缺失时补空串」分支——建议用一条 curl 最小复现确认。
5. **8MB 上限**：落盘保留思考使压缩后体积变大，极端情况下（无图可剥且文本不可压缩）会走到「拒绝保存，请新开会话」。`trim` 预算（256k token / 保留末 2 轮）使其实际难以触达。
6. **既有 flaky 用例**（§6 末）：**已于 2026-09-21 修复**（分支 `test/provider-midstream-flaky`，成因与验证见 §6 末）。

## 9. 审查记录

| 阶段 | 结论 |
|---|---|
| 方案对齐审查（reviewer） | 「与方案完全对齐」；🔴 0；🟡：测试载荷生成逻辑重复、8MB 拒存分支零覆盖、注释瑕疵——均已修复 |
| 代码质量审查（code-reviewer，7 维度） | 发现 **🔴 1**：兜底「每 run 只生效一次」，拒收型端点在 step N+1 硬失败（阀门自述目的失效）→ 已改为会话级粘性并复审通过 |
| 复审（reviewer） | 「🔴 已修复且无新增 🔴 问题」；🟡：粘性标记无复位点 → 误判/切模型会短暂锁死会话——已补 `Demanded` 复位自愈 + 子代理继承，并补回归叙事用例 |
| 测试执行（tester） | 后端全绿、前端全绿；环境问题（pnpm 依赖未装全）与既有 flaky 均已归因隔离 |
