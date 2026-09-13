# command 输出链 ANSI 乱码与「正在运行？」锚点缺陷——根因分析与修复方案（已批准，待实施）

> 状态：**方案已批准，实施延后**（2025 年会话批准记录：用户批复「保存文档，稍后实施」）。
> 来源：用户运行 `pnpm --dir ui test` 后报告终端输出乱码 + 工具卡标题「正在运行？？ ▸」异常（附截图）。

## 一、现象

1. **终端乱码**：命令输出里大量 `␛[32m✓␛[39m` 形态的原始转义序列文本，`✓` 等多字节字符偶发裂成 `�?`；vitest 的 stderr 警告行（antd List deprecation）同样带色透传。
2. **工具卡标题异常**：流式期间工具卡头部渲染为「正在运行？？ ▸」，正常应为「正在运行 命令」。

## 二、根因分析（已逐层取证）

### ① ANSI 转义序列透传（非 CodeWave 注入，但采集链零清洗）

- vitest（Chalk 系）检测到「支持颜色的管道」即坚持输出 ANSI 颜色码。**取证**：同一条 vitest 命令在管道下捕获到 43 处 `ESC[` 序列；设置 `NO_COLOR=1` / `FORCE_COLOR=0` 后为 0 处。
- 后端 `command` 工具子进程经 `Stdio::piped()` + Windows `CREATE_NO_WINDOW` 启动（`src-tauri/src/tools/command/tool.rs:311`），不伪造 TTY——颜色是子进程自己加的，CodeWave 侧无法从环境层面杜绝。
- 采集链 `pump()` 以 4096 字节分块 `String::from_utf8_lossy` 直接堆进 `Collector.buf`（tool.rs:344-363），**无任何控制序列清洗**，最终原样进入 `model_tail` → `ToolProgress` 帧 / outcome。

### ② U+FFFD（？）替代符：多字节字符被分块拦腰切断

- `pump()` 逐 chunk 独立解码，`✓`（E2 9C 93）、中文等 3 字节 UTF-8 序列若跨越 4096 边界，前半截成为非法序列，`from_utf8_lossy` 产出 U+FFFD——截图里「正在运行？？」的「？」与此同源（流式 progress 帧尾部的裂字符）。
- `service.rs` 的 `RingLog::push` 按字节入环、`tail()` 整体一次性 `from_utf8_lossy`，无此问题，但同样不清洗 ANSI。

### ③ 「正在运行？」标题：前端锚点回填链缺陷

- `Frame::ToolProgress` 先到时，`runFrames.ts:106-110` `ensureToolAnchorIm` 以 `batch:index` 合成锚点，`tool: "?"`；`ToolCallCard` 的 `verbLabel` 拼出 `正在运行 ?`。
- `tool:result` 事件后到（`run.ts:275-305` `onToolResult`）只回填 `tool.tool / status / outcome / argsPreview / durationMs`，**`progressTail` 从不清空**——结果落定后残留的 ANSI 乱码 tail 继续污染卡片（仅在 `status === "running"` 时展示，故最终卡不显示，但流式期间持续可见）。
- 截图时机为流式期间：「？？」= 锚点名未回填的 `"?"` + progress tail 里的 U+FFFD/ANSI 残渣叠加。

## 三、修复方案（批准稿，3 文件 + 测试）

### 1. 后端：`src-tauri/src/tools/command/tool.rs`（+ 复用至 `service.rs`）

新增共享模块（建议 `src-tauri/src/tools/sanitize.rs`）：

- **ANSI/控制序列剥离**：ECMA-48 CSI 序列（`ESC [ ... 终结字节 @-~`）、OSC 序列、裸 `\r`（行内回车重绘，如进度条）剔除；保留 `\n` 与常规可打印字符。
- **分块边界安全解码**：维护跨 chunk 的不完整 UTF-8 尾部缓存——块尾若是被截断的多字节序列前缀（合法前导字节 + 不完整续字节），缓存至下一块拼接解码，**不再产出 U+FFFD**；非法字节仍按 lossy 处理兜底。
- `command` 的 `pump()` 与 `service` 的 `drain()` 接入同一清洗器；`model_tail` / signal line 逻辑零变化。

### 2. 前端：`ui/src/stores/run.ts`

`onToolResult` 结果落定时 `tool.progressTail = ""`（结果卡 `tail` 已含完整输出，progress 残留无意义且是乱码载体）。主会话与子代理流（[docs/subagent-interaction-drawer](./subagent-interaction-drawer.md) 分支）两处同步。

### 3. 前端：`ui/src/features/tools/ToolCallCard.tsx`（或 runFrames 锚点）

`tool === "?"` 时标题降级为中性文案（不渲染孤悬的 `?`），如显示 `tools.running` 原文；`tool:result` 回填后自然恢复真实工具名。

### 4. 测试

- 后端：ANSI 剥离单测（CSI/OSC/`\r`/混合中文多字节跨块用例）；接入点冒烟（echo 带色输出 → outcome 无 `ESC`）。
- 前端：`onToolResult` 清空 `progressTail` 断言；`tool="?"` 中性标题渲染断言。

### 5. 验证基线

`cargo test`（基线 393/0）+ `pnpm --dir ui test`（基线 255）+ `pnpm --dir ui build` 全绿。

## 四、明确不做（Non-Goals）

- **不给子进程强设 `NO_COLOR` / `FORCE_COLOR`**：全局改环境变量会波及用户显式依赖颜色的命令输出语义（如 `git diff --color=always` 的消费方），且 TUI 类程序（vim/htop）本就不该在管道下运行；清洗比抑制更稳。
- **不改 27 键事件面 / Frame 协议**：纯输出内容治理，wire 契约零变化。

## 五、关联

- 前置批次：[docs/tool-optimizations-port](./tool-optimizations-port.md)（工具输出瘦身 / 凡截断必落盘）、[docs/subagent-interaction-drawer](./subagent-interaction-drawer.md)（子代理流式帧信封）。
- 遗留相关：`progressTail` 残留清理与 [docs/tool-card-multi-file-summary](./tool-card-multi-file-summary.md)「入参截断后头部空白」同属工具卡回填链 hygiene，可一并回归。
