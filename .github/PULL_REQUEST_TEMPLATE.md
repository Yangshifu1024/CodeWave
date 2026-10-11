## 概要

<!-- 一句话描述本次变更。 -->

## 关联文档

<!-- 链接受影响的 docs 长期主题指南；优先更新已有主题，不为本次修复单独新增报告。无文档影响可说明原因。 -->

- 相关主题与本次更新：

## 类型

<!-- Conventional Commits type，单选 -->

- [ ] feat
- [ ] fix
- [ ] docs
- [ ] refactor
- [ ] test
- [ ] chore

## 变更内容

<!-- 用户可见 / 开发者可见的变更要点。 -->

-

## 验证

<!-- 门槛：两边都动 = 两者都要过；GUI 改动另附分步手动验证清单，不做 GUI 自动点验 -->

- [ ] `cargo test` 全绿（0 warning）
- [ ] `pnpm --dir ui test` + `pnpm --dir ui build` 通过
- [ ] 手动验证清单走查（GUI 改动）
- [ ] 事件面 / IPC 契约未破坏（改到事件键、Message/Content、config schema 时勾选）

## 风险与迁移说明

<!-- 评审需要重点关注的部分；涉及契约 / 数据兼容（serde default）/ 存量数据迁移时必填。 -->
