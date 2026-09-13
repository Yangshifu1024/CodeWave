# CodeWave · 10 质量收尾 + 功能补全 批次报告（A/B 组任务）

> 日期：2026-08-30
> 范围：上轮规划 A 组（质量收尾 A1–A3）+ B 组（功能补全 B4–B8）全部任务
> 约束遵守：零 git 操作；界面改动不自动化点验，交付手动验证清单（§7）

---

## 1. 结果总览

| 项 | 状态 |
|---|---|
| `cargo test`（src-tauri/） | ✅ **147 passed / 0 failed / 0 warning**（137 基线 → 147；另有 1 个 `#[ignore]` 真实 E2E 显式运行通过） |
| `pnpm --dir ui test` | ✅ **16/16**（8 冒烟 + 2 事件契约 + 6 新增 markdown/公式） |
| `pnpm --dir ui build` | ✅ tsc --noEmit + vite build（mermaid/katex 正确拆分为懒加载 chunk） |
| A1 双代理审查 | ✅ 完成：后端 1C+5H+6M / 前端 4H+9M+12L，High 级与低成本 Medium 全部修复 |
| A2 真实端到端 | ✅ **首次跑通真实 GLM 链路**（流式 + 工具 + 跨根，38s，§5） |
| A3 真实 MCP | ✅ stdio + streamable-http 双形态真连真调（§6） |
| B4–B8 | ✅ 全部落地（§4） |

开工时发现基线已被上一轮会话中断态破坏（`openai_chat.rs` 测试引用缺失的 `test_request()` 编译失败），先修复基线并顺带收尾了 B8 的残余（见 §4.5）。

---

## 2. A1 · 双代理深度审查（React 迁移 + 多根重构）

审查方式：两个 code-reviewer 并行（Rust 后端多根面 / 前端全部 React 源文件），均先对账 [docs/code-review-findings](./code-review-findings.md) §四/§五 已修项避免重复。前端基线实测 tsc 0 错、10/10 测试通过后出报告。

### 2.1 后端发现与修复（🔴/🟠 全修）

| 编号 | 问题 | 修复 |
|---|---|---|
| **C1** | host 层 `read_workspace_file`/`save_workspace_file`/`list_workspace_dir` 三处 `WriteRoots::new` 丢失 extra_roots → 多根项目文件树/编辑器对 extra 根**完全不可用**（报错被前端静默吞掉） | 新增唯一构造点 `session_write_roots()` 三处复用；新增 commands.rs 首个测试 `session_write_roots_include_extra`（含 Explorer 绝对路径读/写/相对回退/区外拒绝四类断言） |
| **H1** | fence 透传剥离不处理 flag-value：`sudo -u root rm` 把 `root` 当命令 → **删除黑名单静默放行** | `strip_transparency` 重写：已知取值短 flag（-u/-g/-n）连值剥离；取值不明的 flag 形态返回「无法静态判定」→ 调用方保守升级 Confirm（关审批则 Block）。新增 `adv_flag_value_prefix_variants` 对抗用例 |
| **H2** | `is_dangerous_delete` 只保护主根：AI 可 `delete` 整棵 extra 根及其 `.git` | 新增 `is_dangerous_delete_multi`：对每个根自身与根下 `.git` 同等保护；delete 工具接入 |
| **H3** | `TaskTable::remove` 只改内存并重写 json，不删文件 → **已删任务重启复活** | remove 改为删除 `projects/<id>/tasks/<task-id>.json`；TaskTable 持久化根从全局 `data_dir()` 解耦为字段（可测试）；新增 `removed_task_file_is_deleted` |
| **H4** | 计划任务在「任意会话的工作区」执行（DashMap 迭代序），且 runtime 无 roots → 围栏放行错误目录、多根任务触达不了成员目录 | `task_scope()`：按 task.project_id 查注册表取主目录+成员根快照；项目缺失→跳过执行（last_status=skipped）；自由任务→中立 data_dir。新增 `task_scope_resolves_project_roots` |
| **H5** | delete_project 级联竞态：取消等待仅 500ms，迟到的 checkpoint 把会话写回索引**复活幽灵会话** + project_log 重建已删目录 | SessionRuntime 增 `zombie` 标志：删除前置位，checkpoint/project_log 见 zombie 即跳过；等待延长 3s；删除后对 store 再做一次同项目复查兜底 |
| M1 | memory「项目级同名覆盖」实际是 append 双条目注入 | 同名条目项目级覆盖用户级 + 全局 MEMORY_LIMIT 截断；新增覆盖测试 |
| M3 | SkillIndex 全局 TTL 缓存不区分 workspace/project → 切会话技能串目录 | 缓存键加入 `workspace|project_dir` |
| M4 | git 聚合 fail-fast：一根仓库损坏拖垮整个面板 | status/diff/log 三处逐根容错（warn + 跳过） |
| M5 | fence 参数收集漏 `number` 节点（`timeout 5` 靠巧合工作、数值写目标不可见）+ 一段空 if 死代码 | 收集补 `number`；死代码删除 |
| L6 | skills 死变量重构残渣 | 清理 |

### 2.2 前端发现与修复（🟠 High 全修 + 低成本 M/L）

| 编号 | 问题 | 修复 |
|---|---|---|
| **H-1** | 流式性能风暴：全消息列表每 delta 帧重跑 markdown 解析 + diff 计算，无任何 memo（相对 Vue 细粒度响应式的迁移回退） | `AssistantMessage`/`UserMessage`/`ToolCallCard` 包 `React.memo`（immer 结构共享保证未变条目引用稳定）；markdown 结果按文本缓存（流式中的条目不进缓存防前缀污染）；`editFiles`/`prettyJson` 折叠态零开销 |
| **H-2** | 文件树点目录即弹系统 alert（后端 read 目录必报错）——资源管理器基本交互受损 | onSelect 判 `isLeaf === false` 直接返回（只展开不打开） |
| **H-3** | deleteProject 级联关 Tab 按 workspace 目录匹配：误关恰好选了项目目录的自由会话、漏关目录已变更的项目会话 | 改按现成的 `tab.projectId === id` 过滤 |
| **H-4** | [docs/code-review-findings](./code-review-findings.md) §四 已修项「openSession 错误捕获」在迁移中丢失：loadSession 失败 → unhandled rejection 无反馈 | catch + toast |
| M-1 | 关 Tab 后迟到事件重建状态桶（9 个 handler），最坏产生永远无人能答复的幽灵审批桶 | 全部改为未知会话直接 return（与 run:error 既有写法对齐） |
| M-2 | 运行中按 Enter：输入框已清空但消息被静默丢弃 | `send()` 返回受理 boolean，Composer 成功受理才清空 |
| M-3 | ProjectNav 在 useMemo 里做副作用（渲染期 setState + IPC） | 改 useEffect |
| M-4 | MCP ready 判断把 Error newtype 序列化的对象也计为就绪 | 只认 `state === "ready"` |
| M-5 | bindEvents 返回的 unlisten 被丢弃（HMR/重挂载叠加重复 handler） | useEffect 返回清理函数 + cancelled 竞态保护 |
| M-6 | openProjectSession/openFreeSession 无防抖无错误捕获 | 复用 loading 守卫 + catch toast |
| M-8 | 构建脚本无类型检查（与 AGENTS.md「build = type check + vite build」不符） | `"build": "tsc --noEmit && vite build"` |
| M-9 | `service:update` handler 空壳：服务退出/被停后工具卡恒显「运行中」 | 按 payload（removed/id/tail/exited）更新对应 ToolView 的 data.tail |
| L-2 | AskPanel selected/notes 跨 ask 残留（继承 Vue L4） | askId 变化时重置 |
| L-5 | subs 跨 run 累积：子代理卡永不消失 | send() 时重置 subs |
| — | 审查 M-7「session_running 前端死代码」 | 即本轮 B5 的接线（报告基于改动前快照），已完成 |

### 2.3 审查遗留（登记待后续迭代，均为 🟡/🔵）

- 后端 M2：AGENTS.md/CODEWAVE.md/CODEGRAPH 等项目指令文件只扫主目录（extra 根的规则不注入）——涉及提示词结构与 cache 断点，需单独设计
- 后端 M6：build_stream_request 每步重复读注册表/记忆（与 [docs/code-review-findings](./code-review-findings.md) M11 cache-first 快照同方向）
- 后端 L1 相对路径跨根歧义静默主目录胜出、L2 roots/extra_roots 双份事实、L3 `<project>` 段格式串与 context.rs 重复、L4 索引 LRU 淘汰后项目会话降级、L5 save_project 后端零校验、L7 Windows 路径无 CI 验证
- 前端 M-7 已修（见上）；M-10 Escape 作用域残留（antd 静态 confirm 场景）、L-1/L-3（`/` 前缀回车语义）、L-4 key 用索引、L-6 GitDiffModal 无 catch、L-7 SettingsModal 原地改 state、L-8 死代码 `st()`/恒等三元、L-9 dirName 五处重复、L-10 冒烟 fixture 缺 project_id/roots、L-11 hljs 全量进主 chunk、L-12 窄选择器/smooth scroll
- 测试盲区：fence 数值写目标对抗用例、delete_project 命令级级联竞态用例、mcp 项目级覆盖用例（config merging 现仅 user vs 代码仓）

---

## 3. B 组功能补全明细

### 3.1 B4 · mermaid/katex 渲染（P1 历史欠账清偿）

- 依赖：`mermaid@11` `katex@0.18`（均动态 import，不进主 chunk；构建产物 mermaid.core 679KB / katex 261KB 独立 chunk）
- 管线：`markdown.ts` 产出占位 DOM（```mermaid / ```math|katex|latex|tex 围栏 + 行内 `$...$` + 块级 `$$...$$`）→ `diagrams.ts` 的 `upgradeDiagrams()` 懒加载渲染替换
  - 行内/块级公式规则移植 markdown-it-katex 定界算法，改用 KaTeX auto-render 语义（闭符前禁空白而非禁数字——否则 `$E=mc^2$` 全失效；金额场景由「开符后禁空白」保护）
  - 渲染结果按内容缓存（FIFO 240）：流式期间每帧重复升级都是缓存命中，不重复触发异步渲染
  - mermaid `securityLevel: "strict"`；主题跟随 `html.dark`（初始化时定）；katex `throwOnError:false`，非法 TeX 回退原文展示
- ChatMessages 吸底逻辑与升级合并：图表改变高度后在升级完成后再吸底
- 安全面：占位符只存转义原文；katex 不开 trust；mermaid strict；`html:false` 不变
- 新增 `markdown.math.test.ts` 6 用例（行内/块级/围栏/金额不误判/`\$` escape/katex 升级与回退）

### 3.2 B5 · 重开会话恢复 running 态（前端 M4 收口）

- 后端新增 `session_running` IPC 命令（读 runtime `running` 原子位）
- `openSession` 恢复转录后异步查询：运行中 → `markRunning` 恢复 running 态（Esc 可停、Composer 阻止双发、导航转圈）；配合 M-1 修复（关 Tab 迟到 delta 丢弃、重开由原 Channel 续写新条目）

### 3.3 B6 · 子代理/计划任务 usage 入 stats（L10 清偿）

- `UsageRecord` 增 `kind`（main/sub/task）；`DailyStats` 增 `by_kind`（serde default 兼容旧文件）；flush 合并同步处理
- 三处接入：run_chat（main）、subagent drive 结束（sub，挂父会话名下）、run_task_agent 签名改为返回 usage → scheduler 记录（task）
- 统计面板在有子代理/任务用量时展示「来源：主会话 X · 子代理 Y · 计划任务 Z」拆分行
- stats 测试补 by_kind 断言

### 3.4 B7 · 任务日志落项目目录

- 新增 `task_log()`：追加写 `projects/<project_id>/logs/<task-id>.log`（触发/完成含完整汇报正文/失败/跳过四种行，时间戳前缀）；自由会话任务跳过
- 新增 `task_log_appends_under_project_dir` 测试

### 3.5 B8 · L13 协议细节（收尾上一轮中断的工作）

- 现状核查：anthropic 空 key 不发 `x-api-key`、openai_chat `max_completion_tokens`（o1/o3/o4/gpt-5）、Responses `store:false` 代码均已就位；但 openai_chat 两个新测试引用不存在的 `test_request()` 导致**基线编译损坏**
- 修复：补 `test_request()` 帮助函数并收敛 `request_body_shape` 复用；Responses body 测试补 `store:false`/`max_output_tokens` 断言

---

## 4. A2 · 真实端到端验证（首次跑通）

新增 `src/core/e2e_glm.rs`：`#[ignore]` 真实 E2E，不进常规基线（不花真 token）。

```bash
cargo test e2e_real_glm -- --ignored --nocapture
```

本轮实测（本机已配置 `glm-5.3-flash` @ open.bigmodel.cn anthropic 端点，key 走系统钥匙串）：

- 真实流式 run **38.29s** 正常收尾
- 模型真实调用 create 工具 → `hello.txt` 落盘且内容正确
- 模型按绝对路径跨第二目录 read 成功（多根语义实战）
- 最终汇报正确引用两个文件内容
- 验证面覆盖：anthropic 协议流式解析、工具批执行、跨根围栏放行、running 复位、统计记录

支持 `CODEWAVE_E2E_KEY` 环境变量绕过钥匙串（供无 GUI 授权的 CI/终端环境）。

---

## 5. A3 · 真实 MCP server 接入验证

- 新增 `scripts/mcp-test-server.mjs`：官方 `@modelcontextprotocol/sdk` 实现的真实 MCP server（echo/add 工具），stdio 与 streamable-http（stateless JSON 响应形态）双模式
- Rust 侧两个集成测试（`mcp::tests::real_*`，无 node 环境自动跳过）：
  - `real_stdio_server_connect_list_call`：spawn node → initialize → list_tools（schema 归一化断言）→ **真实调用 echo 往返**断言 `echo: hello codewave`
  - `real_streamable_http_server_connect_list_call`：起 http server（随机端口）→ 就绪探测 → streamable-http 连接 → **真实调用 add** 断言 `add(6,7)=13`
- 实测 2/2 通过；至此 [docs/p1-p2-implementation-report](./p1-p2-implementation-report.md) §6 的「真实 MCP server 接入空缺」补上

---

## 6. 验证基线

| 套件 | 结果 |
|---|---|
| `cargo test` | 147 通过 / 0 失败 / 0 warning（含 6 个本轮新增回归测试 + 2 个真实 MCP 测试；e2e_real_glm 默认 ignore） |
| `pnpm --dir ui test` | 16/16（冒烟 8 + 契约 2 + markdown/公式 6） |
| `pnpm --dir ui build` | tsc 0 错误 + vite 构建通过（mermaid/katex chunk 正确拆分） |
| 真实 GLM E2E | 显式运行通过（§4） |
| **打包（MVP 收口追加）** | ✅ `pnpm tauri build --debug` 重新打包通过（本轮改动后首次）：产出 `CodeWave.app` + dmg，无 error/warning；重启冒烟与 §7 手动清单待用户执行 |

**MVP 评估结论（2026-08-30，[docs/quality-and-feature-batch-report](./quality-and-feature-batch-report.md) 收口时）**：口径一（用户本机自用）——打包通过后即达到"可开始日常使用"状态，剩余验收全部是人工项（§7 清单 + dogfooding）；口径二（分发陌生用户）——尚不具备，缺口 = CI 真跑（ci.yml 已修正路径但从未运行，Windows 链路未知）、签名/updater、fence L14 与 SSRF rebinding 补强、体验毛刺清理。

**ci.yml 修正（同轮）**：`--dir frontend` → `--dir ui`（迁移遗留）；补前端测试步骤；pnpm workspace（root+ui）与 `cargo test --workspace`（单包）布局核对有效。CI 真跑需用户 push（AI 零 git 操作）。

## 7. 手动验证清单（界面相关，需 GUI）

1. **B4 公式/图表**：会话中让 AI 输出一段 `$$...$$` 公式与一个 ```mermaid 流程图 → 公式渲染为数学排版、流程图渲染为 SVG；输出非法 mermaid 时显示原始代码 + 错误信息
2. **B5 running 恢复**：长任务运行中关闭该 Tab（进程不退出）→ 左侧导航重新打开该会话 → 输入区应显示「停止」按钮（running 态恢复），Esc 可取消
3. **B6 统计来源**：跑一次带子代理的会话 + 触发一次计划任务 → 统计面板底部出现「来源：…」拆分行
4. **B7 任务日志**：建一个挂项目的 `every:1 h` 任务并手动等触发（或临时 once）→ `~/.codewave/projects/<id>/logs/<task-id>.log` 出现触发/完成/完整汇报
5. **C1 回归（多根文件面）**：多目录项目的文件树展开第二根 → 能列出/打开/保存文件（此前永远为空）
6. **审查修复抽查**：删除项目后其任务不复活（重启验证）；git 面板在某根仓库损坏时其余根仍显示
7. **A2 完整体感**：真实双仓项目里让 AI 做「问整体」类任务（提示词语义实战检验）

## 8. 遗留与建议（下一轮候选）

- C 组任务未动：三平台 CI（Windows 兼容仍是未知数，含本轮 pathutil 的多根 delete 保护建议上 CI 验证）、[docs/technical-design](./technical-design.md) 的 D3 标注、updater 启用说明
- §2.3 审查遗留清单（建议先做：后端 M2 项目指令文件多根注入、delete_project 级联竞态测试盲区）
- 子代理步数进度仍为 800ms 轮询近似（[docs/p1-p2-implementation-report](./p1-p2-implementation-report.md) §5 偏差 3），如需精确逐步钩子需 drive_agent 事件化
