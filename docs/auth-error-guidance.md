# 认证失败引导批次（可读错误文案 / run:error 附 kind / 错误卡直达供应商设置 / key 冷却随保存清零）

> 缘起：真实场景中模型供应商返回 `认证失败：{"error":{"message":"Header中未收到Authorization参数，无法进行身份验证。","type":"1001"}}`——
> 错误已被后端正确分类（`ProviderError::Auth`，HTTP 401/403 命中，不重试直接 `run:error`），但前端只收到纯文本，
> 展示为错误气泡里的一坨原始 JSON，且没有任何修复引导。本批次把「认证失败」从死胡同消息变成两步修复闭环：
> **可读文案 + 错误卡内一键打开设置（定位供应商页签）**。方案决策（worktree 实施基线、交互形态、定位深度）经用户逐项确认。

## 1 · 交互决策（经 AskUserQuestion 确认）

| 决策点 | 结论 | 理由 |
|---|---|---|
| 交互形态 | **错误卡内加「打开模型设置」按钮，不自动弹窗** | run 错误异步到达——可能发生在用户正在其他会话输入/浏览时，模态框抢焦点打断性强；按钮保留 95% 便利且零打断 |
| 定位深度 | **只定位到「AI 供应商」页签列表**，不做供应商深链 | 改动最小；深链需后端附 model_id + ProvidersPanel 初始视图参数，收益/成本比不划算 |
| 发送前 key 非空预检查 | **不做** | 本地无鉴权端点（LM Studio/Ollama 等）合法无 key，强校验会误伤 |

## 2 · 后端改动（src-tauri）

### 2.1 错误文案美化（provider/dto.rs）

`ProviderError::from_status` 新增 `error.message` 提取（helper `api_error_message`）：body 可解析为 JSON 且
`error.message` 为非空字符串时，错误详情改为 `{message} (HTTP {code})`；任何其他形态回退既有 500 字符 snippet。
三协议适配器共用 `from_status`，零散改动全仓生效。效果对比：

- 改前：`认证失败：{"error":{"message":"Header中未收到Authorization参数，无法进行身份验证。","type":"1001"}}`
- 改后：`认证失败：Header中未收到Authorization参数，无法进行身份验证。 (HTTP 401)`

`_` 兜底分支（Protocol）保留 snippet 形态，避免「HTTP 409: … (HTTP 409)」双重状态码。

### 2.2 run:error payload 附结构化 kind（core/agent/drive.rs）

`ProviderError` 新增 `kind_tag()`（与 serde variant tag 逐字一致：auth / rate_limited / server / bad_request /
network / billing / cancelled / protocol），`run_chat` 失败分支的 `run:error` payload 增加 `"kind"` 字段。
**契约零破坏**：既有 `session/run_id/error` 字段不动，仅新增可选字段；事件契约测试只锁事件名不锁 payload 形状
（`events.contract.test.ts` 双向扫描的是 emit/listen 的事件名集合）。

### 2.3 key 冷却随配置保存清零（provider/keys.rs + host/commands/project.rs）

背景：KeyPool 按「pool_key（provider id）+ key 下标」记账冷却，Auth 失败冷却 30 分钟（`AUTH_COOLDOWN`）。
单 key 供应商重试走「全部冷却取最早恢复」分支照常发出，不构成死锁；但**多 key** 场景下，用户修好 key[0] 保存后，
`pick` 仍会先打向冷却中的其他 key，修复不生效。

- `KeyPool` 新增 `reset_all()`（清空健康表）；
- `save_config` 在写回 cfg 后调用——配置保存是低频操作，全量清零最简单且语义正确（改了配置就假设所有 key 值得重试）。

## 3 · 前端改动（ui/src）

| 文件 | 改动 |
|---|---|
| `stores/ui.ts` | 设置页签提升进 store（套 `rbTab`/`showChanges()` 既有范式）：`settingsTab`（默认 "general"）+ `showSettings(tab?)`（打开弹窗并定位） |
| `features/panels/SettingsModal.tsx` | 局部 `tab` useState 改为消费 `useUi.settingsTab`；保存校验失败跳 providers（[docs/provider-form-validation](./provider-form-validation.md)）经同一 store 状态，行为不变 |
| 打开设置调用点 ×4 | AppShell 设置按钮 / macOS 菜单 → `showSettings()`（默认 general，消除上次定位残留）；Composer「管理供应商」与「未配置模型」→ `showSettings("providers")`（语义本就指向供应商配置，顺带直达） |
| `stores/run.types.ts` | error item 增加可选 `errorKind?: string` |
| `stores/runHandlers.ts` | `run:error` handler 将 `p.kind` 透传为 `errorKind` |
| `features/chat/ChatMessages.tsx` | error 分支：`errorKind === "auth" / "billing"` 时 Alert 追加 description——引导文案 + 「打开模型设置」按钮（`showSettings("providers")`）；其余 kind 渲染不变，永不自动弹窗 |
| `i18n/zh-CN.ts` / `en-US.ts` | notice 命名空间 +3 键（authErrorHint / billingErrorHint / openModelSettings），中英对称 |

## 4 · 验证记录

| 检查 | 结果 |
|---|---|
| `cargo test` | **397 passed / 0 failed / 3 ignored**（基线 393 + 新增 4：error.message 提取、非 JSON 回退、kind_tag 稳定性、reset_all 清冷却）、0 warning |
| `pnpm --dir ui test` | **260/260 全绿**（基线 254 + 新增 6：showSettings 定位 ×1、errorKind 透传 ×2、错误卡引导渲染 ×3） |
| `pnpm --dir ui build` | 通过（type check + vite build） |
| `cargo clippy --all-targets` | 0 findings（[docs/follow-ups-batch](./follow-ups-batch.md) 清零态保持） |

实施于独立 worktree（`feat/auth-error-guidance` 分支，基于 [docs/session-pref-switch-toast](./session-pref-switch-toast.md) 之后的 master），与主树未提交改动完全隔离。

## 5 · 手动验证清单（GUI 不做自动点验，由用户执行）

1. **触发认证失败**：将某供应商 API Key 改为错误值保存，同会话发送消息 → 错误卡文案为可读的
   `认证失败：<供应商原始 message> (HTTP 401)`，且出现引导行与「打开模型设置」按钮；
2. **一键直达**：点击按钮 → 设置弹窗打开且停在「AI 供应商」页签（列表视图）；
3. **修复即生效**：改回正确 Key 保存 → 同会话重试成功（冷却已随保存清零；多 key 供应商不再被旧冷却拖住）；
4. **普通错误无按钮**：断网/错误 Base URL 触发网络错误 → 错误卡只显示文案，无引导行；
5. **无残留**：走完 2 后 Esc 关闭弹窗，再从侧栏底部设置按钮打开 → 停在「通用」页签而非供应商。

## 6 · 后续建议（未实施）

- 供应商深链（run:error 附 model_id → ProvidersPanel 直达该供应商编辑视图）：若页签级引导验证后仍嫌不够直达再考虑；
- 流内 error 帧的分类（当前 HTTP 200 + 流内 error 一律归 `Server` 可重试；若某供应商以 200+error 报认证错误，
  会白白重试 6 次——真实世界暂未观察到该形态，观察到了再按 body 内容细分）。

## 7 · 提交信息草案（git 由用户执行）

```
feat(run): readable provider errors + auth/billing settings shortcut on error card（[docs/auth-error-guidance](./auth-error-guidance.md)）
```
