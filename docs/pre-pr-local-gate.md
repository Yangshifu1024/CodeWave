# 开 PR 前本地门禁（pre-pr-local-gate）

> 类型：流程 / 工程化（无产品行为变更）· 影响面：`scripts/pre-pr.mjs`（新增）、root `package.json`、`AGENTS.md`、`CONTRIBUTING.md`、`.agents/skills/codewave-release/SKILL.md` · 契约影响：零（事件面 / config schema / IPC 均未动）。

## 1. 动因

2026-09-20 处理四个并行 PR 时实测：**#53 / #54 两个 PR 的 CI 失败原因都是 `cargo fmt --check`**（长表达式与 `assert_eq!` 未按 rustfmt 折行），修法各自只是一个 `cargo fmt`。这类问题本可以在推送前 30 秒内发现，却占用了两轮 PR 反馈周期与两轮三平台矩阵跑（每轮 ~10 分钟）。

结论：**CI 是复核，不是第一道防线**。开 PR 前必须在本地跑完 CI 会跑的全部检查。

## 2. 门禁内容（与 CI 逐条对齐）

CI 只有两个 workflow（`.github/workflows/lint.yml` + `test.yml`），除系统依赖安装外共 8 条检查命令：

| # | 步骤名 | 命令 | 工作目录 | 硬度 | CI 出处 |
|---|---|---|---|---|---|
| 1 | `lockfile` | `pnpm install --frozen-lockfile` | 仓库根 | 硬 | test.yml |
| 2 | `rust-fmt` | `cargo fmt --all -- --check` | `src-tauri/` | 硬 | lint.yml |
| 3 | `rust-clippy` | `cargo clippy --all-targets` | `src-tauri/` | **软** | lint.yml（`continue-on-error: true`） |
| 4 | `rust-test` | `cargo test --workspace` | `src-tauri/` | 硬 | test.yml（三平台 ×） |
| 5 | `ui-lint` | `pnpm --dir ui run lint` | 仓库根 | 硬 | lint.yml |
| 6 | `ui-test` | `pnpm --dir ui test` | 仓库根 | 硬 | test.yml（三平台 ×） |
| 7 | `ui-build` | `pnpm --dir ui build` | 仓库根 | 硬 | test.yml（三平台 ×） |
| 8 | `scripts-test` | `node --test "scripts/**/*.test.mjs"` | 仓库根 | 硬 | test.yml |

`clippy` 是唯一软步骤：CI 侧因 `#[cfg(test)]` / `tests/` 存量告警未清而 `continue-on-error`，本地同样只报告不阻断（发版 skill 的门禁说明与此一致）。

## 3. 用法

```bash
pnpm prepr                    # 全量（开 PR 前的标准动作）
pnpm prepr --only=rust-fmt,ui-lint   # 只跑指定步骤
pnpm prepr --skip-soft        # 跳过 clippy
```

行为：逐条执行 → 打印每步标题与结果 → 末尾汇总表（含总耗时）。**硬步骤失败即停**（后续步骤多依赖同一份代码，继续跑只是浪费与噪音），退出码 1 并列出失败清单；软失败只列名、不影响退出码。全部硬步骤通过时提示「可以开 PR」（并注明本地只覆盖当前平台）。

## 4. 为什么用数据 + 反向守门（而不是写死一串命令）

`scripts/pre-pr.mjs` 把步骤导出为纯数据 `PRE_PR_STEPS`；配套的 `scripts/pre-pr.test.mjs` 直接解析 `.github/workflows/*.yml` 的 `run:` 语句，做双向断言：

- **CI 跑了但本地没覆盖 → 测试红**（新增检查时被迫同步本地门禁）；
- **软/硬集合与 CI 的 `continue-on-error` 不一致 → 测试红**（当前断言「唯一软步骤是 clippy」）；
- 解析器本身有失效保护（解析出的命令数 < 5 即失败）。

即：这条纪律不是靠文档自觉，而是被测试钉住的。

实现细节：`spawnSync` + `shell: false` 逐参数执行（跨平台一致，Windows 上同样可用）；`scripts/**/*.test.mjs` 的 glob 作为**字面量参数**交给 node 自己展开——与 CI 的做法一致（CI 注释记录了原因：`node --test` 不认目录参数，而 pwsh 不会为原生命令展开通配符）；`runCommands()` 会跳过 YAML 块指示符（`run: |` 的 `|`）并合并反斜杠续行，避免把系统依赖安装脚本误判成检查项。

## 5. 验证

| 项 | 结果 |
|---|---|
| `node --test "scripts/**/*.test.mjs"` | **20 passed**（新增 5 条：CI 覆盖度、软硬一致、字段完整性、summarize、selectSteps） |
| `pnpm prepr` 实跑 | 见 §6 实跑记录 |
| `pnpm --dir ui build` / `cargo test` | 未受改动影响（本批次只加脚本与文档） |

## 6. 首轮实跑记录（2026-09-20）

在 `fix/tool-card-live-key` 上 `pnpm prepr`：

- 全部硬步骤通过；`rust-clippy` 亦无告警输出。
- 同时用它复核了四个在途 PR 的分支：#52 本就全绿；#53 与 #54 的 `rust-fmt` 报出差异（即 CI 报的那几处），各补一个 `chore(lint): cargo fmt 重排 quota 模块` 提交后本地门禁转绿。

## 7. 边界与已知取舍

- **平台覆盖**：本地只跑当前平台（macOS），CI 的 ubuntu / windows 差异仍以 CI 为准——「本地全绿 → CI 全绿」是预期而非保证。若将来需要，可在 CI 里加一个 `pnpm prepr --skip-soft` 的 job 使两侧完全同源。
- **耗时**：全量本地门禁约数分钟（主要在后端编译与测试）；`--only` 可按改动面收窄，但**开 PR 前建议跑全量**。
- **不自动触发**：门禁是显式命令，未挂进 git hooks（hooks 会在 rebase / 多分支切换等场景反复打扰，且团队成员环境不一）；纪律由 `AGENTS.md` + `CONTRIBUTING.md` + 本文件约定，由测试守门覆盖度。
