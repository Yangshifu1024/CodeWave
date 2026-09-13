# antd 6 升级 + Composer 多条流光边框

> 日期：2026-09-03 · 类型：界面批次（轻量路径） · 涉及：`ui/` 纯前端，后端零改动
>
> 需求：为 Composer 加上多条流光（参照 antd BorderBeam 组件文档）；经确认该组件需 antd ≥ 6.4（`count` 多条流光需 ≥ 6.6），用户选择升级 antd。

## 一、antd 5.29.3 → 6.6.2 升级

- `pnpm add antd@latest`：5.29.3 → **6.6.2**（`@ant-design/icons` 6.3.2 兼容不动）；主 bundle 反而缩小约 60KB（2,338 → 2,278 kB）。
- **破坏面极小**：`tsc --noEmit` + vite build + 110 个前端测试全量摸底，唯一命中的破坏性变更是 Tabs 页签位置 API：
  - `tabPosition` → `tabPlacement`（废弃告警）
  - 取值逻辑化：`"left"` / `"right"` → `"start"` / `"end"`（`TabPlacement = 'top' | 'end' | 'bottom' | 'start'`）
  - 唯一用点：SettingsModal 左侧页签导航 → `tabPlacement="start"`
- 其余约定（`destroyOnHidden`、`items` 写法、`App.useApp()` message、`variant="borderless"` 等）在 v5 后期已对齐 v6，零改动通过。

## 二、Composer 多条流光

antd 6 `BorderBeam`（组件自 6.4.0，`count` 自 6.6.0）挂在输入卡片上：

```tsx
<BorderBeam count={3} color="var(--ws-accent)">
  <div className="composer-card">…</div>
</BorderBeam>
```

要点：

- **count={3}**：三条流光沿边框均匀分布（antd 以负 delay `-duration·i/count` 错相实现）。
- **color="var(--ws-accent)"**：antd 对 color 只做字符串拼进 `linear-gradient`（`getBorderBeamGradient` 不解析颜色），CSS 变量可用 → 流光颜色随主题强调色（与 `.composer-card:focus-within` 边框同源），浅/深主题自动适配。
- **position: relative**：流光层经 portal 插入 children 真实 DOM、`position: absolute` 贴边框外扩（inset = 边框宽度取负），宿主须自带定位上下文 → `.composer-card` 补 `position: relative`。
- size/duration/lineWidth 用默认值（100px 可见段 / 6s 一圈 / 1px 线宽，1px 与卡片边框同宽）。
- 无障碍与降级：antd 内部处理 `prefers-reduced-motion: reduce` 时隐藏流光。

## 三、测试

前端 110/110 全绿、`pnpm --dir ui build` 通过。

- 冒烟「工具条渲染」用例补断言：`.composer-card .ant-border-beam` 数量为 3。
- 升级本身无行为回归（全量套件原地通过）；Tabs 告警消除。

## 四、手动验证清单

1. `pnpm tauri dev`：输入卡片（有会话时）边框上三条流光沿圆角边框匀速巡游、互不重叠（相隔约 1/3 周长）。
2. 浅色/深色主题切换：流光颜色与聚焦边框强调色一致，无白色残留。
3. 输入框聚焦/失焦：边框色变化照旧，流光不受影响；附件、权限菜单、发送按钮等交互正常。
4. 设置弹窗左侧页签导航位置与升级前一致（`tabPlacement="start"`）。
5. 系统开启「减弱动态效果」时流光隐藏（antd 内建行为）。
