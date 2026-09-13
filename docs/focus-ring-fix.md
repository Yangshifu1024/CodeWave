# 输入控件 focus 双重边框（墨色化后显形缺陷修复）

## 缺陷

「计划任务」弹窗中「任务名」Input 聚焦时，边框呈现内外两圈描边：内圈 1px 近黑实线、外圈一圈浅灰色晕环，视觉上是粗糙的双重边框。设置弹窗、任意表单的 Input/TextArea/Select 聚焦时同样存在。

## 根因（antd 6.6 源码级证据链）

antd 输入类控件 focus 态默认绘制两层描边：

1. **内圈**：`border-color → colorPrimary`——[docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md) 墨色化后为近黑 `#1f1f1f`（亮）/ `#424242`（暗）；
2. **外圈**：`box-shadow: activeShadow`（`node_modules/antd/es/input/style/index.js:23`），而
   - `activeShadow = 0 0 0 {controlOutlineWidth}px {controlOutline}`（`antd/es/input/style/token.js:50`），
   - `controlOutlineWidth = lineWidth * 2 = 2`、`controlOutline = getAlphaColor(colorPrimaryBg, colorBgContainer)`（`antd/es/theme/util/alias.js:74,82`）——即 colorPrimaryBg 摊到容器底色上的浅色晕圈。

强调色还是蓝色时，淡蓝晕圈与蓝色边框融为一体，观感是单一 focus ring；[docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md) 改成中性墨色后，近黑边框与浅灰外圈对比被拉开，「双重边框」由此显形。`size="small"`（24px 高）控件进一步压缩间距，两圈更挤更明显。

项目侧零叠加：`app.css` 无任何全局 `:focus`/`outline`/`box-shadow` 规则命中 antd Input（仅有 Composer/AskPanel 局部作用域样式），TaskCenterPanel 的 Input 是纯净 antd 组件——纯 antd 默认行为在墨色主题下的观感劣化。

## 影响面

`controlOutline` 是 alias 级全局 token，消费方不止 Input：

| 消费方 | 用途 |
|---|---|
| Input / TextArea（`input/style/token.js`） | `activeShadow` focus 光晕（本缺陷主体） |
| Select（`select/style/token.js:61`） | `activeOutlineColor` focus 光晕 |
| Radio（`radio/style/index.js:505-508`） | `radioFocusShadow` 聚焦晕圈 |
| Form（`form/style/index.js:41`） | 原生 `file/radio/checkbox` 聚焦晕圈（reboot 样式） |
| Pagination（`pagination/style/index.js:160`） | 页码项聚焦晕圈 |
| Button（`button/style/token.js:40`） | `primaryShadow`（`0 2px 0` 底部硬阴影，墨色下本就近乎不可见） |

即全 app 输入类控件共病，修复必须全局生效而非只改单个弹窗。

## 修复

`ui/src/App.tsx` 的 `themeCfg.token` 新增一行：

```ts
controlOutline: "transparent",
```

- antd `theme/util/alias.js` 的 override 机制支持用户覆盖 alias 级 token（未在 seed/map 层出现则透传进 alias），亮/暗两套算法下均生效；
- 光晕置透明后，focus 态收敛为**单圈墨色边框**（边框变色本身即是清晰的聚焦指示），与 Composer 输入区无光晕的现状、[docs/ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md) 确立的「无彩色=默认」墨色极简语言一致；
- 已知取舍：radio/checkbox 聚焦时的晕圈一并消失，聚焦指示只剩边框变色——可接受，且原先那层浅灰晕圈本就几乎无聚焦提示价值。

## 验证

- `pnpm --dir ui test` + `pnpm --dir ui build` 通过（前端-only 改动，无需 cargo test）。
- 手动验证清单（界面改动不做 GUI 自动点验）：
  1. 任务面板 → 计划任务弹窗 → focus「任务名」/「任务指令」：单圈墨色边框，无外晕；
  2. 设置弹窗（通用/外观/安全/供应商）各 Input/Select focus 同样单圈；
  3. 系统切深色主题重复 1–2；
  4. Composer 聊天输入区与 ask 提问卡外观不变（原本即 border:none / 自绘 focus）。
