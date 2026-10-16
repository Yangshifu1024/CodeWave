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

### CI 并行与耗时观测

- [test.yml](../.github/workflows/test.yml) 的前端 Vitest/Node 脚本矩阵与 Rust 矩阵独立并行，均保留 macOS、Ubuntu、Windows。Rust job 保留 Node、pnpm workspace 与前端 build，满足真实 MCP server fixture 和 Tauri `frontendDist`；没有用占位资源替代构建，也没有减少 Rust 单测、集成测试、doctest 或改变 ignored 语义。
- 原有 `Test (macos-14)`、`Test (ubuntu-24.04)`、`Test (windows-2022)` 名称由 `always()` 汇总 job 保留。每个汇总都要求**两个完整矩阵**成功，任一平台失败、取消、跳过都不通过；其名称不再表示仅检查该平台。Rust/UI lint 门槛不变。分支保护配置仍需仓库管理员核对，名称保留不是配置已核实的证明。
- Rust job ID 仍为 `test`，保留 rust-cache 默认 job 隔离；不与 clippy/release 共享 target key，不改 stable、Cargo profile 或缓存权限。缓存摘要只报告 exact hit；`false` 不能区分部分恢复与完全 miss，具体 key、恢复/保存大小与耗时以 action 日志为准。PR merge-ref 的缓存作用域与 main 不同，详见 [GitHub 缓存规则](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching) 与 [rust-cache](https://github.com/Swatinem/rust-cache)。
- [metrics.mjs](../scripts/ci/metrics.mjs) 流式运行原命令一次，保留退出码；安装、前端测试/build 与 Rust 测试有 wall time。Rust 从 Cargo 原始输出读取 test-profile 编译时长及各测试套件时长/通过/失败/ignored 数，不二次运行测试。套件时长之和不含 Cargo 开销、doctest 编译；缺少指标显示为缺失，不能算作 0 秒。失败时也尽力写摘要，runner 强制终止可能无最终摘要。
- job summary 记录 runner/image、CPU/RAM、工具版本与缓存 exact hit，不输出整个环境或 secrets。前端 job 的 Rust/CMake 版本只是镜像探测值，不代表该 job 执行过后端检查。
- [CI 回归测试](../scripts/ci/workflow.test.mjs) 守护矩阵覆盖、汇总失败语义与依赖分组；[CMake 行为测试](../scripts/ci/ensure-cmake.test.mjs) 在有 PowerShell 时执行，Windows 缺 PowerShell 为失败，其他平台无 PowerShell 明确跳过。Linux 成功不能替代 Windows 实际探测/安装链路验证。

比较优化效果时，先记录同一依赖树、工具链和 runner image 的 main 基线，再在获准发布的 PR 上记录第一次与后续热缓存运行。首次运行可能已恢复 main 缓存，必须按日志标注 exact/partial/miss，不能把“首次”直接写成“冷缓存”。若必须做严格冷缓存 A/B，应另获授权后使用隔离缓存 key，不清空共享缓存。两次都记录排队、整个 workflow 关键路径、各 job/步骤、Rust 编译/套件、缓存传输和总 runner 分钟，并核对通过/ignored/doctest 数。拆分增加 checkout/install 和 runner 启动开销，账户并发不足可能变成排队；只有真实数据能判断 wall-clock 收益，不能只报单步骤节省。

## 可靠性修复自动验证记录（2026-10-12）

本记录仅对应 [`6c3a205e312778bffce39c9df60935d55247f40f`（v0.11.0）](https://github.com/Yangshifu1024/CodeWave/commit/6c3a205e312778bffce39c9df60935d55247f40f)，不承诺后续 checkout 沿用此结果。R01–R11 的实现、实际回归入口及剩余人工验收见[专项方案](./reliability-remediation-plan.md#实施与验收状态2026-10-12)。

- [三平台测试 workflow](https://github.com/Yangshifu1024/CodeWave/actions/runs/38255537448)：三个 Backend、三个 Frontend 及三个汇总 job 均成功。Backend 包含前端生产构建及 `src-tauri/` 中 `cargo test --workspace`；Frontend 包含 Vitest 和 Node 脚本测试。
- [lint workflow](https://github.com/Yangshifu1024/CodeWave/actions/runs/38255537545)：Rust fmt/clippy 与 UI eslint 步骤成功；此结论不等于 clippy 无 warning。
- 本地完整 `pnpm prepr` 未执行完成：云环境缺少 GTK/GLib/WebKit 等系统依赖。此次发版经维护者明确批准，以最终提交的三平台 CI 补足完整后端验证；不是修改默认本地门禁，也不能写成本地 prepr 成功。

| 实际日志 | Rust 主测试套件 | 说明 |
|---|---|---|
| [macOS](https://github.com/Yangshifu1024/CodeWave/actions/runs/38255537448/job/114832523184) | 1414 passed / 0 failed / 3 ignored | 后续二进制测试与 doctest 各 0 项 |
| [Linux](https://github.com/Yangshifu1024/CodeWave/actions/runs/38255537448/job/114832523311) | 1414 passed / 0 failed / 2 ignored | 后续二进制测试与 doctest 各 0 项 |
| [Windows](https://github.com/Yangshifu1024/CodeWave/actions/runs/38255537448/job/114832523341) | 1397 passed / 0 failed / 3 ignored | 后续二进制测试与 doctest 各 0 项 |

[Windows 前端日志](https://github.com/Yangshifu1024/CodeWave/actions/runs/38255537448/job/114832523217)明确记录 122 个文件、1586 项 Vitest 通过；Node 脚本 34 passed / 0 failed / 1 skipped，跳过项为 `cancellation cannot succeed even when the child handles the signal and exits zero`（POSIX signal handling regression）。不同平台条件编译和跳过项不得合并成“所有测试均执行”。

Rust ignored 的完整名称：
- 三平台共有：`core::quota::tests::live_probe_opencode_go_usage`（真实账户/网络探针）、`tools::skill::tests::disabled_after_load_should_invalidate_within_ttl`（尚未关闭的缓存失效回归）。
- macOS 另有：`host::notify::tests::probe_macos_notification_no_choose_application`（真实通知人工探针）。
- Windows 另有：`host::notify::tests::probe_own_aumid_toast`（真实 toast 点击人工探针）。

平台覆盖的窄边界：`document_windows_atomic_preserves_user_bak_when_target_is_locked` 在 Windows 通过，验证独占句柄占用下替换失败且原件/用户 `.bak` 保留；`process_tree_is_reclaimed_on_session_release` 在三平台通过。`aborted_probe_reaps_child_during_initialize_and_tools_list` 为 `cfg(unix)`，在 macOS/Linux 通过，在 Windows 未编译纳入，**不是 Windows ignored 用例**。这些均不替代真实系统钥匙串锁定/拒绝、Office 打开/还原、GUI 操作、完整应用重启、安装/自动更新或发布签名核验。发布产物与签名应另按[发布指南](./release.md#人工验收清单)核对。

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

使用非阻塞 listener 轮询的标准库 HTTP 夹具，accept 后要显式 `stream.set_nonblocking(false)` 再使用阻塞读取和 socket timeout：macOS 的原生 accept 会继承 O_NONBLOCK。不得把 `WouldBlock` 或读错误当作完整空请求并消费响应，否则会出现提前响应、POST RST 和重定向请求错位；读错误/提前 EOF 应让夹具明确失败。

- mock TCP server 要持有 listener 直到测试结束、读完请求头再优雅关闭写半部；直接丢弃带未读数据的 socket 在 Windows 会变成 RST，提前释放临时端口还可能被并发测试复用。`midstream_disconnect_maps_to_network` 的偶发失败不能未经调查就归为代理问题。
- 真实 keyring 是机器级资源，相关测试串行且清理自己的条目；无 Secret Service 时验证明确的安全失败、旧配置保留和无新增明文，不伪造迁移成功或恢复明文回退。
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
- 只验证图表 / 数学占位符的组件用例应隔离异步升级器，并断言组件把真实容器交给升级入口；渲染、流式、缓存与去重由 [diagrams.streaming.test.ts](../ui/src/__tests__/diagrams.streaming.test.ts) 单独守护。不要让无关 lazy import 与渲染在用例结束后继续产生日志或修改 DOM。

## 已知验证边界与待核实事项

以下是源码可见的现行限制或未闭环问题，不是把旧审查清单整体搬回待办：

| 事项 | 已核实边界 / 后续验证 |
|---|---|
| 技能在途撤销 | get/list 缓存已包含作用域和 disabled 集，撤销探针已启用；已加载进历史或冻结提示的正文不作回收。详见 [mcp-and-skills](./mcp-and-skills.md#缓存与禁用生效) |
| 网络代理校验 | 工具逐跳固定已校验地址；禁止内网时拒绝无法固定目标的远端 DNS 代理域名请求，不静默直连。真实代理、TLS 与三平台联调仍须另验；不覆盖 Provider/MCP。详见 [tools-and-safety](./tools-and-safety.md) |
| 长期会话索引 | 索引仍有 2000 条 LRU 上限；索引元数据缺失时恢复会退回单根、无项目归属。完整历史保留不等于元数据无限保留，见 [sessions-and-projects](./sessions-and-projects.md) |
| 项目提示与命令 cwd | 运行提示中的 temps 描述和工具实际默认 project_dir 存在差异；应以执行代码为准并独立修正/测试，见 [runtime-and-scheduling](./runtime-and-scheduling.md) |
| 终态事件丢失 | 快照对账修复了展示与续队能力，不证明底层事件丢失原因已定位；需保留日志与 run 身份关联证据 |
| updater 进度 | store 降频回归已覆盖；没有由单测证明所有真机投递瓶颈都消失。全速仍卡时用限速对照定位 IPC 与渲染侧 |
| 发布/平台能力 | 签名、公证、安装、更新重启和不同目标平台必须由实际产物/日志验证，历史全绿或 workflow 配置不等于本次成功 |

## 交付验证结论的写法

记录命令、工作目录、平台、退出码与失败/跳过原因；使用当前输出中的测试数量，不复制历史数字。对未跑项目明确说明阻塞；环境失败与产品缺陷分开，但都不能报作通过。

界面变更交付人工验证清单，不做 GUI 自动点验。清单应写前置状态、操作、可观察结果与失败分支；优先覆盖本次改动的跨 Tab、并发、重启、取消和持久化路径。发布专用清单集中在 [release](./release.md)。
