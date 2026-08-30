mod agent_cli;
mod codex_agent;
mod store;
pub mod video;
mod video_commands;

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SampleFormat, SizedSample,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use uuid::Uuid;

use store::{normalize_batch_rows, priority_value, Store};

const MAX_RPC_REQUEST_BYTES: usize = 1_048_576;
const MAX_RPC_RESPONSE_BYTES: u64 = 8_388_608;
const MAX_BATCH_IMPORT_BYTES: u64 = 8_388_608;
const GPU_COLD_LOAD_HEADROOM_MB: u64 = 512;
const SHARED_GPU_HEADROOM_MB: u64 = 768;
const SHARED_NVENC_SESSION_LIMIT: u8 = 2;
// The checked-in qualification gate proves only this exact pairing. Keep every
// larger speech, music, image, tracking, and exclusive workload serialized.
const QUALIFIED_VIDEO_OVERLAP_ENGINE: &str = "transformers";
const QUALIFIED_VIDEO_OVERLAP_MODEL: &str = "openai/whisper-tiny";
const QUALIFIED_VIDEO_OVERLAP_MAX_VIDEO_VRAM_MB: u64 = 2_048;

fn cold_load_needs_idle_reclamation(available_mb: u64, required_mb: u64) -> bool {
    required_mb > 0 && available_mb < required_mb.saturating_add(GPU_COLD_LOAD_HEADROOM_MB)
}

struct PythonProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    engine: String,
    loaded_models: Vec<String>,
    last_response_at: Instant,
}

#[derive(Clone)]
struct RuntimeState {
    worker_pool: Arc<Mutex<Vec<PythonProcess>>>,
    scheduler: Arc<(Mutex<InferenceScheduler>, Condvar)>,
    setup_lock: Arc<Mutex<()>>,
    model_operation_lock: Arc<Mutex<()>>,
    active_download: Arc<Mutex<Option<ActiveDownload>>>,
    active_syntheses: Arc<Mutex<HashMap<String, ActiveSynthesis>>>,
    active_batch_coordinators: Arc<Mutex<HashSet<String>>>,
    api_server: Arc<Mutex<Option<ActiveApiServer>>>,
    active_recording: Arc<Mutex<Option<ActiveRecording>>>,
    active_playback: Arc<Mutex<Option<ActivePlayback>>>,
    runtime_root: PathBuf,
    python_path: PathBuf,
    model_registry_path: PathBuf,
    store: Arc<Store>,
    video: Arc<video::VideoStudioService>,
    worker_probe_after: Duration,
    codex_agent: Arc<codex_agent::CodexAgentState>,
}

struct ActiveDownload {
    model_id: String,
    child: Child,
    cancelled: bool,
}

struct ActiveSynthesis {
    process_id: u32,
}

struct InferenceScheduler {
    active_workers: usize,
    active_gpu_workers: usize,
    active_qualified_video_overlap_workers: usize,
    active_engines: HashMap<String, usize>,
    reserved_vram_mb: u64,
    video_reserved_vram_mb: u64,
    video_nvenc_sessions: u8,
    active_video_gpu_jobs: HashMap<String, VideoGpuReservation>,
    exclusive_video_job: Option<String>,
    available_vram_budget_mb: Option<u64>,
    max_workers: usize,
    next_ticket: u64,
    waiters: Vec<SchedulerWaiter>,
    benchmark_reservation: Option<BenchmarkReservation>,
}

struct SchedulerWaiter {
    ticket: u64,
    priority: i64,
    enqueued_at: Instant,
    reserved_vram_mb: u64,
    uses_gpu: bool,
    qualified_video_overlap: bool,
    benchmark_token: Option<String>,
}

#[derive(Clone, Copy)]
struct VideoGpuReservation {
    vram_mb: u64,
    nvenc_sessions: u8,
    exclusive: bool,
}

struct BenchmarkReservation {
    token: String,
    engine: String,
    remaining_admissions: usize,
    expires_at: Instant,
}

fn new_inference_scheduler(max_workers: usize) -> InferenceScheduler {
    InferenceScheduler {
        active_workers: 0,
        active_gpu_workers: 0,
        active_qualified_video_overlap_workers: 0,
        active_engines: HashMap::new(),
        reserved_vram_mb: 0,
        video_reserved_vram_mb: 0,
        video_nvenc_sessions: 0,
        active_video_gpu_jobs: HashMap::new(),
        exclusive_video_job: None,
        available_vram_budget_mb: None,
        max_workers,
        next_ticket: 0,
        waiters: Vec::new(),
        benchmark_reservation: None,
    }
}

fn scheduler_rank(waiter: &SchedulerWaiter, now: Instant) -> (i64, std::cmp::Reverse<u64>) {
    let age_boost = (now.duration_since(waiter.enqueued_at).as_secs() / 30).min(3) as i64;
    (
        (waiter.priority + age_boost).min(3),
        std::cmp::Reverse(waiter.ticket),
    )
}

struct SchedulerLease {
    scheduler: Arc<(Mutex<InferenceScheduler>, Condvar)>,
    engine: String,
    reserved_vram_mb: u64,
    uses_gpu: bool,
    qualified_video_overlap: bool,
    benchmark_token: Option<String>,
}

struct GlobalVideoGpuGate {
    scheduler: Arc<(Mutex<InferenceScheduler>, Condvar)>,
    python_path: PathBuf,
    observation: Mutex<Option<GpuObservation>>,
}

#[derive(Clone, Copy)]
struct GpuObservation {
    observed_at: Instant,
    cuda: bool,
    total_mb: u64,
    available_mb: u64,
}

struct GlobalVideoGpuLease {
    scheduler: Arc<(Mutex<InferenceScheduler>, Condvar)>,
    job_id: String,
}

struct BatchCoordinatorLease {
    coordinators: Arc<Mutex<HashSet<String>>>,
    batch_id: String,
}

impl Drop for BatchCoordinatorLease {
    fn drop(&mut self) {
        if let Ok(mut coordinators) = self.coordinators.lock() {
            coordinators.remove(&self.batch_id);
        }
    }
}

impl Drop for SchedulerLease {
    fn drop(&mut self) {
        let (lock, changed) = &*self.scheduler;
        if let Ok(mut scheduler) = lock.lock() {
            scheduler.active_workers = scheduler.active_workers.saturating_sub(1);
            if self.uses_gpu {
                scheduler.active_gpu_workers = scheduler.active_gpu_workers.saturating_sub(1);
            }
            if self.qualified_video_overlap {
                scheduler.active_qualified_video_overlap_workers = scheduler
                    .active_qualified_video_overlap_workers
                    .saturating_sub(1);
            }
            if let Some(active) = scheduler.active_engines.get_mut(&self.engine) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    scheduler.active_engines.remove(&self.engine);
                }
            }
            scheduler.reserved_vram_mb = scheduler
                .reserved_vram_mb
                .saturating_sub(self.reserved_vram_mb);
            if scheduler.active_workers == 0 {
                scheduler.available_vram_budget_mb = None;
            }
            if let (Some(token), Some(benchmark)) = (
                self.benchmark_token.as_deref(),
                scheduler.benchmark_reservation.as_mut(),
            ) {
                if benchmark.token == token {
                    benchmark.remaining_admissions =
                        benchmark.remaining_admissions.saturating_sub(1);
                    if benchmark.remaining_admissions == 0 {
                        scheduler.benchmark_reservation = None;
                    }
                }
            }
            changed.notify_all();
        }
    }
}

impl GlobalVideoGpuGate {
    fn new(scheduler: Arc<(Mutex<InferenceScheduler>, Condvar)>, python_path: PathBuf) -> Self {
        Self {
            scheduler,
            python_path,
            observation: Mutex::new(None),
        }
    }

    fn gpu_observation(&self) -> GpuObservation {
        let mut cached = self
            .observation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(observation) = *cached {
            if observation.observed_at.elapsed() < Duration::from_millis(500) {
                return observation;
            }
        }
        let status = gpu_status(&self.python_path);
        let total_mb = status
            .get("vram_total_mb")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let used_mb = status
            .get("vram_used_mb")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let observation = GpuObservation {
            observed_at: Instant::now(),
            cuda: status.get("cuda_available").and_then(Value::as_bool) == Some(true),
            total_mb,
            available_mb: total_mb.saturating_sub(used_mb),
        };
        *cached = Some(observation);
        observation
    }
}

fn video_request_supports_qualified_whisper_overlap(
    request: &video::SharedGpuAdmissionRequest,
) -> bool {
    !request.exclusive
        && request.requested_nvenc_sessions == 1
        && u64::from(request.requested_vram_mb) <= QUALIFIED_VIDEO_OVERLAP_MAX_VIDEO_VRAM_MB
        && matches!(
            request.resource_class,
            video::ResourceClass::Medium | video::ResourceClass::Heavy
        )
}

fn active_video_jobs_support_qualified_whisper_overlap(scheduler: &InferenceScheduler) -> bool {
    scheduler.active_video_gpu_jobs.len() == 1
        && scheduler.video_nvenc_sessions == 1
        && scheduler.active_video_gpu_jobs.values().all(|reservation| {
            !reservation.exclusive
                && reservation.nvenc_sessions == 1
                && reservation.vram_mb <= QUALIFIED_VIDEO_OVERLAP_MAX_VIDEO_VRAM_MB
        })
}

fn request_supports_qualified_video_overlap(
    operation: &str,
    engine: &str,
    model_id: Option<&str>,
) -> bool {
    operation == "transcribe"
        && engine == QUALIFIED_VIDEO_OVERLAP_ENGINE
        && model_id == Some(QUALIFIED_VIDEO_OVERLAP_MODEL)
}

impl video::SharedGpuAdmissionGate for GlobalVideoGpuGate {
    fn try_acquire(
        &self,
        request: &video::SharedGpuAdmissionRequest,
    ) -> video::ServiceResult<video::SharedGpuAdmissionOutcome> {
        let observation = self.gpu_observation();
        let (lock, _) = &*self.scheduler;
        let mut scheduler = lock.lock().map_err(|_| {
            video::VideoServiceError::new(
                "video.gpu_scheduler_unavailable",
                "The shared GPU scheduler lock failed",
            )
            .retryable(true)
        })?;
        if scheduler
            .active_video_gpu_jobs
            .contains_key(&request.job_id)
        {
            return Err(video::VideoServiceError::new(
                "video.gpu_duplicate_lease",
                "The Video Studio task already owns shared GPU capacity",
            ));
        }

        let waiting = |reason: &str, details: Value| {
            video::SharedGpuAdmissionOutcome::Waiting(video::SharedGpuAdmissionWait {
                reason: reason.to_string(),
                retry_after_ms: 100,
                details: Some(details),
            })
        };
        if scheduler.exclusive_video_job.is_some() {
            return Ok(waiting(
                "another exclusive video workload is active",
                json!({ "exclusive_job": scheduler.exclusive_video_job }),
            ));
        }
        let qualified_active_inference = scheduler.active_gpu_workers == 1
            && scheduler.active_qualified_video_overlap_workers == 1
            && video_request_supports_qualified_whisper_overlap(request);
        if scheduler.active_gpu_workers > 0 && !qualified_active_inference {
            return Ok(waiting(
                "GPU inference is active outside the measured Whisper/NVENC overlap envelope",
                json!({
                    "active_gpu_inference_jobs": scheduler.active_gpu_workers,
                    "qualified_overlap_jobs": scheduler.active_qualified_video_overlap_workers,
                }),
            ));
        }
        if !scheduler.waiters.is_empty() {
            return Ok(waiting(
                "an earlier inference task is waiting for GPU capacity",
                json!({ "waiting_inference_jobs": scheduler.waiters.len() }),
            ));
        }
        if request.exclusive && !scheduler.active_video_gpu_jobs.is_empty() {
            return Ok(waiting(
                "exclusive GPU work requires all video workloads to finish",
                json!({ "active_video_jobs": scheduler.active_video_gpu_jobs.len() }),
            ));
        }
        let requested_vram_mb = u64::from(request.requested_vram_mb);
        let combined_vram_mb = scheduler
            .reserved_vram_mb
            .saturating_add(scheduler.video_reserved_vram_mb)
            .saturating_add(requested_vram_mb);
        if observation.cuda
            && (combined_vram_mb.saturating_add(SHARED_GPU_HEADROOM_MB) > observation.total_mb
                || requested_vram_mb.saturating_add(SHARED_GPU_HEADROOM_MB)
                    > observation.available_mb)
        {
            return Ok(waiting(
                "insufficient safe free VRAM",
                json!({
                    "requested_vram_mb": requested_vram_mb,
                    "reserved_vram_mb": scheduler.reserved_vram_mb + scheduler.video_reserved_vram_mb,
                    "available_vram_mb": observation.available_mb,
                    "headroom_mb": SHARED_GPU_HEADROOM_MB,
                }),
            ));
        }
        if scheduler
            .video_nvenc_sessions
            .saturating_add(request.requested_nvenc_sessions)
            > SHARED_NVENC_SESSION_LIMIT
        {
            return Ok(waiting(
                "the safe NVENC session envelope is full",
                json!({
                    "active_nvenc_sessions": scheduler.video_nvenc_sessions,
                    "limit": SHARED_NVENC_SESSION_LIMIT,
                }),
            ));
        }

        let reservation = VideoGpuReservation {
            vram_mb: requested_vram_mb,
            nvenc_sessions: request.requested_nvenc_sessions,
            exclusive: request.exclusive,
        };
        scheduler
            .active_video_gpu_jobs
            .insert(request.job_id.clone(), reservation);
        scheduler.video_reserved_vram_mb = scheduler
            .video_reserved_vram_mb
            .saturating_add(reservation.vram_mb);
        scheduler.video_nvenc_sessions = scheduler
            .video_nvenc_sessions
            .saturating_add(reservation.nvenc_sessions);
        if reservation.exclusive {
            scheduler.exclusive_video_job = Some(request.job_id.clone());
        }
        Ok(video::SharedGpuAdmissionOutcome::admitted(
            GlobalVideoGpuLease {
                scheduler: Arc::clone(&self.scheduler),
                job_id: request.job_id.clone(),
            },
        ))
    }
}

impl video::SharedGpuAdmissionLease for GlobalVideoGpuLease {}

impl Drop for GlobalVideoGpuLease {
    fn drop(&mut self) {
        let (lock, changed) = &*self.scheduler;
        if let Ok(mut scheduler) = lock.lock() {
            if let Some(reservation) = scheduler.active_video_gpu_jobs.remove(&self.job_id) {
                scheduler.video_reserved_vram_mb = scheduler
                    .video_reserved_vram_mb
                    .saturating_sub(reservation.vram_mb);
                scheduler.video_nvenc_sessions = scheduler
                    .video_nvenc_sessions
                    .saturating_sub(reservation.nvenc_sessions);
                if scheduler.exclusive_video_job.as_deref() == Some(self.job_id.as_str()) {
                    scheduler.exclusive_video_job = None;
                }
                if scheduler.active_workers == 0 && scheduler.active_video_gpu_jobs.is_empty() {
                    scheduler.available_vram_budget_mb = None;
                }
            }
            changed.notify_all();
        }
    }
}

struct ActiveApiServer {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct ActiveRecording {
    device_name: String,
    output_path: PathBuf,
    sample_rate: u32,
    channels: u16,
    frames: Arc<AtomicU64>,
    peak_bits: Arc<AtomicU32>,
    queued_frames: Arc<AtomicU64>,
    dropped_frames: Arc<AtomicU64>,
    speech_active: Arc<AtomicBool>,
    speech_detected: Arc<AtomicBool>,
    speech_frames: Arc<AtomicU64>,
    silence_frames: Arc<AtomicU64>,
    noise_floor_bits: Arc<AtomicU32>,
    auto_stopped: Arc<AtomicBool>,
    vad_enabled: bool,
    auto_stop: bool,
    silence_ms: u64,
    input_gain: f32,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

struct ActivePlayback {
    device_name: String,
    audio_path: PathBuf,
    duration_seconds: f64,
    output_sample_rate: u32,
    played_frames: Arc<AtomicU64>,
    underrun_frames: Arc<AtomicU64>,
    completed: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
    started_at: Instant,
    startup_seconds: f64,
}

#[derive(Clone, Copy, Debug)]
struct VadSnapshot {
    speech_active: bool,
    speech_detected: bool,
    speech_frames: u64,
    silence_frames: u64,
    noise_floor: f32,
}

struct VoiceActivityDetector {
    sample_rate: u32,
    window_frames: u64,
    window_sum_squares: f64,
    window_samples: u64,
    noise_floor: f32,
    voice_windows: u8,
    quiet_windows: u8,
    speech_active: bool,
    speech_detected: bool,
    speech_frames: u64,
    silence_frames: u64,
}

impl VoiceActivityDetector {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            window_frames: (u64::from(sample_rate) / 100).max(1),
            window_sum_squares: 0.0,
            window_samples: 0,
            noise_floor: 0.003,
            voice_windows: 0,
            quiet_windows: 0,
            speech_active: false,
            speech_detected: false,
            speech_frames: 0,
            silence_frames: 0,
        }
    }

    fn process(&mut self, samples: &[f32]) -> VadSnapshot {
        for sample in samples {
            self.window_sum_squares += f64::from(*sample) * f64::from(*sample);
            self.window_samples += 1;
            if self.window_samples < self.window_frames {
                continue;
            }
            let rms = (self.window_sum_squares / self.window_samples as f64).sqrt() as f32;
            let threshold = (self.noise_floor * 3.5).clamp(0.012, 0.08);
            if rms >= threshold {
                self.voice_windows = self.voice_windows.saturating_add(1);
                self.quiet_windows = 0;
                self.silence_frames = 0;
                self.speech_frames += self.window_samples;
                if self.voice_windows >= 2 {
                    self.speech_active = true;
                    self.speech_detected = true;
                }
            } else {
                self.voice_windows = 0;
                self.quiet_windows = self.quiet_windows.saturating_add(1);
                if self.speech_detected {
                    self.silence_frames += self.window_samples;
                }
                if self.quiet_windows >= 20 {
                    self.speech_active = false;
                }
                if !self.speech_active {
                    self.noise_floor = (self.noise_floor * 0.95 + rms * 0.05).clamp(0.000_1, 0.025);
                }
            }
            self.window_sum_squares = 0.0;
            self.window_samples = 0;
        }
        self.snapshot()
    }

    fn snapshot(&self) -> VadSnapshot {
        VadSnapshot {
            speech_active: self.speech_active,
            speech_detected: self.speech_detected,
            speech_frames: self.speech_frames,
            silence_frames: self.silence_frames,
            noise_floor: self.noise_floor,
        }
    }

    fn should_auto_stop(&self, silence_ms: u64) -> bool {
        self.speech_detected
            && self.silence_frames.saturating_mul(1_000)
                >= u64::from(self.sample_rate).saturating_mul(silence_ms)
    }
}

impl RuntimeState {
    fn foundation_runtime_ready(&self) -> bool {
        foundation_runtime_ready(&self.python_path)
    }

    fn new(runtime_root: PathBuf, python_path: PathBuf) -> Result<Self, String> {
        let store = Store::open(product_state_dir(), home_dir().join(".soundAr/exports"))?;
        Ok(Self::new_with_store(runtime_root, python_path, store))
    }

    fn new_with_store(runtime_root: PathBuf, python_path: PathBuf, store: Store) -> Self {
        Self::new_with_store_and_registry(
            runtime_root,
            python_path,
            home_dir().join(".soundAr/state/models.json"),
            store,
        )
    }

    fn new_with_store_and_registry(
        runtime_root: PathBuf,
        python_path: PathBuf,
        model_registry_path: PathBuf,
        store: Store,
    ) -> Self {
        let max_workers = env::var("SOUNDAR_MAX_PARALLEL_JOBS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4)
            .clamp(1, 8);
        let store = Arc::new(store);
        let scheduler = Arc::new((
            Mutex::new(new_inference_scheduler(max_workers)),
            Condvar::new(),
        ));
        let shared_gpu_gate: Arc<dyn video::SharedGpuAdmissionGate> = Arc::new(
            GlobalVideoGpuGate::new(Arc::clone(&scheduler), python_path.clone()),
        );
        let video = Arc::new(
            video::VideoStudioService::new_with_gpu_admission_gate(
                Arc::clone(&store),
                shared_gpu_gate,
            )
            .expect("Store initialization guarantees writable video artifact storage"),
        );
        Self {
            worker_pool: Arc::new(Mutex::new(Vec::new())),
            scheduler,
            setup_lock: Arc::new(Mutex::new(())),
            model_operation_lock: Arc::new(Mutex::new(())),
            active_download: Arc::new(Mutex::new(None)),
            active_syntheses: Arc::new(Mutex::new(HashMap::new())),
            active_batch_coordinators: Arc::new(Mutex::new(HashSet::new())),
            api_server: Arc::new(Mutex::new(None)),
            active_recording: Arc::new(Mutex::new(None)),
            active_playback: Arc::new(Mutex::new(None)),
            runtime_root,
            python_path,
            model_registry_path,
            store,
            video,
            worker_probe_after: env::var("SOUNDAR_WORKER_PROBE_AFTER_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_secs(30)),
            codex_agent: Arc::new(codex_agent::CodexAgentState::with_video_dispatcher(
                video_commands::dispatch_video_operation,
            )),
        }
    }

    fn engine_python_path(&self, engine: &str) -> (PathBuf, bool) {
        let engine_root = app_data_dir().join("engines").join(engine);
        let isolated = engine_root.join(".venv/bin/python");
        let manifest = read_json(engine_root.join("runtime.json"), json!({}));
        let current = cfg!(debug_assertions)
            || (manifest.get("schema_version").and_then(Value::as_u64) == Some(2)
                && manifest.get("foundation_schema").and_then(Value::as_u64) == Some(2));
        if isolated.is_file() && current {
            (isolated, true)
        } else {
            (self.python_path.clone(), false)
        }
    }

    fn request_engine(&self, request: &Value) -> Result<String, String> {
        let operation = request
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("synthesize");
        if operation == "health" {
            if let Some(engine) = request.get("requested_engine").and_then(Value::as_str) {
                if engine != "foundation" {
                    validate_engine_argument(engine)?;
                }
                return Ok(engine.to_string());
            }
            return Ok("foundation".to_string());
        }
        if matches!(
            operation,
            "capabilities"
                | "analyze_audio"
                | "prepare_voice_reference"
                | "prepare_transcription_audio"
                | "master_audio"
        ) {
            return Ok("foundation".to_string());
        }
        let model_id = request
            .get("model_id")
            .and_then(Value::as_str)
            .ok_or("An inference model is required")?;
        let registry = read_json(self.model_registry_path.clone(), json!({ "models": [] }));
        registry
            .get("models")
            .and_then(Value::as_array)
            .and_then(|models| {
                models.iter().find_map(|model| {
                    (model.get("model_id").and_then(Value::as_str) == Some(model_id))
                        .then(|| model.get("engine").and_then(Value::as_str))
                        .flatten()
                })
            })
            .map(str::to_string)
            .ok_or_else(|| format!("The installed model registry has no engine for {model_id}"))
    }

    fn stop_process(process: &mut PythonProcess) {
        let _ = process.child.kill();
        let _ = process.child.wait();
    }

    fn return_idle_process(&self, mut process: PythonProcess) {
        if let Ok(mut pool) = self.worker_pool.lock() {
            pool.push(process);
        } else {
            Self::stop_process(&mut process);
        }
    }

    fn record_worker_start(&self, engine: &str) {
        let recovering = self.store.engine_needs_recovery(engine).unwrap_or(false);
        let _ = self
            .store
            .record_engine_event(engine, "started", "worker_started");
        if recovering {
            self.record_worker_recovery(engine);
        }
    }

    fn record_worker_failure(&self, engine: &str, error: &str) {
        let normalized = error.to_ascii_lowercase();
        let detail = if normalized.contains("out of memory")
            || normalized.contains("cuda oom")
            || normalized.contains("device-side assert")
        {
            "gpu_memory_failure"
        } else if error.contains("deadline") {
            "deadline_exceeded"
        } else if error.contains("response limit") || error.contains("8 MB") {
            "response_limit_exceeded"
        } else if error.contains("liveness") || error.contains("Liveness") {
            "liveness_probe_failed"
        } else if error.contains("UTF-8")
            || error.contains("malformed")
            || error.contains("Invalid worker response")
        {
            "invalid_response"
        } else if error.contains("send") {
            "request_write_failed"
        } else {
            "process_exited"
        };
        let _ = self.store.record_engine_event(engine, "failed", detail);
    }

    fn record_worker_recovery(&self, engine: &str) {
        let _ = self
            .store
            .record_engine_event(engine, "recovered", "worker_recovered");
    }

    fn worker_health_snapshot(&self, engine: &str, mut health: Value) -> Result<Value, String> {
        let warm_workers = self
            .worker_pool
            .lock()
            .map_err(|_| "Python worker pool lock failed")?
            .iter()
            .filter(|process| process.engine == engine)
            .count();
        let mut loaded_models = self
            .worker_pool
            .lock()
            .map_err(|_| "Python worker pool lock failed")?
            .iter()
            .filter(|process| process.engine == engine)
            .flat_map(|process| process.loaded_models.iter().cloned())
            .collect::<Vec<_>>();
        loaded_models.sort();
        loaded_models.dedup();
        let summary = self.store.engine_event_summary(engine)?;
        let object = health
            .as_object_mut()
            .ok_or("Engine health response must be an object")?;
        object.insert("warm_workers".to_string(), json!(warm_workers));
        object.insert("loaded_models".to_string(), json!(loaded_models));
        for key in [
            "worker_starts",
            "worker_restarts",
            "worker_failures",
            "last_started_at",
            "last_failure_at",
            "last_error",
        ] {
            object.insert(
                key.to_string(),
                summary.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        Ok(health)
    }

    fn check_engine_health(&self, engine: &str) -> Result<Value, String> {
        let request = json!({ "operation": "health", "requested_engine": engine });
        let health = match self.request(request.clone()) {
            Ok(health) => health,
            Err(first_error) => {
                let recovered = self.request(request).map_err(|second_error| format!(
                    "Engine health failed twice. First failure: {first_error}. Recovery failure: {second_error}"
                ))?;
                recovered
            }
        };
        self.worker_health_snapshot(engine, health)
    }

    fn start_process(&self, engine: &str) -> Result<PythonProcess, String> {
        let (python_path, isolated) = self.engine_python_path(engine);
        let mut command = Command::new(&python_path);
        command
            .arg(self.runtime_root.join("bridge.py"))
            .arg("--serve")
            .current_dir(&self.runtime_root)
            .env("PYTHONPATH", &self.runtime_root)
            .env("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
            .env("TOKENIZERS_PARALLELISM", "false")
            .env("SOUNDAR_ENGINE_SCOPE", engine)
            .env(
                "SOUNDAR_ENGINE_RUNTIME",
                if isolated { "layered" } else { "legacy-shared" },
            );
        if engine == "transformers" {
            if let Some(library_path) = managed_cuda_library_path(&app_data_dir())? {
                command.env("LD_LIBRARY_PATH", library_path);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| {
                format!(
                    "Could not start the Python runtime at {}: {error}. Run install-linux.sh to install the local inference runtime.",
                    python_path.display()
                )
            })?;
        let stdin = child.stdin.take().ok_or("Python stdin is unavailable")?;
        let stdout = child.stdout.take().ok_or("Python stdout is unavailable")?;
        self.record_worker_start(engine);
        Ok(PythonProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            engine: engine.to_string(),
            loaded_models: Vec::new(),
            last_response_at: Instant::now(),
        })
    }

    fn probe_worker(&self, process: &mut PythonProcess) -> Result<(), String> {
        let payload = json!({
            "operation": "health",
            "requested_engine": process.engine,
        })
        .to_string();
        writeln!(process.stdin, "{payload}")
            .and_then(|_| process.stdin.flush())
            .map_err(|error| format!("Could not send worker liveness probe: {error}"))?;
        wait_for_worker_output(process, 30)?;
        let mut bytes = Vec::new();
        let count = process
            .stdout
            .by_ref()
            .take(MAX_RPC_RESPONSE_BYTES + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|error| format!("Could not read worker liveness response: {error}"))?;
        if count == 0 || bytes.len() as u64 > MAX_RPC_RESPONSE_BYTES {
            return Err("The worker returned an invalid liveness response".to_string());
        }
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("The worker returned malformed liveness data: {error}"))?;
        if response.get("ok").and_then(Value::as_bool) != Some(true)
            || response.pointer("/result/status").and_then(Value::as_str) != Some("ready")
        {
            return Err(response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("The worker did not report ready")
                .to_string());
        }
        process.last_response_at = Instant::now();
        Ok(())
    }

    fn request(&self, request: Value) -> Result<Value, String> {
        self.request_internal(request, None)
    }

    fn request_for_job(&self, request: Value, job_id: &str) -> Result<Value, String> {
        self.request_internal(request, Some(job_id))
    }

    fn request_internal(&self, mut request: Value, job_id: Option<&str>) -> Result<Value, String> {
        let runtime_started = Instant::now();
        let requested_engine = self.request_engine(&request)?;
        let priority = priority_value(request.get("priority"))?;
        let benchmark_token = request
            .get("benchmark_token")
            .and_then(Value::as_str)
            .map(str::to_string);
        let operation = request
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("synthesize")
            .to_string();
        let qualified_video_overlap = request_supports_qualified_video_overlap(
            &operation,
            &requested_engine,
            request.get("model_id").and_then(Value::as_str),
        );
        if let (Some(job_id), Some(object)) = (job_id, request.as_object_mut()) {
            object.insert("_job_id".to_string(), json!(job_id));
        }
        let request_payload = request.to_string();
        if request_payload.len() > MAX_RPC_REQUEST_BYTES {
            return Err("The inference request exceeds the 1 MB local RPC limit".to_string());
        }
        let (_lease, mut existing) = self.acquire_scheduler_slot(
            &requested_engine,
            job_id,
            priority,
            benchmark_token.as_deref(),
            qualified_video_overlap,
        )?;
        let mut worker_state = if existing.is_some() { "warm" } else { "cold" };
        if let Some(job_id) = job_id {
            let status = match self.store.start_job(job_id) {
                Ok(status) => status,
                Err(error) => {
                    if let Some(process) = existing.take() {
                        self.return_idle_process(process);
                    }
                    return Err(error);
                }
            };
            if status != "running" {
                if let Some(process) = existing.take() {
                    self.return_idle_process(process);
                }
                return Err(if status == "cancelled" {
                    "Generation cancelled while waiting for a worker".to_string()
                } else {
                    format!("The job cannot start from its {status} state")
                });
            }
        }
        let mut process = match existing {
            Some(process) => process,
            None => {
                let status = gpu_status(&self.python_path);
                let cuda = status.get("cuda_available").and_then(Value::as_bool) == Some(true);
                let available = status
                    .get("vram_total_mb")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .saturating_sub(
                        status
                            .get("vram_used_mb")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                let required = self.engine_minimum_vram(&requested_engine)?;
                if cuda && cold_load_needs_idle_reclamation(available, required) {
                    let mut pool = self
                        .worker_pool
                        .lock()
                        .map_err(|_| "Python worker pool lock failed")?;
                    for mut idle in pool.drain(..) {
                        Self::stop_process(&mut idle);
                    }
                }
                self.start_process(&requested_engine)?
            }
        };
        if operation != "health" && process.last_response_at.elapsed() >= self.worker_probe_after {
            if let Err(error) = self.probe_worker(&mut process) {
                self.record_worker_failure(
                    &requested_engine,
                    &format!("Liveness probe failed: {error}"),
                );
                Self::stop_process(&mut process);
                process = self.start_process(&requested_engine)?;
                worker_state = "cold";
            }
        }
        if let Some(job_id) = job_id {
            let mut active = match self.active_syntheses.lock() {
                Ok(active) => active,
                Err(_) => {
                    Self::stop_process(&mut process);
                    return Err("Synthesis cancellation lock failed".to_string());
                }
            };
            let status = match self.store.job_status(job_id) {
                Ok(status) => status,
                Err(error) => {
                    drop(active);
                    Self::stop_process(&mut process);
                    return Err(error);
                }
            };
            if status.as_deref() == Some("cancelled") {
                drop(active);
                self.return_idle_process(process);
                return Err("Generation cancelled before inference started".to_string());
            }
            active.insert(
                job_id.to_string(),
                ActiveSynthesis {
                    process_id: process.child.id(),
                },
            );
        }
        if let Err(error) =
            writeln!(process.stdin, "{request_payload}").and_then(|_| process.stdin.flush())
        {
            if let Some(job_id) = job_id {
                if let Ok(mut active) = self.active_syntheses.lock() {
                    active.remove(job_id);
                }
            }
            self.record_worker_failure(
                &requested_engine,
                &format!("Could not send the inference request: {error}"),
            );
            Self::stop_process(&mut process);
            return Err(format!("Could not send the inference request: {error}"));
        }
        let timeout_seconds = match operation.as_str() {
            "health" | "capabilities" => 30,
            "analyze_audio"
            | "prepare_voice_reference"
            | "prepare_transcription_audio"
            | "master_audio"
            | "compare_speakers" => 120,
            "transcribe" | "synthesize" | "generate_music" | "diarize" | "align_transcript" => {
                1_800
            }
            _ => 300,
        };
        let read_result = (|| {
            let deadline = Instant::now() + Duration::from_secs(timeout_seconds as u64);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(format!(
                        "The {requested_engine} worker exceeded its {timeout_seconds} second deadline"
                    ));
                }
                wait_for_worker_output(
                    &mut process,
                    remaining.as_secs().max(1).min(i32::MAX as u64) as i32,
                )?;
                let mut bytes = Vec::new();
                let count = process
                    .stdout
                    .by_ref()
                    .take(MAX_RPC_RESPONSE_BYTES + 1)
                    .read_until(b'\n', &mut bytes)
                    .map_err(|error| format!("Could not read the inference response: {error}"))?;
                if count == 0 {
                    let status = process.child.try_wait().ok().flatten();
                    return Err(format!(
                        "The {requested_engine} worker exited without a response{}",
                        status
                            .map(|value| format!(" ({value})"))
                            .unwrap_or_default()
                    ));
                }
                if bytes.len() as u64 > MAX_RPC_RESPONSE_BYTES {
                    return Err("The inference worker exceeded the 8 MB response limit".to_string());
                }
                let line = String::from_utf8(bytes)
                    .map_err(|_| "The inference worker returned non-UTF-8 output".to_string())?;
                let message: Value = serde_json::from_str(&line).map_err(|error| {
                    format!("The inference worker stopped unexpectedly: {error}")
                })?;
                if let Some(event) = message.get("event") {
                    if event.get("type").and_then(Value::as_str) == Some("audio-preview") {
                        let job_id = job_id
                            .ok_or("The worker sent an audio preview outside a durable job")?;
                        self.store.update_job_preview(
                            job_id,
                            event
                                .get("audio_path")
                                .and_then(Value::as_str)
                                .ok_or("The worker preview has no audio path")?,
                            event
                                .get("duration_seconds")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                            event
                                .get("first_audio_seconds")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                            event
                                .get("progress")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.78),
                        )?;
                    }
                    continue;
                }
                return Ok(line);
            }
        })();
        if let Some(job_id) = job_id {
            if let Ok(mut active) = self.active_syntheses.lock() {
                active.remove(job_id);
            }
        }
        let line = match read_result {
            Ok(line) => line,
            Err(error) => {
                let cancelled = job_id
                    .and_then(|id| self.store.job_status(id).ok().flatten())
                    .as_deref()
                    == Some("cancelled");
                if !cancelled {
                    self.record_worker_failure(&requested_engine, &error);
                }
                Self::stop_process(&mut process);
                return Err(if cancelled {
                    "Task cancelled during inference".to_string()
                } else {
                    error
                });
            }
        };
        let response: Value = match serde_json::from_str(&line) {
            Ok(response) => response,
            Err(error) => {
                self.record_worker_failure(
                    &requested_engine,
                    &format!("Invalid worker response: {error}"),
                );
                Self::stop_process(&mut process);
                return Err(format!(
                    "The inference worker stopped unexpectedly: {error}"
                ));
            }
        };
        process.last_response_at = Instant::now();
        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            let mut result = response.get("result").cloned().unwrap_or(Value::Null);
            if operation == "health" {
                process.loaded_models = result
                    .get("loaded_models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
            } else if matches!(
                operation.as_str(),
                "load"
                    | "synthesize"
                    | "generate_music"
                    | "transcribe"
                    | "compare_speakers"
                    | "diarize"
                    | "align_transcript"
            ) {
                if let Some(model_id) = result.get("model_id").and_then(Value::as_str) {
                    process.loaded_models = vec![model_id.to_string()];
                }
            }
            if matches!(operation.as_str(), "synthesize" | "generate_music") {
                let elapsed = runtime_started.elapsed().as_secs_f64();
                let inference = result
                    .get("inference_seconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                if let Some(object) = result.as_object_mut() {
                    object.insert("runtime_worker_state".to_string(), json!(worker_state));
                    object.insert("end_to_end_seconds".to_string(), json!(elapsed));
                    object.insert(
                        "runtime_overhead_seconds".to_string(),
                        json!((elapsed - inference).max(0.0)),
                    );
                }
            }
            self.return_idle_process(process);
            Ok(result)
        } else {
            let error = response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Synthesis failed")
                .to_string();
            let normalized = error.to_ascii_lowercase();
            let gpu_memory_failure = normalized.contains("out of memory")
                || normalized.contains("cuda oom")
                || normalized.contains("device-side assert");
            if gpu_memory_failure {
                self.record_worker_failure(&requested_engine, &error);
                Self::stop_process(&mut process);
            } else if operation == "load" {
                process.loaded_models.clear();
                let _ = self.store.record_engine_event(
                    &requested_engine,
                    "failed",
                    "model_load_failed",
                );
                Self::stop_process(&mut process);
            } else {
                self.return_idle_process(process);
            }
            Err(error)
        }
    }

    fn engine_minimum_vram(&self, engine: &str) -> Result<u64, String> {
        if engine == "foundation" {
            return Ok(0);
        }
        let capabilities = read_json(
            self.runtime_root.join("data/engine_manifests.json"),
            json!({ "engines": [] }),
        );
        capabilities
            .get("engines")
            .and_then(Value::as_array)
            .and_then(|engines| {
                engines.iter().find_map(|entry| {
                    (entry.get("id").and_then(Value::as_str) == Some(engine))
                        .then(|| entry.get("minimum_vram_mb").and_then(Value::as_u64))
                        .flatten()
                })
            })
            .ok_or_else(|| format!("No GPU envelope is registered for {engine}"))
    }

    fn scheduler_status(&self) -> Result<Value, String> {
        let scheduler = self
            .scheduler
            .0
            .lock()
            .map_err(|_| "GPU scheduler lock failed")?;
        let active_batches = self
            .active_batch_coordinators
            .lock()
            .map_err(|_| "Batch coordinator lock failed")?
            .len();
        Ok(json!({
            "active_workers": scheduler.active_workers,
            "active_gpu_workers": scheduler.active_gpu_workers,
            "active_qualified_video_overlap_workers": scheduler.active_qualified_video_overlap_workers,
            "max_workers": scheduler.max_workers,
            "reserved_vram_mb": scheduler.reserved_vram_mb.saturating_add(scheduler.video_reserved_vram_mb),
            "inference_reserved_vram_mb": scheduler.reserved_vram_mb,
            "video_reserved_vram_mb": scheduler.video_reserved_vram_mb,
            "active_video_gpu_jobs": scheduler.active_video_gpu_jobs.len(),
            "video_nvenc_sessions": scheduler.video_nvenc_sessions,
            "exclusive_video_job": scheduler.exclusive_video_job,
            "available_vram_budget_mb": scheduler.available_vram_budget_mb,
            "active_batches": active_batches,
            "waiting_jobs": scheduler.waiters.len(),
            "benchmark_reserved": scheduler.benchmark_reservation.is_some(),
            "engine_limits": {
                "fish-speech": env::var("SOUNDAR_FISH_MAX_PARALLEL_JOBS")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, scheduler.max_workers),
            },
        }))
    }

    fn acquire_scheduler_slot(
        &self,
        engine: &str,
        job_id: Option<&str>,
        priority: i64,
        benchmark_token: Option<&str>,
        qualified_video_overlap: bool,
    ) -> Result<(SchedulerLease, Option<PythonProcess>), String> {
        let model_reservation = self.engine_minimum_vram(engine)?;
        let (lock, changed) = &*self.scheduler;
        let mut scheduler = lock.lock().map_err(|_| "GPU scheduler lock failed")?;
        if scheduler
            .benchmark_reservation
            .as_ref()
            .is_some_and(|reservation| reservation.expires_at <= Instant::now())
        {
            scheduler.benchmark_reservation = None;
        }
        match (benchmark_token, scheduler.benchmark_reservation.as_ref()) {
            (Some(token), Some(reservation))
                if token == reservation.token && engine == reservation.engine => {}
            (Some(_), _) => {
                return Err("The benchmark reservation is invalid or expired".to_string());
            }
            (None, _) => {}
        }
        let ticket = scheduler.next_ticket;
        scheduler.next_ticket = scheduler.next_ticket.saturating_add(1);
        scheduler.waiters.push(SchedulerWaiter {
            ticket,
            priority,
            enqueued_at: Instant::now(),
            reserved_vram_mb: model_reservation,
            uses_gpu: model_reservation > 0,
            qualified_video_overlap,
            benchmark_token: benchmark_token.map(str::to_string),
        });
        loop {
            if let Some(job_id) = job_id {
                let status = match self.store.job_status(job_id) {
                    Ok(status) => status,
                    Err(error) => {
                        scheduler.waiters.retain(|waiter| waiter.ticket != ticket);
                        changed.notify_all();
                        return Err(error);
                    }
                };
                if status.as_deref() == Some("cancelled") {
                    scheduler.waiters.retain(|waiter| waiter.ticket != ticket);
                    changed.notify_all();
                    return Err("Generation cancelled while waiting for a worker".to_string());
                }
            }
            let mut pool = match self.worker_pool.lock() {
                Ok(pool) => pool,
                Err(_) => {
                    scheduler.waiters.retain(|waiter| waiter.ticket != ticket);
                    changed.notify_all();
                    return Err("Python worker pool lock failed".to_string());
                }
            };
            let matching_worker = pool.iter().position(|process| process.engine == engine);
            let reservation = if matching_worker.is_some() {
                0
            } else {
                model_reservation
            };
            if let Some(waiter) = scheduler
                .waiters
                .iter_mut()
                .find(|waiter| waiter.ticket == ticket)
            {
                waiter.reserved_vram_mb = reservation;
            }
            let status = gpu_status(&self.python_path);
            let cuda = status.get("cuda_available").and_then(Value::as_bool) == Some(true);
            let total = status
                .get("vram_total_mb")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let used = status
                .get("vram_used_mb")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let available = total.saturating_sub(used);
            let budget = *scheduler.available_vram_budget_mb.get_or_insert(available);
            let engine_limit = if engine == "fish-speech" {
                env::var("SOUNDAR_FISH_MAX_PARALLEL_JOBS")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, scheduler.max_workers)
            } else {
                scheduler.max_workers
            };
            let active_for_engine = scheduler.active_engines.get(engine).copied().unwrap_or(0);
            let video_overlap_available = scheduler.active_video_gpu_jobs.is_empty()
                || (qualified_video_overlap
                    && scheduler.active_gpu_workers == 0
                    && active_video_jobs_support_qualified_whisper_overlap(&scheduler));
            let capacity = scheduler.active_workers < scheduler.max_workers
                && active_for_engine < engine_limit
                && (model_reservation == 0 || video_overlap_available);
            if cuda && reservation > 0 && total < reservation {
                scheduler.waiters.retain(|waiter| waiter.ticket != ticket);
                changed.notify_all();
                return Err(format!(
                    "{engine} requires at least {reservation} MB VRAM, but this GPU reports {total} MB"
                ));
            }
            let can_reclaim_idle_memory =
                scheduler.active_workers == 0 && scheduler.active_video_gpu_jobs.is_empty();
            let total_reserved_vram = scheduler
                .reserved_vram_mb
                .saturating_add(scheduler.video_reserved_vram_mb);
            let memory = !cuda
                || reservation == 0
                || can_reclaim_idle_memory
                || budget >= total_reserved_vram.saturating_add(reservation);
            let now = Instant::now();
            let available_memory = budget.saturating_sub(total_reserved_vram);
            let next_ticket = scheduler
                .waiters
                .iter()
                .filter(|waiter| {
                    let reservation_match =
                        scheduler
                            .benchmark_reservation
                            .as_ref()
                            .is_none_or(|reservation| {
                                waiter.benchmark_token.as_deref() == Some(&reservation.token)
                                    && engine == reservation.engine
                            });
                    reservation_match
                        && (!waiter.uses_gpu
                            || scheduler.active_video_gpu_jobs.is_empty()
                            || (waiter.qualified_video_overlap
                                && scheduler.active_gpu_workers == 0
                                && active_video_jobs_support_qualified_whisper_overlap(&scheduler)))
                        && (!cuda
                            || waiter.reserved_vram_mb == 0
                            || can_reclaim_idle_memory
                            || waiter.reserved_vram_mb <= available_memory)
                })
                .max_by_key(|waiter| scheduler_rank(waiter, now))
                .map(|waiter| waiter.ticket);
            if capacity && memory && next_ticket == Some(ticket) {
                scheduler.waiters.retain(|waiter| waiter.ticket != ticket);
                scheduler.active_workers += 1;
                if model_reservation > 0 && cuda {
                    scheduler.active_gpu_workers += 1;
                }
                if model_reservation > 0 && cuda && qualified_video_overlap {
                    scheduler.active_qualified_video_overlap_workers += 1;
                }
                *scheduler
                    .active_engines
                    .entry(engine.to_string())
                    .or_insert(0) += 1;
                scheduler.reserved_vram_mb = scheduler.reserved_vram_mb.saturating_add(reservation);
                let process = matching_worker.map(|index| pool.swap_remove(index));
                drop(pool);
                changed.notify_all();
                return Ok((
                    SchedulerLease {
                        scheduler: Arc::clone(&self.scheduler),
                        engine: engine.to_string(),
                        reserved_vram_mb: reservation,
                        uses_gpu: model_reservation > 0 && cuda,
                        qualified_video_overlap: model_reservation > 0
                            && cuda
                            && qualified_video_overlap,
                        benchmark_token: benchmark_token.map(str::to_string),
                    },
                    process,
                ));
            }
            drop(pool);
            scheduler = changed
                .wait_timeout(scheduler, std::time::Duration::from_millis(250))
                .map_err(|_| "GPU scheduler wait failed")?
                .0;
        }
    }

    fn cancel_job(&self, job_id: &str) -> Result<bool, String> {
        let active = self
            .active_syntheses
            .lock()
            .map_err(|_| "Synthesis cancellation lock failed")?;
        let process_id = active.get(job_id).map(|active| active.process_id);
        let durable_cancelled = self.store.cancel_job(job_id)?;
        drop(active);
        let Some(process_id) = process_id else {
            return Ok(durable_cancelled);
        };
        let status = Command::new("kill")
            .args(["-TERM", &process_id.to_string()])
            .status()
            .map_err(|error| format!("Could not stop the inference worker: {error}"))?;
        if !status.success() {
            return Err("The inference worker did not accept cancellation".to_string());
        }
        Ok(true)
    }

    fn cancel_all_active_syntheses(&self) -> Result<bool, String> {
        let job_ids = self
            .active_syntheses
            .lock()
            .map_err(|_| "Synthesis cancellation lock failed")?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut cancelled = false;
        for job_id in job_ids {
            cancelled |= self.cancel_job(&job_id)?;
        }
        Ok(cancelled)
    }

    fn execute_comparison(&self, comparison_id: &str) -> Result<Value, String> {
        let plan = self.store.comparison_execution_plan(comparison_id)?;
        let mut handles = Vec::with_capacity(plan.len());
        for (take_id, job_id, request) in plan {
            let runtime = self.clone();
            let thread_job_id = job_id.clone();
            match std::thread::Builder::new()
                .name(format!(
                    "soundar-compare-{}",
                    &take_id[..take_id.len().min(8)]
                ))
                .spawn(move || {
                    runtime
                        .request_for_job(request.clone(), &thread_job_id)
                        .and_then(|result| {
                            runtime
                                .store
                                .complete_synthesis(&thread_job_id, &request, &result)
                        })
                }) {
                Ok(handle) => handles.push((take_id, job_id, handle)),
                Err(error) => {
                    let error = format!("Could not start comparison take: {error}");
                    self.store.fail_job(&job_id, &error)?;
                    self.store.finish_comparison_take(
                        comparison_id,
                        &take_id,
                        None,
                        Some(&error),
                    )?;
                }
            }
        }
        for (take_id, job_id, handle) in handles {
            let outcome = handle
                .join()
                .unwrap_or_else(|_| Err("A comparison worker stopped unexpectedly".to_string()));
            match outcome {
                Ok(history) => {
                    if let Some(history_id) = history.get("id").and_then(Value::as_str) {
                        self.store.finish_comparison_take(
                            comparison_id,
                            &take_id,
                            Some(history_id),
                            None,
                        )?;
                    } else {
                        let error = "A comparison result did not include a history ID";
                        self.store.finish_comparison_take(
                            comparison_id,
                            &take_id,
                            None,
                            Some(error),
                        )?;
                    }
                }
                Err(error) => {
                    let cancelled = self.store.job_status(&job_id)?.as_deref() == Some("cancelled");
                    if !cancelled {
                        self.store.fail_job(&job_id, &error)?;
                    }
                    self.store.finish_comparison_take(
                        comparison_id,
                        &take_id,
                        None,
                        Some(if cancelled { "cancelled" } else { &error }),
                    )?;
                }
            }
        }
        self.store
            .get_comparison(comparison_id)?
            .ok_or_else(|| "The completed comparison was not found".to_string())
    }

    fn cancel_comparison(&self, comparison_id: &str) -> Result<bool, String> {
        let jobs = self.store.comparison_active_jobs(comparison_id)?;
        let mut cancelled = false;
        for (take_id, job_id) in jobs {
            cancelled |= self.cancel_job(&job_id)?;
            self.store
                .finish_comparison_take(comparison_id, &take_id, None, Some("cancelled"))?;
        }
        Ok(cancelled)
    }

    fn start_background_synthesis(&self, job_id: String, request: Value) -> Result<(), String> {
        if let Some(reference) = request.get("reference_audio_path").and_then(Value::as_str) {
            if let Err(error) = self.store.validate_voice_reference(reference) {
                self.store.fail_job(&job_id, &error)?;
                return Err(error);
            }
        }
        let runtime = self.clone();
        let thread_job_id = job_id.clone();
        std::thread::Builder::new()
            .name("soundar-synthesis".to_string())
            .spawn(move || {
                let outcome = runtime
                    .request_for_job(request.clone(), &thread_job_id)
                    .and_then(|result| {
                        runtime
                            .store
                            .complete_synthesis(&thread_job_id, &request, &result)
                    });
                if let Err(error) = outcome {
                    if runtime
                        .store
                        .job_status(&thread_job_id)
                        .ok()
                        .flatten()
                        .as_deref()
                        != Some("cancelled")
                    {
                        let _ = runtime.store.fail_job(&thread_job_id, &error);
                    }
                }
            })
            .map(|_| ())
            .map_err(|error| {
                let message = format!("Could not start synthesis: {error}");
                let _ = self.store.fail_job(&job_id, &message);
                message
            })
    }

    fn start_background_model_load(&self, job_id: String, model_id: String) -> Result<(), String> {
        let runtime = self.clone();
        let thread_job_id = job_id.clone();
        std::thread::Builder::new()
            .name("soundar-model-load".to_string())
            .spawn(move || {
                let request = json!({
                    "operation": "load",
                    "model_id": model_id,
                    "priority": "urgent",
                });
                let outcome = runtime.request_for_job(request, &thread_job_id);
                match outcome {
                    Ok(_) => match runtime.store.complete_job(&thread_job_id) {
                        Ok(true) => {}
                        Ok(false) => {
                            let _ = runtime.unload_model_runtime(&model_id);
                        }
                        Err(error) => {
                            let _ = runtime.unload_model_runtime(&model_id);
                            let _ = runtime.store.fail_job(&thread_job_id, &error);
                        }
                    },
                    Err(error) => {
                        if runtime
                            .store
                            .job_status(&thread_job_id)
                            .ok()
                            .flatten()
                            .as_deref()
                            != Some("cancelled")
                        {
                            let _ = runtime.store.fail_job(&thread_job_id, &error);
                        }
                    }
                }
            })
            .map(|_| ())
            .map_err(|error| {
                let message = format!("Could not start model loading: {error}");
                let _ = self.store.fail_job(&job_id, &message);
                message
            })
    }

    fn claim_batch_coordinator(&self, batch_id: &str) -> Result<BatchCoordinatorLease, String> {
        let mut coordinators = self
            .active_batch_coordinators
            .lock()
            .map_err(|_| "Batch coordinator lock failed")?;
        if !coordinators.insert(batch_id.to_string()) {
            return Err("This batch already has an active coordinator".to_string());
        }
        Ok(BatchCoordinatorLease {
            coordinators: Arc::clone(&self.active_batch_coordinators),
            batch_id: batch_id.to_string(),
        })
    }

    fn execute_batch(&self, batch_id: &str, requested_parallelism: usize) -> Result<Value, String> {
        let _coordinator = self.claim_batch_coordinator(batch_id)?;
        self.run_batch(batch_id, requested_parallelism)
    }

    fn run_batch(&self, batch_id: &str, requested_parallelism: usize) -> Result<Value, String> {
        let batch = self
            .store
            .get_batch(batch_id)?
            .ok_or("The selected batch was not found")?;
        if batch.get("status").and_then(Value::as_str) == Some("cancelled") {
            return Err("The selected batch was cancelled".to_string());
        }
        let settings = batch
            .pointer("/request/settings")
            .and_then(Value::as_object)
            .cloned()
            .ok_or("The batch has no generation settings")?;
        let model_id = settings
            .get("model_id")
            .and_then(Value::as_str)
            .ok_or("Batch settings require model_id")?;
        validate_model_argument(model_id)?;
        let mut rows = batch
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.get("status").and_then(Value::as_str) == Some("queued"))
            .collect::<Vec<_>>();
        rows.sort_by_key(|item| {
            (
                std::cmp::Reverse(priority_value(item.get("priority")).unwrap_or(1)),
                item.get("item_index")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MAX),
            )
        });
        let worker_count = requested_parallelism.clamp(1, 8).min(rows.len().max(1));
        let pending = Arc::new(Mutex::new(VecDeque::from(rows)));
        let mut handles = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let runtime = self.clone();
            let batch_id = batch_id.to_string();
            let settings = settings.clone();
            let pending = Arc::clone(&pending);
            let handle = std::thread::Builder::new()
                .name(format!("soundar-batch-worker-{worker_index}"))
                .spawn(move || -> Result<(), String> {
                    loop {
                        let Some(state) = runtime.store.get_batch(&batch_id)? else {
                            break;
                        };
                        if state.get("paused").and_then(Value::as_bool) == Some(true)
                            || !matches!(
                                state.get("status").and_then(Value::as_str),
                                Some("queued" | "running")
                            )
                        {
                            break;
                        }
                        let item = pending
                            .lock()
                            .map_err(|_| "Batch work queue lock failed")?
                            .pop_front();
                        let Some(item) = item else { break };
                        if let Err(error) = runtime.execute_batch_item(&batch_id, &item, &settings)
                        {
                            if let Some(index) = item.get("item_index").and_then(Value::as_i64) {
                                runtime.store.update_batch_item(
                                    &batch_id,
                                    index,
                                    "failed",
                                    None,
                                    Some(&error),
                                )?;
                            }
                        }
                    }
                    Ok(())
                })
                .map_err(|error| format!("Could not start a batch worker: {error}"))?;
            handles.push(handle);
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| "A batch worker stopped unexpectedly".to_string())??;
        }
        self.store
            .get_batch(batch_id)?
            .ok_or_else(|| "The batch disappeared after execution".to_string())
    }

    fn execute_batch_item(
        &self,
        batch_id: &str,
        item: &Value,
        settings: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        let item_index = item
            .get("item_index")
            .and_then(Value::as_i64)
            .ok_or("A batch row has no item index")?;
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .ok_or("A batch row has no text")?;
        let mut request = Value::Object(settings.clone());
        let object = request.as_object_mut().expect("batch request is an object");
        if let Some(overrides) = item.get("settings").and_then(Value::as_object) {
            for (key, value) in overrides {
                object.insert(key.clone(), value.clone());
            }
        }
        object.insert(
            "priority".to_string(),
            item.get("priority")
                .cloned()
                .unwrap_or_else(|| json!("normal")),
        );
        object.insert("text".to_string(), json!(text));
        object
            .entry("speaker".to_string())
            .or_insert_with(|| json!("default"));
        object
            .entry("language".to_string())
            .or_insert_with(|| json!("en"));
        object
            .entry("speed".to_string())
            .or_insert_with(|| json!(1.0));
        object
            .entry("output_format".to_string())
            .or_insert_with(|| json!("wav"));
        object
            .entry("voice_name".to_string())
            .or_insert_with(|| json!("Default voice"));
        let seed = object.get("seed").and_then(Value::as_i64).unwrap_or(42_817);
        if item.pointer("/settings/seed").is_none() {
            object.insert("seed".to_string(), json!(seed.saturating_add(item_index)));
        }
        let model_id = object
            .get("model_id")
            .and_then(Value::as_str)
            .ok_or("Batch row settings require model_id")?;
        validate_model_argument(model_id)?;
        object.insert(
            "title".to_string(),
            item.get("name").cloned().unwrap_or_else(|| {
                json!(text
                    .split(['.', '!', '?'])
                    .next()
                    .unwrap_or("Batch generation")
                    .trim()
                    .chars()
                    .take(56)
                    .collect::<String>())
            }),
        );
        if let Some(output_name) = item.get("output_name").and_then(Value::as_str) {
            let attempt = item.get("attempt").and_then(Value::as_i64).unwrap_or(0) + 1;
            object.insert(
                "output_name".to_string(),
                json!(format!(
                    "batch-{}-{output_name}-a{attempt:02}",
                    &batch_id[..8.min(batch_id.len())]
                )),
            );
        }
        if let Some(reference) = request.get("reference_audio_path").and_then(Value::as_str) {
            self.store.validate_voice_reference(reference)?;
        }
        let job_id = self.store.create_job("batch-synthesis", &request)?;
        if !self.store.start_batch_item(batch_id, item_index, &job_id)? {
            self.store.cancel_job(&job_id)?;
            return Ok(());
        }
        let outcome = self
            .request_for_job(request.clone(), &job_id)
            .and_then(|result| self.store.complete_synthesis(&job_id, &request, &result));
        match outcome {
            Ok(history) => {
                let history_id = history.get("id").and_then(Value::as_str);
                self.store.update_batch_item(
                    batch_id,
                    item_index,
                    "completed",
                    history_id,
                    None,
                )?;
            }
            Err(error) => {
                let cancelled = self.store.job_status(&job_id)?.as_deref() == Some("cancelled");
                if !cancelled {
                    self.store.fail_job(&job_id, &error)?;
                }
                self.store.update_batch_item(
                    batch_id,
                    item_index,
                    if cancelled { "cancelled" } else { "failed" },
                    None,
                    if cancelled { None } else { Some(&error) },
                )?;
            }
        }
        Ok(())
    }

    fn cancel_batch(&self, batch_id: &str) -> Result<Value, String> {
        for job_id in self.store.cancel_batch(batch_id)? {
            self.cancel_job(&job_id)?;
        }
        self.store
            .get_batch(batch_id)?
            .ok_or_else(|| "The batch disappeared after cancellation".to_string())
    }

    fn pause_batch(&self, batch_id: &str) -> Result<Value, String> {
        self.store.pause_batch(batch_id)
    }

    fn resume_batch(
        &self,
        batch_id: &str,
        parallelism: usize,
        retry_failed: bool,
    ) -> Result<Value, String> {
        let coordinator = self.claim_batch_coordinator(batch_id)?;
        let batch = self.store.resume_batch(batch_id, retry_failed)?;
        let runtime = self.clone();
        let id = batch_id.to_string();
        std::thread::Builder::new()
            .name("soundar-batch-resume".to_string())
            .spawn(move || {
                let _coordinator = coordinator;
                let _ = runtime.run_batch(&id, parallelism.clamp(1, 8));
            })
            .map_err(|error| format!("Could not resume batch execution: {error}"))?;
        Ok(batch)
    }

    fn queue_batch(&self, request: &Value, parallelism: usize) -> Result<Value, String> {
        let parallelism = parallelism.clamp(1, 8);
        let mut stored_request = request.clone();
        stored_request
            .as_object_mut()
            .ok_or("A batch request must be a JSON object")?
            .insert("parallelism".to_string(), json!(parallelism));
        let batch = self.store.create_batch(&stored_request)?;
        let batch_id = batch
            .get("id")
            .and_then(Value::as_str)
            .ok_or("The batch has no identifier")?
            .to_string();
        let coordinator = self.claim_batch_coordinator(&batch_id)?;
        let runtime = self.clone();
        std::thread::Builder::new()
            .name("soundar-batch".to_string())
            .spawn(move || {
                let _coordinator = coordinator;
                let _ = runtime.run_batch(&batch_id, parallelism);
            })
            .map_err(|error| format!("Could not start batch execution: {error}"))?;
        Ok(batch)
    }

    fn queue_idempotent_batch(
        &self,
        request: &Value,
        parallelism: usize,
        idempotency_key: &str,
    ) -> Result<Option<Value>, String> {
        let parallelism = parallelism.clamp(1, 8);
        let mut stored_request = request.clone();
        stored_request
            .as_object_mut()
            .ok_or("A batch request must be a JSON object")?
            .insert("parallelism".to_string(), json!(parallelism));
        let Some((batch, _created)) = self
            .store
            .create_idempotent_batch(idempotency_key, &stored_request)?
        else {
            return Ok(None);
        };
        if batch.get("status").and_then(Value::as_str) == Some("queued") {
            let batch_id = batch
                .get("id")
                .and_then(Value::as_str)
                .ok_or("The batch has no identifier")?
                .to_string();
            let coordinator = match self.claim_batch_coordinator(&batch_id) {
                Ok(coordinator) => coordinator,
                Err(error) if error.contains("active coordinator") => return Ok(Some(batch)),
                Err(error) => return Err(error),
            };
            let runtime = self.clone();
            std::thread::Builder::new()
                .name("soundar-api-batch".to_string())
                .spawn(move || {
                    let _coordinator = coordinator;
                    let _ = runtime.run_batch(&batch_id, parallelism);
                })
                .map_err(|error| format!("Could not start batch execution: {error}"))?;
        }
        Ok(Some(batch))
    }

    fn stop_active_recording(
        &self,
    ) -> Result<Option<(ActiveRecording, Result<(), String>)>, String> {
        let mut guard = self
            .active_recording
            .lock()
            .map_err(|_| "Recording lock failed")?;
        let Some(mut recording) = guard.take() else {
            return Ok(None);
        };
        recording.stop.store(true, Ordering::Release);
        let result = recording
            .thread
            .take()
            .ok_or("Recording thread is unavailable")?
            .join()
            .map_err(|_| "Recording thread stopped unexpectedly".to_string())?;
        Ok(Some((recording, result)))
    }

    fn stop_active_playback(&self) -> Result<Option<(ActivePlayback, Result<(), String>)>, String> {
        let mut guard = self
            .active_playback
            .lock()
            .map_err(|_| "Playback lock failed")?;
        let Some(mut playback) = guard.take() else {
            return Ok(None);
        };
        playback.stop.store(true, Ordering::Release);
        let result = playback
            .thread
            .take()
            .ok_or("Playback thread is unavailable")?
            .join()
            .map_err(|_| "Playback thread stopped unexpectedly".to_string())?;
        Ok(Some((playback, result)))
    }

    fn stop_active_worker(&self) -> Result<(), String> {
        let mut pool = self
            .worker_pool
            .lock()
            .map_err(|_| "Python worker pool lock failed")?;
        for mut process in pool.drain(..) {
            Self::stop_process(&mut process);
        }
        Ok(())
    }

    fn stop_idle_workers_for_engine(&self, engine: &str) -> Result<usize, String> {
        let mut pool = self
            .worker_pool
            .lock()
            .map_err(|_| "Python worker pool lock failed")?;
        let mut retained = Vec::with_capacity(pool.len());
        let mut retired = 0usize;
        for mut process in pool.drain(..) {
            if process.engine == engine {
                Self::stop_process(&mut process);
                retired += 1;
            } else {
                retained.push(process);
            }
        }
        *pool = retained;
        Ok(retired)
    }

    #[cfg(test)]
    fn prewarm_model(&self, model_id: &str) -> Result<Value, String> {
        validate_model_argument(model_id)?;
        self.request(json!({
            "operation": "load",
            "model_id": model_id,
            "priority": "urgent",
        }))
    }

    fn unload_model_runtime(&self, model_id: &str) -> Result<Value, String> {
        validate_model_argument(model_id)?;
        let engine = self.request_engine(&json!({
            "operation": "load",
            "model_id": model_id,
        }))?;
        let mut scheduler = self
            .scheduler
            .0
            .lock()
            .map_err(|_| "GPU scheduler lock failed")?;
        let active_jobs = scheduler.active_engines.get(&engine).copied().unwrap_or(0);
        if active_jobs > 0 {
            return Err(format!(
                "{engine} has {active_jobs} active job{}. Cancel or wait for the work to finish before unloading.",
                if active_jobs == 1 { "" } else { "s" }
            ));
        }

        let mut pool = self
            .worker_pool
            .lock()
            .map_err(|_| "Python worker pool lock failed")?;
        let mut retained = Vec::with_capacity(pool.len());
        let mut retired = 0usize;
        let mut unloaded_models = Vec::new();
        for mut process in pool.drain(..) {
            if process.engine != engine {
                retained.push(process);
                continue;
            }
            let payload = json!({ "operation": "unload" }).to_string();
            if writeln!(process.stdin, "{payload}")
                .and_then(|_| process.stdin.flush())
                .is_ok()
                && wait_for_worker_output(&mut process, 30).is_ok()
            {
                let mut bytes = Vec::new();
                if process
                    .stdout
                    .by_ref()
                    .take(MAX_RPC_RESPONSE_BYTES + 1)
                    .read_until(b'\n', &mut bytes)
                    .is_ok()
                {
                    if let Ok(response) = serde_json::from_slice::<Value>(&bytes) {
                        if let Some(models) = response
                            .pointer("/result/unloaded_models")
                            .and_then(Value::as_array)
                        {
                            unloaded_models.extend(
                                models.iter().filter_map(Value::as_str).map(str::to_string),
                            );
                        }
                    }
                }
            }
            Self::stop_process(&mut process);
            retired += 1;
        }
        *pool = retained;
        scheduler.available_vram_budget_mb = None;
        drop(pool);
        drop(scheduler);
        self.store
            .record_engine_event(&engine, "stopped", "user_unloaded")?;
        Ok(json!({
            "status": "unloaded",
            "engine": engine,
            "model_id": model_id,
            "retired_workers": retired,
            "unloaded_models": unloaded_models,
        }))
    }

    fn prepare_benchmark_engine(&self, model_id: &str) -> Result<Value, String> {
        validate_model_argument(model_id)?;
        let engine = self.request_engine(&json!({
            "operation": "synthesize",
            "model_id": model_id,
        }))?;
        let mut scheduler = self
            .scheduler
            .0
            .lock()
            .map_err(|_| "GPU scheduler lock failed")?;
        if scheduler
            .benchmark_reservation
            .as_ref()
            .is_some_and(|reservation| reservation.expires_at <= Instant::now())
        {
            scheduler.benchmark_reservation = None;
        }
        if scheduler.active_workers > 0
            || !scheduler.waiters.is_empty()
            || scheduler.benchmark_reservation.is_some()
        {
            return Err(
                "A cold benchmark requires an idle inference queue. Let active jobs finish and try again."
                    .to_string(),
            );
        }
        let mut pool = self
            .worker_pool
            .lock()
            .map_err(|_| "Python worker pool lock failed")?;
        let mut retained = Vec::with_capacity(pool.len());
        let mut retired = 0usize;
        for mut process in pool.drain(..) {
            if process.engine == engine {
                Self::stop_process(&mut process);
                retired += 1;
            } else {
                retained.push(process);
            }
        }
        *pool = retained;
        let token = Uuid::new_v4().simple().to_string();
        scheduler.benchmark_reservation = Some(BenchmarkReservation {
            token: token.clone(),
            engine: engine.clone(),
            remaining_admissions: 3,
            expires_at: Instant::now() + Duration::from_secs(30 * 60),
        });
        Ok(json!({
            "engine": engine,
            "retired_workers": retired,
            "ready": true,
            "token": token,
        }))
    }

    fn release_benchmark_engine(&self, token: &str) -> Result<bool, String> {
        let (lock, changed) = &*self.scheduler;
        let mut scheduler = lock.lock().map_err(|_| "GPU scheduler lock failed")?;
        let matches = scheduler
            .benchmark_reservation
            .as_ref()
            .is_some_and(|reservation| reservation.token == token);
        if matches {
            scheduler.benchmark_reservation = None;
            changed.notify_all();
        }
        Ok(matches)
    }

    fn setup(&self, app: &tauri::AppHandle) -> Result<(), String> {
        let _setup_guard = self
            .setup_lock
            .lock()
            .map_err(|_| "Runtime setup lock failed")?;
        if self.foundation_runtime_ready() {
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
        if !self.foundation_runtime_ready() {
            let detail = if self.python_path.is_file() {
                format!(
                    "Runtime setup completed, but {} is missing or incompatible",
                    app_data_dir().join("runtime.json").display()
                )
            } else {
                format!(
                    "Runtime setup completed without creating {}",
                    self.python_path.display()
                )
            };
            return Err(detail);
        }

        self.stop_active_worker()?;
        Ok(())
    }

    fn setup_engine(&self, app: &tauri::AppHandle, engine: &str) -> Result<(), String> {
        validate_engine_argument(engine)?;
        let _setup_guard = self
            .setup_lock
            .lock()
            .map_err(|_| "Runtime setup lock failed")?;
        if !self.foundation_runtime_ready() {
            return Err("Set up the soundAr foundation runtime first.".to_string());
        }
        let script = self.runtime_root.join("setup-engine-runtime.sh");
        if !script.is_file() {
            return Err(
                "The bundled engine runtime installer is missing. Reinstall soundAr.".to_string(),
            );
        }
        let active_jobs = self
            .scheduler
            .0
            .lock()
            .map_err(|_| "GPU scheduler lock failed")?
            .active_engines
            .get(engine)
            .copied()
            .unwrap_or(0);
        if active_jobs > 0 {
            return Err(format!(
                "{engine} has {active_jobs} active job{}. Wait for the work to finish before changing its runtime.",
                if active_jobs == 1 { "" } else { "s" }
            ));
        }
        self.stop_idle_workers_for_engine(engine)?;
        let mut child = Command::new("/bin/bash")
            .arg(script)
            .arg(engine)
            .arg(app_data_dir())
            .arg(self.runtime_root.join("requirements-engines"))
            .current_dir(&self.runtime_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not launch {engine} runtime setup: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Engine setup output is unavailable")?;
        let mut recent_output = VecDeque::with_capacity(24);
        for line in BufReader::new(stdout).lines() {
            let line =
                line.map_err(|error| format!("Could not read engine setup output: {error}"))?;
            if recent_output.len() == 24 {
                recent_output.pop_front();
            }
            recent_output.push_back(line.clone());
            if let Some(message) = line.strip_prefix("soundar-engine:") {
                app.emit(
                    "engine-runtime-progress",
                    json!({ "engine": engine, "message": message }),
                )
                .ok();
            }
        }
        let status = child
            .wait()
            .map_err(|error| format!("Could not finish engine setup: {error}"))?;
        if !status.success() {
            let detail = recent_output
                .iter()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(String::as_str)
                .unwrap_or("No installer details were returned");
            return Err(format!("{engine} runtime setup failed: {detail}"));
        }
        let (python, isolated) = self.engine_python_path(engine);
        if !isolated || !python.is_file() {
            return Err(format!(
                "Engine setup completed without creating {}",
                python.display()
            ));
        }
        Ok(())
    }

    fn model_command_with_revision(
        &self,
        operation: &str,
        model_id: &str,
        revision: Option<&str>,
    ) -> Result<Value, String> {
        validate_model_argument(model_id)?;
        if let Some(revision) = revision {
            validate_revision(revision)?;
        }
        if !self.foundation_runtime_ready() {
            return Err("Set up the local inference runtime before managing models.".to_string());
        }
        let mut command = Command::new(&self.python_path);
        command
            .arg(self.runtime_root.join("model_cli.py"))
            .arg(operation)
            .arg(model_id);
        if let Some(revision) = revision {
            command.arg("--revision").arg(revision);
        }
        let output = command
            .current_dir(&self.runtime_root)
            .env("PYTHONPATH", &self.runtime_root)
            .output()
            .map_err(|error| format!("Could not start model {operation}: {error}"))?;
        let response = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .last()
            .ok_or_else(|| format!("Model {operation} returned no valid response."))?;
        if output.status.success() {
            Ok(response)
        } else {
            Err(response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("The model operation failed.")
                .to_string())
        }
    }

    fn model_command(&self, operation: &str, model_id: &str) -> Result<Value, String> {
        self.model_command_with_revision(operation, model_id, None)
    }

    fn install_model(
        &self,
        app: &tauri::AppHandle,
        model_id: &str,
        revision: &str,
    ) -> Result<Value, String> {
        validate_model_argument(model_id)?;
        validate_revision(revision)?;
        if !self.foundation_runtime_ready() {
            return Err("Set up the local inference runtime before installing models.".to_string());
        }
        let catalog = read_json(
            self.runtime_root.join("data/curated_models.json"),
            json!({ "models": [] }),
        );
        let engine = catalog
            .get("models")
            .and_then(Value::as_array)
            .and_then(|models| {
                models.iter().find_map(|model| {
                    (model.get("model_id").and_then(Value::as_str) == Some(model_id))
                        .then(|| model.get("engine").and_then(Value::as_str))
                        .flatten()
                })
            })
            .ok_or_else(|| format!("The model catalog has no engine for {model_id}"))?;
        self.setup_engine(app, engine)?;
        let _operation_guard = self
            .model_operation_lock
            .lock()
            .map_err(|_| "Model operation lock failed")?;
        if self
            .active_download
            .lock()
            .map_err(|_| "Model download lock failed")?
            .is_some()
        {
            return Err("Another model download is already running.".to_string());
        }

        let mut child = Command::new(&self.python_path)
            .arg(self.runtime_root.join("model_cli.py"))
            .arg("install")
            .arg(model_id)
            .arg("--revision")
            .arg(revision)
            .current_dir(&self.runtime_root)
            .env("PYTHONPATH", &self.runtime_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not start model installation: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Model installation output is unavailable")?;
        *self
            .active_download
            .lock()
            .map_err(|_| "Model download lock failed")? = Some(ActiveDownload {
            model_id: model_id.to_string(),
            child,
            cancelled: false,
        });

        let mut result = None;
        let mut operation_error = None;
        for line in BufReader::new(stdout).lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    operation_error = Some(format!("Could not read model progress: {error}"));
                    break;
                }
            };
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match event.get("type").and_then(Value::as_str) {
                Some("progress") => {
                    app.emit("model-download-progress", &event).ok();
                }
                Some("complete") => result = event.get("model").cloned(),
                Some("error") => {
                    operation_error = event
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                _ => {}
            }
        }

        let active = self
            .active_download
            .lock()
            .map_err(|_| "Model download lock failed")?
            .take()
            .ok_or("Model download state was lost")?;
        let cancelled = active.cancelled;
        let mut child = active.child;
        let status = child
            .wait()
            .map_err(|error| format!("Could not finish model installation: {error}"))?;
        if cancelled || !status.success() {
            let _ = self.model_command("cleanup", model_id);
        }
        if cancelled {
            return Err("Model installation was cancelled.".to_string());
        }
        if !status.success() {
            return Err(operation_error.unwrap_or_else(|| "Model installation failed.".to_string()));
        }
        result.ok_or_else(|| "Model installation completed without a registry record.".to_string())
    }

    fn cancel_model_install(&self, model_id: &str) -> Result<bool, String> {
        validate_model_argument(model_id)?;
        let mut guard = self
            .active_download
            .lock()
            .map_err(|_| "Model download lock failed")?;
        let Some(active) = guard.as_mut() else {
            return Ok(false);
        };
        if active.model_id != model_id {
            return Err(format!("{} is currently downloading.", active.model_id));
        }
        active.cancelled = true;
        active
            .child
            .kill()
            .map_err(|error| format!("Could not cancel model installation: {error}"))?;
        Ok(true)
    }

    fn remove_model(&self, model_id: &str) -> Result<bool, String> {
        validate_model_argument(model_id)?;
        let _operation_guard = self
            .model_operation_lock
            .lock()
            .map_err(|_| "Model operation lock failed")?;
        if self
            .active_download
            .lock()
            .map_err(|_| "Model download lock failed")?
            .is_some()
        {
            return Err("Cancel the active model download before removing a model.".to_string());
        }
        self.unload_model_runtime(model_id)?;
        self.model_command("delete", model_id).map(|response| {
            response
                .get("removed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
    }

    fn start_api_server(&self, requested_port: Option<u16>) -> Result<Value, String> {
        let mut guard = self
            .api_server
            .lock()
            .map_err(|_| "Developer API lock failed")?;
        if let Some(server) = guard.as_ref() {
            return Ok(api_server_value(server));
        }
        let port = requested_port.unwrap_or(17_843);
        if port != 0 && port < 1024 {
            return Err("The developer API port must be 1024 or higher".to_string());
        }
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
            format!("Could not bind the developer API to 127.0.0.1:{port}: {error}")
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("Could not configure the developer API: {error}"))?;
        let actual_port = listener
            .local_addr()
            .map_err(|error| format!("Could not inspect the developer API address: {error}"))?
            .port();
        let token = Uuid::new_v4().simple().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_token = token.clone();
        let runtime = self.clone();
        let thread = std::thread::Builder::new()
            .name("soundar-local-api".to_string())
            .spawn(move || serve_local_api(listener, runtime, thread_token, thread_stop))
            .map_err(|error| format!("Could not start the developer API: {error}"))?;
        *guard = Some(ActiveApiServer {
            port: actual_port,
            token,
            stop,
            thread: Some(thread),
        });
        Ok(api_server_value(
            guard.as_ref().expect("server was inserted"),
        ))
    }

    fn api_server_status(&self) -> Result<Value, String> {
        let guard = self
            .api_server
            .lock()
            .map_err(|_| "Developer API lock failed")?;
        Ok(guard
            .as_ref()
            .map(api_server_value)
            .unwrap_or_else(|| json!({ "running": false })))
    }

    fn stop_api_server(&self) -> Result<bool, String> {
        let mut server = self
            .api_server
            .lock()
            .map_err(|_| "Developer API lock failed")?
            .take();
        let Some(server) = server.as_mut() else {
            return Ok(false);
        };
        server.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", server.port));
        if let Some(thread) = server.thread.take() {
            thread
                .join()
                .map_err(|_| "The developer API thread stopped unexpectedly".to_string())?;
        }
        Ok(true)
    }
}

fn wait_for_worker_output(process: &mut PythonProcess, timeout_seconds: i32) -> Result<(), String> {
    let mut descriptor = libc::pollfd {
        fd: process.stdout.get_ref().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = timeout_seconds.saturating_mul(1_000);
    // SAFETY: descriptor points to one valid pollfd for the duration of this call.
    let outcome = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if outcome < 0 {
        return Err(format!(
            "Could not monitor the inference worker: {}",
            std::io::Error::last_os_error()
        ));
    }
    if outcome == 0 {
        return Err(format!(
            "The {} worker exceeded its {} second deadline",
            process.engine, timeout_seconds
        ));
    }
    if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
        && descriptor.revents & libc::POLLIN == 0
    {
        return Err(format!(
            "The {} worker stopped unexpectedly",
            process.engine
        ));
    }
    Ok(())
}

fn api_server_value(server: &ActiveApiServer) -> Value {
    json!({
        "running": true,
        "host": "127.0.0.1",
        "port": server.port,
        "base_url": format!("http://127.0.0.1:{}", server.port),
        "token": server.token,
    })
}

fn serve_local_api(
    listener: TcpListener,
    runtime: RuntimeState,
    token: String,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, address)) => {
                if !address.ip().is_loopback() {
                    let _ = write_http_response(
                        &mut stream,
                        403,
                        "application/json",
                        br#"{"error":{"message":"Loopback clients only"}}"#,
                    );
                    continue;
                }
                let connection_runtime = runtime.clone();
                let connection_token = token.clone();
                let _ = std::thread::Builder::new()
                    .name("soundar-api-request".to_string())
                    .spawn(move || {
                        if let Err(error) = handle_api_connection(
                            &mut stream,
                            &connection_runtime,
                            &connection_token,
                        ) {
                            let payload = json!({ "error": { "message": error } }).to_string();
                            let _ = write_http_response(
                                &mut stream,
                                400,
                                "application/json",
                                payload.as_bytes(),
                            );
                        }
                    });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            Err(_) => break,
        }
    }
}

fn handle_api_connection(
    stream: &mut TcpStream,
    runtime: &RuntimeState,
    token: &str,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .map_err(|error| format!("Could not configure API request timeout: {error}"))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|error| format!("Could not read API request: {error}"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("Malformed HTTP request")?;
    let path = parts.next().ok_or("Malformed HTTP request")?;
    let mut content_length = 0usize;
    let mut authorization = String::new();
    let mut origin = String::new();
    let mut idempotency_key = String::new();
    let mut last_event_id = 0i64;
    let mut header_bytes = request_line.len();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        header_bytes += line.len();
        if header_bytes > 32 * 1024 {
            return Err("API request headers exceed 32 KB".to_string());
        }
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => {
                    content_length = value.trim().parse().map_err(|_| "Invalid Content-Length")?
                }
                "authorization" => authorization = value.trim().to_string(),
                "origin" => origin = value.trim().to_string(),
                "idempotency-key" => idempotency_key = value.trim().to_string(),
                "last-event-id" => {
                    last_event_id = value.trim().parse().map_err(|_| "Invalid Last-Event-ID")?
                }
                _ => {}
            }
        }
    }
    if authorization != format!("Bearer {token}") {
        return write_http_response(
            stream,
            401,
            "application/json",
            br#"{"error":{"message":"Valid bearer token required"}}"#,
        );
    }
    if !origin.is_empty()
        && origin != "null"
        && !origin.starts_with("http://127.0.0.1:")
        && !origin.starts_with("http://localhost:")
    {
        return write_http_response(
            stream,
            403,
            "application/json",
            br#"{"error":{"message":"Browser origins must be loopback"}}"#,
        );
    }
    if content_length > MAX_RPC_REQUEST_BYTES {
        return write_http_response(
            stream,
            413,
            "application/json",
            br#"{"error":{"message":"Request body exceeds 1 MB"}}"#,
        );
    }
    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|error| format!("Could not read API body: {error}"))?;

    match (method, path) {
        ("GET", "/health") => {
            let payload = json!({ "status": "ready", "local_only": true }).to_string();
            write_http_response(stream, 200, "application/json", payload.as_bytes())
        }
        ("GET", "/v1/models") => {
            let registry = read_json(runtime.model_registry_path.clone(), json!({ "models": [] }));
            let models = registry
                .get("models")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let payload = json!({ "object": "list", "data": models.into_iter().map(|model| json!({ "id": model.get("model_id"), "object": "model", "owned_by": "local" })).collect::<Vec<_>>() }).to_string();
            write_http_response(stream, 200, "application/json", payload.as_bytes())
        }
        ("GET", "/v1/capabilities") => {
            let capabilities = read_json(
                runtime.runtime_root.join("data/engine_manifests.json"),
                json!({ "engines": [] }),
            );
            let payload = json!({
                "object": "list",
                "data": capabilities.get("engines").cloned().unwrap_or_else(|| json!([])),
            })
            .to_string();
            write_http_response(stream, 200, "application/json", payload.as_bytes())
        }
        ("GET", "/v1/voices") => {
            let voices = runtime.store.list_voices()?;
            let safe = voices
                .into_iter()
                .map(|voice| {
                    json!({
                        "id": voice.get("id"),
                        "name": voice.get("name"),
                        "style": voice.get("style"),
                        "engines": voice.get("engines"),
                        "consent": voice.get("consent"),
                        "state": voice.get("state"),
                        "source_kind": voice.get("source_kind"),
                    })
                })
                .collect::<Vec<_>>();
            let payload = json!({ "object": "list", "data": safe }).to_string();
            write_http_response(stream, 200, "application/json", payload.as_bytes())
        }
        ("GET", "/v1/jobs") => write_api_list(stream, runtime.store.list_jobs()?),
        ("GET", path) if path.starts_with("/v1/jobs/") && path.ends_with("/events") => {
            let id = path
                .trim_start_matches("/v1/jobs/")
                .trim_end_matches("/events");
            validate_api_resource_id(id)?;
            write_job_event_stream(stream, runtime, id, last_event_id)
        }
        ("GET", path) if path.starts_with("/v1/jobs/") && path.ends_with("/audio") => {
            let id = path
                .trim_start_matches("/v1/jobs/")
                .trim_end_matches("/audio");
            validate_api_resource_id(id)?;
            let Some(job) = runtime.store.get_job(id)? else {
                return write_http_response(
                    stream,
                    404,
                    "application/json",
                    br#"{"error":{"message":"Job not found"}}"#,
                );
            };
            if job.get("status").and_then(Value::as_str) != Some("completed") {
                return write_http_response(
                    stream,
                    409,
                    "application/json",
                    br#"{"error":{"message":"Job audio is not ready"}}"#,
                );
            }
            let (bytes, format) = runtime.store.generated_audio_for_job(id)?;
            write_http_response(
                stream,
                200,
                if format == "flac" {
                    "audio/flac"
                } else {
                    "audio/wav"
                },
                &bytes,
            )
        }
        ("GET", path) if path.starts_with("/v1/jobs/") => {
            let id = path.trim_start_matches("/v1/jobs/");
            validate_api_resource_id(id)?;
            let Some(job) = runtime.store.get_job(id)? else {
                return write_http_response(
                    stream,
                    404,
                    "application/json",
                    br#"{"error":{"message":"Job not found"}}"#,
                );
            };
            let response = job.to_string();
            write_http_response(stream, 200, "application/json", response.as_bytes())
        }
        ("GET", "/v1/runtime/scheduler") => {
            let response = runtime.scheduler_status()?.to_string();
            write_http_response(stream, 200, "application/json", response.as_bytes())
        }
        ("POST", "/v1/jobs/clear-finished") => {
            let cleared = runtime.store.clear_finished_jobs()?;
            let response = json!({ "cleared": cleared }).to_string();
            write_http_response(stream, 200, "application/json", response.as_bytes())
        }
        ("POST", path) if path.starts_with("/v1/jobs/") && path.ends_with("/retry") => {
            let id = path
                .trim_start_matches("/v1/jobs/")
                .trim_end_matches("/retry");
            if id.is_empty() || id.contains('/') {
                return Err("Invalid job identifier".to_string());
            }
            let (job, request) = runtime.store.retry_synthesis_job(id)?;
            runtime.start_background_synthesis(id.to_string(), request)?;
            let response = serde_json::to_string(&job).map_err(|error| error.to_string())?;
            write_http_response(stream, 202, "application/json", response.as_bytes())
        }
        ("GET", "/v1/history") => write_api_list(stream, runtime.store.list_history(None)?),
        ("GET", "/v1/batches") => write_api_list(stream, runtime.store.list_batches()?),
        ("GET", "/v1/benchmarks") => write_api_list(stream, runtime.store.list_benchmarks()?),
        ("POST", "/v1/batches") => {
            validate_idempotency_key(&idempotency_key)?;
            let payload: Value = serde_json::from_slice(&body)
                .map_err(|error| format!("Invalid JSON body: {error}"))?;
            let parallelism = payload
                .get("parallelism")
                .and_then(Value::as_u64)
                .unwrap_or(2) as usize;
            let Some(batch) =
                runtime.queue_idempotent_batch(&payload, parallelism, &idempotency_key)?
            else {
                return write_http_response(
                    stream,
                    409,
                    "application/json",
                    br#"{"error":{"message":"Idempotency-Key was already used for a different batch"}}"#,
                );
            };
            let response = serde_json::to_string(&batch).map_err(|error| error.to_string())?;
            write_http_response(stream, 202, "application/json", response.as_bytes())
        }
        ("GET", path) if path.starts_with("/v1/batches/") => {
            let id = path.trim_start_matches("/v1/batches/");
            if id.is_empty() || id.contains('/') {
                return Err("Invalid batch identifier".to_string());
            }
            let batch = runtime.store.get_batch(id)?.ok_or("Batch not found")?;
            let response = serde_json::to_string(&batch).map_err(|error| error.to_string())?;
            write_http_response(stream, 200, "application/json", response.as_bytes())
        }
        ("POST", path) if path.starts_with("/v1/batches/") && path.ends_with("/cancel") => {
            let id = path
                .trim_start_matches("/v1/batches/")
                .trim_end_matches("/cancel");
            if id.is_empty() || id.contains('/') {
                return Err("Invalid batch identifier".to_string());
            }
            let batch = runtime.cancel_batch(id)?;
            let response = serde_json::to_string(&batch).map_err(|error| error.to_string())?;
            write_http_response(stream, 200, "application/json", response.as_bytes())
        }
        ("POST", path) if path.starts_with("/v1/batches/") && path.ends_with("/pause") => {
            let id = path
                .trim_start_matches("/v1/batches/")
                .trim_end_matches("/pause");
            if id.is_empty() || id.contains('/') {
                return Err("Invalid batch identifier".to_string());
            }
            let batch = runtime.pause_batch(id)?;
            let response = serde_json::to_string(&batch).map_err(|error| error.to_string())?;
            write_http_response(stream, 200, "application/json", response.as_bytes())
        }
        ("POST", path) if path.starts_with("/v1/batches/") && path.ends_with("/resume") => {
            let id = path
                .trim_start_matches("/v1/batches/")
                .trim_end_matches("/resume");
            if id.is_empty() || id.contains('/') {
                return Err("Invalid batch identifier".to_string());
            }
            let payload: Value = if body.is_empty() {
                json!({})
            } else {
                serde_json::from_slice(&body)
                    .map_err(|error| format!("Invalid JSON body: {error}"))?
            };
            let parallelism = payload
                .get("parallelism")
                .and_then(Value::as_u64)
                .unwrap_or(2) as usize;
            let retry_failed = payload
                .get("retry_failed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let batch = runtime.resume_batch(id, parallelism, retry_failed)?;
            let response = serde_json::to_string(&batch).map_err(|error| error.to_string())?;
            write_http_response(stream, 202, "application/json", response.as_bytes())
        }
        ("POST", "/v1/audio/speech/jobs") => {
            validate_idempotency_key(&idempotency_key)?;
            let payload: Value = serde_json::from_slice(&body)
                .map_err(|error| format!("Invalid JSON body: {error}"))?;
            if payload
                .get("input")
                .and_then(Value::as_str)
                .is_some_and(|input| input.chars().count() > 20_000)
            {
                return write_http_response(
                    stream,
                    413,
                    "application/json",
                    br#"{"error":{"message":"Input exceeds 20,000 characters"}}"#,
                );
            }
            let (request, _) = prepare_api_synthesis_request(runtime, &payload)?;
            let Some((job_id, created)) =
                runtime
                    .store
                    .create_idempotent_job("api-synthesis", &idempotency_key, &request)?
            else {
                return write_http_response(
                    stream,
                    409,
                    "application/json",
                    br#"{"error":{"message":"Idempotency-Key was already used for a different request"}}"#,
                );
            };
            if created {
                runtime.start_background_synthesis(job_id.clone(), request)?;
            }
            let job = runtime.store.get_job(&job_id)?.ok_or("Job not found")?;
            let response = job.to_string();
            write_http_response(stream, 202, "application/json", response.as_bytes())
        }
        ("POST", "/v1/audio/speech") => {
            let payload: Value = serde_json::from_slice(&body)
                .map_err(|error| format!("Invalid JSON body: {error}"))?;
            if payload
                .get("input")
                .and_then(Value::as_str)
                .is_some_and(|input| input.chars().count() > 20_000)
            {
                return write_http_response(
                    stream,
                    413,
                    "application/json",
                    br#"{"error":{"message":"Input exceeds 20,000 characters"}}"#,
                );
            }
            let (request, response_format) = prepare_api_synthesis_request(runtime, &payload)?;
            let job_id = runtime.store.create_job("api-synthesis", &request)?;
            let result = match runtime.request_for_job(request.clone(), &job_id) {
                Ok(result) => runtime
                    .store
                    .complete_synthesis(&job_id, &request, &result)?,
                Err(error) => {
                    runtime.store.fail_job(&job_id, &error)?;
                    return Err(error);
                }
            };
            let path = result
                .get("audio_path")
                .and_then(Value::as_str)
                .ok_or("Generation returned no audio path")?;
            let bytes = runtime.store.generated_audio_bytes(path)?;
            write_http_response(
                stream,
                200,
                if response_format == "flac" {
                    "audio/flac"
                } else {
                    "audio/wav"
                },
                &bytes,
            )
        }
        ("POST", path) if path.starts_with("/v1/jobs/") && path.ends_with("/cancel") => {
            let id = path
                .trim_start_matches("/v1/jobs/")
                .trim_end_matches("/cancel");
            if id.is_empty() || id.contains('/') {
                return Err("Invalid job identifier".to_string());
            }
            let cancelled = runtime.cancel_job(id)?;
            let payload = json!({ "id": id, "cancelled": cancelled }).to_string();
            write_http_response(stream, 200, "application/json", payload.as_bytes())
        }
        _ => write_http_response(
            stream,
            404,
            "application/json",
            br#"{"error":{"message":"Route not found"}}"#,
        ),
    }
}

fn write_api_list(stream: &mut TcpStream, data: Vec<Value>) -> Result<(), String> {
    let payload = json!({ "object": "list", "data": data }).to_string();
    write_http_response(stream, 200, "application/json", payload.as_bytes())
}

fn write_job_event_stream(
    stream: &mut TcpStream,
    runtime: &RuntimeState,
    job_id: &str,
    mut after: i64,
) -> Result<(), String> {
    if runtime.store.get_job(job_id)?.is_none() {
        return write_http_response(
            stream,
            404,
            "application/json",
            br#"{"error":{"message":"Job not found"}}"#,
        );
    }
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n"
    )
    .map_err(|error| format!("Could not start job event stream: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(1_900);
    let mut heartbeat = Instant::now();
    loop {
        let events = runtime
            .store
            .job_events_since(job_id, after)?
            .ok_or("Job not found")?;
        for event in events {
            let sequence = event
                .get("sequence")
                .and_then(Value::as_i64)
                .ok_or("Job event has no sequence")?;
            writeln!(stream, "id: {sequence}\nevent: job\ndata: {event}\n")
                .map_err(|error| format!("Could not write job event: {error}"))?;
            after = sequence;
            heartbeat = Instant::now();
        }
        stream
            .flush()
            .map_err(|error| format!("Could not flush job event stream: {error}"))?;
        let status = runtime.store.job_status(job_id)?.ok_or("Job not found")?;
        if matches!(status.as_str(), "completed" | "failed" | "cancelled") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        if heartbeat.elapsed() >= Duration::from_secs(10) {
            write!(stream, ": keep-alive\n\n")
                .and_then(|_| stream.flush())
                .map_err(|error| format!("Could not write job event heartbeat: {error}"))?;
            heartbeat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn required_api_string<'a>(value: &'a Value, key: &str, message: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .ok_or_else(|| message.to_string())
}

fn validate_api_resource_id(id: &str) -> Result<(), String> {
    if id.len() == 32 && id.chars().all(|value| value.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("Invalid resource identifier".to_string())
    }
}

fn validate_idempotency_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("Idempotency-Key header is required".to_string());
    }
    if key.len() > 128
        || !key
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.'))
    {
        return Err(
            "Idempotency-Key must contain 1-128 letters, numbers, dots, dashes, or underscores"
                .to_string(),
        );
    }
    Ok(())
}

fn prepare_api_synthesis_request(
    runtime: &RuntimeState,
    payload: &Value,
) -> Result<(Value, String), String> {
    let model_id = required_api_string(payload, "model", "Model is required")?;
    validate_model_argument(model_id)?;
    let input = required_api_string(payload, "input", "Input text is required")?;
    if input.chars().count() > 20_000 {
        return Err("Input exceeds 20,000 characters".to_string());
    }
    let response_format = payload
        .get("response_format")
        .and_then(Value::as_str)
        .unwrap_or("wav");
    if !matches!(response_format, "wav" | "flac") {
        return Err("response_format must be wav or flac".to_string());
    }
    let registry = read_json(runtime.model_registry_path.clone(), json!({ "models": [] }));
    let engine = registry
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| {
            models
                .iter()
                .find(|model| model.get("model_id").and_then(Value::as_str) == Some(model_id))
        })
        .and_then(|model| model.get("engine"))
        .and_then(Value::as_str)
        .ok_or("Requested model is not installed")?;
    let requested_voice = payload
        .get("voice")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let (speaker, reference_audio_path, voice_name) = if matches!(engine, "kokoro" | "foundation") {
        (
            requested_voice.to_string(),
            None,
            requested_voice.to_string(),
        )
    } else if engine == "speecht5" && requested_voice == "default" {
        ("default".to_string(), None, "SpeechT5 default".to_string())
    } else if engine == "breeze" && requested_voice == "default" {
        (
            "default".to_string(),
            None,
            "Breeze TTS 2 default".to_string(),
        )
    } else if engine == "fish-speech" && requested_voice == "default" {
        (
            "default".to_string(),
            None,
            "Fish Speech 1.5 default".to_string(),
        )
    } else if requested_voice == "default" && matches!(engine, "chatterbox" | "chatterbox-turbo") {
        (
            "default".to_string(),
            None,
            "Chatterbox default".to_string(),
        )
    } else {
        let (name, path) = runtime
            .store
            .voice_reference_for_id(requested_voice)?
            .ok_or("This engine requires a clone-ready consent-backed voice profile ID")?;
        ("default".to_string(), Some(path), name)
    };
    let mut request = json!({
        "model_id": model_id,
        "text": input,
        "speaker": speaker,
        "reference_audio_path": reference_audio_path,
        "language": payload.get("language").and_then(Value::as_str).unwrap_or("en"),
        "speed": payload.get("speed").and_then(Value::as_f64).unwrap_or(1.0),
        "seed": payload.get("seed").and_then(Value::as_i64).unwrap_or(42817),
        "output_format": response_format,
        "title": "Local API generation",
        "voice_name": voice_name,
        "priority": payload.get("priority").cloned().unwrap_or_else(|| json!("normal")),
    });
    priority_value(request.get("priority"))?;
    for control in ["temperature", "top_p", "repetition_penalty", "cfg_scale"] {
        if let Some(value) = payload.get(control).filter(|value| !value.is_null()) {
            request
                .as_object_mut()
                .expect("API synthesis request is an object")
                .insert(control.to_string(), value.clone());
        }
    }
    if let Some(instruction) = payload.get("instruction").and_then(Value::as_str) {
        if instruction.chars().count() > 1_000 {
            return Err("Voice instructions are limited to 1,000 characters".to_string());
        }
        request
            .as_object_mut()
            .expect("API synthesis request is an object")
            .insert("instruction".to_string(), json!(instruction));
    }
    Ok((request, response_format.to_string()))
}

fn prepare_music_generation_request(mut request: Value) -> Result<Value, String> {
    let prompt = request
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("A music prompt is required")?;
    if prompt.chars().count() > 1_000 {
        return Err("A music prompt is limited to 1,000 characters".to_string());
    }
    if let Some(lyrics) = request.get("lyrics") {
        let lyrics = lyrics.as_str().ok_or("Lyrics must be plain text")?.trim();
        if lyrics.chars().count() > 4_096 {
            return Err("Lyrics are limited to 4,096 characters".to_string());
        }
    }
    if let Some(language) = request.get("vocal_language") {
        let language = language.as_str().ok_or("Vocal language must be text")?;
        if language.trim().chars().count() > 32 {
            return Err("Vocal language is too long".to_string());
        }
    }
    let model_id = request
        .get("model_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("A music model is required")?;
    validate_model_argument(model_id)?;
    let mode = request
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("song");
    if !matches!(
        mode,
        "song" | "instrumental" | "extend" | "edit-region" | "cover" | "extract"
    ) {
        return Err("The music workflow is not supported".to_string());
    }
    let source_path = request
        .get("source_audio_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let reference_path = request
        .get("reference_audio_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let is_acestep = model_id.starts_with("ACE-Step/");
    if !is_acestep && !matches!(mode, "song" | "instrumental") {
        return Err("Advanced music workflows require an ACE-Step model".to_string());
    }
    if !is_acestep && (source_path.is_some() || reference_path.is_some()) {
        return Err("MusicGen does not accept audio conditioning".to_string());
    }
    if matches!(mode, "extend" | "edit-region" | "cover" | "extract") && source_path.is_none() {
        return Err(format!("The {mode} workflow requires a source audio file"));
    }
    if source_path.is_some() && !matches!(mode, "extend" | "edit-region" | "cover" | "extract") {
        return Err(
            "Source audio is only accepted by an extend, edit-region, cover, or extract workflow"
                .to_string(),
        );
    }
    for path in [source_path, reference_path].into_iter().flatten() {
        let candidate = Path::new(path);
        let extension = candidate
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        if !candidate.is_absolute()
            || !candidate.is_file()
            || !extension
                .as_deref()
                .is_some_and(|value| matches!(value, "wav" | "flac" | "mp3" | "m4a" | "ogg"))
        {
            return Err("Music reference and source audio must be an existing WAV, FLAC, MP3, M4A, or OGG file".to_string());
        }
    }
    if reference_path.is_some() {
        if request
            .get("reference_consent_confirmed")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(
                "Confirm that you own or have permission to use the reference audio".to_string(),
            );
        }
        let basis = request
            .get("reference_consent_basis")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if basis.is_empty() || basis.chars().count() > 240 {
            return Err("Provide a short permission basis for the reference audio".to_string());
        }
    }
    let variations = request
        .get("variations")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if !matches!(variations, 1 | 2 | 4) {
        return Err("Music variations must be 1, 2, or 4".to_string());
    }
    if let Some(sections) = request.get("song_sections") {
        let sections = sections.as_array().ok_or("Song sections must be a list")?;
        if sections.len() > 24 {
            return Err("A song can contain at most 24 sections".to_string());
        }
        for section in sections {
            let section = section
                .as_object()
                .ok_or("Song section entries must be objects")?;
            let section_type = section.get("type").and_then(Value::as_str).unwrap_or("");
            if !matches!(
                section_type,
                "intro" | "verse" | "pre-chorus" | "chorus" | "bridge" | "instrumental" | "outro"
            ) {
                return Err("A song section has an invalid type".to_string());
            }
            if section
                .get("lyrics")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .count()
                > 1_200
            {
                return Err("A song section is limited to 1,200 lyric characters".to_string());
            }
        }
    }
    if let Some(timing) = request.get("lyric_timing") {
        let timing = timing.as_array().ok_or("Lyric timing must be a list")?;
        if timing.len() > 400 {
            return Err("Lyric timing is limited to 400 lines".to_string());
        }
        let mut previous_start = -1.0_f64;
        for line in timing {
            let start = line
                .get("start_seconds")
                .and_then(Value::as_f64)
                .ok_or("Lyric timing start must be a number")?;
            let end = line
                .get("end_seconds")
                .and_then(Value::as_f64)
                .ok_or("Lyric timing end must be a number")?;
            if start < previous_start || end <= start {
                return Err("Lyric timing must be ordered with positive durations".to_string());
            }
            previous_start = start;
        }
    }
    if mode == "edit-region" {
        let start = request
            .get("repainting_start")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let end = request
            .get("repainting_end")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if start < 0.0 || end <= start {
            return Err("The edit region must have an ordered positive duration".to_string());
        }
    }
    if let Some(format) = request.get("output_format").and_then(Value::as_str) {
        if !matches!(format, "wav" | "flac") {
            return Err("Music output format must be wav or flac".to_string());
        }
    }
    if let Some(operation) = request.get("operation").and_then(Value::as_str) {
        if operation != "generate_music" {
            return Err("A music request cannot override its runtime operation".to_string());
        }
    }
    if let Some(kind) = request.get("generation_kind").and_then(Value::as_str) {
        if kind != "music" {
            return Err("A music request must use generation_kind music".to_string());
        }
    }
    let object = request
        .as_object_mut()
        .ok_or("A music request must be a JSON object")?;
    object.insert("operation".to_string(), json!("generate_music"));
    object.insert("generation_kind".to_string(), json!("music"));
    Ok(request)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Error",
    };
    write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n", body.len()).and_then(|_| stream.write_all(body)).and_then(|_| stream.flush()).map_err(|error| format!("Could not write API response: {error}"))
}

fn validate_model_argument(model_id: &str) -> Result<(), String> {
    let segments: Vec<&str> = model_id.split('/').collect();
    if model_id.is_empty()
        || model_id.len() > 160
        || segments.len() != 2
        || segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
        || !model_id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '/' | '-' | '_' | '.'))
    {
        return Err("The model identifier is invalid.".to_string());
    }
    Ok(())
}

fn validate_engine_argument(engine: &str) -> Result<(), String> {
    if matches!(
        engine,
        "kokoro"
            | "transformers"
            | "speaker-verification"
            | "alignment"
            | "speecht5"
            | "chatterbox"
            | "chatterbox-turbo"
            | "coqui"
            | "nemo"
            | "musicgen"
            | "acestep"
            | "breeze"
            | "fish-speech"
    ) {
        Ok(())
    } else {
        Err("The engine identifier is not supported by this release.".to_string())
    }
}

fn validate_revision(revision: &str) -> Result<(), String> {
    if revision.len() != 40 || !revision.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err("Model installation requires a pinned 40-character revision.".to_string());
    }
    Ok(())
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

fn managed_cuda_library_path(
    runtime_data_root: &Path,
) -> Result<Option<std::ffi::OsString>, String> {
    let venv_lib = runtime_data_root.join(".venv/lib");
    let entries = match fs::read_dir(&venv_lib) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not inspect the managed CUDA runtime at {}: {error}",
                venv_lib.display()
            ))
        }
    };
    let canonical_root = fs::canonicalize(runtime_data_root).map_err(|error| {
        format!(
            "Could not validate the managed runtime directory {}: {error}",
            runtime_data_root.display()
        )
    })?;
    let mut python_dirs = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("python"))
        })
        .collect::<Vec<_>>();
    python_dirs.sort();

    let mut library_dirs = Vec::new();
    for python_dir in python_dirs {
        for package in ["cublas", "cudnn"] {
            let candidate = python_dir
                .join("site-packages/nvidia")
                .join(package)
                .join("lib");
            let Ok(canonical) = fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(&canonical_root) && canonical.is_dir() {
                library_dirs.push(canonical);
            }
        }
    }
    if library_dirs.is_empty() {
        return Ok(None);
    }
    library_dirs.extend(
        env::var_os("LD_LIBRARY_PATH")
            .iter()
            .flat_map(env::split_paths),
    );
    env::join_paths(library_dirs)
        .map(Some)
        .map_err(|error| format!("Could not prepare the managed CUDA library path: {error}"))
}

fn product_state_dir() -> PathBuf {
    home_dir().join(".soundAr/state")
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

#[tauri::command]
fn read_generated_audio(
    state: tauri::State<'_, RuntimeState>,
    path: String,
) -> Result<tauri::ipc::Response, String> {
    state
        .store
        .generated_audio_bytes(&path)
        .map(tauri::ipc::Response::new)
}

#[tauri::command]
fn read_job_preview(
    state: tauri::State<'_, RuntimeState>,
    job_id: String,
) -> Result<tauri::ipc::Response, String> {
    state
        .store
        .job_preview_audio(&job_id)
        .map(tauri::ipc::Response::new)
}

#[tauri::command]
fn read_transcription_audio(
    state: tauri::State<'_, RuntimeState>,
    path: String,
) -> Result<tauri::ipc::Response, String> {
    state
        .store
        .transcription_audio_bytes(&path)
        .map(tauri::ipc::Response::new)
}

#[tauri::command]
fn read_voice_audio(
    state: tauri::State<'_, RuntimeState>,
    path: String,
) -> Result<tauri::ipc::Response, String> {
    state
        .store
        .voice_audio_bytes(&path)
        .map(tauri::ipc::Response::new)
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
                    "python_ready": foundation_runtime_ready(python)
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
        "python_ready": foundation_runtime_ready(python)
    })
}

fn foundation_runtime_ready(python: &Path) -> bool {
    foundation_runtime_ready_for_install(python, cfg!(debug_assertions))
}

fn foundation_runtime_ready_for_install(python: &Path, allow_unmanaged: bool) -> bool {
    if !python.is_file() {
        return false;
    }
    if allow_unmanaged {
        return true;
    }
    let Some(runtime_dir) = python
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    else {
        return false;
    };
    let manifest = read_json(runtime_dir.join("runtime.json"), json!({}));
    manifest.get("schema_version").and_then(Value::as_u64) == Some(2)
        && manifest.get("transformers").and_then(Value::as_str) == Some("5.5.0")
}

fn engine_runtime_states(capabilities: &Value, worker_pool: &[PythonProcess]) -> Vec<Value> {
    capabilities
        .get("engines")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|manifest| {
            let engine = manifest.get("id")?.as_str()?;
            let root = app_data_dir().join("engines").join(engine);
            let python = root.join(".venv/bin/python");
            let runtime_manifest = read_json(root.join("runtime.json"), json!({}));
            let current = runtime_manifest
                .get("schema_version")
                .and_then(Value::as_u64)
                == Some(2)
                && runtime_manifest
                    .get("foundation_schema")
                    .and_then(Value::as_u64)
                    == Some(2);
            let mut loaded_models = worker_pool
                .iter()
                .filter(|process| process.engine == engine)
                .flat_map(|process| process.loaded_models.iter().cloned())
                .collect::<Vec<_>>();
            loaded_models.sort();
            loaded_models.dedup();
            Some(json!({
                "engine": engine,
                "state": if python.is_file() && current { "layered" } else { "needs-setup" },
                "python_path": if python.is_file() && current { python.to_string_lossy().to_string() } else { python_path().to_string_lossy().to_string() },
                "runtime_manifest": runtime_manifest,
                "warm_workers": worker_pool.iter().filter(|process| process.engine == engine).count(),
                "loaded_models": loaded_models,
            }))
        })
        .collect()
}

#[tauri::command]
fn bootstrap_state(state: tauri::State<'_, RuntimeState>) -> Result<Value, String> {
    let root = &state.runtime_root;
    let home = home_dir();
    let catalog = read_json(
        root.join("data/curated_models.json"),
        json!({ "models": [] }),
    );
    let registry = read_json(state.model_registry_path.clone(), json!({ "models": [] }));
    let engine_capabilities = read_json(
        root.join("data/engine_manifests.json"),
        json!({ "engines": [] }),
    );
    let engine_runtimes = {
        let worker_pool = state
            .worker_pool
            .lock()
            .map_err(|_| "Python worker pool lock failed")?;
        engine_runtime_states(&engine_capabilities, &worker_pool)
    };
    let voices = state.store.list_voices()?;
    let presets = state.store.list_presets()?;
    let projects = state.store.list_projects()?;
    let transcriptions = state.store.list_transcriptions()?;
    let benchmarks = state.store.list_benchmarks()?;
    let batches = state.store.list_batches()?;
    let comparisons = state.store.list_comparisons()?;
    let jobs = state.store.list_jobs()?;
    let settings = state.store.application_settings()?;
    Ok(json!({
        "catalog": catalog.get("models").cloned().unwrap_or_else(|| json!([])),
        "installed": registry.get("models").cloned().unwrap_or_else(|| json!([])),
        "system": gpu_status(&state.python_path),
        "scheduler": state.scheduler_status()?,
        "export_dir": home.join(".soundAr/exports").to_string_lossy(),
        "voices": voices,
        "presets": presets,
        "projects": projects,
        "transcriptions": transcriptions,
        "benchmarks": benchmarks,
        "batches": batches,
        "comparisons": comparisons,
        "jobs": jobs,
        "settings": settings,
        "features": {
            "generate": "stable",
            "video": "stable",
            "projects": "beta",
            "models": "beta",
            "voices": "beta",
            "history": "beta",
            "compare": "experimental",
            "benchmarks": "experimental",
            "live": "beta",
            "transcribe": "beta",
            "developer_api": "beta"
        },
        "engine_capabilities": engine_capabilities.get("engines").cloned().unwrap_or_else(|| json!([])),
        "engine_runtimes": engine_runtimes,
        "install_kind": install_kind(),
        "runtime": "tauri"
    }))
}

#[tauri::command]
async fn synthesize(
    state: tauri::State<'_, RuntimeState>,
    request: Value,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    if let Some(reference) = request.get("reference_audio_path").and_then(Value::as_str) {
        runtime.store.validate_voice_reference(reference)?;
    }
    let job_id = runtime.store.create_job("synthesis", &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    tauri::async_runtime::spawn_blocking(move || {
        match runtime.request_for_job(request.clone(), &job_id) {
            Ok(result) => runtime.store.complete_synthesis(&job_id, &request, &result),
            Err(error) => {
                if runtime.store.job_status(&job_id)?.as_deref() != Some("cancelled") {
                    runtime.store.fail_job(&job_id, &error)?;
                }
                Err(error)
            }
        }
    })
    .await
    .map_err(|error| format!("Synthesis worker failed: {error}"))?
}

#[tauri::command]
async fn generate_music(
    state: tauri::State<'_, RuntimeState>,
    request: Value,
) -> Result<Value, String> {
    let request = prepare_music_generation_request(request)?;
    let runtime = state.inner().clone();
    let job_id = runtime.store.create_job("music-generation", &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    tauri::async_runtime::spawn_blocking(move || {
        match runtime.request_for_job(request.clone(), &job_id) {
            Ok(result) => runtime.store.complete_synthesis(&job_id, &request, &result),
            Err(error) => {
                if runtime.store.job_status(&job_id)?.as_deref() != Some("cancelled") {
                    runtime.store.fail_job(&job_id, &error)?;
                }
                Err(error)
            }
        }
    })
    .await
    .map_err(|error| format!("Music generation worker failed: {error}"))?
}

#[tauri::command]
fn queue_synthesis(state: tauri::State<'_, RuntimeState>, request: Value) -> Result<Value, String> {
    let runtime = state.inner().clone();
    if let Some(reference) = request.get("reference_audio_path").and_then(Value::as_str) {
        runtime.store.validate_voice_reference(reference)?;
    }
    let job_id = runtime.store.create_job("synthesis", &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    let queued_id = job_id.clone();
    let queued_title = request
        .get("title")
        .or_else(|| request.get("text"))
        .cloned();
    let queued_model = request.get("model_id").cloned();
    let queued_priority = request
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("normal")
        .to_string();
    runtime.start_background_synthesis(job_id, request)?;
    Ok(json!({
        "id": queued_id,
        "kind": "synthesis",
        "status": "preparing",
        "progress": 0.05,
        "attempt": 1,
        "priority": queued_priority,
        "title": queued_title,
        "model_id": queued_model,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    }))
}

#[tauri::command]
fn queue_music_generation(
    state: tauri::State<'_, RuntimeState>,
    request: Value,
) -> Result<Value, String> {
    let request = prepare_music_generation_request(request)?;
    let runtime = state.inner().clone();
    let job_id = runtime.store.create_job("music-generation", &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    let queued_id = job_id.clone();
    let queued_title = request
        .get("title")
        .or_else(|| request.get("prompt"))
        .cloned();
    let queued_model = request.get("model_id").cloned();
    let queued_priority = request
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("normal")
        .to_string();
    runtime.start_background_synthesis(job_id, request)?;
    Ok(json!({
        "id": queued_id,
        "kind": "music-generation",
        "status": "preparing",
        "progress": 0.05,
        "attempt": 1,
        "priority": queued_priority,
        "title": queued_title,
        "model_id": queued_model,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    }))
}

#[tauri::command]
async fn transcribe_audio(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
    audio_path: String,
    cleanup: Option<bool>,
) -> Result<Value, String> {
    validate_model_argument(&model_id)?;
    let runtime = state.inner().clone();
    let managed_audio_path = runtime.store.import_transcription_source(&audio_path)?;
    let original_audio = managed_audio_path.to_string_lossy().to_string();
    let cleanup_enabled = cleanup.unwrap_or(false);
    let (managed_audio, processing) = if cleanup_enabled {
        let output_path = managed_audio_path
            .parent()
            .ok_or("Transcription source has no managed directory")?
            .join(format!("cleaned-{}.wav", Uuid::new_v4().simple()));
        let prepared = runtime.request(json!({
            "operation": "prepare_transcription_audio",
            "audio_path": original_audio,
            "output_path": output_path,
        }))?;
        let path = prepared
            .get("audio_path")
            .and_then(Value::as_str)
            .ok_or("Speech cleanup returned no audio path")?
            .to_string();
        (
            path,
            prepared
                .get("processing")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
    } else {
        (
            original_audio.clone(),
            json!({ "schema_version": 1, "algorithm": "none" }),
        )
    };
    let request = json!({
        "operation": "transcribe",
        "model_id": model_id,
        "audio_path": managed_audio,
        "original_audio_path": original_audio,
        "processing": processing,
    });
    let job_id = runtime.store.create_job("transcription", &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    tauri::async_runtime::spawn_blocking(move || match runtime.request_for_job(request, &job_id) {
        Ok(result) => runtime.store.complete_transcription(
            &job_id,
            &managed_audio,
            &original_audio,
            &processing,
            &result,
        ),
        Err(error) => {
            if runtime.store.job_status(&job_id)?.as_deref() != Some("cancelled") {
                runtime.store.fail_job(&job_id, &error)?;
            }
            Err(error)
        }
    })
    .await
    .map_err(|error| format!("Transcription worker failed: {error}"))?
}

#[tauri::command]
fn update_transcription(
    state: tauri::State<'_, RuntimeState>,
    transcription_id: String,
    text: String,
    segments: Value,
) -> Result<Value, String> {
    state
        .store
        .update_transcription(&transcription_id, &text, &segments)
}

#[tauri::command]
async fn diarize_transcription(
    state: tauri::State<'_, RuntimeState>,
    transcription_id: String,
    model_id: String,
    speaker_count: Option<u8>,
) -> Result<Value, String> {
    validate_model_argument(&model_id)?;
    if speaker_count.is_some_and(|count| !(1..=8).contains(&count)) {
        return Err("Speaker count must be between 1 and 8".to_string());
    }
    let runtime = state.inner().clone();
    let source = runtime
        .store
        .transcription_diarization_request(&transcription_id)?;
    let request = json!({
        "operation": "diarize",
        "model_id": model_id,
        "audio_path": source.get("audio_path").cloned().unwrap_or(Value::Null),
        "words": source.get("words").cloned().unwrap_or_else(|| json!([])),
        "speaker_count": speaker_count,
        "priority": "high",
    });
    let job_id = runtime.store.create_job("speaker-diarization", &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    tauri::async_runtime::spawn_blocking(move || match runtime.request_for_job(request, &job_id) {
        Ok(result) => {
            runtime
                .store
                .complete_transcription_diarization(&job_id, &transcription_id, &result)
        }
        Err(error) => {
            if runtime.store.job_status(&job_id)?.as_deref() != Some("cancelled") {
                runtime.store.fail_job(&job_id, &error)?;
            }
            Err(error)
        }
    })
    .await
    .map_err(|error| format!("Speaker separation worker failed: {error}"))?
}

#[tauri::command]
async fn align_transcription(
    state: tauri::State<'_, RuntimeState>,
    transcription_id: String,
    model_id: String,
) -> Result<Value, String> {
    validate_model_argument(&model_id)?;
    let runtime = state.inner().clone();
    let source = runtime
        .store
        .transcription_alignment_request(&transcription_id)?;
    let request = json!({
        "operation": "align_transcript",
        "model_id": model_id,
        "audio_path": source.get("audio_path").cloned().unwrap_or(Value::Null),
        "segments": source.get("segments").cloned().unwrap_or_else(|| json!([])),
        "source_revision": source.get("source_revision").cloned().unwrap_or(Value::Null),
        "source_text_sha256": source.get("source_text_sha256").cloned().unwrap_or(Value::Null),
        "priority": "high",
    });
    let job_id = runtime.store.create_job("forced-alignment", &request)?;
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    tauri::async_runtime::spawn_blocking(move || match runtime.request_for_job(request, &job_id) {
        Ok(result) => {
            runtime
                .store
                .complete_transcription_alignment(&job_id, &transcription_id, &result)
        }
        Err(error) => {
            if runtime.store.job_status(&job_id)?.as_deref() != Some("cancelled") {
                runtime.store.fail_job(&job_id, &error)?;
            }
            Err(error)
        }
    })
    .await
    .map_err(|error| format!("Forced-alignment worker failed: {error}"))?
}

#[tauri::command]
fn update_transcription_speaker_labels(
    state: tauri::State<'_, RuntimeState>,
    transcription_id: String,
    labels: Value,
) -> Result<Value, String> {
    state
        .store
        .update_transcription_speaker_labels(&transcription_id, &labels)
}

#[tauri::command]
fn cancel_active_synthesis(state: tauri::State<'_, RuntimeState>) -> Result<bool, String> {
    state.cancel_all_active_syntheses()
}

#[tauri::command]
fn cancel_job(state: tauri::State<'_, RuntimeState>, job_id: String) -> Result<bool, String> {
    state.cancel_job(&job_id)
}

#[tauri::command]
fn retry_job(state: tauri::State<'_, RuntimeState>, job_id: String) -> Result<Value, String> {
    let runtime = state.inner().clone();
    let (job, request) = runtime.store.retry_synthesis_job(&job_id)?;
    runtime.start_background_synthesis(job_id, request)?;
    Ok(job)
}

#[tauri::command]
fn clear_finished_jobs(state: tauri::State<'_, RuntimeState>) -> Result<usize, String> {
    state.store.clear_finished_jobs()
}

#[tauri::command]
fn save_application_setting(
    state: tauri::State<'_, RuntimeState>,
    key: String,
    value: Value,
) -> Result<Value, String> {
    state.store.save_application_setting(&key, &value)
}

#[tauri::command]
async fn list_history(
    state: tauri::State<'_, RuntimeState>,
    query: Option<String>,
    filters: Option<Value>,
) -> Result<Vec<Value>, String> {
    let store = Arc::clone(&state.store);
    tauri::async_runtime::spawn_blocking(move || {
        store.list_history_filtered(query.as_deref(), filters.as_ref())
    })
    .await
    .map_err(|error| format!("History worker failed: {error}"))?
}

#[tauri::command]
fn duplicate_history(state: tauri::State<'_, RuntimeState>, id: String) -> Result<Value, String> {
    state.store.duplicate_history(&id)
}

#[tauri::command]
fn export_history(
    state: tauri::State<'_, RuntimeState>,
    id: String,
    destination: String,
) -> Result<Value, String> {
    state.store.export_history(&id, &destination)
}

#[tauri::command]
async fn delete_history(
    state: tauri::State<'_, RuntimeState>,
    id: String,
    delete_audio: bool,
) -> Result<bool, String> {
    let store = Arc::clone(&state.store);
    tauri::async_runtime::spawn_blocking(move || store.delete_history(&id, delete_audio))
        .await
        .map_err(|error| format!("History deletion worker failed: {error}"))?
}

#[tauri::command]
fn list_jobs(state: tauri::State<'_, RuntimeState>) -> Result<Vec<Value>, String> {
    state.store.list_jobs()
}

#[tauri::command]
fn scheduler_status(state: tauri::State<'_, RuntimeState>) -> Result<Value, String> {
    state.scheduler_status()
}

#[tauri::command]
fn create_batch(state: tauri::State<'_, RuntimeState>, request: Value) -> Result<Value, String> {
    state.store.create_batch(&request)
}

fn read_batch_import(path: &Path) -> Result<Value, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("Could not inspect the batch file: {error}"))?;
    if !metadata.is_file() {
        return Err("The selected batch input is not a file".to_string());
    }
    if metadata.len() > MAX_BATCH_IMPORT_BYTES {
        return Err("Batch import files cannot exceed 8 MB".to_string());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or("The batch input has no extension")?;
    let content = fs::read_to_string(path)
        .map_err(|error| format!("Could not read the batch file as UTF-8: {error}"))?;
    let rows = match extension.as_str() {
        "txt" => content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|text| json!({ "text": text }))
            .collect::<Vec<_>>(),
        "jsonl" => content
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                serde_json::from_str::<Value>(line)
                    .map_err(|error| format!("JSONL row {} is invalid: {error}", index + 1))
            })
            .collect::<Result<Vec<_>, _>>()?,
        "csv" => {
            let mut reader = csv::ReaderBuilder::new()
                .flexible(false)
                .from_reader(content.as_bytes());
            let headers = reader
                .headers()
                .map_err(|error| format!("Could not read CSV headers: {error}"))?
                .clone();
            let text_column = headers
                .iter()
                .position(|header| header.trim().eq_ignore_ascii_case("text"))
                .ok_or("CSV batch imports require a text column")?;
            let column = |name: &str| {
                headers
                    .iter()
                    .position(|header| header.trim().eq_ignore_ascii_case(name))
            };
            let mut rows = Vec::new();
            for (index, record) in reader.records().enumerate() {
                let record =
                    record.map_err(|error| format!("CSV row {} is invalid: {error}", index + 2))?;
                let mut settings = serde_json::Map::new();
                for key in ["model_id", "speaker", "language", "output_format"] {
                    if let Some(value) = column(key)
                        .and_then(|position| record.get(position))
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        settings.insert(key.to_string(), json!(value));
                    }
                }
                for key in [
                    "speed",
                    "exaggeration",
                    "cfg_weight",
                    "temperature",
                    "top_p",
                    "repetition_penalty",
                ] {
                    if let Some(value) = column(key)
                        .and_then(|position| record.get(position))
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        settings.insert(
                            key.to_string(),
                            json!(value.parse::<f64>().map_err(|_| format!(
                                "CSV row {} has an invalid {key}",
                                index + 2
                            ))?),
                        );
                    }
                }
                if let Some(value) = column("seed")
                    .and_then(|position| record.get(position))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    settings.insert(
                        "seed".to_string(),
                        json!(value
                            .parse::<i64>()
                            .map_err(|_| format!("CSV row {} has an invalid seed", index + 2))?),
                    );
                }
                let mut row = json!({
                    "text": record.get(text_column).unwrap_or(""),
                    "name": column("name").and_then(|position| record.get(position)).unwrap_or(""),
                    "output_name": column("output_name").and_then(|position| record.get(position)).unwrap_or(""),
                    "settings": settings,
                });
                if let Some(priority) = column("priority")
                    .and_then(|position| record.get(position))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    row.as_object_mut()
                        .expect("CSV batch row is an object")
                        .insert("priority".to_string(), json!(priority));
                }
                rows.push(row);
            }
            rows
        }
        _ => return Err("Batch input must be TXT, CSV, or JSONL".to_string()),
    };
    let normalized = normalize_batch_rows(&json!({ "rows": rows }))?;
    Ok(json!({
        "name": path.file_stem().and_then(|value| value.to_str()).unwrap_or("Imported batch"),
        "source_format": extension,
        "rows": normalized,
    }))
}

#[tauri::command]
fn import_batch_file(source_path: String) -> Result<Value, String> {
    read_batch_import(Path::new(&source_path))
}

#[tauri::command]
fn queue_batch(
    state: tauri::State<'_, RuntimeState>,
    request: Value,
    parallelism: usize,
) -> Result<Value, String> {
    state.queue_batch(&request, parallelism.clamp(1, 8))
}

#[tauri::command]
async fn execute_batch(
    state: tauri::State<'_, RuntimeState>,
    batch_id: String,
    parallelism: usize,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.execute_batch(&batch_id, parallelism))
        .await
        .map_err(|error| format!("Batch coordinator failed: {error}"))?
}

#[tauri::command]
fn get_batch(state: tauri::State<'_, RuntimeState>, batch_id: String) -> Result<Value, String> {
    state
        .store
        .get_batch(&batch_id)?
        .ok_or_else(|| "The selected batch was not found".to_string())
}

#[tauri::command]
fn cancel_batch(state: tauri::State<'_, RuntimeState>, batch_id: String) -> Result<Value, String> {
    state.cancel_batch(&batch_id)
}

#[tauri::command]
fn pause_batch(state: tauri::State<'_, RuntimeState>, batch_id: String) -> Result<Value, String> {
    state.pause_batch(&batch_id)
}

#[tauri::command]
fn resume_batch(
    state: tauri::State<'_, RuntimeState>,
    batch_id: String,
    parallelism: usize,
    retry_failed: bool,
) -> Result<Value, String> {
    state.resume_batch(&batch_id, parallelism, retry_failed)
}

#[tauri::command]
fn update_batch_item(
    state: tauri::State<'_, RuntimeState>,
    batch_id: String,
    item_index: i64,
    status: String,
    history_id: Option<String>,
    error: Option<String>,
) -> Result<Value, String> {
    state.store.update_batch_item(
        &batch_id,
        item_index,
        &status,
        history_id.as_deref(),
        error.as_deref(),
    )
}

#[tauri::command]
fn list_batches(state: tauri::State<'_, RuntimeState>) -> Result<Vec<Value>, String> {
    state.store.list_batches()
}

#[tauri::command]
fn update_history_metadata(
    state: tauri::State<'_, RuntimeState>,
    id: String,
    changes: Value,
) -> Result<Value, String> {
    state.store.update_history_metadata(&id, &changes)
}

#[tauri::command]
fn history_request(state: tauri::State<'_, RuntimeState>, id: String) -> Result<Value, String> {
    state.store.history_request(&id)
}

#[tauri::command]
fn save_comparison(
    state: tauri::State<'_, RuntimeState>,
    comparison: Value,
) -> Result<Value, String> {
    state.store.save_comparison(&comparison)
}

#[tauri::command]
fn create_comparison(
    state: tauri::State<'_, RuntimeState>,
    request: Value,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    for take in request
        .get("takes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(reference) = take.get("reference_audio_path").and_then(Value::as_str) {
            runtime.store.validate_voice_reference(reference)?;
        }
    }
    let comparison = runtime.store.create_comparison(&request)?;
    let comparison_id = comparison
        .get("id")
        .and_then(Value::as_str)
        .ok_or("The comparison did not return an ID")?
        .to_string();
    let coordinator = runtime.clone();
    let coordinator_id = comparison_id.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("soundar-comparison".to_string())
        .spawn(move || {
            let _ = coordinator.execute_comparison(&coordinator_id);
        })
    {
        let error = format!("Could not start comparison coordinator: {error}");
        for (take_id, job_id) in runtime.store.comparison_active_jobs(&comparison_id)? {
            runtime.store.fail_job(&job_id, &error)?;
            runtime
                .store
                .finish_comparison_take(&comparison_id, &take_id, None, Some(&error))?;
        }
        return Err(error);
    }
    Ok(comparison)
}

#[tauri::command]
fn get_comparison(
    state: tauri::State<'_, RuntimeState>,
    comparison_id: String,
) -> Result<Value, String> {
    state
        .store
        .get_comparison(&comparison_id)?
        .ok_or_else(|| "The selected comparison was not found".to_string())
}

#[tauri::command]
fn update_comparison_review(
    state: tauri::State<'_, RuntimeState>,
    comparison_id: String,
    changes: Value,
) -> Result<Value, String> {
    state
        .store
        .update_comparison_review(&comparison_id, &changes)
}

#[tauri::command]
fn cancel_comparison(
    state: tauri::State<'_, RuntimeState>,
    comparison_id: String,
) -> Result<bool, String> {
    state.cancel_comparison(&comparison_id)
}

#[tauri::command]
fn list_presets(state: tauri::State<'_, RuntimeState>) -> Result<Vec<Value>, String> {
    state.store.list_presets()
}

#[tauri::command]
fn save_preset(state: tauri::State<'_, RuntimeState>, preset: Value) -> Result<Value, String> {
    state.store.save_preset(&preset)
}

#[tauri::command]
fn save_benchmark(state: tauri::State<'_, RuntimeState>, result: Value) -> Result<Value, String> {
    state.store.save_benchmark(&result)
}

#[tauri::command]
fn prepare_benchmark_engine(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
) -> Result<Value, String> {
    state.prepare_benchmark_engine(&model_id)
}

#[tauri::command]
fn release_benchmark_engine(
    state: tauri::State<'_, RuntimeState>,
    token: String,
) -> Result<bool, String> {
    state.release_benchmark_engine(&token)
}

#[tauri::command]
fn list_projects(state: tauri::State<'_, RuntimeState>) -> Result<Vec<Value>, String> {
    state.store.list_projects()
}

#[tauri::command]
fn save_project(state: tauri::State<'_, RuntimeState>, project: Value) -> Result<Value, String> {
    state.store.save_project(&project)
}

#[tauri::command]
fn import_project_script(
    state: tauri::State<'_, RuntimeState>,
    source_path: String,
) -> Result<Value, String> {
    let path = PathBuf::from(source_path);
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("Could not inspect the selected script: {error}"))?;
    if !metadata.is_file() || metadata.len() > 5 * 1024 * 1024 {
        return Err("Project scripts must be files no larger than 5 MB".to_string());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "txt" | "md" | "markdown" | "csv" | "jsonl" | "srt"
    ) {
        return Err("Project import supports TXT, Markdown, CSV, JSONL, and SRT".to_string());
    }
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read the selected script as UTF-8: {error}"))?;
    let chapters = parse_project_script(&source, &extension)?;
    let name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Imported project");
    state.store.save_project(&json!({
        "name": name,
        "document": {
            "script": chapters.iter().filter_map(|chapter| chapter.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n\n"),
            "chapters": chapters,
            "speaker_assignments": {},
            "source": { "path": path.to_string_lossy(), "format": extension }
        }
    }))
}

#[tauri::command]
async fn export_project_master(
    state: tauri::State<'_, RuntimeState>,
    project_id: String,
    settings: Value,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        build_project_master(&runtime, project_id, settings)
    })
    .await
    .map_err(|error| format!("Project export worker failed: {error}"))?
}

fn build_project_master(
    runtime: &RuntimeState,
    project_id: String,
    settings: Value,
) -> Result<Value, String> {
    let plan = runtime.store.project_master_plan(&project_id)?;
    let project_name = plan
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("project");
    let format = settings
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("wav");
    if !matches!(format, "wav" | "flac") {
        return Err("Project masters must use WAV or FLAC".to_string());
    }
    let output_id = Uuid::new_v4().simple().to_string();
    let output_path = home_dir().join(".soundAr/exports").join(format!(
        "{}-master-{}.{}",
        safe_file_stem(project_name),
        &output_id[..8],
        format
    ));
    let request = json!({
        "operation": "master_audio",
        "project_id": project_id,
        "audio_paths": plan.get("audio_paths").cloned().unwrap_or_else(|| json!([])),
        "output_path": output_path,
        "sample_rate": settings.get("sample_rate").and_then(Value::as_i64).unwrap_or(48_000),
        "gap_ms": settings.get("gap_ms").and_then(Value::as_i64).unwrap_or(250),
        "fade_ms": settings.get("fade_ms").and_then(Value::as_i64).unwrap_or(12),
        "target_lufs": settings.get("target_lufs").and_then(Value::as_f64).unwrap_or(-16.0),
        "output_format": format,
        "title": format!("{project_name} master"),
        "voice_name": "Project sequence",
        "text": plan.get("document").and_then(|value| value.get("script")).and_then(Value::as_str).unwrap_or("Project master"),
    });
    let job_id = runtime.store.create_job("project-master", &request)?;
    let started = std::time::Instant::now();
    let mut result = match runtime.request_for_job(request.clone(), &job_id) {
        Ok(result) => result,
        Err(error) => {
            runtime.store.fail_job(&job_id, &error)?;
            return Err(error);
        }
    };
    let object = match result.as_object_mut() {
        Some(object) => object,
        None => {
            let error = "Mastering returned an invalid result".to_string();
            runtime.store.fail_job(&job_id, &error)?;
            fs::remove_file(&output_path).ok();
            return Err(error);
        }
    };
    object.insert("id".to_string(), json!(output_id));
    object.insert("model_id".to_string(), json!("soundar/project-master"));
    object.insert("engine".to_string(), json!("finishing"));
    object.insert(
        "inference_seconds".to_string(),
        json!(started.elapsed().as_secs_f64()),
    );
    object.insert("rtf".to_string(), json!(0.0));
    object.insert("vram_peak_mb".to_string(), json!(0));
    let manifest_path = output_path.with_extension(format!("{format}.provenance.json"));
    let publish = (|| -> Result<Value, String> {
        let manifest = json!({
            "schema_version": 1,
            "application": { "name": "soundAr", "version": env!("CARGO_PKG_VERSION") },
            "project": plan,
            "processing": result.get("processing").cloned().unwrap_or_else(|| json!({})),
            "output": {
                "path": output_path,
                "sha256": sha256_path(&output_path)?,
                "generated_at": chrono::Utc::now().to_rfc3339(),
            }
        });
        write_json_atomically(&manifest_path, &manifest)?;
        runtime.store.complete_synthesis(&job_id, &request, &result)
    })();
    let history = match publish {
        Ok(history) => history,
        Err(error) => {
            runtime.store.fail_job(&job_id, &error).ok();
            fs::remove_file(&output_path).ok();
            fs::remove_file(&manifest_path).ok();
            return Err(error);
        }
    };
    let export = runtime.store.record_project_export(
        &project_id,
        history
            .get("id")
            .and_then(Value::as_str)
            .ok_or("Master history has no identifier")?,
        &settings,
        &manifest_path,
    )?;
    let project = runtime
        .store
        .attach_project_master(&project_id, &history, &export)?;
    Ok(json!({ "history": history, "export": export, "project": project }))
}

fn parse_project_script(source: &str, extension: &str) -> Result<Vec<Value>, String> {
    let mut rows: Vec<(String, String)> = match extension {
        "csv" => {
            let mut reader = csv::ReaderBuilder::new()
                .flexible(true)
                .from_reader(source.as_bytes());
            let headers = reader
                .headers()
                .map_err(|error| format!("Invalid CSV header: {error}"))?
                .clone();
            let title_index = headers.iter().position(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "title" | "chapter" | "name"
                )
            });
            let text_index = headers
                .iter()
                .position(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "text" | "script" | "content"
                    )
                })
                .ok_or("CSV import requires a text, script, or content column")?;
            reader
                .records()
                .enumerate()
                .map(|(index, record)| {
                    let record = record
                        .map_err(|error| format!("Invalid CSV row {}: {error}", index + 2))?;
                    Ok((
                        title_index
                            .and_then(|column| record.get(column))
                            .unwrap_or("")
                            .to_string(),
                        record.get(text_index).unwrap_or("").to_string(),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?
        }
        "jsonl" => source
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                let value: Value = serde_json::from_str(line)
                    .map_err(|error| format!("Invalid JSONL line {}: {error}", index + 1))?;
                let text = value
                    .get("text")
                    .or_else(|| value.get("script"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("JSONL line {} requires text", index + 1))?;
                Ok((
                    value
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    text.to_string(),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?,
        "srt" => parse_srt_cues(source),
        "md" | "markdown" => parse_markdown_chapters(source),
        _ => source
            .split("\n\n\n")
            .enumerate()
            .map(|(index, text)| (format!("Chapter {}", index + 1), text.trim().to_string()))
            .collect(),
    };
    rows.retain(|(_, text)| !text.trim().is_empty());
    if rows.is_empty() {
        return Err("The selected script contains no readable text".to_string());
    }
    if rows.len() > 1_000 {
        return Err("Project import is limited to 1,000 chapters".to_string());
    }
    Ok(rows.into_iter().enumerate().map(|(index, (title, text))| json!({
        "id": Uuid::new_v4().simple().to_string(),
        "title": if title.trim().is_empty() { format!("Chapter {}", index + 1) } else { title.trim().to_string() },
        "text": text.trim(),
        "language": "en",
    })).collect())
}

fn parse_markdown_chapters(source: &str) -> Vec<(String, String)> {
    let mut chapters = Vec::new();
    let mut title = String::new();
    let mut body = Vec::new();
    for line in source.lines() {
        if line.starts_with('#') && line.trim_start_matches('#').starts_with(' ') {
            if !body.join("\n").trim().is_empty() {
                chapters.push((title, body.join("\n")));
            }
            title = line.trim_start_matches('#').trim().to_string();
            body.clear();
        } else {
            body.push(line);
        }
    }
    if !body.join("\n").trim().is_empty() {
        chapters.push((title, body.join("\n")));
    }
    chapters
}

fn parse_srt_cues(source: &str) -> Vec<(String, String)> {
    source
        .replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(|block| {
            let lines = block
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>();
            let timing = lines.iter().position(|line| line.contains(" --> "))?;
            let text = lines
                .iter()
                .skip(timing + 1)
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            (!text.is_empty()).then(|| {
                (
                    format!("Cue {}", lines.first().copied().unwrap_or("")),
                    text,
                )
            })
        })
        .collect()
}

fn safe_file_stem(value: &str) -> String {
    let stem = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let collapsed = stem
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "project".to_string()
    } else {
        collapsed.chars().take(64).collect()
    }
}

fn sha256_path(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes =
        fs::read(path).map_err(|error| format!("Could not checksum the master: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn write_json_atomically(path: &Path, value: &Value) -> Result<(), String> {
    let temporary = path.with_extension(format!(
        "{}.partial",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("json")
    ));
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("Could not write provenance: {error}"))?;
    fs::rename(&temporary, path).map_err(|error| format!("Could not publish provenance: {error}"))
}

fn audio_input_devices() -> Result<Vec<Value>, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let devices = host
        .input_devices()
        .map_err(|error| format!("Could not enumerate audio inputs: {error}"))?;
    let mut values = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        let Ok(config) = device.default_input_config() else {
            continue;
        };
        values.push(json!({
            "id": name,
            "name": name,
            "is_default": default_name.as_deref() == Some(name.as_str()),
            "sample_rate": config.sample_rate().0,
            "channels": config.channels(),
            "sample_format": config.sample_format().to_string(),
        }));
    }
    values.sort_by_key(|value| {
        !value
            .get("is_default")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    Ok(values)
}

fn audio_output_devices() -> Result<Vec<Value>, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|device| device.name().ok());
    let devices = host
        .output_devices()
        .map_err(|error| format!("Could not enumerate audio outputs: {error}"))?;
    let mut values = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        let Ok(config) = device.default_output_config() else {
            continue;
        };
        values.push(json!({
            "id": name,
            "name": name,
            "is_default": default_name.as_deref() == Some(name.as_str()),
            "sample_rate": config.sample_rate().0,
            "channels": config.channels(),
            "sample_format": config.sample_format().to_string(),
        }));
    }
    values.sort_by_key(|value| {
        !value
            .get("is_default")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    Ok(values)
}

fn read_wav_mono(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("Could not inspect routed audio: {error}"))?;
    if metadata.len() > 64 * 1024 * 1024 {
        return Err("Routed playback is limited to 64 MB per capture".to_string());
    }
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| format!("Could not decode routed WAV audio: {error}"))?;
    let specification = reader.spec();
    if specification.channels == 0 || specification.sample_rate == 0 {
        return Err("Routed WAV audio has an invalid format".to_string());
    }
    let raw = if specification.sample_format == hound::SampleFormat::Float {
        reader
            .samples::<f32>()
            .map(|sample| sample.map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?
    } else if specification.bits_per_sample <= 16 {
        let scale = (1_u64 << specification.bits_per_sample.saturating_sub(1)) as f32;
        reader
            .samples::<i16>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / scale)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let scale = (1_u64 << specification.bits_per_sample.saturating_sub(1)) as f32;
        reader
            .samples::<i32>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / scale)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let channels = usize::from(specification.channels);
    let mono = raw
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len().max(1) as f32)
        .collect();
    Ok((mono, specification.sample_rate))
}

fn resample_mono(samples: &[f32], source_rate: u32, output_rate: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate == output_rate {
        return samples.to_vec();
    }
    let output_frames =
        ((samples.len() as u64 * u64::from(output_rate)).div_ceil(u64::from(source_rate))) as usize;
    (0..output_frames)
        .map(|index| {
            let position = index as f64 * source_rate as f64 / output_rate as f64;
            let left = position.floor() as usize;
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            samples[left] * (1.0 - fraction) + samples[right] * fraction
        })
        .collect()
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    played_frames: Arc<AtomicU64>,
    underrun_frames: Arc<AtomicU64>,
    completed: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let mut current = Vec::new();
    let mut cursor = 0usize;
    let stream_error = Arc::clone(&error);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                for frame in output.chunks_mut(channels) {
                    while cursor >= current.len() {
                        match receiver.try_recv() {
                            Ok(next) => {
                                current = next;
                                cursor = 0;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                underrun_frames.fetch_add(1, Ordering::Relaxed);
                                frame.fill(T::from_sample(0.0));
                                break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                completed.store(true, Ordering::Release);
                                frame.fill(T::from_sample(0.0));
                                break;
                            }
                        }
                    }
                    if cursor < current.len() {
                        let sample = T::from_sample(current[cursor].clamp(-1.0, 1.0));
                        frame.fill(sample);
                        cursor += 1;
                        played_frames.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            move |failure| {
                if let Ok(mut error) = stream_error.lock() {
                    *error = Some(format!("Audio output stopped: {failure}"));
                }
            },
            None,
        )
        .map_err(|error| format!("Could not open the audio output: {error}"))
}

fn playback_audio_thread(
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
    samples: Vec<f32>,
    stop: Arc<AtomicBool>,
    played_frames: Arc<AtomicU64>,
    underrun_frames: Arc<AtomicU64>,
    completed: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let (sender, receiver) = std::sync::mpsc::sync_channel::<Vec<f32>>(50);
    let chunk_frames = (config.sample_rate().0 / 100).max(1) as usize;
    let chunks = samples
        .chunks(chunk_frames)
        .map(Vec::from)
        .collect::<Vec<_>>();
    let prebuffered = chunks.len().min(25);
    for chunk in chunks.iter().take(prebuffered) {
        sender
            .try_send(chunk.clone())
            .map_err(|_| "Could not pre-buffer routed audio".to_string())?;
    }
    let stream = match config.sample_format() {
        SampleFormat::F32 => build_output_stream::<f32>(
            &device,
            &stream_config,
            receiver,
            Arc::clone(&played_frames),
            Arc::clone(&underrun_frames),
            Arc::clone(&completed),
            Arc::clone(&error),
        ),
        SampleFormat::I16 => build_output_stream::<i16>(
            &device,
            &stream_config,
            receiver,
            Arc::clone(&played_frames),
            Arc::clone(&underrun_frames),
            Arc::clone(&completed),
            Arc::clone(&error),
        ),
        SampleFormat::U16 => build_output_stream::<u16>(
            &device,
            &stream_config,
            receiver,
            Arc::clone(&played_frames),
            Arc::clone(&underrun_frames),
            Arc::clone(&completed),
            Arc::clone(&error),
        ),
        format => Err(format!("Audio output format {format} is not supported")),
    };
    let stream = match stream {
        Ok(stream) => stream,
        Err(failure) => {
            let _ = ready.send(Err(failure.clone()));
            return Err(failure);
        }
    };
    if let Err(failure) = stream.play() {
        let message = format!("Could not start routed playback: {failure}");
        let _ = ready.send(Err(message.clone()));
        return Err(message);
    }
    let _ = ready.send(Ok(()));
    'audio: for chunk in chunks.into_iter().skip(prebuffered) {
        let mut pending = chunk;
        loop {
            if stop.load(Ordering::Acquire) {
                break 'audio;
            }
            match sender.try_send(pending) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                    pending = returned;
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    return Err("Audio output stopped before playback completed".to_string())
                }
            }
        }
    }
    drop(sender);
    while !stop.load(Ordering::Acquire)
        && !completed.load(Ordering::Acquire)
        && error.lock().ok().is_some_and(|value| value.is_none())
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(stream);
    if let Some(failure) = error.lock().ok().and_then(|value| value.clone()) {
        return Err(failure);
    }
    Ok(())
}

fn build_capture_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_sender: std::sync::mpsc::SyncSender<Vec<f32>>,
    peak_bits: Arc<AtomicU32>,
    queued_frames: Arc<AtomicU64>,
    dropped_frames: Arc<AtomicU64>,
    speech_active: Arc<AtomicBool>,
    speech_detected: Arc<AtomicBool>,
    speech_frames: Arc<AtomicU64>,
    silence_frames: Arc<AtomicU64>,
    noise_floor_bits: Arc<AtomicU32>,
    auto_stopped: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    vad_enabled: bool,
    auto_stop: bool,
    silence_ms: u64,
    input_gain: f32,
    error: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let mut detector = VoiceActivityDetector::new(config.sample_rate.0);
    let max_buffered_frames = u64::from(config.sample_rate.0) / 2;
    let data_error = Arc::clone(&error);
    let stream_error = Arc::clone(&error);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let mut callback_peak = 0.0_f32;
                let mut mono_samples = Vec::with_capacity(data.len() / channels.max(1));
                for frame in data.chunks(channels) {
                    let mono = frame
                        .iter()
                        .map(|sample| f32::from_sample(*sample))
                        .sum::<f32>()
                        / frame.len().max(1) as f32
                        * input_gain;
                    let mono = mono.clamp(-1.0, 1.0);
                    callback_peak = callback_peak.max(mono.abs());
                    mono_samples.push(mono);
                }
                peak_bits.store(callback_peak.to_bits(), Ordering::Relaxed);
                let should_auto_stop = if vad_enabled {
                    let snapshot = detector.process(&mono_samples);
                    speech_active.store(snapshot.speech_active, Ordering::Relaxed);
                    speech_detected.store(snapshot.speech_detected, Ordering::Relaxed);
                    speech_frames.store(snapshot.speech_frames, Ordering::Relaxed);
                    silence_frames.store(snapshot.silence_frames, Ordering::Relaxed);
                    noise_floor_bits.store(snapshot.noise_floor.to_bits(), Ordering::Relaxed);
                    auto_stop && detector.should_auto_stop(silence_ms)
                } else {
                    false
                };
                let frame_count = mono_samples.len() as u64;
                if queued_frames
                    .load(Ordering::Relaxed)
                    .saturating_add(frame_count)
                    > max_buffered_frames
                {
                    dropped_frames.fetch_add(frame_count, Ordering::Relaxed);
                } else {
                    queued_frames.fetch_add(frame_count, Ordering::Relaxed);
                    match sample_sender.try_send(mono_samples) {
                        Ok(()) => {}
                        Err(std::sync::mpsc::TrySendError::Full(_)) => {
                            queued_frames.fetch_sub(frame_count, Ordering::Relaxed);
                            dropped_frames.fetch_add(frame_count, Ordering::Relaxed);
                        }
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                            queued_frames.fetch_sub(frame_count, Ordering::Relaxed);
                            if let Ok(mut error) = data_error.lock() {
                                *error = Some("Microphone writer stopped unexpectedly".to_string());
                            }
                        }
                    }
                }
                if should_auto_stop {
                    auto_stopped.store(true, Ordering::Release);
                    stop.store(true, Ordering::Release);
                }
            },
            move |failure| {
                if let Ok(mut error) = stream_error.lock() {
                    *error = Some(format!("Audio input stopped: {failure}"));
                }
            },
            None,
        )
        .map_err(|error| format!("Could not open the audio input: {error}"))
}

fn capture_audio_thread(
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
    output_path: PathBuf,
    frames: Arc<AtomicU64>,
    peak_bits: Arc<AtomicU32>,
    queued_frames: Arc<AtomicU64>,
    dropped_frames: Arc<AtomicU64>,
    speech_active: Arc<AtomicBool>,
    speech_detected: Arc<AtomicBool>,
    speech_frames: Arc<AtomicU64>,
    silence_frames: Arc<AtomicU64>,
    noise_floor_bits: Arc<AtomicU32>,
    auto_stopped: Arc<AtomicBool>,
    vad_enabled: bool,
    auto_stop: bool,
    silence_ms: u64,
    input_gain: f32,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let specification = hound::WavSpec {
        channels: 1,
        sample_rate: config.sample_rate().0,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = match hound::WavWriter::create(&output_path, specification) {
        Ok(writer) => writer,
        Err(error) => {
            let message = format!("Could not create the microphone recording: {error}");
            let _ = ready.send(Err(message.clone()));
            return Err(message);
        }
    };
    let (sample_sender, sample_receiver) = std::sync::mpsc::sync_channel::<Vec<f32>>(50);
    let callback_error = Arc::new(Mutex::new(None));
    let stream = match config.sample_format() {
        SampleFormat::F32 => build_capture_stream::<f32>(
            &device,
            &stream_config,
            sample_sender,
            Arc::clone(&peak_bits),
            Arc::clone(&queued_frames),
            Arc::clone(&dropped_frames),
            Arc::clone(&speech_active),
            Arc::clone(&speech_detected),
            Arc::clone(&speech_frames),
            Arc::clone(&silence_frames),
            Arc::clone(&noise_floor_bits),
            Arc::clone(&auto_stopped),
            Arc::clone(&stop),
            vad_enabled,
            auto_stop,
            silence_ms,
            input_gain,
            Arc::clone(&callback_error),
        ),
        SampleFormat::I16 => build_capture_stream::<i16>(
            &device,
            &stream_config,
            sample_sender,
            Arc::clone(&peak_bits),
            Arc::clone(&queued_frames),
            Arc::clone(&dropped_frames),
            Arc::clone(&speech_active),
            Arc::clone(&speech_detected),
            Arc::clone(&speech_frames),
            Arc::clone(&silence_frames),
            Arc::clone(&noise_floor_bits),
            Arc::clone(&auto_stopped),
            Arc::clone(&stop),
            vad_enabled,
            auto_stop,
            silence_ms,
            input_gain,
            Arc::clone(&callback_error),
        ),
        SampleFormat::U16 => build_capture_stream::<u16>(
            &device,
            &stream_config,
            sample_sender,
            Arc::clone(&peak_bits),
            Arc::clone(&queued_frames),
            Arc::clone(&dropped_frames),
            Arc::clone(&speech_active),
            Arc::clone(&speech_detected),
            Arc::clone(&speech_frames),
            Arc::clone(&silence_frames),
            Arc::clone(&noise_floor_bits),
            Arc::clone(&auto_stopped),
            Arc::clone(&stop),
            vad_enabled,
            auto_stop,
            silence_ms,
            input_gain,
            Arc::clone(&callback_error),
        ),
        format => Err(format!("Audio input format {format} is not supported")),
    };
    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    if let Err(error) = stream.play() {
        let message = format!("Could not start microphone capture: {error}");
        let _ = ready.send(Err(message.clone()));
        return Err(message);
    }
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Acquire) || queued_frames.load(Ordering::Relaxed) > 0 {
        if let Ok(samples) = sample_receiver.recv_timeout(Duration::from_millis(25)) {
            queued_frames.fetch_sub(samples.len() as u64, Ordering::Relaxed);
            for sample in samples {
                writer
                    .write_sample(sample)
                    .map_err(|error| format!("Could not write microphone samples: {error}"))?;
                frames.fetch_add(1, Ordering::Relaxed);
            }
        }
        if callback_error
            .lock()
            .ok()
            .and_then(|value| value.clone())
            .is_some()
        {
            break;
        }
    }
    drop(stream);
    writer
        .finalize()
        .map_err(|error| format!("Could not finalize the microphone WAV: {error}"))?;
    if let Some(error) = callback_error.lock().ok().and_then(|value| value.clone()) {
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
fn list_audio_input_devices() -> Result<Vec<Value>, String> {
    audio_input_devices()
}

#[tauri::command]
fn list_audio_output_devices() -> Result<Vec<Value>, String> {
    audio_output_devices()
}

fn audio_playback_value(
    playback: &ActivePlayback,
    playing: bool,
    playback_error: Option<&str>,
) -> Value {
    let elapsed = playback.started_at.elapsed().as_secs_f64();
    let played_seconds = (playback.played_frames.load(Ordering::Relaxed) as f64
        / f64::from(playback.output_sample_rate.max(1)))
    .min(playback.duration_seconds);
    json!({
        "playing": playing,
        "device_name": playback.device_name,
        "audio_path": playback.audio_path,
        "duration_seconds": playback.duration_seconds,
        "played_seconds": played_seconds,
        "progress": if playback.duration_seconds > 0.0 { played_seconds / playback.duration_seconds } else { 0.0 },
        "output_sample_rate": playback.output_sample_rate,
        "startup_seconds": playback.startup_seconds,
        "elapsed_seconds": elapsed,
        "underrun_frames": playback.underrun_frames.load(Ordering::Relaxed),
        "playback_error": playback_error,
    })
}

#[tauri::command]
fn start_audio_playback(
    state: tauri::State<'_, RuntimeState>,
    audio_path: String,
    device_id: Option<String>,
) -> Result<Value, String> {
    state.stop_active_playback()?;
    let path = state.store.transcription_audio_path(&audio_path)?;
    let (samples, source_rate) = read_wav_mono(&path)?;
    if samples.is_empty() {
        return Err("Routed WAV audio contains no samples".to_string());
    }
    let host = cpal::default_host();
    let device = if let Some(requested) = device_id.as_deref().filter(|value| !value.is_empty()) {
        host.output_devices()
            .map_err(|error| format!("Could not enumerate audio outputs: {error}"))?
            .find(|device| device.name().ok().as_deref() == Some(requested))
            .ok_or("The selected audio output is no longer available")?
    } else {
        host.default_output_device()
            .ok_or("No default audio output is available")?
    };
    let device_name = device.name().unwrap_or_else(|_| "Audio output".to_string());
    let config = device
        .default_output_config()
        .map_err(|error| format!("Could not read the audio output format: {error}"))?;
    let output_sample_rate = config.sample_rate().0;
    let duration_seconds = samples.len() as f64 / f64::from(source_rate);
    let samples = resample_mono(&samples, source_rate, output_sample_rate);
    let played_frames = Arc::new(AtomicU64::new(0));
    let underrun_frames = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let error = Arc::new(Mutex::new(None));
    let started_at = Instant::now();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let thread = {
        let thread_played = Arc::clone(&played_frames);
        let thread_underruns = Arc::clone(&underrun_frames);
        let thread_completed = Arc::clone(&completed);
        let thread_error = Arc::clone(&error);
        let thread_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            playback_audio_thread(
                device,
                config,
                samples,
                thread_stop,
                thread_played,
                thread_underruns,
                thread_completed,
                thread_error,
                sender,
            )
        })
    };
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "Audio output did not start within 5 seconds".to_string())??;
    let playback = ActivePlayback {
        device_name,
        audio_path: path,
        duration_seconds,
        output_sample_rate,
        played_frames,
        underrun_frames,
        completed,
        error,
        stop,
        thread: Some(thread),
        started_at,
        startup_seconds: started_at.elapsed().as_secs_f64(),
    };
    let value = audio_playback_value(&playback, true, None);
    *state
        .active_playback
        .lock()
        .map_err(|_| "Playback lock failed")? = Some(playback);
    Ok(value)
}

#[tauri::command]
fn audio_playback_status(state: tauri::State<'_, RuntimeState>) -> Result<Value, String> {
    let finished = {
        let active = state
            .active_playback
            .lock()
            .map_err(|_| "Playback lock failed")?;
        active.as_ref().is_some_and(|playback| {
            playback.completed.load(Ordering::Acquire)
                || playback
                    .error
                    .lock()
                    .ok()
                    .is_some_and(|value| value.is_some())
                || playback
                    .thread
                    .as_ref()
                    .is_some_and(JoinHandle::is_finished)
        })
    };
    if finished {
        let Some((playback, result)) = state.stop_active_playback()? else {
            return Ok(json!({ "playing": false }));
        };
        return Ok(audio_playback_value(
            &playback,
            false,
            result.err().as_deref(),
        ));
    }
    let active = state
        .active_playback
        .lock()
        .map_err(|_| "Playback lock failed")?;
    Ok(active
        .as_ref()
        .map(|playback| audio_playback_value(playback, true, None))
        .unwrap_or_else(|| json!({ "playing": false })))
}

#[tauri::command]
fn stop_audio_playback(state: tauri::State<'_, RuntimeState>) -> Result<Value, String> {
    let Some((playback, result)) = state.stop_active_playback()? else {
        return Ok(json!({ "playing": false }));
    };
    Ok(audio_playback_value(
        &playback,
        false,
        result.err().as_deref(),
    ))
}

#[tauri::command]
fn start_audio_recording(
    state: tauri::State<'_, RuntimeState>,
    device_id: Option<String>,
    vad_enabled: Option<bool>,
    auto_stop: Option<bool>,
    silence_ms: Option<u64>,
    input_gain: Option<f32>,
) -> Result<Value, String> {
    let mut active = state
        .active_recording
        .lock()
        .map_err(|_| "Recording lock failed")?;
    if active.is_some() {
        return Err("A microphone recording is already active".to_string());
    }
    let host = cpal::default_host();
    let device = if let Some(requested) = device_id.as_deref().filter(|value| !value.is_empty()) {
        host.input_devices()
            .map_err(|error| format!("Could not enumerate audio inputs: {error}"))?
            .find(|device| device.name().ok().as_deref() == Some(requested))
            .ok_or("The selected audio input is no longer available")?
    } else {
        host.default_input_device()
            .ok_or("No default audio input is available")?
    };
    let device_name = device.name().unwrap_or_else(|_| "Audio input".to_string());
    let config = device
        .default_input_config()
        .map_err(|error| format!("Could not read the audio input format: {error}"))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let vad_enabled = vad_enabled.unwrap_or(true);
    let auto_stop = auto_stop.unwrap_or(false) && vad_enabled;
    let silence_ms = silence_ms.unwrap_or(1_200).clamp(500, 5_000);
    let input_gain = input_gain.unwrap_or(1.0);
    if !input_gain.is_finite() || !(0.25..=4.0).contains(&input_gain) {
        return Err("Input gain must be between 0.25x and 4.00x".to_string());
    }
    let output_path = state.store.capture_path()?;
    let frames = Arc::new(AtomicU64::new(0));
    let peak_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let queued_frames = Arc::new(AtomicU64::new(0));
    let dropped_frames = Arc::new(AtomicU64::new(0));
    let speech_active = Arc::new(AtomicBool::new(false));
    let speech_detected = Arc::new(AtomicBool::new(false));
    let speech_frames = Arc::new(AtomicU64::new(0));
    let silence_frames = Arc::new(AtomicU64::new(0));
    let noise_floor_bits = Arc::new(AtomicU32::new(0.003_f32.to_bits()));
    let auto_stopped = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let thread = {
        let path = output_path.clone();
        let thread_frames = Arc::clone(&frames);
        let thread_peak = Arc::clone(&peak_bits);
        let thread_queued = Arc::clone(&queued_frames);
        let thread_dropped = Arc::clone(&dropped_frames);
        let thread_speech_active = Arc::clone(&speech_active);
        let thread_speech_detected = Arc::clone(&speech_detected);
        let thread_speech_frames = Arc::clone(&speech_frames);
        let thread_silence_frames = Arc::clone(&silence_frames);
        let thread_noise_floor = Arc::clone(&noise_floor_bits);
        let thread_auto_stopped = Arc::clone(&auto_stopped);
        let thread_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            capture_audio_thread(
                device,
                config,
                path,
                thread_frames,
                thread_peak,
                thread_queued,
                thread_dropped,
                thread_speech_active,
                thread_speech_detected,
                thread_speech_frames,
                thread_silence_frames,
                thread_noise_floor,
                thread_auto_stopped,
                vad_enabled,
                auto_stop,
                silence_ms,
                input_gain,
                thread_stop,
                sender,
            )
        })
    };
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "Audio input did not start within 5 seconds".to_string())??;
    *active = Some(ActiveRecording {
        device_name: device_name.clone(),
        output_path: output_path.clone(),
        sample_rate,
        channels,
        frames,
        peak_bits,
        queued_frames,
        dropped_frames,
        speech_active,
        speech_detected,
        speech_frames,
        silence_frames,
        noise_floor_bits,
        auto_stopped,
        vad_enabled,
        auto_stop,
        silence_ms,
        input_gain,
        stop,
        thread: Some(thread),
    });
    Ok(
        json!({ "recording": true, "device_name": device_name, "audio_path": output_path, "sample_rate": sample_rate, "channels": channels, "duration_seconds": 0.0, "peak": 0.0, "speech_active": false, "speech_detected": false, "speech_seconds": 0.0, "silence_seconds": 0.0, "noise_floor": 0.003, "dropped_frames": 0, "buffered_frames": 0, "vad_enabled": vad_enabled, "auto_stop": auto_stop, "silence_ms": silence_ms, "input_gain": input_gain, "stop_reason": null }),
    )
}

fn audio_recording_value(
    recording: &ActiveRecording,
    active: bool,
    capture_error: Option<&str>,
) -> Value {
    let sample_rate = recording.sample_rate.max(1);
    json!({
        "recording": active,
        "device_name": recording.device_name,
        "audio_path": recording.output_path,
        "sample_rate": recording.sample_rate,
        "channels": if active { recording.channels } else { 1 },
        "duration_seconds": recording.frames.load(Ordering::Relaxed) as f64 / sample_rate as f64,
        "peak": f32::from_bits(recording.peak_bits.load(Ordering::Relaxed)),
        "speech_active": recording.speech_active.load(Ordering::Relaxed),
        "speech_detected": recording.speech_detected.load(Ordering::Relaxed),
        "speech_seconds": recording.speech_frames.load(Ordering::Relaxed) as f64 / sample_rate as f64,
        "silence_seconds": recording.silence_frames.load(Ordering::Relaxed) as f64 / sample_rate as f64,
        "noise_floor": f32::from_bits(recording.noise_floor_bits.load(Ordering::Relaxed)),
        "dropped_frames": recording.dropped_frames.load(Ordering::Relaxed),
        "buffered_frames": recording.queued_frames.load(Ordering::Relaxed),
        "vad_enabled": recording.vad_enabled,
        "auto_stop": recording.auto_stop,
        "silence_ms": recording.silence_ms,
        "input_gain": recording.input_gain,
        "stop_reason": if recording.auto_stopped.load(Ordering::Acquire) { Some("silence") } else if capture_error.is_some() { Some("device") } else if active { None } else { Some("user") },
        "capture_error": capture_error,
    })
}

#[tauri::command]
fn audio_recording_status(state: tauri::State<'_, RuntimeState>) -> Result<Value, String> {
    let finished = {
        let active = state
            .active_recording
            .lock()
            .map_err(|_| "Recording lock failed")?;
        active.as_ref().is_some_and(|recording| {
            recording.auto_stopped.load(Ordering::Acquire)
                || recording
                    .thread
                    .as_ref()
                    .is_some_and(JoinHandle::is_finished)
        })
    };
    if finished {
        return finish_audio_recording(&state);
    }
    let active = state
        .active_recording
        .lock()
        .map_err(|_| "Recording lock failed")?;
    let Some(recording) = active.as_ref() else {
        return Ok(json!({ "recording": false }));
    };
    Ok(audio_recording_value(recording, true, None))
}

fn finish_audio_recording(state: &RuntimeState) -> Result<Value, String> {
    let Some((recording, result)) = state.stop_active_recording()? else {
        return Err("No microphone recording is active".to_string());
    };
    let frames = recording.frames.load(Ordering::Relaxed);
    if frames < u64::from(recording.sample_rate) / 5 {
        fs::remove_file(&recording.output_path).ok();
        return Err(result.err().unwrap_or_else(|| {
            "Recording is too short; capture at least 0.2 seconds".to_string()
        }));
    }
    Ok(audio_recording_value(
        &recording,
        false,
        result.err().as_deref(),
    ))
}

#[tauri::command]
fn stop_audio_recording(state: tauri::State<'_, RuntimeState>) -> Result<Value, String> {
    finish_audio_recording(&state)
}

#[tauri::command]
fn delete_project(state: tauri::State<'_, RuntimeState>, id: String) -> Result<bool, String> {
    state.store.delete_project(&id)
}

#[tauri::command]
async fn create_voice(
    state: tauri::State<'_, RuntimeState>,
    request: Value,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let voice = runtime.store.create_voice(&request)?;
        let id = voice
            .get("id")
            .and_then(Value::as_str)
            .ok_or("Voice profile has no identifier")?;
        let path = voice
            .get("local_path")
            .and_then(Value::as_str)
            .ok_or("Voice profile has no managed reference")?;
        let reference_id = voice
            .get("active_reference_id")
            .and_then(Value::as_str)
            .ok_or("Voice profile has no reference identifier")?;
        let original = PathBuf::from(path);
        let output_path = original
            .parent()
            .ok_or("Voice reference has no managed directory")?
            .join(format!("processed-{reference_id}.wav"));
        let prepared = runtime.request(json!({
            "operation": "prepare_voice_reference",
            "audio_path": path,
            "output_path": output_path
        }));
        match prepared {
            Ok(prepared) => runtime
                .store
                .finalize_voice_reference(id, reference_id, &prepared),
            Err(error) => runtime
                .store
                .mark_voice_processing_error(id, reference_id, &error),
        }
    })
    .await
    .map_err(|error| format!("Voice import worker failed: {error}"))?
}

#[tauri::command]
async fn add_voice_reference(
    state: tauri::State<'_, RuntimeState>,
    voice_id: String,
    source_path: String,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let reference = runtime.store.add_voice_reference(&voice_id, &source_path)?;
        let reference_id = reference
            .get("id")
            .and_then(Value::as_str)
            .ok_or("Voice reference has no identifier")?;
        let original_path = reference
            .get("original_path")
            .and_then(Value::as_str)
            .ok_or("Voice reference has no managed original")?;
        let output_path = PathBuf::from(original_path)
            .parent()
            .ok_or("Voice reference has no managed directory")?
            .join(format!("processed-{reference_id}.wav"));
        let prepared = runtime.request(json!({
            "operation": "prepare_voice_reference",
            "audio_path": original_path,
            "output_path": output_path,
        }));
        match prepared {
            Ok(prepared) => {
                runtime
                    .store
                    .finalize_voice_reference(&voice_id, reference_id, &prepared)
            }
            Err(error) => {
                runtime
                    .store
                    .mark_voice_processing_error(&voice_id, reference_id, &error)
            }
        }
    })
    .await
    .map_err(|error| format!("Voice reference worker failed: {error}"))?
}

#[tauri::command]
async fn process_voice_reference(
    state: tauri::State<'_, RuntimeState>,
    voice_id: String,
    reference_id: String,
    edits: Value,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let reference = runtime.store.voice_reference_for_processing(&voice_id, &reference_id)?;
        let original_path = reference.get("original_path").and_then(Value::as_str).ok_or("Voice reference has no managed original")?;
        let output_path = PathBuf::from(original_path)
            .parent()
            .ok_or("Voice reference has no managed directory")?
            .join(format!("processed-{reference_id}-{}.wav", Uuid::new_v4().simple()));
        let prepared = runtime.request(json!({
            "operation": "prepare_voice_reference",
            "audio_path": original_path,
            "output_path": output_path,
            "trim_start_seconds": edits.get("trim_start_seconds").cloned().unwrap_or_else(|| json!(0)),
            "trim_end_seconds": edits.get("trim_end_seconds").cloned().unwrap_or_else(|| reference.get("analysis").and_then(|value| value.get("duration_seconds")).cloned().unwrap_or_else(|| json!(0))),
            "remove_silence": edits.get("remove_silence").cloned().unwrap_or_else(|| json!(true)),
            "normalize": edits.get("normalize").cloned().unwrap_or_else(|| json!(true)),
            "peak_target_dbfs": edits.get("peak_target_dbfs").cloned().unwrap_or_else(|| json!(-1.0)),
        }));
        match prepared {
            Ok(prepared) => runtime.store.finalize_voice_reference(&voice_id, &reference_id, &prepared),
            Err(error) => Err(error),
        }
    }).await.map_err(|error| format!("Voice edit worker failed: {error}"))?
}

#[tauri::command]
fn update_voice_reference_transcript(
    state: tauri::State<'_, RuntimeState>,
    voice_id: String,
    reference_id: String,
    transcript: String,
    source: String,
) -> Result<Value, String> {
    state
        .store
        .update_voice_reference_transcript(&voice_id, &reference_id, &transcript, &source)
}

#[tauri::command]
fn save_voice_evaluation(
    state: tauri::State<'_, RuntimeState>,
    evaluation: Value,
) -> Result<Value, String> {
    state.store.save_voice_evaluation(&evaluation)
}

#[tauri::command]
async fn measure_voice_similarity(
    state: tauri::State<'_, RuntimeState>,
    evaluation_id: String,
    model_id: String,
) -> Result<Value, String> {
    validate_model_argument(&model_id)?;
    if evaluation_id.is_empty()
        || evaluation_id.len() > 80
        || !evaluation_id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
    {
        return Err("Voice evaluation identifier is invalid".to_string());
    }
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let evidence = runtime.store.voice_similarity_request(&evaluation_id)?;
        let request = json!({
            "operation": "compare_speakers",
            "model_id": model_id,
            "reference_audio_path": evidence["reference_audio_path"],
            "candidate_audio_path": evidence["candidate_audio_path"],
            "reference_sha256": evidence["reference_sha256"],
            "candidate_sha256": evidence["candidate_sha256"],
            "priority": "high",
        });
        let job_id = runtime.store.create_job("speaker-similarity", &request)?;
        runtime.store.update_job(&job_id, "preparing", 0.05)?;
        match runtime.request_for_job(request, &job_id) {
            Ok(result) => {
                if result.get("model_id").and_then(Value::as_str) != Some(model_id.as_str()) {
                    let error =
                        "Speaker verifier returned evidence for a different model".to_string();
                    runtime.store.fail_job(&job_id, &error)?;
                    return Err(error);
                }
                runtime
                    .store
                    .complete_voice_similarity(&job_id, &evaluation_id, &evidence, &result)
            }
            Err(error) => {
                if runtime.store.job_status(&job_id)?.as_deref() != Some("cancelled") {
                    runtime.store.fail_job(&job_id, &error)?;
                }
                Err(error)
            }
        }
    })
    .await
    .map_err(|error| format!("Speaker-similarity worker failed: {error}"))?
}

#[tauri::command]
async fn delete_voice(state: tauri::State<'_, RuntimeState>, id: String) -> Result<bool, String> {
    let store = Arc::clone(&state.store);
    tauri::async_runtime::spawn_blocking(move || store.delete_voice(&id))
        .await
        .map_err(|error| format!("Voice deletion worker failed: {error}"))?
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

#[tauri::command]
async fn setup_engine_runtime(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    engine: String,
) -> Result<(), String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.setup_engine(&app, &engine))
        .await
        .map_err(|error| format!("Engine runtime setup worker failed: {error}"))?
}

#[tauri::command]
async fn engine_health(
    state: tauri::State<'_, RuntimeState>,
    engine: String,
) -> Result<Value, String> {
    validate_engine_argument(&engine)?;
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.check_engine_health(&engine))
        .await
        .map_err(|error| format!("Engine health worker failed: {error}"))?
}

#[tauri::command]
fn queue_model_runtime_load(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
) -> Result<Value, String> {
    validate_model_argument(&model_id)?;
    let request = json!({
        "operation": "load",
        "model_id": model_id,
        "title": format!("Load {}", model_id.rsplit('/').next().unwrap_or(&model_id)),
        "priority": "urgent",
    });
    state.request_engine(&request)?;
    let job_id = state.store.create_job("model-load", &request)?;
    state.store.update_job(&job_id, "preparing", 0.05)?;
    state.start_background_model_load(job_id.clone(), model_id)?;
    state
        .store
        .get_job(&job_id)?
        .ok_or_else(|| "The queued model load was not found".to_string())
}

#[tauri::command]
async fn unload_model_runtime(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.unload_model_runtime(&model_id))
        .await
        .map_err(|error| format!("Model unload worker failed: {error}"))?
}

#[tauri::command]
fn developer_api_status(state: tauri::State<'_, RuntimeState>) -> Result<Value, String> {
    state.api_server_status()
}

#[tauri::command]
fn start_developer_api(
    state: tauri::State<'_, RuntimeState>,
    port: Option<u16>,
) -> Result<Value, String> {
    state.start_api_server(port)
}

#[tauri::command]
fn stop_developer_api(state: tauri::State<'_, RuntimeState>) -> Result<bool, String> {
    state.stop_api_server()
}

#[tauri::command]
async fn model_install_plan(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
    revision: Option<String>,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        runtime.model_command_with_revision("plan", &model_id, revision.as_deref())
    })
    .await
    .map_err(|error| format!("Model planning worker failed: {error}"))?
    .and_then(|response| {
        response
            .get("plan")
            .cloned()
            .ok_or_else(|| "Model planning returned no install plan.".to_string())
    })
}

#[tauri::command]
async fn verify_model(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.model_command("verify", &model_id))
        .await
        .map_err(|error| format!("Model verification worker failed: {error}"))?
        .and_then(|response| {
            response
                .get("integrity")
                .cloned()
                .ok_or_else(|| "Model verification returned no integrity report.".to_string())
        })
}

#[tauri::command]
async fn install_model(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
    revision: String,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.install_model(&app, &model_id, &revision))
        .await
        .map_err(|error| format!("Model installation worker failed: {error}"))?
}

#[tauri::command]
fn cancel_model_install(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
) -> Result<bool, String> {
    state.cancel_model_install(&model_id)
}

#[tauri::command]
async fn remove_model(
    state: tauri::State<'_, RuntimeState>,
    model_id: String,
) -> Result<bool, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.remove_model(&model_id))
        .await
        .map_err(|error| format!("Model removal worker failed: {error}"))?
}

#[tauri::command]
fn codex_agent_status(state: tauri::State<'_, RuntimeState>) -> Value {
    state.codex_agent.status()
}

#[tauri::command]
fn codex_agent_connect(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
) -> Result<Value, String> {
    state.codex_agent.connect(app, state.inner().clone())
}

#[tauri::command]
fn codex_agent_disconnect(state: tauri::State<'_, RuntimeState>) -> Result<bool, String> {
    state.codex_agent.disconnect()
}

#[tauri::command]
fn assistant_video_thread_link(
    state: tauri::State<'_, RuntimeState>,
    thread_id: String,
) -> Result<Option<Value>, String> {
    if !state.codex_agent.has_registered_thread(&thread_id)? {
        return Err(
            "soundar.thread_not_registered: Resume the exact assistant task before restoring its Video Studio result"
                .into(),
        );
    }
    state.store.latest_assistant_video_artifact(&thread_id)
}

#[tauri::command]
fn codex_agent_request(
    state: tauri::State<'_, RuntimeState>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let mut params = params;
    let access = codex_agent::AgentAccess::from_value(
        params
            .get("soundarAccess")
            .or_else(|| params.get("sandbox")),
    );
    let requested_thread_id = params
        .get("threadId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if let Some(object) = params.as_object_mut() {
        object.remove("soundarAccess");
    }
    if method == "thread/start" {
        if let Some(object) = params.as_object_mut() {
            let studio_root = home_dir().join(".soundAr");
            fs::create_dir_all(&studio_root).map_err(|error| {
                format!("Could not prepare the soundAr assistant workspace: {error}")
            })?;
            object.insert("cwd".into(), json!(studio_root));
            object.insert(
                "runtimeWorkspaceRoots".into(),
                json!([home_dir().join(".soundAr")]),
            );
            object.insert("dynamicTools".into(), codex_agent::dynamic_tools());
            object.entry("developerInstructions").or_insert_with(|| json!(
                "You are soundAr's creative producer, not merely a command parser. Help the user turn an incomplete goal into an executable brief; research outside the studio with Codex capabilities when the selected access permits it; draft the required scripts, lyrics, directions, and project structure; and use the provided soundAr tools to create speech, music, batches, projects, videos, and revisions end to end. Inspect studio state before choosing model or voice identifiers. State a concise plan, execute it, monitor durable jobs, and report registered playable output artifacts. For a multi-part audio project, save the project with every completed chapter history_id, then call export_project_master and report completion only after that tool returns a registered playable master. For Video Studio work, use the shared Video Studio tools rather than shell media commands. Call preview_link before import_link; never assert media rights or set any rights or approval confirmation unless the user explicitly authorized the exact canonical URL or action; keep link intake to one source by default. When a locally generated illustration or image is part of the plan, call register_generated_visual with its authenticated image-generation item id, then pass the returned receipt to add_visual_asset with the project-clock range; never supply or infer a local source path or provenance. Preview the assembled speech, music, captions, and visual layers together. Present progress at Source, Analyze, Plan & revise, Preview, and Export level without flooding the conversation, poll durable jobs through their project/job tools, and after export call get_video_project so the registered playable final master is the prominent final result rather than a scene artifact or raw path. For a recurring series, save_show_format stores the decisions that do not change between episodes and create_episode starts the next one from a brief; instantiation copies, so editing a format never changes an episode that already exists. Use edit_video_timeline's set_lexicon_entry to fix an invented name's pronunciation once instead of respelling it throughout the script; changing a rule re-reads only the lines that rule governs. For a multi-character story, podcast, or audio drama, call write_video_script to declare the cast and the speaker-attributed script before generating any narration: each character is bound to one voice, and each `NAME: line` becomes one durable turn. Re-applying a revised script keeps every turn whose words are unchanged, so narrate only the turns the tool reports as new and never re-read a line that already has a valid take. Treat follow-up video feedback as revise_video, write_video_script, or edit_video_timeline against the exact base version and rerender only invalidated stages. Never assemble project media with shell commands, expose an unregistered raw path, install Codex, or modify soundAr application code. Ask only for genuinely blocking choices and always ask before destructive actions."
            ));
        }
    } else if method == "thread/resume" {
        if let Some(object) = params.as_object_mut() {
            object.entry("developerInstructions").or_insert_with(|| json!(
                "Continue as soundAr's creative producer. Research and plan when needed, use the soundAr tools already attached to this thread for every supported speech, music, batch, project, and revision workflow, and preserve prior creative intent. Complete multi-part audio projects with export_project_master so the final audio is registered and playable in soundAr; never substitute a raw filesystem path. If this pre-upgrade thread does not already have the shared Video Studio tools, explain that Video Studio requires a fresh assistant task rather than attempting a shell-media fallback. When the tools are present, call preview_link before import_link, never assert exact-URL media rights or confirmation flags for the user, use one source by default, register locally generated images through register_generated_visual and then pass its receipt to add_visual_asset, present Source through Export phases, poll durable jobs, revise or edit the exact base version, and finish with get_video_project so the registered playable master is prominent. Never supply a generated-image path or provenance, install Codex, or modify soundAr application code. Ask before destructive actions."
            ));
        }
    }
    let response = state.codex_agent.request(&method, params)?;
    let thread_id = if method == "thread/start" {
        response
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    } else if matches!(
        method.as_str(),
        "thread/resume" | "turn/start" | "turn/steer"
    ) {
        requested_thread_id
    } else {
        None
    };
    if let Some(thread_id) = thread_id {
        state.codex_agent.set_thread_access(&thread_id, access)?;
    }
    Ok(response)
}

#[tauri::command]
fn codex_agent_respond(
    state: tauri::State<'_, RuntimeState>,
    id: u64,
    result: Value,
) -> Result<(), String> {
    state.codex_agent.respond(id, result)
}

/// Loopback origin that serves rendered media to the webview.
///
/// WebKitGTK cannot decode media delivered over a custom URI scheme, so `asset:` URLs fail every
/// `<video>` element on Linux. Exported files are streamed from a real HTTP origin instead.
struct MediaOrigin(video::LocalMediaServer);

/// Guard against a navigation replacing the single-page app.
///
/// A cross-origin `<a download>` is not honoured by the webview: it navigates instead, and with
/// `decorations: false` there is no chrome to navigate back with, so the window is left showing a
/// blank media document. Downloads go through `save_media_artifact`; nothing else may navigate.
fn navigation_is_allowed(url: &tauri::Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" | "https" => matches!(url.host_str(), Some("tauri.localhost")) || cfg!(dev),
        _ => false,
    }
}

/// Report the media origin so the webview can recover if the injected global is missing.
#[tauri::command]
fn video_media_endpoint(app: tauri::AppHandle) -> Result<Value, String> {
    let origin = app
        .try_state::<MediaOrigin>()
        .ok_or("media.origin_unavailable: The local media origin is not running")?;
    Ok(json!({ "origin": origin.0.origin(), "token": origin.0.token() }))
}

/// Resolve a save request to a real export, refusing anything outside the export root.
///
/// The path arrives from the webview, so it is canonicalised before the prefix check: a symlink or
/// `..` inside the export tree must not be able to reach the rest of the filesystem.
fn resolve_saveable_export(source_path: &str, exports_root: &Path) -> Result<PathBuf, String> {
    let source = fs::canonicalize(source_path)
        .map_err(|_| "media.source_missing: That export is no longer on disk".to_string())?;
    if !source.starts_with(exports_root) || !source.is_file() {
        return Err("media.source_forbidden: Only soundAr exports can be saved".into());
    }
    Ok(source)
}

/// Pick the name to pre-fill in the save dialog, ignoring any suggestion carrying a path separator.
fn saved_file_name(suggested: Option<&str>, source: &Path) -> String {
    suggested
        .map(str::trim)
        .filter(|name| {
            !name.is_empty() && !name.contains('/') && !name.contains('\\') && *name != ".."
        })
        .map(str::to_string)
        .or_else(|| {
            source
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "soundar-export".to_string())
}

/// Copy an exported artifact to a location the user picks.
///
/// The webview cannot do this itself: an `<a download>` pointing at another origin is not honoured,
/// so the click navigates the window to the file instead of saving it. Saving happens here, where
/// the source path is checked against the export root before any bytes are copied.
#[tauri::command]
async fn save_media_artifact(
    app: tauri::AppHandle,
    source_path: String,
    suggested_name: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let exports_root = fs::canonicalize(home_dir().join(".soundAr/exports"))
        .map_err(|error| format!("media.exports_unavailable: {error}"))?;
    let source = resolve_saveable_export(&source_path, &exports_root)?;
    let default_name = saved_file_name(suggested_name.as_deref(), &source);
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string();

    tauri::async_runtime::spawn_blocking(move || {
        let mut builder = app
            .dialog()
            .file()
            .set_title("Save export")
            .set_file_name(&default_name);
        if !extension.is_empty() {
            builder = builder.add_filter(extension.to_uppercase(), &[extension.as_str()]);
        }
        let Some(destination) = builder.blocking_save_file() else {
            return Ok(None);
        };
        let destination = destination
            .into_path()
            .map_err(|error| format!("media.destination_invalid: {error}"))?;
        fs::copy(&source, &destination).map_err(|error| {
            format!("media.save_failed: The export could not be saved: {error}")
        })?;
        Ok(Some(destination.to_string_lossy().to_string()))
    })
    .await
    .map_err(|error| format!("media.save_worker_failed: {error}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The store creates this root, but only later in `setup`. Create it here so a first run still
    // gets a media origin instead of silently losing every `<video>` until the next launch.
    let exports_root = home_dir().join(".soundAr/exports");
    let media_origin = store::ensure_private_directory(&exports_root, "artifact")
        .map_err(|error| format!("the export directory is unavailable: {error}"))
        .and_then(|()| {
            video::LocalMediaServer::start(vec![exports_root])
                .map_err(|error| format!("the local media origin could not start: {error}"))
        })
        .map_err(|error| {
            eprintln!("soundAr: {error} Video and audio playback will be unavailable.");
            error
        })
        .ok();
    let media_init_script = media_origin.as_ref().map_or_else(
        || "window.__SOUNDAR_MEDIA__ = null;".to_string(),
        |origin| {
            format!(
                "window.__SOUNDAR_MEDIA__ = {{ origin: {}, token: {} }};",
                serde_json::to_string(origin.origin()).unwrap_or_else(|_| "null".into()),
                serde_json::to_string(origin.token()).unwrap_or_else(|_| "null".into()),
            )
        },
    );
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry, ()>::new("soundar-shell")
                .js_init_script(media_init_script)
                .on_navigation(|_webview, url| navigation_is_allowed(url))
                .build(),
        )
        .setup(move |app| {
            let root = runtime_root(app.handle());
            app.manage(RuntimeState::new(root, python_path())?);
            if let Some(origin) = media_origin {
                app.manage(MediaOrigin(origin));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap_state,
            setup_runtime,
            setup_engine_runtime,
            engine_health,
            queue_model_runtime_load,
            unload_model_runtime,
            developer_api_status,
            start_developer_api,
            stop_developer_api,
            synthesize,
            generate_music,
            queue_synthesis,
            queue_music_generation,
            transcribe_audio,
            update_transcription,
            diarize_transcription,
            align_transcription,
            update_transcription_speaker_labels,
            cancel_active_synthesis,
            cancel_job,
            retry_job,
            clear_finished_jobs,
            save_application_setting,
            read_generated_audio,
            read_job_preview,
            read_transcription_audio,
            read_voice_audio,
            list_audio_input_devices,
            list_audio_output_devices,
            start_audio_recording,
            audio_recording_status,
            stop_audio_recording,
            start_audio_playback,
            audio_playback_status,
            stop_audio_playback,
            list_history,
            duplicate_history,
            export_history,
            delete_history,
            list_jobs,
            scheduler_status,
            create_batch,
            import_batch_file,
            queue_batch,
            execute_batch,
            get_batch,
            cancel_batch,
            pause_batch,
            resume_batch,
            update_batch_item,
            list_batches,
            update_history_metadata,
            history_request,
            save_comparison,
            create_comparison,
            get_comparison,
            update_comparison_review,
            cancel_comparison,
            list_presets,
            save_preset,
            prepare_benchmark_engine,
            release_benchmark_engine,
            save_benchmark,
            list_projects,
            save_project,
            import_project_script,
            export_project_master,
            delete_project,
            create_voice,
            add_voice_reference,
            process_voice_reference,
            update_voice_reference_transcript,
            save_voice_evaluation,
            measure_voice_similarity,
            delete_voice,
            model_install_plan,
            verify_model,
            install_model,
            cancel_model_install,
            remove_model,
            video_media_endpoint,
            save_media_artifact,
            video_commands::video_runtime_status,
            video_commands::video_caption_presets,
            video_commands::preview_video_link,
            video_commands::import_video_link,
            video_commands::import_video_file,
            video_commands::analyze_video,
            video_commands::plan_video,
            video_commands::create_video_project,
            video_commands::list_video_projects,
            video_commands::get_video_project,
            video_commands::revise_video,
            video_commands::edit_video_timeline,
            video_commands::write_video_script,
            video_commands::preview_video_pronunciation,
            video_commands::generate_video_cue_music,
            video_commands::list_show_formats,
            video_commands::save_show_format,
            video_commands::delete_show_format,
            video_commands::create_episode,
            video_commands::choose_video_visual_asset,
            video_commands::add_video_visual_asset,
            video_commands::render_video_preview,
            video_commands::export_video,
            video_commands::export_publish_package,
            video_commands::cancel_video_job,
            video_commands::resume_video_job,
            assistant_video_thread_link,
            codex_agent_status,
            codex_agent_connect,
            codex_agent_disconnect,
            codex_agent_request,
            codex_agent_respond
        ])
        .build(tauri::generate_context!())
        .expect("error while building soundAr");
    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let state = handle.state::<RuntimeState>();
            state.stop_active_recording().ok();
            state.stop_active_playback().ok();
            state.stop_api_server().ok();
            state.stop_active_worker().ok();
            state.codex_agent.disconnect().ok();
        }
    });
}

/// Runs the same Video Studio control plane without constructing a Tauri application or window.
/// `main.rs` dispatches here before GUI bootstrap when the first argument is `agent`.
pub fn run_agent_cli(arguments: Vec<std::ffi::OsString>) -> i32 {
    agent_cli::run(arguments)
}

#[cfg(test)]
mod tests {
    use super::{
        active_video_jobs_support_qualified_whisper_overlap, capture_audio_thread,
        cold_load_needs_idle_reclamation, foundation_runtime_ready_for_install, gpu_status,
        home_dir, managed_cuda_library_path, navigation_is_allowed, new_inference_scheduler,
        parse_project_script, read_batch_import, read_json,
        request_supports_qualified_video_overlap, resample_mono, resolve_saveable_export,
        saved_file_name, scheduler_rank, sha256_path, validate_model_argument, validate_revision,
        video_request_supports_qualified_whisper_overlap, write_json_atomically, ActivePlayback,
        ActiveRecording, GlobalVideoGpuGate, RuntimeState, SchedulerWaiter, VideoGpuReservation,
        VoiceActivityDetector, GPU_COLD_LOAD_HEADROOM_MB, MAX_RPC_REQUEST_BYTES,
    };
    use crate::store::Store;
    use cpal::traits::{DeviceTrait, HostTrait};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    };
    use std::{
        fs,
        io::{Read, Write},
        net::TcpStream,
        process::Command,
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    struct RuntimeCleanup(RuntimeState);

    impl Drop for RuntimeCleanup {
        fn drop(&mut self) {
            self.0.cancel_all_active_syntheses().ok();
            self.0.stop_api_server().ok();
            self.0.stop_active_worker().ok();
        }
    }

    #[test]
    fn packaged_foundation_runtime_reads_manifest_from_runtime_root() {
        let root = std::env::temp_dir().join(format!("soundar-runtime-ready-{}", Uuid::new_v4()));
        let python = root.join(".venv/bin/python");
        fs::create_dir_all(python.parent().expect("python parent"))
            .expect("create runtime fixture");
        fs::write(&python, b"python fixture").expect("write python fixture");
        fs::write(
            root.join("runtime.json"),
            r#"{"schema_version":2,"transformers":"5.5.0"}"#,
        )
        .expect("write runtime manifest");

        assert!(foundation_runtime_ready_for_install(&python, false));
        fs::rename(root.join("runtime.json"), root.join(".venv/runtime.json"))
            .expect("move manifest to the formerly checked location");
        assert!(!foundation_runtime_ready_for_install(&python, false));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn managed_transformer_workers_prepend_private_cuda_wheel_libraries() {
        let root = std::env::temp_dir().join(format!("soundar-cuda-libs-{}", Uuid::new_v4()));
        let site_packages = root.join(".venv/lib/python3.11/site-packages/nvidia");
        let cublas = site_packages.join("cublas/lib");
        let cudnn = site_packages.join("cudnn/lib");
        fs::create_dir_all(&cublas).expect("create cuBLAS fixture");
        fs::create_dir_all(&cudnn).expect("create cuDNN fixture");

        let joined = managed_cuda_library_path(&root)
            .expect("discover managed CUDA libraries")
            .expect("managed CUDA path");
        let paths = std::env::split_paths(&joined).collect::<Vec<_>>();
        assert_eq!(
            paths.first(),
            Some(&cublas.canonicalize().expect("cuBLAS path"))
        );
        assert_eq!(
            paths.get(1),
            Some(&cudnn.canonicalize().expect("cuDNN path"))
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cold_model_load_reclaims_idle_workers_before_the_vram_envelope_is_exhausted() {
        let required = 8_600;
        assert!(cold_load_needs_idle_reclamation(
            required + GPU_COLD_LOAD_HEADROOM_MB - 1,
            required
        ));
        assert!(!cold_load_needs_idle_reclamation(
            required + GPU_COLD_LOAD_HEADROOM_MB,
            required
        ));
        assert!(!cold_load_needs_idle_reclamation(0, 0));
    }

    fn synthesize_and_publish(runtime: &RuntimeState, request: Value) -> Value {
        let job = runtime
            .store
            .create_job("synthesis", &request)
            .expect("create GPU test job");
        runtime
            .store
            .update_job(&job, "running", 0.2)
            .expect("start GPU test job");
        let result = runtime
            .request_for_job(request.clone(), &job)
            .expect("GPU synthesis");
        runtime
            .store
            .complete_synthesis(&job, &request, &result)
            .expect("publish GPU artifact")
    }

    fn synthesize_and_publish_result(
        runtime: &RuntimeState,
        request: &Value,
    ) -> Result<Value, String> {
        let job = runtime.store.create_job("synthesis", request)?;
        runtime.store.update_job(&job, "running", 0.2)?;
        let result = runtime.request_for_job(request.clone(), &job)?;
        runtime.store.complete_synthesis(&job, request, &result)
    }

    fn generate_music_and_publish_result(
        runtime: &RuntimeState,
        request: &Value,
    ) -> Result<Value, String> {
        let job = runtime.store.create_job("music-generation", request)?;
        runtime.store.update_job(&job, "running", 0.2)?;
        let result = runtime.request_for_job(request.clone(), &job)?;
        runtime.store.complete_synthesis(&job, request, &result)
    }

    fn verify_playable_wav(runtime: &RuntimeState, result: &Value) -> Result<Value, String> {
        let path = result
            .get("audio_path")
            .and_then(Value::as_str)
            .ok_or("Synthesis returned no audio path")?;
        let bytes = runtime.store.generated_audio_bytes(path)?;
        if !bytes.starts_with(b"RIFF") {
            return Err("Generated audio has no RIFF header".to_string());
        }
        let reader = hound::WavReader::new(std::io::Cursor::new(&bytes))
            .map_err(|error| format!("Generated audio is not a decodable WAV: {error}"))?;
        if reader.duration() == 0 || reader.spec().channels == 0 || reader.spec().sample_rate == 0 {
            return Err("Generated WAV has an invalid stream description".to_string());
        }
        Ok(json!({
            "path_file_name": Path::new(path)
                .file_name()
                .and_then(|value| value.to_str()),
            "size_bytes": bytes.len(),
            "sample_rate": reader.spec().sample_rate,
            "channels": reader.spec().channels,
            "frames": reader.duration(),
            "sha256": sha256_path(Path::new(path))?,
        }))
    }

    fn require_quiescent_scheduler(runtime: &RuntimeState) -> Result<Value, String> {
        let scheduler = runtime.scheduler_status()?;
        let quiescent = scheduler["active_workers"].as_u64() == Some(0)
            && scheduler["active_gpu_workers"].as_u64() == Some(0)
            && scheduler["active_video_gpu_jobs"].as_u64() == Some(0)
            && scheduler["reserved_vram_mb"].as_u64() == Some(0)
            && scheduler["active_batches"].as_u64() == Some(0)
            && scheduler["waiting_jobs"].as_u64() == Some(0)
            && scheduler["benchmark_reserved"].as_bool() == Some(false);
        if !quiescent {
            return Err(format!(
                "GPU scheduler did not return to a quiescent state: {scheduler}"
            ));
        }
        Ok(scheduler)
    }

    struct GpuPeakSampler {
        stop: Arc<AtomicBool>,
        peak_vram_mb: Arc<AtomicU64>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    struct GeneratedArtifactCleanup(Arc<Mutex<Vec<PathBuf>>>);

    impl Drop for GeneratedArtifactCleanup {
        fn drop(&mut self) {
            if let Ok(paths) = self.0.lock() {
                for path in paths.iter() {
                    fs::remove_file(path).ok();
                    let partial = PathBuf::from(format!("{}.partial", path.to_string_lossy()));
                    fs::remove_file(partial).ok();
                }
            }
        }
    }

    impl GpuPeakSampler {
        fn start(python_path: PathBuf) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let peak_vram_mb = Arc::new(AtomicU64::new(0));
            let thread_stop = Arc::clone(&stop);
            let thread_peak = Arc::clone(&peak_vram_mb);
            let thread = std::thread::spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    let used = gpu_status(&python_path)["vram_used_mb"]
                        .as_u64()
                        .unwrap_or(0);
                    thread_peak.fetch_max(used, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(250));
                }
            });
            Self {
                stop,
                peak_vram_mb,
                thread: Some(thread),
            }
        }

        fn peak(&self) -> u64 {
            self.peak_vram_mb.load(Ordering::Relaxed)
        }

        fn finish(mut self) -> u64 {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            self.peak()
        }
    }

    impl Drop for GpuPeakSampler {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn deterministic_oom_recovery_probe(root: &Path) -> Result<Value, String> {
        let runtime_root = root.join("oom-runtime");
        fs::create_dir_all(&runtime_root)
            .map_err(|error| format!("Could not create OOM probe runtime: {error}"))?;
        fs::write(
            runtime_root.join("bridge.py"),
            r#"import json, os, pathlib, sys
marker = pathlib.Path(__file__).with_name("oom-once")
for line in sys.stdin:
    json.loads(line)
    if not marker.exists():
        marker.write_text(str(os.getpid()))
        response = {"ok": False, "error": "CUDA out of memory during controlled release probe"}
    else:
        response = {"ok": True, "result": {"served_by": os.getpid()}}
    print(json.dumps(response), flush=True)
"#,
        )
        .map_err(|error| format!("Could not write OOM probe bridge: {error}"))?;
        let store = Store::open(runtime_root.join("state"), runtime_root.join("artifacts"))?;
        let runtime =
            RuntimeState::new_with_store(runtime_root.clone(), PathBuf::from("python3"), store);
        let first_error = match runtime.request(json!({ "operation": "analyze_audio" })) {
            Err(error) => error,
            Ok(_) => return Err("Controlled OOM request unexpectedly succeeded".to_string()),
        };
        if !first_error.to_ascii_lowercase().contains("out of memory") {
            return Err(format!(
                "OOM probe returned the wrong failure: {first_error}"
            ));
        }
        if !runtime
            .worker_pool
            .lock()
            .map_err(|_| "OOM worker pool lock failed")?
            .is_empty()
        {
            return Err("OOM worker was not quarantined".to_string());
        }
        let failed_pid = fs::read_to_string(runtime_root.join("oom-once"))
            .map_err(|error| format!("Could not read failed OOM worker PID: {error}"))?;
        let recovered = runtime.request(json!({ "operation": "analyze_audio" }))?;
        let recovered_pid = recovered["served_by"]
            .as_u64()
            .ok_or("Recovered OOM worker returned no PID")?;
        if failed_pid.trim() == recovered_pid.to_string() {
            return Err("OOM recovery reused the quarantined worker".to_string());
        }
        let health = runtime.worker_health_snapshot("foundation", json!({}))?;
        if health["worker_failures"].as_u64() != Some(1)
            || health["worker_restarts"].as_u64() != Some(1)
            || health["last_error"] != "gpu_memory_failure"
        {
            return Err(format!("OOM recovery telemetry is incomplete: {health}"));
        }
        runtime.stop_active_worker()?;
        let scheduler = require_quiescent_scheduler(&runtime)?;
        Ok(json!({
            "mode": "deterministic_fault_injection",
            "failure_class": "gpu_memory_failure",
            "quarantined_worker_pid": failed_pid.trim(),
            "recovered_worker_pid": recovered_pid,
            "worker_health": health,
            "final_scheduler": scheduler,
            "passed": true,
        }))
    }

    #[test]
    fn adaptive_vad_requires_speech_before_trailing_silence_can_stop_capture() {
        let sample_rate = 16_000;
        let mut detector = VoiceActivityDetector::new(sample_rate);
        let quiet = vec![0.001_f32; 8_000];
        detector.process(&quiet);
        assert!(!detector.snapshot().speech_detected);
        assert!(!detector.should_auto_stop(500));

        let voiced = (0..3_200)
            .map(|index| ((index as f32 * 0.12).sin()) * 0.2)
            .collect::<Vec<_>>();
        detector.process(&voiced);
        assert!(detector.snapshot().speech_detected);
        assert!(detector.snapshot().speech_frames > 0);

        detector.process(&quiet);
        assert!(!detector.snapshot().speech_active);
        assert!(detector.should_auto_stop(500));
    }

    #[test]
    fn adaptive_vad_tracks_low_background_without_classifying_it_as_voice() {
        let mut detector = VoiceActivityDetector::new(48_000);
        let background = (0..48_000)
            .map(|index| if index % 2 == 0 { 0.004 } else { -0.004 })
            .collect::<Vec<_>>();
        let snapshot = detector.process(&background);
        assert!(!snapshot.speech_active);
        assert!(!snapshot.speech_detected);
        assert!((0.003..=0.005).contains(&snapshot.noise_floor));
    }

    #[test]
    fn routed_playback_resampling_preserves_duration_and_endpoints() {
        let source = vec![0.0_f32, 0.5, 1.0, 0.5];
        let doubled = resample_mono(&source, 4, 8);
        assert_eq!(doubled.len(), 8);
        assert_eq!(doubled[0], 0.0);
        assert_eq!(*doubled.last().expect("resampled endpoint"), 0.5);
        assert_eq!(resample_mono(&source, 4, 4), source);
    }

    #[test]
    fn interrupted_audio_workers_release_active_session_state() {
        let root = std::env::temp_dir().join(format!("soundar-audio-recovery-{}", Uuid::new_v4()));
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        let recording_error = "Audio input stopped: device unavailable".to_string();
        let playback_error = "Audio output stopped: device unavailable".to_string();

        *runtime.active_recording.lock().expect("recording lock") = Some(ActiveRecording {
            device_name: "Disconnected input".to_string(),
            output_path: root.join("capture.wav"),
            sample_rate: 48_000,
            channels: 1,
            frames: Arc::new(AtomicU64::new(48_000)),
            peak_bits: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
            queued_frames: Arc::new(AtomicU64::new(0)),
            dropped_frames: Arc::new(AtomicU64::new(0)),
            speech_active: Arc::new(AtomicBool::new(false)),
            speech_detected: Arc::new(AtomicBool::new(false)),
            speech_frames: Arc::new(AtomicU64::new(0)),
            silence_frames: Arc::new(AtomicU64::new(0)),
            noise_floor_bits: Arc::new(AtomicU32::new(0.003_f32.to_bits())),
            auto_stopped: Arc::new(AtomicBool::new(false)),
            vad_enabled: true,
            auto_stop: false,
            silence_ms: 1_200,
            input_gain: 1.0,
            stop: Arc::new(AtomicBool::new(false)),
            thread: Some(std::thread::spawn({
                let error = recording_error.clone();
                move || Err(error)
            })),
        });
        let (_, result) = runtime
            .stop_active_recording()
            .expect("stop failed recording")
            .expect("active recording");
        assert_eq!(result.expect_err("capture interruption"), recording_error);
        assert!(runtime
            .active_recording
            .lock()
            .expect("recording lock")
            .is_none());

        *runtime.active_playback.lock().expect("playback lock") = Some(ActivePlayback {
            device_name: "Disconnected output".to_string(),
            audio_path: root.join("output.wav"),
            duration_seconds: 1.0,
            output_sample_rate: 48_000,
            played_frames: Arc::new(AtomicU64::new(24_000)),
            underrun_frames: Arc::new(AtomicU64::new(0)),
            completed: Arc::new(AtomicBool::new(false)),
            error: Arc::new(Mutex::new(Some(playback_error.clone()))),
            stop: Arc::new(AtomicBool::new(false)),
            thread: Some(std::thread::spawn({
                let error = playback_error.clone();
                move || Err(error)
            })),
            started_at: Instant::now(),
            startup_seconds: 0.01,
        });
        let (_, result) = runtime
            .stop_active_playback()
            .expect("stop failed playback")
            .expect("active playback");
        assert_eq!(result.expect_err("output interruption"), playback_error);
        assert!(runtime
            .active_playback
            .lock()
            .expect("playback lock")
            .is_none());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn navigation_is_confined_to_the_application_shell() {
        // A cross-origin `<a download>` is not honoured by the webview; it navigates instead, and
        // with no window decorations that leaves a blank media document and no way back.
        let blocked = [
            "http://127.0.0.1:41234/media/token//home/user/.soundAr/exports/master.mp4",
            "file:///home/user/.soundAr/exports/master.mp4",
            "asset://localhost//home/user/.soundAr/exports/master.mp4",
            "https://example.com/",
        ];
        for candidate in blocked {
            let url = tauri::Url::parse(candidate).expect("parse candidate");
            assert!(
                !navigation_is_allowed(&url) || cfg!(dev),
                "{candidate} must not replace the application shell"
            );
        }
        assert!(navigation_is_allowed(
            &tauri::Url::parse("tauri://localhost/index.html").expect("parse app url")
        ));
    }

    #[test]
    fn saving_is_restricted_to_files_inside_the_export_root() {
        let root = std::env::temp_dir().join(format!("soundar-save-{}", Uuid::new_v4()));
        let exports = root.join("exports");
        let outside = root.join("elsewhere");
        fs::create_dir_all(&exports).expect("exports");
        fs::create_dir_all(&outside).expect("outside");
        let export = exports.join("master.mp4");
        fs::write(&export, b"master").expect("write export");
        let secret = outside.join("secret.mp4");
        fs::write(&secret, b"secret").expect("write secret");
        let exports_root = fs::canonicalize(&exports).expect("canonicalize exports");

        assert_eq!(
            resolve_saveable_export(export.to_str().expect("path"), &exports_root),
            Ok(fs::canonicalize(&export).expect("canonicalize export"))
        );
        assert!(resolve_saveable_export(secret.to_str().expect("path"), &exports_root).is_err());
        assert!(resolve_saveable_export(
            exports.join("missing.mp4").to_str().expect("path"),
            &exports_root
        )
        .is_err());
        // A symlink planted inside the export tree must not reach the rest of the filesystem.
        let escape = exports.join("escape.mp4");
        std::os::unix::fs::symlink(&secret, &escape).expect("symlink");
        assert!(resolve_saveable_export(escape.to_str().expect("path"), &exports_root).is_err());
        // Directories are not exports.
        assert!(resolve_saveable_export(exports.to_str().expect("path"), &exports_root).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn suggested_save_names_cannot_carry_a_path() {
        let source = Path::new("/home/user/.soundAr/exports/video/master.mp4");
        assert_eq!(saved_file_name(Some("reel.mp4"), source), "reel.mp4");
        assert_eq!(saved_file_name(Some("  reel.mp4  "), source), "reel.mp4");
        // Anything with a separator falls back to the artifact's own file name.
        assert_eq!(
            saved_file_name(Some("../../etc/passwd"), source),
            "master.mp4"
        );
        assert_eq!(saved_file_name(Some("a/b.mp4"), source), "master.mp4");
        assert_eq!(saved_file_name(Some(""), source), "master.mp4");
        assert_eq!(saved_file_name(None, source), "master.mp4");
        assert_eq!(saved_file_name(None, Path::new("/")), "soundar-export");
    }

    #[test]
    fn generated_audio_reader_rejects_paths_outside_exports() {
        let root = std::env::temp_dir().join(format!("soundar-reader-{}", Uuid::new_v4()));
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let error = store
            .generated_audio_bytes("/etc/hosts")
            .expect_err("outside path should be rejected");
        assert_eq!(
            error,
            "Generated audio is outside the managed artifact directory"
        );
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn worker_pool_executes_independent_requests_in_parallel() {
        let root = std::env::temp_dir().join(format!("soundar-parallel-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create test runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, sys, time
for line in sys.stdin:
    time.sleep(0.35)
    print(json.dumps({"ok": True, "result": {"parallel": True}}), flush=True)
"#,
        )
        .expect("write fake bridge");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        let started = Instant::now();
        let first = {
            let runtime = runtime.clone();
            std::thread::spawn(move || runtime.request(json!({ "operation": "analyze_audio" })))
        };
        let second = {
            let runtime = runtime.clone();
            std::thread::spawn(move || runtime.request(json!({ "operation": "analyze_audio" })))
        };
        assert_eq!(
            first.join().expect("first thread").expect("first request")["parallel"],
            true
        );
        assert_eq!(
            second
                .join()
                .expect("second thread")
                .expect("second request")["parallel"],
            true
        );
        assert!(
            started.elapsed() < Duration::from_millis(650),
            "two 350 ms requests should overlap"
        );
        assert_eq!(runtime.worker_pool.lock().expect("pool").len(), 2);
        runtime.stop_active_worker().expect("stop fake workers");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn model_runtime_load_health_and_unload_are_observable() {
        let root = std::env::temp_dir().join(format!("soundar-lifecycle-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create lifecycle runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, os, sys
loaded = []
for line in sys.stdin:
    request = json.loads(line)
    operation = request.get("operation")
    if operation == "load":
        loaded = [request["model_id"]]
        result = {"status": "loaded", "model_id": loaded[0], "engine": "foundation", "task": "tts", "device": "test", "vram": {"used_mb": 12, "total_mb": 64, "percent": 18.75}}
    elif operation == "unload":
        previous, loaded = loaded, []
        result = {"status": "unloaded", "engine_scope": "foundation", "unloaded_models": previous}
    else:
        result = {"status": "ready", "device": "test", "engine_scope": "foundation", "engine_runtime": "isolated", "process_id": os.getpid(), "loaded_models": loaded}
    print(json.dumps({"ok": True, "result": result}), flush=True)
"#,
        )
        .expect("write lifecycle bridge");
        let registry = root.join("models.json");
        fs::write(
            &registry,
            r#"{"models":[{"model_id":"test/lifecycle","engine":"foundation","task":"tts"}]}"#,
        )
        .expect("write lifecycle registry");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry,
            store,
        );

        let loaded = runtime
            .prewarm_model("test/lifecycle")
            .expect("prewarm model");
        assert_eq!(loaded["status"], "loaded");
        assert_eq!(loaded["model_id"], "test/lifecycle");
        let health = runtime
            .check_engine_health("foundation")
            .expect("resident health");
        assert_eq!(health["loaded_models"], json!(["test/lifecycle"]));
        assert_eq!(health["warm_workers"], 1);

        let unloaded = runtime
            .unload_model_runtime("test/lifecycle")
            .expect("unload model");
        assert_eq!(unloaded["status"], "unloaded");
        assert_eq!(unloaded["retired_workers"], 1);
        assert_eq!(unloaded["unloaded_models"], json!(["test/lifecycle"]));
        assert!(runtime.worker_pool.lock().expect("pool").is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn model_runtime_unload_refuses_active_engine_work() {
        let root = std::env::temp_dir().join(format!("soundar-unload-active-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create active runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, os, sys, time
for line in sys.stdin:
    request = json.loads(line)
    if request.get("operation") == "health":
        result = {"status": "ready", "device": "test", "engine_scope": "foundation", "engine_runtime": "isolated", "process_id": os.getpid(), "loaded_models": []}
    else:
        time.sleep(10)
        result = {"finished": True}
    print(json.dumps({"ok": True, "result": result}), flush=True)
"#,
        )
        .expect("write active bridge");
        let registry = root.join("models.json");
        fs::write(
            &registry,
            r#"{"models":[{"model_id":"test/active","engine":"foundation","task":"tts"}]}"#,
        )
        .expect("write active registry");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry,
            store,
        );
        let job = runtime
            .store
            .create_job("test-inference", &json!({ "operation": "analyze_audio" }))
            .expect("create active job");
        let inference = {
            let runtime = runtime.clone();
            let job = job.clone();
            std::thread::spawn(move || {
                runtime.request_for_job(json!({ "operation": "analyze_audio" }), &job)
            })
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let active = runtime
                .scheduler
                .0
                .lock()
                .expect("scheduler")
                .active_engines
                .get("foundation")
                .copied()
                .unwrap_or(0);
            let registered = runtime
                .active_syntheses
                .lock()
                .expect("active syntheses")
                .contains_key(&job);
            if active == 1 && registered {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "request should acquire engine lease"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = runtime
            .unload_model_runtime("test/active")
            .expect_err("active engine cannot unload");
        assert!(error.contains("1 active job"));
        assert!(runtime.cancel_job(&job).expect("cancel active job"));
        inference
            .join()
            .expect("join inference")
            .expect_err("cancelled request");
        assert!(runtime
            .scheduler
            .0
            .lock()
            .expect("scheduler")
            .active_engines
            .is_empty());
        runtime.stop_active_worker().expect("stop cancelled worker");
        assert!(runtime.worker_pool.lock().expect("pool").is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn background_model_load_is_durably_cancellable_without_a_worker_leak() {
        let root = std::env::temp_dir().join(format!("soundar-cancel-load-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create cancellable load runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, sys, time
for line in sys.stdin:
    request = json.loads(line)
    if request.get("operation") == "load":
        time.sleep(10)
    print(json.dumps({"ok": True, "result": {"status": "loaded", "model_id": request.get("model_id")}}), flush=True)
"#,
        )
        .expect("write cancellable bridge");
        let registry = root.join("models.json");
        fs::write(
            &registry,
            r#"{"models":[{"model_id":"test/cancellable","engine":"foundation","task":"tts"}]}"#,
        )
        .expect("write cancellable registry");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry,
            store,
        );
        let request = json!({
            "operation": "load",
            "model_id": "test/cancellable",
            "title": "Load cancellable",
            "priority": "urgent",
        });
        let job = runtime
            .store
            .create_job("model-load", &request)
            .expect("create model load job");
        runtime
            .store
            .update_job(&job, "preparing", 0.05)
            .expect("prepare model load");
        runtime
            .start_background_model_load(job.clone(), "test/cancellable".to_string())
            .expect("start background load");

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if runtime
                .active_syntheses
                .lock()
                .expect("active tasks")
                .contains_key(&job)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "model load should register its worker"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(runtime.cancel_job(&job).expect("cancel model load"));

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let active = runtime
                .active_syntheses
                .lock()
                .expect("active tasks")
                .contains_key(&job);
            if !active {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cancelled load should release its worker"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            runtime
                .store
                .job_status(&job)
                .expect("job status")
                .as_deref(),
            Some("cancelled")
        );
        assert!(runtime.worker_pool.lock().expect("pool").is_empty());
        assert!(runtime
            .scheduler
            .0
            .lock()
            .expect("scheduler")
            .active_engines
            .is_empty());
        runtime
            .stop_active_worker()
            .expect("stop cancelled workers");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn gpu_memory_failure_quarantines_worker_and_next_request_recovers() {
        let root = std::env::temp_dir().join(format!("soundar-oom-recovery-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create OOM runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, os, pathlib, sys
marker = pathlib.Path(__file__).with_name("oom-once")
for line in sys.stdin:
    request = json.loads(line)
    if not marker.exists():
        marker.write_text(str(os.getpid()))
        response = {"ok": False, "error": "CUDA out of memory while loading checkpoint"}
    else:
        response = {"ok": True, "result": {"served_by": os.getpid()}}
    print(json.dumps(response), flush=True)
"#,
        )
        .expect("write OOM bridge");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        let error = runtime
            .request(json!({ "operation": "analyze_audio" }))
            .expect_err("first request should OOM");
        assert!(error.contains("CUDA out of memory"));
        assert!(runtime.worker_pool.lock().expect("pool").is_empty());
        let first_pid = fs::read_to_string(root.join("oom-once")).expect("first PID");
        let recovered = runtime
            .request(json!({ "operation": "analyze_audio" }))
            .expect("clean worker should recover");
        assert_ne!(recovered["served_by"].to_string(), first_pid.trim());
        let health = runtime
            .worker_health_snapshot("foundation", json!({}))
            .expect("health telemetry");
        assert_eq!(health["worker_failures"], 1);
        assert_eq!(health["last_error"], "gpu_memory_failure");
        runtime.stop_active_worker().expect("stop recovered worker");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn benchmark_preparation_retires_only_idle_engine_workers_and_runtime_labels_reuse() {
        let root = std::env::temp_dir().join(format!("soundar-benchmark-state-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create benchmark runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, sys
for line in sys.stdin:
    request = json.loads(line)
    print(json.dumps({"ok": True, "result": {"model_id": request.get("model_id"), "engine": "foundation", "inference_seconds": 0.01}}), flush=True)
"#,
        )
        .expect("write fake bridge");
        let registry_path = root.join("models.json");
        fs::write(
            &registry_path,
            r#"{"models":[{"model_id":"test/benchmark","engine":"foundation"}]}"#,
        )
        .expect("write benchmark registry");
        let store =
            Store::open(root.join("state"), root.join("artifacts")).expect("benchmark state store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry_path,
            store,
        );
        runtime
            .request(json!({ "operation": "analyze_audio" }))
            .expect("seed idle foundation worker");
        let prepared = runtime
            .prepare_benchmark_engine("test/benchmark")
            .expect("prepare cold benchmark");
        assert_eq!(prepared["retired_workers"], 1);
        let token = prepared["token"].as_str().expect("benchmark token");

        let request = json!({
            "operation": "synthesize",
            "model_id": "test/benchmark",
            "benchmark_token": token,
        });
        let persisted_job = runtime
            .store
            .create_job("synthesis", &request)
            .expect("persist benchmark request");
        let persisted = runtime
            .store
            .list_jobs()
            .expect("list benchmark job")
            .into_iter()
            .find(|job| job["id"] == persisted_job)
            .expect("persisted benchmark job");
        assert!(persisted["request"].get("benchmark_token").is_none());
        let cold = runtime.request(request.clone()).expect("cold request");
        let warm = runtime.request(request).expect("warm request");
        assert_eq!(cold["runtime_worker_state"], "cold");
        assert_eq!(warm["runtime_worker_state"], "warm");
        assert!(cold["end_to_end_seconds"].as_f64().is_some());
        assert!(cold["runtime_overhead_seconds"].as_f64().is_some());

        runtime
            .scheduler
            .0
            .lock()
            .expect("scheduler")
            .active_workers = 1;
        assert!(runtime
            .prepare_benchmark_engine("test/benchmark")
            .expect_err("busy scheduler must reject cold preparation")
            .contains("idle inference queue"));
        runtime
            .scheduler
            .0
            .lock()
            .expect("scheduler")
            .active_workers = 0;
        assert!(runtime
            .release_benchmark_engine(token)
            .expect("release benchmark reservation"));
        runtime.stop_active_worker().expect("stop benchmark worker");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn comparison_coordinator_runs_four_takes_in_parallel_and_preserves_partial_failure() {
        let root = std::env::temp_dir().join(format!("soundar-compare-{}", Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).expect("create comparison runtime");
        let artifacts_json =
            serde_json::to_string(&artifacts.to_string_lossy()).expect("artifact path JSON");
        fs::write(
            root.join("bridge.py"),
            format!(
                r#"import json, pathlib, struct, sys, time, uuid, wave
for line in sys.stdin:
    request = json.loads(line)
    if request.get("model_id") == "test/fail":
        time.sleep(0.15)
        print(json.dumps({{"ok": False, "error": "intentional take failure"}}), flush=True)
        continue
    time.sleep(0.35)
    output = pathlib.Path({artifacts_json}) / f"{{uuid.uuid4().hex}}.wav"
    with wave.open(str(output), "wb") as audio:
        audio.setnchannels(1); audio.setsampwidth(2); audio.setframerate(16000)
        audio.writeframes(struct.pack("<h", 0) * 800)
    print(json.dumps({{"ok": True, "result": {{
        "id": uuid.uuid4().hex, "model_id": request["model_id"], "engine": "foundation",
        "audio_path": str(output), "sample_rate": 16000, "duration_seconds": 0.05,
        "inference_seconds": 0.35, "rtf": 7.0, "vram_peak_mb": 0, "waveform": [0.0]
    }}}}), flush=True)
"#
            ),
        )
        .expect("write comparison worker");
        let registry_path = root.join("models.json");
        fs::write(
            &registry_path,
            r#"{"models":[{"model_id":"test/one","engine":"foundation"},{"model_id":"test/two","engine":"foundation"},{"model_id":"test/three","engine":"foundation"},{"model_id":"test/fail","engine":"foundation"}]}"#,
        )
        .expect("write comparison registry");
        let store = Store::open(root.join("state"), artifacts).expect("comparison store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry_path,
            store,
        );
        runtime.scheduler.0.lock().expect("scheduler").max_workers = 2;
        let run = runtime
            .store
            .create_comparison(&json!({
                "script": "Parallel comparison proof.",
                "takes": [
                    {"model_id":"test/one"}, {"model_id":"test/two"},
                    {"model_id":"test/three"}, {"model_id":"test/fail"}
                ]
            }))
            .expect("create comparison");
        let id = run["id"].as_str().expect("comparison ID");
        let started = Instant::now();
        let completed = runtime.execute_comparison(id).expect("execute comparison");
        assert!(started.elapsed() < Duration::from_millis(950));
        assert_eq!(completed["status"], "partial");
        assert_eq!(
            completed["takes"]
                .as_array()
                .expect("takes")
                .iter()
                .filter(|take| take["status"] == "completed")
                .count(),
            3
        );
        assert!(completed["takes"]
            .as_array()
            .expect("takes")
            .iter()
            .any(|take| take["error"] == "intentional take failure"));
        assert_eq!(
            runtime.scheduler_status().expect("scheduler")["active_workers"],
            0
        );
        runtime.stop_active_worker().expect("stop workers");
        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scheduler_priority_ages_and_preserves_fifo_ties() {
        let now = Instant::now();
        let old_low = SchedulerWaiter {
            ticket: 1,
            priority: 0,
            enqueued_at: now - Duration::from_secs(91),
            reserved_vram_mb: 0,
            uses_gpu: false,
            qualified_video_overlap: false,
            benchmark_token: None,
        };
        let fresh_urgent = SchedulerWaiter {
            ticket: 2,
            priority: 3,
            enqueued_at: now,
            reserved_vram_mb: 0,
            uses_gpu: false,
            qualified_video_overlap: false,
            benchmark_token: None,
        };
        assert!(scheduler_rank(&old_low, now) > scheduler_rank(&fresh_urgent, now));

        let earlier_high = SchedulerWaiter {
            ticket: 3,
            priority: 2,
            enqueued_at: now,
            reserved_vram_mb: 0,
            uses_gpu: false,
            qualified_video_overlap: false,
            benchmark_token: None,
        };
        let later_high = SchedulerWaiter {
            ticket: 4,
            priority: 2,
            enqueued_at: now,
            reserved_vram_mb: 0,
            uses_gpu: false,
            qualified_video_overlap: false,
            benchmark_token: None,
        };
        assert!(scheduler_rank(&earlier_high, now) > scheduler_rank(&later_high, now));
    }

    #[test]
    fn video_and_inference_share_one_cancel_safe_gpu_envelope() {
        use crate::video::SharedGpuAdmissionGate as _;

        let scheduler = Arc::new((Mutex::new(new_inference_scheduler(4)), Condvar::new()));
        let gate = GlobalVideoGpuGate::new(Arc::clone(&scheduler), PathBuf::from("python3"));
        let request = crate::video::SharedGpuAdmissionRequest {
            job_id: "video-render-one".into(),
            project_id: "video-project-one".into(),
            resource_class: crate::video::ResourceClass::Medium,
            requested_vram_mb: 1_024,
            requested_nvenc_sessions: 1,
            exclusive: false,
        };

        scheduler.0.lock().expect("scheduler").active_gpu_workers = 1;
        assert!(matches!(
            gate.try_acquire(&request).expect("normal backpressure"),
            crate::video::SharedGpuAdmissionOutcome::Waiting(_)
        ));
        scheduler
            .0
            .lock()
            .expect("scheduler")
            .active_qualified_video_overlap_workers = 1;
        let overlap_lease = match gate
            .try_acquire(&request)
            .expect("qualified overlap admission")
        {
            crate::video::SharedGpuAdmissionOutcome::Admitted(lease) => lease,
            crate::video::SharedGpuAdmissionOutcome::Waiting(wait) => {
                panic!("unexpected qualified overlap backpressure: {}", wait.reason)
            }
        };
        drop(overlap_lease);
        {
            let mut state = scheduler.0.lock().expect("scheduler");
            state.active_gpu_workers = 0;
            state.active_qualified_video_overlap_workers = 0;
        }

        let lease = match gate.try_acquire(&request).expect("admit video render") {
            crate::video::SharedGpuAdmissionOutcome::Admitted(lease) => lease,
            crate::video::SharedGpuAdmissionOutcome::Waiting(wait) => {
                panic!("unexpected GPU backpressure: {}", wait.reason)
            }
        };
        {
            let state = scheduler.0.lock().expect("scheduler");
            assert_eq!(state.active_video_gpu_jobs.len(), 1);
            assert_eq!(state.video_reserved_vram_mb, 1_024);
            assert_eq!(state.video_nvenc_sessions, 1);
        }
        let exclusive = crate::video::SharedGpuAdmissionRequest {
            job_id: "video-exclusive".into(),
            exclusive: true,
            ..request.clone()
        };
        assert!(matches!(
            gate.try_acquire(&exclusive)
                .expect("exclusive backpressure"),
            crate::video::SharedGpuAdmissionOutcome::Waiting(_)
        ));

        drop(lease);
        let state = scheduler.0.lock().expect("scheduler");
        assert!(state.active_video_gpu_jobs.is_empty());
        assert_eq!(state.video_reserved_vram_mb, 0);
        assert_eq!(state.video_nvenc_sessions, 0);
    }

    #[test]
    fn only_qualified_whisper_tiny_transcription_can_overlap_nvenc() {
        assert!(request_supports_qualified_video_overlap(
            "transcribe",
            "transformers",
            Some("openai/whisper-tiny"),
        ));
        assert!(!request_supports_qualified_video_overlap(
            "transcribe",
            "transformers",
            Some("openai/whisper-small"),
        ));
        assert!(!request_supports_qualified_video_overlap(
            "generate_music",
            "musicgen",
            Some("facebook/musicgen-small"),
        ));

        let request = crate::video::SharedGpuAdmissionRequest {
            job_id: "qualified-final".into(),
            project_id: "project".into(),
            resource_class: crate::video::ResourceClass::Heavy,
            requested_vram_mb: 2_048,
            requested_nvenc_sessions: 1,
            exclusive: false,
        };
        assert!(video_request_supports_qualified_whisper_overlap(&request));
        assert!(!video_request_supports_qualified_whisper_overlap(
            &crate::video::SharedGpuAdmissionRequest {
                requested_vram_mb: 2_049,
                ..request.clone()
            }
        ));

        let mut scheduler = new_inference_scheduler(4);
        scheduler.active_video_gpu_jobs.insert(
            "qualified-final".into(),
            VideoGpuReservation {
                vram_mb: 2_048,
                nvenc_sessions: 1,
                exclusive: false,
            },
        );
        scheduler.video_reserved_vram_mb = 2_048;
        scheduler.video_nvenc_sessions = 1;
        assert!(active_video_jobs_support_qualified_whisper_overlap(
            &scheduler
        ));
        scheduler.active_video_gpu_jobs.insert(
            "second-render".into(),
            VideoGpuReservation {
                vram_mb: 1_024,
                nvenc_sessions: 1,
                exclusive: false,
            },
        );
        scheduler.video_nvenc_sessions = 2;
        assert!(!active_video_jobs_support_qualified_whisper_overlap(
            &scheduler
        ));
    }

    #[test]
    fn fish_jobs_share_one_resident_worker_and_accept_progress_events() {
        let root = std::env::temp_dir().join(format!("soundar-fish-queue-{}", Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(root.join("data")).expect("create Fish runtime data");
        fs::create_dir_all(&artifacts).expect("create Fish artifacts");
        let artifacts_json =
            serde_json::to_string(&artifacts.to_string_lossy()).expect("artifact JSON");
        fs::write(
            root.join("bridge.py"),
            format!(
                r#"import json, pathlib, sys, time
for line in sys.stdin:
    request = json.loads(line)
    time.sleep(0.22)
    preview = pathlib.Path({artifacts_json}) / ('.preview-' + request['_job_id'] + '.wav')
    preview.write_bytes(b'RIFF\x10\x00\x00\x00WAVEprogressive')
    print(json.dumps({{"event": {{"type": "audio-preview", "audio_path": str(preview), "duration_seconds": 0.5, "first_audio_seconds": 0.22, "progress": 0.82}}}}), flush=True)
    time.sleep(0.22)
    print(json.dumps({{"ok": True, "result": {{"model_id": request["model_id"], "engine": "fish-speech", "inference_seconds": 0.44}}}}), flush=True)
"#
            ),
        )
        .expect("write Fish bridge");
        fs::write(
            root.join("data/engine_manifests.json"),
            r#"{"engines":[{"id":"fish-speech","minimum_vram_mb":0}]}"#,
        )
        .expect("write Fish manifest");
        let registry = root.join("models.json");
        fs::write(
            &registry,
            r#"{"models":[{"model_id":"test/fish","engine":"fish-speech","task":"tts"}]}"#,
        )
        .expect("write Fish registry");
        let store = Store::open(root.join("state"), artifacts).expect("Fish store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry,
            store,
        );
        runtime.scheduler.0.lock().expect("scheduler").max_workers = 4;
        let request = json!({"operation":"synthesize", "model_id":"test/fish"});
        let jobs = [
            runtime
                .store
                .create_job("synthesis", &request)
                .expect("first Fish job"),
            runtime
                .store
                .create_job("synthesis", &request)
                .expect("second Fish job"),
        ];
        let started = Instant::now();
        let handles = jobs
            .iter()
            .map(|job| {
                let runtime = runtime.clone();
                let request = request.clone();
                let job = job.clone();
                std::thread::spawn(move || runtime.request_for_job(request, &job))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("join Fish job").expect("run Fish job");
        }
        assert!(started.elapsed() >= Duration::from_millis(800));
        assert_eq!(runtime.worker_pool.lock().expect("worker pool").len(), 1);
        assert_eq!(
            runtime.scheduler_status().expect("status")["engine_limits"]["fish-speech"],
            1
        );
        let records = runtime.store.list_jobs().expect("Fish job previews");
        assert!(records
            .iter()
            .all(|job| job["preview_duration_seconds"] == 0.5));
        for job in &jobs {
            runtime.store.cancel_job(job).expect("clear Fish preview");
        }
        runtime.stop_active_worker().expect("stop Fish worker");
        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scheduler_admits_urgent_waiter_before_normal_waiter() {
        let root = std::env::temp_dir().join(format!("soundar-priority-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create priority runtime");
        let event_path = root.join("events.txt");
        fs::write(
            root.join("bridge.py"),
            r#"import json, pathlib, sys, time
for line in sys.stdin:
    request = json.loads(line)
    with pathlib.Path(request["event_path"]).open("a") as handle:
        handle.write(request["label"] + "\n")
    time.sleep(0.45)
    print(json.dumps({"ok": True, "result": {"label": request["label"]}}), flush=True)
"#,
        )
        .expect("write priority bridge");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        runtime.scheduler.0.lock().expect("scheduler").max_workers = 1;

        let launch = |runtime: RuntimeState,
                      label: &'static str,
                      priority: &'static str,
                      event_path: PathBuf| {
            std::thread::spawn(move || {
                runtime.request(json!({
                "operation": "analyze_audio", "label": label, "priority": priority, "event_path": event_path
            }))
            })
        };
        let active = launch(runtime.clone(), "active", "normal", event_path.clone());
        let deadline = Instant::now() + Duration::from_secs(2);
        while fs::read_to_string(&event_path)
            .unwrap_or_default()
            .lines()
            .next()
            != Some("active")
        {
            assert!(Instant::now() < deadline, "active request should start");
            std::thread::sleep(Duration::from_millis(5));
        }
        let normal = launch(runtime.clone(), "normal", "normal", event_path.clone());
        let waiting_deadline = Instant::now() + Duration::from_secs(2);
        while runtime.scheduler_status().expect("scheduler status")["waiting_jobs"] != 1 {
            assert!(
                Instant::now() < waiting_deadline,
                "normal request should wait"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let urgent = launch(runtime.clone(), "urgent", "urgent", event_path.clone());
        let urgent_deadline = Instant::now() + Duration::from_secs(2);
        while runtime.scheduler_status().expect("urgent scheduler status")["waiting_jobs"] != 2 {
            assert!(
                Instant::now() < urgent_deadline,
                "urgent request should join the wait queue"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        active
            .join()
            .expect("active thread")
            .expect("active request");
        normal
            .join()
            .expect("normal thread")
            .expect("normal request");
        urgent
            .join()
            .expect("urgent thread")
            .expect("urgent request");
        let order = fs::read_to_string(&event_path).expect("priority event log");
        assert_eq!(
            order.lines().collect::<Vec<_>>(),
            vec!["active", "urgent", "normal"]
        );
        assert_eq!(
            runtime.scheduler_status().expect("settled scheduler")["waiting_jobs"],
            0
        );
        runtime.stop_active_worker().expect("stop priority worker");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn batch_coordinator_runs_a_rolling_parallel_queue_once() {
        let root = std::env::temp_dir().join(format!("soundar-batch-parallel-{}", Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&root).expect("create batch runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, pathlib, struct, sys, time, uuid, wave
for line in sys.stdin:
    request = json.loads(line)
    text = request.get("text", "")
    events = pathlib.Path(request["test_event_path"])
    with events.open("a") as handle:
        handle.write(f"start\t{text}\t{time.monotonic()}\n")
    time.sleep(0.8 if text == "slow" else 0.15)
    output = pathlib.Path(request["test_output_dir"]) / f"{uuid.uuid4().hex}.wav"
    staging = pathlib.Path(str(output) + ".partial")
    with wave.open(str(staging), "wb") as audio:
        audio.setnchannels(1); audio.setsampwidth(2); audio.setframerate(16000)
        audio.writeframes(struct.pack("<h", 0) * 800)
    with events.open("a") as handle:
        handle.write(f"end\t{text}\t{time.monotonic()}\n")
    print(json.dumps({"ok": True, "result": {
        "id": uuid.uuid4().hex, "model_id": request["model_id"], "engine": "foundation",
        "audio_path": str(output), "staging_path": str(staging), "sample_rate": 16000,
        "duration_seconds": 0.05, "inference_seconds": 0.15, "rtf": 3.0,
        "vram_peak_mb": 0, "waveform": [0.0, 0.0]
    }}), flush=True)
"#,
        )
        .expect("write fake batch bridge");
        let registry_path = root.join("models.json");
        fs::write(
            &registry_path,
            r#"{"models":[{"model_id":"test/model","engine":"foundation"}]}"#,
        )
        .expect("write isolated model registry");
        let store = Store::open(root.join("state"), artifacts.clone()).expect("test store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry_path,
            store,
        );
        let event_path = root.join("batch-events.tsv");
        let batch = runtime
            .queue_batch(
                &json!({
                    "name": "Rolling queue proof",
                    "rows": [
                        {"text": "slow", "priority": "normal"},
                        {"text": "fast-1", "priority": "urgent"},
                        {"text": "fast-2", "priority": "normal"},
                        {"text": "fast-3", "priority": "high"},
                        {"text": "fast-4", "priority": "normal"}
                    ],
                    "settings": {
                        "model_id": "test/model",
                        "test_output_dir": artifacts,
                        "test_event_path": event_path,
                    }
                }),
                2,
            )
            .expect("queue parallel batch");
        let batch_id = batch["id"].as_str().expect("batch id");
        assert_eq!(batch["request"]["parallelism"], 2);
        assert_eq!(
            runtime.scheduler_status().expect("scheduler")["active_batches"],
            1
        );
        assert!(runtime
            .execute_batch(batch_id, 2)
            .expect_err("duplicate coordinator must be rejected")
            .contains("active coordinator"));

        let deadline = Instant::now() + Duration::from_secs(5);
        let completed = loop {
            let current = runtime
                .store
                .get_batch(batch_id)
                .expect("read batch")
                .expect("batch exists");
            if current["status"] == "completed" {
                break current;
            }
            assert!(Instant::now() < deadline, "parallel batch should complete");
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(completed["completed_items"], 5);
        assert!(completed["items"]
            .as_array()
            .expect("batch rows")
            .iter()
            .all(|item| item["job_id"].is_string() && item["history_id"].is_string()));
        let events = fs::read_to_string(&event_path).expect("read batch timing events");
        let timestamp = |kind: &str, text: &str| -> f64 {
            events
                .lines()
                .find_map(|line| {
                    let fields = line.split('\t').collect::<Vec<_>>();
                    (fields.len() == 3 && fields[0] == kind && fields[1] == text)
                        .then(|| fields[2].parse::<f64>().expect("event timestamp"))
                })
                .expect("timing event")
        };
        assert!(
            timestamp("start", "fast-2") < timestamp("end", "slow"),
            "the next queued row must start as soon as a worker becomes free"
        );
        assert!(
            timestamp("start", "fast-1") <= timestamp("start", "slow"),
            "urgent batch rows must be admitted before normal rows"
        );
        let coordinator_deadline = Instant::now() + Duration::from_secs(1);
        while runtime.scheduler_status().expect("settled scheduler")["active_batches"] != 0 {
            assert!(
                Instant::now() < coordinator_deadline,
                "batch coordinator should release its lease"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            runtime
                .store
                .list_history(None)
                .expect("batch history")
                .len(),
            5
        );
        runtime.stop_active_worker().expect("stop fake workers");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn health_check_restarts_a_crashed_worker_and_releases_scheduler_capacity() {
        let root = std::env::temp_dir().join(format!("soundar-recovery-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create recovery runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, os, pathlib, sys
marker = pathlib.Path(__file__).with_name("crashed-once")
for line in sys.stdin:
    if not marker.exists():
        marker.write_text("yes")
        os._exit(17)
    request = json.loads(line)
    print(json.dumps({"ok": True, "result": {"status": "ready", "device": "test", "engine_scope": request.get("requested_engine"), "engine_runtime": "isolated", "process_id": os.getpid()}}), flush=True)
"#,
        ).expect("write crashing bridge");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);

        let health = runtime
            .check_engine_health("foundation")
            .expect("recover health check");
        assert_eq!(health["status"], "ready");
        assert_eq!(health["worker_starts"], 2);
        assert_eq!(health["worker_restarts"], 1);
        assert_eq!(health["worker_failures"], 1);
        assert_eq!(health["warm_workers"], 1);
        assert_eq!(health["last_error"], "process_exited");
        let scheduler = runtime.scheduler.0.lock().expect("scheduler");
        assert_eq!(scheduler.active_workers, 0);
        assert_eq!(scheduler.reserved_vram_mb, 0);
        drop(scheduler);
        runtime.stop_active_worker().expect("stop recovered worker");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn repeated_synthesis_crashes_reopen_cleanly_and_retry_without_leaks() {
        let root = std::env::temp_dir().join(format!("soundar-crash-injection-{}", Uuid::new_v4()));
        let state = root.join("state");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).expect("create crash-injection runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, os, pathlib, sys
for line in sys.stdin:
    request = json.loads(line)
    marker = pathlib.Path(request["test_marker_path"])
    staging = pathlib.Path(request["test_staging_path"])
    if not marker.exists():
        staging.write_bytes(b"RIFF\x04\x00\x00\x00WAVEpartial")
        marker.write_text(str(os.getpid()))
        os._exit(17)
    staging.write_bytes(b"RIFF\x04\x00\x00\x00WAVErecovered")
    print(json.dumps({"ok": True, "result": {
        "id": request["test_history_id"], "model_id": request["model_id"],
        "engine": "foundation", "audio_path": request["test_final_path"],
        "staging_path": request["test_staging_path"], "sample_rate": 24000,
        "duration_seconds": 1.0, "inference_seconds": 0.1, "rtf": 0.1,
        "vram_peak_mb": 0, "waveform": [0.25, 0.5, 0.25]
    }}), flush=True)
"#,
        )
        .expect("write repeatedly crashing bridge");
        let registry = root.join("models.json");
        fs::write(
            &registry,
            r#"{"models":[{"model_id":"test/model","engine":"foundation"}]}"#,
        )
        .expect("write crash-injection registry");

        for cycle in 0..5 {
            let marker = root.join(format!("crash-{cycle}.marker"));
            let final_path = artifacts.join(format!("recovered-{cycle}.wav"));
            let staging_path = artifacts.join(format!("recovered-{cycle}.wav.partial"));
            let request = json!({
                "operation": "synthesize",
                "model_id": "test/model",
                "text": format!("Crash injection cycle {cycle}"),
                "speaker": "test",
                "test_marker_path": marker,
                "test_final_path": final_path,
                "test_staging_path": staging_path,
                "test_history_id": format!("recovered-history-{cycle}"),
            });
            let store = Store::open(state.clone(), artifacts.clone()).expect("open cycle store");
            let runtime = RuntimeState::new_with_store_and_registry(
                root.clone(),
                PathBuf::from("python3"),
                registry.clone(),
                store,
            );
            let job = runtime
                .store
                .create_job("synthesis", &request)
                .expect("create crash-injection job");
            runtime
                .start_background_synthesis(job.clone(), request.clone())
                .expect("start crashing synthesis");
            let failed_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if runtime
                    .store
                    .job_status(&job)
                    .expect("read failed status")
                    .as_deref()
                    == Some("failed")
                {
                    break;
                }
                assert!(
                    Instant::now() < failed_deadline,
                    "crash-injection job should fail durably"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                staging_path.is_file(),
                "the worker should leave a partial file"
            );
            assert!(!final_path.exists(), "a crash must not publish an artifact");
            assert!(runtime.worker_pool.lock().expect("failed pool").is_empty());
            let scheduler = runtime.scheduler.0.lock().expect("failed scheduler");
            assert_eq!(scheduler.active_workers, 0);
            assert_eq!(scheduler.reserved_vram_mb, 0);
            assert!(scheduler.active_engines.is_empty());
            drop(scheduler);
            drop(runtime);

            let reopened = Store::open(state.clone(), artifacts.clone())
                .expect("reopen after simulated app crash");
            assert!(
                !staging_path.exists(),
                "startup must remove an abandoned partial artifact"
            );
            assert_eq!(
                reopened
                    .job_status(&job)
                    .expect("recovered job status")
                    .as_deref(),
                Some("failed")
            );
            let (retried, retry_request) = reopened
                .retry_synthesis_job(&job)
                .expect("prepare a durable retry");
            assert_eq!(retried["attempt"], 2);
            let retry_runtime = RuntimeState::new_with_store_and_registry(
                root.clone(),
                PathBuf::from("python3"),
                registry.clone(),
                reopened,
            );
            retry_runtime
                .start_background_synthesis(job.clone(), retry_request)
                .expect("start recovered synthesis");
            let completed_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if retry_runtime
                    .store
                    .job_status(&job)
                    .expect("read completion status")
                    .as_deref()
                    == Some("completed")
                {
                    break;
                }
                assert!(
                    Instant::now() < completed_deadline,
                    "retried crash-injection job should complete"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(final_path.is_file());
            assert!(!staging_path.exists());
            assert!(retry_runtime
                .store
                .generated_audio_for_job(&job)
                .expect("checksum-verified retry playback")
                .0
                .starts_with(b"RIFF"));
            assert_eq!(
                retry_runtime
                    .store
                    .list_history(None)
                    .expect("recovered history")
                    .len(),
                cycle + 1
            );
            let health = retry_runtime
                .worker_health_snapshot("foundation", json!({}))
                .expect("crash recovery telemetry");
            assert_eq!(health["worker_failures"], cycle + 1);
            assert_eq!(health["worker_restarts"], cycle + 1);
            assert_eq!(health["last_error"], "process_exited");
            retry_runtime
                .stop_active_worker()
                .expect("stop recovered cycle worker");
        }

        let final_store = Store::open(state, artifacts).expect("open final recovered store");
        assert_eq!(
            final_store.list_history(None).expect("final history").len(),
            5
        );
        assert!(final_store
            .list_jobs()
            .expect("final jobs")
            .iter()
            .all(|job| job["status"] == "completed" && job["attempt"] == 2));
        drop(final_store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn stale_dead_worker_is_replaced_before_the_user_request_runs() {
        let root = std::env::temp_dir().join(format!("soundar-stale-worker-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create stale worker runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    if request.get("operation") == "health":
        result = {"status": "ready", "device": "test", "engine_scope": request.get("requested_engine"), "engine_runtime": "isolated", "process_id": os.getpid()}
    else:
        result = {"served_by": os.getpid()}
    print(json.dumps({"ok": True, "result": result}), flush=True)
"#,
        ).expect("write healthy bridge");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let mut runtime =
            RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        runtime.worker_probe_after = Duration::ZERO;
        runtime
            .check_engine_health("foundation")
            .expect("warm first worker");
        let first_pid = {
            let mut pool = runtime.worker_pool.lock().expect("pool");
            let worker = pool.first_mut().expect("warm worker");
            let pid = worker.child.id();
            worker.child.kill().expect("kill stale worker");
            worker.child.wait().expect("reap stale worker");
            pid
        };

        let result = runtime
            .request(json!({ "operation": "analyze_audio" }))
            .expect("replace stale worker before request");
        let replacement_pid = result["served_by"].as_u64().expect("replacement pid") as u32;
        assert_ne!(first_pid, replacement_pid);
        let health = runtime
            .worker_health_snapshot("foundation", json!({}))
            .expect("telemetry");
        assert_eq!(health["worker_restarts"], 1);
        assert_eq!(health["worker_failures"], 1);
        assert_eq!(health["last_error"], "liveness_probe_failed");
        runtime
            .stop_active_worker()
            .expect("stop replacement worker");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn local_rpc_rejects_oversized_requests_and_worker_responses() {
        let root = std::env::temp_dir().join(format!("soundar-rpc-limits-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create RPC runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, sys
for line in sys.stdin:
    print(json.dumps({"ok": True, "result": {"payload": "x" * 9000000}}), flush=True)
"#,
        )
        .expect("write oversized bridge");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        let request_error = runtime
            .request(json!({
                "operation": "analyze_audio", "payload": "x".repeat(MAX_RPC_REQUEST_BYTES)
            }))
            .expect_err("reject oversized request");
        assert!(request_error.contains("1 MB"));
        let response_error = runtime
            .request(json!({ "operation": "analyze_audio" }))
            .expect_err("reject oversized response");
        assert!(response_error.contains("8 MB"));
        assert!(runtime.worker_pool.lock().expect("pool").is_empty());
        let scheduler = runtime.scheduler.0.lock().expect("scheduler");
        assert_eq!(scheduler.active_workers, 0);
        drop(scheduler);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cancelling_active_worker_preserves_cancelled_job_and_runtime_recovers() {
        let root = std::env::temp_dir().join(format!("soundar-cancel-recovery-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create cancellation runtime");
        fs::write(
            root.join("bridge.py"),
            r#"import json, os, sys, time
for line in sys.stdin:
    request = json.loads(line)
    if request.get("operation") == "health":
        result = {"status": "ready", "device": "test", "engine_scope": request.get("requested_engine"), "engine_runtime": "isolated", "process_id": os.getpid()}
    else:
        time.sleep(10)
        result = {"unexpected": True}
    print(json.dumps({"ok": True, "result": result}), flush=True)
"#,
        ).expect("write cancellable bridge");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        let job = runtime
            .store
            .create_job("test-inference", &json!({ "operation": "analyze_audio" }))
            .expect("create cancellable job");
        runtime
            .store
            .update_job(&job, "running", 0.1)
            .expect("run cancellable job");
        let inference = {
            let runtime = runtime.clone();
            let job = job.clone();
            std::thread::spawn(move || {
                runtime.request_for_job(json!({ "operation": "analyze_audio" }), &job)
            })
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while !runtime
            .active_syntheses
            .lock()
            .expect("active requests")
            .contains_key(&job)
        {
            assert!(
                Instant::now() < deadline,
                "request should register for cancellation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(runtime.cancel_job(&job).expect("cancel worker"));
        let failure = inference
            .join()
            .expect("join cancelled request")
            .expect_err("cancelled request should fail");
        assert_eq!(failure, "Task cancelled during inference");
        assert_eq!(
            runtime
                .store
                .job_status(&job)
                .expect("job status")
                .as_deref(),
            Some("cancelled")
        );
        assert!(runtime
            .active_syntheses
            .lock()
            .expect("active requests")
            .is_empty());
        assert!(runtime.worker_pool.lock().expect("pool").is_empty());
        let scheduler = runtime.scheduler.0.lock().expect("scheduler");
        assert_eq!(scheduler.active_workers, 0);
        drop(scheduler);
        let health = runtime
            .check_engine_health("foundation")
            .expect("runtime recovers after cancellation");
        assert_eq!(health["status"], "ready");
        assert_eq!(health["worker_restarts"], 0);
        runtime.stop_active_worker().expect("stop recovered worker");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn model_arguments_accept_curated_identifiers_and_pinned_revisions() {
        assert!(validate_model_argument("hexgrad/Kokoro-82M").is_ok());
        assert!(validate_revision("0123456789abcdef0123456789abcdef01234567").is_ok());
    }

    #[test]
    fn model_arguments_reject_paths_and_moving_revisions() {
        assert!(validate_model_argument("../../private").is_err());
        assert!(validate_revision("main").is_err());
    }

    fn api_request(port: u16, token: Option<&str>, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect API");
        let authorization = token
            .map(|value| format!("Authorization: Bearer {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{authorization}Connection: close\r\n\r\n"
        )
        .expect("write API request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read API response");
        response
    }

    fn api_request_bytes(port: u16, token: &str, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect API");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        )
        .expect("write API request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("read API response");
        response
    }

    fn api_speech_request(port: u16, token: &str, body: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect speech API");
        write!(stream, "POST /v1/audio/speech HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("write speech request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("read speech response");
        response
    }

    fn api_raw_request(
        port: u16,
        token: &str,
        method: &str,
        path: &str,
        body: &str,
        extra_headers: &str,
    ) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect API");
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
        .expect("write API request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read API response");
        response
    }

    #[test]
    fn developer_api_requires_token_and_stops_cleanly() {
        let root = std::env::temp_dir().join(format!("soundar-api-{}", Uuid::new_v4()));
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime =
            RuntimeState::new_with_store(root.clone(), PathBuf::from("/missing/python"), store);
        let state = runtime.start_api_server(Some(0)).expect("start API");
        let port = state["port"].as_u64().expect("port") as u16;
        let token = state["token"].as_str().expect("token");
        let unauthorized = api_request(port, None, "/health");
        assert!(unauthorized.starts_with("HTTP/1.1 401"));
        let healthy = api_request(port, Some(token), "/health");
        assert!(healthy.starts_with("HTTP/1.1 200"));
        assert!(healthy.contains("\"local_only\":true"));
        assert!(runtime.stop_api_server().expect("stop API"));
        assert!(!runtime.api_server_status().expect("status")["running"]
            .as_bool()
            .unwrap_or(true));
        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn music_requests_are_scoped_to_text_to_music_before_queueing() {
        let prepared = super::prepare_music_generation_request(json!({
            "model_id": "facebook/musicgen-small",
            "prompt": "Warm instrumental ambient music",
            "duration_seconds": 10,
            "output_format": "wav",
            "priority": "normal",
        }))
        .expect("prepare music request");
        assert_eq!(prepared["operation"], "generate_music");
        assert_eq!(prepared["generation_kind"], "music");

        let reference_error = super::prepare_music_generation_request(json!({
            "model_id": "facebook/musicgen-small",
            "prompt": "Warm instrumental ambient music",
            "reference_audio_path": "/private/reference.wav",
        }))
        .expect_err("reference audio must be rejected");
        assert!(reference_error.contains("does not accept audio conditioning"));

        let lyric_request = super::prepare_music_generation_request(json!({
            "model_id": "ACE-Step/acestep-v15-xl-turbo-diffusers",
            "prompt": "Intimate indie-pop with close-mic vocals",
            "lyrics": "[Verse]\nHold the light until the morning comes",
            "vocal_language": "en",
            "duration_seconds": 10,
        }))
        .expect("direction and lyrics are accepted as independent text fields");
        assert_eq!(
            lyric_request["prompt"],
            "Intimate indie-pop with close-mic vocals"
        );
        assert_eq!(
            lyric_request["lyrics"],
            "[Verse]\nHold the light until the morning comes"
        );

        let operation_error = super::prepare_music_generation_request(json!({
            "model_id": "facebook/musicgen-small",
            "prompt": "Warm instrumental ambient music",
            "operation": "synthesize",
        }))
        .expect_err("cross-task operation must be rejected");
        assert!(operation_error.contains("cannot override"));
    }

    #[test]
    fn developer_api_prepares_speecht5_with_its_builtin_voice() {
        let root = std::env::temp_dir().join(format!("soundar-api-speecht5-{}", Uuid::new_v4()));
        let registry_path = root.join("models.json");
        fs::create_dir_all(&root).expect("create SpeechT5 API fixture");
        fs::write(
            &registry_path,
            r#"{"models":[{"model_id":"microsoft/speecht5_tts","engine":"speecht5"}]}"#,
        )
        .expect("write SpeechT5 registry");
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("/missing/python"),
            registry_path,
            store,
        );

        let (request, format) = super::prepare_api_synthesis_request(
            &runtime,
            &json!({
                "model": "microsoft/speecht5_tts",
                "input": "SpeechT5 uses its built-in speaker.",
                "voice": "default"
            }),
        )
        .expect("prepare SpeechT5 request without a clone profile");

        assert_eq!(request["speaker"], "default");
        assert!(request["reference_audio_path"].is_null());
        assert_eq!(request["voice_name"], "SpeechT5 default");
        assert_eq!(format, "wav");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn developer_api_control_plane_uses_durable_store_and_rejects_remote_origins() {
        let root = std::env::temp_dir().join(format!("soundar-api-control-{}", Uuid::new_v4()));
        let store = Store::open(root.join("state"), root.join("artifacts")).expect("test store");
        let runtime =
            RuntimeState::new_with_store(root.clone(), PathBuf::from("/missing/python"), store);
        let queued = runtime
            .store
            .create_job("test", &json!({ "source": "desktop" }))
            .expect("queue fixture job");
        let state = runtime.start_api_server(Some(0)).expect("start API");
        let port = state["port"].as_u64().expect("port") as u16;
        let token = state["token"].as_str().expect("token");

        let capabilities = api_request(port, Some(token), "/v1/capabilities");
        assert!(capabilities.starts_with("HTTP/1.1 200"));
        assert!(capabilities.contains("\"object\":\"list\""));
        let jobs = api_request(port, Some(token), "/v1/jobs");
        assert!(jobs.contains(&queued));
        let scheduler = api_request(port, Some(token), "/v1/runtime/scheduler");
        assert!(scheduler.starts_with("HTTP/1.1 200"));
        assert!(scheduler.contains("\"max_workers\":"));
        assert!(scheduler.contains("\"active_batches\":0"));

        let batch = api_raw_request(
            port,
            token,
            "POST",
            "/v1/batches",
            r#"{"name":"API batch","scripts":["one","two"],"settings":{"model_id":"hexgrad/Kokoro-82M"}}"#,
            "Idempotency-Key: api-control-batch\r\n",
        );
        assert!(batch.starts_with("HTTP/1.1 202"));
        let replayed_batch = api_raw_request(
            port,
            token,
            "POST",
            "/v1/batches",
            r#"{"name":"API batch","scripts":["one","two"],"settings":{"model_id":"hexgrad/Kokoro-82M"}}"#,
            "Idempotency-Key: api-control-batch\r\n",
        );
        assert!(replayed_batch.starts_with("HTTP/1.1 202"));
        let batch_body = batch.split("\r\n\r\n").nth(1).expect("batch response body");
        let replayed_body = replayed_batch
            .split("\r\n\r\n")
            .nth(1)
            .expect("replayed batch body");
        let batch_json: Value = serde_json::from_str(batch_body).expect("batch JSON");
        let replayed_json: Value =
            serde_json::from_str(replayed_body).expect("replayed batch JSON");
        assert_eq!(batch_json["id"], replayed_json["id"]);
        assert_eq!(
            runtime.store.list_batches().expect("durable batches").len(),
            1
        );
        let missing_key = api_raw_request(
            port,
            token,
            "POST",
            "/v1/batches",
            r#"{"name":"Missing key","scripts":["one"]}"#,
            "",
        );
        assert!(missing_key.starts_with("HTTP/1.1 400"));
        let conflicting_batch = api_raw_request(
            port,
            token,
            "POST",
            "/v1/batches",
            r#"{"name":"Changed batch","scripts":["different"]}"#,
            "Idempotency-Key: api-control-batch\r\n",
        );
        assert!(conflicting_batch.starts_with("HTTP/1.1 409 Conflict"));
        let rich_batch = api_raw_request(
            port,
            token,
            "POST",
            "/v1/batches",
            r#"{"name":"Localized API batch","rows":[{"text":"Bonjour","name":"French intro","output_name":"Opening","settings":{"language":"fr","seed":9}}],"settings":{"model_id":"hexgrad/Kokoro-82M"}}"#,
            "Idempotency-Key: rich-api-batch\r\n",
        );
        assert!(rich_batch.starts_with("HTTP/1.1 202"));
        assert!(rich_batch.contains("\"name\":\"French intro\""));
        assert!(rich_batch.contains("\"output_name\":\"0001-opening\""));
        assert!(rich_batch.contains("\"seed\":9"));
        let unsafe_batch = api_raw_request(
            port,
            token,
            "POST",
            "/v1/batches",
            r#"{"name":"Unsafe","rows":[{"text":"No","settings":{"reference_audio_path":"/tmp/private.wav"}}],"settings":{"model_id":"hexgrad/Kokoro-82M"}}"#,
            "Idempotency-Key: unsafe-api-batch\r\n",
        );
        assert!(unsafe_batch.starts_with("HTTP/1.1 400"));

        let controllable = runtime
            .store
            .create_batch(&json!({
                "name": "API controlled batch", "scripts": ["one", "two"],
                "settings": {"model_id": "hexgrad/Kokoro-82M"}
            }))
            .expect("create controllable batch");
        let controllable_id = controllable["id"].as_str().expect("controllable batch id");
        let paused = api_raw_request(
            port,
            token,
            "POST",
            &format!("/v1/batches/{controllable_id}/pause"),
            "{}",
            "",
        );
        assert!(paused.starts_with("HTTP/1.1 200"));
        assert!(paused.contains("\"status\":\"paused\""));
        let resumed = api_raw_request(
            port,
            token,
            "POST",
            &format!("/v1/batches/{controllable_id}/resume"),
            r#"{"parallelism":2,"retry_failed":false}"#,
            "",
        );
        assert!(resumed.starts_with("HTTP/1.1 202"));
        assert!(resumed.contains("\"status\":\"queued\""));

        let cancelled = api_raw_request(
            port,
            token,
            "POST",
            &format!("/v1/jobs/{queued}/cancel"),
            "{}",
            "",
        );
        assert!(cancelled.starts_with("HTTP/1.1 200"));
        assert!(cancelled.contains("\"cancelled\":true"));
        assert_eq!(
            runtime
                .store
                .job_status(&queued)
                .expect("job state")
                .as_deref(),
            Some("cancelled")
        );

        let rejected = api_raw_request(
            port,
            token,
            "GET",
            "/v1/jobs",
            "",
            "Origin: https://untrusted.example\r\n",
        );
        assert!(rejected.starts_with("HTTP/1.1 403"));
        assert!(runtime.stop_api_server().expect("stop API"));
        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn developer_api_retries_failed_synthesis_and_clears_finished_jobs() {
        let root = std::env::temp_dir().join(format!("soundar-api-retry-{}", Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).expect("create retry runtime");
        let final_path = artifacts.join("retried.wav");
        let staging_path = artifacts.join("retried.wav.partial");
        let final_json =
            serde_json::to_string(&final_path.to_string_lossy()).expect("final path JSON");
        let staging_json =
            serde_json::to_string(&staging_path.to_string_lossy()).expect("staging path JSON");
        fs::write(
            root.join("bridge.py"),
            format!(
                r#"import json, pathlib, sys
for line in sys.stdin:
    request = json.loads(line)
    pathlib.Path({staging_json}).write_bytes(b"RIFF\x04\x00\x00\x00WAVEretry")
    print(json.dumps({{"ok": True, "result": {{
        "id": "retry-history", "model_id": "test/model", "engine": "foundation",
        "audio_path": {final_json}, "staging_path": {staging_json}, "sample_rate": 24000,
        "duration_seconds": 1.0, "inference_seconds": 0.1, "rtf": 0.1,
        "vram_peak_mb": 0, "waveform": [0.5]
    }}}}), flush=True)
"#
            ),
        )
        .expect("write fake retry bridge");
        let store = Store::open(root.join("state"), artifacts).expect("test store");
        let runtime = RuntimeState::new_with_store(root.clone(), PathBuf::from("python3"), store);
        let request = json!({
            "operation": "analyze_audio", "text": "Retry through the API", "speaker": "test"
        });
        let failed = runtime
            .store
            .create_job("synthesis", &request)
            .expect("create failed job");
        runtime
            .store
            .fail_job(&failed, "first attempt failed")
            .expect("fail first attempt");
        let state = runtime.start_api_server(Some(0)).expect("start API");
        let port = state["port"].as_u64().expect("port") as u16;
        let token = state["token"].as_str().expect("token");

        let retried = api_raw_request(
            port,
            token,
            "POST",
            &format!("/v1/jobs/{failed}/retry"),
            "{}",
            "",
        );
        assert!(retried.starts_with("HTTP/1.1 202"));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if runtime
                .store
                .job_status(&failed)
                .expect("retry status")
                .as_deref()
                == Some("completed")
            {
                break;
            }
            assert!(Instant::now() < deadline, "retried job did not complete");
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            runtime
                .store
                .list_history(None)
                .expect("retry history")
                .len(),
            1
        );
        assert!(final_path.is_file());

        let cleared = api_raw_request(port, token, "POST", "/v1/jobs/clear-finished", "{}", "");
        assert!(cleared.starts_with("HTTP/1.1 200"));
        assert!(cleared.contains("\"cleared\":1"));
        assert!(runtime
            .store
            .list_jobs()
            .expect("visible jobs after clear")
            .is_empty());
        assert_eq!(
            runtime
                .store
                .list_history(None)
                .expect("history survives clear")
                .len(),
            1
        );
        assert!(runtime.stop_api_server().expect("stop API"));
        runtime.stop_active_worker().expect("stop fake worker");
        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn developer_api_async_speech_is_idempotent_and_serves_verified_audio() {
        let root = std::env::temp_dir().join(format!("soundar-api-async-{}", Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).expect("create async runtime");
        let artifacts_json =
            serde_json::to_string(&artifacts.to_string_lossy()).expect("artifact path JSON");
        fs::write(
            root.join("bridge.py"),
            format!(
                r#"import json, pathlib, struct, sys, time, uuid, wave
for line in sys.stdin:
    request = json.loads(line)
    time.sleep(0.15)
    output = pathlib.Path({artifacts_json}) / f"{{uuid.uuid4().hex}}.wav"
    staging = pathlib.Path(str(output) + ".partial")
    with wave.open(str(staging), "wb") as audio:
        audio.setnchannels(1); audio.setsampwidth(2); audio.setframerate(16000)
        audio.writeframes(struct.pack("<h", 0) * 800)
    print(json.dumps({{"ok": True, "result": {{
        "id": uuid.uuid4().hex, "model_id": request["model_id"], "engine": "foundation",
        "audio_path": str(output), "staging_path": str(staging), "sample_rate": 16000,
        "duration_seconds": 0.05, "inference_seconds": 0.15, "rtf": 3.0,
        "vram_peak_mb": 0, "waveform": [0.0, 0.0]
    }}}}), flush=True)
"#
            ),
        )
        .expect("write fake async bridge");
        let registry_path = root.join("models.json");
        fs::write(
            &registry_path,
            r#"{"models":[{"model_id":"test/model","engine":"foundation"}]}"#,
        )
        .expect("write async model registry");
        let store = Store::open(root.join("state"), artifacts.clone()).expect("test store");
        let runtime = RuntimeState::new_with_store_and_registry(
            root.clone(),
            PathBuf::from("python3"),
            registry_path,
            store,
        );
        let state = runtime.start_api_server(Some(0)).expect("start API");
        let port = state["port"].as_u64().expect("port") as u16;
        let token = state["token"].as_str().expect("token");
        let body = serde_json::to_string(&json!({
            "model": "test/model", "input": "Idempotent asynchronous speech",
            "voice": "default", "priority": "high",
        }))
        .expect("async body");
        let headers = "Idempotency-Key: async-test-1\r\n";
        let first = api_raw_request(port, token, "POST", "/v1/audio/speech/jobs", &body, headers);
        let second = api_raw_request(port, token, "POST", "/v1/audio/speech/jobs", &body, headers);
        assert!(first.starts_with("HTTP/1.1 202"));
        assert!(second.starts_with("HTTP/1.1 202"));
        let first_body = first.split("\r\n\r\n").nth(1).expect("first response body");
        let second_body = second
            .split("\r\n\r\n")
            .nth(1)
            .expect("second response body");
        let first_job: Value = serde_json::from_str(first_body).expect("first job JSON");
        let second_job: Value = serde_json::from_str(second_body).expect("second job JSON");
        assert_eq!(first_job["id"], second_job["id"]);
        assert_eq!(runtime.store.list_jobs().expect("one API job").len(), 1);

        let conflict_body = body.replace("Idempotent asynchronous speech", "Different speech");
        let conflict = api_raw_request(
            port,
            token,
            "POST",
            "/v1/audio/speech/jobs",
            &conflict_body,
            headers,
        );
        assert!(conflict.starts_with("HTTP/1.1 409 Conflict"));

        let job_id = first_job["id"].as_str().expect("job ID");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let job = runtime
                .store
                .get_job(job_id)
                .expect("job status")
                .expect("job");
            if job["status"] == "completed" {
                assert_eq!(
                    job["result"]["audio_url"],
                    format!("/v1/jobs/{job_id}/audio")
                );
                break;
            }
            assert!(Instant::now() < deadline, "async job did not complete");
            std::thread::sleep(Duration::from_millis(25));
        }
        let status = api_request(port, Some(token), &format!("/v1/jobs/{job_id}"));
        assert!(status.starts_with("HTTP/1.1 200"));
        assert!(status.contains("\"status\":\"completed\""));
        let events = api_raw_request(
            port,
            token,
            "GET",
            &format!("/v1/jobs/{job_id}/events"),
            "",
            "Last-Event-ID: 0\r\n",
        );
        assert!(events.starts_with("HTTP/1.1 200"));
        assert!(events.contains("Content-Type: text/event-stream"));
        assert!(events.contains("event: job"));
        assert!(events.contains("\"status\":\"preparing\""));
        assert!(events.contains("\"status\":\"completed\""));
        let audio = api_request_bytes(port, token, &format!("/v1/jobs/{job_id}/audio"));
        assert!(audio.starts_with(b"HTTP/1.1 200"));
        assert!(audio.windows(4).any(|bytes| bytes == b"RIFF"));
        assert_eq!(
            runtime
                .store
                .list_history(None)
                .expect("one history item")
                .len(),
            1
        );
        assert!(runtime.stop_api_server().expect("stop API"));
        runtime.stop_active_worker().expect("stop fake worker");
        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn project_import_parses_supported_structured_formats() {
        let markdown = parse_project_script("# Opening\nHello world.\n\n# Ending\nGoodbye.", "md")
            .expect("parse Markdown");
        assert_eq!(markdown.len(), 2);
        assert_eq!(markdown[0]["title"], "Opening");
        assert_eq!(markdown[1]["text"], "Goodbye.");

        let csv = parse_project_script("title,text\nIntro,Welcome\nOutro,Thanks", "csv")
            .expect("parse CSV");
        assert_eq!(csv.len(), 2);
        assert_eq!(csv[1]["title"], "Outro");

        let jsonl = parse_project_script(
            "{\"title\":\"One\",\"text\":\"First\"}\n{\"text\":\"Second\"}",
            "jsonl",
        )
        .expect("parse JSONL");
        assert_eq!(jsonl.len(), 2);
        assert_eq!(jsonl[1]["title"], "Chapter 2");

        let srt = parse_project_script(
            "1\n00:00:00,000 --> 00:00:01,000\nFirst line\n\n2\n00:00:01,000 --> 00:00:02,000\nSecond line",
            "srt",
        )
        .expect("parse SRT");
        assert_eq!(srt.len(), 2);
        assert_eq!(srt[0]["text"], "First line");
    }

    #[test]
    fn project_import_rejects_missing_csv_text_column() {
        let error = parse_project_script("title,notes\nIntro,Missing", "csv")
            .expect_err("CSV without text should fail");
        assert!(error.contains("text, script, or content"));
    }

    #[test]
    fn batch_import_parses_csv_and_jsonl_with_row_overrides() {
        let root = std::env::temp_dir().join(format!("soundar-batch-import-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create import root");
        let csv = root.join("campaign.csv");
        fs::write(
            &csv,
            "name,text,language,speed,output_name,priority\nIntro,\"Hello, listener.\",en,0.9,Opening Clip,urgent\n",
        )
        .expect("write CSV");
        let imported = read_batch_import(&csv).expect("parse CSV batch");
        assert_eq!(imported["source_format"], "csv");
        assert_eq!(imported["rows"][0]["text"], "Hello, listener.");
        assert_eq!(imported["rows"][0]["settings"]["speed"], 0.9);
        assert_eq!(imported["rows"][0]["output_name"], "0001-opening-clip");
        assert_eq!(imported["rows"][0]["priority"], "urgent");
        let store = Store::open(root.join("data"), root.join("artifacts"))
            .expect("open imported batch store");
        let queued = store
            .create_batch(&json!({
                "name": imported["name"],
                "rows": imported["rows"],
                "settings": {"model_id": "hexgrad/Kokoro-82M"}
            }))
            .expect("persist imported batch");
        assert_eq!(queued["items"][0]["output_name"], "0001-opening-clip");
        drop(store);

        let jsonl = root.join("localized.jsonl");
        fs::write(
            &jsonl,
            "{\"text\":\"Bonjour\",\"name\":\"French\",\"settings\":{\"language\":\"fr\",\"seed\":9}}\n",
        )
        .expect("write JSONL");
        let imported = read_batch_import(&jsonl).expect("parse JSONL batch");
        assert_eq!(imported["rows"][0]["name"], "French");
        assert_eq!(imported["rows"][0]["settings"]["seed"], 9);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn batch_import_reports_missing_columns_and_invalid_rows() {
        let root =
            std::env::temp_dir().join(format!("soundar-batch-import-errors-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create import root");
        let csv = root.join("missing.csv");
        fs::write(&csv, "name,script\nOne,Hello\n").expect("write invalid CSV");
        assert!(read_batch_import(&csv)
            .expect_err("reject missing text column")
            .contains("text column"));
        let jsonl = root.join("broken.jsonl");
        fs::write(&jsonl, "{not-json}\n").expect("write invalid JSONL");
        assert!(read_batch_import(&jsonl)
            .expect_err("reject invalid JSONL")
            .contains("row 1"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    #[ignore = "requires a real default microphone"]
    fn default_microphone_records_a_valid_wav() {
        let host = cpal::default_host();
        let device = host.default_input_device().expect("default microphone");
        let config = device.default_input_config().expect("microphone config");
        let root = std::env::temp_dir().join(format!("soundar-capture-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("capture temp directory");
        let output = root.join("capture.wav");
        let frames = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let queued = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let speech_active = Arc::new(AtomicBool::new(false));
        let speech_detected = Arc::new(AtomicBool::new(false));
        let speech_frames = Arc::new(AtomicU64::new(0));
        let silence_frames = Arc::new(AtomicU64::new(0));
        let noise_floor = Arc::new(AtomicU32::new(0.003_f32.to_bits()));
        let auto_stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let thread = {
            let frames = Arc::clone(&frames);
            let peak = Arc::clone(&peak);
            let stop = Arc::clone(&stop);
            let output = output.clone();
            std::thread::spawn(move || {
                capture_audio_thread(
                    device,
                    config,
                    output,
                    frames,
                    peak,
                    queued,
                    dropped,
                    speech_active,
                    speech_detected,
                    speech_frames,
                    silence_frames,
                    noise_floor,
                    auto_stopped,
                    true,
                    false,
                    1_200,
                    1.0,
                    stop,
                    sender,
                )
            })
        };
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("microphone start signal")
            .expect("start microphone");
        std::thread::sleep(std::time::Duration::from_millis(600));
        stop.store(true, std::sync::atomic::Ordering::Release);
        thread.join().expect("capture thread").expect("capture WAV");
        let reader = hound::WavReader::open(&output).expect("open recorded WAV");
        assert_eq!(reader.spec().channels, 1);
        assert!(reader.duration() > reader.spec().sample_rate / 5);
        drop(reader);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    #[ignore = "requires a real default audio output"]
    fn default_output_completes_silent_routed_playback() {
        let host = cpal::default_host();
        let device = host.default_output_device().expect("default audio output");
        let config = device.default_output_config().expect("output config");
        let output_rate = config.sample_rate().0;
        let played = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(AtomicU64::new(0));
        let completed = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let samples = vec![0.0_f32; (output_rate / 4) as usize];
        let thread = {
            let played = Arc::clone(&played);
            let underruns = Arc::clone(&underruns);
            let completed = Arc::clone(&completed);
            let error = Arc::clone(&error);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                super::playback_audio_thread(
                    device, config, samples, stop, played, underruns, completed, error, sender,
                )
            })
        };
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("output start signal")
            .expect("start audio output");
        thread
            .join()
            .expect("playback thread")
            .expect("play silence");
        assert!(completed.load(Ordering::Acquire));
        assert!(played.load(Ordering::Relaxed) >= u64::from(output_rate / 4));
    }

    #[test]
    #[ignore = "requires the packaged runtime, managed Python, NVIDIA GPU, and installed Kokoro, Whisper, and Parakeet models"]
    fn packaged_gpu_model_switch_soak_writes_release_evidence() {
        let runtime_root = PathBuf::from(
            std::env::var("SOUNDAR_E2E_RUNTIME_ROOT")
                .expect("SOUNDAR_E2E_RUNTIME_ROOT must point to packaged runtime resources"),
        );
        let python_path = PathBuf::from(
            std::env::var("SOUNDAR_E2E_PYTHON")
                .expect("SOUNDAR_E2E_PYTHON must point to managed Python"),
        );
        let report_path = PathBuf::from(
            std::env::var("SOUNDAR_SOAK_REPORT")
                .expect("SOUNDAR_SOAK_REPORT must name the JSON evidence file"),
        );
        let target_seconds = std::env::var("SOUNDAR_SOAK_DURATION_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1_800)
            .clamp(1, 86_400);
        let package_path = std::env::var("SOUNDAR_SOAK_PACKAGE")
            .ok()
            .map(PathBuf::from);
        let package_sha256 = package_path
            .as_deref()
            .map(sha256_path)
            .transpose()
            .expect("checksum candidate package");
        assert!(runtime_root.join("bridge.py").is_file());
        assert!(python_path.is_file());
        if let Some(parent) = report_path.parent() {
            fs::create_dir_all(parent).expect("create soak report directory");
        }

        let test_root = std::env::temp_dir().join(format!("soundar-gpu-soak-{}", Uuid::new_v4()));
        let store = Store::open(test_root.join("state"), home_dir().join(".soundAr/exports"))
            .expect("open isolated GPU soak store");
        let runtime =
            RuntimeState::new_with_store(runtime_root.clone(), python_path.clone(), store);
        let _cleanup = RuntimeCleanup(runtime.clone());
        let generated_artifacts = Arc::new(Mutex::new(Vec::new()));
        let _artifact_cleanup = GeneratedArtifactCleanup(Arc::clone(&generated_artifacts));
        let gpu_before = gpu_status(&python_path);
        assert_eq!(
            gpu_before["cuda_available"], true,
            "the packaged GPU soak requires an NVIDIA GPU visible through nvidia-smi"
        );
        let started_at = chrono::Utc::now();
        let started = Instant::now();
        let sampler = GpuPeakSampler::start(python_path.clone());
        let catalog = read_json(
            runtime_root.join("data/curated_models.json"),
            json!({ "models": [] }),
        );
        let engine_manifests = read_json(
            runtime_root.join("data/engine_manifests.json"),
            json!({ "engines": [] }),
        );
        let selected_models = [
            "hexgrad/Kokoro-82M",
            "openai/whisper-tiny",
            "nvidia/parakeet-tdt-1.1b",
        ];
        let model_evidence = catalog["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry["model_id"]
                    .as_str()
                    .is_some_and(|id| selected_models.contains(&id))
            })
            .map(|entry| {
                let engine = entry["engine"].as_str().unwrap_or("");
                let license = entry.get("license").cloned().or_else(|| {
                    engine_manifests["engines"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|manifest| manifest["id"].as_str() == Some(engine))
                        .and_then(|manifest| manifest.get("license").cloned())
                });
                json!({
                    "model_id": entry["model_id"],
                    "engine": entry["engine"],
                    "revision": entry.get("revision").cloned().unwrap_or(Value::Null),
                    "license": license.unwrap_or(Value::Null),
                })
            })
            .collect::<Vec<_>>();
        let mut iterations = Vec::new();
        let mut failure: Option<String> = None;

        while started.elapsed() < Duration::from_secs(target_seconds) || iterations.is_empty() {
            let iteration_number = iterations.len() + 1;
            let iteration_started = Instant::now();
            let run = (|| -> Result<Value, String> {
                let synthesis_started = Instant::now();
                let synthesis = synthesize_and_publish_result(
                    &runtime,
                    &json!({
                        "model_id": "hexgrad/Kokoro-82M",
                        "text": format!("soundAr release soak cycle {iteration_number} verifies model switching and recovery."),
                        "speaker": "af_heart",
                        "language": "en",
                        "speed": 1.0,
                        "seed": 50_000 + iteration_number,
                        "output_format": "wav",
                        "output_name": format!("soundar-gpu-soak-{}-{iteration_number}", Uuid::new_v4().simple()),
                        "title": format!("GPU soak cycle {iteration_number}"),
                        "priority": "urgent",
                    }),
                )?;
                let audio = verify_playable_wav(&runtime, &synthesis)?;
                let audio_path = synthesis["audio_path"]
                    .as_str()
                    .ok_or("Kokoro soak result has no audio path")?;
                generated_artifacts
                    .lock()
                    .map_err(|_| "GPU soak artifact cleanup lock failed")?
                    .push(PathBuf::from(audio_path));
                let kokoro_seconds = synthesis_started.elapsed().as_secs_f64();
                let kokoro_unload = runtime.unload_model_runtime("hexgrad/Kokoro-82M")?;
                require_quiescent_scheduler(&runtime)?;

                let whisper_started = Instant::now();
                let whisper = runtime.request(json!({
                    "operation": "transcribe",
                    "model_id": "openai/whisper-tiny",
                    "audio_path": audio_path,
                    "language": "en",
                    "task": "transcribe",
                    "priority": "high",
                }))?;
                if whisper["text"].as_str().unwrap_or("").trim().is_empty() {
                    return Err("Whisper returned an empty soak transcript".to_string());
                }
                let whisper_seconds = whisper_started.elapsed().as_secs_f64();
                let whisper_unload = runtime.unload_model_runtime("openai/whisper-tiny")?;
                require_quiescent_scheduler(&runtime)?;

                let parakeet_started = Instant::now();
                let parakeet = runtime.request(json!({
                    "operation": "transcribe",
                    "model_id": "nvidia/parakeet-tdt-1.1b",
                    "audio_path": audio_path,
                    "language": "en",
                    "task": "transcribe",
                    "priority": "high",
                }))?;
                if parakeet["text"].as_str().unwrap_or("").trim().is_empty() {
                    return Err("Parakeet returned an empty soak transcript".to_string());
                }
                let parakeet_seconds = parakeet_started.elapsed().as_secs_f64();
                let parakeet_unload = runtime.unload_model_runtime("nvidia/parakeet-tdt-1.1b")?;
                let scheduler = require_quiescent_scheduler(&runtime)?;
                let gpu = gpu_status(&python_path);

                Ok(json!({
                    "iteration": iteration_number,
                    "elapsed_seconds": started.elapsed().as_secs_f64(),
                    "duration_seconds": iteration_started.elapsed().as_secs_f64(),
                    "kokoro": {
                        "wall_seconds": kokoro_seconds,
                        "inference_seconds": synthesis["inference_seconds"],
                        "rtf": synthesis["rtf"],
                        "runtime_worker_state": synthesis["runtime_worker_state"],
                        "unload": kokoro_unload,
                    },
                    "audio": audio,
                    "whisper": {
                        "wall_seconds": whisper_seconds,
                        "inference_seconds": whisper["inference_seconds"],
                        "language": whisper["language"],
                        "language_probability": whisper["language_probability"],
                        "unload": whisper_unload,
                    },
                    "parakeet": {
                        "wall_seconds": parakeet_seconds,
                        "inference_seconds": parakeet["inference_seconds"],
                        "unload": parakeet_unload,
                    },
                    "gpu_after_cycle": gpu,
                    "scheduler_after_cycle": scheduler,
                }))
            })();
            match run {
                Ok(iteration) => iterations.push(iteration),
                Err(error) => {
                    failure = Some(format!("Cycle {iteration_number}: {error}"));
                    break;
                }
            }
        }

        let _ = runtime.stop_active_worker();
        let final_scheduler = runtime
            .scheduler_status()
            .unwrap_or_else(|error| json!({ "status_error": error }));
        if failure.is_none() {
            if let Err(error) = require_quiescent_scheduler(&runtime) {
                failure = Some(error);
            }
        }
        let oom_recovery = if failure.is_none() {
            match deterministic_oom_recovery_probe(&test_root) {
                Ok(evidence) => evidence,
                Err(error) => {
                    failure = Some(format!("Deterministic OOM recovery: {error}"));
                    json!({ "passed": false, "error": error })
                }
            }
        } else {
            json!({ "passed": false, "skipped": "real GPU stage failed" })
        };
        let peak_vram_mb = sampler.finish();
        let gpu_after = gpu_status(&python_path);
        let engine_health = ["kokoro", "transformers", "nemo"]
            .into_iter()
            .map(|engine| {
                (
                    engine.to_string(),
                    runtime
                        .worker_health_snapshot(engine, json!({}))
                        .unwrap_or_else(|error| json!({ "error": error })),
                )
            })
            .collect::<serde_json::Map<String, Value>>();
        let passed = failure.is_none();
        let report = json!({
            "schema_version": 1,
            "suite": "packaged_gpu_model_switch_oom_soak",
            "passed": passed,
            "started_at": started_at.to_rfc3339(),
            "completed_at": chrono::Utc::now().to_rfc3339(),
            "target_duration_seconds": target_seconds,
            "actual_duration_seconds": started.elapsed().as_secs_f64(),
            "completed_iterations": iterations.len(),
            "app_version": env!("CARGO_PKG_VERSION"),
            "package": package_path.as_deref().map(|path| json!({
                "file_name": path.file_name().and_then(|value| value.to_str()),
                "sha256": package_sha256,
            })),
            "runtime": {
                "bridge_sha256": sha256_path(&runtime_root.join("bridge.py")).ok(),
                "python_version": Command::new(&python_path).arg("--version").output().ok().map(|output| {
                    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    if stdout.is_empty() { stderr } else { stdout }
                }),
            },
            "models": model_evidence,
            "gpu_before": gpu_before,
            "gpu_after": gpu_after,
            "peak_system_vram_used_mb": peak_vram_mb,
            "iterations": iterations,
            "engine_health": engine_health,
            "final_scheduler": final_scheduler,
            "oom_recovery": oom_recovery,
            "failure": failure,
        });
        write_json_atomically(&report_path, &report).expect("write GPU soak evidence");
        println!("soundAr GPU soak evidence: {}", report_path.display());
        fs::remove_dir_all(test_root).ok();
        if !passed {
            panic!(
                "packaged GPU soak failed: {}",
                report["failure"].as_str().unwrap_or("unknown failure")
            );
        }
    }

    #[test]
    #[ignore = "requires the packaged MusicGen runtime, an NVIDIA GPU, and an installed facebook/musicgen-small checkpoint"]
    fn packaged_runtime_generates_playable_music_through_native_bridge() {
        let runtime_root = PathBuf::from(
            std::env::var("SOUNDAR_E2E_RUNTIME_ROOT")
                .expect("SOUNDAR_E2E_RUNTIME_ROOT must point to packaged runtime resources"),
        );
        let python_path = PathBuf::from(
            std::env::var("SOUNDAR_E2E_PYTHON")
                .expect("SOUNDAR_E2E_PYTHON must point to managed Python"),
        );
        assert!(runtime_root.join("core/music_engine.py").is_file());
        assert!(runtime_root.join("engines/music/musicgen.py").is_file());
        let test_root = std::env::temp_dir().join(format!("soundar-music-gpu-{}", Uuid::new_v4()));
        let store = Store::open(test_root.join("state"), home_dir().join(".soundAr/exports"))
            .expect("open isolated MusicGen GPU test store");
        let runtime = RuntimeState::new_with_store(runtime_root, python_path, store);
        let _cleanup = RuntimeCleanup(runtime.clone());
        let generated_artifacts = Arc::new(Mutex::new(Vec::new()));
        let _artifact_cleanup = GeneratedArtifactCleanup(Arc::clone(&generated_artifacts));
        let request = json!({
            "operation": "generate_music",
            "generation_kind": "music",
            "model_id": "facebook/musicgen-small",
            "prompt": "Warm instrumental ambient music with slowly evolving analog synths, sparse piano, and no vocals.",
            "duration_seconds": 4,
            "guidance_scale": 3.0,
            "temperature": 1.0,
            "top_k": 250,
            "top_p": 0.0,
            "seed": 42817,
            "output_format": "wav",
            "title": "Packaged MusicGen smoke",
            "priority": "urgent",
        });
        let cold = generate_music_and_publish_result(&runtime, &request)
            .expect("generate packaged MusicGen audio");
        let cold_audio =
            verify_playable_wav(&runtime, &cold).expect("decode packaged MusicGen WAV");
        let cold_path = cold["audio_path"]
            .as_str()
            .expect("MusicGen artifact path")
            .to_string();
        generated_artifacts
            .lock()
            .expect("music artifact cleanup lock")
            .push(PathBuf::from(&cold_path));
        assert_eq!(cold["generation_kind"], "music");
        assert_eq!(cold["voice"], "Not applicable");
        assert_eq!(cold["engine"], "musicgen");
        assert_eq!(cold["sample_rate"], 32_000);
        assert_eq!(cold["runtime_worker_state"], "cold");
        assert!(cold["duration_seconds"].as_f64().unwrap_or(0.0) >= 3.5);
        assert!(cold_audio["frames"].as_u64().unwrap_or(0) > 0);
        let history_request = runtime
            .store
            .history_request(cold["id"].as_str().expect("music history id"))
            .expect("read durable music request");
        assert_eq!(history_request["generation_kind"], "music");
        assert_eq!(history_request["prompt"], request["prompt"]);

        let warm = generate_music_and_publish_result(&runtime, &request)
            .expect("generate warm packaged MusicGen audio");
        let warm_path = warm["audio_path"]
            .as_str()
            .expect("warm MusicGen artifact path")
            .to_string();
        generated_artifacts
            .lock()
            .expect("music artifact cleanup lock")
            .push(PathBuf::from(warm_path));
        assert_eq!(warm["runtime_worker_state"], "warm");
        verify_playable_wav(&runtime, &warm).expect("decode warm packaged MusicGen WAV");

        runtime
            .unload_model_runtime("facebook/musicgen-small")
            .expect("unload MusicGen worker");
        require_quiescent_scheduler(&runtime).expect("release MusicGen scheduler reservation");
        runtime.stop_active_worker().expect("stop MusicGen worker");
        fs::remove_dir_all(test_root).ok();
    }

    #[test]
    #[ignore = "requires the packaged ACE-Step runtime, an NVIDIA GPU, and an installed ACE-Step/Ace-Step1.5 checkpoint"]
    fn packaged_runtime_generates_playable_acestep_studio_music_through_native_bridge() {
        let runtime_root = PathBuf::from(
            std::env::var("SOUNDAR_E2E_RUNTIME_ROOT")
                .expect("SOUNDAR_E2E_RUNTIME_ROOT must point to packaged runtime resources"),
        );
        let python_path = PathBuf::from(
            std::env::var("SOUNDAR_E2E_PYTHON")
                .expect("SOUNDAR_E2E_PYTHON must point to managed Python"),
        );
        assert!(runtime_root.join("core/music_engine.py").is_file());
        assert!(runtime_root.join("engines/music/acestep.py").is_file());
        let test_root =
            std::env::temp_dir().join(format!("soundar-acestep-gpu-{}", Uuid::new_v4()));
        let store = Store::open(test_root.join("state"), home_dir().join(".soundAr/exports"))
            .expect("open isolated ACE-Step GPU test store");
        let runtime = RuntimeState::new_with_store(runtime_root, python_path, store);
        let _cleanup = RuntimeCleanup(runtime.clone());
        let generated_artifacts = Arc::new(Mutex::new(Vec::new()));
        let _artifact_cleanup = GeneratedArtifactCleanup(Arc::clone(&generated_artifacts));
        let request = json!({
            "operation": "generate_music",
            "generation_kind": "music",
            "model_id": "ACE-Step/Ace-Step1.5",
            "prompt": "Warm, intimate indie-pop with brushed drums, soft electric piano, a restrained build, and a close-mic vocal performance.",
            "lyrics": "[Verse]\nThe city hums beneath the rain\nI trace your name across the windowpane\n\n[Chorus]\nHold the light until the morning comes",
            "vocal_language": "en",
            "duration_seconds": 10,
            "inference_steps": 8,
            "shift": 3.0,
            "bpm": 96,
            "seed": 42817,
            "output_format": "wav",
            "title": "Packaged ACE-Step lyric smoke",
            "priority": "urgent",
        });
        let cold = generate_music_and_publish_result(&runtime, &request)
            .expect("generate packaged ACE-Step lyric music");
        let cold_audio =
            verify_playable_wav(&runtime, &cold).expect("decode packaged ACE-Step WAV");
        let cold_path = cold["audio_path"]
            .as_str()
            .expect("ACE-Step artifact path")
            .to_string();
        generated_artifacts
            .lock()
            .expect("ACE-Step artifact cleanup lock")
            .push(PathBuf::from(&cold_path));
        assert_eq!(cold["generation_kind"], "music");
        assert_eq!(cold["voice"], "Not applicable");
        assert_eq!(cold["engine"], "acestep");
        assert_eq!(cold["sample_rate"], 48_000);
        assert_eq!(cold["runtime_worker_state"], "cold");
        assert!(cold["duration_seconds"].as_f64().unwrap_or(0.0) >= 9.5);
        assert_eq!(cold_audio["sample_rate"], 48_000);
        assert_eq!(cold_audio["channels"], 2);
        assert!(cold_audio["frames"].as_u64().unwrap_or(0) > 0);
        let history_request = runtime
            .store
            .history_request(cold["id"].as_str().expect("ACE-Step music history id"))
            .expect("read durable ACE-Step music request");
        assert_eq!(history_request["generation_kind"], "music");
        assert_eq!(history_request["prompt"], request["prompt"]);
        assert_eq!(history_request["lyrics"], request["lyrics"]);
        assert_eq!(history_request["vocal_language"], "en");

        let warm = generate_music_and_publish_result(&runtime, &request)
            .expect("generate warm packaged ACE-Step lyric music");
        let warm_path = warm["audio_path"]
            .as_str()
            .expect("warm ACE-Step artifact path")
            .to_string();
        generated_artifacts
            .lock()
            .expect("ACE-Step artifact cleanup lock")
            .push(PathBuf::from(warm_path));
        assert_eq!(warm["runtime_worker_state"], "warm");
        verify_playable_wav(&runtime, &warm).expect("decode warm packaged ACE-Step WAV");

        runtime
            .unload_model_runtime("ACE-Step/Ace-Step1.5")
            .expect("unload ACE-Step worker");
        require_quiescent_scheduler(&runtime).expect("release ACE-Step scheduler reservation");
        runtime.stop_active_worker().expect("stop ACE-Step worker");
        fs::remove_dir_all(test_root).ok();
    }

    #[test]
    #[ignore = "requires a packaged runtime, managed Python, GPU, and installed smoke model"]
    fn packaged_runtime_generates_playable_audio_through_native_bridge() {
        let runtime_root = PathBuf::from(
            std::env::var("SOUNDAR_E2E_RUNTIME_ROOT")
                .expect("SOUNDAR_E2E_RUNTIME_ROOT must point to packaged runtime resources"),
        );
        let python_path = PathBuf::from(
            std::env::var("SOUNDAR_E2E_PYTHON")
                .expect("SOUNDAR_E2E_PYTHON must point to managed Python"),
        );
        assert!(
            runtime_root.join("bridge.py").is_file(),
            "runtime root must contain bridge.py"
        );
        let test_root = std::env::temp_dir().join(format!("soundar-gpu-{}", Uuid::new_v4()));
        let store = Store::open(test_root.join("state"), home_dir().join(".soundAr/exports"))
            .expect("open isolated GPU test store");
        let runtime = RuntimeState::new_with_store(runtime_root, python_path, store);
        let _cleanup = RuntimeCleanup(runtime.clone());
        let benchmark_preparation = runtime
            .prepare_benchmark_engine("hexgrad/Kokoro-82M")
            .expect("reserve real cold benchmark");
        let benchmark_token = benchmark_preparation["token"]
            .as_str()
            .expect("real benchmark token")
            .to_string();
        let result = synthesize_and_publish(
            &runtime,
            json!({
                "model_id": "hexgrad/Kokoro-82M",
                "text": "soundAr packaged native bridge test.",
                "speaker": "af_heart",
                "language": "en",
                "speed": 1.0,
                "seed": 42817,
                "output_format": "wav"
                ,"priority": "urgent",
                "benchmark_token": benchmark_token
            }),
        );

        let path = result["audio_path"]
            .as_str()
            .expect("synthesis should return an audio path");
        let bytes = runtime
            .store
            .generated_audio_bytes(path)
            .expect("native playback guard should read output");
        assert!(bytes.starts_with(b"RIFF"));
        assert_eq!(result["engine"], "kokoro");
        assert_eq!(result["sample_rate"], 24_000);
        assert_eq!(result["runtime_worker_state"], "cold");
        assert!(result["end_to_end_seconds"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(result["runtime_overhead_seconds"].as_f64().is_some());
        assert!(result["duration_seconds"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(result["waveform"]
            .as_array()
            .is_some_and(|values| !values.is_empty()));
        for pass in 2..=3 {
            let warm = synthesize_and_publish(
                &runtime,
                json!({
                    "model_id": "hexgrad/Kokoro-82M",
                    "text": "soundAr verifies a warm native benchmark pass.",
                    "speaker": "af_heart", "language": "en", "speed": 1.0,
                    "seed": 42817, "output_format": "wav",
                    "title": format!("GPU benchmark pass {pass}"),
                    "benchmark_token": benchmark_token
                }),
            );
            assert_eq!(warm["runtime_worker_state"], "warm");
        }
        assert_eq!(
            runtime
                .scheduler_status()
                .expect("released benchmark reservation")["benchmark_reserved"],
            false
        );
        let history_id = result["id"].as_str().expect("generated history ID");
        let duplicate = runtime
            .store
            .duplicate_history(history_id)
            .expect("duplicate generated GPU artifact");
        let duplicate_bytes = runtime
            .store
            .generated_audio_bytes(
                duplicate["audio_path"]
                    .as_str()
                    .expect("duplicate audio path"),
            )
            .expect("duplicate playback bytes");
        assert_eq!(duplicate_bytes, bytes);
        let export_path = test_root.join("gpu-history-export.wav");
        let export_receipt = runtime
            .store
            .export_history(history_id, &export_path.to_string_lossy())
            .expect("export generated GPU artifact");
        assert_eq!(export_receipt["size_bytes"], bytes.len());
        assert_eq!(fs::read(&export_path).expect("read GPU export"), bytes);

        let first_pid = {
            let pool = runtime.worker_pool.lock().expect("runtime lock");
            let process = pool
                .iter()
                .find(|process| process.engine == "kokoro")
                .expect("Kokoro worker should be warm");
            assert_eq!(process.engine, "kokoro");
            process.child.id()
        };
        let parallel_started = Instant::now();
        let parallel_first = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                synthesize_and_publish(
                    &runtime,
                    json!({
                        "model_id": "hexgrad/Kokoro-82M",
                        "text": "The first parallel soundAr generation is active.",
                        "speaker": "af_heart", "language": "en", "speed": 1.0,
                        "seed": 42819, "output_format": "wav"
                    }),
                )
            })
        };
        let parallel_second = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                synthesize_and_publish(
                    &runtime,
                    json!({
                        "model_id": "hexgrad/Kokoro-82M",
                        "text": "The second parallel soundAr generation is active.",
                        "speaker": "af_heart", "language": "en", "speed": 1.0,
                        "seed": 42820, "output_format": "wav"
                    }),
                )
            })
        };
        let parallel_results = [
            parallel_first.join().expect("first parallel thread"),
            parallel_second.join().expect("second parallel thread"),
        ];
        for output in &parallel_results {
            assert!(runtime
                .store
                .generated_audio_bytes(output["audio_path"].as_str().expect("parallel path"))
                .expect("parallel audio bytes")
                .starts_with(b"RIFF"));
        }
        let summed_inference = parallel_results
            .iter()
            .filter_map(|output| output["inference_seconds"].as_f64())
            .sum::<f64>();
        assert!(
            parallel_started.elapsed().as_secs_f64() < summed_inference + 8.0,
            "parallel GPU jobs should not be serialized behind one worker"
        );
        let transcript = runtime
            .request(json!({
                "operation": "transcribe",
                "model_id": "openai/whisper-tiny",
                "audio_path": path,
                "language": "en",
                "task": "transcribe"
            }))
            .expect("packaged transcription should succeed");
        assert!(!transcript["text"].as_str().unwrap_or("").is_empty());
        let benchmark_source = runtime
            .store
            .import_transcription_source(path)
            .expect("import generated audio as benchmark evidence");
        let benchmark_transcription_request = json!({
            "operation": "transcribe", "model_id": "openai/whisper-tiny",
            "audio_path": benchmark_source
        });
        let benchmark_transcription_job = runtime
            .store
            .create_job("transcription", &benchmark_transcription_request)
            .expect("create benchmark transcription job");
        runtime
            .store
            .start_job(&benchmark_transcription_job)
            .expect("start benchmark transcription job");
        let benchmark_transcription = runtime
            .store
            .complete_transcription(
                &benchmark_transcription_job,
                benchmark_source
                    .to_str()
                    .expect("benchmark transcription path"),
                benchmark_source
                    .to_str()
                    .expect("benchmark original transcription path"),
                &json!({ "algorithm": "none" }),
                &transcript,
            )
            .expect("persist benchmark transcription");
        let benchmark = runtime
            .store
            .save_benchmark(&json!({
                "history_id": history_id,
                "transcription_id": benchmark_transcription["id"],
                "warm_state": "warm",
                "gpu_name": "NVIDIA GeForce RTX 4080 Laptop GPU",
                "app_version": "0.3.0"
            }))
            .expect("derive benchmark intelligibility evidence");
        assert_eq!(benchmark["verifier_model_id"], "openai/whisper-tiny");
        assert_eq!(benchmark["warm_state"], "cold");
        assert!(benchmark["end_to_end_seconds"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(benchmark["word_error_rate"].as_f64().is_some());
        assert!(benchmark["character_error_rate"].as_f64().is_some());
        assert_eq!(benchmark["source_sha256"], export_receipt["sha256"]);
        let second_pid = {
            let pool = runtime.worker_pool.lock().expect("runtime lock");
            pool.iter()
                .find(|process| process.engine == "transformers")
                .expect("Whisper worker should be warm")
                .child
                .id()
        };
        assert_ne!(
            first_pid, second_pid,
            "separate engines must use isolated worker processes"
        );
        let parakeet = runtime
            .request(json!({
                "operation": "transcribe",
                "model_id": "nvidia/parakeet-tdt-1.1b",
                "audio_path": path,
                "language": "en",
                "task": "transcribe"
            }))
            .expect("isolated Parakeet transcription should succeed");
        assert_eq!(parakeet["engine"], "nemo");
        assert!(!parakeet["text"].as_str().unwrap_or("").is_empty());
        let parakeet_pid = {
            let pool = runtime.worker_pool.lock().expect("runtime lock");
            assert!(
                pool.iter().all(|process| process.engine != "transformers"),
                "the scheduler should retire the idle Whisper worker before loading Parakeet"
            );
            pool.iter()
                .find(|process| process.engine == "nemo")
                .expect("Parakeet worker should be warm")
                .child
                .id()
        };
        assert_ne!(
            second_pid, parakeet_pid,
            "Whisper and Parakeet must use distinct isolated workers"
        );

        let speecht5 = synthesize_and_publish(
            &runtime,
            json!({
                "model_id": "microsoft/speecht5_tts",
                "text": "Speech T five is qualified inside its isolated soundAr runtime.",
                "speaker": "default",
                "language": "en",
                "speed": 1.0,
                "seed": 42830,
                "output_format": "wav"
            }),
        );
        assert_eq!(speecht5["engine"], "speecht5");
        assert_eq!(speecht5["sample_rate"], 16_000);
        assert!(runtime
            .store
            .generated_audio_bytes(speecht5["audio_path"].as_str().expect("SpeechT5 path"))
            .expect("SpeechT5 playback bytes")
            .starts_with(b"RIFF"));

        let chatterbox = synthesize_and_publish(
            &runtime,
            json!({
                "model_id": "ResembleAI/chatterbox",
                "text": "Standard Chatterbox is qualified by the native soundAr runtime.",
                "speaker": "default",
                "language": "en",
                "exaggeration": 0.5,
                "cfg_weight": 0.5,
                "seed": 42831,
                "output_format": "wav"
            }),
        );
        assert_eq!(chatterbox["engine"], "chatterbox");
        assert!(runtime
            .store
            .generated_audio_bytes(chatterbox["audio_path"].as_str().expect("Chatterbox path"))
            .expect("Chatterbox playback bytes")
            .starts_with(b"RIFF"));

        let xtts = synthesize_and_publish(
            &runtime,
            json!({
                "model_id": "coqui/XTTS-v2",
                "text": "X T T S is qualified with a synthetic local reference.",
                "speaker": "default",
                "language": "en",
                "reference_audio_path": chatterbox["audio_path"],
                "speed": 1.0,
                "seed": 42832,
                "output_format": "wav"
            }),
        );
        assert_eq!(xtts["engine"], "coqui");
        assert!(runtime
            .store
            .generated_audio_bytes(xtts["audio_path"].as_str().expect("XTTS path"))
            .expect("XTTS playback bytes")
            .starts_with(b"RIFF"));

        let second_result = synthesize_and_publish(
            &runtime,
            json!({
                "model_id": "hexgrad/Kokoro-82M",
                "text": "soundAr switched back to the voice engine.",
                "speaker": "af_heart",
                "language": "en-US",
                "speed": 1.0,
                "seed": 42818,
                "output_format": "wav"
            }),
        );
        assert_eq!(second_result["engine"], "kokoro");
        let third_pid = runtime
            .worker_pool
            .lock()
            .expect("runtime lock")
            .iter()
            .find(|process| process.engine == "kokoro")
            .expect("Kokoro worker should be warm")
            .child
            .id();
        assert_ne!(
            second_pid, third_pid,
            "engine runtimes must remain isolated"
        );

        let turbo = synthesize_and_publish(
            &runtime,
            json!({
                "model_id": "ResembleAI/chatterbox-turbo",
                "text": "soundAr Turbo is qualified with a [chuckle] compact local test.",
                "speaker": "default",
                "language": "en",
                "temperature": 0.8,
                "top_p": 0.95,
                "repetition_penalty": 1.2,
                "seed": 42821,
                "output_format": "wav"
            }),
        );
        assert_eq!(turbo["engine"], "chatterbox-turbo");
        assert!(turbo["rtf"].as_f64().unwrap_or(10.0) < 1.0);
        assert!(runtime
            .store
            .generated_audio_bytes(turbo["audio_path"].as_str().expect("Turbo path"))
            .expect("Turbo playback bytes")
            .starts_with(b"RIFF"));

        let api = runtime.start_api_server(Some(0)).expect("start local API");
        let port = api["port"].as_u64().expect("API port") as u16;
        let token = api["token"].as_str().expect("API token");
        let api_response = api_speech_request(
            port,
            token,
            r#"{"model":"hexgrad/Kokoro-82M","input":"soundAr local API GPU test.","voice":"af_heart","response_format":"wav"}"#,
        );
        let header_end = api_response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP header terminator");
        assert!(
            String::from_utf8_lossy(&api_response[..header_end]).starts_with("HTTP/1.1 200"),
            "API generation failed: {}",
            String::from_utf8_lossy(&api_response)
        );
        assert!(api_response[header_end + 4..].starts_with(b"RIFF"));
        assert!(runtime
            .store
            .list_history(Some("local API GPU test"))
            .expect("API history")
            .iter()
            .any(|item| item["text"] == "soundAr local API GPU test."));

        let batch_response = api_raw_request(
            port,
            token,
            "POST",
            "/v1/batches",
            r#"{"name":"GPU API batch","priority":"high","rows":[{"text":"First API batch row.","name":"First GPU row","output_name":"first-row"},{"text":"Second API batch row.","name":"Second GPU row","output_name":"second-row","priority":"urgent","settings":{"speed":1.05,"seed":42909}}],"parallelism":2,"settings":{"model_id":"hexgrad/Kokoro-82M","speaker":"af_heart","language":"en","speed":1.0,"seed":42900,"output_format":"wav","voice_name":"Heart"}}"#,
            "Idempotency-Key: gpu-api-batch\r\n",
        );
        assert!(batch_response.starts_with("HTTP/1.1 202"));
        let body = batch_response
            .split("\r\n\r\n")
            .nth(1)
            .expect("batch response body");
        let batch: Value = serde_json::from_str(body).expect("batch response JSON");
        let batch_id = batch["id"].as_str().expect("batch ID");
        let mut completed = None;
        for _ in 0..120 {
            let response = api_request(port, Some(token), &format!("/v1/batches/{batch_id}"));
            let state: Value =
                serde_json::from_str(response.split("\r\n\r\n").nth(1).expect("batch poll body"))
                    .expect("batch poll JSON");
            if matches!(
                state["status"].as_str(),
                Some("completed" | "failed" | "cancelled")
            ) {
                completed = Some(state);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let completed = completed.expect("API batch should finish within 12 seconds");
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["completed_items"], 2);
        assert_eq!(completed["priority"], "high");
        assert_eq!(completed["items"][0]["priority"], "high");
        assert_eq!(completed["items"][1]["priority"], "urgent");
        assert!(completed["items"]
            .as_array()
            .expect("batch items")
            .iter()
            .all(|item| item["job_id"].is_string() && item["history_id"].is_string()));
        let batch_history = ["First GPU row", "Second GPU row"]
            .into_iter()
            .map(|title| {
                runtime
                    .store
                    .list_history(Some(title))
                    .expect("batch row history")
                    .into_iter()
                    .find(|item| item["title"] == title)
                    .expect("named batch row history")
            })
            .collect::<Vec<_>>();
        let first_path = batch_history[0]["audio_path"]
            .as_str()
            .expect("first batch path");
        let second_path = batch_history[1]["audio_path"]
            .as_str()
            .expect("second batch path");
        let batch_prefix = &batch_id[..8];
        assert!(first_path.contains(&format!("batch-{batch_prefix}-0001-first-row-a01.wav")));
        assert!(second_path.contains(&format!("batch-{batch_prefix}-0002-second-row-a01.wav")));
        assert_ne!(first_path, second_path);
        assert!(runtime
            .store
            .generated_audio_bytes(first_path)
            .expect("first batch audio")
            .starts_with(b"RIFF"));
        assert!(runtime
            .store
            .generated_audio_bytes(second_path)
            .expect("second batch audio")
            .starts_with(b"RIFF"));

        let comparison = runtime
            .store
            .create_comparison(&json!({
                "script": "The same GPU voice should preserve a useful difference between takes.",
                "blind": true,
                "takes": [
                    {"model_id":"hexgrad/Kokoro-82M","speaker":"af_heart","language":"en","speed":1.0,"seed":43001,"output_format":"wav"},
                    {"model_id":"hexgrad/Kokoro-82M","speaker":"af_heart","language":"en","speed":1.08,"seed":43002,"output_format":"wav"}
                ]
            }))
            .expect("create GPU comparison");
        let comparison = runtime
            .execute_comparison(comparison["id"].as_str().expect("comparison ID"))
            .expect("execute GPU comparison");
        assert_eq!(comparison["status"], "completed");
        assert_eq!(
            comparison["takes"]
                .as_array()
                .expect("comparison takes")
                .len(),
            2
        );
        for take in comparison["takes"].as_array().expect("comparison takes") {
            let path = take["result"]["audio_path"]
                .as_str()
                .expect("comparison audio path");
            assert!(runtime
                .store
                .generated_audio_bytes(path)
                .expect("comparison playback bytes")
                .starts_with(b"RIFF"));
        }
        assert!(runtime.stop_api_server().expect("stop local API"));

        let loaded = runtime
            .prewarm_model("hexgrad/Kokoro-82M")
            .expect("prewarm Kokoro through the lifecycle API");
        assert_eq!(loaded["status"], "loaded");
        assert_eq!(loaded["device"], "cuda:0");
        let health = runtime
            .check_engine_health("kokoro")
            .expect("inspect resident Kokoro worker");
        assert!(health["loaded_models"]
            .as_array()
            .is_some_and(|models| models.iter().any(|model| model == "hexgrad/Kokoro-82M")));
        let unloaded = runtime
            .unload_model_runtime("hexgrad/Kokoro-82M")
            .expect("unload Kokoro through the lifecycle API");
        assert_eq!(unloaded["status"], "unloaded");
        assert!(unloaded["retired_workers"].as_u64().unwrap_or(0) >= 1);
        assert!(runtime
            .worker_pool
            .lock()
            .expect("runtime lock")
            .iter()
            .all(|process| process.engine != "kokoro"));

        runtime.stop_active_worker().expect("stop worker pool");
        fs::remove_dir_all(test_root).ok();
    }
}
