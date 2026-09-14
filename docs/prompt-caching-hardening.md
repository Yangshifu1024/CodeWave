# Prompt 缓存命中强化批次

> 工程化批次（2026-09-14，分支 `feat/prompt-cache-hardening`）。起因：V2EX 帖（Ally 项目）宣传
> 「高缓存命中」，对标确认后发现 CodeWave 三件套已全部落地，本批次做的是真实增量——封堵三个
> 实际拉低命中率的漏洞，并让命中率可感知。

## 1 对标结论

Ally（Go+Wails 桌面编码 agent）作者自述实现方式：「openai 的就看源码用 cache key，ds 就前缀
字节命中，claude 比较麻烦也是看 sdk 去贴合缓存的」——即各对接各家的标准缓存机制，无特殊技术。

CodeWave 对应现状（本批次前即已具备）：

| 机制 | Ally 说法 | CodeWave 落点 |
|---|---|---|
| Anthropic `cache_control` 断点 | 「看 sdk 去贴合」 | `provider/anthropic.rs`：system 块 + 末条非瞬态消息末块双断点（本批次扩展为 4 断点体系） |
| OpenAI Responses `prompt_cache_key` | 「看源码用 cache key」 | `provider/openai_responses.rs`：会话 id 作 cache key |
| DeepSeek/OpenAI-Chat 前缀字节命中 | 「前缀字节命中」 | 工具按名排序 + system 字节稳定（自动前缀缓存的前提） |
| 缓存 usage 采集 | —（未提） | `cache_read`/`cache_write` 三协议解析 → stats 落盘全链路 |

## 2 修复的三个漏洞

| 漏洞 | 描述 | 修复 |
|---|---|---|
| V1 | `build_stream_request` 每步重组 system prompt，六层中第 2-5 层（技能/记忆索引、项目指令文件、CODEGRAPH/lessons）每步磁盘重读——agent 运行中改了这些文件会静默打穿 system 前缀缓存 | **M1-A run 内字节冻结** |
| V2 | Anthropic 只用 2/4 断点：切档（plan-mode 注入 `system_extra`）打穿整个 system 缓存；长对话空闲 >5min（断点 TTL 过期）后只能命中 system 断点、全量重算 | **M1-B 断点重排 + M2 代际断点** |
| V3 | 统计面板看不到缓存命中，且总量口径 `input+output+cache_read` 混算——OpenAI 语义下 cache_read 是 input 子集，实际重复计数 | **M1-C 命中率可视化** |

## 3 实现要点

### M1-A system prompt run 内字节冻结

- `SessionRuntime.system_frozen: Mutex<Option<String>>`：`build_stream_request` 首步组装稳定
  主块后冻结，run 内复用；`drive_agent` 每个 run 清空（文件变更从「下一步生效」变「下一条用户
  消息生效」，与子代理 spawn 冻结语义一致）
- 同时省每步 10+ 文件重读与 16KB+ 字符串重建
- ⚠ 实现坑（已踩）：`match rt.system_frozen.lock().unwrap().clone()` 的 scrutinee 临时
  MutexGuard 活到 match 结束，None 分支内再锁同一把锁会自锁死锁——clone 必须先落到局部变量

### M1-B Anthropic 断点重排（2 → 3，预算 4 留 1）

1. `tools` 段末位工具 `cache_control`（字节稳定，独立缓存条目：主块跨 run 变更时 tools 段仍命中）
2. `system` 拆双块：`StreamRequest.system_core`（稳定主块，带断点）+ `system_extra`（可变段，
   不带断点）——切档只失效可变段之后的前缀；openai 两协议经 `system_full()` 拼接维持原字节
3. 末条非瞬态消息末块（既有）

### M2 历史代际断点

- `StreamRequest.cache_gen_index: Option<usize>` + `SessionRuntime.cache_gen_anchor` 滞回锚点：
  目标位 = min(n-8, 3n/4)，锚点漂移未超 1/4 时保持不动（稳定期间该前缀条目被每步请求命中刷新，
  5min TTL 不过期），超阈值才前移；历史不足 16 条不启用，压缩后 n 骤减自动重置
- 场景：长会话空闲 >5min 回来，末条断点已过期时命中次新代，避免全量重算
- anthropic.rs 消费：标注该内部消息产出的最后一条 wire 消息末块（内部消息可能展开/滤空产出
  0 条 wire 消息——此时静默跳过防错标，见 `convert_message` 前后长度对比）

### M1-C 统计面板命中率与口径修复

- 纯前端（`TokenStatsModal.tsx`）：新增「缓存命中 X tokens（命中率 Y%）· 缓存写入 Z tokens」；
  总量与柱状图改按 `by_model` 聚合（`total` 聚合无 model 维度无法归一口径）
- **口径关键点**：anthropic_messages 的 `input` 不含缓存（真输入 = input+read+write，命中率 =
  read/(input+read+write)）；openai_chat / openai_responses 的 `input` 已含 cached（命中率 =
  read/input）。由 model_id 反查 provider 级 `api_format` 决定公式；历史已删模型不参与比率
- 按来源拆分行改用 **output tokens**：输出语义跨协议一致，而 kind 聚合无 model 维度

## 4 明确不做（含重议判据）

- **OpenAI Responses `store:true` + `previous_response_id` 链式引用**：服务端留存完整对话与
  local-first/BYOK 隐私立场冲突；事实源挪到服务端与多 key failover / 模型切换 / 压缩重写历史 /
  会话导入导出相斥；成本收益已被 prompt_cache_key + 自动缓存拿走大半。重议：出现显式服务端
  留存换成本诉求时做 opt-in
- **Anthropic 1h extended TTL**：缓存写价 1.25x→2x（+60%），只在「空闲 >5min 回来」场景受益；
  与 M2 代际断点重叠；依赖 beta header。重议：命中率数据显示空闲重算占比显著且 M2 覆盖不足时
  做成高级设置项

## 5 验证

- 单测（后端 562 passed / 0 failed）：`system_prompt_frozen_within_run`（tempdir 实改文件，run 内
  字节不变、新 run 重组装）、`cache_gen_anchor_hysteresis`（滞回三段断言）+ 锚点纯函数边界
  （短历史禁用 / 越界重置）、`system_extra_block_and_gen_breakpoint`（双块结构与三断点位置）、
  `system_full_joins_core_and_extra`（拼接字节与拆分前一致）
- 门禁：`pnpm --dir ui test` 324 passed + `pnpm --dir ui build` ✓
- 实测路径：Anthropic 真实多步会话看 session_log 的 `cache r=/w=`（第二步起接近全命中）；切档
  一次观察失效范围收敛到可变段；统计面板手动核对接连两个工作日的命中率数值

## 6 手动验证清单（界面改动不做 GUI 自动点验）

1. 统计弹窗：有历史数据时出现「缓存命中 …（命中率 …）· 缓存写入 …」行；柱状图与合计数值正常
2. 旧数据兼容：升级前积累的 stats 日期照常显示（含已删除模型的日期不显示命中率行属预期）
