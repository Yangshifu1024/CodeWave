# 命令行威胁分析方案调研：mvdan.sh → tree-sitter（技术调研）

> 纯技术调研整理稿（无代码改动）；整理时联网补充了缺失的技术细节并一并核实了版本现状。

**标注约定**（全文适用）：

- **【会话】** —— 会话内已经外部数据源（go.mod 原文、crates.io API）实际核实的结论；
- **【补充】** —— 本文档整理时联网补充调研的内容（会话内未涉及或仅有描述性说明）。

## 一、背景与缘起

调研问题最初指向开源项目 [ally-agent](https://github.com/Bronya0/ally-agent)（Go + Wails 桌面 AI agent）：它用什么依赖对即将执行的命令行做威胁分析。会话在回答该问题的同时，顺带调研了 Rust 生态的等价方案与 tree-sitter 的技术细节。

这与 CodeWave 直接相关：`safety/fence` 命令安全围栏（[docs/p0-plan](./p0-plan.md) §7.2）解决的正是同一问题——在命令执行前静态判定其写语义与危险程度。第七章给出两者的对照与可选改进路径。

## 二、ally-agent 的 Go 方案：mvdan.cc/sh/v3

**结论【会话】**：ally-agent 用于命令行威胁分析的依赖是 **`mvdan.cc/sh/v3` v3.13.1**（来源：其 [`go.mod`](https://raw.githubusercontent.com/Bronya0/ally-agent/main/go.mod) 原文）。

[mvdan/sh](https://github.com/mvdan/sh) 自述为 "A shell parser, formatter, and interpreter"——即 [shfmt](https://github.com/mvdan/sh#shfmt) 格式化工具背后的库，支持 POSIX Shell、Bash、Zsh、mksh 四种方言。ally-agent 用它把 shell 命令解析为 AST，再做静态分析识别危险模式：`rm -rf`、`curl ... | sh`（管道到 shell）等，在命令执行前完成威胁检测【会话】。

**排除结论【会话】**：go.mod 中其余依赖均与威胁分析无关——`anthropic-sdk-go` / `openai-go` / `sashabaranov/go-openai`（LLM SDK）、`wails`（桌面 UI 框架）、`mcp-go`、`go-git` 等；release 包内打包的 `third_party/ripgrep` 仅用于代码搜索。

### 2.1 API 形态【补充】

会话内只有能力描述、无 API 细节，以下经 [pkg.go.dev/mvdan.cc/sh/v3/syntax](https://pkg.go.dev/mvdan.cc/sh/v3/syntax) 核实：

```go
// 解析：NewParser() + Parse(io.Reader, name) → AST 根节点 *syntax.File
f, err := syntax.NewParser(syntax.Variant(syntax.LangBash)).Parse(strings.NewReader(cmd), "")

// 遍历：深度优先；f 返回 false 可剪枝
syntax.Walk(f, func(node syntax.Node) bool {
    switch n := node.(type) {
    case *syntax.CallExpr:   // 简单命令：Assigns []*Assign + Args []*Word
    case *syntax.BinaryCmd:  // 二元连接（管道 | 、&& 、||）：X, Y *Stmt
    case *syntax.CmdSubst:   // 命令替换 $()：内嵌 Stmts []*Stmt，需递归检查
    }
    return true
})
```

结构要点：

- AST 根为 `*File`（`Stmts []*Stmt`）；`*Stmt` 携带 `Redirs []*Redirect`（重定向）与 `Negated`/`Background` 等标志；`Command` 接口的实现覆盖 `*CallExpr` / `*BinaryCmd` / `*IfClause` / `*Subshell` / `*FuncDecl` 等全部语句形态；
- 关键威胁面均有独立节点：`*CmdSubst`（`$(...)`/反引号）、`*ProcSubst`（进程替换）、`*ParamExp`（变量展开）；
- 错误恢复：`syntax.RecoverErrors(maximum)`（v3.11+）允许解析器尽力跳过最多 N 个语法错误继续解析——与 tree-sitter 的 error recovery 同一思路，面向的正是「不可信/残缺输入也要尽量出结构」的威胁检测场景。

## 三、Rust 生态等价方案对比

**核心结论【会话】**：Rust 生态**没有** `mvdan.cc/sh/v3` 的完整对等物（解析 + 格式化 + 解释一体）。实际可选方案按形态分两类：

| 方案 | 形态 | 下载量 | 维护状态 | 结论 |
|---|---|---|---|---|
| [`shlex`](https://crates.io/crates/shlex) 2.0.1 | POSIX 分词（无语法结构） | ~7.9 亿 | 2026-05 维护中 | 轻量规则匹配首选 |
| [`shell-words`](https://crates.io/crates/shell-words) 1.1.1 | 按 UNIX shell 规则切分 | ~1.4 亿 | 2025-12 | 同上备选 |
| [`conch-parser`](https://crates.io/crates/conch-parser) 0.1.1 | POSIX shell 完整 AST | ~2.4 万 | **2019-05 后停更** | 已死，不推荐新项目 |
| [`shrs`](https://crates.io/crates/shrs) | 完整 Rust shell（内含 parser） | — | — | 为「做一个 shell」而生，非分析库 |
| [`tree-sitter-bash`](https://crates.io/crates/tree-sitter-bash) | CST 解析（见第四章） | ~1,140 万 | tree-sitter 官方，2025-12 | **AST 威胁检测的最佳方案** |

> 注意：会话过程中修正过一次结论——最初推荐的 `conch-parser` 经 crates.io 数据核实实际已死（近期下载仅千余、七年未更新），更正为不推荐。

**分界线【会话】**：分词 + 规则匹配足以覆盖简单命令（命令名黑名单、危险 flag）；一旦涉及 `&&`、`|`、`$()`/反引号嵌套、引号内元字符等复合结构，词法切分会失真，需要真正的 AST 解析。

**业界现状【会话】**：多数 agent 产品（含不少 Rust 写的）实际用**规则 + 正则**（匹配危险二进制名、`rm -rf`、管道到 `sh` 等）做命令检测，并不真正解析 AST；只有需要处理复合命令时才上 AST。此为一句话结论，会话内未逐产品核实证据。

## 四、tree-sitter 深入

### 4.1 项目与能力【会话】

[tree-sitter](https://tree-sitter.github.io/tree-sitter/) 是解析器生成工具 + 增量解析库（作者 Max Brunsfeld，源起 Atom 编辑器），运行时为纯 C11、零依赖；官方绑定覆盖 Rust、Go、Python、Node、Wasm 等，官方组织维护约 25 个语法仓库（含 Bash）。

对威胁检测最关键的四项能力：

- **CST 具体语法树**：保留全部语法细节，节点带字节级位置——重定向目标、命令名可精确定位回原文；
- **语法错误恢复**：残缺/对抗输入也能解析出部分结构，不因一个非法 token 整体作废；
- **增量解析**：按键级实时重解析（编辑器场景的核心能力，一次性命令检测用不到）；
- **S-expression 查询系统**：以模式在树上批量匹配结构（下文 4.3）。

### 4.2 tree-sitter-bash 主要节点类型【补充】

会话内未给出 shell 语法树的具体形态，以下经官方仓库 [`src/node-types.json`](https://raw.githubusercontent.com/tree-sitter/tree-sitter-bash/master/src/node-types.json) 核实（威胁检测直接相关的部分）：

| 节点 | 说明 |
|---|---|
| `pipeline` / `list` | 管道 / `;` `&&` `\|\|` 序列，children 为 `_statement` supertype 展开（每段管道命令是一个 `command` 节点） |
| `command` | 简单命令；fields：`name`（→ `command_name`）、`argument`（多个）、`redirect`（→ `file_redirect` / `herestring_redirect`） |
| `command_substitution` | `$(...)`/反引号，children 为内嵌 statements（需递归检查） |
| `file_redirect` | 文件重定向；fields：`descriptor`（fd）、`destination`（目标 word） |
| `function_definition` / `variable_assignment` / `test_command` / `negated_command` | 函数体、赋值、`[[ ]]`、`! cmd` 等其余可携带执行语义的形态 |

### 4.3 危险命令检测查询示例【补充】

会话内只有通用查询语法示例（`(function_declaration name: (identifier) @func-name)`），以下是按上述节点类型写出的威胁检测模式（`.scm` 查询语法，仓库 [`queries/`](https://github.com/tree-sitter/tree-sitter-bash) 目录即此用法）：

```scheme
; ① 命令名提取（最基础的模式）
(command name: (command_name (word) @cmd))

; ② 删除命令携带递归 flag（rm -rf 家族）
(command
  name: (command_name (word) @del)
  argument: (word) @flag
  (#eq? @del "rm")
  (#match? @flag "^-[a-zA-Z]*[rR]"))

; ③ 命令替换 $()/反引号——内嵌执行点，命中后对其内部 statements 递归检查
(command_substitution) @subst

; ④ 重定向目标指向系统敏感路径
(file_redirect
  destination: (word) @dest
  (#match? @dest "^/etc/"))

; ⑤ 管道内出现 shell 解释器命令（curl … | sh 家族；此模式不区分位置，
;    「末段才是解释器」的分级需消费端再取 pipeline 的最后一个 command 判断）
(pipeline
  (command name: (command_name (word) @interp))
  (#match? @interp "^(b|z|da|k)?sh$"))
```

消费端注意：`#eq?` / `#match?` 等谓词的求值由**消费端**负责（tree-sitter CLI、Neovim 等宿主内建支持；Rust crate 侧需自行检查谓词）。因此做纯 Rust 的威胁检测时，另一种等效做法是**直接遍历 AST 节点 + 常规代码判断**——这正是 CodeWave fence 的选择（见 7.1）。

### 4.4 版本现状【补充】

| crate | 最新版本 | 发布日期 | 备注 |
|---|---|---|---|
| [`tree-sitter`](https://crates.io/crates/tree-sitter)（运行时） | 0.27.0 | 2026-08-30 | CodeWave 在用 **0.23** |
| [`tree-sitter-bash`](https://crates.io/crates/tree-sitter-bash) | 0.25.1 | 2025-12-02 | CodeWave 在用 **0.23** |
| [`tree-sitter-powershell`](https://crates.io/crates/tree-sitter-powershell) | 0.26.4 | 2026-05-04 | 见第五章 |

## 五、多 shell 语法覆盖

**各 shell 的 tree-sitter 解析器现状【会话】**：

| Shell | Crate | 下载量 | 成熟度 |
|---|---|---|---|
| Bash | [`tree-sitter-bash`](https://crates.io/crates/tree-sitter-bash)（**官方**） | ~1,140 万 | 最活跃，持续更新 |
| PowerShell | [`tree-sitter-powershell`](https://crates.io/crates/tree-sitter-powershell)（社区） | ~244 万 | 社区最活跃；可解析 cmdlet、管道、变量、脚本块；类 / DSC / splatting 等边角覆盖不全（另有 `tree-sitter-pwsh` 仅 6,378 下载，可忽略） |
| fish | `tree-sitter-fish`（社区） | ~10 万 | 较稳（fish 语法规整） |
| zsh | `tree-sitter-zsh`（社区） | ~7.6 万 | **最不成熟**：zsh 语法未完全形式化，全局别名 / 扩展 glob / zle 有解析缺口，更新不频繁 |

（另有 arborium 系列变体仓库，下载量级更小，作为备选来源。）

**落地建议【会话】**：Bash 用官方解析器；Windows 场景加 PowerShell；zsh/fish 大多兼容 Bash 语法，`tree-sitter-bash` 可覆盖绝大多数 agent 生成的命令；zsh/fish 专属语法在**解析失败或低置信时降级为分词 + 规则匹配**。

## 六、业界做法参考

【会话】多数 agent 产品（含不少 Rust 系）的命令威胁检测实际是**规则 + 正则**而非真正 AST——匹配危险二进制名、`rm -rf`、管道到 `sh` 等模式已覆盖绝大多数场景；AST 方案的增量价值集中在复合命令（`&&` / `|` / `$()` 嵌套）的精确判定。会话内未对 Claude Code、Cursor 等具体产品的实现取证，此结论证据有限，仅作方向参考。

## 七、CodeWave fence 对照

### 7.1 现状盘点

判定链位于 `src-tauri/src/safety/fence.rs` 的 `check_command_inner`（fence.rs:224）：

```
L0 计划只读白名单 → L3 灾难/高危（字符串启发，最高优先）→ tree-sitter bash AST → 解析失败降级 fallback_scan
```

| 层 | 实现 | 位置 |
|---|---|---|
| L0 计划白名单 | `PLAN_READONLY_CMDS`（POSIX + PowerShell 只读 cmdlet/别名段），引号感知切分 `split_unquoted_separators`（fence.rs:108）逐段判定；Confirm 经 G5 统一转 `Block E_PLAN_READONLY`（fence.rs:203） | fence.rs:80 / [docs/fence-plan-readonly-powershell](./fence-plan-readonly-powershell.md) |
| L1 删除黑名单 | `DELETE_CMDS` 纯词匹配 + `find -delete` / `xargs rm` 特判 | fence.rs:24 |
| L2 AST 写目标分析 | **真 tree-sitter bash 解析**（`tree_sitter::Parser` + `tree_sitter_bash::LANGUAGE`，fence.rs:266）；`walk` 遍历 `command` / `file_redirect`，写目标经 `check_write_target` 做规范化 + 符号链接逃逸检测；`sh -c` 内层递归（深度上限 2） | fence.rs:266–294 |
| L3 高危/灾难 | 小写化全文 token 启发：`disaster_match` / `high_risk_match`（`curl|sh`、`chmod 777`、`mkfs`、写 `/etc` 等） | fence.rs:671 |
| 降级 | 解析失败或 `root.has_error()` → `fallback_scan` 词法兜底（L1/L3 仍生效） | fence.rs:276 |

shell 形态（`src-tauri/src/tools/command.rs:64`）：macOS/Linux 用 `bash -lc`；Windows 先探测 Git Bash，找不到才回退 PowerShell。

### 7.2 选型验证：现状即调研推荐架构

本次调研反过来看是一次对 fence 现状的**外部验证**：

- fence 已经用 `tree-sitter-bash` 做真正的 AST 解析，而非业界常见的纯规则+正则——处在第六章「规则为主流」之上的一档，与「AST 用于复合命令精确判定」的建议一致；
- 命令名/写目标判定走 AST 遍历、`$()` 嵌套天然被 walk 覆盖、`has_error()` 整体降级词法兜底——与第五章「解析失败/低置信降级为分词+规则」的落地建议同构；
- `mvdan.cc/sh/v3` 的 `RecoverErrors` 限量错误恢复思路（2.1）比 fence 的 `has_error` 即整体降级更精细，但 fence 当前「整体降级 + L1/L3 兜底」的保守路线在安全场景下是合理取舍：宁可放弃 AST 精度，也不对残缺语法树上的节点位置做假设。

### 7.3 可选改进路径（讨论，非承诺）

1. **`tree-sitter-powershell` 补 Windows AST 缺口**。[docs/fence-plan-readonly-powershell](./fence-plan-readonly-powershell.md) 已记载：Windows 回退 PowerShell 时 bash AST 无法解析，L2 重定向扫描整体失效（`tee-object` 因参数式写目标对 L2 不可见而被排除出白名单，根因即此）。引入 `tree-sitter-powershell`（0.26.4，2026-05 仍活跃）后，重定向目标、变量参数可恢复 AST 级判定；但其 grammar 边角覆盖不全（类 / DSC / splatting），需配套「低置信降级 fallback_scan」策略——即**双解析器 + 各自降级链**，改动面集中在 `check_command_inner` 的解析入口与 `walk` 的节点类型适配。
2. **依赖版本跟进**。`tree-sitter` / `tree-sitter-bash` 在用 0.23，上游最新 0.27.0 / 0.25.1（4.4）。次版本升级包含 grammar 修正与误判修复，对安全组件有直接价值；升级时需全量回归 fence 测试（语义变化可能改变个别 verdict）。
3. **`shlex` 替换手写切分：不建议**。`split_unquoted_separators`（fence.rs:108）虽是手写，但携带特判规则（`2>&1` 的 `&` 不切、`&&` 第二个 `&` 必须切）；shlex 是完整 POSIX 分词器，语义并不等价，替换收益有限、回归风险实在——倾向保留现状。

## 八、来源清单

**会话内已核实**（sess_bd583d34）：

- ally-agent `go.mod`：https://raw.githubusercontent.com/Bronya0/ally-agent/main/go.mod
- crates.io API（下载量与维护状态）：`https://crates.io/api/v1/crates/{shlex|conch-parser|shell-words|tree-sitter-bash|tree-sitter-powershell|tree-sitter-zsh|tree-sitter-fish}`（关键词检索：shell parser / tree-sitter fish|powershell|zsh）

**会话外补充**（本文档整理时核实）：

- mvdan/sh README（包结构、方言、定位）：https://github.com/mvdan/sh
- mvdan.cc/sh/v3/syntax API 文档（Parse/Walk/节点类型/RecoverErrors）：https://pkg.go.dev/mvdan.cc/sh/v3/syntax
- tree-sitter-bash 官方仓库（组织归属、queries 目录）：https://github.com/tree-sitter/tree-sitter-bash
- tree-sitter-bash 节点类型定义：https://raw.githubusercontent.com/tree-sitter/tree-sitter-bash/master/src/node-types.json
- 版本现状：https://crates.io/crates/tree-sitter · https://crates.io/crates/tree-sitter-bash · https://crates.io/crates/tree-sitter-powershell
