# POSIX 命令全集风险矩阵与 fence 判定映射

> 2026-09-11 · 状态标注约定：**✅ = 已随本批次落地**（fence 判定表现状，落点为真实表名）；**📋 = 矩阵留位**（建议等级与建议落点，本期未落地，仅作全集盘点与后续批次排期依据）。
>
> 本文档是 CodeWave fence（`src-tauri/src/safety/fence/check.rs`）判定表的系统性盘点基准。动机：fence 此前按「痛点驱动逐案增补」（命令名反混淆、PowerShell AST、check_write_target 四象限等，见 [docs/fence-hardening-and-powershell-ast](./fence-hardening-and-powershell-ast.md) 与 [AGENTS.md](../AGENTS.md) 契约锚点），本批次升级为「全集覆盖」——以 IEEE Std 1003.1-2017（POSIX.1-2017）「Shell & Utilities」章节名录为底册逐条定级，非 POSIX 的常用生态（GNU coreutils 补充、压缩家族、macOS 集等）单列第四节。权限四档与 fence 的语义总表见 [docs/composer-toolbar-batch-report](./composer-toolbar-batch-report.md) §2.2；plan 档白名单机制见 [docs/fence-plan-readonly-powershell](./fence-plan-readonly-powershell.md)。

## §1 五级风险框架与 fence 映射

| 等级 | 语义 | fence 落点（check.rs 符号） | 审批开 | 审批关 | plan 档 |
|---|---|---|---|---|---|
| **R0 只读** | 无文件系统写语义（查看 / 文本变换 / 查询） | L0 白名单 `PLAN_READONLY_CMDS`；非 plan 档走正常判定链自然 Allow | Allow | Allow | 白名单直通；带写重定向仍经 L2 判 InsideWrite → G5 转 Block |
| **R1 隐式写** | 默认形态即改写文件系统，写目标不来自参数（就地压缩/解压改写或删除原文件） | `IMPLICIT_WRITE_CMDS` 按名 Confirm（stdout 豁免 flag 例外，见 §2.1） | Confirm | Block | G5 → Block（`E_PLAN_READONLY`） |
| **R2 显式写** | 写目标来自参数 | `WRITE_ALL` / `WRITE_LAST` / `WRITE_FIRST` + `check_write_target` | 见下方四象限 | Confirm 类退化为 Block，Block 不变 | 同左；区内写 → G5 Block |
| **R3 高危** | 进程 / 服务 / 持久化 / 远程宿主 / 任意代码执行面 | `high_risk_match` 与按名高危判定表 → `HighRisk` | Confirm | Block | G5 → Block |
| **R4 灾难** | 不可逆 / 系统级（写盘、文件系统、电源、/etc） | `disaster_match` 与按名灾难判定表 → `Disaster` | Confirm | Block | G5 → Block |
| **L1 删除** | 删除语义（比 R4 更严：不提供确认路径） | `DELETE_CMDS` 黑名单 | Block | Block | Block |

**check_write_target 四象限**（R2 的写目标判定核心）：①词法在 roots 内且解析在内 → Allow（ConfirmEach/Plan 档 `confirm_inside_writes` 下升级 InsideWrite Confirm）；②词法在外、经符号链接解析回根内 → 同①；③区外且路径已存在 → Block（`E_PATH_OUTSIDE`）；④区外且为新建 → 按 `confirm_outside_create` 策略 Confirm / Allow。词法在内而解析在外（符号链接逃逸）一律 Block。

**plan 档语义**：`plan_readonly` 下只有 L0 白名单可直通；白名单外命令一律 Confirm 后由 G5 统一转 Block（`E_PLAN_READONLY`）——包括白名单内命令携带写重定向（InsideWrite）的形态。plan 档的只读承诺不可被一次误点确认绕过（G5 设计见 check.rs 判定主流程注释与 [docs/fence-plan-readonly-powershell](./fence-plan-readonly-powershell.md)）。

## §2 已落地的判定表增补（✅ 本批次内容）

### 2.1 R1 隐式写压缩族（`IMPLICIT_WRITE_CMDS`）

收录 14 个命令：`gzip` `gunzip` `compress` `uncompress` `pack` `unpack` `bzip2` `bunzip2` `xz` `unxz` `zstd` `unzstd` `lz4` `unlz4`。语义：默认形态就地改写文件系统（如 `gzip file` 删除原文件留下 `file.gz`，解压反之），写目标不来自参数——按名 Confirm，审批关退化为 Block，plan 档 G5 转 Block。

| flag | 语义 | 判定 |
|---|---|---|
| `-c` / `--stdout` / `--to-stdout` | 输出至 stdout，原文件不动 | 豁免 → 不升级（按普通只读链判定） |
| `-t` / `--test` | 完整性测试，纯读 | 豁免 |
| `-l` / `--list` | 列压缩元信息，纯读 | 豁免 |
| 组合短 flag（`-dc` / `-9c` / `-tv` 等） | 同一 flag 内多字母 | 按字母拆分：**含 c 或 t 且不含 k/r 即豁免**（含 k 或 r 则整体不豁免，`bzip2 -kr` 照 Confirm） |
| `-k` / `--keep` | 保留原文件但仍写压缩件（新文件落盘） | **不豁免** → Confirm |
| `-r` | 递归子目录批量写 | **不豁免** → Confirm |
| `zcat` 族（zcat/bzcat/xzcat/zstdcat/lz4cat） | 等价 `-c` 形态的既有别名 | 归 R0，入 L0 白名单（§2.2） |

### 2.2 L0 白名单扩充（`PLAN_READONLY_CMDS`）

| 分组 | 条目 |
|---|---|
| 压缩只读族（15） | `zcat` `zgrep` `zfgrep` `zegrep` `zless` `zmore` `zcmp` `zdiff` `bzcat` `bzgrep` `xzcat` `xzgrep` `zstdcat` `zstdgrep` `lz4cat` |
| 文件查证 / 文本变换（18） | `cmp` `diff3` `sdiff` `nl` `tac` `rev` `od` `xxd` `hexdump` `strings` `readlink` `realpath` `basename` `dirname` `md5sum` `sha256sum` `shasum` `cksum` |

### 2.3 R2 写目标表扩充

| 表 | 新增 | 说明 |
|---|---|---|
| `WRITE_LAST` | `ln` `rsync` `chmod` `chown` `chgrp` | 末个非 flag 参数按写目标判定；`chmod 777` 类仍由 L3 特判先行命中 |
| `DOWNLOAD_CMDS` 带值写 flag | curl/wget：`-o` `--output` `--output-document`；PowerShell iwr/irm：`-outfile` | flag 的值按写目标判定（下载落盘进入可见范围） |

### 2.4 L3 高危（按名 Confirm，审批关退化为 Block）

| 分组 | 命令 | 理由 |
|---|---|---|
| 进程 | `kill` `killall` `pkill` | 信号 / 批量终止进程，影响面超出工作区 |
| 持久化 / 服务 | `crontab` `at` `systemctl` `service` `launchctl` | 计划任务与服务管理 = 延迟 / 持久化执行通道 |
| 任意代码执行 | `osascript` | AppleScript 可驱动任意应用与系统行为 |
| 远程宿主 | `ssh` `scp` `sftp` | 远程宿主内**不做内层递归**（无法静态判定远端语义），按名确认即止 |
| macOS 配置写 | `defaults` `plutil` | 系统配置域写入，落点在 roots 外且语义非文件路径 |

### 2.5 R4 灾难

| 命令 | 判定 |
|---|---|
| `fdisk` `disklabel` `parted` `gparted` | 磁盘 / 分区操作 → 按名灾难 |
| `diskutil` | 子命令感知：`erase*` 与 `apfs delete*` → 灾难；其余子命令 → 高危 |

### 2.6 S7 审查返工增补（内容驱动写与子命令感知）

首审发现 sed -i / tar / unzip / cpio 需求断点（plan 档 `sed -i` 因 sed 在白名单被静默放行），返工补齐：

| 命令 | 判定 |
|---|---|
| `sed -i` / `-i.bak` / `--in-place` | 就地改写输入文件本身（隐式写）→ Confirm；无 -i 维持 L0 只读放行；plan 档 L3 命中 Confirm 后 G5 转 Block |
| `tar` | 子命令感知：`-x`/`--extract` 解包 → Confirm（写落点不可见）；`-t`/`--list` 只读（tar 入 L0，白名单直通≠放行）；`-c`/`-r`/`-u`/`--delete` 的 `-f` 归档名走写目标判定 |
| `unzip` | 只读形态（`-l`/`-t`/`-v`/`-Z` 等）不升级（unzip 入 L0）；解包 → Confirm |
| `cpio -i` / `--extract` | copy-in 解包写落点不可见 → Confirm（cpio 不入 L0，plan 档由 L0 拦） |

四组均 AST + 词法双路径，文案含「原地/解包」关键词；tar 词法镜像无 -f 归档名判定（fallback 本无写目标扫描，已知残留）；BSD 无前导 `-` 形态（`tar xf x.tar`）AST 侧按短簇含 x 识别。

## §3 POSIX.1-2017 Shell & Utilities 全集分类表

> 底册：IEEE Std 1003.1-2017「Shell & Utilities」章节名录，共 **162 条**，按字母序分组逐条列出、无遗漏。分类原则：§2 已落地表直接映射（✅）；未落地的标准命令给「建议等级」并标注留位（📋）——留位条目非本批次范围。
>
> 图例：`R0*` = 语义只读但携带子执行 / 环境注入面，按 §6 审计原则暂缓入 L0；`L1` = 删除黑名单硬 Block（无确认路径）。差异备注仅在 GNU/BSD/BusyBox 有实质差异时填写。

### A–B（admin … bg）

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| admin | 📋 R2 | 留位：SCCS s-file 历史写（建议 WRITE_LAST） | 现代 SCCS 少随附 |
| alias | 📋 R0 | 留位：建议入 L0（shell 会话内定义，不落盘） | builtin |
| ar | 📋 R2 | 留位：归档创建 / 修改（建议 WRITE_LAST） | ops 子命令语义各家一致 |
| asa | 📋 R0 | 留位：建议入 L0（FORTRAN 控制符变换，纯 stdout） | — |
| at | ✅ R3 | L3 high_risk（§2.4 持久化） | — |
| awk | ✅ R0 | L0 白名单 | `system()` / `print > file` 为已知残留（§5）；gawk/mawk/BusyBox awk 特性子集不一 |
| basename | ✅ R0 | L0 白名单（本批） | — |
| bc | 📋 R0 | 留位：建议入 L0 | — |
| bg | 📋 R0 | 留位：建议入 L0（作业控制；非交互 shell 无实际意义） | builtin |

### C

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| c99 | 📋 R2 | 留位：编译器 `-o` 输出与中间产物（建议 `-o` 值按写目标查） | 现代 clang/gcc 承载 |
| cal | 📋 R0 | 留位：建议入 L0 | — |
| cat | ✅ R0 | L0 白名单 | — |
| cd | ✅ R0 | L0 白名单 | builtin |
| cflow | 📋 R0 | 留位：建议入 L0（默认 stdout） | — |
| chgrp | ✅ R2 | WRITE_LAST + check_write_target（本批） | — |
| chmod | ✅ R2 | WRITE_LAST + check_write_target（本批）；777/a+rwx/o+w/+s/4755 由 L3 特判先行 | — |
| chown | ✅ R2 | WRITE_LAST + check_write_target（本批） | — |
| cksum | ✅ R0 | L0 白名单（本批） | — |
| cmp | ✅ R0 | L0 白名单（本批） | — |
| comm | 📋 R0 | 留位：建议入 L0 | — |
| compress | ✅ R1 | IMPLICIT_WRITE_CMDS Confirm（§2.1） | macOS/BSD 内置；Linux 多需 ncompress 包 |
| cp | ✅ R2 | WRITE_LAST + check_write_target | — |
| cpio | ✅ R1 | copy-in（-i/--extract）解包 Confirm（§2.6） | o/p 形态留位：`-o` copy-out 读 stdin 写 stdout 归档，属 R2 留位 |
| crontab | ✅ R3 | L3 high_risk（§2.4 持久化） | — |
| csplit | 📋 R2 | 留位：固定名片段 xx00… 写 cwd（建议按固定名写 Confirm） | `-f` 前缀各家一致 |
| ctags | 📋 R2 | 留位：默认写 ./tags | exuberant / universal 实现差异 |
| cut | ✅ R0 | L0 白名单 | — |
| cxref | 📋 R0 | 留位：建议入 L0（默认 stdout；`-o` 写形态随编译器批次处理） | — |

### D–E

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| date | ✅ R0 | L0 白名单 | GNU `date -s` / BSD 位置参数可设置系统时钟——L3 未覆盖，残留边角（§5 补充观察） |
| dd | ✅ R2 | WRITE_ALL 逐参查 + `of=/dev/*` 灾难特判 | — |
| delta | 📋 R2 | 留位：SCCS s-file 写 | — |
| df | ✅ R0 | L0 白名单 | — |
| diff | 📋 R0 | 留位：建议入 L0（本批仅收 cmp/diff3/sdiff） | 各家一致 |
| diff3 | ✅ R0 | L0 白名单（本批） | — |
| dirname | ✅ R0 | L0 白名单（本批） | — |
| du | ✅ R0 | L0 白名单 | — |
| echo | ✅ R0 | L0 白名单 | builtin 与 /bin/echo 并存 |
| ed | 📋 R2 | 留位：stdin 脚本化 `w` 可写任意文件（写落点不可见，建议 Confirm） | — |
| env | ✅ R0 | TRANSPARENT 透传剥离后按真实命令重判 | — |
| ex | 📋 R2 | 留位：同 ed（ex 脚本写落点不可见） | vi 家族 ex 模式 |
| expand | 📋 R0 | 留位：建议入 L0 | — |
| expr | 📋 R0 | 留位：建议入 L0 | — |

### F–G

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| false | 📋 R0 | 留位：建议入 L0 | builtin |
| fc | 📋 R0* | 留位：`fc -s` 重执行历史命令属子执行面，按 §6 暂缓 | builtin |
| fg | 📋 R0 | 留位：建议入 L0（非交互 shell 无实际意义） | builtin |
| file | ✅ R0 | L0 白名单 | — |
| find | ✅ R0 | L0 白名单；`-delete` / `-exec rm` 由 L1 特判 Block | `-exec` 语义各家一致；`-delete` 为 GNU/BSD 共有扩展 |
| fold | 📋 R0 | 留位：建议入 L0 | — |
| fort77 | 📋 R2 | 留位：编译器（同 c99） | 现代系统普遍无 f77 前端 |
| fuser | 📋 R0* | 留位：查询形态 R0；`-k` 杀进程建议按 R3（BSD 无 `-k`） | — |
| gencat | 📋 R2 | 留位：消息目录文件写（建议 WRITE_LAST） | — |
| getconf | 📋 R0 | 留位：建议入 L0 | — |
| getopts | 📋 R0 | 留位：建议入 L0 | builtin |
| gettext | 📋 R0 | 留位：建议入 L0（翻译查询，纯读） | GNU gettext 工具族 |
| grep | ✅ R0 | L0 白名单 | GNU `-P` 扩展各家不一；压缩 grep 族见 §4.2 |

### H–L

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| hash | 📋 R0 | 留位：建议入 L0 | builtin |
| head | ✅ R0 | L0 白名单 | — |
| iconv | 📋 R0 | 留位：建议入 L0（默认 stdout） | — |
| id | 📋 R0 | 留位：建议入 L0 | — |
| ipcrm | 📋 R3 | 留位：不可逆删除 System V IPC 资源（无文件写目标可查，按名 Confirm） | — |
| ipcs | 📋 R0 | 留位：建议入 L0 | — |
| jobs | 📋 R0 | 留位：建议入 L0 | builtin |
| join | 📋 R0 | 留位：建议入 L0 | — |
| kill | ✅ R3 | L3 high_risk（§2.4，本批） | builtin 与 /bin/kill 语义一致 |
| link | 📋 R2 | 留位：硬链接创建（建议 WRITE_LAST） | 各家一致 |
| ln | ✅ R2 | WRITE_LAST + check_write_target（本批）；多目标只查末参（§5） | — |
| locale | 📋 R0 | 留位：建议入 L0 | — |
| localedef | 📋 R2 | 留位：编译 locale 写系统目录（roots 外，区外已存在 Block 兜底） | — |
| logger | 📋 R1 | 留位：写 syslog（固定系统路径、非参数目标） | BusyBox `-n` 网络 syslog 扩展 |
| logname | 📋 R0 | 留位：建议入 L0 | — |
| lp | 📋 R1 | 留位：打印 spool 写 + 物理外泄 | CUPS lp 各家一致 |
| ls | ✅ R0 | L0 白名单 | — |

### M

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| m4 | 📋 R0* | 留位：syscmd/esyscmd 子执行面，按 §6 暂缓入 L0 | GNU m4 扩展 builtin |
| mailx | 📋 R1 | 留位：写系统邮箱（/var/mail/* 追加）+ 网络外发 | BSD mailx vs s-nail（Linux） |
| make | 📋 R3 | 留位：recipe 任意构建期 shell 执行（§5），按名 Confirm | GNU make vs BSD make 语法分歧 |
| man | 📋 R0* | 留位：MANPAGER/PAGER 环境注入执行面，按 §6 暂缓 | man-db 用 MANPAGER、BSD 用 PAGER |
| mesg | 📋 R0 | 留位：建议入 L0（终端写权限开关，非文件） | — |
| mkdir | ✅ R2 | WRITE_ALL + check_write_target | — |
| mkfifo | 📋 R2 | 留位：FIFO 创建（建议 WRITE_ALL，语义同 mkdir） | — |
| more | 📋 R0 | 留位：建议入 L0 | — |
| msgconv | 📋 R0 | 留位：默认 stdout（`-o` 写形态随 gettext 批次处理） | — |
| msgfmt | 📋 R2 | 留位：.mo 编译写盘（建议 `-o` 值按写目标查） | — |
| msggrep | 📋 R0 | 留位：建议入 L0 | — |
| msginit | 📋 R2 | 留位：po 文件生成写盘 | — |
| msgmerge | 📋 R2 | 留位：po 就地更新（写目标 = 参数） | — |
| msgunfmt | 📋 R0 | 留位：默认 stdout | — |
| mv | ✅ R2 | WRITE_LAST + check_write_target | — |

### N–P

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| newgrp | 📋 R0* | 留位：启动新 shell（子执行面），按 §6 暂缓 | — |
| nice | ✅ R0 | TRANSPARENT 透传剥离 | — |
| nl | ✅ R0 | L0 白名单（本批） | — |
| nm | 📋 R0 | 留位：建议入 L0 | — |
| nohup | ✅ R0 | TRANSPARENT 透传剥离 | stdout 为终端时写 ./nohup.out（残留边角，§5 补充观察） |
| od | ✅ R0 | L0 白名单（本批） | — |
| paste | 📋 R0 | 留位：建议入 L0 | — |
| patch | 📋 R2 | 留位：写目标来自补丁内容（写落点不可见），建议语义级 Confirm | GNU `--dry-run` / BSD 兼容 |
| pathchk | 📋 R0 | 留位：建议入 L0 | — |
| pax | 📋 R2 | 留位：`-r` 解包写落点不可见 / `-w` 写归档（两面性同 tar，§4.2） | BSD 主推；GNU 侧少随附 |
| pr | 📋 R0 | 留位：建议入 L0 | — |
| printf | 📋 R0 | 留位：建议入 L0 | builtin 与 /usr/bin 并存 |
| prs | 📋 R0 | 留位：建议入 L0（SCCS 历史打印） | — |
| ps | ✅ R0 | L0 白名单 | BSD `ps aux` 与 UNIX `ps -ef` 语法分歧（POSIX 只定后者） |
| pwd | ✅ R0 | L0 白名单 | builtin |

### Q–R

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| qdel | 📋 R3 | 留位：批处理作业删除（信号类） | XSI 队列接口，现代发行版普遍未实现（Q 组同） |
| qhold | 📋 R1 | 留位：队列状态写 | — |
| qmove | 📋 R1 | 留位：队列状态写 | — |
| qrerun | 📋 R1 | 留位：队列状态写 | — |
| qrls | 📋 R1 | 留位：队列状态写 | — |
| qselect | 📋 R0 | 留位：建议入 L0（队列查询） | — |
| qsig | 📋 R3 | 留位：向作业发信号 | — |
| qstat | 📋 R0 | 留位：建议入 L0（队列查询） | — |
| qsub | 📋 R3 | 留位：提交脚本延迟执行（子执行面） | — |
| read | 📋 R0 | 留位：建议入 L0（stdin 赋值变量，无落盘） | builtin |
| renice | 📋 R0* | 留位：无文件写但影响系统进程调度，暂缓入 L0 观察 | — |
| rm | ✅ L1 | DELETE_CMDS 黑名单硬 Block | — |
| rmdel | 📋 R3 | 留位：SCCS delta 删除（不可逆） | — |
| rmdir | ✅ L1 | DELETE_CMDS 黑名单硬 Block | — |

### S

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| sact | 📋 R0 | 留位：建议入 L0（SCCS 活动查询） | — |
| sccs | 📋 R2 | 留位：前端随子命令（delta/admin 等）写 s-file | — |
| sed | ✅ R0/R1 | L0 白名单；`-i`/`-i.bak`/`--in-place` 就地写 Confirm（§2.6） | `-i`：GNU 可无后缀、BSD 必带备份后缀、BusyBox 同 GNU；两形态均已被 sed_in_place_hit 谓词覆盖 |
| sh | ✅ R3 | INTERP_CMDS + `-c` 内层递归（深度 ≤ 2） | dash/zsh 同族处理 |
| sleep | 📋 R0 | 留位：建议入 L0 | — |
| sort | ✅ R0 | L0 白名单 | `-o` 可写输出文件（各家一致）——L0 直通下的残留边角（§5 补充观察） |
| split | 📋 R2 | 留位：固定名片段 xaa… 写 cwd（建议按固定名写 Confirm） | GNU `-d` 数字后缀扩展 |
| strings | ✅ R0 | L0 白名单（本批） | — |
| strip | 📋 R2 | 留位：就地改写二进制（建议按参数写目标 Confirm） | 各家一致 |
| stty | 📋 R0 | 留位：终端设备状态（非文件） | — |

### T–U

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| tabs | 📋 R0 | 留位：终端 tab stop 设置（非文件） | — |
| tail | ✅ R0 | L0 白名单 | — |
| talk | 📋 R0 | 留位：建议入 L0（终端会话） | 现代系统渐弃 |
| tar | ✅ R0/R1 | 子命令感知：`-t` 只读入 L0；`-x` 解包 R1 Confirm；`-c/-r/-u/--delete` 归档名走写目标（§2.6） | `-f` 值形态各家一致；BSD 老式无前导 `-` 形态 AST 侧已覆盖、词法镜像不识别 |
| tee | ✅ R2 | WRITE_ALL + check_write_target | — |
| test | 📋 R0 | 留位：建议入 L0（文件测试，纯读） | builtin；`[` 同义 |
| time | ✅ R0 | TRANSPARENT 透传剥离 | builtin 与 GNU /usr/bin/time 格式差异 |
| touch | ✅ R2 | WRITE_ALL + check_write_target | — |
| tput | 📋 R0 | 留位：建议入 L0 | terminfo / termcap 库差异 |
| tr | ✅ R0 | L0 白名单 | — |
| tsort | 📋 R0 | 留位：建议入 L0 | — |
| tty | 📋 R0 | 留位：建议入 L0 | — |
| type | 📋 R0 | 留位：建议入 L0 | builtin |
| ulimit | 📋 R0 | 留位：建议入 L0 | builtin |
| umask | 📋 R0 | 留位：显示形态无害；`umask 000` 影响后续创建文件权限，准入时留意 | builtin |
| unalias | 📋 R0 | 留位：建议入 L0 | builtin |
| uname | ✅ R0 | L0 白名单 | — |
| uncompress | ✅ R1 | IMPLICIT_WRITE_CMDS Confirm（§2.1） | — |
| unexpand | 📋 R0 | 留位：建议入 L0 | — |
| unget | 📋 R2 | 留位：SCCS p-file 写 | — |
| uniq | ✅ R0 | L0 白名单 | — |
| unlink | ✅ L1 | DELETE_CMDS 黑名单硬 Block | — |
| uucp | 📋 R2 | 留位：UUCP 队列写 + 远程复制 | 现代普遍未安装 |
| uudecode | 📋 R2 | 留位：输出文件名内嵌编码流（写落点不可见），建议 Confirm | 各家一致 |
| uuencode | 📋 R0 | 留位：建议入 L0（stdout 编码） | — |
| uustat | 📋 R0 | 留位：建议入 L0（队列查询） | — |
| uux | 📋 R3 | 留位：远程宿主命令执行 | 同 uucp |

### V–Z

| 命令 | 等级 | fence 落点 | GNU/BSD/BusyBox 差异备注 |
|---|---|---|---|
| val | 📋 R0 | 留位：建议入 L0（SCCS 校验，纯读） | — |
| vi | 📋 R2 | 留位：脚本化 ex 模式可写文件 | BSD nvi vs Linux vim |
| wait | 📋 R0 | 留位：建议入 L0 | builtin |
| wc | ✅ R0 | L0 白名单 | — |
| what | 📋 R0 | 留位：建议入 L0（SCCS 串查找） | — |
| who | 📋 R0 | 留位：建议入 L0 | — |
| write | 📋 R0 | 留位：向他人终端写消息（非文件；干扰性留位观察） | — |
| xargs | ✅ R0* | L1 删除特判（任一参数为删除命令名即 Block）；子执行面按 §6 不入 L0 | 各家一致 |
| xstr | 📋 R2 | 留位：字符串表中间文件写（xstr.c / xs.c） | 历史工具，少实现 |
| yacc | 📋 R2 | 留位：y.tab.c / y.tab.h 写 cwd | bison `-y` 兼容模式 |
| zcat | ✅ R0 | L0 白名单（本批；等价 `gzip -dc` 形态） | 各家一致 |

## §4 扩充生态

### 4.1 GNU coreutils 补充

| 命令 | 状态 | 备注 |
|---|---|---|
| `sha256sum` `shasum` | ✅ L0（本批文件查证族） | macOS 无 sha256sum，惯用 `shasum -a 256`；md5 系在 BSD 名为 `md5` 而非 `md5sum` |
| `realpath` `readlink` | ✅ L0（本批） | realpath 与 readlink `-f` 为 macOS 13+ 才内置（旧版缺） |
| `stat` | 既有 L0 | GNU `stat -c` 与 BSD `stat -f` 语法分歧（只读语义不受影响） |
| `timeout` | 既有 TRANSPARENT 透传剥离 | BSD/macOS 无内置 timeout（coreutils 装后名 `gtimeout`） |
| `xargs` | 既有 L1 删除特判 | 任一参数为删除命令名即 Block；子执行面按 §6 不入 L0 |

### 4.2 压缩家族全景

| 家族 | R1 写工具（✅ IW，§2.1） | cat 别名族（✅ R0 L0） | 查看 / grep 族（✅ R0 L0） |
|---|---|---|---|
| gzip | gzip · gunzip | zcat | zgrep · zfgrep · zegrep · zless · zmore · zcmp · zdiff |
| bzip2 | bzip2 · bunzip2 | bzcat | bzgrep |
| xz | xz · unxz | xzcat | xzgrep |
| zstd | zstd · unzstd | zstdcat | zstdgrep |
| lz4 | lz4 · unlz4 | lz4cat | — |
| compress | compress · uncompress | （复用 zcat） | — |
| pack | pack · unpack | — | — |

**tar / unzip（✅ 已随 S7 返工落地）**：`tar -t`/`unzip -l` 只读形态已入 L0（白名单直通≠放行）；`tar -x`/`unzip` 解包 → Confirm；`tar -c/-r/-u/--delete` 归档名走写目标判定（§2.6）。pax（📋 留位）两面性同类，后续批次照 §2.6 模式处理。

### 4.3 macOS 最小集

| 命令 | 等级 | 状态 |
|---|---|---|
| osascript | R3 | ✅ 已落地（§2.4 任意代码执行） |
| defaults · plutil | R3 | ✅ 已落地（§2.4 配置写） |
| diskutil | erase* 与 apfs delete* → R4；其余 → R3 | ✅ 已落地（§2.5 子命令感知） |
| pbcopy · open · mdfind | 无害 | 不入表（剪贴板 / 系统应用打开 / Spotlight 查询） |

## §5 已知静态分析残留（如实声明）

沿 [docs/composer-toolbar-batch-report](./composer-toolbar-batch-report.md) §5.3 先例，以下残留为静态分析的固有权衡，如实声明不遮掩：

| # | 残留 | 说明 |
|---|---|---|
| 1 | awk `system()` | awk 在 L0 白名单，脚本内 `system("…")`（及 `print > file` 输出语句）可执行任意命令 / 写文件，fence 不可见 |
| 2 | 运行时 unset IFS | `${IFS}` 展开模型假设 IFS 保持默认值；运行时 `unset IFS` 后 `r${IFS}m` 等价 `rm`，文本层不可见（与 check.rs `expand_ifs` 注释同口径） |
| 3 | 管道跨进程写 | 管道右端 `tee /outside` 的 tee 已按 WRITE_ALL 查；进程替换 `<(...)` 内的写目标不可见（AST 不下钻进程替换语义） |
| 4 | WRITE_LAST 多目标漏查 | `ln a b c` 只查末参 c，多链接 / 多源形态的中段目标漏查（WRITE_LAST「末参 = 目标」语义对 cp/mv 恰好正确、对 ln 多目标场景漏） |
| 5 | make install | makefile recipe 任意构建期 shell 代码执行，fence 不可见（make 留位 R3，见 §3） |
| 6 | ~~sed -i~~ 已解决 | 首版未入表致 plan 档 sed -i 静默放行；S7 审查拦截后已随返工落地 Confirm 化（§2.6），plan 档 G5 转 Block（rw02 用例固化） |
| 7 | curl 组合短 flag 内嵌 -o | `-sSo out` 内嵌的 `-o` 写目标不可见；带值写 flag（§2.3）只识别独立 `-o` / `--output` 形态 |

**补充观察**（非本批次新增口径，第三节差异备注的延伸，留待后续批次评估）：`date -s` / BSD 位置参数设置系统时钟（已入 L0、无 L3 兜底）；`sort -o` 可写输出文件（已入 L0）；`nohup` 在 stdout 为终端时写 `./nohup.out`。

## §6 白名单审计三原则与包管理器留位

L0 白名单（`PLAN_READONLY_CMDS`）准入三原则——任一命中即不得准入：

| 原则 | 排除面 | 触发例（本矩阵留位依据） |
|---|---|---|
| 无子执行 | 命令可执行「输入 / 参数 / 环境」中的任意代码 | xargs、man（MANPAGER）、m4（syscmd）、make、fc -s、uux、qsub、newgrp；awk `system()` 属已入名单的同类残留（§5） |
| 无参数写能力 | 非 WRITE_* 形态的隐藏写（固定名输出 / 内容驱动写） | csplit/split（固定名片段）、ctags（./tags）、patch/uudecode/ed（内容驱动）、pax -r（§3） |
| 无环境注入执行体 | 环境变量决定执行体 | MANPAGER / PAGER（man）、VISUAL / EDITOR 类、FCEDIT（fc） |

**包管理器留位（📋 本批次明确不纳入）**：`apt` / `brew` / `npm` / `pip` / `cargo install` 等不进矩阵判定表——全局安装落点在 roots 之外，由 `check_write_target` 既有四象限兜底（区外已存在 Block、区外新建按 `confirm_outside_create` 策略）；包管理器自身的安装后脚本执行面（npm postinstall 等）需独立批次设计。矩阵留位，不进 L0。
