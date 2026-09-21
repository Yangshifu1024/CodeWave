# Composer 触发符改为光标感知（缺陷修复）

> 日期：2026-09-21。用户反馈：「composer 中，已存在输入内容的情况下，在内容中或头部无法使用 slash 调起技能 / dollar 拉起子代理 / @ 拉起上下文」。
> 关联：[slash-skills-and-dollar-agents](./slash-skills-and-dollar-agents.md)（触发符语义契约与模型侧点名规则）、[composer-file-ref-chips](./composer-file-ref-chips.md)（`@` 引用的 chip 化与回填门禁）、[run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md)（菜单键盘导航来源）。

## 1. 成因

三个触发符的判定**全部锚定「整段文本的末尾」，且完全不读光标**（`ui/src/features/chat/Composer.tsx` 改造前）：

| 触发符 | 改造前判定 | 失效场景 |
|---|---|---|
| `/` 技能 | `/^\/(\S*)$/`——要求**整段就是** `/…` 且不含空白 | 正文里只要还有别的字就永不命中 |
| `$` 子代理 | `/\$([^\$\s]*)$/`——整段末尾的片段 | 光标在中间/开头时片段后面还跟着正文 → 不命中；查询词被算成「触发符到**文末**」 |
| `@` 提及 | `/@([^@\s]*)$/`——同上 | 同上；且 `@` 前不要求边界，`me@x.com` 会误弹 |

回填同源：`pickMention/pickSkill/pickAgent` 的替换正则也是整段末尾锚定，光标在中间时会把**末尾**那段替换掉。

## 2. 新口径（只看光标前的片段）

新增纯函数模块 `ui/src/features/chat/composerTriggers.ts`：

```ts
fragmentBefore(text, caret)  // 光标前最后一段不含空白的文本（起点天然是行首或空白之后）
detectTrigger(text, caret)   // → { kind, query, start, end } | null；end = 光标，回填替换 [start, end)
```

| 触发符 | 门禁 | 结果 |
|---|---|---|
| `@` | 片段起点落在**行首或空白之后**（片段本身不含空白） | 正文**任意位置**可用（句首、句中、行首）；`me@x.com`、`看这个@a` 不再触发 |
| `/` | 片段必须从**消息 offset 0** 开始 | 修好「消息开头打、后面已有正文时打不开」；句中静默不触发 |
| `$` | 同上 | 同上 |

**`/`、`$` 为什么不一起放宽到句中**：模型侧的点名规则写死「以 … 开头」——
`src-tauri/src/skills/mod.rs` 的 `prompt_listing()`：`（用户消息以 /<name> 开头 = 点名该技能…）`；
`src-tauri/src/core/prompt.rs` 的 `CORE_PROMPT`：`消息以 $<role> 开头 = 用户点名内置子代理…`。
UI 放宽到句中只会造出「以为点名了、其实没点名」的坑（提示词软约束、无代码强制）。要真正支持句中点名，
得先改提示词契约（另案）。`@` 没有模型侧协议（纯文本提示），可以放心放宽。

## 3. 实现要点

- **光标是唯一锚点**：`caretRef`（供事件回调读即时值）与 `caret` state（供渲染期复验）在 `onChange`（事件里的 `selectionStart`）、`onSelect` / `onClick` / `onKeyUp`（点击与方向键移动光标不过 `onChange`）以及程序化插入处同步；
  判定与回填都走 `clampCaret` 校正越界值。
- **触发片段进状态 + 渲染期复验**：`trigger: TriggerHit | null` 由 `onInputChange` 写入；真正生效的是 `hit = trigger && trigger.end === caret ? trigger : null`——
  光标被方向键/点击移走后，菜单随之收起，不会按过期片段回填。菜单开合 = `hit.kind` 匹配 **且** 候选非空（不再从整段 `text` 派生 `/^\/(\S*)$/`）。
- **外部改写作废**：`fill` / `insert` / 历史召回 / 队列编辑 / 发送清空这些**不经过 `onChange`** 的文本链路，统一回调 `onTextReplaced(len)`（作废触发片段 + 把光标收敛到新位置）。
- **回填现算片段**：`useComposerMentions` 不再自己改文本，改为调用 Composer 的 `replaceFragment(insert)`——
  内部按「当前文本 + 当前光标」重新 `detectTrigger`（**不信任**上一次 onChange 的快照，否则方向键把光标移到片段之前时会拼出重复正文），
  把 `[start, caret)` 换成 `insert`，**光标之后的正文一字不改**，随后把 DOM 光标推到插入内容之后。
  空插入（提及选文件 → 转 chip）会顺手吃掉片段前的**整段**空白，避免正文留个尾空格。
- **`+` 菜单的「插入 @ / $ / /」改为插到光标处**（原来追加到末尾），与光标前的字符用空格隔开，并按新片段就地判定是否开菜单。

## 4. 行为边界与已接受的代价

1. **菜单仍固定在输入框上方**（`topLeft`），不跟随光标——`@` 现在能在正文中间弹菜单，位置不动。
2. **`/`、`$` 在正文中间静默无反应**（不弹菜单、不给提示）：门槛来自模型侧契约，文档写清即可。
3. **光标停在开头片段中间时 `Enter` = 选中而不是发送**（键盘语义保持「菜单开着 Enter=选中」）：
   例如 `/re|po-index 分析 X` 按 Enter 会回填技能名，把 `/re` 替换掉、后文留在原位。用户可用 Esc 或方向键移开。
4. 片段判定含**全角空格**（`\s` 含 U+3000）：中日文排版里全角空格后同样视为片段起点。
5. 触发判定不再读整段文本，因此「光标在触发片段内、但片段后面还有同一行的字」也会弹菜单——这正是修好的诉求。
6. **光标被移到片段之前时**：菜单收起，`Enter` 回到「发送」语义（正文一字不改）；`+` 菜单的插入点随光标走，不会插到过期偏移。

## 5. 验证

- 纯函数：`ui/src/__tests__/composer.triggers.test.ts`（13 例：光标校正、片段切分、三个触发符的门禁、空查询、空格收起、越界）。
- 集成（真实 Composer 渲染）：`ui/src/__tests__/composer.trigger-caret.test.tsx`（10 例：句中/开头 `@` 拉起、邮箱不误弹、回填只替换片段且后文不动、`/` `$` 句中静默、空格后收起、片段中间 Enter=选中、`+` 菜单插到光标处并就地打开菜单）。
- 回归：既有 `composer.per-tab` / `composer.history` / `composer.fileref` / `composer.paste` 全绿（happy-dom 在 `fireEvent.change` 时会把光标置到文末，与真人继续打字一致，故旧用例无需改写）。
- 手动：`pnpm tauri dev` →
  ① 先写一句正文，再把光标移到句中打 `@` → 菜单出现、选中后只替换该片段、后半句不动；
  ② 在消息开头打 `/` 或 `$`（后面已有正文）→ 菜单出现；
  ③ 在正文中间打 `/` 或 `$` → 无反应；
  ④ 点 `+` 菜单里的「使用 @/$//」→ 插到光标处；
  ⑤ 方向键/鼠标移动光标后再打 `@` → 菜单按新光标位置判定；
  ⑥ 拉出 `@` 菜单后用 `←` 把光标移到片段之前 → 菜单收起，按 `Enter` 正常发送、正文不被重复。

## 6. 审查纪要（code-reviewer）

首轮结论：**需返工（2 🔴）**，均已修复并有测试守护：

| 级别 | 问题 | 处置 |
|---|---|---|
| 🔴 | `replaceFragment` 信任上一次 `onChange` 的 `trigger` 快照：方向键把光标移到片段**之前**时 `start > caret`，`text.slice(caret)` 会把中间那段重复拼接（正文被改写） | 回填时按「当前文本 + 当前光标」**现算** `detectTrigger`，并 `Math.min(start, caret)` 保护；新增「方向键移到片段前 → Enter 正常发送」用例 |
| 🔴 | 外部链路（`fill` / 历史召回 / 队列编辑 / 发送清空）改写文本后 `trigger` 不失效，菜单悬挂在旧位置且可能按过期片段回填 | 引入 `onTextReplaced(len)`：这两个 hook 与 Composer 的三条内部链路统一调用（作废 trigger + 收敛光标）；另在渲染期用 `trigger.end === caret` 复验 |
| 🟡 | `caretRef` 不跟随外部改写同步，`+` 菜单会插到过期偏移 | 同上（`onTextReplaced` 同步光标） |
| 🟡 | `caretRef.current \|\| v.length` 的 falsy 陷阱（光标 0 是合法值） | 改为「事件给了 cursor 就用、没给就按文末」 |
| 🟡 | 空插入只吃**一个**空格，与注释/文档「吃掉那段空白」不符 | 改为 while 吃掉整段 `[ \t]` |
| 🟡 | `docs/slash-skills-and-dollar-agents.md` 仍写「`@` 尾片段」 | 已改为「`@` 光标前片段」 |
| 🟡 | 负例断言偏弱（只等 30ms 看 DOM）、缺回归用例 | 负例补断言 IPC 未被调用；新增方向键移光标、菜单开着时 fill、句中插 `/` 静默、全角空格/制表符/CRLF/emoji 用例 |
