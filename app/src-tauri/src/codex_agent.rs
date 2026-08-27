use super::{prepare_music_generation_request, RuntimeState};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
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

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_NAME: &str = "soundAr";

pub struct CodexAgentState {
    session: Mutex<Option<Arc<CodexSession>>>,
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
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
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
                    return Ok(
                        json!({ "connected": true, "path": session.executable, "version": session.version }),
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
        spawn_reader(session.clone(), stdout, app, runtime);
        session.request("initialize", json!({
            "clientInfo": { "name": CLIENT_NAME, "title": "soundAr Studio Assistant", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true }
        }))?;
        session.notify("initialized", Value::Null)?;
        Ok(json!({ "connected": true, "path": executable, "version": version }))
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
        self.write(json!({ "method": method, "params": params }))
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
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| format!("Codex did not answer {method} within 30 seconds"))?
    }
}

fn spawn_reader(
    session: Arc<CodexSession>,
    stdout: std::process::ChildStdout,
    app: AppHandle,
    runtime: RuntimeState,
) {
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                if message.get("method").is_none() {
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
                    continue;
                }
                if message.get("method").and_then(Value::as_str) == Some("item/tool/call") {
                    let result = execute_dynamic_tool(
                        &runtime,
                        &session,
                        message.get("params").cloned().unwrap_or(Value::Null),
                    );
                    session.write(json!({ "id": id, "result": {
                        "success": result.is_ok(),
                        "contentItems": [{ "type": "inputText", "text": result.unwrap_or_else(|error| format!("Tool failed: {error}")) }]
                    }})).ok();
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
    json!([
        { "name": "get_studio_state", "description": "Inspect soundAr models, voices, projects, generation history, jobs, and scheduler state before planning audio work.", "inputSchema": { "type": "object", "properties": {} } },
        { "name": "queue_music_generation", "description": "Queue local text-to-music generation in soundAr and return a durable job id. Requires Studio or Full access.", "inputSchema": { "type": "object", "required": ["prompt"], "properties": { "prompt": {"type":"string"}, "lyrics": {"type":"string"}, "model_id": {"type":"string"}, "duration_seconds": {"type":"number","minimum":1,"maximum":300}, "title": {"type":"string"}, "priority": {"type":"string","enum":["low","normal","high"]}, "seed": {"type":"integer"} } } },
        { "name": "queue_speech_generation", "description": "Queue local text-to-speech generation in soundAr and return a durable job id. Requires Studio or Full access.", "inputSchema": { "type": "object", "required": ["text", "model_id", "voice_id"], "properties": { "text":{"type":"string"}, "model_id":{"type":"string"}, "voice_id":{"type":"string"}, "title":{"type":"string"}, "language":{"type":"string"}, "priority":{"type":"string","enum":["low","normal","high"]} } } },
        { "name": "queue_speech_batch", "description": "Queue an ordered batch of local speech generations for a campaign, course, audiobook, podcast, or other multi-part plan. Requires Studio or Full access.", "inputSchema": { "type":"object", "required":["rows"], "properties": { "rows":{"type":"array","minItems":1,"maxItems":500,"items":{"type":"object","required":["text"],"properties":{"name":{"type":"string"},"text":{"type":"string"},"model_id":{"type":"string"},"voice_id":{"type":"string"},"priority":{"type":"string","enum":["low","normal","high"]},"settings":{"type":"object"}}}}, "parallelism":{"type":"integer","minimum":1,"maximum":8} } } },
        { "name": "save_project", "description": "Create or update a soundAr long-form audio project with chapters. Requires Studio or Full access.", "inputSchema": { "type":"object", "required":["name","document"], "properties": { "id":{"type":"string"}, "name":{"type":"string"}, "document":{"type":"object"} } } },
        { "name": "export_project_master", "description": "Assemble rendered project chapters into one registered, playable soundAr master. Use this for every completed multi-part project instead of shell commands or raw filesystem paths. Requires Studio or Full access.", "inputSchema": { "type":"object", "required":["project_id"], "properties": { "project_id":{"type":"string"}, "settings":{"type":"object","properties":{"format":{"type":"string","enum":["wav","flac"]},"sample_rate":{"type":"integer","enum":[24000,44100,48000]},"gap_ms":{"type":"integer","minimum":0,"maximum":5000},"fade_ms":{"type":"integer","minimum":0,"maximum":1000},"target_lufs":{"type":"number","minimum":-24,"maximum":-9}}} } } },
        { "name": "list_jobs", "description": "List current queued, running, completed, cancelled, and failed soundAr jobs.", "inputSchema": { "type":"object", "properties":{} } },
        { "name": "cancel_job", "description": "Cancel a queued or running soundAr job by id. Requires Studio or Full access and user confirmation.", "inputSchema": { "type":"object", "required":["job_id"], "properties":{"job_id":{"type":"string"}} } }
    ])
}

fn execute_dynamic_tool(
    runtime: &RuntimeState,
    session: &CodexSession,
    params: Value,
) -> Result<String, String> {
    let tool = params
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("Dynamic tool name is missing")?;
    let thread_id = params
        .get("threadId")
        .and_then(Value::as_str)
        .ok_or("Dynamic tool thread is missing")?;
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
    ) && !access.permits_studio_writes()
    {
        return Err("This conversation is in read-only mode. Choose Studio access or Full access before asking soundAr to make changes.".to_string());
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let value = match tool {
        "get_studio_state" => json!({
            "models": super::read_json(runtime.model_registry_path.clone(), json!({"models":[]})).get("models").cloned().unwrap_or_else(|| json!([])),
            "voices": runtime.store.list_voices()?,
            "projects": runtime.store.list_projects()?,
            "history": runtime.store.list_history(None)?,
            "jobs": runtime.store.list_jobs()?,
            "scheduler": runtime.scheduler_status()?,
        }),
        "list_jobs" => json!(runtime.store.list_jobs()?),
        "save_project" => runtime.store.save_project(&arguments)?,
        "export_project_master" => super::build_project_master(
            runtime,
            arguments
                .get("project_id")
                .and_then(Value::as_str)
                .ok_or("project_id is required")?
                .to_string(),
            arguments
                .get("settings")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )?,
        "cancel_job" => {
            json!({ "cancelled": runtime.cancel_job(arguments.get("job_id").and_then(Value::as_str).ok_or("job_id is required")?)? })
        }
        "queue_music_generation" => queue_music(runtime, arguments)?,
        "queue_speech_generation" => queue_speech(runtime, arguments)?,
        "queue_speech_batch" => queue_speech_batch(runtime, arguments)?,
        _ => return Err(format!("Unknown soundAr tool: {tool}")),
    };
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
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
    use super::{codex_version_key, discover_codex, dynamic_tools};

    #[test]
    fn dynamic_tool_catalog_supports_access_changes_without_resetting_the_thread() {
        let tools = dynamic_tools();
        let names = tools
            .as_array()
            .expect("tool catalog")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 8);
        assert!(names.contains(&"get_studio_state"));
        assert!(names.contains(&"queue_speech_generation"));
        assert!(names.contains(&"queue_music_generation"));
        assert!(names.contains(&"export_project_master"));
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
}
