# 13 · Composer 工具条重构 + 会话级运行参数批次报告

> 参考截图 1:1 复刻聊天输入框「圆角卡片 + 底部工具条」样式与功能；权限四档、模型/推理力度会话级落地（含 code-reviewer 审查修订）。
> 分支：`feat/composer-toolbar`（worktree `../CodeWave-composer`），基于 master 97749d3。

## §1 范围与决策

| 决策点 | 结论 |
|---|---|
| 权限模式 | 完整四档落后端（变更前确认 / 自动编辑 / 计划模式 / 完全访问） |
| 模型 / 推理力度 | 会话级（每 Tab 独立），非全局 |
| 附加功能 | 图片附件 + `$` 技能菜单；不做「电脑操作」按钮 |
| 模型菜单 | 供应商分组头 + 「视觉」标签 + 管理模型入口（本轮做） |
| 力度档位 | 默认 / 低 / 中 / 高 / 最高 五项 |
| FullAccess 边界 | 灾难级命令（dd 写盘/mkfs/写 /etc/电源操作）仍直接拦截 |
| Plan 模式边界 | 保留 shell 供只读研究，但 shell 区内写弹确认；MCP 全排除 |
| 不做 | 文件更改撤销卡（git 只读定约）、账号类型标签 |

## §2 后端改动（src-tauri/）

### 2.1 会话级运行参数（新 `core/prefs.rs`）

```rust
pub enum ApprovalMode { ConfirmEach, AutoEdit, Plan, FullAccess }   // serde snake_case
pub enum EffortLevel  { Low, Medium, High, Max }
pub struct SessionPrefs { approval_mode, model_id: Option<String>, reasoning_effort: Option<EffortLevel> }
pub struct ImageIn { mime, data }   // start_chat 附件
pub fn effective_model(cfg, prefs)  // override → 悬空回落 → 全局 active
```

- 挂 `SessionRuntime.prefs: Mutex<SessionPrefs>`（短临界区不跨 await；运行中切换安全）。新建会话按全局 `approval.enabled` 映射初始值（`true→AutoEdit`，`false→FullAccess`）——全局开关降级为「新会话初始值」。
- `SessionRuntime::new_sub` 复制父 prefs：子代理与主会话同权限/模型/力度语义。
- 新 IPC：`set_session_prefs`（全量替换 + 校验 model_id 存在性）/ `get_session_prefs`（同进程重开 Tab 回读对齐）。**本轮为内存态**，应用重启回落默认（见 §5 限制）。

### 2.2 权限四档与 fence 接缝（安全语义表）

fence 新增 `FencePolicy { approval_enabled, confirm_outside_create, confirm_inside_writes }` 与 `ConfirmReason::Disaster / InsideWrite`；L3 拆两级：

| 模式 | edit/create/delete | shell 区内写（重定向/cp/mv/tee） | shell 区外新建 | fence 高危（curl\|sh、chmod 777、push --force） | fence 灾难（dd 写盘、mkfs、写 /etc、shutdown） | MCP |
|---|---|---|---|---|---|---|
| ConfirmEach 变更前确认 | **执行前弹审批**（batch 消费点） | **弹确认** | 按全局开关确认/放行 | 弹确认 | 弹确认 | 可用 |
| AutoEdit 自动编辑（默认） | 自动放行 | 放行 | 按全局开关确认/放行 | 弹确认 | 弹确认 | 可用 |
| Plan 计划模式 | **从工具集排除** | **弹确认** | 按全局开关确认/放行 | 弹确认 | 弹确认 | **全排除** + 系统提示词 `<plan-mode>` 约束 |
| FullAccess 完全访问 | 自动放行 | 放行 | 放行（跳过弹窗） | **免确认执行** | **直接拦截** | 可用 |

- FullAccess 的实现路径：fence 一律以 `approval_enabled=true` 产出 Confirm，在 command/service 两个消费点按模式跳过审批弹窗；灾难级 Confirm 防御性兜底为 Block。**不**把 `approval_enabled=false` 当 FullAccess 传（现行 false 语义是高危变 Block，更严）。
- L1 删除黑名单（rm/xargs/find -delete）与符号链接逃逸 Block 在所有模式不变。
- ConfirmEach 的文件写审批挂在 `batch.rs::run_tool`（ToolKind::FileWrite 执行前），120s 超时=拒绝（复用现有审批门）。
- Plan 排除集经 `DriveParams`（`main_drive_params()`）：`exclude_tools=["edit","create","delete"] + exclude_mcp=true`；shell 保留供只读研究，区内写由 fence 升级确认兜住（取舍见 §5）。

### 2.3 模型 / 推理力度请求级化

- `StreamRequest.reasoning_effort: Option<EffortLevel>`（类型化，非法值挡在边界）：编排层解析 `prefs.reasoning_effort → ModelConfig.reasoning_effort`（未知字符串视为未配置）。
- openai_chat / openai_responses：填 `reasoning_effort` / `reasoning.effort`；**Max 显式降级为 "high"**（非 OpenAI 标准枚举）。协议层不再直读 `ModelConfig.reasoning_effort`。
- anthropic：新增 `thinking={type:"enabled", budget_tokens}`，budget = pct×max_tokens 钳制在 `[1024, max_tokens-1]`（low 20% / medium 40% / high 60% / max 80%），max_tokens≤1025 不发送。
- 全局 active_model 残留三处改会话感知：stats 归因（agent.rs run_chat）、checkpoint model_id（checkpoint + rename_session）、context 窗口与压缩模型（context.rs）。
- start_chat：签名加 `images: Vec<ImageIn>`；`StartChatBody` serde default 兼容；消息构造 `Text + Image[]`。
- `ModelConfig` 新增 `provider: Option<String>`、`vision: Option<bool>`（容器 `#[serde(default)]`，旧 config.json 透明）；`scripts/generate-model-catalog.mjs` 从 models.dev 输入模态推导 `vision` 并已重新生成目录（212 供应商 / 7494 模型）。

## §3 前端改动（ui/）

- **Composer 重构**（features/chat/Composer.tsx）：`.composer-card` 圆角容器（`--ws-border`/聚焦 `--ws-accent`）+ borderless TextArea + 底部工具条；保留 `.composer`/`.composer-wrap` 类名（smoke 选择器不破）。
- 工具条左侧：`+` 菜单（添加附件 / @ 上下文 / / 能力 / $ 技能，antd Dropdown 项目首次引入）；权限模式下拉（四档富条目=图标+标题+描述+勾选；FullAccess 选中态橙色 `--ws-warn`）。
- 工具条右侧：模型下拉（按 `provider` 分组头、无 provider 归「其他」、`vision` 带「视觉」标签、当前勾选、分割线+「管理模型」→设置弹窗）；力度下拉（默认带说明 / 低 / 中 / 高 / 最高）；圆形发送钮（↑，运行中红色停止 + spinner）。
- `$` 技能：输入 `$` 触发 Popover（复用 / @ 模式），listSkills 过滤+前缀过滤，选中插入 `$名称 `。
- 附件：file input(image/*, multiple) → base64 → chips 预览可删；限制：单张 5MB / 最多 4 张 / base64 总额 20MB（超限 message 提示）；随 `startChat` 发送。
- 会话感知：ContextInfoBar 模型标签与 Composer 发送守卫改读「会话 override → 全局 active」。
- 状态管线：`Tab.prefs`（sessions store，前端为状态源）；`updatePrefs` 乐观更新 + `setSessionPrefs` 失败回滚并提示；`openSession` 时 `getSessionPrefs` 回读对齐；events.ts 24 键未动。
- i18n：zh-CN/en-US 新增 `composer.*` 全量 key。

## §4 测试与验证

- 后端 `cargo test`：**172 全绿 / 0 warning**（基线 154 + 新增 18）：SessionPrefs serde/初始映射/悬空回落、new_sub 继承、Plan 排除集与提示词、fence 区内写升级与灾难拆级（legacy false 语义不回归）、command 消费点四模式集成（预取消令牌=快速拒绝）、ConfirmEach 批次写审批、三协议 effort 断言（Max 降级、budget 钳制）。
- 前端 `pnpm --dir ui test`：**33/33**（基线 23 + 新增 10）：工具条渲染、权限/模型/力度菜单交互、$ 技能插入；`pnpm --dir ui build` 通过。
- 真机 E2E：`cargo test e2e_real_glm -- --ignored` 保留原用例，**新增 thinking 变体** `e2e_real_glm_thinking_effort`（anthropic 协议 + 会话级力度，验证 GLM 端点接受 thinking 参数）——**尚未运行**，见 §5。

## §5 已知限制与风险（如实记录）

1. **prefs 为内存态**：应用重启后各会话回落全局默认（模型/力度/权限）。后续可持久化到 SessionMeta。
2. **anthropic thinking 与多轮回放**：Thinking 块不带 signature 不回放历史（anthropic.rs 既有约束），开启 thinking 的多轮工具循环在部分端点可能 400。已备真机 E2E 变体验证；若失败按回退方案（anthropic 协议不发送 thinking）处理。
3. **ConfirmEach/Plan 语义洞如实声明**：ConfirmEach 只拦「文件写入类」与 shell 区内写目标；shell 的写不仅限重定向形态（如 `tee` 已覆盖、管道写进程外文件无法静态判定），Plan 模式 shell 仍可执行只读以外的边角写（fence L2/L3 与区内写确认覆盖主要形态）。
4. **图片 token 粗估**：token_est 按 1600 tokens/张估，大图明显低估；图片随历史持久化与每轮重发会放大历史体积（上限：单张 5MB、4 张、20MB base64 总额）。
5. **confirm_git_push 为死字段**（历史遗留，全库无引用）：本轮未清理，留配置清理批次。
6. 手动验证清单交付用户（界面不做 GUI 自动点验）。

## §6 手动验证清单

准备：`pnpm tauri dev`（仓库根、worktree 内执行）；确认无打包版同时运行（单实例互斥）。

1. **布局**：输入框为圆角卡片，聚焦时边框高亮；placeholder 居上；底部工具条 = 左 [+] [权限模式] ，右 [模型名] [力度] [圆形↑]；卡下方信息条（模型/上下文/MCP/压缩）不受影响。
2. **权限四档**：点「自动编辑」→ 菜单四项（图标+描述+当前勾选）；切「完全访问」→ 按钮变橙色；切「变更前确认」→ 让模型改一个文件 → 弹「文件写入确认」，拒绝则模型收到拒绝；切「计划模式」→ 让模型改文件 → 模型无法调用写工具并倾向输出计划。
3. **模型下拉**：显示当前模型名；菜单按供应商分组（从目录填充过的模型）、带「视觉」标签（vision 模型）、当前项勾选；「管理模型」打开设置弹窗模型页签；切换后信息条模型名同步变化（会话级，另开 Tab 不受影响）。
4. **力度下拉**：五项；「默认」带说明文案；选「最高」后发消息，模型思考行为可见变化（GLM anthropic 端点走 thinking 映射）。
5. **+ 菜单**：添加附件 → 选图 → 输入框上方出现缩略图 chips，可删除；「使用 @ 添加上下文」→ 输入框追加 @ 并弹文件菜单；「使用 $ 选择技能」→ 输入 $ 弹技能菜单，选中插入 `$名称`；超 5MB 图片 / 第 5 张 / 仅非图片文件有对应提示。
6. **发送与运行**：圆形 ↑ 发送；运行中变红色停止钮 + 左侧 spinner；点击停止生效。
7. **会话隔离**：两个 Tab 各自选不同模型/力度/权限 → 互不影响；关 Tab 重开 → 选择保持（同进程）；重启应用 → 回落默认（已知限制 §5.1）。
8. **兼容**：旧会话列表/历史打开正常；设置页保存配置正常（旧 config.json 无 provider/vision 字段可加载）。
