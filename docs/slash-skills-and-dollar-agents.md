# 技能触发符 `/` 化与 `$` 子代理点名批次

> 日期：2026-09-08。需求：技能触发从 `$` 改为 `/`；`$` 改为点名单内置子代理；`/` 菜单移除命令只留技能；技能加载路径定为 `.codewave/skills`、`.agents/skills`、`.claude/skills`。
> 关联：[composer-toolbar-batch-report](./composer-toolbar-batch-report.md)（原 `$` 技能菜单）、[arch-orchestrator](./arch-orchestrator.md)（子代理角色注册表）、[standard-workflow](./standard-workflow.md)。

## 1. 背景与语义决策

原 Composer 三触发符：`/` 仅整框恰为 `/` 时打开 5 个本地命令菜单；`$` 弹技能（`list_skills`）；`@` 弹文件提及。三者均为**纯文本信号**——前端原样发送，无后端协议；技能/提及靠系统提示词引导模型行为。

本批次语义决策：

1. **`/` = 技能专属菜单**：命令入口自 `/` 菜单移除——git/diff/tasks/stats 顶栏四入口已覆盖、compact 有工具条按钮，无功能回退；菜单行为更聚焦（输入 `/` 即列技能，继续输入过滤）。
2. **`$` = 子代理点名（文本信号，不做硬路由）**：`$` 弹内置子代理角色清单（新 IPC `list_agents`），选中回填 `$<role> ` 前缀；发送后由核心提示新增规则引导主代理经既有 `subagent` 工具以该角色委派。不做 `start_chat` 结构化参数/旁路主代理直启子代理——子代理排除 `ask` 工具且权限继承父会话，直启会让审批应答与过程引导失去主代理协调点，与现有运行时架构相抵触。
3. **技能加载路径定稿**：`.codewave/skills`（项目会话 = project_dir `<主目录>/.codewave/skills`；临时/全局 = data_dir `~/.codewave/skills`）、`.agents/skills` 与 `.claude/skills`（工作区级）+ `~/.claude/skills`（用户级只读兼容，保留既有用户技能）+ 内置。移除旧路径：工作区 `.codewave/skills`（与 project_dir 同路径冗余/临时会话误嵌套）与项目裸 `skills/`。优先级（低→高）：内置 < `~/.claude/skills` < 工作区 `.claude/skills` < 工作区 `.agents/skills` < 全局 `.codewave/skills`（data_dir）< 项目 `.codewave/skills`（project_dir，托管最高）。
   - ⚠️ 语义钉子：`SessionRuntime.data_dir` 恒为全局数据目录 `~/.codewave`（`get_or_create_session` 硬绑 `core.data_dir`），项目数据目录经 `project_dir` 传入——`.codewave/skills` 的项目/全局两侧必须分别走 project_dir 与 data_dir，不能只扫 data_dir（首版实施即因误读该语义漏扫项目技能，见缺陷修复 §5）。

## 2. 触发与发送语义（契约）

| 触发 | 弹层 | 选中行为 | 发送语义 |
|---|---|---|---|
| `/` 首 token（`/^\/(\S*)$/`，未含空白） | 技能清单（`list_skills`） | 回填 `/<name> ` | `/<name> …` 原样发送，`<available-skills>` 首行点名语义引导模型先经 `skill` 工具加载该技能再执行 |
| `$` 尾片段（`/\$([^$\s]*)$/`） | 内置子代理角色（`list_agents`，剔除内部 title） | 回填 `$<role> ` | `$<role> …` 原样发送，核心提示 `$<role>` 点名规则引导主代理以该角色经 `subagent` 工具委派，其余内容作 task |
| `@` 尾片段 | 文件/目录提及（不变） | 选**目录** → 回填 `@<path>/ `；选**文件** → 转成引用 chip（[composer-file-ref-chips](./composer-file-ref-chips.md)） | 目录提示原样发送；文件引用由 `mergeRefs` 合成 `@<path>` 追加正文末尾（与改造前逐字节一致） |

- `/` 首 token 含空白（如已输入 `/repo-index `）→ 菜单收起，Enter 正常发送——修复了旧版「任何以 `/` 开头的文本 Enter 均被命令菜单吞掉、无法原样发送」的副作用。
- 菜单互斥与键盘导航优先级：`/` 技能 > `$` 子代理 > `@` 提及；Enter 选中、Tab 仅子代理/提及、Escape 全收。
- `@` 提及自 2026-09-21 起：只有第一级「📁 根目录」条目仍写文本（供继续拼路径），搜索命中（含目录）一律进引用 chip——搜索接口不返回 `is_dir`，目录命中进 chip 与改造前「插入 `@dir ` 也拼不了路径」等价（[composer-file-ref-chips](./composer-file-ref-chips.md)）。
- 竞态加固：三组候选各带乱序守卫（`mentionSeq` 既有，新增 `skillSeq`/`agentSeq`）；离开触发符时经 `clearSkills`/`clearAgents`/`clearMentions` 自增 seq 作废在途查询，防止迟到的 IPC 结果把菜单顶回来。

## 3. 改动清单

### 后端

- `agents/mod.rs`：新增 `AgentMeta { name, description }`（serde，不含 body 正文）与 `delegable()`（`builtin()` 剔除内部 `title` 角色）；测试 `delegable_excludes_title_and_matches_find`。
- `host/commands/agents.rs`（新）：`list_agents` 命令，纯转调 `agents::delegable()`；`commands/mod.rs` + `lib.rs` 注册。
- `core/prompt.rs`：`<tool-policy>` 新增「消息以 `$<role>` 开头 = 用户点名内置子代理：以该角色经 subagent 工具委派，其余内容原样作为 task；角色不存在或不可委派时如实说明，不擅自改派」；测试新增 `CORE_PROMPT` 含 `$<role>` 锚点。
- `skills/mod.rs`：扫描路径改为内置 < `~/.claude/skills` < 工作区 `.claude/skills` < 工作区 `.agents/skills` < `data_dir/skills`（全局 `.codewave/skills`）< `project_dir/skills`（项目 `<主目录>/.codewave/skills`，最高）；测试 `scan_paths_and_priority` 钉住新路径优先级、无项目时 data_dir 兜底、旧路径（工作区 `.codewave/skills`、裸 `skills/`）不再扫描。
- `skills/mod.rs`：扫描路径与优先级如 §1.3（`project_dir` 参数保留，`.codewave/skills` 项目侧经它扫描）；`tools/skill.rs`、`core/context.rs`、`core/agent/stream.rs`、`host/commands/skills.rs` 的 `get/list` 调用照旧传 `project_dir`。
- `skills/mod.rs` `prompt_listing`：`<available-skills>` 块首行加 `/<name>` 点名语义说明。

### 前端

- `useComposerMentions.ts`：`pickSkill` 回填改 `/<name> `（替换正则 `/\/[^/\s]*$/`）；新增 `agentResults`/`refreshAgents`/`pickAgent`（回填 `$<role> `）与三个带 seq 作废的 `clear*`；返回面移除裸 setter。
- `Composer.tsx`：`/` 菜单开合由 `text` 派生（`/^\/(\S*)$/`），条目 = `skillResults`（纯技能，命令数组与 `doCommand` 删除，`ipc` 导入随之移除）；`$` 弹层换为子代理清单；键导航/Escape 分支按新优先级重排。
- `ipc/types.ts` + `client.ts`：`AgentMeta` 类型 + `listAgents()`。
- `i18n`（zh/en）：`useSlash` →「使用 / 选择技能」、`useDollar` →「使用 $ 指派子代理」；placeholder 改「`/` 技能 / @ 文件 / $ 子代理」；`commands.*` 命名空间整体移除（已无引用）。

### 测试

- `app.smoke.test.tsx`：`/` 用例改为纯技能断言（`/demo` 在列且旧命令项不出现）；新增「`/` 选技能回填 `/demo `」「`$` 列出内置角色回填 `$backend-dev `」两条；ipc mock 增 `list_agents`。
- 后端：`cargo test` 434 passed / 0 failed。
- 前端：`pnpm --dir ui test` 270 passed；`pnpm --dir ui build` 通过。

## 4. 手动验证清单

1. `pnpm tauri dev`（确认无其他实例占用 bundle id）。
2. 项目会话：`<主目录>/.codewave/skills/<name>/SKILL.md` 存在时，输入 `/` 应列出；项目 `.codewave/skills` 与 `~/.claude/skills` 同名时项目版胜出（描述不同可辨别）。
3. 继续输入过滤（如 `/my`）；选技能回填 `/my-skill `；补参数后 Enter 原样发送，模型加载该技能。
4. 输入 `$`：列出 8 个内置角色（无 title）；选 `$backend-dev` 发送，聊天流出现子代理卡、抽屉可见过程。
5. `/` 菜单不再出现 compact/git/diff/tasks/stats；顶栏「变更/任务/统计」与工具条压缩按钮仍可用。
6. `@` 提及、Shift+Tab 权限循环、Esc 收菜单回归正常。

## 5. 缺陷修复记录：项目技能未读取（同日）

**现象**：项目 `<主目录>/.codewave/skills/grilling` 存在，切到该项目会话后 `/` 菜单不出现。

**根因**：首版实施误读 `SessionRuntime.data_dir` 语义——它恒为全局数据目录 `~/.codewave`（`AgentCore::get_or_create_session` 硬绑 `core.data_dir`），并非「项目会话 = `<主目录>/.codewave`」；项目数据目录另经 `rt.project_dir`（`project_data_dir`）传入。首版删除旧路径后只扫 `data_dir/skills`，实际扫的是全局 `~/.codewave/skills`，项目 `.codewave/skills` 无人扫描（旧代码靠 `workspace/.codewave/skills` 与 `project_dir/skills` 覆盖到）。

**修复**：恢复 `project_dir` 参数（cache_key 同步回 workspace+project_dir），扫描链路补回 `project_dir/skills` 作为项目 `.codewave/skills` 源、优先级最高；`data_dir/skills` 回归全局用户级定位。`list_skills` 命令、`skill` 工具、context/stream 注入链路同步恢复传参。

**回归锚点**：`skills::tests::scan_paths_and_priority`（项目覆盖全局/compat、无项目 data_dir 兜底、旧路径不扫描）。

## 6. 批次追加（同日）：技能列表显示排序 + 点击详情

- **显示排序**（`SkillIndex::list`）：按来源分桶——内置 > 项目 `.codewave/skills`（project_dir）> 全局 `~/.codewave/skills`（data_dir）> 其他（`.claude`/`.agents` compat 目录），桶内按名升序。注意与**覆盖优先级**（§1.3，项目 > 全局 > compat > 内置）是两回事：显示序内置最前，覆盖序内置最低。归类用 origin 路径前缀匹配（统一正斜杠）。`<available-skills>` 注入顺序随显示序。回归锚点 `skills::tests::list_orders_by_origin_bucket`。
- **点击详情**：共享组件 `features/shell/SkillDetailModal.tsx`——元信息（描述/触发时机/来源路径）+ SKILL.md 正文（`renderMarkdown` 渲染，限高滚动）；正文经新 IPC `get_skill`（`SkillIndex::get`，禁用/不存在返回 None）。
  - **右栏信息页技能行**（详情唯一入口）：行可点击（悬停高亮），弹详情；footer「使用」经 `ws:composer-insert` 把 `/<name> ` 追加进当前会话输入框（保留既有草稿，与 + 菜单 insertTrigger 同语义）并关闭弹层；右栏关闭时收起弹层（与产物查看弹窗同一守卫）。排序由后端 `list_skills` 分桶序直接生效。回归锚点 `__tests__/rightbar.skills.test.tsx`。
  - **composer `/` 菜单技能项**：点击/Enter 均直接回填 `/<name> `（点击详情逻辑曾短暂加到 composer 后按用户裁决移除，详情只保留右栏入口）。
  - i18n 键 `skills.detailUse/detailClose/detailWhen/detailOrigin/detailLoading`；行为面 window 事件增 `ws:composer-insert`（useComposerEvents 第三契约，追加不覆盖，事件名勿改）。
