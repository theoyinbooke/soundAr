use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    env, fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Manager};

struct PythonProcess {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Clone)]
struct RuntimeState {
    process: Arc<Mutex<Option<PythonProcess>>>,
    setup_lock: Arc<Mutex<()>>,
    runtime_root: PathBuf,
    python_path: PathBuf,
}

impl RuntimeState {
    fn new(runtime_root: PathBuf, python_path: PathBuf) -> Self {
        Self {
            process: Arc::new(Mutex::new(None)),
            setup_lock: Arc::new(Mutex::new(())),
            runtime_root,
            python_path,
        }
    }

    fn start_process(&self) -> Result<PythonProcess, String> {
        let mut child = Command::new(&self.python_path)
            .arg(self.runtime_root.join("bridge.py"))
            .arg("--serve")
            .current_dir(&self.runtime_root)
            .env("PYTHONPATH", &self.runtime_root)
            .env("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
            .env("TOKENIZERS_PARALLELISM", "false")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| {
                format!(
                    "Could not start the Python runtime at {}: {error}. Run install-linux.sh to install the local inference runtime.",
                    self.python_path.display()
                )
            })?;
        let stdin = child.stdin.take().ok_or("Python stdin is unavailable")?;
        let stdout = child.stdout.take().ok_or("Python stdout is unavailable")?;
        Ok(PythonProcess {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    fn request(&self, request: Value) -> Result<Value, String> {
        let mut guard = self
            .process
            .lock()
            .map_err(|_| "Python runtime lock failed")?;
        if guard.is_none() {
            *guard = Some(self.start_process()?);
        }
        let process = guard.as_mut().ok_or("Python runtime is unavailable")?;
        writeln!(process.stdin, "{request}")
            .map_err(|error| format!("Could not send the synthesis request: {error}"))?;
        process
            .stdin
            .flush()
            .map_err(|error| format!("Could not flush the synthesis request: {error}"))?;
        let mut line = String::new();
        process
            .stdout
            .read_line(&mut line)
            .map_err(|error| format!("Could not read the synthesis response: {error}"))?;
        let response: Value = serde_json::from_str(&line)
            .map_err(|error| format!("Invalid runtime response: {error}"))?;
        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(response.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Synthesis failed")
                .to_string())
        }
    }

    fn setup(&self, app: &tauri::AppHandle) -> Result<(), String> {
        let _setup_guard = self
            .setup_lock
            .lock()
            .map_err(|_| "Runtime setup lock failed")?;
        if self.python_path.is_file() {
            app.emit(
                "runtime-setup-progress",
                "Local inference runtime is already ready.",
            )
            .ok();
            return Ok(());
        }

        let script = self.runtime_root.join("setup-runtime.sh");
        if !script.is_file() {
            return Err(format!(
                "The bundled runtime installer was not found at {}. Reinstall soundAr and retry.",
                script.display()
            ));
        }

        let mut child = Command::new("/bin/bash")
            .arg(&script)
            .arg(app_data_dir())
            .current_dir(&self.runtime_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not launch runtime setup: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Runtime setup output is unavailable")?;
        let mut recent_output = VecDeque::with_capacity(24);

        for line in BufReader::new(stdout).lines() {
            let line =
                line.map_err(|error| format!("Could not read runtime setup output: {error}"))?;
            if recent_output.len() == 24 {
                recent_output.pop_front();
            }
            recent_output.push_back(line.clone());
            if let Some(message) = line.strip_prefix("soundar:") {
                app.emit("runtime-setup-progress", message).ok();
            }
        }

        let status = child
            .wait()
            .map_err(|error| format!("Could not finish runtime setup: {error}"))?;
        if !status.success() {
            let detail = recent_output
                .iter()
                .rev()
                .find(|line| !line.trim().is_empty() && !line.starts_with("soundar:"))
                .map(String::as_str)
                .unwrap_or("No installer details were returned");
            return Err(format!("Runtime setup failed: {detail}"));
        }
        if !self.python_path.is_file() {
            return Err(format!(
                "Runtime setup completed without creating {}",
                self.python_path.display()
            ));
        }

        *self
            .process
            .lock()
            .map_err(|_| "Python runtime lock failed")? = None;
        Ok(())
    }
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn app_data_dir() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/share"))
        .join("soundar/runtime")
}

fn runtime_root(app: &tauri::AppHandle) -> PathBuf {
    if let Some(path) = env::var_os("SOUNDAR_RUNTIME_ROOT") {
        return PathBuf::from(path);
    }
    if cfg!(debug_assertions) {
        return project_root();
    }
    app.path()
        .resource_dir()
        .map(|path| path.join("runtime"))
        .unwrap_or_else(|_| project_root())
}

fn python_path() -> PathBuf {
    if let Some(path) = env::var_os("SOUNDAR_PYTHON") {
        return PathBuf::from(path);
    }
    if cfg!(debug_assertions) {
        let development_python = project_root().join(".venv/bin/python");
        if development_python.is_file() {
            return development_python;
        }
    }
    app_data_dir().join(".venv/bin/python")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn install_kind() -> &'static str {
    if env::var_os("APPIMAGE").is_some() {
        "appimage"
    } else if env::current_exe()
        .ok()
        .is_some_and(|path| path.starts_with("/usr"))
    {
        "deb"
    } else {
        "development"
    }
}

fn read_json(path: PathBuf, fallback: Value) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or(fallback)
}

fn generated_audio_bytes(path: &str) -> Result<Vec<u8>, String> {
    let export_root = home_dir().join(".soundAr/exports");
    fs::create_dir_all(&export_root)
        .map_err(|error| format!("Could not access the export directory: {error}"))?;
    let export_root = export_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve the export directory: {error}"))?;
    let audio_path = PathBuf::from(path)
        .canonicalize()
        .map_err(|error| format!("Generated audio was not found: {error}"))?;
    if !audio_path.starts_with(&export_root) {
        return Err("Playback is restricted to soundAr exports".to_string());
    }
    let allowed = audio_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "wav" | "flac"));
    if !allowed {
        return Err("Generated audio must be WAV or FLAC".to_string());
    }
    fs::read(audio_path).map_err(|error| format!("Could not read generated audio: {error}"))
}

#[tauri::command]
fn read_generated_audio(path: String) -> Result<tauri::ipc::Response, String> {
    generated_audio_bytes(&path).map(tauri::ipc::Response::new)
}

fn gpu_status(python: &PathBuf) -> Value {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.used,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let fields: Vec<&str> = text
                .lines()
                .next()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .collect();
            if fields.len() >= 4 {
                return json!({
                    "gpu_name": fields[0],
                    "vram_total_mb": fields[1].parse::<u64>().unwrap_or(0),
                    "vram_used_mb": fields[2].parse::<u64>().unwrap_or(0),
                    "driver_version": fields[3],
                    "cuda_available": true,
                    "python_ready": python.is_file()
                });
            }
        }
    }
    json!({
        "gpu_name": "CPU",
        "vram_total_mb": 0,
        "vram_used_mb": 0,
        "driver_version": "",
        "cuda_available": false,
        "python_ready": python.is_file()
    })
}

#[tauri::command]
fn bootstrap_state(state: tauri::State<'_, RuntimeState>) -> Value {
    let root = &state.runtime_root;
    let home = home_dir();
    let catalog = read_json(
        root.join("data/curated_models.json"),
        json!({ "models": [] }),
    );
    let registry = read_json(
        home.join(".soundAr/state/models.json"),
        json!({ "models": [] }),
    );
    json!({
        "catalog": catalog.get("models").cloned().unwrap_or_else(|| json!([])),
        "installed": registry.get("models").cloned().unwrap_or_else(|| json!([])),
        "system": gpu_status(&state.python_path),
        "export_dir": home.join(".soundAr/exports").to_string_lossy(),
        "voices": [
            { "id": "mara", "name": "Mara", "style": "Warm documentary", "sample_label": "Owner-recorded sample", "sample_seconds": 18, "engines": ["Kokoro", "Chatterbox"], "consent": "confirmed", "state": "ready", "color": "green" },
            { "id": "amara", "name": "Amara", "style": "Clear narration", "sample_label": "Verified local sample", "sample_seconds": 31, "engines": ["Kokoro", "XTTS"], "consent": "confirmed", "state": "ready", "color": "green" },
            { "id": "studio-neutral", "name": "Studio Neutral", "style": "Utility preset", "sample_label": "Built-in voice", "sample_seconds": 0, "engines": ["Kokoro"], "consent": "not-required", "state": "preset", "color": "amber" }
        ],
        "install_kind": install_kind(),
        "runtime": "tauri"
    })
}

#[tauri::command]
async fn synthesize(
    state: tauri::State<'_, RuntimeState>,
    request: Value,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.request(request))
        .await
        .map_err(|error| format!("Synthesis worker failed: {error}"))?
}

#[tauri::command]
async fn setup_runtime(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
) -> Result<(), String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.setup(&app))
        .await
        .map_err(|error| format!("Runtime setup worker failed: {error}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let root = runtime_root(app.handle());
            app.manage(RuntimeState::new(root, python_path()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap_state,
            setup_runtime,
            synthesize,
            read_generated_audio
        ])
        .run(tauri::generate_context!())
        .expect("error while running soundAr");
}

#[cfg(test)]
mod tests {
    use super::{generated_audio_bytes, home_dir};
    use std::fs;

    #[test]
    fn generated_audio_reader_rejects_paths_outside_exports() {
        let error =
            generated_audio_bytes("/etc/hosts").expect_err("outside path should be rejected");
        assert_eq!(error, "Playback is restricted to soundAr exports");
    }

    #[test]
    fn generated_audio_reader_returns_export_bytes() {
        let path = home_dir().join(".soundAr/exports/soundar-ipc-test.wav");
        fs::create_dir_all(path.parent().expect("export parent")).expect("create exports");
        fs::write(&path, b"RIFF-test-audio").expect("write fixture");
        let bytes =
            generated_audio_bytes(path.to_str().expect("fixture path")).expect("read fixture");
        fs::remove_file(path).expect("remove fixture");
        assert_eq!(bytes, b"RIFF-test-audio");
    }
}
