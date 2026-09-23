# 缺陷修复：聊天区自动滚动失效（新会话不跟随 / 提问后要手动点「回到底部」）

> 2026-09-23。现象：① 新会话里内容触底之后不再自动滚动；② 提问（发送消息）后也要手动点一下「回到底部」才跟随。
> 分支 `fix/updater-notes-progress`（与更新弹窗那批改动同分支，见文末说明）。改动全在前端：`utils/scrollAnchor.ts`、
> `features/chat/ChatMessages.tsx`、`tests/scrollAnchor.test.ts`。

## 1 根因

滚动模型（[docs/thinking-scroll-fix](./thinking-scroll-fix.md)）是「贴底跟随 + 用户接管判定」：程序化跳底前登记一个
150ms 豁免窗口，窗口内自家产生的 `scroll` 事件不得被当成用户滚动。**豁免的目标值取错了**：

```ts
progTarget.current = el.scrollHeight;   // 旧实现
el.scrollTo({ top: el.scrollHeight });
```

`scrollTop` 会被浏览器钳到 `scrollHeight - clientHeight`，因此事件到达时 `scrollTop` 与 `progTarget` 的差值
**恒等于 `clientHeight`**（几百像素），远超 40px 容差 —— 豁免判定永远不成立，每一次程序化跳底产生的
`scroll` 事件都会掉进几何判定。

而 `scroll` 事件在下一帧才派发，流式场景下内容几乎必然在这期间又长了一截。此时几何判定算出「未贴底」，
于是 `stickBottom = false`、`atBottom = false`：**跟随被关掉，而且不会自愈**（跟随 effect 以 `stickBottom`
为闸门），用户只能手动滚回底部或点「回到底部」按钮 —— 这正是两个现象的来源。

失败链条（时序，数值为真实量级）：

| 步骤 | 状态 |
|---|---|
| 1 | 贴底态：scrollTop 400 / scrollHeight 1000 / clientHeight 600；内容增长 → 跟随 effect 程序化跳底 |
| 2 | 跳底登记 `progTarget = 1000`（旧实现取了未钳的 scrollHeight），实际落点 400（被钳） |
| 3 | `scroll` 事件派发前内容又长到 1600，scrollTop 仍是 400（浏览器不会因为内容变高就重新滚） |
| 4 | 事件到达：`\|400 - 1000\| = 600 > 40` → 不豁免 → 几何判定 `1600 - 400 - 600 = 600` → 未贴底 → **跟随关闭** |

另有一处同类缺口：切 Tab / 首次激活的「贴底」分支直接 `restoreAnchor(el, null)`（等价于
`scrollTop = scrollHeight`），**根本没登记豁免窗口**，同样会被随后的 scroll 事件误判。

## 2 改动

### `ui/src/utils/scrollAnchor.ts`（新增两个纯函数）

- `bottomScrollTarget({ scrollHeight, clientHeight })` = `max(0, scrollHeight - clientHeight)`，即**浏览器实际落点**；
- `isSelfScroll(scrollTop, from, target)`：位置落在 `[from, target]`（各留 `BOTTOM_EPS` 容差）区间内即视为自家滚动。
  用区间而不是单点，是为覆盖两种情况：① 平滑滚动动画期间位置在 from→target 之间逐帧移动；② 连续多次跳底
  （事件在下一帧才派发，期间目标已变）。

### `ui/src/features/chat/ChatMessages.tsx`

- 新增唯一入口 `jumpToBottom(el, smooth)`（激活贴底 / 内容增长跟随 / **发送后强制跳底** / 「回到底部」按钮
  四处共用），一并登记豁免窗口与跟随态；`markProgScroll` 改签名为 `(el, target, ms, from = target)`：
  target 一律传**实际落点**，from 默认等于 target（瞬时跳底落点即豁免点），只有平滑滚动才显式传发起位置。
- `onScroll` 改用 `isSelfScroll` 做落点/区间豁免（瞬时跳底只剩一个点，不会误吞用户上滚）。
- 激活时的贴底分支改走 `jumpToBottom`（补上缺失的豁免登记）。
- 跟随 effect **先同步跳底**，再在图表升级后补一次：跟随不再押在 `upgradeDiagrams` 的 promise 上
  （升级链路一旦 reject，视图就再也跟不上），并 `catch` 掉升级失败。
- 两个 helper 用 `useCallback` 包住并进 effect 依赖数组（eslint 基线保持原有的 1 条既有 warning）。

## 3 验证

- `scrollAnchor.test.ts` 新增 4 条纯函数用例：落点钳位、旧单点比对必然失败（`|落点 - scrollHeight| = clientHeight`）、
  跳底之后内容又长仍须判为自家事件、平滑区间与用户上滚的边界。
- 同文件新增 1 条 **DOM 回归用例**：happy-dom 没有布局，必须自建桩才能测出这条缺陷 —— 桩里复刻两条真实浏览器事实
  （`scrollTop` 被钳到 `scrollHeight - clientHeight`；`scroll` 事件派发前内容又长了一截），断言自家 scroll 事件
  不得中断跟随。
- **判别力已验证**：把生产代码退回旧写法（目标取 `scrollHeight` + 单点比对）时该用例转红（「回到底部」按钮出现），
  恢复后转绿。用例同时断言「不出现按钮」与「再增长一次内容后位置仍在底部」两半。
- `pnpm --dir ui test`：89 文件 / 1002 用例全绿；`pnpm --dir ui build` 通过；`pnpm --dir ui run lint` 0 error。
- **手动验证清单（GUI 不做自动点验）**：
  1. 新建会话发第一条消息 → 回答流式期间视图持续贴底跟随（不出现「回到底部」按钮）；
  2. 已有会话继续提问 → 同样持续跟随；
  3. 滚轮上滚 → 立即暂停并出现「回到底部」；点它 → 回底并恢复跟随（流式中点也应恢复）；
  4. 拖滚动条 / PageUp 上滚 → 同样能暂停跟随、不被拽回；
  5. 切 Tab 再切回、重开会话 → 仍按上次阅读位置还原（既有行为不回归）。

## 4 已知边界

- 平滑回底（「回到底部」按钮）**才**保留 `from→target` 区间豁免：动画期间位置逐帧移动，必须落在区间内才不被判成用户滚动。
  区间下端是点击时的阅读位置，因此动画期间（窗口 700ms）用户若在区间内小幅滚动会被当成自家事件 —— 这是本批**新引入**的
  宽区间（旧实现因豁免永不成立反而没有它），窗口长度取的是经验上限，按手动清单第 3 项实测校准。
- 平滑动画结束若内容恰好又长高，由跟随 effect 下一帧补跳。
- **同源缺陷已一并修复**（同日后续提交）：`features/subagent/SubagentDrawer.tsx` 原有一份逐字同构的旧写法
  （`progTarget.current = el.scrollHeight` + `Math.abs(el.scrollTop - progTarget.current) < 40`），子代理抽屉的流式
  跟随同样会被自家跳转误关。现复用 `bottomScrollTarget` / `isSelfScroll` / `isAtBottom`（该抽屉只有瞬时跳转，
  豁免区间退化为落点这一点），回归用例在 `subagent.drawer.test.tsx`（同样自建钳位几何桩，
  已做判别力验证：退回旧写法即转红）。
- 本次只动滚动跟随，不改锚点记录/还原、不改「回到底部」按钮的样式与文案。

## 5 与更新弹窗那批改动同分支的说明

本批改动与 [updater-notes-markdown-and-progress-throttle](./updater-notes-markdown-and-progress-throttle.md) 曾同处
`fix/updater-notes-progress` 分支的工作区；收尾时已按主题拆成三个提交（`fix(updater)` / `fix(chat)` / `feat(tasks)`），
文件集合互不重叠。
