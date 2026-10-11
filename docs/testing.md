# 测试、审查与本地门禁

验证对象是当前 checkout 与当前平台。测试通过、警告数量和构建成功必须来自本次实际执行；历史交付报告不构成当前全绿证明。环境准备见 [development](./development.md)，发布验收见 [release](./release.md)。

## 开 PR 前的完整门禁

CI 的检查来源是 [lint.yml](../.github/workflows/lint.yml) 与 [test.yml](../.github/workflows/test.yml)，本地入口是 [scripts/pre-pr.mjs](../scripts/pre-pr.mjs)：

```bash
pnpm prepr
```

| 步骤 | 命令 | 工作目录 | 失败语义 |
|---|---|---|---|
| `lockfile` | `pnpm install --frozen-lockfile` | 根 | 硬失败 |
| `rust-fmt` | `cargo fmt --all -- --check` | `src-tauri/` | 硬失败 |
| `rust-clippy` | `cargo clippy --all-targets` | `src-tauri/` | 软失败，必须报告 |
| `rust-test` | `cargo test --workspace` | `src-tauri/` | 硬失败 |
| `ui-lint` | `pnpm --dir ui run lint` | 根 | 硬失败 |
| `ui-test` | `pnpm --dir ui test` | 根 | 硬失败 |
| `ui-build` | `pnpm --dir ui build` | 根 | 硬失败，包含 TypeScript 检查与 Vite 构建 |
| `scripts-test` | `node --test "scripts/**/*.test.mjs"` | 根 | 硬失败 |

- 硬步骤失败即停止，未执行的后续步骤不能记作通过；退出码、硬失败清单和软失败清单分开报告。
- clippy 是当前唯一软步骤，与 CI 的 `continue-on-error` 对齐；“退出 0”不等于 clippy 无告警。工程要求仍是不引入新 warning，不能以软门槛为理由忽略新增问题。
- `pnpm prepr --only=rust-fmt,ui-lint` 可局部检查，`--skip-soft` 可跳过 clippy；两者都不是完整门禁。
- CI 覆盖 macOS、Ubuntu、Windows；本地只覆盖当前平台。`cfg(unix)`、Windows 路径/进程、系统通知与 WebView 差异须单独说明。
- 门禁是显式命令，不自动挂 Git hooks。CI 是复核，不应成为发现格式和 lint 问题的第一道防线。

[pre-pr.test.mjs](../scripts/pre-pr.test.mjs) 将 `PRE_PR_STEPS` 与两个 workflow 的检查命令双向比对，并核对软硬集合、YAML 续行、CRLF 和 Windows 启动语义。新增 CI 检查须同时改本地清单与测试。脚本测试的 glob 必须作为一个参数交给 Node，不能假设 PowerShell 会替原生命令展开。

## 按改动面选回归

| 改动面 | 关键验证与入口 |
|---|---|
| Rust 实现 | `cargo test`/workspace 全量；新增 warning、条件编译与目标平台；模块局部用例不能取代跨层回归 |
| UI 组件或 store | `pnpm --dir ui test` + `build`；完整 App/设置挂载、状态复位、错误反馈和交互载荷 |
| IPC 与事件 | [events.contract.test.ts](../ui/src/__tests__/events.contract.test.ts)、[ipc.events.test.ts](../ui/src/__tests__/ipc.events.test.ts)、[runFrames.test.ts](../ui/src/__tests__/runFrames.test.ts)；双向事件名守卫之外另测实际帧与 payload |
| 模型流与请求 | [provider/tests_integration.rs](../src-tauri/src/provider/tests_integration.rs)、[agent/tests.rs](../src-tauri/src/core/agent/tests.rs)；TCP 分片、断流、连续角色、空响应、重试代数、请求字节稳定 |
| 存储与会话 | [sessions/](../src-tauri/src/core/sessions/)、[run.restore-toolresults.test.ts](../ui/src/__tests__/run.restore-toolresults.test.ts)；并发索引、固定锁序、坏段/残行、配对、blob GC、分页、恢复 sidecar |
| 运行时与队列 | [run.terminal-fallback.test.ts](../ui/src/__tests__/run.terminal-fallback.test.ts)、[run.queue.test.ts](../ui/src/__tests__/run.queue.test.ts)、[run.service-update.test.ts](../ui/src/__tests__/run.service-update.test.ts)；事件与快照交错、run_id、迟到回执、取消/失败暂停与成功单次续队 |
| 工具与权限 | [fence/](../src-tauri/src/safety/fence/)、[pathutil.rs](../src-tauri/src/tools/pathutil.rs)、[batch.rs](../src-tauri/src/tools/batch.rs)、[approval.rs](../src-tauri/src/safety/approval.rs)；四档、越界/symlink、取消、批次冲突、预览不授权、子代理分支 |
| MCP 与 Skills | [mcp/](../src-tauri/src/mcp/)、[skills/mod.rs](../src-tauri/src/skills/mod.rs)、[tools/skill.rs](../src-tauri/src/tools/skill.rs)；作用域覆盖、连接错误分类、非幂等禁止重放、进程树回收、缓存失效 |
| 配置与设置 | [settings.registry.test.ts](../ui/src/__tests__/settings.registry.test.ts)、[settings.page.test.tsx](../ui/src/__tests__/settings.page.test.tsx)、[projects.contract.test.ts](../ui/src/__tests__/projects.contract.test.ts)；保存载荷、默认值、掩码回填、即时字段与草稿隔离、双语键 |
| 更新器/发版脚本 | [updateCheck.test.ts](../ui/src/__tests__/updateCheck.test.ts)、[updater.modal.test.tsx](../ui/src/__tests__/updater.modal.test.tsx)、[updater.startup.test.tsx](../ui/src/__tests__/updater.startup.test.tsx)、[scripts/](../scripts/) 的 Node 测试 |

事件契约测试当前守护 handler 表 29 键；这是特定 handler 表的口径，不是全部桌面事件数量。新增可选字段仍需同步类型、serde 默认值和载荷测试，不能凭键名未变就判断兼容。

## 审查方法与不能删的回归

代码审查覆盖正确性、安全、性能、可维护性、可读性、测试覆盖和最佳实践。先复现再修复，再独立回看**真实调用点**：辅助函数有测试并不证明生产链路调用了它。

### 算术与边界审查

逐个确认单位、计数方向与端点，而不是仅搜索乘除符号：

- 升序 step 与剩余预算不可混用；零基/一基、闭区间/半开区间、首末步与重试上限分别列例。
- bytes、Unicode chars、tokens、ms、s 不能互换。`find(char)` 返回字节偏移，向前扫描要加 `char.len_utf8()`；缩进计算与剥离必须使用同一种单位。
- 先验证 clamp 下界不高于上界；乘法用 checked/saturating 或明确上限；零分母、空集合、单轮历史和极大入参都要覆盖。
- 统计合并逐字段对账，各维度使用同一 token 口径；计时速率的分子分母必须来自同一组计时样本。
- 并发上限必须原子认领（CAS/等价机制），check-then-add 不是上限保证。
- PID 缺失不能拿 0 或 1 代替；Unix 负 PID 信号有进程组/全用户语义，必须拒绝或安全回收。

现有回归入口包括 `fuzzy_multibyte_and_case_insensitive`、`indent_units_chars_consistent_for_multibyte_space`、`flush_should_merge_by_workspace_cache_fields`、`trim_keep_last_zero_single_round_no_panic`、`every_interval_parse_bounds` 和 [retry.rs](../src-tauri/src/provider/retry.rs) 的重试边界。不要把修复前的错误边界重新写成“现有行为”断言。

### 协议与并发审查

- Anthropic SSE 流内不能调用 `parser.finish()`；`anthropic_sse_line_split_across_segments` 必须模拟一行 data 被真实 TCP 分片撕裂。
- 网络重试须清空展示缓冲且防旧代数回流；每 step 的重试预算独立，不能让长 run 消耗掉后续 turn 的重试资格。
- running 清理覆盖正常、错误、取消、panic；审批与压缩等待响应取消；关 Tab 后迟到事件不得重建无主桶。
- 出网历史 repair、完整磁盘历史和 UI 恢复分别验证，不能因 Provider 要求而删除用户可见转录。
- 项目删除覆盖取消超时与迟到 checkpoint；测试应触达命令编排而不仅是文件删除函数。
- `ask`、`wait`、`suggest` 的混批策略分别断言；“所有交互工具都一样”会掩盖实际分支。
- 写入后检查既要测执行/跳过原因，也要经 `compact_for_model` 验证结论确实出现在模型侧 `data.check/checks`。

## 测试基础设施的坑

### Rust、网络与系统资源

- mock TCP server 要持有 listener 直到测试结束、读完请求头再优雅关闭写半部；直接丢弃带未读数据的 socket 在 Windows 会变成 RST，提前释放临时端口还可能被并发测试复用。`midstream_disconnect_maps_to_network` 的偶发失败不能未经调查就归为代理问题。
- 真实 keyring 是机器级资源，相关测试串行且清理自己的条目；无 Secret Service 时验证明确的明文回退分支，不伪造迁移成功。
- 定时等待用 Tokio 虚拟时钟测临界点和取消，进程退出/OS 信号等真实调度边界仍需集成测试。
- 读取本地数据、真实网络或显示通知的 ignored 测试须逐项了解前置条件与副作用；不要用 `--ignored` 无差别运行。真实通知探针会打扰用户，额度探针可能读取本机配置并访问真实端点。
- `e2e_real_glm` 已不在当前测试树中；早期真实模型一次跑通记录不是现成测试入口，也不证明当前端到端链路已验收。

### React、antd 与 happy-dom

- zustand 是模块级单例；测试结束后重置 settings/diff/tasks/stats 等开关和 run/session 状态，避免前一用例污染后一用例。
- happy-dom 需要 `matchMedia`、`ResizeObserver`、`crypto.randomUUID` 等垫片；mock invoke/listen 只验证存在不够，要保存 handler 并主动派发。
- antd 两字按钮可能插空格；文本匹配先归一空白。Dropdown 选择实际 menu item 后点击。
- `TextArea autoSize` 另建测量 textarea；用 placeholder 等稳定语义定位输入真身。
- Composer 测试应明确 `selectionStart`；点击与方向键不会经过 onChange。替换草稿同时检查 text/refs/images，不能只检查输入框文本。
- 多 Tab、迟到 IPC、重复打开、关闭重开、StrictMode effect 重放和 ask 切换都要覆盖；构建通过不能代替完整组件挂载。
- Markdown 安全用结构断言：原始 HTML 不执行、`javascript:` 不可点击、外链安全属性正确；动态图表和数学升级另测，不以字符串存在代替渲染结果。

## 已知验证边界与待核实事项

以下是源码可见的现行限制或未闭环问题，不是把旧审查清单整体搬回待办：

| 事项 | 已核实边界 / 后续验证 |
|---|---|
| Skills 禁用缓存 | `get/list` 命中 TTL 时不重判 disabled，保存/切换禁用未失效缓存；`disabled_after_load_should_invalidate_within_ttl` 仍 ignored。详见 [mcp-and-skills](./mcp-and-skills.md) |
| DNS rebinding | 工具先解析校验，reqwest 连接未固定已验证 IP；逐跳校验并不消除二次解析窗口。详见 [tools-and-safety](./tools-and-safety.md) |
| 长期会话索引 | 索引仍有 2000 条 LRU 上限；索引元数据缺失时恢复会退回单根、无项目归属。完整历史保留不等于元数据无限保留，见 [sessions-and-projects](./sessions-and-projects.md) |
| 项目提示与命令 cwd | 运行提示中的 temps 描述和工具实际默认 project_dir 存在差异；应以执行代码为准并独立修正/测试，见 [runtime-and-scheduling](./runtime-and-scheduling.md) |
| 终态事件丢失 | 快照对账修复了展示与续队能力，不证明底层事件丢失原因已定位；需保留日志与 run 身份关联证据 |
| updater 进度 | store 降频回归已覆盖；没有由单测证明所有真机投递瓶颈都消失。全速仍卡时用限速对照定位 IPC 与渲染侧 |
| 发布/平台能力 | 签名、公证、安装、更新重启和不同目标平台必须由实际产物/日志验证，历史全绿或 workflow 配置不等于本次成功 |

## 交付验证结论的写法

记录命令、工作目录、平台、退出码与失败/跳过原因；使用当前输出中的测试数量，不复制历史数字。对未跑项目明确说明阻塞；环境失败与产品缺陷分开，但都不能报作通过。

界面变更交付人工验证清单，不做 GUI 自动点验。清单应写前置状态、操作、可观察结果与失败分支；优先覆盖本次改动的跨 Tab、并发、重启、取消和持久化路径。发布专用清单集中在 [release](./release.md)。
