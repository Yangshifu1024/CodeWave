//! An independent root guardian. Guarded commands authenticate and register their
//! process group before exec; a parent killed in the spawn/registration window
//! cannot leave an unregistered executable running.
use super::{LeaseStore, owner::io_err};
use crate::core::coordinator::{CoordinateError, Lease};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started: String,
}
impl ProcessIdentity {
    fn current() -> Result<Self, CoordinateError> {
        let pid = std::process::id();
        Ok(Self {
            pid,
            started: fingerprint(pid).ok_or_else(|| {
                CoordinateError::new(
                    "process_identity_unavailable",
                    "Cannot prove process start identity",
                )
            })?,
        })
    }
    fn alive(&self) -> bool {
        !process_exit_observed(self.pid)
            && process_present(self.pid)
            && fingerprint(self.pid).as_deref() == Some(self.started.as_str())
    }
    // A failed identity probe is not proof of exit. Only a different observed
    // creation identity or an OS-confirmed absent PID permits subtree cleanup.
    fn confirmed_gone(&self) -> bool {
        self.exit_proven_by(fingerprint(self.pid).as_deref())
    }
    fn exit_proven_by(&self, observed: Option<&str>) -> bool {
        if process_exit_observed(self.pid) || !process_present(self.pid) {
            return true;
        }
        match observed {
            Some(started) => started != self.started,
            None => !process_present(self.pid),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    schema: u32,
    parent: ProcessIdentity,
    lease: Lease,
    coord_root: PathBuf,
    token: String,
    address: Option<String>,
    completed: PathBuf,
    #[serde(default)]
    dependencies: Vec<GuardianDependency>,
    #[serde(default)]
    guardian_identity: Option<ProcessIdentity>,
}
#[derive(Clone, Serialize, Deserialize)]
struct GuardianDependency {
    proof: PathBuf,
    guardian: ProcessIdentity,
}
#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    action: String,
    process: Option<ProcessIdentity>,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    accepted: bool,
    reason: String,
}

pub struct GuardianClient {
    state: PathBuf,
    data: State,
    executable: PathBuf,
    child: std::sync::Mutex<std::process::Child>,
}
impl GuardianClient {
    pub fn launch(store: &LeaseStore, lease: &Lease) -> Result<Self, CoordinateError> {
        let dependencies = process_guards().lock().unwrap().clone();
        Self::launch_with_dependencies(
            store,
            lease,
            dependencies,
            &std::env::current_exe().map_err(io_err)?,
        )
    }
    pub fn launch_rootless(data_dir: &Path) -> Result<std::sync::Arc<Self>, CoordinateError> {
        Self::launch_rootless_with_executable(data_dir, &std::env::current_exe().map_err(io_err)?)
    }
    /// Trusted embedding host may explicitly supply the compatible helper binary.
    pub fn launch_rootless_with_executable(
        data_dir: &Path,
        executable: &Path,
    ) -> Result<std::sync::Arc<Self>, CoordinateError> {
        let store = LeaseStore::new(&data_dir.join("coord"));
        let run = uuid::Uuid::new_v4().to_string();
        let lease = store.acquire(&crate::core::coordinator::AcquireRequest {
            session_id: format!("process-{}", std::process::id()),
            root_run_id: run.clone(),
            resource: format!("process:{}:{run}", std::process::id()),
            attempt: 0,
            expected_generation: None,
        })?;
        match Self::launch_with_dependencies(&store, &lease, Vec::new(), executable) {
            Ok(client) => {
                if let Some(guardian) = client.data.guardian_identity.clone() {
                    process_guards().lock().unwrap().push(GuardianDependency {
                        proof: client.data.completed.clone(),
                        guardian,
                    });
                }
                Ok(std::sync::Arc::new(client))
            }
            Err(error) => {
                let _ = store.release(&lease);
                Err(error)
            }
        }
    }
    fn launch_with_dependencies(
        store: &LeaseStore,
        lease: &Lease,
        dependencies: Vec<GuardianDependency>,
        executable: &Path,
    ) -> Result<Self, CoordinateError> {
        let executable = std::fs::canonicalize(executable).map_err(io_err)?;
        let token = uuid::Uuid::new_v4().to_string();
        let private_dir =
            std::env::temp_dir().join(format!("codewave-guardian-{}", uuid::Uuid::new_v4()));
        let mut directory = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(&private_dir).map_err(io_err)?;
        let state = private_dir.join("control.json");
        let completed = store.coord_root().join("completed").join(format!(
            "{}.json",
            super::key_digest(&format!("{}:{}", lease.session_id, lease.root_run_id))
        ));
        let mut data = State {
            schema: 1,
            parent: ProcessIdentity::current()?,
            lease: lease.clone(),
            coord_root: store.coord_root().to_path_buf(),
            token,
            address: None,
            completed,
            dependencies,
            guardian_identity: None,
        };
        super::atomic_write(
            &state,
            &serde_json::to_vec(&data)
                .map_err(|e| CoordinateError::new("guardian_encode", e.to_string()))?,
        )
        .map_err(io_err)?;
        let mut child = std::process::Command::new(&executable)
            .arg("--codewave-guardian")
            .arg(&state)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(io_err)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(bytes) = std::fs::read(&state)
                && let Ok(ready) = serde_json::from_slice::<State>(&bytes)
                && ready.token == data.token
                && ready.address.is_some()
            {
                data = ready;
                break;
            }
            if child.try_wait().map_err(io_err)?.is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&state);
                let _ = std::fs::remove_dir(&private_dir);
                return Err(CoordinateError::new(
                    "guardian_unavailable",
                    "Executable does not provide the independent guardian entrypoint",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(Self {
            state,
            data,
            executable,
            child: std::sync::Mutex::new(child),
        })
    }
    pub fn invocation(
        &self,
        program: &str,
        argv: &[String],
    ) -> Result<(PathBuf, Vec<std::ffi::OsString>), CoordinateError> {
        if !self.data.parent.alive() {
            return Err(CoordinateError::new(
                "guardian_parent_changed",
                "Parent identity changed",
            ));
        }
        let mut args = vec![
            "--codewave-guarded-exec".into(),
            self.state.as_os_str().to_owned(),
            program.into(),
        ];
        args.extend(argv.iter().map(std::ffi::OsString::from));
        Ok((self.executable.clone(), args))
    }
    /// Read-only reconciliation after a deadline: only this exact generation's
    /// clean receipt can turn an earlier unknown result into a proven cleanup.
    pub fn cleanup_proven(&self) -> bool {
        let Ok(bytes) = std::fs::read(&self.data.completed) else {
            return false;
        };
        let Ok(proof) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return false;
        };
        proof["schema"] == 1
            && proof["clean"] == true
            && proof["generation"].as_u64() == Some(self.data.lease.generation)
            && proof["session"].as_str() == Some(self.data.lease.session_id.as_str())
            && proof["run"].as_str() == Some(self.data.lease.root_run_id.as_str())
            && proof["parent"]["pid"].as_u64() == Some(u64::from(self.data.parent.pid))
            && proof["parent"]["started"].as_str() == Some(self.data.parent.started.as_str())
    }
    pub fn finish(&self) -> Result<(), CoordinateError> {
        if self.cleanup_proven() {
            let _ = self.child.lock().unwrap().try_wait();
            return Ok(());
        }
        let reply = send(&self.data, "finish", None)?;
        if !reply.accepted {
            return Err(CoordinateError::new(
                "guardian_cleanup_pending",
                reply.reason,
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if self
                .child
                .lock()
                .unwrap()
                .try_wait()
                .map_err(io_err)?
                .is_some()
            {
                if self.cleanup_proven() {
                    return Ok(());
                }
                return Err(CoordinateError::new(
                    "guardian_cleanup_unknown",
                    "Guardian did not prove subtree cleanup",
                ));
            }
            if Instant::now() >= deadline {
                return Err(CoordinateError::new(
                    "guardian_cleanup_pending",
                    "Guardian shutdown deadline exceeded; lease retained",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn send(
    state: &State,
    action: &str,
    process: Option<ProcessIdentity>,
) -> Result<Reply, CoordinateError> {
    let mut result = send_once(state, action, process.clone());
    for _ in 0..2 {
        if action != "register"
            || !matches!(&result,Err(error) if error.code=="guardian_empty_reply")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        result = send_once(state, action, process.clone());
    }
    result
}
fn send_once(
    state: &State,
    action: &str,
    process: Option<ProcessIdentity>,
) -> Result<Reply, CoordinateError> {
    let addr = state
        .address
        .as_ref()
        .ok_or_else(|| CoordinateError::new("guardian_not_ready", "Guardian not ready"))?;
    let addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|_| CoordinateError::new("guardian_address", "Invalid guardian address"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(io_err)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .map_err(io_err)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(io_err)?;
    stream.set_nodelay(true).map_err(io_err)?;
    let mut bytes = serde_json::to_vec(&Request {
        token: state.token.clone(),
        action: action.into(),
        process,
    })
    .map_err(|e| CoordinateError::new("guardian_protocol", e.to_string()))?;
    bytes.push(b'\n');
    stream.write_all(&bytes).map_err(io_err)?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(16384)
        .read_line(&mut line)
        .map_err(io_err)?;
    if line.is_empty() {
        return Err(CoordinateError::new(
            "guardian_empty_reply",
            "Guardian closed registration channel before acknowledging",
        ));
    }
    serde_json::from_str(&line)
        .map_err(|e| CoordinateError::new("guardian_protocol", e.to_string()))
}

/// Call before desktop initialization; this private helper never loads credentials or UI.
pub fn guardian_entry() -> Option<i32> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mode = args.first()?.to_str()?;
    #[cfg(unix)]
    if mode == "--codewave-group-anchor" {
        let parent = args
            .get(1)
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<u32>().ok());
        let start = args.get(2).and_then(|value| value.to_str());
        let group = args
            .get(3)
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<i32>().ok());
        return Some(match (parent, start, group) {
            (Some(parent), Some(start), Some(group))
                if group > 1 && unsafe { libc::getpgrp() } == group =>
            {
                let parent = ProcessIdentity {
                    pid: parent,
                    started: start.into(),
                };
                loop {
                    if parent.confirmed_gone() {
                        unsafe {
                            libc::kill(-group, libc::SIGKILL);
                        }
                        return Some(4);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            _ => 4,
        });
    }
    if mode != "--codewave-guardian" && mode != "--codewave-guarded-exec" {
        return None;
    }
    let result = (|| -> Result<i32, CoordinateError> {
        let path = args
            .get(1)
            .ok_or_else(|| CoordinateError::new("guardian_args", "Missing state path"))?;
        let data: State = serde_json::from_slice(&std::fs::read(path).map_err(io_err)?)
            .map_err(|e| CoordinateError::new("guardian_state", e.to_string()))?;
        if data.schema != 1 {
            return Err(CoordinateError::new(
                "guardian_schema",
                "Unsupported guardian state",
            ));
        }
        if mode == "--codewave-guardian" {
            run(Path::new(path), data)?;
            return Ok(0);
        }
        let executable = args
            .get(2)
            .ok_or_else(|| CoordinateError::new("guardian_args", "Missing guarded executable"))?;
        let own = ProcessIdentity::current()?;
        let reply = send(&data, "register", Some(own))?;
        if !reply.accepted {
            return Err(CoordinateError::new("guardian_refused", reply.reason));
        }
        let mut command = std::process::Command::new(executable);
        command.args(&args[3..]);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            Err(io_err(command.exec()))
        }
        #[cfg(windows)]
        {
            return Ok(command.status().map_err(io_err)?.code().unwrap_or(1));
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(CoordinateError::new(
                "unsupported_guardian",
                "Unsupported process platform",
            ))
        }
    })();
    Some(match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("guardian: {error}");
            4
        }
    })
}
fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut bytes = serde_json::to_vec(reply).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)
}
fn run(state_path: &Path, mut state: State) -> Result<(), CoordinateError> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(io_err)?;
    listener.set_nonblocking(true).map_err(io_err)?;
    state.address = Some(listener.local_addr().map_err(io_err)?.to_string());
    state.guardian_identity = Some(ProcessIdentity::current()?);
    super::atomic_write(state_path, &serde_json::to_vec(&state).unwrap()).map_err(io_err)?;
    let mut groups: Vec<OwnedGroup> = Vec::new();
    loop {
        for group in &mut groups {
            if group.process.confirmed_gone() {
                group.terminate_if_owned();
            }
            group.reap_anchor();
        }
        groups.retain(|group| group_alive(group.process.pid));
        if state.parent.confirmed_gone() {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .map_err(io_err)?;
                let mut line = String::new();
                if BufReader::new(stream.try_clone().map_err(io_err)?)
                    .take(16384)
                    .read_line(&mut line)
                    .is_err()
                {
                    let _ = write_reply(
                        &mut stream,
                        &Reply {
                            accepted: false,
                            reason: "incomplete_registration_frame".into(),
                        },
                    );
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Request>(&line) else {
                    let _ = write_reply(
                        &mut stream,
                        &Reply {
                            accepted: false,
                            reason: "invalid_registration_frame".into(),
                        },
                    );
                    continue;
                };
                let mut finish = false;
                let reply = if request.token != state.token {
                    Reply {
                        accepted: false,
                        reason: "unauthorized".into(),
                    }
                } else if request.action == "finish" {
                    finish = true;
                    Reply {
                        accepted: true,
                        reason: "settling".into(),
                    }
                } else if request.action == "register" && state.parent.alive() {
                    if let Some(process) = request
                        .process
                        .filter(|process| process.alive() && own_group(process.pid))
                    {
                        if !groups.iter().any(|entry| {
                            entry.process.pid == process.pid
                                && entry.process.started == process.started
                        }) {
                            match OwnedGroup::register(process) {
                                Ok(group) => groups.push(group),
                                Err(error) => {
                                    let reply = Reply {
                                        accepted: false,
                                        reason: error.to_string(),
                                    };
                                    let _ = write_reply(&mut stream, &reply);
                                    continue;
                                }
                            }
                        }
                        Reply {
                            accepted: true,
                            reason: "registered_before_exec".into(),
                        }
                    } else {
                        Reply {
                            accepted: false,
                            reason: "process identity/group cannot be proven".into(),
                        }
                    }
                } else {
                    Reply {
                        accepted: false,
                        reason: "parent stopped".into(),
                    }
                };
                let _ = write_reply(&mut stream, &reply);
                if finish {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(io_err(error)),
        }
    }
    // A process group id cannot be reused while any member of that group remains.
    // Never signal a reused leader with a different process start identity.
    for group in &groups {
        group.terminate_if_owned();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while groups.iter().any(|group| group_alive(group.process.pid))
        || state.dependencies.iter().any(|dependency| {
            !clean_dependency(&dependency.proof)
                && (state.parent.confirmed_gone() || !dependency.guardian.alive())
        })
    {
        if Instant::now() >= deadline {
            return Err(CoordinateError::new(
                "guardian_children_busy",
                "Subtree exit unconfirmed; root lease retained",
            ));
        }
        for group in &mut groups {
            group.reap_anchor();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let proof = serde_json::json!({"schema":1,"session":state.lease.session_id,"run":state.lease.root_run_id,"generation":state.lease.generation,"parent":state.parent,"clean":true});
    super::atomic_write(&state.completed, &serde_json::to_vec(&proof).unwrap()).map_err(io_err)?;
    LeaseStore::new(&state.coord_root).release(&state.lease)?;
    let _ = std::fs::remove_file(state_path);
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}
#[cfg(unix)]
fn own_group(pid: u32) -> bool {
    unsafe { libc::getpgid(pid as i32) == pid as i32 }
}
#[cfg(windows)]
fn own_group(_pid: u32) -> bool {
    true
}
#[cfg(unix)]
fn kill_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}
#[cfg(windows)]
fn kill_group(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    unsafe {
        libc::kill(-(pid as i32), 0) == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}
#[cfg(windows)]
fn group_alive(pid: u32) -> bool {
    process_present(pid)
}
#[cfg(target_os = "macos")]
fn process_exit_observed(pid: u32) -> bool {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info);
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size as i32,
        )
    };
    // Darwin proc.h SZOMB=5: this is an observed exited process, not a failed probe.
    if got == size as i32 {
        return info.pbi_status == 5;
    }
    // Darwin libproc omits zombie entries (ESRCH) while kill(pid,0) still
    // succeeds. Query the trusted system process utility only in this rare
    // ambiguous case; every unavailable/error result remains Unknown.
    if !process_present(pid) {
        return false;
    }
    let mut child = match std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                let mut bytes = vec![];
                if child
                    .stdout
                    .take()
                    .is_some_and(|stdout| stdout.take(256).read_to_end(&mut bytes).is_ok())
                {
                    return String::from_utf8_lossy(&bytes)
                        .trim_start()
                        .starts_with('Z');
                }
                return false;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}
#[cfg(target_os = "linux")]
fn process_exit_observed(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, fields)| matches!(fields.split_whitespace().next(), Some("Z" | "X")))
        })
        == Some(true)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_exit_observed(_pid: u32) -> bool {
    false
}
#[cfg(unix)]
fn process_present(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
#[cfg(windows)]
fn process_present(pid: u32) -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    unsafe {
        let handle = OpenProcess(0x1000, 0, pid);
        if handle.is_null() {
            return GetLastError() != 87;
        }
        let mut code = 259;
        let got = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        got == 0 || code == 259
    }
}
#[cfg(not(any(unix, windows)))]
fn process_present(_pid: u32) -> bool {
    true
}
#[cfg(target_os = "macos")]
fn fingerprint(pid: u32) -> Option<String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info);
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size as i32,
        )
    };
    (got == size as i32).then(|| {
        format!(
            "{}:{}:{}",
            info.pbi_uid, info.pbi_start_tvsec, info.pbi_start_tvusec
        )
    })
}
#[cfg(target_os = "linux")]
fn fingerprint(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start = stat.rsplit_once(") ")?.1.split_whitespace().nth(19)?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(format!("{}:{start}", boot.trim()))
}
#[cfg(windows)]
fn fingerprint(pid: u32) -> Option<String> {
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetProcessTimes(
            process: *mut std::ffi::c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    unsafe {
        let handle = OpenProcess(0x1000, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut created = FileTime { low: 0, high: 0 };
        let mut exited = FileTime { low: 0, high: 0 };
        let mut kernel = FileTime { low: 0, high: 0 };
        let mut user = FileTime { low: 0, high: 0 };
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        (ok != 0).then(|| format!("{}:{}", created.high, created.low))
    }
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn fingerprint(_pid: u32) -> Option<String> {
    None
}

/// Recovery consumes a guardian proof, never PID absence alone. A PID reused by
/// another process cannot match the recorded creation identity.
pub fn cleanup_proven(root: &Path, session: &str, run: &str, owner_pid: u32) -> bool {
    let path = root.join(".codewave/coord/completed").join(format!(
        "{}.json",
        super::key_digest(&format!("{session}:{run}"))
    ));
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(proof) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let Ok(parent) = serde_json::from_value::<ProcessIdentity>(proof["parent"].clone()) else {
        return false;
    };
    proof["schema"] == 1
        && proof["clean"] == true
        && proof["session"].as_str() == Some(session)
        && proof["run"].as_str() == Some(run)
        && parent.pid == owner_pid
        && parent.confirmed_gone()
}

fn process_guards() -> &'static std::sync::Mutex<Vec<GuardianDependency>> {
    static GUARDS: std::sync::OnceLock<std::sync::Mutex<Vec<GuardianDependency>>> =
        std::sync::OnceLock::new();
    GUARDS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}
fn clean_dependency(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|proof| proof["clean"] == true)
}
impl Drop for GuardianClient {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

struct OwnedGroup {
    process: ProcessIdentity,
    #[cfg(unix)]
    anchor: ProcessIdentity,
    #[cfg(unix)]
    anchor_child: std::process::Child,
}
impl OwnedGroup {
    fn register(process: ProcessIdentity) -> Result<Self, CoordinateError> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let parent = ProcessIdentity::current()?;
            let mut command = std::process::Command::new(std::env::current_exe().map_err(io_err)?);
            command
                .args([
                    "--codewave-group-anchor",
                    &parent.pid.to_string(),
                    &parent.started,
                    &process.pid.to_string(),
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let group = process.pid as i32;
            unsafe {
                command.pre_exec(move || {
                    if libc::setpgid(0, group) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
            let mut child = command.spawn().map_err(io_err)?;
            let pid = child.id();
            let Some(started) = fingerprint(pid) else {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CoordinateError::new(
                    "anchor_identity",
                    "Cannot prove guardian group anchor",
                ));
            };
            if unsafe { libc::getpgid(pid as i32) } != group {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CoordinateError::new(
                    "anchor_group",
                    "Guardian anchor failed to join owned group",
                ));
            }
            Ok(Self {
                process,
                anchor: ProcessIdentity { pid, started },
                anchor_child: child,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self { process })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(CoordinateError::new(
                "guardian_platform",
                "Unsupported process groups",
            ))
        }
    }
    fn terminate_if_owned(&self) {
        #[cfg(unix)]
        if self.anchor.alive()
            && unsafe { libc::getpgid(self.anchor.pid as i32) } == self.process.pid as i32
        {
            kill_group(self.process.pid);
        }
        #[cfg(windows)]
        if self.process.alive() {
            kill_group(self.process.pid);
        }
    }
    fn reap_anchor(&mut self) {
        #[cfg(unix)]
        {
            let _ = self.anchor_child.try_wait();
        }
    }
}

#[cfg(all(test, unix))]
#[test]
fn incompatible_embedding_helper_is_rejected_without_retaining_a_process_lease() {
    let dir = tempfile::tempdir().unwrap();
    let error =
        GuardianClient::launch_rootless_with_executable(dir.path(), Path::new("/usr/bin/true"))
            .err()
            .expect("unsupported helper must fail closed");
    assert_eq!(error.code, "guardian_unavailable");
    let leases = dir.path().join("coord/leases");
    assert_eq!(
        std::fs::read_dir(leases)
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        0
    );
}

#[cfg(all(test, unix))]
#[test]
fn cleanup_receipt_must_match_the_exact_lease_and_parent_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = LeaseStore::new(dir.path());
    let lease = store
        .acquire(&crate::core::coordinator::AcquireRequest {
            session_id: "session".into(),
            root_run_id: "run".into(),
            resource: "root".into(),
            attempt: 0,
            expected_generation: None,
        })
        .unwrap();
    let data = State {
        schema: 1,
        parent: ProcessIdentity::current().unwrap(),
        lease: lease.clone(),
        coord_root: dir.path().into(),
        token: "private".into(),
        address: None,
        completed: dir.path().join("completed.json"),
        dependencies: vec![],
        guardian_identity: None,
    };
    let child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let client = GuardianClient {
        state: dir.path().join("private"),
        data,
        executable: "/usr/bin/true".into(),
        child: std::sync::Mutex::new(child),
    };
    let mut proof = serde_json::json!({"schema":1,"clean":true,"session":"session","run":"run","generation":lease.generation,"parent":client.data.parent});
    let write = |proof: &serde_json::Value| {
        std::fs::write(&client.data.completed, serde_json::to_vec(proof).unwrap()).unwrap()
    };
    proof["generation"] = serde_json::json!(lease.generation + 1);
    write(&proof);
    assert!(!client.cleanup_proven());
    proof["generation"] = serde_json::json!(lease.generation);
    proof["parent"]["started"] = serde_json::json!("reused-pid");
    write(&proof);
    assert!(!client.cleanup_proven());
    proof["parent"] = serde_json::to_value(&client.data.parent).unwrap();
    write(&proof);
    assert!(client.cleanup_proven());
    assert!(client.finish().is_ok());
    client.child.lock().unwrap().wait().unwrap();
}

#[cfg(test)]
#[test]
fn unavailable_fingerprint_of_a_present_parent_never_proves_exit() {
    let own = ProcessIdentity::current().unwrap();
    assert!(!own.exit_proven_by(None));
    assert!(!own.exit_proven_by(Some(&own.started)));
    assert!(own.exit_proven_by(Some("a-different-process-creation-identity")));
}
#[cfg(test)]
#[test]
fn registration_retries_only_an_unacknowledged_control_frame_before_exec() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            assert_eq!(request.action, "register");
            if attempt == 1 {
                write_reply(
                    &mut stream,
                    &Reply {
                        accepted: true,
                        reason: "same_registration_acknowledged".into(),
                    },
                )
                .unwrap();
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let store = LeaseStore::new(dir.path());
    let lease = store
        .acquire(&crate::core::coordinator::AcquireRequest {
            session_id: "test-session".into(),
            root_run_id: "test-run".into(),
            resource: "root".into(),
            attempt: 0,
            expected_generation: None,
        })
        .unwrap();
    let state = State {
        schema: 1,
        parent: ProcessIdentity::current().unwrap(),
        lease,
        coord_root: dir.path().into(),
        token: "test-private".into(),
        address: Some(address),
        completed: dir.path().join("proof"),
        dependencies: vec![],
        guardian_identity: None,
    };
    assert!(send(&state, "register", None).unwrap().accepted);
    server.join().unwrap();
}

#[cfg(all(test, unix))]
#[test]
fn an_os_observed_zombie_is_exited_even_before_its_host_reaps_the_child() {
    let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !process_exit_observed(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        process_exit_observed(pid),
        "owned child must reach an observable zombie state"
    );
    let identity = ProcessIdentity {
        pid,
        started: fingerprint(pid).unwrap_or_else(|| "unavailable-zombie-fingerprint".into()),
    };
    assert!(!identity.alive());
    assert!(identity.confirmed_gone());
    child.wait().unwrap();
}

#[cfg(all(test, unix))]
#[path = "guardian_native_tests.rs"]
mod native_tests;
