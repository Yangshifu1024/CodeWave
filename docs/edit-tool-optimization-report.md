# edit 工具优化批次报告（worktree: feat/tools-edit-optimization）

> 批次内容：[docs/tools-optimization-and-gap-fill-plan](./tools-optimization-and-gap-fill-plan.md)「工作项 1（EOL 归一 + 两级模糊替换）+ 工作项 2（进程级写互斥）+ 工作项 3（ConfirmEach 审批 diff 预览）」在独立 worktree（分支 `feat/tools-edit-optimization`，自本地 master `1e492d3` 切出）实施完成。
> edit 入参 schema 零变化；registry 契约测试与前端零改动；未做任何 git commit（改动留待用户审查提交）。

## 1. 改动清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/tools/writelock.rs`（新增，~70 行含测试） | 进程级文件写互斥：`OnceLock<Mutex<HashMap<canonicalPath, Arc<tokio::sync::Mutex<()>>>>>`；`acquire_all` 排序去重依次获取（防死锁）。batch 层 per-runtime `file_ops` 保持不动（session 内串行 vs 进程域串行两层正交） |
| `src-tauri/src/tools/edit.rs` | ① `detect_eol` + EOL 归一匹配（内容/oldText/newText 统一 LF 预演，写回按原 EOL；version 令牌仍按原始字节）② 两级模糊替换器 `fuzzy_indent_flex`（整块缩进平移，保留块内相对缩进）→ `fuzzy_line_trimmed`（逐行 trim），仅精确命中 0 次时从严到宽尝试，唯一命中才替换并附 warning ③ 小项：`oldText == newText` 报错；lineRange 替换首行时 UTF-8 BOM 转移 ④ `apply_changes` 签名 `Result<(String, Vec<String>), String>`（warnings 回流模型）⑤ 写入段（读→预检→预演→备份→倒序写→回滚）整体移入进程级锁内（锁内重读最新内容，消除跨 runtime TOCTOU）⑥ 新增 `edit_approval_detail` 纯函数 + `unified_diff`（similar）+ trait `approval_detail` override |
| `src-tauri/src/tools/create.rs` | E_EXISTS 检查与写入移入进程级锁；`approval_detail` override（已存在→新旧 diff；新文件→标题+内容头 2000 字符预览） |
| `src-tauri/src/tools/delete.rs` | 删除操作移入进程级锁（与 edit/create 写路径互斥） |
| `src-tauri/src/tools/mod.rs` | `pub mod writelock;`；trait `Tool` 新增默认方法 `approval_detail(&ctx, &args) -> Option<String>`（默认 None） |
| `src-tauri/src/tools/batch.rs` | ConfirmEach 预弹窗 detail 改调 `tool.approval_detail()`，None 回退原 JSON dump（delete 等无预览工具不受影响） |

## 2. 关键设计决策

1. **模糊替换器顺序从严到宽**：`fuzzy_indent_flex`（只剥块公共缩进，块内相对缩进必须一致）先于 `fuzzy_line_trimmed`（逐行 trim，更宽容）——优先命中结构保真的匹配。两级均要求**唯一命中**，多命中直接报错「需加长上下文」（绝不猜）；零命中回落 lineRange 兜底/报错（原语义不变）。明确不做 Levenshtein/转义归一等激进策略（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §15 不移植清单）。
2. **锁内重读闭环**：edit 的预演不放锁外——跨 runtime（arch 并行 dev 子代理 ≤4、计划任务 runtime、多会话）并发写同一文件时，后进锁者基于前者写入后的最新内容重新预演，version 冲突走既有 stale 软降级/E_VERSION_STALE 语义，从「原子写但后写覆盖先写」变为「串行 + 后者复检报错重读」。
3. **`similar` 启用**：Cargo.toml 既有 `similar = "2"`（此前零使用的死依赖），`unified_diff` 用 `TextDiff::from_lines` + `grouped_ops(3)` 渲染 `+/-/空格` 前缀 diff；审批弹窗 detail 在前端 `<pre className="ask-detail">` 等宽展示（`AskPanel.tsx:203`），**前端零改动**。
4. **审批预览与执行同源**：`edit_approval_detail` 复用 `apply_changes` 同一预演链路，保证「预览的 diff = 实际执行的变更」；预演失败返回 None 回退 JSON dump（弹窗永不为空）。

## 3. 测试结果

- `cargo test`（worktree 内，共享主工作区 target 目录）：**262 passed / 0 failed / 2 ignored**（ignored 为真实 GLM E2E，按约定 `-- --ignored` 显式运行）；`cargo build` 0 warning。
- 新增测试 11 个：
  - edit：`crlf_file_edited_with_lf_oldtext_roundtrip`（跨行 LF oldText 编辑 CRLF 文件、写回保持 CRLF）、`fuzzy_indent_shift_matches_uniquely`、`fuzzy_line_trailing_whitespace_matches`、`fuzzy_ambiguous_rejected`（模糊 2 处拒绝）、`oldtext_equal_newtext_rejected`、`bom_transferred_on_line1_line_range`、`fuzzy_match_warning_surfaces_in_outcome`（端到端 warning 回流）、`approval_detail_renders_unified_diff`、`approval_detail_none_when_apply_fails`、`concurrent_runtimes_editing_same_file_serialize`（主+子 runtime 并发编辑同文件不同片段，两处变更都生效——无锁时必丢一处）
  - writelock：`different_paths_do_not_block_each_other`（防死锁）、`same_path_second_acquire_waits`
- 既有测试：`unique_oldtext_replaces`/`line_range_replaces_block` 因返回值携带 warnings 补 `.0`；`full_edit_flow_with_rollback`（stale 三态）、产物登记、batch ConfirmEach/G3 等全部零修改通过。
- 环境备注：worktree 初次运行时两个真实 MCP E2E（`real_stdio`/`real_streamable_http`）因缺 Node 依赖失败——`pnpm --dir ui install` 后通过（与本次改动无关，主工作区同测试原本即通过）。

## 4. 已知局限（留档）

- 混合 EOL 文件被整体归一为 CRLF；UTF-8 BOM 仅在 lineRange 整行替换首行时转移，oldText 部分替换场景 BOM 天然保留在匹配段之外。
- 模糊命中时 newText 采用模型原文（缩进以模型输入为准），warning 中已提示模型自查。
- 审批预览会双跑预演（batch 层一次 + edit run 内一次），小文件场景开销可忽略；两次之间文件被外部改动时以 run 内预演为准。
- 模糊匹配仅覆盖「缩进平移」与「行首尾空白」两类最高频失配；内部空白差异（如 `fn  a` vs `fn a`）仍报错（保守取舍）。

## 5. 手动验证清单（GUI，按仓库约定由用户执行）

1. **M-1 审批 diff**：ConfirmEach 档发起 edit（多文件多 change）→ 审批弹窗 detail 应展示 `### <path>` 分节的 `+/-` diff（等宽字体）；create 覆盖已有文件同样展示 diff，新文件展示内容预览。
2. **M-2 CRLF 编辑**：找一个真实 CRLF 仓库文件，用 LF oldText（跨行）编辑成功且文件保持 CRLF。
3. **M-3 并发写**：并行两个子代理编辑同一文件（各改不同片段）→ 两处变更都生效；编辑同一段 → 后者报 `E_VERSION_STALE`（而非静默覆盖）。
4. **M-4 模糊提示**：缩进层级给错的 oldText 编辑 → 成功且模型侧收到「缩进平移归一」warning。

## 6. 建议提交切分（git 由用户执行）

1. `feat(tools): edit EOL 归一匹配 + 两级低风险模糊替换 + old==new/BOM 小项`（edit.rs 的匹配部分）
2. `feat(tools): 进程级文件写互斥（跨 runtime TOCTOU/覆盖防护）`（writelock.rs + edit/create/delete 接入）
3. `feat(tools): ConfirmEach 审批 diff 预览（edit/create）`（trait approval_detail + batch 接入）
