; CodeWave NSIS 安装器钩子（tauri.conf.json → bundle.windows.nsis.installerHooks）。
; Windows 通知 AUMID 注册（[docs/windows-toast-aumid](../docs/windows-toast-aumid.md)）：
; toast 的图标/标题归属与点击激活都跟随 AUMID 属主——安装时把本应用 identifier 注册为
; AppUserModelId（DisplayName + IconUri），运行时 notify.rs 只读检测后用自家 AUMID 发通知，
; 不再借用 POWERSHELL_APP_ID（那会让通知显示为 PowerShell 且点击回跳失效）。
; 注意：identifier / DisplayName 必须与 tauri.conf.json（identifier / productName）保持一致。
; 仅写 HKCU——安装无需管理员权限，卸载仅清理本键。

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Classes\AppUserModelId\xyz.yangshifu.codewave" "DisplayName" "CodeWave"
  ; IconUri 必须是真实图片文件（toast 平台不解析 exe 内嵌图标，写 exe 会渲染为空）；
  ; 128x128.png 经 bundle.resources 随安装落盘（tauri.conf.json → bundle.resources）
  WriteRegStr HKCU "Software\Classes\AppUserModelId\xyz.yangshifu.codewave" "IconUri" "$INSTDIR\icons\128x128.png"
  ; 品牌改名兜底：清理改名前旧 bundle id 的 AUMID 键（2026-09-13 前版本所写），
  ; 避免升级后通知归属残留指向旧标识
  DeleteRegKey /ifempty HKCU "Software\Classes\AppUserModelId\work.gitwave.studio"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DeleteRegKey /ifempty HKCU "Software\Classes\AppUserModelId\xyz.yangshifu.codewave"
!macroend
