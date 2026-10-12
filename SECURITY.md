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

- API Key 经 `core/config_commit.rs` 先写入不可变的系统钥匙串账户并读回核验，再一次性提交只含占位符与 `credential_ref` 的配置。配置、字体、技能、子代理模型与“始终允许”白名单共用提交锁；钥匙串不可用时拒绝新增或改变密钥，不降级为明文。配置替换失败保留旧引用，已提交后的清理失败单独返回警告
- 当前 v2 文件若已有明文，启动使用同一安全提交协议整理；失败保留原文件并显示警告，不新增明文配置或 `.bak`。该保证不清除历史备份、交换区或用户复制的秘密；自定义请求头及代理密码仍不在 API Key 保护范围内。配置分享前仍须检查这些内容
- 非秘密恢复记录在新账户创建前写入，孤儿账户延迟到下次启动清理；无法确认配置引用或遇到未来格式时锁定本进程配置写入及破坏性清理，修复文件并重启后才恢复，防止默认配置覆盖原件。在途请求持有的旧账户不立即撤销。成功升级后仅支持理解 `credential_ref` 的修补版或前向修复，禁止直接降级到旧的原位钥匙串写入版本
- 自动故障/竞争测试使用临时文件与假凭据，已通过[当前发布提交三平台 CI](./docs/testing.md#可靠性修复自动验证记录2026-10-12)。通用文件写入器的 Windows 独占文件锁下替换失败保留原件/用户 `.bak` 已有自动平台证据（不等于配置专用替换器已验收）；真实系统钥匙串（含 Linux Secret Service）锁定/拒绝、凭据保存与应用重启仍需分平台完成，不能将无 GUI 测试当作这些实机验收通过
- 文件写操作经 `tools/pathutil.rs` 的 `safe_join` / `resolve_write` 边界校验（拒 `..` 逃逸与绝对路径注入）
- 命令执行先过三层静态围栏（`safety/fence`：删除黑名单 → AST 写目标分析 → 高危模式审批）
- 写类工具按会话权限档位（plan / confirm_each / auto_edit / full_access）走审批门
- 更新器（tauri-plugin-updater）**已启用**（`tauri.conf.json` 的 `plugins.updater.active: true`、`createUpdaterArtifacts: true`，签名公钥与端点指向 GitHub Release 的 `latest.json`）；应用启动 3 秒后静默检查一次，有新版才弹窗（`ui/src/utils/updateCheck.ts` + `stores/updater.ts` 的 8 相位状态机 + `panels/UpdateModal.tsx`），可在设置页「关于」页用「启动时自动检查更新」开关关闭（`ws_auto_update`）
