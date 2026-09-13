# CodeWave · 09 项目目录化 + 多目录工作区 重构报告

> 日期：2026-08-30（多根重构 + 前端导航改版）
> 前置：[docs/code-review-findings](./code-review-findings.md) §五 复核轮完成（137 后端测试基线）；本轮同时完成「前端 Vue → React 迁移」（见 [docs/p1-p2-implementation-report](./p1-p2-implementation-report.md) §14 追加与 ui/ 目录）
> 约束遵守：零 git 操作；用户代码目录零改动

---

## 1. 背景与决策

原模型「会话绑定单一工作区目录」升级为 **「项目 = 多目录工作区」**：

| 决策点 | 结论（用户拍板） |
|---|---|
| 本地目录改名（.codewave → .agent） | **取消，保留 `.codewave`** |
| 旧会话数据 | 推翻重来，不写迁移（旧 SessionMeta 无 project_id 自然落 None） |
| 无项目会话 | **允许自由会话**（单目录），与项目会话长期并存 |
| Git 面板 | **聚合全部目录**（每根独立探测，带根标注） |
| 删除项目 | **连带删除其下会话**（含历史转录）+ 托管目录；带影响说明确认弹框；代码目录永不动 |
| 命令 cwd | 挂项目 = `~/.codewave/projects/<id>/`（托管中立区）；自由会话 = 所选目录 |
| @ 提及 | 两层：`@` 先列项目全部目录，继续输入跨根匹配文件 |
| 顶栏 | 移除「选择工作区」「新 Tab」——一切从左侧导航开始 |

## 2. 项目目录结构（每项目一目录）

```
~/.codewave/projects/<project-id>/
  project.json    # { id, name, directories: [绝对路径], created_at }
  temps/          # Agent 临时区（data_dir 内天然过围栏；提示词告知；清空入口）
  logs/           # <session-id>.log 每会话运行日志
  memory/         # 项目级记忆（与用户级 ~/.codewave/memories 两级合并注入）
  skills/         # 项目级技能托管（优先级最高）
  mcp.json        # 项目级 MCP 配置托管
  lessons.md      # 项目经验教训托管
  tasks/          # 计划任务持久化 <task-id>.json（重启恢复）
```

存储为目录式（一项目一目录，非单 JSON）：`list_projects` 遍历子目录、`save_project` 原子写、`delete_project` `remove_dir_all`；损坏目录跳过不炸整表。

## 3. 多根运行时语义

- **快照装载**：`create_session(project_id?, workspace?)` 双形态——挂项目读注册表 → 校验目录（失效跳过）→ `runtime.workspace = directories[0]`、`extra_roots = directories[1..]`、`roots/project_id/project_dir` 落 SessionMeta + runtime
- **相对路径**：读/列主目录优先逐根命中（resolve_read 跨根）；写/新建只落主目录（resolve_write 不变）；围栏校验全部根（复用 WriteRoots，temps 在 data_dir 内天然合法）
- **Prompt 注入**：CORE_PROMPT 声明「项目由一或多个目录组成、同等地位、问整体必须遍历每个目录」；环境段列 `Project directories` 编号清单；`<project>` 段注入项目名 + 成员目录 + temps + 指令工作目录说明
- **Git 聚合**：status/diff/log 对每根独立探测（是仓库才纳入），返回带 `root` 标注与首仓 `branch`
- **Skills/MCP/Lessons 托管**：优先级（低→高）内置 < 用户级/claude < 代码仓 `.codewave`（兼容）< 项目托管
- **计划任务持久化**：TaskTable upsert/remove 同步落 `projects/<id>/tasks/`；supervisor 启动恢复（once 过期不恢复）；ScheduledTask 增 `project_id`
- **logs 分流**：run 开始/完成/取消/失败追加写 `projects/<id>/logs/<session-id>.log`（自由会话跳过）
- **memory 两级**：`scan(data_dir, project_dir?)` 合并注入；项目级路径天然在写白名单

## 4. 前端改动

- **导航（ProjectNav）**：临时会话区（项目上方，`+` 选目录创建）→ 项目区（`∨` 折叠、`⠿` 管理弹框、`＋` 新建；项目行悬停 `+` 建会话；会话行相对时间/运行中转圈/待确认绿标/显示更多/行内重命名删除）→ 任务区
- **删除项目**：管理弹框 → 影响说明确认弹框（托管数据/连带 N 会话/代码目录不动）→ 级联删除 + 关 Tab
- **@ 两层提及**：`@` 空查询列全部根目录（📁 置顶）；输入后跨根模糊匹配文件（绝对路径插入）
- **文件树**：多根虚拟顶层（空路径返回各根，绝对路径作 key）；默认收起 + 折叠/展开按钮（受控 expandedKeys）
- **右边栏（RightBar，常驻）**：Git（分支+改动数）/ 模型（名称+ID）/ 项目目录（托管路径+目录清单）/ 会话（开始时间/Tokens/上下文%）
- **工具卡**：渲染在回答文本上方、默认折叠
- 顶栏精简：移除「选择工作区」「新 Tab」；无会话发送消息 → toast 引导从导航创建
- Tab 增 `projectId/createdAt`；SessionMeta（前端类型）增 `project_id/roots`

## 5. 验收结果

| 项 | 结果 |
|---|---|
| `cargo test` | **137/137**（+2 目录式存储、跨根/聚合/持久化用例） |
| `pnpm --dir ui test` | **10/10**（含新建项目弹框、删除项目级联确认用例） |
| TSC | 0 错误 |
| `pnpm tauri build --debug` + 启动冒烟 | 通过 |
| GUI 抽查 | 多根文件树、git 聚合、项目持久化均实测（用户手动验证进行中） |

## 6. 未验证项与后续候选

- 真实双仓项目上 AI 的跨目录操作体感（提示词语义已强化，待更多实战）
- 计划任务 once 过期恢复清理、跨项目任务表规模
- 远期候选（目录布局已预留）：`cache/`（索引缓存）、`checkpoints/`（编辑回滚快照）、`reviews/`（报告沉淀）、`env/`（项目环境变量，需安全设计）、`prompts/`（项目级提示词）、`sessions/`（按项目归档）
- 已知取舍：快照语义下项目目录变更不影响已开会话；目录级权限差异、跨目录移动/重命名本轮不做
