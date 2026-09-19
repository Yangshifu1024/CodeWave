//! 集成测试：用假 LSP server（`CARGO_BIN_EXE_codewave-fake-lsp`）跑真实 stdio 全链路。
//!
//! 纪律：
//! - 所有用例共享一把**串行锁**（`SERIAL`）——假 server 靠环境变量驱动，而 `set_var` 与
//!   其它线程读环境有竞争；串行化同时也让进程类用例不会互相抢资源（避免与既有 flaky 用例叠乘）。
//! - 所有等待都有显式上限，绝不允许「挂死」式的等待。

use codewave_lib::lsp::discovery::{self, Discoverer};
use codewave_lib::lsp::pool::{AcquireError, LspPool};
use codewave_lib::lsp::server_spec;
use codewave_lib::lsp::{
    Baseline, DiagnosticItem, Lang, LspManager, LspSettings, SkipReason, ValidateRequest,
    ValidationOutcome, ValidationSettings,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// 进程类用例的串行锁（见模块头注释）。
static SERIAL: Mutex<()> = Mutex::new(());

/// 假 server 认得的所有环境变量（每次 setup 先全部清掉，避免用例间串味）。
const KEYS: &[&str] = &[
    "CW_FAKE_LSP",
    "CW_FAKE_LSP_LOG",
    "CW_FAKE_LSP_MODE",
    "CW_FAKE_LSP_DIAG",
    "CW_FAKE_LSP_DIAG_SEQ",
    "CW_FAKE_LSP_DELAY_MS",
    "CW_FAKE_LSP_DELAY_MS_CHANGE",
    "CW_FAKE_LSP_EMPTY_FIRST_MS",
    "CW_FAKE_LSP_EMPTY_TIMES",
    "CW_FAKE_LSP_ALWAYS_EMPTY",
    "CW_FAKE_LSP_FRAGMENT",
    "CW_FAKE_LSP_SERVER_REQUESTS",
    "CW_FAKE_LSP_NO_LENGTH",
];

/// 一把不会因别的用例 panic 而中毒的串行锁。
fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// 环境变量守卫（drop 时清干净）。
struct FakeEnv;

impl FakeEnv {
    fn apply(pairs: &[(String, String)]) -> FakeEnv {
        for k in KEYS {
            // SAFETY: 全测试二进制内的进程类用例都在 SERIAL 锁内串行执行
            unsafe { std::env::remove_var(k) };
        }
        for (k, v) in pairs {
            // SAFETY: 同上
            unsafe { std::env::set_var(k, v) };
        }
        FakeEnv
    }
}

impl Drop for FakeEnv {
    fn drop(&mut self) {
        for k in KEYS {
            // SAFETY: 同上
            unsafe { std::env::remove_var(k) };
        }
    }
}

/// 假 server 可执行文件路径（cargo 注入）。
fn fake_program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codewave-fake-lsp"))
}

/// 一个含 `tsconfig.json` 的临时项目（TypeScript 语言根 = 项目根）。
struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    file: PathBuf,
    log: PathBuf,
}

impl Project {
    fn new(label: &str) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("tsconfig.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join(label);
        std::fs::write(&file, "const a: number = 1;\n").unwrap();
        let log = root.join("fake-lsp-log.txt");
        Project {
            _dir: dir,
            root,
            file,
            log,
        }
    }

    /// 写该用例的环境变量（自动带上 `CW_FAKE_LSP=1` 与日志路径）。
    fn env(&self, pairs: &[(&str, &str)]) -> FakeEnv {
        let mut all: Vec<(String, String)> = vec![
            ("CW_FAKE_LSP".into(), "1".into()),
            (
                "CW_FAKE_LSP_LOG".into(),
                self.log.to_string_lossy().to_string(),
            ),
        ];
        all.extend(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        FakeEnv::apply(&all)
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// 把配置里的 TS server 指向假 server。
fn cfg_ts(sync_window_ms: u64) -> LspSettings {
    let mut cfg = LspSettings::default();
    cfg.commands.typescript = fake_program().to_string_lossy().to_string();
    cfg.sync_window_ms = sync_window_ms;
    cfg.dedupe_limit = 2;
    cfg.max_diagnostics = 20;
    cfg.max_chars = 4000;
    cfg
}

fn request(p: &Project, prev: &str, new: &str) -> ValidateRequest {
    ValidateRequest {
        project_id: Some("p-integration".into()),
        project_root: p.root.clone(),
        path: p.file.clone(),
        rel_path: format!("src/{}", p.file.file_name().unwrap().to_string_lossy()),
        lang: Lang::TypeScript,
        new_content: Some(new.to_string()),
        prev_content: Some(prev.to_string()),
    }
}

fn err(line: u32, code: &str, message: &str) -> Value {
    json!({
        "range": { "start": { "line": line, "character": 4 }, "end": { "line": line, "character": 9 } },
        "severity": 1,
        "code": code,
        "message": message,
        "source": "fake"
    })
}

fn warn(line: u32, code: &str, message: &str) -> Value {
    json!({
        "range": { "start": { "line": line, "character": 4 }, "end": { "line": line, "character": 9 } },
        "severity": 2,
        "code": code,
        "message": message
    })
}

/// 取回喂内容（`Passed` 视为无内容；`Skipped` 直接报错，用例不该走到那里）。
fn feedback(outcome: &ValidationOutcome) -> (Vec<DiagnosticItem>, usize, usize) {
    match outcome {
        ValidationOutcome::Diagnosed {
            items,
            removed,
            truncated,
            ..
        } => (items.clone(), *removed, *truncated),
        ValidationOutcome::Passed { .. } => (Vec::new(), 0, 0),
        ValidationOutcome::Skipped { reason, .. } => {
            panic!("期望有回喂结论，实得 Skipped：{reason:?}")
        }
    }
}

fn skip_reason(outcome: &ValidationOutcome) -> &SkipReason {
    match outcome {
        ValidationOutcome::Skipped { reason, .. } => reason,
        other => panic!("期望 Skipped，实得 {other:?}"),
    }
}

/// 轮询到「可信基线」（server 冷启动期间会返回 None）。
async fn reliable_baseline(
    m: &LspManager,
    req: &ValidateRequest,
    cfg: &LspSettings,
    budget: Duration,
) -> Baseline {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(b) = m.baseline(req, cfg).await {
            if b.reliable() {
                return b;
            }
        }
        assert!(Instant::now() < deadline, "基线在预算内未变为可信");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 轮询等日志满足条件（用于「服务端请求已被应答」这类异步证据）。
fn wait_log(p: &Project, budget: Duration, pred: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + budget;
    loop {
        let text = p.log_text();
        if pred(&text) {
            return text;
        }
        if Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("tasklist");
    cmd.args(["/FI", &format!("PID eq {pid}"), "/NH"]);
    cmd.creation_flags(0x0800_0000);
    cmd.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 进程确实退出（不留孤儿）：轮询到 pid 消失。
fn assert_pid_gone(pid: u32, budget: Duration) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("假 server 进程（pid {pid}）在预算内未退出——存在孤儿进程");
}

/// ① 正常 didOpen → 诊断 → 差集：基线 3 条，改后 2 条旧 + 1 条新 → 只回喂新增的 1 条 + removed 计数。
#[tokio::test]
async fn normal_flow_feeds_only_new_errors() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let seq = json!([
        [
            err(
                2,
                "TS2322",
                "Type 'string' is not assignable to type 'number'."
            ),
            err(3, "TS9999", "second existing error"),
            err(4, "TS1111", "third existing error")
        ],
        [
            err(
                2,
                "TS2322",
                "Type 'string' is not assignable to type 'number'."
            ),
            err(3, "TS9999", "second existing error"),
            err(9, "TS8888", "brand new error")
        ]
    ]);
    let _env = p.env(&[("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string())]);
    let m = LspManager::new();
    let cfg = cfg_ts(1500);
    let req = request(&p, "const a: number = 1;\n", "const a: number = 'x';\n");

    let base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;
    let outcome = m.validate(&req, Some(&base), &cfg).await;
    let (items, removed, truncated) = feedback(&outcome);
    assert_eq!(items.len(), 1, "只回喂新增的 1 条：{items:?}");
    assert_eq!(items[0].code, "TS8888");
    assert_eq!(items[0].line, 10, "0-based 行 9 → 1-based 10");
    assert_eq!(removed, 1, "基线里的 TS1111 被消除");
    assert_eq!(truncated, 0);

    let log = p.log_text();
    let open = log
        .find("recv textDocument/didOpen")
        .expect("应收到 didOpen");
    let change = log
        .find("recv textDocument/didChange")
        .expect("应收到 didChange");
    assert!(open < change, "didOpen 必须先于 didChange：{log}");
    m.shutdown_all().await;
}

/// ② 分片 framing：每条帧拆 3 段写入，解析结论必须与不拆时一致。
#[tokio::test]
async fn fragmented_frames_still_parse() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let seq = json!([
        [err(0, "TS1", "existing")],
        [err(0, "TS1", "existing"), err(1, "TS2", "new one")]
    ]);
    let _env = p.env(&[
        ("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string()),
        ("CW_FAKE_LSP_FRAGMENT", "1"),
    ]);
    let m = LspManager::new();
    let cfg = cfg_ts(2000);
    let req = request(&p, "old\n", "new\n");

    let base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;
    let (items, removed, _) = feedback(&m.validate(&req, Some(&base), &cfg).await);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].code, "TS2");
    assert_eq!(removed, 0);
    m.shutdown_all().await;
}

/// ③ 服务端主动请求：握手不卡住，且 `workspace/configuration` / `registerCapability` 都被应答。
#[tokio::test]
async fn server_initiated_requests_are_answered() {
    let _guard = serial();
    let p = Project::new("x.ts");
    // 必须给**非空** payload：真 server 在 didOpen 后总会推一次诊断，而冷启动的**空集**不算
    // 「已就绪」证据（見 `client::Readiness::ready_for`）——空集基线要等 `Quiescent`（rust-analyzer）
    // 或本实例推过非空诊断（`Analyzed`）。本用例用的是 TS 假 server，故给一条非空诊断。
    let _env = p.env(&[
        ("CW_FAKE_LSP_SERVER_REQUESTS", "1"),
        (
            "CW_FAKE_LSP_DIAG",
            &json!([err(1, "X1", "pre-existing error")]).to_string(),
        ),
    ]);
    let m = LspManager::new();
    let cfg = cfg_ts(2000);
    let req = request(&p, "old\n", "new\n");

    // 拿到可信基线 == initialize/initialized 往返成功（握手没被服务端请求卡住）
    let _base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;
    let log = wait_log(&p, Duration::from_secs(5), |t| {
        t.contains("resp 1 ") && t.contains("resp 2 ")
    });
    assert!(
        log.contains("recv initialized"),
        "客户端必須发 initialized：{log}"
    );
    assert!(
        log.contains("resp 1 [{},{}]"),
        "workspace/configuration 必须回与 items 等长的数组：{log}"
    );
    assert!(
        log.contains("resp 2 null"),
        "client/registerCapability 必须回 null：{log}"
    );
    assert!(
        log.contains("resp 3 null") && log.contains("resp 4 null"),
        "{log}"
    );
    m.shutdown_all().await;
}

/// ④ 立即退出：解析仍成功（程序在），但 acquire 必须在重启一次后判 Unavailable；manager 映射为 NoServer。
#[tokio::test]
async fn exit_immediately_becomes_unavailable() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let _env = p.env(&[("CW_FAKE_LSP_MODE", "exit_immediately")]);
    let cfg = cfg_ts(500);
    let disc = Discoverer::new();
    let res = discovery::resolve(Lang::TypeScript, &cfg, &p.root, &disc);
    assert!(res.found, "假 server 存在，必须解析成功：{res:?}");
    assert_eq!(res.source, "config");

    let pool = LspPool::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut starting = 0usize;
    let mut unavailable: Option<String> = None;
    while Instant::now() < deadline {
        match pool
            .acquire(Lang::TypeScript, &p.root, &p.root, &res, &cfg)
            .await
        {
            Ok(_) => panic!("立刻退出的 server 不该被判为 Ready"),
            Err(AcquireError::Starting) => starting += 1,
            Err(AcquireError::Unavailable(msg)) => {
                unavailable = Some(msg);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(unavailable.is_some(), "必须在预算内判 Unavailable");
    assert!(!unavailable.unwrap().is_empty(), "Unavailable 必须带原因");
    assert!(
        starting >= 2,
        "应经历「首次 + 重启一次」两轮 Starting，实得 {starting}"
    );

    // manager 侧：最终判 NoServer（引导安装/修复），绝不渲染成「通过」
    let m = LspManager::new();
    let req = request(&p, "old\n", "new\n");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut final_reason: Option<SkipReason> = None;
    while Instant::now() < deadline {
        match m.validate(&req, None, &cfg).await {
            ValidationOutcome::Skipped {
                reason: SkipReason::ServerLoading,
                ..
            } => {}
            ValidationOutcome::Skipped { reason, .. } => {
                final_reason = Some(reason);
                break;
            }
            other => panic!("立刻退出的 server 不该给出 {other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    match final_reason {
        Some(SkipReason::NoServer { server }) => assert_eq!(server, "typescript-language-server"),
        other => panic!("期望 NoServer，实得 {other:?}"),
    }
    m.shutdown_all().await;
}

/// ④b 配置指向不存在的程序 → 解析不到 server（found=false + 详细原因）。
#[tokio::test]
async fn missing_configured_command_is_not_found() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let mut cfg = cfg_ts(500);
    cfg.commands.typescript = "definitely-not-a-real-lsp-xyz".into();
    let disc = Discoverer::new();
    let res = discovery::resolve(Lang::TypeScript, &cfg, &p.root, &disc);
    assert!(!res.found, "{res:?}");
    assert!(res.detail.contains("不存在"), "{}", res.detail);
    assert!(res.install.is_some(), "未找到必须带安装引导");

    let m = LspManager::new();
    let req = request(&p, "old\n", "new\n");
    match m.validate(&req, None, &cfg).await {
        ValidationOutcome::Skipped {
            reason: SkipReason::NoServer { server },
            ..
        } => assert_eq!(server, "typescript-language-server"),
        other => panic!("期望 NoServer，实得 {other:?}"),
    }
}

/// ⑤ 吐非协议字节：不 panic、不挂死，最终判不可用（或按未找到处理）。
#[tokio::test]
async fn garbage_output_does_not_hang() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let _env = p.env(&[("CW_FAKE_LSP_MODE", "garbage")]);
    let cfg = cfg_ts(500);
    let disc = Discoverer::new();
    let res = discovery::resolve(Lang::TypeScript, &cfg, &p.root, &disc);
    assert!(res.found);
    let pool = LspPool::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut unavailable = false;
    while Instant::now() < deadline {
        match pool
            .acquire(Lang::TypeScript, &p.root, &p.root, &res, &cfg)
            .await
        {
            Ok(_) => panic!("吐垃圾的 server 不该被判为 Ready"),
            Err(AcquireError::Starting) => {}
            Err(AcquireError::Unavailable(_)) => {
                unavailable = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(unavailable, "垃圾流必须在预算内被判不可用（而不是挂死）");

    let m = LspManager::new();
    let req = request(&p, "old\n", "new\n");
    let out = m.validate(&req, None, &cfg).await;
    assert!(
        matches!(out, ValidationOutcome::Skipped { .. }),
        "只能是 Skipped：{out:?}"
    );
    assert!(!out.ran());
    m.shutdown_all().await;
}

/// ⑥ 诊断晚于同步窗口：`validate` 判 ServerLoading，`validate_async` 最终补到结果。
#[tokio::test]
async fn late_diagnostics_are_recovered_async() {
    let _guard = serial();
    let p = Project::new("x.ts");
    // 基线必须**非空**：收紧后单次空集推送不构成「server 已就绪」证据（见「空集不得立信」）
    let seq = json!([
        [err(0, "TS9000", "pre-existing error")],
        [
            err(0, "TS9000", "pre-existing error"),
            err(5, "TS1000", "late error")
        ]
    ]);
    let _env = p.env(&[
        ("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string()),
        ("CW_FAKE_LSP_DELAY_MS_CHANGE", "2000"),
    ]);
    let m = LspManager::new();
    let cfg = cfg_ts(800);
    let req = request(&p, "old\n", "new\n");

    let base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;
    let out = m.validate(&req, Some(&base), &cfg).await;
    match &out {
        ValidationOutcome::Skipped {
            reason: SkipReason::ServerLoading,
            ..
        } => {}
        other => panic!("超窗必须判 ServerLoading，实得 {other:?}"),
    }

    let recovered = m
        .validate_async(req.clone(), Some(base), cfg.clone())
        .await
        .expect("异步补条必须拿到结果");
    let (items, _, _) = feedback(&recovered);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].code, "TS1000");
    m.shutdown_all().await;
}

/// ⑦ severity 混合：只回喂 error 级。
#[tokio::test]
async fn only_errors_are_fed_back() {
    let _guard = serial();
    let p = Project::new("x.ts");
    // 基线必须**非空**（单次空集不构成就绪证据）；旧错误保留到改后，才能断言 removed = 0
    let seq = json!([
        [err(9, "OLD1", "pre-existing error")],
        [
            err(9, "OLD1", "pre-existing error"),
            err(1, "E1", "a real error"),
            warn(2, "W1", "a warning"),
            warn(3, "W2", "another warning")
        ]
    ]);
    let _env = p.env(&[("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string())]);
    let m = LspManager::new();
    let cfg = cfg_ts(1500);
    let req = request(&p, "old\n", "new\n");
    let base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;
    let (items, removed, _) = feedback(&m.validate(&req, Some(&base), &cfg).await);
    assert_eq!(items.len(), 1, "warning 绝不回喂：{items:?}");
    assert_eq!(items[0].code, "E1");
    assert_eq!(removed, 0);
    m.shutdown_all().await;
}

/// ⑧ 指纹刹车：同一诊断第 3 次起不再回喂。
#[tokio::test]
async fn dedupe_brake_stops_third_feedback() {
    let _guard = serial();
    let p = Project::new("x.ts");
    // 基线非空（就绪证据），旧错误保留 → 三次差集都能看到 E1，制动只受 dedupe_limit 影响
    let seq = json!([
        [err(9, "OLD", "pre-existing error")],
        [
            err(9, "OLD", "pre-existing error"),
            err(1, "E1", "same error every time")
        ]
    ]);
    let _env = p.env(&[("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string())]);
    let m = LspManager::new();
    let cfg = cfg_ts(1500);
    let req = request(&p, "old\n", "new\n");
    let base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;

    let first = feedback(&m.validate(&req, Some(&base), &cfg).await).0;
    let second = feedback(&m.validate(&req, Some(&base), &cfg).await).0;
    let third = feedback(&m.validate(&req, Some(&base), &cfg).await).0;
    assert_eq!(first.len(), 1, "第 1 次应回喂");
    assert_eq!(second.len(), 1, "第 2 次应回喂（limit=2）");
    assert!(third.is_empty(), "第 3 次必须刹车：{third:?}");
    m.shutdown_all().await;
}

/// ⑨ 预算截断：`max_diagnostics` 生效且文案标注截断条数。
#[tokio::test]
async fn budget_truncates_and_marks() {
    let _guard = serial();
    let p = Project::new("x.ts");
    // 基线非空（就绪证据）；改后除旧错误外多出 3 条新错误 → 预算算的是「新增」
    let seq = json!([
        [err(9, "OLD", "pre-existing error")],
        [
            err(9, "OLD", "pre-existing error"),
            err(1, "E1", "first"),
            err(2, "E2", "second"),
            err(3, "E3", "third")
        ]
    ]);
    let _env = p.env(&[("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string())]);
    let m = LspManager::new();
    let mut cfg = cfg_ts(1500);
    cfg.max_diagnostics = 1;
    let req = request(&p, "old\n", "new\n");
    let base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;
    let outcome = m.validate(&req, Some(&base), &cfg).await;
    let (items, removed, truncated) = feedback(&outcome);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(truncated, 2, "3 条里只回喂 1 条");
    let text = codewave_lib::lsp::diagnostics::format_feedback(&items, removed, truncated, 4000);
    assert!(text.contains("…（已截断 2 条）"), "{text}");
    m.shutdown_all().await;
}

/// ⑩ `.vue` 等未覆盖类型 → Unsupported。
#[tokio::test]
async fn vue_file_is_unsupported() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let m = LspManager::new();
    let mut req = request(&p, "old\n", "new\n");
    req.path = p.root.join("src/App.vue");
    req.rel_path = "src/App.vue".into();
    let out = m.validate(&req, None, &cfg_ts(500)).await;
    match (&out, skip_reason(&out)) {
        (ValidationOutcome::Skipped { lang: None, .. }, SkipReason::Unsupported) => {}
        other => panic!("期望 Unsupported，实得 {other:?}"),
    }
    assert!(!out.ran());
}

/// ⑪ 语言开关关闭 → Disabled（不是「通过」，也不是「跑过了」）。
#[tokio::test]
async fn disabled_language_is_reported() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let m = LspManager::new();
    let mut v = ValidationSettings::default();
    v.typescript = false;
    m.set_settings(&v);
    let req = request(&p, "old\n", "new\n");
    let out = m.validate(&req, None, &cfg_ts(500)).await;
    match (&out, skip_reason(&out)) {
        (
            ValidationOutcome::Skipped {
                lang: Some(Lang::TypeScript),
                ..
            },
            SkipReason::Disabled,
        ) => {}
        other => panic!("期望 Disabled，实得 {other:?}"),
    }
    assert!(!out.ran());
}

/// ⑬ 缺 `Content-Length` 的帧：不挂死、不误判，最终判不可用且绝不渲染成「通过」。
#[tokio::test]
async fn missing_content_length_never_hangs() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let _env = p.env(&[("CW_FAKE_LSP_DIAG", "[]"), ("CW_FAKE_LSP_NO_LENGTH", "1")]);
    let m = LspManager::new();
    let cfg = cfg_ts(500);
    let req = request(&p, "old\n", "new\n");

    // 首帧（initialize 响应）正常，诊断帧缺长度 → 读循环报 BadHeader → 客户端置死；
    // 基线永远不可能变得可信，且这个循环必须在预算内结束（不挂死）
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut reliable_seen = false;
    while Instant::now() < deadline {
        if let Some(b) = m.baseline(&req, &cfg).await {
            if b.reliable() {
                reliable_seen = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(!reliable_seen, "缺 Content-Length 的流不可能给出可信基线");

    let out = m.validate(&req, None, &cfg).await;
    assert!(
        matches!(out, ValidationOutcome::Skipped { .. }),
        "只能是 Skipped：{out:?}"
    );
    assert!(!out.ran());
    // 日志里应留下「收到 didOpen」（证明协议前半段是通的，坏的是后续帧）
    let log = p.log_text();
    assert!(
        log.contains("recv textDocument/didOpen") || log.contains("recv initialize"),
        "{log}"
    );
    m.shutdown_all().await;
}

/// ⑫ 关闭后假 server 进程确实退出（不留孤儿），且 shutdown/exit 握手真的发生过。
#[tokio::test]
async fn shutdown_leaves_no_orphan_process() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let seq = json!([[], [err(1, "E1", "boom")]]);
    let _env = p.env(&[("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string())]);
    let m = LspManager::new();
    let cfg = cfg_ts(1500);
    let req = request(&p, "old\n", "new\n");
    let _base = reliable_baseline(&m, &req, &cfg, Duration::from_secs(20)).await;
    assert_eq!(m.server_count(), 1, "应有一个运行中的实例");

    let log = wait_log(&p, Duration::from_secs(5), |t| t.contains("pid "));
    let pid: u32 = log
        .lines()
        .find_map(|l| l.strip_prefix("pid "))
        .and_then(|v| v.trim().parse().ok())
        .expect("假 server 必须记录自身 pid");
    assert!(pid_alive(pid), "关闭前进程应活着（pid {pid}）");

    m.shutdown_all().await;
    assert_eq!(m.server_count(), 0, "关闭后池内不应有实例");
    let log = wait_log(&p, Duration::from_secs(5), |t| t.contains("bye"));
    assert!(
        log.contains("recv shutdown"),
        "必须先发 shutdown 请求：{log}"
    );
    assert!(
        log.contains("bye"),
        "必须发 exit 通知让 server 自行退出：{log}"
    );
    assert_pid_gone(pid, Duration::from_secs(10));
}

/// ⑭（🔴-2）冷启动「先推空集、随后才推真实诊断」：基线在占位空集上建立时**不得可信**，
/// 否则项目存量错误会在写后差集里全部变成「本次新增」被回喂（本需求要消灭的痛点）。
#[tokio::test]
async fn empty_first_publish_is_not_a_trusted_baseline() {
    let _guard = serial();
    let p = Project::new("x.ts");
    let seq = json!([[
        err(2, "TS2322", "pre-existing 1"),
        err(3, "TS9999", "pre-existing 2"),
        err(4, "TS1111", "pre-existing 3")
    ]]);
    let _env = p.env(&[
        ("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string()),
        // 占位空集先到，6s 后才是真诊断（远宽于基线同步窗口，避免真诊断抢先到达）
        ("CW_FAKE_LSP_EMPTY_FIRST_MS", "6000"),
    ]);
    let m = LspManager::new();
    let cfg = cfg_ts(1500);
    let req = request(&p, "const a: number = 1;\n", "const a: number = 'x';\n");

    // 基线在占位空集上建立（首次 acquire 会先经历 Starting，故轮询到句柄为止）：
    // server 尚无就绪证据 → 必须判不可信
    let deadline = Instant::now() + Duration::from_secs(5);
    let base = loop {
        if let Some(b) = m.baseline(&req, &cfg).await {
            break b;
        }
        assert!(Instant::now() < deadline, "server 未在预算内启动（拿不到基线句柄）");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        !base.reliable(),
        "占位空集不得立为可信基线（否则存量错误会被当成新增）"
    );
    // 假 server 确实演出的是「先空集、后真实」
    let log = wait_log(&p, Duration::from_secs(15), |t| {
        t.contains("push empty-first placeholder") && t.contains("push diagnostics round 0")
    });
    let empty_at = log
        .find("push empty-first placeholder")
        .unwrap_or_else(|| panic!("应推过占位空集：{log}"));
    let real_at = log
        .find("push diagnostics round 0")
        .unwrap_or_else(|| panic!("应推过真实诊断：{log}"));
    assert!(empty_at < real_at, "必须先空集后真实：{log}");

    // 不可信基线：不回喂任何诊断（包括那 3 条早已存在的错误），也绝不能渲染成「通过」
    let out = m.validate(&req, Some(&base), &cfg).await;
    assert!(!out.ran(), "不可信基线必须不回喂（Skipped）：{out:?}");
    match skip_reason(&out) {
        SkipReason::ServerNotReady => {}
        other => panic!("期望 ServerNotReady（预热中、本轮不回喂），实得 {other:?}"),
    }
    m.shutdown_all().await;
}

/// ⑮（🔴-2 回归）暖机后的正常路径不得被一起砍掉：server 已就绪时，
/// 「真干净的文件的空集基线」仍可信，写后差集照常只回喂新增错误。
#[tokio::test]
async fn warm_server_still_diffs_normally() {
    let _guard = serial();
    let p = Project::new("x.ts");
    // 轮 0：文件 A 的非空诊断 → 给出「确实在分析」的就绪证据
    // 轮 1：文件 B 的空集（真干净）→ 现在应当可信
    // 轮 2：文件 B 改后新增 1 条 → 必须回喂
    let seq = json!([
        [err(2, "A1", "existing error in A")],
        [],
        [err(9, "B1", "brand new error in B")]
    ]);
    let _env = p.env(&[("CW_FAKE_LSP_DIAG_SEQ", &seq.to_string())]);
    let m = LspManager::new();
    let cfg = cfg_ts(1500);

    let req_a = request(&p, "old A\n", "new A\n");
    let base_a = reliable_baseline(&m, &req_a, &cfg, Duration::from_secs(20)).await;
    assert!(base_a.reliable(), "非空基线本来就应可信（不能把正常路径也砍掉）");

    // 同一项目、同一语言根 → 复用同一个 server 实例（就绪证据是实例级的）
    let b = p.root.join("src/b.ts");
    std::fs::write(&b, "const b: number = 1;\n").unwrap();
    let req_b = ValidateRequest {
        project_id: Some("p-integration".into()),
        project_root: p.root.clone(),
        path: b.clone(),
        rel_path: "src/b.ts".into(),
        lang: Lang::TypeScript,
        new_content: Some("const b: number = 'x';\n".into()),
        prev_content: Some("const b: number = 1;\n".into()),
    };
    let base_b = m
        .baseline(&req_b, &cfg)
        .await
        .expect("server 已就绪，基线句柄应存在");
    assert!(
        base_b.reliable(),
        "暖机后（已有非空诊断证据）的空集基线仍必须可信"
    );

    let (items, removed, _) = feedback(&m.validate(&req_b, Some(&base_b), &cfg).await);
    assert_eq!(items.len(), 1, "只回喂新增的 1 条：{items:?}");
    assert_eq!(items[0].code, "B1");
    assert_eq!(items[0].line, 10, "0-based 行 9 → 1-based 10");
    assert_eq!(removed, 0);
    m.shutdown_all().await;
}

/// ⑯（🔴-1 返工，核心验收项）**连推空集、时间跨过预热窗**：证据档位会到 `Warm`，但它**不给空集基线
/// 背书**，第 3 次写入仍必须「未就绪」、绝不「通过」。
///
/// 旧实现的漏洞路径（本次要关闭的窗口）：`publish_count` 把空集占位也计数，`Warm` 只需「距 initialize
/// ≥3s 且累计 ≥2 次」——于是 t≈0.2s / 1.5s 两次空集判不可信，t≈3.5s 第三次空集就让 `Warm` 成立、
/// 把「全干净」当真，server 随后推出的**项目存量错误**全部变成「本次新增」回喂给模型。
///
/// 现在两道闸门叠加：① 预热窗按语言分档（TS ≥10s，不再是统一 3s）；② 空集基线只认强证据
/// （`Quiescent`/`Analyzed`），`Warm` 只给非空基线背书（见 `client::Readiness::ready_for`）。
/// 证据链的两半各有单测：本用例钉「端到端不回喂」，「三次空集 + 跨预热窗 ⇒ Warm」
/// 与「Warm 不给空集背书」由 `client.rs` 的 `warm_evidence_does_not_back_an_empty_baseline` 钉。
///
/// 文案：集成测试看不到私有 `tools` 模块，`outcome_text` 的措辞由
/// `tools/validation.rs` 的 `skipped_texts_are_explicit_and_never_claim_success` 钉死
/// （断言含「未就绪」且绝不含「校验通过」）；这里的「不回喂 + `ServerNotReady`」就是它的结构条件。
#[tokio::test]
async fn empty_only_stream_across_warmup_window_is_never_a_trusted_baseline() {
    let _guard = serial();
    let p = Project::new("x.ts");
    // 假 server 只推空集（永不推真实 payload）：全干净项目的占位流水线
    let _env = p.env(&[("CW_FAKE_LSP_ALWAYS_EMPTY", "1")]);
    let m = LspManager::new();
    let cfg = cfg_ts(500);
    let req = request(&p, "old\n", "new\n");
    let warmup = Duration::from_millis(server_spec::spec(Lang::TypeScript).warmup_ms);
    assert!(
        warmup >= Duration::from_secs(10),
        "TypeScript 的预热档位必须 ≥10s（统一的 3s 正是旧漏洞）：{warmup:?}"
    );

    let started = Instant::now();
    for round in 1..=3u32 {
        if round == 3 {
            // 等到确实跨过预热窗（多给 2s 余量：预热期从 server `initialize` 成功起算，
            // 而它是测试开始之后才完成的）。此後 publish_count = 3 ≥ 2 且 elapsed ≥ 档位
            // ⇒ `client::Readiness` 档位就是 `Warm`——正是旧实现会立信的时点。
            let deadline = warmup + Duration::from_secs(2);
            while started.elapsed() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let base = loop {
            if let Some(b) = m.baseline(&req, &cfg).await {
                break b;
            }
            assert!(
                started.elapsed() < warmup * 4,
                "server 未在预算内启动（拿不到基线句柄）"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(
            !base.reliable(),
            "第 {round} 轮：只推空集的实例不得给出可信基线"
        );
        let out = m.validate(&req, Some(&base), &cfg).await;
        assert!(
            !out.ran(),
            "第 {round} 轮：不可信基线必须不回喂（Skipped），绝不能渲染成「通过」：{out:?}"
        );
        assert_eq!(
            skip_reason(&out),
            &SkipReason::ServerNotReady,
            "第 {round} 轮：应为「未就绪」（不是「通过」，也不承诺补条）"
        );
    }

    // 假 server 确实演的是「连续空集」：三次 didOpen 各推一次空集，且从未推过真实 payload
    let log = p.log_text();
    assert_eq!(
        log.matches("push empty-set").count(),
        3,
        "应恰好三次空集占位（每轮 didOpen 一次）：{log}"
    );
    assert!(
        !log.contains("push diagnostics round"),
        "本用例里不得出现真实诊断推送：{log}"
    );
    assert!(
        started.elapsed() >= warmup,
        "必须确实跨过了预热窗，否则本用例是空跑：{log}"
    );
    m.shutdown_all().await;
}
