# Composer 工具条上下文阈值 / 缓存命中率（composer-toolbar-context-hit-rate）

> 类型：需求批次 + 一处缺陷修复 · 影响层：前端 store（运行态与帧 reducer）+ `features/chat/Composer.tsx` + `features/tools/AskPanel.tsx` + `theme/app.css` + i18n · 契约影响：**零后端改动**（事件面 29 键不变 / config schema 零变化 / 无新增 IPC / `TabRunState` 仅新增一个运行态字段）。
> 用户需求原文：①「上下文增加命中率显示 / 自动压缩阈值显示，上文长度百分比根据实际值区分颜色显示」；②「模型增加提供商名称显示，如 OpenCode Go / deepseek-v4.1-flash」。修订轮追加：③命中率改 4 档着色；④「Composer 中的提交回答不支持回车键，希望支持」。

## 1. 决策记录

需求经 3 轮澄清 + 1 轮修订收敛，逐条锁定：

| # | 决策点 | 结论 | 理由 |
|---|---|---|---|
| 1 | 「命中率」的指标 | **会话级缓存命中率**；**分母按协议口径**（见 §2.2）：anthropic `input + cache_read + cache_write` / openai `input` | provider KV 缓存是否生效是成本与延迟的代理指标；后端已有该数据（usage 帧），不动后端即可显示；口径与统计弹窗同源，否则两处数字不一致（openai 系会系统性偏低） |
| 2 | 上下文百分比的着色基准 | **相对自动压缩阈值**：`ratio ≥ compact_threshold` 红 / `≥ 阈值×0.7` 橙 / 否则中性 | 与「何时会被自动压缩」强绑定，且契合既有约定「色彩强度映射风险等级：无彩色=默认、橙=需注意、红=危险」 |
| 3 | 命中率分档（用户修订） | **四档**：`≥99%` 绿 / `95–99%` 黄 / `90–95%` 橙 / `<90%` 红 | 用户给定；99% 归绿档（用户原表述 `≤99% 黄` 与 `≥99% 绿` 在 99% 处重叠，取闭区间上档） |
| 4 | 排布 | 上下文默认**单行内**追加（`· 阈 60% · 命中 78%`）+ 模型按钮改「供应商 / 模型」；窄窗口折行见 §9 | 阈值与用量/窗口同属「占用事实」；命中率独立成段以便单独隐藏 |
| 5 | 数据的时间范围 | **进程内、按 Tab 累计，不持久化** | 不引入后端/IPC 改动；重启后从 0 重新累计属可接受 |
| 6 | 空态 | `breakdown` 缺失 → 保持 `上下文 —` 且**不渲染**阈值/命中段；usage 全零 → 不渲染命中段 | 避免「— · 阈 60%」这类半空组合，也不为无数据的段挤出占位符 |
| 7 | ask 回车提交（缺陷） | 只把**回车**从「输入框内放行」路径里拎出来提交 | 见 §3：旧守卫让焦点在输入框时的所有按键早退，提交只认鼠标 |
| 8 | 语义解析不到时 | **不显示命中段**（不是回退到某一套公式） | 没有任何依据猜协议；缺省比给一个可能错 30 个百分点的数字安全 |

## 2. 实现要点

### 2.1 命中率数据源：usage 帧此前被丢弃

后端每轮 LLM 调用后下发 `Frame::Usage{input, output, cache_read, cache_write}`（`core/agent/drive.rs`），但前端 `stores/runFrames.ts` 的 `applyFrameToTab` 结尾只有一行注释「usage 帧不进转录」——帧被**静默丢弃**，因此 UI 里根本没有任何缓存命中数据可用。

本轮在 `TabRunState` 新增会话级累加器：

```ts
usage?: UsageTotals;  // { input; output; cacheRead; cacheWrite }
```

两个不显然但必须守住的点：

1. **累加必须放在 `if (!t.running) return;` 早退之前**。该早退是为「运行结束后的迟到 delta 帧不得重建流式项」而设的；而本轮最后一条 usage 帧常在 run 收尾之后才到达，若沿用同一早退，命中率会永久漏掉最新一轮。usage 分支只更新累加器、不碰 timeline，因此不影响那条守卫的语义。
2. **帧字段是 snake_case**（`cache_read` / `cache_write`，与 `ipc/types.ts` 的 `Frame` 联合一致），累加器字段是 camelCase；`applyUsageFrame` 负责这一层映射（写错不会报错、只会让命中率恒为零——首次实现正栽在这里，靠单测抓出）。

`usage` 声明为**可选**字段：`blank()` 一定会给初值，但既有测试夹具里的 `TabRunState` 字面量不会（TS 编译期会逐个点名，实测 18 个测试文件）；消费方（`applyUsageFrame` / `cacheHitRate` / Composer）全部做了缺省处理，历史字面量因此无需改动。

### 2.2 显示形态

```
上下文 62.3%（1.2k / 128k · 阈 60% · 命中 78%）
```

- 仅**百分比数字**与**命中率**着色，用量/窗口/阈值保持次级色（`--ws-dim`）；
- 分档判定一律用**原始比值**（`breakdown.ratio`、命中率原值），不用展示用四舍五入后的百分比——否则会出现「显示 60.0% 却不显红」的显示与语义不一致；
- `compact_threshold` 非法（缺省 / 0 / 负 / >1 / NaN）时隐藏阈值段，且百分比一律中性色（不臆测风险等级，与后端 `clamp(0.05,0.95)` 的比较结果保持一致）；
- 悬浮 `title` 给全量口径（占用原始 token 数、阈值、命中分子/分母），是窄窗口截断时的信息兜底；
- 模型按钮：`{providerName} / {model}`（providerName 为空时只显示模型名，不留 ` / ` 前缀残留）；无生效模型时保持「未配置模型」。

### 2.3 命中率分母：按协议口径（与统计弹窗同源）

命中率的分母**不是一个公式，而是两套**：

| 协议 | `usage.input` 的含义 | 分母 |
|---|---|---|
| `anthropic_messages` | **不含**缓存部分 | `input + cache_read + cache_write` |
| `openai_chat` / `openai_responses` | **已含** `cached_tokens`（cache_read 是它的子集） | `input` |

同一组数字（input 1000 / cache_read 700）：openai 语义 = **70%**；若统一用 `read/(read+input)` 则得 **41%**——openai 系命中率便永远过不了 50%，且与统计弹窗对不上。故本轮不另立公式，而是把口径收敛到**单一事实源**（[prompt-caching-hardening](./prompt-caching-hardening.md) §M1-C 定下的既有口径）：

- `utils/models.ts`：`cacheSemanticsOfFormat(apiFormat)`（协议 → 语义）、`cacheSemanticsOf(config, modelId)`（生效模型 → 语义，会话覆盖优先）、`cacheDenominator(counters, sem)`（唯一分母公式）。
- `stores/runFrames.ts::cacheHitRate(usage, sem)`：语义是**必传参数**（无默认值，避免调用方默默用错口径）。
- `features/panels/TokenStatsModal.tsx`：删掉本地 `semOf` / `hitDenominator`，改用同两个函数——此前这个「协议口径」知识在仓库里有两份副本，正是漂移的温床。
- Composer：`cacheSemanticsOf(config, prefs.model_id)`（与生效模型的解析路径同源：会话覆盖 → 全局回退）；**解析不到（未配置模型 / 供应商已删 / 模型 id 失效）时不显示命中段**——宁可缺省也不猜口径。title 里的分子/分母与显示值同源。

### 2.4 着色映射（`theme/app.css`，一律走 `--ws-*` token）

| 段 | 档 | 类名 | 颜色 |
|---|---|---|---|
| 上下文百分比 | 默认 / 需注意 / 危险 | `.ctx-pct` / `.warn` / `.danger` | `--ws-accent` / `--ws-warn` / `--ws-err` |
| 命中率 | 绿 / 黄 / 橙 / 红 | `.ctx-hit.ok` / `.yellow` / `.warn` / `.danger` | `--ws-ok` / `--ws-accent` / `--ws-warn` / `--ws-err` |

命中率的黄档落在 `--ws-accent`（中性墨色）而非新色值：项目色板只有「中性墨 / 橙 / 红」三色，约定不引入彩色 accent（见 [ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md)）。若后续要真正的黄色，加一个 `--ws-warn-soft` 之类的 token 即可，其余不动。

## 3. 缺陷修复：ask 的「提交回答」不支持回车

**根因**：`AskPanel.tsx` 的 `isFormTarget()` 让「焦点在 INPUT/TEXTAREA 内」时**所有按键一律早退**。而展开卡片后焦点就在「补充说明」输入框里——用户敲回车时必然在此，于是 `onKeyAsk` 的 Enter 分支永远走不到，提交只认鼠标点击。同一守卫也吞掉了审批形态的 Enter 确认（焦点在卡片上时正常，故表现为「有时灵有时不灵」）。此外 hint 文案当时写着「回车 = 下一题 / 提交」，与实际行为不符。

**修复**：抽出 `enterCommits(e)`（`Enter` + 无 Shift + 非 IME 组合期 → `preventDefault()` 并返回 true），在 `isFormTarget` 早退**之前**拦截；三条路径（卡片上的 `onKeyAsk` / `onKeyApproval`、输入框自身的 `onKeyDown`）统一走 `commitAsk()`（审批 = 应答高亮项；询问 = 非末页翻页 / 末页提交）。方向键 / 数字键 / 空格保持旧语义「输入框内不拦截」。

输入框下新增一行提示 `ask.noteHint`（回车 = 下一题 / 提交；Shift + 回车换行）。

## 4. 改动清单

**前端**

- `stores/run.types.ts`：`TabRunState.usage?` + 新增 `UsageTotals`。
- `stores/runFrames.ts`：`blank()` 补 usage 初值；`applyFrameToTab` 新增 usage 分支（置于 `!running` 早退之前）；新增 `applyUsageFrame` / `cacheHitRate(t, sem)`（分母按协议语义，分母 0 → `null`）/ `contextTier` / `hitRateTier` 四个纯函数（可独立单测）。
- `utils/models.ts`：新增 `CacheSemantics` 与 `cacheSemanticsOfFormat` / `cacheSemanticsOf` / `cacheDenominator`（协议口径的唯一事实源，与统计弹窗共用）。
- `features/panels/TokenStatsModal.tsx`：删本地 `semOf` / `hitDenominator`，改用上述共享函数（公式只留一份）。
- `stores/run.ts`：**未新增**派生 hook——语义需读 config，改由 Composer 用纯函数派生（实现过程中曾加过 `useCacheHitRate()`，它只能拿到半套信息，已移除）。
- `features/chat/Composer.tsx`：读 `compact_threshold` 并校验；上下文段改结构渲染（着色 span + 阈值段）；新增命中段；模型按钮 label / title 改「供应商 / 模型」。
- `features/tools/AskPanel.tsx`：`enterCommits` + `commitAsk`；输入框 `onKeyDown`；补 `note-hint` 行。
- `theme/app.css`：`.ctx-pct` / `.ctx-hit` 分档规则。
- `i18n/zh-CN.ts` + `en-US.ts`：`composer.contextThreshold` / `composer.cacheHit` / `composer.ctxTitle` / `ask.noteHint`（双侧同步）。

**未触动的契约**：`ipc/types.ts`（`Frame` 联合早已含 usage 形状）、`host/commands`、事件面 29 键、`settingsRegistry` 的键清单（新增键属 `composer.*` 命名空间，不在设置项闭包内）。

## 5. 验证

| 项 | 结果 |
|---|---|
| `pnpm --dir ui test` | **781 passed / 72 文件**（基线 732 / 71；新增 `runUsage` 10 例 + `utils.models` 4 例 + `app.smoke` 3 例 + `askpanel` 1 例） |
| `pnpm --dir ui build` | `tsc --noEmit && vite build` 通过 |
| `eslint ui/src` | 无告警 |

新增用例：

- `__tests__/runUsage.test.ts`：逐帧累加（含 snake_case 映射）／字段缺失不产生 NaN／`running=false` 后到达的 usage 帧仍累加且不进 timeline／`cacheHitRate` 边界（无数据 → `null`、有数据零命中 → `0`）／**两套协议的同一组数字对比**（openai 分母 = input；anthropic 分母含 cache_write；并断言同数据在两语义下取不同值）／命中率四档边界矩阵（0.89 / 0.90 / 0.94 / 0.95 / 0.98 / 0.99 / 1.0）／上下文三档矩阵（`阈值×0.7` 与阈值两侧）＋非法阈值一律中性。
- `__tests__/utils.models.test.ts`：`cacheSemanticsOfFormat`（含旧写法 `anthropic` 不兜底、空值回 openai）／`cacheSemanticsOf`（会话覆盖与全局回退 / 未知 id 与无 config → `null`）／`cacheDenominator` 两口径。
- `__tests__/app.smoke.test.tsx`：工具条出现「Test Provider / test-model」（供应商名可见）；阈值 + 命中段渲染与分档 class（ratio = 阈值 → `danger`；命中率随数据变化 `danger` → `ok`）；**分母跟随协议**（把同一份用量切到 anthropic 语义，断言显示值随之改变，且 title 的分子/分母同源）；**语义解析不到时不渲染命中段**；无 `breakdown` 时保持「上下文 —」且不出现阈值/命中段。
- `__tests__/askpanel.test.tsx`：在 `.ask-note` 输入框内敲回车 → 提交且载荷携带 note 文本。

## 6. 手动验证清单（界面改动不做 GUI 自动点验）

1. 新开临时会话未发消息：工具条只显示「上下文 —」，无阈值/命中段、单行。
2. 发一轮：出现百分比与用量，并出现「阈 60%」（与设置页滑条一致）；连发 3–5 轮长对话看「命中 NN%」出现并随轮次变化、颜色随档位变化。
3. 设置页把阈值滑到 0.1：同一会话百分比立即转橙/红，无需重开会话。
4. 点压缩按钮：百分比回落、颜色退回低档，命中率段仍在。
5. 切两个 Tab：命中率各自累计、互不串。
6. 切到不同供应商的同名模型：按钮立刻显示新「供应商 / 模型」，hover 为 `供应商 / 模型`。
7. 触发一次 ask：在「补充说明」框内直接敲回车 → 提交；再试点卡片后回车、以及 Shift+回车（不提交）。
8. 拖窄窗口 + 暗色主题 + 切英文：**控件行**不换行；信息段在窄窗口于「命中」「速率」前折行（各段完整可见、不截断，见 §9）；模型名省略但 hover 是全名；四档颜色可读。

## 7. 已知偏差与遗留（本次刻意不做）

- **openai 系命中率口径**：**已修**（见 §2.3）——分母按协议语义取，与统计弹窗同源；未修的残余只在于「命中率仍不跨重启、不与 30 天统计同窗口」（见下一条）。
- **黄档用中性墨色**（见 §2.4）：色板无独立黄色 token，横比橙档时层次偏弱。
- **命中率不持久化**：关 Tab 或重启即清零，与统计弹窗的 30 天口径不同源。
- **子代理 usage 不计入**：`Frame::Sub` 信封内的帧由 `applyFrameToSub` 消费，其 usage 不进主 Tab 累加器；子代理的 token 目前在子代理卡上单独展示。
- **Shift+Enter 无可见换行**：补充说明仍是单行 `Input`（多行需换 `TextArea`），hint 按现状表述；若要多行补充说明，是独立的小改动。
- **阈值/命中段的降级顺序仍未做代码级实现**（2026-09-24 起口径改为**折行**，见 §9）：不再追求「命中 → 阈值 → 括号」逐段隐藏，也不再靠省略号截断——窄窗口按 `<wbr>` 折行、段内 `nowrap`，hover 的 `title` 仍给全量。

## 8. 后续追加：token 速率段（2026-09-21）

在同一行的命中段**之后**追加了第三段「生成速率」（[composer-token-rate](./composer-token-rate.md)），本档的既有约定继续沿用：

- **同一行同一段**、同样中性灰小字、原生 `title` 承载明细；**不做分档着色**（速率不是风险等级）。
- **数据源分工**：本档的上下文/命中段读**会话级** `TabRunState.usage`（跨 run 累加、不持久化，见本档 §7）；速率段读**本轮** `TabRunState.runMetrics`（`send()` 处归零，避免跨轮累积）。
- **空态不变**：`breakdown` 缺失时仍是 `上下文 —`，速率段整体不渲染——本档 §1 决策 6 与相关断言逐字保持。
- 速率段只在收到带 `duration_ms` 的 usage 帧后出现（旧后端 / 前端重挂载后未收帧 → 隐藏，不显陈旧值）。

## 9. 后续追加：窄窗口折行（2026-09-24）

**缺陷**：左右栏拖宽后 composer 变窄，这串小字（上下文 / 命中 / 速率）与右侧模型选择器（`供应商 / 模型`）**文字重叠、完全不可读**。

**根因**（纯样式层，与 `position` / `z-index` 无关）：`.composer-toolbar .ctx-label` 当时只有 `white-space: nowrap`，**既无 `min-width: 0` 也无任何裁剪边界**（有 `min-width: 0` 的是它的父级 `.toolbar-info`）。`nowrap` 让这个 flex 项的自动最小尺寸 = min-content = 整串宽度 → 它根本不能收缩，内容便**溢出绘制**到右邻居（模型选择器）上。

**取舍**：用户明确要「折行（完整可见）」而非「截断（省略号）」——这三个数字正是要读的内容，截断等于看不见。

- **折行点**：`Composer.tsx` 在「命中」「速率」两段前各插一个 `<wbr />`（零字符断点，`textContent` 逐字不变）；`ctx-hit` / `ctx-rate` 两条既有规则**就地**追加 `white-space: nowrap`，首个原子段（上下文 + 阈值括号）包进 `.ctx-seg`（同样 `nowrap`）——折行只发生在段间，段内不拆。
- **分隔符不可断**：两处「 · 」各自包进 `.ctx-sep`（`nowrap`）。裸文本节点里的空格本身就是断行机会，浏览器会把「 · 」单独甩到下一行行首；包起来之后 `<wbr>` 才是唯一断点。
- **兜底**：`.ctx-label` 去掉 `nowrap`、加 `min-width: 0; overflow: hidden`——单个原子段仍比可用宽度更宽时裁切，不再压到邻居上。
- **刻意不做**：不给 `.composer-toolbar` 加 `flex-wrap`（控件行仍单行，按钮不换行）；不给 `.composer-card` / `.composer-toolbar` 加 `overflow: hidden`（会切掉 BorderBeam 的负 inset 流光与按钮 focus 环）。
- **副作用（已知并接受）**：① 折行后工具条随之增高 1–2 行（极端窄宽 3 行），输入卡片整体变高；② 可用宽度下限 ≈ 最长的原子段（`上下文 60%（76.8k / 128k · 阈 60%）` ≈ 170px），再窄则首个原子段被硬裁——那正是最该读的数字，属刻意取舍；③ 分隔符「 · 」不再继承命中档颜色（原先它住在 `.ctx-hit` 里），回到 `.ctx-label` 的中性灰。
- **契约**：`ui/src/__tests__/composer.toolbar.style.test.tsx`（9 用例）——CSS 侧钉住「`.ctx-label` 无 nowrap + `min-width:0` + `overflow:hidden`」「`.ctx-seg` / `.ctx-sep` / `.ctx-hit` / `.ctx-rate` 均 nowrap」「任何以 `.ctx-label` 结尾的选择器都不得重新引入 nowrap」「容器不加 flex-wrap / overflow」；DOM 侧钉住「`.toolbar-info > .ctx-label`」「恰两个 `<wbr>` 且分别在命中段与速率段之前」「全量与半段（只有命中 / 只有速率）文本逐字不变」「空态不引入断点」。
- **手动验证**：1440×900 把左右栏拖到最宽（composer 最窄）、1024 宽窗口、有/无速率段、长/短模型名、暗色 + 英文各看一遍：小字折行后完整可读、不压模型选择器、行首不出现孤立「 · 」；恢复默认栏宽回单行。

## 10. 后续追加：中等 / 窄窗口不再折叠 `.toolbar-info`（2026-09-26，[fix/composer-toolbar-and-anthropic-cache]）

**缺陷**：`[data-narrow="medium"]` / `narrow` 下，Composer 工具条**速率段消失**（上下文/命中率已被迁到 Popover，`.toolbar-info` 块内仅剩“18.2 tok/s”这一段）。

**根因**（CSS 折叠遗留）：§9 把上下文/命中迁到 Popover 后，`.toolbar-info` 只剩速率段，但 `ui/src/theme/app.css` 还有遗留折叠规则——该规则是「上下文/命中/速率三段合一」时代留下的（[docs/composer-responsive-toolbar] 原始设计），折叠整块 = 速率消失。

**修复**：删 `.composer-card[data-narrow="medium|narrow"] .toolbar-info { display: none }`。`.toolbar-info` 块在 narrow 模式下只留下 ≈ 70px 的速率文字，保留不与邻居冲突。`tb-model-provider` / `tb-mode-text` / `tb-effort-text` 的折叠规则不变。

**契约补**：`ui/src/__tests__/composer.toolbar.style.test.tsx` 新增 1 例，钉死任何 `.composer-card[data-narrow=…] .toolbar-info … { display: none }` 都不允许出现在 CSS 中。

**刻意不做**：不重引入 `.toolbar-info` 的 `min-width` 或 `overflow:hidden` 拦截——速率段（"18.2 tok/s"）在最窄窗口下仍可被压缩到 1 字符级别，且与 Popover 按钮不重叠。
