# 自动更新下载 403 Forbidden：成因、排查与修复

> 2026-09-18 事故记录与修复。现象：应用内「检查更新」提示「发现新版本 v0.3.8，正在下载…」后立刻失败
> `更新下载或安装失败：Download request failed with status: 403 Forbidden`。
> 结论：**不是网络问题、不是代理问题、与签名无关**——清单把下载地址指向了 GitHub 的 REST 资产 API 端点，
> 而该端点对匿名请求只有 60 次/小时/出口 IP 的配额。

## 1 根因

`latest.json`（更新清单）里每个平台条目的 `url` 形如：

```
https://api.github.com/repos/Yangshifu1024/CodeWave/releases/assets/572426046
```

这是 GitHub 的 **REST 资产 API**，不是下载地址。tauri-plugin-updater 拿到清单后直接对它发 GET，于是：

- 匿名请求该 API 的配额是 **60 次/小时/出口 IP**，耗尽返回 **403**（少数情况 429）；
- 配额按 **IP** 计，不区分应用、用户或仓库——共享出口（家宽 CGNAT、机场节点、公司 NAT）下极易被打满；
- `latest.json` 自身与安装包走的是 `github.com` 域名（Azure Blob + CDN 承载），**不消耗该配额**，所以「检查更新」总是成功、只有「下载」失败，这个反差正是本案最典型的指纹。

这个 url 由 `tauri-action@action-v1.0.0` 的 `src/upload-version-json.ts` 生成，写死为：

```ts
url: `${githubBaseUrl}/repos/${owner}/${repo}/releases/assets/${data.id}`
```

`githubBaseUrl` 默认 `https://api.github.com`，且**没有任何输入项可以切换成浏览器下载地址**（同一个
baseUrl 还用于认证调用，改成 github.com 会 404）。所以这是上游动作的既定行为，只能在下游纠正。

## 2 代理为什么不是解药

排查时最容易走错的一步就是去配代理。**换出口 IP 只是换一份「别人的预算」，不改变被拒绝这件事本身。**

两个可直接验证的事实：

1. **403 是穿透代理拿回来的 HTTP 状态码。** 报错文案 `Download request failed with status: 403 Forbidden`
   来自 tauri-plugin-updater 的下载分支（`updater.rs`：`if !response.status().is_success()` 后拼
   `Download request failed with status: {status}`）。能拿到状态码说明请求已经成功穿过代理抵达 GitHub
   并收到应答；若代理本身不通（端口未监听、节点不可用、规则 reject），报的会是 reqwest 的连接错误，而不是状态码。
2. **代理在下载阶段确实生效。** `updater.rs` 的 `download()` 内部复用 `check()` 传入的代理：

   ```rust
   if self.no_proxy { request = request.no_proxy(); }
   else if let Some(ref proxy) = self.proxy {
       let proxy = reqwest::Proxy::all(proxy.as_str())?;
       request = request.proxy(proxy);
   }
   ```

   即「配了代理仍 403」是预期内的，恰好证明链路没有走错。

顺带一提，「无代理」模式并不是真的直连：reqwest 默认启用 `system-proxy` 特性，`proxy: null` 时
仍跟随系统代理（[docs/network-proxy-settings](./network-proxy-settings.md)）。所以把设置从「系统代理」
换成「自定义代理 + 同一个 7890」等于什么都没改。

## 3 三类 403 的区分方法

| 成因 | 判别特征 | 处置 |
|---|---|---|
| **匿名配额耗尽**（本案） | 响应体含 `API rate limit exceeded for <IP>`；响应头 `x-ratelimit-remaining: 0`、`x-ratelimit-used: 60`；`github.com/.../releases/download/...` 同时可正常访问 | 等配额窗口重置（`x-ratelimit-reset`，HTTP 日期）、手动下载安装包；根治见第 4 节 |
| **缺 User-Agent** | 响应体为 `Request forbidden by administrative rules. Please make sure your request has a User-Agent header` | 与本项目无关：插件显式设置 `UPDATER_USER_AGENT`（`tauri-plugin-updater/<版本>`），排查脚本别漏了 UA 造成误判 |
| **代理返回 403** | 响应体是代理自家的 HTML（如 Cloudflare 拦截页）、无 `x-ratelimit-*` 头 | 调整代理规则；注意 `api.github.com` 是否被规则漏到 DIRECT / 落到已耗尽额度的出口 |

判别命令（把 `<owner>/<repo>`、`<id>`、`<ip>` 换掉）：

```bash
# 匿名配额现状（200 也可能 remaining: 0）
curl -s -H 'User-Agent: probe' https://api.github.com/rate_limit

# 资产 API 端点：命中限流时 403 + rate limit 文案
curl -sS -o /dev/null -w '%{http_code}\n' -H 'User-Agent: probe' \
  https://api.github.com/repos/<owner>/<repo>/releases/assets/<id>

# 同一个资产的 CDN 路径：不受配额影响
curl -sS -o /dev/null -w '%{http_code}\n' \
  https://github.com/<owner>/<repo>/releases/download/<tag>/<assetName>
```

注意别用同样被限流的域名去判断「网络好不好」：`github.com` 通不代表 `api.github.com` 通，两个域要分别验。

## 4 修复

### 4.1 发布收尾改写清单（根治）

`.github/workflows/release.yml` 新增作业 `finalize-updater-json`，在**三平台 build 全部完成之后**：

1. `gh release download <tag> --pattern latest.json`
2. `node scripts/fix-updater-json.mjs --tag <tag> --in latest.json --out latest.json`
   —— 按 asset id → name 的**权威映射**把 url 改写成
   `https://github.com/<owner>/<repo>/releases/download/<tag>/<encodeURIComponent(name)>`
3. **断言**：改写后的清单里不存在 `api.github.com`，且所有 url 以 `https://github.com/` 开头
4. `gh release upload <tag> latest.json --clobber`

断言排在回传**之前**是刻意的：不过关就不覆盖 draft 上的旧清单。

为什么必须是独立作业且排在 build 之后：三个平台 job 各自在 tauri-action 内 **delete + upload**
latest.json 做合并（后完成者产出最终态），过早改写会被后续 job 覆盖。

改写注意点：

- **同一 asset id 服务多个平台条目**（实测 v0.3.8：`572426046` 同时是 `darwin-aarch64` 与
  `darwin-aarch64-app`；`572428553` 服务 AppImage；`572436176` 服务 msi），必须**按 id 建映射统一替换**，
  逐条改名会产生两个不同 URL 或丢条目；
- 只能从 API 资产列表取 name，**不要按平台名猜文件名**（bundler 本地产物名与 tauri-action 上传名不同，
  会带版本号与架构后缀）；
- `signature` 字段原样保留——minisign 校验只针对下载字节与 `tauri.conf.json` 的公钥，与 URL 无关；
- 已是 `releases/download/` 前缀的 url 保持幂等，不重复改写。

### 4.2 存量 release 修补

已发布的 release 不会自动受益（清单是发布时点生成的），需对该版本补跑一次（幂等，可安全重跑）：

```bash
gh release download v0.3.8 --pattern latest.json --clobber
node scripts/fix-updater-json.mjs --tag v0.3.8 --in latest.json --out latest.json
gh release upload v0.3.8 latest.json --clobber
```

**这是写操作，会改动线上 release 资产**，须经用户明确授权后执行。旧版客户端如果失败，临时出路是
到 Releases 页面手动下载安装包（`github.com` 域名，不受配额影响）或等配额窗口重置。

### 4.3 用户侧可观测性

`ui/src/utils/updateCheck.ts` 对下载失败做了原因识别：命中 `403` / `429` / `rate limit` 时改用专用文案
（`notice.updateFailedRateLimited`，zh-CN 与 en-US 成对），明确告知「配额已耗尽 → 稍后重试或手动下载，
**切换代理无法解决**」。其余错误仍走通用 `notice.updateFailed`（带原始错误串）。

## 5 已知边界

- **CDN 缓存绕不开**：`releases/latest/download/latest.json` 由 Varnish/Azure 缓存（响应头 `x-cache`、`age`），
  GitHub 不提供 purge。刚发布或刚修补后短时间内可能仍读到旧清单，属预期。
  **特别注意：tag 固定路径（`releases/download/<tag>/latest.json`）并不绕开缓存**——实测它与 latest
  路径返回同一个 `etag`、相同的 `age`（同一份缓存对象），加随机 query 也不重置。要确认线上清单的真实内容，
  请直接读 release 资产（`gh api repos/<owner>/<repo>/releases/tags/<tag>`）或等待缓存 TTL 过期。
- **默认 Windows 键语义**：tauri-action 产出的 `windows-x86_64` 可能指向 MSI 而非 NSIS（v0.3.8 实测
  `windows-x86_64` 与 `windows-x86_64-msi` 同 id，`windows-x86_64-nsis` 才指向 `x64-setup.exe`）。
  改写脚本按 id 忠实映射、**目标文件不变**（只换传输通道），行为与修复前一致；键语义本身是独立议题。
- **签名/公证链路不受影响**：本次只改 URL，未触碰密钥、公钥与 macOS 公证配置。
- **同类项目同病**：任何使用 tauri-action 默认清单生成的 Tauri 项目都产出相同的 api 端点 url；
  GitWave 的清单形态与本文案一致，同样需要处理（已另外登记，不混入本次改动）。

## 6 验证

- 单测：`node --test "scripts/**/*.test.mjs"`（覆盖同 id 多平台条目、percent-encode、幂等、
  未知 id / 空文件名报错、`__proto__` 平台键不丢失、宽松判据（大小写 host / 尾随斜杠）、
  tag 与文件名版本号不混淆、真实 v0.3.8 形状整体断言；**glob 必须用双引号包裹**——
  `node --test <目录>` 在 Node 22/24 上都会报 `Cannot find module`）
- 本地 dry-run：对真实清单跑 `--dry-run`，断言 9/9 平台 url 改写为 `releases/download/` 形态且无残留
- 端到端（用户手动）：装旧版 → 检查更新 → 下载不再 403 → 重启后版本号更新。
  最有说服力的回归：先打满本机匿名配额（连续请求 `api.github.com/rate_limit` 至 `remaining: 0`），
  此时旧版必然复现 403（对照组）、修复版应仍成功。
