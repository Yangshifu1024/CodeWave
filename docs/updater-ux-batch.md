# 自动更新体验批次：弹窗 / 发布说明 / 进度 / 重试（对齐 GitWave）

> 需求：**实现与 GitWave 一致的升级体验**，包括弹窗显示 release notes、下载进度、重试等。
> 分支 `feat/updater-ux`。此前应用内的自动更新只有 toast（无弹窗、无进度、无重试，且清单 `notes` 恒为空）。

## 1 现状与缺口

改造前 `ui/src/utils/updateCheck.ts` 只有一个 `checkForUpdates()`：check → toast → `downloadAndInstall()` → toast → 1.5s 后重启。四个缺口：

| 缺口 | 表现 |
|---|---|
| 无更新弹窗 | 只有一句 toast，看不到新版本信息 |
| **发布说明不可达** | `latest.json` 的 `notes` **恒为空字符串**（发布侧从未给 tauri-action 传 `releaseBody`，见 §4），即便有内容也无处展示 |
| 无下载进度 | 大安装包（15–90MB）下载期间界面只有一句「正在下载…」，无进度、无法判断卡死 |
| 无重试 | 失败即结束，必须重新点「检查更新」；拉取到的 `Update` 实例被丢弃 |

另外 Linux deb/rpm 装在系统路径下无法自替换，GitWave 会降级为「打开发布页手动下载」，CodeWave 没有这条分支。

## 2 方案

### 2.1 状态机（`ui/src/stores/updater.ts`，新增）

8 个相位，与 GitWave 的 `updaterStore` 等价：

`idle` / `checking` / `available` / `manual-download` / `downloading` / `ready` / `up-to-date` / `error`

字段：`modalOpen`、`currentVersion`、`newVersion`、`notes`、`downloadedBytes`、`totalBytes`、`error`、`checkEpoch`。

关键设计（照抄 GitWave 的两条不变量）：

- **phase 与 modalOpen 解耦**：`downloading` 允许隐藏（下载继续在后台跑）；`markReady()` **强制** `modalOpen=true`——用户可能中途关过弹窗，重启入口不能被吞掉。
- **`checkEpoch` 陈旧结果丢弃**：每次检查递增代次，`fresh()` 判定「我是否仍是最新一次检查」，旧代次的结果（哪怕也发现了更新）一律丢弃，避免把 `available` 回滚成 `up-to-date`。

CodeWave 额外的一条：`fail()` 也置 `modalOpen=true`。原因——改造后失败反馈从 toast 移交给弹窗，而「关于」按钮 / macOS 菜单触发的显式检查**弹窗本就没开**，不主动打开就等于把失败静默吞掉。

### 2.2 流程（`ui/src/utils/updateCheck.ts`，重写）

| 导出 | 作用 |
|---|---|
| `checkForUpdate({ silent })` | 检查更新。`silent=true`（启动自动检查）全程不可见：无更新/失败/离线都不打扰 |
| `checkForUpdates()` | 兼容入口（= 显式检查）：既有的「关于」按钮与 macOS 菜单项继续调它 |
| `startUpdate()` | 下载并安装：`installInFlight` 防重入 + 进度事件 + 失败归类文案 |
| `retryUpdate()` | `pendingUpdate` 存在 → 复用实例重下；否则重新检查 |
| `restartUpdate()` / `openReleases(version)` | 重启应用 / 打开 Release 页（后者是 deb-rpm 与配额耗尽时的替代路径） |
| `useStartupUpdateCheck()` | AppShell 挂载时调用一次：延迟 3s 静默检查，`ws_auto_update=false` 可关（默认开启） |

要点：

- **进度**：`Started` 取 `contentLength`、`Progress` 累加 `chunkLength`、`Finished` 把总量钳到 `total`（让进度条能到 100%，不卡在 99%）；`contentLength` 缺省时只显示已下载字节、进度条走不确定态。
- **失败文案**：`403/429/rate limit` 命中 → `notice.updateFailedRateLimited`（上一批加的「配额耗尽」可行动指引，明确写「切换代理无效」）；其余 → `notice.updateFailed`（带原始错误串）。
- **`pendingUpdate` 是模块级槽位**：失败后可复用同一 `Update` 实例重试，不必重新走网络检查。
- **StrictMode 时序**：`startedRef` 必须在 `setTimeout` 回调内翻转——若在 effect 体内翻转，首挂载就把开关用掉，remount 的「重放」反而跳过检查。
- **无更新仍给反馈**：显式检查走 `notice.upToDate` toast（弹窗只服务「有更新 / 失败 / 待重启」三态，无更新弹窗是打扰）；静默检查完全不可见。

### 2.3 弹窗（`ui/src/features/panels/UpdateModal.tsx`，新增）

由 AppShell 常驻渲染，可见性取自 store。标题/正文/页脚三分支按相位驱动：

| 相位 | 正文 | 页脚 |
|---|---|---|
| `available` | 版本行（新版本 + 当前版本）、「查看发布说明」、**发布说明区块** | 稍后 / 下载并安装 |
| `manual-download` | 同上 + 「deb / rpm 无法自替换」说明 | 稍后 / 打开发布页 |
| `downloading` | 进度条 + 已下载/总量字节 | 隐藏（隐藏 ≠ 取消） |
| `ready` | 「安装包已就绪，重启后生效」 | 稍后 / 立即重启 |
| `error` | 错误文案（红） | 关闭 / 重试 |

发布说明按**纯文本**展示（`latest.json` 的 `notes` 是 markdown 源文；弹窗不做渲染，完整排版在 Release 页），容器限高可滚。样式在 `app.css` 的 `.updater-*` 段，全部走 `--ws-*` token。

### 2.4 Linux 分支判定

`manual = 运行在 Linux && 不是 AppImage`。AppImage 判定走**新增的后端命令** `is_appimage`（`std::env::var("APPIMAGE").is_ok_and(|v| !v.is_empty())`；该变量只在 AppImage 的 AppRun 包裹运行时注入，其它平台恒 false）。未引入 `@tauri-apps/plugin-os`（避免为一个判定加插件依赖），Linux 判定用 `navigator.userAgent`——误判的最坏结果是多显示一条手动下载说明。

## 3 与 GitWave 的对照

| 能力 | GitWave | 本批次 |
|---|---|---|
| 相位机 | 8 相位 `updaterStore` | 同名同义 8 相位 |
| 发布说明 | `update.body → notes`，弹窗展示 | 同 |
| 进度 | `Started/Progress/Finished` + `Finished` 钳制 | 同 |
| 重试 | `pendingUpdate ? install : check` | 同 |
| 暂停/恢复 | `installInFlight` | 同 |
| 待重启 | `markReady` 强制重开弹窗 | 同 |
| Linux 降级 | `platform() === "linux" && !isAppimage()` | `navigator.userAgent` + 后端 `is_appimage`（无 plugin-os 依赖） |
| 启动静默检查 | localStorage 偏好 + 3s 延迟 | 同（key 为 `ws_auto_update`，与 CodeWave 既有 `ws_*` 惯例一致） |
| 偏好开关入口 | 设置页「更新」区块：自动检查复选框 + 手动检查按钮 | 同（设置 → 通用 → 更新） |
| 失败即弹窗 | GitWave 弹窗仅在有更新/失败时打开 | 同（CodeWave 额外让 `fail()` 显式开窗，理由见 §2.1） |
| 视觉 | tailwind + heroui | antd + `--ws-*` token（项目核心约束：组件样式由 antd 接管，不手搓皮肤） |

## 4 发布侧：让 `notes` 真的有内容

`ui/src` 的展示只是一半——`latest.json` 的 `notes` 由 tauri-action 的 `releaseBody` 输入决定，而 `.github/workflows/release.yml` **从未传过它**，所以 v0.3.8 / v0.3.9 的清单里 `notes` 都是空字符串（实测）。修法：

1. `prepare-release` 的 job outputs 增 `release_notes: ${{ steps.notes.outputs.body }}`（复用既有的自建正文：逐条列上一 tag 到本 tag 的提交）；
2. 三平台共用的 `Tauri build + upload` 步骤增 `releaseBody: ${{ needs.prepare-release.outputs.release_notes }}`。

同一个正文喂两处：draft Release 的 body（人看的）+ 清单的 `notes`（应用内弹窗看的）。

## 5 验证

- `pnpm --dir ui test`：**57 文件 / 486 passed**（新增 `updater.modal.test.tsx` 9 例 + `updateCheck.test.ts` 重写 15 例）
- `pnpm --dir ui build`：通过（含 `update.body`、进度事件的类型）
- `cargo test`（src-tauri）：**647 passed**（新增 `is_appimage` 命令与注册）
- 覆盖的相位与行为：available（含 notes 多行文本 / 无 notes 不渲染区块 / 查看发布说明打开 tag 页）、manual-download（含「不得出现下载并安装」反向断言）、downloading（进度文案、隐藏不改相位、总量未知不出现 NaN）、ready（立即重启调 `restart_app`）、error（文案 + 重试两条分支）、无可操作相位不渲染动作按钮；流程侧：403/429/rate-limit 专用文案、epoch 陈旧丢弃、proxy 参数、`Started/Progress/Finished` 到 100%、静默检查全程不可见、`pendingUpdate` 存在/不存在两种重试路径
- **GUI 手动验证清单见 §7**（界面改动按项目约定不做自动点验）

## 6 遗留与边界

- **`notes` 的存量为空**：已发布版本的清单不会追溯（v0.3.9 仍是空 notes），要等**下一次发版**才有说明可显。
- **Windows 上的重启入口**：`downloadAndInstall` 在 Windows 已自行退出并起安装器，弹窗的「立即重启」主要服务 macOS/Linux。
- **`userAgent` 判 Linux**：非权威判定（有意取舍，代价是多一条说明或让 AppImage 用户看到手动下载按钮）；若日后要精确，可加一个后端命令返回平台。
- **`fail()` 开窗**：CodeWave 特有（GitWave 把结果写在设置页状态文案里，无此风险）；代价是弹窗可能在「关于」弹窗之上再叠一层（已用 `zIndex=1100` 固定在上）。
- **偏好开关**：已进设置页（设置 → 通用 → 更新，与 GitWave 的 `UpdatesSection` 对应）；`ws_auto_update` 的值本身不入 config.json（与字体/右栏开合同惯例，纯 UI 偏好存 localStorage）。

## 7 手动验证清单（GUI，用户执行）

> 准备：确认无打包版实例在跑；`pnpm tauri dev`。开发实例的版本号低于线上发布版即可自然触发「有更新」。

1. **手动检查 → 弹窗**：关于 → 检查更新 → 弹出「发现新版本」，显示新版本号 + 当前版本 + 「查看发布说明」；点它打开对应 tag 的 Release 页。
2. **发布说明**：下次发版后（notes 非空）重跑第 1 步 → 弹窗内出现「发布说明」区块，多行文本可滚动；超长行正确断行。
3. **下载进度**：点「下载并安装」→ 进度条推进 + 「已下载 X / Y」；点「隐藏」→ 弹窗关闭、下载继续，完成后弹窗**自动重新出现**并提示重启。
4. **重试**：拔网线/改端口让下载失败 → 弹窗显示错误文案 + 「重试」；恢复网络后点重试 → 直接续下（不重新检查）。
5. **已是最新**：改回等价版本再点检查更新 → toast「已是最新版本」，不弹窗。
6. **启动静默检查**：`localStorage.ws_auto_update = "false"` → 重启应用后 3s 内不应有任何更新请求；删除该键 → 启动 3s 后有更新则自动弹窗，无更新则完全无感。
7. **Linux deb/rpm 降级**（需 Linux 环境）：deb/rpm 安装的实例检查更新 → 弹窗为手动下载说明，主按钮「打开发布页」；AppImage 实例则应能自更新。
8. **失败不静默**：让检查阶段就失败（如把 endpoints 指向不可达地址）→ 显式检查弹错误弹窗；启动静默检查同样失败则**不应**弹窗。
