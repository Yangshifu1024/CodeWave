# 供应商表单校验规则收紧

> 日期：2026-09-03 · 类型：界面批次（轻量路径） · 涉及：`ui/` 纯前端，后端零改动
>
> 需求（在 [docs/provider-form-validation](./provider-form-validation.md) 基础上收紧三条）：API 格式移动到名称之下并标必填；API Key 必填；模型列表不能为空。

## 一、改动内容

### 1. 字段顺序：名称 → API 格式 → Base URL → API Key

API 格式从末位上移到名称之下——先定协议、再同步默认端点（协议切换会把空/已知默认 Base URL 替换为对应官方端点），填表动线更顺。API 格式加 `required` 必填标记（Select 有默认值，结构上恒满足，不产生校验错误）。

### 2. validateProvider 规则扩展

| 字段 | 规则 | 备注 |
|---|---|---|
| 名称 | 必填 | [docs/provider-form-validation](./provider-form-validation.md) |
| Base URL | 必填 + `http(s)://` 格式 | [docs/provider-form-validation](./provider-form-validation.md) |
| API Key | **必填**（至少一行非空）| 本批新增；`***` 掩码行视为已配置（编辑既有供应商不误报） |
| 模型列表 | **非空** | 本批新增；新增供应商原本就要求，现编辑视图与保存守卫同样覆盖 |

### 3. 展示与时机（沿用 [docs/provider-form-validation](./provider-form-validation.md) 机制）

- 新增视图：字段改过或点过提交才显示红字；模型列表错误显示在「模型列表」分隔条下方的红字行（`.provider-models-error`），底部提示恢复常灰 informational。
- 编辑视图：名称/Base URL/API Key/模型列表实时红字（清空 key 掩码行、删空模型即触发）。
- 主弹框保存守卫：错误清单字段名映射补齐（API Key / 模型列表），报错 + 跳转供应商页签 + 不落盘不变。

### 4. 兼容性说明

历史配置中「无 key」或「无模型」的供应商在下次保存时会被守卫拦下，需补齐后才能保存——这是需求本意（每个供应商必须有可用凭据与至少一个模型）。

## 二、改动落点

| 文件 | 内容 |
|---|---|
| `ProvidersPanel.tsx` | `validateProvider` 增 keys/models 规则；`ProviderFields` 字段重排 + API 格式/API Key 必填标记 + keys 红字 + models 错误行；两视图 errors 传参与 touched 追踪（keys） |
| `SettingsModal.tsx` | 保存守卫字段名映射补 keys（"API Key"）与 models（模型列表） |
| i18n | 无新键（复用 `vRequired`；守卫清单 keys 字段名用技术词 "API Key" 直书） |

## 三、测试

前端 107/107 全绿（用例数不变：主流程/新增校验/编辑校验原地扩展）、`pnpm --dir ui build` 通过。

- 主流程：补填 API Key；新增字段顺序断言（名称 < API 格式 < Base URL）；无模型提交 → `.provider-models-error` 红字。
- 新增校验：名称/URL 合法但缺 key/模型 → API Key 所在 Form.Item 红字 + 模型列表红字、不入 draft。**坑**：antd 错误提示退场动画在 happy-dom 不回收节点，`.ant-form-item-explain-error` 数量断言不可靠，须按字段容器（label → 所属 Form.Item）断言。
- 编辑校验：清空名称 → 红字；清空 key 掩码行 → 红字；删掉唯一模型 → 模型列表红字。
- 删除类用例 fixture 的 `keys: []` 改为 `["sk-x"]`（避免编辑视图进入即红的噪音）。

## 四、手动验证清单

1. 添加供应商：字段顺序为 名称 / API 格式（带必填星标）/ Base URL / API Key（带必填星标）；切协议自动同步默认端点。
2. 不填 API Key 直接提交 → API Key 必填红字；不添加模型提交 → 「模型列表」分隔条下红字。
3. 编辑既有供应商（key 为 `***` 掩码行）：直接进入无红字；清空掩码行 → 红字；删空模型列表 → 红字。
4. 无 key 或无模型的供应商点主弹框「保存」→ 报错清单含「API Key 必填 / 模型列表 必填」+ 跳转供应商页签 + 不落盘。
