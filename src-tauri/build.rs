fn main() {
    inject_commit_sha();
    tauri_build::build()
}

/// 编译期注入 HEAD commit short-sha（「关于」弹框版本号括号段用）。
/// 任何失败（无 .git、git 不可用、输出非法）都静默跳过——版本信息只是
/// 锦上添花，绝不阻断构建；运行时经 option_env! 读取，缺失时前端仅显示纯版本号。
fn inject_commit_sha() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");

    // commit 变化时触发 build.rs 重跑（尽力而为：dev 增量编译接受一次构建的滞后）
    println!("cargo:rerun-if-changed={manifest_dir}/../.git/HEAD");
    println!("cargo:rerun-if-changed={manifest_dir}/../.git/refs/heads");

    let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(&manifest_dir)
        .output()
    else {
        return; // git 不可用：静默降级
    };
    if !output.status.success() {
        return; // 无 .git 或非法仓库：静默降级
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // 宽松校验：7 位十六进制才注入，防止异常环境注入任意字符串。
    // 注：--short=7 是「至少 7 位」语义，短 sha 不唯一时 git 可能返回更长值，
    // 此处有意按降级处理（显示纯版本号）而非截断——截断会有歧义风险。
    if sha.len() == 7 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        println!("cargo:rustc-env=WS_COMMIT_SHA={sha}");
    }
}
