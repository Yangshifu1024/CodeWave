//! Opt-in native coverage of the desktop executable's private guardian entry.
use super::*;
use crate::core::coordinator::AcquireRequest;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

const ROLE: &str = "CODEWAVE_NATIVE_GUARDIAN_WORKER";
const ROOT: &str = "CODEWAVE_NATIVE_GUARDIAN_ROOT";
const SESSION: &str = "native-guardian-session";
const RUN: &str = "native-guardian-run";

fn request(root: &Path, run: &str) -> AcquireRequest {
    AcquireRequest {
        session_id: SESSION.into(),
        root_run_id: run.into(),
        resource: super::super::workspace::resource_key(root),
        attempt: 0,
        expected_generation: None,
    }
}

fn worker() {
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let executable = PathBuf::from(std::env::var_os("CODEWAVE_GUARDIAN_BIN").unwrap());
    let store = LeaseStore::new(&root.join(".codewave/coord"));
    let lease = store.acquire(&request(&root, RUN)).unwrap();
    let guardian = GuardianClient::launch_with_dependencies(&store, &lease, vec![], &executable)
        .expect("desktop binary must provide the private guardian entrypoint");
    let argv = vec![
        "-c".into(),
        "echo $$ > \"$1\"; sleep 120 & echo $! > \"$2\"; wait".into(),
        "guardian-native".into(),
        root.join("shell.pid").to_string_lossy().into_owned(),
        root.join("grandchild.pid").to_string_lossy().into_owned(),
    ];
    let (program, args) = guardian.invocation("/bin/sh", &argv).unwrap();
    let mut shell = Command::new(program)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    until(Duration::from_secs(8), || {
        assert!(
            shell.try_wait().unwrap().is_none(),
            "guarded command exited"
        );
        root.join("grandchild.pid").is_file()
    });
    std::fs::write(
        root.join("ready.json"),
        serde_json::to_vec(&guardian.data).unwrap(),
    )
    .unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native guardian deadline exceeded"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Fixture {
    root: tempfile::TempDir,
    host: Child,
    shell: Option<ProcessIdentity>,
}
impl Fixture {
    fn launch(test_name: &str) -> Self {
        let executable = PathBuf::from(
            std::env::var_os("CODEWAVE_GUARDIAN_BIN")
                .expect("set CODEWAVE_GUARDIAN_BIN to the freshly built desktop codewave binary"),
        );
        assert!(executable.is_file(), "desktop helper executable must exist");
        let root = tempfile::tempdir().unwrap();
        let host = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--ignored", "--nocapture"])
            .env(ROLE, "1")
            .env(ROOT, root.path())
            .env("CODEWAVE_GUARDIAN_BIN", executable)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(root.path().join("host.stdout")).unwrap())
            .stderr(std::fs::File::create(root.path().join("host.stderr")).unwrap())
            .spawn()
            .unwrap();
        let mut fixture = Self {
            root,
            host,
            shell: None,
        };
        let path = fixture.root.path().join("ready.json");
        until(Duration::from_secs(15), || {
            assert!(
                fixture.host.try_wait().unwrap().is_none(),
                "native worker exited before ready: {}",
                std::fs::read_to_string(fixture.root.path().join("host.stderr"))
                    .unwrap_or_default()
            );
            path.is_file()
        });
        let pid = fixture.pid("shell.pid");
        fixture.shell = Some(ProcessIdentity {
            pid,
            started: fingerprint(pid).expect("owned shell identity"),
        });
        fixture
    }
    fn state(&self) -> State {
        serde_json::from_slice(&std::fs::read(self.root.path().join("ready.json")).unwrap())
            .unwrap()
    }
    fn pid(&self, file: &str) -> u32 {
        std::fs::read_to_string(self.root.path().join(file))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    fn wait_subtree_gone(&self) {
        let shell = self.pid("shell.pid");
        let grandchild = self.pid("grandchild.pid");
        until(Duration::from_secs(12), || {
            process_exit_observed(shell) || !process_present(shell)
        });
        until(Duration::from_secs(12), || {
            process_exit_observed(grandchild) || !process_present(grandchild)
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.host.kill();
        let _ = self.host.wait();
        // Test-owned process group only; clean failures without touching unrelated processes.
        if let Some(shell) = &self.shell
            && shell.alive()
            && own_group(shell.pid)
        {
            unsafe {
                libc::kill(-(shell.pid as i32), libc::SIGKILL);
            }
        }
    }
}

#[test]
#[ignore = "Requires freshly built desktop codewave in CODEWAVE_GUARDIAN_BIN; real SIGKILL/process tree"]
fn desktop_guardian_reaps_host_subtree_and_releases_root() {
    if std::env::var_os(ROLE).is_some() {
        worker();
        return;
    }
    let mut fixture = Fixture::launch(
        "core::process_coordination::guardian::native_tests::desktop_guardian_reaps_host_subtree_and_releases_root",
    );
    let state = fixture.state();
    fixture.host.kill().unwrap();
    fixture.host.wait().unwrap();
    fixture.wait_subtree_gone();
    until(Duration::from_secs(12), || state.completed.is_file());
    assert!(cleanup_proven(
        fixture.root.path(),
        SESSION,
        RUN,
        state.parent.pid
    ));
    let store = LeaseStore::new(&state.coord_root);
    until(Duration::from_secs(3), || {
        store.read(&state.lease.resource).is_none()
    });
    let next = store
        .acquire(&request(fixture.root.path(), "replacement-run"))
        .unwrap();
    assert!(next.generation > state.lease.generation);
    store.release(&next).unwrap();
}

#[test]
#[ignore = "Requires freshly built desktop codewave in CODEWAVE_GUARDIAN_BIN; real SIGKILL/process tree"]
fn desktop_guardian_crash_reaps_subtree_but_keeps_root_busy() {
    if std::env::var_os(ROLE).is_some() {
        worker();
        return;
    }
    let fixture = Fixture::launch(
        "core::process_coordination::guardian::native_tests::desktop_guardian_crash_reaps_subtree_but_keeps_root_busy",
    );
    let state = fixture.state();
    let guardian = state.guardian_identity.as_ref().unwrap();
    assert!(guardian.alive());
    assert_eq!(unsafe { libc::kill(guardian.pid as i32, libc::SIGKILL) }, 0);
    fixture.wait_subtree_gone();
    assert!(
        !state.completed.exists(),
        "dead guardian cannot prove cleanup"
    );
    assert!(!cleanup_proven(
        fixture.root.path(),
        SESSION,
        RUN,
        state.parent.pid
    ));
    let store = LeaseStore::new(&state.coord_root);
    let error = store
        .acquire(&request(fixture.root.path(), "replacement-run"))
        .unwrap_err();
    assert_eq!(error.code, "resource_held");
    assert_eq!(
        store.read(&state.lease.resource).unwrap().lease.generation,
        state.lease.generation
    );
}
