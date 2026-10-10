# CodeWave 开发环境准备（macOS / Linux / Windows）

> 对应 [docs/p0-plan](./p0-plan.md) G1/G8。CI 配置见 `.github/workflows/`（test / lint / release）。

## 前置依赖（当前要求）

| 依赖 | 要求 | 依据 |
|---|---|---|
| Rust | **MSRV 1.98** | `src-tauri/Cargo.toml` 的 `rust-version` |
| Node.js | 22+ | 前端构建与测试工具链 |
| pnpm | 任意近期版本（`corepack enable pnpm` 即可） | 前端是 pnpm workspace |
| Tauri 2 系统依赖 | 见下方各平台清单 | `pnpm tauri dev` / `pnpm tauri build` |
| Tauri CLI | **走 pnpm（`pnpm tauri …`），不装 `cargo-tauri`** | 根 `package.json` 的 `@tauri-apps/cli` devDependency |

两条命令执行位置的既定规约（易踩）：`pnpm tauri …` **必须在仓库根执行**（依赖根 `package.json` 的 CLI 与 `src-tauri/tauri.conf.json`）；`cargo test` **必须在 `src-tauri/` 执行**。

## macOS（主开发平台，已在 Apple Silicon 上验证）

```bash
# Xcode 命令行工具
xcode-select --install
# Rust（brew 或 rustup 任一）
brew install rust
# Node 22+ 与 pnpm
brew install node && corepack enable pnpm
# Tauri CLI 依赖
pnpm install   # 在仓库根执行（workspace = ui，见 pnpm-workspace.yaml）
```

构建验证：2026-08-30 那次是历史时点记录，当时跑通的是 `cargo test` / `pnpm build` / `pnpm tauri build --debug`。**当前命令以仓库现状为准**（用例数持续增长，本文不写死数字）：

| 用途 | 命令 | 执行位置 |
|---|---|---|
| 后端测试 | `cargo test` | `src-tauri/` |
| 前端测试 | `pnpm --dir ui test` | 仓库根 |
| 前端构建（type check + vite build） | `pnpm --dir ui build` | 仓库根 |
| 开 PR 前门禁（fmt / clippy / 前后端测试 / 构建 / 脚本测试 / 锁文件） | `pnpm prepr` | 仓库根 |

## Linux（CI 冒烟）

```bash
sudo apt-get install -y cmake build-essential pkg-config \
  libgtk-3-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

## Windows（G8：git2/libgit2 构建验证）

- VS Build Tools（MSVC）
- cmake（`choco install cmake --installargs 'ADD_CMAKE_TO_PATH=System' -y`）
- WebView2 Runtime（Win11 自带）
- 前端：Node 22 + pnpm

git2（libgit2-sys）源码自编译，无需系统安装 libgit2；未启用 `ssh` feature，无需 libssh2。

## 首次运行

1. 在仓库根执行 `pnpm install`
2. 开发模式：`pnpm tauri dev`
3. 打包：`pnpm tauri build`（或 `--debug` 快速验证）
4. 数据目录：`~/.codewave/`（首次启动自动创建）；日志在 `~/.codewave/logs/`

可选：设置环境变量 `RUST_LOG=debug` 提升日志级别。
