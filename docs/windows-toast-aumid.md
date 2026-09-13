# Windows 通知 AUMID 安装时注册

> 2026-09-08。缺陷：Windows 系统通知显示 PowerShell 图标/标题、点击无法回跳对应会话。
> 根因与修复方案：AUMID 归属；注册动作前移到**安装时**（NSIS 钩子写注册表），运行时零子进程。

## 一、根因：AUMID 属主

Windows toast 的图标/标题归属与点击激活都跟随 **AUMID（AppUserModelID）属主**。此前
[notify.rs](../src-tauri/src/host/notify.rs) 的 Windows 路径借用
`POWERSHELL_APP_ID` 发通知（未注册 AUMID 的应用发 toast 不显示的社区兜底，插件现行行为一致），
两个症状同根：

1. 通知显示为 **PowerShell** 图标与「Windows PowerShell」标题（crate 文档原话：
   「the toast will erroneously report its origin as powershell」）；
2. 点击激活被路由给 PowerShell——进程内 `on_activated` 回调不触发，
   `notify:activate` 事件不发，前端 reveal 链路（[notification-click-reveal](./notification-click-reveal.md)）无从启动。

## 二、修复：安装时注册 AUMID（NSIS 钩子）

**设计约束（用户定）**：不得在运行时拉子进程/写注册表（杀软误报高风险）；注册动作前移到安装时。

- **安装钩子**（[src-tauri/windows/installer-hooks.nsh](../src-tauri/windows/installer-hooks.nsh)，
  经 tauri.conf.json `bundle.windows.nsis.installerHooks` 接线）：
  - `NSIS_HOOK_POSTINSTALL`：写 `HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave`
    （`DisplayName` = CodeWave，`IconUri` = `$INSTDIR\icons\128x128.png`）；
  - `NSIS_HOOK_PREUNINSTALL`：`DeleteRegKey /ifempty` 清理。
  - 只写 HKCU——安装无需管理员权限。该键是 unpackaged 应用注册 AUMID 的官方简化方式
    （Microsoft.Toolkit.Uwp.Notifications v7 起同款机制），注册后 toast 以 CodeWave 品牌
    显示、点击激活在本进程内触发。
  - **IconUri 必须是真实图片文件**（PNG/JPG）：toast 平台不解析 exe 内嵌图标——写 exe 路径
    会渲染为空图标（实测）。128x128.png 经 `bundle.resources` 随安装落盘至
    `$INSTDIR\icons\128x128.png`，钩子指向该路径；dev 键同理指向仓库内 PNG。
- **运行时只读检测**（notify.rs `aumid_registered`，winreg 只读打开键）：键存在 →
  `Toast::new(identifier)`；不存在（dev/便携/MSI 安装）→ 回退 `POWERSHELL_APP_ID`
  （只保显示，PowerShell 归属与点击回跳不可用——与旧行为一致，无回归）。
- **禁令**：运行时绝不写注册表/创建快捷方式/拉起 powershell——曾评估的三条运行时路线全部
  废弃（Rust 手搓 COM 属性存储堆损坏；PowerShell `ExtendedProperty` 写入在 Win11 26200 报
  DISP_E_MEMBERNOTFOUND；运行时拉 PowerShell 有杀软误报风险）。

## 三、依赖升级（随本批）

`cargo update` 全量 + `tauri-winrt-notification` 0.7.3 → **0.8.1**（API 兼容；
`POWERSHELL_APP_ID` / `on_activated` / `Toast::new(&str)` 不变）。
tauri 2.11.5 / tauri-plugin-notification 2.4.0 已是最新 2.x。

## 四、dev 环境验证

dev/便携形态没有安装器，需一次性手动注册（与安装器写入一致）：

```bat
reg add "HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave" /v DisplayName /t REG_SZ /d "CodeWave" /f
reg add "HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave" /v IconUri /t REG_SZ /d "<exe 绝对路径>" /f
```

删除：`reg delete "HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave" /f`。

探针（真实 toast，人工确认品牌归属与点击回跳）：

```bash
cargo test --lib probe_own_aumid_toast -- --ignored --nocapture
```

## 五、WiX（MSI）路径

MSI 走 WiX 片段（[src-tauri/windows/toast-aumid.wxs](../src-tauri/windows/toast-aumid.wxs)，
经 tauri.conf.json `bundle.windows.wix.fragmentPaths` + `componentRefs` 接线，组件注入默认模板的
External Feature）：`Component`（稳定 GUID，Directory="INSTALLDIR"）下
`RegistryKey Root="HKCU" Key="Software\Classes\AppUserModelId\xyz.yangshifu.codewave"`，
`DisplayName` 为 KeyPath（卸载时随组件移除），`IconUri = [INSTALLDIR]icons\128x128.png`。
已验证：MSI 构建成功且 Registry 表含上述两条（Root=1 即 HKCU）。
注意：per-machine MSI 的 HKCU 写入发生在执行安装的用户下（Windows Installer 语义）。

## 六、已知边界

- 点击回跳的端到端确认（事件 → 前端 reveal）仍需人工：点探针 toast 观察主窗口是否聚焦并
  切到对应会话。
- 前端回退链不变：`notify_system` 失败（如 unsupported 平台）→ 插件路径（无 session_id，
  点击仅聚焦）。
