# skill 注入的 ask 交互规范

> skill 工具加载任意技能时统一附加 `<ask-interaction-norm>` 条件式规范块：流程含「向用户提问并等待作答」轮次的技能，提问必经 ask 弹窗呈现；不涉提问的技能（如内置 repo-index / doc-convert）规范自动休眠，行为零变化。

## 背景

访谈/提问型技能（如用户级 grilling）按正文以纯 markdown 文本生成提问轮次，未走宿主 ask 工具的结构化弹窗（推荐项高亮、单选/多选、每题备注）。技能正文只是提示词，宿主无法在正文之外强制工具选择——这是机制缺口而非单技能缺陷。

## 方案

`src-tauri/src/tools/skill.rs` 的 `run()` 在 `<skill-loaded>` 闭合前（caller-context 之后）统一附加常量规范块 `ASK_INTERACTION_NORM`（英文，与 `<skill-loaded>`/`<caller-context>` 同风格）：

- **适用条件由模型语义判断**：本技能流程含「提问并等待作答」轮次才生效；无提问轮次的技能忽略该块，行为不变。
- **映射规则**：正文推荐标记（➡️）→ ask 选项 `recommended: true`（推荐项保持可见可选，不隐藏）；互斥题 `single: true`，真多选不设 `single`；开放题 → 无 `options` 的 ask 题；单轮超过 ask 上限（5 问 / 每题 6 选项）拆连续多次 ask 调用，不丢问题。
- **降级**：ask 不可用（如子代理上下文）→ 回退技能自身文本格式，不中断流程。

## 权衡

- **关键词识别被否决**：字符串匹配编码的是「形似提问」而非「应走 ask」的意图——误报（含示例问答、路径确认类技能被误触发）与漏报（措辞变体）不可控，且行为变化对用户无提示。
- **frontmatter `interaction: ask` 声明机制被搁置（YAGNI）**：声明方案需改 `skills/mod.rs` 解析 + `SkillMeta` wire 契约 + 各技能文档加声明；条件式规范零技能文档改动即覆盖全部同类技能。已知代价：提示词级软约束，极端措辞下可能偶发文本提问——若实测如此，再按需升级为声明机制。

## 相关

- ask 工具：`src-tauri/src/tools/ask/tool.rs`（questions 1–5、options ≤6、recommended/single 语义）；前端弹窗 `ui/src/features/tools/AskPanel.tsx`
- 技能注入链路：[slash-skills-and-dollar-agents](./slash-skills-and-dollar-agents.md)
