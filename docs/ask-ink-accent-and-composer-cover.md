# ask 界面对齐批次：全局强调色墨化 + ask 覆盖 Composer + 提问卡降噪

> 缘起：与主流编码 Agent 提问界面的截图对比。参考实现的和谐来自极端克制——全屏唯一强调色（黑）、层级只靠字重+灰度、形状只有圆角一种词汇、密度紧凑；本项目的提问界面则是 antd 默认值的无意识叠加（6 处蓝色、4 种圆角、字号全挤 11–13px、计划卡 markdown 样式未生效）。用户决策：**全局强调色改中性墨色** + **ask 弹出时覆盖 Composer**（参考实现的提问面板即唯一底部输入区，元素少所以整洁）。

## 一、根因结论（对比分析）

| 维度 | 参考实现 | CodeWave（改前） |
|---|---|---|
| 强调色 | 黑，与文字同源，只给动作 | antd 默认蓝 #1677ff（App.tsx 从未设置 colorPrimary，**是默认值不是品牌决策**），出现在 Tag/两个 primary 按钮/Input focus 边框/ask-card focus 边框/Composer focus 边框/BorderBeam 共 6 类 |
| 元数据 vs 内容 | 「需要权限」描边中性 pill，安静 | 「Agent 提问」`Tag color="processing"` 蓝底填充，元数据比问题内容更抢眼（层级倒挂） |
| 排版层级 | 18/15/13/11 字号梯度 + 粗体标题 + 灰正文 + inline code chip | 全挤 11–13px；q-text 13px/400 无加粗；**计划卡 `.md` 样式挂在 `.assistant .md` 下而 AskPanel 无此前缀，样式整体未生效**（app.css 原 L109-126），渲染为素文本 |
| 形状 | pill 一以贯之 | Tag pill / 按钮 antd 6px / 选项条 8px / 卡 12px / Composer 14px 混用 |
| 输入区 | ask 面板即唯一底部输入，补充输入只是一行占位 | ask 卡「补充说明」Input 与下方 Composer「输入内容进行排队」+ 蓝色流光同屏双输入 |
| 分隔 | 两张独立卡 | 单卡内 1px **虚线**（噪音元素） |

## 二、改动清单

### A. 全局强调色墨化

- `ui/src/App.tsx`：ConfigProvider `theme.token.colorPrimary` = 亮色 `#1f1f1f` / 暗色 `#424242`（暗色取中灰，primary 按钮白字对比度 ~10:1）。
- `ui/src/theme/bridge.tsx`：`--ws-accent` 由 `token.colorPrimary` 改取 **`token.colorPrimaryText`**（算法自适应前景变体）——亮色 = 近黑主色本身（观感不变）；暗色 = 提亮灰。直接取 colorPrimary 在暗色下作文字/边框/流光（`#424242` on 深底）不可读；全仓 23 处 `--ws-accent` 消费点均为前景用法（文字/图标/边框/流光/柱状填充），无一在强调色底上叠白字，故统一取前景变体、零调用点改动。
- 波及面（预期内统一回归）：发送按钮、全部 primary 按钮、BorderBeam 流光、`.cursor` 闪烁光标、tool-card 工具名/running 圆点、chips hover、composer/ask focus 边框。
- **权限胶囊语义升级**（[docs/composer-shift-tab-mode-cycle](./composer-shift-tab-mode-cycle.md) §5 后续变更）：confirm_each 由「蓝」变「墨」——色彩强度映射风险等级：无彩色（墨）= 默认档 / 橙 = 自动编辑需注意 / 红 = 完全访问危险；着色规则与 class 未动（token 驱动自动生效）。
- 残留蓝审计：`Tag color="processing"` 全仓仅 ProvidersPanel「当前」标签一处 → 改描边墨色（inline style，`--ws-accent` 同源）；hljs 语法高亮色不动（内容非 chrome）。

### B. ask 弹出时覆盖 Composer（`ui/src/features/chat/Composer.tsx`）

- `askActive = !!active.ask`：活跃时只渲染 `<AskPanel />`，Composer 本体（流光/工具条/发送键）与队列面板不渲染；回答后原样恢复。
- 草稿 `text` 存于 Composer 组件 state，子树卸载不丢；`ws:focus-composer`/`ws:composer-fill` 在 el 为 null 时已有空值守卫，无需改动。
- `.ask-wrap` 下边距 8px→6px 与 composer 对齐，视觉上「输入区换了个面板」。

### C. 提问卡降噪（`ui/src/features/tools/AskPanel.tsx` + `ui/src/theme/app.css`）

- **标签**：去 `color={isApproval ? "warning" : "processing"}` preset → `.ask-card .ask-tag` 自定义描边 pill（透明底 + `--ws-border` + text-2，11px）；approval 形态 `.warn` 描边橙保语义。
- **分页器**：`Button type="text" size="small"` → 裸图标 `.pager-btn`（20px 命中区、11px 图标、`--ws-dim`，hover 加深，禁用 0.35 透明）；新增 i18n `ask.prevPage/nextPage`（中/英）。
- **计划卡分隔**：`1px dashed` → `1px solid var(--ws-border)`。
- **计划卡 markdown 修复**：`.assistant .md …` 共用规则改 `:is(.assistant .md, .plan-body.md) …`——inline code chip / 表格 / 代码框 / 标题在计划卡内首次生效；inline code 硬编码 `#efefef/#1f2328` → `var(--ws-code-bg)/var(--ws-text-1)`（暗色可读）。`.plan-body` 12.5px → 13px + line-height 1.7。
- **问题文本层级**：`.q-text` 13px/400 → **14px/500 + text-1**（卡片视觉入口）。
- **补充输入 ghost 化**：Input `variant="borderless"` + `.ask-note`（透明底线，focus 浮现 `--ws-accent` 底线；对话式输入的极简质感）。
- **按钮形状统一**：`.ask-card .ant-btn { border-radius: 999px }`（局部作用域）——「查看完整计划」「提交回答」成墨色胶囊，极简胶囊语言。
- 密度微调：`.ask-options`/`.ask-foot` margin 收紧 1-2px。

## 三、测试与验证

- `pnpm --dir ui test`：**162/162 全绿**（基线 110 → 现批次全量）。更新点：
  - `askpanel.test.tsx`：分页器选择器 `.ask-pager button` → `.ask-pager .pager-btn`；
  - `app.smoke.test.tsx`：ask 用户新增「ask 活跃时 `.composer-card` 不渲染 + ask 清空后恢复」断言（恢复走显式置空 `ask=null`——真实链路由后端事件清除，mock 不发）；`afterEach` 补 `useRun.tabs = {}` 清残留（ask 覆盖 Composer 后，运行态残留会让后续用例找不到输入框，首次跑出 4 连败即此因）。
- `pnpm --dir ui build`：type check + vite build 通过。

## 四、手动验证清单（GUI 不做自动点验）

1. 亮色模式：发送键 / 「提交回答」/「查看完整计划」/ 保存按钮均为墨色胶囊或墨色按钮；BorderBeam 流光为墨色；无残留默认蓝。
2. 暗色模式（系统切换）：primary 按钮为中灰底白字，流光/焦点边框中灰，无黑底黑字。
3. 多题 ask：标签中性描边 pill、分页小灰箭头、题干 14px 中字重、补充输入平时仅占位行/聚焦出底线、选项条点选正常。
4. approval 形态：标签描边橙、命令块、三选项、「确认」墨色胶囊；ConfirmEach 档 ⚠ 升档提示仍为橙色醒目。
5. ask 打开时 Composer 与队列面板消失、底部只有提问卡；回答/忽略后 Composer 原样恢复，草稿仍在；队列「编辑」回填在恢复后可用。
6. 计划卡：markdown 标题/表格/`inline code` 灰 chip 生效（暗色下 chip 仍可读）；分隔为实线；「复制计划全文」正常。
7. 权限胶囊：确认=墨、自动编辑=橙、完全访问=红、计划=不着色；Shift+Tab 循环正常。
8. 设置 → 供应商：模型行「当前」标签为墨色描边。

## 五、非目标与后续

- 不合并「补充说明」与队列排队语义（仅不同时可见）。
- hljs 语法高亮保持彩色（代码内容着色，非 chrome）；antd `colorInfo` 系（若有）未逐一墨化，实测观感如仍有蓝点再行收敛。
- `settings.accent`（UiPrefs 字段，从未接线）保持现状；若未来做「强调色可配」，墨色应作为默认值。

## 六、追加调整：流光时机收敛

用户追加需求：流光（BorderBeam）**仅在两个时机出现**——① 输入框聚焦；② 任务进行中；同时取消原 focus 时的强调色边框（流光即聚焦反馈）；`duration` 不变（空闲 12s / 运行中 3s）。

- `Composer.tsx`：新增 `composerFocused` 聚焦态（TextArea `onFocus/onBlur`）；`beamActive = active.running || composerFocused`；`<BorderBeam>` 常驻挂载，非活跃时传 `className="composer-beam-idle"`。
- `app.css`：删除 `.composer-card:focus-within { border-color: var(--ws-accent) }` 与随之失效的 `transition`；新增 `.composer-card .ant-border-beam.composer-beam-idle { display: none }`。
- 实现取舍：antd BorderBeam 把流光层 portal 进卡片 DOM 且 effect div 接受外部 className，故选「常驻挂载 + `display:none` 显隐」而非条件卸载——动画完全停摆零绘制、无重挂载/重测量开销、`duration` 切换（运行中 3s）在隐藏期间已就位，出现即时生效。
- 测试：`app.smoke.test.tsx` 流光用例扩展——空闲 3 条全带 `composer-beam-idle`、`fireEvent.focus` 后为 0、失焦复归 3（162/162 全绿，build 通过）。

### 手动验证补充

1. 点进输入框：灰色边框不变，墨色流光出现（空闲速度）；点外部/失焦后流光消失
2. 发送任务运行中：流光出现且加速（3s），不依赖聚焦
3. 输入框聚焦时流光与聚焦同时出现，无强调色边框残留
