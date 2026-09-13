# 开源准备批次：全量 i18n + AI 回复语言 + 旧版本兼容移除 + 文档/注释英译 + 测试扩容（v0.2.0）

> 目标：面向 GitHub 开源发布的整备批次。回归基线：`cargo test` 350 passed / 0 failed / 0 warning、`pnpm --dir ui test` 220 passed（33 文件）、`pnpm --dir ui build` 通过。

## 1. 开源基建

- `.github/` 全套（参考 GitWave）：`workflows/ci.yml`（三平台测试+构建，并发取消 + docs 路径过滤）、`workflows/release.yml`（tag `v*` → 三平台 tauri 构建上传 draft Release，人工发布）、`workflows/lint.yml`（fmt/clippy，起步非阻断）、`dependabot.yml`（cargo/npm/actions 周检）、Issue 模板 ×2、PR 模板。
- 根目录：`LICENSE`（MIT）、`CONTRIBUTING.md`。
- GitLab 相关配置（`.gitlab-ci.yml` / `.gitlab/`）决策不采用，已移除。

## 2. 文档清洗（竞品引用清零）

- [docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) 整体重写为「内置工具设计说明」（单侧解析，外部对比与源码引用全部移除，章节锚点保持）；[docs/tools-optimization-and-gap-fill-plan](./tools-optimization-and-gap-fill-plan.md) 依据改写；[docs/tool-optimizations-port](./tool-optimizations-port.md) 重命名为 `tool-optimizations-port` 并去除外部项目/个人路径标注；[docs/composer-shift-tab-mode-cycle](./composer-shift-tab-mode-cycle.md)/37/39/43 及 AGENTS.md 索引同步清洗。全仓 opencode/ally/zcode/Claude Code 习惯类引用 0 残留（`~/.claude/skills` 生态兼容与 `CLAUDE.md` 项目指令为功能事实，保留）。

## 3. 代码注释英译

- 全部 144 个含中文注释的源文件（Rust 72 + TS/TSX 72）翻译为英文，约 1900 行；补充缺失的公共函数文档注释。
- 保留项：字符串字面量（UI 文案/错误消息/日志）、测试断言、i18n 字典、`docs/NN` 引用编号、agents/skills 的中文 prompt 数据。

## 4. 全量 i18n（中英双语完备）

- 新增命名空间与键约 120 个（`notice`/`queue`/`rightbar`/`nav`/`files`/`diagrams` 等），stores 与组件的硬编码中文 UI 文案全部接入 `t()`；`en-US.ts` 补齐至与 `zh-CN.ts` 对称。
- 非 React 上下文（stores/utils）统一 `import { i18n }`（named export，无默认导出）。

## 5. AI 回复语言（新功能）

- `UiPrefs.ai_language: Option<String>`（设置 → 通用 → AI 语言，自由输入）；`core/prompt.rs::assemble` 注入 `<reply-language>` 指令，显式覆盖默认「跟随用户消息语言」行为；空/未设置不注入。
- 测试：Some/None/空串/纯空格四态断言。

## 6. 旧版本兼容移除（全新发布，[docs/oss-prep-batch](./oss-prep-batch.md) 后不再新增迁移代码）

- config：schema v1 扁平 `models` → v2 迁移（`migrate_legacy_models` + load 落盘）、`ModelConfig` legacy serde、`ApiFormat` snake_case 旧别名（`open_ai_chat`/`open_ai_responses`）、`ProviderConfig.legacy_key_accounts` 全部移除。
- keyring：legacy 按模型账户回读/合并移除（明文 → 钥匙串的 `migrate()` 为现行安全功能，保留）。
- projects：legacy 目录（`~/.codewave/projects/<id>`）兜底加载/清理、`ProjectEntry.directories` 字段、`project_dir` 兼容函数移除；`save_project` 归一化持久化 `data_dir`（None → `<primary>/.codewave`），`data_dir_by_id` 成为唯一按 id 解析入口（scheduler/context/agent 已切换）。
- 前端：`ProjectEntry.directories` / `legacy_key_accounts` 只读字段及全部消费点（ProjectNav ×3、Composer）移除。
- ⚠️ 升级影响：旧格式本地数据（v1 扁平模型、legacy 项目、旧 keyring 账户链）不再迁移，相关配置需重新设置。
- AGENTS.md 契约锚点同步：「配置结构变更必须 serde default（新字段向前兼容）；旧版本迁移代码已移除，不再新增」。

## 7. 测试扩容（+115 用例）

- 前端 +6 文件 35 用例：`utils.path/platform/models/diff`（含 LCS 超 400 行降级路径）、`stores.ui`（通知栈 6s 生命周期/localStorage 持久化）、`ipc.events`（handler keys → listen 注册契约）。
- 后端 +80 用例：7 个零测试模块全覆盖（http_request SSRF/重定向/脱敏、delete 保护矩阵、net 的 is_private_ip 全分支与 IPv4-mapped、skill、render_html、wait 虚拟时钟、batch_read 委托、tools/mod 序列化）+ 低覆盖加深（stats 合并/保留清理、context 核算、approval auto_confirm 虚拟时钟、prompt 分层、registry、scheduled_task、create、fuzz、atomic、token_est）。
- 发现 2 个疑似实现缺陷，以 `#[ignore]` 探针标注待修：skills TTL 缓存未复查 disabled 清单；stats flush 合并丢弃 by_workspace 的 cache 字段。

## 8. 版本与流程

- 版本 0.1.0 → **0.2.0**（tauri.conf.json / Cargo.toml / package.json / ui/package.json + Cargo.lock）。
- 发布流程沉淀为 skill：`.codewave/skills/bump-version/SKILL.md`（本地资产，.codewave/ 为 gitignore 数据目录）。

## 9. 已知边界与遗留决策

- 提交作者邮箱随历史公开与否（当前 `yangshifu1024@qq.com`）。
- 开源历史形态：完整提交历史 vs squash 初始提交。
- `provider::tests_integration::midstream_disconnect_maps_to_network` 在本机系统代理（Clash）下偶发环境性失败，单独复跑必过，与代码无关。
