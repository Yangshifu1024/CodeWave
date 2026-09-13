# 会话产物登记 + 右侧栏标签页化（信息 / 日志 / 文件）

> 改动背景：AI 经 create/edit 工具落盘的文档（arch 四文档、plan 档分析产物、日常代码/图片等）此前没有任何「会话 → 文件」归属记录——arch 产物写到 `<主目录>/.codewave/tasks/<时间戳>-<slug>/`，目录名与会话 id 无关联（[docs/arch-orchestrator](./arch-orchestrator.md) §5 已把「任务产物列表展示」列为后续方向）。本批次补齐该机制：后端登记归属，右侧栏新增「文件」标签页展示并支持点击查看。

## 一、设计决策

- **逻辑归属，不迁移文件**：产物留在原地（arch 契约、S9 汇报的绝对路径均不变），后端登记「哪个会话写过哪些文件」的归属视图。
- **登记范围 = 全部 create/edit 成功写入**（含 dev 子代理写的代码文件），前端按类型图标区分，不做过滤分组（单列表全展示）。
- **边车落点 = 全局 `<data_root>/sessions/<id>.artifacts.json`**（仿 `<id>.todos.json`）：归属关系属于会话元数据而非项目托管数据，随会话删除级联清理；临时会话（无项目目录）天然支持。

## 二、后端

- **归属字段**：`SessionRuntime.root_session_id: Option<String>`（`core/agent.rs`）。主会话/计划任务 = None；`new_sub()` 经 `Arc::get_mut` 设为 `Some(parent.root_session_id ?? parent.id)`——子代理写入归属主会话，嵌套子代理归到根。另加 `is_task_runtime: bool`（`new_task` 置位）供登记点跳过。
- **边车存储**：`core/sessions/mod.rs` 新增 `SessionArtifact { path(绝对), first_op, last_op, first_at, last_at, count }` 与 `ArtifactOp`（serde 序列化为 `"create" | "edit"`）。`SessionStore` 新增：
  - `append_artifact(id, path, op)`：`artifacts_lock` 互斥下读 → 按 path 去重合并（重复累加 count、刷新 last_*）→ 原子写；
  - `load_artifacts(id)`：无文件/损坏返回空（登记是辅助视图，不阻断会话；追加后自愈）；
  - `remove()` 级联删除 artifacts 边车，**顺带补删此前泄漏未清理的 `<id>.todos.json`**。
- **登记点 = 工具体内部**（`tools/create.rs` / `tools/edit.rs`）：`atomic_write` 成功后调 `ctx.core.store.append_artifact(...)`。放工具体而非 batch 层的原因：`ToolCtx` 同时持有 `core` 与 `rt`，主会话与子代理两条执行路径天然全覆盖。三条护栏：登记路径先经 `canonical_best_effort` 归一（AI 相对/绝对路径混写同一文件仍去重为一条）；计划任务 runtime（`is_task_runtime`，产物登记无消费点）跳过登记防边车泄漏；登记失败仅 `tracing::warn`，不影响工具结果。edit 一次写多文件时逐个登记。
- **新 IPC 命令**（`host/commands.rs` + `lib.rs` 注册）：
  - `list_session_files(session_id)`：读边车（子代理会话查归属 root）→ last_at 降序 → stat 补 `exists` / `size`（文件被外部删除时 exists=false）。主体逻辑独立为 `session_files_payload()` 便于测试；
  - `read_workspace_file_base64(session_id, path)`：与 `read_workspace_file` 同 `resolve_read` 根校验、同 1MB 上限，返回 base64（图片预览；文本通道是 UTF-8 lossy 读不了二进制）。主体独立为 `read_file_base64()`；
  - `Cargo.toml` 新增 `base64 = "0.22"`。

## 三、前端

- **写入信号**：`stores/run.ts` 的 `TabRunState` 新增 `writeTick`；`onToolResult` 中 `tool ∈ {create, edit} && ok` 时 +1。**复用既有 `tool:result` 事件，不新增事件键**（`events.ts` 契约与 `events.contract.test.ts` 不动）。
- **RightBar 标签页化**（`features/shell/RightBar.tsx`）：堆叠 section 重构为 antd Tabs 三标签：
  - **信息**：原 Git / 项目目录 / 会话 / 当前计划 4 个 section 原样迁入（`InfoPanel`）；
  - **日志**：[docs/session-logging-report](./session-logging-report.md) 的运行日志区去掉折叠头迁入（`LogPanel`），内部「会话 | 全局」Segmented、自动轮询（仅标签页激活时轮询，`visible` prop 控制）、粘底滚动等行为不变；
  - **文件**：新 `FilesPanel`，tab label 常显计数（如「文件 3」，未激活也拉取）。
- **数据流**（`features/files/useSessionFiles.ts`）：后端登记边车为唯一事实；挂载 / activeId 变化 / `writeTick` 变化（防抖 300ms）时重拉 `list_session_files`；reqIdRef 过期响应守卫（切会话后晚到的旧响应不覆盖，与日志区同款）。
- **FilesPanel**（`features/files/FilesPanel.tsx`）：单列表按 last_at 降序，每项类型图标（md / 图片 / 文本 / 其他）+ 文件名 + 「新/改」徽标 + 相对时间；tooltip 显完整路径；`exists=false` 灰显「已删除」且不可点击；头部「共 N 个 + 刷新」。
- **FileViewerModal**（`features/files/FileViewerModal.tsx`）：
  - `.md`：`read_workspace_file` → `renderMarkdown` → 弹窗内渲染（容器复用 `.assistant` 作用域类继承全部 markdown/mermaid/katex 样式）→ `upgradeDiagrams` 升级图表占位符；
  - 图片：`read_workspace_file_base64` → `data:` URL → antd `Image` 预览（SVG 经 `<img>` 渲染，内嵌脚本不执行，安全）；
  - 其他文本：`CodeBlock` 按扩展名映射 highlight.js 语言；
  - 超 1MB / 越界 / 失败：Alert 提示，不崩溃。
- 样式：`theme/app.css` 新增 `.rb-tabs`（窄栏紧凑 Tabs）、`.rb-files-*`（列表项 hover / 灰显）。

## 四、测试

- 后端（`cargo test` 全绿 / 0 warning，总数 218）：
  - `artifact_append_dedups_and_merges`：同路径合并（first/last/count）、跨会话隔离；
  - `remove_cascades_sidecars`：删除级联清理 artifacts + 补删 todos 边车；
  - `corrupted_artifacts_sidecar_isolated`：损坏边车静默空 + 追加自愈；
  - `edit_registers_artifact_with_root_owner`：edit 成功登记、子代理写入归属主会话、相对/绝对路径写同一文件去重为一条（count 合并）；
  - `task_runtime_skips_artifact_registration`：计划任务 runtime 跳过登记（无边车文件）；
  - `create_flow` 扩展断言：成功登记 / 失败不登记 / overwrite 合并 count；
  - `session_files_payload_sorts_and_stats`：降序 + stat 填充 + exists=false；
  - `read_file_base64_bounds_and_encoding`：二进制安全编码 + 根外拒绝。
- 前端（`pnpm --dir ui test` 63/63 + `build` 通过）：
  - `rightbar.log.test.tsx` 适配 Tabs（未激活零 IPC / 激活加载 / 过期守卫三条全保留）；
  - 新增 `rightbar.files.test.tsx`：tab 计数常显 / 列表渲染 / md 弹窗渲染 / 图片 base64 通道 / 已删除灰显不可点 / 切会话过期响应守卫 / writeTick 递增语义；
  - 既有 4 个测试文件手工构造的 `TabRunState` 桶补 `writeTick: 0`。
- 双代理代码审查（7 维度）通过：无 🔴；3 条 🟡（计划任务边车泄漏 / 登记路径归一 / 前端过期守卫测试缺口）已全部修复。

## 五、遗留与边界

- 登记以「写入即归属」为口径：会话里被 edit 的既有文件（含用户文件）也会进列表——这是「本次会话动过哪些文件」的完整视图；如需仅看「新建文档」可在 UI 上按「新」徽标辨识。
- 手动登记清理（用户在会话外删除文件）不回写边车，列表以 exists=false 灰显。
- 计划任务 runtime 写入不登记（无 UI 消费点，防边车泄漏）；临时会话写入正常登记归属自身。
- 代码审查 🟢 项（登记排序依赖 rfc3339 字典序，当前恒 UTC 成立；`list_session_files` 读边车不加锁依赖原子写；弹窗重开同文件不重载等）留待后续批次按需处理。

## 六、手动验证清单

1. `pnpm tauri dev` 启动，确认右侧栏顶部为「信息 | 日志 | 文件」三个标签；信息标签内容与升级前一致（Git/项目目录/会话/当前计划）。
2. 「日志」标签：会话/全局切换、自动刷新开关、刷新/目录按钮、WARN/ERROR 着色、粘底滚动均与升级前一致；切到其他标签时确认不再轮询（网络面板无 `read_session_log` 持续调用）。
3. 新建临时会话（免目录）→「文件」标签显示「无活动会话」/ 空态文案；发起会话让 AI create 一个 md 文件 → 标签计数 +1，列表出现该文件（「新」徽标），不切标签计数也常显。
4. 让 AI edit 既有文件 → 列表对应条目徽标变「改」、时间刷新。
5. 点击 md 条目 → 弹窗渲染 markdown（含 mermaid/公式块可渲染）；点击图片条目 → 弹窗图片预览；点击代码文件 → 高亮展示；点击灰显「已删除」条目 → 无反应。
6. 切换到另一个会话 → 文件列表与计数随之切换；AI 运行中写入文件 → 列表约 0.3s 后自动刷新。
7. 删除一个有产物的会话 → 确认 `~/.codewave/sessions/<id>.artifacts.json` 与 `<id>.todos.json` 一并消失。
8. arch 流程（项目会话 $arch）跑一轮 → 「文件」标签能看到 requirement.md / plan.md / review.md / report.md 四文档，点击可读。

## 七、提交信息（用户执行）

```
feat(files): 会话产物登记 + 右侧栏标签页化（信息/日志/文件）
```
