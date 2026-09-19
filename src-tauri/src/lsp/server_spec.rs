//! 六语言 server 的静态描述表（命令、参数、manifest 倒排、初始化选项、安装引导）。
//!
//! 本文件是「知识表」——只放事实，不放探测逻辑（探测见 `discovery.rs`，编排见 `manager.rs`）。

use super::{InstallKind, Lang};
use serde_json::{Value, json};
use std::path::Path;

/// 安装引导的静态描述（[`super::InstallHint`] 的运行时形态）。
#[derive(Debug, Clone)]
pub struct InstallSpec {
    /// 形态
    pub kind: InstallKind,
    /// 可一键执行的命令（Manual/ConfirmEnable 为 None）
    pub command: Option<String>,
    /// 官方文档/下载地址
    pub docs_url: &'static str,
    /// 前置条件说明
    pub prerequisite: Option<String>,
}

/// 单语言 server 描述。
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// 语言
    pub lang: Lang,
    /// server 可执行名（PATH 查找 + 展示名）
    pub server_name: &'static str,
    /// 启动参数（配置覆盖自带参数时不追加）
    pub args: Vec<String>,
    /// 该语言的「项目根标志文件」清单
    pub manifest: &'static [&'static str],
    /// `initialize` 的 `initializationOptions`
    pub init_options: Value,
    /// 找不到本地安装时的 npx 降级命令（仅 TS/JS 与 Python）
    pub npx_fallback: Option<Vec<String>>,
    /// 安装引导
    pub install: InstallSpec,
    /// 判定 [`super::client::Readiness::Warm`] 的**预热窗**（毫秒，**按语言分档**）。
    ///
    /// 为什么不是一个统一的 3s：server 启动后推的第一个空集是**占位通告**，它与「这个文件真的
    /// 0 错误」在协议上长得一模一样，只能靠「server 到底分析完了没有」来区分，而不同 server 的
    /// 索引成本差一个数量级（tsserver 看单文件、jdtls 要连依赖树一起索引）。档位越重，说明
    /// 「多久还没出声就不可信」的等待越长——**宁可久报「未就绪」，也不要早报「通过」**。
    ///
    /// 分档（只增不减，调大是安全方向）：
    /// - TS/Python 10s（tsserver / pyright 单文件起步，项目一大多文件仍要几秒）；
    /// - Go/Dart 20s（gopls 要加载包、analysis server 要解 `package_config.json`）；
    /// - Java/Rust 30s（jdtls 要索引依赖树；rust-analyzer 有 `serverStatus.quiescent` 这条权威
    ///   信号兜底，这里的档位只服务于不报 serverStatus 的版本，取最保守值）。
    pub warmup_ms: u64,
}

/// 取某语言的 server 描述。
pub fn spec(lang: Lang) -> ServerSpec {
    match lang {
        Lang::TypeScript => ServerSpec {
            lang,
            server_name: "typescript-language-server",
            args: vec!["--stdio".into()],
            manifest: &["tsconfig.json", "package.json"],
            warmup_ms: 10_000,
            // tsserver.path 由 [`assemble_init_options`] 在 manager 侧按项目内 typescript 填充
            init_options: json!({}),
            npx_fallback: Some(vec![
                "npx".into(),
                "-y".into(),
                "typescript-language-server".into(),
                "--stdio".into(),
            ]),
            install: InstallSpec {
                kind: InstallKind::Installable,
                command: Some("npm i -g typescript-language-server typescript".into()),
                docs_url: "https://github.com/typescript-language-server/typescript-language-server",
                prerequisite: Some("需 Node.js（npm 在 PATH 中）".into()),
            },
        },
        Lang::Rust => ServerSpec {
            lang,
            server_name: "rust-analyzer",
            args: vec![],
            manifest: &["Cargo.toml"],
            warmup_ms: 30_000,
            // checkOnSave=false：用 rust-analyzer 的 native diagnostics（不额外跑 cargo check，避免写后抖动）
            init_options: json!({ "checkOnSave": false }),
            npx_fallback: None,
            install: InstallSpec {
                kind: InstallKind::Installable,
                command: Some("rustup component add rust-analyzer".into()),
                docs_url: "https://rust-analyzer.github.io/",
                prerequisite: Some("需 rustup（或从 release 页下载独立二进制）".into()),
            },
        },
        Lang::Python => ServerSpec {
            lang,
            server_name: "pyright-langserver",
            args: vec!["--stdio".into()],
            manifest: &["pyproject.toml", "pyrightconfig.json", "setup.py"],
            warmup_ms: 10_000,
            init_options: json!({}),
            npx_fallback: Some(vec![
                "npx".into(),
                "-y".into(),
                "pyright-langserver".into(),
                "--stdio".into(),
            ]),
            install: InstallSpec {
                kind: InstallKind::Installable,
                command: Some("npm i -g pyright".into()),
                docs_url: "https://microsoft.github.io/pyright/",
                prerequisite: Some("需 Node.js（npm 在 PATH 中）".into()),
            },
        },
        Lang::Go => ServerSpec {
            lang,
            server_name: "gopls",
            args: vec![],
            manifest: &["go.mod"],
            warmup_ms: 20_000,
            // **真机确认项**：analyses 的子键名以 gopls 官方文档为准（此处关闭 style/lint 类分析，
            // 只留编译期错误，避免写后被风格建议淹没）。键名拼错时 gopls 会忽略而非报错。
            init_options: json!({
                "analyses": {
                    "staticcheck": false,
                    "stylecheck": false,
                    "unusedparams": false,
                    "unusedvariable": false,
                    "unusedwrite": false,
                }
            }),
            npx_fallback: None,
            install: InstallSpec {
                kind: InstallKind::Installable,
                command: Some("go install golang.org/x/tools/gopls@latest".into()),
                docs_url: "https://pkg.go.dev/golang.org/x/tools/gopls",
                prerequisite: Some(
                    "需 Go 工具链；`go install` 的产物在 $(go env GOPATH)/bin".into(),
                ),
            },
        },
        Lang::Java => ServerSpec {
            lang,
            server_name: "jdtls",
            // **真机确认项**：`-data` 由 pool 追加（`<项目数据目录>/lsp/java`）
            args: vec![],
            warmup_ms: 30_000,
            manifest: &[
                "pom.xml",
                "build.gradle",
                "build.gradle.kts",
                "settings.gradle",
                "settings.gradle.kts",
            ],
            init_options: json!({}),
            npx_fallback: None,
            install: InstallSpec {
                kind: InstallKind::Manual,
                command: None,
                docs_url: "https://github.com/eclipse-jdtls/eclipse.jdt.ls",
                prerequisite: Some("需 JDK 21+；jdtls 不会使用 JAVA_HOME".into()),
            },
        },
        Lang::Dart => ServerSpec {
            lang,
            server_name: "dart",
            args: vec!["language-server".into()],
            manifest: &["pubspec.yaml"],
            warmup_ms: 20_000,
            init_options: json!({}),
            npx_fallback: None,
            install: InstallSpec {
                kind: InstallKind::Manual,
                command: None,
                docs_url: "https://dart.dev/get-dart",
                prerequisite: Some("需 Flutter/Dart SDK（cmd 中已 `flutter pub get`）".into()),
            },
        },
    }
}

/// 全部语言（设置页行序 / 扫描用）。
pub fn all_specs() -> Vec<ServerSpec> {
    Lang::all().into_iter().map(spec).collect()
}

/// 组装最终 `initializationOptions`：在静态表基础上按项目现场补事实。
///
/// 当前只做一件事：TypeScript 若在项目内找到 `node_modules/typescript`，把 `tsserver.path`
/// 指过去（用项目锁定的 tsc 版本，避免全局版本与项目不一致导致的假报错）。
pub fn assemble_init_options(lang: Lang, root: &Path, spec: &ServerSpec) -> Value {
    let mut opts = spec.init_options.clone();
    if lang == Lang::TypeScript {
        let tsserver = root
            .join("node_modules")
            .join("typescript")
            .join("lib")
            .join("tsserver.js");
        if tsserver.is_file() {
            let path = tsserver.to_string_lossy().to_string();
            if let Some(obj) = opts.as_object_mut() {
                obj.insert("tsserver".into(), json!({ "path": path }));
                obj.insert(
                    "preferences".into(),
                    json!({ "includeInlayParameterNameHints": "none" }),
                );
            }
        }
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_languages_have_distinct_servers() {
        let specs = all_specs();
        assert_eq!(specs.len(), 6);
        let mut names: Vec<&str> = specs.iter().map(|s| s.server_name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 6);
    }

    #[test]
    fn lang_manifest_matches_language() {
        assert_eq!(spec(Lang::Rust).manifest, &["Cargo.toml"]);
        assert!(spec(Lang::TypeScript).manifest.contains(&"tsconfig.json"));
        assert!(spec(Lang::TypeScript).manifest.contains(&"package.json"));
        assert_eq!(spec(Lang::Dart).args, vec!["language-server".to_string()]);
        assert!(spec(Lang::TypeScript).npx_fallback.is_some());
        assert!(spec(Lang::Python).npx_fallback.is_some());
        assert!(spec(Lang::Go).npx_fallback.is_none());
    }

    /// 预热窗必须**按语言分档**且远大于旧版统一的 3s（空集基线在重索引语言上会被过早立信）。
    #[test]
    fn warmup_tiers_are_per_language_and_conservative() {
        assert!(spec(Lang::TypeScript).warmup_ms >= 10_000);
        assert!(spec(Lang::Python).warmup_ms >= 10_000);
        assert!(spec(Lang::Go).warmup_ms >= 20_000);
        assert!(spec(Lang::Dart).warmup_ms >= 20_000);
        assert!(spec(Lang::Java).warmup_ms >= 30_000);
        // rust-analyzer 有 quiescent 兜底，但档位不得低于最轻的一档
        assert!(spec(Lang::Rust).warmup_ms >= 10_000);
        for s in all_specs() {
            assert!(
                s.warmup_ms > 3_000,
                "{} 的预热窗不得回到「统一 3s」：{}",
                s.lang.id(),
                s.warmup_ms
            );
        }
    }

    #[test]
    fn java_and_dart_are_manual_install() {
        assert_eq!(spec(Lang::Java).install.kind, InstallKind::Manual);
        assert_eq!(spec(Lang::Dart).install.kind, InstallKind::Manual);
        assert!(
            spec(Lang::Java)
                .install
                .prerequisite
                .as_deref()
                .unwrap()
                .contains("JDK 21")
        );
    }

    #[test]
    fn init_options_picks_up_project_local_typescript() {
        let tmp = tempfile::tempdir().unwrap();
        let s = spec(Lang::TypeScript);
        let plain = assemble_init_options(Lang::TypeScript, tmp.path(), &s);
        assert!(plain.get("tsserver").is_none());
        std::fs::create_dir_all(tmp.path().join("node_modules/typescript/lib")).unwrap();
        std::fs::write(
            tmp.path().join("node_modules/typescript/lib/tsserver.js"),
            "// stub",
        )
        .unwrap();
        let with_ts = assemble_init_options(Lang::TypeScript, tmp.path(), &s);
        assert!(
            with_ts["tsserver"]["path"]
                .as_str()
                .unwrap()
                .replace('\\', "/")
                .ends_with("node_modules/typescript/lib/tsserver.js")
        );
    }
}
