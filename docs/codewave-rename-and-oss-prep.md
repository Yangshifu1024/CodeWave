# CodeWave 品牌改名 + 开源前审查与修复批次报告

> 分支 `feat/codewave-rename`（2026-09-13）。两个任务一批完成：① 品牌整体改名 WaveStudio → CodeWave；② 开源前审查（code-reviewer 七维度）与缺失项落地修复。基线验证：`cargo test` **543 passed / 0 warning**（新增 2 例）、`pnpm --dir ui test` **308 passed / 43 文件**、`pnpm --dir ui build` 通过。

## 一、已确认决策（用户拍板）

| 决策点 | 结论 |
|---|---|
| 数据目录 | `.wavestudio` → `.codewave`，**不写自动迁移**（存量数据手动改名） |
| bundle id / keyring | `work.gitwave.studio` → `xyz.yangshifu.codewave`；`studio.gitwave.work` → `codewave.yangshifu.xyz`；**不做旧值回退**（旧 API key 重录） |
| 仓库地址 | `https://github.com/Yangshifu1024/CodeWave`（替换 4 处硬编码自建 Gitea 地址） |
| 审查产出 | 审查报告 + 落地修复同批完成 |

## 二、改名清单

- **标识符**：crate `codewave` + lib `codewave_lib`（main.rs 同步）；productName / 窗口标题 `CodeWave`；`tauri.conf.json` identifier 新值；`windows/installer-hooks.nsh`、`windows/toast-aumid.wxs`、`notify.rs` 双平台探针的 AUMID 三端一致；Cargo.lock / package-lock 随构建重生成
- **数据目录（收拢为常量）**：`core/config.rs` 新增 `MANAGED_DIR_NAME = ".codewave"` 与 `LEGACY_MANAGED_DIR_NAME = ".wavestudio"`（仅供手工迁移识别/回归测试）；`projects.rs`（索引发现/自愈/删除全部 join 点）、`mcp/mod.rs`（仓库根兼容路径）、`prompt.rs`（lessons 兼容路径）、`tools/ask/tool.rs`（plan 落盘相对路径）改引常量；`logging.rs` 日志名收拢为 `LOG_BASE_NAME = "codewave.log"`（滚动/白名单/清理/测试同源）；前端 `utils/path.ts` 导出同源 `MANAGED_DIR_NAME`（ProjectNav 拼接、RightBar 回退复用）；`.gitignore` 过渡期两行并存
- **行为性字符串**：system prompt 自称 + 项目指令文件 `CODEWAVE.md`（**新名优先、旧名 `WAVESTUDIO.md` 兼容读取且新名在场时跳过**，防双份注入）；web_fetch User-Agent 改 `concat!("CodeWave/", env!("CARGO_PKG_VERSION"), …)`；keyring SERVICE 新值
- **前端**：ui/根 package.json（name + license 字段）、index.html、TopBar/AboutModal/ChatMessages/AppShell/RightBar/stores、i18n 双语 8 行、ipc/types.ts 注释、全部品牌测试断言（~50 处）与 fixture 路径
- **文档**：README 重写、AGENTS.md（「数据目录勿改名」条款改写为新约定 + 基线数字更新 + docs slug 纪律）、CONTRIBUTING.md（文档纪律对齐 slug 约定）、`.github/` 四文件、docs/ 37 文件批量替换（标识符精确替换优先于泛化替换）；`technical-design.md` **D6 决策记录保留历史原值并加变更注记**（不篡改决策语境）

## 三、开源前修复清单

- 补 `license = "MIT"` / `repository` / `authors`（Cargo.toml）与 `license` 字段（两个 package.json）
- 新增 `CODE_OF_CONDUCT.md`（Contributor Covenant 2.1 改编）、`SECURITY.md`（报告渠道 + 安全设计速览）
- README 重写：面向使用者（特性 / 下载安装 / 源码构建 / 架构一览 / 文档地图 / 隐私与安全 / License），旧版 30KB 维护者走读内容由 AGENTS.md 与 docs/ 承接，`docs/NN` 断链引用随之清零
- 删除误留的 `src-tauri/clippy_fixes.py`；`.gitignore` 显式补 `.env`；`pnpm-workspace.yaml` 移除 `dangerouslyAllowAllBuilds`（保留 `onlyBuiltDependencies: [esbuild]` 白名单）
- 残留清理：个人绝对路径 4 处（`D:\Code\…` / `D:/Code/…`）、内部 commit 哈希引用（drive.rs「源自 ally 0682cf8」→ 中性表述）、survey 文档头部内部会话 id、workflows 头注释的前身项目提法
- **有意保留**：docs 中前身项目 GitWave 的事实性历史提法（决策记录/移植来源，无个人路径）；`skills/mod.rs` legacy 路径回归测试字面量（守卫「旧托管名不再扫描」）；survey 文档对公开仓库 ally-agent 的带 URL 正当引用（符合「引用外部资料必须带 URL」纪律）

## 四、code-reviewer 审查结论与修复落地

七维度审查（108 文件）：**🔴 严重问题 0**；🟡 一般问题 4（全部修复）；🟢 优化建议 7（采纳 5、核实不成立 1、遗留 1）。

| # | 级别 | 问题 | 处置 |
|---|---|---|---|
| 1 | 🟡 | `prompt.rs` 新旧指令文件并存时双份注入 | **已修**：`CODEWAVE.md` 在场时跳过 `WAVESTUDIO.md`（互斥读取）+ 新增并存/仅旧名两场景测试 `project_instructions_codewave_shadows_legacy_name` |
| 2 | 🟡 | 手工迁移后 project.json 内旧 `data_dir` 半迁移错位 | **已修**：`save_project` 对等于旧默认位置（`<主目录>/.wavestudio`）的显式 data_dir 重归一化到新约定位置 + 新增测试 `stale_legacy_data_dir_renormalized_on_save`。复核确认 `load()` 本就走索引权威路径无条件重建 data_dir，运行时无错位面，save 侧为双保险 |
| 3 | 🟡 | Windows 升级残留旧 AUMID 注册表键 | **已修**：POSTINSTALL 追加 `DeleteRegKey /ifempty` 兜底清理改名前旧键 `work.gitwave.studio` |
| 4 | 🟡 | 不迁移决策的用户引导缺口 | **已修**：README 新增「从旧版 WaveStudio 升级」三步说明（目录改名 / key 重录 / 指令文件兼容） |
| 5 | 🟢 | UA 版本号硬编码必漂移 | **已修**：改 `concat!` + `env!("CARGO_PKG_VERSION")` |
| 6 | 🟢 | 前端 `.codewave` 散落无常量 | **已修**：`utils/path.ts` 导出同源常量，ProjectNav/RightBar 复用 |
| 7 | 🟢 | technical-design §4.7 第 4 层列序与代码不一致 + windows-toast-aumid.md IconUri 描述漂移（存量） | **已修**：两处文档对齐实现（互斥语义注明；IconUri = `$INSTDIR\icons\128x128.png`） |
| 8 | 🟢 | 「补回 AGENTS.md 的 GLM E2E 运行入口」 | **核实不成立**：`e2e_real_glm` 仅存于 anthropic.rs 注释（历史验证记录），测试本体已删，AGENTS.md 删该行正确 |
| 9 | 🟢 | GitHub 仓库实际地址一致性 / icons 与 store-logo 品牌视觉 | **遗留**：见下节 |

## 五、遗留给用户的决策与手动项

1. **git 历史清洗**：148 个提交含个人邮箱（`yangshifu1024@qq.com`）与 2 个 GitWave 作者提交；建议开源时 squash 为干净初始提交（git 写操作由你执行）
2. **logo/icons 出图**：`src-tauri/icons/` 全套与 `ui/src/assets/store-logo.png` 图片内容仍是旧品牌视觉，AI 不重绘，出图后替换（文件名无需变）
3. **本地存量数据迁移**（改名不迁移的直接后果）：`mv ~/.wavestudio ~/.codewave`；各项目主目录内 `.wavestudio/` 同理整体改名；设置页重录 API Key
4. **GitHub 仓库创建**：确认 `https://github.com/Yangshifu1024/CodeWave` 实际可用（Cargo repository / README / AboutModal 常量三处同源），git remote 切换由你执行
5. 可选：`lint.yml` 的 fmt/clippy continue-on-error 转硬门槛（全仓 fmt 差异为历史存量，与本批无关）

## 六、验证基线

| 项 | 结果 |
|---|---|
| `cargo test`（src-tauri/） | 543 passed / 0 failed / 2 ignored（手动探针）/ 0 warning |
| `pnpm --dir ui test` | 308 passed / 43 文件 |
| `pnpm --dir ui build` | type check + vite build 通过 |
| 全仓 grep 残留扫描 | 仅剩有意保留项（gitignore 过渡行、AGENTS/technical-design 历史注记、legacy 回归测试、GitWave 事实性历史提法） |
