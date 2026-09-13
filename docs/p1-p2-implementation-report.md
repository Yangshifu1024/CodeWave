# CodeWave P1 + P2 实施报告（含 J 组评估）

> 日期：2026-08-30（会话内接续 P0 实施）
> 依据：`[docs/p1-plan](./p1-plan.md).md`（A–E 组）、`[docs/p2-plan](./p2-plan.md).md`（F–J 组）
> 约束遵守：未执行任何 git 操作；仓库保持零提交。

---

## 1. 结果总览

| 项 | 状态 |
|---|---|
| `cargo test` | ✅ **120 passed / 0 failed**（P0 86 → P1/P2 新增 34） |
| `cargo check` warnings | ✅ **0** |
| 前端构建 | ✅ Vite 通过（新 UI：多 Tab/Explorer/六分区设置/任务中心/统计/子代理卡） |
| 可运行产物 | ✅ debug `.app` + dmg 重新打包，**实测启动成功、托盘常驻、退出干净** |
| git | ✅ 零操作 |

## 2. P1 对账（[docs/p1-plan](./p1-plan.md) A–E 组）

### A 组：工具补全（10/10）
| 工具 | 实现 | 测试 |
|---|---|---|
| web_fetch | 手动重定向逐跳 SSRF 校验 + dom_smoothie 正文抽取 + 每主机 1s 限流 + 50MB 上限 | 私网矩阵 22 断言 / SSRF 拦截 / 抽取 / 粗剥标签 |
| http_request | 7 方法 + 结构化预览（JSON pretty 24K / 文本 96K / 二进制元信息）+ 头脱敏 | （走同一守卫；URL 编码内联） |
| service | ServiceTable（≤16）+ 512KB 环形缓冲 + TERM→10s→KILL 进程树 + service:update 节流事件 | 端到端生命周期（start/read/list/stop）+ 环上限 |
| wait | 1–3600s 可取消 | 编译级 |
| suggest | 1–4 chips；成功即结束 run（`run:suggestions` 事件 + run:done 载荷） | run 循环特判 |
| plan | todo 状态机（≤1 in_progress/≤100）+ plan:update + sidecar 持久化 + **每 run 首请求瞬态快照**（置于 Anthropic 缓存断点之后） | 状态机规则 / 渲染标记 / 快照注入（字节稳定测试覆盖） |
| batch_read | read 兼容别名（标注 deprecated） | — |
| calculate | 手写递归下降（右结合 ^、一元负号低于 ^、20 函数、pi/e/tau） | 基础/优先级/错误清单一组 |
| render_html | ≤50k 校验；前端 sandbox iframe（无 same-origin） | 长度上限 |
| skill | SkillIndex 双通道加载 `<skill-loaded>` | 见 C 组 |

### B 组：质量增强（3/3）
- **写入后校验**：py(json 内建)/rust(rustc 单文件)/ts·js·vue(tsc/node --check)/go(got vet)；缺工具链自动跳过；失败文本进 warnings 回填模型 —— edit/create 均已接线；JSON 内建 + 汇总测试。
- **多 Key 池**：Auth 冷却 30min、瞬时 10s×2ⁿ≤30min、全冷却取最早恢复者；run 循环逐尝试选 key + 结果上报；3-key 场景矩阵测试。
- **OpenAI Responses 协议**：input/instructions 组装、function_call(_output) 映射、5 类事件映射、completed usage、**会话级 prompt_cache_key**；事件流与请求体测试。

### C 组：扩展生态
- **Skills**：目录优先级（项目 > 用户 ~/.codewave + ~/.claude 只读 > 内置 2 个原创 skill：repo-index/doc-convert）+ frontmatter（含 when_to_use 兼容）+ disabled_skills 开关 + TTL 索引 + 系统提示词层 2 注入 + `/skillname`（前端 Composer 斜杠）——扫描/覆盖/禁用测试。
- **记忆**：`~/.codewave/memories/*.md` 索引注入层 3 + 白名单内模型直写维护 —— 扫描/注入测试。
- **MCP**：rmcp 3.1.4（升级 reqwest 至 0.13 统一版本）；**stdio + streamable-http 两 transport**（后者含服务端不支持时 SSE 协商，即 docs 所列三形态）；`mcp__<server>__<tool>` 命名 + schema 归一化 + 全工具统一排序；invalid session 自动重连一次；配置 = 用户级 + 项目级覆盖；IPC：get/save_mcp_config、connect_mcp、mcp:status 事件 + 设置页 JSON 编辑器。命名/归一化/覆盖测试 + **内置+MCP 合并字节稳定回归**。

### D 组：工作区体验
- **多 Tab**：Tab{sessionId/workspace}、每 Tab 独立 RunState（Record 键控）、v-show 保状态、Cmd/Ctrl+T/W/N/←→ 快捷键。
- **WorkspaceExplorer**：单层懒加载文件树（≤3 层展开）+ 编辑器（textarea + 脏标记 + 保存走 pathutil）——**偏差：P1 用轻量编辑器替代 Ace**（见 §5）。
- **@文件提及**：P0 的路径索引直接沿用（TTL 10min + fuzzy 评分）。
- **GitDiff**：`diff_tree_to_workdir_with_index`（含 untracked→added 映射修正）+ 行统计 + patch 文本 + `recent_log`；GitDiffModal + `/diff` 命令 —— 临时仓库端到端测试。

### E 组：基础设施
- **keyring**：启动迁移（明文→钥匙串，服务 `codewave.yangshifu.xyz`，账户=model.id，多 key 按行存）；解析时占位回读；不可用环境回退明文 —— 迁移/回读/回退测试。
- **审批矩阵**：设置 Security 分区（approval.enabled / confirm_outside_create / confirm_git_push / allow_private_network + 5 语言校验开关）。
- **主题**：7 套强调色（CSS 变量切换即时生效，持久化）。
- **i18n**：新增 8 组词条（中英同步）。
- **model catalog**：`scripts/generate-model-catalog.mjs`（models.dev → 14 常用 provider / 534 模型 / 72KB）+ 设置页级联填充（自动回填 base_url/协议/窗口）。

## 3. P2 对账（[docs/p2-plan](./p2-plan.md) F–J 组）

### F 组：子代理 ✅
- **drive_agent 重构**：run_chat 抽为参数化驱动（DriveParams：max_steps/排除集/排除 MCP/system_extra/预算提醒/事件开关/强制汇报），主会话/子代理/计划任务三路共用 —— 这是本阶段最大重构，120 测试保持全绿。
- subagent 工具：必填 task/role/maxSteps（≤1000）；独立 runtime（继承 workspace/roots；clean_context=false 可带近 6 条背景）；排除 ask/subagent/plan/skill/scheduled_task/suggest/wait；**全局并发 4**（含守卫防泄漏）；步数进度轮询事件 sub:spawn/step/done/error；StopSubagent 命令；前端 SubagentInlineCard（角色徽标/步数/最近 8 工具/状态）。
- 低预算 20% 提醒注入 + 末步强制汇报。

### G 组：计划任务 ✅
- 计划文法 `cron:<5 段>`（cron crate，秒位自动补 0）/`every:<n> <m|h|d>`/`once:<RFC3339>` —— 语法矩阵测试。
- TaskTable + 15s supervisor tick（tauri async runtime 上启动）；到期触发 → **every 重排 / once 移除**（tick 测试覆盖）。
- 执行：全局串行锁 + 隔离 runtime + 30 步预算 + 无 MCP + 强制汇报；结果回写 last_status/last_summary；scheduled:fired/done 事件。
- scheduled_task 工具（create/list/delete）+ 任务中心面板（新建/列表/删除）。

### H 组：Token 统计 ✅
- 惰性启动采集器（修复了 setup 同步上下文 tokio::spawn panic 的真实 bug）+ 有界队列 2048（满则丢弃计数）+ 60s/512 条双阈值 flush + 当日残留合并 + 90 天清理。
- run 结束记录 usage（input/output/cache_read/cache_write/runs）；按天/模型/工作区聚合落盘。
- `get_token_stats(days)` + 统计面板（30 天柱状图 + 合计 + 最常用模型，无图表库自绘 SVG/CSS）。

### I 组：桌面完整 ✅
- **托盘**：TrayIconBuilder（菜单：显示/退出；左键单击聚焦）；**关闭到托盘**（ui.close_to_tray 默认开，CloseRequested 拦截隐藏）——冒烟实测常驻与退出。
- **系统通知**：run 完成/计划任务事件 + 窗口未聚焦 → 应用内通知栈 + 系统通知（plugin-notification，权限请求静默降级）。
- **updater**：插件接入但按 D7 默认禁用（tauri.conf plugins.updater.active=false——修复了无配置节点启动 panic 的真实 bug）；启用步骤已具备。

### J 组：评估报告（按 [docs/p2-plan](./p2-plan.md) §8 裁剪约定，不投入开发）
| 项 | 评估结论 |
|---|---|
| SSH 远程工作区 | **顺延**。本机当前无跨机开发场景；方案留存：首选系统 ssh + ControlMaster 复用（Windows 无 CM 每命令一连接可接受），写走 sftp batch/base64 管道，命令过 L1/L3+审批（bash AST 仅远端 bash 可靠时启用 L2）。投入预估 1–1.5 周，待真实需求出现再立项 |
| server 模式 | **挂起**。core 层已具备 host 无关性（EventSink 抽象 + drive_agent 可脱 Tauri 驱动），axum 包一层成本低（~2 天），但无 headless 使用方，暂不做；架构门已留好 |
| 移动端 | **不可行（现阶段）**。核心价值依赖本地 shell/文件系统（command/工作区写），iOS/Android 均受限；"遥控本机 agent"形态需先有 server 模式 + 公网通道，投入产出比低。Tauri 2 移动端能力保留但不承诺 |

## 4. 新增测试清单（34 个）

cache-first 字节稳定回归（agent）、SSRF 私网矩阵、web 抽取/降级、service 生命周期+环形缓冲、calculate 三组、plan 状态机/渲染、写入校验三组、KeyPool 四组、Responses 事件流+请求体、skills 三组、记忆、MCP 命名/归一化/覆盖、git diff/log 端到端、keyring 迁移回退、stats 采集对账、scheduler 文法/tick/once、计划任务文法。

## 5. 与 [docs/p1-plan](./p1-plan.md)/05 的偏差（已固化）

| # | 偏差 | 原因 |
|---|---|---|
| 1 | **Explorer 用 textarea 编辑器而非 Ace** | 减少重依赖；编辑器核心诉求（打开/改/保存/pathutil 校验）已满足；Ace 可随 P3 无痛替换（组件边界已隔离） |
| 2 | MCP 传输实现为 stdio + streamable-http（SSE 由协议协商退化覆盖），未单列 legacy SSE transport | rmcp 3.x 的 streamable-http 客户端按 MCP 规范处理两类服务端；行为等价 |
| 3 | 子代理进度事件用 800ms 轮询 runtime 历史近似（步数/最近工具），非精确逐步钩子 | 保持 drive_agent 单一实现；卡片信息需求满足 |
| 4 | model catalog 收敛为 14 个常用 provider（534 模型，72KB） | 全量 7484 模型产物过重；脚本改一行白名单即可扩 |
| 5 | 计划任务的工作区取"第一个打开的会话"快照 | [docs/p1-plan](./p1-plan.md) 要求创建会话快照——实现从简，后续可在 ScheduledTask 增 workspace 字段固化 |
| 6 | run:suggestions 的 chips 在 suggest 成功的 run:done 载荷 + 独立事件双发 | 兼容恢复会话与实时两种展示路径 |

## 6. 诚实边界（未验证项）

- 真实 MCP server 接入（场景 D）、真实网页抓取（场景 E）、真实 LLM 端到端（场景 H 三并行子代理）、`every:1m` 实际定时触发（场景 I）——均需外部资源/长时观察，单测覆盖了对应逻辑层。
- Windows/Linux/三平台 CI 未运行（需推送）。
- 计划任务为进程本地（重启清空）——[docs/p2-plan](./p2-plan.md) §3 约定如此，UI 有提示。


---

## 8. 追加变更：UI 样式回归原生（2026-08-30）

应用户要求移除自定义 UI 外观，恢复原生形态：

| 变更 | 之前 | 现在 |
|---|---|---|
| 窗口装饰 | `decorations:false` + 自定义标题栏（拖拽区/自绘最小化·最大化·关闭） | **系统原生标题栏**（tauri.conf 移除 decorations 配置） |
| 主题 | 强制暗色 + 7 套自定义强调色皮肤 | **跟随系统**（Naive UI `useOsTheme`：系统暗色→darkTheme，否则默认亮色）；CSS token 双调色板随系统切换 |
| 强调色 | settings 可选 7 色 + localStorage 持久化 | 已移除（Naive UI 默认 primary；后端 `ui.accent` 字段保留以兼容旧 config，不再使用） |
| 自定义色板 | `--ws-*` 单套暗色值 | 保留同名 token 作为组件桥接，但值映射为 Naive UI 中性色（亮/暗双套，`html.dark` 切换）——视觉语言即组件库默认 |
| 通知 | 自绘通知卡片 | Naive UI `NAlert` 栈 |
| Tab | 自绘 tab 条 | Naive UI `NTabs`（card 型）+ 关闭按钮 |

保留未动：无边框以外的交互功能（多 Tab/Explorer/面板/快捷键/托盘行为 close-to-tray——托盘常驻与退出语义不变）。
验证：前端构建绿、120 测试全绿、重新打包后实测启动正常（原生标题栏、跟随系统主题）。


---

## 9. 追加变更：模型提供商全量支持（2026-08-30）

应用户要求，模型目录从 14 个常用 provider 扩为 **models.dev 全量**：

- **覆盖**：207 家 provider / 7484 个模型（compact 760KB，gzip 后 ~93KB），按需动态加载（独立 chunk，不进主包，主包体积不变）
- **协议推断**：npm 后缀 `anthropic` → anthropic_messages（10 家）；其余（openai-compatible 167 家等）→ openai_chat；`provider.api` 字段直接作为 base_url
- **兼容性标注**：bedrock/vertex/azure/watsonx 4 家为专有协议（SigV4/OAuth/IAM/资源 URL），标记 `compat:false`，UI 选择时明确警告"需走兼容网关或手工配置"——诚实呈现而非静默失败
- **已知端点兜底**：openai/anthropic/google（OpenAI 兼容端点）/cohere（compatibility 端点）/ollama 内置官方兼容 URL
- **设置页**：供应商与模型两级可搜索下拉（filterable），模型条目展示上下文窗口与 reasoning 标记；选择后自动回填 name/base_url/协议/上下文窗口/最大输出
- 生成器 `scripts/generate-model-catalog.mjs` 重写（协议推断 + 兼容标记 + compact 输出）；验证：构建绿（chunk 拆分正确）、打包冒烟通过


---

## 10. 追加变更：设置弹窗布局修复（2026-08-30）

用户反馈设置界面丑陋。根因与修复：

1. **弹窗铺满全屏**：Naive UI Modal teleport 到 body 后，scoped 的 `.settings-modal { width: 820px }` 不生效 → 四个 Modal（设置/变更/任务/统计）宽度全部改为 `:style` 内联绑定（teleport 下必然命中）。
2. **表单 label 竖排换行**（"名/称"、"协议格/式"）：手写 flex 表单 CSS 压缩了 label → 设置表单整体换 **Naive UI 原生 `NForm` + `NFormItem`**（label-placement=left、固定 label 宽度），删除全部手搓表单样式。
3. 顺带：模型列表行改为两行结构（名称+ID 分行、省略号），footer 关闭按钮改为标准"取消"。

结论：属于自定义布局 CSS 与组件库 Modal 机制的冲突，与上一节"回归原生"同方向——现在表单/弹窗完全由组件库接管。验证：构建绿、打包冒烟通过。


---

## 11. 追加变更：全面去除手搓样式，组件库接管（2026-08-30）

继 §8/§10 后，把剩余手搓 UI 全部替换为 Naive UI 组件与主题体系：

| 区域 | 之前 | 现在 |
|---|---|---|
| 外壳布局 | 自绘 flex 工具栏/侧栏/主区 + 手写 `--ws-*` 调色板 | **NLayout / NLayoutHeader / NLayoutSider**；主题 token 由 **useThemeVars()** 注入 `--ws-*`（唯一来源 = Naive 主题，跟随系统亮暗） |
| Tab 条 | 自绘关闭按钮 | NTabs 原生 `closable` + `@close` |
| 输入框 | 自绘 textarea + 手写 autoGrow | NInput textarea `:autosize` |
| 命令/提及菜单 | 自绘浮层 | **NPopover**（manual 触发，锚定输入框） |
| 通知/错误行 | 自绘卡片/红线 | NAlert |
| 提问/审批面板 | 自绘面板 | NCard + NTag + NSpace |
| 计划面板 | 自绘清单 | NCard + NTag 状态 |
| 上下文信息条 | 自绘彩字 | NText（primary/depth3）+ NSpace |
| 子代理卡 | 自绘卡 + 手写步数文本 | NCard + NTag + **NProgress** |
| 会话抽屉 | 自绘行 | **NList + NListItem + NThing** + NTag 当前标记 |
| git patch / 命令输出 / JSON 详情 | 自绘 pre | **NCode**（hljs 经 ConfigProvider 下发，diff/bash/json 高亮） |
| 文件树 | 自绘缩进树 | **NTree**（懒加载 on-load、block-line、选中打开） |

保留的自定义内容仅剩：Markdown 渲染样式（渲染所需）、diff 行配色（无对应组件）、少量纯布局 flex。`--ws-*` 变量名保留为组件与 Naive 主题之间的桥接层，值全部来自主题 token。
验证：构建绿、打包冒烟通过（原生标题栏 + 跟随系统主题 + NLayout 外壳）。


---

## 12. 修复：设置弹窗空白（组件名导入错误）

§10 重写时设置弹窗误导入 `NTab`（AppShell 的页签组件）而非 `NTabPane`，未注册的窗格导致整个设置内容区渲染为空。已改为正确导入并重新打包验证（启动正常）。教训：Naive UI 中 `NTab`（配合 NTabs 的纯页签）与 `NTabPane`（带内容窗格）语义不同。


---

## 13. 追加：问题排查轮（2026-08-30）

针对"验证还有没有其他问题"做了一轮系统化排查，**共发现并修复 4 个问题**：

| # | 问题 | 根因 | 修复 |
|---|---|---|---|
| 1 | ChatMessages 建议按钮渲染为未知元素 | 缺 `NButton` 导入（静态扫描发现） | 补导入 |
| 2 | 文件浏览器编辑器头部布局失效 | 缺 `NSpace` 导入（静态扫描发现） | 补导入 |
| 3 | 未选工作区时设置页技能列表恒为空 | `list_skills` 强依赖 session；无会话时前端根本不调用 | 后端 `session_id` 改可选（回退用户级目录扫描），前端无条件加载 |
| 4 | IPC 拒绝时（如浏览器调试）启动链路中断 | `settings.load()` 无守卫 | 回退默认配置 |

**新增验证设施**（防止此类问题再漏）：`frontend/src/__tests__/app.smoke.test.ts` —— vitest + happy-dom + Tauri IPC mock，**完整挂载 App** 断言 6 类渲染：主窗口工具栏/占位符/模型名、设置弹窗 5 分区页签 + NForm 字段 + 目录级联、MCP 编辑器、技能开关、会话抽屉 fixture、`/` 命令菜单。运行：`pnpm --dir frontend test`（当前 6/6 绿）。此类测试专门捕捉「构建通过但渲染空白」的组件级回归（NTabPane 事件即其原型）。

其余核查项均无问题：120 cargo 测试全绿、0 warnings、静态导入交叉扫描（误报外无缺失）、打包冒烟通过、运行日志无 error/panic。
