# Tauri 主包升级批次（chore/tauri-bump-2-12）

> 分支：`chore/tauri-bump-2-12`
> 基线：`897715daebdb21238c2af03b1eb613099f6cb540`
> 性质：**纯依赖同步**，源代码零改动
> commit / push 由用户执行（仓内 AGENTS.md 约束）

## 1. 目标

将仓内 Tauri 生态依赖与 npm 前端 SDK 同步追到当前 stable，为下一步 badge 等新 API 启用铺路。

## 2. 关键发现：Rust 侧被插件的 `^2.12` 要求卡住，npm 侧正常升级

`@tauri-apps/*` npm 包与 `tauri` Rust crate 走**两套 release 节奏**，版本号**不是 1:1 对应**。本批落地实测如下：

| 包 | 仓内（前）→ 现状（后） | 类型 |
|---|---|---|
| `tauri` (Rust crate) | 2.11.5 → 2.11.5 | **卡住未动**（见 §2.1） |
| `tauri-plugin-dialog` | 2.7.3 → 2.8.0 | 被动升级（`cargo update` 解析结果） |
| `tauri-plugin-updater` | 2.11.0 → 2.11.0 | 卡住未动 |
| `tauri-plugin-clipboard-manager` | 2.3.3 → **已删除** | 移除未注册声明 |
| `tauri-plugin-decoration` (oovz) | 3.0.5 → 3.0.5 | 无变动 |
| **`@tauri-apps/cli`** (npm) | ^2.11.0 → ^2.12.0 | **升级** |
| **`@tauri-apps/api`** (npm) | ^2.11.0 → ^2.12.0 | **升级** |
| **`@tauri-apps/plugin-notification`** (npm) | ^2.4.0 → ^2.5.0 | **升级** |
| **`@tauri-apps/plugin-updater`** (npm) | ^2.11.0 → ^2.13.0 | **升级** |

### 2.1 Rust 侧「零变动」的真实原因（重要）

`cargo update -w` 报 "Locking 0 packages to latest Rust 1.98 compatible versions"，
最初被读作「已是 crates.io latest」。**这是误读**——实测强制升 patch 版本会失败：

```
$ cargo update -p tauri --precise 2.11.6 --dry-run
error: failed to select a version for the requirement `tauri = "^2.12"`
candidate versions found which didn't match: 2.11.6
location searched: crates.io index
required by package `tauri-plugin-dialog v2.8.0`
```

**`tauri 2.11.6` 确实存在**（crates.io 索引可查到，dependabot 已开 PR #94），
但 `tauri-plugin-dialog 2.8.0` 要求 `tauri = "^2.12"`，`2.11.6` 不满足该范围，
故**依赖图把 tauri 钉在 2.12 系列**；而 tauri 2.12.x 与本仓插件链的解析组合
在当前 Rust 1.98 工具链下无法一次解出，`cargo update -w` 于是报「无可升项」。

**结论**：Rust 侧不是「已是最新」，而是「被插件的 `^2.12` 要求卡住」。
解法是整条插件链一起升（dialog / updater / notification / single-instance
逐个跟进），由 dependabot 逐个 PR 推进（#94 tauri、#96 plugin-updater 等），
**本批不重复该工作**。

### 2.2 npm 侧实证

直接读 npm registry `/latest` 接口：
`@tauri-apps/cli@2.12.0`、`@tauri-apps/api@2.12.0`、
`@tauri-apps/plugin-updater@2.13.0`、`@tauri-apps/plugin-notification@2.5.0`

## 3. 改动清单

| 文件 | 改动 |
|---|---|
| `package.json` | `@tauri-apps/cli` ^2.11.0 → ^2.12.0 |
| `ui/package.json` | `@tauri-apps/api` ^2.11.0 → ^2.12.0；`@tauri-apps/plugin-notification` ^2.4.0 → ^2.5.0；`@tauri-apps/plugin-updater` ^2.11.0 → ^2.13.0 |
| `src-tauri/Cargo.toml` | 删去未注册的 `tauri-plugin-clipboard-manager` 声明（其余 "2" 范围保持不动） |
| `src-tauri/Cargo.lock` | 由 `cargo update` 重生成（连带解析 `tauri-plugin-dialog` 2.7.3 → 2.8.0，删除 clipboard-manager 段） |
| `pnpm-lock.yaml` | **本机 pnpm install 因缓存不全无法全量重生成**（详见 §6 已知约束）；测试与构建验证使用现有 ui/node_modules，**测试/构建全绿** |
| `src-tauri/src/lib.rs` | **无改动** |
| `src-tauri/capabilities/default.json` | **无改动** |
| 前端产品代码 | **无改动** |
| `docs/0-README.md` | 消除重复拼接 + 本文档登记条目 |

## 4. 与计划的偏差

原计划措辞为「Tauri 2.11.5 → 2.12.0」，探索阶段给出的理由是「crates.io 上 tauri 2.11.5 已是 latest，故零变动」。**该探索结论不准确**——实测 `tauri 2.11.6` 存在，只是被 `tauri-plugin-dialog 2.8.0` 的 `tauri = "^2.12"` 要求卡住（详见 §2.1）。

**实际发生的升级动作只在 npm 侧**；Rust 侧保持 2.11.5 不动，原因是「依赖图无可解的升法」而非「已是最新」。两者结论相同、**理由不同**，后者才是准确表述。

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

### 6.2 Rust 后端未启用 badge / overlay API

本次升级**没有为 badge 等新 API 启用任何代码**。Tauri 2.11.5 已含 `set_badge_count` / `set_overlay_icon` / `TrayIcon::set_tooltip`，后续 badge 批次可直接基于现有 2.11.5 实施，**无需等 Rust 侧升到 2.12**。

### 6.3 Rust 侧升级的解法（不在本批范围）

需整条插件链一起升（dialog / updater / notification / single-instance 逐个跟进）才能解开 `tauri = "^2.12"` 的约束。仓库已开 9 个 dependabot PR（#91–#100）覆盖这批升级，**建议逐个 review 后合并**，不要在本 PR 里夹带。

## 7. 验证

| 步骤 | 命令 | 结果 |
|---|---|---|
| 后端测试 | `cd src-tauri && cargo test --workspace` | **1175 passed / 3 ignored / 0 failed**（基线 1111，新增用例来自后续批次） |
| 前端测试 | `pnpm --dir ui test` | **1221 passed / 101 文件 / 0 failed**（基线 1109 / 95 文件） |
| 前端构建 | `pnpm --dir ui build` | **type check + vite build 通过** |
| 本地门禁 | `pnpm prepr` | **8/8 硬步骤通过**（lockfile / rust-fmt / rust-clippy / rust-test / ui-lint / ui-test / ui-build / scripts-test）

## 8. 提交与后续

已分三批提交并推送到 `origin/chore/tauri-bump-2-12`：

| hash | subject |
|---|---|
| `36295d6` | `chore(deps): 同步 Tauri 前端 SDK 到 npm 最新 stable` |
| `f02a92b` | `chore(deps): 移除未注册的 tauri-plugin-clipboard-manager` |
| `4ca4fbe` | `docs(deps): 补 Tauri 依赖同步批次记录 + 修 0-README 重复拼接` |

下一步：开 PR 评审合并；badge 批次在合并后另起分支 `feat/badge-app-icon` 实施。
- 用户执行 `git push -u origin chore/tauri-bump-2-12`
- badge 批次在合并后另起分支 `feat/badge-app-icon`（基于升级后 HEAD），按 badge 计划文件实施