# 供应商管理重构（模型管理 → Provider 管理）

> 批次目标：按参考交互稿将 AI 接入的管理对象从「模型」改为「供应商」——供应商 = 端点 + 协议 + key 池 + 自有模型列表；
> 移除 models.dev 内置目录（`ui/src/data/modelCatalog.json`，868KB / 207 供应商 / 7000+ 模型），模型定义一切以用户输入为准；
> 设置弹窗模型页签重构为供应商页签（antd 组件化）。

## 1. 数据结构（config schema v1 → v2）

### v2 形态（`src-tauri/src/core/config.rs`）

```
ConfigState {
  schema_version: 2,
  providers: Vec<ProviderConfig>,
  active_model_id: Option<String>,   // 语义不变：指向 ProviderModel.id（uuid，全局唯一）
  models: Vec<ModelConfig>,          // legacy：仅作迁移输入（skip_serializing_if 空），迁移后不再落盘
  ...
}

ProviderConfig { id, name, api_format, base_url, keys: Vec<String>, models: Vec<ProviderModel>,
                 legacy_key_accounts: Vec<String> }   // v1 按模型入钥匙串的账户（升级回退/迁移合并用）

ProviderModel { id, model(wire id), max_tokens(8192), context_window(128000),
                reasoning_effort: Option<String>, vision: bool, video: bool }
```

- **wire id 即显示名**：模型不再有独立 name 字段（参考稿如此）；菜单、工具条、tooltip 均显示 wire id。
- **vision/video 为硬布尔**（输入类型胶囊：文本🔒恒开 + 图片 + 视频；输出类型：文本🔒恒开）。video 为预留标记，发送链路暂不消费。
- `ModelConfig` 保留为**运行时摊平结构**（`find_model`/`flatten` 产出），provider 三协议适配层与 `StreamRequest` 签名不变。

### v1 → v2 迁移（`migrate_legacy_models`，load() 时自动执行并落盘，幂等）

- 分组键 = `(api_format, base_url 尾斜杠归一, 供应商显示名)`；显示名缺省取 base_url host，再缺省 `Provider N`。
- **模型 id 原样保留** → `active_model_id` / 会话 override / 统计 by_model 全部不失效。
- keys 合并：明文行并入去重；**含占位（`__keyring__`）即记录 `legacy_key_accounts`**（混合形态也记录）；组内只有占位形态时带入占位行。
- `vision: None`（v1 未知语义）→ `true`，避免升级后附件被突然拦截。
- `schema_version` load 时无条件升至 2。

### keyring（账户粒度 model.id → provider.id）

- `migrate()`：明文 keys → keyring（写入前**过滤占位与空行**，防占位串污染钥匙串）；**legacy 模型账户旧 key 合并迁入**供应商账户（回退链一次收口）。
- `resolve_keys()`：空 keys → 空（删光即无 key，不复活钥匙串旧值）；含占位 → 账户链回读（供应商账户 → legacy 账户）**与明文行合并去重**；纯明文原样返回。
- `save_config` 保存即内联 migrate（明文 key 不留落盘窗口期）；启动 migrate 保留（兜底手改 config 的场景）。

### 其他后端接线

- `effective_model`（core/prefs.rs）由借用改为返回 owned `ModelConfig`（摊平形态）；agent/title/context 调用点同步简化。
- KeyPool 冷却键 `model.id` → `model.provider_id`（同供应商各模型共享 key 池冷却）。
- `sanitized()`/`unmask_from()` 按 provider id 匹配；掩码行无匹配（行序变动**或新供应商**）一律丢弃——掩码串永不当 key 落盘。
- `set_session_prefs` 模型校验改 `find_model`；e2e key 注入改 `provider_of_model_mut`。

## 2. 前端

- **类型契约**（`ipc/types.ts`）：`ProviderConfig`/`ProviderModel`/`FlatModel` 取代 `ModelConfig`；`ConfigState.providers` 取代 `models`。
- **摊平工具**（`utils/models.ts`）：`flattenModels`/`findModel`——菜单分组与发送守卫同源（顺序 = 供应商序 × 模型序）。
- **Composer**：模型菜单分组 = 供应商（空名兜底「其他」）；vision 守卫 `!vision` 拦截（硬布尔）；工具条常显 wire id，tooltip `供应商 · wire id`；「管理模型」→「管理供应商」。
- **SettingsModal**：删除目录级联选择与 `fillFromCatalog`；模型页签 → 供应商页签，渲染交给新组件 **`features/panels/ProvidersPanel.tsx`**：
  - 列表视图：供应商行（名称 + 协议标签 + base_url + N 模型 + 编辑/删除）+ 虚线「添加供应商」；
  - 新增视图：本地表单（名称/Base URL/API Key 多行/协议切换同步默认端点）+ 内嵌模型列表 + 底部「至少添加一个模型」提示与禁用态提交；
  - 编辑视图：同字段直接 patch draft；
  - **添加/编辑模型 Modal**：wire id（必填）+ 上下文窗口 + 最大输出 Token + 输入类型 `Tag.CheckableTag` 胶囊（文本🔒+图片+视频）+ 输出类型（文本🔒）+ 推理力度默认档；
  - 子组件全部顶层定义（组件内定义会每渲染重建组件类型 → 输入框逐键失焦）。
- **保存兜底**：active 未设/悬空回落第一个模型；key 行 trim 去空行（掩码/占位行保留，后端 unmask）。
- 删除 `modelCatalog.json` 与动态 import；`stores/settings.ts` 默认配置/选择器改 providers；清理 `stores/run.ts` 死导入与 `app.css` 旧 `.model-row` 样式（供应商面板全 antd 组件，无自定义皮肤）。

## 3. 审查与修复（code-reviewer 批次审查）

无 🔴；🟡 已修复：

1. **占位+明文混合 keys 链**（同源三处）：migrate 占位串污染钥匙串 / 迁移混组静默丢 key / 删光 key 复活旧值——按 §1 keyring 三条修复统一收口。
2. **save_config 明文落盘窗口期**：保存即内联 migrate。
3. **unmask 新供应商掩码行落盘**：无匹配掩码一律丢弃。
4. **schema_version 停留 1**：load 无条件升 2。
5. **测试缺口**：迁移混组/空 keys/同名不同端点、unmask 新供应商、keyring 混合形态、ProvidersPanel 交互（新增/删模型 active 回落/删供应商）均有专测。

未处理（🟢 记录）：UI 向用户裸示 `__keyring__` 占位行（误删由 resolve 合并逻辑兜底）；摊平逻辑前后端双份无契约对拍；`host_of` 对 IPv6 字面量命名丑；编辑视图无必填校验（后端 save_config 亦不校验，坏配置在请求时报错）。

## 4. 测试

- 后端：`cargo test` **243 passed / 0 failed / 0 warning**（基线 235 → 新增迁移边界 3 + keyring 混合 1 + unmask 1 + 原有迁移/摊平 3）
- 前端：`pnpm --dir ui test` **96/96**（新增 `providers.panel.test.tsx` 3 用例）；`pnpm --dir ui build`（tsc + vite）通过

## 5. 手动验证清单（GUI 不做自动点验，按此人工过一遍）

1. **升级迁移**：用旧版 config.json（含多个模型）启动 → 设置 → 供应商页签出现分组后的供应商；模型 id/当前模型/会话 override 不丢；API key（明文与钥匙串两种形态）请求仍通。
2. **新增供应商**：设置 → 供应商 → 添加供应商 → 填名称/Base URL/API Key/协议 → 添加模型（弹窗填 wire id、上下文窗口、最大输出、勾选图片）→ 添加供应商提交 → Composer 模型菜单出现该分组。
3. **协议切换**：编辑供应商切换 API 格式 → Base URL 仅在空或已知默认值时被替换，自定义 URL 不动。
4. **vision 守卫**：模型不勾「图片」→ 粘贴图片附件发送 → 弹「当前模型不支持图片」；勾选后正常发送。
5. **key 往返**：保存后重开设置 → key 显示掩码 `***尾4` → 不改动直接保存 → 请求仍通（unmask 还原）；新增一行 key → 保存即入钥匙串（config.json 无明文）。
6. **删除回落**：删除当前 active 模型 → active 回落同供应商下一模型/首模型；删除整个供应商 → active 清空。
7. **会话模型切换**：Composer 菜单按供应商分组切换 → `set_session_prefs` 镜像；「管理供应商」入口打开设置。
