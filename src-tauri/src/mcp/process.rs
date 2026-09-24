//! MCP stdio 子进程：统一 spawn 参数 + **进程树回收**。
//!
//! 旧实现 `TokioChildProcess::new(tokio::process::Command)` 走的是
//! `CommandWrap::from(Command)`，而那条路径构造的 wrappers 表是**空的**
//! （`process-wrap-10.0.0/src/generic_wrap.rs`），所以**没有 Job Object**：
//! 主进程退出时只会 kill 直接子进程，server 自己 spawn 的孙进程会变成孤儿。
//!
//! 这里显式包裹：
//! - Windows：`JobObject`（整棵树挂在同一个 job 上）+ `KillOnDrop`（job 句柄关闭时
//!   连带 kill）+ [`NoWindow`]（补回被 Job Object 覆盖掉的 `CREATE_NO_WINDOW`）。
//! - unix：`ProcessGroup::leader()`（子进程自成进程组，杀组即连带整棵树）。
//!
//! **注册顺序有语义**：wrappers 是 `IndexMap`，`pre_spawn` 按注册顺序执行。
//! `JobObject::pre_spawn` 是**覆盖式**写 `creation_flags`，因此 [`NoWindow`] 必须最后注册。

use std::process::Stdio;

use process_wrap::tokio::{CommandWrap, CommandWrapper, JobObject, KillOnDrop};
use tokio::process::Command;

use super::config::{McpServerConfig, McpTransport};
use super::error::McpError;

#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;

/// Windows `CREATE_NO_WINDOW`。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Windows `CREATE_SUSPENDED`。
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// 补回 `CREATE_NO_WINDOW`。
///
/// `JobObject::pre_spawn` 会用 `job_creation_flags(user_flags)` 覆盖 `creation_flags`，
/// 而 `user_flags` 只认 process-wrap 自己的 `CreationFlags` wrapper —— 裸
/// `Command::creation_flags()` 设的位会被冲掉（表现为 Windows 上每次连接 MCP
/// 都闪一个控制台窗口）。故这里在 `JobObject` **之后**注册，把位补回去。
///
/// 同时保留 `CREATE_SUSPENDED`：Job Object 需要进程在被挂进 job 之前保持挂起
/// （它的 `resume_after_assignment` 会在挂好之后恢复主线程），若这里把该位抹掉，
/// 子进程会在入 job 之前就跑起来。
#[cfg(windows)]
#[derive(Debug)]
struct NoWindow;

#[cfg(windows)]
impl CommandWrapper for NoWindow {
    fn pre_spawn(&mut self, command: &mut Command, core: &CommandWrap) -> std::io::Result<()> {
        let mut flags = CREATE_NO_WINDOW;
        if core.has_wrap::<JobObject>() {
            flags |= CREATE_SUSPENDED;
        }
        command.creation_flags(flags);
        Ok(())
    }
}

/// 为 stdio 传输构造带进程树回收保证的 `CommandWrap`。
///
/// 参数全部来自配置：`command` / `args` / `env` / `cwd`（旧实现没有 `cwd`，
/// 无法把 server 的工作目录指到项目）。
pub fn build_stdio_wrap(cfg: &McpServerConfig) -> Result<CommandWrap, McpError> {
    if cfg.resolve_transport()? != McpTransport::Stdio {
        return Err(McpError::config("build_stdio_wrap 只接受 stdio 配置"));
    }
    let command = cfg
        .command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| McpError::config("stdio transport 需要 command"))?;

    let mut cmd = Command::new(command);
    cmd.args(&cfg.args);
    cmd.envs(&cfg.env);
    if let Some(cwd) = cfg.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        let dir = std::path::Path::new(cwd);
        if !dir.is_dir() {
            return Err(McpError::spawn(format!("工作目录不存在：{cwd}"))
                .with_hint("检查该 server 的 cwd 配置，或清空它（默认继承应用工作目录）。"));
        }
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut wrap = CommandWrap::from(cmd);
    #[cfg(windows)]
    {
        wrap.wrap(JobObject).wrap(KillOnDrop).wrap(NoWindow);
    }
    #[cfg(unix)]
    {
        wrap.wrap(ProcessGroup::leader());
    }
    Ok(wrap)
}

/// 兜底杀进程树（当 Job Object / 进程组都不可用，或需要强制收尾时用）。
///
/// 幂等、绝不 panic：失败只 `warn`。不按 PID 杀的场景优先靠 job/进程组，
/// 因为 PID 可能被复用（误杀他人进程）。
pub async fn kill_tree_by_pid(pid: u32) {
    #[cfg(windows)]
    {
        let out = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags_no_window()
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => tracing::warn!(
                "taskkill /T /F /PID {pid} 退出码非 0：{}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => tracing::warn!("taskkill /T /F /PID {pid} 执行失败：{e}"),
        }
    }
    #[cfg(unix)]
    {
        // 负 PID = 向整个进程组发信号
        let out = Command::new("kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .output()
            .await;
        if let Err(e) = out {
            tracing::warn!("kill 进程组 -{pid} 失败：{e}");
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
    }
}

#[cfg(windows)]
trait NoWindowExt {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl NoWindowExt for Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self.creation_flags(CREATE_NO_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg_of(v: serde_json::Value) -> McpServerConfig {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn stdio_wrap_requires_command() {
        let c = cfg_of(json!({ "transport": "stdio" }));
        let e = build_stdio_wrap(&c).unwrap_err();
        assert!(e.message.contains("command"));
    }

    #[test]
    fn stdio_wrap_rejects_http_config() {
        let c = cfg_of(json!({ "url": "https://h/mcp" }));
        let e = build_stdio_wrap(&c).unwrap_err();
        assert!(e.message.contains("只接受 stdio"));
    }

    #[test]
    fn stdio_wrap_rejects_missing_cwd_with_actionable_hint() {
        let c = cfg_of(json!({ "command": "node", "cwd": "Z:/definitely/not/here" }));
        let e = build_stdio_wrap(&c).unwrap_err();
        assert!(e.message.contains("工作目录不存在"));
        assert!(e.hint.is_some());
    }

    #[test]
    fn stdio_wrap_accepts_valid_config() {
        let c = cfg_of(json!({ "command": "node", "args": ["-v"], "env": {"K": "V"} }));
        assert!(build_stdio_wrap(&c).is_ok());
    }
}
