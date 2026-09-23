# 更新弹窗：发布说明按 markdown 渲染 + 下载进度不再「卡在某个百分比」

> 2026-09-23。两条来自用户的问题：
> ①「更新内容需要当作 markdown 来渲染」；②「下载进度有问题，17% 就卡住，直到下载完成」。
> 分支 `fix/updater-notes-progress`（基线 `ab08b56`）。改动全部在前端：无后端改动、无新依赖、无删除。

## 1 现象与根因

### 1.1 发布说明按纯文本展示（需求）

`ui/src/features/panels/UpdateModal.tsx` 把 `notes` 直接塞进 `<div className="updater-notes-body">{notes}</div>`，
文件头注释也写着「发布说明按纯文本展示…不做 markdown 渲染」。

而 `notes` 的真实来源就是 Release 正文（`.github/workflows/release.yml` 生成、经 tauri-action 的
`releaseBody` 写进 `latest.json` 的 `notes`），本身就是 markdown 源文：

```
## What's Changed
- chore(release): v0.7.0 (ab08b56)
- feat(tasks): rebuild scheduled tasks as a full page with a period picker (6355c0e)
```

用户看到的是原样标记字符，链接也不可点。

### 1.2 下载进度卡在某个百分比直到下载完成（缺陷）

根因：**每个下载分片都触发一次 IPC + 一次整弹窗重渲染，主线程被渲染吃满，显示进度远远落后于真实下载。**

证据链（逐条读源码核实，非推测）：

| 环节 | 事实 |
|---|---|
| 事件量 | 线上 macOS 更新包 `CodeWave_0.7.0_aarch64.app.tar.gz` = 19,089,436 字节；tauri-plugin-updater 对**每个 HTTP 分片**发一条 `Progress{chunkLength}`（千级到数千级事件） |
| 投递代价 | tauri 2.11.5 的 `Channel` 对每条消息都走 `webview.eval("window.__TAURI_INTERNALS__.runCallback(...)")`，在 macOS 上即 WKWebView 的 `evaluateJavaScript`——**每条都要在主线程事件循环排一次队**（`tauri-runtime-wry` 的 `eval_script` → `send_user_message`） |
| 渲染代价 | 旧实现的回调**每个事件**都调 `store().setProgress()`，唯一订阅者 `UpdateModal` 因此每个事件重渲染一次（antd Modal + Progress + 发布说明整块） |

处理速度 < 事件到达速度 → 显示值一路落后于真实下载；`downloadAndInstall` 一 resolve 就 `markReady()`，
进度区立刻被换成「待重启」，**用户永远看不到积压事件的排空**——表现就是「卡在 17% 然后直接完成」。

已排除：`Content-Length` 失真（方向相反，且 `Finished` 会把进度钳到总量）；相位 / epoch 逻辑
（进度区只由 `phase` 与两个进度字段驱动，与 `checkEpoch` 无关）。

**附带确认的结构性隐患（一并加固）**：JS 侧 `Channel` 用严格递增序号保序——只有
`index === nextMessageIndex` 才回调并递增，否则把消息塞进 `pendingMessages` 等待；而
`window.__TAURI_INTERNALS__.runCallback` **没有 try/catch**。也就是说回调里抛一次异常就会让序号停住、
**之后所有进度事件永久积压且永不复原**。这条与根因无关，但触发后的表现与本次现象完全一致，故一并堵死。

## 2 改动

### 2.1 发布说明按 markdown 渲染

- `ui/src/features/panels/UpdateModal.tsx`：`notes` 经 `utils/markdown.ts` 的 `renderMarkdown()` 渲染
  （`useMemo` 缓存，下载相位的高频重渲染不会白白重解析），容器改为 `className="updater-notes-body md"`
  + `dangerouslySetInnerHTML`。沿用既有渲染器的安全语义：`html:false` 不信任原始 HTML、外链自动带
  `target=_blank rel=noopener`、代码块带 Copy 按钮（`codecopy.ts` 的 document 级委托接管）。
- `ui/src/theme/app.css`：
  - 共享 markdown 样式组 `:is(.assistant .md, .plan-body.md)` **追加** `.updater-notes-body.md`
    （与 `AskPanel` 的 `.plan-body.md` 同法；只加选择器、不改既有声明值，故聊天区与计划卡的观感不变）。
  - `.updater-notes-body` 去掉 `white-space: pre-line`（块级元素自带换行，留着会与块标签之间的换行
    叠加出多余空行），补 `max-width: 100%`。
  - 新增**弹窗局部**规则（不改共享组，避免波及聊天区/计划卡）：标题收敛到 13px/600、列表缩进、
    引用左线、首尾元素去外边距、表格整体横向可滚、图片限宽。
- 空值口径不变：`notes` 为空或纯空白已由 `stores/updater.ts` 的 `markAvailable`
  （`notes && notes.trim() ? notes : null`）归一为 `null`，弹窗侧不重复判定。

### 2.2 进度写入降频 + 回调不抛异常

`ui/src/utils/updateCheck.ts` 的 `startUpdate()`：

1. **只把「用户能看出来的变化」写进 store**（`writeProgress`）：有总量时按可见整数百分比
   （`Math.min(100, Math.round(downloaded/total*100))`，与弹窗显示口径一致）、无总量时按字节跨过
   64 KiB；`Finished` 与 `await` 返回后的收尾一律强制写最终值。
   19 MB 包**有总量时**从千级整弹窗重渲染降到 **≤101 次 + 1 次收尾**（无总量时按 64 KiB 跨步约 291 次，
   仍是「千级 → 百级」）。
2. **事件回调整体包 `try/catch`**：任何异常都不许穿出 Channel 回调（穿出即永久卡死后续事件，见 §1.2），
   畸形事件直接忽略。

两条不变量都写进了注释，防止后人回退成「每事件一次写入」。

## 3 验证

- `pnpm --dir ui test`：新增/改写的用例全绿
  - `updateCheck.test.ts` 新增：进度写入次数上界（1200 个分片 ≤110 次 store 通知）、畸形事件不抛异常、
    总量未知时按字节跨步且收尾必写。**既有两条进度用例（`Started/Progress/Finished` 到 100%、
    总量未知不产生 NaN）一字未改即通过**——这本身就是「最终态语义未变」的证据。
  - `updater.modal.test.tsx`：「纯文本展示」用例改为结构断言（产出 `h2`/`ul`/`li`/`strong`/`code`，
    正文不再出现标记字符），新增 XSS 与链接安全断言（原始 HTML 转义、`javascript:` 不可点、
    外链带 `target=_blank rel=noopener`）、`notes` 为空/空串不渲染。
- `pnpm --dir ui build`：type check + vite build 通过。
- **真机手动验证（用户执行，界面改动按项目约定不做自动点验）**：
  1. 用低于线上版本的打包版启动 → 设置 → 关于 → 检查更新 → 弹窗点「下载并安装」；
     进度应**平滑推进到 100%** 再切到「安装完成，重启后生效」。
  2. 同一弹窗里发布说明应有排版（`## What's Changed` 成标题、条目成列表），长内容可滚动、
     不撑破 440px 弹窗，链接点击走系统浏览器；带表格的说明也看一眼（表格整体横向可滚，
     `display: block` 会让单元格边框失去 `border-collapse` 合并，可能呈双线观感——已知且影响极小）。
  3. 点「隐藏」后下载继续，完成后弹窗自动重新出现（行为不变）。

## 4 边界与遗留

- **根因判断的残余不确定性**：本方案的主因是「渲染/IPC 饱和导致显示滞后」，机制与量级都自洽，但没有
  在真机上做过限速对照实验。**若降频后真机仍卡住**，说明瓶颈在 tauri `Channel` 逐条 `eval` 的投递侧
  （上游），届时另立方案——候选是命令式直写 DOM（会偏离「组件样式一律 antd 接管」的约束，需用户拍板）
  或向上游记录该观察。判别的办法：限速到 100–200 KB/s 复跑，若此时平滑、全速仍卡，则瓶颈在投递侧。
- **发布说明里的 mermaid / 数学公式不渲染**：`renderMarkdown` 会把 ```mermaid / ```math 代码块产出
  `.ws-diagram` / `.ws-math` 占位符，而升级链路（`utils/diagrams.ts`）与样式都绑在聊天气泡上。
  发布正文不会出现这两类内容，故不接通。
- **暗色主题下弹窗内代码块仍是浅色、链接仍是 UA 默认蓝**：前者是共享 `pre` 规则硬编码的 github 亮色，
  后者全仓库都没有全局 `a` 规则——两处都是跨消费点的既有观感，单改弹窗会造成新的不一致，本次不动。
- **发布说明里的远端图片会显示为破图**：CSP 是 `img-src 'self' data: blob:`（`src-tauri/tauri.conf.json`），
  远端图被拦。样式里的 `img { max-width: 100% }` 只保证不撑宽。发布正文由本项目 CI 生成、不含图片，故不管。
- **进度显示颗粒变粗**（1% 步进 / 64 KiB 步进）：这是刻意取舍，远优于「卡住」；不提供回退开关。
