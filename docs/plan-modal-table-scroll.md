# 计划弹框表格裁切修复

> 缺陷批次（`fix/plan-modal-table-scroll`，基线 `main` @ `cba9ebf`）。用户报：ask 计划卡的「查看完整计划」弹框太小、markdown 表格显示不全、**且没有横向滚动条**。

## 一、根因

三个现象是三条独立原因叠加的结果，其中第一条是**长期存在的死 CSS**。

| 现象 | 根因 | 证据 |
|---|---|---|
| 表格无横向滚动条 | **`.table-wrap` 是死 CSS**——规则存在但全仓零 DOM 生产点 | `app.css` 有 `.table-wrap { overflow-x: auto }`；`markdown.ts` 只注册了 `link_open` 与 math 规则，markdown-it 15 默认只 push `table_open`/`table_close`（tag `"table"`），默认 `renderToken` 输出裸 `<table>` |
| 表格被裁 | `.assistant .md { max-width: 860px; overflow-x: hidden }` 静默吞掉横向溢出；`.assistant { overflow: hidden }` 再兜一层 | `FileViewerModal` 的 markdown/doc 分支复用聊天区 `.assistant` 容器 |
| 弹窗太小 | `width={wide ? 1100 : 760}`，而 `wide = (st.phase === "sheet")`——**markdown 里的表格永远拿不到 1100**（判据按文件扩展名而非内容是否含表格） | `FileViewerModal.tsx` |

### 为什么死 CSS 能一直绿灯

`markdown.style.test.ts` 里那条「表格 th 底色 + 行 hover + .table-wrap 横向滚动」用例**只断言 CSS 文本形态**，从不断言 DOM 里有这个类：

```ts
expect(appCss).toMatch(/:is\([^*]+?\)\s+\.table-wrap\s*\{\s*overflow-x:\s*auto/);
```

于是规则与测试是同一批凭空写下的产物，互相「守护」着通过。**教训：样式契约测试必须同时守 CSS 与 DOM，否则「写了但没人用」的规则永远测不出来。** 本批次已补上渲染层断言（见 §三）。

## 二、改动

### 1. `ui/src/utils/markdown.ts` — 补上唯一的生产点

```ts
md.renderer.rules.table_open = () => '<div class="table-wrap"><table>';
md.renderer.rules.table_close = () => "</table></div>";
```

只接管首尾 token 的输出，`thead`/`tbody`/`tr`/`th`/`td` 仍走默认 `renderToken`，配对不受影响。

`renderUserMarkdown`（用户气泡）复用同一 md 实例，会自动继承包裹——这是已知且接受的次要面（用户气泡不在共享 `:is()` 组内，无边框无滚动样式；用户消息本不渲染宽表格）。

### 2. `ui/src/theme/app.css`

- **`.table-wrap` 去掉自身边框**：它与内部 `th/td` 的 1px 边框同时存在会叠成双层网格。边框交给 `th/td`。
- **共享 `:is()` 组新增 `.viewer-md.md`**（43 处成员列表），让预览弹框拿到与聊天区一致的完整排版。
- **mermaid / katex 那 8 条规则一并改走共享 `:is()` 组**——它们此前全部挂在 `.assistant .md` 前缀下，**换容器后会整体失配**（公式丢 `pre-wrap` 兜底、`.katex-display` 丢横向滚动、mermaid 丢底色）。这是本批次 review 抓出的真实回归。

### 3. `ui/src/features/files/FileViewerModal.tsx`

- markdown / doc 两个分支的外层容器：`<div className="assistant">` → `<div className="viewer-md md">`，**不再借用聊天区容器**（860px 行长上限是聊天排版的刻意约束，用在预览上只会静默裁切）。
- 宽度改响应式，并导出为常量：

```ts
export const VIEWER_WIDTH_TEXT = "min(920px, 94vw)";
export const VIEWER_WIDTH_TABLE = "min(1200px, 94vw)";
```

## 三、测试

| 文件 | 改动 |
|---|---|
| `markdown.style.test.ts` | 新增「渲染层真的产出 `.table-wrap` 包裹」——把死 CSS 契约从**只守文本**升级为**CSS + DOM 双守**；另加一条断言共享组含 `.viewer-md.md` 且不吃 860px |
| `fileviewer.document.test.tsx` | 新增 `describe("markdown 预览")`：表格被 `.table-wrap` 包裹、不再复用 `.assistant`、宽度常量契约 |

### 测试环境的一个坑

**happy-dom 的 CSS 校验器不认 `min()` 这类函数值**：`el.style.width = "min(920px, 94vw)"` 会被判非法而丢弃（读回 `""`），而数字宽度 `"920px"` 正常。因此宽度断言不能从 DOM 的 inline style 读，改断言导出的宽度常量本身。

## 四、刻意保留的既有约束

- **`.assistant { overflow: hidden }` 不动**——它是子代理抽屉链的 flex 最小尺寸保护（`app.css` 有注释明述）。
- **聊天区 860px 可读行长上限不动**——那是聊天排版的刻意约束；聊天区表格同样获得横向滚动能力，但版式约束不变。
- **窄表格被 `.table-wrap > table { width: 100% }` 拉满**是既有观感，不在本次范围。

## 五、验证

- 自动：`pnpm --dir ui test`（108 文件 / 1333 用例全绿）、`pnpm --dir ui build`、`pnpm --dir ui run lint`。
- 手动（界面改动不做自动点验，交付清单由用户执行）：见 §六。

## 六、手动 GUI 验证清单

1. 打开含长表格的 plan 文件 → 列全可读，底部有横向滚动条可拖动；
2. 大屏（≥1920）下弹窗明显比修复前宽；
3. 窄窗（约 800px 宽）下弹窗不溢出窗口；
4. 含 mermaid / KaTeX 的 markdown 仍正常渲染（**重点**：公式底色与横向滚动未丢）；
5. 聊天气泡内长表格出现横向滚动条而非被裁；
6. 亮 / 暗主题下表格无双层边框。