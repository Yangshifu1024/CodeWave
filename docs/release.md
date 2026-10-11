# 版本发布、签名与自动更新

发布入口是 [.agents/skills/codewave-release/SKILL.md](../.agents/skills/codewave-release/SKILL.md)，实际管道见 [release.yml](../.github/workflows/release.yml)。本文维护操作边界、签名和更新链路，不把历史成功日志当成本次发布结果。

## 发布流程与授权边界

完整流程：确认版本 → 干净且同步的 main → [完整本地门禁](./testing.md) → bump → 审核差异并提交 → **明确确认推送** → main 与 `v*` tag → 三平台构建 draft Release → 核对资产 → 人工 Publish。

1. 用户明确给版本时使用该版本；否则根据上个 tag 以来的提交提出 semver 建议，得到确认后才 bump。0.x 期间破坏性变化/新功能通常建议 minor，修复和文档通常建议 patch，但以实际兼容风险与用户决定为准。
2. `git status --porcelain` 必须为空，当前分支应为 main；同步 main 的 Git 写操作仍须在授权范围内。工作区已有改动时先停，不混进发布提交。
3. 在根执行 `pnpm prepr`。任一硬步骤失败都停止，不 bump；clippy 软失败必须明示，不能笼统声称所有检查全绿。
4. 执行 `pnpm bump <x.y.z>`，核对版本一致与锁文件差异。
5. 有提交授权时使用 `chore(release): vX.Y.Z`。默认 Git 写操作由用户执行，文档和技能不是隐含授权。
6. 汇总版本、提交范围、构建目标、签名验证状态和人工发布要求，**停在推送前**问明是否推 main 与该 tag。沉默或含糊回应不视作同意。
7. 授权后推 main、创建并推 tag。tag 触发的是构建和 draft 创建，**不自动 Publish**。等待构建与收尾完成，再审核 draft 资产。

版本一旦有 tag，不复用、不移动 tag 来覆盖失败版本；另选版本号。发布 draft 后会改变已安装客户端的更新来源，不能把 Publish 当成普通文档操作。

当前 concurrency 为 `${{ github.workflow }}-${{ github.ref }}`：同一 ref 的新运行取消同组旧运行，**不同 tag 属不同组，不保证互相取消**。因此发新 tag 前要看是否仍有另一版本在构建，不能依赖“新版本自动停止旧发布”的旧说法。

### 版本脚本

[scripts/bump-version.mjs](../scripts/bump-version.mjs) 接受可选 `v` 前缀和预发布后缀，统一改：

- 根 `package.json`
- `ui/package.json`
- `src-tauri/tauri.conf.json`
- `src-tauri/Cargo.toml`

随后运行 `cargo update -w` 和 `pnpm install --lockfile-only` 刷新两份锁文件。通常纯版本 bump 的实际 diff 是上述四处加 `Cargo.lock`；pnpm 锁文件不记录 workspace 自身版本，因此可能无差异。若第三方依赖或其它文件也变动，要调查，不能机械按文件数判正确。Windows autocrlf 可产生状态条目而无实际 diff，也须查看差异确认。

同版本运行是合法 no-op，脚本以 version 字段是否匹配为守卫。脚本不是跨文件事务：后续锁文件命令失败时，前面的版本文件可能已经写入，应核对当前差异后恢复或继续，不得误报整次原子回滚。不要为测试而在正在使用的工作区来回 bump。

## 发布管道与产物

[release.yml](../.github/workflows/release.yml) 三个阶段：

1. `prepare-release`：从上一 tag 至当前 tag 生成提交列表，创建 draft；同一正文提供给 Release body 与 tauri-action 的 `releaseBody`，后者进入 `latest.json.notes`。
2. `build`：macOS、Linux、Windows matrix，安装锁定依赖，经 tauri-action 构建并上传安装包和更新资产。macOS 另对 dmg 公证、staple 后重新上传。
3. `finalize-updater-json`：只有三平台 build 成功后才下载合并的 `latest.json`、把 REST 资产 URL 改为浏览器下载 URL、校验后回传 draft。

实际目标与版本以 workflow、[tauri.conf.json](../src-tauri/tauri.conf.json) 和产物为准。配置中 `bundle.createUpdaterArtifacts` 开启，更新器包含公钥、发布源和前端 capability；这些配置存在不证明仓库私钥可用或本次签名成功。

发布前至少人工检查：

- 各目标安装包齐全，版本与 tag 一致，Release notes 可读。
- `latest.json.platforms` 至少有本仓目标对应的 `darwin-aarch64`、`linux-x86_64`、`windows-x86_64`，签名非空，URL 指向正确资产。
- 清单不残留 `api.github.com`，下载 URL 能正常取回产物；同一资产的别名平台键不能丢失。
- macOS 日志和实际 app/dmg 的签名、公证、staple 通过，不能仅看 secret 名存在。
- 用较旧的已安装版做检查、下载、校验、安装与重启验证，确认运行的是新版本。

**自动断言的真实边界**：当前收尾 job 只检查 platforms 非空、URL 不含 `api.github.com` 且以 `https://github.com/` 开头；没有自动保证三平台主键齐全、signature 非空或远端资产可下载。上述人工核对不能省略。

## macOS 签名与公证

Apple Developer ID 签名/公证与 updater 的 minisign 签名是两套独立机制。前者服务 Gatekeeper，后者让客户端验证更新字节；不能用一方成功推断另一方成功。

### 凭据与配置

证书需要包含私钥的 Developer ID Application `.p12`，身份串须与证书匹配。公证使用 App Store Connect API key：Key ID、Issuer ID 和 `.p8` 私钥不是同一个值。创建资格、当前角色要求和密钥管理以 [Apple 官方公证文档](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)、[API key 文档](https://developer.apple.com/documentation/appstoreconnectapi/creating-api-keys-for-app-store-connect-api) 与 [Tauri macOS 签名文档](https://v2.tauri.app/distribute/sign/macos/) 为准。

| GitHub secret | 内容 |
|---|---|
| `APPLE_CERTIFICATE` | 含私钥 `.p12` 的 base64 |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 导出密码 |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: … (TEAMID)` 完整身份串 |
| `APPLE_API_ISSUER` | App Store Connect Issuer ID |
| `APPLE_API_KEY` | API Key ID，不能填私钥正文或路径 |
| `APPLE_API_KEY_P8` | `.p8` 原始文件内容，非 base64 |
| `TAURI_SIGNING_PRIVATE_KEY` | updater minisign 私钥 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 与 updater 私钥匹配的密码；无密码密钥才可为空 |

凭据通过仓库 Secrets/安全凭据渠道配置，不进入仓库、聊天、截图或普通日志。不能打印私钥来确认内容。用 `security find-identity -v -p codesigning` 核对本机可用签名身份；若导出看不到私钥，应回创建证书的机器导出，或重新申请证书。

workflow 会逐项导出非空 Apple 变量；当 `APPLE_API_KEY` 非空时，写 `AuthKey.p8`、验证 BEGIN/END 行并设置**绝对路径** `APPLE_API_KEY_PATH`。bundler 在临时目录调用公证工具，相对路径会导致找不到文件。

- 所有 Apple 凭据都未配置时保留未签名构建路线。
- **部分缺失不能概括为安全降级**：例如 Key ID 存在而 P8 缺失会立即失败；其它不完整组合可能失败或仅签名未公证。变更后必须看实际日志和产物。
- bundler 签名、公证并 staple app；dmg 自己的公证票据由独立 `Notarize and staple dmg` 步骤补齐。只有 app 公证成功不足以证明 dmg 可通过安装评估。

维护记录登记过一张证书的到期日为 **2027-02-01**；未在本次文档整理中读取当前证书或线上 Secrets，发版负责人应核对是否已续期。续期需要更新证书、导出密码与匹配身份；API 公证密钥是独立生命周期。

### draft 阶段的本机验证

将 draft 的 app/dmg 下载到测试机后验证，路径替换为实际产物：

```bash
codesign -dv --verbose=4 /Applications/CodeWave.app
codesign --verify --deep --strict --verbose=2 /Applications/CodeWave.app
spctl -a -vv -t exec /Applications/CodeWave.app
xcrun stapler validate /Applications/CodeWave.app
xcrun stapler validate CodeWave_<version>_aarch64.dmg
```

检查签名链、TeamIdentifier、hardened runtime、Gatekeeper 的 Notarized Developer ID 结果和两份 staple；必要时在干净 macOS 首次打开验证。使用 `notarytool log <submission-id>` 查看拒绝原因，授权参数指向本地安全保存的 key 文件。不要因旧版曾公证成功便跳过新产物验证。

### 常见故障

| 症状 | 排查 |
|---|---|
| `The file couldn't be opened` / `MissingApiKey` | P8 是否真正写入、BEGIN/END 是否完整、路径是否绝对路径 |
| 公证 401/403 | Key ID、Issuer ID、有效权限与 key 状态；不把所有 403 都当网络故障 |
| identity 不匹配 | `.p12` 与完整 signing identity 是否属于同一证书 |
| 无法构建证书链 | 检查 Apple 中间证书，来源用 [Apple Certificate Authority](https://www.apple.com/certificate-authority/) |
| app 已公证但 dmg 被拒 | 检查独立 dmg 公证/staple/回传步骤是否执行成功 |
| updater 签名错误 | 核对更新字节、`.sig` 和配置公钥，Apple 签名无助于解决此错误 |

## 应用内更新契约

状态源是 [stores/updater.ts](../ui/src/stores/updater.ts)，流程在 [updateCheck.ts](../ui/src/utils/updateCheck.ts)，展示在 [UpdateModal.tsx](../ui/src/features/panels/UpdateModal.tsx)。

- 相位：idle、checking、available、manual-download、downloading、ready、up-to-date、error。相位和弹窗开关解耦，隐藏下载弹窗不取消下载；ready 和显式错误要重新打开弹窗。
- 每次检查递增 `checkEpoch`，陈旧结果丢弃并释放旧 Update 实例；`installInFlight` 防重复安装。失败时保留可重试实例，重试是重新下载，**不承诺断点续传**。
- 启动延迟 3 秒检查；无更新/失败保持安静，**有更新仍弹窗**。`ws_auto_update` 只有字符串 `false` 关闭，偏好在 localStorage，不属于 config.json；StrictMode 的“已经开始”标记在 timer 回调里置位。
- 显式检查无更新给 toast，失败给错误弹窗。Linux 由 userAgent 配合后端 `is_appimage` 判断；deb/rpm 走发布页手动下载，AppImage 才走自替换。平台判断有误判边界。
- Windows 的下载安装流程可能自行退出并启动安装器；ready 的手动重启入口主要服务 macOS/Linux，必须在真机按平台验收。
- 更新请求使用解析后的代理设置；无显式代理参数不等于可保证绕过系统代理，详见 [providers-and-network](./providers-and-network.md)。

### 发布说明与进度

`notes` 经公共 Markdown 渲染器和 useMemo 展示；保持 `html:false`、安全外链和 XSS 回归。空白 notes 归一为 null，不展示空区块。

下载事件按 chunk 到达，但**不能每个事件都写 store**：有总量按可见整数百分比变更，无总量按 64 KiB 步长；结束后强制写最终值。Channel 回调内部兜住异常，不能让坏事件中断后续有序回调。缺 content-length 时显示已下载字节，不伪造百分比。

已知边界：

- 前端降频不等于降低底层每 chunk 的 IPC 投递量；真机仍卡时以低速/全速对照判断投递和渲染瓶颈，不直接宣布上游已修。
- 更新弹窗不升级 Mermaid/数学占位符；发布说明不要依赖这两类渲染。
- CSP 的 `img-src` 不允许远端图片；Markdown 中的远端图可能显示失败。
- 发布说明样式复用公共 Markdown；局部修改需回归聊天、计划与更新弹窗，避免复制一套安全或主题实现。

## 更新下载 403 与清单修补

“能检查到更新、下载即 403”首先检查清单 URL。若指向 `api.github.com/repos/…/releases/assets/<id>`，客户端会走 GitHub REST 资产 API；响应含 rate-limit 文案且 `x-ratelimit-remaining: 0` 才能确认该类限流。插件给出 403 说明收到 HTTP 响应，但**不证明响应一定来自 GitHub**，代理也可能自行返回 403。

| 证据 | 处理 |
|---|---|
| API rate-limit 响应且浏览器下载 URL 可用 | 等窗口恢复或手动下载；发布端修清单 URL |
| 行政规则提示缺 User-Agent | 核对请求头与探针；探针本身不能制造假故障 |
| 代理自己的 HTML/拦截页 | 排查代理规则和出口，不把它误报成 GitHub 配额 |
| 连接拒绝/超时，无 HTTP 响应 | 检查端点、代理和网络连通性 |

当前 UI 用宽松正则把 `403/429/rate limit` 归为配额指引，这不是精确诊断；维护排查须看响应头、正文和实际 URL，不能只复述“切代理无效”。

### 发布收尾规则

[scripts/fix-updater-json.mjs](../scripts/fix-updater-json.mjs) 根据 release 资产 **id → name 权威映射**生成 `https://github.com/<owner>/<repo>/releases/download/<tag>/<encoded-name>`：

- 不按平台名猜资产名；同一 id 的多个平台键使用同一 URL。
- 保持 signature 原样，URL 替换不改变签名对象；已是浏览器下载 URL 的条目保持幂等。
- 先完整验证再写文件，未知 id、空资产名或残留 API URL 均应失败。
- 收尾必须在全部平台上传完后运行，否则后完成的 tauri-action 清单合并会覆盖改写。
- Windows 默认键可能指向 MSI，另有 NSIS 键；改写只改变下载通道，不能顺手更换资产类型。

存量 release 不会自动追溯修补。经用户明确授权某个线上版本后，才可下载该版本清单、运行脚本验证并用 `gh release upload <tag> latest.json --clobber` 覆盖资产；不要把幂等误当作无需授权。

`releases/latest/download/latest.json` 与固定 tag 下载路径都可能受 CDN 缓存影响，固定 tag 或随机 query 不保证绕缓存。排查时结合 ETag/Age，读取有权限访问的 release 资产或等待缓存；草稿需有写权限的凭证，并可先用 `gh release view <tag> --json databaseId` 取 id，再查资产，不因公开 tag API 404 就断言 draft 不存在。

## 人工验收清单

1. 确认无同 bundle id 的另一实例，以旧打包版检查更新，核对新旧版本与 notes Markdown。
2. 下载时进度有界且最终完成；隐藏后仍下载，成功后弹窗重新出现；未知总量不显示 NaN。
3. 模拟断网或不可达更新源，显式检查有错误反馈；恢复后重试可用，静默检查失败不打扰。
4. 重启进入新版本；Windows 安装器、Linux deb/rpm 手动路径与 AppImage 自更新分别验证。
5. 核对 macOS app/dmg 签名和 staple；未验证的目标明确标记，而不是据一个平台推断全部通过。
6. 三平台安装包、更新资产、签名、清单 URL 和下载结果都正确后，人工 Publish draft。

自动回归入口见 [testing](./testing.md)。模拟更新源脚本 [mock-updater-endpoint.mjs](../scripts/mock-updater-endpoint.mjs) 仅用于隔离测试配置；临时 endpoint 修改不能进入正式发布配置。
