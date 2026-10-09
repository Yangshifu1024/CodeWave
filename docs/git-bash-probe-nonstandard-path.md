# Windows Git Bash 探测：支持非标准安装路径

> 日期：2026-10-10 · 分支：`fix/pending-todo-finish`（改动自上一会话 `f19c3890` 延续）
> 相关：[main-run-finish-with-pending-todos](./main-run-finish-with-pending-todos.md)（同分支的另一缺陷修复）

## 1. 问题

`command` 工具的 Shell 变体在 Windows 上需要定位 Git Bash（`Shell::Bash` 的程序路径）。修复前的 `windows_probe("git_bash")` 只有两级兜底：

1. **固定候选路径**：`%ProgramFiles%\Git\bin\bash.exe`、`%ProgramFiles%\Git\usr\bin\bash.exe`、`%LocalAppData%\Programs\Git\bin\bash.exe`、`C:\Program Files\Git\…`、`C:\Git\bin\bash.exe`；
2. **PATH 兜底**：在 PATH 的每个目录里找 `bash.exe`，但**只认目录名含 `git`** 的那些。

两者同时落空的场景：**Git 装在任意非标准位置（如 `D:\App\Git`），且 PATH 里只加了 `D:\App\Git\cmd`（该目录含 `git.exe` 但不含 `bash.exe`）**。此时——

- 固定候选全是标准安装位置，一个都不存在；
- PATH 兜底在 `…\Git\cmd` 里找不到 `bash.exe`（`bash.exe` 在同级的 `…\Git\bin\` 下，而 `bin` 未必在 PATH 里）。

结果：`Shell::Bash` 拿不到程序路径，`shell_invocation_per_variant` 在 Windows 上断言 `prog` 以 `bash.exe` 结尾——该用例在非标准安装的机器上失败。

## 2. 方案

跟着**已确认存在的 `git.exe`** 走，而不是猜安装位置。

### 2.1 `git_root_from_exe`（纯函数，`tool.rs`）

```rust
#[cfg(windows)]
pub(super) fn git_root_from_exe(git_exe: &std::path::Path) -> Option<std::path::PathBuf> {
    git_exe
        .parent()
        .and_then(std::path::Path::parent)
        .filter(|root| !root.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
}
```

Git for Windows 有两种常见布局，二者都收敛到同一个 `root`：

| 布局 | PATH 里通常加的目录 | git.exe 位置 | bash.exe 位置 |
|---|---|---|---|
| A | `<root>\cmd` | `<root>\cmd\git.exe` | `<root>\bin\bash.exe` |
| B | `<root>\bin`（整个 bin） | `<root>\bin\git.exe` | `<root>\bin\bash.exe` |

统一「取父目录的父目录」即可覆盖两种。

**空根过滤是必需的**（易踩）：Rust 的 `Path::parent` 对 `"git.exe"` 这类**单段路径返回 `Some("")` 而非 `None`**。不过滤的话，后续 `root.join("bin\\bash.exe")` 会产出**相对路径**，可能误命中当前工作目录下的同名目录——静默拿到错误的 bash。负例测试因此可以严格断言 `git_root_from_exe("git.exe") == None`。

### 2.2 `bash_via_git_exe`

在 PATH 的非 `system32` 目录里用 `find_exe_in_all(&dirs, "git")` 收集**全部** `git.exe` 命中，对每个反推出 root，再按 `bin\bash.exe` → `usr\bin\bash.exe` 顺序检查存在性。

排除 `system32` 的理由：那里的 `git.exe`（若存在）通常来自非 Git-for-Windows 的第三方 git，反推出的 root 下不会有 Git Bash。

### 2.3 `find_exe_in_all`

`find_exe_in` 的多命中版（后者只取首个）。同名 exe 在 PATH 多个目录各有一份时，全部尝试一遍比只取首个更稳；按路径去重。

Windows 下同时尝试 `<exe>.exe` 与裸名（与既有 `find_exe_in` 同语义）。

### 2.4 接入探测链

`windows_probe("git_bash")` 的顺序变为：

```
固定候选路径 → bash_via_git_exe（非标准安装路径）→ PATH 含 git 目录兜底（原逻辑）
```

新增的中间层放在原兜底**之前**——原兜底虽然覆盖面窄，但零额外开销，放在末尾不影响标准安装路径的命中。

## 3. 测试

`src-tauri/src/tools/command/tests.rs` 三个用例：

| 用例 | 作用 |
|---|---|
| `git_root_from_exe_covers_both_layouts` | 纯函数矩阵：布局 A（`D:\App\Git\cmd\git.exe`）、布局 B（`D:\App\Git\bin\git.exe`）都归到 `D:\App\Git`；负例 `git.exe` / `cmd\git.exe`（深度不足）严格断言 `None` |
| `git_bash_probe_finds_nonstandard_install_via_git_exe` | 端到端：本机装了 Git Bash 时 `windows_probe("git_bash")` 必须返回真实存在的 `bash.exe`；且由 PATH 上的 `git.exe` 反推出的 root 下确实存在 bash（**不依赖 PATH 是否含 git 目录**）。这是 CI 的 `windows-2022` runner 结构性覆盖不到的盲区（非标准安装路径），故钉一个回归 |
| `find_exe_in_all_collects_and_dedups` | 造两个目录各放一个同名 exe：全部命中、同路径不重复、不存在的 exe 返空 |

## 4. 判别力

`git_root_from_exe_covers_both_layouts` 与 `find_exe_in_all_collects_and_dedups` 是纯函数断言，在修复前这两个函数不存在，编译即失败。

`git_bash_probe_finds_nonstandard_install_via_git_exe` 在**标准安装路径**的机器上恒通过（此时走的是原兜底）——它的判别力来自「非标准安装路径机器上修复前会红、修复后转绿」，本地以非标准路径实测确认过。

## 5. 边界与遗留

- `#[cfg(windows)]` 门控：Unix 侧 `unix_probe` 不变（`/bin/bash` 等固定布局已足够）。
- `shell_invocation_per_variant` 的断言逻辑未改——本批只让 Windows 上的 `prog` 能被正确探测到。
- 已知未覆盖：Git 安装在**网络驱动器 / 符号链接**下的情形（`parent().parent()` 不解析链接）。实际影响面小，未做处理。