# AI 供应商表单校验 + 保存行为修正

> 日期：2026-09-03 · 类型：界面批次（轻量路径） · 涉及：`ui/` 纯前端，后端零改动
>
> 需求（用户反馈两条）：AI provider 的表单完全没有校验；保存后不要关闭弹框。

## 一、问题与方案

### 1. 表单校验（原先完全没有）

原状：新增视图只有一个 `addValid` 整体布尔——提交按钮静默禁用，无任何字段级反馈；编辑视图零校验，清空名称/Base URL 直接 patch 进 draft 并随主弹框「保存」落盘；添加模型弹窗仅空 id 禁用确定键。

校验规则（`validateProvider`，纯函数，结构化返回 `{field, kind}`）：

| 字段 | 规则 | 说明 |
|---|---|---|
| 名称 | 必填（trim 非空） | |
| Base URL | 必填 + `^https?:\/\/[^\s/]+` 格式 | 兼容 `http://localhost:11434/v1` 等本地端点（不强制域名点号） |
| API Key | **不校验**（可选） | BYOK 语义——Ollama 等本地端点无 key |
| 模型列表 | 新增供应商时 ≥1 个 | wire id 由模型弹窗守卫 |

展示与时机（antd `Form.Item validateStatus/help` 红字；输入为受控组件，不引入 Form instance 绑定）：

- **新增视图**：字段「改动过」或「点过提交」才显示红字（打开即红太吵）；提交按钮由静默禁用改为**常可点 + 点击校验**——点提交一次性亮出全部问题；无模型时底部提示转红。
- **编辑视图**：实时校验（进入时值合法，红字只在改坏后出现）。
- **模型弹窗**：确定键不再禁用；空 wire id 点确定 → 必填红字且弹窗不关，输入后红字自动消除。

### 2. 主弹框保存守卫 + 不自动关闭

- `save()` 先对 `draft.providers` 全量跑 `validateProvider`：任一无效 → `message.error` 列出「「供应商」字段 问题」清单 + **自动跳转供应商页签**（Tabs 受控化 `activeKey/onChange`）+ 不落盘。
- 保存成功后**不再关闭设置弹框**：仅 success 提示，关闭时机交给用户（取消/叉）。
- 顺带修正：保存被拦时 `saving` 不空转（校验先行 return）。

## 二、改动落点

| 文件 | 内容 |
|---|---|
| `ProvidersPanel.tsx` | 导出 `validateProvider`；`ProviderFields` 增 `errors` prop（Form.Item 红字）；新增视图 touched/submitTried 状态 + 提交点击校验；编辑视图实时红字；ModelModal 点击校验 |
| `SettingsModal.tsx` | `save()` 供应商守卫（报错 + `setTab("providers")`）；成功不关闭；Tabs 受控化 |
| i18n（zh/en） | `settings.vRequired`（必填）/ `vBaseUrl`（需为以 http(s):// 开头的完整 URL）/ `vSaveBlocked`（供应商配置无效，未保存：） |

## 三、测试

前端 107/107 全绿（基线 103 + providers.panel 新增 3 例 + smoke 新增 1 例）、`pnpm --dir ui build` 通过。

- `providers.panel.test.tsx`：原「提交按钮禁用」断言改为「无模型点提交 → 红字提示且不入 draft」；新增三例——空字段/非法 URL 校验、编辑清空名称实时红字、模型弹窗空 id 点确定不关弹窗。
- `app.smoke.test.tsx` 新用例：有效保存 → 「已保存」提示且弹框保留（settingsOpen 仍 true）；清空 Base URL → 实时红字 → 保存被「供应商配置无效」拦截、停留在供应商页签（断言收窄到 `.ant-modal .ant-tabs-tab-active`——文档级首个 active Tab 可能是右栏/顶栏 Tab 条，勿放宽）。

## 四、手动验证清单

1. 设置 → 供应商 → 添加供应商：留空直接点「添加供应商」→ 名称/Base URL 出必填红字；Base URL 填 `abc` → 出 URL 格式红字；补齐合法字段但无模型 → 底部提示变红且不入列表。
2. 添加模型弹窗：清空模型 ID 点「保存」→ 必填红字、弹窗不关；输入后红字消失。
3. 编辑既有供应商：清空名称/Base URL → 实时红字。
4. 供应商无效（如清空 Base URL）时点主弹框「保存」→ 报错「供应商配置无效，未保存：…」+ 自动跳到供应商页签 + 配置未落盘（重开设置仍是旧值）。
5. 配置合法时点「保存」→ 「已保存」提示，**弹框保持打开**；点取消/叉可正常关闭。
6. API Key 留空不报错（本地端点场景）。
