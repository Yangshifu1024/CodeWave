# CodeWave 标识统一与开源前审查批次报告

> 2026-09-13 批次的标识统一与开源前审查记录。2026-10-11 清理了旧品牌兼容与迁移内容，保留 CodeWave 标识约定和开源整备记录。下方验证数字为当时快照。

## 一、已确认决策（用户拍板）

| 决策点 | 结论 |
|---|---|
| 数据目录 | 全局 `~/.codewave` 与项目 `<主目录>/.codewave` |
| bundle id / keyring | `xyz.yangshifu.codewave` / `codewave.yangshifu.xyz` |
| 仓库地址 | `https://github.com/Yangshifu1024/CodeWave`（替换 4 处硬编码自建 Gitea 地址） |
| 审查产出 | 审查报告 + 落地修复同批完成 |

## 二、标识统一清单

- **标识符**：crate `codewave` + lib `codewave_lib`（main.rs 同步）；productName / 窗口标题 `CodeWave`；`tauri.conf.json` identifier 新值；`windows/installer-hooks.nsh`、`windows/toast-aumid.wxs`、`notify.rs` 双平台探针的 AUMID 三端一致；Cargo.lock / package-lock 随构建重生成
- **数据目录（收拢为常量）**：`core/config.rs` 的 `MANAGED_DIR_NAME = ".codewave"` 供项目、MCP、prompt 与计划落盘路径复用；`logging.rs` 的 `LOG_BASE_NAME = "codewave.log"` 供滚动、白名单、清理与测试复用；前端 `utils/path.ts` 导出同源 `MANAGED_DIR_NAME`
- **行为性字符串**：system prompt 自称与项目指令文件使用 CodeWave 标识；web_fetch User-Agent 版本取 `env!("CARGO_PKG_VERSION")`；keyring SERVICE 与包标识对齐
- **前端**：ui/根 package.json（name + license 字段）、index.html、TopBar/AboutModal/ChatMessages/AppShell/RightBar/stores、i18n 双语 8 行、ipc/types.ts 注释、全部品牌测试断言（~50 处）与 fixture 路径
- **文档**：README 重写；AGENTS.md、CONTRIBUTING.md、`.github/` 与 docs/ 对齐标识及 slug 约定

## 三、开源前修复清单

- 补 `license = "MIT"` / `repository` / `authors`（Cargo.toml）与 `license` 字段（两个 package.json）
- 新增 `CODE_OF_CONDUCT.md`（Contributor Covenant 2.1 改编）、`SECURITY.md`（报告渠道 + 安全设计速览）
- README 重写：面向使用者（特性 / 下载安装 / 源码构建 / 架构一览 / 文档地图 / 隐私与安全 / License），旧版 30KB 维护者走读内容由 AGENTS.md 与 docs/ 承接，`docs/NN` 断链引用随之清零
- 删除误留的 `src-tauri/clippy_fixes.py`；`.gitignore` 显式补 `.env`；`pnpm-workspace.yaml` 移除 `dangerouslyAllowAllBuilds`（保留 `onlyBuiltDependencies: [esbuild]` 白名单）
- 残留清理：个人绝对路径 4 处（`D:\Code\…` / `D:/Code/…`）、内部 commit 哈希引用（drive.rs「源自 ally 0682cf8」→ 中性表述）、survey 文档头部内部会话 id、workflows 头注释的前身项目提法
- **有意保留**：docs 中前身项目 GitWave 的事实性历史提法（决策记录/移植来源，无个人路径）；survey 文档对公开仓库 ally-agent 的带 URL 正当引用（符合「引用外部资料必须带 URL」纪律）

## 四、code-reviewer 审查结论与修复落地

当时七维度审查覆盖 108 文件，严重问题为 0。下表仅保留与开源整备相关的审查项。

| # | 级别 | 问题 | 处置 |
|---|---|---|---|
| 5 | 🟢 | UA 版本号硬编码必漂移 | **已修**：改 `concat!` + `env!("CARGO_PKG_VERSION")` |
| 6 | 🟢 | 前端 `.codewave` 散落无常量 | **已修**：`utils/path.ts` 导出同源常量，ProjectNav/RightBar 复用 |
| 7 | 🟢 | technical-design §4.7 第 4 层列序与代码不一致 + windows-toast-aumid.md IconUri 描述漂移（存量） | **已修**：两处文档对齐实现；IconUri = `$INSTDIR\icons\128x128.png` |
| 8 | 🟢 | 「补回 AGENTS.md 的 GLM E2E 运行入口」 | **核实不成立**：`e2e_real_glm` 仅存于 anthropic.rs 注释（历史验证记录），测试本体已删，AGENTS.md 删该行正确 |
| 9 | 🟢 | GitHub 仓库实际地址一致性 | **遗留**：见下节 |

## 五、遗留给用户的决策与手动项

1. **git 历史清洗**：148 个提交含个人邮箱（`yangshifu1024@qq.com`）与 2 个 GitWave 作者提交；建议开源时 squash 为干净初始提交（git 写操作由你执行）
2. **GitHub 仓库创建**：确认 `https://github.com/Yangshifu1024/CodeWave` 实际可用（Cargo repository / README / AboutModal 常量三处同源），git remote 切换由你执行
3. 可选：`lint.yml` 的 fmt/clippy continue-on-error 转硬门槛（全仓 fmt 差异为历史存量，与本批无关）

## 六、验证基线

| 项 | 结果 |
|---|---|
| `cargo test`（src-tauri/） | 543 passed / 0 failed / 2 ignored（手动探针）/ 0 warning |
| `pnpm --dir ui test` | 308 passed / 43 文件 |
| `pnpm --dir ui build` | type check + vite build 通过 |

## 七、2026-10-11 兼容清理

- 移除旧品牌项目指令读取、数据目录识别、Windows 安装器清理分支与专用测试夹具。
- README、AGENTS.md、[technical-design](./technical-design.md) 与本报告统一保留 CodeWave 约定。
- 项目主目录变更时的数据目录归一化、现行项目指令读取和技能扫描优先级继续由后端测试守护。
- 验证：`cargo test --workspace` **1304 passed / 0 failed / 3 ignored**（正常系统临时目录，`CARGO_INCREMENTAL=0`，无编译警告）；`cargo fmt --check` 与 `git diff --check` 通过；旧品牌名称、目录、指令文件及 bundle/keyring 标识的仓库文本匹配为 0。code-reviewer 七维度审查无严重或一般问题。
- PR 门禁：`pnpm prepr` **8/8 通过**；前端 **1462 passed / 115 文件**，脚本 **24 passed**，Rust clippy、前端 lint 与 build 均通过。本地验证平台为 Windows。
