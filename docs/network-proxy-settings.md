# 网络代理设置（网络页签）实施报告

> 需求：设置中增加「网络」tab，radio-group 卡片式三选一（无代理 / 系统代理 / 自定义代理），自定义代理支持 https 与 socks5；使用代理后应用请求与更新请求都走代理。
> 分支 `feat/network-proxy-settings`（基线 main@9c7ac72）；流程：需求类全流程（pm 需求分析 → 方案 → 实施 → code-reviewer 审查）。

## 1. 现状与缺口

后端此前已有 `ProxyConfig { mode: none|system|manual, url }`（`core/config.rs`）与 `provider/proxy.rs::build_client` 解析链，但四处缺口：

1. 设置页无任何 UI 入口（前端 `proxy` 字段恒 null）；
2. Windows 系统代理探测为空桩（`system_proxy_url()` 恒 `None`，注释标 P1）；
3. 更新请求（tauri-plugin-updater）自持 client，未接代理；
4. client 启动时构建一次（`lib.rs` setup），保存配置后不生效。

关键前置事实：**reqwest 0.13 默认启用 `system-proxy` 特性**（自动读 Windows 注册表 / macOS 系统配置），即「无代理」必须显式 `no_proxy()` 才是真直连；tauri-plugin-updater 2.11.0 的 JS `check(options)` 把 options 原样透传到命令层，命令层原生接受 `proxy: Option<String>`，且 `Update` 对象携带代理供 download/install 复用。

## 2. 代理语义（唯一事实源 `provider/proxy.rs`）

| config 形态 | 出网行为 | 实现路径 |
|---|---|---|
| `proxy: null`（从未配置） | **保持 reqwest 默认（跟随系统）**，存量用户零行为变化 | `apply_proxy` 不干预 builder |
| `mode: none` | 显式直连 | `ClientBuilder::no_proxy()`（屏蔽默认系统探测） |
| `mode: system` | 探测 OS 设置：Windows 注册表 / macOS `scutil --proxy` / Linux 环境变量；命中 → 显式代理，未命中 → 显式直连 | `Proxy::all(url)` 或 `no_proxy()` |
| `mode: manual`（url 非空） | fail-closed：只用配置值（支持 `http://` `https://` `socks5://` `socks5h://`，可含 `user:pass@host:port`）；非法 URL → warn（脱敏）+ 直连 | `Proxy::all(url)` |

补充规则：

- **loopback 恒直连**：显式代理一律追加 `NoProxy::from_string("localhost,127.0.0.1,::1")`——本地模型网关 / 127.0.0.1 的 MCP server 经代理几乎必不通，且内网放行另有 `network.allow_private_network` 闸门。
- 决议拆为纯函数 `resolve_explicit_proxy(cfg) -> Option<Option<String>>`：`None` = null 不干预；`Some(None)` = 显式直连；`Some(Some(url))` = 显式代理。两种 None 来源严格区分（code-reviewer 🔴 修复项）。
- Windows 注册表解析 `parse_windows_proxyserver`：全局 `host:port` 与分协议 `http=h:p;https=h:p;socks=h:p` 两形态，优先级 socks > https > http（与 macOS 解析一致）；PAC URL 不解析（视为未检测到）。

## 3. 生效范围与热生效

| 请求面 | 接入方式 |
|---|---|
| LLM provider（三协议） | 共享 client（`drive.rs` / `context.rs` / `title.rs` 读锁 clone） |
| `web_fetch` / `http_request` 工具 | `build_client_with(cfg, redirect::Policy::none())`——保留逐跳 SSRF 校验语义；http_request 超时改挂 `RequestBuilder::timeout`（与原 client 级 total timeout 等价） |
| MCP streamable-http | `McpManager::start/call` 新增 `http: reqwest::Client` 参数，host 层从 `core.client` 读锁 clone 传入；重连路径（`call` 内 Option::take）同样走代理 |
| 更新检查 + 下载安装 | 前端 `updateCheck.ts`：`ipc.resolveProxy()` → `check({ timeout, proxy })` 透传（null 不传保持默认） |

热生效：`AgentCore.client` 改为 `RwLock<reqwest::Client>`（读取方读锁 clone，std 锁不跨 await）；`save_config` 落盘后**无条件重建**（与日志级别热切换同一「便宜且自愈」思路，顺带完成 System 模式重探测）。运行中的请求持旧 client clone 不受影响。

新 IPC 命令 `resolve_proxy -> Option<String>`：设置页「系统代理」探测回显与前端更新检查共用；`proxy = null` 时命令层返回 `system_proxy_url()` 探测值（null 的实际出网行为就是跟随系统，回显与更新传参同口径）。

## 4. 前端（设置 · 网络 tab，位于「安全」之后）

- heroui radio-group 风格三卡片（`.proxy-mode-card`，整卡可点、选中墨色描边，`--ws-*` token，无彩色 accent）；
- `proxy = null` 显示为「系统代理」——与 HTTP 栈默认行为一致（诚实呈现），首次保存即固化为显式配置；
- 「系统代理」卡片显示探测回显（当前生效端点 / 未检测到），保存后重探测刷新；
- 「自定义代理」展开 URL 输入框：即时红字校验（前缀白名单），切走再切回草稿保留，保存时 trim；
- 保存校验沿用 providers 惯例：非法 → `message.error` + 跳转网络页签 + 不落盘。

## 5. code-reviewer 审查结论（P5）

🔴 1 项已修复：

- R1 `apply_proxy` 把 `proxy=null`（从未配置）也当显式直连（`no_proxy` 关闭系统探测），存量用户会静默丢失系统代理跟随，违背模块文档与 UI 呈现承诺 → 拆 `resolve_explicit_proxy` 纯函数，null 路径原样返回 builder，补区分两种 None 的用例。

🟡 4 项已采纳：Y1 显式代理追加 loopback NoProxy（System 模式 bypass 丢失的最低成本兜底）；Y2 `none` 模式 updater 无 no_proxy 通道（插件命令层只有 `proxy` 参数），文案明示「更新检查暂仍跟随系统代理」，彻底闭环（Rust 侧封装 check）列后续；Y3 代理 URL 日志脱敏（`sanitize_proxy_url` 剥离 userinfo，manual 地址可含凭据）；Y4 本文档落盘消除悬空引用。

🟢 记录项：updateCheck 的类型断言已简化（2.11.0 `CheckOptions` 自带 `proxy`）；macOS `scutil` 探测在 async 命令中同步 spawn（毫秒级、低频，后续可统一 `spawn_blocking`）；save_config 先写 client 后写 cfg 存在瞬时「旧 cfg + 新 client」窗口（无害最终一致）。

## 6. 验证

- 后端：`cargo test` 全绿（549 passed / 0 failed；Windows 实测），0 warning；新增 `parse_windows_proxyserver` ×4（全局 / https>http / socks 优先 / PAC+空值+未知协议）、`resolve_explicit_by_mode`、`sanitize_proxy_url` 用例。
- 前端：`pnpm --dir ui test` 全绿（324 passed / 45 文件，含新增 `settings.network.test.tsx` 5 用例：null 默认选中系统代理 + 探测回显 / none 保存联动 / manual 合法保存 trim / 非法前缀拦截不落盘 / 模式切换草稿保留）；`pnpm --dir ui build` 通过。
- GUI 手动验证清单（需本机代理环境，见下）由用户执行。

## 7. 手动验证清单（GUI，用户执行）

> 准备：`pnpm tauri dev`（确认无打包版实例并存）；本机一个可用 HTTP 代理（如 Clash 7890，可在其面板/日志观察命中）。

1. **页签与默认态**：设置 → 「网络」tab 出现在「安全」之后；未配置过的环境默认选中「系统代理」，卡片下方显示探测回显（有系统代理显示地址，无则显示「未检测到系统代理，请求将直连」）。
2. **自定义代理生效（应用请求）**：选「自定义代理」填 `http://127.0.0.1:7890` 保存 → 发起一次 LLM 对话、一次 web_fetch，代理面板可见命中；改填 `socks5://127.0.0.1:<socks端口>` 保存重试同样命中。
3. **更新请求走代理**：关于 → 检查更新，代理面板可见 GitHub Releases 相关请求命中（无更新也应有 latest.json 请求）。
4. **热生效**：由自定义代理切「无代理」保存，不重启直接发起对话 → 直连成功（系统代理开着也不经代理）；再切回自定义代理保存 → 立即恢复走代理。
5. **系统代理模式**：Windows 设置中开启/关闭手动代理后，回到应用选「系统代理」保存 → 回显跟随变化，请求行为一致。
6. **非法输入**：填 `ftp://1.2.3.4` → 输入框即时红字，点保存被拦（报错 + 停在网络页签，配置未落盘）。
7. **loopback 例外**：配置了本地模型网关（`http://127.0.0.1:xxxx`）+ 自定义代理时，本地网关请求仍直连可用。
8. **MCP streamable-http**：配置一个远程 streamable_http MCP server，连接后代理面板可见命中；stdio server 不受影响。
9. **存量兼容**：不改设置直接关闭弹窗 → `~/.codewave/config.json` 中 `proxy` 仍为 null，行为与升级前一致。

## 8. 后续迭代候选（本期明确不做）

- 「测试连接」按钮（保存前探测端点可达性）；
- Windows `ProxyOverride` / macOS ExceptionsList 完整 bypass 解析（本期仅固定 loopback 例外）；
- 系统代理运行期自动重探测（监听变更）；
- 代理凭据入系统钥匙串；updater 的 no_proxy 通道（Rust 侧封装 check 命令）。
