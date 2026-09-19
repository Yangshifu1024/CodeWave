//! LSP 语义校验命令（[docs/lsp-post-write-diagnostics](../../../docs/lsp-post-write-diagnostics.md)）：
//! 设置页徽标（`lsp_status` / `lsp_redetect` / `lsp_restart`）与引导卡动作（`lsp_enable` / `lsp_install`）。
//! 只做参数校验 + 转调 core/lsp，不放业务逻辑。
//!
//! `lsp_install` 的确认路径（见函数内注释）：审批门 `safety::approval::confirm` 强绑定
//! **运行中的会话 runtime**（`Arc<SessionRuntime>` + 取消 token + ask 应答路由），host 层
//! 无会话上下文、命令入参也只有 `language`（前端 `ipc.lspInstall` 契约已冻结，见
//! `ui/src/ipc/client.ts`），故不新造审批事件（事件面锁 29 键）——确认依据是引导卡上
//! **已完整展示的命令 + 用户显式点击**。命令本身仍过 fence（灾难级照样拦）。

use super::util::{err, Core};
use crate::core::config::ValidationSettings;
use crate::lsp::{install, server_spec, Lang, ServerStatus};
use std::time::Duration;

/// 一键安装的超时预算（npm / rustup / go install 都可能跑几分钟）。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// 回传文案里的输出尾部字符数（完整输出只进日志，避免卡顿式长文案）。
const OUTPUT_TAIL_CHARS: usize = 800;

/// 语言 id → `Lang`（未知语言明确报错，不静默）。
fn lang_of(language: &str) -> Result<Lang, String> {
    let want = language.trim().to_ascii_lowercase();
    Lang::all()
        .into_iter()
        .find(|l| l.id() == want)
        .ok_or_else(|| format!("未知语言：{language}"))
}

/// 打开某语言的开关（配置字段与 `Lang` 一一对应）。
fn enable_in(v: &mut ValidationSettings, lang: Lang) {
    match lang {
        Lang::TypeScript => v.typescript = true,
        Lang::Rust => v.rust = true,
        Lang::Python => v.python = true,
        Lang::Go => v.go = true,
        Lang::Java => v.java = true,
        Lang::Dart => v.dart = true,
    }
}

/// 六语言 server 状态（设置页徽标数据源）。
#[tauri::command]
pub async fn lsp_status(core: Core<'_>) -> Result<Vec<ServerStatus>, String> {
    let vcfg = core.cfg.read().unwrap().validation.clone();
    Ok(core.lsp.status(&vcfg).await)
}

/// 重新探测（设置页「重新检测」）：换新 PATH 快照 + 清探测缓存后重查。
#[tauri::command]
pub async fn lsp_redetect(core: Core<'_>) -> Result<Vec<ServerStatus>, String> {
    core.lsp.redetect().await;
    let vcfg = core.cfg.read().unwrap().validation.clone();
    Ok(core.lsp.status(&vcfg).await)
}

/// 重启某语言的全部 server 实例（下一次写入重新 spawn + initialize）。
#[tauri::command]
pub async fn lsp_restart(core: Core<'_>, language: String) -> Result<(), String> {
    let lang = lang_of(&language)?;
    core.lsp.restart(lang).await;
    Ok(())
}

/// 启用某语言的语义校验并**持久化**（与 `save_config` 同一条落盘路径，`ConfigState::save`）。
///
/// 先落盘再改内存：写盘失败时内存与磁盘保持一致（不会出现「看着开了、重启又关了」）；
/// 内存侧**原地置位**（不整份覆盖 `cfg`，不丢并发写入的其他配置），并同步 manager 的开关快照。
#[tauri::command]
pub async fn lsp_enable(core: Core<'_>, language: String) -> Result<(), String> {
    let lang = lang_of(&language)?;
    let mut snapshot = core.cfg.read().unwrap().clone();
    enable_in(&mut snapshot.validation, lang);
    snapshot.save().map_err(err)?;
    {
        let mut guard = core.cfg.write().unwrap();
        enable_in(&mut guard.validation, lang);
        core.lsp.set_settings(&guard.validation);
    }
    tracing::info!(language = lang.id(), "LSP 语义校验已启用并落盘");
    Ok(())
}

/// 一键安装该语言的 server（结果文案回前端；失败附 stderr/输出尾部）。
///
/// 顺序固定：**幂等探测 → 形态校验 → 围栏 → 执行 → 重探测**。
#[tauri::command]
pub async fn lsp_install(core: Core<'_>, language: String) -> Result<String, String> {
    let lang = lang_of(&language)?;
    let spec = server_spec::spec(lang);
    let vcfg = core.cfg.read().unwrap().validation.clone();

    // ① 幂等：解析顺序本就含 PATH / 项目 / extra_roots 探测，已找到就不再执行
    let status = core.lsp.status(&vcfg).await;
    if let Some(st) = status.iter().find(|s| s.language == lang.id()) {
        if st.found {
            return Ok(format!(
                "{} 已安装（{}），无需重复安装",
                spec.server_name, st.command
            ));
        }
    }

    // ② 只有可一键安装的形态有执行计划；Java/Dart 是 Manual（指向官方地址，不静默）
    let Some(plan) = install::plan(lang) else {
        let docs = spec.install.docs_url;
        let prereq = spec
            .install
            .prerequisite
            .clone()
            .unwrap_or_else(|| "无额外的安装命令".to_string());
        return Err(format!(
            "{} 需要手动安装：{prereq}。官方地址：{docs}",
            spec.server_name
        ));
    };

    // ③ 用户确认依据：引导卡（`ui/src/features/chat/LspGuideCard.tsx`）已经完整展示过
    //    这条命令，本次调用由用户点击「安装」触发。审批门需要运行中的会话 runtime，
    //    host 层拿不到（且命令入参只有 language），故不复用审批通道、不新造事件键。
    //    围栏仍照过：命令固定，但灾难级判定一律拦（审批关闭时高危也会退化为 Block）。
    {
        let cfg = core.cfg.read().unwrap().clone();
        let roots = crate::tools::pathutil::WriteRoots::new(
            core.data_dir.clone(),
            core.data_dir.clone(),
        );
        let policy = crate::safety::fence::FencePolicy::legacy(
            cfg.approval.enabled,
            cfg.approval.confirm_outside_create,
        );
        match crate::safety::fence::check_command_policy(
            &plan.display,
            &core.data_dir,
            &roots,
            policy,
        ) {
            crate::safety::fence::Verdict::Block { code, message } => {
                return Err(format!("{code}：{message}"));
            }
            verdict => {
                // Confirm 不另开审批卡：确认依据见上（用户显式点击已展示过的命令）
                tracing::info!(command = %plan.display, ?verdict, "LSP 安装命令围栏结论");
            }
        }
    }

    // ④ 执行：进程组 + Windows CREATE_NO_WINDOW + 超时 + 输出截尾（见 lsp/install.rs）
    tracing::info!(command = %plan.display, "开始执行 LSP server 安装命令");
    let out = install::execute(&plan, INSTALL_TIMEOUT).await?;
    let total = out.chars().count();
    let tail: String = out.chars().skip(total.saturating_sub(OUTPUT_TAIL_CHARS)).collect();
    // ⑤ 安装改变 PATH / 探测面 → 清缓存重探测一次，下一次写入即可用上；把结论一并回给用户
    core.lsp.redetect().await;
    let vcfg_after = core.cfg.read().unwrap().validation.clone();
    let after = core.lsp.status(&vcfg_after).await;
    let mut msg = format!("已安装 {}（{}）", spec.server_name, plan.display);
    match after.iter().find(|s| s.language == lang.id()) {
        Some(st) if st.found => msg.push_str(&format!("\n已生效：{}", st.command)),
        Some(st) => msg.push_str(&format!(
            "\n注意：重探测仍未找到 {}（{}），请检查安装输出或 PATH",
            spec.server_name, st.detail
        )),
        None => {}
    }
    let tail = tail.trim();
    if !tail.is_empty() {
        msg.push('\n');
        msg.push_str(tail);
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::fence::{check_command_policy, FencePolicy, Verdict};
    use crate::tools::pathutil::WriteRoots;

    #[test]
    fn lang_lookup_is_exact_and_case_insensitive() {
        assert_eq!(lang_of("TypeScript").unwrap(), Lang::TypeScript);
        assert_eq!(lang_of(" rust ").unwrap(), Lang::Rust);
        assert!(lang_of("vue").is_err());
        assert!(lang_of("").is_err());
    }

    #[test]
    fn enable_flips_only_the_named_language() {
        let mut v = ValidationSettings::default();
        v.rust = false;
        v.java = false;
        enable_in(&mut v, Lang::Java);
        assert!(v.java);
        assert!(!v.rust, "只动目标语言的开关");
        assert!(v.typescript && v.python && v.go && v.dart);
    }

    /// 安装命令是固定拼装的（非用户输入），但仍然过 fence：**至少不能被拦死**，
    /// 否则「一键安装」在审批关闭的机器上会整体失效。
    #[test]
    fn install_commands_are_not_blocked_by_fence() {
        let dir = tempfile::tempdir().unwrap();
        let roots = WriteRoots::new(dir.path().to_path_buf(), dir.path().to_path_buf());
        for lang in [Lang::TypeScript, Lang::Rust, Lang::Python, Lang::Go] {
            let plan = install::plan(lang).expect("可一键安装的语言必须有计划");
            for approval in [true, false] {
                let verdict = check_command_policy(
                    &plan.display,
                    dir.path(),
                    &roots,
                    FencePolicy::legacy(approval, false),
                );
                assert!(
                    !matches!(verdict, Verdict::Block { .. }),
                    "{} 的安装命令被围栏拦死（approval={approval}）：{verdict:?}",
                    lang.id()
                );
            }
        }
    }
}
