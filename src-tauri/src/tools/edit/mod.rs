//! edit 工具：version 乐观并发令牌、EOL 归一匹配（CRLF 文件接受 LF 的 oldText）+
//! 两级低风险模糊替换（缩进平移 / 行首尾空白），oldText 唯一命中优先、lineRange 兜底，
//! 进程级写互斥（跨 runtime，tools/writelock.rs）、倒序应用、备份回滚。

mod tool;
pub use tool::*;

#[cfg(test)]
mod tests;
