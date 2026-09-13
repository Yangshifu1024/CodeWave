//! 命令安全围栏 fence（[docs/p0-plan](../../../../docs/p0-plan.md) §7.2）：L1 删除黑名单 → L2 AST 写目标分析 → L3 高危模式。
//! 纯函数；解析链 = bash 语法 → PowerShell 语法（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：Windows 回退 shell，
//! [docs/fence-plan-readonly-powershell](../../../../docs/fence-plan-readonly-powershell.md)）→ 词法回退扫描，后者仍适用 L1/L3。
//! 加固批次 [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：字面量掩码的 L3 启发式（字符串/注释数据不再触发）、
//! 命令名反混淆（`r"m"`、`${IFS}`）、`--force-with-lease` 不再误报、PowerShell AST 遍历。

mod check;
pub use check::*;

#[cfg(test)]
mod tests;
