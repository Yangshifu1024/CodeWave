# 版本升级与发版流程（bump 脚本 + release 技能）

> 工程化批次（2026-09-13）。参照 PlanWave 的 `scripts/bump-version.mjs` 与 `.agents/skills/planwave-release` 移植适配：
> 统一版本升级入口 `pnpm bump <x.y.z>` + 完整发版流程技能 `.agents/skills/codewave-release`（会被 CodeWave 自身的
> 工作区技能扫描（[docs/slash-skills-and-dollar-agents](./slash-skills-and-dollar-agents.md)）识别为 `/codewave-release`）。

## 1 版本落点与脚本

| 落点 | 作用 | 刷新方式 |
|---|---|---|
| `package.json`（根） | pnpm 工作区根 | 脚本正则改写 |
| `ui/package.json` | 前端包 | 脚本正则改写 |
| `src-tauri/tauri.conf.json` | 桌面安装包版本（tauri-action 打包产物取此处） | 脚本正则改写 |
| `src-tauri/Cargo.toml` | Rust crate `[package] version` | 脚本正则改写 |
| `src-tauri/Cargo.lock` | Rust 锁文件 | `cargo update -w`（只同步工作区成员自身版本，不动三方依赖） |
| `pnpm-lock.yaml` | pnpm 锁文件 | `pnpm install --lockfile-only`（只改锁文件不装依赖） |

用法：`pnpm bump 0.2.1`（接受 `v` 前缀与 `-预发布` 后缀）。**纯版本 bump 实际只改 5 个文件**——pnpm-lock.yaml
不记录工作区包版本，锁文件刷新对纯版本 bump 是 no-op（保留该步骤是为了维持「锁文件与 package.json 一致」的
不变量，即 2026-09-13 CI `ERR_PNPM_OUTDATED_LOCKFILE` 事故的教训，见 [docs/composer-per-tab-draft](./composer-per-tab-draft.md) 同日记录）。

脚本守卫语义：以「正则是否命中」判失败，而非「结果串是否变化」——同版本 bump 是合法 no-op
（PlanWave 原版用 `next === s` 判失败，同版本运行会误报「未找到 version 字段」，移植时已修正）。

## 2 发版流程（`/codewave-release` 技能）

发版 = 门禁 → bump → commit → **确认** → 推 `v*` tag。tag 推送触发 `.github/workflows/release.yml`：
创建 **draft** Release → 三平台（macOS / Linux / Windows）tauri-action 构建并上传安装包 → **人工核对资产后手动 Publish**（CI 绝不自动发布）。

1. **定版本**：显式指定优先；否则按上个 tag 以来的提交推 semver（0.x 期间 `feat!`→minor、`feat`→minor、`fix/chore/docs`→patch），经用户确认；已有 tag 的版本号永不复用（产物带版本、已发布不可变）。
2. **门禁**（任一失败即停）：工作区干净 → 在 `main` 且 `git pull --ff-only` → `cargo test`（src-tauri，0 warning 基线）→ `pnpm --dir ui test` → `pnpm --dir ui build`。fmt/clippy 目前是 lint.yml 的 continue-on-error 软门槛，不阻塞发版。
3. **bump**：`pnpm bump <version>`；`git status --porcelain` 应恰好 5 个修改文件（Windows + autocrlf 下 no-op 可能出现 Cargo.lock 幻影条目，`git diff` 为空即忽略）。
4. **commit**：`chore(release): vX.Y.Z`。
5. **停下汇总并等确认**：版本与出仓提交清单；release CI 将构建什么（draft Release + 三平台安装包；updater 签名 secret 未配置时不带签名；macOS 未配置 `APPLE_*` 时出未签名产物，配置方法见 [docs/macos-signing-and-notarization](./macos-signing-and-notarization.md)）；新 tag 会取消在途 release（concurrency）。**推送前必须得到用户明确同意**（AGENTS.md git 约定）。
6. **推送**：`git push origin main && git tag vX.Y.Z && git push origin vX.Y.Z`；可 `gh run watch` 盯流水线，结束后提醒人工 Publish draft。

## 3 验证

- 非法入参（`abc` / 缺参）→ usage + exit 1；
- 同版本 `pnpm bump 0.2.0` → 全部落点改写成功、锁文件刷新 no-op、`git status` 无差异；
- 真实 bump 回环 `0.2.0 → 0.2.1 → 0.2.0` → 中间态恰好 5 文件修改、各落点版本值正确（含 Cargo.lock 的 `codewave` 版本段）、回摆后除本次批次新增文件外工作区干净。

## 4 自动更新链路（2026-09-13 补齐，同日启用）

参照链路审计（5 断点全修）：插件注册早已就位（`lib.rs` + `tauri-plugin-updater`），本批次补齐其余环节：

| 环节 | 落点 |
|---|---|
| 签名密钥 | minisign 密钥对，私钥 + 空密码；私钥入 GitHub secrets `TAURI_SIGNING_PRIVATE_KEY`（密码 secret 留空即匹配），本地备份 `~/.tauri/codewave.key(.pub)`；公钥固化进 `tauri.conf.json` `plugins.updater.pubkey` |
| 更新器配置 | `plugins.updater.active: true` + `endpoints: [https://github.com/Yangshifu1024/CodeWave/releases/latest/download/latest.json]` |
| 更新产物 | `bundle.createUpdaterArtifacts: true`（构建产出安装包 + `.sig`；tauri-action 检测到签名 env 自动生成/合并 latest.json 上传到 release） |
| 前端权限 | `capabilities/default.json` 增 `updater:default` |
| 检查入口 | 「关于」弹框「检查更新」按钮（三平台）+ macOS 应用菜单项（既有） |
| 安装流程 | `ui/src/utils/updateCheck.ts`：check → `downloadAndInstall()` → toast → `restart_app` IPC（`host/commands/system.rs`，`app.restart()`） |

端到端验证路径：装 v0.3.0（无签名产物，仅作为旧实例）→ 发布 v0.3.1（首个带 latest.json 的 release）→ 0.3.0 检查更新 → 下载签名产物校验公钥 → 安装重启 → 关于页显示 0.3.1。本地模拟：`scripts/mock-updater-endpoint.mjs` 可在 8080 端口伪造更新源（需临时把 endpoints 指向 `http://127.0.0.1:8080/latest.json` 重打包，模拟产物版本号与实际二进制一致属预期）。

注意：latest.json 由三平台 matrix job 各自上传合并——v0.3.1 发布后需核对 release 资产里 latest.json 是否含全部三平台条目（tauri-action 合并逻辑的已知关注点）。
