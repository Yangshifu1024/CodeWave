//! 语言工具链（SDK）就绪探测：把「语言服务器没装」与「语言本身没装」分开讲。
//!
//! 只做**命令存在性检查**（不启进程）：`cargo` / `go` / `python3` / `node` / `dart` 走生效 PATH
//! 与语言约定安装目录（[`discovery::locate_command`]），Java 走 [`discovery::find_jdk21`]——与
//! jdtls 启动时用的是同一套 JDK 定位结论，避免「页面说 JDK 就绪、jdtls 却说找不到」。

use super::Lang;
use super::SdkStatus;
use super::discovery::{self, Discoverer};
use crate::core::config::LspSettings;

/// 探测目标表：展示名 + 可执行名候选（按顺序取第一个命中的）。
///
/// Java 的可执行名候选为空：它的判据是「JDK 21+ 定位」，不是「某个命令在 PATH 里」
/// （`JAVA_HOME` 常指向旧版本，故一律不读）。
pub fn targets(lang: Lang) -> (&'static str, &'static [&'static str]) {
    match lang {
        Lang::TypeScript => ("Node.js", &["node"]),
        Lang::Rust => ("Rust 工具链", &["cargo", "rustc"]),
        Lang::Python => ("Python", &["python3", "python"]),
        Lang::Go => ("Go 工具链", &["go"]),
        Lang::Java => ("JDK 21+", &[]),
        Lang::Dart => ("Dart / Flutter SDK", &["dart", "flutter"]),
    }
}

/// 探测某语言的工具链是否就绪。
pub fn probe(lang: Lang, cfg: &LspSettings, discoverer: &Discoverer) -> SdkStatus {
    let (name, commands) = targets(lang);
    if lang == Lang::Java {
        return match discovery::find_jdk21(&cfg.java_home, &cfg.extra_roots) {
            Some(jdk) => SdkStatus {
                ready: true,
                name: name.to_string(),
                detail: format!("已定位 {name}：{}", jdk.display()),
            },
            None => SdkStatus {
                ready: false,
                name: name.to_string(),
                detail: format!("未定位到 {name}（jdtls 需要 JDK 21+，且不会使用 JAVA_HOME）"),
            },
        };
    }
    for cmd in commands {
        if let Some(p) = discovery::locate_command(lang, cmd, discoverer) {
            return SdkStatus {
                ready: true,
                name: name.to_string(),
                detail: format!("已找到 {cmd}：{}", p.display()),
            };
        }
    }
    SdkStatus {
        ready: false,
        name: name.to_string(),
        detail: format!("未找到 {}", commands.join(" / ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 六语言都有展示名；除 Java 外都必须给出可执行名候选（否则「未就绪」无从判起）。
    #[test]
    fn every_language_has_a_probe_target() {
        for lang in Lang::all() {
            let (name, commands) = targets(lang);
            assert!(!name.is_empty(), "{} 缺展示名", lang.id());
            if lang != Lang::Java {
                assert!(!commands.is_empty(), "{} 缺可执行名候选", lang.id());
            }
        }
    }

    /// 探测结论必须自洽：`ready` 与 `detail` 不得互相矛盾（找到就该说找到）。
    #[test]
    fn probe_conclusion_is_self_consistent() {
        let cfg = LspSettings::default();
        let disc = Discoverer::new();
        for lang in Lang::all() {
            let sdk = probe(lang, &cfg, &disc);
            assert_eq!(sdk.name, targets(lang).0);
            assert!(!sdk.detail.is_empty(), "{} 的 detail 不得为空", lang.id());
            if sdk.ready {
                assert!(
                    sdk.detail.contains("已"),
                    "{} 就绪时 detail 应说明找到了什么：{}",
                    lang.id(),
                    sdk.detail
                );
            } else {
                assert!(
                    sdk.detail.contains("未"),
                    "{} 未就绪时 detail 应说明原因：{}",
                    lang.id(),
                    sdk.detail
                );
            }
        }
    }

    /// Java 的判据是 JDK 定位：显式配置一个临时 JDK 目录后必须转为就绪（不依赖真机装没装 JDK）。
    #[test]
    fn java_readiness_follows_jdk_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("jdk-21.0.9");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("release"), "JAVA_VERSION=\"21.0.9\"\n").unwrap();

        let cfg = LspSettings {
            java_home: home.to_string_lossy().to_string(),
            ..LspSettings::default()
        };
        let disc = Discoverer::new();
        let sdk = probe(Lang::Java, &cfg, &disc);
        assert!(sdk.ready, "{sdk:?}");
        assert!(sdk.detail.contains("JDK 21+"), "{}", sdk.detail);
    }
}
