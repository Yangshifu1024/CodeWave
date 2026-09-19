//! `core::openers` 单测：探测全部走注入谓词（不触碰真实安装目录），启动路径不真开进程。

use super::*;
use std::fs;

fn bases(path_dirs: Vec<PathBuf>) -> EnvBases {
    EnvBases {
        path_dirs,
        ext: vec![".exe".to_string(), ".cmd".to_string()],
        ..Default::default()
    }
}

fn empty_dir(_: &Path) -> Vec<PathBuf> {
    Vec::new()
}

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

#[test]
fn files_alias_candidates_prefers_stable() {
    let candidates = files_alias_candidates(Path::new("C:/Users/demo/AppData/Local"));
    assert_eq!(candidates.len(), 4);
    let first = candidates[0].to_string_lossy().replace('\\', "/");
    assert!(
        first.ends_with("Microsoft/WindowsApps/files-stable.exe"),
        "{first}"
    );
    assert_eq!(
        candidates
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        vec![
            "files-stable.exe",
            "files-dev.exe",
            "files-preview.exe",
            "files-canary.exe"
        ]
    );
}

#[test]
fn pick_windows_file_manager_prefers_stable_then_channel_then_none() {
    let local = Path::new("C:/local");
    let stable = local.join("Microsoft/WindowsApps/files-stable.exe");
    let dev = local.join("Microsoft/WindowsApps/files-dev.exe");

    // 三个渠道都在 → 稳版优先
    let picked = pick_windows_file_manager(local, &|p| p == stable || p == dev).unwrap();
    assert_eq!(picked, stable);

    // 只有 dev → 返回 dev
    let picked = pick_windows_file_manager(local, &|p| p == dev).unwrap();
    assert_eq!(picked, dev);

    // 全不命中 → None（调用方回退 explorer）
    assert!(pick_windows_file_manager(local, &|_| false).is_none());
}

#[test]
fn expand_template_only_maps_known_bases() {
    let env = EnvBases {
        home: Some(PathBuf::from("C:/home/u")),
        local_app_data: Some(PathBuf::from("C:/home/u/AppData/Local")),
        program_files: Some(PathBuf::from("C:/Program Files")),
        program_files_x86: Some(PathBuf::from("C:/Program Files (x86)")),
        applications: None,
        ..Default::default()
    };
    let expanded = expand_template("{LOCALAPPDATA}/Programs/Zed/zed.exe", &env).unwrap();
    assert_eq!(
        expanded,
        PathBuf::from("C:/home/u/AppData/Local/Programs/Zed/zed.exe")
    );
    // 缺失基目录 / 未知前缀 → None（绝不猜测路径）
    assert!(expand_template("{APPLICATIONS}/Zed.app/Contents/MacOS/zed", &env).is_none());
    assert!(expand_template("{UNKNOWN}/x", &env).is_none());
}

#[test]
fn resolve_in_path_prefers_dir_order_then_exe_over_cmd() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    touch(&dir_b.path().join("code.exe"));
    touch(&dir_a.path().join("code.cmd"));

    // PATH 顺序优先：dir_a 在前即命中 .cmd
    let env = bases(vec![dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);
    let hit = resolve_in_path("code", &env, &|p| p.exists()).unwrap();
    assert!(hit.ends_with("code.cmd"), "{hit:?}");

    // 同目录内 .exe 优先于 .cmd
    touch(&dir_a.path().join("code.exe"));
    let hit = resolve_in_path("code", &env, &|p| p.exists()).unwrap();
    assert!(hit.ends_with("code.exe"), "{hit:?}");

    // 无扩展名平台（ext 为空）只按名称命中
    let env = EnvBases {
        path_dirs: vec![dir_a.path().to_path_buf()],
        ext: Vec::new(),
        ..Default::default()
    };
    assert!(resolve_in_path("code.exe", &env, &|p| p.exists()).is_some());
    assert!(resolve_in_path("code", &env, &|p| p.exists()).is_none());
}

#[test]
fn detect_editors_keeps_table_order_and_default_is_first_hit() {
    let bin = tempfile::tempdir().unwrap();
    // 故意让 Zed 的 exe 先存在于文件系统，验证顺序由候选表决定而非目录顺序
    touch(&bin.path().join("zed.exe"));
    touch(&bin.path().join("code.cmd"));

    let env = bases(vec![bin.path().to_path_buf()]);
    let found = detect_editors_with(&env, &|p| p.exists(), &empty_dir);
    let ids: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["vscode", "zed"]);
    assert_eq!(found[0].name, "VS Code");
    // 默认项 = 第一个检测到的
    assert!(found[0].path.ends_with("code.cmd"));
}

#[test]
fn detect_editors_uses_fixed_install_paths_when_path_is_empty() {
    // 各平台候选表不同（Windows：%LOCALAPPDATA%/%PROGRAMFILES%；macOS：/Applications；
    // Linux：绝对路径 /snap/bin/code），所以本用例**从本平台表里取**第一条带固定路径的条目，
    // 把全部基目录换成不存在的探针路径 + 注入谓词命中——CI 上不碰 /snap、/Applications 等系统目录，
    // 也不需要 cfg 分支（否则 Linux/macOS 上会按 Windows 的路径断言而必红）。
    let env = EnvBases {
        path_dirs: Vec::new(),
        home: Some(PathBuf::from("/probe/home")),
        local_app_data: Some(PathBuf::from("/probe/localappdata")),
        program_files: Some(PathBuf::from("/probe/programfiles")),
        program_files_x86: Some(PathBuf::from("/probe/programfilesx86")),
        applications: Some(PathBuf::from("/probe/applications")),
        ..Default::default()
    };
    let spec = EDITOR_SPECS
        .iter()
        .find(|s| !s.fixed.is_empty())
        .expect("每个平台的候选表都应至少有一条固定安装路径");
    let target = expand_template(spec.fixed[0], &env).expect("首条固定安装模板应可展开");

    let found = detect_editors_with(&env, &|p| p == target, &empty_dir);
    assert_eq!(found.len(), 1, "PATH 为空时应恰好命中固定安装路径这一条");
    assert_eq!(found[0].id, spec.id);
    assert_eq!(PathBuf::from(&found[0].path), target);
}

#[test]
fn expand_template_accepts_absolute_paths() {
    // Linux 快照安装是绝对路径而非模板；若返回 None，该候选永远不会进入探测（真实缺陷）
    assert_eq!(
        expand_template("/snap/bin/code", &EnvBases::default()),
        Some(PathBuf::from("/snap/bin/code"))
    );
}

#[test]
fn detect_editors_returns_empty_when_nothing_installed() {
    let env = bases(vec![PathBuf::from("Z:/definitely/missing")]);
    assert!(detect_editors_with(&env, &|_| false, &empty_dir).is_empty());
}

#[test]
fn jetbrains_scan_maps_product_names_and_appends_last() {
    let program_files = PathBuf::from("C:/Program Files");
    let product_dir = program_files.join("JetBrains/RustRover 2026.2.2");
    let exe = product_dir.join("bin/rustrover64.exe");
    let script_dir = PathBuf::from("C:/local/JetBrains/Toolbox/scripts");

    let env = EnvBases {
        path_dirs: vec![PathBuf::from("C:/bin")],
        ext: vec![".exe".to_string()],
        local_app_data: Some(PathBuf::from("C:/local")),
        program_files: Some(program_files.clone()),
        ..Default::default()
    };
    let read_dir = |p: &Path| -> Vec<PathBuf> {
        if p == script_dir {
            vec![script_dir.join("idea.cmd")]
        } else if p == program_files.join("JetBrains") {
            vec![product_dir.clone()]
        } else if p == product_dir.join("bin") {
            vec![exe.clone()]
        } else {
            Vec::new()
        }
    };
    let exists = |p: &Path| p == exe || p == script_dir.join("idea.cmd");

    // PATH 里有一个 VS Code，验证 JetBrains 组恒在末位
    let code = tempfile::tempdir().unwrap();
    let code_exe = code.path().join("code.exe");
    touch(&code_exe);
    let env = EnvBases {
        path_dirs: vec![code.path().to_path_buf()],
        ..env.clone()
    };

    // 谓词注入：真实 FS 与假安装目录混合命中（PATH 命中用真实文件，JetBrains 用固定列表）
    let exists = |p: &Path| exists(p) || p == code_exe;

    let found = detect_editors_with(&env, &exists, &read_dir);
    let ids: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["vscode", "jetbrains:intellijidea", "jetbrains:rustrover"]
    );
    let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["VS Code", "IntelliJ IDEA", "RustRover"]);
}

#[test]
fn jetbrains_product_mapping_ignores_non_jetbrains_stems() {
    assert_eq!(
        jetbrains_product_name("rustrover64"),
        Some("RustRover".to_string())
    );
    assert_eq!(
        jetbrains_product_name("idea64"),
        Some("IntelliJ IDEA".to_string())
    );
    assert_eq!(jetbrains_product_name("zed"), None);
    assert_eq!(jetbrains_product_name("code"), None);
}

#[cfg(target_os = "windows")]
#[test]
fn script_command_line_wraps_quotes_for_cmd_c_semantics() {
    // 路径含空格：必须再包一层引号，否则 cmd /C 会剔掉首尾引号把命令拼坏
    let line = script_command_line(
        Path::new("D:/App/Microsoft VS Code/bin/code.cmd"),
        &[Path::new("D:/Work/SideProjects/CodeWave")],
    );
    assert_eq!(
        line,
        "\"\"D:/App/Microsoft VS Code/bin/code.cmd\" \"D:/Work/SideProjects/CodeWave\"\""
    );
    // 无空格路径同样成立（剥壳后仍是 `"prog" "arg"`）
    let plain = script_command_line(Path::new("C:/x/subl.cmd"), &[Path::new("D:/p")]);
    assert_eq!(plain, "\"\"C:/x/subl.cmd\" \"D:/p\"\"");
}

#[test]
fn open_in_editor_rejects_unknown_id() {
    // 未检测到的 id 一律报错，且不落到任何默认编辑器
    let err = open_in_editor("nope", Path::new(".")).unwrap_err();
    assert!(err.contains("未检测到编辑器"), "{err}");
}
