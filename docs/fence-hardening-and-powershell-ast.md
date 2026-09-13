# fence 加固批次：PowerShell AST + 反混淆 + 掩码 L3（批次报告）

> 来源：[docs/shell-threat-analysis-survey](./shell-threat-analysis-survey.md) 对 pi_agent_rust / dcg 命令威胁处理的对照调研，落地其中四项借鉴；同步引入 `tree-sitter-powershell` 补 [docs/fence-plan-readonly-powershell](./fence-plan-readonly-powershell.md) 记载的「Windows 回退 PowerShell 时 AST 整体失效」缺口。批次经 code-reviewer 强制审查（🔴×4 全修复），基线为 cfe8e88。

## 一、缺陷与缺口（改动前）

1. **auto_confirm 可自动放行灾难级 Confirm**：[docs/session-nav-row-states](./session-nav-row-states.md) 的「5 分钟未响应自动确认」在 `tools/command.rs` / `tools/service.rs` 两个调用点无条件透传——`mkfs`、`dd of=/dev/sda` 的 Confirm(Disaster) 弹窗无人响应 300 秒即被自动允许，与 fence「灾难级 even under full access 直接拦」的设计意图矛盾（`allow_always` 与「始终允许」白名单早已豁免 disaster，唯独漏了 auto_confirm）。
2. **命令名混淆形态全链路绕过**：命令名取 `command_name` 节点原文比对黑名单，`r"m" -rf /`（引号拼接）、`'rm' x`（raw_string）、`\rm`（转义）、`rm${IFS}-rf`（IFS 展开分词）四类合法 bash 形态均判 Allow，且解析成功不走 fallback、L3 模式亦不含 rm。
3. **`git push --force-with-lease` 误报**：`high_risk_match` 为子串包含，`--force-with-lease`（安全变体）命中 `--force` → Confirm(HighRisk)。
4. **L3 全文匹配的字面量误报**：`git commit -m "fix git push --force handling"`、`echo "用 mkfs 前先备份"` 等字符串散文触发 Confirm。
5. **PowerShell 命令无 AST 判定**（[docs/fence-plan-readonly-powershell](./fence-plan-readonly-powershell.md) 已记载）：bash 语法树解析失败的 PS 原生语法（`if ($x) {…}`、脚本块）整体降级词法兜底；且能被 bash 语法吞下的 PS 命令（`new-item`、`out-file` 等参数式写）在任何路径都无写语义判定。
6. **`mkdir` 区外创建不判定**：mkdir 不在任何写语义表，`mkdir /etc/evil` 恒 Allow（bash/PS 同）。

## 二、改动清单

| # | 改动 | 位置 |
|---|---|---|
| 1 | `effective_auto_confirm(auto_confirm, is_disaster)`：灾难级 Confirm 永不 ride 自动确认超时；两个审批调用点接入 | safety/approval.rs、tools/command.rs、tools/service.rs |
| 2 | 命令名反混淆 `deobfuscate_name`（`${IFS}`/`$ifs`→空格、剥 `'`/`"`/`\`、取首个空白分词），应用于 bash 命令名、`strip_transparency` 真实名、`xargs` 参数、L1 词法网；`expand_ifs` / `l1_token_scan` 提取为共享助手 | safety/fence.rs |
| 3 | 强推判定改 token 级（`--force`/`-f`/`--force=` 命中，`--force-with-lease` 排除）；补 PowerShell 下载执行模式（iwr/irm/invoke-webrequest/invoke-restmethod 管道到 iex/invoke-expression） | safety/fence.rs `high_risk_match` |
| 4 | **掩码 L3**：`mask_literals` 将字面量节点（bash raw_string/comment、无命令替换的 string；PS 无 `sub_expression` 的 string_literal/expandable_string_literal/here-string）字节置空格，解析成功后 L3 跑在掩码文本上；解析失败仍走原文 fallback（保守方向） | safety/fence.rs `check_command_inner` |
| 5 | **PowerShell AST 路径**：解析链 = bash 语法（error 树过滤）→ PowerShell 语法 → fallback；`walk_ps`/`handle_command_ps`（command_name/command_name_expr 名字、`redirection → redirected_file_name` 写目标、`powershell -Command` 递归入 bash sh 族且大小写无关、`& $var`/变量命令名保守升级）；PS 分支保留 `l1_token_scan` L1 兜底网 | safety/fence.rs |
| 6 | 写语义表扩容：`WRITE_ALL` += mkdir/md；`WRITE_LAST` += copy/cpi/mi/copy-item/move-item；新增 `WRITE_FIRST`（set-content/add-content/new-item/out-file/tee-object，**全**非 flag 参数判定） | safety/fence.rs |
| 7 | **check_write_target 边界重构**：词法归一优先的 lex_inside/canon_inside 四象限（见 §四） | safety/fence.rs |
| 8 | 依赖升级：tree-sitter 0.23→0.26、tree-sitter-bash 0.23→0.25、新增 tree-sitter-powershell 0.26（其运行时仅依赖 `tree-sitter-language ^0.1`） | Cargo.toml |

## 三、code-reviewer 审查与修复记录

审查按 7 维度（多文件/跨层强制流程），🔴 必修全部落地：

- **🔴-1 L3 掩码的系统性不对称**：掩码使命令名可被引号化逃过 disaster/high-risk 分类（`"shutdown" -h now`、`"mkfs.ext4" /dev/sda1`、`echo x > "/etc/new"` 旧为 Disaster、新曾静默 Allow）。修复：name 级判定表下沉 `eval_command_with`（shutdown/reboot/halt/poweroff、mkfs 前缀、`dd of=/dev/`、init 0/6、chmod 777 族、git push 强推族，全部 key 在去混淆后的 AST 名字上）；`file_redirect`/`redirection` 对去引号目标补 `/etc/` 灾难判定；下载工具与解释器名以 walk 标记（`saw_download`/`saw_interp`）在 walk 后关联升级——标记在 `handle_command` 顶部与 `eval_command_with` 双点设置（sh 族短路 return 不经过后者，裸 `| sh` 必经前者；幂等）。
- **🔴-2 lex/canon 命名空间错位**：`fs::canonicalize` 在 Windows 产 verbatim `\\?\` 形态、macOS `/var→/private/var`——用户形态 cwd 拼出的词法路径对 canonical roots `starts_with` 恒 false，逃逸硬 Block 与悬空链接扫描沦为死代码，junction 逃逸相对旧代码回归。修复：相对目标改挂 `canonical_best_effort(cwd)` 再词法归一；新增 `cfg(windows)` junction（`mklink /J`）逃逸测试钉死（t29）。
- **🔴-3 `..` 穿跃不存在目标旧 Block → 新 Confirm(OutsideCreate)**：旧 Block 是祖先循环误报的产物（词法 `..` 穿越 ≠ 符号链接逃逸），按「零放宽」标准显式签核为语义统一（见 §四）；t04 保持预创建目标（区外已存在 → Block 仍成立），新增 t30 钉死「不存在 → Confirm(OutsideCreate)」。
- **🔴-4 PS 路径丢失 fallback 的 L1 兜底网**：`& 'rm' x`（command_name_expr）、`iex "rm $f"`（名字藏于字符串）旧经 fallback 词法 L1 命中，AST 路径曾放行。修复：`l1_token_scan` 接入 PS AST 分支 + 名字节点识别扩至 `command_name_expr`。
- **🟡 全处理**：powershell/pwsh 入 bash sh 族递归 + `-Command` 大小写无关（内层字符串参数已在收集链去引号）；WRITE_FIRST 改全非 flag 参数判定（`-ItemType file` 值形 flag 不再遮挡 `-Path` 真目标，过拦方向可接受）；掩码表与 PS 参数收集补双引号/here-string 形态；测试盲区按清单补齐（t31–t38）。
- **🟢 采纳**：`expand_ifs` 提取共享（含 IFS-unset 近似注释）、`$null` 重定向豁免（对齐 `/dev/null`）、`v_push_ps` 成对剥引号。

## 四、安全边界（写语义零放宽）

- **唯一 Block→Allow 语义变化（显式签核）**：`../` 穿越根目录且目标不存在，旧 Block（E_PATH_OUTSIDE「符号链接逃逸」）→ 新 Confirm(OutsideCreate)。论证：旧 Block 是祖先 canonicalize 循环的误报——该循环对任何 `../` 上穿路径必然经过根目录自身（canonical 在 roots 内即判逃逸），词法 `..` 穿越是普通路径语义而非链接改道；新语义与「区外新建」统一（confirm_outside_create 可拦、FullAccess 下与其它区外新建行为一致）。双钉：t04（区外已存在 → Block 不变）、t30（区外新建 → Confirm）。
- **收紧项（Allow→Confirm/Block）**：mkdir/md 入 WRITE_ALL；WRITE_FIRST 五个参数式写 cmdlet 全参数判定；PS 路径 L1 词法网；`/etc/` 去引号目标灾难判定；下载×解释器 AST 关联；`/dev/null` 同权豁免 `$null`（PS 对齐，不属放宽）。
- **逃逸检测语义**（check_write_target 四象限）：lex 内 + canon 外 → Block 符号链接逃逸；lex 内 + canon 内 → 根下组件链接扫描（任何中间链接解析不出/解析出根即 Block，补旧代码悬空链接不设防的缺口）后按 confirm_inside_writes 走 InsideWrite/Allow；lex 外 + canon 内 → 按真实落点走根内流；lex 外 + canon 外 → 已存在 Block / 否则 confirm_outside_create 决定 OutsideCreate/Allow。旧 parent_inside 尾支在新语义下不可达，删除。
- **已知残差**：IFS-unset 运行时 `r${IFS}m` 拼接形态（文本防御通行近似，注释已记）；PS 变量命令名（`& $cmd` → 保守 Confirm/Block）；值形 named flag 可能在根内多报无害目标（过拦方向）；PS here-string 经管道喂解释器的内层脚本不做 AST 下钻（与 bash heredoc 同类残差）。

## 五、测试

新增 fence 用例 t16–t38（13 + 10）与 approval `effective_auto_confirm_never_applies_to_disaster`：

| 用例 | 断言 |
|---|---|
| t16/t17/t18 | `r"m"` / `'rm'` / `rm${IFS}` 反混淆 → Block |
| t19/t20 | `--force-with-lease` → Allow；`-f` → Confirm(HighRisk) |
| t21/t22 | 引号散文不触发 L3；含 `$( )` 的字符串保持扫描 |
| t23/t38 | `iwr \| iex`、引号 `"curl" \| sh` → Confirm(HighRisk) |
| t24/t26/t27/t35/t30 | PS 重定向 / out-file 全参数 / mkdir 区外新建 → Confirm(OutsideCreate) |
| t25/t33/t36/t37 | PS 脚本块删除、`& 'rm'`、PS 混淆删除（L1 网）、双解析失败 fallback → Block |
| t28/t31/t32/t34 | 区内 mkdir → Allow；引号 shutdown / 引号 `/etc/` 目标 → Confirm(Disaster)；`powershell -Command` 递归 → Block |
| t29（cfg(windows)） | junction 逃逸 → Block |

验证统计：`cargo test` **393 passed / 0 failed**（3 ignored 为既有手动探针），其中 fence 模块 57 用例；改动文件 clippy 零警告（service.rs 余留两处为已提交代码的既有警告，不在本批 diff）。

## 六、来源

- 对照调研：[docs/shell-threat-analysis-survey](./shell-threat-analysis-survey.md) §7（pi_agent_rust `bash_mediation.rs` / `approval.rs` / `exec_mediation.rs`；[destructive_command_guard](https://github.com/Dicklesworthstone/destructive_command_guard)）
- 语法依据：[tree-sitter-powershell node-types](https://github.com/airbus-cert/tree-sitter-powershell)（command/command_name_expr、redirections→redirected_file_name、string_literal 族）
- 前置：[docs/fence-plan-readonly-powershell](./fence-plan-readonly-powershell.md)（PowerShell 白名单与 L2 失效记载）、[docs/session-nav-row-states](./session-nav-row-states.md)（auto_confirm 语义）、[docs/composer-toolbar-batch-report](./composer-toolbar-batch-report.md)（审批档位语义表）
