# 设置弹窗表单全量 vertical 化

> 日期：2026-09-03 · 类型：界面批次（轻量路径） · 涉及：`ui/` 纯前端，后端零改动
>
> 需求：设置界面中的表单全量替换为 vertical 样式，即 label 在上的样式。

## 一、需求与范围

P0 分类：小型 UI 调整 → 轻量路径（[docs/plan-mode-workflow](./plan-mode-workflow.md)）。

设置弹窗（SettingsModal，880px 宽、左侧页签导航）内所有「label + 控件」形态统一为 label 在上：

| 位置 | 改造前 | 改造后 |
|---|---|---|
| 通用页签（SettingsModal） | `layout="horizontal"` + labelCol 8 / wrapperCol 16 | `layout="vertical"` |
| 外观页签（FontSettings） | horizontal + labelCol 6 / wrapperCol 18 | vertical |
| 安全页签（SettingsModal） | horizontal + labelCol 8 / wrapperCol 16 | vertical；「校验」区块的 `label=" "` + `wrapperCol` 占位 hack 一并移除（Divider 已有标题） |
| 供应商页签（ProvidersPanel） | 已是 vertical | 不动 |
| MCP 页签条目行（自定义 div） | `.mcp-entry-row` 左侧 64px 固定宽 label | 纵向排列，label 在上 |

非目标：技能页签列表行、命令白名单行、校验网格等非「label+输入控件」布局不动；表单受控逻辑与提交流程零改动。

## 二、实现

- antd Form：`layout="horizontal" labelCol/wrapperCol` → `layout="vertical"`（通用/外观/安全共 3 处）；`labelCol/wrapperCol` 常量与 `label=" "` 占位项删除。
- 自定义 MCP 行（非 antd Form）：`.mcp-entry-row` 改 `flex-direction: column; gap: 3px`，`.mcp-label` 去掉 `width: 64px; padding-top: 5px`。
- `mcp-entry-head`（名称输入 + transport 选择 + 删除按钮）是工具行而非 label 行，保留横排。

## 三、测试

前端 103/103 全绿、`pnpm --dir ui build` 通过。

| 文件 | 变更 |
|---|---|
| `app.smoke.test.tsx` | 设置弹窗用例断言 `.ant-modal .ant-form-horizontal` 不存在、`.ant-form-vertical` 存在 |
| `settings.style.test.ts`（新增） | CSS 契约：`.mcp-entry-row` 纵向排列且 `.mcp-label` 无固定宽（防回退横排） |

## 四、手动验证清单

1. `pnpm tauri dev` 打开设置，逐页签确认 label 在控件上方：
   - 通用：语言 / 压缩阈值 / 压缩超时 / 自定义提示词 / 日志级别 / 会话详细日志；
   - 外观：界面字体 / 等宽字体 label 在上，字体预览块与重置按钮正常；
   - 供应商：本就 vertical，编辑弹窗观感一致；
   - 安全：审批开关 / 命令白名单 / 私网开关 label 在上，「校验」Divider 下网格不再有空 label 行；
   - MCP：添加服务器后，命令 / 参数 / 环境变量 / URL 的 label 在输入框上方。
2. 各页签改值 → 保存 → 重开设置确认生效（逻辑未动，应与原行为一致）。
3. 中英文语言切换后 label 文案正常（i18n 键未动）。
