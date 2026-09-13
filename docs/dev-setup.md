# CodeWave 开发环境准备（macOS / Linux / Windows）

> 对应 [docs/p0-plan](./p0-plan.md) G1/G8。CI 配置见 `.github/workflows/ci.yml`。

## macOS（主开发平台，已在 Apple Silicon 上验证）

```bash
# Xcode 命令行工具
xcode-select --install
# Rust（brew 或 rustup 任一）
brew install rust
# Node 22+ 与 pnpm
brew install node && corepack enable pnpm
# Tauri CLI 依赖
pnpm install   # 在 frontend/ 下执行
```

本机构建验证（2026-08-30 通过）：`cargo test`（85 用例）、`pnpm build`、`pnpm tauri build --debug`。

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

1. `cd frontend && pnpm install`
2. 开发模式：`pnpm tauri dev`
3. 打包：`pnpm tauri build`（或 `--debug` 快速验证）
4. 数据目录：`~/.codewave/`（首次启动自动创建）；日志在 `~/.codewave/logs/`

可选：设置环境变量 `RUST_LOG=debug` 提升日志级别。
