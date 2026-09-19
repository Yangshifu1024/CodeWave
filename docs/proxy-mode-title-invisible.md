# 设置·网络「代理模式」选项名不可见：antd `font-size: 0` 容器内的继承陷阱

> 2026-09-19 · 缺陷修复（`fix/proxy-card-fontsize`）。现象：设置 → 网络 → 代理模式的三张卡片只显示说明文案，**模式名（无代理 / 系统代理 / 自定义代理）完全看不到**。

## 现象与定位过程

用户在 **v0.3.10 安装版**里报「代理模式名称没有显示」。截图特征是：三张卡片里描述文案正常（灰色、12px、缩进 24px），**唯独模式名那一行是空白** —— 但辐射圆点在。

静态排除（每项都有证据，不是猜）：

| 曾经怀疑 | 排除依据 |
|---|---|
| 文案键缺失 | `ui/src/i18n/zh-CN.ts` 三个标题键都在，且 **v0.3.10 tag 内也有** |
| 跑的版本太旧 | 标题 `<span className="proxy-mode-title">`（`SettingsModal.tsx:560`）与三个标题键由**同一提交 `89c02bf`** 引入，该提交经 `git merge-base --is-ancestor` 确认在 `v0.3.10` 内 |
| 其他分支改坏 | 分支 diff 中 `SettingsModal.tsx` **0 行**涉及 `proxy`；app.css 的 LSP 新增规则位置与代理规则不相邻 |
| 打包丢了规则 | 构建产物 `ui/dist/assets/*.css` 里确有 `.proxy-mode-title{font-weight:600;color:var(--ws-text-1)}` |
| token 作用域（Modal portal 出 `:root`） | `ThemeBridge` 把 `--ws-*` 写在 `document.documentElement`，portal 可继承；且同卡片里 `--ws-dim`（描述用）明显生效 |
| 颜色 = 背景色 | `--ws-text-1 = token.colorText`（antd 主文字色），且全应用另有 191 处在用它 |
| i18n 返回空串 | `ui/src/i18n/index.ts` 无 `parseMissingKeyHandler`，且有 `fallbackLng: "zh-CN"` 兜底，缺键只会显示键名而非空串 |

定位手段（安装版无 devtools、又不能跑 dev —— 同 bundle id 单实例互斥会顶掉正在用的会话）：先用 `render_html` 在**应用自身的 WebView2 引擎**里渲染 5 个只差一处的复刻变体做二分（`A` 原样 / `B` token 透明 / `C` 去 flex / `D` 去圆圈 / `E` 圆圈撑满），用户回报「A/C/D 可见」—— 锁定问题在**字号继承**而非布局或颜色，再到 `node_modules/antd` 源码里找到那行 `fontSize: 0`。

## 根因

antd 6 的 Radio 组件样式在**分组容器**上设置了 `font-size: 0`：

```js
// ui/node_modules/antd/es/radio/style/index.js:52-55
[`.ant-radio-group`]: {
  ...
  fontSize: 0,        // 消除 inline-block 兄弟之间的空白间隙
}
```

代理卡片的 DOM 层级是：

```
Radio.Group            →  .ant-radio-group { font-size: 0 }
  .proxy-mode-list                        ← 继承了 0
    label.proxy-mode-card                 ← 继承了 0
      .proxy-mode-head
        span.proxy-mode-title             ← 只声明 font-weight/color，无 font-size ⇒ 继承 0 ⇒ 隐形
      .proxy-mode-desc                    ← 显式 font-size: 12px ⇒ 正常可见
```

所以缺陷在 `89c02bf`（代理功能）落地时即已存在，与 antd 升级时点无关，也与任何后续改动无关。

## 为什么既有测试没拦住

`ui/src/__tests__/settings.network.test.tsx` 用 `cardByTitle("系统代理")` 定位卡片，断言的是 **DOM 里的文本**；happy-dom 不做字号级联与绘制。**「文本存在于 DOM」不等于「肉眼可见」** —— 这类缺陷只能靠条纹级契约断言或人工肉眼验证覆盖（本仓界面改动本就不做 GUI 自动点验，故补了下节的契约断言）。

影响面：全仓 `Radio.Group` 仅此一处使用；antd 中另两处 `fontSize: 0`（`.ant-list-item-action`、`.ant-table-column-sorter`）都是内部容器，不承载自定义文本。

## 修法

1. `ui/src/theme/bridge.tsx`：新增桥接变量 `--ws-font-size`（取自 `token.fontSize`），沿「手写样式只消费 `--ws-*` token」的既定约定，避免硬编码像素值。
2. `ui/src/theme/app.css`：`.proxy-mode-list` 增加 `font-size: var(--ws-font-size);`。放在列表容器而非标题元素上，可同时覆盖未来新增的子节点。
3. `ui/src/__tests__/proxyCard.style.test.ts`（新增）：用 node fs 直读 `app.css` / `bridge.tsx` 做契约断言（与 `titlebar.style.test.ts` 同法），守住这两条规则不被后人删掉。

## 验证

- `pnpm --dir ui test`、`pnpm --dir ui build` 通过（新增契约测试计入）。
- **必须人工肉眼确认**：设置 → 网络 → 三个模式名可见。该类绘制层缺陷无法由 DOM 断言覆盖。
