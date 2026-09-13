# Shell 路径回显 + lightweight 批准门放宽

> 日期：2026-09-11 · 类型：界面 + 行为批次 · 涉及：`ui/` 前端 4 文件 + `src-tauri/` 后端 3 文件，IPC / config schema / 事件面零变化

## 一、需求分析（product-manager 产出摘要）

两条独立需求：

1. **Shell 路径回显**（UI 增强）：设置弹窗通用页的 Shell 下拉选中后，在下拉框右侧回显该 shell 可执行文件绝对路径。用户故事：配置 shell 的用户需确认实际执行的可执行文件位置（同名 shell 多安装位置场景）；「自动」用户需知道自动探测落在哪个 shell；WSL 等 `path=null` 需明确占位而非空白。
2. **lightweight 批准门放宽**（行为）：移除 ask 批准门 G2 豁免通道「todos ≤ 3」硬校验。动机：模型为过门被迫把方案压缩成 ≤3 条粗粒度 todo，损失计划可读性；条数与计划质量无必然关系，「todos 非空」（G3）已足够兜底。

边界与非目标：不改后端探测逻辑与 `ShellInfo` 结构；不显示 kind/版本/打开目录等扩展信息；不引入 todo 质量校验、不改 skipAnalysis 口令机制与判定顺序；仅设置弹窗通用页回显。

## 二、改动清单

### A. Shell 路径回显（前端）

| 文件 | 改动 |
|---|---|
| `ui/src/features/panels/SettingsModal.tsx` | 新增 `ShellDisplay` 三态判别联合 + `resolveShellDisplay()` 纯函数：固定选择命中探测项且 `path != null` → 显示绝对路径；`selection == null`（自动）→ 取探测列表 `auto:true` 项的 path（与「自动（默认：X）」标注同源，即执行时实际所用 shell）；所选 shell 存在但 `path === null`（如 WSL）→ 占位文案；探测失败 / 所选 shell 已卸载 / auto 探测项缺失 → 不显示（既有警示文案兜底，不重复提示）。Shell Form.Item 内容区改 flex 行：Select 260px（`flexShrink:0`）+ 右侧 `<code>` 占满剩余宽度，`var(--ws-font-mono)` mono token、`var(--ws-dim)` 次要色、nowrap + ellipsis 超长省略、`title` 悬停看完整路径。不发任何额外 IPC |
| `ui/src/i18n/zh-CN.ts` / `en-US.ts` | 新增 `shellNoPath`：zh「该 shell 无固定可执行文件路径（如 WSL 发行版）」/ en "This shell has no fixed executable path (e.g. WSL distros)" |
| `ui/src/__tests__/shell.settings.test.tsx` | 探测失败 / 已卸载用例补「不回显路径」（`not.toContain("bash.exe")`）；新增「Shell 路径回显」describe：auto 回显 Git Bash 绝对路径 → 切 PowerShell 路径跟随 → 切 WSL 显示占位且不残留上一路径；`code[title]` 悬停完整路径断言 |

### B. lightweight 批准门放宽（后端）

| 文件 | 改动 |
|---|---|
| `src-tauri/src/tools/ask/tool.rs` | `plan_approval_gate` 删除 `todo_count <= 3` 判断与 Y4 超限拒绝分支，`lightweight=true` 直接放行；**不变项**：G3「todos 非空」硬校验保留在函数前段、skipAnalysis 口令命中机制与判定顺序（skip 先判）、非 lightweight 时分析产物要求。L522 拒绝文案去掉「（要求 todos ≤3 条）」括注；schema description 与 `Args.lightweight` 字段注释同步 |
| `src-tauri/src/tools/ask/mod.rs` | 模块头注释同步为新语义 |
| `src-tauri/src/tools/ask/tests.rs` | `lightweight_exception_requires_small_todos` → `lightweight_exception_ignores_todo_count`：5 条 todos + lightweight 放行；空 todos + lightweight 仍 `E_PLAN_TODOS_REQUIRED`（G3 边界被测试钉死） |

## 三、验证

- `cargo test`（src-tauri）：**538 passed / 0 failed / 2 ignored**。
- `pnpm --dir ui test`：**288 passed（41 文件）**，全量两轮（token 替换后复跑）。
- `pnpm --dir ui build`：type check + vite build 通过。

## 四、代码审查（code-reviewer，7 维度）

对齐表逐项 ✅；重点核查：三态边界（shells=[] / auto 项缺失 / 已卸载）一致性 ✅、flex 收缩链（`minWidth:0` 覆盖 flex item min-width:auto）无溢出 ✅、`plan_approval_gate` 全仓唯一非测试调用点在 `switch && (Plan || arch_flag)` 分支内，ConfirmEach 普通应答不过 gate，无误放行路径 ✅、测试断言非恒真 ✅、i18n 双语完整 ✅、字体走 token / 无彩色 accent / 契约面零变化 ✅。

**结论：通过（无 🔴）**。两项 🟡 已顺手修复：

1. `--ant-color-text-tertiary`（antd 内部 cssVar 直消费）→ 改用 ThemeBridge 桥接的 `var(--ws-dim)`，回归项目 token 惯例（全量测试复跑全绿）。
2. [plan-mode-workflow.md](./plan-mode-workflow.md) §7.2 例外通道描述同步为新语义（历史调研报告 builtin-tools-source-comparison / run-queue-and-ask-revamp 中的旧描述不回改，以本文档为准）。

## 五、手动验证清单（GUI 不做自动点验）

1. 确认无 dev 实例与打包版并存后 `pnpm tauri dev`（仓库根）。
2. 顶栏「设置」→ 通用页签 → Shell 行：默认「自动（默认：X）」时右侧显示 X 的绝对路径（mono 字体、次要色）。
3. 切 Git Bash / PowerShell / CMD：右侧路径跟随切换；悬停路径可见完整 title（深路径截断场景）。
4. 切 WSL：显示「该 shell 无固定可执行文件路径（如 WSL 发行版）」占位。
5. 保存 → 重开设置：路径与所选 shell 一致（持久化联动正常）。
6. 中英切换：占位文案随语言切换。
7. plan 档会话发起批准：模型可携带超过 3 条 todos 的 lightweight 批准询问（不再报 E_PLAN_ANALYSIS_REQUIRED）；空 todos 批准仍被拒。
