# fence 计划模式白名单误拦 PowerShell 只读命令（缺陷修复）

## 缺陷

计划（plan）模式下 command 工具执行纯只读的 PowerShell 原生管道：

```powershell
Get-ChildItem -Path '<项目目录>\ui' | Select-Object Name, Length | Format-Table -AutoSize
```

被安全围栏拦截为 `E_PLAN_READONLY`（计划模式只读拦截），提示「命令不在只读白名单内（ls/cd/head/grep/git log/gh pr view 等只读命令）」（2026-09-19 起因错误文案统一点名被拦命令，见 [docs/plan-mode-workflow](./plan-mode-workflow.md) §7.1）。该命令语义等价于 `ls`（列目录 + 元信息），属白名单应放行的只读形态；Windows 环境（无 Git Bash 时 command 工具回退 PowerShell，`tools/command.rs`）下计划模式几乎无法用原生方式列目录/读文件。

## 根因

`src-tauri/src/safety/fence.rs` 的判定链三处叠加：

1. **平台错配**：`PLAN_READONLY_CMDS` 只收录 POSIX 命令形态（ls/cat/grep/git/…），零 PowerShell cmdlet；而 Windows 回退 shell 是 PowerShell——白名单与实际执行 shell 脱节；
2. **L0 全有或全无**：`plan_readonly_allowed()` 要求整条命令**每个管道段首命令**都命中名单，`Get-ChildItem` / `Select-Object` / `Format-Table` 三段全不在名单 → 整条不直通；
3. **G5 转换**：L0 兜底 `Confirm(HighRisk)` 后由计划模式出口统一转 `Block E_PLAN_READONLY`（[docs/plan-mode-workflow](./plan-mode-workflow.md) §7 的只读承诺设计），不可被误点确认旁路——拦截是「设计内生效、名单外误伤」。

## 修复

`PLAN_READONLY_CMDS` 追加 PowerShell 只读段（比较前 `to_ascii_lowercase`，大小写无关）：

| 分组 | 条目 |
|---|---|
| 枚举/读取/定位/元信息 | get-childitem · get-content · get-item · get-psdrive · get-process · get-service · get-command · get-help · get-member · get-date · get-location · get-random · test-path · resolve-path · measure-object · compare-object · select-string |
| 对象管线（无副作用变换，= POSIX sort/uniq/cut 同级） | select-object · where-object · foreach-object · sort-object · group-object · format-table · format-list · format-wide · out-string · out-host |
| 常用别名 | gci · gc · gi · sls · ft · fw · select · foreach · where · `%` · `?` |

POSIX 名在 PowerShell 中同为别名（ls/cat/echo/sort/diff 等），天然复用既有条目。L0 拦截提示文案同步提及 PowerShell（`Get-ChildItem 等只读命令`）。

## 安全边界（写语义零放宽）

- **写类 cmdlet 一律不入名单**：Set-/New-/Add-/Remove-/Copy-/Move-/Clear-/Invoke-/Start-/Stop- 等全部排除；
- **tee-object 明确排除**：其写目标在 `-FilePath` 参数中，L2 重定向扫描（只看 bash `file_redirect` 节点）不可见，一旦收录即成静默写通道——与 POSIX tee 因此列 `WRITE_ALL` 同理；同理不入名单的还有 Out-File（测试用例覆盖）；
- **白名单直通 ≠ 放行**：命中名单的命令仍落 L3 + AST 写目标扫描，`Get-ChildItem > 区内文件` 经 `file_redirect` 判 InsideWrite → G5 转 Block；区外/灾难语义同理；
- **L1 兜底不变**：bash AST 解析失败走 fallback 词扫描时 `remove-item` / `del` / `ri` 逐词命中删除黑名单（t15 用例既有），管道段混入 `Remove-Item` 时由 AST `handle_command` 判定拦截（新用例覆盖）；
- **已知取舍**：`foreach-object` / `where-object` 等脚本块类按 awk/sed 收录先例处理——块内参数式写与 awk `system()` 同类残留风险，L1/L3 仍全文兜底。

## 测试

新增 3 个用例函数（覆盖计划用例 5 项）：

| 用例 | 断言 |
|---|---|
| `plan_readonly_powershell_pipe_passes` | 用户原命令直通 Allow（大小写不敏感）；`Get-Content \| sls` 直通；`gci \| ft` 别名直通 |
| `plan_readonly_powershell_delete_still_blocked` | `Get-ChildItem -Recurse \| Remove-Item -Force` → Block |
| `plan_readonly_powershell_writes_still_blocked` | `Get-ChildItem > inside.txt`（区内重定向，G5）→ Block；`Get-Content \| Out-File out.txt` → Block |

验证：`cargo test` 全量 272 passed / 0 failed / 0 warning（其中 `provider::tests_integration::midstream_disconnect_maps_to_network` 首轮失败经单独复跑 3 次全过，判定为并发环境抖动，与本改动无关——fence 为纯函数层，不触 provider）；fence 模块 34/34。端到端：修复后在计划模式会话内重放用户原始命令，直通执行。
