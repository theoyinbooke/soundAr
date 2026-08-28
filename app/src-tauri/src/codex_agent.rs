mod video_tools;

use super::{prepare_music_generation_request, RuntimeState};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::{CStr, OsString},
    fs,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::{
        ffi::OsStringExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::{AsRawFd, FromRawFd},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

pub(crate) use video_tools::{
    VideoAgentDispatcher, VideoAgentOperation, VideoAgentOperationKind, VideoAgentResult,
    VideoAgentResultStatus, VideoAgentToolError, VideoDispatchCallback,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_NAME: &str = "soundAr";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EnrolledCodexBrokerIdentity {
    schema_version: u32,
    canonical_path: PathBuf,
    device: String,
    inode: String,
    size_bytes: u64,
    sha256: String,
    version: String,
    codex_home: PathBuf,
    user_home: PathBuf,
    enrolled_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrokerFileSnapshot {
    device: u64,
    inode: u64,
    size_bytes: u64,
    sha256: String,
}

pub struct CodexAgentState {
    session: Mutex<Option<Arc<CodexSession>>>,
    video_dispatcher: VideoAgentDispatcher,
}

struct CodexSession {
    executable: PathBuf,
    version: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    thread_access: Mutex<HashMap<String, AgentAccess>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentAccess {
    ReadOnly,
    Studio,
    Full,
}

impl AgentAccess {
    pub fn from_value(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("workspace-write") => Self::Studio,
            Some("danger-full-access") => Self::Full,
            _ => Self::ReadOnly,
        }
    }

    fn permits_studio_writes(self) -> bool {
        matches!(self, Self::Studio | Self::Full)
    }
}

impl CodexAgentState {
    /// Connects Codex dynamic tools to the exact dispatcher used by the native Video Studio
    /// commands. Keeping this a function pointer prevents either transport from owning workflow
    /// state or creating a second renderer.
    pub(crate) fn with_video_dispatcher(callback: VideoDispatchCallback) -> Self {
        Self {
            session: Mutex::new(None),
            video_dispatcher: VideoAgentDispatcher::new(callback),
        }
    }

    pub fn status(&self) -> Value {
        let running = self.session.lock().ok().and_then(|guard| guard.clone());
        if let Some(session) = running {
            let alive = session
                .child
                .lock()
                .map(|mut child| child.try_wait().ok().flatten().is_none())
                .unwrap_or(false);
            return json!({
                "available": true,
                "connected": alive,
                "path": session.executable,
                "version": session.version,
                "studio_root": super::home_dir().join(".soundAr"),
            });
        }
        match discover_codex() {
            Some((path, version)) => {
                json!({ "available": true, "connected": false, "path": path, "version": version, "studio_root": super::home_dir().join(".soundAr") })
            }
            None => json!({
                "available": false,
                "connected": false,
                "message": "Codex CLI was not found. Install Codex separately, then ask soundAr to scan again."
            }),
        }
    }

    pub fn connect(&self, app: AppHandle, runtime: RuntimeState) -> Result<Value, String> {
        {
            let mut current = self
                .session
                .lock()
                .map_err(|_| "Codex session lock failed")?;
            if let Some(session) = current.clone() {
                let alive = session
                    .child
                    .lock()
                    .map(|mut child| child.try_wait().ok().flatten().is_none())
                    .unwrap_or(false);
                if alive {
                    let headless_broker = enroll_connected_codex_broker(&session)
                        .map(|identity| {
                            json!({
                                "ready": true,
                                "path": identity.canonical_path,
                                "sha256": identity.sha256,
                            })
                        })
                        .unwrap_or_else(|message| json!({"ready": false, "message": message}));
                    return Ok(
                        json!({ "connected": true, "path": session.executable, "version": session.version, "headless_generation_broker": headless_broker }),
                    );
                }
                *current = None;
            }
        }
        let (executable, version) = discover_codex().ok_or(
            "Codex CLI could not be detected in PATH or common Linux package-manager locations.",
        )?;
        let mut child = Command::new(&executable)
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not start Codex app-server: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("Codex app-server stdin was unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Codex app-server stdout was unavailable")?;
        let session = Arc::new(CodexSession {
            executable: executable.clone(),
            version: version.clone(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            thread_access: Mutex::new(HashMap::new()),
        });
        *self
            .session
            .lock()
            .map_err(|_| "Codex session lock failed")? = Some(session.clone());
        spawn_reader(session.clone(), stdout, app, runtime, self.video_dispatcher);
        let initialized = session
            .request("initialize", json!({
                "clientInfo": { "name": CLIENT_NAME, "title": "soundAr Studio Assistant", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true, "requestAttestation": false }
            }))
            .and_then(|_| session.notify("initialized", Value::Null));
        if let Err(error) = initialized {
            session.child.lock().ok().map(|mut child| child.kill().ok());
            self.session.lock().ok().map(|mut current| current.take());
            return Err(error);
        }
        let headless_broker = enroll_connected_codex_broker(&session)
            .map(|identity| {
                json!({
                    "ready": true,
                    "path": identity.canonical_path,
                    "sha256": identity.sha256,
                })
            })
            .unwrap_or_else(|message| json!({"ready": false, "message": message}));
        Ok(
            json!({ "connected": true, "path": executable, "version": version, "headless_generation_broker": headless_broker }),
        )
    }

    pub fn disconnect(&self) -> Result<bool, String> {
        let session = self
            .session
            .lock()
            .map_err(|_| "Codex session lock failed")?
            .take();
        if let Some(session) = session {
            session
                .child
                .lock()
                .map_err(|_| "Codex process lock failed")?
                .kill()
                .ok();
            return Ok(true);
        }
        Ok(false)
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        const ALLOWED: &[&str] = &[
            "account/read",
            "account/login/start",
            "account/login/cancel",
            "account/logout",
            "model/list",
            "thread/list",
            "thread/start",
            "thread/resume",
            "thread/read",
            "turn/start",
            "turn/steer",
            "turn/interrupt",
        ];
        if !ALLOWED.contains(&method) {
            return Err(format!("Unsupported Codex operation: {method}"));
        }
        let session = self
            .session
            .lock()
            .map_err(|_| "Codex session lock failed")?
            .clone()
            .ok_or("Codex is not connected")?;
        session.request(method, params)
    }

    pub fn respond(&self, id: u64, result: Value) -> Result<(), String> {
        let session = self
            .session
            .lock()
            .map_err(|_| "Codex session lock failed")?
            .clone()
            .ok_or("Codex is not connected")?;
        session.write(json!({ "id": id, "result": result }))
    }

    pub fn set_thread_access(&self, thread_id: &str, access: AgentAccess) -> Result<(), String> {
        let session = self
            .session
            .lock()
            .map_err(|_| "Codex session lock failed")?
            .clone()
            .ok_or("Codex is not connected")?;
        session
            .thread_access
            .lock()
            .map_err(|_| "Codex access lock failed")?
            .insert(thread_id.to_string(), access);
        Ok(())
    }

    /// Returns true only after this connected app-server session successfully started, resumed,
    /// or used the exact thread. Native recovery commands use this to avoid exposing saved
    /// conversation relationships through an arbitrary caller-supplied thread id.
    pub fn has_registered_thread(&self, thread_id: &str) -> Result<bool, String> {
        let session = self
            .session
            .lock()
            .map_err(|_| "Codex session lock failed")?
            .clone()
            .ok_or("Codex is not connected")?;
        let registered = session
            .thread_access
            .lock()
            .map_err(|_| "Codex access lock failed")?
            .contains_key(thread_id);
        Ok(registered)
    }
}

impl CodexSession {
    fn write(&self, value: Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().map_err(|_| "Codex input lock failed")?;
        serde_json::to_writer(&mut *stdin, &value)
            .map_err(|error| format!("Could not encode Codex request: {error}"))?;
        stdin
            .write_all(b"\n")
            .and_then(|_| stdin.flush())
            .map_err(|error| format!("Could not send Codex request: {error}"))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        if params.is_null() {
            self.write(json!({ "method": method }))
        } else {
            self.write(json!({ "method": method, "params": params }))
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| "Codex pending request lock failed")?
            .insert(id, sender);
        if let Err(error) = self.write(json!({ "id": id, "method": method, "params": params })) {
            self.pending
                .lock()
                .ok()
                .map(|mut pending| pending.remove(&id));
            return Err(error);
        }
        match receiver.recv_timeout(REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.pending
                    .lock()
                    .ok()
                    .map(|mut pending| pending.remove(&id));
                Err(format!("Codex did not answer {method} within 30 seconds"))
            }
        }
    }
}

fn account_home_dir() -> Result<PathBuf, String> {
    let effective_uid = unsafe { libc::geteuid() };
    let suggested = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut buffer = vec![0_u8; usize::try_from(suggested).unwrap_or(16_384).max(16_384)];
    let mut record = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            effective_uid,
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return Err("Could not resolve the operating-system account home directory".into());
    }
    let record = unsafe { record.assume_init() };
    if record.pw_dir.is_null() {
        return Err("The operating-system account has no home directory".into());
    }
    let bytes = unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes().to_vec();
    let path = PathBuf::from(OsString::from_vec(bytes));
    if !path.is_absolute() {
        return Err("The operating-system account home directory is not absolute".into());
    }
    Ok(path)
}

fn trusted_broker_identity_path() -> Result<PathBuf, String> {
    Ok(account_home_dir()?
        .join(".config/soundar")
        .join("trusted-codex-generation-broker-v1.json"))
}

fn ensure_private_broker_directory(path: &Path) -> Result<(), String> {
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path).map_err(|error| {
            format!(
                "Could not create the trusted Codex broker directory {}: {error}",
                path.display()
            )
        })?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "Could not inspect the trusted Codex broker directory {}: {error}",
            path.display()
        )
    })?;
    let effective_uid = unsafe { libc::geteuid() };
    if fs::canonicalize(path).ok().as_deref() != Some(path)
        || metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid
    {
        return Err("The trusted Codex broker directory is not a private owned directory".into());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "Could not secure the trusted Codex broker directory {}: {error}",
            path.display()
        )
    })
}

fn snapshot_broker_file(file: &mut File) -> Result<BrokerFileSnapshot, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect the Codex broker executable: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 512 * 1024 * 1024 {
        return Err("The Codex broker executable is not a bounded regular file".into());
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err("The Codex broker executable is not executable".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("Could not rewind the Codex broker executable: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not hash the Codex broker executable: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("Could not reset the Codex broker executable: {error}"))?;
    Ok(BrokerFileSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        size_bytes: metadata.len(),
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn process_tree(root_pid: u32) -> Vec<(u32, usize)> {
    let mut pending = vec![(root_pid, 0_usize)];
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    while let Some((pid, depth)) = pending.pop() {
        if depth > 8 || !seen.insert(pid) {
            continue;
        }
        result.push((pid, depth));
        let children_path = format!("/proc/{pid}/task/{pid}/children");
        if let Ok(children) = fs::read_to_string(children_path) {
            pending.extend(
                children
                    .split_whitespace()
                    .filter_map(|value| value.parse::<u32>().ok())
                    .map(|child| (child, depth + 1)),
            );
        }
    }
    result.sort_by_key(|(_, depth)| std::cmp::Reverse(*depth));
    result
}

fn process_runs_app_server(pid: u32) -> bool {
    fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .is_some_and(|cmdline| {
            let arguments = cmdline
                .split(|byte| *byte == 0)
                .filter(|argument| !argument.is_empty())
                .collect::<Vec<_>>();
            arguments.get(1).copied() == Some(b"app-server".as_slice())
        })
}

fn trusted_default_codex_home(account_home: &Path) -> Result<PathBuf, String> {
    let canonical_account_home = fs::canonicalize(account_home).map_err(|error| {
        format!(
            "Could not resolve the operating-system account home directory {}: {error}",
            account_home.display()
        )
    })?;
    let account_metadata = fs::symlink_metadata(account_home).map_err(|error| {
        format!(
            "Could not inspect the operating-system account home directory {}: {error}",
            account_home.display()
        )
    })?;
    let effective_uid = unsafe { libc::geteuid() };
    if canonical_account_home != account_home
        || account_metadata.file_type().is_symlink()
        || !account_metadata.is_dir()
        || account_metadata.uid() != effective_uid
        || account_metadata.permissions().mode() & 0o002 != 0
    {
        return Err(
            "The operating-system account home directory is not eligible for trusted Codex history"
                .into(),
        );
    }

    let default_home = account_home.join(".codex");
    let metadata = fs::symlink_metadata(&default_home).map_err(|error| {
        format!(
            "Could not inspect the trusted default Codex data directory {}: {error}",
            default_home.display()
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o002 != 0
    {
        return Err(
            "The trusted default Codex data directory is not an eligible history store".into(),
        );
    }
    let canonical = fs::canonicalize(&default_home).map_err(|error| {
        format!(
            "Could not resolve the trusted default Codex data directory {}: {error}",
            default_home.display()
        )
    })?;
    if canonical != default_home {
        return Err(
            "The trusted default Codex data directory cannot traverse symbolic links".into(),
        );
    }
    Ok(canonical)
}

fn codex_home_for_enrollment(account_home: &Path) -> Result<PathBuf, String> {
    let canonical = trusted_default_codex_home(account_home)?;
    if let Some(requested) = env::var_os("CODEX_HOME") {
        let requested = fs::canonicalize(PathBuf::from(requested)).map_err(|error| {
            format!("Could not resolve the connected custom Codex data directory: {error}")
        })?;
        if requested != canonical {
            return Err(
                "Custom CODEX_HOME locations cannot be enrolled for headless generation verification"
                    .into(),
            );
        }
    }
    Ok(canonical)
}

fn connected_codex_broker_identity(
    session: &CodexSession,
) -> Result<EnrolledCodexBrokerIdentity, String> {
    let root_pid = session
        .child
        .lock()
        .map_err(|_| "Codex process lock failed")?
        .id();
    let account_home = account_home_dir()?;
    let codex_home = codex_home_for_enrollment(&account_home)?;
    for (pid, _) in process_tree(root_pid) {
        if !process_runs_app_server(pid) {
            continue;
        }
        let proc_executable = PathBuf::from(format!("/proc/{pid}/exe"));
        let canonical_path = match fs::read_link(&proc_executable)
            .ok()
            .and_then(|path| fs::canonicalize(path).ok())
        {
            Some(path) => path,
            None => continue,
        };
        let mut running_file = match File::open(&proc_executable) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let mut magic = [0_u8; 4];
        if running_file.read_exact(&mut magic).is_err() || magic != *b"\x7fELF" {
            continue;
        }
        running_file.seek(SeekFrom::Start(0)).ok();
        let running = snapshot_broker_file(&mut running_file)?;
        let mut installed_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&canonical_path)
            .map_err(|error| {
                format!(
                    "Could not open the connected Codex executable {}: {error}",
                    canonical_path.display()
                )
            })?;
        let installed = snapshot_broker_file(&mut installed_file)?;
        if running != installed {
            continue;
        }
        return Ok(EnrolledCodexBrokerIdentity {
            schema_version: 1,
            canonical_path,
            device: installed.device.to_string(),
            inode: installed.inode.to_string(),
            size_bytes: installed.size_bytes,
            sha256: installed.sha256,
            version: session.version.clone(),
            codex_home,
            user_home: account_home,
            enrolled_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        });
    }
    Err("The connected Codex app-server executable could not be enrolled for headless generation verification".into())
}

fn persist_codex_broker_identity_at(
    path: &Path,
    identity: &EnrolledCodexBrokerIdentity,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("The trusted Codex broker identity path has no parent")?;
    ensure_private_broker_directory(parent)?;
    let staging = parent.join(format!(
        ".trusted-codex-generation-broker-{}.partial",
        Uuid::new_v4().simple()
    ));
    let bytes = serde_json::to_vec(identity)
        .map_err(|error| format!("Could not encode the trusted Codex broker identity: {error}"))?;
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&staging)
            .map_err(|error| format!("Could not stage the Codex broker identity: {error}"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("Could not persist the Codex broker identity: {error}"))?;
        fs::rename(&staging, path)
            .map_err(|error| format!("Could not publish the Codex broker identity: {error}"))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!("Could not commit the Codex broker identity directory: {error}")
            })
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    write_result
}

fn enroll_connected_codex_broker(
    session: &CodexSession,
) -> Result<EnrolledCodexBrokerIdentity, String> {
    let identity = connected_codex_broker_identity(session)?;
    persist_codex_broker_identity_at(&trusted_broker_identity_path()?, &identity)?;
    Ok(identity)
}

#[cfg(test)]
pub(crate) fn account_home_dir_for_test() -> PathBuf {
    account_home_dir().expect("test account home")
}

#[cfg(test)]
pub(crate) fn enroll_test_codex_broker_at(
    identity_path: &Path,
    executable: &Path,
    codex_home: &Path,
) -> Result<(), String> {
    let account_home = account_home_dir()?;
    let canonical_path = fs::canonicalize(executable)
        .map_err(|error| format!("Could not canonicalize the test broker: {error}"))?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical_path)
        .map_err(|error| format!("Could not open the test broker: {error}"))?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)
        .map_err(|error| format!("Could not inspect the test broker: {error}"))?;
    if magic != *b"\x7fELF" {
        return Err("The test broker must be an ELF executable".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("Could not rewind the test broker: {error}"))?;
    let snapshot = snapshot_broker_file(&mut file)?;
    let canonical_codex_home = fs::canonicalize(codex_home)
        .map_err(|error| format!("Could not canonicalize the test Codex home: {error}"))?;
    let identity = EnrolledCodexBrokerIdentity {
        schema_version: 1,
        canonical_path,
        device: snapshot.device.to_string(),
        inode: snapshot.inode.to_string(),
        size_bytes: snapshot.size_bytes,
        sha256: snapshot.sha256,
        version: "codex-cli trusted-test".to_string(),
        codex_home: canonical_codex_home,
        user_home: account_home,
        enrolled_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    persist_codex_broker_identity_at(identity_path, &identity)
}

fn read_codex_broker_identity_at(
    path: &Path,
) -> Result<EnrolledCodexBrokerIdentity, VideoAgentToolError> {
    let parent = path.parent().ok_or_else(|| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The enrolled Codex broker identity path has no parent",
        )
    })?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|_| {
        VideoAgentToolError::new(
            "video.generation_broker_setup_required",
            "Open soundAr and connect Codex once before registering generated visuals headlessly",
        )
    })?;
    let effective_uid = unsafe { libc::geteuid() };
    if fs::canonicalize(parent).ok().as_deref() != Some(parent)
        || parent_metadata.file_type().is_symlink()
        || !parent_metadata.is_dir()
        || parent_metadata.uid() != effective_uid
        || parent_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The enrolled Codex broker identity directory is not private and account-owned",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| {
            VideoAgentToolError::new(
                "video.generation_broker_setup_required",
                "Open soundAr and connect Codex once before registering generated visuals headlessly",
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            format!("Could not inspect the enrolled Codex broker identity: {error}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() == 0
        || metadata.len() > 64 * 1024
    {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The enrolled Codex broker identity is not a private owned regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes).map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            format!("Could not read the enrolled Codex broker identity: {error}"),
        )
    })?;
    serde_json::from_slice::<EnrolledCodexBrokerIdentity>(&bytes).map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            format!("The enrolled Codex broker identity is invalid: {error}"),
        )
    })
}

fn codex_history_home_is_eligible(account_home: &Path, canonical_home: &Path) -> bool {
    if trusted_default_codex_home(account_home).ok().as_deref() == Some(canonical_home) {
        return true;
    }
    #[cfg(test)]
    {
        let test_root = account_home.join(".config/soundar");
        if canonical_home.starts_with(&test_root)
            && canonical_home.components().any(|component| {
                component
                    .as_os_str()
                    .to_string_lossy()
                    .starts_with("headless-broker-test-")
            })
        {
            return true;
        }
    }
    false
}

fn trusted_image_generation_from_thread(
    response: &Value,
    expected_thread_id: &str,
    generation_id: &str,
    producer_version: Option<String>,
) -> Result<super::video::TrustedGeneratedVisual, VideoAgentToolError> {
    let thread = response.get("thread").ok_or_else(|| {
        VideoAgentToolError::new(
            "video.generation_not_registered",
            "Codex did not return the authenticated generation thread",
        )
    })?;
    if thread.get("id").and_then(Value::as_str) != Some(expected_thread_id) {
        return Err(VideoAgentToolError::new(
            "video.generation_thread_mismatch",
            "Codex returned a different generation thread",
        ));
    }
    let turns = thread
        .get("turns")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            VideoAgentToolError::new(
                "video.generation_not_registered",
                "Codex generation history was unavailable",
            )
        })?;
    let mut matches = turns.iter().filter_map(|turn| {
        let turn_id = turn.get("id").and_then(Value::as_str)?;
        let item = turn
            .get("items")
            .and_then(Value::as_array)?
            .iter()
            .find(|item| {
                item.get("type").and_then(Value::as_str) == Some("imageGeneration")
                    && item.get("id").and_then(Value::as_str) == Some(generation_id)
            })?;
        Some((turn_id, item))
    });
    let Some((turn_id, item)) = matches.next() else {
        return Err(VideoAgentToolError::new(
            "video.generation_not_registered",
            "The requested Codex image-generation item was not found in authenticated history",
        ));
    };
    if matches.next().is_some() {
        return Err(VideoAgentToolError::new(
            "video.generation_identity_conflict",
            "Codex history contains a duplicate image-generation identity",
        ));
    }
    let completed = matches!(
        item.get("status").and_then(Value::as_str),
        Some("completed" | "succeeded")
    ) && item.get("failure").is_none_or(Value::is_null);
    if !completed {
        return Err(VideoAgentToolError::new(
            "video.generation_not_ready",
            "The authenticated Codex image generation did not complete successfully",
        ));
    }
    let source_path = item
        .get("savedPath")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            VideoAgentToolError::new(
                "video.generation_not_registered",
                "The authenticated generation has no absolute saved output path",
            )
        })?;
    Ok(super::video::TrustedGeneratedVisual {
        thread_id: expected_thread_id.to_string(),
        turn_id: turn_id.to_string(),
        generation_id: generation_id.to_string(),
        source_path,
        producer_version,
        revised_prompt: item
            .get("revisedPrompt")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn write_broker_message(stdin: &mut ChildStdin, value: Value) -> Result<(), VideoAgentToolError> {
    serde_json::to_writer(&mut *stdin, &value).map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_unavailable",
            format!("Could not encode a Codex broker request: {error}"),
        )
    })?;
    stdin
        .write_all(b"\n")
        .and_then(|_| stdin.flush())
        .map_err(|error| {
            VideoAgentToolError::new(
                "video.generation_broker_unavailable",
                format!("Could not send a Codex broker request: {error}"),
            )
        })
}

fn receive_broker_response(
    receiver: &mpsc::Receiver<Value>,
    expected_id: u64,
) -> Result<Value, VideoAgentToolError> {
    let deadline = std::time::Instant::now() + REQUEST_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let message = receiver.recv_timeout(remaining).map_err(|_| {
            VideoAgentToolError::new(
                "video.generation_broker_unavailable",
                "Codex did not answer the generation verification request",
            )
            .retryable(true)
        })?;
        if message.get("id").and_then(Value::as_u64) != Some(expected_id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(VideoAgentToolError::new(
                "video.generation_broker_unavailable",
                "Codex rejected the generation verification request",
            )
            .details(json!({ "diagnostic": error })));
        }
        return Ok(message.get("result").cloned().unwrap_or(Value::Null));
    }
}

/// Resolves a completed image-generation item from durable Codex app-server history. The caller
/// supplies only thread/item identities; savedPath and provenance come from the broker response.
pub(crate) fn resolve_headless_generated_visual(
    thread_id: &str,
    generation_id: &str,
) -> Result<super::video::TrustedGeneratedVisual, VideoAgentToolError> {
    let identity_path = trusted_broker_identity_path().map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The trusted Codex broker location could not be resolved",
        )
        .details(json!({"diagnostic": error}))
    })?;
    resolve_headless_generated_visual_at(&identity_path, thread_id, generation_id)
}

pub(crate) fn resolve_headless_generated_visual_at(
    identity_path: &Path,
    thread_id: &str,
    generation_id: &str,
) -> Result<super::video::TrustedGeneratedVisual, VideoAgentToolError> {
    let thread_id = thread_id.trim();
    let generation_id = generation_id.trim();
    if thread_id.is_empty() || generation_id.is_empty() {
        return Err(VideoAgentToolError::new(
            "video.generation_identity_required",
            "Headless generation registration requires Codex thread and item identities",
        ));
    }
    let identity = read_codex_broker_identity_at(identity_path)?;
    resolve_generated_visual_via_broker(&identity, thread_id, generation_id)
}

fn prepare_enrolled_broker_executable(
    identity: &EnrolledCodexBrokerIdentity,
) -> Result<File, VideoAgentToolError> {
    let account_home = account_home_dir().map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The operating-system account could not be verified",
        )
        .details(json!({"diagnostic": error}))
    })?;
    if identity.schema_version != 1
        || identity.canonical_path.as_os_str().is_empty()
        || !identity.canonical_path.is_absolute()
        || identity.user_home != account_home
        || identity.version.trim().is_empty()
        || identity.sha256.len() != 64
        || !identity
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The enrolled Codex broker identity is incomplete or belongs to another account",
        ));
    }
    let canonical_path = fs::canonicalize(&identity.canonical_path).map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex broker executable is no longer available",
        )
        .details(json!({"diagnostic": error.to_string()}))
    })?;
    if canonical_path != identity.canonical_path {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex broker executable path changed; reconnect Codex in soundAr",
        ));
    }
    let canonical_codex_home = fs::canonicalize(&identity.codex_home).map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex history directory is no longer available",
        )
        .details(json!({"diagnostic": error.to_string()}))
    })?;
    let codex_home_metadata = fs::symlink_metadata(&canonical_codex_home).map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex history directory could not be inspected",
        )
        .details(json!({"diagnostic": error.to_string()}))
    })?;
    let effective_uid = unsafe { libc::geteuid() };
    if canonical_codex_home != identity.codex_home
        || !codex_home_metadata.is_dir()
        || codex_home_metadata.file_type().is_symlink()
        || codex_home_metadata.uid() != effective_uid
        || codex_home_metadata.permissions().mode() & 0o002 != 0
        || !codex_history_home_is_eligible(&account_home, &canonical_codex_home)
    {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex history directory no longer matches its trusted account binding",
        ));
    }
    let expected_device = identity.device.parse::<u64>().map_err(|_| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The enrolled Codex broker device identity is invalid",
        )
    })?;
    let expected_inode = identity.inode.parse::<u64>().map_err(|_| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_invalid",
            "The enrolled Codex broker inode identity is invalid",
        )
    })?;
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&identity.canonical_path)
        .map_err(|error| {
            VideoAgentToolError::new(
                "video.generation_broker_identity_changed",
                "The enrolled Codex broker executable could not be opened safely",
            )
            .details(json!({"diagnostic": error.to_string()}))
        })?;
    let metadata = source.metadata().map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex broker executable could not be inspected",
        )
        .details(json!({"diagnostic": error.to_string()}))
    })?;
    if !metadata.is_file()
        || metadata.dev() != expected_device
        || metadata.ino() != expected_inode
        || metadata.len() != identity.size_bytes
        || metadata.len() == 0
        || metadata.len() > 512 * 1024 * 1024
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex broker file identity changed; reconnect Codex in soundAr",
        ));
    }
    let memfd_name = b"soundar-codex-broker\0";
    let raw_memfd = unsafe {
        libc::memfd_create(
            memfd_name.as_ptr().cast(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw_memfd < 0 {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_unavailable",
            format!(
                "Could not create a sealed Codex broker executable: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    let mut sealed = unsafe { File::from_raw_fd(raw_memfd) };
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut first_bytes = [0_u8; 4];
    let mut first_bytes_read = 0_usize;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = source.read(&mut buffer).map_err(|error| {
            VideoAgentToolError::new(
                "video.generation_broker_identity_changed",
                format!("Could not read the enrolled Codex broker executable: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        let first_take = (first_bytes.len() - first_bytes_read).min(read);
        if first_take > 0 {
            first_bytes[first_bytes_read..first_bytes_read + first_take]
                .copy_from_slice(&buffer[..first_take]);
            first_bytes_read += first_take;
        }
        hasher.update(&buffer[..read]);
        sealed.write_all(&buffer[..read]).map_err(|error| {
            VideoAgentToolError::new(
                "video.generation_broker_unavailable",
                format!("Could not stage the sealed Codex broker executable: {error}"),
            )
        })?;
        copied = copied.saturating_add(read as u64);
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    let final_metadata = source.metadata().map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            format!("Could not recheck the enrolled Codex broker executable: {error}"),
        )
    })?;
    if first_bytes != *b"\x7fELF"
        || copied != identity.size_bytes
        || actual_sha256 != identity.sha256
        || final_metadata.dev() != expected_device
        || final_metadata.ino() != expected_inode
        || final_metadata.len() != identity.size_bytes
    {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_identity_changed",
            "The enrolled Codex broker bytes changed; reconnect Codex in soundAr",
        ));
    }
    let descriptor = sealed.as_raw_fd();
    if unsafe { libc::fchmod(descriptor, 0o500) } != 0
        || unsafe {
            libc::fcntl(
                descriptor,
                libc::F_ADD_SEALS,
                libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
            )
        } != 0
    {
        return Err(VideoAgentToolError::new(
            "video.generation_broker_unavailable",
            format!(
                "Could not seal the Codex broker executable: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    sealed.seek(SeekFrom::Start(0)).map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_unavailable",
            format!("Could not rewind the sealed Codex broker executable: {error}"),
        )
    })?;
    Ok(sealed)
}

fn resolve_generated_visual_via_broker(
    identity: &EnrolledCodexBrokerIdentity,
    thread_id: &str,
    generation_id: &str,
) -> Result<super::video::TrustedGeneratedVisual, VideoAgentToolError> {
    let sealed_executable = prepare_enrolled_broker_executable(identity)?;
    let executable_fd = sealed_executable.as_raw_fd();
    let executable_path = format!("/proc/self/fd/{executable_fd}");
    let mut command = Command::new(executable_path);
    command
        .arg("app-server")
        .arg("--listen")
        .arg("stdio://")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .env("HOME", &identity.user_home)
        .env("CODEX_HOME", &identity.codex_home)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .current_dir(&identity.codex_home);
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(executable_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(executable_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| {
        VideoAgentToolError::new(
            "video.generation_broker_unavailable",
            format!("Could not start Codex generation verification: {error}"),
        )
    })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        VideoAgentToolError::new(
            "video.generation_broker_unavailable",
            "Codex generation verification stdin was unavailable",
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        VideoAgentToolError::new(
            "video.generation_broker_unavailable",
            "Codex generation verification stdout was unavailable",
        )
    })?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                if message.get("id").and_then(Value::as_u64).is_some()
                    && sender.send(message).is_err()
                {
                    break;
                }
            }
        }
    });
    let result = (|| {
        write_broker_message(
            &mut stdin,
            json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": { "name": "soundAr-headless", "title": "soundAr Headless Generation Broker", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "experimentalApi": true, "requestAttestation": false }
                }
            }),
        )?;
        let _ = receive_broker_response(&receiver, 1)?;
        write_broker_message(&mut stdin, json!({ "method": "initialized" }))?;
        write_broker_message(
            &mut stdin,
            json!({
                "id": 2,
                "method": "thread/read",
                "params": { "threadId": thread_id, "includeTurns": true }
            }),
        )?;
        let response = receive_broker_response(&receiver, 2)?;
        trusted_image_generation_from_thread(
            &response,
            thread_id,
            generation_id,
            Some(identity.version.clone()),
        )
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn spawn_reader(
    session: Arc<CodexSession>,
    stdout: std::process::ChildStdout,
    app: AppHandle,
    runtime: RuntimeState,
    video_dispatcher: VideoAgentDispatcher,
) {
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(request_id) = message.get("id").cloned() {
                if message.get("method").is_none() {
                    // soundAr originates numeric ids, while the app-server protocol also permits
                    // string ids for requests initiated by the server. Only numeric responses can
                    // belong to this pending map.
                    if let Some(id) = request_id.as_u64() {
                        if let Some(sender) = session
                            .pending
                            .lock()
                            .ok()
                            .and_then(|mut pending| pending.remove(&id))
                        {
                            let result = message
                                .get("error")
                                .map(|error| Err(error.to_string()))
                                .unwrap_or_else(|| {
                                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                                });
                            sender.send(result).ok();
                        }
                    }
                    continue;
                }
                if message.get("method").and_then(Value::as_str) == Some("item/tool/call") {
                    // Tool calls may include transcription, link ingest and final rendering. Run
                    // them away from the stdout reader so app-server notifications, interruption
                    // requests and other conversations remain responsive while local work runs.
                    let tool_session = Arc::clone(&session);
                    let tool_runtime = runtime.clone();
                    let tool_app = app.clone();
                    let params = message.get("params").cloned().unwrap_or(Value::Null);
                    thread::spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            execute_dynamic_tool(
                                &tool_runtime,
                                &tool_session,
                                &tool_app,
                                video_dispatcher,
                                params,
                            )
                        }))
                        .unwrap_or_else(|_| {
                            Err(VideoAgentToolError::new(
                                "soundar.tool_panicked",
                                "The local soundAr tool stopped unexpectedly; its durable job can be inspected or resumed",
                            )
                            .retryable(true))
                        });
                        let (success, text) = match result {
                            Ok(value) => (
                                true,
                                serde_json::to_string_pretty(&value)
                                    .unwrap_or_else(|_| value.to_string()),
                            ),
                            Err(error) => {
                                let value = json!({
                                    "schema_version": 1,
                                    "error": error,
                                });
                                (
                                    false,
                                    serde_json::to_string_pretty(&value)
                                        .unwrap_or_else(|_| value.to_string()),
                                )
                            }
                        };
                        tool_session
                            .write(json!({ "id": request_id, "result": {
                                "success": success,
                                "contentItems": [{ "type": "inputText", "text": text }]
                            }}))
                            .ok();
                    });
                    continue;
                }
            }
            app.emit("codex-agent-event", &message).ok();
        }
        app.emit(
            "codex-agent-event",
            json!({ "method": "soundar/codex-disconnected", "params": {} }),
        )
        .ok();
    });
}

pub fn dynamic_tools() -> Value {
    let mut tools = json!([
        { "name": "get_studio_state", "description": "Inspect soundAr models, voices, projects, generation history, jobs, and scheduler state before planning audio work.", "inputSchema": { "type": "object", "properties": {} } },
        { "name": "queue_music_generation", "description": "Queue local text-to-music generation in soundAr and return a durable job id. Requires Studio or Full access.", "inputSchema": { "type": "object", "required": ["prompt"], "properties": { "prompt": {"type":"string"}, "lyrics": {"type":"string"}, "model_id": {"type":"string"}, "duration_seconds": {"type":"number","minimum":1,"maximum":300}, "title": {"type":"string"}, "priority": {"type":"string","enum":["low","normal","high"]}, "seed": {"type":"integer"} } } },
        { "name": "queue_speech_generation", "description": "Queue local text-to-speech generation in soundAr and return a durable job id. Requires Studio or Full access.", "inputSchema": { "type": "object", "required": ["text", "model_id", "voice_id"], "properties": { "text":{"type":"string"}, "model_id":{"type":"string"}, "voice_id":{"type":"string"}, "title":{"type":"string"}, "language":{"type":"string"}, "priority":{"type":"string","enum":["low","normal","high"]} } } },
        { "name": "queue_speech_batch", "description": "Queue an ordered batch of local speech generations for a campaign, course, audiobook, podcast, or other multi-part plan. Requires Studio or Full access.", "inputSchema": { "type":"object", "required":["rows"], "properties": { "rows":{"type":"array","minItems":1,"maxItems":500,"items":{"type":"object","required":["text"],"properties":{"name":{"type":"string"},"text":{"type":"string"},"model_id":{"type":"string"},"voice_id":{"type":"string"},"priority":{"type":"string","enum":["low","normal","high"]},"settings":{"type":"object"}}}}, "parallelism":{"type":"integer","minimum":1,"maximum":8} } } },
        { "name": "save_project", "description": "Create or update a soundAr long-form audio project with chapters. Requires Studio or Full access.", "inputSchema": { "type":"object", "required":["name","document"], "properties": { "id":{"type":"string"}, "name":{"type":"string"}, "document":{"type":"object"} } } },
        { "name": "export_project_master", "description": "Assemble rendered project chapters into one registered, playable soundAr master. Use this for every completed multi-part project instead of shell commands or raw filesystem paths. Requires Studio or Full access.", "inputSchema": { "type":"object", "required":["project_id"], "properties": { "project_id":{"type":"string"}, "settings":{"type":"object","properties":{"format":{"type":"string","enum":["wav","flac"]},"sample_rate":{"type":"integer","enum":[24000,44100,48000]},"gap_ms":{"type":"integer","minimum":0,"maximum":5000},"fade_ms":{"type":"integer","minimum":0,"maximum":1000},"target_lufs":{"type":"number","minimum":-24,"maximum":-9}}} } } },
        { "name": "list_jobs", "description": "List current queued, running, completed, cancelled, and failed soundAr jobs.", "inputSchema": { "type":"object", "properties":{} } },
        { "name": "cancel_job", "description": "Cancel a queued or running soundAr job by id. Requires Studio or Full access and user confirmation.", "inputSchema": { "type":"object", "required":["job_id"], "properties":{"job_id":{"type":"string"}} } }
    ]);
    tools
        .as_array_mut()
        .expect("soundAr dynamic tool catalog is always an array")
        .extend(video_tools::tool_catalog());
    for tool in tools
        .as_array_mut()
        .expect("soundAr dynamic tool catalog is always an array")
    {
        if let Some(specification) = tool.as_object_mut() {
            // DynamicToolSpec is a tagged union in the current app-server protocol.
            specification
                .entry("type".to_string())
                .or_insert_with(|| Value::String("function".to_string()));
        }
    }
    tools
}

/// Returns only the Video Studio tools supported by the headless control plane.
pub(crate) fn video_dynamic_tools() -> Value {
    let mut tools = video_tools::tool_catalog();
    for tool in &mut tools {
        if let Some(specification) = tool.as_object_mut() {
            specification
                .entry("type".to_string())
                .or_insert_with(|| Value::String("function".to_string()));
        }
    }
    Value::Array(tools)
}

fn execute_dynamic_tool(
    runtime: &RuntimeState,
    session: &CodexSession,
    app: &AppHandle,
    video_dispatcher: VideoAgentDispatcher,
    params: Value,
) -> Result<Value, VideoAgentToolError> {
    let tool = params.get("tool").and_then(Value::as_str).ok_or_else(|| {
        VideoAgentToolError::new("soundar.tool_name_missing", "Dynamic tool name is missing")
    })?;
    let thread_id = params
        .get("threadId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VideoAgentToolError::new(
                "soundar.tool_thread_missing",
                "Dynamic tool thread is missing",
            )
        })?;
    let access = session
        .thread_access
        .lock()
        .map_err(|_| "Codex access lock failed")?
        .get(thread_id)
        .copied()
        .unwrap_or(AgentAccess::ReadOnly);
    if matches!(
        tool,
        "queue_music_generation"
            | "queue_speech_generation"
            | "queue_speech_batch"
            | "save_project"
            | "export_project_master"
            | "cancel_job"
    ) || video_tools::requires_studio_access(tool)
    {
        if !access.permits_studio_writes() {
            return Err(VideoAgentToolError::new(
                "soundar.read_only",
                "This conversation is in read-only mode. Choose Studio access or Full access before asking soundAr to make changes.",
            ));
        }
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if video_tools::is_video_tool(tool) {
        // These identities come from the authenticated app-server envelope. Never accept a
        // model/tool argument as conversation provenance: that would let a tool call attach its
        // result to another saved conversation.
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 512)
            .ok_or_else(|| {
                VideoAgentToolError::new(
                    "soundar.tool_turn_missing",
                    "Dynamic video tool turn identity is missing or invalid",
                )
            })?;
        let call_id = params
            .get("callId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 512)
            .ok_or_else(|| {
                VideoAgentToolError::new(
                    "soundar.tool_call_missing",
                    "Dynamic video tool call identity is missing or invalid",
                )
            })?;
        let operation = VideoAgentOperation::parse(tool, arguments)?;
        let fallback_phase = operation.kind().phase();
        let result = match operation {
            VideoAgentOperation::RegisterGeneratedVisual(request) => {
                if request
                    .thread_id
                    .as_deref()
                    .is_some_and(|requested| requested != thread_id)
                {
                    return Err(VideoAgentToolError::new(
                        "video.generation_thread_mismatch",
                        "In-app generation registration is bound to the authenticated current thread",
                    ));
                }
                let response = session
                    .request(
                        "thread/read",
                        json!({ "threadId": thread_id, "includeTurns": true }),
                    )
                    .map_err(|error| {
                        VideoAgentToolError::new(
                            "video.generation_broker_unavailable",
                            "Codex generation history could not be verified",
                        )
                        .details(json!({ "diagnostic": error }))
                    })?;
                let generation = trusted_image_generation_from_thread(
                    &response,
                    thread_id,
                    &request.generation_id,
                    Some(session.version.clone()),
                )?;
                let receipt = runtime
                    .video
                    .register_trusted_generated_visual(
                        super::video::AuthorizeVisualSelectionRequest {
                            project_id: request.project_id.clone(),
                            expected_revision: request.expected_revision,
                            expected_version_id: request.expected_version_id,
                        },
                        generation,
                    )
                    .map_err(VideoAgentToolError::from)?;
                VideoAgentResult::project_data(
                    VideoAgentOperationKind::RegisterGeneratedVisual,
                    "Authenticated Codex generation registered; use this one-use receipt with add_visual_asset",
                    request.project_id,
                    serde_json::to_value(receipt).map_err(|error| {
                        VideoAgentToolError::new(
                            "video.agent_result_encode_failed",
                            format!("Could not encode the generation receipt: {error}"),
                        )
                    })?,
                )
            }
            operation => video_dispatcher.dispatch(
                runtime,
                operation,
                Some(video_tools::compact_progress_callback(
                    app.clone(),
                    fallback_phase,
                )),
            )?,
        };
        if let Some(project_id) = result.project_id.as_deref() {
            let prominent = result
                .output
                .final_master
                .as_ref()
                .or(result.output.artifact.as_ref());
            let output_id = prominent
                .and_then(|artifact| artifact.get("id"))
                .and_then(Value::as_str);
            let relationship = prominent
                .and_then(|artifact| artifact.get("role"))
                .and_then(Value::as_str)
                .filter(|role| {
                    matches!(
                        *role,
                        "preview" | "master" | "variation" | "publish-package"
                    )
                })
                .unwrap_or("project");
            runtime
                .store
                .link_assistant_video_artifact(&json!({
                    "thread_id": thread_id,
                    "turn_id": turn_id,
                    "item_id": call_id,
                    "project_id": project_id,
                    "output_id": output_id,
                    "relationship": relationship,
                }))
                .map_err(|error| {
                    VideoAgentToolError::new(
                        "video.assistant_link_failed",
                        "The Video Studio result completed but could not be attached to this saved conversation",
                    )
                    .details(json!({ "diagnostic": error }))
                })?;
        }
        return serde_json::to_value(result).map_err(|error| {
            VideoAgentToolError::new(
                "video.agent_result_encode_failed",
                "Video Studio completed the operation but its result could not be encoded",
            )
            .details(json!({ "diagnostic": error.to_string() }))
        });
    }
    let value = match tool {
        "get_studio_state" => {
            let video_root = runtime.store.video_artifacts_root();
            let video_projects = runtime
                .video
                .list_projects()
                .map_err(VideoAgentToolError::from)?
                .iter()
                .map(|project| {
                    super::video::present_video_project_summary(project, &video_root)
                        .map_err(VideoAgentToolError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            json!({
                "models": super::read_json(runtime.model_registry_path.clone(), json!({"models":[]})).get("models").cloned().unwrap_or_else(|| json!([])),
                "voices": runtime.store.list_voices()?,
                "projects": runtime.store.list_projects()?,
                "video_projects": video_projects,
                "history": runtime.store.list_history(None)?,
                "jobs": runtime.store.list_jobs()?,
                "scheduler": runtime.scheduler_status()?,
            })
        }
        "list_jobs" => json!(runtime.store.list_jobs()?),
        "save_project" => runtime.store.save_project(&arguments)?,
        "export_project_master" => super::build_project_master(
            runtime,
            arguments
                .get("project_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    VideoAgentToolError::new("soundar.invalid_arguments", "project_id is required")
                })?
                .to_string(),
            arguments
                .get("settings")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )?,
        "cancel_job" => {
            json!({ "cancelled": runtime.cancel_job(arguments.get("job_id").and_then(Value::as_str).ok_or_else(|| VideoAgentToolError::new("soundar.invalid_arguments", "job_id is required"))?)? })
        }
        "queue_music_generation" => queue_music(runtime, arguments)?,
        "queue_speech_generation" => queue_speech(runtime, arguments)?,
        "queue_speech_batch" => queue_speech_batch(runtime, arguments)?,
        _ => {
            return Err(VideoAgentToolError::new(
                "soundar.unknown_tool",
                format!("Unknown soundAr tool: {tool}"),
            ))
        }
    };
    Ok(value)
}

fn queue_music(runtime: &RuntimeState, request: Value) -> Result<Value, String> {
    let request = prepare_music_generation_request(request)?;
    queue_background(runtime, "music-generation", request)
}

fn queue_speech(runtime: &RuntimeState, request: Value) -> Result<Value, String> {
    queue_background(runtime, "synthesis", request)
}

fn queue_speech_batch(runtime: &RuntimeState, request: Value) -> Result<Value, String> {
    let parallelism = request
        .get("parallelism")
        .and_then(Value::as_u64)
        .unwrap_or(2)
        .clamp(1, 8) as usize;
    runtime.queue_batch(&request, parallelism)
}

fn queue_background(runtime: &RuntimeState, kind: &str, request: Value) -> Result<Value, String> {
    let job_id = runtime.store.create_job(kind, &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    runtime.start_background_synthesis(job_id.clone(), request.clone())?;
    Ok(
        json!({ "id": job_id, "kind": kind, "status": "preparing", "progress": 0.05, "request": request }),
    )
}

fn discover_codex() -> Option<(PathBuf, String)> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("SOUNDAR_CODEX_BIN").or_else(|| env::var_os("CODEX_BIN")) {
        // An explicit override is authoritative, including when the caller is
        // intentionally validating an older or pre-release Codex build.
        return validate_codex_candidate(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("PATH") {
        candidates.extend(env::split_paths(&path).map(|directory| directory.join("codex")));
    }
    candidates.extend(
        [
            "/usr/bin/codex",
            "/usr/local/bin/codex",
            "/opt/codex/bin/codex",
            "/snap/bin/codex",
            "/var/lib/snapd/snap/bin/codex",
            "/var/lib/flatpak/exports/bin/codex",
        ]
        .into_iter()
        .map(PathBuf::from),
    );
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.extend(
            [
                ".local/bin/codex",
                ".cargo/bin/codex",
                ".npm-global/bin/codex",
                ".volta/bin/codex",
                ".asdf/shims/codex",
                ".local/share/mise/shims/codex",
                ".bun/bin/codex",
                ".local/share/pnpm/codex",
            ]
            .into_iter()
            .map(|path| home.join(path)),
        );
        for root in [
            home.join(".nvm/versions/node"),
            home.join(".local/share/fnm/node-versions"),
            home.join(".local/share/pnpm"),
            home.join(".local/share/flatpak/exports/bin"),
        ] {
            collect_named(&root, "codex", 5, &mut candidates);
        }
    }
    for root in [
        PathBuf::from("/opt"),
        PathBuf::from("/usr/local"),
        PathBuf::from("/var/lib/flatpak/exports/bin"),
    ] {
        collect_named(&root, "codex", 6, &mut candidates);
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let canonical = fs::canonicalize(&candidate).ok()?;
            if !seen.insert(canonical.clone()) {
                return None;
            }
            validate_codex_candidate(canonical)
        })
        // Desktop launchers do not reliably inherit shell-manager PATH entries
        // (NVM, fnm, mise, and similar). Choose the newest valid installation
        // found across every supported location instead of the first PATH hit.
        .max_by_key(|(_, version)| codex_version_key(version))
}

fn validate_codex_candidate(candidate: PathBuf) -> Option<(PathBuf, String)> {
    let canonical = fs::canonicalize(candidate).ok()?;
    if !is_executable(&canonical) {
        return None;
    }
    let output = Command::new(&canonical).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    version
        .to_ascii_lowercase()
        .contains("codex")
        .then_some((canonical, version))
}

fn codex_version_key(version: &str) -> (u64, u64, u64, bool) {
    let raw = version
        .split_whitespace()
        .find(|part| {
            part.chars()
                .next()
                .is_some_and(|value| value.is_ascii_digit())
        })
        .unwrap_or_default();
    let stable = !raw.contains('-');
    let mut numeric = raw.split('-').next().unwrap_or_default().split('.');
    (
        numeric
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        numeric
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        numeric
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        stable,
    )
}

fn collect_named(root: &Path, name: &str, depth: usize, output: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten().take(256) {
        let path = entry.path();
        if path.file_name().and_then(|value| value.to_str()) == Some(name) {
            output.push(path.clone());
        }
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            collect_named(&path, name, depth - 1, output);
        }
    }
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        codex_history_home_is_eligible, codex_home_for_enrollment, codex_version_key,
        discover_codex, dynamic_tools, trusted_image_generation_from_thread,
    };
    use serde_json::json;
    use std::{fs, os::unix::fs::symlink, path::PathBuf};
    use uuid::Uuid;

    #[test]
    fn dynamic_tool_catalog_supports_access_changes_without_resetting_the_thread() {
        let tools = dynamic_tools();
        let names = tools
            .as_array()
            .expect("tool catalog")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert!(tools
            .as_array()
            .expect("tool catalog")
            .iter()
            .all(|tool| tool.get("type").and_then(serde_json::Value::as_str) == Some("function")));
        assert_eq!(names.len(), 25);
        assert!(names.contains(&"get_studio_state"));
        assert!(names.contains(&"queue_speech_generation"));
        assert!(names.contains(&"queue_music_generation"));
        assert!(names.contains(&"export_project_master"));
        assert!(names.contains(&"preview_link"));
        assert!(names.contains(&"import_link"));
        assert!(names.contains(&"analyze_video"));
        assert!(names.contains(&"edit_video_timeline"));
        assert!(names.contains(&"register_generated_visual"));
        assert!(names.contains(&"add_visual_asset"));
        assert!(names.contains(&"render_video_preview"));
        assert!(names.contains(&"revise_video"));
        assert!(names.contains(&"export_video"));
        assert!(names.contains(&"export_publish_package"));
    }

    #[test]
    fn discovers_current_codex_installation_when_available() {
        if std::env::var_os("PATH").is_some() {
            let result = discover_codex();
            if let Some((path, version)) = result {
                assert!(path.is_absolute());
                assert!(version.to_ascii_lowercase().contains("codex"));
            }
        }
    }

    #[test]
    fn compares_codex_versions_independently_of_installation_order() {
        assert!(codex_version_key("codex-cli 0.150.1") > codex_version_key("codex-cli 0.134.0"));
        assert!(
            codex_version_key("codex-cli 0.150.1")
                > codex_version_key("codex-cli 0.147.0-alpha.6.6")
        );
        assert!(
            codex_version_key("codex-cli 0.151.0") > codex_version_key("codex-cli 0.151.0-alpha.6")
        );
    }

    #[test]
    fn trusted_codex_history_rejects_a_symlinked_default_home() {
        let root = std::env::temp_dir().join(format!(
            "soundar-codex-history-symlink-{}",
            Uuid::new_v4().simple()
        ));
        let account_home = root.join("account");
        let attacker_history = root.join("workspace-controlled-history");
        fs::create_dir_all(&account_home).expect("create synthetic account home");
        fs::create_dir_all(&attacker_history).expect("create attacker history directory");
        symlink(&attacker_history, account_home.join(".codex"))
            .expect("redirect default Codex history through a symlink");
        let canonical_attacker_history =
            fs::canonicalize(&attacker_history).expect("canonical attacker history");

        assert!(codex_home_for_enrollment(&account_home).is_err());
        assert!(!codex_history_home_is_eligible(
            &account_home,
            &canonical_attacker_history
        ));

        fs::remove_dir_all(&root).expect("remove synthetic account home");
    }

    #[test]
    fn authenticated_codex_history_resolves_only_the_saved_generation_path() {
        let response = json!({
            "thread": {
                "id": "thread-trusted",
                "turns": [{
                    "id": "turn-trusted",
                    "items": [{
                        "type": "imageGeneration",
                        "id": "generation-trusted",
                        "status": "completed",
                        "result": "{\"savedPath\":\"/tmp/model-asserted.png\",\"producer\":\"model\"}",
                        "savedPath": "/tmp/broker-saved.png",
                        "revisedPrompt": "Broker revised prompt",
                        "failure": null
                    }]
                }]
            }
        });
        let generation = trusted_image_generation_from_thread(
            &response,
            "thread-trusted",
            "generation-trusted",
            Some("codex-cli test".to_string()),
        )
        .expect("resolve trusted generation item");
        assert_eq!(generation.thread_id, "thread-trusted");
        assert_eq!(generation.turn_id, "turn-trusted");
        assert_eq!(
            generation.source_path,
            PathBuf::from("/tmp/broker-saved.png")
        );
        assert_eq!(generation.generation_id, "generation-trusted");
        assert_eq!(
            generation.producer_version.as_deref(),
            Some("codex-cli test")
        );
    }

    #[test]
    fn generation_history_is_thread_bound_and_requires_completed_absolute_output() {
        let response = json!({
            "thread": {
                "id": "thread-a",
                "turns": [{
                    "id": "turn-a",
                    "items": [{
                        "type": "imageGeneration",
                        "id": "generation-a",
                        "status": "failed",
                        "result": "",
                        "savedPath": "relative.png",
                        "failure": {"type": "usageLimitExceeded", "limitId": "images"}
                    }]
                }]
            }
        });
        let mismatch =
            trusted_image_generation_from_thread(&response, "thread-b", "generation-a", None)
                .expect_err("authenticated thread mismatch must fail");
        assert_eq!(mismatch.code, "video.generation_thread_mismatch");
        let incomplete =
            trusted_image_generation_from_thread(&response, "thread-a", "generation-a", None)
                .expect_err("failed generation must fail");
        assert_eq!(incomplete.code, "video.generation_not_ready");
    }
}
