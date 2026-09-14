# macOS 签名与公证操作指南（Developer ID + Notarization）

> 目标：让 GitHub Releases 分发的 CodeWave `.dmg`/`.app` 带 Developer ID 签名并通过 Apple 公证（notarize + staple），
> 用户下载后 Gatekeeper 直接放行，不再出现「已损坏，无法打开」或需要右键放行。
> 参照 GitWave 的已验证实现（`../GitWave/docs/tasks/feat-macos-code-signing/plan.md` +
> `../GitWave/.github/workflows/build.yml`，同款 tauri-action `action-v1.0.0`，v0.3.0 起生产实测）；
> GitWave 实际踩过的坑在本文对应步骤标注。

## 0 现状与结论

| 项 | 现状 |
|---|---|
| 发布管道 | `.github/workflows/release.yml`：推 `v*` tag → draft Release → 三平台 tauri-action 构建 |
| 签名入口 | 已预留 `Export optional macOS signing envs` 步骤（release.yml:123-140）：5 个 `APPLE_*` secret **非空才导出**，全部未配置 → 未签名产物（当前状态） |
| 管道缺口 | **`AuthKey.p8` 无人写入**：workflow 只设了 `APPLE_API_KEY_PATH=AuthKey.p8`，但 tauri-action 不负责写该文件（源码已核对无此逻辑），公证时会因找不到 `.p8` 报 `MissingApiKey` / `The file couldn't be opened` → 阶段五一次性修补 |
| 配置改动 | `src-tauri/tauri.conf.json` 无需改动（签名/公证全程环境变量驱动） |

Tauri bundler 读取的环境变量语义（核对自 [tauri-bundler sign.rs 源码](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-bundler/src/bundle/macos/sign.rs)）：

| 环境变量 | 含义 |
|---|---|
| `APPLE_CERTIFICATE` | base64 编码的 `.p12` 证书（CI 中 bundler 自动导入临时钥匙串，无需手动 `security create-keychain`） |
| `APPLE_CERTIFICATE_PASSWORD` | 该 `.p12` 的导出密码 |
| `APPLE_SIGNING_IDENTITY` | 签名身份完整串，如 `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_API_ISSUER` | App Store Connect API Issuer ID（UUID） |
| `APPLE_API_KEY` | **API Key ID**（10 位字母数字——不是密钥内容，也不是路径） |
| `APPLE_API_KEY_PATH` | `.p8` 私钥文件路径；**必须绝对路径**（bundler 在临时目录调用 notarytool，相对路径报 `The file couldn't be opened`，GitWave v0.3.0 实测踩坑） |

6 个变量配齐后，`tauri build` 全自动完成：临时钥匙串导入证书 → codesign 签 `.app`（hardened runtime）→ notarytool 公证 → stapler 回签（staple）`.dmg`。

## 1 阶段一 · Apple Developer Program 与 Team ID

公证需要付费的 Apple Developer Program 会员（免费账号不能公证）。

1. 注册/确认会员：<https://developer.apple.com/programs/>（99 美元/年）。个人账号即以本人身份签发；组织账号需 D-U-N-S 编号。
2. 记下 **Team ID**（10 位字母数字）：<https://developer.apple.com/account> → Membership details。后文身份串和验证输出都会用到。

角色注意：创建 Developer ID Application 证书需要 **Account Holder**（个人账号即本人）；组织账号由 Account Holder 操作，或由持有人创建后用其他开发者的 CSR 绑定（[Tauri 文档](https://v2.tauri.app/distribute/sign/macos/)）。

## 2 阶段二 · 制作 Developer ID Application 证书

> 钥匙串里已有此证书的可跳过创建，直接从下面第 2 步（导出 `.p12`）开始。

**创建证书（二选一）**

- 路径 A（推荐）：Xcode → Settings → Accounts → 选中 Apple ID 与 Team → Manage Certificates… → 左下 `+` → **Developer ID Application**。证书自动进入「登录」钥匙串。
- 路径 B（网页 + CSR）：
  1. 「钥匙串访问」→ 菜单 钥匙串访问 → 证书助理 → 从证书颁发机构请求证书… → 填邮箱与常用名 → 存储到磁盘，得到 `.certSigningRequest`；
  2. <https://developer.apple.com/account/resources/certificates/list> → `+` → 选 **Developer ID Application** → 上传 CSR → 下载 `.cer`；
  3. 双击 `.cer` 导入钥匙串。

**校验与导出**

```bash
# 1. 校验证书存在，记下完整身份串（引号内全部内容，含括号里的 Team ID）
security find-identity -v -p codesigning
# 期望出现：
#   1) XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX "Developer ID Application: Your Name (TEAMID)"

# 2. 导出 .p12（给 CI 用）：钥匙串访问 → 登录 → 我的证书 → 右键该证书 → 导出…
#    格式选 .p12，设一个密码（即 APPLE_CERTIFICATE_PASSWORD），存到临时位置如 ~/Downloads/codewave-sign.p12

# 3. base64 编码（给 APPLE_CERTIFICATE secret 用）
base64 -i ~/Downloads/codewave-sign.p12 | pbcopy   # 已复制到剪贴板，直接粘贴进 GitHub secret
```

注意：`.p12` 必须包含私钥（导出的是「证书 + 私钥」对）。若导出时没有私钥可选，说明证书不是在这台 Mac 创建的——从创建它的机器导出，或用路径 B 重做。

## 3 阶段三 · 制作 App Store Connect API Key（公证凭据）

公证用 API key 方式（而非 Apple ID + 专用密码），与 GitWave / 本仓库 CI 管道一致：

1. 打开 <https://appstoreconnect.apple.com/access/integrations/api>（App Store Connect → 用户和访问 → 集成 → App Store Connect API → 团队密钥）→ `+` 新建密钥。
2. 名称随意（如 `codewave-notary`），角色选 **App Manager**（App 经理）——electron/notarize 要求此角色；Tauri 文档称 Developer 亦可，但 Developer 角色 key 有公证 403 的社区反馈；Admin 一定能用但权限过宽，不推荐。
3. 记录两项：**Issuer ID**（密钥页上方的 UUID）与 **Key ID**（密钥表中 10 位）。
4. 点「下载」保存 `.p8` 私钥——**只能下载一次**，关掉弹窗就再也拿不到（丢了只能撤销重建）。存到 bundler 的默认搜索目录最省事：

```bash
mkdir -p ~/.appstoreconnect/private_keys
mv ~/Downloads/AuthKey_XXXXXXXXXX.p8 ~/.appstoreconnect/private_keys/
```

放在这个位置后，本机跑 `tauri build` 无需再设 `APPLE_API_KEY_PATH`（bundler 会自动在 `~/.appstoreconnect/private_keys/` 找 `AuthKey_<KeyID>.p8`）。

参考：[Creating API keys for App Store Connect API](https://developer.apple.com/documentation/appstoreconnectapi/creating-api-keys-for-app-store-connect-api) · [App Store Connect 角色权限](https://developer.apple.com/help/app-store-connect/reference/role-permissions) · [electron/notarize 对角色的要求](https://github.com/electron/notarize/blob/main/README.md)

## 4 阶段四 · 配置 GitHub secrets（6 个）

仓库 GitHub 页 → Settings → Secrets and variables → Actions → New repository secret；命令行方式推荐文件型 secret 用 `gh secret set NAME < file`，避免网页粘贴引入换行问题。

| Secret 名称 | 内容 | 来源 |
|---|---|---|
| `APPLE_CERTIFICATE` | `.p12` 的 base64 | 阶段二 `base64 -i … \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 导出时设的密码 | 阶段二 |
| `APPLE_SIGNING_IDENTITY` | 完整身份串 | 阶段二 `security find-identity` 引号内全部 |
| `APPLE_API_ISSUER` | Issuer ID（UUID） | 阶段三 |
| `APPLE_API_KEY` | Key ID（10 位） | 阶段三 |
| `APPLE_API_KEY_P8` | `.p8` **原始文件内容**（非 base64） | `gh secret set APPLE_API_KEY_P8 < ~/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8` |

注意：

- 本仓库 release.yml 是「非空才导出」的条件语义——secret 置空字符串等价于未配置，不需要删除 secret 来关闭签名；
- 6 个一次配齐：只配签名不配公证 → 产物已签名但未公证（Gatekeeper 仍拦首次打开）；只配公证不配签名 → 签名环节就过不去。

## 5 阶段五 · release.yml 管道补缺（一次性）

当前缺口见 §0：`AuthKey.p8` 无人写入，且现有 `APPLE_API_KEY_PATH=AuthKey.p8` 是相对路径（公证时必踩 `The file couldn't be opened`，GitWave v0.3.0 同坑）。

将 `Export optional macOS signing envs` 步骤（release.yml:123-140）替换为（新增 `APPLE_API_KEY_P8` 透传 + 写文件 + 绝对路径；写文件完整性守卫照抄 GitWave build.yml）：

```yaml
      - name: Export optional macOS signing envs
        if: runner.os == 'macOS'
        env:
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_API_ISSUER: ${{ secrets.APPLE_API_ISSUER }}
          APPLE_API_KEY: ${{ secrets.APPLE_API_KEY }}
          APPLE_API_KEY_P8: ${{ secrets.APPLE_API_KEY_P8 }}
        run: |
          for name in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_API_ISSUER APPLE_API_KEY; do
            value="${!name}"
            if [ -n "$value" ]; then
              echo "$name=$value" >> "$GITHUB_ENV"
            fi
          done
          if [ -n "$APPLE_API_KEY" ]; then
            printf '%s\n' "$APPLE_API_KEY_P8" > AuthKey.p8
            grep -q '^-----BEGIN PRIVATE KEY-----$' AuthKey.p8 || {
              echo "::error::APPLE_API_KEY_P8 缺少 BEGIN 私钥行，内容损坏；用 gh secret set APPLE_API_KEY_P8 < AuthKey_XXX.p8 重设"
              exit 1
            }
            grep -q '^-----END PRIVATE KEY-----$' AuthKey.p8 || {
              echo "::error::APPLE_API_KEY_P8 缺少 END 私钥行，内容损坏；用 gh secret set APPLE_API_KEY_P8 < AuthKey_XXX.p8 重设"
              exit 1
            }
            echo "APPLE_API_KEY_PATH=${{ github.workspace }}/AuthKey.p8" >> "$GITHUB_ENV"
          fi
```

「全部 secret 未配置 → 仍出未签名产物」的既有语义保持不变。

两点说明：

- 此补丁与 GitWave 最终实现等价（其 `.github/workflows/build.yml` 的 build-macos job）。GitWave plan.md 里提到的手动钥匙串导入步骤（`security create-keychain` + `set-key-partition-list` + `KEYCHAIN_PASSWORD` secret）在最终版并未采用——bundler 从 `APPLE_CERTIFICATE` 自建临时钥匙串即可；
- 补丁属于代码改动，需随签名配置一并 commit（可并入该次发版的准备提交，或独立 `ci(release): wire macOS signing secrets` 提交）。

## 6 阶段六 · 走一次发版验证

按既有流程（[docs/version-bump-and-release](./version-bump-and-release.md) / `/codewave-release`）：门禁 → `pnpm bump` → commit → 确认 → 推 tag → CI 构建 **draft** Release。

**在 draft 阶段（点 Publish 之前）完成校验**——公证失败或产物不对时把 draft 删掉重来即可，不影响已发布版本：

```bash
# 下载 draft 里的 CodeWave_x.y.z_aarch64.dmg 并安装到 /Applications

# 1. 签名详情：应看到三层 Authority（Developer ID Application → Developer ID Certification
#    Authority → Apple Root CA），且 flags 含 runtime（hardened runtime）、TeamIdentifier 为你的 Team ID
codesign -dv --verbose=4 /Applications/CodeWave.app

# 2. 签名完整性：无报错（exit 0）即通过
codesign --verify --deep --strict --verbose=2 /Applications/CodeWave.app

# 3. Gatekeeper 评估：期望 accepted + source=Notarized Developer ID
spctl -a -vv -t exec /Applications/CodeWave.app

# 4. 公证票据（.app 与 .dmg 都验）：期望输出含 validated
xcrun stapler validate /Applications/CodeWave.app
xcrun stapler validate CodeWave_x.y.z_aarch64.dmg

# 5.（可选）查公证历史记录
xcrun notarytool history --key ~/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8 \
  --key-id <KeyID> --issuer <IssuerID>

# 6.（可选）在另一台/全新 macOS 上首次打开，应无任何拦截弹窗
```

全部通过 → 回 GitHub 手动 Publish。发布后按惯例核对 `latest.json` 是否含三平台条目（[docs/version-bump-and-release](./version-bump-and-release.md) §4 的已知关注点）。

## 7 阶段七 · 本机手动签名 + 公证（可选）

证书在钥匙串、`.p8` 在默认目录的前提下：

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
export APPLE_API_ISSUER="<Issuer ID>"
export APPLE_API_KEY="<Key ID>"
# .p8 不在 ~/.appstoreconnect/private_keys 时才需要：
# export APPLE_API_KEY_PATH="$HOME/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8"
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/codewave.key)"   # updater 产物签名（createUpdaterArtifacts: true 必需）
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""

pnpm tauri build
```

- 只签名不公证（快速本地验证）：只设 `APPLE_SIGNING_IDENTITY`，不设 API 三件套；
- 正式发版仍以 CI 为准——本地全量公证会消耗公证额度，且 CI 环境干净可复现。

## 8 附录

**排查**

| 症状 | 原因 / 处理 |
|---|---|
| CI 公证步报 `The file couldn't be opened` | `APPLE_API_KEY_PATH` 是相对路径 → 用 `${{ github.workspace }}/AuthKey.p8`（GitWave v0.3.0 同坑） |
| CI 报 `MissingApiKey` | `.p8` 文件未写入 workspace → 确认阶段五补丁在位 |
| 公证 401 / 403 | API key 角色不够（建议 App Manager），或 Key ID / Issuer ID 抄错 |
| `does not match provided identity` | `APPLE_SIGNING_IDENTITY` 与 `.p12` 不一致 → 原样粘贴 `security find-identity` 引号内全串 |
| `unable to build chain to self-signed root` | 缺 Apple 中间证书 → 从 <https://www.apple.com/certificate-authority/> 下载安装对应 Developer ID 中间证书 |
| 公证被拒，要看具体违规项 | `xcrun notarytool log <submission-id> --key … --key-id … --issuer …` 查看详情 |
| 证书导出找不到私钥 | 证书不是本机创建 → 从创建机器导出 `.p12`，或重做证书 |

**与自动更新（updater）的关系**：minisign 更新链路（`TAURI_SIGNING_PRIVATE_KEY`）与 Apple 签名互相独立，互不影响。存量未签名旧版用户经应用内更新升到签名版无额外迁移——updater 校验的是 minisign 签名，不检查 Apple 签名。

**dmg 不会被 tauri-bundler 公证（v0.3.4 实测）**：bundler 的流程是「公证并 staple `.app` → 打 dmg → 仅 codesign 签名 dmg」，dmg 本身不带公证票据，`spctl -a -t install` 会判 `rejected / Unnotarized Developer ID`（打开 dmg 时可能触发 Gatekeeper 提示；app 因自带 staple 不受影响）。根治：release.yml 已加「Notarize and staple dmg」步骤（CI 内下载 dmg → `notarytool submit --wait` → `stapler staple` → 校验 → `--clobber` 回传 draft；未配置公证凭据时自动跳过）。本机手动命令仍可作应急路径：`xcrun notarytool submit <dmg> --key ~/.appstoreconnect/private_keys/AuthKey_XXXX.p8 --key-id <KeyID> --issuer <IssuerID> --wait` → `xcrun stapler staple <dmg>` → `gh release upload v<X.Y.Z> <dmg> --clobber`。

**证书续期**：Developer ID Application 证书有效期数年（到期日以钥匙串访问中证书详情为准）。现用证书（`Frank Yang (ZE5SZ85EZQ)`，指纹 `218B3C5A…7AFB`）**2027-02-01 到期**，建议提前一两周续期。续期 = 重新执行阶段二 + 更新 `APPLE_CERTIFICATE` / `APPLE_CERTIFICATE_PASSWORD` / `APPLE_SIGNING_IDENTITY` 三个 secret；公证凭据不受影响。

**自定义 entitlements**：现阶段不需要——bundler 默认 entitlements 已覆盖 WKWebView 的 JIT 需求；将来确有需要时在 `tauri.conf.json` 的 `bundle.macOS.entitlements` 配置。

**参考**

- [Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution) — 公证总览与要求
- [Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow) — notarytool 用法
- [Tauri · macOS Code Signing](https://v2.tauri.app/distribute/sign/macos/) — 证书类型与环境变量语义
- [electron/notarize README](https://github.com/electron/notarize/blob/main/README.md) — API key 角色要求
- GitWave 参照实现：`../GitWave/docs/tasks/feat-macos-code-signing/plan.md` + `../GitWave/.github/workflows/build.yml`
