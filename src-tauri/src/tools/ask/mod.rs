//! ask 工具：暂停执行向用户提问（多选 + 自由作答）；取消即视为拒绝。
//! G2/G3 硬门（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) §7）：plan 档批准生效前校验 todos 非空且分析产物存在；
//! 豁免通道 = 轻量声明（跳过分析产物校验；todos 非空硬要求不变）或用户口令（在最近一条用户消息中命中）。

mod tool;
pub use tool::*;

#[cfg(test)]
mod tests;
