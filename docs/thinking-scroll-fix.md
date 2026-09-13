# 15 · 缺陷修复：思考流式期间无法向上滚动

> 缺陷：思考进行中（思考块未展开）时，聊天区无法向上滚动，视图被持续拽回底部；期望展开/未展开都能向上滚。

## §1 根因

滚动跟随由 `ChatMessages.tsx` 的两个机制共同维护：

1. **程序化滚动豁免窗口**：跟随滚动（`scrollToBottom` / 跟随 effect 的 `el.scrollTop = …`）前设置 `progScroll.current = Date.now() + 150`，窗口期内自身触发的 scroll 事件被 `onScroll` 忽略（防 80/40 双阈值振荡）。
2. **跟随 effect**（依赖 timeline 文本长度等，每帧触发）：先做几何判定 `nearBottom < 80` → **为真则强制恢复** `stickBottom = true`，再在 `stickBottom` 为真时续 150ms 豁免窗口并拽回底部。

问题链条：思考 delta 高频到达 → 跟随 effect 每帧刷新，**豁免窗口永续打开**；用户上滚产生的 scroll 事件全部命中豁免被吞，`stickBottom` 永远停在 `true`，视图每帧被拽回底部 → 无法上滚。

展开思考块之所以正常：`onChange` 触发 `suspendFollow`（同步暂停跟随），先于豁免窗口起作用。

## §2 修复（ui/src/features/chat/ChatMessages.tsx）

1. **scroller 增加 `onWheel`**：`deltaY < 0`（向上滚 = 阅读意图）且当前在跟随时——立即 `stickBottom = false`、**清零豁免窗口**（`progScroll.current = 0`，让本次滚动产生的 scroll 事件正常参与贴底判定）、`atBottom = false`（按钮出现）。挂在滚动容器根，任何元素上的上滚都冒泡到此；展开/折叠态与正文流式一并覆盖。
2. **跟随 effect 去掉几何重接管**：删除「`nearBottom < 80` → 强制恢复跟随」——用户暂停是意图，优先于几何；否则刚滚轮暂停就被下一帧恢复。几何恢复只保留两条路径：`onScroll` 的 `nearBottom < 40` 与「回到底部」按钮。
3. **恢复跟随路径**（不变）：暂停后豁免窗口已清零，用户滚回底部触发的 scroll 事件由 `onScroll` 正常处理恢复；或点「回到底部」。
   - 注：曾考虑在 `onWheel` 加「下滚到底立即恢复」分支，因其依赖几何判定（无布局/触控板惯性场景不稳）且与既有 scroll 路径冗余，不设。

## §3 测试与验证

- 新增回归用例（`app.smoke.test.tsx`）：真实发送路径进入思考流式 → 折叠态上滚滚轮 → 「回到底部」按钮出现；内容继续增长后按钮仍在（不被拽回）；点按钮恢复跟随。以行为断言代几何断言（happy-dom 无布局，`scrollTop` 恒 0）。
- `pnpm --dir ui test`：**37/37 全绿**（基线 36 + 新增 1）；`pnpm --dir ui build` 通过。
- 后端未改动。

## §3.1 code-reviewer 审查后续修（结论：通过，无 🔴；3 项 🟡 全部落地）

1. **豁免窗口目标比对**（`ChatMessages.tsx` `onScroll`）：新增 `progTarget` 记录最近一次程序化跳底的目标 scrollTop；窗口期内仅吞「`scrollTop` ≈ 目标（±40px）」的自身事件，位置偏离 = 用户抢滚（滚动条拖拽/键盘 PageUp 等无 wheel 输入）→ 清零豁免、交出控制权按用户滚动处理。未采纳审查原案的「落点是否贴底」判断：流式期间渲染与滚动交错时几何读数漂移会把正常跟随误判成用户滚动。
2. **Tab 切换重置跟随状态**：`ChatMessages` 单实例挂载随 activeKey 换数据不重挂，stickBottom/atBottom 跨 Tab 残留会让新 Tab 凭空出现按钮且不跟随；新增 `useEffect([activeKey])` 重置（声明在跟随 effect 之前，同 commit 内先重置后执行）。
3. **「回到底部」按钮 aria-label i18n 化**：新增 `chat.scrollToBottom`（zh/en），按钮与测试断言走同源文案。

审查其余维度（正确性/安全/性能/可读性/最佳实践/测试守护性——回滚必红推演）均无问题；`onWheel` 未调 preventDefault 与 React root passive wheel 监听语义兼容。

## §4 手动验证清单（GUI 不做自动点验）

1. `pnpm tauri dev`；发一条会触发长思考的消息。
2. 思考中（折叠态）滚轮向上：可正常上滚，出现「回到底部」圆钮；内容继续流式不被拽回。
3. 思考中展开思考块：内部实况尾随；在思考体内上滚可暂停尾随（既有行为不回归）。
4. 滚回底部：恢复自动跟随（新内容继续贴底）。
5. 正文流式期间重复 2–4：行为一致。
6. 流式期间拖滚动条向上 / 按 PageUp：同样能暂停跟随不被拽回（§3.1 目标比对豁免）。
7. 流式期间切到另一 Tab：新 Tab 无「回到底部」残留按钮、正常贴底跟随；切回原 Tab 后滚回底部可恢复跟随。
