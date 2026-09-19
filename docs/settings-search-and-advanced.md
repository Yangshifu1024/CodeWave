# 设置页 · 批③：搜索、进阶折叠与 3 档控件宽度

> 状态：已实施（批③）。批① 把设置从弹窗改成常驻全屏页（[settings-fullscreen-shell](./settings-fullscreen-shell.md)）；
> 批② 把 7 页重划为 8 页 + 3 组导航，并把「哪一项住在哪一页」抽成设置项注册表（[settings-ia](./settings-ia.md)）。
> 本批在注册表之上补三件事：**找得到**（搜索）、**收得住**（进阶折叠）、**排得齐**（控件宽度三档）。
> 术语统一与键重命名仍留批④。

本批**不做**（边界）：动态条目搜索（供应商 / 模型、MCP 服务器、技能条目）；`Cmd/Ctrl+,`、`Cmd/Ctrl+F`；
后端与配置 schema；容器级宽度与其他面板宽度；焦点陷阱；搜索增强（拼音 / 首字母 / 模糊 / 权重 / 词内 mark / 历史 / 最近访问）；
页签与分组结构调整；批①/批② 的能力（工作区保留挂载、Esc 白名单链、三选拦截、运行中指示、深链语义、8 页 3 组、ARIA 闭环与导航方向键）一律不回退。

## 1. 搜索

### 1.1 位置与整行约束

- 位置：左导航顶部、「返回工作区」**下方**，**整行**。
  「返回工作区」与运行中指示在 `.settings-nav-head`（`display:flex; flex-wrap:wrap`）里同行；
  搜索框容器 `.settings-search` 用 `flex: 1 0 100%` 强制换行独占一行——**不与运行中指示挤在同一行**。
- 初始焦点契约（批①）保持：打开设置后焦点仍在「返回工作区」。
  批③ 起焦点用**专用类名 `.settings-nav-back`** 定位，不再泛选 `.settings-nav-head button`。
  「泛选会被 allowClear 的清除按钮抢走焦点」是**防御性**写法：搜索框挂载时值恒为空、清除按钮并不存在，
  该保护当前**不可构造验证**（已实测：改回泛选选择器全部用例仍绿），保留以防未来实现变化
  （如打开设置时恢复上次查询）。

### 1.2 匹配规则（纯函数 `matchSettings(query, t)`，注册表内）

命中范围 = **该项 i18n 显示名 + `keywords`（直字符串、中英混排、不进 i18n）+ 所属页名 + 所属组名**，四者拼成一个 haystack。

| 规则 | 行为 |
|---|---|
| 归一 | query 与 haystack 一律 `toLowerCase()`；query 先 `trim()`（haystack 归一直小写由 `en` 大写缩写用例守护，删掉即红） |
| 分词 | 按空白切分为多词 |
| 逻辑 | **多词 AND**：每个词都要命中同一项的 haystack（词序无关） |
| 空值 | 空串 / 仅空白（含 `\t\n`）返回 `[]`——调用方据此回到常规导航 |
| 排序 | 页序（`PAGE_ORDER`）→ 组序（注册表内组首次出现顺序）→ 注册表原序；`sort` 比较器全序，结果稳定 |
| 不做 | 拼音 / 首字母 / 模糊 / 权重 / 词内高亮（非目标） |

`keywords` 是「用户会用什么词找它」的清单，中英混排（如 `ui.theme` 的 `["theme","dark","light","主题","暗色","亮色"]`），
**不得写成 `settings.*` 键**（契约测试拦）。

### 1.3 搜索态与导航态互斥（本批重点之一）

**搜索态 = query trim 后非空。** 搜索态下**不渲染**左导航 tablist（`.settings-nav-list[role=tablist]` 整块不在 DOM 里），
结果列表用**独立类名**占位：

| 元素 | 类名 / 语义 |
|---|---|
| 搜索框 | `role="combobox"` + `aria-label={settings.searchPlaceholder}` + `aria-expanded={searching}` + `aria-controls` 指向结果列表 + **`aria-activedescendant` 挂在这里** |
| 结果容器 | `#settings-search-listbox`、`.settings-search-results`，`role="listbox"` + `aria-label={settings.searchResults}` + `tabIndex={-1}`（无焦点：方向键与 Enter 都由搜索框接管） |
| 结果行 | `.settings-search-item`（高亮项加 `.settings-search-item-active`），`role="option"` + `aria-selected` + `id=settings-search-opt-<id>`（`aria-activedescendant` 的指向） |
| 行内容 | 显示名 `.settings-search-item-label` + 所属页名 `.settings-search-item-page`（弱化次标，11px dim） |
| 空态 | `.settings-search-empty`：`settings.searchEmpty` + `settings.searchEmptyHint`（引导「供应商 / 模型、MCP 服务器、技能等条目请在各自页面内查找」） |
| 行属性 | `data-search-hit="<item.id>"`（**刻意不复用 `data-setting-id`**：后者是页内锚点，两者语义不能混） |

两套列表因此**永不同时存在**，方向键不可能串味：`onNavArrow` 挂在 tablist 上（不渲染即无绑定），
结果列表的 ↑/↓ 走搜索框自己的 `onKeyDown`。

### 1.4 键盘

| 键 | 行为 |
|---|---|
| `↑` / `↓` | 只移动**结果列表**高亮项；无默认选中，**-1（未选中）时两个方向都落到第 0 项**（↑ 从 -1 被钳到 0，是刻意行为而非漏改）；**两端停住不循环**；结果为空时不拦 |
| `Enter` | 有高亮项才跳转（**无默认选中**：未显式选中时不动作）；跳转后**焦点留在搜索框**（不主动移焦） |
| `Esc` | 第一次只**清空查询**（高亮下标一并复位）；清空后再按才回落既有 Esc 链 |
| IME | `isComposing` 组合中不拦 `Esc` / `Enter` / ↑↓ |

Esc 的完整优先级（window 捕获监听，见 `SettingsPage` 的 `escRef`/`queryRef`）：
**三选弹框 / 浮层（Select/Dropdown/Popover/Drawer）→ 清空搜索查询 → 既有「返回工作区」链**。
查询值经 `queryRef` 镜像读取（监听不随 query 重挂）。

### 1.5 命中跳转与临时高亮

- 命中跳转走**与点击导航同一条** `onTabChange`：脏改动存在时同样先走三选拦截；选「留在原地」则不跳，
  并丢弃本次定位（否则以后手动切到该页会莫名高亮）。
- 定位的丢弃与保留（三条离开路径，`settings.page.test.tsx` 有交互用例）：
  | 离开路径 | 定位 |
  |---|---|
  | 留在原地 | **丢弃**（不跳页，留着就会“以后莫名滚动 + 高亮”） |
  | 保存并离开（落盘成功） | **保留**（跳页后照常定位高亮） |
  | 保存被拦（代理地址非法 / 供应商校验失败：`save()` 报错跳页、不落盘） | **丢弃**（两条校验失败分支内 `setPendingHit(null)`） |
  | 放弃改动 | **保留**（与「保存并离开」同形：跳转照常执行，故目标页仍要约定位） |
- **跨页时序用两段式 effect**（目标节点在切页那一帧可能还不存在）：
  1. 段一（deps `[pendingHit, tab, advancedVisible]`）——目标页成为当前页后，若命中项是**被折叠的进阶项**
     先临时展开该页进阶行，再把命中项交给段二；
  2. 段二（deps `[hitTarget]`）——节点已渲染，`scrollIntoView({block:"center"})` + 加临时高亮类
      `.settings-item-hit`，**约 1.5s 后（`HIT_HIGHLIGHT_MS`）摘掉**（同一项连点两次靠 nonce 重新触发）。
     **换项时先摘旧高亮**（`hitElRef`）：1.5s 内连续命中不同项时旧定时器已被 `clearTimeout`，
     不显式摘掉就会在旧元素上永驻（卸载同样补一道摘除）。
- 命中项在**当前页**时：只滚动 + 高亮，**不切页、不改导航选中态**。
- 命中项落在**已收起的进阶区**时：**临时展开该页进阶行**（`forcedAdvanced`，**不写 localStorage**）；
  离开该页即回手动偏好值。开关本身仍反映手动值（临时展开不动开关状态）。
- 临时高亮是**命令式**加的类（不属于渲染态，1.5s 后自动消失，故不往渲染态里塞第二个状态位）。

### 1.6 锚点约定：`data-setting-id`

- 每个设置项的控件/行都带 `data-setting-id="<注册表 id>"`，包裹层统一用类名 `.setting-anchor`（`min-width:0`）。
  优先挂**已有容器**（`.validation-row`、`.lsp-roots`、`.mcp-pane`、`.settings-update-row` 内的包裹 div、
  供应商三视图根节点），没有天然容器时在控件外包一层 `div.setting-anchor`。
- 命中定位读 `[data-setting-id="<id>"]`；**锚点缺失或命中节点为 0 高度锚点时退化为高亮页体容器**（`.settings-pane-body`）。
  0 高度锚点判定 = `offsetHeight === 0` **与** `getClientRects().length === 0`（两个条件都成立才算没布局盒：无布局引擎的测试环境里 offsetHeight 恒为 0，单看它会把所有节点都判成退化）。
- 当前**两个退化项**（表现均为「切页 + 高亮页体容器」）：
  1. `active_model_id`——锚点本身不存在：「当前」标记只出现在「编辑供应商」视图的模型列表里（列表视图无此节点）；
  2. `approval.command_allowlist`——锚点**常驻但常为 0 高度**：其内 `<Form.Item>` 只在白名单非空时渲染，而白名单默认为空
     （不退化的话：`scrollIntoView` 无效、高亮退化成 1px 线，用户观感是「搜了没反应」）。
  契约测试 `锚点覆盖` 用例逐页核对全部 40 项，**锚点存在性**的例外清单仍只允许 `active_model_id` 这一项（0 高度不属于“缺锚点”）。
- 排错提示：**动态条目**（供应商 / 模型、MCP 服务器、技能条目）没有锚点也不该有——搜索只覆盖注册表里的设置项。

## 2. 进阶折叠

### 2.1 形态：页级开关 + 行内过滤（零 DOM 搬迁）

- 每页页体**顶部一行**开关（类名 `.settings-advanced-toggle`，不动 Form.Item 就在 pane body 顶部）：
  文案 `settings.showAdvanced`（带该页计数 `{{n}}`）+ 说明 `settings.advancedHint`；**该页进阶项数为 0 时该行不渲染**
  （界面 / 网络与连接 / 关于三页没有进阶项，故看不到开关）。
- 进阶行**留在原分组内**（不是页底统一「高级区」）：收起时只加类 `.settings-advanced-hidden`（`display:none`），
  **DOM 位置不动**；**整组皆为进阶项**的组（当前仅 LSP 预算组）整组隐藏（组容器 `data-setting-group-id`）。
- 判定来源：`ADVANCED_ITEM_IDS`（注册表 `advanced: true` 派生）+ `isAdvancedOnlyGroup(page, group)`。
- 开关文案的计数来自 `advancedCountByPage(page)`。

### 2.2 记忆策略

| 项 | 值 |
|---|---|
| 载体 | localStorage，键 `ws_settings_show_advanced`（常量 `SETTINGS_ADVANCED_PREF_KEY`，契约测试钉死） |
| 语义 | 全局**单一**偏好：`"1"` = 展开、`"0"` / 缺省 = 收起（**默认收起**） |
| 范围 | 跨页、跨次打开均记忆（不在页级或会话级分桶） |
| 不可写时 | 隐私模式 / 配额受限 → `try/catch` 静默，本次会话内仍生效 |
| 与脏标记 | 切换折叠**不碰 draft** → **不产生未保存改动**；进阶项自身的改动仍由 `PAGE_FIELDS` 判定（语义不变） |

### 2.3 进阶项清单（11 项，本批范围不变）

| 页 | 项 | 说明 |
|---|---|---|
| 模型与供应商 | `active_model_id` | 活跃模型「当前」标记（模型列表内） |
| 安全与审批 | `approval.command_allowlist` | 命令白名单（整行列表） |
| 工具与集成 | `validation.lsp.sync_window_ms` / `max_diagnostics` / `max_chars` / `idle_ttl_ms` / `max_servers` / `max_file_bytes` / `dedupe_limit` | LSP 全局预算 7 项（**整组皆为进阶项** → 整组隐藏） |
| 工具与集成 | `validation.lsp.java_home` | JDK 21+ 路径（发现组内单项） |
| 日志 | `log.session_verbose` | 会话详细日志 |

逐页计数：`tools` 8 / `security` 1 / `providers` 1 / `logs` 1，合计 11（契约测试断言）。

## 3. 控件宽度三档

### 3.1 类定义（`ui/src/theme/app.css`）

| 档 | 类 | 宽度 |
|---|---|---|
| 窄 | `.w-narrow` | `180px` + `max-width:100%` |
| 中 | `.w-mid` | `240px` + `max-width:100%` |
| 宽 | `.w-wide` | `360px` + `max-width:100%` |

- 三档**都带 `max-width:100%`**：窄窗下宽档降为 100%，**不横向溢出**。
- 三档各补一组复合选择器（`.w-narrow.ant-input` / `.ant-input-number` / `.ant-select` / `.ant-input-affix-wrapper`）抬高权重：
  antd 自带宽度规则（`.ant-input { width:100% }`、`.ant-input-number { width:90px }`）与本表单类规则同权重，
  后注入的 antd 样式可能胜出，复合选择器保证在任何注入顺序下都生效。
- **页体内不得再写像素内联 `width`**（正则断言：`\bwidth:\s*\d` 在 `SettingsPage.tsx` / `ProvidersPanel.tsx` /
  `FontSettings.tsx` 里必须为 0 命中）。`style={{ maxWidth: 420/560/640 }}` 这类**容器**宽度是本批明确非目标，不动。

### 3.2 每项 → 档位映射（实测口径：18 处像素内联 `width` / 7 个不同数值 / 12 组控件）

| 原内联值 | 出现处（控件与所在页） | 归属设置项 | 现档位 |
|---|---|---|---|
| `240` | 模型与供应商页：AI 语言 `Input` | `ui.ai_language` | `w-mid` |
| `360` | 网络与连接页：代理地址 `Input` | `network.proxy` | `w-wide` |
| `180` ×7 | 工具与集成页预算组：7 个 `InputNumber` | `validation.lsp.*`（预算） | `w-narrow` |
| `360` | 工具与集成页发现组：JDK 21+ 路径 `Input` | `validation.lsp.java_home` | `w-wide` |
| `160` | 工具与集成页 MCP：服务器名 `Input` | `mcp.servers` | `w-narrow` |
| `170` | 工具与集成页 MCP：传输方式 `Select` | `mcp.servers` | `w-narrow` |
| `260` | 工作区与智能体页：Shell `Select` | `shell.selection` | `w-mid`（保留 `flexShrink:0`） |
| `320` | 工作区与智能体页：自动压缩阈值 `Slider` | `compact_threshold` | **去掉内联宽度**（Slider 整行，宽度随容器，不设档） |
| `160` | 日志页：日志级别 `Select` | `log.level` | `w-narrow` |
| `180` | 模型弹框：「推理强度默认档」`Select`（ProvidersPanel） | `providers`（复合容器内散装控件） | `w-narrow` |
| `160` | 界面页：主题 `Select`（FontSettings） | `ui.theme` | `w-narrow` |
| `160` | 界面页：界面语言 `Select`（FontSettings） | `ui.language` | `w-narrow` |
| *无内联*（antd 默认 `90px`） | 工作区与智能体页：压缩请求超时 `InputNumber` | `compact_timeout_seconds` | `w-narrow`（180；与同类超时/阈值控件就近取档，视觉更整） |

> 口径：上表 12 组控件对应**实测**的 18 处像素内联 `width`（§3.1 的正则断言现已全绿）；
> 末行 `compact_timeout_seconds` 原本没有内联宽度（antd `InputNumber` 默认 90px），本批按“就近取档”归入 `narrow`
> ——补记在此以免与“本批替换的内联值”混淆。

> 另注：`220` 出现在 `TaskCenterPanel.tsx`（任务面板容器），**不在设置页体内**，本批不动。

### 3.3 未进注册表的动态行

MCP 行内控件（名称 / 传输方式）与 LSP 额外 SDK 根目录行（`lsp.roots` 内整行 `Input`）**不在注册表里**，
按同一套类名手工标注：前者用 `w-narrow`（属 `mcp.servers` 的一部分），后者是整行输入**不设档**（宽度由 `.lsp-root-row` 的 `flex` 决定）。

### 3.4 登记规则（收口）

注册表对每项的要求是二选一，**不得两者皆无**（契约测试双向断言）：

- **标 `width`**：`"narrow" | "mid" | "wide"` 之一，同时页体/子组件里挂对应类名；
- **或进 `WIDTH_EXEMPT_ITEM_IDS`**：逐条带注释说明「为什么它没有宽度档」。
  `WIDTH_EXEMPT_ITEM_IDS` 与「已标注 `width` 的项」**不得重叠**（重叠 = 清单在掩盖过时登记），
  两者数量相加恰好等于 `SETTINGS_ITEMS.length`（40）。

当前豁免分类（24 项）：整行 / 多行文本（`ui.font_sans`、`ui.font_mono`、`custom_prompt`、`validation.lsp.extra_roots`）、
整行开关（7 个）、`Slider`（`compact_threshold`）、行内网格控件（六语言 + JSON 共 7 项，宽度由 `.validation-row` 网格列决定）、
整行列表与复合容器 / 动作项（`approval.command_allowlist`、`disabled_skills`、`providers`、`active_model_id`、`app.check_updates`）。

## 4. 新增一项设置：登记清单（批③ 口径）

在 [settings-ia](./settings-ia.md) §3 的流程（`SETTINGS_ITEMS` → `PAGE_FIELDS` → 双侧 i18n → 跑测试）之上，本批追加三项：

1. **`width`**：可调宽度的 `Input` / `Select` / `InputNumber` 标三档之一并在页体挂类名；
   整行控件（TextArea / Switch / Radio / Slider / 网格内控件 / 复合容器 / 动作按钮）**加进 `WIDTH_EXEMPT_ITEM_IDS` 并写清理由**。
   判定顺序：先看该控件是否天然独占整行 → 是则豁免；否则按现有近似宽度就近取档（≈180 窄 / ≈240 中 / ≈360 宽）。
2. **`keywords`**：给「用户会用什么词找它」的词（中英混排、直字符串、**不进 i18n**）。
   英文关键词尽量含产品里出现的术语（如所有 LSP 项都带 `lsp`）；不要硬凑同义词（不做模糊匹配）。
3. **`advanced`**：只有「改了会出事 / 只有排障或深度定制才碰」的项才标 `advanced: true`；
   标了就必须在该页页体提供锚点 `data-setting-id`，并确认收起时**只加类**不搬 DOM；
   若整组都是进阶项，组容器加 `data-setting-group-id` 以便整组隐藏。
4. **锚点**：新项的控件/行必须带 `data-setting-id="<item.id>"`（`.setting-anchor` 包裹层），
   否则搜索命中会退化为「高亮页体容器」（契约测试的锚点覆盖用例会先红）。

## 5. 测试与文件

| 文件 | 内容 |
|---|---|
| `settingsRegistry.ts` | `width` 字段 + `WIDTH_CLASS` / `WIDTH_TIERS` / `WIDTH_EXEMPT_ITEM_IDS`、`SETTINGS_ADVANCED_PREF_KEY` / `ADVANCED_ITEM_IDS` / `advancedCountByPage` / `isAdvancedOnlyGroup`、`matchSettings` + 稳定排序比较器 |
| `SettingsPage.tsx` | 搜索状态与 UI（`.settings-search` / `.settings-search-results` / `.settings-search-item`）、键盘与 Esc 优先级、两段式命中定位与临时高亮、页级进阶开关、全部锚点与宽度类 |
| `FontSettings.tsx` / `AboutSettings.tsx` / `ProvidersPanel.tsx` | 界面页 / 关于页锚点、供应商三视图锚点与 `active_model_id` 折叠、`providers` 内散装控件归档 |
| `theme/app.css` | 三档宽度类、搜索框与结果列表、进阶开关行、`.settings-advanced-hidden`、`.setting-anchor`、`.settings-item-hit` |
| `i18n/zh-CN.ts` + `en-US.ts` | `settings.searchPlaceholder` / `searchResults` / `searchEmpty` / `searchEmptyHint` / `showAdvanced` / `advancedHint`（双侧同步）；`SHELL_SETTING_KEYS` 同步豁免登记 |
| `__tests__/settings.page.test.tsx` | 新增 19 例：搜索框整行与焦点（防御性表述）、搜索框 ARIA（combobox/aria-activedescendant）、搜索态替掉 tablist、↑↓/Enter 语义与焦点、跨页命中与临时高亮、命中当前页、同页连中两项只留一处高亮、高亮自动摘除、0 高度锚点退化、空态、Esc 两级、进阶折叠与记忆、折叠整行（含 label）隐藏、临时展开、进阶项脏点往返、命中跳转 × 三选拦截三路径、锚点覆盖 40 项 |
| `__tests__/settings.registry.test.ts` | 新增 18 例：宽度档与豁免双向闭合、CSS 三档与 `max-width:100%`、页体无像素内联宽度、`matchSettings` 归一（含 haystack 大小写归一的 en 大写用例）/ 多词 AND / 空值 / 稳定排序、进阶项计数与整组判定、偏好键钉死 |

前端全量：`pnpm --dir ui test` 全绿（67 文件 / 627 例），`pnpm --dir ui build`（`tsc --noEmit` + vite build）通过。

## 6. 批③ 审查返工（10 项）

| 项 | 结论 |
|---|---|
| F1 折叠 `log.session_verbose` 留孤立标签 | 已修：`<Form.Item>` 整体包进锚点容器（与 `approval.command_allowlist` / `validation.lsp.java_home` 同形），新增「整行含 label 都在隐藏容器内」用例 |
| F2 连续命中不同项时旧高亮永驻 | 已修：新增 `hitElRef`，加类前先摘旧类、卸载一并摘除；新增「同页连中两项只剩一处高亮」用例 |
| F3 保存失败未清 `pendingHit` | 已修：`save()` 两条校验失败分支均 `setPendingHit(null)`；「放弃改动」路径**刻意保留**定位（跳转照常执行，与「保存并离开」同形，见 §1.5） |
| F4 0 高度锚点退化为页体定位 | 已修：`hitTargetOf()` 判定 + `approval.command_allowlist` 登记进 §1.6 |
| F5 搜索 ARIA 语义 | 已修：搜索框 `role=combobox` + `aria-activedescendant`（取值 `results[hitIdx]?.id`，越界保护），listbox 加 `tabIndex={-1}` 见 §1.3 |
| F6 `compact_timeout_seconds` 90 → 180 | **保持 180**：与同类超时/阈值控件就近取档，映射表已补该行（§3.2 末行） |
| F7 haystack 归一无守护 | 已补 en 大写缩写用例（`ttl` / `confirm`），变异自证：删 `.toLowerCase()` → 该用例必红 |
| F8 焦点保护表述不实 | 已改表述（源码注释 + 本文档 + 测试注释）：防御性写法，当前不可构造验证，保留代码 |
| F9 文档台账口径 | 已改为实测口径（18 处 / 7 个数值 / 12 组控件），并修 `docs/0-README.md` 行首多余缩进 |
| F10 ↑ 从 -1 被钳到第 0 项 | **改文档**：明确「-1 时 ↑/↓ 都落到第 0 项」是刻意行为（行为不变，用例已守护） |
