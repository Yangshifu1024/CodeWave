# 思考跑马灯重写（同行剩余宽 + 溢出左爬 + 新行上翻）

> 批次内容：按需求重写「思考中」头部跑马灯——① band 与「思考中 · Ns」同一行，占满剩余区域；② 一行内容先左对齐停留，显示不全时才开始从右向左爬动；③ 上翻只在真正出现新一行（`\n`）时触发，同行增长不翻页。纯前端（`ui/src/features/chat/segments.tsx` + `ui/src/theme/app.css`），无事件/IPC 契约变化。

## 旧实现与差距

旧跑马灯取思考文本**末尾 120 字符**（`text.slice(-120)`）塞进标题右侧剩余宽度，CSS 无限循环「停留 1/3 → 向左平移自身宽度 2/3」（14s loop）：不分行、永远整体滚出，看不出「当前正在想的那一行」。

## 新实现（`ThinkingMarquee`，segments.tsx）

内容模型：按 `\n` 切分思考文本，band 始终展示**最新非空行**（流式思考的「当前行」）。

1. **同行剩余宽 band**：`.thinking-label` 保持单行 flex（标题 `flex:none` + marquee `flex:1 1 0` 占满剩余区域），槽高固定 `--marquee-slot: 16px`；左右各 12px 对称掩码淡入淡出（行自右缘进入、左缘退出）。
2. **先左停、溢出才爬**：新行入场 `translateX(0)` 左对齐，停留 700ms（`MARQUEE_DWELL_MS`）；之后每 150ms 采样 `scrollWidth - clientWidth`，溢出则把行平移到 `-(overflow)`——已完成的长行滚出尾部后停住；仍在增长的行溢出量持续变大，CSS `transition: transform 0.25s linear` 把逐次 retarget 平滑成连续右→左爬行（tail-follow，最新内容始终钉在右缘）。溢出 ≤2px 的行保持静止左对齐。`prefers-reduced-motion`：不爬、行静态左对齐。
3. **上翻仅在真换行时**：用 `lineCountRef` 记录已处理行数，只有行数增长（`\n` 到来且新行有内容）才触发 `.rolling`——CSS 动画 `thinking-roll-up` 把轨道 translateY 推一个槽高（0.3s ease），旧行上滑出、新行自下滑入；320ms 超时收尾（不用 `animationend`，reduced-motion 下动画被禁也能完成）。**同一行内容增长走原地换字分支**：不重置爬动偏移、不重启停留计时、不出现 `rolling`。`\n` 后的空白瞬间（字符未到）跳过且不推进计数，字符迟到仍按「新行」翻页；滚动中途又来新行仅替换目标内容、不重启动画。

## 追加调整（同批用户反馈）

- 初版把 band 放在标题下方独立整行（width:100%），按反馈改回**与标题同一行、flex 占满剩余区域**。
- 初版对最新行文本做相等比较，流式逐 chunk 到达会次次触发上翻；改为**行数比较**，只有真正出现新一行才翻页。

## 验证

- `pnpm --dir ui test`：255/255 全绿，新增用例「思考跑马灯（[docs/thinking-marquee-rewrite](./thinking-marquee-rewrite.md)）：全宽 band 展示最新行，换行后上翻到新行」（`app.smoke.test.tsx`，走真实 send 路径注 `delta_thinking` 帧：断言 band 在头部、`.in` 行随最新行更新、**同行增长无 `rolling`**、`\n` 帧后上翻到新行）。
- `pnpm --dir ui build`：type check + vite build 通过。
- 手动验证清单（`pnpm tauri dev`）：
  1. 发消息触发思考流式：「思考中 · Ns」右侧 band 出现，内容为最新一行、先靠左出现；
  2. 单行变长超出 band 宽：约 0.7s 后开始向左爬动，右缘始终显示最新字符；
  3. 思考内容换行：band 上翻切换到新行；同一行持续输出时 band 不翻页、持续爬动；
  4. 系统开启「减弱动态效果」：band 静态显示最新行、不爬不翻。
