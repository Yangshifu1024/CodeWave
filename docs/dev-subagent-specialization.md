# dev 子代理专业化拆分：backend-dev / frontend-dev / app-dev

> 批次报告：原通用实现角色 `dev` 拆分为三个领域专业化角色，arch 编排 S6/S7 按任务包技术栈自动选角；彻底移除 `dev` 角色名（无别名）。
> 关联：[arch-orchestrator](./arch-orchestrator.md)（角色注册表来源批次）。

## 1. 需求与决策

用户需求：将原 dev subagent 修改为后端开发者角色（backend-dev），新增资深前端开发者角色（frontend-dev）与资深 App 开发者角色（app-dev）。经澄清确认的决策：

| 决策点 | 结论 |
|---|---|
| dev 别名 | **彻底移除**，不保留——`role="dev"` 走未知角色回退（通用子代理行为，不报错）；与 `test→tester`/`pm→product-manager` 别名策略刻意不对称 |
| backend-dev 定位 | 语言无关通用后端（Rust/Go/Java/Python/Node…），覆盖 API 设计、数据建模、并发/事务 |
| frontend-dev 定位 | 聚焦现代框架与语言（React/Vue/Svelte + TS 类型/组件化/状态管理） |
| app-dev 定位 | 移动（iOS/Android 原生 + Flutter/RN）+ 桌面（Electron/Tauri）双端 |
| arch S6 选角 | 按任务包技术栈自动选角色，混合栈项目允许单批内混派 |
| 前端 | 零改动（role 字符串纯透传，前端无角色枚举硬编码） |

## 2. 改动清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/agents/mod.rs` | `dev` 条目替换为 `backend-dev`/`frontend-dev`/`app-dev` 三条目：三份 body 保留原 dev 通用工程骨架（read 先于 edit/create、遵循仓库风格分层、小步实现、自测后汇报、汇报含改动文件清单、不做 git、不越界删除）+ 各自领域纪律（backend：契约先行/数据兼容/并发锁边界；frontend：组件化复用/类型完整/状态分层；app：平台差异先行/桥接契约/未验证平台显式标注）。头注释、`builtin_roles_present` 同步；新增 `dev_family_bodies_keep_engineering_skeleton`（三 body 骨架锚点防漂移）与 `find("dev").is_none()` 防回填断言 |
| `src-tauri/src/tools/subagent.rs` | 工具 description 与 schema 角色枚举更新（explore \| backend-dev \| frontend-dev \| app-dev \| reviewer \| code-reviewer \| product-manager \| tester）；`system_extra_injects_definition_for_known_role` 改用 backend-dev；`analysis_role_consistent_with_registry_aliases` 补三新角色断言 |
| `src-tauri/src/skills/mod.rs` | arch 技能：S0 引言、S6（技术栈选角规则 + 拿不准按任务包主要文件目录判断 + 通用工程任务兜底 backend-dev + 单批 ≤4 措辞泛化）、S7 返工（按修复项技术栈派对应开发子代理）；`arch_skill_contains_mechanism_anchors` 增补三角色名锚点 + 注册表可命中校验（防剧本笔误→静默回退通用子代理） |
| `src-tauri/src/tools/command/tests.rs` | 顺手修复存量编译缺陷（见 §4） |
| `AGENTS.md` | 工程分层表 `agents/` 行角色清单同步（`agents.md` 为同文件别名——macOS 文件系统大小写不敏感，git 亦只跟踪 AGENTS.md） |
| `ui/src/__tests__/run.subagent.test.ts` + `subagent.drawer.test.tsx` | 测试夹具 `role: "dev"` → `"backend-dev"`（透传字段，非硬编码枚举；防 legacy 词在测试资产中扩散） |
| `docs/arch-orchestrator.md` | L3 批次摘要 / §1 流程描述 / §2.1 角色表行同步（历史语义保留 + 指向本文档） |

未改：`normalize_role()` 归一、`test`/`pm` 别名、title 内部角色机制、`is_analysis_role` 逻辑、subagent 入参协议（task/role/maxSteps）、前端任何代码。

## 3. 语义影响

- **委派新角色**：`find()` 命中 → 注入 `<subagent-discipline>` + 完整 `<agent-definition>`（与既有机制一致）。
- **旧写法回退**：`role="dev"`（含 `Dev`/`dev ` 等归一变体）→ `find()` 返回 None → legacy 通用子代理（仅纪律块），静默降级、不报错。防回填断言已钉死该语义。
- **归一查找仍生效**：`backend_dev`/`APP-DEV` 等写法可命中（`normalize_role` 不变）。

## 4. 顺手修复：tools/command/tests.rs 存量编译缺陷

`cargo test` 在 macOS 首跑即暴露：该文件全部用例 `#[cfg(unix)]`，而 `use super::*` 未引入 `Tool` trait（父模块非 pub use），macOS 上 lib test 目标整体 E0599 编译失败；Windows 下 cfg(unix) 用例整体编译期剔除（tests 模块本身仍参与编译），基线因此全绿、缺陷漏检。修复：`#[cfg(unix)] use crate::tools::Tool as _;`（同步 cfg 避免 Windows 端 unused import warning）。

## 5. 验证

- `cargo test`（src-tauri，macOS）：**422 passed / 0 failed / 0 warning**（2 ignored 为真实 GLM E2E，需显式运行）。
- code-reviewer 七维审查两轮：首轮发现 2 🔴（AGENTS.md 编辑落错表格且工程分层表漏改；0-README 相邻条目缩进/时序错乱）+ 4 🟡（arch-orchestrator 角色计数与现行流程残留、前端夹具 legacy 词、arch 选角文本无测试锚点）+ 3 🟢，全部采纳修复；复审确认修复到位、无新问题。不采纳一项 🟢（`.agents/agents/` 开发期文档补 dev 系三角色 md）：该目录仅为 3 个专门代理的开发期参考文档、运行时不读取，注册表 body 本身已是运行时单一事实源。
- 顺手修复的存量缺陷：macOS 上 `cargo test` 基线原为红（`tools/command/tests.rs` 全部用例 cfg(unix) 且缺 `Tool` trait 导入，E0599 拖垮整个 lib test 目标），修复后 422 全绿可复跑验证。
- 手动验证清单：
  1. 新会话向主代理发出实现类需求，观察其 subagent 委派是否按技术栈选 backend-dev/frontend-dev/app-dev（工具 description 动态可见）；
  2. `$arch` 流程 S6 任务包派发角色与技术栈匹配（混合栈项目应出现混派）；
  3. 历史 role="dev" 会话恢复：子代理卡片正常显示，无报错。
