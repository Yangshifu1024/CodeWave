# 参与贡献 CodeWave

> 工作流规则与约定的速查版。权威来源是 [AGENTS.md](./AGENTS.md)（**改代码前必读**），本文只做贡献者视角的收敛。

## 快速定位

- **新功能 / 想法** → 需求流程：product-manager 分析 → 技术方案 → 批准后实施（AGENTS.md §需求流程）
- **缺陷 / 回归** → 缺陷流程：先分类，tester 复现与根因，修复 + 回归测试（AGENTS.md §缺陷流程）
- **多文件 / 跨层变更完成后** → code-reviewer 七维审查（AGENTS.md §代码审查流程）
- 环境准备见 [docs/dev-setup.md](./docs/dev-setup.md)；项目全貌与架构走读见 [README.md](./README.md)

## 分支与提交约定

- 分支自 `master` 拉出：`feat/<name>` / `fix/<name>`（可配 `git worktree`，仓库已有多个 worktree 并行的惯例）
- 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/)：`<type>(<scope>): <subject>`，type ∈ `feat` · `fix` · `docs` · `refactor` · `test` · `chore`
- **AI 协作纪律**：AI 不执行任何 git 写操作（commit / push / merge / branch / stash 全由人执行）；界面改动不做 GUI 自动点验，完成后交付分步手动验证清单

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

- 每个功能 / 修复批次落一份报告：`docs/<topic>.md`（英文主题 slug，平铺不编号），并在 [docs/0-README.md](./docs/0-README.md) 登记日期与条目
- 触及契约锚点（事件面 29 键、SessionMeta 快照、CI 系统依赖清单等）时同步更新对应文档
## 提交 PR

使用仓库内置模板 [`.github/PULL_REQUEST_TEMPLATE.md`](./.github/PULL_REQUEST_TEMPLATE.md)（GitHub 新建 PR 时自动填入），核心字段：概要、关联文档、类型、变更内容、验证清单、风险与迁移说明。
