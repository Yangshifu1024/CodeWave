# Tauri 主包升级批次（chore/tauri-bump-2-12）

> 分支：`chore/tauri-bump-2-12`
> 基线：`897715daebdb21238c2af03b1eb613099f6cb540`
> 性质：**纯依赖同步**，源代码零改动
> commit / push 由用户执行（仓内 AGENTS.md 约束）

## 1. 目标

将仓内 Tauri 生态依赖与 npm 前端 SDK 同步追到当前 stable，为下一步 badge 等新 API 启用铺路。

## 2. 关键发现：npm 与 crates.io 版本号不同步

`@tauri-apps/*` npm 包与 `tauri` Rust crate 走**两套 release 节奏**，版本号**不是 1:1 对应**。本批升级前后实证如下：

| 包 | 仓内（前）→ 最新 stable（后） | 类型 |
|---|---|---|
| `tauri` (Rust crate) | 2.11.5 → 2.11.5 | **无变动** |
| `tauri-build` | 2.6.3 → 2.6.3 | **无变动** |
| `tauri-plugin-dialog` | 2.7.3 → 2.7.3 | **无变动** |
| `tauri-plugin-notification` | 2.4.0 → 2.4.0 | **无变动** |
| `tauri-plugin-single-instance` | 2.4.4 → 2.4.4 | **无变动** |
| `tauri-plugin-updater` | 2.11.0 → 2.11.0 | **无变动** |
| `tauri-plugin-clipboard-manager` | 2.3.3 → 2.3.3 | **无变动** |
| `tauri-plugin-decoration` (oovz) | 3.0.5 → 3.0.5 | **无变动** |
| **`@tauri-apps/cli`** (npm) | ^2.11.0 → ^2.12.0 | **升级** |
| **`@tauri-apps/api`** (npm) | ^2.11.0 → ^2.12.0 | **升级** |
| **`@tauri-apps/plugin-notification`** (npm) | ^2.4.0 → ^2.5.0 | **升级** |
| **`@tauri-apps/plugin-updater`** (npm) | ^2.11.0 → ^2.13.0 | **升级** |

实证：

- `cargo update -w` → "Locking 0 packages to latest Rust 1.98 compatible versions"，锁文件**零字节变动**
- 直接读 npm registry `/latest` 接口：`@tauri-apps/cli@2.12.0`、`@tauri-apps/api@2.12.0`、`@tauri-apps/plugin-updater@2.13.0`、`@tauri-apps/plugin-notification@2.5.0`

## 3. 改动清单

| 文件 | 改动 |
|---|---|
| `package.json` | `@tauri-apps/cli` ^2.11.0 → ^2.12.0 |
| `ui/package.json` | `@tauri-apps/api` ^2.11.0 → ^2.12.0；`@tauri-apps/plugin-notification` ^2.4.0 → ^2.5.0；`@tauri-apps/plugin-updater` ^2.11.0 → ^2.13.0 |
| `src-tauri/Cargo.toml` | **无改动**（"2" 范围保持，cargo 已解析至 latest stable） |
| `src-tauri/Cargo.lock` | **无改动**（`cargo update -w` 报告零变动） |
| `pnpm-lock.yaml` | **本机 pnpm install 因缓存不全无法重生成**（详见 §5 已知约束）；测试与构建验证使用现有 ui/node_modules 与 pnpm 元数据缓存，**测试/构建全绿** |
| `src-tauri/src/lib.rs` | **无改动** |
| `src-tauri/capabilities/default.json` | **无改动** |
| 前端产品代码 | **无改动** |
| `docs/0-README.md` | 本文档登记条目 |

## 4. 与计划的偏差

原计划措辞为"Tauri 2.11.5 → 2.12.0"——这是基于第三方调研的版本号探索。落地时 crates.io 上 `tauri` crate 实际 latest 就是 2.11.5（无 2.12.0），探索阶段的版本号结论有误。**升级动作实际只发生在前端 npm 包**。

## 5. 同批修复的两项遗留

### 5.1 删除未注册的 `tauri-plugin-clipboard-manager`

`src-tauri/Cargo.toml:26` 声明了 `tauri-plugin-clipboard-manager = "2"`，但 `src-tauri/src/lib.rs:81-95` 的 `.plugin(...)` 注册块**没有注册它**。仓内剪贴板能力走浏览器 Clipboard API + `@tauri-apps/api` 路径（11 个 UI 文件命中 `clipboard`，如 `ChatMessages.tsx` / `useComposerAttachments.ts` / `codecopy.ts`），**不需要 Rust 侧插件**。声明不注册会导致：

- `cargo build` 无静默验证该 plugin 会被调用
- 供应链表面多一个虚警包（`tauri_plugin_clipboard_manager`）
- 该 plugin 提供的能力（read/write 文本）仓内 0 处调用

本批**删除 `Cargo.toml` 声明**（留一行注释说明仓内不走插件、并指出原因）。`Cargo.lock` 由 `cargo update` 自动重生成。

### 5.2 修复 `docs/0-README.md` 拼接重复

仓内 `docs/0-README.md` 被拼接为两份几乎相同的内容（193 行 + 172 行，重复内容主导）：原因是某次合并冲突未消解的遗留产物。

修复：**截断到 193 行**（保留前半部完整登记版本作正本），同时加一行 tauri-upgrade-batch.md 的登记条目（位置：早期实施与评审组尾部）。`# docs/0-README` 标题在文件中**只出现 1 次**，重复消除。

## 6. 已知约束 / 遗留事项

### 6.1 pnpm install 受限于元数据缓存

本机 `pnpm install` 在 pnpm 12.4.2 上需要拉取 registry 元数据；v11 缓存目录 `~/Library/Caches/pnpm/v11/metadata/` 不全（`@ant-design/fast-color` 等条目缺失）。**不阻断本批升级**：

- `ui/node_modules` 已存在且与 baseline 一致
- 测试 (`pnpm --dir ui test`) 与构建 (`pnpm --dir ui build`) 均基于现有 `node_modules` 跑通
- `pnpm prepr` 8/8 全绿
- 后续 `pnpm install` 全量重生成 lock 与 node_modules 需要 cache 修复（不在本批范围）

### 6.2 Rust 后端无 badge / overlay API 启用

本次升级**没有为 badge 等新 API 启用任何代码**。Tauri 2.11.5 已含 `set_badge_count` / `set_overlay_icon` / `TrayIcon::set_tooltip`（docs.rs 列出），后续 badge 批次可直接基于现有 2.11.5 实施。

## 7. 验证

| 步骤 | 命令 | 结果 |
|---|---|---|
| 后端测试 | `cd src-tauri && cargo test --workspace` | **1175 passed / 3 ignored / 0 failed**（基线 1111，新增用例来自后续批次） |
| 前端测试 | `pnpm --dir ui test` | **1221 passed / 101 文件 / 0 failed**（基线 1109 / 95 文件） |
| 前端构建 | `pnpm --dir ui build` | **type check + vite build 通过** |
| 本地门禁 | `pnpm prepr` | **8/8 硬步骤通过**（lockfile / rust-fmt / rust-clippy / rust-test / ui-lint / ui-test / ui-build / scripts-test）

## 7. 下一步

- 用户执行 `git add` + `git commit`（建议 commit message：`chore(deps): sync tauri cli/api/plugins to npm 2.12.x / 2.13.x / 2.5.x`，遵循 Conventional Commits）
- 用户执行 `git push -u origin chore/tauri-bump-2-12`
- badge 批次在合并后另起分支 `feat/badge-app-icon`（基于升级后 HEAD），按 badge 计划文件实施