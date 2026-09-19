# 右栏信息面板重构 + 订阅额度 + 可拖拽栏宽

> 类型：功能批次（含一处行为修正）· 影响层：`core/quota`（新）· `core/openers`（新）· `host/commands` · `ui/features/shell` · `ui/features/quota`（新）· `ui/i18n` · `ui/theme` · `tauri.conf.json`
> 契约影响：**事件面 28 键不变**（纯拉取式 IPC）、**config schema 不变**（新偏好全走 localStorage）、**零新依赖**（Rust 侧复用 reqwest/chrono/dirs/serde_json；前端手写分隔条）
> 基线：`fix/reasoning-passthrough` @ `8f8726e` → 分支 `feat/rightbar-info-quota`

## 1. 需求与已定决策

需求原文四项 + 过程中追加一项，全部经 7 轮 grilling 收敛为 33 项决策：

| 需求 | 落地口径 |
|---|---|
| ① 移除信息面板的「数据目录」行 | 删 UI 行 + `rightbar.dataDir` 键；`open_data_dir` 能力保留给「关于」弹框 |
| ② 「在文件管理器中打开」兼容 Windows Files 应用 + 新增「在编辑器中打开」下拉 | Windows 按序探测 `files-stable → -dev → -preview → -canary` 别名，命中用 `files-*.exe -Directory <目录>`，spawn 失败降级裸路径、再失败回退 `explorer`；编辑器候选表 **VS Code → Cursor → Windsurf → Zed → Sublime Text → Notepad++ → JetBrains 组（末尾追加）**，默认 = 第一个检测到的，选择即打开当前目录并记忆，未检测到则不渲染控件 |
| ③ 技能 / 当前计划可折叠 | antd `Collapse`（ghost），默认展开，折叠态 localStorage **全局一份** |
| ④ 移除「会话」段，替换为订阅剩余量，可扩展 | 顶替原段位（项目目录 → 额度 → 技能 → 计划）；后端 `core/quota` 通用模型 + 提供商注册表，本期 7 家；**只列检测到凭证的**；任何会话状态（含无 Tab、临时会话）都显示 |
| 追加：左右栏可拖拽调宽并记忆 | 手写分隔条（pointer + 方向键 ±16px + 双击复位），宽度存 localStorage；窗口变窄**只夹显示、保留记忆值**；折叠态不渲染分隔条；窗口最小宽 960 → **1024** |

## 2. 后端改动

### 2.1 `core/quota/`（新域：订阅额度）

- `mod.rs`：与厂商无关的模型 —— `QuotaEntry { key, label, used_percent, remaining_percent, value_text, resets_at }`（**百分比行与数值行二居其一**）、`QuotaStatus { ok, invalid, error }`（**「未配置凭证」不是错误**，不进返回列表）、`QuotaSnapshot`；`ProviderKind` 固定注册序（OpenCode Go → DeepSeek → MiniMax → MiniMax CN → Kimi → Zhipu → Z.ai）+ `order_for(active_base_url)`：当前会话模型的 provider **按 base_url 域名**命中即置顶，其余保持固定序。HTTP 复用 `provider::proxy::build_client`（尊重用户代理），单家 10s 超时、7 家并发、失败互不影响、无缓存。
- `credentials.rs`：凭证链 `环境变量 → opencode 全局配置 provider.<key>.options.apiKey（支持 ${ENV} 模板）→ auth.json（type == "api"）`；`Resolver` 注入环境/文件读取，单测不碰真实凭证。
- `opencode_paths.rs`：按 opencode 运行目录语义解析（`OPENCODE_CONFIG_DIR` / `$XDG_CONFIG_HOME/opencode` / `~/.config/opencode`、`$XDG_DATA_HOME/opencode` / `~/.local/share/opencode`，Windows 追加 `%APPDATA%`、`%LOCALAPPDATA%` 候选）+ jsonc（去注释/尾逗号）。
- `providers/`：7 家适配器（取数 + 纯解析分离，解析收注入的 `now`）。要点与坑：MiniMax 国际响应计数是「剩余」、中国响应是「已用」（同字段不同语义，勿按字段名直觉改）；Zhipu/Z.ai 鉴权头是**裸 key**（无 `Bearer`），window 由 `type`+`unit`(3/6) 映射，`TIME_LIMIT → MCP`；Kimi 重置时间有「字符串 / 秒数 / 窗口时长」三级来源；DeepSeek 是**数值行**（按币种，非百分比）。
- 安全：凭证与响应体绝不进日志；错误文案按 token 抹除（`sanitize`）。

### 2.2 `core/openers/`（新域：外部打开器）

- 探测与启动分离：所有探测函数收注入的 `EnvBases` + 存在性/目录枚举谓词，单测不触碰真实安装目录、不真开进程。
- Windows `.cmd/.bat` 垫片经 `cmd /C`，全部进程统一 `CREATE_NO_WINDOW`（沿用 `tools/command` 先例，不闪黑框）。
- `open_dir` / `open_data_dir` / `open_logs_dir` 共用同一路径（三处行为一致）。

### 2.3 IPC 与窗口

- 新命令 `list_editors` / `open_in_editor` / `quota_snapshots(active_base_url?)`（`host/commands/openers.rs`、`quota.rs`，`commands/mod.rs` + `lib.rs` 注册）；`open_dir` **签名不变**（既有测试与调用方零改动），内部改走 `core::openers`。
- `tauri.conf.json#minWidth` 960 → 1024，并同步 `core/ui_state.rs::MIN_WINDOW_WIDTH`（两套独立下限不允许漂移）。

## 3. 前端改动

- `ipc/types.ts` / `client.ts`：`EditorInfo`、`QuotaEntry`、`QuotaSnapshot` 与三个调用；UI 只认通用结构，**未来加提供商不改组件**。
- `features/shell/RightBar.tsx`：删数据目录行与会话段；标签行 = 文件管理器按钮 + `<OpenInEditorSelect>`；技能/计划改 `Collapse`（ghost；标题字号/颜色与信息页其他标题统一为 **11px + dim**，选择器钉 antd 6 的 `.ant-collapse-title`（旧版 `-header-text` 已废弃，写成它等于死规则）；箭头用 `expandIconPosition="end"` 落行末，使标题文字与「项目目录 / 订阅额度」左对齐）；`<QuotaSection visible={rightBarOpen && rbTab === "info"}>`。
- `features/shell/OpenInEditorSelect.tsx`（新）：下拉默认第一个检测到的 / 上次选择，改选即打开并记忆，未检测到不渲染。
- `features/quota/QuotaSection.tsx`（新）+ `quotaFormat.ts`（纯函数：风险分级、窗口标签映射、摘要取值、倒计时、相对更新时间）：每家一行摘要（名 + 最紧张窗口 + 剩余% + 细条），点整行展开全部窗口（**展开后标题行的额度摘要隐藏**，只留提供商名与异常标记，避免与明细行重复；再点折叠则恢复）；百分比行按**剩余**方向渲染（<20% 橙、<5% 红，颜色走 `--ws-warn` / `--ws-err` token）；数值行（DeepSeek 余额）直接给文本；凭证形态异常/请求失败 → 摘要行带 `!` 标记（详情在展开内容里）；全家无凭证 → 单行指引 + 悬浮列出环境变量与两个路径；刷新 = 可见时进入拉一次 + 每 5 分钟 + 手动（`visible` 门控，面板收起/切页签即停）；另有 60s 纯文本 ticker 刷新倒计时与「X 分钟前更新」（**写在刷新按钮左侧的同一标签行**，不占列表底部）。
- 可拖拽栏宽：`utils/layout.ts`（常量 + `clampNavWidth` / `clampRightBarWidth` / `resolveDisplayWidths` / `dragLimit` 纯函数）、`features/shell/ResizeHandle.tsx`（`role=separator` + `aria-*` + `setPointerCapture` + 方向键 + 双击复位 + 拖动期间 `<html>.ws-resizing` 关过渡）、`useDisplayWidths.ts`（AppShell 的 Sider / 顶栏左段 / **RightBar 的 `--rb-width`** 三处消费同一份显示宽度，保证分隔线不断开）；`ui` store 新增 `navWidth` / `rightBarWidth`（localStorage `ws_nav_width` / `ws_rb_width`）。
- **显示被夹取时禁用拖动**（`aria-disabled` + `not-allowed`）：窗口放不下时拖动会把夹取后的显示宽写回记忆值，静默抹掉用户原来的宽度；夹取期间只保留双击复位，与「只夹显示、保留记忆值」的决策一致。
- 右栏宽度改为 `width: var(--rb-width, 328px)`（折叠态 `width: 0` 契约与 0.2s 过渡不变）；右栏下限 328 = 内容下限 300 + 左右 padding 28（`.rb-tabs{min-width:300px}` 与既有样式契约测试不破、不裁切）。
- i18n：双侧对称增删（新增 `rightbar.openInEditor*` / `rightbar.quota*` / `rightbar.window.*` / `app.resize*`；删除 `rightbar.dataDir` / `rightbar.startedAt`）+ **新增「中英键集合一致」守护测试**（此前仓库无此守护，漏改另一语言不会红）。
- 窄窗行为：左 180 + 中栏保底 480 + 右 328 = 988 ≤ 1024（窗口最小宽配套）；窗口再窄时按「先压右栏 → 再压左栏 → 中栏继续压缩」夹取显示。

## 4. 测试与验证

| 层 | 结果 |
|---|---|
| `cargo test`（src-tauri） | **670 passed / 0 failed / 3 ignored**（新增 32 例：凭证链命中顺序、jsonc、opencode 路径候选、`${ENV}` 模板、7 家解析（MiniMax 双语义 + 单靠 weekly 重置字段的端点 + GLM unit 映射与 Zai 错误体 + Kimi 三级重置来源与「无 limit 行跳过」+ DeepSeek 币种过滤）、Files 选择三分支、编辑器探测顺序与去重、固定安装路径（含绝对路径）与跨平台用例、`.cmd` 垫片命令行引号规则、未知 editor id、窗口下限、`open_url` 命令形状）。本机默认并行下偶发既有 flaky `provider::tests_integration::midstream_disconnect_maps_to_network`（单跑即过，与本批无关；CI 三平台均绿） |
| `pnpm --dir ui test` | **465 passed / 57 文件**（新增：i18n 键对称、数据目录行与会话段不存在、折叠默认展开与持久化、折叠标题类名与箭头位置、编辑器下拉四态、额度多提供商（摘要/展开/数值行/风险类与进度条填充元素/更新时间位置/无凭证/失败/可见性门控）+ 展示纯函数、栏宽纯函数、分隔条交互（拖动/键盘/双击/失焦清理/夹取期禁用）、`--rb-width` 真接线、偏好读盘容错） |
| `pnpm --dir ui build` | type check + vite build 通过 |
| 真实接口探针（授权） | `cargo test --lib -- --ignored live_probe` 用本机 OpenCode Go 凭证实调成功：`rolling 100% / weekly 31% / monthly 66%`，凭证来源 `auth.json`，字段语义与解析一致 |

## 5. 手动验证清单（界面改动不做 GUI 自动点验）

1. **文件管理器**：Windows 上点项目目录后的图标 → 应拉起 Files 应用并定位到该目录（装了 Files 时）；卸载/改名 Files 别名后应回退 explorer。
2. **编辑器下拉**：默认显示 VS Code（本机候选表第一个检测到的）；下拉切到 Zed → 应当即用 Zed 打开项目主目录；重启后下拉仍停在 Zed。
3. **折叠**：点「技能」「当前计划」标题可折叠/展开；重启后折叠态保持（全局一份，所有项目一致）。
4. **额度段**：应显示 OpenCode Go（本机 `auth.json` 有凭证）的三窗口；摘要行是最紧张的窗口，**展开后标题行不再重复额度信息**（名称下方的窗口/剩余%/进度条隐去，折叠即恢复），点开看全部与重置倒计时；切到其他页签或收起右栏后不应再发请求（可用后端日志观察）。
5. **无凭证态**：临时把 `~/.local/share/opencode/auth.json` 改名 → 面板应显示一行提示，悬浮可见环境变量名与两个路径。
6. **栏宽**：拖左右栏分隔条、方向键微调、双击复位；重启后宽度保持；把窗口拖到最小宽（1024）时三栏仍可用（左栏自动收窄但不会被压没）。

## 6. 非目标与已知限制
- **非目标**：额度阈值系统通知、顶栏/Composer 额度入口、额度历史曲线、Zen 账单抓取与 `opencode.db` 用量、手填编辑器路径、设置页手填 API key、其余 8 家提供商（OpenAI/OpenRouter/Chutes/NanoGPT/Ollama Cloud/Synthetic/xAI/GitHub Copilot）、额度数据落盘。
- **本机无凭证的 6 家**（DeepSeek / MiniMax 国际+CN / Kimi / Zhipu / Z.ai）按上游实现语义编写，**待实测确认**：解析对异常形态容错（缺字段/类型变化 → 该家单独报错，不影响其他家），单测已钉住上游语义。
- **Files `-Directory` 参数**：上游稳定渠道对命令行的识别与 dev 渠道存在耦合可能，已做三级降级（`-Directory` → 裸路径 → explorer）；若实测出现「打开首页而非目标目录」，退化方案是改用 AUMID 启动或读取用户注册的 Folder 处理程序。
- **隐藏项**：`rightbar.startedAt`（原会话开始时间）随段移除，`tab.createdAt` 仍是会话数据的一部分（会话列表/自动命名照旧使用），只是信息页不再展示。
- 额度不落盘：每次可见即重新拉取（无进程内缓存），失败即错误态 + 重试，不展示过期数据。

## 7. 评审与修复（code-reviewer 轮）

首次审查结论为「需修复后复审」，两个 🔴 均属「功能写了但用户用不到」：

| 级别 | 问题 | 修复 |
|---|---|---|
| 🔴1 | `.cmd`/`.bat` 整片的启动命令：`cmd /C` 会把含空格路径的首尾引号剔掉（本机 VS Code 必经此路径） | 新增 `script_command_line()`（`""<脚本>" "<参数>""` 形式）+ `raw_arg` 拼命令行；纯函数单测钉住引号规则 |
| 🔴2 | 右栏宽度变量 `--rb-width` 无人写入 → 拖拽完全不生效 | `RightBar` 根节点写入显示宽度（与分隔条同源）；新增「`--rb-width` 真接线」与「拖动写 store + localStorage」两条集成钉子 |
| 🟡 | `ws-resizing` 在指针被系统夺走/组件卸载时可能残留（全局关掉宽度过渡） | `onLostPointerCapture` + 卸载兜底清理，并补两条用例 |
| 🟡 | MiniMax 硬卡 `remains_time` 会把中国端点整家滤掉 | 改为「`remains_time` 或 `weekly_remains_time` 任一存在」+ 用例 |
| 🟡 | Kimi 只有 `used` 没有 `limit` 的行会伪造「剩余 0%」红色告警 | 无可用 `limit` 的行直接跳过（不伪造告警）+ 用例 |
| 🟡 | 窄窗夹取期间拖动会抹掉记忆值 | 夹取时禁用拖动（`aria-disabled` + `not-allowed`，保留双击复位）+ 用例 |
| 🟢 | 颜色硬编码、死代码、无效 i18n 键、同步探测塔主线程 | 改用 `--ws-warn`/`--ws-err` token、删除死 `catch` 与 `rightbar.session` 键、`list_editors` 改 async |
| 🔴2（复审追加） | 进度条颜色覆盖写成 `.ant-progress-bg`，而 antd 6 行进度条的填充类名是 `.ant-progress-track` → 覆盖是死规则，且移除 `strokeColor` 后回退为 antd 默认 `colorInfo`（蓝），既丢风险色又破强调色约束 | 改用 `.ant-progress-track`，并补默认态（中性墨色）+ 风险态三条规则；新增断言钉住「包裹类 + `.ant-progress-track` 真实存在」 |

**交付后补充（用户确认的计划外小修）**：`host/commands/system.rs` 的 `open_url` 原本在 Windows 走 `cmd /C start` 但**未设 `CREATE_NO_WINDOW`** → 点「关于」里的仓库链接会闪一下控制台窗口。已抽出 `windows_open_command()` 统一带标志（常量从 `core::openers::CREATE_NO_WINDOW` 收敛复用，不再各写一份魔术数字），并加「`cmd /C start "" <url>` 形状」钉子（空标题占位丢了就打不开链接；标志本身无法从 std 读出，靠共享常量 + 同一路径保证）。

**CI 轮修复（PR #38）**：首轮 CI 上 `Test (macos-14)` / `Test (ubuntu-24.04)` 红、`windows-2022` 绿。定位到**一条真缺陷**加**一条写死平台假设的测试**：

| 问题 | 修复 |
|---|---|
| `expand_template` 只认 `{HOME}` / `{LOCALAPPDATA}` / `{PROGRAMFILES}` / `{PROGRAMFILES_X86}` / `{APPLICATIONS}` 模板，不以占位符开头的条目直接返回 None——而 Linux 表里 VS Code 的固定位置就是绝对路径 `/snap/bin/code`，该候选**永远不会进入探测**（snap 安装的编辑器识别不出来） | 不以 `{` 开头的条目按字面路径受理（缺失基目录仍返回 None 的语义不变）；新增 `expand_template_accepts_absolute_paths` 钉住 |
| `detect_editors_uses_fixed_install_paths_when_path_is_empty` 按 Windows 的 `%LOCALAPPDATA%/Programs/Microsoft VS Code/Code.exe` 断言（注入 local_app_data + 真实文件系统），macOS/Linux 上必然 0 命中 | 改为从**本平台**候选表里取第一条带固定路径的条目、展开其首条模板，全部基目录换成 `/probe/...` 探针路径并用注入谓词命中——不碰系统目录、无需 cfg 分支，三平台同一条用例都成立 |

修完 CI 三平台（macos-14 / ubuntu-24.04 / windows-2022）与 `Rust lint` 全部转绿。

## 8. 提交建议

```
feat(rightbar): info panel refactor with subscription quota and resizable sidebars
```
