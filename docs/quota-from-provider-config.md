# 额度段改为按 CodeWave 供应商配置驱动

> 2026-09-20 · 本批把右栏「订阅额度」段的数据源从 **OpenCode 凭证链**（环境变量 / `~/.config/opencode/opencode.jsonc` / `~/.local/share/opencode/auth.json`）改为 **CodeWave 自己配置的供应商列表**，并让「不支持查询」的供应商降噪呈现。
> 需求与逐条决策（43 问）见 `.codewave/tasks/20260920-121925-quota-from-config/{requirement.md,plan.md}`。

## 1. 问题与目标

旧口径下，面板显示哪些提供商完全由**别的工具**（opencode）的环境与凭证决定，与用户在 CodeWave 里配了什么无关：配了 OpenAI 官方或自建网关看不到额度，反而会看到自己没在用的 DeepSeek key。用户要求两条：

1. 显示**所有添加到供应商中的订阅**；
2. 对应供应商**不支持查询则不显示**。

落地口径（第 2 条的「不显示」）= **降噪折叠**：不支持者默认不占独立行（≤3 家平铺、>3 家折叠为一行可展开汇总），而不是整条消失在无解释的黑箱里。

## 2. 数据流

```
config.providers（~/.codewave/config.json）
  │
  ├─ host/keyring::resolve_provider_keys(&ProviderConfig)        ← 新增 provider 级入口（account = provider.id）
  │      Err  → invalid 行（错误行「密钥读取失败」，重试无用）
  │      []   → no_key 行
  │      keys → 只取 keys[0]（keys 是池，不轮换、不多 key 各一行）
  │
  ├─ core/quota::snapshots(cfg, active_provider_id, data_dir)
  │      base_url 空        → unsupported{ empty_base_url }（不发请求）
  │      域名不命中 7 家白名单 → unsupported{ no_adapter }（不发请求）
  │      其余：providers::fetch(kind, keys[0], client)（全并发，无并发上限）
  │            成功                        → ok
  │            401/403/404 且从未成功过     → rejected（降级灰行）
  │            401/403/404 但曾成功过       → error
  │            其它（超时/连接/5xx/429/解析）→ error
  │      排序：可查询类（ok/error/invalid/no_key/rejected）在前 —— active_provider_id 命中者置顶、其余按配置顺序；
  │            unsupported 殿后（按配置顺序）
  │      批次末：state::commit（临界区内 load → 清孤儿 → record_ok 全部成功者 → save，一次写盘）
  ▼
quota_snapshots(active_provider_id) → QuotaSection（五类行 / 折叠 / 去设置跳转 / 配置变化重拉）
```

## 3. 行态与展示

| status | 判定 | 展示 | 折叠 |
|---|---|---|---|
| `ok` | 取数成功 | 名称 + 最紧张窗口 + 剩余% + 进度条；点开全部窗口明细 + 重置倒计时 | — |
| `error` | 超时 / 连接 / 5xx / 429 / 解析失败；或 401·403·404 但**曾成功过** | `!` 标记 + 展开原因 + 重试；带历史 `last_ok_at` 时展开显示「上次成功：…」 | 不折叠 |
| `invalid` | keyring 回读失败（`keys` 含占位符但读不出来） | 同错误行（`quotaInvalid`「密钥读取失败：…」） | 不折叠 |
| `rejected` | 401/403/404 **且从未成功过** → 名额/账号被拒 | 降级灰行「查询被拒（可能非订阅账号）」+ 展开原因 + 「从未成功查询」+ 重试 + 去设置 | 不折叠 |
| `no_key` | `keys` 为空（或全空串） | 只读行「未配置密钥」+ 去设置 | 不折叠 |
| `unsupported` | `base_url` 为空（`empty_base_url`）/ 域名不命中白名单（`no_adapter`） | 灰行「不支持额度查询」+ 原因标签（「未填 base URL」/「域名不在支持范围」）+ 去设置 | **>3 家折叠**为一行汇总 |

补充规则：

- **标题消歧**：`display_name` 同名 → 补 `base_url` 主机名后缀；**名字与主机名都相同**（同家两账号）→ 再追加序号 `(2)`、`(3)`。
- **上次成功时间**：判据是**数据**（`last_ok_at` 是否存在），不是行态白名单 —— `ok` 行不显示（避免与「X 分钟前更新」重复）；`error`/`invalid` 有值才显示；`rejected` 恒显示时间行（无值 → 「从未成功查询」）。
  这条曾写反过：按冻结分类，`rejected` 的 `last_ok_at` **恒为 null**（从未成功），真正带历史时间的是 `error` 行；只按行态白名单渲染会导致该信息在真实数据下永不出现。
- **窗口级 `status` 上屏**：`rate-limited` / `exceeded` / `blocked` / `limited`（大小写与 `-`/`_` 归一）→ 橙色「已限流」内联标签；其它非 `ok` 值**原样灰色显示**（上游新增取值不会崩）；危险语义仍只由「剩余 0% 的红条」承担。
- **密钥来源 tooltip**：按**实际取用的那一枚 key** 判定 → 「系统钥匙串」（keyring 回读）/「配置文件」（明文）。不再出现 `env:` / `auth.json` / `jsonc` 字样。
- **空态**（一个供应商都没配）：一行说明「额度按你在 CodeWave 中配置的供应商显示；还没有添加供应商」+ 去设置按钮。
- **窄栏**（右栏默认 328px）：原因标签可截断 + 悬浮补全；去设置按钮只留图标；行高恒定。
- **刷新**：进入面板一次 + 每 5 分钟 + 手动按钮 + **供应商配置变化时重拉**（监听 providers 的 id/name/base_url/keys 与当前会话模型，debounce 500ms；无关配置变化不触发）；面板隐藏即停。

## 4. 契约变更

| 契约 | 前 | 后 |
|---|---|---|
| IPC | `quota_snapshots(active_base_url)` | `quota_snapshots(active_provider_id)` |
| `QuotaSnapshot.provider_id` | `ProviderKind::id()`（`opencode-go` …） | **provider uuid**（= `config.providers[].id`） |
| 凭证来源字段 | `credential_source`（`env:NAME` / `auth.json` / `opencode.jsonc`） | `key_source`：`keyring` / `config` |
| `QuotaSnapshot.status` | `ok` / `invalid` / `error` | 6 值（见上表）+ `reason`（`no_adapter` / `empty_base_url`） |
| `QuotaSnapshot.last_ok_at` | 无 | `Option<String>`（RFC3339；ok 行 = 本次成功时刻） |
| `QuotaEntry.status` | 无 | `Option<String>`（窗口级上游 status） |
| 新文件 | — | `~/.codewave/quota.json` |
| 事件面 | 28 键 | **不变**（未新增事件） |
| localStorage `ws_rb_quota_expanded` | 存 kind 串（`opencode-go` …） | 存 uuid；读取时按当前 providers 过滤，旧值一次性失效（表现为已展开行折叠一次），脏值不回写 |

## 5. `~/.codewave/quota.json`

```json
{
  "version": 1,
  "verified": [ { "provider_id": "<uuid>", "last_ok_at": "2026-09-20T04:42:06Z" } ]
}
```

- **全局**数据目录下（额度是账号级，与项目无关）；文件权限 `0600`（实测 `-rw-------`），**不含任何密钥**。
- 「读时按当前 providers 过滤、写时清孤儿」；**一次刷新批次只写一次盘**；全失败且无孤儿则完全不写。
- **并发安全**：`state::commit` 在进程内互斥临界区里完成「重新 load → 清孤儿 → 记录本批成功者 → 原子 save」，且临界区内**只有同步文件 IO、不跨 `.await`**。
  为什么需要它：两次刷新并发（手动连点 + 定时器）若各自「load → 内存改 → save」，后写会覆盖前写、丢掉某家的 `last_ok_at` → 该家下次 401/403/404 会被判成「从未成功」而降级为 `rejected` 灰行，正是本批要消灭的「额度静默消失」。
- **启动期自愈**（[docs/quota-state-heal](./quota-state-heal.md)）：每次启动在 `lib.rs` 的 setup 里同步调一次 `core::quota::state::heal(&data_dir)`（与日志清理 / 崩溃恢复 / 幽灵会话清扫同类的一次性维护），规则见下表。

| 盘上形态 | 处置 | 结果 |
|---|---|---|
| 文件不存在 / 读不到 | 什么都不做 | 不创建、不写盘（保持「无记录 == 文件不存在」不变式） |
| `version` 数值形态 > 当前 schema | **完全不碰** | 只记日志（降级运行时不挪走新版数据、不把未知字段写没） |
| JSON 语法坏 / 顶层非对象 / `verified` 非数组 | **移动隔离**为 `quota.json.corrupt`（先删同名旧备份），**不重建** quota.json | 原字节逐字保留作证据；下次成功刷新自然重建 |
| 元素类型错（如 `last_ok_at: null`） | 逐元素丢弃该条 | **不隔离**：同文件里的合法记录照旧生效（隔离会让它们的「曾成功过」事实一起失效→误报「查询被拒」） |
| `provider_id` trim 后为空 / `last_ok_at` 空串或非法 RFC3339 | 清掉该条 | 删除判据只收窄到「必然不可用」 |
| `version` 形态非法（`"abc"`/`null`）/ `verified: null` | 重写为可读形态（不删记录） | 这类形态 `load` 会**整文件**回退默认——不修就永远每次启动告警、且「曾成功过」全失效 |
| 健康 | 什么都不做 | **不写盘、不记日志**（不 churn mtime） |

- 时间戳宽容度：只用 `chrono::DateTime::parse_from_rfc3339`——`Z` / `+08:00` 等偏移 / 小数秒**都算合法**（自定义正则或只认 `...Z` 会误删合法记录）。
- **核心不变式**：`heal` 返回后，quota.json 要么不存在（读不到 / 已隔离），要么 `QuotaState::load` **必定成功**且等于自愈保留的状态。写盘判据为「丢过记录 **或** 盘上内容按严格语义读出来 ≠ 保留结果」。
- `.corrupt` 语义**同 `ui-state.json.corrupt`**（只作证据、可被下次隔离覆盖、不参与任何判据、不自动清理），**不是** `sessions/index.json.corrupt` 那种「持久不可信信号」。
- 与 `commit` 共用同一把进程内互斥（临界区内只有同步 IO、不跨 `.await`）；任何失败路径只 `tracing::warn`，不 panic；日志只带结构化摘要（类别 + 行列号），**不回显文件内容**（日志常被用户分享）。
- 隔离出的 `.corrupt` 不会被本应用读取或上传；修完故障后用户可自行删除。

## 6. 设置页行级锚点（跳转定位）

灰行行内的「去设置」会调 `useUi.getState().showSettingsAt("providers", \`providers.${providerId}\`)`：打开设置 → 切到「模型与供应商」→ 滚动到该供应商行并高亮 1.5s（**不自动进入编辑视图**）。

- `stores/ui.ts` 新增 `settingsHit: { anchorId, nonce } | null` 与 `showSettingsAt`；`SettingsPage` 消费后立即置 `null`（一次性请求），`nonce` 保证同一行连点两次仍重新定位。
- 供应商列表行新增 `data-setting-id="providers.<uuid>"` 行级锚点（与三视图根节点的 `providers` 同名空间不冲突：`hitTargetOf` 按属性选择器**等值**匹配）。
- **draft 时序**：`ProvidersPanel` 只在 `ipc.getConfig` 回填 draft 后渲染，若不把 draft 就绪纳入定位依赖，冷启动点「去设置」会静默落空。见 [docs/settings-search-and-advanced](./settings-search-and-advanced.md) §1.5/§1.6（该文档原先明文禁止「动态条目锚点」，本批把它改成**允许供应商行级锚点**这一唯一例外，并写明边界）。

## 7. 白名单口径与已知限制

- 能否查询**只按 `base_url` 域名**判定（7 家：`opencode.ai` / `api.deepseek.com`·`deepseek.com` / `api.minimax.io`·`minimax.io` / `api.minimaxi.com`·`minimaxi.com` / `api.kimi.com`·`api.moonshot.cn`·`moonshot.cn` / `bigmodel.cn` / `api.z.ai`·`z.ai`）；**不校验路径**（`https://opencode.ai/v1` 也会命中），`api_format` 不参与匹配。
- 额度请求打的是**各适配器内置的官方端点常量**，不是用户填的 `base_url`；`base_url` 只用于「是否可查」的白名单判定与置顶/消歧。
- **接受的盲区**：自建网关（new-api / one-api / Ollama 等）指向兼容端点时域名不命中 → 永远归 `unsupported`（灰行）；Anthropic / OpenAI 官方本身没有额度接口。**不提供**手动指定适配器的开关。
- **普通账号被白名单误伤**：域名命中但 key 不是订阅套餐（如 Moonshot 普通 key、智谱普通 key）→ 上游报错 → 首次即 401/403/404 判 `rejected` 灰行（文案写「可能非订阅账号」），**曾成功过**才升格为 `error` 错误行。
- `quota.json` 里 `last_ok_at` 为空串 / 非法的记录（旧构建中断写入或手改的残留）：每次启动由自愈清除。此后该家按「从未成功过」参与判定——即下次 401/403/404 会显示为 `rejected` 灰行而不是 `error` 错误行（有意取舍：空串时间本就在界面上渲染不出，「状态与展示一致」优先于「保留一条半残事实」）。
- 该文件是**托管文件**：请勿手改。自愈会按上表重写，手改的痕迹不保证保留（会留 `.corrupt` 证据）。
- 设置页处于「新增/编辑」视图时，外部跳转会退化为高亮页体（当前不可达：设置页全屏覆盖右栏，用户路径上点不到额度行的按钮）。

## 8. 非目标

不提供手动指定/自定义额度端点；不加升级提示横幅；设置页不标注供应商支持状态；不做 GUI 自动点验（交付手动清单）；不新增适配器（仍 7 家）；不做额度历史/趋势/告警；不做「不支持即整条隐藏」开关（以折叠落地）；不加请求并发上限/退避策略。

## 9. 验证记录

| 项 | 结果 |
|---|---|
| `cargo test`（src-tauri） | **805 passed / 0 failed / 3 ignored**（主批改造基线 769 → 787；自愈增量 +8 → +7 → +3，见 [docs/quota-state-heal](./quota-state-heal.md)） |
| `cargo clippy --lib` | 0 warning（强制重编译后复核） |
| `pnpm --dir ui test` | **733 passed / 71 文件**（基线 702） |
| `pnpm --dir ui build` + `npx tsc --noEmit` | 通过（exit 0） |
| `cargo test --lib -- --ignored live_probe --nocapture` | 真实网络 + 真实钥匙串通过：三窗口齐出（`rolling 剩余 100% / weekly 剩余 0% status rate-limited / monthly 剩余 50%`），`key_source=keyring`，`quota.json` 正常落盘 |
| 方案对齐审查 | 26 条 AC 全部落实、零 🔴；唯一「方案承诺未落实」项为本文档与 `0-README` 登记，落地后关闭 |
| `quota.json` 启动期自愈 | 14 条 AC 对齐、零 🔴；反向自检确认关键用例能咬住风险（把逐元素容错 / 写盘判据改回去会立即变红） |
| 界面行为 | **未做 GUI 自动点验**（仓库约定），交付 20 步手动验证清单（见任务目录 `report.md`） |

## 10. 提交建议

```
feat(quota): drive the quota panel from CodeWave provider config

- 数据源：删除 opencode 凭证链（credentials.rs / opencode_paths.rs），改按 config.providers 枚举
- 行态：ok / error / invalid / rejected / no_key / unsupported 六态 + reason；不支持类 >3 家折叠汇总
- 状态：新增 ~/.codewave/quota.json 记录 last_ok_at（全局、0600、批次末互斥提交、清孤儿）
- 跳转：设置页新增行级锚点 providers.<uuid> 与 settingsHit 通道（含 draft 就绪时序）
- 契约：quota_snapshots(active_provider_id)；provider_id 改 uuid；credential_source → key_source
```
