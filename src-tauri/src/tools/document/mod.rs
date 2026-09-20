//! document 工具组：Office 与 PDF 文件的读取、生成与保真修改
//!（[docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)）。
//!
//! 两条贯穿全组的设计约定：
//!
//! 1. **读取先给摘要、再按区域取数**：一张几十万行的表若整份返回会直接灌爆上下文，
//!    所以先返回结构摘要（工作表名、行列数、表头、前几行），模型再按 `sheet` + `range` 取具体数据。
//! 2. **保真修改绝不整份解析重建**：一律走「解开压缩包 → 只替换目标内部文件 → 其余原样搬运重打包」。
//!    重建路线（把整份读成内存模型再重新序列化）会丢掉模型没覆盖到的内容——图表、数据透视表、
//!    迷你图、切片器都会凭空消失，这是格式决定的，不是实现好坏的差异。

pub mod backup;
pub mod edit;
pub mod docx;
pub mod edit_word;
pub mod patch;
pub mod pdf;
pub mod read;
pub mod sheet_edit;
pub mod workbook;
pub mod write;
pub mod write_word;
pub mod xlsx;
pub mod xml_util;

#[cfg(test)]
mod faithful_spike;
