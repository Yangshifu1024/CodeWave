# plan 档：gh 子命令白名单 + 拦截点位名被拦命令

> 类型：缺陷修复 + 小功能 · 影响层：`safety/fence`（判定与文案）· `core/agent`（plan 档提示）· docs
> 契约影响：错误码不变（仍 `E_PLAN_READONLY`）；`Verdict::Block.message` 文案变化（含被拦命令摘要）；事件面与前端零改动
> 起因：用户报告「被计划模式拦截时，应该明确显示是哪个命令被拦截；顺带将 gh 命令加入白名单」

## 1. 两个问题

1. **看不出是哪条命令被拦**：`E_PLAN_READONLY` 文案只有泛化句「命令不在只读白名单内…」，命令本体只在工具卡片正文里；卡片标题还会把命令截在半个 token 上（实测截图断在 `HTTPS_PROXY="http:`），结果/日志里无从判断。
2. **plan 档下 `gh` 完全不可用**：`gh` 不在 `PLAN_READONLY_CMDS`，任何 `gh` 调用一律 `E_PLAN_READONLY`——而 plan 档的核心工作（调研 PR、看 CI 结果、读 release 资产）恰好依赖它。

## 2. 根因与关键判断

- 拦截点：`safety/fence/check.rs` 的 G5 出口——`plan_readonly` 下一切 `Confirm` 转 `Block{E_PLAN_READONLY}`（[docs/plan-mode-workflow](./plan-mode-workflow.md) §7.1）。泛化文案来自白名单未命中分支。
- **为什么 gh 不能整命令放行**：L1-L3 安全网覆盖的是**文件写/重定向/已知高危命令**，**不覆盖 gh 的远端写**（`gh pr merge`、`gh release edit --draft=false`、`gh api -X POST`、`gh secret set`）——整命令放行等于让 plan 档能合并 PR、发布 release、改 secret，只读承诺作废。故名单下沉到**子命令级**。（补正：`git` 也是命令级白名单，L3 只在 `--force` 时兜底，故 plan 档 `git push`（无 force）本就放行——这是既有洞，不在本批范围。）

## 3. 改动

**`safety/fence/check.rs`**

- `PLAN_READONLY_CMDS` 增 `"gh"`（命令名入列只是第一关）。
- 新增 `PLAN_READONLY_GH_PAIRS`（二级形态：`pr view|list|checks|diff|status`、`run view|list`、`release view|list`、`issue/repo/workflow` 的 `view|list`、`secret/variable/label/cache list`、`auth status`、`config get`、`alias/extension/gist list`、`ruleset list|view`）、`PLAN_READONLY_GH_WORDS`（单词形态：`status`、`search`）。
- `gh_api_is_readonly`：只信显式的只读方法（`get`/`head`）；`-X POST|PUT|PATCH|DELETE`、`--method …`、`-f`、`--field`、`--raw-field`、`--input`（含 `=` 紧凑式与前置/后置位置）判写——**gh 在有参数且未显式 `-X` 时默认改用 POST**；方法值不可信（变量/未知写法/`-X` 后无取值）也判写。
- `normalize_gh_token`（审查返工）：比对前先去掉引号与转义并剥掉 `$(…)`/`` `…` ``/`${…}` 外壳再小写——否则 `-X "POST"` / `-X='POST'` / `--method "DELETE"` 能把写方法送进去（子命令门是唯一防线）。
- `split_unquoted_separators`（审查返工）：**换行也归为命令分隔符**——不切分的话 `gh pr view 38\ngh pr merge 38` 会被当成一段、子命令门只看段首 `pr view` 而放行（相对改动前是安全侧回归）。副作用见 §5。
- `gh_plan_readonly_allowed`：保守口径——认不出的形态（裸 `gh`、`gh --version`）一律不放行，落回拦截。
- `command_excerpt`：折叠换行与连续空白 → 截断 120 字符 → 超长补 `…`。
- G5 文案改为 `计划模式只读拦截（被拦命令：<摘要>）：<why>。请将该命令纳入方案…`；白名单未命中分支的 `why` 简化为「命令不在只读白名单内（ls/cd/head/grep/git log/gh pr view 等只读命令）」，避免与新摘要重复。

**`core/agent/drive.rs`**：plan 档提示补 gh 的放行口径（`gh` 按子命令放行；`pr merge`/`release edit`/`api -X POST`/`secret set` 会被拦）。

## 4. 测试（`safety/fence/tests.rs` 新增 7 例）

- 只读形态放行（含 `gh api -X GET`）：`gh pr view 38`、`gh pr checks 38`、`gh pr list --state open`、`gh run view --log`、`gh release view v0.3.10`、`gh api repos/o/r/pulls`、`gh api -X GET repos/o/r/pulls`、`gh api -X "GET" …`、`gh status`、`gh search repos codewave`。
- 远端写拦截且**错误信息点名命令**：`gh pr merge 38`、`gh release edit v0.3.10 --draft=false`、`gh secret set FOO --body bar`、`gh api -X POST …`（前置/后置/紧凑/引号四种写法）、`gh api --method "DELETE" …`、`gh api … -f title=x`、`gh workflow run release.yml`、`gh --version`。
- 管道/换行混合：`gh pr list | head -3` 放行；`… && gh pr merge 38`、`gh pr view 38\ngh pr merge 38` 与 `gh pr view 1 \\` + 换行 + `gh pr merge 2`（转义反斜杠后的换行）均整条拦截。
- 行继续：`grep -n foo \` + 换行 + `  file.rs`、CRLF 版本、双引号内续行均放行（归一化生效）。
- 摘要：超长命令被截断到 ≤120 字符并带 `…`；多行命令折叠为单行。
- 只读表自检：`PLAN_READONLY_GH_PAIRS`/`_WORDS` 不得出现写子命令（防后续维护误加 `pr merge` 之类）。
- **钉住既有缺口**：`echo $(gh pr merge 38)` 当前仍放行（见 §5），将来把门下沉到 AST 后该用例会变红。

## 5. 已知边界（取舍）

- `gh api` 带 `-f` 的 GET 查询参数会被判写而拦截（保守方向的误伤，逃生 = 改 `-X GET` + URL 查询串，或把命令写进方案批准后跑）。
- 引号/紧凑写法已在 `normalize_gh_token` 里归一，但**方法值来自变量**（`-X "$M"`）仍按写拦（静态不可判 → 保守）。
- **行继续已归一**：切分前先把 `\` + 换行（LF/CRLF）吃掉——引号外与双引号内都算续行（shell 语义），单引号内是字面量；`\\`（转义反斜杠）整体吞掉，保证其后换行仍是命令分隔符。因此 `grep foo \` + 换行 + `  file.rs` 这类多行只读命令在 plan 档可正常放行（原「被切段而多拦」的取舍已消除）。残留：PowerShell 反引号续行未归一（按字符区分不了 bash 反引号命令替换）。
- **命令替换/反引号内的 gh 写是既有缺口**（非本批引入，`echo $(npm i)` 同理）：`echo $(gh pr merge 38)` 当前仍放行。根治需要把「首词白名单 + gh 子命令门」从 L0 下沉到 AST 的每个 command 节点；本批只在测试里钉住现状 + 此处声明。
- 同类既有洞（不在本批范围）：`git` 是命令级白名单，L3 只在 `--force` 时兜底，故 plan 档 `git push`（无 force）本就放行。
- 认不出的 gh 子命令形态（含全局 flag 前置）一律拦截；宁可拦错不放过。
- `gh browse`/`gh repo clone` 等有副作用或落盘的形态不入名单。
- 错误摘要会把命令原文（≤120 字符）带进模型上下文与日志：与工具卡片/日志同一通道，无新增泄露面；若命令里带凭据（如 `-H "Authorization: …"`），后续可统一走一处脱敏。

## 6. 验证

- `cargo test --lib safety::fence` → 134 passed / 0 failed（含上述 9 例）。
- 全量 `cargo test --lib` → 682 passed / 1 failed / 3 ignored，唯一失败为既有 flaky `provider::tests_integration::midstream_disconnect_maps_to_network`（默认并行下偶发失败、单跑即过，与本批无关）。
- 审查轮：code-reviewer 实测出两个 🔴（引号包裹写方法、换行分隔回归）与若干 🟡，已全部修复（🟡 中「命令替换缺口」「git push 既有洞」仅改文案与声明，不修代码）。
- 手动（plan 档内）：`gh pr checks <n>` 应通过；`gh pr merge <n>` / `gh api … -X POST` 应被拦且错误信息含该命令。
