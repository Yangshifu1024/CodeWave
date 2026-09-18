# docs/0-README · 文档总目录

> 本文件是 docs/ 的唯一目录。约定：
>
> - **文档文件名一律不带编号**，用英文主题 slug 命名（存量文件已从旧编号体系迁移，`git mv` 保留历史）。
> - 新增文档后**在本文件登记一行**：日期 + 链接 + 一句话摘要，归入对应主题组。
> - 文中引用其他文档一律用 markdown 链接（编辑器内可点击跳转）。

## 阅读路径建议

1. [technical-design](./technical-design.md) — 技术方案基准（决策记录 D1–D7、分层规则）
2. [p0-implementation-report](./p0-implementation-report.md) / [p1-p2-implementation-report](./p1-p2-implementation-report.md) — 各阶段落地情况
3. [plan-mode-workflow](./plan-mode-workflow.md) — plan 档阶段化流程；[standard-workflow](./standard-workflow.md) — 标准工作流（完整研发流水线，原 arch 技能内置化）
4. 其余按下方主题组按需查阅

## 基准与方案

- 2026-08-30 · [technical-design.md](./technical-design.md) — 技术方案基准（决策记录 D1–D7、分层规则）
- 2026-08-30 · [p0-plan.md](./p0-plan.md) — P0 阶段方案
- 2026-08-30 · [p1-plan.md](./p1-plan.md) — P1 阶段方案
- 2026-08-30 · [p2-plan.md](./p2-plan.md) — P2 阶段方案
- 2026-09-03 · [tools-optimization-and-gap-fill-plan.md](./tools-optimization-and-gap-fill-plan.md) — 内置工具优化与补齐实施方案（基于 builtin-tools-source-comparison 的分级建议：批次 A 现有工具优化 / 批次 B 审批 diff 预览与外部路径放行 / 批次 C 补齐 web_search、后台子代理、按模型裁剪）
- 2026-09-06 · [command-output-ansi-sanitize-plan.md](./command-output-ansi-sanitize-plan.md) — command 输出链 ANSI 乱码与「正在运行？」锚点缺陷根因分析 + 修复方案（已批准待实施）：后端 sanitize 模块 + progressTail 落定清空 + 中性标题
- 2026-09-12 · [plan-branch-proposal.md](./plan-branch-proposal.md) — 计划批准即定分支：S4/P3 拟定分支名（`<type>/<slug>`）+ 批准询问列明 + 批准 = 预授权创建/切换分支（agent 执行 `git switch -c`，零协议改动）；fence branch 写形态缺口独立立项

## 早期实施与评审

- 2026-08-30 · [p0-implementation-report.md](./p0-implementation-report.md) — P0 实施报告
- 2026-08-30 · [p1-p2-implementation-report.md](./p1-p2-implementation-report.md) — P1/P2 实施报告（尚未端到端验证项见其 §6）
- 2026-08-30 · [code-review-findings.md](./code-review-findings.md) — 代码评审发现；遗留项以 §四/§五 为准
- 2026-08-30 · [project-multi-root-refactor.md](./project-multi-root-refactor.md) — 项目目录化重构报告（历史背景；现行语义以 session-semantics-and-ui-batch 为准）
- 2026-08-30 · [quality-and-feature-batch-report.md](./quality-and-feature-batch-report.md) — A/B 批次报告：双代理审查与修复、真实 GLM E2E 与真实 MCP 验证、mermaid/katex 等功能补全
- 2026-08-31 · [session-semantics-and-ui-batch.md](./session-semantics-and-ui-batch.md) — 会话语义重构（项目单目录 + 数据随项目走 + 临时会话免目录）+ 界面批次

## 架构与核心机制

- 2026-08-30 · [thinking-interleave-report.md](./thinking-interleave-report.md) — 思考过程穿插显示：帧协议拆分（delta_text/delta_thinking）+ 前端 timeline 转录模型
- 2026-09-01 · [session-logging-report.md](./session-logging-report.md) — 日志增强：会话级细粒度黑匣子日志（run/LLM 请求/工具/审批/压缩/子代理全轨迹）+ 全局级别热切换 + 14 天滚动清理 + RightBar 日志查看器
- 2026-09-01 · [session-artifacts-and-files-tab.md](./session-artifacts-and-files-tab.md) — 会话产物登记（create/edit 边车 + 子代理归属主会话）+ 右侧栏标签页化（信息/日志/文件）+ 产物点击查看
- 2026-09-02 · [tool-optimizations-port.md](./tool-optimizations-port.md) — 工具链优化：工具输出瘦身 + run panic 兜底（主循环 catch_unwind + 子代理收尾 guard）+ compaction 强化
- 2026-09-02 · [session-auto-title.md](./session-auto-title.md) — title 子代理会话自动命名：首条消息后一次性 LLM 生成 ≤20 字标题
- 2026-09-03 · [message-timestamps.md](./message-timestamps.md) — 消息时间戳持久化缺陷修复：后端 Message.created_at + 入转录盖章 + 前端透传渲染
- 2026-09-03 · [notification-click-reveal.md](./notification-click-reveal.md) — 系统通知点击回跳：后端按平台直驱 + 会话窗口 reveal + 失败回退链
- 2026-09-04 · [macos-notify-use-default-dialog-fix.md](./macos-notify-use-default-dialog-fix.md) — 缺陷修复：macOS 首次通知弹「Choose Application」系统对话框（mac-notification-sys 内部 Once 消费 + 主动 set_application）
- 2026-09-08 · [windows-toast-aumid.md](./windows-toast-aumid.md) — 缺陷修复：Windows 通知显示 PowerShell 图标/标题且点击不回跳（AUMID 借用 POWERSHELL_APP_ID 之故）——NSIS 钩子 + WiX 片段在安装时写 AppUserModelId 注册表键，运行时只读检测 + 回退链，依赖升级 tauri-winrt-notification 0.8.1
- 2026-09-16 · [reasoning-content-passthrough.md](./reasoning-content-passthrough.md) — 缺陷修复：thinking 上游要求历史 assistant 回传 `reasoning_content`（缺失即 400，DeepSeek 系经中转）——出站映射按内容块顺序拼接思考块上 wire + 落盘保留思考（`sanitize_for_save`）/ 8MB 回退改「剥图留思考」（`sanitize_keep_thinking`）+ 400 文案分类（Rejected/Demanded/Unrelated）与会话级粘性标记（拒收型端点一次命中即停发、Demanded 复位自愈、子代理继承），空守卫与 anthropic/openai_responses 边界不变，前端零改动

## 编排与工作流

- 2026-08-31 · [plan-mode-workflow.md](./plan-mode-workflow.md) — plan 档强制阶段流程（P0 分类 → 澄清 → P2 分析 → P3 方案 → 批准 → P4 执行 → P5 审查 → P6 汇报）
- 2026-09-07 · [plan-discipline-host-enforcement.md](./plan-discipline-host-enforcement.md) — plan 纪律宿主强制：批次层两硬一软三门（E_PLAN_REQUIRED / E_PLAN_STALE / in_progress 软提醒）+ 子代理/同批双豁免 + wire 层 Tool 消息丢块语义钉死（含 read 图片注入同通道遗留缺陷记录）
- 2026-08-31 · [arch-orchestrator.md](./arch-orchestrator.md) — 内置 arch 编排智能体：`$arch` 技能全流程编排 + 内置子代理角色注册表 + `.codewave/tasks/` 四文档产物契约
- 2026-09-02 · [run-queue-and-ask-revamp.md](./run-queue-and-ask-revamp.md) — 运行队列（运行中提交入队顺序执行）+ 询问窗口重构（编号选项/键盘导航/分页）+ 命令白名单 + 计划卡片
- 2026-09-03 · [session-nav-new-top.md](./session-nav-new-top.md) — 左栏新会话置顶（合成兜底 meta + navOrder 统一比较器）
- 2026-09-04 · [session-nav-row-states.md](./session-nav-row-states.md) — 会话行状态批次：等待确认行操作互斥 + 会话结束未读点 + 审批等待策略可配置（auto_confirm 5 分钟自动确认）
- 2026-09-04 · [subagent-interaction-drawer.md](./subagent-interaction-drawer.md) — 子智能体交互批次：聊天卡单行化 + 子代理流式帧经 Frame::Sub 信封下发 + 过程抽屉 + 完整历史落盘
- 2026-09-07 · [dev-subagent-specialization.md](./dev-subagent-specialization.md) — dev 子代理专业化拆分：backend-dev（语言无关后端）/ frontend-dev（现代前端框架）/ app-dev（移动+桌面双端），arch S6/S7 按任务包技术栈自动选角，彻底移除 dev 角色名
   - 2026-09-06 · [subdrawer-scroll-hardening.md](./subdrawer-scroll-hardening.md) — 子代理过程抽屉滚动加固批次：滚动链三层防线（CSS 钉死 + 视口兜底钳制 + 运行时探针）+ 贴底语义对齐 + 任务块可折叠
- 2026-09-07 · [standard-workflow.md](./standard-workflow.md) — arch 技能内置化为标准工作流：分档路由（轻量直改/完整流水线）+ 常驻 WORKFLOW_SECTION 注入 + 尽量并行（S1∥S2、S6 额度内拉满、S7∥S8）+ 产物与批准门保留，$arch 移除
- 2026-09-08 · [subagent-file-isolation.md](./subagent-file-isolation.md) — 子代理隔离与运行监督批次：文件写认领制（兄弟 runtime 写已认领路径 E_FILE_CLAIMED，主会话豁免）+ 取消级联（parent_cancel child_token）+ 子代理卡停止按钮（E_SUBAGENT_STOPPED → ask 询问重派）+ 派发失败重试硬化（E_ARGS alias/文案、BUSY 有界等待）+ 运行监督（重复失败纠偏/终止 + 流停滞看门狗 stall_timeout_seconds）+ 步数真实化（step_count 取代 history.len()）+ 主会话读预算（E_READ_TOO_BROAD 强制委派 explore）+ ask 多题分页提交修复

## 界面与交互

- 2026-08-31 · [composer-toolbar-batch-report.md](./composer-toolbar-batch-report.md) — Composer 工具条重构（权限四档 + 会话级模型/力度 + 附件/$技能 + 供应商分组与视觉标签）
- 2026-09-08 · [slash-skills-and-dollar-agents.md](./slash-skills-and-dollar-agents.md) — 技能触发符 `/` 化（/ 纯技能菜单、/name 点名语义入 available-skills、命令入口移除）+ `$` 改为内置子代理点名（list_agents IPC + 回填 $role + 核心提示 $<role> 委派规则）+ 技能加载路径定稿（项目/全局 .codewave/skills > 工作区 .agents/skills、.claude/skills > ~/.claude/skills > 内置；含 rt.data_dir 恒为全局的语义钉子与漏扫缺陷修复）+ 列表显示排序（内置 > 项目 > 全局 > 其他）与点击详情弹层（共享 SkillDetailModal：右栏技能行 + composer / 菜单，get_skill IPC + SKILL.md 正文渲染），三候选乱序守卫补齐
- 2026-08-31 · [composer-shift-tab-mode-cycle.md](./composer-shift-tab-mode-cycle.md) — Composer Shift+Tab 循环切换权限模式 + 权限胶囊按档位着色
- 2026-08-31 · [thinking-scroll-fix.md](./thinking-scroll-fix.md) — 缺陷修复：思考流式期间无法向上滚动（豁免窗口永续 + 几何重接管）
- 2026-08-31 · [composer-paste-and-history-recall.md](./composer-paste-and-history-recall.md) — Composer 粘贴图片（clipboardData 双通道 + vision 软阻断）+ ↑↓ 历史消息召回
- 2026-09-02 · [custom-font-and-titlebar.md](./custom-font-and-titlebar.md) — 自定义字体（sans/mono 双槽 CSS token）+ 自绘标题栏（tauri-plugin-decoration v3）
- 2026-09-03 · [sidebar-toggle-buttons.md](./sidebar-toggle-buttons.md) — 左右侧栏顶部展开/折叠按钮 + 侧栏开关入口收敛
- 2026-09-03 · [settings-forms-vertical.md](./settings-forms-vertical.md) — 设置弹窗表单全量 vertical 化
- 2026-09-03 · [workspace-explorer-removal-and-chat-scrollbar.md](./workspace-explorer-removal-and-chat-scrollbar.md) — 移除左栏工作区文件面板 + 聊天区滚动条主题化
- 2026-09-03 · [antd6-upgrade-and-composer-border-beam.md](./antd6-upgrade-and-composer-border-beam.md) — antd 6.6 升级（唯一破坏点 Tabs tabPlacement/start）+ Composer 多条流光边框
- 2026-09-03 · [topbar-migration-and-git-identity.md](./topbar-migration-and-git-identity.md) — 顶栏「变更/任务/统计/设置」四入口迁移 + 左下角 git 提交身份条
- 2026-09-03 · [titlebar-content-batch.md](./titlebar-content-batch.md) — 顶栏标题栏内容批次：两段式背景 + 会话历史导航（后移除）+ 标题/工作目录/分支胶囊 + 用户消息悬停操作
- 2026-09-03 · [focus-ring-fix.md](./focus-ring-fix.md) — 输入控件 focus 双重边框缺陷修复：token 全局 controlOutline transparent
- 2026-09-04 · [rightbar-visual-batch.md](./rightbar-visual-batch.md) — 右栏视觉批次：侧栏分色 token + 页签调序 + 日志滚动根因修复
- 2026-09-18 · [rightbar-info-refactor-and-subscription-quota.md](./rightbar-info-refactor-and-subscription-quota.md) — 右栏信息面板重构 + 订阅额度 + 可拖拽栏宽：数据目录行/会话段移除、Files 应用与编辑器下拉（VS Code/Cursor/Windsurf/Zed/Sublime/Notepad++/JetBrains）、技能与计划可折叠、`core/quota` 七家额度提供商（OpenCode Go/DeepSeek/MiniMax 国际+CN/Kimi/Zhipu/Z.ai，自动读本机 opencode 凭证）、手写栏宽分隔条（记忆 + 双击复位 + 窄窗只夹显示）、窗口最小宽 960→1024
- 2026-09-04 · [titlebar-logo-toggle.md](./titlebar-logo-toggle.md) — 标题栏批次：会话历史导航移除 + 左栏开合入口收口到标题栏 Logo（悬停同位交换）
- 2026-09-04 · [titlebar-logo-right-segment.md](./titlebar-logo-right-segment.md) — 标题栏 Logo 移至右段段首（同位交换交互保留；左段退化为纯背景带）
- 2026-09-04 · [sidebar-collapse-animation-and-titlebar-blend.md](./sidebar-collapse-animation-and-titlebar-blend.md) — 侧栏折叠动画与标题栏融合批次：左栏折叠 0 宽完全隐藏 + 标题栏左段融合 + 右栏裁切折叠动画
- 2026-09-04 · [notify-top-center-and-ask-question-dedup.md](./notify-top-center-and-ask-question-dedup.md) — 通知顶部居中 + ask 计划批准问句去重（纯渲染层契约零变化）
- 2026-09-06 · [dropdown-selected-fill.md](./dropdown-selected-fill.md) — Dropdown 选中项高亮中性化：App.tsx alias token 覆盖，全应用下拉/Select 选中面统一
- 2026-09-06 · [thinking-marquee-rewrite.md](./thinking-marquee-rewrite.md) — 思考跑马灯重写：全宽 band 展示最新行、换行上翻、溢出左爬（同行增长不翻页）
- 2026-09-11 · [shell-path-echo-and-lightweight-gate.md](./shell-path-echo-and-lightweight-gate.md) — 设置 Shell 下拉旁回显可执行文件绝对路径（auto 同源取探测默认项；WSL 占位）+ lightweight 批准门移除 todos≤3 上限（G3 非空与 skipAnalysis 口令通道不变）
- 2026-09-13 · [composer-per-tab-draft.md](./composer-per-tab-draft.md) — 缺陷修复批次：composer 草稿按 Tab 隔离（run store 顶层 `drafts` 平行分桶 + 显式 key 防异步窗口竞态，击键不再广播整桶重渲染）+ 带图消息「修改」回显图片（ws:composer-fill detail 带 images，与队列编辑同一还原链）+ 切 Tab 收起提及菜单残留
- 2026-09-16 · [session-restore-batch1.md](./session-restore-batch1.md) — 会话保存与恢复优化 · 批1「回到现场」：后端 `ui-state.json`（schema v1 + 原子写 + 损坏备份）+ 中断标记与退出拦截（`running.marker` / `app:exit_requested`，事件面 27 → 28 键）+ 前端启动 hydrate（Tab/树态/未读/草稿/面板态）与滚动锚点（sig + 偏移，替掉切 Tab 贴底硬重置）+ 关 Tab 二次确认
- 2026-09-16 · [empty-assistant-and-request-rebuild-fix.md](./empty-assistant-and-request-rebuild-fix.md) — 缺陷修复：空 assistant 消息上 wire（400 Invalid assistant message）与「修复后重试」不重建请求体（sanitize 结果从未发出，同文 400 相隔 2.45s）——三层纵深防御（历史层清理 / 出网副本 repair / wire 层拦截）+ 出网消息数组幂等性守护

## 供应商与设置

- 2026-09-02 · [provider-management-refactor.md](./provider-management-refactor.md) — 供应商管理重构：config schema v2（providers 嵌套 models）+ 移除 models.dev 目录 + 设置弹窗供应商页签 + keyring 账户迁移
- 2026-09-03 · [provider-form-validation.md](./provider-form-validation.md) — AI 供应商表单校验：新增点击校验 + 编辑实时红字 + 主弹框保存守卫
- 2026-09-03 · [provider-form-rules-tightened.md](./provider-form-rules-tightened.md) — 供应商表单规则收紧：API 格式必填 + API Key 必填 + 模型列表非空
- 2026-09-04 · [max-tokens-truncation-fix.md](./max-tokens-truncation-fix.md) — max_tokens 截断缺陷修复：provider 三协议截断尾注 + 新模型默认 32768 + MAX_TOKENS_NOTICE 公共常量
- 2026-09-13 · [network-proxy-settings.md](./network-proxy-settings.md) — 设置「网络」页签：代理模式三选一卡片（无代理/系统代理/自定义代理，支持 http(s)/socks5）+ Windows 注册表探测补齐 + 应用请求与更新请求全链路走代理 + save_config 热重建 client 即时生效（proxy=null 保持 reqwest 默认，存量零变化）
- 2026-09-15 · [provider-custom-headers.md](./provider-custom-headers.md) — 供应商级自定义请求头：per-provider headers（明文存 config）+ 默认 `User-Agent: CodeWave/<版本>`（可覆盖）+ `${session_id}` 占位符 + 保留名保护/日志脱敏/前后端校验；落地 OpenCode Go 对 UA 与 `x-opencode-session` 的要求

## ask / 审批交互

- 2026-09-03 · [ask-ink-accent-and-composer-cover.md](./ask-ink-accent-and-composer-cover.md) — ask 界面对齐批次：全局强调色墨化（colorPrimary 亮 #1f1f1f/暗 #424242）+ 权限胶囊色彩强度映射风险等级 + 提问卡覆盖 Composer
- 2026-09-03 · [ask-unified-plan-card-and-answer-switch.md](./ask-unified-plan-card-and-answer-switch.md) — ask 统一计划卡片（所有 ask 落盘计划文件）+ ConfirmEach 档有效应答切自动编辑档 + 忽略按钮末页直提缺陷修复
- 2026-09-04 · [ask-ignore-not-answered-fix.md](./ask-ignore-not-answered-fix.md) — 缺陷修复：ask「忽略」显式失败（返回 err E_ASK_NOT_ANSWERED，错误消息硬约束模型不继续执行）
- 2026-09-04 · [ask-approval-shape-note-nav.md](./ask-approval-shape-note-nav.md) — ask 交互加固批次：批准形判定单一事实源（后端识别）+ 选项视觉区分（复选/单选）+ 补充说明纳入键盘导航环
- 2026-09-13 · [skill-ask-norm.md](./skill-ask-norm.md) — skill 注入条件式 ask 交互规范：任意技能加载时在 <skill-loaded> 闭合标签前附加 <ask-interaction-norm>，提问型技能（如用户级 grilling）提问轮次必经 ask 弹窗（推荐答案→recommended、>5 问拆连续调用、ask 不可用降级文本格式），无提问轮次技能行为零变化；关键词匹配否决 / frontmatter 声明搁置（YAGNI）的理由存档

## 安全与工具链

- 2026-09-19 · [fence-plan-readonly-gh-and-block-message.md](./fence-plan-readonly-gh-and-block-message.md) — plan 档围栏两项：`E_PLAN_READONLY` 错误文案**点名被拦命令**（`command_excerpt`：折叠换行 + 截断 120 字符）+ `gh` 按**子命令白名单**放行（pr/run/release/issue/repo/workflow 的 view|list；`gh api` 仅隐式/显式 GET，出现 `-f/--field/--input` 即判写；pr merge / release edit / api -X POST / secret set 等远端写继续被拦；比对前先做引号/外壳归一化，换行纳为命令分隔符且行继续（`\`+换行）先归一化）
- 2026-09-03 · [builtin-tools-source-comparison.md](./builtin-tools-source-comparison.md) — 内置工具设计说明（11 组工具的实现解析：入参/出参/实现逻辑 + 分级优化建议）
- 2026-09-03 · [edit-tool-optimization-report.md](./edit-tool-optimization-report.md) — edit 工具优化批次：EOL 归一 + 两级低风险模糊替换 + 进程级文件写互斥 + ConfirmEach 审批 diff 预览
- 2026-09-03 · [tool-card-multi-file-summary.md](./tool-card-multi-file-summary.md) — 工具卡头部批量文件名展示：read/edit 批量入参 summary 列全部 basename（修复入参截断后头部空白）
- 2026-09-04 · [fence-plan-readonly-powershell.md](./fence-plan-readonly-powershell.md) — 缺陷修复：fence 计划模式白名单误拦 PowerShell 只读命令（白名单追加 PowerShell 只读 cmdlet/别名段）
- 2026-09-05 · [budget-notice-step-fix.md](./budget-notice-step-fix.md) — 缺陷修复：budget_notice 低预算提醒在「消耗 20%」时误触发（升序计数器当剩余量用的方向反转）
- 2026-09-05 · [arithmetic-audit.md](./arithmetic-audit.md) — 全仓算术/计算点审查清单：三路并行审查 ~180 处计算点，14 处存疑逐一复核
- 2026-09-05 · [arithmetic-fixes-batch.md](./arithmetic-fixes-batch.md) — 计算点修复批次：审查清单 14 项全修（后端 8 文件 + 前端 4 文件，契约零变化）
- 2026-09-06 · [shell-threat-analysis-survey.md](./shell-threat-analysis-survey.md) — 技术调研：命令行威胁分析方案（tree-sitter 深入 + Rust 生态对比 + fence 对照）
- 2026-09-06 · [fence-hardening-and-powershell-ast.md](./fence-hardening-and-powershell-ast.md) — fence 加固批次：auto_confirm 豁免灾难级 + 命令名反混淆 + PowerShell AST 路径 + check_write_target 四象限重构
- 2026-09-11 · [posix-command-risk-matrix.md](./posix-command-risk-matrix.md) — fence 全集加固批次：R1 隐式写词表（gzip/bzip2/xz/zstd/lz4 家族 + stdout flag 豁免）+ L0 白名单扩容（压缩只读/文件查证/tar/unzip）+ sed -i/tar -x/unzip/cpio -i 判定 + WRITE_LAST 与下载写 flag 扩充 + 进程/持久化/远程/灾难扩充（fdisk/diskutil erase）+ POSIX.1-2017 全集 162 条风险矩阵
- 2026-09-15 · [subagent-text-turn-premature-exit.md](./subagent-text-turn-premature-exit.md) — 缺陷修复：非主会话 run 把「无工具调用回合」（过程旁白 / 唯一调用参数不可解析被拒）当成最终汇报提前成功退出（`drive.rs` 空 calls 无条件 break）——`DriveParams.finish_on_text` + 纯函数 `text_turn_action`（`<report>` 标记收尾 / 有界续跑 MAX_TEXT_TURNS / 超限显式失败）+ 被拒调用以 user 提示反馈模型不再静默丢弃 + 空 assistant 消息不入历史；`sub:done` 增 `steps_used`/`ended`，子代理卡区分「提前结束/预算耗尽」与绿勾（含脚本化 SSE 端到端与反向验证）

- 2026-09-16 · [rejected-call-silent-finish.md](./rejected-call-silent-finish.md) — 缺陷修复：主会话「正文非空 + 全部工具调用被拒」回合静默成功收尾（`text_turn_action` 新增 `rejected` 维度并先于 `finish_on_text` 判定，提示注入后必须让模型看到；超限显式失败；`diagnose_unparsable` + `log_rejected_calls` 补齐被拒 args 取证；含反向验证）
- 2026-09-15 · [lsp-post-write-diagnostics.md](./lsp-post-write-diagnostics.md) — LSP 写后语义校验：写后校验从**单文件外部命令**（rustc / tsc / go vet 单文件各自看不到 crate 兄弟模块、tsconfig/node_modules、同包其他文件 → **误报是结构性的**；`npx --yes typescript@5` 每写必付冷启动；工具链缺失时 `ran=false, ok=true` 渲染成空串、批次里有一个文件跑过就整批打「通过」即**假通过**）升级为项目级常驻 LSP 语义诊断——新增后端顶层 `src-tauri/src/lsp/`（10 文件：framing / 客户端 / 发现 / 池 / 门面 / 安装，纯 Rust 不依赖 tauri）+ 六语言（TS/JS、Rust、Python、Go、Java 默认关闭、Dart）+ 写前 `didOpen` 基线 → 原子写 → 写后 `didChange` 等 ≤1500ms 差集（只回喂 severity==1 新增 + 指纹刹车 + 预算截断；基线不可信则「未就绪」+ 异步补条经 `run:inject`）+ 三态 `Passed/Diagnosed/Skipped{六种原因}`（**未运行绝不出「通过」**，逐文件成文消除批次假通过）+ 新鲜 PATH（Windows 注册表环境块 / login shell，探测一次 + 缓存 + 手动重新探测）+ 配置 `validation.lsp` 子对象（serde default，旧五 bool 语义升格为 LSP 开关）+ **事件面 28 → 29 键**（`lsp:server_missing` 三景引导卡）+ 5 条 IPC（`lsp_status/install/enable/redetect/restart`；`lsp_install` 未走审批门 = 本批次唯一安全让步）；后端 758 passed（+88）+ 集成测试 14 项、前端 545 passed / 65 文件（含 29 键硬锚点）；决策推翻记账 [builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §13.4 / §15 P3-1

## 工程化与开源

- 2026-08-30 · [dev-setup.md](./dev-setup.md) — 三平台开发环境准备
- 2026-09-04 · [oss-prep-batch.md](./oss-prep-batch.md) — 开源准备批次：.github 全套 + MIT LICENSE + 注释英译 + 全量 i18n + AI 回复语言 + 旧版本兼容移除 + v0.2.0
- 2026-09-06 · [long-file-split-and-edition-2024.md](./long-file-split-and-edition-2024.md) — 超长文件拆分 + edition 2024 升级批次（ask/fence/edit/command 测试外移 + run.ts 拆分 + agent.rs 拆分 + MSRV 1.85）
- 2026-09-06 · [follow-ups-batch.md](./follow-ups-batch.md) — 后续建议收敛批次：drive_agent 三 helper 拆分 + mod.rs 纯声明枢纽 + clippy 全量清零 + ci line-limit job
- 2026-09-06 · [session-pref-switch-toast.md](./session-pref-switch-toast.md) — 模式/模型切换 toast + 生效时机确认（prefs 全程 Mutex 实时读取，运行中切换下一轮生效）
- 2026-09-06 · [auth-error-guidance.md](./auth-error-guidance.md) — 认证失败引导批次：可读供应商错误文案 + run:error 附结构化 kind + 错误卡直达供应商设置 + key 冷却随保存清零
- 2026-09-07 · [subagent-ghost-session-fix.md](./subagent-ghost-session-fix.md) — 缺陷修复：子代理/任务运行 checkpoint 泄漏进主会话索引（项目下 untitled 幽灵会话）——runtime 加 is_main_session 身份字段 + checkpoint 早退 + 启动时存量清理
- 2026-09-13 · [codewave-rename-and-oss-prep.md](./codewave-rename-and-oss-prep.md) — 品牌改名 CodeWave 批次（数据目录 `.codewave` 常量收拢 / bundle id+keyring 换新 / `CODEWAVE.md` 指令兼容）+ 开源前审查与修复（license 元数据 / CoC / SECURITY / README 重写 / 残留清理，🔴 0 · 🟡 4 全修）
- 2026-09-13 · [version-bump-and-release.md](./version-bump-and-release.md) — 版本升级与发版流程（参照 PlanWave 移植）：`pnpm bump <x.y.z>` 统一改 4 处版本号 + 刷新两锁文件（守卫修正：按正则命中判失败，同版本 no-op 合法）+ `/codewave-release` 发版技能（门禁 → bump → commit → 确认后推 tag 触发 draft Release 三平台构建）
- 2026-09-14 · [macos-signing-and-notarization.md](./macos-signing-and-notarization.md) — macOS Developer ID 签名 + 公证操作指南：证书 → App Store Connect API key → 6 个 GitHub secrets → release.yml 管道补缺（`AuthKey.p8` 写入 + 绝对路径）→ draft 发版验证，参照 GitWave 同款已验证实现
- 2026-09-14 · [prompt-caching-hardening.md](./prompt-caching-hardening.md) — Prompt 缓存命中强化批次（对标 V2EX Ally「高缓存命中」：三件套已落地，做真实增量）：system prompt run 内字节冻结 + Anthropic 断点重排（tools / system 双块 / 末条 / 历史代际滞回锚点）+ 统计面板命中率与口径修复（anthropic/openai input 语义差异），含 store:true 链式引用与 1h TTL 的不做判据
- 2026-09-18 · [updater-download-403.md](./updater-download-403.md) — 缺陷修复：自动更新下载 403 Forbidden（tauri-action 产出的 latest.json 把下载地址指向 REST 资产 API 端点 `api.github.com/.../releases/assets/<id>`，匿名配额 60 次/小时/出口 IP 耗尽即 403）——发布收尾新增 `finalize-updater-json` 作业按 asset id→name 权威映射改写为 `releases/download/<tag>/<name>`（CDN 不计配额）+ 下载失败文案识别 403/429 并明确「切换代理无效」；含三类 403 区分方法、CDN 缓存边界与存量修补命令
- 2026-09-18 · [updater-ux-batch.md](./updater-ux-batch.md) — 自动更新体验批次（对齐 GitWave）：8 相位状态机（stores/updater）+ 弹窗（发布说明 / 进度条 / 重试 / deb-rpm 手动下载降级 / 待重启）+ 启动 3s 静默检查与设置页开关（`ws_auto_update`）+ 后端 `is_appimage` 命令；发布侧补 `releaseBody`（此前 latest.json 的 `notes` 恒为空，弹窗无说明可显）