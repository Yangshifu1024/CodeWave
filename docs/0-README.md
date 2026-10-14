# CodeWave 文档导航

> 维护日期：2026-10-14。这里是当前行为与长期维护指南；实现细节以文中链接的源码和测试为准。

## 按任务阅读

- 上手开发：[开发环境与工程约定](./development.md) → [架构与决策](./technical-design.md) → [测试与本地门禁](./testing.md)
- 修改运行行为：[会话与项目](./sessions-and-projects.md)、[运行与调度](./runtime-and-scheduling.md)、[子代理](./subagents.md)
- 修改授权或能力：[计划与审批](./plan-and-approval.md)、[工具与安全](./tools-and-safety.md)、[文档工具](./documents.md)、[MCP 与技能](./mcp-and-skills.md)
- 修改产品交互：[供应商与网络](./providers-and-network.md)、[设置](./settings-guide.md)、[聊天与界面](./chat-and-interface.md)
- 设计外部 Agent 接入：[ACP 接入方案](./acp-integration.md)（待批准、未实施；codeg 为 Client，CodeWave 为 Agent）
- 交付版本：[发布、签名与自动更新](./release.md)

## 全部主题

| 文档 | 负责的长期内容 |
|---|---|
| [development](./development.md) | 平台依赖、启动、工程约定与故障定位 |
| [technical-design](./technical-design.md) | 分层、跨层契约与 D1–D8 架构决策 |
| [testing](./testing.md) | 本地门禁、测试策略、平台限制与手动验证纪律 |
| [release](./release.md) | 版本升级、授权边界、签名、公证、更新与发布排障 |
| [sessions-and-projects](./sessions-and-projects.md) | 单目录项目、会话、历史存储、恢复、清理与日志 |
| [runtime-and-scheduling](./runtime-and-scheduling.md) | 主循环、steer、终态对账、运行总览、条件等待与计划任务 |
| [subagents](./subagents.md) | 角色、预算、权限继承、隔离、停止、报告与终态恢复 |
| [plan-and-approval](./plan-and-approval.md) | 研发流程、方案产物、四档权限、批准门、预览与 ask |
| [tools-and-safety](./tools-and-safety.md) | 工具批次、编辑、写入后检查、命令围栏、路径与网络边界 |
| [documents](./documents.md) | Office/PDF 读取、生成、保真修改与预览 |
| [mcp-and-skills](./mcp-and-skills.md) | MCP 配置与生命周期、技能来源、加载与管理 |
| [providers-and-network](./providers-and-network.md) | Provider 模型、协议、密钥、缓存、代理、额度与重试 |
| [settings-guide](./settings-guide.md) | 设置页面、保存语义、注册表、搜索、术语与布局 |
| [chat-and-interface](./chat-and-interface.md) | 输入草稿、流式显示、工具卡、窗口、侧栏、字体与通知 |

## 待批准或验收专项方案

- [可靠性问题修复方案](./reliability-remediation-plan.md)：11 项问题的证据、设计、拆分顺序与验收标准。集中保留跨主题设计与尚未关闭的平台验收矩阵；现行契约已随修复回写长期指南，全部验收关闭后移出当前导航。
- [CodeWave ACP 接入方案](./acp-integration.md)：待批准，未实施。独立 ACP CLI、宿主适配、询问与 MCP、codeg 批准回执兼容依赖、共享数据协调、三平台分发与验收。ACP 双向协议及互操作矩阵需要独立维护，现有架构/MCP 指南保留入口；实施后原位转为接入指南，完成计划留在 Git 历史。

14 篇长期指南、2 篇待批准或验收专项方案，连同本索引共 17 篇。项目总体介绍见 [README](../README.md)，协作硬约束见 [AGENTS](../AGENTS.md)，贡献流程见 [CONTRIBUTING](../CONTRIBUTING.md)，安全披露见 [SECURITY](../SECURITY.md)。

## 维护规则

1. 先更新已有主题，保持平铺目录与英文主题 slug；不要为每次修复新增一篇。新增独立主题须说明现有指南为何容纳不了，并保持 `docs/` 总数（含索引）不超过 20。
2. 指南写当前行为、边界、决策缘由、排障与回归入口。历史实现计划、已完成的一次性验收和纯视觉微调不留在当前目录；原文通过 Git 历史查阅，不创建归档副本规避篇数目标。
3. 旧记录里的有效规则归入对应主题；同一契约以一个主题为主，其他地方只保留必要摘要并链接。测试数量、版本和某次“全绿”不作为长期基线。
4. 未验证的事项必须明确标注；运行总览仍有待实机确认的 GUI 检查点，见[运行与调度](./runtime-and-scheduling.md)。文档整理不代表这些验收已完成。
5. 合并、改名或删除时，同步全仓 Markdown 链接、源码注释引用和读取文档的测试；不得为文档维护修改业务逻辑。
6. 相对链接应从当前文件解析，链接到具体源码或长期主题。使用 Git 查旧文档时可运行 `git log --all -- docs/<旧名>.md`，再以相应提交查看原文。
