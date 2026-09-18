# 子智能体抽屉纵向滚动：三层排查 + 防线加固批次

> 缺陷：「子智能体 Drawer 无法纵向滚动内容」第三次报告（前两次分别由 [docs/rightbar-visual-batch](./rightbar-visual-batch.md) 同类 `.rb-tabs` 缺陷史与 e766796 修复覆盖）。
> 本批不做第四次盲修 CSS——先给出完整排查证据链，再落地「CSS 钉死 + JS 视口兜底 + 贴底语义对齐 + 运行时探针」四层防线。

## §1 排查证据链（本批核心产出）

### 1.1 静态层：HEAD 滚动链在所有级联排列下成立

逐一核对 antd 6.6.2 源码（`antd/es/drawer/`、`@rc-component/drawer@1.4.2`、`@ant-design/cssinjs-utils`）后的事实清单：

| 层 | 来源 | 关键样式 | 结论 |
|---|---|---|---|
| `.ant-drawer`（root，portal 到 body） | antd | `position:fixed; inset:0; pointer-events:none` | 视口定界 |
| `.ant-drawer-content-wrapper` | antd `&-right > wrapper` 规则 | `position:absolute; top:0; right:0; bottom:0` | 定界 ✓ |
| `.ant-drawer-section.sub-drawer` | 项目 `.sub-drawer.ant-drawer-section` | `display:flex; column; height:100%; overflow:hidden` | 定界 ✓ |
| `.ant-drawer-header` | 项目 | `flex:none` | 不收缩 ✓ |
| `.ant-drawer-body` | 项目 + antd 默认 | `flex:1; min-height:0; display:flex; column` + antd `overflow:auto` | 定界 ✓ |
| `.sub-drawer-body` | 项目 | `flex:1; min-height:0; overflow-y:auto` | **唯一滚动体** ✓ |

关键级联事实：**antd v6 组件样式包裹在 `@layer antd` 中**（`cssinjs-utils` `genComponentStyleHook`：`config.layer || { name: 'antd' }`），CSS Cascade 规定未分层声明恒胜分层声明（与顺序、特异性无关）——项目 app.css 未分层，对 section/body 的钉死规则稳赢 antd 默认值；body 的 `overflow:auto`（antd）无竞争方，本就生效。即：**e766796 之后的静态链每一层要么定界要么可滚，双 scroller 兜底**。

另核对的排除项：`mask=false` 时 section `pointer-events:auto`（antd 分层规则，无竞争方，生效，事件可达）；antd 6 的 `size` 数字宽度正常；`no-mask` 类无样式副作用；`@rc-component/drawer` 单版本 1.4.2（无解析漂移）；探针测试锁定的 DOM 契约（section 直下 header+body）与源码一致。

### 1.2 历史：v0.2.0 旧链同样成立

v0.2.0（3346870）时 antd 已是 ^6.6.2（section DOM 已就位），旧链 `.sub-drawer-body{height:100%}` 在 Chromium 下百分比解析同样成立。即**新旧两版静态上都能滚**——「用户三次报告同一症状」无法用任何一版 CSS 解释。

### 1.3 决定性线索：截图视图停在顶部

用户复现截图（product-manager 子代理运行中）里视图停在**顶部**：若 scroller 正常，贴底逻辑会把视图钉在底部。视图在顶部 ⇒ 运行时 scroller 无溢出（内容被外层 `overflow:hidden` 裁切、`scrollTop=scrollHeight` 写入为 no-op）⇒ 存在静态分析覆盖不到的**运行时几何偏移**。

### 1.4 用户确认：`pnpm tauri dev` + 滚轮完全无反应

结合 1.3，两大候选：**(a) 陈旧 dev 会话**——vite HMR 因休眠/断连失效后，Tauri 窗口仍跑 9/4 修复之前的前端代码（与全部静态证据相容，概率最高）；**(b) 运行时几何偏移**——只能靠运行时探针定位（见 §2.4）。

> **Step 0（用户执行）**：彻底退出 dev 实例（含 node/vite 进程）后干净重启 `pnpm tauri dev` 复测。若修复，根因即 (a)；若仍复现，§2.4 探针输出直接指名断裂层。

## §2 加固内容（无论 Step 0 结果均落地）

### 2.1 CSS 链项目侧全链钉死（app.css）

- Drawer 传 `rootClassName="sub-drawer-root"`；新增 `.sub-drawer-root > .ant-drawer-content-wrapper { top:0; bottom:0 }`——wrapper 定界不再依赖 antd `@layer` 内的 placement 规则
- `.sub-drawer .ant-drawer-body` 追加 `overflow:hidden`——中间层明确为纯透传层，贯彻「唯一滚动体 = `.sub-drawer-body`」

### 2.2 JS 视口兜底钳制（SubagentDrawer.tsx，[docs/rightbar-visual-batch](./rightbar-visual-batch.md) → e766796 → 本批的防线升级）

开抽屉/resize 时把 scroller 钳到 `window.innerHeight - header.offsetHeight`。CSS 链健康时与 flex 结果恒等（零副作用）；任意上级运行时失真时它独立恢复滚动——**唯一不依赖任何上级 CSS 的保险**。且钳制前后高度差超过阈值即 DEV 告警「CSS chain is not bounding」（见 2.4）。

### 2.3 贴底逻辑对齐 [docs/thinking-scroll-fix](./thinking-scroll-fix.md)（修复 40px 回贴竞态）

现版缺程序化滚动豁免窗：小幅上滚（触控板 <40px）被 onScroll 重判回贴底，流式期间视图被反复拽回。按 ChatMessages 成熟模式补齐：

- `progUntil/progTarget` 豁免窗（150ms + 目标比较 40px）
- onWheel 上滚立即暂停并清窗；onScroll 豁免自身事件后再做 nearBottom 判定
- **审查补全**（三处移植缺口）：`onUserToggle={suspendFollow}`（展开卡片=阅读意图，不再被拽出视口）；`prevSubId` 变化时重置跟随态（切换子代理不再滞留原滚动位）；`sig` 改为全量 reduce（thinking 增量并入尾段不改 timeline 长度，纯思考流式期间跟随不再冻结）

### 2.4 运行时自检探针（dev-only）

- **钳制告警**：钳制实际改变布局高度（before > cap+2 且内容溢出）⇒ `console.warn` CSS 链未定界
- **裁切探针**：scroller 报告无溢出但底边超出视口（`getBoundingClientRect().bottom > innerHeight+1`，比 scrollHeight 对比更精确）⇒ 逐层 dump wrapper/section/body/scroller 的 computed `display/height/overflow/client/scroll`
- 均每次打开至多一次，`import.meta.env.DEV` 门控，生产零输出

## §3 审查与验证

- code-reviewer：🔴 0；🟡 5 项全部采纳修复（2.3 三处移植缺口 + 探针底边判据 + header 契约断言）；🟢 采纳泛型 querySelector、DEV 早退
- `pnpm --dir ui test` 255/255 全绿；`pnpm --dir ui build`（tsc + vite）通过
- 契约零变化：无新增事件键、无 IPC 变化；改动 3 文件（SubagentDrawer.tsx / app.css / subagent.drawer.test.tsx）

## §4 手动验证清单（Step 0 重启后逐项）

1. **重启验证**：彻底退出 dev 实例（任务管理器确认 node/vite/CodeWave 全部退出）→ `pnpm tauri dev` → 子代理运行中打开抽屉
2. 滚轮上下滚：内容超出一屏时上下均可达；贴底时新流式内容自动跟随
3. 触控板小幅上滚：松手后视图不被拽回底部（40px 回贴竞态修复）
4. 展开「思考完成」块/工具卡：流式期间展开后卡头不被拽出视口
5. 切换子代理（Composer 指示器菜单）：新子代理视图从底部开始跟随
6. 窗口缩放后再滚：钳制随 resize 更新，仍可滚到底
7. DevTools 控制台：不应出现 `[sub-drawer-probe]`；若出现「viewport clamp engaged」说明 CSS 链未定界（钳制已兜底但需回报断层信息）；若出现逐层 dump，把输出发回作为下一轮定位输入

## §5 二轮修复：flex 收缩压扁流块（用户复测新症状后的真浏览器取证）

### 5.1 新症状

「出现了滚动条，但不显示子代理的运行过程，滚动条始终锁死在底部」——首轮防线（§2）生效了一半：钳制/钉死让滚动体真正有界，但暴露出一个此前不存在的布局缺陷。

### 5.2 真浏览器取证（vite dev + 页面内 seed 假运行态 + 实测 Chromium 布局）

修复前实测数据：

```
.sub-drawer-body       clientH=905  scrollH=905    ← 滚动体报告「无溢出」，滚轮完全无反应
.msg.assistant(sub)    rectH=238    scrollH=1614   overflowY=hidden  ← 1376px 过程内容被压没
```

**根因**：有界的 flex 列滚动容器会把子项**压缩到恰好装满**而不是溢出——流块带全局 `.assistant { overflow: hidden }`，CSS 规定 overflow 非 visible 的 flex 项**自动最小尺寸为 0**，于是 flex-shrink 把整个运行过程压扁进剩余空间：过程内容在 DOM 里（scrollHeight 巨大）但不可见；滚动体永远「无溢出」；贴底逻辑钉住的是压扁后的假底部。对照：主聊天滚体 `.chat-messages` 是普通 block，无此问题。

### 5.3 修复

1. **`.sub-drawer-body > * { flex-shrink: 0 }`**——子项保持自然高度，溢出归滚动体所有（app.css）
2. **钳制时序**：面板 DOM 比 `open` 翻转晚一拍挂载（rc-drawer CSSMotion），开抽屉时 bodyRef 还是 null，clamp 空跑（实测 maxHeight:"none"）→ clamp 提为 useCallback，由 callback ref 在滚体真实挂载后的下一帧补跑（同时补首次贴底）
3. **首开贴底落空**：同因——open/sig 依赖的 effects 在面板挂载前已跑完且不重跑，首次 `scrollToBottom` 扑空（实测 scrollTop 停 0）→ 同由 callback ref 兜住

### 5.4 修复后回归（同一浏览器 harness 实测）

```
freshOpen  scrollTop=551 == maxScroll=551   maxHeight=905px   ← 首开自动贴底 + 钳制武装 ✓
follow     scrollTop=577 == maxScroll=577                     ← 流式增长自动跟随 ✓
wheelUp    scrollTop=227                                      ← 上滚暂停 ✓
paused     scrollTop=227 保持（maxScroll 涨至 603）            ← 暂停不被拽回 ✓
stream     rectH=1044 == scrollH=1044                         ← 过程完整可见 ✓
```

`pnpm --dir ui test` 255/255 全绿；`pnpm --dir ui build` 通过。

## §6 追加调整：分配的任务可折叠

抽屉里的「分配的任务」块改为**默认展开显示全文**（2026-09-18 调整，原为默认收起）：标签行是开关（右侧 chevron，展开时旋转 90°），收起态显示单行 ellipsis 预览气泡（hover 有完整 title），点击标签行或预览气泡均可展开/收起，`aria-expanded` 同步。切换子代理时重置回展开。

改动理由：原默认收起 + `sub:spawn` 的 `trunc(&args.task, 2000)` 二者叠加，导致展开后也只能看到 2000 字符 +「…(truncated)」，任务全文在 UI 里根本不可达（实测近期委派任务长度 2408–5716 字符，全部超限）；同时落盘历史存的是全文，因此恢复会话后反而完整，实时/归档不一致。现全量下发 + 默认展开，折叠能力保留供长任务让出首屏。

历史动机（保留档案）：任务文本是静态上下文（且动辄数屏），过程流才是抽屉的活内容——收起后首屏直接让给过程流。浏览器 harness 实测：收起预览单行 35px、展开完整气泡 590px、aria 状态正确。
