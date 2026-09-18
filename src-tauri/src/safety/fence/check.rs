//! fence 判定核心：三级分析 L1 删除黑名单 → L2 AST 写目标 → L3 高危模式，外加 plan 档只读白名单（L0）。
//! shell 可切换（cmd/fish/WSL）加固：cmd/fish 危险命令词表（名字级 + 清空写短语）与 wsl 内层命令串递归检查。
//! 加固批次 [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：命令名反混淆（`r"m"`、`${IFS}`）、
//! `--force-with-lease` 不再误报、PowerShell AST 遍历。

use crate::tools::pathutil::{self, WriteRoots};
use std::path::{Path, PathBuf};

/// Confirm 升级的理由：决定审批卡上的提示文案，也是 plan 档 G5 拦截时的「为什么」。
#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmReason {
    /// 高危操作（chmod 777 / git push --force / 下载直接执行等）——审批关闭时退化为 Block
    HighRisk(&'static str),
    /// 不可逆灾难（dd 写盘 / mkfs / 写 /etc / 电源操作）——全权限档也直接拦截
    Disaster(&'static str),
    /// 写目标落在工作区内（ConfirmEach/Plan 档下升级为确认）
    InsideWrite(String),
    /// 写目标落在 roots 之外且为新建路径（按 confirm_outside_create 策略确认）
    OutsideCreate(String),
}

/// fence 判定结论：允许 / 需确认 / 直接拦截。
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// 放行
    Allow,
    /// 升级为用户确认（审批关闭时由调用方退化为 Block）
    Confirm(ConfirmReason),
    /// 直接拦截（含错误码与给用户/模型看的原因）
    Block {
        /// 稳定错误码（如 E_COMMAND_BLOCKED / E_PATH_OUTSIDE / E_PLAN_READONLY）
        code: String,
        /// 可读拦截原因
        message: String,
    },
}

/// L1 删除黑名单：反混淆后的命令名一出现即 Block（POSIX 与 PowerShell 形态合并维护）。
const DELETE_CMDS: &[&str] = &[
    "rm",
    "rmdir",
    "unlink",
    "del",
    "erase",
    "rd",
    "remove-item",
    "ri",
    "shred",
];
/// 每个非 flag 参数都视为写目标的命令
const WRITE_ALL: &[&str] = &["tee", "dd", "touch", "install", "mkdir", "md"];
/// 最后一个非 flag 参数是写目标的命令（[POSIX 命令全集加固批次]：ln/rsync/chmod/chown/chgrp
/// 的最后一个位置参数同样是写目标；多目标形态如 `ln a b c`（目录）只查最后一个，
/// 属已知残留——宁可过拦的 WRITE_ALL 不适用，这些命令首个位置参数多为源/模式）。
const WRITE_LAST: &[&str] = &[
    "cp", "mv", "copy", "cpi", "mi", "copy-item", "move-item",
    // [POSIX 命令全集加固批次]：写目标在末位（ln 目标、rsync 目标、chmod/chown/chgrp 的目标文件）
    "ln", "rsync", "chmod", "chown", "chgrp",
];
/// PowerShell 参数式写盘 cmdlet（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：AST 化后可见，此前不可见，[docs/fence-plan-readonly-powershell](../../../../docs/fence-plan-readonly-powershell.md)）。
/// 每个非 flag 参数都按写目标判定——带值的命名 flag（如
/// `-ItemType file`）可能贡献一个无害的根内额外目标（Allow/InsideWrite），但真正的
/// `-Path`/位置参数目标绝不会漏判。宁可过度拦截。
const WRITE_FIRST: &[&str] = &[
    "set-content",
    "add-content",
    "new-item",
    "out-file",
    "tee-object",
];
/// 下载工具（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：按名字做「下载-执行」关联——掩码 L3 文本扫描
/// 看不到带引号的命令名，如 `"curl" x | sh`）。
const DOWNLOAD_CMDS: &[&str] = &[
    "curl",
    "wget",
    "iwr",
    "irm",
    "invoke-webrequest",
    "invoke-restmethod",
];
/// [POSIX 命令全集加固批次] 隐式写命令：压缩/解压工具默认**替换/删除原文件**（gzip x
/// 删 x 留 x.gz；gunzip x.gz 删 x.gz 留 x），绕过 L2 重定向与写目标扫描（无重定向、
/// 参数是「源」不是「目标」）。除非显式指定 stdout/测试模式（-c/--stdout/-t 等），
/// 否则按高危确认。判定逻辑见 eval_command_with（AST）与 fallback_scan（词法回退）。
/// 组合短 flag 中含 k（--keep）或 r 时不豁免：gzip -k 保留原文件、bzip2 -r 递归
/// 原地处理，隐式写语义仍在（k 只是不删源、不改变写 x.gz 的事实）。
const IMPLICIT_WRITE_CMDS: &[&str] = &[
    "gzip", "gunzip", "compress", "uncompress", "pack", "unpack", "bzip2", "bunzip2", "xz",
    "unxz", "zstd", "unzstd", "lz4", "unlz4",
];
/// 执行管道输入或传参脚本的 shell/解释器命令名。
const INTERP_CMDS: &[&str] = &["sh", "bash", "zsh", "dash", "iex", "invoke-expression"];

/// cmd.exe 危险命令表（shell 可切换为 cmd 后的词法加固；命令名级、只增不删）。
/// 覆盖盘/卷、注册表、服务/计划任务/进程、WMI、权限/属性、引导/系统映像等
/// 破坏性系统操作；命中一律升级 Confirm（审批关闭退化为 Block）。del/erase/rd/
/// rmdir 走 DELETE_CMDS；shutdown 归 disaster_match（灾难级更高，不在此重复）。
/// 按首命令词命中，参数位置的普通词（`grep format x`）不受影响。
const CMD_DANGER_CMDS: &[&str] = &[
    // 盘与文件系统
    "format", "diskpart", "cipher", "chkdsk", "compact", "label", "subst",
    // 注册表
    "reg", "regedit", "regsvr32",
    // 服务 / 计划任务 / 进程
    "sc", "schtasks", "taskkill", "tskill",
    // WMI
    "wmic",
    // 权限 / 属性 / ACL
    "takeown", "icacls", "cacls", "attrib",
    // 引导 / 挂载 / 系统映像
    "mountvol", "bcdedit", "bcdboot", "bootcfg", "dism", "sfc",
    // 卷影 / 备份 / 事件日志 / 审计
    "vssadmin", "wbadmin", "wevtutil", "auditpol",
    // 网络 / 共享 / 用户（net use / net user … /delete）
    "net", "netsh",
];

/// cmd 清空写/擦除短语（词法回扫专用）：解析失败时无 AST 写目标扫描，
/// `type nul > x`、`more +0 > x` 的截断写语义靠短语命中兜底。
const CMD_DANGER_KEYWORDS: &[&str] = &["type nul", "more +0"];

/// fish 持久化/执行入口（shell 可切换为 fish 后）：funcsave/funced/abbr 把函数与
/// 缩写写进 ~/.config/fish 持久化配置，source 立即执行任意脚本——按名确认。
const FISH_DANGER_CMDS: &[&str] = &["source", "funcsave", "funced", "abbr"];

/// WSL 宿主入口名：wsl/wsl.exe 是无自身文件写语义的分发器，外层放行、
/// 内层命令串经 extract_wsl_inner 提取后递归判定。
const WSL_HOSTS: &[&str] = &["wsl", "wsl.exe"];

/// cmd 宿主入口名：`cmd /c|/k <内层>` 在 bash/PS AST 中是「外层 cmd + 参数内层」
/// 结构，内层危险命令对名字级判定表不可见，需提取重判（judge_cmd_host）。
const CMD_HOSTS: &[&str] = &["cmd", "cmd.exe"];

/// fence 策略（权限档 → 判定参数，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）。
#[derive(Debug, Clone, Copy)]
pub struct FencePolicy {
    /// false = 高危/无法判定的 Confirm 退化为 Block（现行「审批关闭」语义）
    pub approval_enabled: bool,
    /// roots 外新建路径是否需要 Confirm
    pub confirm_outside_create: bool,
    /// 工作区内写目标是否升级为 Confirm（ConfirmEach / Plan）
    pub confirm_inside_writes: bool,
    /// Plan 档：只读白名单命令放行；白名单外一律 Confirm（无静默放行路径）
    pub plan_readonly: bool,
}

impl FencePolicy {
    /// 现行默认行为（AutoEdit 语义）。
    pub fn legacy(approval_enabled: bool, confirm_outside_create: bool) -> Self {
        FencePolicy {
            approval_enabled,
            confirm_outside_create,
            confirm_inside_writes: false,
            plan_readonly: false,
        }
    }
}

/// Plan 档只读命令白名单：命令名（剥路径；sudo/env 等透传前缀本身不列入）。
/// plan_readonly 下，每条管道段的首命令都命中白名单才放行；
/// 任一段不在名单内 → 落回正常判定。写语义（git push --force、
/// find -delete 等）仍由既有 L1-L3 安全网兜底。
/// Windows 回退 PowerShell（tools/command.rs）：白名单同时收录 PowerShell
/// 只读 cmdlet/别名，比较前先 to_ascii_lowercase，故大小写无关。安全
/// 边界：只收录无副作用的只读 cmdlet；
/// 全部写类（Set-/Add-/New-/Remove-/Copy-/Move-/Invoke-/Start-/Out-File/Clear-）不入名单；
/// remove-item/del/ri 仍由 L1 删除黑名单兜底；以参数落盘的 tee-object 明确排除——
/// 其写目标在参数里，L2 重定向扫描不可见（与 POSIX tee 列入 WRITE_ALL 同理）。
/// 注意：foreach/where/%/? 等脚本块管道沿 awk/sed 先例列入
///（bash AST 仍会判定块内每个命令名/重定向，L1/L2/L3 兜底；
/// 块内参数式写入是已知残留，与 awk system() 同类）。
const PLAN_READONLY_CMDS: &[&str] = &[
    // 目录/文件查看
    "ls", "pwd", "cat", "head", "tail", "wc", "stat", "du", "df", "tree", "which", "file",
    // 目录切换（只读定位；与后续白名单命令配合使用，写语义由 AST 写目标扫描兜底）
    "cd", // 搜索
    "grep", "rg", "find", "ag",
    // 基本 git 只读（push --force 等写语义经 L3 升级）
    "git", "diff", "show", "log", "blame",
    // gh：命令名入列后**仍须过子命令白名单**（gh_plan_readonly_allowed）——
    // 远端写（pr merge / release edit --draft=false / api -X POST / secret set）不在 L1-L3 覆盖范围，
    // 整命令放行等于让 plan 档能合 PR、发版、改 secret（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) §7）
    "gh",
    // 文本处理（管道内只读；出现重定向时由 L2 拦截）
    "echo", "sort", "uniq", "cut", "tr", "column", "jq", "sed", "awk",
    // 系统/进程信息
    "ps", "uname", "whoami", "date", "printenv", "lsof",
    // ===== POSIX 命令全集加固批次：压缩只读（不解压落盘，只向 stdout 输出/查看） =====
    // 压缩文件上的 grep/cat 等价物；裸 zcat 形态（含 .Z/.gz/.xz/.zst）全家族：
    // zcat/zgrep/zfgrep/zegrep/zless/zmore/zcmp/zdiff（gzip 家族）·
    // bzcat/bzgrep（bzip2）· xzcat/xzgrep（xz）· zstdcat/zstdgrep（zstd）· lz4cat（lz4）。
    // 注意：gzip/gunzip 等隐式写本体不入白名单，由 IMPLICIT_WRITE_CMDS 单独判定。
    "zcat", "zgrep", "zfgrep", "zegrep", "zless", "zmore", "zcmp", "zdiff", "bzcat", "bzgrep",
    "xzcat", "xzgrep", "zstdcat", "zstdgrep", "lz4cat",
    // ===== POSIX 命令全集加固批次：文件查证（逐字节比对/十六进制转储/元信息读取） =====
    // cmp/diff3/sdiff：文件比对只读；nl/tac/rev：文本变换到 stdout；
    // od/xxd/hexdump/strings：内容转储；readlink/realpath/basename/dirname：路径演算；
    // md5sum/sha256sum/shasum/cksum：哈希校验。全部无写语义（结果走 stdout）。
    "cmp", "diff3", "sdiff", "nl", "tac", "rev", "od", "xxd", "hexdump", "strings", "readlink",
    "realpath", "basename", "dirname", "md5sum", "sha256sum", "shasum", "cksum",
    // ===== [S7 审查返工增补] 归档工具（白名单直通≠放行）：tar -t/unzip -l 只读形态
    // 经 AST 判定放行；tar -x/unzip 解包形态由 AST Confirm 后经 G5 转 E_PLAN_READONLY，
    // plan 档只读承诺不破。cpio 不入白名单（plan 档由 L0 拦，属预期）。
    "tar", "unzip",
    // ===== PowerShell 只读 cmdlet / 别名（[docs/fence-plan-readonly-powershell](../../../../docs/fence-plan-readonly-powershell.md)：Windows 回退 shell 的原生只读管道） =====
    // 目录/文件/系统信息：枚举/读取/定位/存在性/元数据——纯读
    "get-childitem", "get-content", "get-item", "get-psdrive", "get-process", "get-service",
    "get-command", "get-help", "get-member", "get-date", "get-location", "get-random",
    "test-path", "resolve-path", "measure-object", "compare-object", "select-string",
    // 对象管道工具（无副作用变换，与 POSIX sort/uniq/cut 同级）
    "select-object", "format-table", "format-list", "format-wide", "out-string", "out-host",
    "sort-object", "group-object", "where-object", "foreach-object",
    // 常用别名（select-string 连同其 sls 别名一并列入；POSIX 名
    // ls/cat/echo/sort/diff 在 PowerShell 中本就是别名，天然复用）
    "gci", "gc", "gi", "sls", "ft", "fw", "select", "foreach", "where", "%", "?",
];

/// [POSIX 命令全集加固批次] L3 新增高危命令表（进程控制/持久化/任意代码执行/远程宿主/
/// macOS 配置写；审批开启 → Confirm，关闭 → Block，经 high_risk_verdict 分叉）。
/// 白名单审计三原则（plan 档为什么不放行它们）：
/// ① 无子执行——命令不把参数当程序/脚本执行（排 env、xargs、find -exec：参数即执行体，
///    可注入任意命令）；
/// ② 无参数写——所有参数只读不落盘（排 tee、touch、install：参数就是写目标）；
/// ③ 无环境注入执行体——不从环境变量/配置文件解析并执行代码（排 less/more：
///    LESSOPEN 钩子会执行外部程序；排 man：MANPAGER/LESSOPEN 同理；排 env：-u/-i
///    之外任意形态都能携带 VAR=cmd 执行；排 xargs：构造并执行命令行）。
/// 因此 less/more/env/xargs/man 一律不入 PLAN_READONLY_CMDS；本表为 L3 高危而非白名单成员。
/// AST 镜像：eval_command_with 按名判定；词法路径：high_risk_match 逐词扫描。
const HIGH_RISK_CMDS: &[&str] = &[
    // 进程控制：终止/信号任意进程，可杀掉用户会话或系统服务（pgrep 仅查询不入）
    "kill", "killall", "pkill",
    // 持久化与计划任务：写入 cron/at 队列、操纵系统服务（可驻留重启后执行）
    "crontab", "at", "systemctl", "service", "launchctl",
    // 任意代码执行：osascript 可执行 AppleScript/JXA，等价任意代码
    "osascript",
    // 远程宿主：ssh/scp/sftp 在远端执行命令或传输文件，本地 fence 无法判定远端语义
    //（不做内层递归提取：远端 shell 语法/环境不可静态枚举，直接按名确认）
    "ssh", "scp", "sftp",
    // macOS 配置写：defaults/plutil 读写用户偏好与 plist，可篡改应用/系统行为实现持久化
    "defaults", "plutil",
];

/// 感知引号的分隔符切分：单/双引号内的 | ; & 不作分隔；与重定向相邻的
/// & 也不切（如 2>&1，其写语义由 AST file_redirect 扫描覆盖）。修复：此前朴素
/// 按字符切分会把 grep 正则 "(a|b)" 里的 | 当管道，误拦首词为白名单的段。
fn split_unquoted_separators(cmd: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let bytes: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < bytes.len() {
        let c = bytes[i];
        // 行继续归一：`\` + 换行在 shell 里被移除（引号外与双引号内都生效，单引号内是字面量）。
        // 不先吃掉它，`grep foo \` + 换行 + `  file.rs` 会被下面的换行分隔切成两段，
        // 第二段首词（如 `file.rs`）不在白名单 → plan 档多拦一条本应合法的只读命令。
        // 注意 `\\`（转义的反斜杠）必须先整体吃掉：否则 `\\` + 换行会被误当续行，
        // 把两条命令拼成一段而绕过后面的分段判定（写成原子处理即无此漏）。
        if c == '\\' && quote != Some('\'') {
            match (bytes.get(i + 1).copied(), bytes.get(i + 2).copied()) {
                (Some('\\'), _) => {
                    cur.push('\\');
                    cur.push('\\');
                    i += 2;
                    continue;
                }
                (Some('\n'), _) => {
                    i += 2;
                    continue;
                }
                (Some('\r'), Some('\n')) => {
                    i += 3;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            cur.push(c);
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                cur.push(c);
            }
            '|' | ';' | '&' => {
                // 与重定向相邻的 & 不切分：>&（含 2>&1）与 &>（&>file）。
                // 注意 && 的第二个 &（prev=='&'）仍要切分——否则 cd x && head 不再分段。
                let prev = if cur.is_empty() {
                    bytes.get(i.wrapping_sub(1)).copied()
                } else {
                    cur.chars().last()
                };
                let next = bytes.get(i + 1).copied();
                let redirect_adjacent =
                    (c == '&' && prev == Some('>')) || (c == '&' && next == Some('>'));
                if redirect_adjacent {
                    cur.push(c);
                } else {
                    segs.push(cur.clone());
                    cur.clear();
                }
            }
            // 换行也是命令分隔符（shell 语义）：不切分的话 `gh pr view 38\ngh pr merge 38`
            // 会被当成一段，gh 子命令门只看段首 `pr view` 而放行（审查：本批引入的安全侧回归）。
            // 行继续（`\` + 换行）已在循环开头归一掉，不会再被这里切段。
            '\n' | '\r' => {
                segs.push(cur.clone());
                cur.clear();
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    segs.push(cur);
    segs
}

/// plan 档 `gh` 只读形态：二级子命令组（如 `pr view`）。
/// 为什么 gh 需要子命令粒度：L1-L3 安全网只覆盖文件写/重定向/已知高危命令，
/// **不认识** `gh pr merge` / `gh release edit --draft=false` / `gh api -X POST` / `gh secret set`
/// 这类远端写。注意别拿 `git` 类比：`git` 是命令级白名单，L3 只在 `--force` 时才兜
/// （`git push`（无 force）本就放行，是既有洞），所以远端写只能靠本表拦住。
/// （`pub(super)`：仅供同模块树的 `tests` 做「表内不得混入写子命令」的自检）
pub(super) const PLAN_READONLY_GH_PAIRS: &[(&str, &str)] = &[
    ("pr", "view"),
    ("pr", "list"),
    ("pr", "checks"),
    ("pr", "diff"),
    ("pr", "status"),
    ("run", "view"),
    ("run", "list"),
    ("release", "view"),
    ("release", "list"),
    ("issue", "view"),
    ("issue", "list"),
    ("issue", "status"),
    ("repo", "view"),
    ("repo", "list"),
    ("workflow", "view"),
    ("workflow", "list"),
    ("secret", "list"),
    ("variable", "list"),
    ("label", "list"),
    ("cache", "list"),
    ("auth", "status"),
    ("config", "get"),
    ("alias", "list"),
    ("extension", "list"),
    ("gist", "list"),
    ("ruleset", "list"),
    ("ruleset", "view"),
];

/// plan 档 `gh` 只读形态：单词子命令（`gh status`、`gh search repos …`）。
/// （`pub(super)`：同上，供 `tests` 做写子命令自检）
pub(super) const PLAN_READONLY_GH_WORDS: &[&str] = &["status", "search"];

/// `gh` 相关判定用的词归一化：去掉引号与转义（`-X "POST"` / `-X='POST'` / `-X\"POST\"`
/// 在 shell 去引号后都是写），并剥掉命令替换外壳（`$(gh …)` / `` `gh …` `` / `${gh …}`），
/// 最后统一小写。审查实测：不归一化就能用引号把写方法送进去，而 gh 子命令门是唯一防线。
fn normalize_gh_token(raw: &str) -> String {
    let mut s = raw.replace(['"', '\'', '\\'], "");
    let mut unwrapped = false;
    for prefix in ["$(", "${", "`"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            unwrapped = true;
        }
    }
    s = if unwrapped {
        // 只在真的剥过命令替换外壳时才去尾缀，避免把普通路径（如 `issues(1)`）改形（审查 🟢）
        s.trim_end_matches([')', '}']).to_string()
    } else {
        s
    };
    s.to_ascii_lowercase()
}

/// `gh api` 只读判定（入参已归一化，且不含开头的 `gh api`）。
/// 口径：只信显式的只读方法（`get`/`head`）；`-f/--field/-F/--raw-field/--input` 一出现即判写
/// （gh 有参数且未显式 `-X` 时默认改 POST）；方法值不可信（变量/未知写法/参数缺失）也判写。
fn gh_api_is_readonly(args: &[String]) -> bool {
    /// 唯一可信的只读方法
    fn is_read_method(method: &str) -> bool {
        matches!(method, "get" | "head")
    }

    let mut expect_method = false;
    for word in args {
        if expect_method {
            expect_method = false;
            if !is_read_method(word) {
                return false;
            }
            continue;
        }
        if word == "-f"
            || word == "--field"
            || word == "--raw-field"
            || word == "--input"
            || word.starts_with("-f=")
            || word.starts_with("--field=")
            || word.starts_with("--raw-field=")
            || word.starts_with("--input=")
        {
            return false;
        }
        if word == "-x" || word == "--method" {
            expect_method = true;
            continue;
        }
        if let Some(method) = word.strip_prefix("--method=") {
            if !is_read_method(method) {
                return false;
            }
            continue;
        }
        if let Some(rest) = word.strip_prefix("-x") {
            // 紧凑写法：`-XPOST` / `-X=POST`
            let rest = rest.trim_start_matches('=');
            if !rest.is_empty() && !is_read_method(rest) {
                return false;
            }
        }
    }
    // `-X` 后面没有取值（命令行到末尾）→ 不可信，判写
    !expect_method
}

/// plan 档 `gh` 子命令白名单判定（调用方已确认该段首命令是 gh）。
/// 保守口径：认不出的形态一律不放行（落回拦截）——宁可拦错，不放过远端写。
fn gh_plan_readonly_allowed(seg: &str) -> bool {
    let tokens: Vec<String> = seg.split_whitespace().map(normalize_gh_token).collect();
    let Some((_, rest)) = tokens.split_first() else {
        return false;
    };
    let Some(first) = rest.first() else {
        return false; // 裸 `gh`
    };
    if first.is_empty() || first.starts_with('-') {
        // `gh --version` 之类：不给子命令级判定，落回隐式拦截
        return false;
    }
    if PLAN_READONLY_GH_WORDS.contains(&first.as_str()) {
        return true;
    }
    if first == "api" {
        return gh_api_is_readonly(&rest[1..]);
    }
    let second = rest.get(1).map(String::as_str).unwrap_or("");
    PLAN_READONLY_GH_PAIRS
        .iter()
        .any(|(group, sub)| *group == first && *sub == second)
}

/// 被拦命令摘要：折叠换行与连续空白 → 截断 120 字符 → 超长补 `…`。
/// 用途：错误信息里点名**是哪条命令**被拦（此前只有泛化文案，卡片标题还会把命令截在半个 token 上）。
fn command_excerpt(cmd: &str) -> String {
    const MAX_CHARS: usize = 120;
    let flat = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_CHARS {
        return flat;
    }
    format!("{}…", flat.chars().take(MAX_CHARS).collect::<String>())
}

/// 判断整条命令是否全部由白名单命令构成（感知引号切分；每段首命令必须在列）。
fn plan_readonly_allowed(cmd: &str) -> bool {
    split_unquoted_separators(cmd)
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .all(|seg| {
            let first = seg.split_whitespace().next().unwrap_or("");
            let base = first
                .rsplit('/')
                .next()
                .unwrap_or(first)
                .to_ascii_lowercase();
            if !PLAN_READONLY_CMDS.contains(&base.as_str()) {
                return false;
            }
            // gh 额外过一道子命令白名单：远端写不在 L1-L3 覆盖范围内（见 gh_plan_readonly_allowed）
            base != "gh" || gh_plan_readonly_allowed(seg)
        })
}

/// 兼容入口：按旧版二元参数构造 FencePolicy::legacy 后判定。
pub fn check_command(
    cmd: &str,
    cwd: &Path,
    roots: &WriteRoots,
    approval_enabled: bool,
    confirm_outside_create: bool,
) -> Verdict {
    check_command_policy(
        cmd,
        cwd,
        roots,
        FencePolicy::legacy(approval_enabled, confirm_outside_create),
    )
}

/// 完整策略入口：构造/调用方传入 FencePolicy，内部转 depth 计数版本。
pub fn check_command_policy(
    cmd: &str,
    cwd: &Path,
    roots: &WriteRoots,
    policy: FencePolicy,
) -> Verdict {
    check_command_depth(cmd, cwd, roots, policy, 0)
}

/// 带嵌套深度的判定主流程：先走 inner 判定，plan_readonly 下把一切 Confirm 统一转 Block（G5）。
fn check_command_depth(
    cmd: &str,
    cwd: &Path,
    roots: &WriteRoots,
    policy: FencePolicy,
    depth: u8,
) -> Verdict {
    // G5（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) §7）：plan_readonly 下任何 Confirm 一律转 Block（E_PLAN_READONLY）。
    // 理由：plan 档的只读承诺不能被一次误点确认绕过；重定向引导语 = 把命令写进方案、批准后执行。
    // 覆盖面：白名单外命令、灾难/高危命令、白名单命令带写重定向（InsideWrite）——一切本需确认的路径。
    let verdict = check_command_inner(cmd, cwd, roots, policy, depth);
    if policy.plan_readonly {
        if let Verdict::Confirm(reason) = verdict {
            let why = match &reason {
                ConfirmReason::Disaster(w) | ConfirmReason::HighRisk(w) => *w,
                ConfirmReason::InsideWrite(_) | ConfirmReason::OutsideCreate(_) => "命令含写目标",
            };
            return Verdict::Block {
                code: "E_PLAN_READONLY".into(),
                message: format!(
                    "计划模式只读拦截（被拦命令：{}）：{why}。请将该命令纳入方案，经用户批准后执行；或改用只读白名单内的替代命令",
                    command_excerpt(cmd)
                ),
            };
        }
    }
    verdict
}

/// 实际判定逻辑：L0 白名单 → 解析链 → 掩码 L3 → AST 遍历（L1/L2）→ 下载-执行关联。
fn check_command_inner(
    cmd: &str,
    cwd: &Path,
    roots: &WriteRoots,
    policy: FencePolicy,
    depth: u8,
) -> Verdict {
    // L0：plan 档只读白名单。白名单外命令绝不静默放行；白名单内命令也不直接放行——
    // 它们可能带写重定向（如 `echo x > /etc/hosts`）或高危语义
    //（如 `git push --force`；git 在名单内），落向下方的 L3 + AST 写目标扫描，Confirm 由外层 G5 统一转 Block。
    if policy.plan_readonly && !plan_readonly_allowed(cmd) {
        if let Some(why) = disaster_match(cmd) {
            return Verdict::Confirm(ConfirmReason::Disaster(why));
        }
        if let Some(why) = high_risk_match(cmd) {
            return Verdict::Confirm(ConfirmReason::HighRisk(why));
        }
        return Verdict::Confirm(ConfirmReason::HighRisk(
            "命令不在只读白名单内（ls/cd/head/grep/git log/gh pr view 等只读命令）",
        ));
    }
    // 解析链（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)）：bash 语法 → PowerShell 语法（Windows 回退 shell，[docs/fence-plan-readonly-powershell](../../../../docs/fence-plan-readonly-powershell.md)）
    // → 词法回退。bash ERROR树先改投 PowerShell 语法再降级，原生 PS 语法
    //（`if ($x) { … }`、脚本块）因此进入 AST 可见范围。
    let tree = parse_grammar(cmd, tree_sitter_bash::LANGUAGE.into())
        .filter(|t| !t.root_node().has_error())
        .map(|t| (t, Grammar::Bash))
        .or_else(|| {
            parse_grammar(cmd, tree_sitter_powershell::LANGUAGE.into())
                .filter(|t| !t.root_node().has_error())
                .map(|t| (t, Grammar::PowerShell))
        });
    let Some((tree, grammar)) = tree else {
        return fallback_scan(cmd);
    };
    let root = tree.root_node();

    // L3 高危模式跑在字面量掩码后的文本上：字符串/注释数据不得触发
    // 灾难/高危启发式（如 `git commit -m "fix git push --force handling"`）。
    let masked = mask_literals(root, cmd.as_bytes());
    if let Some(why) = disaster_match(&masked) {
        if policy.approval_enabled {
            return Verdict::Confirm(ConfirmReason::Disaster(why));
        }
        return Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: format!("灾难性命令已拦截：{why}"),
        };
    }
    if let Some(why) = high_risk_match(&masked) {
        if policy.approval_enabled {
            return Verdict::Confirm(ConfirmReason::HighRisk(why));
        }
        return Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: format!("高危命令已拦截（审批开关关闭）：{why}"),
        };
    }
    // cmd 清空写短语（type nul > x / more +0 > x）：bash AST 下 type/more 在名字表
    // 可见，但重定向目标只是普通根内写——cmd 语义下这是截断写，需确认。
    if policy.approval_enabled && CMD_DANGER_KEYWORDS.iter().any(|k| masked.contains(k)) {
        return Verdict::Confirm(ConfirmReason::HighRisk(
            "cmd 清空写短语（type nul / more +0 重定向）按高危确认",
        ));
    }

    let src = cmd.as_bytes();
    let mut ctx = FenceCtx {
        src,
        cwd: cwd.to_path_buf(),
        roots,
        policy,
        depth,
        verdict: None,
        saw_download: false,
        saw_interp: false,
    };
    match grammar {
        Grammar::Bash => walk(root, &mut ctx),
        Grammar::PowerShell => {
            // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：PS 语法命令保留词法 L1 网——旧的 fallback 扫描有这层，
            // 而 AST 遍历只能看到 command_name 位置的删除命令名（`iex "rm $f"`
            // 把 `rm` 藏在字符串里，语法不会作为命令下钻）。
            if let Some(tok) = l1_token_scan(cmd) {
                return Verdict::Block {
                    code: "E_COMMAND_BLOCKED".into(),
                    message: format!("shell 删除命令 `{tok}` 被拦截，请改用 delete 工具"),
                };
            }
            // cmd/fish 词法网（与上方 L1 网同位）：PS AST 可能拆词（如 `ta^skill`），
            // raw 词扫描（lexical_tokens 剥 ^/引号）保持词表覆盖。宁可过度拦截。
            if cmd_danger_match(cmd).is_some() {
                if policy.approval_enabled {
                    return Verdict::Confirm(ConfirmReason::HighRisk(
                        "cmd 破坏性系统命令需用户确认；删除类（del/rd/erase）已由删除黑名单硬拦",
                    ));
                }
                return Verdict::Block {
                    code: "E_COMMAND_BLOCKED".into(),
                    message: "cmd 破坏性系统命令已拦截（审批开关关闭）".into(),
                };
            }
            if FISH_DANGER_CMDS
                .iter()
                .any(|f| lexical_tokens(cmd).iter().any(|t| t == f))
            {
                if policy.approval_enabled {
                    return Verdict::Confirm(ConfirmReason::HighRisk(
                        "fish 持久化/执行入口（source/funcsave/funced/abbr）需用户确认",
                    ));
                }
                return Verdict::Block {
                    code: "E_COMMAND_BLOCKED".into(),
                    message: "fish 持久化/执行入口已拦截（审批开关关闭）".into(),
                };
            }
            walk_ps(root, &mut ctx);
        }
    }
    // 按名字做「下载-执行」关联（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)）：带引号的下载/解释器名
    //（`"curl" x | sh`）对掩码 L3 文本扫描不可见，但作为 AST 命令存在。
    if ctx.saw_download && ctx.saw_interp {
        ctx.escalate(high_risk_verdict(ctx.policy, "下载脚本直接执行"));
    }
    ctx.verdict.unwrap_or(Verdict::Allow)
}

/// 灾难级结论（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md) 策略分叉）：审批开启时 Confirm，关闭时硬 Block。
fn disaster_verdict(policy: FencePolicy, why: &'static str) -> Verdict {
    if policy.approval_enabled {
        Verdict::Confirm(ConfirmReason::Disaster(why))
    } else {
        Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: format!("灾难性命令已拦截：{why}"),
        }
    }
}

/// 高危结论（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md) 策略分叉）：审批开启时 Confirm，关闭时硬 Block。
fn high_risk_verdict(policy: FencePolicy, why: &'static str) -> Verdict {
    if policy.approval_enabled {
        Verdict::Confirm(ConfirmReason::HighRisk(why))
    } else {
        Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: format!("高危命令已拦截（审批开关关闭）：{why}"),
        }
    }
}

/// [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md) 解析链中解析成功者对应的 shell 语法。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Grammar {
    /// POSIX bash 语法（tree-sitter-bash）
    Bash,
    /// Windows 回退 PowerShell 语法（tree-sitter-powershell）
    PowerShell,
}

/// 用指定语法解析命令文本；解析失败（含语言加载失败）返回 None。
fn parse_grammar(cmd: &str, language: tree_sitter::Language) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(cmd, None)
}

/// 把字面量节点（字符串/注释数据）抹成空白，让 L3 只评判真正会执行的代码而非引号内文本。
/// 内嵌命令替换（`$( )` / 反引号）或 PowerShell `$( )` 子表达式的字符串不掩码——其内容确实会执行；
/// heredoc 正文同样保持扫描（可能被管道喂给解释器）。按字节区间填空白可保 UTF-8 合法
///（语法的 offset 恒在字符边界上）。
fn mask_literals(root: tree_sitter::Node, src: &[u8]) -> String {
    fn descends_into(node: tree_sitter::Node, kinds: &[&str]) -> bool {
        let mut cursor = node.walk();
        let mut queue: Vec<_> = node.children(&mut cursor).collect();
        while let Some(n) = queue.pop() {
            if kinds.contains(&n.kind()) {
                return true;
            }
            let mut c = n.walk();
            queue.extend(n.children(&mut c));
        }
        false
    }
    fn collect(node: tree_sitter::Node, out: &mut Vec<(usize, usize)>) {
        let masked = match node.kind() {
            // bash：单引号原始字符串与注释是纯数据
            "raw_string" | "comment" => true,
            // bash 双引号字符串：除非内嵌命令替换，否则是纯数据
            "string" => !descends_into(node, &["command_substitution"]),
            // PowerShell 字符串（含双引号可展开串与 here-string）：除非内嵌子表达式，否则是纯数据
            "string_literal" | "expandable_string_literal" | "expandable_here_string_literal" => {
                !descends_into(node, &["sub_expression"])
            }
            _ => false,
        };
        if masked {
            out.push((node.start_byte(), node.end_byte()));
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect(child, out);
        }
    }
    let mut ranges = Vec::new();
    collect(root, &mut ranges);
    let mut masked: Vec<u8> = src.to_vec();
    for (s, e) in ranges {
        masked[s..e].fill(b' ');
    }
    String::from_utf8_lossy(&masked).into_owned()
}

/// AST 遍历共享上下文：源字节、工作目录、写根、策略与累计结论。
struct FenceCtx<'a> {
    /// 命令源文本字节（节点 utf8_text 取词用）
    src: &'a [u8],
    /// 命令执行目录（相对路径落点基准）
    cwd: PathBuf,
    /// 允许写入的根集合（工作区 + 数据目录）
    roots: &'a WriteRoots,
    /// 权限档对应的判定策略
    policy: FencePolicy,
    /// sh -c / powershell -Command 递归深度（上限 2）
    depth: u8,
    /// 已累计的最高优先级结论
    verdict: Option<Verdict>,
    /// [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：按名字的下载-执行关联标记（AST 遍历置位，遍历后统一判定）
    saw_download: bool,
    /// 是否出现过解释器命令（与 saw_download 配对触发升级）
    saw_interp: bool,
}

impl FenceCtx<'_> {
    // H4 修复：显式优先级 Block(2) > Confirm(1) > Allow(0)；只有更高位阶才能替换
    /// 累计结论：仅当新结论位阶更高时替换，避免低级别结论覆盖高级别。
    fn escalate(&mut self, v: Verdict) {
        let replace = match &self.verdict {
            None => true,
            Some(cur) => rank_of(&v) > rank_of(cur),
        };
        if replace {
            self.verdict = Some(v);
        }
    }
}

/// 结论位阶：Allow=0 < Confirm=1 < Block=2。
fn rank_of(v: &Verdict) -> u8 {
    match v {
        Verdict::Allow => 0,
        Verdict::Confirm(_) => 1,
        Verdict::Block { .. } => 2,
    }
}

/// bash AST 遍历：命令交给 handle_command，file_redirect 的目标按写目标判定。
fn walk(node: tree_sitter::Node, ctx: &mut FenceCtx) {
    if matches!(ctx.verdict, Some(Verdict::Block { .. })) {
        return;
    }
    match node.kind() {
        "command" => handle_command(node, ctx),
        "file_redirect" => {
            if let Some(dest) = node.child_by_field_name("destination") {
                let target = dest.utf8_text(ctx.src).unwrap_or("");
                // fd 复用（2>&1 / >&2）：目标是一个文件描述符而非文件——不是写目标。
                // 判据 = 目标全为数字且重定向操作符含 &；`cmd > 2`（写名为 "2" 的文件）无 &，仍照查。
                let node_text = node.utf8_text(ctx.src).unwrap_or("");
                let is_fd_dup = !target.is_empty()
                    && target.chars().all(|c| c.is_ascii_digit())
                    && node_text.contains('&');
                if !is_fd_dup {
                    // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：去引号目标的灾难级——带引号的目标（`> "/etc/x"`）
                    // 在掩码 L3 文本里被抹白，这里按反混淆形态判定。
                    let dequoted = target.trim_matches('"').trim_matches('\'');
                    if dequoted.starts_with("/etc/") {
                        ctx.escalate(disaster_verdict(ctx.policy, "写入 /etc"));
                    }
                    let v = check_write_target(target, &ctx.cwd, ctx.roots, ctx.policy);
                    ctx.escalate(v);
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, ctx);
    }
}

/// 透传前缀命令：剥离后按真实命令重判（H5）。
const TRANSPARENT: &[&str] = &["sudo", "env", "nohup", "nice", "timeout", "command", "time"];

/// [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：把 `${IFS}`/`$IFS` 展开为词分隔符。近似性说明：此模型假设「IFS 保持默认值」；
/// 运行时被 unset 的 IFS 会把 `r${IFS}m` 变成 `rm`，文本展开捕捉不到——业界文本防御共有的已知残留。
fn expand_ifs(lower: &str) -> String {
    lower.replace("${ifs}", " ").replace("$ifs", " ")
}

/// 反混淆命令名 token（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)）：展开 `${IFS}`/`$IFS`（运行时词分隔符），
/// 剥掉引号与反斜杠转义——`r"m"`、`'rm'`、`\rm`、`rm${IFS}` 一律归类为
/// `rm`。返回展开后的首个空白分隔 token（IFS 切出的名字片段只保留第一段：L1 按名判定，
/// 危险首 token 照样拦；无害首 token 在 AST 层面本就没有那些参数，无损失）。
fn deobfuscate_name(tok: &str) -> String {
    expand_ifs(&tok.to_ascii_lowercase())
        .chars()
        // cmd 转义符 ^ 一并剥除（ta^skill = taskkill）——只收紧不放松
        .filter(|c| !matches!(c, '\'' | '"' | '\\' | '^'))
        .collect::<String>()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// 词法 L1 兜底网：反混淆后逐 token 扫删除命令名。fallback_scan 与 PowerShell AST
/// 路径共用（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：PS 遍历只能看到 command_name 位置的名字，
/// 解析成功的 PS 命令因此保持回退扫描的覆盖面）。
fn l1_token_scan(cmd: &str) -> Option<String> {
    let lower = expand_ifs(&cmd.to_ascii_lowercase());
    for tok in lower.split(|c: char| {
        c.is_whitespace() || c == ';' || c == '|' || c == '&' || c == '(' || c == ')'
    }) {
        let tok: String = tok
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | '\\'))
            .collect();
        if DELETE_CMDS.contains(&tok.as_str()) {
            return Some(tok);
        }
    }
    None
}

/// bash 命令节点处理：提取（反混淆）命令名与参数，处理 sh -c 递归、透传前缀、xargs，
/// 最后交 eval_command_with 做名字级判定。
fn handle_command(node: tree_sitter::Node, ctx: &mut FenceCtx) {
    let name_node = node.child_by_field_name("name");
    let name = name_node
        .and_then(|n| n.utf8_text(ctx.src).ok())
        .map(deobfuscate_name)
        .unwrap_or_default();
    let args: Vec<String> = {
        let mut v = Vec::new();
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            if Some(ch) == name_node {
                continue;
            }
            // H3 修复：raw_string（单引号）与 concatenation 节点也收集；
            // M5 修复：number 节点同样收集（此前 `timeout 5` 能通过纯属数字被丢弃的巧合，数字型写目标完全不可见）
            if matches!(
                ch.kind(),
                "word" | "string" | "raw_string" | "concatenation" | "number"
            ) {
                let t = ch.utf8_text(ctx.src).unwrap_or("").to_string();
                let t = t
                    .trim_start_matches('\'')
                    .trim_end_matches('\'')
                    .to_string();
                v.push(t.trim_matches('"').to_string());
            }
        }
        v
    };

    // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：关联标记必须在这里也置位——sh 家族命令在下走到
    // eval_command_with 之前就短路返回（裸 `| sh` 不带 -c）。与覆盖透传剥离后
    // 名字的 eval_command_with 置位幂等（sudo curl …）。
    if DOWNLOAD_CMDS.contains(&name.as_str()) {
        ctx.saw_download = true;
    }
    if INTERP_CMDS.contains(&name.as_str()) {
        ctx.saw_interp = true;
    }

    // H5 修复：sh -c 'rm …' → 递归检查内层命令（深度上限 2；内层保持现行「视为审批开启」语义）。
    // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：powershell/pwsh 加入家族——`powershell -Command '…'` 会被解析成普通 bash
    // 单词，没有这一步嵌套的 PS 脚本永远不会被重判。
    if matches!(name.as_str(), "sh" | "bash" | "dash" | "zsh" | "powershell" | "pwsh")
        && ctx.depth < 2
    {
        if let Some(pos) = args
            .iter()
            .position(|a| a == "-c" || a.eq_ignore_ascii_case("-command"))
        {
            if let Some(inner) = args.get(pos + 1) {
                let sub = check_command_depth(
                    inner,
                    &ctx.cwd,
                    ctx.roots,
                    FencePolicy {
                        approval_enabled: true,
                        ..ctx.policy
                    },
                    ctx.depth + 1,
                );
                ctx.escalate(sub);
            }
        }
        return;
    }
    // cmd 宿主（shell 可切换为 cmd 后）：`cmd /c|/k <内层>` 提取内层重判
    // （WSL 判定之后、透传剥离之前；命中即短路）。
    if judge_cmd_host(&name, &args, ctx) {
        return;
    }
    // WSL 宿主（shell 可切换为 WSL 后）：提取内层命令串重判（比照 powershell
    // -Command 递归，深度上限 2；内层视为审批开启，确保最严格判定）。
    if judge_wsl(&name, &args, ctx) {
        return;
    }
    // H5 修复：剥离透传前缀（sudo/env/nohup/…）后按真实命令判定
    if TRANSPARENT.contains(&name.as_str()) {
        match strip_transparency(&name, &args) {
            Ok(Some((real_name, real_args))) => eval_command(&real_name, &real_args, ctx),
            Ok(None) => return,
            // H1 修复：flag-值形态无法静态判定（如 `sudo -u root rm` 曾把 root 当命令放过）
            // → 保守升级；宁可误 Confirm 不可漏 Allow
            Err(()) => {
                if ctx.policy.approval_enabled {
                    ctx.escalate(Verdict::Confirm(ConfirmReason::HighRisk(
                        "透传前缀带无法静态判定的 flag 参数",
                    )));
                } else {
                    ctx.escalate(Verdict::Block {
                        code: "E_COMMAND_BLOCKED".into(),
                        message: "透传前缀带无法静态判定的 flag 参数（审批开关关闭）".into(),
                    });
                }
                return;
            }
        }
    }
    // xargs：任一参数是删除命令名即拦（`find . | xargs rm`）
    if name == "xargs"
        && args.iter().any(|a| {
            let d = deobfuscate_name(a);
            DELETE_CMDS.contains(&a.as_str()) || DELETE_CMDS.contains(&d.as_str())
        })
    {
        ctx.escalate(Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: "xargs 的删除语义被拦截，请改用 delete 工具".into(),
        });
        return;
    }

    eval_command_with(&name, &args, Some(node), name_node, ctx);
}

/// tree-sitter-powershell 遍历（[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)）：PowerShell 语法无字段——命令名是
/// `command_name` 子节点，重定向目标在 `redirections → redirection →
/// redirected_file_name`（合并重定向 `2>&1`/`*>&1` 无文件目标子节点，天然跳过）。
/// 脚本块 / 循环 / 条件分支与其他节点一样下钻。
fn walk_ps(node: tree_sitter::Node, ctx: &mut FenceCtx) {
    if matches!(ctx.verdict, Some(Verdict::Block { .. })) {
        return;
    }
    match node.kind() {
        "command" => handle_command_ps(node, ctx),
        "redirection" => {
            let dest = node
                .children(&mut node.walk())
                .find(|c| c.kind() == "redirected_file_name");
            if let Some(dest) = dest {
                let target = dest.utf8_text(ctx.src).unwrap_or("");
                // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：去引号目标的灾难级（bash file_redirect 检查的镜像）
                let dequoted = target.trim_matches('"').trim_matches('\'');
                if dequoted.starts_with("/etc/") {
                    ctx.escalate(disaster_verdict(ctx.policy, "写入 /etc"));
                }
                let v = check_write_target(target, &ctx.cwd, ctx.roots, ctx.policy);
                ctx.escalate(v);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ps(child, ctx);
    }
}

/// PowerShell 命令节点处理：定位命令名（含 `& 'rm'` 包装形态）、宽松收集参数，
/// 处理 powershell -Command 递归后交共享判定表。
fn handle_command_ps(node: tree_sitter::Node, ctx: &mut FenceCtx) {
    let mut cursor = node.walk();
    let name_node = node.children(&mut cursor).find(|c| {
        // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：`& 'rm' x` 的名字包在 `command_name_expr` 里，普通调用在
        // `command_name`——两者都算。
        matches!(c.kind(), "command_name" | "command_name_expr")
    });
    let name = name_node
        .and_then(|n| n.utf8_text(ctx.src).ok())
        .map(deobfuscate_name)
        .unwrap_or_default();
    // 宽松的参数收集：除名字本身外的所有子 token（flag 是 `command_parameter`，
    // 值是字符串字面量/变量/裸 token）。重定向位于管道级 `redirections` 节点而非
    // `command` 内部，因此绝不会混进来。
    let args: Vec<String> = {
        let mut c = node.walk();
        node.children(&mut c)
            .filter(|ch| Some(*ch) != name_node)
            .flat_map(|ch| match ch.kind() {
                "command_parameter" | "generic_token" | "variable" | "string_literal"
                | "expandable_string_literal" => v_push_ps(ch.utf8_text(ctx.src).unwrap_or("")),
                _ => Vec::new(),
            })
            .collect()
    };

    // H5 等价物：powershell -Command 'inner' → 递归检查内层脚本（深度 ≤ 2）
    if matches!(name.as_str(), "powershell" | "pwsh") && ctx.depth < 2 {
        if let Some(pos) = args
            .iter()
            .position(|a| a.eq_ignore_ascii_case("-command") || a == "-c")
        {
            if let Some(inner) = args.get(pos + 1) {
                let sub = check_command_depth(
                    inner,
                    &ctx.cwd,
                    ctx.roots,
                    FencePolicy {
                        approval_enabled: true,
                        ..ctx.policy
                    },
                    ctx.depth + 1,
                );
                ctx.escalate(sub);
            }
        }
        return;
    }

    // cmd 宿主（shell 可切换为 cmd 后）：`cmd /c|/k <内层>` 提取内层重判；
    // 与 bash 路径同享 judge_cmd_host（powershell -Command 递归特判之后；命中即短路）。
    if judge_cmd_host(&name, &args, ctx) {
        return;
    }
    // WSL 宿主（shell 可切换为 WSL 后）：与 bash 路径同享 judge_wsl（
    // powershell -Command 递归特判之后、共享判定表之前；命中即短路）。
    if judge_wsl(&name, &args, ctx) {
        return;
    }

    // 共享 L1 + 写语义判定表（DELETE_CMDS 已含 remove-item/ri/del/rd/erase）
    eval_command_with(&name, &args, Some(node), name_node, ctx);
}

/// PowerShell 参数文本归一：剥掉包装节点文本携带的那对引号（`string_literal` 自带引号）。
fn v_push_ps(t: &str) -> Vec<String> {
    let trimmed = t
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| t.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(t);
    vec![trimmed.to_string()]
}

/// 已知带值的短 flag（sudo -u/-g、env -u、nice -n）——下一参数是其值，随之一并剥离。
const FLAG_VALUE_SHORT: &[&str] = &["-u", "-g", "-n"];

/// 剥离透传前缀：Ok(Some((name, args))) = 提取出真实命令；Ok(None) = 纯透传、无真实命令；
/// Err(()) = flag-值形态无法静态判定（评审 H1）；调用方必须保守升级为 Confirm/Block。
fn strip_transparency(name: &str, args: &[String]) -> Result<Option<(String, Vec<String>)>, ()> {
    let mut i = 0;
    // timeout 的首个位置参数是时长（`timeout 5 cmd` / `timeout 5s cmd`）
    if name == "timeout" && args.first().map(|a| is_duration(a)).unwrap_or(false) {
        i = 1;
    }
    while i < args.len() {
        let a = &args[i];
        if let Some(long) = a.strip_prefix("--") {
            if long.contains('=') {
                i += 1;
                continue;
            }
            // `--flag value`：无法静态判断是否带值 → 保守处理
            return Err(());
        }
        if a.starts_with('-') && a.len() > 1 {
            if FLAG_VALUE_SHORT.contains(&a.as_str()) {
                // 短 flag + 值：连值一并剥离（缺值时按裸 flag 处理）
                i += if i + 1 < args.len() { 2 } else { 1 };
                continue;
            }
            if a.contains('=') {
                i += 1;
                continue;
            }
            // 未知短 flag（bool 还是带值不明）：后随非 flag 参数时无法区分 → 保守处理
            if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                return Err(());
            }
            i += 1;
            continue;
        }
        break;
    }
    // env VAR=value 形态
    while i < args.len() && !args[i].starts_with('-') && args[i].contains('=') {
        i += 1;
    }
    let rest = &args[i..];
    if rest.is_empty() {
        return Ok(None);
    }
    let real = deobfuscate_name(&rest[0]);
    Ok(Some((real, rest[1..].to_vec())))
}

/// timeout 时长形态：纯数字，或数字 + 单位后缀（s/m/h/d）。
fn is_duration(s: &str) -> bool {
    let core = s.strip_suffix(['s', 'm', 'h', 'd']).unwrap_or(s);
    !core.is_empty() && core.chars().all(|c| c.is_ascii_digit())
}

/// [POSIX 命令全集加固批次] 隐式写命令（IMPLICIT_WRITE_CMDS）的豁免 flag 判定：
/// 参数中存在「纯 stdout 模式」flag（单 flag `-c`/`--stdout`/`--to-stdout`/`-t`/
/// `--test`/`-l`/`--list`，或组合短 flag 拆字符后含 c/t 且不含 k/r）即视为只读豁免——
/// 压缩流走 stdout/测试/列表模式，不替换原文件。比对前剥引号。组合含 k（--keep，
/// 保留源但仍写 .gz）或 r（递归原地处理）时不豁免。
/// 已知残留：单 flag 按整词比对，`gzip --stdout=x` 之类带值长形态不识别（不存在该用法，
/// 无实际风险）；组合拆分把 `-t` 与 `-c` 等价对待，tar 模式（gzip 无 -t，xz -t 为测试）
/// 统一按只读处理——宁可宽松于无写语义的形态。
fn implicit_write_exempt(args: &[String]) -> bool {
    args.iter().any(|a| {
        let a = a.trim_matches('"').trim_matches('\'');
        if matches!(
            a,
            "-c" | "--stdout" | "--to-stdout" | "-t" | "--test" | "-l" | "--list"
        ) {
            return true;
        }
        // 组合短 flag：`-dc`/`-cd`/`-tv`/`-9c` 拆成字符，含 c 或 t 且不含 k/r 即豁免
        if a.len() > 1 && a.starts_with('-') && !a.starts_with("--") {
            let cs: Vec<char> = a[1..].chars().collect();
            return cs.iter().any(|&ch| ch == 'c' || ch == 't')
                && !cs.iter().any(|&ch| ch == 'k' || ch == 'r');
        }
        false
    })
}

/// [POSIX 命令全集加固批次] 下载工具（DOWNLOAD_CMDS）的带值写 flag 判定：
/// 返回写目标值列表。识别形态：`-o 值`（curl 下一参数为值）、`--output=值`、
/// `--output 值`、`--output-document=值`/`--output-document 值`（wget 长形态）、
/// `-outfile 值`（PowerShell iwr/irm）。裸 `-O` 不查（落点恒为 cwd 文件名，
/// 由 L2 既有语义兜底）；curl `-sSo x` 组合短 flag 与 `-o` 混排形态是已知残留，
/// 不过度工程。目标值交给 check_write_target 判定。
fn download_write_targets(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if (a == "-o" || a == "--output" || a == "--output-document" || a.eq_ignore_ascii_case("-outfile"))
            && i + 1 < args.len()
        {
            out.push(args[i + 1].clone());
            i += 2;
            continue;
        }
        if let Some(v) = a
            .strip_prefix("--output=")
            .or_else(|| a.strip_prefix("--output-document="))
        {
            if !v.is_empty() {
                out.push(v.to_string());
            }
        }
        i += 1;
    }
    out
}

// ===== [S7 审查返工增补] sed/tar/unzip/cpio 判定辅助（AST 与词法镜像共用谓词） =====

/// sed 原地修改判定（quote 剥除后）：裸 -i（BSD `sed -i ''` 同形）、-i 组合后缀
///（GNU `sed -i.bak`，len>2 且非 -- 开头）、--in-place 及 --in-place=值。
fn sed_in_place_hit(arg: &str) -> bool {
    let a = arg.trim_matches('"').trim_matches('\'');
    a == "-i"
        || (a.starts_with("-i") && !a.starts_with("--") && a.len() > 2)
        || a == "--in-place"
        || a.starts_with("--in-place=")
}

/// tar 操作归类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TarOp {
    /// 解包（-x/--extract/--get）：归档成员写入不可静态判定的落点
    Extract,
    /// 创建归档（-c/--create）
    Create,
    /// 追加/更新归档（-r/-u/-A 与 --append/--catenate/--concatenate/--update）
    Modify,
    /// 删除归档成员（-d/--delete）
    Delete,
    /// 只读列举（-t/--list）
    List,
}

/// 单个 tar 参数的操作归类（quote 剥除后）：短簇逐字符看（`-czf` 的 c 是操作符、
/// f 只表示下一参数为归档名，不在此归类），长 flag 精确比对；无操作语义返回 None。
fn tar_arg_op(arg: &str) -> Option<TarOp> {
    let a = arg.trim_matches('"').trim_matches('\'');
    if let Some(long) = a.strip_prefix("--") {
        return match long {
            "extract" | "get" => Some(TarOp::Extract),
            "create" => Some(TarOp::Create),
            "append" | "catenate" | "concatenate" | "update" => Some(TarOp::Modify),
            "delete" => Some(TarOp::Delete),
            "list" => Some(TarOp::List),
            _ => None,
        };
    }
    if a.len() > 1 && a.starts_with('-') {
        let cs: Vec<char> = a[1..].chars().collect();
        if cs.contains(&'x') {
            return Some(TarOp::Extract);
        }
        if cs.contains(&'c') {
            return Some(TarOp::Create);
        }
        if cs.iter().any(|&ch| matches!(ch, 'r' | 'u' | 'A')) {
            return Some(TarOp::Modify);
        }
        if cs.contains(&'d') {
            return Some(TarOp::Delete);
        }
        if cs.contains(&'t') {
            return Some(TarOp::List);
        }
    }
    None
}

/// -f/--file 归档名取值：短簇含 f（`-cf x.tar`/`-f x.tar`）时下一参数为值，
/// `--file=值` 内联取值；`--file 值` 长形态与前者同理一并收录（契约遗漏的显然
/// 补全，宁可过拦）。无值返回 None（create/modify/delete 无归档名时落回既有链，
/// stdout 重定向由 L2 兜底）。
fn tar_file_value(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].trim_matches('"').trim_matches('\'');
        if a == "-f" || a == "--file" {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = a.strip_prefix("--file=") {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        if a.len() > 1 && a.starts_with('-') && !a.starts_with("--") && a.contains('f') {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

/// unzip 只读形态：-l/-t/-v/-Z/--list/--test/--zipinfo，命中即不升级。
fn unzip_readonly_form(arg: &str) -> bool {
    matches!(
        arg.trim_matches('"').trim_matches('\''),
        "-l" | "-t" | "-v" | "-Z" | "--list" | "--test" | "--zipinfo"
    )
}

/// cpio copy-in（解包）判定：-i 及 -i 组合（`-id`，len>2）、--extract。
fn cpio_extract_hit(arg: &str) -> bool {
    let a = arg.trim_matches('"').trim_matches('\'');
    a == "-i" || (a.starts_with("-i") && a.len() > 2) || a == "--extract"
}

/// 透传剥离后的真实命令判定入口（AST 层调用）。
fn eval_command(name: &str, args: &[String], ctx: &mut FenceCtx) {
    eval_command_with(name, args, None, None, ctx);
}

/// 名字级判定总表：L1 删除黑名单、按名的灾难/高危类、L2 写目标命令
/// （WRITE_ALL/WRITE_LAST/WRITE_FIRST）与下载/解释器关联置位。
fn eval_command_with(
    name: &str,
    args: &[String],
    _node: Option<tree_sitter::Node>,
    _name_node: Option<tree_sitter::Node>,
    ctx: &mut FenceCtx,
) {
    // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：按名的下载/解释器关联置位（见 check_command_inner 尾部）
    if DOWNLOAD_CMDS.contains(&name) {
        ctx.saw_download = true;
    }
    if INTERP_CMDS.contains(&name) {
        ctx.saw_interp = true;
    }
    // L1：删除黑名单（无论出现在哪都拦）
    if DELETE_CMDS.contains(&name) {
        ctx.escalate(Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: format!("shell 删除命令 `{name}` 被拦截，请改用 delete 工具"),
        });
        return;
    }
    // [POSIX 命令全集加固批次] 隐式写命令（gzip/gunzip/xz/zstd 等压缩/解压工具）：默认
    // 替换/删除原文件，绕过重定向与写目标扫描（参数是「源」不是「目标」）。无
    // stdout/测试模式豁免 flag 时按高危确认（审批关闭退化为 Block）；有豁免则不升级，
    // 落回既有链（重定向写目标由 walk 的 file_redirect 扫描兑底）。词法镜像见 fallback_scan。
    if IMPLICIT_WRITE_CMDS.contains(&name) && !implicit_write_exempt(args) {
        ctx.escalate(high_risk_verdict(
            ctx.policy,
            "该命令默认会替换/删除原文件（压缩/解压隐式写），请确认或改用 -c/-t 等 stdout 只读模式",
        ));
        return;
    }
    // [S7 审查返工增补] sed 原地修改：-i/--in-place 会原地修改每个输入文件本身
    //（隐式写，参数是「源」不是「目标」，绕过 L2 写目标扫描）→ 高危确认。sed 本体
    // 在 plan 档白名单，L0 放行后由本判定与词法镜像（high_risk_match）拦截，
    // 两条路径文案一致；AST 为双保险。
    if name == "sed" && args.iter().any(|a| sed_in_place_hit(a)) {
        ctx.escalate(high_risk_verdict(
            ctx.policy,
            "sed -i 会原地修改每个输入文件本身（隐式写），需确认或改用无 -i 形态配合重定向",
        ));
        return;
    }
    // [S7 审查返工增补] tar 子命令感知：解包（-x/--extract/--get 或短簇含 x）把归档
    // 成员写入当前目录等不可静态判定的落点（隐式写）→ 高危确认；创建/追加/更新/
    // 删除成员（-c/-r/-u/-A/-d 及长形态）取 -f/--file 归档名走写目标判定（区外已存在
    // Block / 区外新建按策略 Confirm / 区内 Allow），不 return——继续走完既有链；
    // -t/--list 只读列举不升级。词法镜像见 high_risk_match 尾部（f 值判定不做词法
    // 镜像：fallback 本无写目标扫描，create 落点属已知残留）。
    if name == "tar" {
        if args.iter().any(|a| tar_arg_op(a) == Some(TarOp::Extract)) {
            ctx.escalate(high_risk_verdict(
                ctx.policy,
                "tar 解包会把归档成员写入当前目录等不可静态判定的落点（隐式写），请确认或改用 -t 只读列举",
            ));
            return;
        }
        let op = args.iter().find_map(|a| tar_arg_op(a));
        if matches!(op, Some(TarOp::Create | TarOp::Modify | TarOp::Delete)) {
            if let Some(f) = tar_file_value(args) {
                let v = check_write_target(&f, &ctx.cwd, ctx.roots, ctx.policy);
                ctx.escalate(v);
            }
        }
    }
    // [S7 审查返工增补] unzip：解包形态把成员文件写入不可静态判定的落点（隐式写）
    // → 高危确认；-l/-t/-v/-Z/--list/--test/--zipinfo 只读形态不升级。词法镜像见
    // high_risk_match 尾部。
    if name == "unzip" && !args.iter().any(|a| unzip_readonly_form(a)) {
        ctx.escalate(high_risk_verdict(
            ctx.policy,
            "unzip 解包会把成员文件写入不可静态判定的落点（隐式写），请确认或改用 -l 只读列举",
        ));
        return;
    }
    // [S7 审查返工增补] cpio copy-in（-i 及组合/--extract）：解包把成员写入不可静态
    // 判定的落点（隐式写）→ 高危确认。cpio 不入 plan 档白名单（L0 拦，属预期）。
    // 词法镜像见 high_risk_match 尾部。
    if name == "cpio" && args.iter().any(|a| cpio_extract_hit(a)) {
        ctx.escalate(high_risk_verdict(
            ctx.policy,
            "cpio copy-in 解包会把成员写入不可静态判定的落点（隐式写），请确认",
        ));
        return;
    }
    // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：按名的灾难/高危类——disaster_match/high_risk_match 的镜像，
    // 以反混淆后的 AST 名字为键，因为掩码 L3 文本扫描看不到带引号的
    // 命令名（`"shutdown" -h now`、`"mkfs.ext4" /dev/sda1`）。
    // shell 可切换（cmd/fish/WSL）加固：cmd 破坏性系统命令与 fish 持久化/执行入口，
    // 按反混淆后的 AST 名字确认（审批关闭退化为 Block）。del/erase/rd/rmdir 已由
    // DELETE_CMDS 硬拦；shutdown 在下方电源操作表；删除类优先级更高不受影响。
    if CMD_DANGER_CMDS.contains(&name) {
        ctx.escalate(high_risk_verdict(
            ctx.policy,
            "cmd 破坏性系统命令需用户确认；删除类（del/rd/erase）已由删除黑名单硬拦",
        ));
        return;
    }
    if FISH_DANGER_CMDS.contains(&name) {
        ctx.escalate(high_risk_verdict(
            ctx.policy,
            "fish 持久化/执行入口（source/funcsave/funced/abbr）需用户确认",
        ));
        return;
    }
    // [POSIX 命令全集加固批次] L3 高危扩充（HIGH_RISK_CMDS）：进程控制/持久化/任意代码
    // 执行/远程宿主/macOS 配置写，按名确认；文案按组区分理由。词法镜像见 high_risk_match。
    if let Some(why) = high_risk_cmd_reason(name) {
        ctx.escalate(high_risk_verdict(ctx.policy, why));
        return;
    }
    if matches!(name, "shutdown" | "reboot" | "halt" | "poweroff") {
        ctx.escalate(disaster_verdict(ctx.policy, "系统电源操作"));
        return;
    }
    if name == "init" && args.iter().any(|a| a == "0" || a == "6") {
        ctx.escalate(disaster_verdict(ctx.policy, "系统电源操作"));
        return;
    }
    if name == "mkfs" || name.starts_with("mkfs.") {
        ctx.escalate(disaster_verdict(ctx.policy, "mkfs 文件系统操作"));
        return;
    }
    // [POSIX 命令全集加固批次] 灾难扩充：分区表/磁盘标签编辑器（fdisk/disklabel/
    // parted/gparted）对磁盘做不可逆写，灾难级；词法镜像见 disaster_match。
    if matches!(name, "fdisk" | "disklabel" | "parted" | "gparted") {
        ctx.escalate(disaster_verdict(ctx.policy, "分区表/磁盘编辑操作（不可逆）"));
        return;
    }
    // [POSIX 命令全集加固批次] diskutil 子命令感知：擦盘（eraseDisk/eraseVolume…）
    // 与 APFS 删除（deleteVolume/deleteContainer）是灾难级；其余（list/info 等）按高危确认。
    // 词法镜像：disaster_match 的 `diskutil erase` 短语与 high_risk_match 的裸 diskutil。
    if name == "diskutil" {
        let erase = args
            .first()
            .map(|a| a.to_ascii_lowercase().starts_with("erase"))
            .unwrap_or(false);
        let apfs_delete = args.iter().position(|a| a.eq_ignore_ascii_case("apfs"))
            .is_some_and(|p| args[p + 1..].iter().any(|a| a.to_ascii_lowercase().starts_with("delete")));
        if erase || apfs_delete {
            ctx.escalate(disaster_verdict(
                ctx.policy,
                "diskutil 擦除/删除卷操作（不可逆）",
            ));
        } else {
            ctx.escalate(high_risk_verdict(ctx.policy, "diskutil 磁盘操作需确认"));
        }
        return;
    }
    if name == "dd" && args.iter().any(|a| a.starts_with("of=/dev/")) {
        ctx.escalate(disaster_verdict(ctx.policy, "dd 写入设备"));
        return;
    }
    if name == "chmod"
        && args.iter().any(|a| {
            a.contains("777")
                || a.contains("a+rwx")
                || a.contains("o+w")
                || a.contains("+s")
                || a.contains("4755")
        })
    {
        ctx.escalate(high_risk_verdict(ctx.policy, "chmod 777"));
        return;
    }
    if name == "git"
        && args.first().map(|s| s.as_str()) == Some("push")
        && args.iter().any(|a| a == "--force" || a == "-f" || a.starts_with("--force="))
    {
        ctx.escalate(high_risk_verdict(ctx.policy, "git push --force"));
        return;
    }
    // git reset：任何形态必确认（可能丢弃未提交工作 / 移动分支；审批关闭时退化为 Block）。
    // 词法镜像见 high_risk_match 的同款匹配。
    if name == "git" && args.first().map(|s| s.as_str()) == Some("reset") {
        ctx.escalate(high_risk_verdict(
            ctx.policy,
            "git reset（可能丢弃工作区更改或移动分支）",
        ));
        return;
    }
    // L1：find 的删除语义
    if name == "find" {
        for (i, a) in args.iter().enumerate() {
            if a == "-delete" || (a == "-exec" && args.get(i + 1).map(|s| s.as_str()) == Some("rm"))
            {
                ctx.escalate(Verdict::Block {
                    code: "E_COMMAND_BLOCKED".into(),
                    message: "find 的删除语义被拦截，请改用 delete 工具".into(),
                });
                return;
            }
        }
    }
    // L2：写目标命令
    if WRITE_ALL.contains(&name) {
        for a in args.iter() {
            if a.starts_with('-') || a == "-" {
                continue;
            }
            if name == "dd" {
                if let Some(val) = a.strip_prefix("of=") {
                    let v = check_write_target(val, &ctx.cwd, ctx.roots, ctx.policy);
                    ctx.escalate(v);
                }
                continue;
            }
            let v = check_write_target(a, &ctx.cwd, ctx.roots, ctx.policy);
            ctx.escalate(v);
        }
    } else if WRITE_LAST.contains(&name) {
        if let Some(last) = args.iter().rfind(|a| !a.starts_with('-')) {
            let v = check_write_target(last, &ctx.cwd, ctx.roots, ctx.policy);
            ctx.escalate(v);
        }
    } else if WRITE_FIRST.contains(&name) {
        // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)（评审）：逐个非 flag 参数判定——带值的命名 flag（`-ItemType
        // file`）会贡献一个无害的根内候选，但真正的 `-Path`/位置参数目标
        // 绝不会漏判。宁可过度拦截。
        for a in args.iter() {
            if a.starts_with('-') || a == "-" {
                continue;
            }
            let v = check_write_target(a, &ctx.cwd, ctx.roots, ctx.policy);
            ctx.escalate(v);
        }
    }
    // [POSIX 命令全集加固批次] 下载工具的带值写 flag（-o/--output/--output-document/-outfile）：
    // 写目标藏在 flag 值里而非位置参数，L2 位置参数扫描不可见——提取后走 check_write_target。
    // 裸 -O 不查（落点恒为 cwd）；`-sSo x` 组合短 flag 混排形态是已知残留（不过度工程）。
    if DOWNLOAD_CMDS.contains(&name) {
        for t in download_write_targets(args) {
            let v = check_write_target(&t, &ctx.cwd, ctx.roots, ctx.policy);
            ctx.escalate(v);
        }
    }
}

/// [POSIX 命令全集加固批次] HIGH_RISK_CMDS 的分组理由文案（AST 与词法路径共用）：
/// 进程控制 / 持久化与计划任务 / 任意代码执行 / 远程命令执行 / 持久化配置写。
fn high_risk_cmd_reason(name: &str) -> Option<&'static str> {
    if !HIGH_RISK_CMDS.contains(&name) {
        return None;
    }
    if matches!(name, "kill" | "killall" | "pkill") {
        Some("进程控制（kill/killall/pkill）可终止任意进程，需用户确认（pgrep 只查询不在此列）")
    } else if matches!(
        name,
        "crontab" | "at" | "systemctl" | "service" | "launchctl"
    ) {
        Some("持久化与计划任务（crontab/at/systemctl/service/launchctl）可在重启后驻留执行，需用户确认")
    } else if name == "osascript" {
        Some("osascript 可执行任意 AppleScript/JXA 代码（任意代码执行），需用户确认")
    } else if matches!(name, "ssh" | "scp" | "sftp") {
        Some("远程宿主操作（ssh/scp/sftp）的远端命令语义无法本地静态判定，需用户确认")
    } else if matches!(name, "defaults" | "plutil") {
        Some("macOS 配置写（defaults/plutil）可篡改应用/系统持久化配置，需用户确认")
    } else {
        None
    }
}

/// 单个写目标判定：规范化（含符号链接解析）后必须落在 roots 之内。
pub fn check_write_target(
    raw: &str,
    cwd: &Path,
    roots: &WriteRoots,
    policy: FencePolicy,
) -> Verdict {
    let t = raw.trim().trim_matches('"').trim_matches('\'');
    if t.is_empty()
        || t.starts_with('-')
        || t == "-"
        || t == "/dev/null"
        || t == "$null"
        || t.starts_with("$(")
        || t.starts_with("`")
    {
        return Verdict::Allow;
    }
    // H6 修复：含变量展开 / ~user 的目标无法静态定位
    if t.contains('$') || (t.starts_with('~') && !t.starts_with("~/")) {
        if policy.approval_enabled {
            return Verdict::Confirm(ConfirmReason::HighRisk(
                "写目标含变量展开，无法静态判定落点",
            ));
        }
        return Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: "含变量展开的写目标已拦截（审批开关关闭）".into(),
        };
    }
    let expanded: PathBuf = if let Some(rest) = t.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(h) => h.join(rest),
            None => return Verdict::Allow,
        }
    } else {
        PathBuf::from(t)
    };
    let path = if expanded.is_absolute() {
        expanded
    } else {
        // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)（评审）：在规范化的 cwd 上 join，让词法包含性检查与规范化 roots
        // 处于同一命名空间（Windows 的 `\\?\` 原样、macOS 的 /var→/private/var 符号链接
        // 已解析）——否则 `starts_with` 永不匹配，下方整套逃逸网沦为死代码。
        pathutil::canonical_best_effort(cwd).join(expanded)
    };
    let roots_canon = roots.canonical_roots();
    let exists = path.symlink_metadata().is_ok();
    // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：先按词法判定越界。显式 `..` 穿越离开 roots 是普通路径语义——
    // 旧的祖先逐级规范化循环对一切 `../` 相对目标都会误报，
    // 因为祖先遍历必然途经根本身。
    let lex = lexical_normalize(&path);
    let lex_inside = pathutil::inside_roots(&lex, &roots_canon);
    let canonical = pathutil::canonical_best_effort(&path);
    let canon_inside = pathutil::inside_roots(&canonical, &roots_canon);

    if lex_inside {
        if !canon_inside {
            // 词法在内、解析在外 → 符号链接逃逸，一律 Block
            return Verdict::Block {
                code: "E_PATH_OUTSIDE".into(),
                message: format!("写目标经符号链接逃逸出工作区：{t}"),
            };
        }
        // 悬空链接：canonicalize 会跳过、best-effort 锚在根内，因此逐组件扫描
        // 根下路径——任何无法证明解析在 roots 内的链接都是逃逸
        //（比旧循环严格收紧，旧循环会放过悬空链接）。
        if let Some(link) = intermediate_symlink_escape(&lex, &roots_canon) {
            return Verdict::Block {
                code: "E_PATH_OUTSIDE".into(),
                message: format!(
                    "写目标经符号链接逃逸出工作区：{}",
                    link.display()
                ),
            };
        }
        // ConfirmEach/Plan：根内写同样需要确认（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md) 语义表）
        if policy.confirm_inside_writes {
            return Verdict::Confirm(ConfirmReason::InsideWrite(t.to_string()));
        }
        return Verdict::Allow;
    }
    if canon_inside {
        // 词法在 roots 外、经链接解析回到根内：真实位置才是权威边界 → 走根内流程。
        if policy.confirm_inside_writes {
            return Verdict::Confirm(ConfirmReason::InsideWrite(t.to_string()));
        }
        return Verdict::Allow;
    }
    if exists {
        return Verdict::Block {
            code: "E_PATH_OUTSIDE".into(),
            message: format!("写目标在工作区外且已存在：{t}"),
        };
    }
    if policy.confirm_outside_create {
        Verdict::Confirm(ConfirmReason::OutsideCreate(t.to_string()))
    } else {
        Verdict::Allow
    }
}

/// 纯文本解析 `..`/`.`，不触碰文件系统。
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 扫描匹配根之下的各组件：符号链接目标离开 roots 或无法解析（悬空）即命中。
/// 返回肇事链接路径。
fn intermediate_symlink_escape(lex: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let root = roots
        .iter()
        .filter(|r| lex.starts_with(*r))
        .max_by_key(|r| r.components().count())?;
    let mut cur = root.clone();
    for comp in lex.components().skip(root.components().count()) {
        cur.push(comp.as_os_str());
        let Ok(meta) = std::fs::symlink_metadata(&cur) else {
            return None; // 检查途中消失：交给根内流程决定
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        match std::fs::canonicalize(&cur) {
            Ok(target) if pathutil::inside_roots(&target, roots) => continue,
            _ => return Some(cur),
        }
    }
    None
}

/// 解析失败时的词法回退：只做 L1/L3 词扫描。
/// cmd/fish 词法加固（shell 可切换为 cmd/fish/WSL 后）：L1 删除黑名单 →
/// cmd 清空写短语（type nul > x / more +0 > x 的截断写语义）→ cmd 危险命令词表
/// → fish 持久化/执行入口 → 既有灾难/高危模式。只增不删。
fn fallback_scan(cmd: &str) -> Verdict {
    // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：与 AST 路径同一套反混淆（共享 l1_token_scan）。
    if let Some(tok) = l1_token_scan(cmd) {
        return Verdict::Block {
            code: "E_COMMAND_BLOCKED".into(),
            message: format!("shell 删除命令 `{tok}` 被拦截，请改用 delete 工具"),
        };
    }
    // [POSIX 命令全集加固批次] 隐式写命令词法镜像（eval_command_with 同款判定）：
    // 首命令命中 IMPLICIT_WRITE_CMDS 且无 stdout/测试模式豁免 flag → Confirm。
    // 解析失败意味着重定向扫描不可用，这里的升级不可省。
    {
        let first = deobfuscate_name(cmd.split_whitespace().next().unwrap_or(""));
        if IMPLICIT_WRITE_CMDS.contains(&first.as_str()) {
            let args: Vec<String> = cmd
                .split_whitespace()
                .skip(1)
                .map(|s| s.to_string())
                .collect();
            if !implicit_write_exempt(&args) {
                return Verdict::Confirm(ConfirmReason::HighRisk(
                    "该命令默认会替换/删除原文件（压缩/解压隐式写），请确认或改用 -c/-t 等 stdout 只读模式",
                ));
            }
        }
    }
    if CMD_DANGER_KEYWORDS.iter().any(|k| lower_all(cmd).contains(k)) {
        return Verdict::Confirm(ConfirmReason::HighRisk(
            "cmd 清空写短语（type nul / more +0 重定向）按高危确认",
        ));
    }
    if cmd_danger_match(cmd).is_some() {
        return Verdict::Confirm(ConfirmReason::HighRisk(
            "cmd 破坏性系统命令需用户确认；删除类（del/rd/erase）已由删除黑名单硬拦",
        ));
    }
    if FISH_DANGER_CMDS
        .iter()
        .any(|f| lexical_tokens(cmd).iter().any(|t| t == f))
    {
        return Verdict::Confirm(ConfirmReason::HighRisk(
            "fish 持久化/执行入口（source/funcsave/funced/abbr）需用户确认",
        ));
    }
    if let Some(why) = disaster_match(cmd) {
        return Verdict::Confirm(ConfirmReason::Disaster(why));
    }
    if let Some(why) = high_risk_match(cmd) {
        return Verdict::Confirm(ConfirmReason::HighRisk(why));
    }
    Verdict::Allow
}

/// 词法回扫专用 token 提取：对整条命令先按分隔符切分再逐词扫描，命令名
/// 反混淆（IFS 展开、去引号与转义）。cmd 的 `^` 是转义符（`^&` = 字面
/// `&`），对判定而言任何 `^` 都当作转义字符剥掉——比保守处理更严（宁可
/// 过拦不放过）。
fn lexical_tokens(cmd: &str) -> Vec<String> {
    let lowered = expand_ifs(&cmd.to_ascii_lowercase());
    lowered
        .split(|c: char| {
            c.is_whitespace() || c == ';' || c == '|' || c == '&' || c == '(' || c == ')'
        })
        .map(|seg| {
            seg.chars()
                .filter(|c| !matches!(c, '"' | '\'' | '`' | '^'))
                .collect::<String>()
        })
        .collect()
}

/// cmd 危险命令词表命中：词形逐词比对（与 DELETE_CMDS 的 l1_token_scan 同一套
/// token 提取逻辑）。
fn cmd_danger_match(cmd: &str) -> Option<&'static str> {
    let toks = lexical_tokens(cmd);
    CMD_DANGER_CMDS
        .iter()
        .find(|c| toks.iter().any(|t| t == **c))
        .copied()
}

/// 小写化辅助（灾难/高危模式匹配与 cmd 短语扫描共用）。
fn lower_all(cmd: &str) -> String {
    cmd.to_ascii_lowercase()
}

/// cmd 宿主判定（bash 与 PowerShell AST 命令处理共用）：`cmd /c <内层>`/
/// `cmd /k <内层>` 的内层命令对名字级判定表不可见——提取内层串后走完整重判
/// （check_command_depth，深度上限 2；内层视为审批开启）。/c 与 /k 语义等价
/// 处理；未识别形态（如 `cmd /ver`、裸 `cmd`）不升级，由回扫与既有安全网兜底。
/// 返回 true = 已判定（调用方短路）。
fn judge_cmd_host(name: &str, args: &[String], ctx: &mut FenceCtx) -> bool {
    if !CMD_HOSTS.contains(&name) || ctx.depth >= 2 {
        return false;
    }
    // 位置参数形态：`cmd /c taskkill /f /im x`（switch 后全部为内层）；
    // 合并形态：`cmd /cdir`（switch 与首词连写，其余参数照拼）。
    let mut inner: Option<String> = None;
    for (i, a) in args.iter().enumerate() {
        let lower = a.to_ascii_lowercase();
        if lower == "/c" || lower == "/k" {
            inner = Some(args[i + 1..].join(" "));
            break;
        }
        if lower.len() > 2 && (lower.starts_with("/c") || lower.starts_with("/k")) {
            let mut parts = vec![a[2..].to_string()];
            parts.extend(args[i + 1..].iter().cloned());
            inner = Some(parts.join(" "));
            break;
        }
    }
    let Some(inner) = inner else {
        return false;
    };
    let sub = check_command_depth(
        &inner,
        &ctx.cwd,
        ctx.roots,
        FencePolicy {
            approval_enabled: true,
            ..ctx.policy
        },
        ctx.depth + 1,
    );
    ctx.escalate(sub);
    true
}

/// WSL 宿主判定（bash 与 PowerShell AST 命令处理共用）：wsl/wsl.exe 本身无文件写
/// 语义，但会代理执行内层命令——提取内层串后重判（比照 powershell -Command 递归，
/// 深度上限 2；内层视为审批开启，确保最严格判定）。未识别形态（如默认发行版直跑
/// ls）保守升级为 Confirm，绝不 Allow。返回 true = 已判定（调用方短路）。
fn judge_wsl(name: &str, args: &[String], ctx: &mut FenceCtx) -> bool {
    if !WSL_HOSTS.contains(&name) || ctx.depth >= 2 {
        return false;
    }
    match extract_wsl_inner(args) {
        Some((inner, true)) => {
            let sub = check_command_depth(
                &inner,
                &ctx.cwd,
                ctx.roots,
                FencePolicy {
                    approval_enabled: true,
                    ..ctx.policy
                },
                ctx.depth + 1,
            );
            ctx.escalate(sub);
            true
        }
        Some((inner, false)) => {
            // -e/--exec 直接执行形态：无 shell 层可递归，按 L1 + 灾难/高危词法扫描兜底
            if let Some(tok) = l1_token_scan(&inner) {
                ctx.escalate(Verdict::Block {
                    code: "E_COMMAND_BLOCKED".into(),
                    message: format!("shell 删除命令 `{tok}` 被拦截，请改用 delete 工具"),
                });
            }
            if let Some(why) = disaster_match(&inner) {
                ctx.escalate(disaster_verdict(ctx.policy, why));
            }
            if let Some(why) = high_risk_match(&inner) {
                ctx.escalate(high_risk_verdict(ctx.policy, why));
            }
            true
        }
        // 保守：wsl 裸跑或未识别形态无法证明内层安全 → Confirm（审批关闭退化为 Block）
        None => {
            ctx.escalate(high_risk_verdict(
                ctx.policy,
                "WSL 调用未识别为可静态判定的内层命令形态",
            ));
            true
        }
    }
}

/// WSL 宿主调用解析（参照 powershell -Command 递归特判的提取模式）：
/// 从 wsl 命令行参数中提取「内层命令串」供递归判定。识别三种形态：
/// ① `wsl [opts] bash -c "<cmd>"` / `wsl [opts] sh -c "<cmd>"`（引号/裸串均可）；
/// ② `wsl [opts] -e <prog> [args…]`（`-e` 后全部为内层命令）；
/// ③ `wsl [opts] --exec <prog> [args…]`（同 -e）。返回 Option<(String, bool)>：
/// 内层串 + 是否 shell 形态（shell 形态走 check_command_depth 深度递归；
/// -e 形态只做 L1 + L3 扫描）。wsl 管理类子命令（--list/--status/--set-* 等，
/// 见 is_wsl_management_arg）返回 None → 外层保守 Confirm。
fn extract_wsl_inner(args: &[String]) -> Option<(String, bool)> {
    let mut i = 0;
    // 前置 option：--cd/--user 的值与 = 合并值；-- 起隔后进入内层
    let mut after_separator = false;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            after_separator = true;
            break;
        }
        if a.starts_with("--cd=") || a.starts_with("--user=") {
            i += 1;
            continue;
        }
        if a == "--cd" || a == "--user" {
            i += 2; // 带值 option：连值跳过
            continue;
        }
        break;
    }
    if i >= args.len() {
        return None;
    }
    // wsl 管理类子命令（--list/--status/--set-* 等）：不是「内层程序名」，
    // 按 prog=list 走直接执行形态会误放行 → 返回 None 触发外层保守 Confirm
    //（审批关闭退化为 Block）。`--` 起隔后的同名词形属于内层文本，不在此列。
    if !after_separator && is_wsl_management_arg(&args[i]) {
        return None;
    }
    // 形态②③：-e/--exec 后全部为内层（prog + args 拼回一条命令串）
    if args[i] == "-e" || args[i] == "--exec" {
        let inner = args[i + 1..].join(" ");
        return (!inner.is_empty()).then_some((inner, false));
    }
    // 形态①：首个非 option 参数是 shell 家族且带 -c → 深度递归
    let first = deobfuscate_name(&args[i]);
    if matches!(first.as_str(), "sh" | "bash" | "dash" | "zsh") {
        let cpos = args[i + 1..].iter().position(|a| a == "-c")? + i + 1;
        let inner = args.get(cpos + 1)?.clone();
        return Some((inner, true));
    }
    // 形态④：裸 `wsl <prog> [args…]` 直接执行（真实 wsl 语义等价 -e）——
    // 拼串后按 L1 + 灾难/高危词扫描兑底。
    Some((args[i..].join(" "), false))
}

/// wsl 管理类子命令识别（清单只增不删；命中即返回 None → 外层保守 Confirm）：
/// 枚举/状态/配置（--list/--status/--set-*）与发行版生命周期管理
///（--import/--export/--unregister/--update/--install/--shutdown/--terminate/
/// --manage/--uninstall）都不携带「待执行的内层命令」，无法静态证明安全，
/// 绝不能当作内层 prog 走直接执行形态放行。
fn is_wsl_management_arg(a: &str) -> bool {
    let lower = a.to_ascii_lowercase();
    lower.starts_with("--list")
        || lower.starts_with("--status")
        || lower.starts_with("--set-")
        || matches!(
            lower.as_str(),
            "--import"
                | "--export"
                | "--unregister"
                | "--update"
                | "--install"
                | "--shutdown"
                | "--terminate"
                | "--manage"
                | "--uninstall"
        )
}

/// 灾难级（不可逆/系统级）：全权限档也直接拦截。
fn disaster_match(cmd: &str) -> Option<&'static str> {
    let lower = lower_all(cmd);
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.iter().any(|t| t.starts_with("mkfs")) {
        return Some("mkfs 文件系统操作");
    }
    if tokens.contains(&"dd") && lower.contains("of=/dev/") {
        return Some("dd 写入设备");
    }
    if lower.contains("> /etc/") || lower.contains(">> /etc/") {
        return Some("写入 /etc");
    }
    if tokens.contains(&"shutdown") || tokens.contains(&"reboot") {
        return Some("系统电源操作");
    }
    // [POSIX 命令全集加固批次] 灾难扩充（AST 镜像见 eval_command_with）：分区表/磁盘
    // 标签编辑器按名灾难；diskutil erase* 子命令（eraseDisk/eraseVolume 等）以
    // 「diskutil erase」短语前缀命中。非 erase 的 diskutil 在 high_risk_match。
    if tokens
        .iter()
        .any(|t| matches!(*t, "fdisk" | "disklabel" | "parted" | "gparted"))
    {
        return Some("分区表/磁盘编辑操作（不可逆）");
    }
    if let Some(p) = tokens.iter().position(|t| *t == "diskutil") {
        if tokens
            .get(p + 1)
            .is_some_and(|t| t.starts_with("erase"))
        {
            return Some("diskutil 擦除/删除卷操作（不可逆）");
        }
        // [POSIX 命令全集加固批次] `diskutil apfs deleteVolume/deleteContainer`（词法短语）：
        // 与 AST 子命令感知同口径（apfs 后随 delete*）——没有这里的灾难短语，
        // 下溢高危表的裸 diskutil 会以低位阶抢先命中，灾难级不可达。
        if tokens.get(p + 1).map(|t| *t == "apfs").unwrap_or(false)
            && tokens
                .get(p + 2)
                .is_some_and(|t| t.starts_with("delete"))
        {
            return Some("diskutil 擦除/删除卷操作（不可逆）");
        }
    }
    None
}

/// 高危级：全权限档下不经确认直接执行。
fn high_risk_match(cmd: &str) -> Option<&'static str> {
    let lower = lower_all(cmd);
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.contains(&"chmod") && lower.contains("777") {
        return Some("chmod 777");
    }
    let pipes_to_shell = ["| sh", "|sh", "| bash", "|bash", "| zsh", "|zsh"]
        .iter()
        .any(|p| lower.contains(p));
    // PowerShell 下载-执行（`iwr … | iex`，[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)）：短别名按 token 比对，
    // 子串无法搭载在路径上；管道解释器一侧仍按子串比对。
    let pipes_to_iex = ["| iex", "|iex", "| invoke-expression", "|invoke-expression"]
        .iter()
        .any(|p| lower.contains(p));
    let ps_download = tokens.iter().any(|t| {
        matches!(
            *t,
            "iwr" | "irm" | "invoke-webrequest" | "invoke-restmethod" | "curl" | "wget"
        )
    });
    let has_download = lower.contains("curl")
        || lower.contains("wget")
        || lower.contains("invoke-webrequest")
        || lower.contains("invoke-restmethod")
        || ps_download;
    if has_download && (pipes_to_shell || pipes_to_iex) {
        return Some("下载脚本直接执行");
    }
    if lower.contains("git push") {
        // `--force-with-lease` 是安全的强制变体（远端有新提交时失败）——[docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：
        // 只有裸 `--force` / `-f` / `--force=…` 算强制推送。
        let forced = tokens
            .iter()
            .any(|t| *t == "--force" || *t == "-f" || t.starts_with("--force="));
        if forced {
            return Some("git push --force");
        }
    }
    // git reset：任何形态必确认（词法回退路径；AST 镜像见 eval_command_with）。
    if let Some(pos) = tokens.iter().position(|t| *t == "git") {
        if tokens.get(pos + 1).is_some_and(|t| *t == "reset") {
            return Some("git reset（可能丢弃工作区更改或移动分支）");
        }    }
    // [POSIX 命令全集加固批次] L3 高危扩充（HIGH_RISK_CMDS）词法镜像（AST 镜像见
    // eval_command_with / high_risk_cmd_reason）：按**首命令词**命中（与
    // CMD_DANGER_CMDS「按首命令词」纪律一致）——参数位置的普通词不得误拦：
    // `grep kill app.log`、`zgrep kill a.log.gz`（后者还是 plan 档白名单成员，
    // 逐词扫描会让白名单命令被自家 L3 废掉）。透传前缀（sudo kill …）由 AST
    // 路径的 strip_transparency 剥离后重判覆盖；词法回退按首词从严。
    // diskutil 非 erase 子命令（list/info 等）在此按高危确认（erase/apfs-delete
    // 形态已被 disaster_match 以更高位阶捕获）。
    if let Some(first) = tokens.first() {
        if let Some(why) = high_risk_cmd_reason(first) {
            return Some(why);
        }
        if *first == "diskutil" {
            let next = tokens.get(1).copied().unwrap_or("");
            if !next.starts_with("erase") {
                return Some("diskutil 磁盘操作需确认");
            }
        }
        // [S7 审查返工增补] sed/tar/unzip/cpio 首命令词镜像（AST 分支见
        // eval_command_with）：掩码 L3 文本扫描先于 AST 执行，plan 档 sed 白名单
        // 放行后仍在此命中 → Confirm → G5 转 E_PLAN_READONLY；AST 判定为双保险，
        // 两条路径文案一致。
        if *first == "sed" && tokens[1..].iter().any(|t| sed_in_place_hit(t)) {
            return Some(
                "sed -i 会原地修改每个输入文件本身（隐式写），需确认或改用无 -i 形态配合重定向",
            );
        }
        // tar 词法镜像只认解包形态（-x/--extract/--get 或短簇含 x）；-f 归档名判定
        // 不做词法镜像——fallback 本无写目标扫描，create 落点属已知残留。
        if *first == "tar"
            && tokens[1..]
                .iter()
                .any(|t| tar_arg_op(t) == Some(TarOp::Extract))
        {
            return Some(
                "tar 解包会把归档成员写入当前目录等不可静态判定的落点（隐式写），请确认或改用 -t 只读列举",
            );
        }
        if *first == "unzip" && !tokens[1..].iter().any(|t| unzip_readonly_form(t)) {
            return Some(
                "unzip 解包会把成员文件写入不可静态判定的落点（隐式写），请确认或改用 -l 只读列举",
            );
        }
        if *first == "cpio" && tokens[1..].iter().any(|t| cpio_extract_hit(t)) {
            return Some("cpio copy-in 解包会把成员写入不可静态判定的落点（隐式写），请确认");
        }
    }
    None
}


