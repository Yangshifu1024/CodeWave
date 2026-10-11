# 参与贡献 CodeWave

> 工作流规则与约定的速查版。权威来源是 [AGENTS.md](./AGENTS.md)（**改代码前必读**），本文只做贡献者视角的收敛。

## 快速定位

- **新功能 / 想法** → 需求流程：product-manager 分析 → 技术方案 → 批准后实施（AGENTS.md §需求流程）
- **缺陷 / 回归** → 缺陷流程：先分类，tester 复现与根因，修复 + 回归测试（AGENTS.md §缺陷流程）
- **多文件 / 跨层变更完成后** → code-reviewer 七维审查（AGENTS.md §代码审查流程）
- 环境准备见 [docs/development.md](./docs/development.md)；项目全貌与架构走读见 [README.md](./README.md)

## 分支与提交约定

- 常规贡献建议以 `main` 为基线，使用 `feat/<name>` / `fix/<name>`；已批准计划按方案列明的当前 HEAD/分支创建，不能据此擅自切回 main。可用 `git worktree` 隔离并行工作，写操作仍遵循下方授权规则。
- 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/)：`<type>(<scope>): <subject>`，type ∈ `feat` · `fix` · `docs` · `refactor` · `test` · `chore`
- **AI 协作纪律**：git 写操作须按用户当次明确授权执行；批准计划可预授权计划所示分支，commit / push / merge 仍需相应授权；界面改动不做 GUI 自动点验，完成后交付分步手动验证清单

## 本地质量门槛

两边都动 = 两者都要过，才算完成：

```bash
# 后端（src-tauri/ 下执行）：全绿 + 0 warning
cargo test

# 前端（仓库根执行）：测试 + 类型检查 + 构建
pnpm --dir ui test
pnpm --dir ui build
```

补充纪律：

- 界面改动交付分步手动验证清单（不依赖截图自动比对）
- 改到事件键、`Message`/`Content`、config schema 时，确认 serde default 兼容旧数据与契约测试

## 文档纪律

- 优先更新 `docs/` 已有长期主题指南；含索引不超过 20 篇。不要为每次修复新增报告；有效契约与未完成验证归主题，已完成计划、一次性验收及视觉微调由 Git 历史保留。目录见 [docs/0-README.md](./docs/0-README.md)
- 触及契约锚点（事件面 29 键、SessionMeta 快照、CI 系统依赖清单等）时同步更新对应文档

## 开 PR 前的本地门禁

**开 PR 前必须本地跑完全部 CI 检查**，由 `pnpm prepr` 一条命令完成（逐条对照 `.github/workflows/` 的 `run:` 语句执行；`scripts/pre-pr.test.mjs` 反向守门：CI 新增检查而本地未跟上会测试红）：

| 步骤 | 命令 | 硬度 |
|---|---|---|
| lockfile | `pnpm install --frozen-lockfile` | 硬 |
| rust-fmt | `cargo fmt --all -- --check` | 硬 |
| rust-clippy | `cargo clippy --all-targets` | 软（CI 亦 `continue-on-error`） |
| rust-test | `cargo test --workspace` | 硬 |
| ui-lint | `pnpm --dir ui run lint` | 硬 |
| ui-test | `pnpm --dir ui test` | 硬 |
| ui-build | `pnpm --dir ui build` | 硬 |
| scripts-test | `node --test "scripts/**/*.test.mjs"` | 硬 |

- 硬步骤失败即停，并给出失败清单；`--only=a,b` 只跑指定步骤，`--skip-soft` 跳过 clippy。
- 本地只能覆盖当前平台；三平台矩阵（macos-14 / ubuntu-24.04 / windows-2022）差异仍以 CI 为准。
- 详见 [docs/testing.md](./docs/testing.md)。
## 提交 PR

使用仓库内置模板 [`.github/PULL_REQUEST_TEMPLATE.md`](./.github/PULL_REQUEST_TEMPLATE.md)（GitHub 新建 PR 时自动填入），核心字段：概要、关联文档、类型、变更内容、验证清单、风险与迁移说明。
