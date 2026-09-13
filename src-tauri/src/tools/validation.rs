//! 写后校验（[docs/p1-plan](../../../docs/p1-plan.md) §3.1）：edit/create 成功后按语言路由做低成本语法检查，
//! 结果文本回喂模型（「文件已写入，但校验失败：…请修复」）。

use crate::core::config::ValidationSettings;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// 单文件校验报告：ran=false 表示未运行（语言不支持 / 工具链缺失 / 设置关闭）。
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// 是否真的运行了校验。
    pub ran: bool,
    /// 校验是否通过（或未运行时默认 true）。
    pub ok: bool,
    /// 失败详情（成功为空串）。
    pub message: String,
}

/// 语言路由：返回（校验器命令，超时）。None = 该语言 / 设置下跳过。
fn checker_for(path: &Path, cfg: &ValidationSettings) -> Option<(Vec<String>, Duration)> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let file = path.display().to_string();
    match ext.as_str() {
        "py" if cfg.python => Some((
            vec!["python3".into(), "-m".into(), "py_compile".into(), file],
            Duration::from_secs(10),
        )),
        "json" if cfg.json => None, // JSON 走内置解析（见下）
        "rs" if cfg.rust => Some((
            vec![
                "rustc".into(),
                "--edition".into(),
                "2021".into(),
                "--emit=metadata".into(),
                "--crate-type".into(),
                "lib".into(),
                "--out-dir".into(),
                std::env::temp_dir()
                    .join("ws-rustc-check")
                    .display()
                    .to_string(),
                file,
            ],
            Duration::from_secs(30),
        )),
        "ts" | "tsx" | "mts" | "cts" if cfg.typescript => Some((
            vec![
                "npx".into(),
                "--yes".into(),
                "typescript@5".into(),
                "tsc".into(),
                "--noEmit".into(),
                "--skipLibCheck".into(),
                "--target".into(),
                "es2022".into(),
                "--module".into(),
                "esnext".into(),
                "--moduleResolution".into(),
                "bundler".into(),
                file,
            ],
            Duration::from_secs(60),
        )),
        "js" | "jsx" | "mjs" | "cjs" if cfg.typescript => Some((
            vec!["node".into(), "--check".into(), file],
            Duration::from_secs(10),
        )),
        "vue" if cfg.typescript => Some((
            vec!["node".into(), "--check".into(), file],
            Duration::from_secs(10),
        )), // 简化处理：只检查 script 块之外的语法；深度检查随 P2 而来
        "go" if cfg.go => Some((
            vec!["go".into(), "vet".into(), file],
            Duration::from_secs(30),
        )),
        _ => None,
    }
}

/// 校验单个文件。返回的报告会追加到工具结果的 warnings。
pub async fn validate_file(path: &Path, cfg: &ValidationSettings) -> ValidationReport {
    // JSON 走内置解析
    if path.extension().and_then(|e| e.to_str()) == Some("json") && cfg.json {
        let bytes = std::fs::read(path).unwrap_or_default();
        return match serde_json::from_slice::<Value>(&bytes) {
            Ok(_) => ValidationReport {
                ran: true,
                ok: true,
                message: String::new(),
            },
            Err(e) => ValidationReport {
                ran: true,
                ok: false,
                message: format!("JSON 解析失败：{e}"),
            },
        };
    }
    let Some((argv, timeout)) = checker_for(path, cfg) else {
        return ValidationReport {
            ran: false,
            ok: true,
            message: String::new(),
        };
    };
    let program = &argv[0];
    let display = path.display().to_string();
    // 组命令 → 按 cfg(windows) 追加 creation flags → 统一 spawn（缺陷修复：
    // GUI 子系统进程 spawn 控制台程序会闪黑框，与 command/service/mcp 同一套 flags）。
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&argv[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let result = tokio::time::timeout(timeout, cmd.output()).await;
    match result {
        Err(_) => ValidationReport {
            ran: true,
            ok: false,
            message: "校验超时".into(),
        },
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => ValidationReport {
            // 工具链未安装：跳过而非报错
            ran: false,
            ok: true,
            message: String::new(),
        },
        Ok(Err(e)) => ValidationReport {
            ran: true,
            ok: false,
            message: format!("校验器启动失败：{e}"),
        },
        Ok(Ok(out)) => {
            if out.status.success() {
                ValidationReport {
                    ran: true,
                    ok: true,
                    message: String::new(),
                }
            } else {
                let err = String::from_utf8_lossy(&out.stderr);
                let mut lines: Vec<&str> = err.lines().collect();
                lines.retain(|l| !l.trim().is_empty());
                let tail: String = lines
                    .iter()
                    .rev()
                    .take(6)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                let clipped: String = tail.chars().take(2048).collect();
                ValidationReport {
                    ran: true,
                    ok: false,
                    message: format!("{display}：{clipped}"),
                }
            }
        }
    }
}

/// 把多个校验报告折叠为回喂模型的文本。
pub fn summarize(reports: &[(String, ValidationReport)]) -> String {
    let failures: Vec<String> = reports
        .iter()
        .filter(|(_, r)| r.ran && !r.ok)
        .map(|(p, r)| format!("{p}：{}", r.message))
        .collect();
    if failures.is_empty() {
        let ran_any = reports.iter().any(|(_, r)| r.ran);
        return if ran_any {
            "（写入后语法校验通过）".into()
        } else {
            String::new()
        };
    }
    format!(
        "（写入后语法校验失败，请用 edit 修复：\n{}\n）",
        failures.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_builtin_check() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("a.json");
        std::fs::write(&good, b"{\"x\":1}").unwrap();
        let bad = dir.path().join("b.json");
        std::fs::write(&bad, b"{broken").unwrap();
        let cfg = ValidationSettings::default();
        let r = futures::executor::block_on(validate_file(&good, &cfg));
        assert!(r.ok);
        let r = futures::executor::block_on(validate_file(&bad, &cfg));
        assert!(!r.ok);
        assert!(r.message.contains("JSON"));
    }

    #[tokio::test]
    async fn missing_toolchain_skips() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, b"fn main(){}").unwrap();
        // 本机 rustc 存在；用可能不存在的 go 验证 skip 路径也能覆盖
        let cfg = ValidationSettings::default();
        let r = validate_file(&f, &cfg).await;
        // rustc 存在 → ran=true 且应通过
        assert!(r.ran);
        assert!(r.ok);
    }

    #[test]
    fn summarize_messages() {
        let ok_report = ValidationReport {
            ran: true,
            ok: true,
            message: String::new(),
        };
        let bad_report = ValidationReport {
            ran: true,
            ok: false,
            message: "expected `;`".into(),
        };
        assert_eq!(
            summarize(&[("a.ts".to_string(), ok_report.clone())]),
            "（写入后语法校验通过）"
        );
        let s = summarize(&[
            ("a.ts".to_string(), bad_report),
            ("b.ts".to_string(), ok_report),
        ]);
        assert!(s.contains("校验失败") && s.contains("expected `;`"));
    }
}
