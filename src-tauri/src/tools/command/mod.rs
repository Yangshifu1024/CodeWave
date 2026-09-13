//! command 工具：按配置 selection 执行 shell（auto = 自动探测；全量可选 bash/Git Bash/
//! zsh/fish/sh/powershell/pwsh/cmd/wsl），cwd 锁定在工作区内，
//! 进程组整树终止（TERM → 宽限 → KILL），超大输出落盘，执行前经安全 fence + 审批。

mod tool;
pub use tool::*;

#[cfg(test)]
mod tests;
