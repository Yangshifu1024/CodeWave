//! 命令安全围栏 fence（[docs/technical-design](../../../../docs/technical-design.md)）：先硬拒绝已识别的宿主内部状态写入，再走 L1 删除黑名单 / L2 AST 写目标 / L3 高危模式。
//! 纯函数；解析链 = bash 语法 → PowerShell 语法（[docs/testing](../../../../docs/testing.md)：Windows 回退 shell，
//! [docs/tools-and-safety](../../../../docs/tools-and-safety.md)）→ 词法回退扫描，后者仍适用 L1/L3。
//! 加固批次 [docs/testing](../../../../docs/testing.md)：字面量掩码的 L3 启发式（字符串/注释数据不再触发）、
//! 命令名反混淆（`r"m"`、`${IFS}`）、`--force-with-lease` 不再误报、PowerShell AST 遍历。

mod check;
pub use check::*;

#[cfg(test)]
mod tests;
