# 安全策略（Security Policy）

## 支持版本

只对最新 Release 提供安全修复。

## 如何报告漏洞

CodeWave 把数据、API Key、会话记录都放在用户本机。涉及以下面的问题请按安全漏洞对待：

- API Key 越过系统钥匙串落明文，或被发送到非用户配置的端点
- 工具写操作逃逸会话可写根（路径边界 `tools/pathutil.rs`）、命令围栏（`safety/fence`）被绕过
- 工作区外的文件读取/删除未按权限档位拦截
- 任意命令执行绕过审批门

**请勿公开开 issue 描述可复现的漏洞**。报告渠道（按优先级）：

1. GitHub 私密安全通告：仓库页 → Security → Report a vulnerability
2. 以上不可用时，通过 GitHub 私信联系维护者

报告时请附：影响面描述、复现步骤（最小化 PoC 最佳）、涉及版本与平台。我们会在收到后尽快响应；修复发布前会与你同步时间线，并在 Release 说明中致谢（除非你希望匿名）。

## 安全设计速览

- API Key 全部经 `host/keyring.rs` 存入系统钥匙串，config.json 中仅存占位符
- 文件写操作经 `tools/pathutil.rs` 的 `safe_join` / `resolve_write` 边界校验（拒 `..` 逃逸与绝对路径注入）
- 命令执行先过三层静态围栏（`safety/fence`：删除黑名单 → AST 写目标分析 → 高危模式审批）
- 写类工具按会话权限档位（plan / confirm_each / auto_edit / full_access）走审批门
- 更新器（tauri-plugin-updater）**已启用**（`tauri.conf.json` 的 `plugins.updater.active: true`、`createUpdaterArtifacts: true`，签名公钥与端点指向 GitHub Release 的 `latest.json`）；应用启动 3 秒后静默检查一次，有新版才弹窗（`ui/src/utils/updateCheck.ts` + `stores/updater.ts` 的 8 相位状态机 + `panels/UpdateModal.tsx`），可在设置页「关于」页用「启动时自动检查更新」开关关闭（`ws_auto_update`）
