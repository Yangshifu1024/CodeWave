# 文档读取、生成与保真修改

> 当前行为指南；源码核对：2026-10-11。处理的是用户原位置的文件，读取与预览共用实现。普通文本编辑和写后检查见[工具与安全](./tools-and-safety.md)，引用 chip 与查看器布局见[聊天与界面](./chat-and-interface.md)。

## 1. 能力边界与工具选择

| 格式 | 读取 | 新建 | 修改既有文件 | 预览 |
|---|---|---|---|---|
| xlsx | read_document | write_document | edit_document 单元格级 | 结构化表格 |
| xlsm | read_document | **不支持** | edit_document 单元格级，未触碰宏包成员原样保留 | 结构化表格 |
| docx | read_document | write_document | edit_document 文字替换 | 标题/表格的文本投影 |
| pdf | read_document 提取文字层 | 不支持 | 不支持 | pdfjs 阅读器 |
| csv/tsv、md/txt | read 文本通道 | create | edit | 表格/文本查看器 |
| xls/doc、pptx/ods/rtf | 不支持；旧格式提示另存新版 | 不支持 | 不支持 | 无专用解析 |

三个文档工具按扩展名分派；read_document 是 ReadOnly，write_document/edit_document 是 FileWrite，服从 Plan 排除、主会话计划门和写入审批。`read` 按扩展名与内容探测拒绝二进制文档并引导正确工具，不允许把替换符乱码当原文分析。

不提供密码文件、OCR、PDF 正文编辑、PowerPoint、图表生成、旧格式回写，以及增删行列/段落/表格等结构编辑。表格中的“保真”表示未改包成员保留，**不意味着 UI 或模型能理解、展示所有 Office 特性**。

主要源码：[document 模块](../src-tauri/src/tools/document/mod.rs)、[读取](../src-tauri/src/tools/document/read.rs)、[生成](../src-tauri/src/tools/document/write.rs)、[编辑](../src-tauri/src/tools/document/edit.rs)。

## 2. 读取先摘要，再按范围取数

表格不带 sheet 时返回工作表名、行列数、预览内容与续读提示；previewRows 默认 5、最大 50。给 sheet，可配 range（如 A1:D50 或单格 B3）取区域；单次最多 200 行、64 列、20,000 格，回执说明实际范围和截断。不要把预览窗口当全表，也不要为方便一次拉整本大工作簿进上下文。

PDF 超过 10 页必须显式给 pages，单次最多 20 页。文字层为空或近空时返回 scanned 提示，如实说明需要 OCR，不猜扫描件内容；缺字形映射的中文 PDF 可能提取乱码，此时也不能据此下结论。PDF 阅读器能显示扫描件，并不代表模型文字提取成功。

文本通道编码按 BOM → 合法 UTF-8 → GBK（替换符比例护栏）→ unknown；返回 encoding 供 UI/模型识别不确定性。无 BOM 的 Big5/Shift-JIS 等可能被误认 GBK，必要时先确认编码再转换。不能把“已返回字符串”当作“已正确识别文字”。

## 3. 保真编辑的不可替代路线

**已有 Office 文件必须走“解包 → 只替换目标内部文件 → 其余成员原样搬运 → 重打包”，绝不整份解析重建。** 重建会丢失未建模的图表、透视表、迷你图、切片器、宏与外部连接；这是为什么 write_document 只新建，不提供 overwrite 的原因。

[patch::apply](../src-tauri/src/tools/document/patch.rs) 的约束：

- 替换条目必须已存在，否则中止而非凭猜测生成结构。
- 加密条目拒绝。
- 保留成员顺序；未修改成员通过 raw_copy_file 复制，连解压/重压缩都不做。
- 目标成员完成写入后，zip finish 返回的 BufWriter 必须显式 flush 并检查错误，防磁盘满时“成功”覆盖出截断文件。
- 临时打包成功后才原子替换目标，不能直接在用户文件上写半包。

### 表格单元格

单次最多 200 个 edit，每项 sheet + cell，以及 text/number/formula/bool **恰好一种**。坐标超出 Excel 1,048,576 行 / 16,384 列直接拒绝；结构增删不会偷偷降级为重建。

[sheet_edit](../src-tauri/src/tools/document/sheet_edit.rs) 定位目标 `<c>`，新文字写 inline string，不改共享字符串表，避免一个索引的修改污染其他单元格。未改格式与其他 XML 节点尽量保持原状，审批展示旧值到新值的整组变更。

### Word 文字

[edit_word](../src-tauri/src/tools/document/edit_word.rs) 将一句话跨多个 `<w:t>` 的命中区间回填，保持非文字节点、run 样式与批注锚点。默认只替换唯一命中，多处命中须补上下文；不用整份段落重建解决匹配失败。

`&/< />` 按 XML 转义对应定位，不能切开实体。当前对生成器写成 `&apos;`/`&quot;` 的引号仍可能无法匹配，应报未找到而非猜位置。替换里的裸换行在 Word 中只是空白，真正换行需要 `<w:br/>`，不在当前文字替换能力内；首尾空格也可能因未设置 xml:space 而被吞。需要结构化重排时明确说明能力边界。

## 4. 公式：生成与编辑不可混为一谈

新建 xlsx 的派生值应使用真实公式，避免把汇总焊死为数字；`write_document` 接受 `{"formula":"=SUM(B2:B3)","cached":270}`，**缓存值由调用方提供**。只传公式字符串或省 cached 时，非计算型阅读器可能看到空白；生成路径没有自动调用编辑路径的公式求值器。

edit_document 对修改公式走[保守求值器](../src-tauri/src/tools/document/formula.rs)：

- 支持数值四则、括号、一元负号与 SUM/AVERAGE/COUNT/MIN/MAX 的单元格/区域参数。
- 可求值则同时写公式与缓存值；不可求值则只写公式，并在 workbook.xml 设置 `calcPr fullCalcOnLoad="1"`。
- 引用单元格本身还是公式、除零、非有限结果、文本算术、超 100,000 格区域或非白名单函数都不编结果。
- calcPr 已有则改属性，没有则在 schema 正确位置插入；必须位于 sheets/definedNames 后、pivotCaches/extLst 前，重复标记不能堆叠元素。

当前边界：不更新 xl/calcChain.xml；期待 Excel 重建，但公式修改后的真 Excel 重算/修复提示仍需专项验证。跨表引用使用载入快照，同批先改别表再引用可能缓存旧值，不能声称自动算到整批最终状态。

## 5. 备份与回退

[backup.rs](../src-tauri/src/tools/document/backup.rs) 按源文件分组保留最近五份，文件名纳秒时间戳并处理占名冲突；普通 edit 的临时事务备份成功即删，**不能替代 Office 持久备份**。右栏文件条目可以列出备份时间/体积，用户确认后原子还原到目标位置；备份必须来自受限备份目录，不能把还原 IPC 变成任意路径复制通道。

维护目标仍是“修改/还原之前保存当前版本，使操作可再撤回”，但要如实说明实现：

- **当前 edit_document 的 Word 与表格两条路径，读取原件或 backup::save 失败时会只记 warn/返回空 backup，仍继续原子覆盖。** 因此现在不能承诺“改之前一定成功备份”。
- [restore_from_backup](../src-tauri/src/host/commands/preview.rs) 保存当前版本也使用 best-effort，失败不阻断还原；“还原本身也一定可撤回”同样不是硬保证。
- 文档写/编辑有 runtime 内批次串行，但未接入 edit/create/delete 共用的进程级 writelock。当前 write.rs 与 edit.rs 的写认领条件是 `root_session_id.is_none()`，与普通工具针对子 runtime 认领的条件相反；不能承诺子代理的文档写入完全受认领保护。外部编辑、跨 runtime 同文件修改与该条件仍需专项修复/验收。

补强这些点需独立代码改动与故障注入：备份目录不可写/磁盘满/并发写时不得静默失去唯一原件。本次指南仅记录现状和必须保留的安全目标。

## 6. 原地引用与项目外访问

非图片附件不复制、不立即塞上下文：原生文件选择框或 Tauri 拖入拿到真实路径，草稿显示文件名 chip，发送前才合成 `@路径` 到正文。图片要进模型 wire，才读成 base64。网页 input[type=file] 没有真实路径，不能替代这条入口。拖入启用 Tauri 原生事件后，不能继续依赖网页 ondrop。

项目外引用先经[check_external_path / allow_external_dir](../src-tauri/src/host/commands/workspace.rs)：仅此一次/始终允许目录/取消。“仅此一次”加入当前 runtime 的 extra_roots；“始终允许”还写 ProjectEntry.allowed_dirs，之后新建/恢复会话并入 extra_roots。授权的是**目录及子目录的读、写、列**，不只是这一个附件的读取，UI 必须让用户理解粒度。

拒绝文件系统根、用户主目录本身与应用托管数据目录的整体放行，要求更具体子目录。内部比较保持 canonical 形态；Windows `\\?\` 仅在展示/给模型引用时去前缀，不能用于内部边界比较。文件工具并无通用 resolve_or_ask 自动扩权路径。

## 7. 预览和体积预算

[read_for_preview](../src-tauri/src/tools/document/read.rs) 复用 read_document，不另建一套解析器。表格用行窗口化 + 工作表选择；Word 是 markdown 文本投影，不能当 Word 排版保真预览；PDF 用 pdfjs 动态加载，worker 随 Vite 产物同源分发，运行时不联网下载。

PDF 预览使用 PDF.js 6 的 `legacy` 主库与同版本 `legacy` worker。上游兼容版支持基线为 Chromium 125+ / Safari 18+（Safari 为部分支持，仍有个别渲染差异）；对应 WebView 也必须满足该基线，旧版 macOS/WebKit 可能无法预览 PDF。这是 PDF 预览的兼容范围，不代表整个应用的最低系统版本。Linux WebKitGTK 需实机验证，不能仅按 Safari 版本推定兼容；生产构建和 happy-dom 测试不替代 WebView 验收。参见 [PDF.js 支持范围](https://github.com/mozilla/pdf.js/wiki/Frequently-Asked-Questions#which-browsersenvironments-are-supported)。

加载任务持有 PDF worker；关闭、切换文件或加载失败时调用 `PDFDocumentLoadingTask.destroy()`，中途取消后不再读取后续文件分片或启动 worker。

| 通道 | 当前上限 | 限制含义 |
|---|---|---|
| 文本/二进制普通预览 | 8 MiB | 普通查看器，不等于文档工具读取上限 |
| 结构化文档读取/预览 | 200 MiB | 压缩文件体积门，不是已验证的解包内存安全值 |
| edit_document | 50 MiB | 需要解包/重打包，成本更高 |
| read_file_chunk | 文件 200 MiB，单片 4 MiB | PDF 按需取字节 |
| write_document 表格 | 最多 20 sheet、50,000 的总行数预算 | 以当前参数校验为准，不代表可无限列/单元格 |

200 MiB 阈值仍待真实大文件与压缩膨胀验证；ZIP 成员读取使用 read_to_end，不能把压缩文件体积门宣称为逐成员/解压后总量硬界限。表格 UI 目前只能看首个至多 200 行/64 列窗口，缺少按范围续取；PDF 只渲染当前页，无缩放/文字选择。未配置 pdfjs 的可选 wasmUrl，ICC/JPEG2000 部分增强依赖回退，不保证所有 PDF 渲染一致。

CSP 放宽仅为本地 PDF 渲染需要的 wasm 与 worker；不要顺带给 render_html widget 放开网络/同源。markdown 宽表应由真实 `.table-wrap` DOM 承接横向滚动，文件查看器不用聊天区 860px 限宽容器，细节见[聊天与界面](./chat-and-interface.md)。

## 8. 维护入口与尚未完成的验收

最低 Rust 版本为 1.98，构建依赖以 Cargo.toml/锁文件为准，不复制历史版本清单作升级指南。内置 xlsx/docx/pdf/doc-convert 技能是操作指南，不携带第三方脚本，也不扩张实际工具能力。

回归入口：

- [faithful_spike.rs](../src-tauri/src/tools/document/faithful_spike.rs)：除目标成员外逐字节一致；覆盖图表、透视表、条件格式、批注等真实样本。
- [patch.rs](../src-tauri/src/tools/document/patch.rs)：原样复制、缺成员拒绝、加密拒绝、封包 flush 错误。
- [edit.rs](../src-tauri/src/tools/document/edit.rs)、[formula.rs](../src-tauri/src/tools/document/formula.rs)：公式缓存/重算标记、范围/类型校验、改前备份内容。
- [edit_word.rs](../src-tauri/src/tools/document/edit_word.rs)、[backup.rs](../src-tauri/src/tools/document/backup.rs)、[preview.rs](../src-tauri/src/host/commands/preview.rs)：跨 run 文字、实体、版本去重、回退目录限制。
- [fileviewer.document.test.tsx](../ui/src/__tests__/fileviewer.document.test.tsx)：结构化预览与 markdown 包裹。happy-dom 无 canvas，不能替代 PDF 渲染失败路径的实机验收。

开放验收集中为：真实 Excel 打开公式编辑产物的修复提示/重算结果；同批跨表改动缓存一致性；200 MiB 与高压缩比文件的内存/延迟；备份失败与并发覆盖；PDF canvas 失败、中文提取与 wasm 回退。这些不能由曾通过的普通单元格保真样本推定已完成。后端测试、前端 test/build 与人工查看清单遵循[测试指南](./testing.md)，不要重引已删除 LSP 的测试入口。
