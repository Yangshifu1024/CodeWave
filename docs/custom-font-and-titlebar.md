# — 自定义字体 + 自绘标题栏批次报告

> 参考实现：前身项目 GitWave 的字体偏好（`fonts.ts` + SettingsModal Appearance）与标题栏（`tauri-plugin-decoration` v3 全套接入）。
> 本批次按功能分两笔 commit：`2669537`（字体）、`149fe80`（标题栏）；code-reviewer 审查后追加修复 commit（见 §5）。
> 验证基线：`cargo test` 229 passed / 0 warning；`pnpm --dir ui test` 90/90（含审查批次新增 6 例）；`pnpm --dir ui build` 通过。
> 界面改动不做 GUI 自动点验，手动清单见 §4。

## 1. 自定义字体（commit 2669537）

### 1.1 机制：双槽 CSS 变量 + 挂载前应用（防 FOUC）

- `ui/src/theme/native.css` 定义 `--ws-font-sans(-fallback)` / `--ws-font-mono(-fallback)` 四个 token；body 与全部代码面（app.css 原 13 处硬编码 `"SF Mono", Menlo, Consolas, monospace` 链）统一改引 `var(--ws-font-mono)`。
- `ui/src/utils/fonts.ts`（移植 GitWave fonts.ts，去字号缩放部分）：偏好为逗号分隔的**本机已安装字体名**，空 = 默认链。`sanitizeFontList` 剥离可逃逸 CSS 字面量的字符（引号/反斜杠/花括号/分号/尖括号/控制字符）但保留中文等非 ASCII 名；`buildFontOverride` 生成 `"用户字体", var(--ws-font-*-fallback)` 引导链写入 `<html>` 内联。
- `main.tsx` 在 `createRoot().render()` 前调 `applyInitialFonts()`——先于 React 挂载应用，防首帧默认字体闪变。
- 持久化走 localStorage（`ws_font_sans` / `ws_font_mono`）：纯 UI 偏好不入后端 config（与 GitWave 一致），空值移除键。
- **antd 接线**（CodeWave 特有，GitWave 是 tailwind 无此问题）：antd 组件不继承 body 字体，`App.tsx` ConfigProvider token 下发 `fontFamily: "var(--ws-font-sans)"` + `fontFamilyCode: "var(--ws-font-mono)"`，随设置即时生效。

### 1.2 设置 UI：外观页签

- SettingsModal 新增「外观」页签（`ui/src/features/panels/FontSettings.tsx`）：界面字体 / 等宽字体两个输入框（placeholder = 默认链头部名），**回车或失焦即时生效**（不走保存按钮），带恢复默认按钮与双行预览（同一段样本文字分别走 sans/mono 槽，预览空草稿时回落默认链）。
- i18n 双语新增 `settings.appearance / uiFont / monoFont / fontHint / fontReset`。

### 1.3 不移植项（有意裁剪）

- GitWave 的**字号缩放**（sans root font-size + mono `--font-mono-scale`）不搬：CodeWave 全线 px 布局（非 rem 体系），全局字号缩放需重写所有字号规则，收益不成比例。后续如需可单独立项。

## 2. 自绘标题栏（commit 149fe80）

### 2.1 方案：tauri-plugin-decoration v3（与 GitWave 同款）

插件供给各平台窗口控制按钮，应用自持标题栏内容与拖拽区：

| 平台 | 行为 |
|---|---|
| Windows 11/10 | 插件注入 HTML 控制（最小化/最大化还原/关闭），保留 **Snap Layout 悬停面板**；窗口去原生框后保留阴影/圆角/贴边 |
| macOS | 原生红绿灯 **Overlay** 悬浮于应用内容上（`titleBarStyle: Overlay` + `hiddenTitle` + `trafficLightPosition`） |
| Linux | GTK 风格 HTML 控制（Wayland 最佳努力，X11 激活失败自动回退原生） |

### 2.2 后端（分层：host 层持有 tauri 依赖，core 零感知）

- `src-tauri/Cargo.toml`：`tauri-plugin-decoration = "3"`；`lib.rs` 在任何 webview 创建前 `.plugin(tauri_plugin_decoration::init())`。
- `host/commands.rs` 新增 `activate_and_show`（照 GitWave 集成指南）：激活失败走 `restore_and_show` 兜底——恢复原生装饰并保证亮窗（窗口以 `visible: false` 起动，**任何路径都必须最终亮窗**），返回 `"custom" | "native"`。macOS 分支 `set_traffic_lights_inset(14, 17)` 对齐 50px 顶栏视觉中心。
- `tauri.conf.json`：main 窗口 `decorations: true`（保持！激活成功才去框，失败才有原生框可回退）+ `visible: false` + `titleBarStyle/hiddenTitle/trafficLightPosition`（均仅 macOS 生效）；CSP `style-src` 追加插件样式协议源（`tauri-plugin-decoration:` 与 Windows 下的 http/https 形态）。
- `capabilities/default.json`：`decoration:default` + `core:window:allow-internal-toggle-maximize`（drag.js 双击最大化用；`start-dragging` 原已具备）。

### 2.3 前端

- `ui/src/features/shell/useTitlebar.ts`（移植 GitWave useTitlebar）：挂载后 invoke `activate_and_show`，mode 与平台标记写 `<html data-titlebar-mode / data-os>` 供 CSS 分支；命令异常按 custom 兜底。
- **AppShell 顶栏即标题栏**：Header 底层铺 `.titlebar-drag-zone`（`data-tauri-drag-region`，全平台——GitWave 在 macOS 走手动拖拽是为避开其自研 instant zoom 与 drag.js 的竞态（tauri#13898），CodeWave 无自研 zoom 故无竞态，标准 drag region 即安全）；内容层 `z-index: 1` 浮于其上，Tabs/按钮照常交互，空白处拖窗。
- 内边距预留（app.css）：`.toolbar` 左右 padding 走 `max(10px, var(--tauri-plugin-decoration-left/right-clearance, 0px))`（插件测量注入，全屏归 0）；macOS 追加 `max(78px, left-clearance)` 红绿灯保底（GitWave 同款兜底，变量首帧未到也不穿帮）；`html[data-titlebar-mode="native"]` 时全部撤销（回退模式恢复普通内边距）。
- 插件消费的应用侧设置（native.css）：`--tauri-plugin-decoration-titlebar-height: 50px`（控制条高度对齐顶栏，32px 按钮垂直居中）、`--tauri-plugin-decoration-z-index: 3`。
- 顺带修复：通知浮层 `.notify-stack` 从 top 16px 下移到 60px，避免遮挡右上角窗口控制按钮。

### 2.4 行为注意事项

- 窗口 `visible: false` 起动：**前端 JS 完全无法启动时窗口永不出现**（GitWave 同款取舍）；激活/回退路径均已兜底，仅残缺 dev 环境可能触发。
- vite HMR 重挂载会重复 invoke：插件对已 Active 状态幂等（GitWave 开发期同模式长跑验证）。
- 托盘「显示」/单实例互斥的 `show()` 不受影响；关闭到托盘语义不变。

## 3. 测试

- `ui/src/__tests__/fonts.test.ts`（新增 13 例）：sanitize 边界（逃逸字符/控制字符/中文保留/空白折叠/空输入）、override 链构建、localStorage 往返与 `<html>` 内联应用、恢复默认路径。
- `app.smoke.test.tsx`：设置页签断言 5→6（+外观，双输入框 + 两行预览）；主窗口断言拖拽层存在且带 `data-tauri-drag-region`、激活后 `html[data-titlebar-mode="custom"]`。
- Rust 侧纯装配无新逻辑分支（activate_and_show 依赖真实窗口，不宜单测），`cargo test` 229 passed 守护编译与既有回归。

## 4. 手动验证清单（用户执行）

### 4.1 自定义字体

1. 设置 → 外观：界面字体输入 `微软雅黑`，回车 → 全局 UI 字体立即切换（含 antd 按钮/输入框）。
2. 等宽字体输入 `Cascadia Mono`，回车 → 代码块/工具输出/右侧栏日志/命令白名单等 mono 面切换。
3. 输入不存在的字体名（如 `NoSuchFont`）→ 回落到默认链（外观无异常）。
4. 两个输入框分别点恢复默认按钮 → 回默认；重启应用 → 偏好保持且**首帧即为所选字体**（无闪变）。
5. 输入 `Map"le` 之类带引号内容 → 被清洗为 `Maple`。

### 4.2 自绘标题栏（Windows 11）

1. 启动 → 窗口短暂不可见后出现，**无原生标题栏**，右上角有最小化/最大化/关闭按钮，与顶栏按钮（设置等）不重叠。
2. 顶栏空白处（tab 条上下空隙、按钮之间）按住拖动 → 移动窗口；双击空白 → 最大化/还原。
3. 悬停最大化按钮 ≥ 500ms → **Snap Layout 四宫格面板**弹出，选择布局可贴边。
4. 关闭按钮悬停变红 (#c42b1c)；点击关闭 → 按既有「关闭到托盘」语义隐藏到托盘（托盘可唤回）。
5. 明暗主题切换（系统）→ 控制按钮图标颜色跟随（暗色下变浅）。
6. 最大化后窗口圆角消失、边缘贴屏（Win11 行为）；还原后圆角恢复。
7. `pnpm tauri dev` 与打包版分别验证（单实例互斥仍生效）。

macOS（如有条件）：红绿灯悬浮于顶栏左侧、Tab 条不与之重叠；红绿灯拖拽/双击缩放正常。Linux（Wayland）：控制按钮渲染为 GTK 风格圆钮；X11 下预期回退原生标题栏。

## 5. code-reviewer 审查与修复批次

审查结论：字体批次无 🔴；标题栏批次 1 项 🔴 + 3 项 🟡，已全部修复：

- **R1（🔴 修复）**：`.toolbar` 缺 `position: relative`——`.titlebar-drag-zone`（absolute inset:0）无定位祖先时包含块回退**初始包含块 = 整个视口**，透明拖拽层会盖住 Sider/聊天区/Composer/RightBar（普通流内容指针事件全被拦截）。补一行定位 + 新增样式契约测试（vitest `css:false` 连 `?raw` 都返回空串，改 node fs 直读 CSS 原文断言：toolbar 定位、拖拽层几何、内容层 z-index、native/macos 分支规则顺序）。
- **Y1（🟡 修复）**：`visible:false` 亮窗完全依赖前端挂载，dev 模式前端编译失败时窗口永不出现——`lib.rs` setup 增加 5s watchdog（`is_visible()` 为 false 则强制 `show()`）。
- **Y2（🟡 修复）**：`useTitlebar` 对命令 Err 一律按 custom 兜底，与后端「已尽力恢复原生框」的真实语义背离——Err 改映射 `native`（撤销 clearance 预留更安全）。
- **Y3（🟡 修复）**：补测试盲区——smoke 增加 native 回退分支用例（mock `activate_and_show` 返回 "native"）；fonts 增加端到端防御用例（localStorage 预置恶意串 → `applyInitialFonts` 输出已剥离）。

审查确认的安全要点（不改动）：sanitize 三重防线（字符剥离 + CSSOM `setProperty` + 段名双引号包裹）足以防 CSS 注入；CSP 追加与插件官方 integration.md 逐字一致；capabilities 最小集；`activate_decoration` 幂等（HMR 重入安全）；antd token 用 `var()` 字符串合法（canvas measureText 边缘回落仅影响亚像素精度，本项目 Tabs 走自绘溢出不触及）。
