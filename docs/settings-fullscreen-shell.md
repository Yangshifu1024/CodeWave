# 设置全屏容器化（设置页批①：覆盖式全屏页）

> 批次：设置全屏容器化 · 批①（仅换容器与离开拦截；页内容归属、8 页重划、注册表、搜索、折叠、命名统一、关于页并入都是后续批次）。
> 本文只讲本批真正落地的东西与其中的坑。

## 拓扑：覆盖式全屏页

设置从 `Modal width={880}` 改为**绝对定位覆盖层**，贴在 AppShell 内层 `Layout`（`position: relative`）内：

```
Layout (100%)
├── Header.toolbar          ← 顶栏（自绘标题栏）：拖拽层 + 窗口控制照常可用（**刻意不在覆盖层内**，见下）
└── Layout (calc(100% - titlebar), position: relative)
    ├── Sider        .workspace-covered   ← 左栏（挂载保留，只隐可见性）
    ├── Content      .workspace-covered   ← 中栏 ChatMessages/Composer + RightBar
    ├── ResizeHandle ×2 .workspace-covered ← 栏宽分隔条（同时 tabIndex=-1：z-index 只挡指针，挡不住键盘焦点）
    └── SettingsPage (.settings-shell) ← 打开时挂载：z-index 20 的覆盖层（role=dialog + aria-modal）
```

`.settings-shell` 两列：左侧 `.settings-nav`（返回工作区 + 运行中指示 + 7 页导航，宽 280，窄窗按
`utils/layout.clampNavWidth` 在 180..480 内收缩）、右侧 `.settings-content`（`.settings-actions` 操作条 +
`.settings-pane` 页体，页体保底 480px 并横向滚动；操作条恒在列内，窄窗换行，保存/取消始终可点）。
页体分两层：
- 外层 `.settings-pane-body`：只承担「保底 + 内边距」职责（`min-width: 480px`、`padding: 14px 16px 24px`，
  横滚兜底），不参与居中；
- 内层 `.settings-pane-inner`：仅承担**居中职责**（`width: 50%`、`margin: 0 auto`、
  `box-sizing: border-box`，左右各 25% 留白），不携带视觉外壳；section 级卡片样式独立到
  `.settings-section-card` 工具类（`background: var(--ws-bg-nav)` + `border-radius: 12px` +
  `padding: 14px 18px`），page renderer 按需对分组显式加 className——参考图里每组相关设置项共用
  一张圆角浅灰卡片，section 标题与单卡片组（如主题选择）留在卡片外；MCP 页通过
  `.settings-pane-body-mcp .settings-pane-inner { max-width: 960px }` 单独挂状态表可读性上限，
  与普通页保持同构（同一居中容器类）。

导航仍由 antd `Tabs tabPlacement="start"` 提供（自建导航是批②）。Tabs 只消费 `key/label`，页体在右列按
当前页渲染——Tabs 自身无法把 nav 与 pane 拆到两列。副作用：**切页会卸载上一页页体**（页内局部状态如供应商
编辑表单不再保留），与 antd 默认保留非活跃 pane 不同。

## 覆盖范围与有意取舍

- **顶栏（`Header`，`AppShell.tsx:413-423`）不在覆盖层内**：两侧栏开合、任务/统计/关于等按钮在设置页打开时仍可点。
  这是有意取舍：顶栏即标题栏，窗口拖动/最大化/关闭必须全程可用（覆盖它就得在覆盖层里重做一套窗口控制）。
- `.settings-nav .ant-tabs-content-holder { display: none }` 只作用于设置页导航列里那个**空的** content-holder
  （Tabs 只消费 key/label，页体渲染在右列），与「工作区禁用 `display:none`」的硬约束无关：
  工作区内没有任何 `.ant-tabs-content-holder`。CSS 中已加同义注释。
- `discardDraft()`（「取消」/「放弃改动」）**不**回滚即时生效项（界面语言、自动更新开关）：它们改完就已落盘
  （`useUi` + localStorage），回滚反而与用户刚看到的界面语言不符。
- 栏宽分隔条进 `.workspace-covered` 与 `tabIndex=-1`：`.settings-shell` 的 `z-index:20` 只挡指针，
  挡不住键盘——不然 Tab 序会停在分隔条上、←/→ 还能静默改栏宽（审查返工修复）。

## 硬约束：不许影响正在运行的会话（禁止 `display:none`）

- 工作区**全程挂载**，打开设置只加 `.workspace-covered { visibility: hidden; pointer-events: none; }` +
  `aria-hidden`。
- **绝不用 `display: none`**：它会让 ResizeObserver 测到 0 尺寸、滚动容器几何全丢，回到工作区时滚动位置
  与流式渲染重排，直接干扰运行中任务（本批的头号约束，有 `settings.page.test.tsx` 的样式契约用例守着）。
- `visibility: hidden` 不产生布局变化，因此打开/关闭设置不会触发任何 resize 连锁。

## Esc 语义（本批最容易踩的坑）

设置页不是 antd 容器，而全局 Esc（AppShell）原本只放行 `.ant-modal/.ant-drawer/.ant-popover/.ant-select-dropdown/.ant-input/textarea/input`
内的按键——全屏页里的 Esc 会被误判成「停止运行中会话」。三层收口：

1. AppShell 的 Esc 分支：`useUi.settingsOpen` 为真时**直接返回**（事件目标可能是 `body`，光靠 `closest`
   白名单会漏）；白名单同时补 `.settings-shell`。
2. SettingsPage 在**捕获阶段**注册 window keydown 并 `preventDefault()`，抢在 AppShell 的冒泡监听之前收口
   （AppShell 首行 `e.defaultPrevented` 即放行）。
3. 行为链：三选弹框开着 → 归弹框（antd Modal 的 Esc = 留在原地）；Select/Dropdown/Popconfirm 浮层开着 →
   让浮层先关；否则走「返回工作区」路径（含未保存拦截）。**任何情况下都不停止运行中会话**。

## 保存与未保存拦截

- 「保存」沿用 `save()` 全量提交语义；**逐页脏标记**由 `draft` 与已保存配置的逐页差集推导（`pageSlice`），
  MCP 例外（不在 config 内，比 `mcp.json` 文本与打开时基线的差）；脏页导航项与操作条各点一枚
  `.settings-dirty-dot`。
- **即时生效项不打点**：界面语言（写 `useUi.setLanguage`）与自动更新开关（写 localStorage）在**项标题的括号里**
  标注「即时生效」（如「更新（即时生效）」；早期是行内独立标注，会被行内布局推到最右侧/控件之外看不见）。
  语言若纳入差集会永远显示未保存。
- 顶部「取消」= 放弃全部未保存改动并返回工作区（不再二次确认，`Tooltip` 说明）。
- 三选拦截 `保存并离开 / 放弃改动 / 留在原地`，**四条路径共用同一份文案与行为**：
  ① 切页（`onTabChange`）② 返回工作区/运行中指示/页内 Esc（`requestClose`）③ 关窗退出。
  关窗退出复用既有 `app:exit_requested` 链：设置侧先接管（`ui.settingsDirty` 为真时 ExitConfirm 让位），
  「留在原地」= `respondExitRequest("cancel")`（**必须回后端应答**，只清 store 会让应用永远退不出去）；
  保存/放弃后脏标记清零，运行中会话确认（ExitConfirm）自然接管。
- 「保存并离开」在供应商/代理校验不通过时不会离开（`save()` 返回布尔，错误提示由它自己给）。

## 契约变更：`showSettings` 深链语义

```ts
showSettings(tab?) // 带参 = 跳到该页；无参 = 保持当前页不重置（旧实现一律回 general）
```

这是**有意变更**：设置从关掉即销毁的弹窗改为常驻全屏页后，「无参重开」若回第一页会把用户甩回起点。
带参深链语义不变——实有 3 处：Composer 未配模型时发送、Composer 模型菜单「管理供应商」、
认证错误卡「打开模型设置」，**均指向「供应商」页**（`Composer.tsx:280/402`、`ChatMessages.tsx:424`）。
（此前文档写的「LSP 引导卡 → 所在页」深链并不存在：`features/chat/LspGuideCard.tsx` 里没有 `showSettings`/`settingsTab`。）
守护用例：`stores.ui.test.ts`（旧断言改为守护新语义）。

## 测试迁移清单

| 文件 | 改了什么 |
|---|---|
| `__tests__/settings.page.test.tsx` | **新增 20 例**（批① 11 例 + 审查返工 9 例）：工作区仍挂载 + `.workspace-covered` 精确集合（含两个分隔条）、运行态/消息流不变、返回与页内 Esc 不停运行、运行中指示（含两会话计数）、三选四条路径、按页打点与保存清点、即时生效不打点、脏标记空值归一（自定义提示词 / proxy=null / 空 SDK 根行）、深链、dialog 可达性、样式契约（visibility 而非 display:none） |
| `__tests__/shell.settings.test.tsx` | 挂载组件改 `SettingsPage`；`.ant-modal .ant-select` → `.settings-shell .ant-select` |
| `__tests__/settings.lsp.test.tsx`、`settings.network.test.tsx`、`settings.skills.test.tsx` | 挂载组件改 `SettingsPage` |
| `__tests__/theme.test.tsx` | 外观页 Select 选择器锚 `.settings-shell` |
| `__tests__/app.smoke.test.tsx` | 设置用例：页签「6 项」→ 7 项（补「网络」）、`.ant-modal` 作用域 → `[data-testid="settings-page"]`、技能行范围限定到设置页 |
| `__tests__/stores.ui.test.ts` | `showSettings` 守护用例改为守护新语义（带参跳页 / 无参不重置） |
| `__tests__/settings.style.test.ts` | 不变（`.mcp-entry-row` 纵向契约仍成立） |

## 人工验证清单（界面改动不做 GUI 自动点验）

1. 左下角状态区点「设置」：整屏切换为设置页，标题栏（拖拽、最大化、关闭）与左侧「返回工作区」都可点；
   窗口拖动/最大化正常。
2. 开一个会跑一会儿的会话（或长任务），运行中点设置：会话不中断；导航顶部出现脉冲点 +「1 个会话运行中」；
   点它回到工作区，输出仍在继续、滚动位置与之前一致。
3. 在工作区把会话列表滚到中间 → 打开设置 → 返回：滚动位置不跳（`display:none` 会跳）。
4. 改「自定义提示词」→ 导航「通用」与操作条出现脏点；点「外观」→ 弹三选；「留在原地」停在本页且改动保留；
   「放弃改动」跳页且脏点消失；「保存并离开」跳页并提示已保存。
5. 改点东西后按 Esc：先关打开的 Select 下拉；无浮层时弹三选（不返回、不停运行）。
6. 改点东西后关窗口：先弹设置三选；「留在原地」= 取消退出（应用继续跑）；「放弃改动」后弹原有的运行中会话确认。
7. 切换界面语言：立即生效、**不出现**脏点；顶部「取消」= 直接放弃并返回工作区（无二次确认）。
8. 窄窗（缩到 ~700px）：导航列收缩到 180 不消失，页体横向滚动，保存/取消仍可点。
9. 深链：制造一个认证错误后点「打开模型设置」→ 直接落在「供应商」页；再从菜单打开设置 → 仍停在「供应商」。

## 审查返工记录（批① 复审后）

- 脏标记空值归一：`normalizeForCompare` 把 `null` / `undefined` / `""` / 纯空白串折成同一形态（并 trim），
  杜绝「自定义提示词删空后脏点常亮、返回时误弹三选、`custom_prompt: ""` 被落盘」；
  `custom_prompt` 输入侧同步改回 `null`；网络页把 `proxy: null` 视作 `ProxyConfig::default()`（mode=system）。
- 分隔条纳入覆盖：`ResizeHandle` 新增 `covered` prop（`.workspace-covered` + `tabIndex=-1` + `aria-hidden`）。
- 三选「留在原地」在「切页/返回意图 + 关窗请求」并存时同时清意图并 `respondExitRequest("cancel")`。
- `overlayOpen()` 补 `.ant-drawer:not(.ant-drawer-hidden)`（子代理抽屉与设置页 Esc 双响应）。
- 设置页根节点 `role="dialog"` + `aria-modal="true"`，打开时焦点移到导航首项「返回工作区」（仍不引入焦点陷阱库）。
